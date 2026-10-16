/**
 * @file prefetch.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief Viewport-aware intent planner, scrolling coalescer, and power-aware background backfill limiter.
 */

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewportWindow {
    pub first_visible_row: u32,
    pub last_visible_row: u32,
    pub total_rows: u32,
}

#[derive(Debug, Clone)]
pub struct PrefetchPlan {
    pub generation: u64,
    pub urgent_uids: Vec<u32>,
    pub speculative_uids: Vec<u32>,
}

/// Coalesces rapid scroll updates to prevent UI churn and excessive prefetching
pub struct ViewportCoalescer {
    last_window: Option<ViewportWindow>,
    last_update: Instant,
    debounce_duration: Duration,
    min_row_delta: u32,
}

impl ViewportCoalescer {
    pub fn new() -> Self {
        Self {
            last_window: None,
            last_update: Instant::now(),
            debounce_duration: Duration::from_millis(50),
            min_row_delta: 3,
        }
    }

    pub fn should_update(&mut self, current: ViewportWindow) -> bool {
        let now = Instant::now();
        if let Some(prev) = self.last_window {
            let row_delta = (current.first_visible_row as i32 - prev.first_visible_row as i32).unsigned_abs();
            if row_delta < self.min_row_delta && now.duration_since(self.last_update) < self.debounce_duration {
                return false;
            }
        }
        self.last_window = Some(current);
        self.last_update = now;
        true
    }

    pub fn reset(&mut self) {
        self.last_window = None;
        self.last_update = Instant::now();
    }
}

impl Default for ViewportCoalescer {
    fn default() -> Self {
        Self::new()
    }
}

pub struct IntentPlanner {
    hovered_row: Option<(u32, Instant)>,
    hover_threshold: Duration,
    low_power_mode: Arc<AtomicBool>,
    coalescer: ViewportCoalescer,
    current_generation: u64,
}

impl IntentPlanner {
    pub fn new() -> Self {
        Self {
            hovered_row: None,
            hover_threshold: Duration::from_millis(300),
            low_power_mode: Arc::new(AtomicBool::new(false)),
            coalescer: ViewportCoalescer::new(),
            current_generation: 1,
        }
    }

    pub fn set_generation(&mut self, gen: u64) {
        self.current_generation = gen;
        self.hovered_row = None;
        self.coalescer.reset();
    }

    pub fn set_low_power(&self, low: bool) {
        self.low_power_mode.store(low, Ordering::Release);
    }

    pub fn is_low_power(&self) -> bool {
        self.low_power_mode.load(Ordering::Acquire)
    }

    pub fn on_row_hover(&mut self, row_idx: u32) {
        self.hovered_row = Some((row_idx, Instant::now()));
    }

    pub fn on_row_unhover(&mut self) {
        self.hovered_row = None;
    }

    pub fn plan_viewport(
        &mut self,
        viewport: ViewportWindow,
        visible_uids: &[u32],
        cached_uids: &HashSet<u32>,
    ) -> Option<PrefetchPlan> {
        if !self.coalescer.should_update(viewport) {
            return None;
        }

        let mut urgent = Vec::new();
        let mut speculative = Vec::new();

        // Check if there is an active hover that exceeded threshold
        if let Some((row, start)) = self.hovered_row {
            if start.elapsed() >= self.hover_threshold {
                if let Some(&uid) = visible_uids.get(row as usize) {
                    if !cached_uids.contains(&uid) {
                        urgent.push(uid);
                    }
                }
            }
        }

        // Viewport visible rows: ensure all visible rows are cached
        let start = viewport.first_visible_row as usize;
        let end = (viewport.last_visible_row as usize + 1).min(visible_uids.len());

        for &uid in &visible_uids[start..end] {
            if !cached_uids.contains(&uid) && !urgent.contains(&uid) {
                urgent.push(uid);
            }
        }

        // Speculative lookahead: only if not in low power mode
        if !self.is_low_power() {
            let lookahead = 15;
            let forward_end = (end + lookahead).min(visible_uids.len());
            for &uid in &visible_uids[end..forward_end] {
                if !cached_uids.contains(&uid) {
                    speculative.push(uid);
                }
            }

            let backward_start = start.saturating_sub(5);
            for &uid in &visible_uids[backward_start..start] {
                if !cached_uids.contains(&uid) && !speculative.contains(&uid) {
                    speculative.push(uid);
                }
            }
        }

        Some(PrefetchPlan {
            generation: self.current_generation,
            urgent_uids: urgent,
            speculative_uids: speculative,
        })
    }
}

impl Default for IntentPlanner {
    fn default() -> Self {
        Self::new()
    }
}
