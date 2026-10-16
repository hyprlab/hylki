/**
 * @file runtime.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief Global Tokio multi-thread runtime instance for background worker execution.
 */

use std::sync::{Arc, OnceLock};
use tokio::runtime::{Builder, Runtime};

static RUNTIME: OnceLock<Arc<Runtime>> = OnceLock::new();

pub fn runtime() -> &'static Arc<Runtime> {
    RUNTIME.get_or_init(|| {
        let rt = Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("hylki-worker")
            .enable_all()
            .build()
            .expect("Failed to initialize background Tokio runtime");
        Arc::new(rt)
    })
}

/// Spawns a future on the global worker Tokio runtime.
pub fn spawn<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime().spawn(future)
}

/// Returns a reference to the global worker Tokio runtime handle.
pub fn handle() -> tokio::runtime::Handle {
    runtime().handle().clone()
}
