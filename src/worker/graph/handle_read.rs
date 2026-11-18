//! The Graph handlers for requests that read: folder lists, bodies,
//! attachments, categories and the refresh tick.

use super::{name_labels, GraphWorker};
use crate::i18n::{i18n, i18n_f};
use crate::models::KeywordFinding;
use crate::worker::graph::chores::{auto_empty_graph, emit_graph_body_hits, graph_preset_color};
use crate::worker::graph::folders::graph_refresh_unread;
use crate::worker::graph::http::{graph_get_json, graph_token, GRAPH_BASE};
use crate::worker::graph::index::{graph_load_folder, graph_sync_folder};
use crate::worker::graph::ops::graph_fetch_raw;
use crate::worker::index_queue::IndexQueue;
use crate::worker::render::attachments::extract_attachments;
use crate::worker::render::body_cacheable;
use crate::worker::{blocking, render_raw, WorkerEvent};

impl<E: Fn(WorkerEvent)> GraphWorker<E> {
    /// The categories defined for the mailbox, with the cached counts.
    pub(super) async fn on_find_keywords(&mut self) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        // Categories are defined once per mailbox, with a name and a
        // color: the master list is the whole answer. Counts come
        // from the cache, the only place Hylki has them.
        let mut found: Vec<KeywordFinding> = Vec::new();
        if let Some(token) = graph_token(account, emit).await {
            let t = token.clone();
            let cats = blocking(move || {
                graph_get_json(&t, &format!("{GRAPH_BASE}/me/outlook/masterCategories"))
            })
            .await;
            match cats {
                Ok(v) => {
                    for c in v["value"].as_array().into_iter().flatten() {
                        let Some(name) = c["displayName"].as_str() else {
                            continue;
                        };
                        let count = cache
                            .as_ref()
                            .map(|c| c.count_with_keyword(account_id, name))
                            .unwrap_or(0);
                        found.push(KeywordFinding {
                            keyword: name.to_string(),
                            name: Some(name.to_string()),
                            color: graph_preset_color(c["color"].as_str().unwrap_or("")),
                            count,
                            folders: Vec::new(),
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!("[account {account_id}] find tags: categories failed: {e}")
                }
            }
        }
        emit(WorkerEvent::KeywordsFound(found));
    }

    /// Bring the cached categories in step: every folder is listed again.
    pub(super) async fn on_refresh_keywords(&mut self) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        // Categories arrive with each folder's listing, so the
        // listing is the sync: every folder is re-read, which
        // rewrites its cached keywords (#166).
        let mut paths = Vec::new();
        if let Some(token) = graph_token(account, emit).await {
            let folders: Vec<(String, u32)> = state
                .folders
                .iter()
                .map(|(p, (id, _))| (p.clone(), *id))
                .collect();
            for (path, folder_id) in folders {
                if graph_load_folder(&token, account_id, folder_id, &path, cache.as_ref(), state)
                    .await
                    .is_ok()
                {
                    paths.push(path);
                }
            }
        }
        emit(WorkerEvent::KeywordsSynced { paths });
    }

    /// A folder's list (`LoadMessages`, or a quiet `SyncFolder` when not
    /// `user`): the cache first, then the newest page, then the index.
    pub(super) async fn on_load_messages(&mut self, folder_id: u32, path: String, user: bool) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        if let Some(c) = cache.as_ref() {
            let cached = c.load_messages(account_id, &path, folder_id);
            if !cached.is_empty() {
                emit(WorkerEvent::Messages {
                    folder_id,
                    messages: cached,
                });
            }
        }
        emit(WorkerEvent::Status(i18n("Syncing…")));
        let Some(token) = graph_token(account, emit).await else {
            // No token, so no mail is coming: end the folder's index
            // (#218) rather than leave the list spinning.
            emit(WorkerEvent::BackfillDone { folder_id });
            emit(WorkerEvent::Status(String::new()));
            return;
        };
        match graph_sync_folder(&token, account_id, folder_id, &path, cache.as_ref(), state).await {
            Ok((messages, complete)) => {
                let unread = state.chip_count(folder_id, &messages);
                emit_graph_body_hits(&token, account, folder_id, &path, &messages, state, emit)
                    .await;
                emit(WorkerEvent::Messages {
                    folder_id,
                    messages,
                });
                emit(WorkerEvent::FolderUnread { folder_id, unread });
                // The index is complete when the first page held the
                // whole folder, or once the background index has run
                // (#429). Said at once when it is, so the list's
                // "Loading more…" tail never waits on nothing (#218).
                if state.index_after_load(&path, complete, user) {
                    emit(WorkerEvent::BackfillDone { folder_id });
                }
            }
            Err(e) => {
                emit(WorkerEvent::net_error(i18n_f(
                    "Could not fetch mail: {e}",
                    &[("e", &(e).to_string())],
                )));
                emit(WorkerEvent::BackfillDone { folder_id });
            }
        }
        emit(WorkerEvent::Status(String::new()));
    }

