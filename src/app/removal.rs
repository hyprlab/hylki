//! Taking mail out of a folder: archive, delete, spam, purge, and the
//! optimistic cleanup of the list and caches that goes with it.

use std::collections::{HashMap, HashSet};

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::models::{FolderKind, Message};
use crate::ui::message_list::{BulkAction, MessageListInput};
use crate::ui::message_view::MessageViewInput;
use crate::ui::notifications::NotifyInput;
use crate::worker::MailRequest;

use super::conversation::body_key;
use super::folders::kind_label;
use super::model::AppModel;
use super::msg::AppMsg;
use crate::ui::dialog::confirm_destructive;

/// A bulk move per (account, source path): the destination path, the
/// UIDs and the rows kept for undo.
pub(super) type MoveGroups = HashMap<(u32, String), (String, Vec<u32>, Vec<Message>)>;

impl AppModel {
    /// Whether a message already lives in its account's Trash — where "delete"
    /// can't mean "move to Trash" any more, so it has to mean erase for good.
    pub(super) fn in_trash(&self, m: &Message) -> bool {
        self.folders.get(&m.account_id).is_some_and(|fs| {
            fs.iter()
                .any(|f| f.id == m.folder_id && f.kind == FolderKind::Trash)
        })
    }

    pub(super) fn in_junk(&self, m: &Message) -> bool {
        self.folders.get(&m.account_id).is_some_and(|fs| {
            fs.iter()
                .any(|f| f.id == m.folder_id && f.kind == FolderKind::Junk)
        })
    }

    /// Whether the reader's target sits in Junk: the spam button, shortcut
    /// and menu entry then read "Not Spam" (#168).
    pub(super) fn target_in_junk(&self) -> bool {
        self.reply_target().is_some_and(|m| self.in_junk(&m))
    }

    /// Delete `messages`: the ones still outside Trash are moved there, and any
    /// already in Trash are erased for good once the user confirms.
    pub(super) fn delete_messages(
        &mut self,
        messages: Vec<Message>,
        sender: &ComponentSender<Self>,
    ) {
        let (purge, move_out): (Vec<Message>, Vec<Message>) =
            messages.into_iter().partition(|m| self.in_trash(m));
        if move_out.len() > 1 {
            self.apply_bulk_move(BulkAction::Delete, move_out);
        } else if let Some(m) = move_out.into_iter().next() {
            self.move_to(m, FolderKind::Trash);
        }
        if !purge.is_empty() {
            self.confirm_purge(purge, sender);
        }
    }

    /// Ask before deleting a whole conversation (a selected thread head). Can
    /// be turned off in Preferences (`confirm_thread_delete`).
    pub(super) fn confirm_delete_thread(
        &self,
        messages: Vec<Message>,
        sender: &ComponentSender<Self>,
    ) {
        let n = messages.len();
        let heading = i18n("Delete this conversation?");
        let body = format!(
            "All {n} messages in this conversation will be deleted. \
             You can turn this warning off in Preferences."
        );
        let s = sender.clone();
        confirm_destructive(
            Some(self.window.upcast_ref()),
            &heading,
            &body,
            "delete",
            &i18n("Delete Conversation"),
            move || s.input(AppMsg::DeleteThreadConfirmed(messages)),
        );
    }

    /// Confirm erasing messages that are already in Trash — there's no undo, so
    /// this always asks first.
    pub(super) fn confirm_purge(&self, messages: Vec<Message>, sender: &ComponentSender<Self>) {
        let n = messages.len();
        if n == 0 {
            return;
        }
        let heading = ni18n_f(
            "Delete this message permanently?",
            "Delete {n} messages permanently?",
            n as u32,
            &[("n", &n.to_string())],
        );
        let binned = messages.iter().all(|m| self.in_trash(m));
        let body = match (binned, n == 1) {
            (true, true) => i18n("It is already in Trash, so it will be erased from the server. This can’t be undone."),
            (true, false) => i18n("They are already in Trash, so they will be erased from the server. \
             This can’t be undone."),
            (false, true) => i18n("It will be erased from the server instead of going to Trash. This can’t be undone."),
            (false, false) => i18n("They will be erased from the server instead of going to Trash. This can’t be undone."),
        };
        let s = sender.clone();
        confirm_destructive(
            Some(self.window.upcast_ref()),
            &heading,
            &body,
            "delete",
            &i18n("Delete"),
            move || s.input(AppMsg::PurgeMessages(messages)),
        );
    }

