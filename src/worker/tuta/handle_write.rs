//! The Tuta handlers for requests that change mail: read state, stars
//! (kept here only), moves, deleting for good, undo and snooze, and mail
//! taken in from another account.

use std::collections::HashMap;

use super::TutaWorker;
use crate::i18n::i18n_f;
use crate::models::{tuta_key_uid, FolderKind};
use crate::worker::helper::HelperError;
use crate::worker::ids::{hash_uid, merge_msgids};
use crate::worker::summary_from_raw;
use crate::worker::tuta::index::{run, tuta_load_folder};
use crate::worker::tuta::ops::{
    emit_folder, tuta_move_ids, tuta_move_uids, tuta_resolve, tuta_set_seen,
};
use crate::worker::tuta::wire::{delete_draft, delete_ids, import, sync};
use crate::worker::WorkerEvent;

impl<E: Fn(WorkerEvent)> TutaWorker<E> {
    /// Mark a message read or unread. The cache follows the helper's
    /// answer for the id, not the request.
    pub(super) async fn on_set_seen(&mut self, path: String, uid: u32, seen: bool) {
        let account_id = self.account_id;
        let emit = &self.emit;
        tuta_set_seen(
            &self.account,
            account_id,
            &mut self.state,
            &path,
            &[uid],
            seen,
            self.cache.as_ref(),
            emit,
        )
        .await;
        emit(WorkerEvent::SeenSettled { path, uid });
    }

    /// Mark every message the cache has in a folder read, in the helper's
    /// batches; what it did not get to stays unread.
    pub(super) async fn on_mark_all_read(&mut self, folder_id: u32, path: String) {
        let account_id = self.account_id;
        let emit = &self.emit;
        let unread: Vec<u32> = self
            .cache
            .as_ref()
            .map(|c| {
                c.load_messages(account_id, &path, folder_id)
                    .into_iter()
                    .filter(|m| m.unread)
                    .map(|m| m.uid)
                    .collect()
            })
            .unwrap_or_default();
        let applied = tuta_set_seen(
            &self.account,
            account_id,
            &mut self.state,
            &path,
            &unread,
            true,
            self.cache.as_ref(),
            emit,
        )
        .await;
        emit(WorkerEvent::FolderUnread {
            folder_id,
            unread: (unread.len() - applied.len()) as u32,
        });
    }

    /// Move messages to `dest`, saying `failure` (with the reason) when not
    /// all of them went.
    pub(super) async fn move_to(
        &mut self,
        path: String,
        uids: Vec<u32>,
        dest: String,
        failure: impl Fn(&str) -> String,
    ) {
        let account_id = self.account_id;
        let emit = &self.emit;
        if let Err(e) = tuta_move_uids(
            &self.account,
            account_id,
            &mut self.state,
            &path,
            &uids,
            &dest,
            self.cache.as_ref(),
            emit,
        )
        .await
        {
            emit(WorkerEvent::error(failure(&e.message())));
        }
    }

    /// Spam and Not Spam are moves: Tuta learns from where mail is filed,
    /// and keeps no `$Junk` keyword. Spam goes to the Junk folder itself
    /// whatever the request names, should it name another.
    pub(super) async fn on_mark_spam(&mut self, path: String, uid: u32, dest: String) {
        let dest = self.state.path_of(FolderKind::Junk).unwrap_or(dest);
        self.move_to(path, vec![uid], dest, |e| {
            i18n_f("Could not mark as spam: {e}", &[("e", e)])
        })
        .await;
    }

