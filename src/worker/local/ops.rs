//! The On This Computer account's requests that change mail: flags and
//! tags in the file names, moves and deletions of the files, folders, mail
//! moved in from another account, and an attachment taken out.

use super::LocalWorker;
use crate::i18n::i18n_f;
use crate::worker::strip::{cache_rewritten, note_date, strip_refused, without_attachment};
use crate::worker::{summary_from_raw, WorkerEvent};

impl<E: Fn(WorkerEvent)> LocalWorker<E> {
    /// Change a message's flags on disk, saying so when the file is gone.
    fn flags(&mut self, path: &str, uid: u32, add: &str, remove: &str) -> bool {
        match self.store.set_flags(path, uid, add, remove) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("local mail: could not flag {path}/{uid}: {e}");
                false
            }
        }
    }

    pub(super) fn on_set_seen(&mut self, path: String, uid: u32, seen: bool) {
        let (add, remove) = if seen { ("S", "") } else { ("", "S") };
        if self.flags(&path, uid, add, remove) {
            if let Some(c) = self.cache.as_ref() {
                c.set_unread(self.account_id, &path, uid, !seen);
            }
        }
        (self.emit)(WorkerEvent::SeenSettled { path, uid });
    }

    pub(super) fn on_set_flagged(&mut self, path: String, uid: u32, flagged: bool) {
        let (add, remove) = if flagged { ("F", "") } else { ("", "F") };
        if self.flags(&path, uid, add, remove) {
            if let Some(c) = self.cache.as_ref() {
                c.set_starred(self.account_id, &path, uid, flagged);
            }
        }
    }

    /// Put a tag on a message or take it off. A folder that has given all
    /// 26 of Maildir's letters keeps a new tag in the cache only.
    pub(super) fn on_set_keyword(
        &mut self,
        path: String,
        uid: u32,
        message_id: String,
        keyword: String,
        add: bool,
    ) {
        let account_id = self.account_id;
        let stored = self.store.set_keyword(&path, uid, &keyword, add);
        if let Some(c) = self.cache.as_ref() {
            match stored {
                Ok(true) => {
                    c.set_keyword(account_id, &path, uid, &keyword, add);
                }
                Ok(false) if !message_id.is_empty() => {
                    c.set_local_tag(account_id, &message_id, &keyword, add)
                }
                _ => tracing::warn!("local mail: could not tag {path}/{uid}"),
            }
        }
        (self.emit)(WorkerEvent::TagSettled { path, uid, keyword });
    }

    pub(super) fn on_mark_all_read(&mut self, folder_id: u32, path: String) {
        let entries = self.store.scan(&path).unwrap_or_default();
        for e in entries.iter().filter(|e| !e.has('S')) {
            self.flags(&path, e.uid, "S", "");
        }
        if let Some(c) = self.cache.as_ref() {
            c.mark_folder_read(self.account_id, &path);
        }
        (self.emit)(WorkerEvent::FolderUnread {
            folder_id,
            unread: 0,
        });
    }

    /// Move messages to `dest`, made if missing, flags changed by `add`
    /// and `remove` on the way; the cache's rows go with each file. How
    /// many moved, or the first error.
    fn move_uids(
        &mut self,
        path: &str,
        uids: &[u32],
        dest: &str,
        add: &str,
        remove: &str,
    ) -> Result<usize, String> {
        let account_id = self.account_id;
        let made = self.folder(dest).is_none();
        let mut moved = 0;
        let mut first_error = None;
        self.store.hold_uid_files(true);
        for &uid in uids {
            let new_uid = match self.store.move_to(path, uid, dest) {
                Ok(new_uid) => new_uid,
                Err(e) => {
                    first_error.get_or_insert(e.to_string());
                    continue;
                }
            };
            if !add.is_empty() || !remove.is_empty() {
                self.flags(dest, new_uid, add, remove);
            }
            if let Some(c) = self.cache.as_ref() {
                c.relocate_message(account_id, path, uid, dest, new_uid);
                if add.contains('S') || remove.contains('S') {
                    c.set_unread(account_id, dest, new_uid, remove.contains('S'));
                }
            }
            moved += 1;
        }
        self.store.hold_uid_files(false);
        if made {
            self.refresh_folders();
        }
        match first_error {
            Some(e) if moved == 0 => Err(e),
            Some(e) => {
                tracing::warn!(
                    "local mail: {} of {} not moved: {e}",
                    uids.len() - moved,
                    uids.len()
                );
                Ok(moved)
            }
            None => Ok(moved),
        }
    }

    pub(super) fn on_move(&mut self, path: &str, uids: &[u32], dest: &str) {
        if let Err(e) = self.move_uids(path, uids, dest, "", "") {
            (self.emit)(WorkerEvent::error(i18n_f(
                "Could not move messages: {e}",
                &[("e", &e)],
            )));
        }
    }

    /// Delete messages for good.
    pub(super) fn on_purge(&mut self, path: &str, uids: &[u32]) {
        for &uid in uids {
            match self.store.delete(path, uid) {
                Ok(()) => {
                    if let Some(c) = self.cache.as_ref() {
                        c.delete_message(self.account_id, path, uid);
                    }
                }
                Err(e) => tracing::warn!("local mail: could not delete {path}/{uid}: {e}"),
            }
        }
    }

    pub(super) fn on_empty_folder(&mut self, folder_id: u32, path: String) {
        let uids: Vec<u32> = match self.store.scan(&path) {
            Ok(entries) => entries.iter().map(|e| e.uid).collect(),
            Err(e) => {
                (self.emit)(WorkerEvent::error(i18n_f(
                    "Could not empty the folder: {e}",
                    &[("e", &e.to_string())],
                )));
                return;
            }
        };
        self.on_purge(&path, &uids);
        tracing::info!("emptied {path}: {} message(s) erased", uids.len());
        (self.emit)(WorkerEvent::Messages {
            folder_id,
            messages: Vec::new(),
        });
        (self.emit)(WorkerEvent::FolderUnread {
            folder_id,
            unread: 0,
        });
    }

    /// Put moved messages back: the ones in `path` with these Message-IDs
    /// go to `dest`, flags changed by `remove` on the way, and `dest` is
    /// shown again.
    pub(super) fn move_back(
        &mut self,
        path: &str,
        dest: &str,
        dest_folder_id: u32,
        message_ids: &[String],
        remove: &str,
    ) -> Result<(), String> {
        let folder_id = self.folder_id(path);
        let uids: Vec<u32> = self
            .read_folder(folder_id, path)?
            .into_iter()
            .filter(|m| message_ids.contains(&m.message_id))
            .map(|m| m.uid)
            .collect();
        if uids.is_empty() {
            tracing::info!("undo: the messages are no longer where that move put them");
            return Ok(());
        }
        self.move_uids(path, &uids, dest, "", remove)?;
        let messages = self.read_folder(dest_folder_id, dest)?;
        let kind = self
            .folder(dest)
            .map_or(crate::models::FolderKind::Custom, |f| f.kind);
        let unread = crate::worker::folders::chip_count_of(kind, &messages);
        (self.emit)(WorkerEvent::Messages {
            folder_id: dest_folder_id,
            messages,
        });
        (self.emit)(WorkerEvent::FolderUnread {
            folder_id: dest_folder_id,
            unread,
        });
        Ok(())
    }

    pub(super) fn on_create_folder(&mut self, path: String) {
        match self.store.create_folder(&path) {
            Ok(()) => {
                self.refresh_folders();
                self.unread_folders.push(path);
            }
            Err(e) => {
                (self.emit)(WorkerEvent::error(i18n_f(
                    "Could not create folder: {e}",
                    &[("e", &e)],
                )));
                // The app drew the change already: the store says how it
                // really is (#554).
                self.refresh_folders();
            }
        }
    }

    pub(super) fn on_rename_folder(&mut self, old_path: String, new_path: String) {
        match self.store.rename_folder(&old_path, &new_path) {
            Ok(()) => {
                if let Some(c) = self.cache.as_ref() {
                    c.rename_folder_rows(self.account_id, &old_path, &new_path);
                }
                self.refresh_folders();
            }
            Err(e) => {
                (self.emit)(WorkerEvent::error(i18n_f(
                    "Could not rename folder: {e}",
                    &[("e", &e)],
                )));
                // The app drew the change already: the store says how it
                // really is (#554).
                self.refresh_folders();
            }
        }
    }

    /// Delete a folder, its mail moved to `trash` first when given, and
    /// otherwise deleted with it.
    pub(super) fn on_delete_folder(&mut self, path: String, trash: Option<String>) {
        let result = self
            .store
            .scan(&path)
            .map_err(|e| e.to_string())
            .and_then(|entries| {
                let uids: Vec<u32> = entries.iter().map(|e| e.uid).collect();
                match trash.as_deref().filter(|t| *t != path) {
                    Some(trash) => self.move_uids(&path, &uids, trash, "", "").map(|_| ()),
                    None => {
                        self.on_purge(&path, &uids);
                        Ok(())
                    }
                }
            })
            .and_then(|()| self.store.delete_folder(&path));
        match result {
            Ok(()) => self.refresh_folders(),
            Err(e) => (self.emit)(WorkerEvent::error(i18n_f(
                "Could not delete folder: {e}",
                &[("e", &e)],
            ))),
        }
    }

    /// Mail moved in from another account (#265): stored whole, with its
    /// read and starred state.
    pub(super) fn on_import_raw(
        &mut self,
        token: u64,
        path: String,
        raw: Vec<u8>,
        seen: bool,
        flagged: bool,
    ) {
        let account_id = self.account_id;
        let made = self.folder(&path).is_none();
        let flags = format!(
            "{}{}",
            if flagged { "F" } else { "" },
            if seen { "S" } else { "" }
        );
        let result = self
            .store
            .deliver(&path, &raw, &flags, &[])
            .map_err(|e| i18n_f("Could not store the message: {e}", &[("e", &e.to_string())]));
        if let (Ok(uid), Some(c)) = (&result, self.cache.as_ref()) {
            let mut m = summary_from_raw(account_id, self.folder_id(&path), *uid, &raw);
            m.unread = !seen;
            m.starred = flagged;
            c.upsert_messages(account_id, &path, &[m]);
        }
        if made {
            self.refresh_folders();
        }
        (self.emit)(WorkerEvent::RawImported {
            token,
            result: result.map(|_| ()),
        });
    }

    /// Take one attachment out of a message (#289): the message is stored
    /// again without it, flags and tags kept, and the old file deleted.
    pub(super) fn on_delete_attachment(
        &mut self,
        message_id: u32,
        path: String,
        uid: u32,
        name: String,
        size: u64,
    ) {
        let account_id = self.account_id;
        let rewritten = self
            .store
            .read(&path, uid)
            .map_err(|e| e.to_string())
            .and_then(|raw| {
                without_attachment(&raw, &name, size, &note_date()).map_err(|e| e.text())
            })
            .and_then(|raw| {
                let entry = self.store.entry(&path, uid).map_err(|e| e.to_string())?;
                let new_uid = self
                    .store
                    .deliver(&path, &raw, &entry.flags, &entry.keywords)
                    .map_err(|e| e.to_string())?;
                if let Err(e) = self.store.delete(&path, uid) {
                    tracing::warn!("local mail: the copy with the file is still in {path}: {e}");
                }
                Ok((new_uid, raw, entry))
            });
        let (new_uid, raw, entry) = match rewritten {
            Ok(done) => done,
            Err(e) => {
                strip_refused(
                    &self.emit,
                    i18n_f("Could not remove the attachment: {e}", &[("e", &e)]),
                    message_id,
                    name,
                    size,
                );
                return;
            }
        };
        if let Some(c) = self.cache.as_ref() {
            let mut m = summary_from_raw(account_id, self.folder_id(&path), new_uid, &raw);
            super::apply_entry(&mut m, &entry);
            c.delete_message(account_id, &path, uid);
            c.save_attachment_meta(account_id, &path, uid, &[]);
            c.upsert_messages(account_id, &path, &[m]);
            cache_rewritten(c, account_id, &path, new_uid, &raw);
        }
        let folder_id = self.folder_id(&path);
        if let Ok(messages) = self.read_folder(folder_id, &path) {
            (self.emit)(WorkerEvent::Messages {
                folder_id,
                messages,
            });
        }
        (self.emit)(WorkerEvent::AttachmentDeleted {
            message_id,
            path,
            uid,
            new_uid: Some(new_uid),
            name,
            size,
        });
    }
}
