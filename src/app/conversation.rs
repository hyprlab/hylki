//! Assembling a conversation for the reader: which members to show, which
//! bodies to fetch, and how worker answers find the message they are about.

use std::collections::{HashMap, HashSet};

use relm4::prelude::*;

use crate::models::{thread_ids, FolderKind, Message};
use crate::ui::message_list::MessageListInput;
use crate::ui::message_view::MessageViewInput;
use crate::worker::MailRequest;

use super::model::AppModel;
use super::msg::AppMsg;
use super::showcase::showcase_pgp;

/// Whether two rows are the same mail stored twice, rather than two messages.
///
/// Message-ID is the identity, but it is written by whoever sent the mail and
/// spam reuses one across unrelated messages — so sender and timestamp have to
/// agree as well before one copy is allowed to stand for another. Gmail's label
/// copies agree on all three. Subject is deliberately left out: a re-decoded
/// encoded-word can rewrite it under one label and not another.
pub(super) fn same_mail(a: &Message, b: &Message) -> bool {
    !a.message_id.is_empty()
        && a.message_id == b.message_id
        && a.from_addr == b.from_addr
        && a.timestamp == b.timestamp
}

/// Keep one copy of each message, dropping the rest of its labels.
///
/// Gmail exposes labels as IMAP folders, so a single message sits in INBOX,
/// All Mail and Important at once — three folder/UID pairs carrying one
/// Message-ID. A conversation that dedupes on those pairs alone therefore shows
/// every copy: a six-message thread renders as eighteen. On a real cache 1210 of
/// this account's 1212 messages are labelled that way, so this is the normal
/// case for Gmail, not an edge one.
///
/// The copy from the folder the reader is showing wins, so the "from folder X"
/// label and the actions on a message refer to the mail actually on screen. A
/// message with no Message-ID can't be matched this way and is left alone.
fn dedupe_label_copies(
    messages: Vec<Message>,
    conv: &[Message],
    shown_folder: u32,
) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::new();
    for r in messages {
        if r.message_id.is_empty() {
            out.push(r);
            continue;
        }
        if conv.iter().any(|m| same_mail(m, &r)) {
            continue; // the conversation already holds this mail under some label
        }
        match out.iter().position(|m| same_mail(m, &r)) {
            Some(i) => {
                if out[i].folder_id != shown_folder && r.folder_id == shown_folder {
                    out[i] = r;
                }
            }
            None => out.push(r),
        }
    }
    out
}

/// A conversation member still waiting for its body:
/// `(account_id, message_id, uid, folder_path)`.
type MissingBody = (u32, u32, u32, String);

/// One `LoadBodies` request: the `(account_id, folder_path)` it goes to, and the
/// `(message_id, uid)` of every member it covers.
type BodyBatch = ((u32, String), Vec<(u32, u32)>);

/// Group a conversation's missing bodies into one request per (account, folder).
///
/// Every member asked for on its own is a round trip each, and each arrival
/// re-renders the reader; `LoadBodies` fetches a whole UID set at once. Members
/// are kept in the order given — the primary is first, so it is first in its
/// batch and renders first — and a conversation that spans folders (or, in the
/// unified inbox, accounts) splits into one batch per folder rather than losing
/// the members that don't fit the first one.
fn batch_bodies_by_folder(to_load: Vec<MissingBody>) -> Vec<BodyBatch> {
    let mut batches: Vec<BodyBatch> = Vec::new();
    for (account_id, message_id, uid, path) in to_load {
        match batches
            .iter_mut()
            .find(|((a, p), _)| *a == account_id && *p == path)
        {
            Some((_, items)) => items.push((message_id, uid)),
            None => batches.push(((account_id, path), vec![(message_id, uid)])),
        }
    }
    batches
}

/// Where to move the reader when its message disappeared from a sync (deleted or
/// moved on another device): whatever now sits in the slot it occupied.
///
/// `None` when that slot cannot be worked out, which is not the same as "start at
/// the top". This used to fall back to the folder's first message, and since
/// deleting prunes the message from the cache before the sync arrives, the
/// lookup fails on exactly that path — so a delete could throw the selection to
/// the top of the folder and scroll the list with it (#19).
pub(super) fn next_after_vanish(
    previous: Option<&Vec<Message>>,
    messages: &[Message],
    cur_uid: u32,
) -> Option<(u32, u32)> {
    let idx = previous?.iter().position(|m| m.uid == cur_uid)?;
    messages
        .get(idx.min(messages.len().checked_sub(1)?))
        .map(|m| (m.account_id, m.id))
}