    /// Erase messages for good. The server erases only what is in the
    /// Trash or Spam, so mail anywhere else goes to the Trash first and is
    /// erased there, as Tuta's own app does; a draft is erased as a draft.
    pub(super) async fn on_purge_messages(&mut self, path: String, uids: Vec<u32>) {
        let account_id = self.account_id;
        let erased = match self.state.kinds.get(&path).copied() {
            Some(FolderKind::Drafts | FolderKind::Templates) => {
                self.erase_drafts(&path, &uids).await
            }
            Some(FolderKind::Trash | FolderKind::Junk) => self.erase_in(&path, &uids).await,
            _ => match self.state.path_of(FolderKind::Trash) {
                Some(trash) => self.erase_via(&path, &uids, &trash).await,
                None => Err(HelperError::new("not_trash_or_spam")),
            },
        };
        if let Err(e) = erased {
            tracing::warn!(
                "[account {account_id}] could not erase {} message(s) in {path}: {}",
                uids.len(),
                e.code
            );
            (self.emit)(WorkerEvent::error(i18n_f(
                "Could not delete permanently: {e}",
                &[("e", &e.message())],
            )));
            // The list took the rows out when it asked.
            self.reload(&path).await;
        }
        (self.emit)(WorkerEvent::BulkComplete);
    }

    /// Erase `uids`, which are in the Trash or Spam folder `path`.
    async fn erase_in(&mut self, path: &str, uids: &[u32]) -> Result<(), HelperError> {
        let mut ids = Vec::with_capacity(uids.len());
        for &uid in uids {
            ids.push(self.resolve(path, uid).await?);
        }
        self.erase_ids(path, ids).await
    }

    /// Erase the mail `ids` in the Trash or Spam folder `path`, and take it
    /// out of the cache and the list.
    async fn erase_ids(&mut self, path: &str, ids: Vec<String>) -> Result<(), HelperError> {
        let account_id = self.account_id;
        let (folder_id, folder) = self
            .state
            .folders
            .get(path)
            .cloned()
            .ok_or_else(|| HelperError::new("folder_not_found"))?;
        let client = self.state.client(&self.account)?;
        let (erased, failed) = run(move || Ok(delete_ids(&client, &ids, &folder)))
            .await
            .unwrap_or_else(|e| (Vec::new(), Some(e)));
        if let Some(c) = self.cache.as_ref() {
            for id in &erased {
                c.delete_message(account_id, path, hash_uid(id));
            }
            emit_folder(
                folder_id,
                c.load_messages(account_id, path, folder_id),
                &self.state,
                &self.emit,
            );
        }
        for id in &erased {
            self.state.uids.remove(&hash_uid(id));
        }
        failed.map_or(Ok(()), Err)
    }

    /// Erase `uids` in `path`, outside the Trash, by way of `trash`.
    async fn erase_via(&mut self, path: &str, uids: &[u32], trash: &str) -> Result<(), HelperError> {
        let account_id = self.account_id;
        let (moved, failed) = tuta_move_ids(
            &self.account,
            account_id,
            &mut self.state,
            path,
            uids,
            trash,
            self.cache.as_ref(),
            &self.emit,
        )
        .await;
        let erased = self.erase_ids(trash, moved).await;
        failed.map_or(erased, Err)
    }

    /// Erase drafts (a superseded copy of one being written, #340), or
    /// templates, which are drafts too on a Tuta account.
    async fn erase_drafts(&mut self, path: &str, uids: &[u32]) -> Result<(), HelperError> {
        let account_id = self.account_id;
        for &uid in uids {
            let id = self.resolve(path, uid).await?;
            let client = self.state.client(&self.account)?;
            run(move || delete_draft(&client, &id)).await?;
            self.state.uids.remove(&uid);
            if let Some(c) = self.cache.as_ref() {
                c.delete_message(account_id, path, uid);
            }
        }
        Ok(())
    }

    /// A uid's compound id in `path`.
    pub(super) async fn resolve(&mut self, path: &str, uid: u32) -> Result<String, HelperError> {
        tuta_resolve(
            &self.account,
            self.account_id,
            &mut self.state,
            path,
            uid,
            self.cache.as_ref(),
            &self.emit,
        )
        .await
    }

