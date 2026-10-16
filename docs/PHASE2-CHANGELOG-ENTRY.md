### Phase 2: Async Architecture & Zero-Latency Performance Optimization

- **Dynamic Priority Scheduler (`src/scheduler.rs`)**:
  - Implemented aging priority queue preventing starvation of low-priority tasks (`AGING_MS = 500ms`, `MAX_AGING_BONUS = 100`).
  - Added cooperative `CancelToken` utilizing `AtomicBool` and `tokio::sync::Notify` for cancellation without race conditions.
  - Implemented `(message, kind)` request deduplication and coalescing backed by `tokio::sync::watch`.
  - Added system budget semaphores (`Network: 4`, `Decode: 4`, `Preview: 2`, `Artwork: 2`, `Render: 2`).

- **Bounded TTL & Negative Caching (`src/bounded_cache.rs`)**:
  - Introduced bounded LRU memory cache with TTL expiration.
  - Added negative caching for missing or expunged message UIDs to prevent repetitive socket lookups.

- **Lane-Based Connection Pool (`src/connection_pool.rs`)**:
  - Implemented isolated connection lanes (`Reader`, `Sync`, and `Speculative`) generic over session type `S`.
  - Dedicated reader lane guarantees zero-wait latency when opening messages while background sync is active.

- **Intent & Viewport Planner (`src/prefetch.rs`)**:
  - Added 300ms hover intent prefetching and scroll viewport lookahead.
  - Power-aware backfill throttling automatically disables speculative loading on battery or low-power modes.

- **Off-Thread Pre-Rendering & Row Payloads (`src/render_pipeline.rs`, `src/list_model.rs`)**:
  - Pre-renders warm HTML and plain text views off the UI open critical path.
  - Constructed row presentation payloads entirely off the GTK thread.

- **SQLite Throughput Optimizations (`src/cache_opt.rs`)**:
  - Configured WAL mode, 256MB memory-mapped I/O (`mmap_size`), and 64MB page cache.
  - Added dedicated readonly SQLite connection pool for non-blocking concurrent queries.

- **Stage Latency Telemetry (`tools/span_report.py`)**:
  - Tool to parse stage spans and output p50, p95, p99, and max latency benchmarks.

- **Compiler & Binary Size Optimization (`Cargo.toml`)**:
  - Upgraded release profile with Fat LTO (`lto = "fat"`) and `panic = "abort"` to strip unused landing pads and unwinding tables.
  - Eliminated glob wildcard imports in `src/worker.rs`, `src/ui/accounts.rs`, and `src/ui/preferences.rs` to accelerate symbol resolution.
  - Remediated deprecations in Chrono timestamp methods across `src/outlook_msg.rs` and `src/ui/pgp_keys.rs`.
