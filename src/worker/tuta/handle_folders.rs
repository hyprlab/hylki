//! The Tuta handlers for folder changes: making, renaming, moving and
//! deleting folders, and emptying the Trash or Spam.

use super::TutaWorker;
use crate::i18n::{i18n, i18n_f};
use crate::models::{slashed_name, FolderKind};
use crate::worker::folders::restore_folders;
use crate::worker::helper::HelperError;
use crate::worker::tuta::index::run;
use crate::worker::tuta::ops::emit_folder;
use crate::worker::tuta::wire::{create_folder, delete_folder, empty_folder, rename_folder};
use crate::worker::WorkerEvent;

/// A folder path's parent path (`None` at the top) and its last level.
fn split(path: &str) -> (Option<&str>, &str) {
    match path.rsplit_once('/') {
        Some((parent, leaf)) => (Some(parent), leaf),
        None => (None, path),
    }
}

impl<E: Fn(WorkerEvent)> TutaWorker<E> {
    /// A folder change the server did not make: said, with the folder list
    /// put back as the server has it, since the app drew the change first
    /// (#554).
    fn folder_refused(&self, text: String) {
        (self.emit)(WorkerEvent::error(text));
        restore_folders(self.cache.as_ref(), self.account_id, &self.emit);
    }

    /// The Tuta id of the folder at `path`, as the last sync listed it.
    fn tuta_folder(&self, path: &str) -> Result<String, HelperError> {
        self.state
            .folders
            .get(path)
            .map(|(_, id)| id.clone())
            .ok_or_else(|| HelperError::new("folder_not_found"))
    }

    /// Make a folder. `path` is the app's: its last level the name, the
    /// rest a folder the last sync listed.
    pub(super) async fn on_create_folder(&mut self, path: String) {
        let account_id = self.account_id;
        let (parent, leaf) = split(&path);
        let parent = match parent.map(|p| self.tuta_folder(p)).transpose() {
            Ok(parent) => parent,
            Err(_) => {
                return self.folder_refused(i18n_f("Could not create folder: {e}", &[("e", &i18n("the folder it goes in is gone"))]))
            }
        };
        let name = slashed_name(leaf);
        let made = match self.state.client(&self.account) {
            Ok(client) => run(move || create_folder(&client, &name, parent.as_deref())).await,
            Err(e) => Err(e),
        };
        match made {
            Ok(_) => self.sync_folders().await,
            Err(e) => {
                tracing::warn!(
                    "[account {account_id}] could not create folder {path}: {}",
                    e.code
                );
                self.folder_refused(i18n_f("Could not create folder: {e}", &[("e", &e.message())]));
            }
        }
    }

    /// Rename a folder, move it under another parent, or both, in one call.
    pub(super) async fn on_rename_folder(&mut self, old_path: String, new_path: String) {
        let account_id = self.account_id;
        let id = match self.tuta_folder(&old_path) {
            Ok(id) => id,
            Err(e) => {
                return self.folder_refused(i18n_f(
                    "Could not rename folder: {e}",
                    &[("e", &e.message())],
                ))
            }
        };
        let ((old_parent, old_leaf), (new_parent, new_leaf)) =
            (split(&old_path), split(&new_path));
        let parent = if new_parent == old_parent {
            None
        } else {
            match new_parent.map(|p| self.tuta_folder(p)).transpose() {
                // `""` is the top of the tree.
                Ok(parent) => Some(parent.unwrap_or_default()),
                Err(_) => {
                    return self.folder_refused(i18n_f("Could not rename folder: {e}", &[("e", &i18n("the folder it goes in is gone"))]))
                }
            }
        };
        let name = (new_leaf != old_leaf).then(|| slashed_name(new_leaf));
        if name.is_none() && parent.is_none() {
            return;
        }
        let renamed = match self.state.client(&self.account) {
            Ok(client) => {
                run(move || rename_folder(&client, &id, name.as_deref(), parent.as_deref())).await
            }
            Err(e) => Err(e),
        };
        if let Err(e) = &renamed {
            tracing::warn!(
                "[account {account_id}] could not rename folder {old_path} to {new_path}: {}",
                e.code
            );
            self.folder_refused(i18n_f("Could not rename folder: {e}", &[("e", &e.message())]));
        }
        self.sync_folders().await;
    }

    /// Delete a folder, as Tuta's own app does: for good when it is in the
    /// Trash or Spam or holds nothing, otherwise into the Trash with its
    /// mail. The app's `trash` is not needed for that.
    pub(super) async fn on_delete_folder(&mut self, path: String) {
        let account_id = self.account_id;
        let id = match self.tuta_folder(&path) {
            Ok(id) => id,
            Err(e) => {
                return self.folder_refused(i18n_f(
                    "Could not delete folder: {e}",
                    &[("e", &e.message())],
                ))
            }
        };
        let deleted = match self.state.client(&self.account) {
            Ok(client) => run(move || delete_folder(&client, &id)).await,
            Err(e) => Err(e),
        };
        match deleted {
            Ok(trashed) => {
                if trashed {
                    tracing::info!("[account {account_id}] {path} went to the Trash with its mail");
                }
                self.sync_folders().await;
            }
            Err(e) => {
                tracing::warn!(
                    "[account {account_id}] could not delete folder {path}: {}",
                    e.code
                );
                self.folder_refused(i18n_f("Could not delete folder: {e}", &[("e", &e.message())]));
            }
        }
    }

    /// Erase everything in the Trash or Spam (#152). The server erases
    /// nothing in another folder, and the app offers this for those two
    /// only.
    pub(super) async fn on_empty_folder(&mut self, folder_id: u32, path: String) {
        let account_id = self.account_id;
        let emit = &self.emit;
        let kind = self.state.kinds.get(&path).copied();
        let emptied = match (kind, self.tuta_folder(&path)) {
            (Some(FolderKind::Trash | FolderKind::Junk), Ok(id)) => {
                match self.state.client(&self.account) {
                    Ok(client) => run(move || empty_folder(&client, &id)).await,
                    Err(e) => Err(e),
                }
            }
            (_, Err(e)) => Err(e),
            _ => Err(HelperError::new("not_trash_or_spam")),
        };
        match emptied {
            Ok(n) => {
                tracing::info!("[account {account_id}] emptied {path}: {n} message(s) erased");
                if let Some(c) = self.cache.as_ref() {
                    for m in c.load_messages(account_id, &path, folder_id) {
                        c.delete_message(account_id, &path, m.uid);
                        self.state.uids.remove(&m.uid);
                    }
                }
                emit_folder(folder_id, Vec::new(), &self.state, emit);
            }
            Err(e) => {
                tracing::warn!("[account {account_id}] could not empty {path}: {}", e.code);
                emit(WorkerEvent::error(i18n_f(
                    "Could not empty the folder: {e}",
                    &[("e", &e.message())],
                )));
                self.reload(&path).await;
            }
        }
    }
}
