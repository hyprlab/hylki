/**
 * @file prefetch.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief Viewport-aware intent planner and power-aware background backfill limiter.
 */

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct ViewportWindow {
    pub first_visible_row: u32,
    pub last_visible_row: u32,
    pub total_rows: u32,
}

#[derive(Debug, Clone)]
pub struct PrefetchPlan {
    pub urgent_uids: Vec<u32>,
    pub speculative_uids: Vec<u32>,
}

pub struct IntentPlanner {
    hovered_row: Option<(u32, Instant)>,
    hover_threshold: Duration,
    low_power_mode: Arc<AtomicBool>,
}

impl IntentPlanner {
    pub fn new() -> Self {
        Self {
            hovered_row: None,
            hover_threshold: Duration::from_millis(300),
            low_power_mode: Arc::new(AtomicBool::new(false)),
        }
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
        &self,
        viewport: ViewportWindow,
        visible_uids: &[u32],
        cached_uids: &HashSet<u32>,
    ) -> PrefetchPlan {
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

        PrefetchPlan {
            urgent_uids: urgent,
            speculative_uids: speculative,
        }
    }
}

impl Default for IntentPlanner {
    fn default() -> Self {
        Self::new()
    }
}
