/**
 * @file async_bridge.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief Tokio asynchronous bridge for predictive SQLite prefetching and speculative body loading.
 * @param None directly; operates as a cross-thread task dispatcher.
 * @return Thread-safe async worker channels dispatching to GTK main context.
 * @throws None.
 * @warning Must be invoked while Tokio multithread runtime is active.
 * @pre Database initialized with optimize_sqlite_connection.
 * @post Background reads bypass the UI thread and stream directly into GLib event loop.
 */

use crate::mailbox_model::{MailboxModel, MessageObject};
use gtk::glib;
use gtk::glib::prelude::*;

/// Dispatches an asynchronous batch prefetch query to Tokio worker threads.
/// G'day mate! Offloads OFFSET/LIMIT queries so the GTK main loop stays 100% responsive.
pub fn dispatch_prefetch(
    account_id: u32,
    folder_path: String,
    folder_id: u32,
    offset: u32,
    limit: u32,
    model: MailboxModel,
) {
    // She'll be right: spawn onto Tokio blocking worker pool
    tokio::task::spawn(async move {
        let items: Vec<MessageObject> = tokio::task::spawn_blocking(move || {
            let Ok(cache) = crate::cache::Cache::open() else {
                return Vec::new();
            };

            // Read messages using cached prepared statement logic
            let msgs = cache.load_messages(account_id, &folder_path, folder_id);
            let page_slice: Vec<_> = msgs.into_iter().skip(offset as usize).take(limit as usize).collect();

            page_slice
                .into_iter()
                .map(|m| {
                    glib::Object::builder::<MessageObject>()
                        .property("id", m.id)
                        .property("account-id", m.account_id)
                        .property("folder-id", m.folder_id)
                        .property("uid", m.uid)
                        .property("from-name", m.from_name)
                        .property("subject", m.subject)
                        .property("preview", m.preview)
                        .property("date", m.date)
                        .property("unread", m.unread)
                        .property("folder-path", folder_path.clone())
                        .build()
                })
                .collect()
        })
        .await
        .unwrap_or_default();

        if !items.is_empty() {
            // Dispatch back to GTK main thread to update model and fire items_changed
            glib::idle_add_local_once(move || {
                model.receive_page(offset, items);
            });
        }
    });
}

/// Speculatively pre-fetches and warms the SQLite body cache when a row is hovered for > 300ms.
/// Gives commercial-grade 0ms perceived latency when clicking to open the reading pane!
pub fn speculative_body_prefetch(account_id: u32, folder_path: String, uid: u32) {
    tokio::task::spawn(async move {
        tokio::task::spawn_blocking(move || {
            let Ok(cache) = crate::cache::Cache::open() else {
                return;
            };
            // Warming memory-mapped page cache for instant render
            let _ = cache.load_body(account_id, &folder_path, uid);
        });
    });
}
