//! The thread that keeps the search index up to date (see
//! `cache/search_index.rs`). It has its own cache connection and works in
//! small batches, resting between them, so a first run over a large mailbox
//! fills the index in the background while the app goes on as before. The
//! index is a convenience: until a message is indexed, search still finds it
//! by its list fields.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Messages indexed per transaction.
const BATCH: usize = 200;

/// Rest between batches while there is a backlog, so the workers' own
/// writes are never kept waiting long for the database.
const BATCH_PAUSE: Duration = Duration::from_millis(40);

/// Rest when there is nothing to do.
const IDLE_PAUSE: Duration = Duration::from_secs(3);

/// Attachments whose text is extracted per round.
const EXTRACT_BATCH: usize = 4;

static STARTED: AtomicBool = AtomicBool::new(false);

/// Start the indexer, once per process. Not for the demo, whose cache is in
/// memory: its sample mail is indexed where it is seeded.
pub fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new().name("hylki-search-index".into()).spawn(run);
    if let Err(e) = spawned {
        tracing::warn!("search index: no thread: {e}");
    }
}

fn run() {
    let cache = loop {
        match crate::cache::Cache::open() {
            Ok(c) => break c,
            Err(e) => {
                tracing::warn!("search index: cache unavailable: {e}");
                std::thread::sleep(Duration::from_secs(30));
            }
        }
    };
    let backlog = cache.index_backlog();
    if backlog > 0 {
        tracing::info!("search index: {backlog} messages to index");
    }
    let mut indexed = 0usize;
    loop {
        match cache.index_pending(BATCH) {
            Ok(0) => {}
            Ok(n) => {
                indexed += n;
                std::thread::sleep(BATCH_PAUSE);
                continue;
            }
            Err(e) => {
                // Busy (another writer held the lock past the timeout) or a
                // damaged index: try again later.
                tracing::debug!("search index: {e}");
                std::thread::sleep(IDLE_PAUSE);
                continue;
            }
        }
        if indexed > 0 {
            tracing::debug!("search index: {indexed} messages indexed");
            indexed = 0;
        }
        if extract_some(&cache) {
            continue;
        }
        std::thread::sleep(IDLE_PAUSE);
    }
}

/// Extract the text of a few stored attachments. `true` when there was any
/// to do, so the caller goes on without resting.
fn extract_some(cache: &crate::cache::Cache) -> bool {
    let pending = match cache.attachments_to_extract(EXTRACT_BATCH, crate::text_extract::MAX_INPUT_BYTES) {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!("search index: attachments: {e}");
            return false;
        }
    };
    if pending.is_empty() {
        return false;
    }
    let mut saved = false;
    for a in &pending {
        let (status, text) = if a.data.is_empty() {
            (crate::cache::ExtractStatus::Failed, String::new())
        } else {
            crate::text_extract::extract(&a.name, &a.data)
        };
        match cache.save_attachment_text(a, status, &text) {
            Ok(()) => saved = true,
            Err(e) => tracing::debug!("search index: attachment text not saved: {e}"),
        }
    }
    // Nothing could be written (a full disk, say): rest before trying again
    // rather than extract the same files over and over.
    saved
}