    /// Erase messages from the server for good (flag `\Deleted` + EXPUNGE),
    /// grouped by source folder so each folder takes a single request.
    pub(super) fn purge_messages(&mut self, messages: Vec<Message>) {
        // One card of the open conversation, erased: the rest stays open.
        let leaving = match messages.as_slice() {
            [m] => self
                .leave_conversation(m)
                .map(|head| ((m.account_id, m.id), head)),
            _ => None,
        };
        let mut groups: HashMap<(u32, String), Vec<u32>> = HashMap::new();
        let mut removed_ids = Vec::with_capacity(messages.len());
        for m in &messages {
            let Some(src) = self.resolve_folder_path(m) else {
                continue;
            };
            groups.entry((m.account_id, src)).or_default().push(m.uid);
            self.discard_message_local(m);
            removed_ids.push(m.id);
        }
        self.bulk_pending += groups.len();
        if !groups.is_empty() {
            self.notifications.emit(NotifyInput::SetStatus(format!(
                "Erasing {} message{} on the server…",
                removed_ids.len(),
                if removed_ids.len() == 1 { "" } else { "s" },
            )));
        }
        for ((account_id, path), uids) in groups {
            self.send_to(account_id, MailRequest::PurgeMessages { path, uids });
        }
        self.update_busy_indicator();
        self.message_list.emit(match leaving {
            Some((gone, head)) => MessageListInput::LeaveConversation { gone, head },
            None => MessageListInput::RemoveMany(removed_ids),
        });
        self.push_unread_counts();
    }

    /// Move a message to its account's folder of `kind` (archive/delete).
    pub(super) fn move_to(&mut self, m: Message, kind: FolderKind) {
        if kind == FolderKind::Archive && self.archives_in_place(&m) {
            self.apply_bulk_move(BulkAction::Archive, vec![m]);
            return;
        }
        let Some(dest) = self.folder_path_for(m.account_id, kind) else {
            self.notice_error(i18n_f(
                "No {v1} folder available",
                &[("v1", &(kind_label(kind)).to_string())],
            ));
            return;
        };
        self.move_to_path(m, dest);
    }

    /// Move a message to an explicit destination folder path (used by both the
    /// kind-based actions and drag-and-drop onto a folder).
    pub(super) fn move_to_path(&mut self, m: Message, dest: String) {
        let Some(src) = self.resolve_folder_path(&m) else {
            return;
        };
        if src == dest {
            return; // already in that folder
        }
        self.push_undo_move(m.account_id, &dest, &src, vec![m.clone()]);
        self.send_to(
            m.account_id,
            MailRequest::MoveMessage {
                path: src,
                uid: m.uid,
                dest,
            },
        );
        self.discard_message(&m);
    }

    /// Mark `m` as spam: tag `$Junk` and move it to the account's Junk folder
    /// (so the server's spam filter can learn from it).
    pub(super) fn mark_spam_msg(&mut self, m: Message) {
        let Some(src) = self.resolve_folder_path(&m) else {
            return;
        };
        let Some(dest) = self.folder_path_for(m.account_id, FolderKind::Junk) else {
            self.notice_error(i18n("No Junk folder available for this account"));
            return;
        };
        self.push_undo_move(m.account_id, &dest, &src, vec![m.clone()]);
        self.send_to(
            m.account_id,
            MailRequest::MarkSpam {
                path: src,
                uid: m.uid,
                dest,
            },
        );
        self.discard_message(&m);
    }

    /// Not spam (#168): tell the server (`$NotJunk`, `$Junk` cleared) and
    /// put the message back in its account's Inbox.
    pub(super) fn mark_ham_msg(&mut self, m: Message) {
        let Some(src) = self.resolve_folder_path(&m) else {
            return;
        };
        let Some(dest) = self.folder_path_for(m.account_id, FolderKind::Inbox) else {
            self.notice_error(i18n("No Inbox folder available for this account"));
            return;
        };
        self.push_undo_move_as(m.account_id, i18n("Not Spam"), &dest, &src, vec![m.clone()]);
        self.send_to(
            m.account_id,
            MailRequest::MarkHam {
                path: src,
                uid: m.uid,
                dest,
            },
        );
        self.discard_message(&m);
    }

