//! The Graph handlers for requests that change mail: flags, categories,
//! moves, deleting for good, folders, and attachment removal.

use super::{name_labels, GraphWorker};
use crate::i18n::{i18n, i18n_f};
use crate::worker::graph::folders::{graph_ensure_folder, refresh_graph_folders};
use crate::worker::graph::http::{graph_delete_req, graph_send_json, graph_token, GRAPH_BASE};
use crate::worker::graph::index::graph_load_folder;
use crate::worker::graph::messages::graph_list_ids;
use crate::worker::graph::ops::{
    graph_delete_attachment, graph_fetch_raw, graph_move_uids, graph_patch_message,
    graph_purge_uids, graph_undo_move,
};
use crate::worker::ids::hash_uid;
use crate::worker::index_queue::IndexQueue;
use crate::worker::strip::{cache_rewritten, strip_refused};
use crate::worker::{blocking, WorkerEvent};

impl<E: Fn(WorkerEvent)> GraphWorker<E> {
    /// Mark a message read or unread.
    pub(super) async fn on_set_seen(&mut self, path: String, uid: u32, seen: bool) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        if let Some(c) = cache.as_ref() {
            c.set_unread(account_id, &path, uid, !seen);
        }
        graph_patch_message(
            account,
            state,
            &path,
            uid,
            serde_json::json!({ "isRead": seen }),
            emit,
        )
        .await;
        emit(WorkerEvent::SeenSettled { path, uid });
    }

    /// Flag a message or clear its flag.
    pub(super) async fn on_set_flagged(&mut self, path: String, uid: u32, flagged: bool) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        if let Some(c) = cache.as_ref() {
            c.set_starred(account_id, &path, uid, flagged);
        }
        let status = if flagged { "flagged" } else { "notFlagged" };
        graph_patch_message(
            account,
            state,
            &path,
            uid,
            serde_json::json!({ "flag": { "flagStatus": status } }),
            emit,
        )
        .await;
    }

    /// Put a category on a message or take it off.
    pub(super) async fn on_set_keyword(
        &mut self,
        path: String,
        uid: u32,
        keyword: String,
        add: bool,
    ) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        // Categories travel as the whole list, so the cached row is
        // the base: patched first, then sent as it now stands.
        let categories = match cache.as_ref() {
            Some(c) => {
                c.set_keyword(account_id, &path, uid, &keyword, add);
                c.keywords_of(account_id, &path, uid)
            }
            None if add => vec![keyword.clone()],
            None => Vec::new(),
        };
        graph_patch_message(
            account,
            state,
            &path,
            uid,
            serde_json::json!({ "categories": categories }),
            emit,
        )
        .await;
        emit(WorkerEvent::TagSettled { path, uid, keyword });
    }

    /// Mark every message in a folder read.
    pub(super) async fn on_mark_all_read(&mut self, folder_id: u32, path: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        // The server side is one PATCH per message; run it over the
        // cached unread rows (the listing window), then settle the cache.
        let unread_uids: Vec<u32> = cache
            .as_ref()
            .map(|c| {
                c.load_messages(account_id, &path, folder_id)
                    .into_iter()
                    .filter(|m| m.unread)
                    .map(|m| m.uid)
                    .collect()
            })
            .unwrap_or_default();
        for uid in unread_uids {
            graph_patch_message(
                account,
                state,
                &path,
                uid,
                serde_json::json!({ "isRead": true }),
                emit,
            )
            .await;
        }
        if let Some(c) = cache.as_ref() {
            c.mark_folder_read(account_id, &path);
        }
        emit(WorkerEvent::FolderUnread {
            folder_id,
            unread: 0,
        });
    }

    /// Move one message to another folder.
    pub(super) async fn on_move_message(&mut self, path: String, uid: u32, dest: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        graph_ensure_folder(account, account_id, cache.as_ref(), state, &dest, emit).await;
        if let Err(e) = graph_move_uids(
            account,
            account_id,
            state,
            &path,
            &[uid],
            &dest,
            cache.as_ref(),
        )
        .await
        {
            emit(WorkerEvent::error(i18n_f(
                "Could not move message: {e}",
                &[("e", &(e).to_string())],
            )));
        }
    }

    /// Move a message to Junk.
    pub(super) async fn on_mark_spam(&mut self, path: String, uid: u32, dest: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        if let Err(e) = graph_move_uids(
            account,
            account_id,
            state,
            &path,
            &[uid],
            &dest,
            cache.as_ref(),
        )
        .await
        {
            emit(WorkerEvent::error(i18n_f(
                "Could not mark as spam: {e}",
                &[("e", &(e).to_string())],
            )));
        }
    }

    /// Move a message out of Junk. Microsoft 365 learns from the move
    /// itself: back to the Inbox.
    pub(super) async fn on_mark_ham(&mut self, path: String, uid: u32, dest: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        if let Err(e) = graph_move_uids(
            account,
            account_id,
            state,
            &path,
            &[uid],
            &dest,
            cache.as_ref(),
        )
        .await
        {
            emit(WorkerEvent::error(i18n_f(
                "Could not mark as not spam: {e}",
                &[("e", &(e).to_string())],
            )));
        }
    }

    /// Move several messages out of Junk.
    pub(super) async fn on_mark_ham_many(&mut self, path: String, uids: Vec<u32>, dest: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        graph_ensure_folder(account, account_id, cache.as_ref(), state, &dest, emit).await;
        if let Err(e) = graph_move_uids(
            account,
            account_id,
            state,
            &path,
            &uids,
            &dest,
            cache.as_ref(),
        )
        .await
        {
            emit(WorkerEvent::error(i18n_f(
                "Could not mark {len} messages as not spam: {e}",
                &[("len", &(uids.len()).to_string()), ("e", &(e).to_string())],
            )));
        }
        emit(WorkerEvent::BulkComplete);
    }

    /// Move several messages to another folder.
    pub(super) async fn on_move_messages(&mut self, path: String, uids: Vec<u32>, dest: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        if let Err(e) = graph_move_uids(
            account,
            account_id,
            state,
            &path,
            &uids,
            &dest,
            cache.as_ref(),
        )
        .await
        {
            emit(WorkerEvent::error(i18n_f(
                "Could not move messages: {e}",
                &[("e", &(e).to_string())],
            )));
        }
        emit(WorkerEvent::BulkComplete);
    }

    /// Delete messages for good.
    pub(super) async fn on_purge_messages(&mut self, path: String, uids: Vec<u32>) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        graph_purge_uids(
            account,
            account_id,
            state,
            &path,
            uids,
            cache.as_ref(),
            emit,
        )
        .await;
        emit(WorkerEvent::BulkComplete);
    }

    /// Delete everything in a folder for good.
    pub(super) async fn on_empty_folder(&mut self, folder_id: u32, path: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        // No bulk endpoint for an arbitrary folder: list it, then
        // delete each message the way a purge does.
        let Some(token) = graph_token(account, emit).await else {
            return;
        };
        // Every message in it, not just the newest page or what the
        // cache has indexed so far (#429).
        let listed = match state.folders.get(&path).cloned() {
            Some((_, gid)) => {
                let (t, base) = (token.clone(), state.base.clone());
                blocking(move || graph_list_ids(&t, &base, &gid)).await
            }
            None => Err(format!("unknown folder {path}")),
        };
        match listed {
            Ok(ids) => {
                let uids: Vec<u32> = ids
                    .into_iter()
                    .map(|(id, _)| {
                        let uid = hash_uid(&id);
                        state.uids.insert(uid, id);
                        uid
                    })
                    .collect();
                let n = uids.len();
                graph_purge_uids(
                    account,
                    account_id,
                    state,
                    &path,
                    uids,
                    cache.as_ref(),
                    emit,
                )
                .await;
                tracing::info!("emptied {path}: {n} message(s) erased");
                emit(WorkerEvent::Messages {
                    folder_id,
                    messages: Vec::new(),
                });
                emit(WorkerEvent::FolderUnread {
                    folder_id,
                    unread: 0,
                });
            }
            Err(e) => emit(WorkerEvent::error(i18n_f(
                "Could not empty the folder: {e}",
                &[("e", &e.to_string())],
            ))),
        }
    }

    /// Put moved messages back.
    pub(super) async fn on_undo_move(
        &mut self,
        path: String,
        dest: String,
        dest_folder_id: u32,
        message_ids: Vec<String>,
    ) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        match graph_undo_move(
            account,
            account_id,
            state,
            &path,
            &dest,
            &message_ids,
            None,
            cache.as_ref(),
        )
        .await
        {
            Ok(0) => {
                tracing::info!("undo: the messages are no longer where that move put them");
            }
            Ok(_) => {
                // Reload the restored folder so the messages reappear.
                if let Some(token) = graph_token(account, emit).await {
                    if let Ok(messages) = graph_load_folder(
                        &token,
                        account_id,
                        dest_folder_id,
                        &dest,
                        cache.as_ref(),
                        state,
                    )
                    .await
                    {
                        let unread = state.chip_count(dest_folder_id, &messages);
                        emit(WorkerEvent::Messages {
                            folder_id: dest_folder_id,
                            messages,
                        });
                        emit(WorkerEvent::FolderUnread {
                            folder_id: dest_folder_id,
                            unread,
                        });
                    }
                }
            }
            Err(e) => emit(WorkerEvent::error(i18n_f(
                "Undo failed: {e}",
                &[("e", &(e).to_string())],
            ))),
        }
        // The app spins its busy indicator until an undo answers.
        emit(WorkerEvent::BulkComplete);
    }

    /// Create a folder.
    pub(super) async fn on_create_folder(&mut self, path: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        let Some(token) = graph_token(account, emit).await else {
            return;
        };
        // "A/B" nests under A (resolved from the last listing);
        // otherwise a top-level folder.
        let (url, name) = match path.rsplit_once('/') {
            Some((parent, leaf)) => match state.folders.get(parent) {
                Some((_, pgid)) => (
                    format!("{GRAPH_BASE}/me/mailFolders/{pgid}/childFolders"),
                    leaf.to_string(),
                ),
                None => (format!("{GRAPH_BASE}/me/mailFolders"), path.clone()),
            },
            None => (format!("{GRAPH_BASE}/me/mailFolders"), path.clone()),
        };
        let t = token.clone();
        let body = serde_json::json!({ "displayName": name });
        let r = blocking(move || graph_send_json(&t, "POST", &url, &body)).await;
        match r {
            Ok(_) => {
                refresh_graph_folders(&token, account_id, cache.as_ref(), state, emit).await;
            }
            Err(e) => emit(WorkerEvent::error(i18n_f(
                "Could not create folder: {e}",
                &[("e", &(e).to_string())],
            ))),
        }
    }

    /// Rename or move a folder.
    pub(super) async fn on_rename_folder(&mut self, old_path: String, new_path: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        let Some(token) = graph_token(account, emit).await else {
            return;
        };
        let Some((_, gid)) = state.folders.get(&old_path).cloned() else {
            emit(WorkerEvent::error(i18n(
                "Could not rename folder: unknown folder",
            )));
            return;
        };
        // Graph renames by displayName; moving between parents would be
        // a different call — the sidebar only renames leaves here.
        let leaf = new_path.rsplit('/').next().unwrap_or(&new_path).to_string();
        let t = token.clone();
        let url = format!("{GRAPH_BASE}/me/mailFolders/{gid}");
        let body = serde_json::json!({ "displayName": leaf });
        let r = blocking(move || graph_send_json(&t, "PATCH", &url, &body)).await;
        match r {
            Ok(_) => {
                refresh_graph_folders(&token, account_id, cache.as_ref(), state, emit).await;
            }
            Err(e) => emit(WorkerEvent::error(i18n_f(
                "Could not rename folder: {e}",
                &[("e", &(e).to_string())],
            ))),
        }
    }

    /// Take a new list of hidden folders (#239) and list the folders again.
    pub(super) async fn on_set_hidden_folders(&mut self, paths: Vec<String>) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        state.hidden = paths;
        let Some(token) = graph_token(account, emit).await else {
            return;
        };
        refresh_graph_folders(&token, account_id, cache.as_ref(), state, emit).await;
    }

    /// Delete a folder (Graph moves it to Deleted Items itself).
    pub(super) async fn on_delete_folder(&mut self, path: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        // Graph's folder delete moves the folder (contents included) to
        // Deleted Items itself; no separate content move needed.
        let Some(token) = graph_token(account, emit).await else {
            return;
        };
        let Some((_, gid)) = state.folders.get(&path).cloned() else {
            emit(WorkerEvent::error(i18n(
                "Could not delete folder: unknown folder",
            )));
            return;
        };
        let t = token.clone();
        let url = format!("{GRAPH_BASE}/me/mailFolders/{gid}");
        let r = blocking(move || graph_delete_req(&t, &url)).await;
        match r {
            Ok(()) => {
                refresh_graph_folders(&token, account_id, cache.as_ref(), state, emit).await;
            }
            Err(e) => emit(WorkerEvent::error(i18n_f(
                "Could not delete folder: {e}",
                &[("e", &(e).to_string())],
            ))),
        }
    }

    /// Take one attachment out of a message on the server (#289).
    /// Microsoft 365 deletes an attachment in place: the message
    /// keeps its id, and only what the cache holds of it changes.
    pub(super) async fn on_delete_attachment(
        &mut self,
        message_id: u32,
        path: String,
        uid: u32,
        name: String,
        size: u64,
    ) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        emit(WorkerEvent::Status(i18n("Removing the attachment…")));
        let result = graph_delete_attachment(account, state, &path, uid, &name, size, emit).await;
        emit(WorkerEvent::Status(String::new()));
        match result {
            Ok(()) => {
                if let Some(c) = cache.as_ref() {
                    c.delete_body(account_id, &path, uid);
                    match graph_fetch_raw(account, state, &path, uid, emit).await {
                        Ok(raw) => cache_rewritten(c, account_id, &path, uid, &raw),
                        // Fetched again at the next open.
                        Err(_) => {
                            c.save_attachments(account_id, &path, uid, &[]);
                            c.save_attachment_meta(account_id, &path, uid, &[]);
                        }
                    }
                }
                emit(WorkerEvent::AttachmentDeleted {
                    message_id,
                    path,
                    uid,
                    new_uid: Some(uid),
                    name,
                    size,
                });
            }
            Err(e) => strip_refused(
                emit,
                i18n_f("Could not remove the attachment: {e}", &[("e", &e)]),
                message_id,
                name,
                size,
            ),
        }
    }
}