/// Where a body is kept in the app's RAM cache: (account, folder, UID).
pub(super) type BodyKey = (u32, u32, u32);

/// A message's [`BodyKey`]. The folder and UID are the message's real ones
/// even where its id is not (a member opened out from another folder), so
/// every copy of a message is found where its own body was filed.
pub(super) fn body_key(m: &Message) -> BodyKey {
    (m.account_id, m.folder_id, m.uid)
}

/// Whether a worker event about `message_id` (whose [`AppModel::event_key`]
/// is `key`) is about `m`. Without a key, for a folder the app does not
/// know, the id alone has to do.
pub(super) fn is_event_target(
    m: &Message,
    key: Option<BodyKey>,
    account_id: u32,
    message_id: u32,
) -> bool {
    match key {
        Some(k) => body_key(m) == k,
        None => m.account_id == account_id && m.id == message_id,
    }
}

/// How long a conversation waits for its outstanding bodies before painting what
/// it has. Long enough that a cached conversation — the common case — always
/// arrives whole, short enough that one unreachable message can't hold the
/// reader hostage.
pub(super) const THREAD_BODY_WAIT: std::time::Duration = std::time::Duration::from_millis(1200);

/// How many messages' reply headers are held for joining conversations across
/// folders. Only a bound on memory and rebuild cost — never on correctness: a
/// conversation whose members are all on screen groups without consulting these
/// at all. They matter for the join that runs through a message that isn't
/// shown, most often your own reply in Sent. The newest are kept, since that is
/// where the mail being read lives.
const THREAD_LINK_LIMIT: usize = 20_000;

/// How many carried-over bodies to hold before giving up on them. They are
/// meant to be claimed by the very next reload; a build-up means the reloads
/// never came, and these are whole message bodies.
pub(super) const CARRIED_BODY_LIMIT: usize = 64;

impl AppModel {
    /// How many assembled conversations are kept. Each holds its messages'
    /// bodies, so this is a memory budget as much as a cache size.
    pub(super) const THREAD_CACHE_MAX: usize = 8;

    pub(super) fn show_message(&mut self, message: Option<Message>, loading: bool) {
        // A hidden reader draws nothing: a body arriving for the selected
        // row would otherwise load a page no one sees (#475).
        if !self.reader_shown() && message.is_some() {
            return;
        }
        let allow_remote = message.as_ref().is_some_and(|m| self.remote_allowed(m));
        let (account_name, account_color) = match message.as_ref() {
            Some(m) => (
                Some(self.account_name(m.account_id)),
                Some(self.account_color(m.account_id)),
            ),
            None => (None, None),
        };
        let stored_check = message.as_ref().and_then(|m| {
            let mut check = self.sender_cache.get(&body_key(m)).cloned()?;
            self.vouchers.gate(Some(m.account_id), &mut check);
            Some(check)
        });
        let sent = message
            .iter()
            .filter(|m| self.folder_kind(m.account_id, m.folder_id) == Some(FolderKind::Sent))
            .map(|m| (m.account_id, m.id))
            .collect();
        self.message_view.emit(MessageViewInput::Show {
            thread: message.into_iter().collect(),
            allow_remote,
            account_name,
            account_color,
            loading,
            primary: None, // a single message is its own primary
            folder_labels: HashMap::new(),
            sent,
            // A single message is one small frame; it is never covered anyway.
            instant: true,
        });
        // After `Show`, which clears the outgoing message's verdict.
        if let Some(check) = stored_check {
            self.message_view
                .emit(MessageViewInput::SetSenderCheck(check));
        }
        self.push_member_checks();
    }