    /// Mark every message in a folder as read: update server, caches, badges,
    /// and the displayed list.
    pub(super) fn mark_folder_read(&mut self, account_id: u32, folder_id: u32) {
        let Some(path) = self.folder_path(account_id, folder_id) else {
            return;
        };
        // Optimistic in-memory update so the UI reacts instantly.
        if let Some(msgs) = self.message_cache.get_mut(&(account_id, folder_id)) {
            for m in msgs {
                m.unread = false;
            }
        }
        if let Some(msgs) = self.unified_slices.get_mut(&(account_id, folder_id)) {
            for m in msgs {
                m.unread = false;
            }
        }
        self.folder_unread.insert((account_id, folder_id), 0);
        self.send_to(account_id, MailRequest::MarkAllRead { folder_id, path });
        // Everything here is read now — the account's new-mail notification
        // included (issue #41).
        crate::desktop::notify::withdraw_mail(account_id);
        self.refresh_list_display();
        self.push_unread_counts();
    }

    /// Apply a removing bulk action (archive/delete/spam) to many messages at once.
    /// Messages are grouped by (account, source folder) and each group is moved in a
    /// SINGLE `MoveMessages` request (one server-side UID MOVE) — far faster and more
    /// reliable than one request per message, which on a huge mailbox (e.g. Gmail's
    /// All Mail) is slow and drops moves when the connection blips. The list is
    /// updated once (`RemoveMany`); the spinner clears when every group's worker
    /// reports `BulkComplete`.
    pub(super) fn apply_bulk_move(&mut self, action: BulkAction, messages: Vec<Message>) {
        // On Gmail what leaves for the archive from a label is the Inbox
        // copy; the mail stays where it is being looked at (#392).
        let messages: Vec<Message> = if action == BulkAction::Archive {
            let mut seen = HashSet::new();
            messages
                .into_iter()
                .filter_map(|m| {
                    if self.archives_in_place(&m) {
                        self.inbox_copy(&m)
                    } else {
                        Some(m)
                    }
                })
                .filter(|m| seen.insert((m.account_id, m.id)))
                .collect()
        } else {
            messages
        };
        let kind = match action {
            BulkAction::Archive => FolderKind::Archive,
            BulkAction::Delete => FolderKind::Trash,
            BulkAction::Spam => FolderKind::Junk,
            BulkAction::NotSpam | BulkAction::MoveToInbox => FolderKind::Inbox,
            // Non-removing actions never reach here (handled inline).
            BulkAction::MarkRead
            | BulkAction::MarkUnread
            | BulkAction::Flag
            | BulkAction::Unflag
            | BulkAction::DeleteForever
            | BulkAction::Snooze
            | BulkAction::Unsnooze => return,
        };
        // (account, source path) → (dest path, uids, the rows for undo).
        // dest is per-account.
        let mut groups: MoveGroups = HashMap::new();
        let mut removed_ids = Vec::with_capacity(messages.len());
        let moving: HashSet<(u32, u32)> = messages.iter().map(|m| (m.account_id, m.id)).collect();
        let mut missing_dest = false;
        for m in &messages {
            let Some(src) = self.resolve_folder_path(m) else {
                continue;
            };
            let Some(dest) = self.folder_path_for(m.account_id, kind) else {
                missing_dest = true;
                continue;
            };
            if src == dest {
                continue;
            }
            let slot = groups
                .entry((m.account_id, src))
                .or_insert_with(|| (dest, Vec::new(), Vec::new()));
            slot.1.push(m.uid);
            slot.2.push(m.clone());
            if self.discard_moved(m, &moving) {
                removed_ids.push(m.id);
            }
        }
        if missing_dest {
            self.notice_error(i18n_f(
                "No {v1} folder available for some messages",
                &[("v1", &(kind_label(kind)).to_string())],
            ));
        }
        self.bulk_pending += groups.len();
        if !groups.is_empty() {
            let count: usize = groups.values().map(|g| g.1.len()).sum();
            self.notifications.emit(NotifyInput::SetStatus(format!(
                "Moving {} message{} to {} on the server…",
                count,
                if count == 1 { "" } else { "s" },
                kind_label(kind),
            )));
        }
        for ((account_id, src), (dest, uids, rows)) in groups {
            if action == BulkAction::NotSpam {
                self.push_undo_move_as(account_id, i18n("Not Spam"), &dest, &src, rows);
            } else {
                self.push_undo_move(account_id, &dest, &src, rows);
            }
            let req = if action == BulkAction::NotSpam {
                MailRequest::MarkHamMany {
                    path: src,
                    uids,
                    dest,
                }
            } else {
                MailRequest::MoveMessages {
                    path: src,
                    uids,
                    dest,
                }
            };
            self.send_to(account_id, req);
        }
        self.update_busy_indicator();
        self.message_list
            .emit(MessageListInput::RemoveMany(removed_ids));
        self.push_unread_counts();
    }

