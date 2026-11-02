//! Which messages an action applies to: the open one, the list's
//! selection, or the conversation.

use adw::prelude::*;
use relm4::prelude::*;

use crate::models::{FolderKind, Message};
use crate::ui::message_list::MessageListInput;
use crate::worker::MailRequest;

use super::model::AppModel;

impl AppModel {
    /// The message the toolbar's per-message actions act on: the open message
    /// itself, or — in a conversation — the one highlighted card, when exactly
    /// one is highlighted. `None` greys the buttons out: with no card (or
    /// several) selected there is no way to say which message an action would
    /// mean.
    pub(super) fn reply_target(&self) -> Option<Message> {
        if self.current_thread.len() <= 1 {
            return self.current.clone();
        }
        match self.list_selection.as_slice() {
            [(account_id, id)] => self
                .current_thread
                .iter()
                .find(|m| m.account_id == *account_id && m.id == *id)
                .cloned(),
            _ => None,
        }
    }

    /// Whether actions mean a selection of rows rather than the message
    /// open: several selected, or any checked with Gmail's x, which opens
    /// none of them (#392).
    pub(super) fn many_selected(&self) -> bool {
        self.list_selection.len() > 1 || (self.checked.is_some() && !self.list_selection.is_empty())
    }

    /// The selection is the list's rows, not cards of the open
    /// conversation: the list resolves it, a conversation's row bringing
    /// all of it (#392).
    pub(super) fn rows_selected(&self) -> bool {
        self.many_selected() && !self.selection_from_cards
    }

    /// Whether the toolbar's per-message actions have something to act on:
    /// the reply target, or a selection of several (#313).
    pub(super) fn acts_on_selection(&self) -> bool {
        self.reply_target().is_some() || self.many_selected()
    }

    /// Every message of a selection of more than one, list rows or cards of
    /// the open conversation, for the flag actions to cover them all (#313).
    /// Empty when one message or none is selected: the reply target is then
    /// what an action means. A row the cache no longer holds is left out, as
    /// a move of the selection leaves it out.
    pub(super) fn multi_selection(&self) -> Vec<Message> {
        if !self.many_selected() {
            return Vec::new();
        }
        if !self.selection_from_cards && self.list_slots.len() == self.list_selection.len() {
            return self.rows_at(&self.list_slots);
        }
        self.list_selection
            .iter()
            .filter_map(|&(account_id, id)| self.shown_copy(account_id, id))
            // A queued message has no server flags to set.
            .filter(|m| self.outbox_item(m.account_id, m.id).is_none())
            .collect()
    }

    /// The messages of these list rows, each from its own folder: a UID
    /// alone could find another folder's message, and in the Filters view
    /// every selected row but the open one did (#481).
    pub(super) fn rows_at(&self, slots: &[crate::ui::message_row::Slot]) -> Vec<Message> {
        slots
            .iter()
            .filter_map(|&(account_id, folder_id, id)| {
                self.shown_copy_in(account_id, folder_id, id)
            })
            // A queued message has no server flags to set.
            .filter(|m| self.outbox_item(m.account_id, m.id).is_none())
            .collect()
    }

    /// `shown_copy`, held to one folder.
    pub(super) fn shown_copy_in(
        &self,
        account_id: u32,
        folder_id: u32,
        id: u32,
    ) -> Option<Message> {
        let same =
            |m: &&Message| m.account_id == account_id && m.folder_id == folder_id && m.id == id;
        self.current
            .iter()
            .chain(self.current_thread.iter())
            .find(same)
            .or_else(|| {
                self.message_cache
                    .get(&(account_id, folder_id))
                    .map(|c| &**c)
                    .into_iter()
                    .chain(self.unified_slices.get(&(account_id, folder_id)))
                    .flatten()
                    .find(same)
            })
            .cloned()
    }

    /// The freshest copy of a message the window is showing: the open one,
    /// a card of the open conversation, or the cache's.
    pub(super) fn shown_copy(&self, account_id: u32, id: u32) -> Option<Message> {
        let same = |m: &&Message| m.account_id == account_id && m.id == id;
        self.current
            .iter()
            .chain(self.current_thread.iter())
            .find(same)
            .cloned()
            .or_else(|| self.find_cached_message(account_id, id))
    }

    /// The message a reply, reply-all or forward addresses: the reply
    /// target, except when only the list row is selected over a
    /// conversation. That row stands for the thread's head, its oldest
    /// message, and answering the oldest message of a conversation is never
    /// what was meant (#210) — so the toolbar's reply addresses the
    /// conversation's newest message from someone else (never the user's own
    /// reply by accident), or the newest of all when every message is theirs.
    /// This holds whichever way the reading pane is ordered: "newest first"
    /// only decides where that message is drawn (#165). A highlighted card is
    /// addressed as itself.
    pub(super) fn compose_target(&self) -> Option<Message> {
        let m = self.reply_target()?;
        tracing::debug!(
            target: "hylki::reply",
            "toolbar target: selected {}:{} from={} cards={} thread={} head={}",
            m.account_id, m.id, m.from_addr, self.selection_from_cards,
            self.current_thread.len(), self.thread_star_target(&m),
        );
        if self.selection_from_cards || !self.thread_star_target(&m) {
            return Some(m);
        }
        let picked = self.newest_to_answer(&self.current_thread, m);
        tracing::debug!(
            target: "hylki::reply",
            "toolbar target: answering {}:{} from={}",
            picked.account_id, picked.id, picked.from_addr,
        );
        Some(picked)
    }

