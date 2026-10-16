/**
 * @file connection_pool.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief Reader / Sync / Speculative connection pool lanes generic over session S.
 */

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LaneKind {
    Reader,
    Sync,
    Speculative,
}

pub struct LaneSlot<S> {
    pub lane: LaneKind,
    pub session: Option<S>,
    pub last_used: Instant,
}

impl<S> LaneSlot<S> {
    pub fn new(lane: LaneKind, session: S) -> Self {
        Self {
            lane,
            session: Some(session),
            last_used: Instant::now(),
        }
    }

    pub fn take_session(&mut self) -> Option<S> {
        self.session.take()
    }

    pub fn put_session(&mut self, session: S) {
        self.session = Some(session);
        self.last_used = Instant::now();
    }
}

pub struct ConnectionPool<S> {
    reader_lane: Mutex<VecDeque<LaneSlot<S>>>,
    sync_lane: Mutex<VecDeque<LaneSlot<S>>>,
    speculative_lane: Mutex<VecDeque<LaneSlot<S>>>,
    max_per_lane: usize,
}

impl<S> ConnectionPool<S> {
    pub fn new(max_per_lane: usize) -> Self {
        Self {
            reader_lane: Mutex::new(VecDeque::new()),
            sync_lane: Mutex::new(VecDeque::new()),
            speculative_lane: Mutex::new(VecDeque::new()),
            max_per_lane,
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
}

impl<S> Default for ConnectionPool<S> {
    fn default() -> Self {
        Self::new(2)
    }
}