    /// Whether the account is Gmail, where every folder but the Inbox, Trash
    /// and Spam is a label.
    pub(super) fn gmail_labels(&self, account_id: u32) -> bool {
        self.saved_account_config(account_id).is_some_and(|c| {
            c.protocol == config::Protocol::Imap
                && crate::worker::has_gmail_labels(
                    c,
                    self.folders
                        .get(&account_id)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                )
        })
    }

    /// Whether archiving `m` takes it out of the Inbox rather than out of
    /// the folder it is listed in: on Gmail, from a label, Starred, Sent or
    /// All Mail, as Gmail's own Archive does (#392). Moving it from a label
    /// to All Mail took the label off and hid it.
    pub(super) fn archives_in_place(&self, m: &Message) -> bool {
        self.gmail_labels(m.account_id)
            && matches!(
                self.folder_kind(m.account_id, m.folder_id),
                Some(
                    FolderKind::Starred
                        | FolderKind::Sent
                        | FolderKind::Archive
                        | FolderKind::Custom
                )
            )
    }

    /// The Inbox's copy of a Gmail message listed under another label, if
    /// the Inbox holds it.
    pub(super) fn inbox_copy(&self, m: &Message) -> Option<Message> {
        if m.message_id.is_empty() {
            return None;
        }
        let key = (m.account_id, self.inbox_of(m.account_id)?.id);
        self.message_cache
            .get(&key)
            .map(|c| &**c)
            .into_iter()
            .chain(self.unified_slices.get(&key))
            .flatten()
            .find(|x| x.message_id == m.message_id)
            .cloned()
    }

    /// Optimistic local cleanup when a message leaves the current folder: close its
    /// popout, drop it from the in-memory caches and the unread count, and clear the
    /// reader if it was the viewed message. Does NOT touch the list widget or push
    /// unread counts — the caller does that (single delete via `discard_message`;
    /// bulk via one `RemoveMany` + one push in `apply_bulk_move`).
    pub(super) fn discard_message_local(&mut self, m: &Message) {
        self.forget_threads(m.account_id);
        if let Some(p) = self.popouts.get(&(m.account_id, m.id)) {
            p.window.close();
        }
        if let Some(msgs) = self.unified_slices.get_mut(&(m.account_id, m.folder_id)) {
            msgs.retain(|x| x.uid != m.uid);
        }
        if let Some(msgs) = self.message_cache.get_mut(&(m.account_id, m.folder_id)) {
            msgs.retain(|x| x.uid != m.uid);
        }
        if let Some(path) = self.folder_path(m.account_id, m.folder_id) {
            if let Some(rows) = self.unified_starred_rows.get_mut(&m.account_id) {
                rows.retain(|(p, x)| *p != path || x.uid != m.uid);
            }
        }
        // The Drafts chip counts every draft, so leaving that folder always
        // drops it by one; elsewhere only unread mail is counted.
        if m.unread || self.is_drafts_folder(m.account_id, m.folder_id) {
            if let Some(n) = self.folder_unread.get_mut(&(m.account_id, m.folder_id)) {
                *n = n.saturating_sub(1);
            }
        }
        if self
            .current
            .as_ref()
            .is_some_and(|c| c.id == m.id && c.account_id == m.account_id)
        {
            // Drop the reader's view state, but DON'T blank it here: the list's
            // Remove handler advances to the next message (or emits SelectionCleared
            // when the folder is empty), which drives the reader. Blanking now would
            // flash "No message selected" before the next one loads.
            self.current = None;
            self.current_thread.clear();
            self.attachments.clear();
            self.sync_attachment_drawer();
            self.attachments_loading = false;
        }
    }