    /// Show a folder as the server has it, after a change that did not go.
    pub(super) async fn reload(&mut self, path: &str) {
        let account_id = self.account_id;
        let emit = &self.emit;
        let Some((folder_id, _)) = self.state.folders.get(path).cloned() else {
            return;
        };
        match tuta_load_folder(
            &self.account,
            account_id,
            folder_id,
            path,
            self.cache.as_ref(),
            &mut self.state,
            emit,
        )
        .await
        {
            Ok(messages) => emit_folder(folder_id, messages, &self.state, emit),
            Err(e) => tracing::info!("[account {account_id}] could not list {path}: {}", e.code),
        }
    }

    /// The uids of the mail `keys` name (see [`crate::models::tuta_key`])
    /// among what `path` holds now, by the id snapshot of one listing: a
    /// Tuta key names its uid, which a move leaves as it was, and a
    /// Message-ID the cached row that read body showed it on.
    async fn keyed_uids(&mut self, path: &str, keys: &[String]) -> Result<Vec<u32>, HelperError> {
        let account_id = self.account_id;
        let (folder_id, folder) = self
            .state
            .folders
            .get(path)
            .cloned()
            .ok_or_else(|| HelperError::new("folder_not_found"))?;
        let client = self.state.client(&self.account)?;
        let page = run(move || sync(&client, Some(&folder), 1, 0)).await?;
        let ids = page
            .all_ids
            .ok_or_else(|| HelperError::new("invalid_response"))?;
        let held: HashMap<u32, String> = ids.into_iter().map(|id| (hash_uid(&id), id)).collect();
        let cached = self
            .cache
            .as_ref()
            .map(|c| c.load_messages(account_id, path, folder_id))
            .unwrap_or_default();
        let mut uids = Vec::new();
        for key in keys {
            let uid = tuta_key_uid(key).or_else(|| {
                cached
                    .iter()
                    .find(|m| !m.message_id.is_empty() && m.message_id == *key)
                    .map(|m| m.uid)
            });
            if let Some((uid, id)) = uid.and_then(|uid| Some((uid, held.get(&uid)?))) {
                self.state.uids.insert(uid, id.clone());
                uids.push(uid);
            }
        }
        Ok(uids)
    }

    /// Put moved messages back. Each is found by the key the app recorded
    /// it by, its uid or its Message-ID, among what the folder the move put
    /// it in holds now.
    pub(super) async fn on_undo_move(
        &mut self,
        path: String,
        dest: String,
        dest_folder_id: u32,
        keys: Vec<String>,
    ) {
        let account_id = self.account_id;
        let moved = match self.keyed_uids(&path, &keys).await {
            Ok(uids) if uids.is_empty() => {
                tracing::info!("undo: the messages are no longer where that move put them");
                Ok(())
            }
            Ok(uids) => {
                let moved = tuta_move_uids(
                    &self.account,
                    account_id,
                    &mut self.state,
                    &path,
                    &uids,
                    &dest,
                    self.cache.as_ref(),
                    &self.emit,
                )
                .await;
                self.emit_cached(dest_folder_id, &dest);
                moved
            }
            Err(e) => Err(e),
        };
        if let Err(e) = moved {
            (self.emit)(WorkerEvent::error(i18n_f(
                "Undo failed: {e}",
                &[("e", &e.message())],
            )));
        }
        // The app spins its busy indicator until an undo answers.
        (self.emit)(WorkerEvent::BulkComplete);
    }

    /// Snoozed mail's time has come (#527): unread again, back where it
    /// was, and that folder listed so the app sees it arrive. Mail no
    /// longer in the Snoozed folder is done with too.
    pub(super) async fn on_unsnooze(
        &mut self,
        path: String,
        dest: String,
        dest_folder_id: u32,
        keys: Vec<String>,
    ) {
        let account_id = self.account_id;
        let woken = match self.keyed_uids(&path, &keys).await {
            Ok(uids) if uids.is_empty() => {
                tracing::info!("snooze: {} message(s) no longer in {path:?}", keys.len());
                Ok(())
            }
            Ok(uids) => {
                // Back as new mail is: unread, so it is seen to have come
                // back. A mark that did not go is not worth keeping the mail
                // asleep for.
                tuta_set_seen(
                    &self.account,
                    account_id,
                    &mut self.state,
                    &path,
                    &uids,
                    false,
                    self.cache.as_ref(),
                    &self.emit,
                )
                .await;
                let moved = tuta_move_uids(
                    &self.account,
                    account_id,
                    &mut self.state,
                    &path,
                    &uids,
                    &dest,
                    self.cache.as_ref(),
                    &self.emit,
                )
                .await;
                self.emit_cached(dest_folder_id, &dest);
                moved
            }
            Err(e) => Err(e),
        };
        if let Err(e) = &woken {
            tracing::warn!(
                "[account {account_id}] snooze: could not bring {} message(s) back from {path:?}: {}",
                keys.len(),
                e.code
            );
        }
        (self.emit)(WorkerEvent::Unsnoozed {
            message_ids: keys,
            done: woken.is_ok(),
        });
    }

