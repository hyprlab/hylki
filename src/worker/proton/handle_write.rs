//! The Proton handlers for requests that change mail: read state, stars,
//! tags (kept here only), moves, and what the helper cannot do yet.

use super::ProtonWorker;
use crate::i18n::{i18n, i18n_f};
use crate::models::FolderKind;
use crate::worker::folders::restore_folders;
use crate::worker::helper::HelperError;
use crate::worker::proton::index::{emit_folder, proton_load_folder};
use crate::worker::proton::ops::{proton_move_uids, proton_set_seen, proton_set_starred};
use crate::worker::WorkerEvent;

impl<E: Fn(WorkerEvent)> ProtonWorker<E> {
    /// Mark a message read or unread. The cache follows the helper's
    /// answer for the id, not the request.
    pub(super) async fn on_set_seen(&mut self, path: String, uid: u32, seen: bool) {
        let account_id = self.account_id;
        let emit = &self.emit;
        proton_set_seen(
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

    /// Star or unstar a message: Proton's Starred label, so the Starred
    /// folder follows on the next poll. Put back and said when the server
    /// did not take it.
    pub(super) async fn on_set_flagged(&mut self, path: String, uid: u32, flagged: bool) {
        let emit = &self.emit;
        if let Err(e) = proton_set_starred(
            &self.account,
            self.account_id,
            &mut self.state,
            &path,
            uid,
            flagged,
            self.cache.as_ref(),
            emit,
        )
        .await
        {
            emit(WorkerEvent::error(i18n_f(
                "Could not change the star: {e}",
                &[("e", &e.message())],
            )));
            self.reload(&path).await;
        }
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
        let applied = proton_set_seen(
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
        if let Err(e) = proton_move_uids(
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

    /// Spam and Not Spam are moves: Proton learns from where mail is filed,
    /// and keeps no `$Junk` keyword. Spam goes to the Junk folder itself
    /// whatever the request names, should it name another.
    pub(super) async fn on_mark_spam(&mut self, path: String, uid: u32, dest: String) {
        let dest = self.state.path_of(FolderKind::Junk).unwrap_or(dest);
        self.move_to(path, vec![uid], dest, |e| {
            i18n_f("Could not mark as spam: {e}", &[("e", e)])
        })
        .await;
    }

    /// Erase messages for good. The helper cannot delete yet, so outside
    /// the Trash they are moved there, the nearest it can do; in the Trash
    /// itself that leaves nothing to do but say so.
    pub(super) async fn on_purge_messages(&mut self, path: String, uids: Vec<u32>) {
        match self.state.path_of(FolderKind::Trash) {
            Some(trash) if trash != path => {
                self.move_to(path, uids, trash, |e| {
                    i18n_f("Could not move messages: {e}", &[("e", e)])
                })
                .await;
            }
            _ => {
                (self.emit)(WorkerEvent::error(permanent_delete_text()));
                // The list took the rows out when it asked.
                self.reload(&path).await;
            }
        }
        (self.emit)(WorkerEvent::BulkComplete);
    }

    /// Show a folder as the server has it, after a change that did not go.
    pub(super) async fn reload(&mut self, path: &str) {
        let account_id = self.account_id;
        let emit = &self.emit;
        let Some((folder_id, _)) = self.state.folders.get(path).cloned() else {
            return;
        };
        match proton_load_folder(
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

    /// Put moved messages back, found in the folder the move put them in
    /// by their Message-ID, which a Proton listing carries.
    pub(super) async fn on_undo_move(
        &mut self,
        path: String,
        dest: String,
        dest_folder_id: u32,
        message_ids: Vec<String>,
    ) {
        let account_id = self.account_id;
        let emit = &self.emit;
        let found: Result<Vec<u32>, HelperError> = match self.state.folders.get(&path).cloned() {
            Some((folder_id, _)) => proton_load_folder(
                &self.account,
                account_id,
                folder_id,
                &path,
                self.cache.as_ref(),
                &mut self.state,
                emit,
            )
            .await
            .map(|messages| {
                messages
                    .into_iter()
                    .filter(|m| !m.message_id.is_empty() && message_ids.contains(&m.message_id))
                    .map(|m| m.uid)
                    .collect()
            }),
            None => Err(HelperError::new("folder_not_found")),
        };
        match found {
            Ok(uids) if uids.is_empty() => {
                tracing::info!("undo: the messages are no longer where that move put them");
            }
            Ok(uids) => {
                if let Err(e) = proton_move_uids(
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
                    emit(WorkerEvent::error(i18n_f(
                        "Undo failed: {e}",
                        &[("e", &e.message())],
                    )));
                }
                let unread = self
                    .cache
                    .as_ref()
                    .map(|c| c.load_messages(account_id, &dest, dest_folder_id))
                    .unwrap_or_default();
                emit_folder(dest_folder_id, unread, &self.state, emit);
            }
            Err(e) => emit(WorkerEvent::error(i18n_f(
                "Undo failed: {e}",
                &[("e", &e.message())],
            ))),
        }
        // The app spins its busy indicator until an undo answers.
        emit(WorkerEvent::BulkComplete);
    }

    /// Folder changes the helper cannot make yet: said, and the folder list
    /// put back as the server has it, since the app drew the change first
    /// (#554).
    pub(super) fn on_folder_change(&mut self) {
        let emit = &self.emit;
        emit(WorkerEvent::error(i18n(
            "Folders of a Proton account can't be created, renamed or deleted from Hylki yet. Use Proton's own apps.",
        )));
        restore_folders(self.cache.as_ref(), self.account_id, emit);
    }
}

/// Why nothing is deleted for good on a Proton account.
pub(super) fn permanent_delete_text() -> String {
    i18n("Proton accounts can't delete mail for good from Hylki yet. Empty the Trash in Proton's own apps.")
}
