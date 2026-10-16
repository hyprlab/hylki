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
