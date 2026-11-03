//! The handlers for requests that change mail on the server: flags and
//! tags, moves, spam reports, imports, attachment removal, undo, deleting
//! for good, and folders.

use super::{ensured, After, Emit, ImapWorker};
use crate::i18n::{i18n, i18n_f};
use crate::models::FolderHierarchy;
use crate::worker::folders::restore_folders;
use crate::worker::imap::folders::refresh_folders;
use crate::worker::imap::gmail::{
    drop_gmail_label_copies, gmail_erase, gmail_labels_wanted, gmail_trash, store_gmail_labels,
    tag_labels,
};
use crate::worker::imap::mutate::{
    create_folder, delete_folder, import_flags, mark_all_read, mark_ham, mark_spam, move_message,
    move_messages, purge_messages, rename_folder, store_flag, store_keyword,
};
use crate::worker::imap::namespace::inside_inbox;
use crate::worker::imap::retry::load_messages_retry;
use crate::worker::imap::snooze::find_by_message_ids;
use crate::worker::imap::strip_attachment::strip_imap_attachment;
use crate::worker::imap::wire::{append_msg, search_uids, sel, uid_set};
use crate::worker::strip::{cache_rewritten, strip_refused};
use crate::worker::{has_gmail_labels, WorkerEvent};

impl<E: Emit> ImapWorker<E> {
    /// Mark a message read or unread.
    pub(super) async fn on_set_seen(&mut self, path: String, uid: u32, seen: bool) -> After {
        let account_id = self.account_id;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let mut lost = false;
        let sess = ensured(session);
        if let Err(e) = store_flag(sess, &path, uid, "\\Seen", seen).await {
            emit(WorkerEvent::error(i18n_f(
                "Could not update message: {e}",
                &[("e", &(e).to_string())],
            )));
            lost = true;
        } else if let Some(c) = cache.as_ref() {
            c.set_unread(account_id, &path, uid, !seen);
        }
        emit(WorkerEvent::SeenSettled { path, uid });
        After::lost_if(lost)
    }

    /// Star a message or remove its star.
    pub(super) async fn on_set_flagged(&mut self, path: String, uid: u32, flagged: bool) -> After {
        let account_id = self.account_id;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let mut lost = false;
        let sess = ensured(session);
        if let Err(e) = store_flag(sess, &path, uid, "\\Flagged", flagged).await {
            emit(WorkerEvent::error(i18n_f(
                "Could not change the star: {e}",
                &[("e", &(e).to_string())],
            )));
            lost = true;
        } else if let Some(c) = cache.as_ref() {
            c.set_starred(account_id, &path, uid, flagged);
        }
        After::lost_if(lost)
    }

    /// Put a tag on a message or take it off: a keyword, and on Gmail a label.
    pub(super) async fn on_set_keyword(
        &mut self,
        path: String,
        uid: u32,
        message_id: String,
        keyword: String,
        add: bool,
    ) -> After {
        let account_id = self.account_id;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let folder_refresh_due = &mut self.folder_refresh_due;
        let local_tags_noticed = &mut self.local_tags_noticed;
        let mut lost = false;
        let sess = ensured(session);
        let stored = store_keyword(sess, &path, uid, &keyword, add).await;
        // On Gmail a tag is a label too (#374), so Gmail's own apps
        // show it; the keyword stays for other IMAP clients.
        if stored.is_ok() && gmail_labels_wanted(account_id) {
            let labels = tag_labels(&keyword, add);
            match store_gmail_labels(sess, uid, &labels, add).await {
                Ok(()) => {
                    if let Some(c) = cache.as_ref() {
                        c.set_gmail_label(account_id, &path, uid, &labels, add);
                    }
                    // A new label is a new folder on Gmail.
                    if add {
                        *folder_refresh_due = true;
                    }
                }
                Err(e) => {
                    tracing::warn!("[account {account_id}] could not set Gmail label: {e}")
                }
            }
        }
        match stored {
            Ok(true) => {
                if let Some(c) = cache.as_ref() {
                    c.set_keyword(account_id, &path, uid, &keyword, add);
                }
            }
            Ok(false) => {
                // The mailbox takes no custom keywords: keep the tag
                // in Hylki, and say so once per session.
                if let Some(c) = cache.as_ref() {
                    if message_id.is_empty() {
                        c.set_keyword(account_id, &path, uid, &keyword, add);
                    } else {
                        c.set_local_tag(account_id, &message_id, &keyword, add);
                    }
                }
                if !*local_tags_noticed {
                    *local_tags_noticed = true;
                    emit(WorkerEvent::Notice(i18n(
                        "This server does not store tags, so Hylki keeps them on this computer only",
                    )));
                }
            }
            Err(e) => {
                emit(WorkerEvent::error(i18n_f(
                    "Could not tag message: {e}",
                    &[("e", &(e).to_string())],
                )));
                lost = true;
            }
        }
        emit(WorkerEvent::TagSettled { path, uid, keyword });
        After::lost_if(lost)
    }

