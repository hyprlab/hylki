//! The Proton handlers for requests that read: folder lists, bodies,
//! attachments, the source, and the refresh tick.

use super::ProtonWorker;
use crate::i18n::{i18n, i18n_f};
use crate::worker::index_queue::IndexQueue;
use crate::worker::proton::index::emit_folder;
use crate::worker::proton::index::proton_sync_folder;
use crate::worker::proton::ops::{proton_fetch_raw, proton_poll};
use crate::worker::render::attachments::extract_attachments;
use crate::worker::render::body_cacheable;
use crate::worker::{render_raw, WorkerEvent};

impl<E: Fn(WorkerEvent)> ProtonWorker<E> {
    /// A folder's list (`LoadMessages`, or a quiet `SyncFolder` when not
    /// `user`): the cache first, then the newest page, then the index.
    pub(super) async fn on_load_messages(&mut self, folder_id: u32, path: String, user: bool) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
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
        match proton_sync_folder(
            account,
            account_id,
            folder_id,
            &path,
            cache.as_ref(),
            state,
            emit,
        )
        .await
        {
            Ok((messages, complete)) => {
                let folder_id = state.folders.get(&path).map_or(folder_id, |(id, _)| *id);
                emit_folder(folder_id, messages, state, emit);
                // Said at once when the index is complete, so the list's
                // "Loading more…" tail never waits on nothing (#218).
                if state.index_after_load(&path, complete, user) {
                    emit(WorkerEvent::BackfillDone { folder_id });
                }
            }
            Err(e) => {
                tracing::warn!("[account {account_id}] could not list {path}: {}", e.code);
                emit(WorkerEvent::net_error(i18n_f(
                    "Could not fetch mail: {e}",
                    &[("e", &e.message())],
                )));
                emit(WorkerEvent::BackfillDone { folder_id });
            }
        }
        emit(WorkerEvent::Status(String::new()));
    }

    /// Bodies, from the cache or decrypted by the helper.
    pub(super) async fn on_load_bodies(&mut self, items: Vec<(u32, u32)>, path: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        for (message_id, uid) in items {
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
                        path: path.clone(),
                        message_id,
                        check,
                    });
                }
                continue;
            }
            match proton_fetch_raw(account, account_id, state, &path, uid, cache.as_ref(), emit)
                .await
            {
                Ok(raw) => {
                    let (body, check, has_attachments) = render_raw(&raw);
                    if let Some(c) = cache.as_ref() {
                        if body_cacheable(&check) {
                            c.save_body(account_id, &path, uid, &body);
                        }
                        c.save_sender_check(account_id, &path, uid, &check);
                        c.set_has_attachment(account_id, &path, uid, has_attachments);
                    }
                    emit(WorkerEvent::Body {
                        message_id,
                        path: path.clone(),
                        body,
                    });
                    emit(WorkerEvent::SenderChecked {
                        path: path.clone(),
                        message_id,
                        check,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        "[account {account_id}] could not load message {uid} in {path}: {}",
                        e.code
                    );
                    emit(WorkerEvent::net_error(i18n_f(
                        "Could not load message: {e}",
                        &[("e", &e.message())],
                    )));
                }
            }
        }
    }

    /// A message's attachments, from the cache or, when the user asked,
    /// out of its decrypted MIME.
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
        let emit = &self.emit;
        if let Some(c) = cache.as_ref() {
            let items = c.load_attachments(account_id, &path, uid);
            if !items.is_empty() {
                emit(WorkerEvent::Attachments {
                    path,
                    message_id,
                    items,
                });
                return;
            }
        }
        if !download {
            emit(WorkerEvent::AttachmentsPending { path, message_id });
            return;
        }
        match proton_fetch_raw(account, account_id, state, &path, uid, cache.as_ref(), emit).await {
            Ok(raw) => {
                let items = extract_attachments(&raw);
                if let Some(c) = cache.as_ref() {
                    c.save_attachments(account_id, &path, uid, &items);
                }
                emit(WorkerEvent::Attachments {
                    path,
                    message_id,
                    items,
                });
            }
            Err(e) => {
                tracing::warn!(
                    "[account {account_id}] could not load the attachments of {uid} in {path}: {}",
                    e.code
                );
                emit(WorkerEvent::net_error(i18n_f(
                    "Could not load attachments: {e}",
                    &[("e", &e.message())],
                )));
            }
        }
    }

    /// A message's MIME, for View Source or for export (Ok) or a reason.
    pub(super) async fn raw_of(&mut self, path: &str, uid: u32) -> Result<Vec<u8>, String> {
        let account_id = self.account_id;
        let emit = &self.emit;
        proton_fetch_raw(
            &self.account,
            account_id,
            &mut self.state,
            path,
            uid,
            self.cache.as_ref(),
            emit,
        )
        .await
        .map_err(|e| {
            tracing::warn!(
                "[account {account_id}] could not read message {uid} in {path}: {}",
                e.code
            );
            e.message()
        })
    }

    /// The refresh tick: the poll, which brings the folder tree with every
    /// folder's unread count, the Inbox, and what changed since the last
    /// event.
    pub(super) async fn on_refresh_unread(&mut self) {
        let account_id = self.account_id;
        let emit = &self.emit;
        if let Err(e) = proton_poll(
            &self.account,
            account_id,
            self.cache.as_ref(),
            &mut self.state,
            emit,
        )
        .await
        {
            tracing::info!("[account {account_id}] refresh: {}", e.code);
        }
    }
}
