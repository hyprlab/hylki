//! Selecting a message: what the reader shows, read marks, and the
//! conversation it belongs to.

use relm4::prelude::*;

use crate::config;
use crate::models::{thread_ids, Message, ThreadSummary};
use crate::ui::contacts_page::ContactsPageInput;
use crate::ui::message_list::{DateBound, MessageListInput, QuickFilters};
use crate::ui::message_view::MessageViewInput;
use crate::worker::MailRequest;

use super::conversation::{body_key, THREAD_BODY_WAIT};
use super::hand_off::HandOffFiles;
use super::model::AppModel;
use super::msg::AppMsg;

/// What the reader shows, as a selection compares it with what it is to
/// show: account, Message-ID, body, unread, star and keywords.
type Showing = (u32, String, String, bool, bool, Vec<String>);

/// Whether the message to show is already on screen, pixel for pixel. A
/// message without a Message-ID never is.
fn same_as_showing(showing: Option<Showing>, display: &Message) -> bool {
    showing.is_some_and(|(aid, mid, body, unread, starred, keywords)| {
        (aid, &mid, &body, unread, starred, &keywords)
            == (
                display.account_id,
                &display.message_id,
                &display.body,
                display.unread,
                display.starred,
                &display.keywords,
            )
            && !mid.is_empty()
    })
}

impl AppModel {
    /// Blanks the reader: nothing open, no conversation, no attachments.
    pub(super) fn blank_reader(&mut self) {
        self.current = None;
        self.current_thread.clear();
        self.attachments.clear();
        self.attachments_loading = false;
        self.sync_attachment_drawer();
        self.show_message(None, false);
    }