    /// Remove a handled message from the list, caches, and badges. Clears the
    /// reader only if that message was the one open. Shared by archive/delete/spam.
    pub(super) fn discard_message(&mut self, m: &Message) {
        if let Some(head) = self.leave_conversation(m) {
            // The conversation moved on first, so the message is no longer
            // the one the reader was opened on and nothing here closes it.
            self.discard_message_local(m);
            self.message_list.emit(MessageListInput::LeaveConversation {
                gone: (m.account_id, m.id),
                head,
            });
            self.push_unread_counts();
            return;
        }
        self.discard_message_local(m);
        self.message_list.emit(MessageListInput::Remove(m.id));
        self.push_unread_counts();
    }

    /// `m` moved out of its folder with the messages `moving`, all of them
    /// taken out of the caches here. Returns whether its list row goes too:
    /// a card of the open conversation moved without the rest of the
    /// conversation's row leaves the conversation instead, as a deleted card
    /// does, and the row stays for what is left.
    pub(super) fn discard_moved(&mut self, m: &Message, moving: &HashSet<(u32, u32)>) -> bool {
        let rest_stays = self.listed_thread.iter().any(|k| !moving.contains(k));
        let head = if rest_stays {
            self.leave_conversation(m)
        } else {
            None
        };
        self.discard_message_local(m);
        match head {
            Some(head) => {
                self.message_list.emit(MessageListInput::LeaveConversation {
                    gone: (m.account_id, m.id),
                    head,
                });
                false
            }
            None => true,
        }
    }

    /// One card of the open conversation leaving its folder: the card goes
    /// and the rest of the conversation stays open. Removing the message as
    /// a list row closed the whole conversation when it was the one the
    /// conversation was opened on, and left its card on screen when it was
    /// not. Returns the message the list's row now stands for, or `None`
    /// when `m` is no such card or nothing of the folder's would be left.
    fn leave_conversation(&mut self, m: &Message) -> Option<(u32, u32)> {
        if self.current_thread.len() < 2 {
            return None;
        }
        let key = (m.account_id, m.id);
        let pos = self
            .current_thread
            .iter()
            .position(|t| (t.account_id, t.id) == key && t.folder_id == m.folder_id)?;
        // The list heads a row with its oldest message in the folder, which
        // is the one the reader is opened on.
        let head = self
            .current_thread
            .iter()
            .filter(|t| (t.account_id, t.id) != key)
            .find(|t| self.listed_thread.contains(&(t.account_id, t.id)))?
            .clone();
        self.current_thread.remove(pos);
        self.listed_thread.retain(|k| *k != key);
        self.list_selection.retain(|k| *k != key);
        self.list_slots
            .retain(|&(a, f, id)| (a, f, id) != (m.account_id, m.folder_id, m.id));
        self.list_heard.retain(|k| *k != key);
        let was_current = self
            .current
            .as_ref()
            .is_some_and(|c| (c.account_id, c.id) == key);
        if was_current {
            self.thread_key = Some(body_key(&head));
            self.current = Some(head.clone());
        }
        // The card picked in the reader was the one that went: the selection
        // is the conversation's row again, as when it was opened, so the
        // toolbar still has a message to act on.
        if let Some(c) = self
            .current
            .as_ref()
            .filter(|_| self.list_selection.is_empty())
        {
            self.list_selection = vec![(c.account_id, c.id)];
            self.list_slots = vec![(c.account_id, c.folder_id, c.id)];
            self.list_heard = self.list_selection.clone();
            self.selection_from_cards = false;
            self.message_view.emit(MessageViewInput::SetSelectedCards(
                self.list_selection.clone(),
            ));
        }
        if self.current_thread.len() > 1 {
            self.thread_painted = true;
            self.show_thread();
        } else {
            self.current_thread.clear();
            self.thread_key = None;
            let current = self.current.clone();
            let loading = current.as_ref().is_some_and(|c| c.body.is_empty());
            self.show_message(current, loading);
        }
        self.refresh_thread_attachments();
        Some((head.account_id, head.id))
    }
}
