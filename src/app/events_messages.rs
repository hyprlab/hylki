//! Worker events about message lists: a folder's messages, more of them,
//! and messages gone.

use std::collections::{HashMap, HashSet};

use adw::prelude::*;
use relm4::prelude::*;

use crate::models::{FolderKind, Message};
use crate::ui::message_list::MessageListInput;
use crate::ui::notifications::NotifyInput;
use crate::worker::MailRequest;

use super::conversation::{body_key, next_after_vanish};
use super::model::AppModel;
use super::msg::AppMsg;

impl AppModel {
    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_events_messages(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::Messages {
                account_id,
                folder_id,
                messages,
            } => {
                self.note_snoozed_listing(account_id, folder_id, messages.len());
                self.on_messages(account_id, folder_id, messages, sender)
            }

            AppMsg::MessagesAppend {
                account_id,
                folder_id,
                messages,
            } => self.on_messages_append(account_id, folder_id, messages),

            AppMsg::BackfillDone {
                account_id,
                folder_id,
            } => {
                self.indexed_folders.insert((account_id, folder_id));
                self.push_index_complete();
            }

            AppMsg::MessageGone {
                account_id,
                message_id,
                path,
                uid,
            } => self.on_message_gone(account_id, message_id, path, uid, sender),

            AppMsg::RefsRepaired {
                account_id,
                folder_id,
            } => {
                // Only the folder on screen, and only its cached copy: the repair
                // rewrote rows on disk, and re-reading is what lets the list see
                // the conversations they now form.
                let showing = self
                    .selected
                    .as_ref()
                    .is_some_and(|sel| sel.account_id == account_id && sel.folder_id == folder_id);
                if showing || self.unified {
                    if let Some(path) = self.folder_path(account_id, folder_id) {
                        self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
                    }
                }
            }
            _ => unreachable!("not routed to on_events_messages"),
        }
    }

    /// Additional indexed summaries from the background backfill (search index).
    fn on_messages_append(&mut self, account_id: u32, folder_id: u32, messages: Vec<Message>) {
        // Background backfill: grow the folder's search index without
        // disturbing the current view (no title/query reset).
        let messages = self.merge_local_tags(account_id, messages);
        let messages = self.drop_pending_moves(account_id, folder_id, messages);
        let messages = self.apply_blacklist(account_id, folder_id, messages);
        // Only onto a folder listed whole already. An entry started
        // here would hold just the newest arrivals, and a search or
        // a unified view would take it for the folder (#472); one
        // not open has its rows on disk, where the worker put them.
        let Some(entry) = self.message_cache.get_mut(&(account_id, folder_id)) else {
            return;
        };
        let existing: std::collections::HashSet<u32> = entry.iter().map(|m| m.uid).collect();
        let fresh: Vec<Message> = messages
            .into_iter()
            .filter(|m| !existing.contains(&m.uid))
            .collect();
        if fresh.is_empty() {
            return;
        }
        entry.extend(fresh.iter().cloned());
        // Feed the visible list so search covers the new messages live.
        if self.unified {
            if self.is_unified_target(account_id, folder_id) {
                // A folder read where it is has them already.
                if !self.unified_unfilled.contains(&(account_id, folder_id)) {
                    self.unified_slices
                        .entry((account_id, folder_id))
                        .or_default()
                        .extend(fresh.iter().cloned());
                }
                self.message_list
                    .emit(MessageListInput::AppendMessages { messages: fresh });
            }
        } else if let Some(sel) = self.selected.as_ref() {
            if sel.account_id == account_id && sel.folder_id == folder_id {
                self.message_list
                    .emit(MessageListInput::AppendMessages { messages: fresh });
            }
        }
    }

    /// A message the cache listed is no longer in `path` on the server.
    fn on_message_gone(
        &mut self,
        account_id: u32,
        message_id: u32,
        path: String,
        uid: u32,
        sender: ComponentSender<Self>,
    ) {
        // A conversation member the cache still listed turned out to
        // be gone from the server (#257): take it out of the open
        // conversation rather than show it as a blank card.
        let folder = self.folder_by_path(account_id, &path).map(|f| f.id);
        if let Some(fid) = folder {
            if let Some(msgs) = self.message_cache.get_mut(&(account_id, fid)) {
                msgs.retain(|m| m.uid != uid);
            }
        }
        // The message the reader was opened on stays: that one is
        // the list's to settle, by its own sync.
        let opened = self
            .current
            .as_ref()
            .map(|c| (c.account_id, c.id, c.folder_id));
        let before = self.current_thread.len();
        self.current_thread.retain(|m| {
            let gone = m.account_id == account_id
                && m.id == message_id
                && folder.is_none_or(|fid| m.folder_id == fid);
            !gone || opened == Some((m.account_id, m.id, m.folder_id))
        });
        // Remembered conversations may hold it too, and the list's
        // badges counted it.
        self.forget_threads(account_id);
        self.message_list
            .emit(MessageListInput::RecheckThreadSummaries(account_id));
        if self.current_thread.len() != before {
            if self.current_thread.len() > 1 {
                self.queue_thread_render(&sender);
            } else {
                // Nothing left to make a conversation of.
                self.current_thread.clear();
                let current = self.current.clone();
                let loading = current.as_ref().is_some_and(|c| c.body.is_empty());
                self.show_message(current, loading);
            }
        }
    }

    /// A folder's messages, from the worker's cache or from the server.
    fn on_messages(
        &mut self,
        account_id: u32,
        folder_id: u32,
        messages: Vec<Message>,
        sender: ComponentSender<Self>,
    ) {
        self.notifications.emit(NotifyInput::ClearConnectivity);
        let (messages, filed) = self.prepare_listed(account_id, folder_id, messages);
        self.carry_moved_bodies(account_id, &messages);
        self.follow_renumbered(account_id, folder_id, &messages);
        let (vanished, next_after_vanish) =
            self.vanished_from_reader(account_id, folder_id, &messages);
        self.notify_new_mail(account_id, folder_id, &messages, &filed);
        self.wake_on_reply(account_id, folder_id, &messages);
        // Cache for instant display when revisiting this folder.
        // The worker answers a load from its cache first and from the
        // server after, and the view was seeded from the same cache
        // when it opened — so most arrivals change nothing. Only a
        // changed list is worth re-threading and re-emitting: a
        // rebuild of a large (or merged) list is not free, and it
        // used to happen several times over per visit.
        let unchanged = self
            .message_cache
            .get(&(account_id, folder_id))
            .map(|c| &**c)
            == Some(&messages);
        self.note_inbox_arrivals(account_id, folder_id, &messages);
        // The notified mail was read somewhere else, on the phone or
        // in another client: the notification goes too (#333).
        if let Some((f, mid)) = crate::desktop::notify::posted_for(account_id) {
            if f == folder_id && messages.iter().any(|m| m.id == mid && !m.unread) {
                crate::desktop::notify::withdraw_mail(account_id);
            }
        }
        // An Archive answers each visit with the same tens of
        // thousands of messages: copying them over themselves only
        // churned the heap (#504).
        if !unchanged {
            self.cache_listed(account_id, folder_id, &messages);
        }
        self.resume_pickers(account_id, folder_id, &sender);
        if !unchanged {
            self.after_list_changed(account_id, &sender);
        }
        self.refile_carried_threads(account_id, &messages);
        self.show_listed(account_id, folder_id, messages, unchanged, &sender);
        if !unchanged && self.flag_starred_accounts().contains(&account_id) {
            // The index just took this folder's flags: read them
            // again now for the view on show, else once a sync's
            // burst of folders has passed.
            if self.starred_by_flag().is_empty() {
                self.refresh_unified_starred_soon(&sender);
            } else {
                self.refresh_unified_starred(&sender);
            }
        }
        self.schedule_smart_counts(&sender);
        // The reader's message was removed by this sync (deleted/moved on
        // another device). Clear it right away so nothing stale lingers, then
        // advance to whatever now sits in its place if there is one — the
        // SelectAndLoad runs after SetMessages, so the target row exists.
        if vanished {
            sender.input(AppMsg::ClearReader);
            if let Some(key) = next_after_vanish {
                self.message_list.emit(MessageListInput::SelectAndLoad(key));
            }
        }
        // Refresh unread badges with the freshly-synced counts.
        self.push_unread_counts();
    }

    /// A folder's list as it arrived, with what this session knows laid
    /// over it: local tags, pending moves dropped, blacklisted senders and
    /// the filters applied (what they filed elsewhere comes back beside
    /// the list), then the pending read, answered and tag marks.
    fn prepare_listed(
        &mut self,
        account_id: u32,
        folder_id: u32,
        messages: Vec<Message>,
    ) -> (Vec<Message>, Vec<(Message, String)>) {
        let messages = self.merge_local_tags(account_id, messages);
        // Mail already moved or deleted here, in a list fetched before
        // the worker got to the move.
        let messages = self.drop_pending_moves(account_id, folder_id, messages);
        // Auto-delete blacklisted senders from the inbox before anything
        // else sees them.
        let messages = self.apply_blacklist(account_id, folder_id, messages);
        let (messages, filed) = self.apply_filters(account_id, folder_id, messages);
        // A list fetched ahead of a read mark still in the worker's
        // queue shows the message unread again; keep the app's state.
        let messages = self.apply_pending_seen(account_id, folder_id, messages);
        let messages = self.apply_pending_answers(account_id, folder_id, messages);
        let messages = self.apply_pending_tags(account_id, folder_id, messages);
        (messages, filed)
    }

    /// A message a move has just brought home arrives with a new
    /// UID — that is what a move does — which leaves its body filed
    /// under the id the old one became. Move the body across before
    /// anything asks for it (#200).
    fn carry_moved_bodies(&mut self, account_id: u32, messages: &[Message]) {
        if !self.carried_bodies.is_empty() {
            for m in messages.iter().filter(|m| !m.message_id.is_empty()) {
                if let Some(body) = self
                    .carried_bodies
                    .remove(&(account_id, m.message_id.clone()))
                {
                    self.body_cache.insert(body_key(m), body);
                }
            }
        }
    }

    /// The reader may still be holding the copy from before the
    /// move. Point it at the row that is really there now: every
    /// action on it addresses a UID, and the check just below would
    /// otherwise read it as a message deleted from under us.
    fn follow_renumbered(&mut self, account_id: u32, folder_id: u32, messages: &[Message]) {
        let renumbered: HashMap<&str, (u32, u32)> = messages
            .iter()
            .filter(|m| !m.message_id.is_empty())
            .map(|m| (m.message_id.as_str(), (m.uid, m.id)))
            .collect();
        let renumber = |m: &mut Message| {
            if m.account_id != account_id || m.folder_id != folder_id {
                return;
            }
            if let Some(&(uid, id)) = renumbered.get(m.message_id.as_str()) {
                m.uid = uid;
                m.id = id;
            }
        };
        if let Some(cur) = self.current.as_mut() {
            renumber(cur);
        }
        for tm in self.current_thread.iter_mut() {
            renumber(tm);
        }
    }

    /// Did this sync remove the message currently open in the reader
    /// (deleted/moved on another device)? Scope the check to the reader's
    /// own folder so a folder switch or another folder's sync doesn't
    /// count. Capture where it sat so we can advance to that slot.
    fn vanished_from_reader(
        &self,
        account_id: u32,
        folder_id: u32,
        messages: &[Message],
    ) -> (bool, Option<(u32, u32)>) {
        // The open message's uid, when the new list no longer has it.
        let vanished_uid = self
            .current
            .as_ref()
            .filter(|c| {
                c.account_id == account_id
                    && c.folder_id == folder_id
                    && !messages.iter().any(|m| m.uid == c.uid)
            })
            .map(|c| c.uid);
        let vanished = vanished_uid.is_some();

        let next_after_vanish = if let Some(cur_uid) = vanished_uid {
            next_after_vanish(
                self.message_cache
                    .get(&(account_id, folder_id))
                    .map(|c| &**c),
                messages,
                cur_uid,
            )
        } else {
            None
        };
        (vanished, next_after_vanish)
    }

    /// Desktop-notify for genuinely new inbox mail. Only when Hylki
    /// isn't the active window (no point notifying about mail you're
    /// watching arrive), only for the Inbox, and never on the first load
    /// of a folder (no prior cache) — that would fire for every existing
    /// message on startup. "New" = unread and not in the previous sync.
    /// Mail a filter just filed elsewhere still counts (#47 feedback):
    /// it is new mail even though it never lands in the inbox list.
    /// With the window in front, the sound alone, when Settings asks
    /// for it there (#337): see announce_new_mail.
    fn notify_new_mail(
        &self,
        account_id: u32,
        folder_id: u32,
        messages: &[Message],
        filed: &[(Message, String)],
    ) {
        if self.notifications_enabled
            && (!self.window.is_active() || crate::config::sound_when_focused())
        {
            let is_inbox = self.folder_kind(account_id, folder_id) == Some(FolderKind::Inbox);
            if let (true, Some(old)) = (is_inbox, self.message_cache.get(&(account_id, folder_id)))
            {
                let old_uids: std::collections::HashSet<u32> = old.iter().map(|m| m.uid).collect();
                let fresh: Vec<&Message> = messages
                    .iter()
                    .filter(|m| m.unread && !old_uids.contains(&m.uid))
                    .collect();
                let fresh_filed: Vec<&(Message, String)> = filed
                    .iter()
                    .filter(|(m, _)| m.unread && !old_uids.contains(&m.uid))
                    .collect();
                let others = (fresh.len() + fresh_filed.len()).saturating_sub(1);
                let newest = fresh.iter().max_by_key(|m| m.timestamp);
                let newest_filed = fresh_filed.iter().max_by_key(|(m, _)| m.timestamp);
                match (newest, newest_filed) {
                    // The newest arrival is still in the inbox: anchor
                    // there. Ties go to the inbox copy — its click
                    // target and action buttons actually work.
                    (Some(m), nf) if nf.is_none_or(|(f, _)| f.timestamp <= m.timestamp) => {
                        self.announce_new_mail(
                            account_id,
                            folder_id,
                            m.id,
                            &m.shown_from_name(),
                            &m.subject,
                            others,
                            true,
                        );
                    }
                    // The newest arrival was filed by a filter: anchor
                    // the notification on its destination folder so a
                    // click lands where the mail went. Its buttons stay
                    // off — the message is no longer where Mark as
                    // Read/Archive would look for it.
                    (_, Some((m, dest_path))) => {
                        let dest_id = self
                            .folder_by_path(account_id, dest_path)
                            .map_or(folder_id, |f| f.id);
                        self.announce_new_mail(
                            account_id,
                            dest_id,
                            m.id,
                            &m.shown_from_name(),
                            &m.subject,
                            others,
                            false,
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    /// What the badge counts when it counts arrivals (#431): the
    /// same new inbox mail as the notification above, whatever
    /// the notification settings, and only what is still in the
    /// inbox, as the badge's other choice counts.
    fn note_inbox_arrivals(&mut self, account_id: u32, folder_id: u32, messages: &[Message]) {
        if !self.window.is_active() && self.inbox_of(account_id).is_some_and(|f| f.id == folder_id)
        {
            let old: Option<HashSet<u32>> = self
                .message_cache
                .get(&(account_id, folder_id))
                .map(|old| old.iter().map(|m| m.uid).collect());
            self.arrivals.note(
                account_id,
                folder_id,
                old.as_ref(),
                messages.iter().map(|m| (m.uid, m.unread)),
            );
        }
    }

    /// Keeps a changed list for the next visit. A large folder off screen
    /// is not taken in: the worker lists a folder again after every move
    /// into it.
    fn cache_listed(&mut self, account_id: u32, folder_id: u32, messages: &[Message]) {
        let key = (account_id, folder_id);
        let on_show = self.folders_on_show();
        if self.message_cache.contains_key(&key)
            || !self.too_large_off_screen(key, messages.len(), &on_show)
        {
            self.message_cache.insert(key, messages.to_vec().into());
        }
        self.shed_large_folders();
    }

    /// A draft picker waiting on this folder's list (Send with
    /// Hylki → Continue a Draft…) opens once every Drafts folder
    /// has answered.
    fn resume_pickers(&mut self, account_id: u32, folder_id: u32, sender: &ComponentSender<Self>) {
        if let Some((_, waiting)) = self.pending_draft_pick.as_mut() {
            waiting.remove(&(account_id, folder_id));
            if waiting.is_empty() {
                sender.input(AppMsg::HandOffDraftPickNow);
            }
        }
        if let Some(waiting) = self.pending_template_pick.as_mut() {
            waiting.remove(&(account_id, folder_id));
            if waiting.is_empty() {
                sender.input(AppMsg::TemplatePickNow);
            }
        }
    }

    /// A sync can add a reply to a conversation already
    /// assembled, so what was stored is no longer necessarily
    /// the conversation.
    fn after_list_changed(&mut self, account_id: u32, sender: &ComponentSender<Self>) {
        self.forget_threads(account_id);
        // In a unified view, with the next merge, once for a run
        // of answers; a folder's own list takes them now, in the
        // same pass as its messages.
        if self.unified {
            self.thread_links_due.set(true);
            self.queue_unified_emit(sender);
        } else {
            self.push_thread_links();
        }
        // A sync that brought in a reply also changed how big the
        // conversations are; the badges have to be counted again
        // rather than kept from before it (#222).
        self.message_list
            .emit(MessageListInput::RecheckThreadSummaries(account_id));
    }

    /// Runs after `after_list_changed` forgets the stored conversations,
    /// not before: a conversation carried over
    /// a move (#200) goes back under the id the message now has,
    /// with the member that moved renumbered to match. The reader
    /// paints a remembered conversation with no lookup and no
    /// spinner, which is the whole point of carrying it.
    fn refile_carried_threads(&mut self, account_id: u32, messages: &[Message]) {
        if !self.carried_threads.is_empty() {
            for m in messages.iter().filter(|m| !m.message_id.is_empty()) {
                let Some(mut thread) = self
                    .carried_threads
                    .remove(&(account_id, m.message_id.clone()))
                else {
                    continue;
                };
                for tm in thread.iter_mut() {
                    if tm.account_id == account_id && tm.message_id == m.message_id {
                        tm.uid = m.uid;
                        tm.id = m.id;
                        tm.folder_id = m.folder_id;
                    }
                }
                tracing::debug!(
                    target: "hylki::undo",
                    "re-filed conversation under id {}",
                    m.id,
                );
                self.remember_thread_for(body_key(m), thread);
            }
        }
    }

    /// Hands the list to the view on show: a unified view merges it, the
    /// open folder lists it, a tag or smart view reads the index again.
    fn show_listed(
        &mut self,
        account_id: u32,
        folder_id: u32,
        messages: Vec<Message>,
        unchanged: bool,
        sender: &ComponentSender<Self>,
    ) {
        if self.unified {
            // Accept only the folders the view merges, by recency.
            if self.is_unified_target(account_id, folder_id) {
                let key = (account_id, folder_id);
                let through = self.unified_reads_through();
                let same_slice = unchanged
                    && if through {
                        self.unified_unfilled.contains(&key)
                    } else {
                        self.unified_slices
                            .get(&key)
                            .is_some_and(|s| *s == messages)
                    };
                if !same_slice {
                    if through {
                        self.unified_slices.remove(&key);
                        self.unified_unfilled.insert(key);
                    } else {
                        self.unified_unfilled.remove(&key);
                        self.unified_slices.insert(key, messages);
                    }
                    // Accounts answer one after another, a large or
                    // slow one in several goes: one merge for the lot
                    // that lands together, not one each.
                    self.unified_dirty.set(true);
                    self.queue_unified_emit(sender);
                }
            }
        } else if let Some(sel) = self.selected.as_ref() {
            if sel.account_id == account_id && sel.folder_id == folder_id && !unchanged {
                self.message_list
                    .emit(MessageListInput::SetMessages { messages });
            }
        } else if self.tag_view.is_some() {
            // The index just took this folder's flags: re-read the
            // tag, off the main thread.
            self.refresh_tag_view(sender);
        } else if self.smart_view.is_some() {
            self.refresh_smart_view(sender);
        }
    }
}