    /// Blanks the reader and shows the list loading, while the folder on
    /// screen is renamed, emptied, hidden or deleted under it. The
    /// attachments are left as they are.
    pub(super) fn blank_for_folder_change(&mut self) {
        self.current = None;
        self.current_thread.clear();
        self.show_message(None, false);
        self.message_list.emit(MessageListInput::SetLoading);
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_reader_select(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::OpenedByKey { already_open } => {
                if already_open {
                    self.message_view.emit(MessageViewInput::FocusBody);
                } else {
                    self.reader_focus_pending = true;
                }
            }
            AppMsg::ClearReader => {
                self.listed_thread.clear();
                self.blank_reader();
            }
            AppMsg::MessageSelected {
                message: m,
                thread,
                solo,
                ahead,
            } => {
                self.on_message_selected(m, thread, solo, sender);
                self.fetch_remote_ahead(&ahead);
            }

            AppMsg::ThreadGrew { message: m, thread } => self.on_thread_grew(m, thread),

            AppMsg::Related {
                account_id,
                message_id,
                messages,
            } => {
                if self
                    .current
                    .as_ref()
                    .is_some_and(|c| c.account_id == account_id && c.id == message_id)
                {
                    self.thread_related_pending = false;
                }
                let found = messages.len();
                self.merge_related(account_id, message_id, messages);
                tracing::debug!(
                    target: "hylki::undo",
                    "related for {account_id}:{message_id}: {found} in the cache, conversation now {}",
                    self.current_thread.len(),
                );
                // One render for the settled conversation, rather than one here
                // and another for whatever this brought with it.
                if self.current_thread.len() > 1 {
                    self.queue_thread_render(&sender);
                    // What was pulled in (a reply from Sent, an archived part)
                    // may carry attachments of its own.
                    self.load_thread_attachments();
                    // The same cache may know more than the row's badge was
                    // told when it asked (#351).
                    self.message_list.emit(MessageListInput::ConversationSize {
                        account_id,
                        id: message_id,
                        size: self.current_thread.len(),
                    });
                }
            }

            AppMsg::ThreadsListed { groups } => {
                self.request_thread_summaries(groups);
            }

            AppMsg::ThreadSummaries {
                account_id,
                summaries,
            } => {
                let summaries: Vec<((u32, String), ThreadSummary)> = summaries
                    .into_iter()
                    .map(|(root, mut summary)| {
                        for m in &mut summary.members {
                            m.id = self.related_id(m);
                        }
                        ((account_id, root), summary)
                    })
                    .collect();
                if !summaries.is_empty() {
                    self.message_list
                        .emit(MessageListInput::SetThreadSummaries(summaries));
                }
            }

            AppMsg::OpenListSearch => {
                // Ctrl+F routes by focus (#102/#103): in the reader it finds
                // within the message, everywhere else it searches the list
                // shown, Contacts' included.
                if self.showing_contacts {
                    self.contacts_page.emit(ContactsPageInput::FocusSearch);
                } else if self.reader_has_focus() && self.current.is_some() {
                    self.message_view.emit(MessageViewInput::OpenFind);
                } else {
                    self.message_list.emit(MessageListInput::FocusSearch);
                }
            }

            AppMsg::OpenReaderFind => {
                self.message_view.emit(MessageViewInput::OpenFind);
            }

            AppMsg::SetQuickFilter(f, on) => self.set_quick_filters(self.quick_filters.with(f, on)),

            AppMsg::SetFilterDate(end, day) => {
                self.set_quick_filters(self.quick_filters.with_date(end, day))
            }

            AppMsg::ClearFilterDates => self.set_quick_filters(
                self.quick_filters
                    .with_date(DateBound::From, None)
                    .with_date(DateBound::To, None),
            ),

            AppMsg::ClearQuickFilters => self.set_quick_filters(QuickFilters::default()),

            AppMsg::DeferredMarkRead { message } => {
                let still_current = self
                    .current
                    .as_ref()
                    .is_some_and(|c| c.id == message.id && c.account_id == message.account_id);
                if still_current {
                    self.mark_opened_read(&message);
                }
            }

            AppMsg::SelectionKeys(slots) => {
                let keys: Vec<(u32, u32)> = slots.iter().map(|&(a, _, id)| (a, id)).collect();
                self.list_slots = slots;
                self.list_heard = keys.clone();
                self.list_selection = keys.clone();
                self.selection_from_cards = false;
                self.message_view
                    .emit(MessageViewInput::SetSelectedCards(keys));
            }

            AppMsg::ListChecked(count) => {
                self.checked = count;
                // Checked rows are not read: whatever was open closes, and
                // the empty pane counts them.
                if count.is_some() && self.current.is_some() {
                    sender.input(AppMsg::ClearReader);
                }
                self.message_view.emit(MessageViewInput::SetChecked {
                    count: count.unwrap_or(0),
                    conversations: self.threading,
                });
            }

            AppMsg::SelectCards(keys) => {
                self.list_selection = keys.clone();
                self.list_slots.clear();
                self.selection_from_cards = !keys.is_empty();
                // Reading is click-driven: scrolling past a conversation
                // message no longer marks it, so an arriving reply stays
                // unread until the user actually clicks its card. A single
                // deliberate selection is that click.
                if let [(aid, id)] = keys.as_slice() {
                    if let Some(m) = self
                        .current_thread
                        .iter()
                        .find(|m| m.account_id == *aid && m.id == *id)
                        .filter(|m| m.unread)
                        .cloned()
                    {
                        self.set_read(&m, true);
                    }
                }
                // The conversation as the reader has it, Sent members and all,
                // so a card the list never listed still keeps its row (#220).
                let conversation = self
                    .current_thread
                    .iter()
                    .map(|m| (m.account_id, m.id))
                    .collect();
                self.message_list
                    .emit(MessageListInput::SelectFromReader { keys, conversation });
            }

            AppMsg::ThreadMessageSeen { account_id, id } => {
                self.on_thread_message_seen(account_id, id)
            }

            AppMsg::RenderThread => {
                self.thread_render_queued = false;
                if self.current_thread.len() > 1 {
                    // Still assembling and still within the grace period: come
                    // back rather than paint a conversation that is about to be
                    // replaced. The re-queue is what makes the deadline fire.
                    let unsettled = self.current_thread.iter().any(|m| m.body.is_empty())
                        || self.thread_related_pending;
                    let waiting = self
                        .thread_opened_at
                        .is_some_and(|opened| opened.elapsed() < THREAD_BODY_WAIT);
                    if unsettled && waiting {
                        self.queue_thread_render(&sender);
                        return;
                    }
                    self.thread_painted = true;
                    self.show_thread();
                    self.remember_thread();
                }
            }
            _ => unreachable!("not routed to on_reader_select"),
        }
    }

