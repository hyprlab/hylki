//! The JMAP handlers for requests that read: folder lists, bodies,
//! attachments, searches, quota, keywords, the refresh tick and reconnecting.

use std::collections::BTreeMap;

use super::JmapWorker;
use crate::i18n::{i18n, i18n_f};
use crate::models::KeywordFinding;
use crate::worker::ids::hash_uid;
use crate::worker::index_queue::IndexQueue;
use crate::worker::jmap::chores::{auto_empty_jmap, clean_lists_jmap, emit_jmap_body_hits};
use crate::worker::jmap::folders::{jmap_refresh_unread, refresh_jmap_folders};
use crate::worker::jmap::index::{jmap_load_folder, jmap_sync_folder};
use crate::worker::jmap::messages::{jmap_get_messages, jmap_query_ids};
use crate::worker::jmap::ops::jmap_fetch_raw;
use crate::worker::jmap::push::{jmap_deliver_body, serve_cached};
use crate::worker::jmap::wire::{
    args, call, jmap_call, jmap_session, jmap_storage_quota, CAP_CORE, CAP_MAIL, CAP_QUOTA,
};
use crate::worker::render::attachments::extract_attachments;
use crate::worker::{blocking, WorkerEvent};

impl<E: Fn(WorkerEvent)> JmapWorker<E> {
    /// Find a message by its Message-ID: the cache, then a header filter on the server.
    pub(super) async fn on_locate(&mut self, message_id: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        // The cache first, as on IMAP: every folder listing went
        // through it. Then a header filter on the server, which finds
        // the message wherever it is filed on a server that indexes
        // Message-ID (Stalwart's full-text index does not).
        let mut hit = cache
            .as_ref()
            .map(|c| c.locate_by_message_id(&message_id))
            .unwrap_or_default()
            .into_iter()
            .find(|(a, _, _)| *a == account_id)
            .map(|(_, path, uid)| (path, uid));
        if hit.is_none() {
            if let Some(s) = jmap_session(account, state, emit).await {
                let sess = s.clone();
                let mid = message_id.clone();
                let found = blocking(move || {
                    let ids = jmap_query_ids(
                        &sess,
                        serde_json::json!({ "header": ["Message-ID", mid] }),
                        5,
                    )?;
                    jmap_get_messages(&sess, &ids, 0, 0)
                        .map(|got| got.into_iter().collect::<Vec<_>>())
                })
                .await;
                if let Ok(got) = found {
                    // The listing carries the mailbox ids in the raw
                    // object, which the summary drops; ask again for
                    // just those.
                    let ids: Vec<String> = got.iter().map(|(_, id, _)| id.clone()).collect();
                    let sess = s.clone();
                    let boxes = blocking(move || {
                    let r = jmap_call(
                        &sess,
                        &[CAP_CORE, CAP_MAIL],
                        vec![call(0, "Email/get", serde_json::json!({
                            "accountId": sess.account, "ids": ids, "properties": ["id", "blobId", "mailboxIds"]
                        }))],
                    )?;
                    Ok(args(&r, 0)["list"].as_array().cloned().unwrap_or_default())
                })
                .await
                .unwrap_or_default();
                    'outer: for e in boxes {
                        let Some(id) = e["id"].as_str() else { continue };
                        let blob = e["blobId"].as_str().unwrap_or("").to_string();
                        for (mbox, _) in e["mailboxIds"].as_object().into_iter().flatten() {
                            if let Some((_, path)) = state.path_of(mbox) {
                                let uid = hash_uid(id);
                                state.uids.insert(uid, (id.to_string(), blob.clone()));
                                hit = Some((path, uid));
                                break 'outer;
                            }
                        }
                    }
                }
            }
        }
        emit(WorkerEvent::Located { message_id, hit });
    }

    /// The mailbox size and its limit, where the server offers the quota capability.
    pub(super) async fn on_quota(&mut self) {
        let account_id = self.account_id;
        let account = &self.account;
        let state = &mut self.state;
        let emit = &self.emit;
        let mut quota = None;
        if let Some(s) = jmap_session(account, state, emit).await {
            if s.quota {
                let sess = s.clone();
                let r = blocking(move || {
                    jmap_call(
                        &sess,
                        &[CAP_CORE, CAP_QUOTA],
                        vec![call(
                            0,
                            "Quota/get",
                            serde_json::json!({ "accountId": sess.account, "ids": null }),
                        )],
                    )
                })
                .await;
                match r {
                    Ok(responses) => quota = jmap_storage_quota(&args(&responses, 0)["list"]),
                    Err(e) => tracing::warn!("[account {account_id}] quota: {e}"),
                }
            }
        }
        emit(WorkerEvent::Quota(quota));
    }

    /// How many messages a mailbox holds (#676): its `totalEmails`.
    pub(super) async fn on_count_folder(&mut self, path: String) {
        let account_id = self.account_id;
        let state = &mut self.state;
        let emit = &self.emit;
        let mut count = None;
        if let (Some(s), Some((_, mailbox_id))) = (
            jmap_session(&self.account, state, emit).await,
            state.folders.get(&path).cloned(),
        ) {
            let sess = s.clone();
            let r = blocking(move || {
                jmap_call(
                    &sess,
                    &[CAP_CORE, CAP_MAIL],
                    vec![call(
                        0,
                        "Mailbox/get",
                        serde_json::json!({
                            "accountId": sess.account,
                            "ids": [mailbox_id],
                            "properties": ["totalEmails"],
                        }),
                    )],
                )
            })
            .await;
            match r {
                Ok(responses) => {
                    count = args(&responses, 0)["list"][0]["totalEmails"]
                        .as_u64()
                        .map(|n| n.min(u64::from(u32::MAX)) as u32);
                }
                Err(e) => tracing::warn!("[account {account_id}] count {path}: {e}"),
            }
        }
        emit(WorkerEvent::FolderCounted { path, count });
    }

    /// The keywords in use across the account, for the tag finder.
    pub(super) async fn on_find_keywords(&mut self) {
        let account_id = self.account_id;
        let cache = &self.cache;
        let emit = &self.emit;
        // JMAP has no keyword listing; the cached rows are the
        // survey, as they are for the count on every path.
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        if let Some(c) = cache.as_ref() {
            for f in c.load_folders(account_id) {
                for m in c.load_messages(account_id, &f.path, f.id) {
                    for k in m.keywords {
                        *seen.entry(k).or_default() += 1;
                    }
                }
            }
        }
        let found = seen
            .into_iter()
            .map(|(keyword, count)| KeywordFinding {
                keyword,
                name: None,
                color: None,
                count,
                folders: Vec::new(),
            })
            .collect();
        emit(WorkerEvent::KeywordsFound(found));
    }

    /// Bring the cached keywords in step: every folder is listed again.
    pub(super) async fn on_refresh_keywords(&mut self) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        let mut paths = Vec::new();
        if let Some(s) = jmap_session(account, state, emit).await {
            let folders: Vec<(String, u32)> = state
                .folders
                .iter()
                .map(|(p, (id, _))| (p.clone(), *id))
                .collect();
            for (path, folder_id) in folders {
                if jmap_load_folder(&s, account_id, folder_id, &path, cache.as_ref(), state)
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
        let Some(s) = jmap_session(account, state, emit).await else {
            emit(WorkerEvent::BackfillDone { folder_id });
            emit(WorkerEvent::Status(String::new()));
            return;
        };
        match jmap_sync_folder(&s, account_id, folder_id, &path, cache.as_ref(), state).await {
            Ok((messages, complete)) => {
                let unread = state.chip_count(folder_id, &messages);
                emit_jmap_body_hits(&s, account, folder_id, &path, &messages, state, emit).await;
                emit(WorkerEvent::Messages {
                    folder_id,
                    messages,
                });
                emit(WorkerEvent::FolderUnread { folder_id, unread });
                // Complete when the first page held the whole folder,
                // or once the background index has run (#429).
                if state.index_after_load(&path, complete, user) {
                    emit(WorkerEvent::BackfillDone { folder_id });
                }
            }
            Err(e) => {
                emit(WorkerEvent::net_error(i18n_f(
                    "Could not fetch mail: {e}",
                    &[("e", &e)],
                )));
                emit(WorkerEvent::BackfillDone { folder_id });
            }
        }
        emit(WorkerEvent::Status(String::new()));
    }

    /// One message's body, from the cache or the blob.
    pub(super) async fn on_load_body(&mut self, message_id: u32, path: String, uid: u32) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        if serve_cached(cache.as_ref(), account_id, &path, uid, message_id, emit) {
            return;
        }
        let Some(s) = jmap_session(account, state, emit).await else {
            return;
        };
        jmap_deliver_body(
            &s,
            account_id,
            message_id,
            &path,
            uid,
            cache.as_ref(),
            state,
            emit,
        )
        .await;
    }

    /// A conversation's bodies, from the cache or the blobs.
    pub(super) async fn on_load_bodies(&mut self, items: Vec<(u32, u32)>, path: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        for (message_id, uid) in items {
            if serve_cached(cache.as_ref(), account_id, &path, uid, message_id, emit) {
                continue;
            }
            let Some(s) = jmap_session(account, state, emit).await else {
                break;
            };
            jmap_deliver_body(
                &s,
                account_id,
                message_id,
                &path,
                uid,
                cache.as_ref(),
                state,
                emit,
            )
            .await;
        }
    }

    /// A message's raw source for the source view.
    pub(super) async fn on_load_source(&mut self, message_id: u32, path: String, uid: u32) {
        let account = &self.account;
        let state = &mut self.state;
        let emit = &self.emit;
        let Some(s) = jmap_session(account, state, emit).await else {
            emit(WorkerEvent::Source {
                message_id,
                text: None,
            });
            return;
        };
        match jmap_fetch_raw(&s, state, &path, uid).await {
            Ok(raw) => emit(WorkerEvent::Source {
                message_id,
                text: Some(String::from_utf8_lossy(&raw).into_owned()),
            }),
            Err(e) => {
                emit(WorkerEvent::net_error(i18n_f(
                    "Could not load source: {e}",
                    &[("e", &e)],
                )));
                emit(WorkerEvent::Source {
                    message_id,
                    text: None,
                });
            }
        }
    }

    /// A message's attachments, from the cache or, when the user asked, the blob.
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
        let Some(s) = jmap_session(account, state, emit).await else {
            return;
        };
        match jmap_fetch_raw(&s, state, &path, uid).await {
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
                &[("e", &e)],
            ))),
        }
    }

    /// A message's raw bytes, for Save As or for the reader.
    pub(super) async fn on_export_raw(&mut self, token: u64, path: String, uid: u32) {
        let account = &self.account;
        let state = &mut self.state;
        let emit = &self.emit;
        let raw = match jmap_session(account, state, emit).await {
            Some(s) => jmap_fetch_raw(&s, state, &path, uid).await,
            None => Err(i18n("Could not reach the server")),
        };
        emit(WorkerEvent::RawExported { token, raw });
    }

    /// The refresh tick: unread counts, then the auto-empty and list clean-up.
    pub(super) async fn on_refresh_unread(&mut self) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let last_auto_empty = &mut self.last_auto_empty;
        let last_list_clean = &mut self.last_list_clean;
        let emit = &self.emit;
        if let Some(s) = jmap_session(account, state, &|_| {}).await {
            jmap_refresh_unread(&s, account_id, cache.as_ref(), state, emit).await;
            auto_empty_jmap(
                &s,
                account_id,
                account,
                cache.as_ref(),
                state,
                last_auto_empty,
                emit,
            )
            .await;
            clean_lists_jmap(
                &s,
                account_id,
                account,
                cache.as_ref(),
                state,
                last_list_clean,
                emit,
            )
            .await;
        }
        // The Sent folders whose count a refresh saw move (#675), push
        // included, listed again so a reply sent from another computer
        // joins its conversation.
        for (folder_id, path) in std::mem::take(&mut self.state.sent_due) {
            self.on_load_messages(folder_id, path, false).await;
        }
    }

    /// A fresh session resource and folder list.
    pub(super) async fn on_reconnect(&mut self) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        // A fresh session resource: the server may have moved, or
        // the credentials changed.
        state.session = None;
        if let Some(s) = jmap_session(account, state, emit).await {
            refresh_jmap_folders(&s, account_id, cache.as_ref(), state, emit).await;
        }
    }
}