    /// Take in a message from another account (#265): the helper imports it
    /// into `path`, re-encrypted for this account, as Tuta's own app
    /// imports a mail file, and the folder is listed again to show it.
    pub(super) async fn on_import_raw(
        &mut self,
        token: u64,
        path: String,
        raw: Vec<u8>,
        seen: bool,
        flagged: bool,
    ) {
        let result = self.import_raw(&path, raw, seen, flagged).await;
        if let Err(e) = &result {
            tracing::warn!(
                "[account {}] could not import a message into {path}: {}",
                self.account_id,
                e.code
            );
        }
        (self.emit)(WorkerEvent::RawImported {
            token,
            result: result.map_err(|e| e.message()),
        });
    }

    async fn import_raw(
        &mut self,
        path: &str,
        raw: Vec<u8>,
        seen: bool,
        flagged: bool,
    ) -> Result<(), HelperError> {
        let account_id = self.account_id;
        let (folder_id, folder) = self
            .state
            .folders
            .get(path)
            .cloned()
            .ok_or_else(|| HelperError::new("folder_not_found"))?;
        let client = self.state.client(&self.account)?;
        // The bytes go to the helper and come back, rather than a copy of a
        // message that may be megabytes being kept for the row below.
        let (raw, imported) = run(move || {
            let id = import(&client, &folder, &raw, !seen)?;
            Ok((raw, id))
        })
        .await?;
        let emit = &self.emit;
        let listed = tuta_load_folder(
            &self.account,
            account_id,
            folder_id,
            path,
            self.cache.as_ref(),
            &mut self.state,
            emit,
        )
        .await;
        // The mail is in either way; the next sync shows it.
        let mut messages = match listed {
            Ok(messages) => messages,
            Err(e) => {
                tracing::info!("[account {account_id}] could not list {path}: {}", e.code);
                return Ok(());
            }
        };
        // A listing leaves out the Message-ID, which an undo of the move
        // finds the copy by, and Tuta keeps no star: both come from the
        // message itself, as a read body would give them.
        let uid = imported.as_deref().map(hash_uid);
        if let Some(m) = messages.iter_mut().find(|m| Some(m.uid) == uid) {
            let parsed = summary_from_raw(account_id, folder_id, m.uid, &raw);
            if !parsed.message_id.is_empty() {
                m.message_id = parsed.message_id;
                m.references = merge_msgids(&parsed.references, &m.references);
                m.preview = parsed.preview;
                if let Some(c) = &self.cache {
                    c.set_threading(
                        account_id,
                        path,
                        m.uid,
                        &m.message_id,
                        &m.references,
                        &m.preview,
                    );
                }
            }
            if flagged {
                m.starred = true;
                if let Some(c) = &self.cache {
                    c.set_starred(account_id, path, m.uid, true);
                }
            }
        }
        emit_folder(folder_id, messages, &self.state, emit);
        Ok(())
    }

    /// A folder's cached rows to the list, with its chip number.
    fn emit_cached(&self, folder_id: u32, path: &str) {
        let rows = self
            .cache
            .as_ref()
            .map(|c| c.load_messages(self.account_id, path, folder_id))
            .unwrap_or_default();
        emit_folder(folder_id, rows, &self.state, &self.emit);
    }
}