    /// The open conversation gained a member (a reply synced in while it
    /// was on screen): show it in place, without a re-selection.
    fn on_thread_grew(&mut self, m: Box<Message>, thread: Vec<Message>) {
        // Only for the conversation on screen: the reader's primary
        // is that head. A reply of the user's own filed in Sent is
        // not in the folder's list and stays as the related lookup
        // left it — kept below.
        let key = (m.account_id, m.id);
        if self.current.as_ref().map(|c| (c.account_id, c.id)) != Some(key) {
            return;
        }
        let existing = std::mem::take(&mut self.current_thread);
        let mut conv: Vec<Message> = existing.clone();
        for tm in &thread {
            let k = (tm.account_id, tm.id);
            if !self.listed_thread.contains(&k) {
                self.listed_thread.push(k);
            }
            if conv.iter().any(|e| (e.account_id, e.id) == k) {
                continue;
            }
            let mut tm = tm.clone();
            if k == key {
                tm.unread = false;
                if let Some(c) = self.current.as_ref().filter(|c| !c.body.is_empty()) {
                    tm.body = c.body.clone();
                }
            } else if tm.body.is_empty() {
                if let Some(b) = self.body_cache.get(&body_key(&tm)) {
                    tm.body = b.clone();
                }
            }
            conv.push(tm);
        }
        // Chronological, as a conversation is stored (display order
        // is show_thread's business).
        conv.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.uid.cmp(&b.uid)));
        tracing::info!(
            "conversation on screen grew: {} -> {} messages",
            existing.len(),
            conv.len()
        );
        self.current_thread = conv;
        self.thread_key = Some(body_key(&m));
        // The conversation is already painted: the new card joins it
        // in place. Its body, when not prefetched, is asked for and
        // the render follows its arrival (the Body handler repaints a
        // conversation as bodies land) rather than showing an empty
        // card first.
        self.thread_painted = true;
        self.thread_related_pending = false;
        self.remember_thread();
        // What is on screen stays where it is: the render keeps the
        // reader's place through its saved anchor, and when none is
        // recorded yet (no scroll since the conversation opened) the
        // card that was at the top of the pane is pinned there — so
        // with newest first, the new card slots in above it unseen,
        // and is marked read only once the user scrolls up to it.
        let top = if self.thread_newest_first {
            existing.iter().max_by_key(|tm| tm.timestamp)
        } else {
            existing.first()
        }
        .map(|tm| (tm.account_id, tm.id))
        .or_else(|| self.current.as_ref().map(|c| (c.account_id, c.id)));
        if let Some((account_id, id)) = top {
            self.message_view
                .emit(MessageViewInput::HoldPlace { account_id, id });
        }
        let to_load = self.missing_thread_bodies(None);
        if to_load.is_empty() {
            self.show_thread();
        } else {
            self.request_bodies(to_load);
        }
        self.load_thread_attachments();
    }

    /// A conversation message was scrolled all the way through — mark it read on
    /// the server, in the caches and in the list.
    fn on_thread_message_seen(&mut self, account_id: u32, id: u32) {
        // Reading one message of a conversation marks only that message,
        // exactly as opening it on its own would.
        let target = self
            .current_thread
            .iter_mut()
            .find(|m| m.account_id == account_id && m.id == id)
            .filter(|m| m.unread)
            .map(|m| {
                m.unread = false;
                (m.uid, m.folder_id)
            });
        let Some((uid, folder_id)) = target else {
            return;
        };
        if let Some(c) = self
            .current
            .as_mut()
            .filter(|c| (c.account_id, c.id) == (account_id, id))
        {
            c.unread = false;
        }
        if let Some(path) = self.folder_path(account_id, folder_id) {
            self.pending_seen.insert(
                (account_id, path.clone(), uid),
                (true, std::time::Instant::now()),
            );
            self.send_to(
                account_id,
                MailRequest::SetSeen {
                    path,
                    uid,
                    seen: true,
                },
            );
        }
        // Reading the new mail inside its conversation answers the
        // notification as opening it on its own does (#333).
        crate::desktop::notify::withdraw_mail(account_id);
        self.message_list
            .emit(MessageListInput::MarkRead((account_id, folder_id, id)));
        self.mark_cached_read(account_id, folder_id, id);
        // The card's dot clears in place as the viewport observer
        // marks it (#100) — this path never told the view before.
        self.message_view
            .emit(MessageViewInput::ClearDot { account_id, id });
        if let Some(n) = self.folder_unread.get_mut(&(account_id, folder_id)) {
            *n = n.saturating_sub(1);
        }
        self.remember_thread();
        self.push_unread_counts();
    }

    /// `solo` marks a reply the user picked out of a conversation on screen:
    /// show that message alone and don't go looking for its siblings.
    fn on_message_selected(
        &mut self,
        m: Message,
        thread: Vec<Message>,
        solo: bool,
        sender: ComponentSender<Self>,
    ) {
        // What the reader is showing right now. A message that has just
        // been moved back arrives under a new UID, so the list hands it
        // over again as if it were a different message; rendering it a
        // second time only makes the reader blink (#200).
        // Captured before anything clears it: a conversation on screen
        // is a different document from the same message shown alone,
        // even though both answer to the one Message-ID.
        let was_thread = self.current_thread.len() > 1;
        let showing = self.current.as_ref().map(|c| {
            (
                c.account_id,
                c.message_id.clone(),
                c.body.clone(),
                c.unread,
                c.starred,
                c.keywords.clone(),
            )
        });
        // Navigating away releases any inline reply (save-if-dirty, or keep
        // it as an independent window if it was popped out).
        self.release_reader_compose();
        // A sent reply still on its way into the conversation left
        // behind: the reader shows the new message at once.
        let elsewhere = self
            .current
            .as_ref()
            .is_some_and(|c| (c.account_id, c.id) != (m.account_id, m.id));
        if elsewhere && self.send_morph.running() {
            self.send_morph.finish();
        }
        // A queued message reads like any other, straight from the bytes
        // already on disk — no folder, no UID, nothing to ask the server.
        if let Some(item) = self.outbox_item(m.account_id, m.id) {
            self.show_outbox_message(&item);
            return;
        }
        // Selecting a draft opens it in the compose editor, in the
        // reading pane where the message would otherwise show.
        if self.is_drafts_folder(m.account_id, m.folder_id) {
            self.reader_focus_pending = false;
            self.open_draft(m, true, HandOffFiles::default(), &sender);
            return;
        }
        self.follow_keyboard_into_reader();
        self.attachments.clear();
        self.attachments_loading = false;
        self.sync_attachment_drawer();
        let account_id = m.account_id;
        let folder_path = self.resolve_folder_path(&m);
        // Use an already-fetched body if we have one (on the message or in
        // our cache) so reopening renders instantly without a spinner.
        let cached_body = if !m.body.is_empty() {
            Some(m.body.clone())
        } else {
            self.body_cache.get(&body_key(&m)).cloned()
        };
        let needs_body = cached_body.is_none();

        // A conversation's row is not one message read: its cards are
        // marked as the reader shows them, the first one included.
        // Marking that one outright left it read, and folded, every
        // time an unread conversation was opened again (#392).
        let conversation = thread.len() > 1;
        self.mark_read_on_open(&m, conversation, &sender);

        let mut current = m.clone();
        if !conversation {
            current.unread = false;
        }
        if let Some(body) = cached_body {
            current.body = body;
        }
        self.current = Some(current.clone());
        self.listed_thread = if thread.is_empty() {
            vec![(m.account_id, m.id)]
        } else {
            thread.iter().map(|t| (t.account_id, t.id)).collect()
        };
        // A new selection: whatever the last one was waiting for no
        // longer holds this one back.
        self.thread_related_pending = false;

        let mut related = self.related_request(&m, &thread, solo);

        if thread.len() > 1 {
            self.open_conversation(&m, &thread, &current, needs_body, related, &sender);
        } else {
            self.current_thread.clear();
            self.thread_key = None;
            let display = current;
            // Already on screen, pixel for pixel: leave it alone.
            let unchanged = !needs_body && !was_thread && same_as_showing(showing, &display);
            tracing::debug!(
                target: "hylki::undo",
                "select {}: single message, needs_body={needs_body}, unchanged={unchanged}",
                m.id,
            );
            if unchanged {
                if let Some(req) = related.take() {
                    self.send_to(account_id, req);
                }
                return;
            }
            self.show_single(&m, display, needs_body, folder_path.clone(), related);
        }

        self.load_attachments_on_open(&m, folder_path);
    }

    /// With the Privacy switch on, the remote content of the rows likely
    /// opened next is fetched while this one is read (#474): only where
    /// it would be loaded on opening anyway, and only from bodies already
    /// here, so nothing is asked of the mail server for it.
    fn fetch_remote_ahead(&self, ahead: &[Message]) {
        if !self.remote_ahead {
            return;
        }
        let bodies: Vec<&str> = ahead
            .iter()
            .filter(|m| self.remote_allowed(m))
            .filter_map(|m| {
                if !m.body.is_empty() {
                    Some(m.body.as_str())
                } else {
                    self.body_cache.get(&body_key(m)).map(String::as_str)
                }
            })
            .collect();
        crate::services::remote_content::fetch_ahead(bodies);
    }

    /// Opened with Gmail's o or Enter: the keyboard follows it into
    /// the reader, as Gmail's conversation view takes it, so j and
    /// k go on to the next conversation and u or Esc come back
    /// (#392). Once the reader has been handed the message.
    fn follow_keyboard_into_reader(&mut self) {
        if std::mem::take(&mut self.reader_focus_pending) {
            let view = self.message_view.sender().clone();
            gtk::glib::idle_add_local_once(move || {
                view.emit(MessageViewInput::FocusBody);
            });
        }
    }

    /// Marks a lone message read as the read-mark setting says: at once,
    /// after two seconds in view (#100), or never. A conversation is marked
    /// card by card as the reader shows them.
    fn mark_read_on_open(
        &mut self,
        m: &Message,
        conversation: bool,
        sender: &ComponentSender<Self>,
    ) {
        if m.unread && !conversation {
            match self.read_mark {
                config::ReadMark::Shown => self.mark_opened_read(m),
                config::ReadMark::Delay => {
                    // Mark after a beat in view — navigating away
                    // before then leaves it unread (#100).
                    let s = sender.clone();
                    let msg = m.clone();
                    gtk::glib::timeout_add_seconds_local_once(2, move || {
                        s.input(AppMsg::DeferredMarkRead {
                            message: Box::new(msg.clone()),
                        });
                    });
                }
                config::ReadMark::Manual => {}
            }
        }
    }

    /// The folder being read holds only its half of the conversation:
    /// your own replies are filed in Sent, and older parts may have
    /// been archived. Ask for the rest from the cache (#21).
    /// …unless the user picked this reply out of a conversation already
    /// on screen. They asked for one message; assembling its thread
    /// again would swap the conversation back in under them.
    /// Asked for after the body, below: the cache lane answers both,
    /// one after the other, and a cached body is what the reader is
    /// waiting to show (#259).
    fn related_request(
        &mut self,
        m: &Message,
        thread: &[Message],
        solo: bool,
    ) -> Option<MailRequest> {
        let mut related = None;
        if self.threading && !solo {
            let only = [m.clone()];
            let ids = thread_ids(if thread.is_empty() { &only[..] } else { thread });
            if !ids.is_empty() {
                related = Some(MailRequest::LoadRelated {
                    message_id: m.id,
                    ids,
                });
                self.thread_related_pending = true;
            }
        }
        related
    }

    /// A conversation: the stored one painted at once, else assembled from
    /// the cached bodies with the rest requested, the reader holding its
    /// spinner until it settles. The related lookup goes after the body
    /// requests (#259).
    fn open_conversation(
        &mut self,
        m: &Message,
        thread: &[Message],
        current: &Message,
        needs_body: bool,
        mut related: Option<MailRequest>,
        sender: &ComponentSender<Self>,
    ) {
        let account_id = m.account_id;
        self.thread_key = Some(body_key(m));
        // Already assembled: paint it now. No lookup, no body
        // gathering, no spinner — returning to a thread shouldn't
        // cost what opening it did.
        tracing::debug!(
            target: "hylki::undo",
            "select {}: thread of {}, remembered={}, needs_body={needs_body}",
            m.id,
            thread.len(),
            self.thread_cache.contains_key(&body_key(m)),
        );
        if let Some(cached) = self.thread_cache.get(&body_key(m)).cloned() {
            self.current_thread = cached;
            // The row's message as the list has it now, whatever
            // the stored copy said when it was put away.
            for tm in self.current_thread.iter_mut() {
                if tm.id == m.id && tm.account_id == account_id {
                    tm.unread = current.unread;
                    tm.body = current.body.clone();
                }
            }
            // A member whose body came after the conversation was
            // put away (one from another folder, say) was stored
            // without it, and would say "Loading…" for good.
            self.load_missing_thread_bodies(Some((account_id, m.id)));
            self.thread_painted = true;
            self.thread_related_pending = false;
            self.show_thread();
            self.load_thread_attachments();
            if let Some(req) = related.take() {
                self.send_to(account_id, req);
            }
        } else {
            // Conversation: assemble the thread with any cached bodies,
            // request the rest, and render it as a scrollable conversation.
            let mut conv: Vec<Message> = Vec::with_capacity(thread.len());
            for tm in thread {
                let mut tm = tm.clone();
                // Each keeps its real unread state, the row's own
                // message too: the reader marks them as it shows them.
                if tm.id == m.id && tm.account_id == account_id {
                    tm.unread = current.unread;
                    tm.body = current.body.clone();
                } else if tm.body.is_empty() {
                    if let Some(b) = self.body_cache.get(&body_key(&tm)) {
                        tm.body = b.clone();
                    }
                }
                conv.push(tm);
            }
            self.current_thread = conv;
            self.thread_opened_at = Some(std::time::Instant::now());
            self.thread_painted = false;
            // The paint happens once the conversation has settled: the
            // lookup answered and the bodies in. Until then the reader
            // holds its spinner.
            self.queue_thread_render(sender);
            // Fetch the bodies we don't have yet (primary first via order).
            let to_load = self.missing_thread_bodies(None);
            self.request_bodies(to_load);
            if let Some(req) = related.take() {
                self.send_to(account_id, req);
            }
            self.show_thread();
            self.load_thread_attachments();
        }
    }

    /// A lone message: its body asked for first if it is not cached, then
    /// the related lookup, then the reader shows it.
    fn show_single(
        &mut self,
        m: &Message,
        display: Message,
        needs_body: bool,
        folder_path: Option<String>,
        related: Option<MailRequest>,
    ) {
        let account_id = m.account_id;
        // Request the body FIRST so it renders before attachments — the
        // worker processes requests in order, so the body must come first.
        if needs_body {
            if let Some(path) = folder_path {
                self.send_to(
                    account_id,
                    MailRequest::LoadBody {
                        message_id: m.id,
                        path,
                        uid: m.uid,
                    },
                );
            }
        }
        if let Some(req) = related {
            self.send_to(account_id, req);
        }
        self.show_message(Some(display), needs_body);
    }

    /// Attachments: use the in-memory cache if present; otherwise
    /// fetch them (disk cache first, then the server). Opening the
    /// message is the request — the paperclip appears when they
    /// land, with no "load attachments" click in between.
    fn load_attachments_on_open(&mut self, m: &Message, folder_path: Option<String>) {
        let account_id = m.account_id;
        if m.has_attachment {
            if let Some(cached) = self.attachment_cache.get(&body_key(m)).cloned() {
                self.attachments = cached;
                self.sync_attachment_drawer();
            } else if let Some(path) = folder_path {
                self.attachments_loading = true;
                self.send_to(
                    account_id,
                    MailRequest::LoadAttachments {
                        message_id: m.id,
                        path,
                        uid: m.uid,
                        download: true,
                    },
                );
            }
        }
    }
}