    /// Ask for one conversation re-render shortly from now, collapsing a burst of
    /// arriving bodies into a single one.
    ///
    /// A conversation is rendered as one document with every member's body
    /// inlined and handed to WebKit. Rendering per arriving body meant N loads of
    /// an N-body document, issued far faster than WebKit could retire them.
    pub(super) fn queue_thread_render(&mut self, sender: &ComponentSender<Self>) {
        if self.thread_render_queued {
            return;
        }
        self.thread_render_queued = true;
        let s = sender.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
            s.input(AppMsg::RenderThread);
        });
    }

    /// Render the current conversation (thread) in the reader, newest first.
    ///
    /// The header follows the message the user selected, not whatever sorted to
    /// the top: a conversation can now include replies of your own pulled in from
    /// Sent, and one of those being the newest shouldn't retitle the reader.
    pub(super) fn show_thread(&mut self) {
        if !self.reader_shown() {
            return;
        }
        let Some(head) = self.current_thread.first() else {
            return;
        };
        let primary = self.current.clone().unwrap_or_else(|| head.clone());
        let account_id = primary.account_id;
        let allow_remote = self.remote_allowed(&primary);
        // Spin while any of the conversation is still on its way: a partial
        // render would be replaced moments later, and each replacement is a full
        // page load in the WebView — which is what the flashing was.
        let missing = self.current_thread.iter().any(|m| m.body.is_empty());
        let waiting = self
            .thread_opened_at
            .is_some_and(|opened| opened.elapsed() < THREAD_BODY_WAIT);
        let unsettled = missing || self.thread_related_pending;
        let loading = if self.current_thread.len() > 1 {
            // The spinner holds the reader from the moment a thread is opened
            // until its single paint, so the wait is shown rather than papered
            // over with the message that was there before.
            !self.thread_painted || (unsettled && waiting)
        } else {
            primary.body.is_empty()
        };
        // Display order is a preference (#70): newest first flips the stored
        // chronological order for rendering only — stepping (w/b) and the
        // related-message bookkeeping stay chronological.
        let mut thread = self.current_thread.clone();
        if self.thread_newest_first {
            thread.reverse();
        }
        self.message_view.emit(MessageViewInput::Show {
            thread,
            allow_remote,
            account_name: Some(self.account_name(account_id)),
            account_color: Some(self.account_color(account_id)),
            loading,
            folder_labels: self.thread_folder_labels(),
            sent: self.thread_sent(),
            primary: Some(Box::new(primary)),
            // Nothing to wait for when the conversation was already assembled
            // and its bodies are in hand.
            instant: self.thread_painted && !loading,
        });
        self.push_member_checks();
    }

    /// The open conversation's messages that are filed in a Sent folder.
    fn thread_sent(&self) -> std::collections::HashSet<(u32, u32)> {
        self.current_thread
            .iter()
            .filter(|m| self.folder_kind(m.account_id, m.folder_id) == Some(FolderKind::Sent))
            .map(|m| (m.account_id, m.id))
            .collect()
    }

    /// Name the folder each conversation message came from, for the ones that
    /// aren't from the folder on screen. The message list only ever shows one
    /// folder, so anything else was pulled in from the cache (#21).
    fn thread_folder_labels(&self) -> HashMap<(u32, u32), String> {
        let shown_folder = self.current.as_ref().map(|m| m.folder_id);
        self.current_thread
            .iter()
            .filter(|m| Some(m.folder_id) != shown_folder)
            .filter_map(|m| {
                let name = self.folder_by_id(m.account_id, m.folder_id)?.name.clone();
                Some(((m.account_id, m.id), name))
            })
            .collect()
    }

    /// Push every cached sender verdict for the on-screen conversation into
    /// the reader, so each card's header seal lights up (#88).
    fn push_member_checks(&mut self) {
        // A single message never fills current_thread — the open message
        // itself is the one card then.
        let mut cards: Vec<Message> = self.current_thread.clone();
        if cards.is_empty() {
            cards.extend(self.current.clone());
        }
        // HYLKI_SHOWCASE_PGP=<good|untrusted|bad|nokey|enc> gives every
        // demo card that OpenPGP verdict, for a capture of the header's
        // chip (#300); the demo has no sender checks of its own.
        let showcase = std::env::var_os("HYLKI_DEMO")
            .and(std::env::var("HYLKI_SHOWCASE_PGP").ok())
            .map(|v| {
                Box::new(crate::models::SenderCheck {
                    pgp: showcase_pgp(&v),
                    ..Default::default()
                })
            });
        // HYLKI_SHOWCASE_RECEIPT=1 has every demo card ask for a read
        // receipt (#520), for a capture of its banner.
        let receipts = std::env::var_os("HYLKI_DEMO")
            .and(std::env::var_os("HYLKI_SHOWCASE_RECEIPT"))
            .is_some();
        for m in cards {
            let stored = self.sender_cache.get(&body_key(&m)).cloned();
            let staged = receipts.then(|| {
                Box::new(crate::models::SenderCheck {
                    receipt: Some(crate::mail::receipt::ReceiptRequest {
                        to: m.from_addr.clone(),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
            });
            if let Some(check) = staged.or(showcase.clone()).or(stored) {
                // Its read receipt request (#520) too, as the card asks it.
                let check = self.reader_check(&m, &check, true);
                self.message_view.emit(MessageViewInput::SenderCheckFor {
                    account_id: m.account_id,
                    id: m.id,
                    check,
                });
            }
        }
    }

    /// Ask the cache again for the rest of the conversation on screen, when
    /// it belongs to `account_id`. The answer goes through [`merge_related`],
    /// which adds only what the conversation lacks.
    pub(super) fn reload_related(&mut self, account_id: u32) {
        let Some(current) = self.current.clone() else {
            return;
        };
        if current.account_id != account_id || !self.threading {
            return;
        }
        let only = [current.clone()];
        let ids = thread_ids(if self.current_thread.is_empty() {
            &only[..]
        } else {
            &self.current_thread
        });
        if ids.is_empty() {
            return;
        }
        self.send_to(
            account_id,
            MailRequest::LoadRelated {
                message_id: current.id,
                ids,
            },
        );
        self.thread_related_pending = true;
    }

    /// The id a message from another folder goes by here: one per (account,
    /// folder, UID), so the reader and the list's opened-out conversations
    /// (#309) name the same message the same way, apart from any folder's own.
    pub(super) fn related_id(&mut self, m: &Message) -> u32 {
        let key = (m.account_id, m.folder_id, m.uid);
        if let Some(id) = self.related_ids.get(&key) {
            return *id;
        }
        self.related_id_seq = self.related_id_seq.saturating_sub(1);
        self.related_ids.insert(key, self.related_id_seq);
        self.related_id_seq
    }

    /// Fold the rest of a conversation — messages from the account's other
    /// folders, chiefly the replies you sent — into the open one (#21).
    ///
    /// Ignored unless it answers the message still on screen: the lookup is
    /// asynchronous and the user may have moved on. Messages already in the
    /// conversation are skipped, so this is safe to apply more than once.
    pub(super) fn merge_related(
        &mut self,
        account_id: u32,
        message_id: u32,
        messages: Vec<Message>,
    ) {
        let Some(current) = self.current.clone() else {
            return;
        };
        if current.account_id != account_id || current.id != message_id || !self.threading {
            return;
        }
        let mut conv = if self.current_thread.is_empty() {
            vec![current.clone()]
        } else {
            self.current_thread.clone()
        };
        let messages = dedupe_label_copies(messages, &conv, current.folder_id);
        // A reply sent from here, read back from Sent, takes over the card
        // that stood in for it, and that card's id, so the card stays where
        // it is. Its body, until its own is loaded, is the stand-in's: the
        // same bytes, built the same way.
        let mut stood_in: HashMap<(u32, u32, u32), String> = HashMap::new();
        for r in &messages {
            let took_over = self
                .provisionals
                .iter()
                .find(|(_, p)| p.copied_by(r))
                .map(|(k, _)| *k);
            if let Some((key, p)) = took_over.and_then(|key| self.provisionals.remove_entry(&key)) {
                self.related_ids
                    .insert((r.account_id, r.folder_id, r.uid), key.1);
                conv.retain(|m| (m.account_id, m.id) != key);
                stood_in.insert((r.account_id, r.folder_id, r.uid), p.message.body);
            }
        }
        // A copy already in the conversation (listed with it) retires its
        // stand-in too.
        let retired: Vec<(u32, u32)> = self
            .provisionals
            .iter()
            .filter(|(k, p)| {
                conv.iter()
                    .any(|m| (m.account_id, m.id) != **k && p.copied_by(m))
            })
            .map(|(k, _)| *k)
            .collect();
        for k in &retired {
            self.provisionals.remove(k);
            conv.retain(|m| (m.account_id, m.id) != *k);
        }
        let mut added = Vec::new();
        for mut r in messages {
            if conv
                .iter()
                .any(|m| m.folder_id == r.folder_id && m.uid == r.uid)
            {
                continue; // already in the conversation, from this folder's own index
            }
            let body = stood_in.remove(&(r.account_id, r.folder_id, r.uid));
            r.id = self.related_id(&r);
            r.unread = false;
            if let Some(b) = self.body_cache.get(&body_key(&r)) {
                r.body = b.clone();
            } else if let Some(b) = body {
                r.body = b;
            }
            added.push(r);
        }
        // A reply sent from here whose copy is not back yet keeps its card
        // in the conversation it answers, however the conversation was put
        // together again.
        for (k, p) in &self.provisionals {
            if !conv.iter().any(|m| (m.account_id, m.id) == *k)
                && conv.iter().any(|m| m.message_id == p.answers)
            {
                added.push(p.message.clone());
            }
        }
        if added.is_empty() && retired.is_empty() {
            // Missing bodies were asked for when the conversation was shown;
            // asking again here would fetch one still on its way a second time.
            return;
        }
        conv.extend(added);
        // Oldest first, as the list threads.
        conv.sort_by_key(|m| m.timestamp);
        // Late arrivals bring bodies of their own; give them the same grace
        // period so they join the first paint instead of causing a second.
        self.thread_opened_at = Some(std::time::Instant::now());
        self.current_thread = conv;
        self.load_missing_thread_bodies(None);
    }

    /// Give every member of the conversation on screen that has no body the
    /// one the app already holds, and ask the workers for the rest. `except`
    /// is a member whose body is already on its way by another request.
    pub(super) fn load_missing_thread_bodies(&mut self, except: Option<(u32, u32)>) {
        for tm in self.current_thread.iter_mut() {
            if tm.body.is_empty() {
                if let Some(b) = self.body_cache.get(&body_key(tm)) {
                    tm.body = b.clone();
                }
            }
        }
        let to_load = self.missing_thread_bodies(except);
        self.request_bodies(to_load);
    }

    /// The conversation's members whose bodies are still missing, `except`
    /// one, with the folder each lives in.
    pub(super) fn missing_thread_bodies(&self, except: Option<(u32, u32)>) -> Vec<MissingBody> {
        self.current_thread
            .iter()
            .filter(|tm| tm.body.is_empty() && except != Some((tm.account_id, tm.id)))
            .filter_map(|tm| {
                self.resolve_folder_path(tm)
                    .map(|p| (tm.account_id, tm.id, tm.uid, p))
            })
            .collect()
    }

    /// Asks the workers for these bodies, one request per folder.
    pub(super) fn request_bodies(&self, to_load: Vec<MissingBody>) {
        for ((aid, path), items) in batch_bodies_by_folder(to_load) {
            self.send_to(aid, MailRequest::LoadBodies { items, path });
        }
    }

    /// Store the conversation on screen so returning to it is instant.
    pub(super) fn remember_thread(&mut self) {
        let Some(key) = self.thread_key else { return };
        let thread = self.current_thread.clone();
        self.remember_thread_for(key, thread);
    }

    /// The same, for a conversation that isn't the one on screen — carrying
    /// one over a move that changed its key (#200).
    pub(super) fn remember_thread_for(&mut self, key: BodyKey, thread: Vec<Message>) {
        if thread.len() <= 1 {
            return;
        }
        if self.thread_cache.insert(key, thread).is_none() {
            self.thread_cache_order.push(key);
        }
        while self.thread_cache_order.len() > Self::THREAD_CACHE_MAX {
            let oldest = self.thread_cache_order.remove(0);
            self.thread_cache.remove(&oldest);
        }
    }

    /// Forget assembled conversations for an account — its mail has changed
    /// underneath them, so what they hold may no longer be the conversation.
    pub(super) fn forget_threads(&mut self, account_id: u32) {
        self.thread_cache
            .retain(|(aid, _, _), _| *aid != account_id);
        self.thread_cache_order
            .retain(|(aid, _, _)| *aid != account_id);
    }

    /// Reply headers for every cached message, so the list can see that two
    /// replies in a folder answer the same message in another one.
    pub(super) fn push_thread_links(&self) {
        // Newest first, and capped: threading the whole mailbox would otherwise
        // put every message's reply headers here, to be cloned on each sync and
        // walked on each rebuild. A conversation is joined by mail near it in
        // time, so the newest links are the ones that do the joining.
        // Not from the folders of a view too large to keep, held only while
        // it is on show: they would change every list's links as they come
        // and go, and every row kept as built with them (#504).
        let nowhere = HashSet::new();
        let mut recent: Vec<&Message> = self
            .message_cache
            .iter()
            .filter(|(k, l)| !self.too_large_off_screen(**k, l.len(), &nowhere))
            .flat_map(|(_, l)| l)
            .filter(|m| !(m.message_id.is_empty() && m.references.is_empty()))
            .collect();
        // The newest few, picked without sorting the lot: across every
        // folder held that is hundreds of thousands of messages.
        if recent.len() > THREAD_LINK_LIMIT {
            recent.select_nth_unstable_by(THREAD_LINK_LIMIT, |a, b| b.timestamp.cmp(&a.timestamp));
            recent.truncate(THREAD_LINK_LIMIT);
        }
        let mut links: Vec<(u32, String, String)> = recent
            .into_iter()
            .map(|m| (m.account_id, m.message_id.clone(), m.references.clone()))
            .collect();
        links.sort();
        links.dedup();
        self.message_list
            .emit(MessageListInput::SetThreadLinks(links));
    }

    /// Ask each account's cache what the conversations on the list's page
    /// really are: how big (#222), and who spoke in them last (#236).
    ///
    /// The list counts what it lists, which in a folder view is one folder; the
    /// replies you sent are in Sent, so the badge was short by exactly them and
    /// the row still read as the other side's last word. The lookup is the
    /// reader's own (`thread_summaries` mirrors what `messages_by_thread_ids`
    /// would find), so the number on the chip is the number of cards the reader
    /// will show when the row is opened.
    ///
    /// Nothing here blocks the page: the answers arrive as
    /// [`AppMsg::ThreadSummaries`] and settle the rows a beat later.
    pub(super) fn request_thread_summaries(&self, groups: Vec<(u32, String, Vec<String>)>) {
        if !self.threading || groups.is_empty() {
            return;
        }
        let mut by_account: HashMap<u32, Vec<(String, Vec<String>)>> = HashMap::new();
        for (account_id, root, ids) in groups {
            by_account.entry(account_id).or_default().push((root, ids));
        }
        for (account_id, groups) in by_account {
            self.send_to(account_id, MailRequest::LoadThreadSummaries { groups });
        }
    }

    /// Re-emit the currently-visible folder/unified list from the caches, so
    /// in-place changes (e.g. mark-all-read) show without a server round-trip.
    pub(super) fn refresh_list_display(&self) {
        if self.unified {
            self.emit_unified();
        } else if self.tag_view.is_some() {
            self.show_tag_view();
        } else if self.smart_view.is_some() {
            self.show_smart_view();
        } else if let Some(sel) = self.selected.as_ref() {
            if let Some(msgs) = self.message_cache.get(&(sel.account_id, sel.folder_id)) {
                self.message_list.emit(MessageListInput::SetMessages {
                    messages: msgs.to_vec(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::{labelled, msg};

    #[test]
    fn one_gmail_message_under_three_labels_joins_the_conversation_once() {
        // INBOX = 1, All Mail = 2, Important = 3 — the same mail in all three,
        // which is how 1210 of one real account's 1212 messages are stored.
        let related = vec![
            labelled(2, 500, "<a@example.com>"),
            labelled(3, 900, "<a@example.com>"),
            labelled(1, 42, "<a@example.com>"),
            labelled(2, 501, "<b@example.com>"),
        ];
        let kept = dedupe_label_copies(related, &[], 1);
        assert_eq!(kept.len(), 2, "two messages, not four copies");
        let a = kept
            .iter()
            .find(|m| m.message_id == "<a@example.com>")
            .unwrap();
        assert_eq!(
            (a.folder_id, a.uid),
            (1, 42),
            "the copy from the folder on screen wins, whatever order it arrived in"
        );
    }

    #[test]
    fn a_label_copy_of_a_message_already_shown_is_dropped() {
        let conv = vec![labelled(1, 42, "<a@example.com>")];
        let kept = dedupe_label_copies(vec![labelled(2, 500, "<a@example.com>")], &conv, 1);
        assert!(kept.is_empty(), "the conversation already holds this mail");
    }

    #[test]
    fn a_reused_message_id_is_not_treated_as_the_same_mail() {
        let mut spam = labelled(2, 900, "<a@example.com>");
        spam.from_addr = "spammer@fake.example".into();
        let mut mine = labelled(1, 42, "<a@example.com>");
        mine.from_addr = "me@real.example".into();

        let kept = dedupe_label_copies(vec![mine, spam], &[], 1);
        assert_eq!(
            kept.len(),
            2,
            "a shared id from another sender is other mail"
        );
    }

    #[test]
    fn messages_without_a_message_id_are_never_merged_together() {
        // Two distinct messages that carry no Message-ID must stay two: there is
        // nothing to match them on, and folding them would lose one.
        let kept = dedupe_label_copies(vec![labelled(1, 7, ""), labelled(1, 8, "")], &[], 1);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn a_conversation_in_one_folder_becomes_a_single_fetch() {
        let batches = batch_bodies_by_folder(vec![
            (1, 10, 100, "INBOX".into()),
            (1, 11, 101, "INBOX".into()),
            (1, 12, 102, "INBOX".into()),
        ]);
        assert_eq!(batches.len(), 1, "one folder is one round trip");
        assert_eq!(batches[0].0, (1, "INBOX".to_string()));
        // Order is the thread's, so the primary is fetched and rendered first.
        assert_eq!(batches[0].1, vec![(10, 100), (11, 101), (12, 102)]);
    }

    #[test]
    fn members_from_other_folders_and_accounts_get_their_own_fetch() {
        // A conversation pulled together across Inbox and Sent, plus a second
        // account: batching must not drop the members that don't share the
        // first one's folder.
        let batches = batch_bodies_by_folder(vec![
            (1, 10, 100, "INBOX".into()),
            (1, 11, 55, "Sent".into()),
            (1, 12, 101, "INBOX".into()),
            (2, 13, 7, "INBOX".into()),
        ]);
        assert_eq!(batches.len(), 3);
        assert_eq!(
            batches[0],
            ((1, "INBOX".to_string()), vec![(10, 100), (12, 101)])
        );
        assert_eq!(batches[1], ((1, "Sent".to_string()), vec![(11, 55)]));
        assert_eq!(batches[2], ((2, "INBOX".to_string()), vec![(13, 7)]));
        let members: usize = batches.iter().map(|(_, items)| items.len()).sum();
        assert_eq!(members, 4, "every member is still requested");
    }

    #[test]
    fn a_vanished_message_never_throws_you_to_the_top() {
        let m = |uid| msg(1, 1, uid);
        let previous = vec![m(10), m(9), m(8), m(7)];
        let after = vec![m(10), m(9), m(7)];
        // The message that vanished sat third; whatever is third now takes over.
        assert_eq!(next_after_vanish(Some(&previous), &after, 8), Some((1, 7)));
        // It sat last: the new last row.
        let after = vec![m(10), m(9)];
        assert_eq!(next_after_vanish(Some(&previous), &after, 7), Some((1, 9)));
        // No slot to go by — deleting prunes the cache, so this is the delete
        // path — and the top of the folder is NOT the answer (#19).
        assert_eq!(next_after_vanish(Some(&previous), &after, 999), None);
        assert_eq!(next_after_vanish(None, &after, 8), None);
        // Nothing left at all.
        assert_eq!(next_after_vanish(Some(&previous), &[], 8), None);
    }

    #[test]
    fn same_mail_needs_id_sender_and_time() {
        let mut a = labelled(1, 1, "<a@x>");
        a.from_addr = "s@x".into();
        a.timestamp = 5;
        let mut b = a.clone();
        b.folder_id = 2;
        b.subject = "other".into();
        assert!(same_mail(&a, &b));
        b.timestamp = 6;
        assert!(!same_mail(&a, &b));
        let mut c = a.clone();
        c.message_id = String::new();
        assert!(!same_mail(&c, &c.clone()));
    }

    #[test]
    fn events_match_by_body_key_or_by_id() {
        let mut m = msg(2, 3, 40);
        m.id = 9;
        assert_eq!(body_key(&m), (2, 3, 40));
        assert!(is_event_target(&m, Some((2, 3, 40)), 7, 7));
        assert!(!is_event_target(&m, Some((2, 3, 41)), 2, 9));
        assert!(is_event_target(&m, None, 2, 9));
        assert!(!is_event_target(&m, None, 2, 40));
    }
}
