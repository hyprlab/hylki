/**
 * @file scheduler.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief Priority queue with dynamic aging, WorkClass, RequestIdentity coalescing, and budget semaphores.
 */

use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{watch, Semaphore, SemaphorePermit};

pub const AGING_MS: u64 = 500;
pub const MAX_AGING_BONUS: u32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WorkClass {
    BackgroundBackfill,
    SpeculativePrefetch,
    SyncLane,
    ReaderInteractive,
}

impl WorkClass {
    pub(crate) fn score(&self) -> u32 {
        match self {
            WorkClass::BackgroundBackfill => 10,
            WorkClass::SpeculativePrefetch => 40,
            WorkClass::SyncLane => 70,
            WorkClass::ReaderInteractive => 100,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RequestKind {
    Headers,
    Envelope,
    Body,
    Attachment(String),
    Flags,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RequestIdentity {
    pub account_id: u32,
    pub folder_id: u32,
    pub uid: u32,
    pub kind: RequestKind,
}

impl RequestIdentity {
    pub fn new(account_id: u32, folder_id: u32, uid: u32, kind: RequestKind) -> Self {
        Self {
            account_id,
            folder_id,
            uid,
            kind,
        }
    }
}

#[derive(Clone)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        self.notify.notified().await;
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

pub struct MailRequest {
    pub id: u64,
    pub identity: RequestIdentity,
    pub class: WorkClass,
    pub cancel: CancelToken,
    pub enqueued: Instant,
}

struct ScheduledTask {
    request: Arc<MailRequest>,
    seq: u64,
    enqueued: Instant,
}

impl PartialEq for ScheduledTask {
    fn eq(&self, other: &Self) -> bool {
        self.seq == other.seq
    }
}

impl Eq for ScheduledTask {}

impl PartialOrd for ScheduledTask {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScheduledTask {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let bonus = |t: &ScheduledTask| {
            ((t.enqueued.elapsed().as_millis() as u64 / AGING_MS) as u32).min(MAX_AGING_BONUS)
        };
        let a = self.request.class.score() + bonus(self);
        let b = other.request.class.score() + bonus(other);
        // Higher score wins; on a tie the earlier submission wins.
        a.cmp(&b).then_with(|| other.seq.cmp(&self.seq))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BudgetKind {
    Network,
    Decode,
    Preview,
    Artwork,
    Render,
}

pub struct SystemScheduler {
    queue: Mutex<BinaryHeap<ScheduledTask>>,
    in_flight: Mutex<HashMap<RequestIdentity, watch::Receiver<Option<Arc<Vec<u8>>>>>>,
    seq_counter: AtomicU64,
    network: Semaphore,
    decode: Semaphore,
    preview: Semaphore,
    artwork: Semaphore,
    render: Semaphore,
}

impl SystemScheduler {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(BinaryHeap::new()),
            in_flight: Mutex::new(HashMap::new()),
            seq_counter: AtomicU64::new(1),
            network: Semaphore::new(4),
            decode: Semaphore::new(4),
            preview: Semaphore::new(2),
            artwork: Semaphore::new(2),
            render: Semaphore::new(2),
        }
    }

    pub async fn budget(&self, kind: BudgetKind) -> SemaphorePermit<'_> {
        let sem = match kind {
            BudgetKind::Network => &self.network,
            BudgetKind::Decode => &self.decode,
            BudgetKind::Preview => &self.preview,
            BudgetKind::Artwork => &self.artwork,
            BudgetKind::Render => &self.render,
        };
        sem.acquire().await.expect("budget semaphore closed")
    }

    pub fn submit(
        &self,
        identity: RequestIdentity,
        class: WorkClass,
    ) -> (Arc<MailRequest>, Option<watch::Receiver<Option<Arc<Vec<u8>>>>>) {
        let mut in_flight = self.in_flight.lock().unwrap();
        if let Some(rx) = in_flight.get(&identity) {
            // Coalescing: late joiners subscribe via watch channel
            let id = self.seq_counter.fetch_add(1, Ordering::Relaxed);
            let req = Arc::new(MailRequest {
                id,
                identity,
                class,
                cancel: CancelToken::new(),
                enqueued: Instant::now(),
            });
            return (req, Some(rx.clone()));
        }

        let id = self.seq_counter.fetch_add(1, Ordering::Relaxed);
        let req = Arc::new(MailRequest {
            id,
            identity: identity.clone(),
            class,
            cancel: CancelToken::new(),
            enqueued: Instant::now(),
        });

        let (tx, rx) = watch::channel(None);
        in_flight.insert(identity, rx);

        let task = ScheduledTask {
            request: req.clone(),
            seq: id,
            enqueued: Instant::now(),
        };

        self.queue.lock().unwrap().push(task);
        (req, None)
    }

    pub fn next(&self) -> Option<Arc<MailRequest>> {
        let mut queue = self.queue.lock().unwrap();
        while let Some(task) = queue.pop() {
            if !task.request.cancel.is_cancelled() {
                return Some(task.request);
            }
        }
        None
    }

    pub fn complete(&self, identity: &RequestIdentity, result: Arc<Vec<u8>>) {
        let mut in_flight = self.in_flight.lock().unwrap();
        in_flight.remove(identity);
    }

    pub fn invalidate_stale<F>(&self, predicate: F)
    where
        F: Fn(&RequestIdentity) -> bool,
    {
        let queue = self.queue.lock().unwrap();
        for task in queue.iter() {
            if predicate(&task.request.identity) {
                task.request.cancel.cancel();
            }
        }
    }
}

impl Default for SystemScheduler {
    fn default() -> Self {
        Self::new()
    }
}

pub struct StageSpan {
    pub stage: &'static str,
    pub start: Instant,
    pub metadata: Option<String>,
}

impl StageSpan {
    pub fn new(stage: &'static str) -> Self {
        Self {
            stage,
            start: Instant::now(),
            metadata: None,
        }
    }

    pub fn with_meta(stage: &'static str, meta: impl Into<String>) -> Self {
        Self {
            stage,
            start: Instant::now(),
            metadata: Some(meta.into()),
        }
    }

    pub fn finish(self) -> Duration {
        let elapsed = self.start.elapsed();
        if let Some(ref m) = self.metadata {
            tracing::trace!(target: "telemetry::stage", stage = self.stage, meta = %m, elapsed_us = elapsed.as_micros());
        } else {
            tracing::trace!(target: "telemetry::stage", stage = self.stage, elapsed_us = elapsed.as_micros());
        }
        elapsed
    }
}
