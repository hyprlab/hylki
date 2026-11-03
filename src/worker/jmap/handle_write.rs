//! The JMAP handlers for requests that change mail: keywords, moves,
//! deleting for good, mailboxes, imports and attachment removal.

use super::JmapWorker;
use crate::i18n::{i18n, i18n_f};
use crate::models::slashed_name;
use crate::worker::folders::restore_folders;
use crate::worker::ids::hash_uid;
use crate::worker::index_queue::IndexQueue;
use crate::worker::jmap::folders::{
    jmap_create_mailbox, jmap_ensure_mailbox, refresh_jmap_folders,
};
use crate::worker::jmap::index::jmap_load_folder;
use crate::worker::jmap::ops::{
    jmap_all_ids_in, jmap_destroy_all, jmap_import, jmap_move_uids, jmap_patch_all,
    jmap_patch_message, jmap_purge_uids, jmap_strip_attachment, jmap_undo_move,
};
use crate::worker::jmap::wire::{args, call, jmap_call, jmap_session, CAP_CORE, CAP_MAIL};
use crate::worker::strip::{cache_rewritten, strip_refused};
use crate::worker::{blocking, WorkerEvent};

impl<E: Fn(WorkerEvent)> JmapWorker<E> {
    /// Mark a message read or unread.
    pub(super) async fn on_set_seen(&mut self, path: String, uid: u32, seen: bool) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        if let Some(c) = cache.as_ref() {
            c.set_unread(account_id, &path, uid, !seen);
        }
        if let Some(s) = jmap_session(account, state, emit).await {
            let patch = serde_json::json!({ "keywords/$seen": if seen { serde_json::Value::Bool(true) } else { serde_json::Value::Null } });
            jmap_patch_message(&s, state, &path, uid, patch).await;
        }
        emit(WorkerEvent::SeenSettled { path, uid });
    }

    /// Flag a message or clear its flag.
    pub(super) async fn on_set_flagged(&mut self, path: String, uid: u32, flagged: bool) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        if let Some(c) = cache.as_ref() {
            c.set_starred(account_id, &path, uid, flagged);
        }
        if let Some(s) = jmap_session(account, state, emit).await {
            let patch = serde_json::json!({ "keywords/$flagged": if flagged { serde_json::Value::Bool(true) } else { serde_json::Value::Null } });
            jmap_patch_message(&s, state, &path, uid, patch).await;
        }
    }

    /// Put a keyword (tag) on a message or take it off.
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
        let emit = &self.emit;
        if let Some(c) = cache.as_ref() {
            c.set_keyword(account_id, &path, uid, &keyword, add);
        }
        if let Some(s) = jmap_session(account, state, emit).await {
            let patch = serde_json::json!({ format!("keywords/{keyword}"): if add { serde_json::Value::Bool(true) } else { serde_json::Value::Null } });
            jmap_patch_message(&s, state, &path, uid, patch).await;
        }
        emit(WorkerEvent::TagSettled { path, uid, keyword });
    }

    /// Mark every message in a folder read.
    pub(super) async fn on_mark_all_read(&mut self, folder_id: u32, path: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        if let Some(s) = jmap_session(account, state, emit).await {
            if let Some((_, mailbox_id)) = state.folders.get(&path).cloned() {
                let sess = s.clone();
                let r = blocking(move || {
                    let ids = jmap_all_ids_in(
                        &sess,
                        &mailbox_id,
                        serde_json::json!({ "notKeyword": "$seen" }),
                    )?;
                    jmap_patch_all(&sess, &ids, &serde_json::json!({ "keywords/$seen": true }))
                })
                .await;
                if let Err(e) = r {
                    tracing::warn!("jmap: mark all read in {path} failed: {e}");
                }
            }
        }
        if let Some(c) = cache.as_ref() {
            c.mark_folder_read(account_id, &path);
        }
        emit(WorkerEvent::FolderUnread {
            folder_id,
            unread: 0,
        });
    }

    /// Move one message to another mailbox.
    pub(super) async fn on_move_message(&mut self, path: String, uid: u32, dest: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        let Some(s) = jmap_session(account, state, emit).await else {
            return;
        };
        jmap_ensure_mailbox(&s, account_id, cache.as_ref(), state, emit, &dest).await;
        if let Err(e) = jmap_move_uids(
            &s,
            account_id,
            state,
            &path,
            &[uid],
            &dest,
            serde_json::json!({}),
            cache.as_ref(),
        )
        .await
        {
            emit(WorkerEvent::error(i18n_f(
                "Could not move message: {e}",
                &[("e", &e)],
            )));
        }
    }

    /// Move a message to Junk, marked as junk.
    pub(super) async fn on_mark_spam(&mut self, path: String, uid: u32, dest: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        let Some(s) = jmap_session(account, state, emit).await else {
            return;
        };
        let verdict = serde_json::json!({ "keywords/$junk": true, "keywords/$notjunk": null });
        if let Err(e) = jmap_move_uids(
            &s,
            account_id,
            state,
            &path,
            &[uid],
            &dest,
            verdict,
            cache.as_ref(),
        )
        .await
        {
            emit(WorkerEvent::error(i18n_f(
                "Could not mark as spam: {e}",
                &[("e", &e)],
            )));
        }
    }

    /// Move a message out of Junk, marked as not junk.
    pub(super) async fn on_mark_ham(&mut self, path: String, uid: u32, dest: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        let Some(s) = jmap_session(account, state, emit).await else {
            return;
        };
        let verdict = serde_json::json!({ "keywords/$junk": null, "keywords/$notjunk": true });
        if let Err(e) = jmap_move_uids(
            &s,
            account_id,
            state,
            &path,
            &[uid],
            &dest,
            verdict,
            cache.as_ref(),
        )
        .await
        {
            emit(WorkerEvent::error(i18n_f(
                "Could not mark as not spam: {e}",
                &[("e", &e)],
            )));
        }
    }

    /// Move several messages out of Junk, marked as not junk.
    pub(super) async fn on_mark_ham_many(&mut self, path: String, uids: Vec<u32>, dest: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        if let Some(s) = jmap_session(account, state, emit).await {
            let verdict = serde_json::json!({ "keywords/$junk": null, "keywords/$notjunk": true });
            if let Err(e) = jmap_move_uids(
                &s,
                account_id,
                state,
                &path,
                &uids,
                &dest,
                verdict,
                cache.as_ref(),
            )
            .await
            {
                emit(WorkerEvent::error(i18n_f(
                    "Could not mark {len} messages as not spam: {e}",
                    &[("len", &uids.len().to_string()), ("e", &e)],
                )));
            }
        }
        emit(WorkerEvent::BulkComplete);
    }

    /// Move several messages to another mailbox.
    pub(super) async fn on_move_messages(&mut self, path: String, uids: Vec<u32>, dest: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        if let Some(s) = jmap_session(account, state, emit).await {
            jmap_ensure_mailbox(&s, account_id, cache.as_ref(), state, emit, &dest).await;
            if let Err(e) = jmap_move_uids(
                &s,
                account_id,
                state,
                &path,
                &uids,
                &dest,
                serde_json::json!({}),
                cache.as_ref(),
            )
            .await
            {
                emit(WorkerEvent::error(i18n_f(
                    "Could not move messages: {e}",
                    &[("e", &e)],
                )));
            }
        }
        emit(WorkerEvent::BulkComplete);
    }

    /// Delete messages for good.
    pub(super) async fn on_purge_messages(&mut self, path: String, uids: Vec<u32>) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        if let Some(s) = jmap_session(account, state, emit).await {
            jmap_purge_uids(&s, account_id, state, &path, uids, cache.as_ref()).await;
        }
        emit(WorkerEvent::BulkComplete);
    }

    /// Delete everything in a mailbox for good.
    pub(super) async fn on_empty_folder(&mut self, folder_id: u32, path: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        let Some(s) = jmap_session(account, state, emit).await else {
            return;
        };
        let Some((_, mailbox_id)) = state.folders.get(&path).cloned() else {
            return;
        };
        let sess = s.clone();
        let r = blocking(move || {
            let ids = jmap_all_ids_in(&sess, &mailbox_id, serde_json::json!({}))?;
            jmap_destroy_all(&sess, &ids)?;
            Ok(ids)
        })
        .await;
        match r {
            Ok(ids) => {
                for id in &ids {
                    let uid = hash_uid(id);
                    state.uids.remove(&uid);
                    if let Some(c) = cache.as_ref() {
                        c.delete_message(account_id, &path, uid);
                    }
                }
                tracing::info!("emptied {path}: {} message(s) erased", ids.len());
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
                &[("e", &e)],
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
        let emit = &self.emit;
        let Some(s) = jmap_session(account, state, emit).await else {
            emit(WorkerEvent::BulkComplete);
            return;
        };
        match jmap_undo_move(
            &s,
            account_id,
            state,
            &path,
            &dest,
            &message_ids,
            serde_json::json!({}),
            cache.as_ref(),
        )
        .await
        {
            Ok(0) => {
                tracing::info!("undo: the messages are no longer where that move put them")
            }
            Ok(_) => {
                if let Ok(messages) =
                    jmap_load_folder(&s, account_id, dest_folder_id, &dest, cache.as_ref(), state)
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
            Err(e) => emit(WorkerEvent::error(i18n_f("Undo failed: {e}", &[("e", &e)]))),
        }
        // The app spins its busy indicator until an undo answers.
        emit(WorkerEvent::BulkComplete);
    }

    /// Create a mailbox. `path` is the app's: its last level the name, the
    /// rest a mailbox the last listing knows.
    pub(super) async fn on_create_folder(&mut self, path: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        let refused = |e: &str| {
            emit(WorkerEvent::error(i18n_f(
                "Could not create folder: {e}",
                &[("e", e)],
            )));
            restore_folders(cache.as_ref(), account_id, emit);
        };
        let Some(s) = jmap_session(account, state, emit).await else {
            restore_folders(cache.as_ref(), account_id, emit);
            return;
        };
        let (parent, leaf) = match path.rsplit_once('/') {
            Some((parent, leaf)) => match state.folders.get(parent) {
                Some((_, pid)) => (Some(pid.clone()), leaf),
                None => return refused(&i18n("the folder it goes in is gone")),
            },
            None => (None, path.as_str()),
        };
        let name = slashed_name(leaf);
        let sess = s.clone();
        let r = blocking(move || jmap_create_mailbox(&sess, &name, parent.as_deref())).await;
        match r {
            Ok(_) => refresh_jmap_folders(&s, account_id, cache.as_ref(), state, emit).await,
            Err(e) => refused(&e),
        }
    }

    /// Rename a mailbox, move it under another parent ("" in the path for
    /// the top), or both: one `Mailbox/set` either way.
    pub(super) async fn on_rename_folder(&mut self, old_path: String, new_path: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        let refused = |e: &str| {
            emit(WorkerEvent::error(i18n_f(
                "Could not rename folder: {e}",
                &[("e", e)],
            )));
            restore_folders(cache.as_ref(), account_id, emit);
        };
        let Some(s) = jmap_session(account, state, emit).await else {
            restore_folders(cache.as_ref(), account_id, emit);
            return;
        };
        let Some((_, id)) = state.folders.get(&old_path).cloned() else {
            return refused(&i18n("unknown folder"));
        };
        let split = |p: &str| match p.rsplit_once('/') {
            Some((parent, leaf)) => (parent.to_string(), leaf.to_string()),
            None => (String::new(), p.to_string()),
        };
        let (old_parent, old_leaf) = split(&old_path);
        let (new_parent, new_leaf) = split(&new_path);
        let mut update = serde_json::Map::new();
        if new_leaf != old_leaf {
            update.insert("name".into(), slashed_name(&new_leaf).into());
        }
        if new_parent != old_parent {
            let parent_id = if new_parent.is_empty() {
                serde_json::Value::Null
            } else {
                match state.folders.get(&new_parent) {
                    Some((_, pid)) => pid.clone().into(),
                    None => return refused(&i18n("the folder it goes in is gone")),
                }
            };
            update.insert("parentId".into(), parent_id);
        }
        if update.is_empty() {
            return;
        }
        let sess = s.clone();
        let r = blocking(move || {
            let r = jmap_call(
                &sess,
                &[CAP_CORE, CAP_MAIL],
                vec![call(
                    0,
                    "Mailbox/set",
                    serde_json::json!({
                        "accountId": sess.account,
                        "update": { id.clone(): update },
                    }),
                )],
            )?;
            let r = args(&r, 0);
            if r["updated"]
                .as_object()
                .is_some_and(|m| m.contains_key(&id))
            {
                Ok(())
            } else {
                Err(r["notUpdated"][&id]["description"]
                    .as_str()
                    .unwrap_or("the server refused")
                    .to_string())
            }
        })
        .await;
        match r {
            Ok(()) => refresh_jmap_folders(&s, account_id, cache.as_ref(), state, emit).await,
            Err(e) => refused(&e),
        }
    }

    /// Take a new list of hidden folders (#239) and list the mailboxes again.
    pub(super) async fn on_set_hidden_folders(&mut self, paths: Vec<String>) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        state.hidden = paths;
        let Some(s) = jmap_session(account, state, emit).await else {
            return;
        };
        refresh_jmap_folders(&s, account_id, cache.as_ref(), state, emit).await;
    }

    /// Delete a mailbox, its mail moved to `trash` first when given.
    pub(super) async fn on_delete_folder(&mut self, path: String, trash: Option<String>) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        let Some(s) = jmap_session(account, state, emit).await else {
            return;
        };
        let Some((_, id)) = state.folders.get(&path).cloned() else {
            emit(WorkerEvent::error(i18n(
                "Could not delete folder: unknown folder",
            )));
            return;
        };
        // The contents go to Trash first when the app names one, as
        // on IMAP; then the mailbox goes, with whatever is left.
        let trash_id = trash
            .as_ref()
            .and_then(|t| state.folders.get(t))
            .map(|(_, id)| id.clone());
        let sess = s.clone();
        let r = blocking(move || {
            if let Some(trash_id) = trash_id {
                let ids = jmap_all_ids_in(&sess, &id, serde_json::json!({}))?;
                jmap_patch_all(
                    &sess,
                    &ids,
                    &serde_json::json!({ "mailboxIds": { trash_id: true } }),
                )?;
            }
            let r = jmap_call(
                &sess,
                &[CAP_CORE, CAP_MAIL],
                vec![call(
                    0,
                    "Mailbox/set",
                    serde_json::json!({
                        "accountId": sess.account,
                        "destroy": [id.clone()],
                        "onDestroyRemoveEmails": true,
                    }),
                )],
            )?;
            let r = args(&r, 0);
            if r["destroyed"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(&id)))
            {
                Ok(())
            } else {
                Err(r["notDestroyed"][&id]["description"]
                    .as_str()
                    .unwrap_or("the server refused")
                    .to_string())
            }
        })
        .await;
        match r {
            Ok(()) => refresh_jmap_folders(&s, account_id, cache.as_ref(), state, emit).await,
            Err(e) => emit(WorkerEvent::error(i18n_f(
                "Could not delete folder: {e}",
                &[("e", &e)],
            ))),
        }
    }

    /// Take one attachment out of a message (#289).
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
        let emit = &self.emit;
        let Some(s) = jmap_session(account, state, emit).await else {
            strip_refused(
                emit,
                i18n("Could not reach the server"),
                message_id,
                name,
                size,
            );
            return;
        };
        emit(WorkerEvent::Status(i18n("Removing the attachment…")));
        let result = jmap_strip_attachment(&s, state, &path, uid, &name, size).await;
        emit(WorkerEvent::Status(String::new()));
        match result {
            Ok((new_id, raw)) => {
                let new_uid = hash_uid(&new_id);
                state.uids.remove(&uid);
                if let Some(c) = cache.as_ref() {
                    c.delete_message(account_id, &path, uid);
                }
                if let Some((folder_id, _)) = state.folders.get(&path).cloned() {
                    if let Ok(messages) =
                        jmap_load_folder(&s, account_id, folder_id, &path, cache.as_ref(), state)
                            .await
                    {
                        emit(WorkerEvent::Messages {
                            folder_id,
                            messages,
                        });
                    }
                }
                if let Some(c) = cache.as_ref() {
                    c.save_attachment_meta(account_id, &path, uid, &[]);
                    cache_rewritten(c, account_id, &path, new_uid, &raw);
                }
                emit(WorkerEvent::AttachmentDeleted {
                    message_id,
                    path,
                    uid,
                    new_uid: Some(new_uid),
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

    /// Import raw message bytes into a mailbox.
    pub(super) async fn on_import_raw(
        &mut self,
        token: u64,
        path: String,
        raw: Vec<u8>,
        seen: bool,
        flagged: bool,
    ) {
        let account = &self.account;
        let state = &mut self.state;
        let emit = &self.emit;
        let mailbox = state.folders.get(&path).map(|(_, id)| id.clone());
        let result = match (jmap_session(account, state, emit).await, mailbox) {
            (Some(s), Some(mailbox)) => {
                let mut keywords = serde_json::Map::new();
                if seen {
                    keywords.insert("$seen".into(), true.into());
                }
                if flagged {
                    keywords.insert("$flagged".into(), true.into());
                }
                blocking(move || {
                    jmap_import(&s, &raw, &mailbox, serde_json::Value::Object(keywords))
                })
                .await
                .map(|_| ())
            }
            (None, _) => Err(i18n("Could not reach the server")),
            (_, None) => Err(i18n("That folder is not on the server")),
        };
        emit(WorkerEvent::RawImported { token, result });
    }
}
