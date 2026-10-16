/**
 * @file list_model.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief Constructs row display payloads off the GTK thread, handles batched updates,
 *        enforces notification consistency, and stabilizes row recycling.
 */

use std::cell::Cell;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowPayload {
    pub id: u32,
    pub account_id: u32,
    pub folder_id: u32,
    pub uid: u32,
    pub from_name: String,
    pub from_addr: String,
    pub subject: String,
    pub preview: String,
    pub date_display: String,
    pub unread: bool,
    pub starred: bool,
    pub has_attachment: bool,
    pub initials: String,
}

#[derive(Debug, Clone)]
pub struct RowPayloadBatch {
    pub generation: u64,
    pub folder_id: u32,
    pub position: usize,
    pub n_removals: usize,
    pub rows: Vec<RowPayload>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpliceSpec {
    pub position: usize,
    pub n_removals: usize,
    pub n_additions: usize,
}

impl SpliceSpec {
    pub fn new(position: usize, n_removals: usize, n_additions: usize) -> Self {
        Self {
            position,
            n_removals,
            n_additions,
        }
    }
}

/// Prevents recursive model mutation inside list change callbacks
pub struct ListMutationGuard<'a> {
    lock: &'a Cell<bool>,
}

impl<'a> ListMutationGuard<'a> {
    pub fn try_acquire(lock: &'a Cell<bool>) -> Option<Self> {
        if lock.get() {
            None
        } else {
            lock.set(true);
            Some(Self { lock })
        }
    }
}

impl<'a> Drop for ListMutationGuard<'a> {
    fn drop(&mut self) {
        self.lock.set(false);
    }
}

/// Disconnects old signal bindings when GTK recycled rows are reused
#[derive(Default)]
pub struct SignalBindingRegistry {
    handlers: Vec<usize>,
}

impl SignalBindingRegistry {
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    pub fn track(&mut self, handler_id: usize) {
        self.handlers.push(handler_id);
    }

    pub fn clear<F>(&mut self, mut disconnect: F)
    where
        F: FnMut(usize),
    {
        for id in self.handlers.drain(..) {
            disconnect(id);
        }
    }
}

/// Batches message-list updates to prevent UI update storms
pub struct RowUpdateBatcher {
    queue: VecDeque<RowPayload>,
    last_flush: Instant,
    max_batch_size: usize,
    max_batch_delay: Duration,
    current_generation: u64,
    current_folder_id: u32,
}

impl RowUpdateBatcher {
    pub const DEFAULT_MAX_BATCH_SIZE: usize = 50;
    pub const DEFAULT_MAX_DELAY: Duration = Duration::from_millis(32); // 2 frames at 60Hz

    pub fn new(generation: u64, folder_id: u32) -> Self {
        Self {
            queue: VecDeque::with_capacity(64),
            last_flush: Instant::now(),
            max_batch_size: Self::DEFAULT_MAX_BATCH_SIZE,
            max_batch_delay: Self::DEFAULT_MAX_DELAY,
            current_generation: generation,
            current_folder_id: folder_id,
        }
    }

    pub fn set_folder(&mut self, generation: u64, folder_id: u32) {
        self.queue.clear();
        self.current_generation = generation;
        self.current_folder_id = folder_id;
        self.last_flush = Instant::now();
    }

    pub fn push(&mut self, row: RowPayload, generation: u64) -> Option<RowPayloadBatch> {
        // Discard rows belonging to older generations
        if generation < self.current_generation {
            return None;
        }

        self.queue.push_back(row);
        if self.queue.len() >= self.max_batch_size || self.last_flush.elapsed() >= self.max_batch_delay {
            self.flush()
        } else {
            None
        }
    }

    /// Immediate bypass for selected-message opens: never deferred behind batch queue
    pub fn bypass_immediate(row: RowPayload, generation: u64) -> RowPayloadBatch {
        RowPayloadBatch {
            generation,
            folder_id: row.folder_id,
            position: 0,
            n_removals: 0,
            rows: vec![row],
        }
    }

    pub fn flush(&mut self) -> Option<RowPayloadBatch> {
        if self.queue.is_empty() {
            return None;
        }
        let rows: Vec<RowPayload> = self.queue.drain(..).collect();
        self.last_flush = Instant::now();
        Some(RowPayloadBatch {
            generation: self.current_generation,
            folder_id: self.current_folder_id,
            position: 0,
            n_removals: 0,
            rows,
        })
    }
}

/// Builds row payloads completely off the GTK thread.
/// Returns the payload vector directly without requiring a glib::Sender.
pub fn build_rows_off_thread(messages: &[crate::models::Message]) -> Vec<RowPayload> {
    messages
        .iter()
        .map(|m| {
            let initials = {
                let parts: Vec<&str> = m.from_name.trim().split_whitespace().collect();
                if parts.len() >= 2 {
                    let first = parts[0].chars().next().unwrap_or('?');
                    let second = parts[1].chars().next().unwrap_or(' ');
                    format!("{}{}", first, second).to_uppercase()
                } else {
                    m.from_name.chars().take(2).collect::<String>().to_uppercase()
                }
            };
            RowPayload {
                id: m.id,
                account_id: m.account_id,
                folder_id: m.folder_id,
                uid: m.uid,
                from_name: m.from_name.clone(),
                from_addr: m.from_addr.clone(),
                subject: m.subject.clone(),
                preview: m.preview.clone(),
                date_display: m.date.clone(),
                unread: m.unread,
                starred: m.starred,
                has_attachment: m.has_attachment,
                initials,
            }
        })
        .collect()
}