    /// Which message of a conversation an untargeted reply answers: the newest
    /// message from someone else, so a reply never lands on the user's own last
    /// word, or the newest of all when every message is theirs. `fallback` is
    /// returned for anything that is not a conversation.
    ///
    /// Shared by every untargeted reply — the reader toolbar, the keyboard
    /// shortcut, and a row's palette or menu — so all of them answer the same
    /// message (#210).
    pub(super) fn newest_to_answer(&self, thread: &[Message], fallback: Message) -> Message {
        if thread.len() <= 1 {
            return fallback;
        }
        let own = self.own_identities(fallback.account_id);
        let newest = |from_others: bool| {
            thread
                .iter()
                .filter(|t| {
                    !from_others || !own.iter().any(|o| t.from_addr.eq_ignore_ascii_case(o))
                })
                .max_by_key(|t| t.timestamp)
                .cloned()
        };
        newest(true).or_else(|| newest(false)).unwrap_or(fallback)
    }

    /// The open-marks-read side effects for the just-selected message (#100):
    /// server flag, list row, cached copy, badges, notification.
    pub(super) fn mark_opened_read(&mut self, m: &Message) {
        let account_id = m.account_id;
        if let Some(path) = self.resolve_folder_path(m) {
            // Held like any other read mark, or a list fetched before the
            // worker stores it shows the message unread again (#255).
            self.pending_seen.insert(
                (account_id, path.clone(), m.uid),
                (true, std::time::Instant::now()),
            );
            self.send_to(
                account_id,
                MailRequest::SetSeen {
                    path,
                    uid: m.uid,
                    seen: true,
                },
            );
        }
        // Reading new mail clears that account's new-mail notification.
        crate::desktop::notify::withdraw_mail(account_id);
        self.message_list
            .emit(MessageListInput::MarkRead((account_id, m.folder_id, m.id)));
        self.mark_cached_read(account_id, m.folder_id, m.id);
        // Optimistically drop the badge by one; the next server count
        // reconciles any drift.
        if let Some(n) = self.folder_unread.get_mut(&(account_id, m.folder_id)) {
            *n = n.saturating_sub(1);
        }
        self.push_unread_counts();
    }

    /// Whether a star toggle (or an untargeted reply) aimed at `m` should act
    /// on the whole open conversation: a thread is open and `m` is the row it
    /// was opened from, which stands for every message in it.
    ///
    /// That row is the head of the conversation *as the folder lists it*, and
    /// not necessarily the oldest message once the members from other folders
    /// have been merged in and the whole re-sorted: a reply of the user's own
    /// pulled in from Sent can precede it. So the message the reader was
    /// opened on is what identifies the row, never `current_thread`'s first
    /// entry (#210). `current` is that message on every path a conversation
    /// is assembled by, including the one where a lone folder message finds
    /// its siblings in other folders.
    pub(super) fn thread_star_target(&self, m: &Message) -> bool {
        self.current_thread.len() > 1
            && self
                .current
                .as_ref()
                .is_some_and(|c| c.account_id == m.account_id && c.id == m.id)
    }

    /// Whether an action from the keyboard or the toolbar means the whole
    /// open conversation (#392): one is open, nothing of several is selected,
    /// and, unless `cards_too`, no card of it has been picked. Gmail's keys
    /// pass `cards_too`: there a card stepped to with n or p is only where
    /// the reader is, and e or Shift+I still act on the conversation.
    pub(super) fn acts_on_conversation(&self, cards_too: bool) -> bool {
        self.current_thread.len() > 1
            && !self.many_selected()
            && (cards_too || !self.selection_from_cards)
    }

    /// What archiving the open conversation moves: the members its row
    /// stands for in the list. A reply the reader brought in from Sent, or
    /// an older part filed elsewhere, stays where it is.
    pub(super) fn conversation_to_archive(&self) -> Vec<Message> {
        self.current_thread
            .iter()
            .filter(|m| self.listed_thread.contains(&(m.account_id, m.id)))
            .cloned()
            .collect()
    }

    /// What marking the open conversation unread marks: every member but the
    /// ones in Sent or Drafts. The user's own messages were never unread,
    /// and marking them so would light Sent's badge.
    pub(super) fn conversation_to_mark_unread(&self) -> Vec<Message> {
        self.current_thread
            .iter()
            .filter(|m| {
                !matches!(
                    self.folder_kind(m.account_id, m.folder_id),
                    Some(FolderKind::Sent | FolderKind::Drafts)
                )
            })
            .cloned()
            .collect()
    }

    /// Run `f` once the window has drawn its next frame.
    pub(super) fn after_paint(&self, f: impl FnOnce() + 'static) {
        match self.window.frame_clock() {
            Some(clock) => {
                clock.request_phase(gtk::gdk::FrameClockPhase::PAINT);
                crate::ui::message_list::after_next_paint(&clock, f);
            }
            None => f(),
        }
    }

    /// Whether the reader toolbar's star shows lit: the target message's own
    /// star, or any member's when the conversation is the target.
    pub(super) fn toolbar_star_lit(&self) -> bool {
        self.reply_target().is_some_and(|m| {
            if self.thread_star_target(&m) {
                self.current_thread.iter().any(|t| t.starred)
            } else {
                m.starred
            }
        })
    }
}
