//! Star, read and tag state: changing it on the server and showing the change
//! before the server confirms it.

use relm4::prelude::*;

use crate::i18n::i18n;
use crate::models::Message;
use crate::ui::message_list::MessageListInput;
use crate::ui::message_view::MessageViewInput;
use crate::ui::message_window::MessageWindowInput;
use crate::worker::MailRequest;

use super::conversation::body_key;
use super::model::AppModel;
use super::pending::PENDING_TAG_MAX;
use super::undo::FlagKind;

/// What the Undo menu calls a star change.
pub(super) fn star_label(starred: bool) -> String {
    if starred {
        i18n("Star")
    } else {
        i18n("Unstar")
    }
}

/// The same for a read/unread change.
pub(super) fn read_label(read: bool) -> String {
    if read {
        i18n("Mark as Read")
    } else {
        i18n("Mark as Unread")
    }
}

/// A sender as a dialog or toast names them: the display name, else the address.
pub(super) fn sender_label(message: &Message) -> String {
    let name = message.from_name.trim();
    if name.is_empty() {
        message.from_addr.clone()
    } else {
        name.to_string()
    }
}

impl AppModel {
    /// What a flag action means: every message of a multi-selection, else
    /// the reply target.
    pub(super) fn selected_targets(&self) -> Vec<Message> {
        let mut targets = self.multi_selection();
        if targets.is_empty() {
            targets.extend(self.reply_target().cloned());
        }
        targets
    }

    /// Whether a message carries the tag with this keyword: the keyword, or
    /// a Gmail label standing for the tag (#374).
    pub(super) fn carries_tag(&self, m: &Message, keyword: &str) -> bool {
        match self
            .tags
            .iter()
            .find(|t| t.keyword.eq_ignore_ascii_case(keyword))
        {
            Some(t) => m.has_tag(t),
            None => m.has_keyword(keyword),
        }
    }

    /// Toggle a tag on `targets`: on all of them, or off all of them once
    /// every one carries it (#313).
    pub(super) fn toggle_tag(&mut self, keyword: String, targets: &[Message]) {
        if targets.is_empty() {
            return;
        }
        let add = !targets.iter().all(|m| self.carries_tag(m, &keyword));
        let what = self.tag_label(&keyword, add);
        self.user_set_flags(what, targets, FlagKind::Tag(keyword), add);
    }

    /// Put a tag on a message or take it off (#71): the server (or the local
    /// store, where the server keeps none), then every copy the UI holds —
    /// the list row, the reader, a pop-out — so nothing waits for a sync.
    pub(super) fn set_tag(&mut self, m: &Message, keyword: &str, add: bool) {
        if self.outbox_item(m.account_id, m.id).is_some() {
            return;
        }
        let Some(path) = self.resolve_folder_path(m) else {
            return;
        };
        self.pending_tags.insert(
            (m.account_id, path.clone(), m.uid, keyword.to_lowercase()),
            (
                add,
                std::time::Instant::now() + PENDING_TAG_MAX,
                keyword.to_string(),
            ),
        );
        self.send_to(
            m.account_id,
            MailRequest::SetKeyword {
                path,
                uid: m.uid,
                message_id: m.message_id.clone(),
                keyword: keyword.to_string(),
                add,
            },
        );
        let mut patched = m.clone();
        // Taking a tag off takes the Gmail labels that stand for it too
        // (#374), or its pill would stay.
        match self
            .tags
            .iter()
            .find(|t| t.keyword.eq_ignore_ascii_case(keyword))
        {
            Some(t) if !add => patched.drop_tag(t),
            _ => patched.set_keyword(keyword, add),
        }
        let (keywords, labels) = (patched.keywords.clone(), patched.labels.clone());
        let (aid, fid, id) = (m.account_id, m.folder_id, m.id);
        let same = |c: &Message| c.account_id == aid && c.id == id;
        let patch = |c: &mut Message| {
            c.keywords = keywords.clone();
            c.labels = labels.clone();
        };
        if let Some(msgs) = self.message_cache.get_mut(&(aid, fid)) {
            msgs.iter_mut().filter(|c| same(c)).for_each(patch);
        }
        if let Some(msgs) = self.unified_slices.get_mut(&(aid, fid)) {
            msgs.iter_mut().filter(|c| same(c)).for_each(patch);
        }
        self.current_thread
            .iter_mut()
            .filter(|c| same(c))
            .for_each(patch);
        if let Some(cur) = self.current.as_mut().filter(|c| same(c)) {
            patch(cur);
        }
        self.message_list.emit(MessageListInput::SetKeywords {
            slot: (aid, fid, id),
            keywords: keywords.clone(),
            labels: labels.clone(),
        });
        self.message_view.emit(MessageViewInput::SetCardKeywords {
            account_id: aid,
            id,
            keywords: keywords.clone(),
            labels: labels.clone(),
        });
        if let Some(p) = self.popouts.get(&(aid, id)) {
            p.controller
                .emit(MessageWindowInput::SetKeywords(keywords, labels));
        }
        // In this tag's own view an untagged message has no row to keep.
        if !add
            && self.tag_view.as_ref().is_some_and(|(_, v)| {
                v.as_deref()
                    .is_some_and(|v| v.eq_ignore_ascii_case(keyword))
            })
        {
            self.message_list.emit(MessageListInput::Remove(id));
        }
    }

