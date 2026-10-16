# Phase 2 Async Performance Run — Integration Guide

This document details the exact integration points in `src/worker.rs` and the core architecture for migrating to the priority queue scheduler, lane-based connection pool, and speculative prefetching.

---

## 1. Main Worker Loop Migration

Replace the ad-hoc polling loop in `src/worker.rs` with the priority queue iterator:

```rust
// Replace previous worker queue dispatch:
while let Some(req) = scheduler.next() {
    if req.cancel.is_cancelled() {
        continue;
    }
    
    let span = StageSpan::new("worker_task_execution");
    match req.class {
        WorkClass::ReaderInteractive => {
            // Prioritized interactive reader load
            process_reader_request(&req, &pool).await;
        }
        WorkClass::SyncLane => {
            // Background mailbox synchronization
            process_sync_request(&req, &pool).await;
        }
        WorkClass::SpeculativePrefetch => {
            // Prefetch viewport neighbors
            process_prefetch_request(&req, &pool).await;
        }
        WorkClass::BackgroundBackfill => {
            // Low-priority body caching
            process_backfill_request(&req, &pool).await;
        }
    }
    span.finish();
}
```

---

## 2. Invalidation Call Sites

Wire `scheduler.invalidate_stale(...)` into the following three critical state transitions:

1. **Folder Switch (`FolderSelected`)**:
   ```rust
   scheduler.invalidate_stale(|id| id.account_id == account_id && id.folder_id != target_folder_id);
   ```

2. **Account Disconnect/Reconnect (`NetworkLost`, `AccountDisabled`)**:
   ```rust
   scheduler.invalidate_stale(|id| id.account_id == disconnected_account_id);
   ```

3. **Message Deletion / Expiry (`MessageExpunged`)**:
   ```rust
   scheduler.invalidate_stale(|id| id.account_id == account_id && id.uid == expunged_uid);
   ```

---

## 3. Connection Lane Acquisition

Acquire isolated sessions from `ConnectionPool` based on the workload kind:

```rust
let slot = pool.acquire(match req.class {
    WorkClass::ReaderInteractive => LaneKind::Reader,
    WorkClass::SyncLane => LaneKind::Sync,
    WorkClass::SpeculativePrefetch | WorkClass::BackgroundBackfill => LaneKind::Speculative,
});
```

---

## 4. Deprecated Code Targets to DELETE

When applying this patch into `worker.rs`, safely delete:
1. `reorder_reader_loads`: replaced by dynamic `WorkClass` aging in `scheduler.rs`.
2. `can_yield_to_reader`: replaced by cooperative Tokio semaphores and cancel tokens.
3. `dead_uid_retry_loop`: replaced by `BoundedCache` negative hit caching.

---

## 5. WebKit and Capability Stubs

- `render_pipeline::WarmViews` and `record_first_paint` provide telemetry hooks to be wired directly into `WebView::connect_load_changed`.
- CONDSTORE and Microsoft Graph batch endpoints should leverage `BudgetKind::Network` semaphores.

---

## 6. GTK Main-Thread Ownership & Dispatching
Worker threads (Tokio async tasks and CPU decoding workers) must never touch GTK widgets, `gio::ListStore`, or `gtk::SelectionModel`.
All background results must be marshaled as pure-Rust payload structs (`RowPayloadBatch`, `WarmViews`) and dispatched to the GTK main context:
```rust
// In Tokio worker task:
let batch = RowPayloadBatch {
    generation,
    folder_id,
    position,
    n_removals,
    rows: build_rows_off_thread(&messages),
};

// Dispatch to GTK main thread:
glib::MainContext::default().spawn_local(async move {
    if scheduler.is_current_generation(batch.generation) {
        apply_row_payload_batch(batch);
    }
});
```

---

## 7. Folder-Generation IDs & Stale Result Filtering
Whenever the active folder changes:
1. Atomically bump generation: `let gen = scheduler.bump_generation();`
2. Attach `{account_id, folder_id, generation: gen, uid, kind}` to outgoing `RequestIdentity` tokens.
3. Automatically cancels all pending tasks from older generations and rejects stale results returning from in-flight transfers.

---

## 8. List-Model Notification Consistency & UI Storm Prevention
To avoid GTK list-view assertion panics and event storms during bulk sync:
1. **Accurate Splice Boundaries**:
   Use `SpliceSpec::new(pos, n_removals, n_additions)` with `gio::ListStore::splice()` to ensure notification indices and counts strictly equal underlying model changes.
2. **Batching**:
   Use `RowUpdateBatcher` to buffer rows into chunks of 50 items or a 32ms deadline.
3. **Selection Bypass**:
   Selected message open requests call `RowUpdateBatcher::bypass_immediate(row, gen)` to jump ahead of bulk folder loading.
4. **Row Recycling**:
   Disconnect old signal handlers with `SignalBindingRegistry::clear()` when GTK factory rebinds a recycled row widget.

---

## 9. IMAP IDLE Timeout Recovery & Concurrency Tuning
1. **Unanswered DONE Timeout**:
   If an IMAP `DONE` token receives no response within 15 seconds (`IdleRecoveryManager::IDLE_DONE_TIMEOUT`), mark slot as timed out, disconnect socket, and reclaim the lane.
2. **Bounded Exponential Backoff**:
   On connection drop, reconnect using `IdleRecoveryManager::next_backoff()` (`min: 1s, max: 60s, + jitter`).
3. **Concurrency Bounds**:
   Network operations are bounded by `BudgetKind::Network` (4 permits) and CPU decoding by `BudgetKind::Decode` (4 permits) and `BudgetKind::Render` (2 permits).
