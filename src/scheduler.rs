/**
 * @file scheduler.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief Priority queue with dynamic aging, WorkClass, RequestIdentity coalescing,
 *        folder generation IDs, IMAP IDLE recovery, and budget semaphores.
 */

use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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
    pub generation: u64,
    pub uid: u32,
    pub kind: RequestKind,
}

impl RequestIdentity {
    pub fn new(account_id: u32, folder_id: u32, generation: u64, uid: u32, kind: RequestKind) -> Self {
        Self {
            account_id,
            folder_id,
            generation,
            uid,
            kind,
        }
    }

    pub fn legacy(account_id: u32, folder_id: u32, uid: u32, kind: RequestKind) -> Self {
        Self {
            account_id,
            folder_id,
            generation: 0,
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
    in_flight: Mutex<HashMap<RequestIdentity, watch::Sender<Option<Arc<Vec<u8>>>>>>,
    seq_counter: AtomicU64,
    current_generation: AtomicU64,
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
            current_generation: AtomicU64::new(1),
            network: Semaphore::new(4),
            decode: Semaphore::new(4),
            preview: Semaphore::new(2),
            artwork: Semaphore::new(2),
            render: Semaphore::new(2),
        }
    }

    pub fn bump_generation(&self) -> u64 {
        let next_gen = self.current_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.invalidate_older_generations(next_gen);
        next_gen
    }

    pub fn current_generation(&self) -> u64 {
        self.current_generation.load(Ordering::Acquire)
    }

    pub fn is_current_generation(&self, gen: u64) -> bool {
        gen >= self.current_generation.load(Ordering::Acquire)
    }

    pub fn invalidate_older_generations(&self, current: u64) {
        self.invalidate_stale(|identity| identity.generation < current);
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
        if let Some(tx) = in_flight.get(&identity) {
            let id = self.seq_counter.fetch_add(1, Ordering::Relaxed);
            let req = Arc::new(MailRequest {
                id,
                identity,
                class,
                cancel: CancelToken::new(),
                enqueued: Instant::now(),
            });
            return (req, Some(tx.subscribe()));
        }

        let id = self.seq_counter.fetch_add(1, Ordering::Relaxed);
        let req = Arc::new(MailRequest {
            id,
            identity: identity.clone(),
            class,
            cancel: CancelToken::new(),
            enqueued: Instant::now(),
        });

        let (tx, _rx) = watch::channel(None);
        in_flight.insert(identity, tx);

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
        let cur_gen = self.current_generation.load(Ordering::Acquire);
        while let Some(task) = queue.pop() {
            // Drop tasks from older obsolete folder generations
            if task.request.identity.generation != 0 && task.request.identity.generation < cur_gen {
                task.request.cancel.cancel();
                continue;
            }
            if !task.request.cancel.is_cancelled() {
                return Some(task.request);
            }
        }
        None
    }

    pub fn complete(&self, identity: &RequestIdentity, result: Arc<Vec<u8>>) {
        let mut in_flight = self.in_flight.lock().unwrap();
        if let Some(tx) = in_flight.remove(identity) {
            // Only publish to subscribers if the result is still relevant
            if identity.generation == 0 || identity.generation >= self.current_generation.load(Ordering::Acquire) {
                let _ = tx.send(Some(result));
            }
        }
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

/// Manages IMAP IDLE state, timeout detection, and bounded exponential backoff
pub struct IdleRecoveryManager {
    reconnect_attempts: AtomicU32,
    last_activity: Mutex<Instant>,
    timeout: Duration,
}

impl IdleRecoveryManager {
    pub const IDLE_DONE_TIMEOUT: Duration = Duration::from_secs(15);
    pub const MIN_BACKOFF: Duration = Duration::from_millis(1000);
    pub const MAX_BACKOFF: Duration = Duration::from_secs(60);

    pub fn new() -> Self {
        Self {
            reconnect_attempts: AtomicU32::new(0),
            last_activity: Mutex::new(Instant::now()),
            timeout: Self::IDLE_DONE_TIMEOUT,
        }
    }

    pub fn touch(&self) {
        let mut lock = self.last_activity.lock().unwrap();
        *lock = Instant::now();
        self.reconnect_attempts.store(0, Ordering::Relaxed);
    }

    pub fn is_timed_out(&self) -> bool {
        let lock = self.last_activity.lock().unwrap();
        lock.elapsed() > self.timeout
    }

    pub fn next_backoff(&self) -> Duration {
        let attempts = self.reconnect_attempts.fetch_add(1, Ordering::SeqCst);
        let factor = 2u64.saturating_pow(attempts.min(6));
        let base = Self::MIN_BACKOFF.as_millis() as u64 * factor;
        let capped = base.min(Self::MAX_BACKOFF.as_millis() as u64);
        let jitter = (capped / 10).min(1000);
        Duration::from_millis(capped + (attempts as u64 * 37 % (jitter + 1)))
    }

    pub fn reset(&self) {
        self.reconnect_attempts.store(0, Ordering::SeqCst);
        let mut lock = self.last_activity.lock().unwrap();
        *lock = Instant::now();
    }
}

impl Default for IdleRecoveryManager {
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
        if let Some(ref _m) = self.metadata {
            tracing::trace!(target: "telemetry::stage", stage = self.stage, meta = %_m, elapsed_us = elapsed.as_micros());
        } else {
            tracing::trace!(target: "telemetry::stage", stage = self.stage, elapsed_us = elapsed.as_micros());
        }
        elapsed
    }
}