    /// Star/unstar a message, updating the server, the list, and the reader.
    pub(super) fn set_star(&mut self, m: &Message, starred: bool) {
        let Some(path) = self.resolve_folder_path(m) else {
            return;
        };
        self.note_flag_star(m, &path, starred);
        self.send_to(
            m.account_id,
            MailRequest::SetFlagged {
                path,
                uid: m.uid,
                flagged: starred,
            },
        );
        self.message_list.emit(MessageListInput::SetStarred {
            slot: (m.account_id, m.folder_id, m.id),
            starred,
        });
        for tm in self
            .current_thread
            .iter_mut()
            .filter(|tm| tm.id == m.id && tm.account_id == m.account_id)
        {
            // Without this the toolbar's reply_target reads a stale copy and
            // a second click re-stars instead of clearing.
            tm.starred = starred;
        }
        if let Some(cur) = self.current.as_mut() {
            if cur.id == m.id && cur.account_id == m.account_id {
                cur.starred = starred;
            }
        }
        if self.current_thread.len() > 1 {
            // A conversation is open: re-showing `current` alone here used to
            // collapse the reader to a single message. Patch the card's star
            // button in place instead.
            self.message_view.emit(MessageViewInput::SetCardStar {
                account_id: m.account_id,
                id: m.id,
                starred,
            });
        } else if self
            .current
            .as_ref()
            .is_some_and(|c| c.id == m.id && c.account_id == m.account_id)
        {
            let current = self.current.clone();
            self.show_message(current, false);
        }
        if let Some(p) = self.popouts.get(&(m.account_id, m.id)) {
            p.controller.emit(MessageWindowInput::SetStarred(starred));
        }
    }