    /// Mark every message in a folder read.
    pub(super) async fn on_mark_all_read(&mut self, folder_id: u32, path: String) -> After {
        let account_id = self.account_id;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let mut lost = false;
        let sess = ensured(session);
        match mark_all_read(sess, &path).await {
            Ok(()) => {
                if let Some(c) = cache.as_ref() {
                    c.mark_folder_read(account_id, &path);
                }
                emit(WorkerEvent::FolderUnread {
                    folder_id,
                    unread: 0,
                });
            }
            Err(e) => {
                emit(WorkerEvent::error(i18n_f(
                    "Could not mark folder read: {e}",
                    &[("e", &(e).to_string())],
                )));
                lost = true;
            }
        }
        After::lost_if(lost)
    }

    /// Report a message as spam and move it to `dest`.
    pub(super) async fn on_mark_spam(&mut self, path: String, uid: u32, dest: String) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let mut lost = false;
        let sess = ensured(session);
        match mark_spam(sess, &path, uid, &dest).await {
            Ok(created) => {
                if let Some(c) = cache.as_ref() {
                    drop_gmail_label_copies(c, account_id, account, &path, &[uid], &dest);
                    c.delete_message(account_id, &path, uid);
                }
                if created {
                    refresh_folders(account_id, account, sess, cache.as_ref(), emit).await;
                }
            }
            Err(e) => {
                emit(WorkerEvent::error(i18n_f(
                    "Could not mark as spam: {e}",
                    &[("e", &(e).to_string())],
                )));
                lost = true;
            }
        }
        After::lost_if(lost)
    }

    /// Report a message as not spam and move it to `dest`.
    pub(super) async fn on_mark_ham(&mut self, path: String, uid: u32, dest: String) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let mut lost = false;
        let sess = ensured(session);
        match mark_ham(sess, &path, &[uid], &dest).await {
            Ok(created) => {
                if let Some(c) = cache.as_ref() {
                    c.delete_message(account_id, &path, uid);
                }
                if created {
                    refresh_folders(account_id, account, sess, cache.as_ref(), emit).await;
                }
            }
            Err(e) => {
                emit(WorkerEvent::error(i18n_f(
                    "Could not mark as not spam: {e}",
                    &[("e", &(e).to_string())],
                )));
                lost = true;
            }
        }
        After::lost_if(lost)
    }

    /// Report several messages as not spam and move them to `dest`.
    pub(super) async fn on_mark_ham_many(
        &mut self,
        path: String,
        uids: Vec<u32>,
        dest: String,
    ) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let mut lost = false;
        let sess = ensured(session);
        match mark_ham(sess, &path, &uids, &dest).await {
            Ok(created) => {
                if let Some(c) = cache.as_ref() {
                    for uid in &uids {
                        c.delete_message(account_id, &path, *uid);
                    }
                }
                if created {
                    refresh_folders(account_id, account, sess, cache.as_ref(), emit).await;
                }
            }
            Err(e) => {
                emit(WorkerEvent::error(i18n_f(
                    "Could not mark {len} messages as not spam: {e}",
                    &[("len", &(uids.len()).to_string()), ("e", &(e).to_string())],
                )));
                lost = true;
            }
        }
        emit(WorkerEvent::BulkComplete);
        After::lost_if(lost)
    }

    /// Append raw message bytes to a folder.
    pub(super) async fn on_import_raw(
        &mut self,
        token: u64,
        path: String,
        raw: Vec<u8>,
        seen: bool,
        flagged: bool,
    ) -> After {
        let session = &mut self.session;
        let emit = &self.emit;
        let sess = ensured(session);
        let flags = import_flags(seen, flagged);
        let result = append_msg(sess, &path, flags.as_deref(), &raw)
            .await
            .map_err(|e| e.to_string());
        emit(WorkerEvent::RawImported { token, result });
        After::Done
    }

    /// Take one attachment out of a message on the server (#289).
    pub(super) async fn on_delete_attachment(
        &mut self,
        message_id: u32,
        path: String,
        uid: u32,
        name: String,
        size: u64,
    ) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let use_envelope = &mut self.use_envelope;
        let mut lost = false;
        let folders = cache
            .as_ref()
            .map(|c| c.load_folders(account_id))
            .unwrap_or_default();
        // Gmail keeps every message in All Mail as well as under the
        // label it is shown in: the original would stay there, file
        // and all, beside the copy.
        if has_gmail_labels(account, &folders) {
            let why = i18n("Gmail keeps a copy of every message in All Mail, so Hylki cannot remove an attachment there");
            strip_refused(emit, why, message_id, name, size);
            return After::Skip;
        }
        emit(WorkerEvent::Status(i18n("Removing the attachment…")));
        let result = strip_imap_attachment(session, account, &path, uid, &name, size).await;
        emit(WorkerEvent::Status(String::new()));
        match result {
            Ok((new_uid, raw)) => {
                if let Some(c) = cache.as_ref() {
                    c.delete_message(account_id, &path, uid);
                }
                if let Some(folder_id) = folders.iter().find(|f| f.path == path).map(|f| f.id) {
                    if let Ok(messages) = load_messages_retry(
                        account_id,
                        session,
                        account,
                        folder_id,
                        &path,
                        use_envelope,
                        cache.as_ref(),
                    )
                    .await
                    {
                        if let Some(c) = cache.as_ref() {
                            c.upsert_messages(account_id, &path, &messages);
                        }
                        emit(WorkerEvent::Messages {
                            folder_id,
                            messages,
                        });
                    }
                }
                if let Some(c) = cache.as_ref() {
                    c.save_attachment_meta(account_id, &path, uid, &[]);
                    if let Some(new_uid) = new_uid {
                        cache_rewritten(c, account_id, &path, new_uid, &raw);
                    }
                }
                emit(WorkerEvent::AttachmentDeleted {
                    message_id,
                    path,
                    uid,
                    new_uid,
                    name,
                    size,
                });
            }
            Err(e) => {
                let why = i18n_f("Could not remove the attachment: {e}", &[("e", &e.text)]);
                strip_refused(emit, why, message_id, name, size);
                lost |= e.connection;
            }
        }
        After::lost_if(lost)
    }

    /// Move one message to another folder.
    pub(super) async fn on_move_message(&mut self, path: String, uid: u32, dest: String) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let mut lost = false;
        let sess = ensured(session);
        match move_message(sess, &path, uid, &dest).await {
            Ok(created) => {
                if let Some(c) = cache.as_ref() {
                    drop_gmail_label_copies(c, account_id, account, &path, &[uid], &dest);
                    c.delete_message(account_id, &path, uid);
                }
                if created {
                    refresh_folders(account_id, account, sess, cache.as_ref(), emit).await;
                }
            }
            Err(e) => {
                emit(WorkerEvent::error(i18n_f(
                    "Could not move message: {e}",
                    &[("e", &(e).to_string())],
                )));
                lost = true;
            }
        }
        After::lost_if(lost)
    }

    /// Move several messages to another folder in one command.
    pub(super) async fn on_move_messages(
        &mut self,
        path: String,
        uids: Vec<u32>,
        dest: String,
    ) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let mut lost = false;
        let sess = ensured(session);
        match move_messages(sess, &path, &uids, &dest).await {
            Ok(created) => {
                if let Some(c) = cache.as_ref() {
                    drop_gmail_label_copies(c, account_id, account, &path, &uids, &dest);
                    for uid in &uids {
                        c.delete_message(account_id, &path, *uid);
                    }
                }
                if created {
                    refresh_folders(account_id, account, sess, cache.as_ref(), emit).await;
                }
            }
            Err(e) => {
                tracing::warn!(
                    "move failed: {path:?} -> {dest:?} set={} ({e})",
                    uid_set(&uids)
                );
                emit(WorkerEvent::error(i18n_f(
                    "Could not move {len} messages: {e}",
                    &[("len", &(uids.len()).to_string()), ("e", &(e).to_string())],
                )));
                lost = true;
            }
        }
        // Always signal completion so the UI's bulk spinner clears.
        emit(WorkerEvent::BulkComplete);
        After::lost_if(lost)
    }

    /// Put moved messages back, found by Message-ID where the move left them.
    pub(super) async fn on_undo_move(
        &mut self,
        path: String,
        dest: String,
        dest_folder_id: u32,
        message_ids: Vec<String>,
    ) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let use_envelope = &mut self.use_envelope;
        let mut uids: Vec<u32> = Vec::new();
        let mut failed: Option<String> = None;
        {
            let sess = ensured(session);
            match find_by_message_ids(sess, &path, &message_ids).await {
                Ok(found) => uids = found,
                Err(e) => failed = Some(e.to_string()),
            }
            if failed.is_none() && !uids.is_empty() {
                if let Err(e) = move_messages(sess, &path, &uids, &dest).await {
                    failed = Some(e.to_string());
                } else if let Some(c) = cache.as_ref() {
                    for uid in &uids {
                        c.delete_message(account_id, &path, *uid);
                    }
                }
            }
        }
        if let Some(e) = failed {
            emit(WorkerEvent::error(i18n_f(
                "Undo failed: {e}",
                &[("e", &(e).to_string())],
            )));
        } else if uids.is_empty() {
            // Not an error the reader needs interrupting for: the
            // step is simply spent (#200).
            tracing::info!("undo: the messages are no longer where that move put them");
        } else {
            // Reload the restored folder so the messages reappear in
            // the list (the app can't restore them optimistically —
            // it never learns their new UIDs).
            if let Ok(messages) = load_messages_retry(
                account_id,
                session,
                account,
                dest_folder_id,
                &dest,
                use_envelope,
                cache.as_ref(),
            )
            .await
            {
                if let Some(c) = cache.as_ref() {
                    c.upsert_messages(account_id, &dest, &messages);
                }
                emit(WorkerEvent::Messages {
                    folder_id: dest_folder_id,
                    messages,
                });
                emit(WorkerEvent::Restored {
                    folder_id: dest_folder_id,
                    message_ids: message_ids.clone(),
                });
            }
        }
        // Undoing something is not news: the messages reappearing in
        // the list is the whole report (#200).
        // Always signal completion — the app spins the refresh
        // indicator while the undo's server work is in flight.
        emit(WorkerEvent::BulkComplete);
        After::Done
    }

    /// Delete messages for good.
    pub(super) async fn on_purge_messages(&mut self, path: String, uids: Vec<u32>) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let mut lost = false;
        let sess = ensured(session);
        // Gmail only takes a label off mail expunged outside Trash,
        // so the mail would sit on in All Mail (#471).
        let trash = gmail_trash(cache.as_ref(), account_id, account).filter(|t| *t != path);
        let result = match &trash {
            Some(trash) => gmail_erase(sess, &path, &uids, trash).await,
            None => purge_messages(sess, &path, &uids).await,
        };
        match result {
            Ok(()) => {
                if let Some(c) = cache.as_ref() {
                    if let Some(trash) = &trash {
                        c.drop_label_copies(account_id, &path, &uids, trash);
                    }
                    for uid in &uids {
                        c.delete_message(account_id, &path, *uid);
                    }
                }
            }
            Err(e) => {
                emit(WorkerEvent::error(i18n_f(
                    "Could not delete permanently: {e}",
                    &[("e", &(e).to_string())],
                )));
                lost = true;
            }
        }
        // Always signal completion so the UI's bulk spinner clears.
        emit(WorkerEvent::BulkComplete);
        After::lost_if(lost)
    }

    /// Delete everything in a folder for good.
    pub(super) async fn on_empty_folder(&mut self, folder_id: u32, path: String) -> After {
        let account_id = self.account_id;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let mut lost = false;
        let sess = ensured(session);
        let found = match sel(sess, &path).await {
            Ok(_) => search_uids(sess, "ALL").await,
            Err(e) => Err(e),
        };
        let result = match found {
            Ok(set) => {
                let uids: Vec<u32> = set.into_iter().collect();
                purge_messages(sess, &path, &uids).await.map(|()| uids)
            }
            Err(e) => Err(e),
        };
        match result {
            Ok(uids) => {
                if let Some(c) = cache.as_ref() {
                    for uid in &uids {
                        c.delete_message(account_id, &path, *uid);
                    }
                }
                tracing::info!("emptied {path}: {} message(s) erased", uids.len());
                emit(WorkerEvent::Messages {
                    folder_id,
                    messages: Vec::new(),
                });
                emit(WorkerEvent::FolderUnread {
                    folder_id,
                    unread: 0,
                });
            }
            Err(e) => {
                emit(WorkerEvent::error(i18n_f(
                    "Could not empty the folder: {e}",
                    &[("e", &e.to_string())],
                )));
                lost = true;
            }
        }
        After::lost_if(lost)
    }

    /// Create a folder.
    pub(super) async fn on_create_folder(&mut self, path: String) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let mut lost = false;
        let sess = ensured(session);
        let made = match create_folder(sess, &path).await {
            Err(async_imap::error::Error::No(why)) => {
                match inside_inbox(cache.as_ref(), account_id, &path) {
                    // A server that didn't say where its folders go, and
                    // refused this one at the top (#554): RFC 3501 has no
                    // way to ask, so the Inbox, the one other place there
                    // is, answers it.
                    Some((h, inside)) => match create_folder(sess, &inside).await {
                        Ok(()) => {
                            let learned = FolderHierarchy::imap(
                                h.delimiter,
                                Some(inside[..inside.len() - path.len()].to_string()),
                            );
                            if let Some(c) = cache.as_ref() {
                                c.save_imap_hierarchy(account_id, &learned);
                            }
                            emit(WorkerEvent::Hierarchy(learned));
                            emit(WorkerEvent::FolderMadeAt {
                                asked: path.clone(),
                                made: inside,
                            });
                            Ok(())
                        }
                        Err(_) => Err(async_imap::error::Error::No(why)),
                    },
                    None => Err(async_imap::error::Error::No(why)),
                }
            }
            other => other,
        };
        match made {
            Ok(()) => refresh_folders(account_id, account, sess, cache.as_ref(), emit).await,
            Err(e) => {
                emit(WorkerEvent::error(i18n_f(
                    "Could not create folder: {e}",
                    &[("e", &(e).to_string())],
                )));
                restore_folders(cache.as_ref(), account_id, emit);
                lost = !matches!(e, async_imap::error::Error::No(_));
            }
        }
        After::lost_if(lost)
    }

    /// Rename or move a folder.
    pub(super) async fn on_rename_folder(&mut self, old_path: String, new_path: String) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let mut lost = false;
        let sess = ensured(session);
        match rename_folder(sess, &old_path, &new_path).await {
            Ok(()) => refresh_folders(account_id, account, sess, cache.as_ref(), emit).await,
            Err(e) => {
                emit(WorkerEvent::error(i18n_f(
                    "Could not move folder: {e}",
                    &[("e", &(e).to_string())],
                )));
                restore_folders(cache.as_ref(), account_id, emit);
                lost = true;
            }
        }
        After::lost_if(lost)
    }

    /// Take a new list of hidden folders (#239) and list the folders again.
    pub(super) async fn on_set_hidden_folders(&mut self, paths: Vec<String>) -> After {
        let account_id = self.account_id;
        let account = &mut self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        account.hidden_folders = paths;
        let sess = ensured(session);
        refresh_folders(account_id, account, sess, cache.as_ref(), emit).await;
        After::Done
    }

    /// Delete a folder, its mail moved to `trash` first when given.
    pub(super) async fn on_delete_folder(&mut self, path: String, trash: Option<String>) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let mut lost = false;
        let sess = ensured(session);
        match delete_folder(sess, &path, trash.as_deref()).await {
            Ok(()) => refresh_folders(account_id, account, sess, cache.as_ref(), emit).await,
            Err(e) => {
                emit(WorkerEvent::error(i18n_f(
                    "Could not delete folder: {e}",
                    &[("e", &(e).to_string())],
                )));
                lost = true;
            }
        }
        After::lost_if(lost)
    }
}
