/**
 * @file connection_pool.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief Reader / Sync / Speculative connection pool lanes generic over session S,
 *        with IMAP IDLE recovery and timeout isolation.
 */

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LaneKind {
    Reader,
    Sync,
    Speculative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleState {
    Active,
    DoneSent(Instant),
    Reconnecting(Instant),
}

pub struct LaneSlot<S> {
    pub lane: LaneKind,
    pub session: Option<S>,
    pub last_used: Instant,
    pub idle_state: IdleState,
}

impl<S> LaneSlot<S> {
    pub fn new(lane: LaneKind, session: S) -> Self {
        Self {
            lane,
            session: Some(session),
            last_used: Instant::now(),
            idle_state: IdleState::Active,
        }
    }

    pub fn take_session(&mut self) -> Option<S> {
        self.session.take()
    }

    pub fn put_session(&mut self, session: S) {
        self.session = Some(session);
        self.last_used = Instant::now();
        self.idle_state = IdleState::Active;
    }

    pub fn mark_done_sent(&mut self) {
        self.idle_state = IdleState::DoneSent(Instant::now());
    }

    pub fn is_idle_timed_out(&self, timeout: Duration) -> bool {
        match self.idle_state {
            IdleState::DoneSent(t) => t.elapsed() > timeout,
            _ => false,
        }
    }
}

pub struct ConnectionPool<S> {
    reader_lane: Mutex<VecDeque<LaneSlot<S>>>,
    sync_lane: Mutex<VecDeque<LaneSlot<S>>>,
    speculative_lane: Mutex<VecDeque<LaneSlot<S>>>,
    max_per_lane: usize,
    idle_timeout: Duration,
}

impl<S> ConnectionPool<S> {
    pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(15);

    pub fn new(max_per_lane: usize) -> Self {
        Self {
            reader_lane: Mutex::new(VecDeque::new()),
            sync_lane: Mutex::new(VecDeque::new()),
            speculative_lane: Mutex::new(VecDeque::new()),
            max_per_lane,
            idle_timeout: Self::DEFAULT_IDLE_TIMEOUT,
        }
    }

    pub fn acquire(&self, lane: LaneKind) -> Option<LaneSlot<S>> {
        let mut queue = match lane {
            LaneKind::Reader => self.reader_lane.lock().unwrap(),
            LaneKind::Sync => self.sync_lane.lock().unwrap(),
            LaneKind::Speculative => self.speculative_lane.lock().unwrap(),
        };
        queue.pop_front()
    }

    pub fn park(&self, slot: LaneSlot<S>) {
        let mut queue = match slot.lane {
            LaneKind::Reader => self.reader_lane.lock().unwrap(),
            LaneKind::Sync => self.sync_lane.lock().unwrap(),
            LaneKind::Speculative => self.speculative_lane.lock().unwrap(),
        };
        if queue.len() < self.max_per_lane {
            queue.push_back(slot);
        }
    }

    pub fn available_count(&self, lane: LaneKind) -> usize {
        match lane {
            LaneKind::Reader => self.reader_lane.lock().unwrap().len(),
            LaneKind::Sync => self.sync_lane.lock().unwrap().len(),
            LaneKind::Speculative => self.speculative_lane.lock().unwrap().len(),
        }
    }

    pub fn reclaim_timed_out_slots(&self) -> Vec<LaneSlot<S>> {
        let mut reclaimed = Vec::new();
        let mut sync = self.sync_lane.lock().unwrap();
        let mut retained = VecDeque::new();
        while let Some(slot) = sync.pop_front() {
            if slot.is_idle_timed_out(self.idle_timeout) {
                reclaimed.push(slot);
            } else {
                retained.push_back(slot);
            }
        }
        *sync = retained;
        reclaimed
    }
}

impl<S> Default for ConnectionPool<S> {
    fn default() -> Self {
        Self::new(2)
    }
}