    /// Mark a message read/unread, updating the server, list, badges, and reader.
    pub(super) fn set_read(&mut self, m: &Message, read: bool) {
        // No-op if it's already in the requested state.
        if read != m.unread {
            return;
        }
        // A draft is neither read nor unread, and the Drafts chip counts
        // drafts, not unread mail: nothing to store, nothing to adjust.
        if self.is_drafts_folder(m.account_id, m.folder_id) {
            return;
        }
        let Some(path) = self.resolve_folder_path(m) else {
            return;
        };
        self.pending_seen.insert(
            (m.account_id, path.clone(), m.uid),
            (read, std::time::Instant::now()),
        );
        self.send_to(
            m.account_id,
            MailRequest::SetSeen {
                path,
                uid: m.uid,
                seen: read,
            },
        );
        if read {
            // The mail was read (or marked read) in the app — the desktop
            // notification for this account's new mail is answered, clear it
            // instead of leaving it stranded in the panel (issue #41).
            crate::desktop::notify::withdraw_mail(m.account_id);
        }
        self.message_list.emit(MessageListInput::SetRead {
            slot: (m.account_id, m.folder_id, m.id),
            read,
        });
        self.set_cached_unread(m.account_id, m.folder_id, m.id, !read);
        if let Some(p) = self.popouts.get(&(m.account_id, m.id)) {
            p.controller.emit(MessageWindowInput::SetUnread(!read));
        }
        if let Some(n) = self.folder_unread.get_mut(&(m.account_id, m.folder_id)) {
            if read {
                *n = n.saturating_sub(1);
            } else {
                *n += 1;
            }
        }
        if let Some(cur) = self.current.as_mut() {
            if cur.id == m.id && cur.account_id == m.account_id {
                cur.unread = !read;
            }
        }
        // The reader shows a conversation's unread marks, so a message whose
        // state changed while its thread is open has to be told about it —
        // otherwise marking one unread does nothing visible until the thread is
        // opened again.
        let in_thread = self
            .current_thread
            .iter_mut()
            .filter(|tm| tm.id == m.id && tm.account_id == m.account_id)
            .fold(false, |_, tm| {
                tm.unread = !read;
                true
            });
        if in_thread {
            self.remember_thread();
        }
        if in_thread && self.current_thread.len() > 1 {
            if read {
                // Reading clears the card's dot — the only visible change, so
                // it is dropped in place via JS. Reloading the whole document
                // here made the cards visibly resettle on every click.
                self.message_view.emit(MessageViewInput::ClearDot {
                    account_id: m.account_id,
                    id: m.id,
                });
            } else {
                // Marked unread deliberately: the reader keeps the mark until
                // this conversation is opened afresh, so a message sitting in
                // view can't immediately undo it.
                self.message_view.emit(MessageViewInput::SuppressAutoRead {
                    account_id: m.account_id,
                    id: m.id,
                });
            }
        }
        self.push_unread_counts();
    }

    /// Fill a message's body from the cache if it isn't already loaded, so
    /// reply/forward from the context menu can quote it when available.
    pub(super) fn with_cached_body(&self, mut m: Message) -> Message {
        if m.body.is_empty() {
            if let Some(b) = self.body_cache.get(&body_key(&m)) {
                m.body = b.clone();
            }
        }
        m
    }

    /// Keep a star set or taken off here in `unified_starred_rows`, so the
    /// unified Starred view opens with it before the index is read again.
    fn note_flag_star(&mut self, m: &Message, path: &str, starred: bool) {
        let Some(rows) = self.unified_starred_rows.get_mut(&m.account_id) else {
            return;
        };
        match rows.iter_mut().find(|(p, r)| p == path && r.uid == m.uid) {
            Some((_, r)) => r.starred = starred,
            None if starred => {
                let mut r = m.clone();
                r.starred = true;
                rows.push((path.to_string(), r));
            }
            None => {}
        }
    }

    /// Mark a cached message read in every list that holds it, so unread badges
    /// update immediately without waiting for the next server sync.
    pub(super) fn mark_cached_read(&mut self, account_id: u32, folder_id: u32, message_id: u32) {
        self.set_cached_unread(account_id, folder_id, message_id, false);
    }

    /// Set a cached message's unread flag in every list that holds it: its
    /// own folder's, and the unified view's slice of it. Only that folder:
    /// a UID names a different message in each folder, and marking every
    /// folder's message with this UID read set the wrong mail read (#333).
    fn set_cached_unread(
        &mut self,
        account_id: u32,
        folder_id: u32,
        message_id: u32,
        unread: bool,
    ) {
        let lists = [
            self.message_cache
                .get_mut(&(account_id, folder_id))
                .map(|c| &mut **c),
            self.unified_slices.get_mut(&(account_id, folder_id)),
        ];
        for msgs in lists.into_iter().flatten() {
            if let Some(m) = msgs.iter_mut().find(|m| m.id == message_id) {
                m.unread = unread;
            }
        }
        if let Some(path) = self.folder_path(account_id, folder_id) {
            if let Some(rows) = self.unified_starred_rows.get_mut(&account_id) {
                if let Some((_, m)) = rows
                    .iter_mut()
                    .find(|(p, m)| *p == path && m.id == message_id)
                {
                    m.unread = unread;
                }
            }
        }
    }
}