    /// One message's body, from the cache or the raw MIME.
    pub(super) async fn on_load_body(&mut self, message_id: u32, path: String, uid: u32) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        if let Some(body) = cache
            .as_ref()
            .and_then(|c| c.load_body(account_id, &path, uid))
        {
            let check = cache
                .as_ref()
                .and_then(|c| c.load_sender_check(account_id, &path, uid));
            emit(WorkerEvent::Body {
                message_id,
                path: path.clone(),
                body,
            });
            if let Some(check) = check {
                emit(WorkerEvent::SenderChecked {
                    path: path.to_string(),
                    message_id,
                    check,
                });
            }
            return;
        }
        match graph_fetch_raw(account, state, &path, uid, emit).await {
            Ok(raw) => {
                let (body, check, _) = render_raw(&raw);
                if let Some(c) = cache.as_ref() {
                    if body_cacheable(&check) {
                        c.save_body(account_id, &path, uid, &body);
                    }
                    c.save_sender_check(account_id, &path, uid, &check);
                }
                emit(WorkerEvent::Body {
                    message_id,
                    path: path.clone(),
                    body,
                });
                emit(WorkerEvent::SenderChecked {
                    path: path.to_string(),
                    message_id,
                    check,
                });
            }
            Err(e) => emit(WorkerEvent::net_error(i18n_f(
                "Could not load message: {e}",
                &[("e", &(e).to_string())],
            ))),
        }
    }

    /// A conversation's bodies, from the cache or the raw MIME.
    pub(super) async fn on_load_bodies(&mut self, items: Vec<(u32, u32)>, path: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        for (message_id, uid) in items {
            if let Some(body) = cache
                .as_ref()
                .and_then(|c| c.load_body(account_id, &path, uid))
            {
                emit(WorkerEvent::Body {
                    message_id,
                    path: path.clone(),
                    body,
                });
                if let Some(check) = cache
                    .as_ref()
                    .and_then(|c| c.load_sender_check(account_id, &path, uid))
                {
                    emit(WorkerEvent::SenderChecked {
                        path: path.to_string(),
                        message_id,
                        check,
                    });
                }
                continue;
            }
            match graph_fetch_raw(account, state, &path, uid, emit).await {
                Ok(raw) => {
                    let (body, check, _) = render_raw(&raw);
                    if let Some(c) = cache.as_ref() {
                        if body_cacheable(&check) {
                            c.save_body(account_id, &path, uid, &body);
                        }
                        c.save_sender_check(account_id, &path, uid, &check);
                    }
                    emit(WorkerEvent::Body {
                        message_id,
                        path: path.clone(),
                        body,
                    });
                    emit(WorkerEvent::SenderChecked {
                        path: path.to_string(),
                        message_id,
                        check,
                    });
                }
                Err(e) => emit(WorkerEvent::net_error(i18n_f(
                    "Could not load message: {e}",
                    &[("e", &(e).to_string())],
                ))),
            }
        }
    }

    /// A message's attachments, from the cache or, when the user asked, the raw MIME.
    pub(super) async fn on_load_attachments(
        &mut self,
        message_id: u32,
        path: String,
        uid: u32,
        download: bool,
    ) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        if let Some(c) = cache.as_ref() {
            let items = c.load_attachments(account_id, &path, uid);
            if !items.is_empty() {
                emit(WorkerEvent::Attachments {
                    path: path.to_string(),
                    message_id,
                    items,
                });
                return;
            }
        }
        if !download {
            emit(WorkerEvent::AttachmentsPending {
                path: path.to_string(),
                message_id,
            });
            return;
        }
        match graph_fetch_raw(account, state, &path, uid, emit).await {
            Ok(raw) => {
                let items = extract_attachments(&raw);
                if let Some(c) = cache.as_ref() {
                    c.save_attachments(account_id, &path, uid, &items);
                }
                emit(WorkerEvent::Attachments {
                    path: path.to_string(),
                    message_id,
                    items,
                });
            }
            Err(e) => emit(WorkerEvent::net_error(i18n_f(
                "Could not load attachments: {e}",
                &[("e", &(e).to_string())],
            ))),
        }
    }

    /// The refresh tick: unread counts, then the auto-empty.
    pub(super) async fn on_refresh_unread(&mut self) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let last_auto_empty = &mut self.last_auto_empty;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        // Quiet token fetch: this rides the auto-fetch tick, and a
        // signed-out account already errors on interactive actions.
        let quiet = |_: WorkerEvent| {};
        if let Some(token) = graph_token(account, &quiet).await {
            graph_refresh_unread(&token, account_id, cache.as_ref(), state, emit).await;
            auto_empty_graph(
                &token,
                account_id,
                account,
                cache.as_ref(),
                state,
                last_auto_empty,
                emit,
            )
            .await;
        }
        // The Sent folders whose count a refresh saw move (#675), listed
        // again so a reply sent from another computer joins its
        // conversation.
        for (folder_id, path) in std::mem::take(&mut self.state.sent_due) {
            self.on_load_messages(folder_id, path, false).await;
        }
    }
}
