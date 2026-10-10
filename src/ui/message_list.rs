//! Middle pane: the scrollable list of messages in the selected folder,
//! with a search field and live filtering.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;

use crate::models::{Message, ThreadSummary};
use crate::ui::context_menu::{show_context_menu_popover, MenuEntry};
use crate::i18n::i18n;
use crate::ui::message_row::{RowData, RowMeta, RowShared};
pub use crate::ui::message_row::{tag_menu_entries, RowAction};

/// The message-list pane's floor: exactly what a conversation-member card
/// needs to show a row's full actions palette — the tightest real constraint
/// in the list. The sum of the card's insets (10px rail margin, 10px + 8px
/// card margins, 12px + 12px card padding), the avatar (38px), the unread
/// dot (8px), three 8px gaps, and the 234px actions-line reservation.
const LIST_MIN_WIDTH: i32 = 348;

/// What an expanded conversation needs beyond [`LIST_MIN_WIDTH`]: the member
/// cards' 10px rail indent plus their card margin/padding beyond a plain
/// pill's. The pane's floor grows by this while any thread is open, so the
/// cards' (and the head pill's) right inset is never clipped off the pane.
const THREAD_EXPANDED_EXTRA: i32 = 12;

/// Fire `DayChanged` shortly after the next local midnight (re-armed each time).
fn schedule_midnight_refresh(sender: &ComponentSender<MessageList>) {
    use chrono::Timelike;
    let secs = 86_400u32
        .saturating_sub(chrono::Local::now().num_seconds_from_midnight())
        .saturating_add(2);
    let input = sender.input_sender().clone();
    gtk::glib::timeout_add_seconds_local(secs, move || {
        let _ = input.send(MessageListInput::DayChanged);
        gtk::glib::ControlFlow::Break
    });
}

/// Order two messages by the chosen sort (ties fall back to date).
fn message_cmp(a: &Message, b: &Message, order: SortOrder) -> std::cmp::Ordering {
    let name_of = |m: &Message| {
        if m.from_name.trim().is_empty() {
            m.from_addr.to_lowercase()
        } else {
            m.from_name.to_lowercase()
        }
    };
    match order {
        SortOrder::DateNewest => b.timestamp.cmp(&a.timestamp),
        SortOrder::DateOldest => a.timestamp.cmp(&b.timestamp),
        SortOrder::Sender => name_of(a).cmp(&name_of(b)).then(b.timestamp.cmp(&a.timestamp)),
        SortOrder::Subject => normalize_subject(&a.subject)
            .cmp(&normalize_subject(&b.subject))
            .then(b.timestamp.cmp(&a.timestamp)),
        // `true` sorts after `false`, so compare b-vs-a to put unread/flagged first.
        SortOrder::UnreadFirst => b.unread.cmp(&a.unread).then(b.timestamp.cmp(&a.timestamp)),
        SortOrder::FlaggedFirst => b.starred.cmp(&a.starred).then(b.timestamp.cmp(&a.timestamp)),
        // The single line's other columns (#334).
        SortOrder::Recipients => {
            let to = |m: &Message| crate::ui::message_row::recipient_names(&m.to).to_lowercase();
            to(a).cmp(&to(b)).then(b.timestamp.cmp(&a.timestamp))
        }
        SortOrder::Account => a.account_id.cmp(&b.account_id).then(b.timestamp.cmp(&a.timestamp)),
        SortOrder::Attachment => b.has_attachment.cmp(&a.has_attachment).then(b.timestamp.cmp(&a.timestamp)),
        SortOrder::Importance => b
            .importance
            .to_i64()
            .cmp(&a.importance.to_i64())
            .then(b.timestamp.cmp(&a.timestamp)),
        // Soonest due first; mail with no due date after all that has one.
        SortOrder::Due => (a.due == 0, a.due).cmp(&(b.due == 0, b.due)).then(b.timestamp.cmp(&a.timestamp)),
    }
}

/// The order a click on a column's heading sorts by (#334), if the column
/// has one. In Sent the sender column names the recipients, and sorts by
/// them.
pub fn column_sort(column: crate::config::ListColumn, show_recipient: bool) -> Option<SortOrder> {
    use crate::config::ListColumn as C;
    Some(match column {
        C::Star => SortOrder::FlaggedFirst,
        C::Sender if show_recipient => SortOrder::Recipients,
        C::Sender => SortOrder::Sender,
        C::Recipients => SortOrder::Recipients,
        C::Subject => SortOrder::Subject,
        C::Attachment => SortOrder::Attachment,
        C::Importance => SortOrder::Importance,
        C::Account => SortOrder::Account,
        C::Due => SortOrder::Due,
        C::Date => SortOrder::DateNewest,
        C::Correspondents | C::Tags => return None,
    })
}

/// The conversation key a message belongs to: its owning account plus the
/// subject with reply/forward prefixes stripped. Messages with no subject get a
/// per-message key (by UID) so they never group together.
/// Lower-case the subject and strip any leading run of reply/forward prefixes
/// (`Re:`, `Fwd:`, …) so subject-sorting keeps a topic and its replies adjacent.
fn normalize_subject(subject: &str) -> String {
    const PREFIXES: &[&str] = &["re:", "fwd:", "fw:", "aw:", "sv:", "antw:", "wg:"];
    let mut s = subject.trim();
    loop {
        let lower = s.to_ascii_lowercase();
        let mut stripped = false;
        for p in PREFIXES {
            if lower.starts_with(p) {
                s = s[p.len()..].trim_start();
                stripped = true;
                break;
            }
        }
        if !stripped {
            break;
        }
    }
    s.trim().to_ascii_lowercase()
}

/// Which shown row stands for a reader key: the message's own row when the
/// list has one, and otherwise the row of the conversation it belongs to.
///
/// With expandable conversations off, a reply never gets a row of its own and
/// never will — the thread is only ever the one head row here. Without this
/// fallback the reader's selection would find nothing to select, and the
/// conversation would appear to deselect itself the moment one of its other
/// messages was clicked in the reading pane (#211). The head row stands for
/// the whole thread, so it is what stays lit however the user moves through
/// the cards.
///
/// A conversation reaches across folders, so some of its cards — the user's
/// own replies, pulled in from Sent — belong to no row in this folder at all
/// and are not in `msg_thread` either. `viewed` is the row the open
/// conversation was opened from, and `emitted` is that conversation as it was
/// handed to the reader: a card from it keeps that row lit.
fn row_for_reader_key<M: std::borrow::Borrow<Message>>(
    key: &(u32, u32),
    shown: &[M],
    msg_thread: &std::collections::HashMap<(u32, u32), (u32, String)>,
    emitted: &[(u32, u32)],
    viewed: Option<(u32, u32)>,
) -> Option<usize> {
    let own_row = shown.iter().map(|m| m.borrow()).position(|m| (m.account_id, m.id) == *key);
    let thread_row = || {
        let tkey = msg_thread.get(key)?;
        shown
            .iter()
            .map(|m| m.borrow())
            .position(|m| msg_thread.get(&(m.account_id, m.id)) == Some(tkey))
    };
    let viewed_row = || {
        if !emitted.contains(key) {
            return None;
        }
        let viewed = viewed?;
        shown.iter().map(|m| m.borrow()).position(|m| (m.account_id, m.id) == viewed)
    };
    own_row.or_else(thread_row).or_else(viewed_row)
}

/// The conversation the reader is showing, for [`row_for_reader_key`]: what
/// this list `emitted` when the row was opened, plus whatever the app has
/// `merged` into it since from other folders — the user's own replies from
/// Sent, which reach the reader under ids this list never listed (#220).
fn reader_conversation(emitted: &[(u32, u32)], merged: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut all = emitted.to_vec();
    all.extend(merged.iter().filter(|k| !emitted.contains(k)).copied());
    all
}

/// Whether the message at `key` is the row its conversation collapses to:
/// the oldest member among those the rows were grouped from, or a message
/// grouped with nothing there (#236).
///
/// Judged over `msg_thread` and `thread_members`, which the rebuild fills
/// from the rendered window, and not over everything the list holds. The
/// two differ exactly when a conversation's older messages sit past the
/// window: the row on screen is then the oldest member *shown*, while the
/// conversation's true head is further down, unrendered. Asked against the
/// whole list, that row failed the head test, was taken for a reply picked
/// out of an opened-up thread, and was shown alone in the reader, with no
/// look in the cache for the rest. The unified Inboxes hit this constantly:
/// several inboxes merged, newest first, put a conversation's start past
/// the rendered window within days, while one folder rarely does.
fn heads_its_row(
    key: (u32, u32),
    msg_thread: &std::collections::HashMap<(u32, u32), (u32, String)>,
    thread_members: &std::collections::HashMap<(u32, String), Vec<(u32, u32)>>,
) -> bool {
    match msg_thread.get(&key) {
        None => true,
        Some(thread) => thread_members.get(thread).and_then(|m| m.first()) == Some(&key),
    }
}

/// The message a conversation's row should speak for when it is not one this
/// folder holds: the cache's newest member, if it is later than anything on
/// screen (#236).
///
/// A mail you answered is a single row in the Inbox, with the answer filed in
/// Sent. The row goes on showing the other side's last word and its time, so
/// nothing short of opening the conversation says you have replied. `None`
/// keeps the row exactly as the folder describes it: the cache knows of
/// nothing newer, or knows of nothing at all yet. Whether it is asked at all
/// is the "Show your own replies in the message list" setting, off by
/// default.
/// Which of a conversation's `found` members, from anywhere in the account,
/// a row nests under the folder's `own` (#309): not the ones in a folder on
/// the list, which are rows of their own, nor a copy of mail already there
/// (Gmail files one message under every label it has), and only those the
/// list's filters let through. A message with no Message-ID cannot be told
/// apart from its copies, so it is left out.
fn nested_members<M: std::borrow::Borrow<Message>>(
    found: &[Message],
    own: &[M],
    listed_folders: &std::collections::HashSet<(u32, u32)>,
    passes: &impl Fn(&Message) -> bool,
) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::new();
    for m in found {
        if m.message_id.is_empty()
            || listed_folders.contains(&(m.account_id, m.folder_id))
            || !passes(m)
            || own.iter().map(|o| o.borrow()).chain(&out).any(|o| o.message_id == m.message_id)
        {
            continue;
        }
        out.push(m.clone());
    }
    out
}

fn latest_elsewhere(
    summary: Option<&crate::models::ThreadSummary>,
    newest_here: i64,
) -> Option<crate::models::ThreadLatest> {
    summary.and_then(|s| s.latest.as_ref()).filter(|l| l.timestamp > newest_here).cloned()
}

/// Which of the page's conversations still need their real size looked up
/// (#222): the ones nobody has asked the cache about yet.
///
/// Every rebuild runs this, and rebuilds are cheap and frequent — a sync, a
/// scroll, each keystroke of a search. Asking is not cheap: it is a scan of the
/// account's message index. So a thread is asked about once and remembered.
/// That is also what stops the loop, since the answer arrives as a rebuild:
/// with every thread on the page already asked about, the next pass has nothing
/// to send and the list settles.
fn unasked_threads(
    listed: &[(u32, String, Vec<String>)],
    asked: &std::collections::HashSet<(u32, String)>,
) -> Vec<(u32, String, Vec<String>)> {
    listed
        .iter()
        .filter(|(aid, root, _)| !asked.contains(&(*aid, root.clone())))
        .cloned()
        .collect()
}

/// Group messages into conversations by their reply headers (Message-ID linked
/// via In-Reply-To / References), scoped per account. Returns each message's
/// thread key `(account_id, root)`, looked up by [`thread_slot`]. Messages with
/// no reply relationship get a unique key (a thread of one) — so unrelated
/// messages that merely share a subject are never threaded together.
///
/// Age plays no part: a message threads because its headers say what it answers,
/// and those are indexed with every message. Grouping runs over the rendered
/// window, so covering the whole mailbox costs no more than covering a day of
/// it; what a conversation costs to *open* is bounded separately, by
/// `THREAD_MEMBER_LIMIT`.
/// How many rows a large list puts on screen at once, and adds each time it
/// is scrolled near the end of them.
const LIST_WINDOW: usize = 1200;

/// What working out conversations reads of a message: from the message
/// itself, or from the copy of just these fields that a large list hands to
/// a background thread.
trait ThreadFields {
    fn account_id(&self) -> u32;
    fn folder_id(&self) -> u32;
    fn id(&self) -> u32;
    fn uid(&self) -> u32;
    fn message_id(&self) -> &str;
    fn references(&self) -> &str;
}

impl ThreadFields for Message {
    fn account_id(&self) -> u32 { self.account_id }
    fn folder_id(&self) -> u32 { self.folder_id }
    fn id(&self) -> u32 { self.id }
    fn uid(&self) -> u32 { self.uid }
    fn message_id(&self) -> &str { &self.message_id }
    fn references(&self) -> &str { &self.references }
}

impl ThreadFields for Rc<Message> {
    fn account_id(&self) -> u32 { self.as_ref().account_id }
    fn folder_id(&self) -> u32 { self.as_ref().folder_id }
    fn id(&self) -> u32 { self.as_ref().id }
    fn uid(&self) -> u32 { self.as_ref().uid }
    fn message_id(&self) -> &str { &self.as_ref().message_id }
    fn references(&self) -> &str { &self.as_ref().references }
}

/// A message's threading fields, owned, to be worked on off the main loop.
struct ThreadInput {
    account_id: u32,
    folder_id: u32,
    id: u32,
    uid: u32,
    message_id: String,
    references: String,
}

impl ThreadFields for ThreadInput {
    fn account_id(&self) -> u32 { self.account_id }
    fn folder_id(&self) -> u32 { self.folder_id }
    fn id(&self) -> u32 { self.id }
    fn uid(&self) -> u32 { self.uid }
    fn message_id(&self) -> &str { &self.message_id }
    fn references(&self) -> &str { &self.references }
}

fn compute_thread_keys<M: ThreadFields>(
    msgs: &[M],
    links: &[(u32, String, String)],
) -> std::collections::HashMap<(u32, u32, u32), (u32, String)> {
    use std::collections::HashMap;

    // Union-find over message-id nodes (namespaced by account). The nodes
    // borrow their ids from the messages: a large folder is grouped whenever
    // it changes, and a String per id made most of the cost (#323).
    #[derive(Clone, Copy, PartialEq, Eq, Hash)]
    enum Node<'a> {
        Id(u32, &'a str),
        Uid(u32, u32, u32),
    }
    #[derive(Default)]
    struct Forest<'a> {
        index: HashMap<Node<'a>, usize>,
        nodes: Vec<Node<'a>>,
        parent: Vec<usize>,
    }
    impl<'a> Forest<'a> {
        fn node(&mut self, n: Node<'a>) -> usize {
            if let Some(&i) = self.index.get(&n) {
                return i;
            }
            let i = self.nodes.len();
            self.nodes.push(n);
            self.parent.push(i);
            self.index.insert(n, i);
            i
        }
        fn find(&mut self, mut x: usize) -> usize {
            while self.parent[x] != x {
                self.parent[x] = self.parent[self.parent[x]];
                x = self.parent[x];
            }
            x
        }
        fn union(&mut self, a: usize, b: usize) {
            let (ra, rb) = (self.find(a), self.find(b));
            if ra != rb {
                self.parent[ra] = rb;
            }
        }
    }
    // A message with its own Message-ID is a real node; one without gets a unique
    // node keyed by folder and uid so it only links through its references (if
    // any). A UID names a message in one folder only (#317).
    fn self_node<M: ThreadFields>(m: &M) -> Node<'_> {
        if m.message_id().is_empty() {
            Node::Uid(m.account_id(), m.folder_id(), m.uid())
        } else {
            Node::Id(m.account_id(), m.message_id())
        }
    }

    let mut forest = Forest::default();
    let mut own = Vec::with_capacity(msgs.len());
    for m in msgs {
        let sn = forest.node(self_node(m));
        own.push(sn);
        for r in m.references().split_whitespace() {
            let rn = forest.node(Node::Id(m.account_id(), r));
            forest.union(sn, rn);
        }
    }

    // Messages from elsewhere in the account contribute their links but never
    // appear: a reply in the Inbox and the one before it are two answers to the
    // same message in Sent, and without that message nothing says so.
    for (aid, id, refs) in links {
        let sn = forest.node(Node::Id(*aid, id));
        for r in refs.split_whitespace() {
            let rn = forest.node(Node::Id(*aid, r));
            forest.union(sn, rn);
        }
    }

    let mut names: HashMap<usize, String> = HashMap::new();
    let mut out = HashMap::with_capacity(msgs.len());
    for (m, sn) in msgs.iter().zip(own) {
        let root = forest.find(sn);
        let name = names.entry(root).or_insert_with(|| match forest.nodes[root] {
            Node::Id(aid, id) => format!("{aid}\u{0}{id}"),
            Node::Uid(aid, folder, uid) => format!("{aid}\u{0}uid{folder}/{uid}"),
        });
        out.insert((m.account_id(), m.folder_id(), m.id()), (m.account_id(), name.clone()));
    }
    out
}

/// [`SourceThreads`] worked out from `msgs` (in the list's order).
fn source_threads_of<M: ThreadFields>(msgs: &[M], links: &[(u32, String, String)], pool: bool) -> SourceThreads {
    let keys = compute_thread_keys(msgs, links);
    let mut members: std::collections::HashMap<(u32, String), Vec<usize>> = std::collections::HashMap::new();
    for (i, m) in msgs.iter().enumerate() {
        if let Some(key) = keys.get(&(m.account_id(), m.folder_id(), m.id())) {
            members.entry(key.clone()).or_default().push(i);
        }
    }
    SourceThreads { pool, keys, members }
}

// Short: the map runs to the size of the folder.
impl std::fmt::Debug for SourceThreads {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SourceThreads({} messages)", self.keys.len())
    }
}

/// How many of a large list's newest messages go on screen while its
/// conversations are worked out (see `MessageList::threads_pending`).
const PREVIEW_ROWS: usize = 2000;

/// A list at least this long works out its conversations on a background
/// thread, and keeps its rows until the answer is in; below it, the work is
/// quick enough to do in place.
const THREADS_OFF_MAIN: usize = 4000;

/// [`compute_thread_keys`] over everything a list holds, with each
/// conversation's members as positions in that same slice.
pub struct SourceThreads {
    /// Worked out from the search pool rather than the folder.
    pool: bool,
    keys: std::collections::HashMap<(u32, u32, u32), (u32, String)>,
    members: std::collections::HashMap<(u32, String), Vec<usize>>,
}

/// Run `f` once, when `clock` has painted its next frame, or after a short
/// wait for a window that is not drawing.
fn after_next_paint(clock: &gtk::gdk::FrameClock, f: impl FnOnce() + 'static) {
    let f = Rc::new(std::cell::Cell::new(Some(Box::new(f) as Box<dyn FnOnce()>)));
    let handler = Rc::new(std::cell::Cell::new(None::<glib::SignalHandlerId>));
    let once = {
        let (f, handler, clock) = (f.clone(), handler.clone(), clock.clone());
        move || {
            if let Some(id) = handler.take() {
                clock.disconnect(id);
            }
            if let Some(f) = f.take() {
                f();
            }
        }
    };
    let on_paint = once.clone();
    handler.set(Some(clock.connect_after_paint(move |_| on_paint())));
    glib::timeout_add_local_once(std::time::Duration::from_millis(100), once);
}

/// Where [`compute_thread_keys`] files a message: its account, folder and id.
/// The id alone is a UID, which only means something inside one folder, and a
/// search over every folder holds the same UID many times; keyed without the
/// folder, one of them joins the other's conversation (#317).
fn thread_slot(m: &Message) -> (u32, u32, u32) {
    (m.account_id, m.folder_id, m.id)
}

pub struct MessageList {
    /// The rows: a `ListView` builds only the ones on screen, so a folder
    /// of any size is one list (#323).
    list_view: gtk::ListView,
    /// The model behind it, its selection, and what every row shares.
    shared: Rc<RowShared>,
    /// The list's own input sender, for work it schedules on the main loop
    /// (coalesced rebuilds).
    input: relm4::Sender<MessageListInput>,
    /// A rebuild asked for and not yet run: `Some(preserve_scroll)`. Several
    /// arrivals in one main-loop pass (a folder's cached copy, its synced
    /// copy, fresh thread links, a view switch's flag changes) collapse into
    /// one rebuild instead of one each.
    rebuild_queued: Option<bool>,
    /// Another view was opened: its first rebuild waits for a frame, and
    /// fills the rows for the new look in passing (#401).
    switched: bool,
    /// The look changed during a switch and the rows on screen were left
    /// for the rebuild to fill.
    look_stale: std::cell::Cell<bool>,
    /// A `SelectAndLoad` that arrived while a rebuild was queued: the rows it
    /// must find are not there yet, so it waits for that rebuild and runs
    /// after it (a notification click follows the folder's list into the
    /// channel in the same pass, and the list is only built on the idle).
    pending_select: Option<(u32, u32)>,
    /// A message asked for from outside (a notification click) that the list
    /// does not hold yet, and when: it is selected once a rebuild brings it,
    /// unless the user picks something first (#332).
    late_select: Option<((u32, u32), std::time::Instant)>,
    /// All messages for the current folder (full searchable index). Shared
    /// with the rows rather than copied: a large folder is listed whole.
    all: Vec<Rc<Message>>,
    /// Every folder's messages (all accounts), supplied by the app while a search
    /// is active, so `AllFolders` scope can filter across the whole mailbox. Empty
    /// when not searching.
    search_pool: Vec<Rc<Message>>,
    /// Which messages the search field filters over.
    scope: SearchScope,
    /// The search field widget, kept so a folder switch can clear its text.
    search_entry: Option<gtk::SearchEntry>,
    /// The search toolbar is hidden until asked for (#102).
    search_open: bool,
    /// When the search closed itself (empty entry losing focus): the button
    /// click that caused that blur arrives right after and must not reopen.
    search_closed_at: Option<std::time::Instant>,
    /// The rows, in order: what the model shows, one message each.
    shown: Vec<Rc<Message>>,
    /// Total messages matching the current filter.
    total_matches: usize,
    query: String,
    gravatar: bool,
    /// Lines of preview text per row (1–3), from Preferences.
    preview_lines: u32,
    /// Whether rows draw their subject line (Focus Mode can take it away).
    show_subject: bool,
    /// Whether the colored avatars are drawn (#29).
    avatars: bool,
    /// Whether a sender's site icon may fill one (#30).
    sender_logos: bool,
    /// Ring each avatar in its account's color (the unified inbox view).
    colorize: bool,
    /// account_id → avatar color, for the rings.
    account_colors: std::collections::HashMap<u32, String>,
    /// Display-wide provider with each account's ring rule.
    color_provider: crate::ui::DisplayCss,
    /// Bumped when the circles must be looked up again, and when the tag
    /// definitions change (see `RowLook`).
    face_gen: u64,
    tags_gen: u64,
    /// The bulk bar's tag button, which its menu hangs from (#313).
    bulk_tag_btn: gtk::Button,
    /// The message currently being viewed, kept selected across list rebuilds.
    /// Keyed by (account_id, id) since UIDs collide across accounts in the
    /// unified "All Inboxes" view.
    selected_id: Option<(u32, u32)>,
    /// (account, message-id, references) for mail in the account's *other*
    /// folders. A conversation is often joined through messages that aren't on
    /// screen — every reply in an Inbox answers something in Sent — so those
    /// links are needed to see that the replies belong together.
    thread_links: Vec<(u32, String, String)>,
    /// The conversations of everything the list holds, worked out once and
    /// kept until the messages or the links change. Opening a message asks
    /// which conversation it belongs to, and grouping a large folder from
    /// scratch on every click held each one up by a visible beat (#323).
    source_threads: std::cell::RefCell<Option<Rc<SourceThreads>>>,
    /// What each conversation really is, read across the account's other
    /// folders and handed down by the app: its size (#222) and its newest
    /// message (#236). The list can only see its own folder, so a thread whose
    /// replies live in Sent would otherwise wear a badge that undercounts it
    /// and a row that never mentions your answer. Keyed by thread key, as
    /// `rebuild` groups them.
    thread_summaries: std::collections::HashMap<(u32, String), ThreadSummary>,
    /// Whether a row may speak for a message in another folder at all (#236).
    /// Off by default: the row describes the newest message this folder holds,
    /// as it always has. The sizes on the badges are not affected either way.
    thread_row_newest: bool,
    /// Each conversation the last rebuild listed, with the folder's own
    /// members: what the app needs to look its real size up when a row
    /// showing it comes on screen.
    groups: std::collections::HashMap<(u32, String), Vec<Rc<Message>>>,
    /// The folders the last rebuild listed: a conversation's members in them
    /// are rows of their own, so only the rest are nested (#309).
    listed_folders: std::collections::HashSet<(u32, u32)>,
    /// Which conversations have already been asked about. A rebuild runs on
    /// every keystroke of a search and on every sync, and each ask is a scan of
    /// the account's index — so a thread is asked about once and remembered,
    /// not re-asked whenever its row is redrawn. Cleared per account by
    /// [`MessageListInput::RecheckThreadSummaries`] when that account's mail moves.
    asked_threads: std::collections::HashSet<(u32, String)>,
    /// Accounts whose conversations on screen are to be asked about again
    /// once the rebuild under way has run.
    recheck_accounts: std::collections::HashSet<u32>,
    /// Every selected message key, so the whole selection survives list rebuilds
    /// (background syncs) until the user clicks away.
    selected_ids: Vec<(u32, u32)>,
    /// The conversation the last `Selected` carried, as keys: after a
    /// rebuild, a selected head whose conversation now holds more is
    /// reported (`ThreadGrew`) so the reader shows the new reply at once.
    emitted_thread: Vec<(u32, u32)>,
    /// Selection changes still expected from a reader-driven selection, and what
    /// that selection is. GTK reports each selection change separately and a
    /// rebuild adds more, so a single flag would be consumed by the first and
    /// let a later one re-open the message; only a change that matches what
    /// the reader asked for is suppressed, and anything else ends it at once.
    from_reader: u8,
    reader_keys: Vec<(u32, u32)>,
    /// How many rows are currently selected (drives the bulk-action bar).
    selection_count: usize,
    /// Which way the user last moved through the list: +1 down, -1 up.
    /// Deleting the viewed message advances in this direction (like Apple
    /// Mail): triaging downward selects the message below the deleted one,
    /// and after moving up the list, deletion selects the one above instead.
    /// Updated only by the user's own selection movement — the programmatic
    /// post-delete advance keeps its index and never flips it.
    nav_direction: i32,
    /// Conversation keys the user has toggled away from the default state
    /// (expanded when the default is collapsed, and vice versa).
    expanded_threads: std::collections::HashSet<(u32, String)>,
    /// Whether conversations start expanded (user preference; collapsed default).
    default_expanded: bool,
    /// The open folder is Sent: rows name recipients instead of senders (#27).
    show_recipient: bool,
    /// The list shows Trash or Junk, where menus offer "Move to Inbox" (#138).
    restorable: bool,
    /// The list shows Junk: "Not Spam" stands where "Mark as Spam" would.
    in_junk: bool,
    /// The list shows Drafts (a folder or the unified row): drafts are
    /// neither read nor unread, so the toggles are not offered.
    in_drafts: bool,
    /// The list shows a Templates folder (#360): a row's menu offers to use
    /// or edit the template, not to answer it.
    in_templates: bool,
    /// Rendered thread membership: message key → conversation key, rebuilt with
    /// the rows. Lets a read-state change on a hidden reply refresh its head.
    msg_thread: std::collections::HashMap<(u32, u32), (u32, String)>,
    /// Conversation key → member message keys (multi-message threads only).
    thread_members: std::collections::HashMap<(u32, String), Vec<(u32, u32)>>,
    /// Members of the conversations on the list that live in other folders
    /// (your replies in Sent, the archived parts), by message key: rows a
    /// conversation opens out into without being part of this folder (#309).
    /// Whole-conversation actions look members up in the folder's own index,
    /// so they never reach these.
    nested: std::collections::HashMap<(u32, u32), Rc<Message>>,
    /// Whether the folder's background index is fully loaded. When false, more
    /// rows may still stream in, so reaching the bottom shows a spinner.
    index_complete: bool,
    /// Whether a SetMessages has arrived since the last SetLoading — gates the
    /// empty-folder placeholder so it never flashes during a folder switch.
    loaded: bool,
    /// The list is scrolled to its bottom: with the index still streaming
    /// in, the spinner there says more is on its way.
    at_bottom: bool,
    /// How many rows the view holds: a large list is put on screen a window
    /// at a time, the rest added as it is scrolled toward its end, so
    /// tens of thousands of messages (All Archive) never stall the window
    /// filling a view nobody is looking at. `shown` and the view stay row
    /// for row the same; `tail` is what follows them.
    window: usize,
    tail: Vec<Rc<RowData>>,
    /// Bumped whenever the source changes, so a background answer about
    /// conversations (`threads_pending`) is used only for the source it was
    /// worked out from; and the generation a job is running for.
    threads_gen: std::cell::Cell<u64>,
    threads_job: std::cell::Cell<Option<u64>>,
    /// Threads whose replies are sliding shut. The rows stay in the list until
    /// the paired timer fires and drops them — otherwise they'd simply vanish
    /// rather than animate away. (PR #79)
    collapsing_threads: std::collections::HashMap<(u32, String), gtk::glib::SourceId>,
    /// The list's scroller.
    scroller: Option<gtk::ScrolledWindow>,
    /// Current sort order for the list.
    sort: SortOrder,
    /// Quick filter (#97): show only unread messages.
    unread_only: bool,
    /// Quick filter: show only starred messages.
    starred_only: bool,
    /// The last count string sent to the header bar, to emit only on change.
    last_count: String,
    /// Group messages into conversation threads (user preference).
    threading: bool,
    /// Whether a conversation row may expand into its member rows. Off: the
    /// row keeps its count chip and chevron, but never opens — the thread is
    /// read through the reader's cards instead.
    thread_expansion: bool,
    /// Whether rows carry the actions palette line at all (preference).
    list_palette: bool,
    /// One line per message (#334): set by the Layout setting, or by the
    /// pane's width when it is Automatic.
    single_line: bool,
    /// The single-line columns, in order, as the setting has them (#334).
    columns: Vec<crate::config::ListColumn>,
    /// The Microsoft 365 accounts: only their mail has a due date, so the
    /// Due column takes room only while the list holds some of it.
    graph_accounts: std::collections::HashSet<u32>,
    /// The list holds mail of one of `graph_accounts`.
    graph_in_view: bool,
    /// The sort runs the other way: a heading clicked twice (#334).
    sort_reversed: bool,
    /// Show the column headings over a single-line list (#334).
    headings: bool,
    headings_bar: gtk::Box,
    /// The widths columns were dragged to by their headings (#334).
    widths: std::collections::HashMap<crate::config::ListColumn, i32>,
    /// How wide the rows are, for fitting the columns into it.
    pane_width: i32,
    /// The headings' sized cells, which a drag resizes in place.
    heading_bins: std::cell::RefCell<std::collections::HashMap<crate::config::ListColumn, crate::ui::column_bin::ColumnBin>>,
}

/// The Correspondents column (#334): who wrote in a conversation, oldest
/// first and each once, you as "me". A conversation only you have written
/// in, a message in Sent say, names who it went to instead.
fn correspondents(msgs: &[&Message]) -> String {
    let mut sorted = msgs.to_vec();
    sorted.sort_by_key(|m| m.timestamp);
    let mut seen = std::collections::HashSet::new();
    let mut names: Vec<String> = Vec::new();
    let mut others = false;
    for m in &sorted {
        let own = crate::avatar::account_face(&m.from_addr).is_some();
        let key = if own { String::new() } else { m.from_addr.to_ascii_lowercase() };
        if !seen.insert(key) {
            continue;
        }
        if own {
            names.push(i18n("me"));
        } else {
            others = true;
            names.push(if m.from_name.is_empty() { m.from_addr.clone() } else { m.from_name.clone() });
        }
    }
    if !others {
        if let Some(newest) = sorted.last() {
            let to = crate::ui::message_row::recipient_names(&newest.to);
            if !to.is_empty() {
                return format!("To: {to}");
            }
        }
    }
    names.join(", ")
}

/// How the message list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    DateNewest,
    DateOldest,
    Sender,
    Subject,
    UnreadFirst,
    FlaggedFirst,
    Recipients,
    Account,
    Attachment,
    Importance,
    Due,
}

impl SortOrder {
    const KEYS: [(SortOrder, &'static str); 11] = [
        (SortOrder::DateNewest, "date_newest"),
        (SortOrder::DateOldest, "date_oldest"),
        (SortOrder::Sender, "sender"),
        (SortOrder::Subject, "subject"),
        (SortOrder::UnreadFirst, "unread"),
        (SortOrder::FlaggedFirst, "flagged"),
        (SortOrder::Recipients, "recipients"),
        (SortOrder::Account, "account"),
        (SortOrder::Attachment, "attachment"),
        (SortOrder::Importance, "importance"),
        (SortOrder::Due, "due"),
    ];

    pub fn from_key(key: &str) -> Self {
        SortOrder::KEYS.iter().find(|(_, k)| *k == key).map(|(o, _)| *o).unwrap_or(SortOrder::DateNewest)
    }

    pub fn key(self) -> &'static str {
        SortOrder::KEYS.iter().find(|(o, _)| *o == self).map(|(_, k)| *k).unwrap_or("date_newest")
    }

    /// Whether the order runs from the most to the least: newest first,
    /// starred first and so on, against A to Z.
    fn downwards(self) -> bool {
        matches!(
            self,
            SortOrder::DateNewest
                | SortOrder::UnreadFirst
                | SortOrder::FlaggedFirst
                | SortOrder::Attachment
                | SortOrder::Importance
        )
    }
}

/// Which messages the search field filters over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    /// Every folder of every account (the merged `search_pool`).
    AllFolders,
    /// Only the folder currently shown (the local `all` index).
    ThisFolder,
}

/// A bulk action applied to every selected message at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkAction {
    MarkRead,
    MarkUnread,
    Flag,
    /// Remove the star from every selected/threaded message (the bulk bar
    /// itself only offers Flag; conversations need the inverse too).
    Unflag,
    Archive,
    Spam,
    /// The reverse of `Spam`, for a selection in Junk (#168).
    NotSpam,
    Delete,
    /// Back to the Inbox, for a selection in Trash or Junk (#138).
    MoveToInbox,
}

#[derive(Debug)]
pub enum MessageListInput {
    /// The header's quick filter (#97): scope the list to unread mail.
    SetUnreadOnly(bool),
    /// The header's starred quick filter.
    SetStarredOnly(bool),
    SetMessages { messages: Vec<Message> },
    /// Merge more indexed messages into the current list (background backfill),
    /// preserving the current search query and view.
    AppendMessages { messages: Vec<Message> },
    SetLoading,
    SetThreading(bool),
    /// Whether conversation rows may expand into their members in the list
    /// (the row keeps its chip and chevron either way).
    SetThreadExpansion(bool),
    /// Whether rows carry the actions palette line at all.
    SetListPalette(bool),
    /// Resolve what deleting the current selection means: a lone selected row
    /// that heads a conversation stands for the whole thread (output
    /// `DeleteThread`); anything else is an ordinary `Bulk` delete.
    ResolveDelete,
    /// Reply headers from the account's other folders, so a conversation joined
    /// through a message that isn't on screen still groups.
    SetThreadLinks(Vec<(u32, String, String)>),
    /// What the whole conversations look like from the cache, keyed by thread
    /// key (#222, #236).
    SetThreadSummaries(Vec<((u32, String), ThreadSummary)>),
    /// This account's mail changed, so what was read may no longer be the
    /// conversation: ask again about the ones on screen (#222). What the rows
    /// say stays until the answers arrive.
    RecheckThreadSummaries(u32),
    /// The reader assembled this message's conversation from the cache, at
    /// this size: a row that counts fewer asks again (#351).
    ConversationSize { account_id: u32, id: u32, size: usize },
    /// Whether a conversation's row speaks for the newest message anywhere in
    /// the account, the replies you sent included (#236).
    SetThreadRowNewest(bool),
    /// Whether conversations start expanded (true) or collapsed (false).
    SetThreadsExpanded(bool),
    SetGravatar(bool),
    /// The GNOME Contacts photo index changed (EDS sync, or the first load
    /// finished) — refresh visible circles without losing the scroll position.
    ContactPhotosChanged,
    /// The open folder is (or stopped being) a Sent folder — rows name the
    /// recipient there instead of the sender (#27).
    SetShowRecipient(bool),
    /// Show or hide the colored avatars (#29).
    SetAvatars(bool),
    /// The avatars and preview lines together, as the settings and Focus
    /// Mode leave them. `animate` (a Focus Mode toggle) slides the avatars
    /// away before the rows are rebuilt without them, or builds them folded
    /// and slides them in.
    SetLook { avatars: bool, preview_lines: u32, subject: bool, animate: bool },
    /// The Focus Mode slide finished: rebuild the rows as they now are.
    LookSettled,
    /// Lay the rows out on one line, or as cards (#334).
    SetSingleLine(bool),
    /// The single-line columns, in order (#334).
    SetColumns(Vec<crate::config::ListColumn>),
    /// Show the column headings over a single-line list, or not (#334).
    SetHeadings(bool),
    /// A column's heading was clicked: sort by it, or the other way (#334).
    SortByColumn(crate::config::ListColumn),
    /// A heading's edge was dragged to `width`, or double-clicked (`None`:
    /// the column's own width). `done` once the drag is over (#334).
    ResizeColumn { column: crate::config::ListColumn, width: Option<i32>, done: bool },
    /// The saved column widths (#334).
    SetColumnWidths(std::collections::HashMap<crate::config::ListColumn, i32>),
    /// Fill the headings again, once a handler that set them going is over.
    RefreshHeadings,
    /// The rows are this wide now.
    PaneWidth(i32),
    /// Each account's name for the Account column, and which accounts are
    /// Microsoft 365 ones, for the Due column (#334).
    SetAccountNames {
        names: std::collections::HashMap<u32, String>,
        graph: std::collections::HashSet<u32>,
    },
    /// Fill them with senders' own site icons, or stop (#30).
    SetSenderLogos(bool),
    /// The date or clock preference changed: every row's date is built with the
    /// row, so they are built again (#32).
    RefreshDates,
    SetColorize(bool),
    /// The local day rolled over — re-render rows so "Today" stays accurate.
    DayChanged,
    SetAccountColors(std::collections::HashMap<u32, String>),
    Search(String),
    /// Change the search scope (all folders vs. the current folder).
    SetScope(SearchScope),
    /// Replace the cross-folder search pool (all folders, all accounts). Sent by
    /// the app when a search begins; cleared to empty when it ends.
    SetSearchPool(Vec<Message>),
    /// The reader's selection: mirror whichever of these the list has rows for,
    /// so everything that acts on the list's selection acts on the messages the
    /// user pointed at. The reader is already showing them, so this must not
    /// re-open anything. Messages the list cannot represent — a reply read in
    /// from Sent, which belongs to another folder — keep the row of the
    /// conversation they were read in with (#211, #220). `conversation` is that
    /// conversation as the reader has it, which is wider than what this list
    /// handed over: the app pulls the user's own replies in from Sent after
    /// the fact, under ids the list never sees.
    SelectFromReader { keys: Vec<(u32, u32)>, conversation: Vec<(u32, u32)> },
    /// The set of selected rows changed (single click, Ctrl/Shift multi-select).
    SelectionChanged,
    /// A row was activated (double-click / Enter): pop it out into its own window.
    RowActivated(i32),
    /// Apply a bulk action to every selected message.
    Bulk(BulkAction),
    /// The bulk bar's read button: read when any selected message is
    /// unread, unread when none is (#313).
    BulkToggleRead,
    /// The bulk bar's tag button: the tag menu for the selection, under
    /// the button.
    BulkTagMenu,
    /// Deselect everything.
    ClearSelection,
    /// Move the selection by `delta` rows (single-key j/k and the arrow keys).
    MoveSelection(i32),
    /// Add or remove the focused row from the selection, without opening it.
    ToggleSelection,
    /// Put keyboard focus on the list (so the arrow keys work again),
    /// deliberately scrolling back to the selected row — the "back to list"
    /// shortcut.
    FocusList,
    /// Housekeeping focus restore (e.g. after a compose window closes) that
    /// doesn't scroll the viewport, unlike `FocusList`.
    ReclaimFocus,
    /// Put the cursor in the search field.
    FocusSearch,
    /// Close and clear the search toolbar (#102): Esc, empty focus-out, or
    /// the header button while open.
    CloseSearch,
    /// Showcase staging: open row N's actions palette (screenshot hook only —
    /// see HYLKI_SHOWCASE_PALETTE in app.rs).
    DebugOpenPalette(usize),
    /// Showcase only (HYLKI_SHOWCASE_SWIPE): drive row `index` through a full
    /// swipe and release, so the commit exit can be caught in stills — there
    /// is no way to inject a real gesture on this desktop.
    DebugSwipe { index: usize, left: bool },
    /// Showcase only (HYLKI_SHOWCASE_ROW_MENU): the first row's menu.
    DebugRowMenu,
    /// Expand/collapse a conversation thread.
    ToggleThread((u32, String)),
    /// A collapsing thread's replies have finished sliding shut — drop them
    /// from the list for real (see `start_collapse_thread`).
    FinishCollapseThread((u32, String)),
    /// Change the list sort order.
    SetSort(SortOrder),
    /// A message was read. Messages are named by their slot (account,
    /// folder, UID) here: a UID repeats from folder to folder and, in the
    /// unified view, from account to account (#333).
    MarkRead((u32, u32, u32)),
    SetRead { slot: (u32, u32, u32), read: bool },
    /// A hover-palette action for a specific message (forwarded to the app).
    RowAction { action: RowAction, message: Box<Message> },
    SetStarred { slot: (u32, u32, u32), starred: bool },
    /// A message's keywords changed (a tag put on or taken off, #71).
    SetKeywords { slot: (u32, u32, u32), keywords: Vec<String> },
    /// The tag definitions changed: rows rebuild their chips.
    SetTags(Vec<crate::config::Tag>),
    /// A row's tag menu toggled a tag — passed up to the app.
    SetTagFor { message: Box<Message>, keyword: String, add: bool },
    /// Update a message's attachment indicator (e.g. clearing a false paperclip).
    /// A message's paperclip, found by where it lives: its own folder and
    /// UID, since a UID is unique only within its folder.
    SetHasAttachment { account_id: u32, folder_id: u32, uid: u32, has: bool },
    Remove(u32),
    /// Remove many messages in a single batch (bulk archive/delete/spam), so the
    /// list updates in one render pass instead of one per message.
    RemoveMany(Vec<u32>),
    /// Secondary-click on the row showing `key`, at (x, y) in the list's
    /// coordinates: open the context menu.
    ContextMenu { x: f64, y: f64, key: (u32, u32) },
    /// Rows have just started showing conversations nobody has asked the
    /// cache about yet (#222).
    AskThreads,
    /// Set the actions palette auto-collapse delay (seconds).
    SetPaletteCollapse(u64),
    /// Open the actions palette on row hover, without the ⋯ click.
    SetPaletteHover(bool),
    /// A row's ⋯, in menu mode: the row's menu at the button's corner.
    /// Swap the swipe-gesture sides (#swipe).
    SetSwipeReversed(bool),
    /// Turn the swipe gesture on or off (#92).
    SetSwipeEnabled(bool),
    /// How far a trackpad two-finger swipe has to travel to fire the action.
    SetSwipeSensitivity(f64),
    /// The list shows Trash or Junk: menus offer "Move to Inbox" (#138).
    SetRestorable(bool),
    /// The list shows Junk: "Not Spam" replaces "Mark as Spam" (#168).
    SetInJunk(bool),
    /// The list shows Drafts: read/unread toggles are withheld.
    SetInDrafts(bool),
    SetInTemplates(bool),
    /// Folder switch: drop any search and scroll back to the top (a plain
    /// `SetMessages` keeps the place, for refreshes).
    ResetPaging,
    /// Whether the current folder's background index is fully loaded.
    SetIndexComplete(bool),
    /// Run the rebuild queued by [`MessageList::queue_rebuild`].
    RunQueuedRebuild,
    /// The list was scrolled to (or away from) its bottom.
    AtBottom(bool),
    /// Scrolled near the end of the rows on screen: add the next window.
    NearEnd,
    /// A large list's conversations, worked out off the main loop.
    ThreadsReady { gen: u64, threads: Box<SourceThreads> },
    /// Mark which message is being viewed so it stays highlighted across
    /// rebuilds; `None` clears the selection (e.g. on folder switch).
    SetSelected(Option<u32>),
    /// Select a message by `(account_id, id)` AND load it in the reader — used to
    /// advance after the viewed message is removed by a background sync.
    SelectAndLoad((u32, u32)),
    /// A row palette's Move to…: open the picker for that row (and, for a
    /// conversation head, its thread) at window point (x, y).
    RowMoveTo { message: Box<Message>, x: f64, y: f64 },
}

#[derive(Debug)]
pub enum MessageListOutput {
    /// A message was selected. `thread` holds the whole conversation (newest
    /// first) when the newest/head row was chosen, so the reader can show it as a
    /// scrollable conversation; otherwise it's just `[message]`.
    /// A message was opened. `thread` is the conversation to render (just
    /// `message` when it is not one). `solo` marks the deliberate case: the user
    /// picked one reply *inside* a conversation shown in the list, and wants
    /// only that message — so the reader must not go looking for its siblings.
    Selected { message: Message, thread: Vec<Message>, solo: bool },
    /// Every selected message, whenever that changes — the reader outlines the
    /// matching cards.
    SelectionKeys(Vec<(u32, u32)>),
    /// The header-bar count changed — app.rs shows it.
    CountChanged(String),
    /// A row was double-clicked: open it in its own window. `thread` is the
    /// whole conversation when the row heads one (same shape as `Selected`),
    /// so the window shows every card — otherwise just `[message]`.
    Activated { message: Message, thread: Vec<Message> },
    /// A context-menu or palette action chosen for a specific message.
    /// `conversation` holds the thread when the row stands for a collapsed
    /// conversation rather than for `message` alone, so a reply started there
    /// can answer the conversation instead of the head it is filed under
    /// (#210). Empty for an ordinary row, and for a head row shown alongside
    /// its expanded replies — there the row means only itself.
    Action { action: RowAction, message: Box<Message>, conversation: Vec<Message> },
    /// A tag toggled on a specific message (#71).
    SetTag { message: Box<Message>, keyword: String, add: bool },
    /// A tag toggled on every selected message from the bulk bar (#313).
    SetTagMany { messages: Vec<Message>, keyword: String, add: bool },
    /// A bulk action chosen for every currently-selected message.
    Bulk { action: BulkAction, messages: Vec<Message> },
    /// "Move To…" from a row's menu: open the folder picker for `messages`
    /// at window point (`x`, `y`). With `offer_whole`, the first message is
    /// the clicked conversation row and the rest its members: the picker
    /// offers moving them all (#171), or just the first.
    MoveTo { messages: Vec<Message>, offer_whole: bool, x: f64, y: f64 },
    /// The selected conversation gained a member since it was opened (a
    /// reply synced in): the head and the whole conversation as it now is.
    ThreadGrew { message: Message, thread: Vec<Message> },
    /// Conversations rows have started showing and the Message-IDs each is
    /// threaded by, so the app can ask the cache how big they really are
    /// (#222). Each is asked about once.
    ThreadsListed { groups: Vec<(u32, String, Vec<String>)> },
    /// Delete requested on a lone selected row that heads a whole conversation:
    /// every member of the thread, for the app to confirm and delete.
    DeleteThread { messages: Vec<Message> },
    /// The viewed message was removed and no row remains to advance to, so the
    /// reader should clear.
    SelectionCleared,
    /// The search field became active (non-empty) or inactive (empty), so the app
    /// can supply or drop the cross-folder search pool.
    SearchActive(bool),
    /// A column heading changed the sort (#334), by its key: the sort
    /// menus follow.
    SortChanged(&'static str),
    /// A column was resized by its heading (#334): the widths to keep.
    ColumnWidths(std::collections::HashMap<crate::config::ListColumn, i32>),
}

#[relm4::component(pub)]
impl SimpleComponent for MessageList {
    type Init = ();
    type Input = MessageListInput;
    type Output = MessageListOutput;

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            add_css_class: "message-list-pane",

            gtk::Box {
                add_css_class: "list-toolbar",
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 8,

                // The folder name, count and sort control live in the pane's
                // header bar now (app.rs) — only search needs this toolbar,
                // and it stays hidden until asked for (#102).
                gtk::Revealer {
                    set_transition_type: gtk::RevealerTransitionType::SlideDown,
                    #[watch]
                    set_reveal_child: model.search_open,

                gtk::Box {
                    set_spacing: 6,
                    add_css_class: "list-search-row",

                    #[name = "search_entry"]
                    gtk::SearchEntry {
                        set_hexpand: true,
                        // Its own default minimum is wider than the rows need.
                        set_width_chars: 3,
                        #[watch]
                        set_placeholder_text: Some(model.search_placeholder().as_str()),
                        connect_search_changed[sender] => move |entry| {
                            sender.input(MessageListInput::Search(entry.text().to_string()));
                        },
                        // Esc closes (SearchEntry's own stop signal).
                        connect_stop_search[sender] => move |_| {
                            sender.input(MessageListInput::CloseSearch);
                        },
                        // Leaving an empty entry closes too.
                        add_controller = gtk::EventControllerFocus {
                            connect_leave[sender] => move |ctl| {
                                let Some(entry) = ctl.widget().and_downcast::<gtk::SearchEntry>() else { return };
                                let sender = sender.clone();
                                crate::ui::close_if_focus_left_row(&entry, move || sender.input(MessageListInput::CloseSearch));
                            },
                        },
                    },

                    // Scope picker: 0 = All folders (default), 1 = This folder.
                    #[name = "scope_dropdown"]
                    gtk::DropDown {
                        set_valign: gtk::Align::Center,
                        set_tooltip_text: Some(i18n("Choose which folders to search").as_str()),
                        set_model: Some(&gtk::StringList::new(&[i18n("All folders").as_str(), i18n("This folder").as_str()])),
                        set_selected: 0,
                        connect_selected_notify[sender] => move |dd| {
                            let scope = if dd.selected() == 0 {
                                SearchScope::AllFolders
                            } else {
                                SearchScope::ThisFolder
                            };
                            sender.input(MessageListInput::SetScope(scope));
                        },
                    },
                },
                },
            },

            // Bulk-action bar, revealed while more than one message is selected.
            gtk::Revealer {
                set_transition_type: gtk::RevealerTransitionType::SlideDown,
                #[watch]
                set_reveal_child: model.selection_count > 1,

                // A revealer sliding *down* still reserves its child's width while
                // collapsed, so this bar of seven buttons was setting the whole
                // pane's minimum width — 340px — however narrow the rows became
                // (#29). Scrolled, its minimum is nothing and its natural size is
                // unchanged, so it only clips when the list is genuinely too narrow
                // to hold it.
                gtk::ScrolledWindow {
                    set_vscrollbar_policy: gtk::PolicyType::Never,
                    set_hscrollbar_policy: gtk::PolicyType::External,
                    set_propagate_natural_width: true,
                    set_propagate_natural_height: true,

                gtk::Box {
                    add_css_class: "bulk-bar",
                    set_spacing: 2,
                    // The buttons never take focus from the list: the
                    // selection keeps its focused highlight and the
                    // single-key shortcuts keep working after a click.

                    gtk::Label {
                        #[watch]
                        set_label: &format!("{} selected", model.selection_count),
                        set_hexpand: true,
                        set_halign: gtk::Align::Start,
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        add_css_class: "bulk-count",
                    },
                    // One toggle, like the star: it shows what a click will
                    // do. Drafts are neither read nor unread: it goes when
                    // the list shows them.
                    gtk::Button {
                        #[watch]
                        set_icon_name: if model.selection_any_unread() {
                            "hylki-mail-read-symbolic"
                        } else {
                            "mail-unread-symbolic"
                        },
                        #[watch]
                        set_tooltip_text: Some(if model.selection_any_unread() { i18n("Mark as Read") } else { i18n("Mark as Unread") }.as_str()),
                        add_css_class: "flat",
                        set_focus_on_click: false,
                        #[watch]
                        set_visible: !model.in_drafts,
                        connect_clicked => MessageListInput::BulkToggleRead,
                    },
                    // Mapped to Unflag by the Bulk handler once every
                    // selected message is starred (#313).
                    gtk::Button {
                        #[watch]
                        set_icon_name: if model.selection_all_starred() {
                            "hylki-non-starred-symbolic"
                        } else {
                            "starred-symbolic"
                        },
                        #[watch]
                        set_tooltip_text: Some(if model.selection_all_starred() { i18n("Remove Star") } else { i18n("Star") }.as_str()),
                        add_css_class: "flat",
                        set_focus_on_click: false,
                        connect_clicked => MessageListInput::Bulk(BulkAction::Flag),
                    },
                    // Only once a tag exists, like the row menu's Tags.
                    #[local_ref]
                    bulk_tag_btn -> gtk::Button {
                        set_icon_name: "tag-outline-symbolic",
                        set_tooltip_text: Some(i18n("Tags").as_str()),
                        add_css_class: "flat",
                        set_focus_on_click: false,
                        #[watch]
                        set_visible: !model.shared.tags.borrow().is_empty(),
                        connect_clicked => MessageListInput::BulkTagMenu,
                    },
                    gtk::Button {
                        set_icon_name: "mail-archive-symbolic",
                        set_tooltip_text: Some(i18n("Archive").as_str()),
                        add_css_class: "flat",
                        set_focus_on_click: false,
                        connect_clicked => MessageListInput::Bulk(BulkAction::Archive),
                    },
                    gtk::Button {
                        #[watch]
                        set_icon_name: if model.in_junk {
                            "mail-mark-notjunk-symbolic"
                        } else {
                            "mail-mark-junk-symbolic"
                        },
                        #[watch]
                        set_tooltip_text: Some(if model.in_junk { i18n("Not Spam") } else { i18n("Mark as Spam") }.as_str()),
                        add_css_class: "flat",
                        set_focus_on_click: false,
                        // Mapped to NotSpam in Junk by the Bulk handler.
                        connect_clicked => MessageListInput::Bulk(BulkAction::Spam),
                    },
                    gtk::Button {
                        set_icon_name: "user-trash-symbolic",
                        set_tooltip_text: Some(i18n("Delete").as_str()),
                        add_css_class: "flat",
                        set_focus_on_click: false,
                        connect_clicked => MessageListInput::Bulk(BulkAction::Delete),
                    },
                    gtk::Separator {
                        set_orientation: gtk::Orientation::Vertical,
                    },
                    gtk::Button {
                        set_icon_name: "edit-clear-symbolic",
                        set_tooltip_text: Some(i18n("Clear selection").as_str()),
                        add_css_class: "flat",
                        set_focus_on_click: false,
                        connect_clicked => MessageListInput::ClearSelection,
                    },
                },
                },
            },

            // The single line's column headings (#334), filled by
            // `sync_headings`; a click on one sorts by its column.
            gtk::Box {
                add_css_class: "list-headings-bar",
                #[watch]
                set_visible: model.single_line && model.headings,
                #[local_ref]
                headings_bar -> gtk::Box {
                    set_hexpand: true,
                },
            },

            gtk::Overlay {
                #[wrap(Some)]
                #[name = "scroller"]
                set_child = &gtk::ScrolledWindow {
                    set_vexpand: true,
                    // External, not Never: with Never the widest row's minimum (the
                    // actions palette reservation plus the avatar column) propagates
                    // all the way up and becomes part of the window's minimum width,
                    // which pushed it past half of a 1920px screen — at which point
                    // GNOME refuses to tile the window to the left/right edge. Rows
                    // ellipsize, so a narrow pane clips gracefully instead.
                    set_hscrollbar_policy: gtk::PolicyType::External,
                    // The pane's own floor, now that rows no longer set one: room
                    // for a row's full actions palette (avatar + dot + the reserved
                    // actions line), so opening the palette never needs to clip —
                    // the narrow-window breakpoint rails the sidebar in time to
                    // afford this even in a half-screen tile. (Grows by the thread
                    // indent while a conversation is expanded — see the rebuild.)
                    set_size_request: (LIST_MIN_WIDTH, -1),

                    // The view must be the scroller's own child to build only
                    // the rows on screen.
                    #[local_ref]
                    list_view -> gtk::ListView {},
                },

                // At the bottom while the rest of the folder streams in.
                add_overlay = &gtk::Box {
                    add_css_class: "list-loading",
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::End,
                    set_spacing: 8,
                    set_margin_bottom: 14,
                    set_can_target: false,
                    #[watch]
                    set_visible: model.is_loading_more(),

                    // Spun only while shown: a spinning spinner redraws every
                    // frame for as long as it is mapped (#275).
                    gtk::Spinner {
                        set_width_request: 18,
                        set_height_request: 18,
                        #[watch]
                        set_spinning: model.is_loading_more(),
                    },
                    gtk::Label {
                        set_label: &i18n("Loading more…"),
                        add_css_class: "dim-label",
                    },
                },

                // Placeholder when the folder has loaded and holds nothing.
                // Same full-size AdwStatusPage styling as the reader's
                // "No message selected", so the two placeholders match.
                add_overlay = &adw::StatusPage {
                    set_icon_name: Some("mail-inbox-symbolic"),
                    set_title: &i18n("No Messages"),
                    set_description: Some(i18n("There's nothing here right now.").as_str()),
                    #[watch]
                    set_visible: model.is_empty_state(),
                },
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let shared = RowShared::new(sender.input_sender().clone());
        let list_view = gtk::ListView::new(
            Some(shared.selection.clone()),
            Some(crate::ui::message_row::factory(&shared)),
        );
        shared.view.set(Some(&list_view));
        Self::wire_list(&list_view, &shared, sender.input_sender());

        let color_provider = crate::ui::DisplayCss::new();

        let mut model = MessageList {
            list_view: list_view.clone(),
            shared,
            input: sender.input_sender().clone(),
            rebuild_queued: None,
            switched: false,
            look_stale: Default::default(),
            pending_select: None,
            late_select: None,
            all: Vec::new(),
            search_pool: Vec::new(),
            scope: SearchScope::AllFolders,
            search_entry: None,
            search_open: false,
            search_closed_at: None,
            shown: Vec::new(),
            total_matches: 0,
            index_complete: true,
            loaded: false,
            at_bottom: false,
            window: LIST_WINDOW,
            tail: Vec::new(),
            threads_gen: std::cell::Cell::new(0),
            threads_job: std::cell::Cell::new(None),
            collapsing_threads: std::collections::HashMap::new(),
            query: String::new(),
            gravatar: false,
            avatars: true,
            sender_logos: false,
            preview_lines: 1,
            show_subject: true,
            colorize: false,
            account_colors: std::collections::HashMap::new(),
            color_provider,
            face_gen: 0,
            tags_gen: 0,
            thread_links: Vec::new(),
            source_threads: std::cell::RefCell::new(None),
            thread_summaries: std::collections::HashMap::new(),
            thread_row_newest: false,
            groups: std::collections::HashMap::new(),
            listed_folders: std::collections::HashSet::new(),
            asked_threads: std::collections::HashSet::new(),
            recheck_accounts: std::collections::HashSet::new(),
            selected_id: None,
            selected_ids: Vec::new(),
            emitted_thread: Vec::new(),
            selection_count: 0,
            nav_direction: 1,
            from_reader: 0,
            reader_keys: Vec::new(),
            expanded_threads: std::collections::HashSet::new(),
            show_recipient: false,
            restorable: false,
            in_junk: false,
            in_drafts: false,
            in_templates: false,
            default_expanded: false,
            msg_thread: std::collections::HashMap::new(),
            thread_members: std::collections::HashMap::new(),
            nested: std::collections::HashMap::new(),
            scroller: None,
            sort: SortOrder::DateNewest,
            unread_only: false,
            starred_only: false,
            last_count: String::new(),
            threading: true,
            thread_expansion: true,
            list_palette: true,
            single_line: false,
            columns: crate::config::ListColumn::DEFAULT.to_vec(),
            graph_accounts: std::collections::HashSet::new(),
            graph_in_view: false,
            sort_reversed: false,
            headings: false,
            headings_bar: gtk::Box::new(gtk::Orientation::Horizontal, 8),
            widths: std::collections::HashMap::new(),
            pane_width: 0,
            heading_bins: Default::default(),
            bulk_tag_btn: gtk::Button::new(),
        };

        let bulk_tag_btn = model.bulk_tag_btn.clone();
        let headings_bar = model.headings_bar.clone();
        let widgets = view_output!();
        model.scroller = Some(widgets.scroller.clone());
        {
            // The rows' width, which the single line's columns fit into: the
            // view's page across, set as it is allocated.
            let input = sender.input_sender().clone();
            widgets.scroller.hadjustment().connect_notify_local(Some("page-size"), move |adj, _| {
                let _ = input.send(MessageListInput::PaneWidth(adj.page_size() as i32));
            });
        }
        {
            // Whether the list sits at its bottom, for the spinner that says
            // more of the folder is on its way.
            let adj = widgets.scroller.vadjustment();
            let input = sender.input_sender().clone();
            let at_bottom = std::rc::Rc::new(std::cell::Cell::new(false));
            let near = std::rc::Rc::new(std::cell::Cell::new(false));
            let check = move |adj: &gtk::Adjustment| {
                let bottom = adj.upper() > adj.page_size() && adj.value() + adj.page_size() >= adj.upper() - 1.0;
                if at_bottom.replace(bottom) != bottom {
                    let _ = input.send(MessageListInput::AtBottom(bottom));
                }
                // Two screens from the end, the next rows are added, so
                // they are there before the scroll reaches them.
                let close = adj.upper() > adj.page_size()
                    && adj.value() + adj.page_size() >= adj.upper() - 2.0 * adj.page_size();
                if near.replace(close) != close && close {
                    let _ = input.send(MessageListInput::NearEnd);
                }
            };
            let c = check.clone();
            adj.connect_value_changed(move |a| c(a));
            adj.connect_changed(move |a| check(a));
        }
        model.sync_look();
        model.search_entry = Some(widgets.search_entry.clone());

        // The scope picker sizes itself to its widest entry ("All folders"), which
        // made it — not the messages — the narrowest the list could ever be (#29).
        // An ellipsizing label on the button lets it give way; the drop-down list
        // keeps its own factory, so the choices are still spelled out in full.
        let button_factory = gtk::SignalListItemFactory::new();
        button_factory.connect_setup(|_, item| {
            let label = gtk::Label::new(None);
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                item.set_child(Some(&label));
            }
        });
        button_factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let text = item
                .item()
                .and_downcast::<gtk::StringObject>()
                .map(|s| s.string().to_string())
                .unwrap_or_default();
            if let Some(label) = item.child().and_downcast::<gtk::Label>() {
                label.set_label(&text);
            }
        });
        widgets.scope_dropdown.set_factory(Some(&button_factory));

        schedule_midnight_refresh(&sender);

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            MessageListInput::SetMessages { messages } => {
                let t = std::time::Instant::now();
                let n = messages.len();
                self.all = messages.into_iter().map(Rc::new).collect();
                self.drop_threads();
                self.loaded = true;
                // Keep any active search query: this also fires for a background
                // re-sync of the folder you're viewing, which shouldn't drop your
                // search. Folder switches clear the query via `ResetPaging` first.
                self.queue_rebuild(true);
                tracing::debug!("list: set {n} messages in {:?}", t.elapsed());
            }
            MessageListInput::AppendMessages { messages } => {
                // Grow the searchable index in place. Dedup by (account, uid) since
                // UIDs collide across accounts in the unified inbox.
                let existing: std::collections::HashSet<(u32, u32)> =
                    self.all.iter().map(|m| (m.account_id, m.uid)).collect();
                let before = self.all.len();
                for m in messages {
                    if !existing.contains(&(m.account_id, m.uid)) {
                        self.all.push(Rc::new(m));
                    }
                }
                // The list holds the whole folder, so what arrives joins it.
                if self.all.len() != before {
                    self.drop_threads();
                    self.queue_rebuild(true);
                }
            }
            MessageListInput::SetLoading => {
                self.all.clear();
                self.drop_threads();
                self.loaded = false;
                self.clear_search();
                // Queued: when the folder's list follows in the same pass
                // (served from cache), the old rows are torn down once, for
                // the new ones, not first for nothing.
                self.queue_rebuild(false);
            }
            MessageListInput::ResetPaging => {
                // Folder switch: drop any active search, scrolled to the top.
                self.switched = true;
                self.window = LIST_WINDOW;
                self.late_select = None;
                self.clear_search();
                self.emitted_thread.clear();
                // However the new view's list arrives, a rebuild follows, and
                // the top is scrolled to then, not over the old rows first.
                self.queue_rebuild(false);
            }
            MessageListInput::SetIndexComplete(complete) => {
                self.index_complete = complete;
            }
            MessageListInput::AtBottom(bottom) => self.at_bottom = bottom,
            MessageListInput::NearEnd => self.grow_window(LIST_WINDOW),
            MessageListInput::ThreadsReady { gen, threads } => {
                if self.threads_job.get() == Some(gen) {
                    self.threads_job.set(None);
                }
                if gen == self.threads_gen.get() && threads.pool == self.searching_pool() {
                    *self.source_threads.borrow_mut() = Some(std::rc::Rc::new(*threads));
                    self.queue_rebuild(true);
                }
            }
            MessageListInput::AskThreads => {
                // Only the conversations rows have come to show, and only once
                // each: the answer arrives as a refresh of those rows, so
                // asking again on the strength of it would never settle.
                let wanted = self.shared.take_wanted();
                if !self.threading {
                    return;
                }
                let mut listed: Vec<(u32, String, Vec<String>)> = Vec::new();
                for key in wanted {
                    if listed.iter().any(|(a, r, _)| (*a, r) == (key.0, &key.1)) {
                        continue;
                    }
                    let Some(members) = self.groups.get(&key) else { continue };
                    let ids = crate::models::thread_ids(members.iter().map(|m| &**m));
                    if !ids.is_empty() {
                        listed.push((key.0, key.1, ids));
                    }
                }
                let fresh = unasked_threads(&listed, &self.asked_threads);
                if !fresh.is_empty() {
                    for (aid, root, _) in &fresh {
                        self.asked_threads.insert((*aid, root.clone()));
                    }
                    let _ = sender.output(MessageListOutput::ThreadsListed { groups: fresh });
                }
            }
            MessageListInput::RunQueuedRebuild => {
                self.switched = false;
                if let Some(preserve) = self.rebuild_queued.take() {
                    self.rebuild();
                    if !preserve {
                        self.scroll_top();
                    }
                }
                self.recheck_bound_rows();
                // The rows exist now: run the selection that waited for them.
                if let Some(key) = self.pending_select.take() {
                    let _ = self.input.send(MessageListInput::SelectAndLoad(key));
                } else if let Some((key, at)) = self.late_select {
                    if at.elapsed() > std::time::Duration::from_secs(30) {
                        self.late_select = None;
                    } else if self.shown.iter().any(|m| (m.account_id, m.id) == key)
                        || self.thread_head_for(key).is_some()
                    {
                        let _ = self.input.send(MessageListInput::SelectAndLoad(key));
                    }
                }
                self.report_thread_growth(&sender);
            }
            MessageListInput::SetThreadLinks(links) => {
                if self.thread_links != links {
                    self.thread_links = links;
                    self.drop_threads();
                    if self.threading {
                        self.queue_rebuild(true);
                    }
                }
            }
            MessageListInput::SetThreadSummaries(summaries) => {
                // They arrive a beat after the rows that asked are shown (the
                // cache is the worker's, not ours). Only what those rows say
                // moves; which conversations are listed does not — which is
                // what keeps this from asking again and looping.
                let mut changed = Vec::new();
                for (key, summary) in summaries {
                    if self.thread_summaries.get(&key) != Some(&summary) {
                        self.thread_summaries.insert(key.clone(), summary);
                        changed.push(key);
                    }
                }
                if !changed.is_empty() && self.threading {
                    // A rebuild already on its way reads them anyway.
                    if self.rebuild_queued.is_none() {
                        self.apply_summaries(changed);
                    }
                }
            }
            MessageListInput::RecheckThreadSummaries(account_id) => {
                // The summaries are kept, not dropped: dropping them turned
                // every conversation row on screen back into its folder-only
                // self, and a row kept across the rebuild is not bound again,
                // so it never asked again either (#330). The answers replace
                // whatever has changed.
                self.asked_threads.retain(|(aid, _)| *aid != account_id);
                self.recheck_accounts.insert(account_id);
                if self.rebuild_queued.is_none() {
                    self.recheck_bound_rows();
                }
            }
            MessageListInput::ConversationSize { account_id, id, size } => {
                if !self.threading || size < 2 {
                    return;
                }
                let key = (account_id, id);
                let row = self
                    .shown
                    .iter()
                    .position(|m| (m.account_id, m.id) == key)
                    .or_else(|| {
                        let head = self.thread_head_for(key)?;
                        self.shown.iter().position(|m| (m.account_id, m.id) == head)
                    })
                    .and_then(|pos| self.shared.model.row(pos));
                let Some(row) = row.filter(|r| r.meta.count < size) else { return };
                if let Some(group) = row.meta.group.clone() {
                    self.asked_threads.remove(&group);
                    self.shared.want(group);
                }
            }
            MessageListInput::SetThreadRowNewest(on) => {
                if self.thread_row_newest != on {
                    self.thread_row_newest = on;
                    if self.threading {
                        self.queue_rebuild(true);
                    }
                }
            }
            MessageListInput::SetThreading(on) => {
                if self.threading != on {
                    self.threading = on;
                    self.rebuild();
                    self.scroll_top();
                }
            }
            MessageListInput::SetThreadExpansion(on) => {
                if self.thread_expansion != on {
                    self.thread_expansion = on;
                    // Turning expansion off folds every open thread (the
                    // `expanded` computation ignores the stored toggles while
                    // off); turning it on restores them.
                    self.sync_look();
                    self.rebuild();
                }
            }
            MessageListInput::SetListPalette(on) => {
                if self.list_palette != on {
                    self.list_palette = on;
                    self.sync_look();
                }
            }
            MessageListInput::ResolveDelete => {
                // A lone selected row that heads a multi-message conversation
                // stands for the whole thread: hand every member to the app
                // (which confirms before deleting). Anything else — multiple
                // rows, a reply row, a plain message — is an ordinary bulk
                // delete of exactly what is selected.
                let selected = self.selected_messages();
                if let [m] = selected.as_slice() {
                    let key = (m.account_id, m.id);
                    if let Some(tkey) = self.msg_thread.get(&key) {
                        let members = self.thread_members.get(tkey).cloned().unwrap_or_default();
                        if members.len() > 1 && members.first() == Some(&key) {
                            let messages: Vec<Message> = members
                                .iter()
                                .filter_map(|mk| {
                                    self.active_source()
                                        .iter()
                                        .find(|x| (x.account_id, x.id) == *mk)
                                        .map(|m| Message::clone(m))
                                })
                                .collect();
                            let _ = sender
                                .output(MessageListOutput::DeleteThread { messages });
                            return;
                        }
                    }
                }
                sender.input(MessageListInput::Bulk(BulkAction::Delete));
            }
            MessageListInput::SetThreadsExpanded(on) => {
                if self.default_expanded != on {
                    self.default_expanded = on;
                    // Per-thread toggles were exceptions to the old default;
                    // drop them so everything follows the new one.
                    self.expanded_threads.clear();
                    self.rebuild();
                    self.scroll_top();
                }
            }
            // Every row's date, and a conversation's latest, is worked out
            // by the rebuild.
            MessageListInput::RefreshDates => self.rebuild(),
            MessageListInput::SetSenderLogos(on) => {
                if self.sender_logos != on {
                    self.sender_logos = on;
                    self.face_gen += 1;
                    self.sync_look();
                }
            }
            MessageListInput::SetSingleLine(on) => {
                if self.single_line != on {
                    self.single_line = on;
                    self.sync_look();
                }
            }
            MessageListInput::SetColumns(columns) => {
                if self.columns != columns {
                    // The Correspondents column is worked out by the rebuild.
                    let people = |c: &[crate::config::ListColumn]| c.contains(&crate::config::ListColumn::Correspondents);
                    let rebuild = people(&columns) != people(&self.columns);
                    self.columns = columns;
                    self.sync_look();
                    if rebuild {
                        self.queue_rebuild(true);
                    }
                }
            }
            MessageListInput::SetAccountNames { names, graph } => {
                let changed = *self.shared.account_names.borrow() != names || self.graph_accounts != graph;
                if changed {
                    *self.shared.account_names.borrow_mut() = names;
                    self.graph_accounts = graph;
                    self.graph_in_view = self.listed_folders.iter().any(|(a, _)| self.graph_accounts.contains(a));
                    self.sync_look();
                }
            }
            MessageListInput::SetLook { avatars, preview_lines, subject, animate } => {
                let preview_lines = preview_lines.min(3);
                let avatars_changed = self.avatars != avatars;
                let lines_changed = self.preview_lines != preview_lines;
                let subject_changed = self.show_subject != subject;
                if !avatars_changed && !lines_changed && !subject_changed {
                    return;
                }
                self.show_subject = subject;
                if animate && avatars_changed && !avatars {
                    // Slide every circle on screen away (and the preview to
                    // its new height in place); the rows give the slot back
                    // once they have gone.
                    self.preview_lines = preview_lines;
                    for row in self.shared.bound_rows() {
                        row.slide_avatar_away();
                        if lines_changed {
                            row.set_preview_lines(preview_lines);
                        }
                    }
                    let s = sender.clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(u64::from(crate::ui::FOCUS_ANIM_MS) + 40),
                        move || s.input(MessageListInput::LookSettled),
                    );
                } else {
                    self.avatars = avatars;
                    self.preview_lines = preview_lines;
                    self.sync_look();
                    // Circles coming back start folded and slide in.
                    if animate && avatars_changed && avatars {
                        for row in self.shared.bound_rows() {
                            row.slide_avatar_in();
                        }
                    }
                }
            }
            MessageListInput::LookSettled => {
                self.avatars = false;
                self.sync_look();
            }
            MessageListInput::SetAvatars(on) => {
                if self.avatars != on {
                    self.avatars = on;
                    self.sync_look();
                }
            }
            MessageListInput::SetGravatar(on) => {
                if self.gravatar != on {
                    self.gravatar = on;
                    self.face_gen += 1;
                    self.sync_look();
                }
            }
            MessageListInput::SetShowRecipient(on) => {
                if self.show_recipient != on {
                    self.show_recipient = on;
                    self.sync_look();
                }
            }
            MessageListInput::SetRestorable(on) => self.restorable = on,
            MessageListInput::SetInJunk(on) => {
                if self.in_junk != on {
                    self.in_junk = on;
                    self.sync_look();
                }
            }
            MessageListInput::SetInDrafts(on) => {
                if self.in_drafts != on {
                    self.in_drafts = on;
                    self.sync_look();
                }
            }
            MessageListInput::SetInTemplates(on) => self.in_templates = on,
            MessageListInput::ContactPhotosChanged => {
                // The circles on screen look again; the rest do when shown.
                self.face_gen += 1;
                self.sync_look();
            }
            MessageListInput::SetColorize(on) => {
                if self.colorize != on {
                    self.colorize = on;
                    self.sync_look();
                }
            }
            MessageListInput::DayChanged => {
                // Re-render so relative labels like "Today" reflect the new date.
                self.rebuild();
                schedule_midnight_refresh(&sender);
            }
            MessageListInput::SetAccountColors(colors) => {
                self.account_colors = colors;
                self.refresh_tint_css();
                self.sync_look();
            }
            MessageListInput::Search(q) => {
                let was_active = self.searching();
                self.query = q;
                let now_active = self.searching();
                // On the empty↔non-empty edge, tell the app to supply or drop the
                // cross-folder pool.
                if was_active != now_active {
                    let _ = sender.output(MessageListOutput::SearchActive(now_active));
                }
                self.rebuild();
                self.scroll_top();
            }
            MessageListInput::SetScope(scope) => {
                if self.scope != scope {
                    self.scope = scope;
                    // Scope only affects the view while a query is present.
                    if self.searching() {
                        self.rebuild();
                        self.scroll_top();
                    }
                }
            }
            MessageListInput::SetSearchPool(pool) => {
                self.search_pool = pool.into_iter().map(Rc::new).collect();
                self.drop_threads();
                if self.searching() && self.scope == SearchScope::AllFolders {
                    self.rebuild();
                }
            }
            MessageListInput::SelectFromReader { keys, conversation } => {
                // An empty set means the reader cleared its card selection (a
                // click on the document's empty space). The list keeps the
                // viewed message highlighted rather than losing its anchor —
                // the focus-within CSS dims the highlight instead.
                let keys = if keys.is_empty() {
                    self.selected_id.into_iter().collect()
                } else {
                    keys
                };
                // The list shows only a thread's head while it is collapsed, so
                // selecting a reply has to open the thread first — otherwise
                // there is no row to select and only the head would ever answer.
                let hidden: Vec<(u32, String)> = keys
                    .iter()
                    .filter(|k| !self.shown.iter().any(|m| (m.account_id, m.id) == **k))
                    .filter_map(|k| self.msg_thread.get(k).cloned())
                    .collect();
                if !hidden.is_empty() && self.thread_expansion {
                    for thread_key in hidden {
                        // `expanded_threads` records the departure from the
                        // default, so which way to move it depends on that.
                        if self.default_expanded {
                            self.expanded_threads.remove(&thread_key);
                        } else {
                            self.expanded_threads.insert(thread_key);
                        }
                    }
                    self.rebuild();
                }
                // The conversation as the reader shows it: what this list
                // handed over plus what the app merged in from other folders
                // since (#220). Either way it was opened from the viewed row.
                let conversation = reader_conversation(&self.emitted_thread, &conversation);
                let positions: Vec<usize> = keys
                    .iter()
                    .filter_map(|key| {
                        row_for_reader_key(key, &self.shown, &self.msg_thread, &conversation, self.selected_id)
                    })
                    .collect();
                // What the reader asked for, as this list can represent it, so
                // the change GTK is about to report is recognised as ours
                // rather than the user's.
                self.reader_keys = self.keys_at(&positions);
                self.from_reader = 8;
                self.select_positions(&positions);
                sender.input(MessageListInput::SelectionChanged);
            }
            MessageListInput::SelectionChanged => {
                let keys = self.keys_at(&self.selected_positions());
                self.selection_count = keys.len();
                // Set from the reader, which is already showing these messages:
                // mirror the selection but leave the reader alone. Reporting it
                // back would also drop whatever the reader has selected that this
                // list has no row for.
                if self.from_reader > 0 {
                    self.from_reader -= 1;
                    if keys == self.reader_keys {
                        self.selected_ids = keys;
                        return;
                    }
                    // Something else moved the selection — stop expecting ours.
                    self.from_reader = 0;
                }
                // A selection the user made here; the reader outlines it, and
                // it wins over one still waiting for its row.
                if !keys.is_empty() {
                    self.late_select = None;
                }
                let _ = sender.output(MessageListOutput::SelectionKeys(keys.clone()));
                match keys.as_slice() {
                    [] => self.selected_id = None,
                    [key] => {
                        // Exactly one selected → show it in the reader. Skip when
                        // it's already the viewed row (e.g. programmatic restore
                        // after a rebuild) so it isn't needlessly reloaded.
                        if self.selected_id != Some(*key) {
                            // Which way did the user move? Only a change with a
                            // known previous row says anything — the post-delete
                            // advance clears selected_id first, so it can never
                            // flip the direction it is itself steering by.
                            let pos =
                                |k: &(u32, u32)| self.shown.iter().position(|m| (m.account_id, m.id) == *k);
                            if let (Some(old), Some(new)) =
                                (self.selected_id.as_ref().and_then(&pos), pos(key))
                            {
                                if new != old {
                                    self.nav_direction = if new > old { 1 } else { -1 };
                                }
                            }
                            self.selected_id = Some(*key);
                            if let Some(m) = self
                                .shown
                                .iter()
                                .find(|m| (m.account_id, m.id) == *key)
                                .map(|m| Message::clone(m))
                            {
                                let (thread, solo) = self.conversation_for(&m);
                                self.emitted_thread =
                                    thread.iter().map(|t| (t.account_id, t.id)).collect();
                                let _ = sender.output(MessageListOutput::Selected {
                                    message: m,
                                    thread,
                                    solo,
                                });
                            }
                        }
                    }
                    // Multiple selected → keep the reader on the primary message.
                    _ => {}
                }
                self.selected_ids = keys;
            }
            MessageListInput::Bulk(action) => {
                // The bulk bar's spam button is one button: in Junk it means
                // the reverse.
                let action = if self.in_junk && action == BulkAction::Spam { BulkAction::NotSpam } else { action };
                let messages = self.selected_messages();
                // Likewise the star: it clears a selection that is starred
                // throughout, or it could never be taken off again (#313).
                let action = if action == BulkAction::Flag
                    && !messages.is_empty()
                    && messages.iter().all(|m| m.starred)
                {
                    BulkAction::Unflag
                } else {
                    action
                };
                if !messages.is_empty() {
                    let _ = sender.output(MessageListOutput::Bulk { action, messages });
                }
                // The selection stays, and with it the bulk bar: read and star
                // change rows in place, so a second action (or the same one
                // again, to undo it) can follow without selecting anew (#313).
                // Row-removing actions need it too: the RemoveMany that
                // follows reads it to know the viewed message is going away
                // and to advance the selection (and reader) in its place.
            }
            MessageListInput::MoveSelection(delta) => {
                if self.shown.is_empty() {
                    return;
                }
                if delta != 0 {
                    self.nav_direction = delta.signum();
                }
                // From the current row, or from the top/bottom when nothing is
                // selected yet, so the first keypress always lands somewhere.
                let current = self
                    .selected_positions()
                    .first()
                    .map(|&p| p as i64)
                    .unwrap_or(if delta > 0 { -1 } else { self.shown.len() as i64 });
                let next = (current + delta as i64).clamp(0, self.shown.len() as i64 - 1) as usize;
                self.shared.selection.select_item(next as u32, true);
                self.focus_row(next);
            }

            MessageListInput::ToggleSelection => {
                // The row the keyboard is on, or the first selected one.
                let focused = self
                    .shared
                    .bound_rows()
                    .into_iter()
                    .find(|r| r.has_focus_within())
                    .and_then(|r| r.position());
                let Some(pos) = focused.or_else(|| self.selected_positions().first().copied()) else {
                    return;
                };
                let selection = &self.shared.selection;
                if selection.is_selected(pos as u32) {
                    selection.unselect_item(pos as u32);
                } else {
                    selection.select_item(pos as u32, false);
                }
            }

            MessageListInput::SetUnreadOnly(on) => {
                if self.unread_only != on {
                    self.unread_only = on;
                    self.rebuild();
                }
            }

            MessageListInput::SetStarredOnly(on) => {
                if self.starred_only != on {
                    self.starred_only = on;
                    self.rebuild();
                }
            }

            MessageListInput::FocusList => {
                // The "back to list" shortcut: deliberately returns to the
                // selected row (that's the point — resume j/k navigation
                // from what you were reading), so it's allowed to scroll.
                if !self.shown.is_empty() {
                    let pos = self.selected_positions().first().copied().unwrap_or(0);
                    self.focus_row(pos);
                }
            }
            MessageListInput::ReclaimFocus => {
                // Housekeeping focus restore (e.g. after a compose window
                // closes) so keyboard shortcuts keep targeting the list —
                // unlike `FocusList`, this isn't a "go back to what I was
                // reading" action, so it shouldn't scroll the viewport away
                // from wherever the user was browsing.
                self.preserving_scroll(|this| {
                    this.list_view.grab_focus();
                });
                self.hide_focus_ring();
            }

            MessageListInput::FocusSearch => {
                // Toggle (#102): the header button closes an open search too.
                if self.search_open {
                    sender.input(MessageListInput::CloseSearch);
                    return;
                }
                // Clicking the button blurs an empty entry, whose focus-leave
                // just closed the bar — that same click must not reopen it.
                if self
                    .search_closed_at
                    .take()
                    .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(300))
                {
                    return;
                }
                self.search_open = true;
                if let Some(entry) = &self.search_entry {
                    let entry = entry.clone();
                    // After the revealer maps it; focusing an unmapped entry
                    // is a no-op.
                    gtk::glib::idle_add_local_once(move || {
                        entry.grab_focus();
                    });
                }
            }

            MessageListInput::CloseSearch => {
                self.search_open = false;
                self.search_closed_at = Some(std::time::Instant::now());
                self.clear_search();
                self.rebuild();
            }

            MessageListInput::BulkToggleRead => {
                let action = if self.selection_any_unread() {
                    BulkAction::MarkRead
                } else {
                    BulkAction::MarkUnread
                };
                sender.input(MessageListInput::Bulk(action));
            }
            MessageListInput::BulkTagMenu => {
                let btn = self.bulk_tag_btn.clone();
                let entries = self.bulk_tag_entries(&sender);
                if !entries.is_empty() {
                    // The menu takes the keyboard while it is open and does
                    // not hand it back, which greys the selection and stops
                    // the single-key shortcuts: give it back to the list.
                    let before = btn.root().and_then(|r| r.focus());
                    let popover = crate::ui::context_menu::show_context_menu_popover(
                        &btn,
                        (btn.width() / 2) as f64,
                        btn.height() as f64,
                        None,
                        vec![entries],
                    );
                    popover.connect_closed(move |_| {
                        if let Some(w) = &before {
                            w.grab_focus();
                        }
                    });
                }
            }
            MessageListInput::ClearSelection => {
                self.shared.selection.unselect_all();
                self.selected_id = None;
                self.selected_ids.clear();
                self.selection_count = 0;
            }
            MessageListInput::SetSort(order) => {
                if self.sort != order || self.sort_reversed {
                    self.sort = order;
                    self.sort_reversed = false;
                    self.rebuild();
                    self.scroll_top();
                    self.sync_headings();
                }
            }
            MessageListInput::ResizeColumn { column, width, done } => {
                match width {
                    Some(px) => self.widths.insert(column, px),
                    None => self.widths.remove(&column),
                };
                // The rows follow every step; the headings are left alone
                // while their handle is held, as rebuilding them would drop
                // it mid-drag. It has moved its own heading already.
                let fitted = self.fitted_widths();
                for (c, bin) in self.heading_bins.borrow().iter() {
                    bin.set_width(fitted.get(c).copied().unwrap_or_else(|| crate::ui::message_row::default_width(*c)));
                }
                self.shared.look.borrow_mut().widths = fitted;
                self.shared.refresh_all();
                if done {
                    let input = self.shared.input.clone();
                    glib::idle_add_local_once(move || {
                        let _ = input.send(MessageListInput::RefreshHeadings);
                    });
                    let _ = sender.output(MessageListOutput::ColumnWidths(self.widths.clone()));
                }
            }
            MessageListInput::SetColumnWidths(widths) => {
                if self.widths != widths {
                    self.widths = widths;
                    self.sync_look();
                }
            }
            MessageListInput::RefreshHeadings => self.sync_headings(),
            MessageListInput::PaneWidth(width) => {
                if self.pane_width != width {
                    let before = self.single_line.then(|| self.fitted_widths());
                    self.pane_width = width;
                    if before.is_some_and(|b| b != self.fitted_widths()) {
                        self.sync_look();
                    }
                }
            }
            MessageListInput::SetHeadings(on) => {
                self.headings = on;
                self.sync_headings();
            }
            MessageListInput::SortByColumn(column) => {
                let Some(order) = column_sort(column, self.show_recipient) else { return };
                match (order, self.sort) {
                    // Date has an order each way of its own.
                    (SortOrder::DateNewest, SortOrder::DateNewest) => self.sort = SortOrder::DateOldest,
                    (SortOrder::DateNewest, SortOrder::DateOldest) => self.sort = SortOrder::DateNewest,
                    (order, current) if order == current => self.sort_reversed = !self.sort_reversed,
                    (order, _) => {
                        self.sort = order;
                        self.sort_reversed = false;
                    }
                }
                self.rebuild();
                self.scroll_top();
                self.sync_headings();
                let _ = sender.output(MessageListOutput::SortChanged(self.sort.key()));
            }
            MessageListInput::ToggleThread(key) => {
                // Expansion disabled: the chevron stays but does nothing — the
                // conversation is read through the reader's cards.
                if !self.thread_expansion {
                    return;
                }
                let was_expanded = self.expanded_threads.contains(&key) != self.default_expanded;
                if was_expanded {
                    // Slide the replies shut in place; the list only drops
                    // them once that animation has actually finished (see
                    // `start_collapse_thread`) — dropping them right away
                    // would just make them vanish instead of collapsing.
                    self.start_collapse_thread(key, &sender);
                } else {
                    if !self.expanded_threads.remove(&key) {
                        self.expanded_threads.insert(key.clone());
                    }
                    // Insert just this thread's replies rather than rebuilding
                    // the whole list — on a long list a full rebuild tears
                    // down and recreates every row (up to RENDER_CAP), which
                    // stutters right as the reveal animation is trying to run.
                    self.expand_thread(&key);
                }
            }
            MessageListInput::FinishCollapseThread(key) => {
                self.collapsing_threads.remove(&key);
                if !self.expanded_threads.remove(&key) {
                    self.expanded_threads.insert(key.clone());
                }
                // Same reasoning as `expand_thread`: drop just these rows.
                self.collapse_thread_rows(&key);
            }
            MessageListInput::RowActivated(index) => {
                if let Some(m) = self.shown.get(index as usize).map(|m| Message::clone(m)) {
                    let (thread, _solo) = self.conversation_for(&m);
                    let _ = sender.output(MessageListOutput::Activated { message: m, thread });
                }
            }
            MessageListInput::MarkRead(slot) => {
                self.update_message(|m| thread_slot(m) == slot, |m| m.unread = false);
                self.refresh_thread_head(slot);
            }
            MessageListInput::SetRead { slot, read } => {
                self.update_message(|m| thread_slot(m) == slot, |m| m.unread = !read);
                self.refresh_thread_head(slot);
            }
            MessageListInput::SetStarred { slot, starred } => {
                self.update_message(|m| thread_slot(m) == slot, |m| m.starred = starred);
                self.refresh_thread_head(slot);
            }
            MessageListInput::SetKeywords { slot, keywords } => {
                self.update_message(|m| thread_slot(m) == slot, |m| m.keywords = keywords.clone());
            }
            MessageListInput::SetTags(tags) => {
                if *self.shared.tags.borrow() != tags {
                    *self.shared.tags.borrow_mut() = tags;
                    // Chips and the palette's tag button follow the
                    // definitions.
                    self.tags_gen += 1;
                    self.sync_look();
                }
            }
            MessageListInput::SetTagFor { message, keyword, add } => {
                let _ = sender.output(MessageListOutput::SetTag { message, keyword, add });
            }
            MessageListInput::SetHasAttachment { account_id, folder_id, uid, has } => {
                self.update_message(
                    |m| m.account_id == account_id && m.folder_id == folder_id && m.uid == uid,
                    |m| m.has_attachment = has,
                );
            }
            MessageListInput::Remove(id) => {
                // Was the removed message the one shown in the reader? If so we'll
                // advance to whatever row slides into its place.
                let was_viewed = self.selected_id.map(|(_, i)| i) == Some(id);
                if was_viewed {
                    self.selected_id = None;
                }
                self.selected_ids.retain(|(_, i)| *i != id);
                self.all.retain(|m| m.id != id);
                self.drop_threads();
                self.tail.retain(|r| r.msg.id != id);
                let removed_idx = self.shown.iter().position(|m| m.id == id);
                // Was the row about to go the one holding keyboard focus? If
                // so, and it isn't the viewed row handled below, focus is
                // put on its neighbour rather than left to GTK's fallback.
                let had_focus = removed_idx
                    .and_then(|idx| self.shared.row_at(idx))
                    .is_some_and(|row| row.has_focus_within());
                if let Some(idx) = removed_idx {
                    self.shown.remove(idx);
                    self.shared.model.splice(idx, 1, Vec::new());
                    // No rebuild follows — keep the header count honest.
                    self.total_matches = self.total_matches.saturating_sub(1);
                }

                if was_viewed {
                    match removed_idx {
                        // Advance in the direction the user was triaging: the
                        // row now at the removed slot when moving down the
                        // list, the one above it when moving up (Apple Mail's
                        // behaviour). Selecting it fires SelectionChanged,
                        // which loads it in the reader.
                        Some(idx) if !self.shown.is_empty() => {
                            let next = self.advance_index(idx);
                            self.select_and_focus(next);
                        }
                        // Nothing left to show → clear the reader.
                        _ => {
                            let _ = sender.output(MessageListOutput::SelectionCleared);
                        }
                    }
                } else if had_focus {
                    if let Some(idx) = removed_idx {
                        if !self.shown.is_empty() {
                            self.focus_only(self.advance_index(idx));
                        }
                    }
                }
            }
            MessageListInput::RemoveMany(ids) => {
                if ids.is_empty() {
                    return;
                }
                let set: std::collections::HashSet<u32> = ids.into_iter().collect();
                let was_viewed = self.selected_id.map(|(_, i)| i).is_some_and(|i| set.contains(&i));
                if was_viewed {
                    self.selected_id = None;
                }
                self.selected_ids.retain(|(_, i)| !set.contains(i));
                self.all.retain(|m| !set.contains(&m.id));
                self.drop_threads();
                self.tail.retain(|r| !set.contains(&r.msg.id));
                // Where the first removed row sat, so we can re-select in its place.
                let first_removed = self.shown.iter().position(|m| set.contains(&m.id));
                let had_focus = self
                    .shared
                    .bound_rows()
                    .into_iter()
                    .any(|r| r.has_focus_within() && r.data().is_some_and(|d| set.contains(&d.msg.id)));
                // Out in runs, back to front, so the view hears of each run
                // once and the positions still to come stay valid.
                let shown_before = self.shown.len();
                let mut idx = self.shown.len();
                while idx > 0 {
                    idx -= 1;
                    if !set.contains(&self.shown[idx].id) {
                        continue;
                    }
                    let end = idx + 1;
                    while idx > 0 && set.contains(&self.shown[idx - 1].id) {
                        idx -= 1;
                    }
                    self.shown.drain(idx..end);
                    self.shared.model.splice(idx, end - idx, Vec::new());
                }
                self.selection_count = self.selected_ids.len();
                // Keep the header count honest; no rebuild follows.
                self.total_matches = self.total_matches.saturating_sub(shown_before - self.shown.len());
                if was_viewed {
                    match first_removed {
                        Some(idx) if !self.shown.is_empty() => {
                            let next = self.advance_index(idx);
                            self.select_and_focus(next);
                        }
                        _ => {
                            let _ = sender.output(MessageListOutput::SelectionCleared);
                        }
                    }
                } else if had_focus {
                    if let Some(idx) = first_removed {
                        if !self.shown.is_empty() {
                            self.focus_only(self.advance_index(idx));
                        }
                    }
                }
            }
            MessageListInput::RowAction { action, message } => {
                // Starring a conversation row stars the conversation (#star):
                // every member together, toggled as a unit — individual
                // members keep their own stars via their own rows/cards.
                if matches!(action, RowAction::ToggleStar) {
                    let members = self.thread_members(&message);
                    let is_head = members.first().is_some_and(|h| {
                        (h.account_id, h.id) == (message.account_id, message.id)
                    });
                    if is_head && members.len() > 1 {
                        // ANY starred member reads as a starred conversation
                        // (matching the head's indicator), so the toggle can
                        // always clear — all-starred semantics deadlocked the
                        // moment one member was individually unstarred.
                        let any = members.iter().any(|m| m.starred);
                        let _ = sender.output(MessageListOutput::Bulk {
                            action: if any { BulkAction::Unflag } else { BulkAction::Flag },
                            messages: members,
                        });
                        return;
                    }
                }
                let conversation = self.row_conversation(&message);
                let _ = sender.output(MessageListOutput::Action { action, message, conversation });
            }
            MessageListInput::SetPaletteCollapse(secs) => self.shared.palette_collapse_secs.set(secs),
            MessageListInput::SetPaletteHover(on) => self.shared.palette_hover.set(on),
            MessageListInput::SetSwipeReversed(on) => self.shared.swipe_reversed.set(on),
            MessageListInput::SetSwipeEnabled(on) => {
                self.shared.swipe_enabled.set(on);
                // The rows on screen switch their trackers; the rest do so
                // when shown.
                self.shared.refresh_all();
            }
            MessageListInput::SetSwipeSensitivity(factor) => {
                self.shared.swipe_sensitivity.set(factor);
                self.shared.refresh_all();
            }
            MessageListInput::SetSelected(id) => {
                match id {
                    // Account-less id resolved against the shown list (the app
                    // only sends `None` today; `Some` kept for completeness).
                    Some(i) => {
                        if let Some(m) = self.shown.iter().find(|m| m.id == i) {
                            let key = (m.account_id, m.id);
                            self.selected_id = Some(key);
                            self.selected_ids = vec![key];
                            self.select_current();
                        }
                    }
                    None => {
                        self.selected_id = None;
                        self.selected_ids.clear();
                        self.selection_count = 0;
                        self.shared.selection.unselect_all();
                    }
                }
            }
            MessageListInput::DebugOpenPalette(idx) => {
                if let Some(row) = self.shared.row_at(idx) {
                    row.toggle_palette();
                }
            }
            MessageListInput::DebugSwipe { index, left } => {
                if let Some(row) = self.shared.row_at(index) {
                    row.debug_swipe(left);
                }
            }
            MessageListInput::DebugRowMenu => {
                if let Some(m) = self.shown.first().map(|m| Message::clone(m)) {
                    self.show_context_menu(&m, 120.0, 40.0, &sender);
                }
            }
            MessageListInput::SelectAndLoad(key) => {
                // Rows not built yet (the list landed in this same pass):
                // wait for the queued rebuild rather than find nothing.
                if self.rebuild_queued.is_some() {
                    self.pending_select = Some(key);
                    return;
                }
                self.late_select = None;
                let asked = key;
                // A row further down than the window reaches comes on screen.
                if let Some(at) = self.tail.iter().position(|r| (r.msg.account_id, r.msg.id) == key) {
                    self.grow_window(at + 1);
                }
                // A reply inside a conversation has no row of its own: its
                // thread head does, and opening that shows the whole thread,
                // the reply included.
                let key = match self.shown.iter().any(|m| (m.account_id, m.id) == key) {
                    true => key,
                    false => match self.thread_head_for(key) {
                        Some(head) => head,
                        None => key,
                    },
                };
                if let Some(m) = self.shown.iter().find(|m| (m.account_id, m.id) == key).map(|m| Message::clone(m)) {
                    self.selected_id = Some(key);
                    self.selected_ids = vec![key];
                    // Exactly this row: anything already selected (e.g. the
                    // row the last deletion advanced to) would otherwise stay
                    // lit and turn this into a two-row selection the reader
                    // ignores.
                    self.select_current();
                    // These arrive from outside the list (a notification
                    // click, an undo) where the row can be far outside the
                    // viewport — bring it into view, in an idle so a rebuild
                    // queued just before this has run first.
                    if let Some(idx) =
                        self.shown.iter().position(|m| (m.account_id, m.id) == key)
                    {
                        let list = self.list_view.clone();
                        gtk::glib::idle_add_local_once(move || {
                            if idx < list.model().map_or(0, |m| m.n_items() as usize) {
                                list.scroll_to(idx as u32, gtk::ListScrollFlags::FOCUS, None);
                                list.grab_focus();
                            }
                            if let Some(win) =
                                list.root().and_then(|r| r.downcast::<gtk::Window>().ok())
                            {
                                win.set_focus_visible(false);
                            }
                        });
                    }
                    let (thread, solo) = self.conversation_for(&m);
                    self.emitted_thread = thread.iter().map(|t| (t.account_id, t.id)).collect();
                    let _ = sender.output(MessageListOutput::Selected { message: m, thread, solo });
                } else {
                    // Not listed yet: mail that has only just arrived, whose
                    // notification was clicked before its folder's list was in.
                    // Dropping the request left the reader empty (#332).
                    self.late_select = Some((asked, std::time::Instant::now()));
                }
            }
            MessageListInput::RowMoveTo { message, x, y } => {
                let (messages, offer_whole) = self.move_to_messages(&message);
                let _ = sender.output(MessageListOutput::MoveTo { messages, offer_whole, x, y });
            }
            MessageListInput::ContextMenu { x, y, key } => {
                let Some(pos) = self.shown.iter().position(|m| (m.account_id, m.id) == key) else {
                    return;
                };
                let selected = self.selected_positions();
                if selected.len() > 1 && selected.contains(&pos) {
                    // Right-clicked inside a multi-selection → bulk menu.
                    self.show_bulk_menu(x, y, &sender);
                } else {
                    // Single-row menu acting on the clicked message. Crucially,
                    // don't select it — selecting would load it in the reader,
                    // and the user may just intend to move/archive/delete it.
                    let msg = Message::clone(&self.shown[pos]);
                    self.show_context_menu(&msg, x, y, &sender);
                }
            }
        }
        // Whatever just happened, restate the header count if it moved — every
        // path that changes the visible rows funnels through here.
        let count = self.count_label();
        if count != self.last_count {
            self.last_count = count.clone();
            let _ = sender.output(MessageListOutput::CountChanged(count));
        }
    }
}

impl MessageList {
    /// What the list holds in RAM, for the memory section of an export: the
    /// folder's index, the rows listed from it, and the whole-mailbox search
    /// pool (empty unless a search is open), each as (messages, bytes). The
    /// rows share their messages with the index.
    pub fn memory_stats(&self) -> [(usize, usize); 3] {
        use crate::memory_report::messages_bytes;
        [
            messages_bytes(self.all.iter().map(|m| &**m)),
            (self.shown.len(), self.shown.len() * std::mem::size_of::<RowData>()),
            messages_bytes(self.search_pool.iter().map(|m| &**m)),
        ]
    }

    /// Build and pop up the right-click menu for `msg` at the click location.
    fn show_context_menu(
        &self,
        msg: &Message,
        x: f64,
        y: f64,
        sender: &ComponentSender<Self>,
    ) {
        // Each entry carries the same icon as the reader-toolbar button (or
        // row-palette button) for that action, tying the two together.
        let conversation = self.row_conversation(msg);
        let item = |action: RowAction, label: &str, icon: &str| -> MenuEntry {
            let s = sender.clone();
            let m = msg.clone();
            let conversation = conversation.clone();
            MenuEntry::new(label, move || {
                let _ = s.output(MessageListOutput::Action {
                    action,
                    message: Box::new(m.clone()),
                    conversation: conversation.clone(),
                });
            })
            .icon(icon)
        };

        // (computed early: the star entry needs it too)
        let members_for_star = self.thread_members(msg);
        let star_is_head = members_for_star
            .first()
            .is_some_and(|h| (h.account_id, h.id) == (msg.account_id, msg.id));
        let mut flag_section = vec![if star_is_head && members_for_star.len() > 1 {
            // A conversation row's star acts on the whole thread, like its
            // read toggle below.
            let any = members_for_star.iter().any(|m| m.starred);
            let s = sender.clone();
            let members = members_for_star.clone();
            MenuEntry::new(
                if any { i18n("Remove Stars") } else { i18n("Star Conversation") },
                move || {
                    let _ = s.output(MessageListOutput::Bulk {
                        action: if any { BulkAction::Unflag } else { BulkAction::Flag },
                        messages: members.clone(),
                    });
                },
            )
            .icon(if any {
                "hylki-non-starred-symbolic"
            } else {
                "starred-symbolic"
            })
        } else if msg.starred {
            item(RowAction::ToggleStar, &i18n("Remove Star"), "hylki-non-starred-symbolic")
        } else {
            item(RowAction::ToggleStar, &i18n("Star"), "starred-symbolic")
        }];

        // A conversation row acts on the whole thread: its read entry marks
        // every member, through the same bulk path as a multi-select, and the
        // singular toggle is dropped (the row isn't a singular message).
        // Expanded replies keep the singular toggle. Labels follow state, like
        // the singular one: any unread member reads as an unread conversation.
        let members = self.thread_members(msg);
        let is_thread_head =
            members.first().is_some_and(|h| (h.account_id, h.id) == (msg.account_id, msg.id));
        // Move To…: the clicked message, then (for a conversation row) the
        // rest of its members, at the click's point in the window so the
        // app can anchor the picker there.
        let move_entry = {
            let (messages, offer_whole) = self.move_to_messages(msg);
            let (wx, wy) = self.window_point(x, y);
            let s = sender.clone();
            MenuEntry::new(&i18n("Move To…"), move || {
                let _ = s.output(MessageListOutput::MoveTo {
                    messages: messages.clone(),
                    offer_whole,
                    x: wx,
                    y: wy,
                });
            })
            .icon("folder-symbolic")
        };
        if self.in_drafts {
            // A draft is neither read nor unread: no toggle to offer.
        } else if is_thread_head {
            let any_unread = members.iter().any(|m| m.unread);
            let s = sender.clone();
            flag_section.push(
                MenuEntry::new(
                    if any_unread { i18n("Mark All as Read") } else { i18n("Mark All as Unread") },
                    move || {
                        let _ = s.output(MessageListOutput::Bulk {
                            action: if any_unread {
                                BulkAction::MarkRead
                            } else {
                                BulkAction::MarkUnread
                            },
                            messages: members.clone(),
                        });
                    },
                )
                .icon(if any_unread {
                    "hylki-mail-read-symbolic"
                } else {
                    "mail-unread-symbolic"
                }),
            );
        } else if msg.unread {
            flag_section
                .push(item(RowAction::ToggleRead, &i18n("Mark as Read"), "hylki-mail-read-symbolic"));
        } else {
            flag_section.push(item(
                RowAction::ToggleRead,
                &i18n("Mark as Unread"),
                "mail-unread-symbolic",
            ));
        }

        // Tags (#71): one toggle per tag, a filled swatch where the message
        // carries it, behind a "Tags" submenu so a long list never makes
        // this menu too tall. Absent until a tag exists.
        let tag_section = {
            let tags = self.shared.tags.borrow().clone();
            let s = sender.clone();
            let m = msg.clone();
            let entries = tag_menu_entries(&tags, msg, move |keyword, add| {
                let _ = s.output(MessageListOutput::SetTag {
                    message: Box::new(m.clone()),
                    keyword,
                    add,
                });
            });
            if entries.is_empty() {
                Vec::new()
            } else {
                vec![MenuEntry::submenu(i18n("Tags"), vec![entries]).icon("tag-outline-symbolic")]
            }
        };

        let sections = vec![
            if self.in_templates {
                vec![
                    item(RowAction::EditAsNew, &i18n("Use Template"), "mail-message-new-symbolic"),
                    item(RowAction::EditTemplate, &i18n("Edit Template"), "document-edit-symbolic"),
                ]
            } else {
                vec![
                    item(RowAction::Reply, &i18n("Reply"), "mail-reply-sender-symbolic"),
                    item(RowAction::ReplyAll, &i18n("Reply All"), "mail-reply-all-symbolic"),
                    item(RowAction::Forward, &i18n("Forward"), "mail-forward-symbolic"),
                    item(
                        RowAction::EditAsNew,
                        &i18n("Edit as New Message"),
                        "document-edit-symbolic",
                    ),
                ]
            },
            flag_section,
            tag_section,
            {
                let mut section = Vec::new();
                // In Junk the way back is "Not Spam" (#168): the server is
                // told, and the message returns to the Inbox. In Trash it
                // is a plain move, with spam still on offer.
                if self.in_junk {
                    section.push(item(RowAction::NotSpam, &i18n("Not Spam"), "mail-mark-notjunk-symbolic"));
                } else {
                    if self.restorable {
                        section.push(item(RowAction::MoveToInbox, &i18n("Move to Inbox"), "mail-inbox-symbolic"));
                    }
                    section.push(item(RowAction::Spam, &i18n("Mark as Spam"), "mail-mark-junk-symbolic"));
                }
                section.push(move_entry);
                section.push(item(RowAction::Archive, &i18n("Archive"), "mail-archive-symbolic"));
                section.push(item(RowAction::Delete, &i18n("Delete"), "user-trash-symbolic"));
                section
            },
            vec![item(
                RowAction::AddContact,
                &i18n("Add Sender to Contacts"),
                "contact-new-symbolic",
            )],
            vec![item(RowAction::ViewSource, &i18n("View Source"), "code-symbolic")],
        ];

        self.popup_menu(x, y, None, sections);
    }

    /// Open a menu on the list, keeping the list where it was scrolled. The
    /// popover hands focus back to the list view as it closes, and the list
    /// view then scrolls to the row its focus starts from, the first one:
    /// every right-click menu, tagging included, sent the list to the top.
    fn popup_menu(&self, x: f64, y: f64, header: Option<&str>, sections: Vec<Vec<MenuEntry>>) {
        let before = self.list_view.root().and_then(|r| gtk::prelude::RootExt::focus(&r));
        let popover = show_context_menu_popover(&self.list_view, x, y, header, sections);
        let Some(scroller) = self.scroller.clone() else { return };
        let adj = scroller.vadjustment();
        let pos = adj.value();
        popover.connect_closed(move |_| {
            // Focus goes back where it was, and the list to where it was
            // scrolled, once GTK has done its own focus hand-back.
            let (adj, before) = (adj.clone(), before.clone());
            gtk::glib::idle_add_local_once(move || {
                if let Some(w) = before.filter(|w| w.root().is_some() && w.is_mapped()) {
                    w.grab_focus();
                }
                adj.set_value(pos);
            });
        });
    }

    /// The messages of the selected rows.
    fn selected_messages(&self) -> Vec<Message> {
        self.selected_positions()
            .into_iter()
            .filter_map(|p| self.shown.get(p).map(|m| Message::clone(m)))
            .collect()
    }

    /// Whether every selected message is starred, so the bulk star takes
    /// the stars off rather than adding them.
    fn selection_all_starred(&self) -> bool {
        let positions = self.selected_positions();
        !positions.is_empty() && positions.iter().all(|&p| self.shown.get(p).is_some_and(|m| m.starred))
    }

    /// Whether any selected message is unread, so the bulk read toggle
    /// marks the selection read rather than unread.
    fn selection_any_unread(&self) -> bool {
        self.selected_positions().iter().any(|&p| self.shown.get(p).is_some_and(|m| m.unread))
    }

    /// One toggle per tag for the whole selection (#313). A tag is ticked
    /// when every selected message carries it: choosing it then takes it
    /// off them all, and otherwise puts it on them all.
    fn bulk_tag_entries(&self, sender: &ComponentSender<Self>) -> Vec<MenuEntry> {
        let messages = self.selected_messages();
        let Some(first) = messages.first() else { return Vec::new() };
        let mut all = first.clone();
        all.keywords.retain(|k| messages.iter().all(|m| m.has_keyword(k)));
        let tags = self.shared.tags.borrow().clone();
        let s = sender.clone();
        tag_menu_entries(&tags, &all, move |keyword, add| {
            let _ = s.output(MessageListOutput::SetTagMany { messages: messages.clone(), keyword, add });
        })
    }

    /// Build and pop up the bulk-action menu for the current multi-selection.
    fn show_bulk_menu(&self, x: f64, y: f64, sender: &ComponentSender<Self>) {
        let item = |action: BulkAction, label: &str, icon: &str| -> MenuEntry {
            let s = sender.clone();
            MenuEntry::new(label, move || s.input(MessageListInput::Bulk(action))).icon(icon)
        };

        let sections = vec![
            {
                let mut section = Vec::new();
                // Drafts are neither read nor unread.
                // One entry, the way the bar's button toggles.
                if !self.in_drafts {
                    section.push(if self.selection_any_unread() {
                        item(BulkAction::MarkRead, &i18n("Mark as Read"), "hylki-mail-read-symbolic")
                    } else {
                        item(BulkAction::MarkUnread, &i18n("Mark as Unread"), "mail-unread-symbolic")
                    });
                }
                // The Bulk handler turns this into Unflag for a starred
                // selection.
                section.push(if self.selection_all_starred() {
                    item(BulkAction::Flag, &i18n("Remove Star"), "hylki-non-starred-symbolic")
                } else {
                    item(BulkAction::Flag, &i18n("Star"), "starred-symbolic")
                });
                let tags = self.bulk_tag_entries(sender);
                if !tags.is_empty() {
                    section.push(MenuEntry::submenu(i18n("Tags"), vec![tags]).icon("tag-outline-symbolic"));
                }
                section
            },
            {
                let mut section = Vec::new();
                if self.in_junk {
                    section.push(item(BulkAction::NotSpam, &i18n("Not Spam"), "mail-mark-notjunk-symbolic"));
                } else {
                    if self.restorable {
                        section.push(item(BulkAction::MoveToInbox, &i18n("Move to Inbox"), "mail-inbox-symbolic"));
                    }
                    section.push(item(BulkAction::Spam, &i18n("Mark as Spam"), "mail-mark-junk-symbolic"));
                }
                {
                    // Move To… for the whole selection.
                    let messages = self.selected_messages();
                    let (wx, wy) = self.window_point(x, y);
                    let s = sender.clone();
                    section.push(
                        MenuEntry::new(&i18n("Move To…"), move || {
                            let _ = s.output(MessageListOutput::MoveTo {
                                messages: messages.clone(),
                                offer_whole: false,
                                x: wx,
                                y: wy,
                            });
                        })
                        .icon("folder-symbolic"),
                    );
                }
                section.push(item(BulkAction::Archive, &i18n("Archive"), "mail-archive-symbolic"));
                section.push(item(BulkAction::Delete, &i18n("Delete"), "user-trash-symbolic"));
                section
            },
        ];

        self.popup_menu(x, y, Some(&format!("{} selected", self.selection_count)), sections);
    }

    /// After a rebuild: a lone selected conversation head whose thread has
    /// gained members since it was opened (a reply just synced in) is
    /// reported, so the reader can show the new message without a
    /// re-selection. Members lost (deleted elsewhere) are left to the
    /// vanish handling.
    fn report_thread_growth(&mut self, sender: &ComponentSender<Self>) {
        if !self.threading || self.selected_ids.len() != 1 {
            return;
        }
        let Some(key) = self.selected_id else { return };
        let Some(m) = self.shown.iter().find(|m| (m.account_id, m.id) == key).map(|m| Message::clone(m)) else {
            return;
        };
        let (thread, solo) = self.conversation_for(&m);
        if solo || thread.len() <= 1 {
            return;
        }
        let keys: Vec<(u32, u32)> = thread.iter().map(|t| (t.account_id, t.id)).collect();
        let grew = keys.len() > self.emitted_thread.len()
            && self.emitted_thread.iter().all(|k| keys.contains(k));
        if grew {
            self.emitted_thread = keys;
            let _ = sender.output(MessageListOutput::ThreadGrew { message: m, thread });
        }
    }

    /// What a Move To… on `msg` offers: the message, then — when it heads a
    /// conversation — the rest of its members, with the whole-conversation
    /// switch (#171).
    fn move_to_messages(&self, msg: &Message) -> (Vec<Message>, bool) {
        let members = self.thread_members(msg);
        let is_head =
            members.first().is_some_and(|h| (h.account_id, h.id) == (msg.account_id, msg.id));
        let mut messages = vec![msg.clone()];
        let offer_whole = is_head && members.len() > 1;
        if offer_whole {
            messages.extend(
                members.iter().filter(|m| (m.account_id, m.id) != (msg.account_id, msg.id)).cloned(),
            );
        }
        (messages, offer_whole)
    }

    /// A point in the rows list, in the window's coordinates (the app
    /// anchors popovers on the window; falls back to the point as given).
    fn window_point(&self, x: f64, y: f64) -> (f64, f64) {
        let list = &self.list_view;
        list.root()
            .and_then(|root| {
                let root: gtk::Widget = root.upcast();
                list.compute_point(&root, &gtk::graphene::Point::new(x as f32, y as f32))
            })
            .map_or((x, y), |p| (p.x() as f64, p.y() as f64))
    }

    /// Toolbar count: every message the filter lets through, all listed.
    fn count_label(&self) -> String {
        format!("{}", self.total_matches)
    }

    /// Whether the bottom loading spinner should show: the folder's index is
    /// still streaming in and the user has reached the end of what is here.
    fn is_loading_more(&self) -> bool {
        !self.index_complete && (self.at_bottom || self.shown.is_empty())
    }

    /// Whether to show the empty-folder placeholder: the folder has loaded,
    /// nothing is in it, no search is filtering it, and no more rows are on
    /// their way (the loading indicator covers that state instead).
    fn is_empty_state(&self) -> bool {
        self.loaded && self.shown.is_empty() && self.query.is_empty() && self.index_complete
    }

    /// Runs `f` (typically some change to the rows in `self.rows`/`self.shown`)
    /// without letting the scroll position move — including a scroll GTK
    /// performs on its own to keep the *selected* row in view whenever the
    /// list's contents change, even if that row was never touched by `f`.
    /// Saves the current position, pins the adjustment there through every
    /// change `f` (or GTK) makes, and does a final restore once layout has
    /// had a tick to settle — a single restore right after `f` can lose the
    /// race against GTK's own scroll, which can land after this returns.
    fn preserving_scroll(&mut self, f: impl FnOnce(&mut Self)) {
        let Some(scroller) = self.scroller.clone() else {
            f(self);
            return;
        };
        let adj = scroller.vadjustment();
        let pos = adj.value();
        let hid = adj.connect_notify_local(Some("value"), move |adj, _| {
            if (adj.value() - pos).abs() > f64::EPSILON {
                adj.set_value(pos);
            }
        });
        f(self);
        let adj2 = adj.clone();
        gtk::glib::idle_add_local_once(move || {
            adj2.set_value(pos);
            adj2.disconnect(hid);
        });
    }

    /// Back to the top of the list (a switch, a new search or sort).
    fn scroll_top(&self) {
        if let Some(s) = &self.scroller {
            s.vadjustment().set_value(0.0);
        }
        // The view keeps its place by a row. A list that grows under it
        // before it is laid out (another folder in kept rows, #401) would
        // keep the old last row in view, and the adjustment does not reach
        // past the old size yet: move the view's own anchor too.
        if self.shared.model.len() > 0 {
            self.list_view.scroll_to(0, gtk::ListScrollFlags::NONE, None);
        }
    }

    /// The list's settings as every row reads them, pushed to the rows on
    /// screen. The rest pick them up when they are shown.
    fn sync_look(&self) {
        {
            let mut look = self.shared.look.borrow_mut();
            look.gravatar = self.gravatar;
            look.avatars = self.avatars;
            look.sender_logos = self.sender_logos;
            look.preview_lines = self.preview_lines;
            look.show_subject = self.show_subject;
            // The palette hangs under a card; a single line has no room for
            // it, so actions come from the menu, swipes and keys there.
            look.show_palette = self.list_palette && !self.single_line;
            look.single_line = self.single_line;
            look.columns = self
                .columns
                .iter()
                .copied()
                .filter(|c| *c != crate::config::ListColumn::Due || self.graph_in_view)
                .collect();
            look.in_junk = self.in_junk;
            look.in_drafts = self.in_drafts;
            look.show_recipient = self.show_recipient;
            look.thread_expansion = self.thread_expansion;
            look.ringed = if self.colorize { self.account_colors.keys().copied().collect() } else { Default::default() };
            look.face_gen = self.face_gen;
            look.tags_gen = self.tags_gen;
            look.widths = self.fitted_widths();
        }
        // The rows on screen are about to be another view's, filled anyway.
        if self.switched {
            self.look_stale.set(true);
        } else {
            self.shared.refresh_all();
        }
        self.sync_headings();
    }

    /// The columns' widths as the rows and the headings draw them (#334):
    /// as set, unless together they leave the subject too little room, when
    /// the name columns give way in proportion (the dates keep theirs). Fitted here rather than left to each
    /// row's box, which shares out a shortage by what that row holds (tags,
    /// a conversation's count), so a heading and its column would part.
    fn fitted_widths(&self) -> std::collections::HashMap<crate::config::ListColumn, i32> {
        use crate::config::ListColumn as C;
        use crate::ui::message_row::{default_width, resizable};
        /// What a date as wide as its text takes, about.
        const DATE_PX: i32 = 64;
        /// Room kept for the subject, and for tags beside it.
        const SUBJECT_PX: i32 = 100;
        const TAGS_PX: i32 = 60;
        let mut widths = self.widths.clone();
        if self.pane_width <= 0 {
            return widths;
        }
        let columns: Vec<C> =
            self.columns.iter().copied().filter(|c| *c != C::Due || self.graph_in_view).collect();
        // The pill's margins and padding, the unread dot and the spacing.
        let mut fixed = 12 + 18 + 18 + 8 * columns.len().saturating_sub(1) as i32;
        if self.avatars {
            fixed += 16 + 8;
        }
        let mut wanted: Vec<(C, i32)> = Vec::new();
        for c in &columns {
            match c {
                C::Star | C::Attachment => fixed += 12,
                C::Importance => fixed += 16,
                C::Tags => fixed += TAGS_PX,
                C::Subject => fixed += SUBJECT_PX,
                // A date cut short says nothing: the names give way instead.
                C::Due | C::Date => {
                    let px = widths.get(c).copied().unwrap_or_else(|| default_width(*c));
                    fixed += if px > 0 { px } else { DATE_PX };
                }
                c if resizable(*c) => {
                    let px = widths.get(c).copied().unwrap_or_else(|| default_width(*c));
                    wanted.push((*c, px.max(1)));
                }
                _ => {}
            }
        }
        let sum: i32 = wanted.iter().map(|(_, px)| px).sum();
        let room = (self.pane_width - fixed).max(0);
        if sum > room && sum > 0 {
            for (c, px) in wanted {
                widths.insert(c, ((px as i64 * room as i64 / sum as i64) as i32).max(40));
            }
        }
        widths
    }

    /// Fill the column headings from the look and the sort (#334). Only
    /// while they are shown: they follow the next change once they are.
    fn sync_headings(&self) {
        if !(self.single_line && self.headings) {
            return;
        }
        let look = self.shared.look.borrow().clone();
        let show_recipient = self.show_recipient;
        let sorted = look
            .columns
            .iter()
            .copied()
            .find(|c| match (column_sort(*c, show_recipient), self.sort) {
                (Some(SortOrder::DateNewest), SortOrder::DateOldest) => true,
                (Some(order), current) => order == current,
                (None, _) => false,
            })
            .map(|c| (c, self.sort.downwards() != self.sort_reversed));
        let input = self.shared.input.clone();
        let resized = self.shared.input.clone();
        *self.heading_bins.borrow_mut() = crate::ui::message_row::fill_headings(
            &self.headings_bar,
            &look,
            sorted,
            &|c| column_sort(c, show_recipient).is_some(),
            Rc::new(move |c| {
                let _ = input.send(MessageListInput::SortByColumn(c));
            }),
            Rc::new(move |column, width, done| {
                let _ = resized.send(MessageListInput::ResizeColumn { column, width, done });
            }),
        );
    }

    /// The selected rows' positions, in list order.
    fn selected_positions(&self) -> Vec<usize> {
        let set = self.shared.selection.selection();
        match gtk::BitsetIter::init_first(&set) {
            Some((iter, first)) => std::iter::once(first).chain(iter).map(|p| p as usize).collect(),
            None => Vec::new(),
        }
    }

    fn keys_at(&self, positions: &[usize]) -> Vec<(u32, u32)> {
        positions.iter().filter_map(|&p| self.shown.get(p).map(|m| (m.account_id, m.id))).collect()
    }

    /// Select exactly these rows, telling GTK only if that changes anything.
    fn select_positions(&self, positions: &[usize]) {
        let mut wanted: Vec<usize> = positions.iter().copied().filter(|&p| p < self.shown.len()).collect();
        wanted.sort_unstable();
        wanted.dedup();
        if wanted == self.selected_positions() {
            return;
        }
        let set = gtk::Bitset::new_empty();
        for p in &wanted {
            set.add(*p as u32);
        }
        let mask = gtk::Bitset::new_range(0, self.shown.len() as u32);
        self.shared.selection.set_selection(&set, &mask);
    }

    /// Change a message wherever the list holds it — the index, the search
    /// pool and its row — and refresh the row if it is on screen.
    fn update_message(&mut self, is: impl Fn(&Message) -> bool, change: impl Fn(&mut Message)) {
        let mut changed: Option<Rc<Message>> = None;
        for list in [&mut self.all, &mut self.search_pool] {
            if let Some(m) = list.iter_mut().find(|m| is(m)) {
                change(Rc::make_mut(m));
                changed.get_or_insert_with(|| m.clone());
            }
        }
        if let Some(idx) = self.shown.iter().position(|m| is(m)) {
            let m = match &changed {
                // The same message, as the index now has it.
                Some(m) if (m.account_id, m.folder_id, m.id) == (self.shown[idx].account_id, self.shown[idx].folder_id, self.shown[idx].id) => m.clone(),
                _ => {
                    let mut m = Message::clone(&self.shown[idx]);
                    change(&mut m);
                    Rc::new(m)
                }
            };
            self.shown[idx] = m.clone();
            self.shared.model.update_row(idx, |d| d.msg = m);
        } else if let Some(row) = self.tail.iter_mut().find(|r| is(&r.msg)) {
            // Not on screen yet: what it says when it gets there.
            let mut data = RowData::clone(row);
            data.msg = match &changed {
                Some(m) => m.clone(),
                None => {
                    let mut m = Message::clone(&data.msg);
                    change(&mut m);
                    Rc::new(m)
                }
            };
            *row = Rc::new(data);
        }
    }

    /// Put up to `n` more rows on screen from the tail.
    fn grow_window(&mut self, n: usize) {
        if self.tail.is_empty() {
            return;
        }
        let n = n.min(self.tail.len());
        let more: Vec<Rc<RowData>> = self.tail.drain(..n).collect();
        self.shown.extend(more.iter().map(|r| r.msg.clone()));
        let at = self.shared.model.len();
        self.shared.model.splice(at, 0, more);
        self.window = self.shown.len();
    }

    /// A message's read or starred state changed: recompute what its
    /// conversation's head row says, so a collapsed thread's highlight
    /// clears exactly when its last unread message is read.
    ///
    /// As [`MessageList::describe_group`] does, only this folder's own
    /// messages count, and they are found by slot. Going through the
    /// (account, UID) thread maps let a message in another folder (the reply
    /// you sent, filed in Sent) stand in for an unread one here that shared
    /// its UID, and the row stayed bold after it was read (#333).
    fn refresh_thread_head(&mut self, slot: (u32, u32, u32)) {
        let Some(own) = self.groups.values().find(|g| g.iter().any(|m| thread_slot(m) == slot)).cloned() else {
            return;
        };
        let head = thread_slot(&own[0]);
        let Some(idx) = self.shown.iter().position(|m| thread_slot(m) == head) else {
            return;
        };
        // A row that stands for one message shows that message's own state.
        if self.shared.model.row(idx).is_none_or(|r| r.meta.key.is_none()) {
            return;
        }
        let now = |m: &Rc<Message>| -> Rc<Message> {
            let s = thread_slot(m);
            self.all.iter().find(|a| thread_slot(a) == s).cloned().unwrap_or_else(|| m.clone())
        };
        let unread = own.iter().any(|m| now(m).unread);
        let starred = own.iter().any(|m| now(m).starred);
        self.shared.model.update_row(idx, |d| {
            d.meta.unread = unread;
            d.meta.starred = starred;
        });
    }


    /// Begin collapsing a thread: slide its replies shut (they stay where
    /// they are in the list — only their own Revealer closes), then drop them
    /// once that animation has finished. Dropping right away — before the
    /// replies have shrunk — would just make them disappear outright. (PR #79)
    fn start_collapse_thread(&mut self, key: (u32, String), sender: &ComponentSender<Self>) {
        if let Some(members) = self.thread_members.get(&key).cloned() {
            // The head survives the toggle (only its replies are removed),
            // so its own chevron can rotate shut in place, in step with the
            // replies sliding closed beneath it.
            if let Some(&head_key) = members.first() {
                if let Some(idx) = self.shown.iter().position(|m| (m.account_id, m.id) == head_key) {
                    self.shared.model.update_row(idx, |d| d.meta.expanded = false);
                }
            }
            // `members` is head-first (see `rebuild`); only the replies
            // beneath it animate closed.
            for child_key in members.iter().skip(1) {
                if let Some(idx) = self.shown.iter().position(|m| (m.account_id, m.id) == *child_key) {
                    self.shared.model.update_row(idx, |d| d.meta.revealed = false);
                }
            }
        }
        if let Some(old) = self.collapsing_threads.remove(&key) {
            old.remove();
        }
        let s = sender.clone();
        let timer_key = key.clone();
        // Matches the Revealer's own transition duration, so the rows are
        // fully closed by the time they're actually dropped from the list.
        let timer = gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
            s.input(MessageListInput::FinishCollapseThread(timer_key));
        });
        self.collapsing_threads.insert(key, timer);
    }

    /// Put a thread's replies in right after its head, without touching any
    /// other row — the counterpart to `collapse_thread_rows`. Each new row
    /// slides open the first time it is shown (see `RowMeta::appear`).
    /// (PR #79)
    fn expand_thread(&mut self, key: &(u32, String)) {
        let Some(members) = self.thread_members.get(key).cloned() else { return };
        let Some(&head_key) = members.first() else { return };
        let Some(head_pos) = self.shown.iter().position(|m| (m.account_id, m.id) == head_key) else {
            return;
        };
        // `members` is already oldest-first (see `rebuild`).
        let children: Vec<Rc<Message>> = {
            let source = self.active_source();
            members
                .iter()
                .skip(1)
                .filter_map(|k| {
                    source.iter().find(|m| (m.account_id, m.id) == *k).or_else(|| self.nested.get(k)).cloned()
                })
                .collect()
        };
        if children.is_empty() {
            return;
        }
        // The head survives the toggle (only its replies are put in), so its
        // own chevron can rotate open in place.
        self.shared.model.update_row(head_pos, |d| d.meta.expanded = true);
        let n = children.len();
        let rows: Vec<Rc<RowData>> = children
            .iter()
            .enumerate()
            .map(|(i, m)| {
                Rc::new(RowData {
                    msg: m.clone(),
                    meta: RowMeta { is_child: true, is_last: i + 1 == n, revealed: true, appear: true, ..Default::default() },
                })
            })
            .collect();
        self.shared.model.splice(head_pos + 1, 0, rows);
        self.shown.splice(head_pos + 1..head_pos + 1, children);
    }

    /// Drop a thread's reply rows (already slid shut by `start_collapse_thread`)
    /// without touching any other row — the counterpart to `expand_thread`.
    /// (PR #79)
    fn collapse_thread_rows(&mut self, key: &(u32, String)) {
        let Some(members) = self.thread_members.get(key).cloned() else { return };
        let mut indices: Vec<usize> = members
            .iter()
            .skip(1)
            .filter_map(|k| self.shown.iter().position(|m| (m.account_id, m.id) == *k))
            .collect();
        indices.sort_unstable();
        // Back to front, so earlier removals don't shift the indices still
        // to come.
        for &idx in indices.iter().rev() {
            self.shown.remove(idx);
            self.shared.model.splice(idx, 1, Vec::new());
        }
    }

    /// Whether a search is currently active (the query is non-empty).
    fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// The message set the search filters over: the cross-folder pool while an
    /// `AllFolders` search is active (and the pool has arrived), otherwise the
    /// current folder's own index.
    fn active_source(&self) -> &[Rc<Message>] {
        if self.searching_pool() {
            &self.search_pool
        } else {
            &self.all
        }
    }

    /// The source changed: its conversations are to be worked out again,
    /// and an answer still on its way from a background thread is stale.
    fn drop_threads(&self) {
        self.source_threads.take();
        self.threads_gen.set(self.threads_gen.get() + 1);
    }

    /// For a large list whose conversations are not worked out yet: start
    /// that on a background thread and answer `true`, the rebuild to wait
    /// for [`MessageListInput::ThreadsReady`]. The rows on screen stay
    /// meanwhile, and the main loop stays free.
    fn threads_pending(&self) -> bool {
        let pool = self.searching_pool();
        if self.source_threads.borrow().as_ref().is_some_and(|t| t.pool == pool) {
            return false;
        }
        let source = self.active_source();
        if source.len() < THREADS_OFF_MAIN {
            return false;
        }
        let gen = self.threads_gen.get();
        if self.threads_job.get() == Some(gen) {
            return true;
        }
        self.threads_job.set(Some(gen));
        let input: Vec<ThreadInput> = source
            .iter()
            .map(|m| ThreadInput {
                account_id: m.account_id,
                folder_id: m.folder_id,
                id: m.id,
                uid: m.uid,
                message_id: m.message_id.clone(),
                references: m.references.clone(),
            })
            .collect();
        let links = self.thread_links.clone();
        let sender = self.input.clone();
        std::thread::spawn(move || {
            let t = std::time::Instant::now();
            let threads = source_threads_of(&input, &links, pool);
            tracing::debug!("list: {} messages threaded off the main loop in {:?}", input.len(), t.elapsed());
            let _ = sender.send(MessageListInput::ThreadsReady { gen, threads: Box::new(threads) });
        });
        true
    }

    fn searching_pool(&self) -> bool {
        self.searching() && self.scope == SearchScope::AllFolders && !self.search_pool.is_empty()
    }

    /// The conversations of `active_source`, from the kept copy while the
    /// source it was worked out from is still the one in use.
    fn source_threads(&self) -> std::rc::Rc<SourceThreads> {
        let pool = self.searching_pool();
        if let Some(t) = self.source_threads.borrow().as_ref().filter(|t| t.pool == pool) {
            return t.clone();
        }
        let t = std::rc::Rc::new(source_threads_of(self.active_source(), &self.thread_links, pool));
        *self.source_threads.borrow_mut() = Some(t.clone());
        t
    }

    fn search_placeholder(&self) -> String {
        i18n(match self.scope {
            SearchScope::AllFolders => "Search all folders",
            SearchScope::ThisFolder => "Search this folder",
        })
    }

    /// Drop any active search: clear the query and the entry text so a folder
    /// switch doesn't leave a stale term filtering the new folder.
    fn clear_search(&mut self) {
        if self.query.is_empty() {
            return;
        }
        self.query.clear();
        if let Some(e) = &self.search_entry {
            e.set_text("");
        }
    }

    /// What the search field and the quick filters let through.
    fn filter(&self) -> impl Fn(&Message) -> bool {
        let q = self.query.to_lowercase();
        let (unread_only, starred_only) = (self.unread_only, self.starred_only);
        move |m: &Message| {
            (!unread_only || m.unread)
                && (!starred_only || m.starred)
                && (q.is_empty()
                    || m.subject.to_lowercase().contains(&q)
                    || m.from_name.to_lowercase().contains(&q)
                    || m.from_addr.to_lowercase().contains(&q)
                    || m.preview.to_lowercase().contains(&q))
        }
    }

    /// One conversation's row: what its head says about it, and its members
    /// head first, the parts filed in other folders included (#309). `own`
    /// is the folder's own members, oldest first. Records the conversation's
    /// membership for the whole-conversation actions as it goes.
    fn describe_group(
        &mut self,
        key: &(u32, String),
        own: &[Rc<Message>],
        passes: &impl Fn(&Message) -> bool,
    ) -> (RowMeta, Vec<Rc<Message>>) {
        let mut msgs = own.to_vec();
        let summary = self.threading.then(|| self.thread_summaries.get(key)).flatten();
        // The parts of the conversation filed elsewhere, which the row opens
        // out into along with this folder's own (#309).
        let extras: Vec<Rc<Message>> = match summary {
            Some(s) if self.thread_expansion => {
                nested_members(&s.members, &msgs, &self.listed_folders, passes).into_iter().map(Rc::new).collect()
            }
            _ => Vec::new(),
        };
        let count = msgs.len() + extras.len();
        // What the badge says. `count` is what the row holds and goes on
        // steering it (whether it nests and what expands) but the number on
        // the chip is the size of the *conversation*, drafts included (#222).
        // Never less than what is on screen: a stale or partial answer from
        // the cache must not make the badge contradict the rows under it.
        let total = if self.threading { summary.map(|s| s.count).unwrap_or(0).max(count) } else { count };
        // The newest member this folder holds, and the newer one the cache
        // found in another folder — the reply you sent, filed in Sent (#236).
        let newest_here = msgs.last().expect("a group holds at least one message").clone();
        let elsewhere = self.thread_row_newest.then(|| latest_elsewhere(summary, newest_here.timestamp)).flatten();
        // Who took part (#334), the replies filed in other folders included.
        let people = self.columns.contains(&crate::config::ListColumn::Correspondents).then(|| {
            let mut all: Vec<&Message> = msgs.iter().map(|m| &**m).collect();
            all.extend(summary.iter().flat_map(|s| s.members.iter()));
            correspondents(&all)
        });
        // `expanded_threads` stores toggles away from the default state. With
        // expansion disabled no thread ever opens in the list; the stored
        // toggles survive for when it is re-enabled.
        let expanded =
            count > 1 && self.thread_expansion && (self.expanded_threads.contains(key) != self.default_expanded);
        // The head stays marked unread while ANY message in its conversation
        // is unread, hidden replies included, but only this folder's own: an
        // archived message still unread is not new mail here.
        let any_unread = count > 1 && msgs.iter().any(|m| m.unread);
        // The head is the thread's *oldest* message, but its row speaks for
        // the conversation's NEWEST one: its sender, its preview and the time
        // it landed, rather than the opener re-shown every time a reply
        // arrives.
        //
        // Which message that is can depend on more than this folder. A mail
        // you answered is one row in the Inbox and the answer is in Sent, so
        // the row quotes the other side however recently you wrote back. With
        // "Show your own replies in the message list" turned on, the cache's
        // newest wins whenever it is later than anything on screen (#236);
        // off, which is how Hylki has always behaved, the folder has the last
        // word.
        let (latest, from, preview, latest_at) = if let Some(l) = &elsewhere {
            (
                Some(crate::models::datetime_list_at(l.timestamp, &l.date)),
                Some((l.from_name.clone(), l.from_addr.clone())),
                Some(l.preview.clone()),
                Some((l.timestamp, l.date.clone())),
            )
        } else if count > 1 {
            (
                Some(newest_here.datetime_list()),
                Some((newest_here.from_name.clone(), newest_here.from_addr.clone())),
                Some(newest_here.preview.clone()),
                Some((newest_here.timestamp, newest_here.date.clone())),
            )
        } else {
            (None, None, None, None)
        };
        let any_starred = count > 1 && msgs.iter().any(|m| m.starred);
        // The row stays this folder's oldest message, whatever the other
        // folders hold that is older, so what is done to the row is done to
        // mail in this folder. The rest follow in time order.
        if !extras.is_empty() {
            for m in &extras {
                self.nested.insert((m.account_id, m.id), m.clone());
            }
            let head = msgs.remove(0);
            msgs.extend(extras);
            msgs.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.uid.cmp(&b.uid)));
            msgs.insert(0, head);
        }
        if count > 1 {
            let members: Vec<(u32, u32)> = msgs.iter().map(|m| (m.account_id, m.id)).collect();
            for k in &members {
                self.msg_thread.insert(*k, key.clone());
            }
            self.thread_members.insert(key.clone(), members);
        }
        let meta = RowMeta {
            count: total,
            // The parts filed in other folders open out too (#309); a
            // conversation with nothing to show beyond its row, all of it in
            // Trash, say, wears a bare count and no caret.
            expandable: count > 1,
            expanded,
            key: (count > 1).then(|| key.clone()),
            from,
            preview,
            latest,
            latest_at,
            unread: any_unread,
            starred: any_starred,
            people,
            revealed: true,
            group: self.threading.then(|| key.clone()),
            ..Default::default()
        };
        (meta, msgs)
    }

    /// Ask again about the conversations on screen whose account's mail
    /// changed. A row only asks when it is first bound, and the rows on
    /// screen already are.
    fn recheck_bound_rows(&mut self) {
        let accounts = std::mem::take(&mut self.recheck_accounts);
        if accounts.is_empty() || !self.threading {
            return;
        }
        for row in self.shared.bound_rows() {
            let group = row.data().and_then(|d| d.meta.group.clone());
            if let Some(group) = group.filter(|(aid, _)| accounts.contains(aid)) {
                self.shared.want(group);
            }
        }
    }

    /// Fresh sizes for these conversations (#222): their head rows say so in
    /// place. A conversation opened out in the list may gain or lose rows,
    /// and that takes a rebuild.
    fn apply_summaries(&mut self, keys: Vec<(u32, String)>) {
        let passes = self.filter();
        for key in keys {
            let Some(own) = self.groups.get(&key).cloned() else { continue };
            let head = (own[0].account_id, own[0].id);
            let Some(pos) = self.shown.iter().position(|m| (m.account_id, m.id) == head) else { continue };
            let Some(row) = self.shared.model.row(pos) else { continue };
            if row.meta.expanded {
                self.queue_rebuild(true);
                return;
            }
            let (meta, _) = self.describe_group(&key, &own, &passes);
            if meta.expanded {
                self.queue_rebuild(true);
                return;
            }
            self.shared.model.update_row(pos, |d| d.meta = meta);
        }
    }

    /// `rebuild_rows`, keeping a list read from the very top at the top.
    /// The view holds its place by a row, so a new message put in above the
    /// first one landed out of sight, just above the view (#400). Scrolled
    /// down, the view keeps the row it has, as before.
    fn rebuild(&mut self) {
        let at_top = self.scroller.as_ref().is_some_and(|s| s.vadjustment().value() < 1.0);
        self.rebuild_rows();
        if at_top {
            self.scroll_top();
        }
    }

    fn rebuild_rows(&mut self) {
        let t_rebuild = std::time::Instant::now();
        let passes = self.filter();
        let source_len = self.active_source().len();
        let mut matches: Vec<Rc<Message>> = self.active_source().iter().filter(|m| passes(m)).cloned().collect();
        let mut filtered = matches.len() != source_len;
        // A large list's conversations are worked out on a background
        // thread (`threads_pending`). Until they are in, its newest messages
        // go on screen at once, grouped among themselves; the whole list
        // follows when the answer comes.
        let preview = self.threading && !filtered && self.threads_pending();
        self.listed_folders = matches.iter().map(|m| (m.account_id, m.folder_id)).collect();
        let graph_in_view = self.listed_folders.iter().any(|(a, _)| self.graph_accounts.contains(a));
        if graph_in_view != self.graph_in_view {
            self.graph_in_view = graph_in_view;
            self.sync_look();
        }
        let sort = self.sort;
        let reversed = self.sort_reversed;
        matches.sort_by(|a, b| {
            let order = message_cmp(a, b, sort);
            if reversed { order.reverse() } else { order }
        });
        self.total_matches = matches.len();
        if preview {
            matches.truncate(PREVIEW_ROWS);
            filtered = true;
        }

        // Group into conversations by reply headers (Message-ID / In-Reply-To /
        // References) across the whole list, preserving the sort's order of
        // threads. With threading off, every message is its own group.
        // Unfiltered, the list is the folder, whose grouping is kept.
        let kept_threads;
        let own_keys;
        let empty = std::collections::HashMap::new();
        let keys = if !self.threading {
            &empty
        } else if !filtered {
            kept_threads = self.source_threads();
            &kept_threads.keys
        } else {
            own_keys = compute_thread_keys(&matches, &self.thread_links);
            &own_keys
        };
        let key_for = |m: &Message| -> (u32, String) {
            keys.get(&thread_slot(m))
                .cloned()
                .unwrap_or_else(|| (m.account_id, format!("\u{0}uid{}/{}", m.folder_id, m.uid)))
        };
        let mut order: Vec<(u32, String)> = Vec::new();
        let mut groups: std::collections::HashMap<(u32, String), Vec<Rc<Message>>> =
            std::collections::HashMap::new();
        for m in matches {
            let key = key_for(&m);
            if let Some(v) = groups.get_mut(&key) {
                v.push(m);
            } else {
                order.push(key.clone());
                groups.insert(key, vec![m]);
            }
        }

        // A reply that is sliding open, or shut, keeps doing so.
        let appearing: std::collections::HashSet<(u32, u32, u32)> = (0..self.shared.model.len())
            .filter_map(|i| self.shared.model.row(i))
            .filter(|r| r.meta.appear)
            .map(|r| r.slot())
            .collect();

        // Flatten back into display order, recording per-row thread metadata.
        let mut shown: Vec<Rc<Message>> = Vec::new();
        let mut rows: Vec<Rc<RowData>> = Vec::new();
        let mut thread_drag = crate::ui::message_row::ThreadDragKeys::new();
        self.msg_thread.clear();
        self.thread_members.clear();
        self.nested.clear();
        self.groups.clear();
        let mut any_expanded = false;
        for key in &order {
            let mut msgs = groups.remove(key).unwrap();
            // A conversation reads like a transcript: the message that started it
            // is the row on screen, and its replies descend beneath it to the
            // newest. Where the *thread* sits among the other rows is still the
            // list's sort order — recent activity keeps it near the top — but
            // inside the thread, time only runs one way.
            msgs.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.uid.cmp(&b.uid)));
            // A drag that starts on the row carries the folder's own members
            // (#171).
            if msgs.len() > 1 {
                let head = &msgs[0];
                thread_drag.insert(
                    (head.account_id, head.id),
                    msgs.iter().map(|m| (m.account_id, m.folder_id, m.uid, m.id)).collect(),
                );
            }
            let (meta, members) = self.describe_group(key, &msgs, &passes);
            if self.threading {
                self.groups.insert(key.clone(), msgs);
            }
            let expanded = meta.expanded;
            let mut it = members.into_iter();
            let head = it.next().unwrap();
            shown.push(head.clone());
            rows.push(Rc::new(RowData { msg: head, meta }));
            if expanded {
                any_expanded = true;
                let closing = self.collapsing_threads.contains_key(key);
                let rest: Vec<Rc<Message>> = it.collect();
                let n = rest.len();
                for (j, child) in rest.into_iter().enumerate() {
                    let slot = thread_slot(&child);
                    shown.push(child.clone());
                    rows.push(Rc::new(RowData {
                        msg: child,
                        meta: RowMeta {
                            is_child: true,
                            is_last: j + 1 == n,
                            revealed: !closing,
                            appear: appearing.contains(&slot),
                            ..Default::default()
                        },
                    }));
                }
            }
        }
        *self.shared.thread_drag.borrow_mut() = thread_drag;

        // A message that was moved away and brought back comes home under a
        // new UID, and a row's id is its UID. The selection would find no row
        // to light, so the highlight blinked off and came back a moment later
        // as the list caught up. Follow the selection by Message-ID over a
        // renumbering instead (#200).
        let lost = self.selected_ids.iter().any(|k| !shown.iter().any(|m| (m.account_id, m.id) == *k));
        if lost {
            let renumbered: Vec<((u32, u32), (u32, u32))> = self
                .selected_ids
                .iter()
                .filter(|k| !shown.iter().any(|m| (m.account_id, m.id) == **k))
                .filter_map(|k| {
                    let was = self
                        .shown
                        .iter()
                        .find(|m| (m.account_id, m.id) == *k)
                        .filter(|m| !m.message_id.is_empty())?;
                    let now =
                        shown.iter().find(|m| m.account_id == was.account_id && m.message_id == was.message_id)?;
                    Some((*k, (now.account_id, now.id)))
                })
                .collect();
            for (was, now) in renumbered {
                for k in self.selected_ids.iter_mut().filter(|k| **k == was) {
                    *k = now;
                }
                if self.selected_id == Some(was) {
                    self.selected_id = Some(now);
                }
            }
        }
        // On screen, the window: as many rows as were before (so a rebuild
        // does not pull the list back from where it was scrolled), at least
        // the first window, and far enough to hold the selection.
        let mut shown = shown;
        let mut rows = rows;
        let selected_at = shown
            .iter()
            .rposition(|m| self.selected_ids.contains(&(m.account_id, m.id)) || self.selected_id == Some((m.account_id, m.id)))
            .map_or(0, |i| i + 1);
        let limit = self.window.max(LIST_WINDOW).max(selected_at).min(rows.len());
        self.tail = rows.split_off(limit);
        shown.truncate(limit);
        self.window = limit.max(LIST_WINDOW);
        self.shown = shown;
        // Only what changed reaches the view: rows that show the same message
        // stay where they are, with whatever they were doing (#323).
        let t_rows = std::time::Instant::now();
        let stale = self.look_stale.take();
        if !self.shared.model.replace(rows, stale) && stale {
            self.shared.refresh_all();
        }
        self.select_current();

        // Expanded conversations indent their member cards; give the pane the
        // extra floor that needs while any thread is open, so nothing is
        // clipped at the right edge (see THREAD_EXPANDED_EXTRA).
        if let Some(s) = &self.scroller {
            let floor = LIST_MIN_WIDTH + if any_expanded { THREAD_EXPANDED_EXTRA } else { 0 };
            s.set_size_request(floor, -1);
        }
        tracing::debug!(
            "list: rebuild {} rows (+{} to come) of {} — sort+group {:?}, model {:?}",
            self.shown.len(),
            self.tail.len(),
            self.total_matches,
            t_rows.duration_since(t_rebuild),
            t_rows.elapsed()
        );
    }

    /// Everything the view needs wired: selection, activation and keys.
    fn wire_list(list: &gtk::ListView, shared: &RowShared, input: &relm4::Sender<MessageListInput>) {
        // Multiple selection: plain click selects one (shown in the reader),
        // Ctrl/Shift extend the selection for bulk actions; double click (or
        // Enter) pops a message out into its own window.
        list.add_css_class("message-list");
        list.set_single_click_activate(false);
        list.set_show_separators(false);
        let s = input.clone();
        shared.selection.connect_selection_changed(move |_, _, _| {
            let _ = s.send(MessageListInput::SelectionChanged);
        });
        let s = input.clone();
        list.connect_activate(move |_, pos| {
            let _ = s.send(MessageListInput::RowActivated(pos as i32));
        });
        // Delete / Backspace on a focused row deletes the selection (single or
        // multi). Scoped to the list, so typing in the search box is unaffected.
        let key = gtk::EventControllerKey::new();
        let s = input.clone();
        key.connect_key_pressed(move |_, keyval, _, _| {
            if matches!(keyval, gtk::gdk::Key::Delete | gtk::gdk::Key::BackSpace) {
                // ResolveDelete, not a straight Bulk: a lone thread-head row
                // stands for its whole conversation (confirmed by the app).
                let _ = s.send(MessageListInput::ResolveDelete);
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            }
        });
        list.add_controller(key);
    }

    /// Ask for a rebuild at the end of the current main-loop pass, folding
    /// any further requests before then into it. A request that must not
    /// keep the scroll offset (a folder switch) wins over ones that would.
    fn queue_rebuild(&mut self, preserve_scroll: bool) {
        let first = self.rebuild_queued.is_none();
        let preserve = self.rebuild_queued.map_or(preserve_scroll, |p| p && preserve_scroll);
        self.rebuild_queued = Some(preserve);
        if !first {
            return;
        }
        let input = self.input.clone();
        let run = move || {
            glib::idle_add_local_full(glib::Priority::HIGH, move || {
                let _ = input.send(MessageListInput::RunQueuedRebuild);
                glib::ControlFlow::Break
            });
        };
        // Another view: the frame that moves the sidebar's highlight is
        // drawn first, and the new rows a frame later, so the click shows at
        // once rather than after the list is filled and laid out (#401).
        if self.switched || !preserve_scroll {
            if let Some(clock) = self.list_view.frame_clock() {
                after_next_paint(&clock, run);
                self.list_view.queue_draw();
                return;
            }
        }
        // Otherwise ahead of GTK's layout and paint, so the old rows are
        // never laid out one more time for nothing before they go.
        run();
    }

    /// The shown row (a thread head) whose conversation holds the message
    /// `key`, when `key` is in the index but has no row of its own.
    fn thread_head_for(&self, key: (u32, u32)) -> Option<(u32, u32)> {
        if !self.threading {
            return None;
        }
        let m = self.active_source().iter().find(|m| (m.account_id, m.id) == key)?;
        let threads = self.source_threads();
        let thread = threads.keys.get(&thread_slot(m))?;
        self.shown
            .iter()
            .find(|m| threads.keys.get(&thread_slot(m)) == Some(thread))
            .map(|m| (m.account_id, m.id))
    }

    /// Every on-screen member of `m`'s conversation (oldest first) — from any
    /// member, head or reply. Empty when threading is off or `m` stands alone,
    /// so callers can treat non-empty as "this is a real thread".
    /// The conversation a row stands for, or empty when the row means only its
    /// own message. A row stands for its thread when it is the head of one and
    /// the thread is not currently opened out in the list — which, with
    /// expandable conversations off, it never is.
    fn row_conversation(&self, m: &Message) -> Vec<Message> {
        let members = self.thread_members(m);
        if members.is_empty()
            || !heads_its_row((m.account_id, m.id), &self.msg_thread, &self.thread_members)
        {
            return Vec::new();
        }
        let expanded = self.thread_expansion
            && self
                .msg_thread
                .get(&(m.account_id, m.id))
                .is_some_and(|key| {
                    self.expanded_threads.contains(key) != self.default_expanded
                });
        if expanded {
            Vec::new()
        } else {
            members
        }
    }

    fn thread_members(&self, m: &Message) -> Vec<Message> {
        if !self.threading {
            return Vec::new();
        }
        let mut members = self.source_members(m);
        if members.len() <= 1 {
            return Vec::new();
        }
        members.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.uid.cmp(&b.uid)));
        members
    }

    /// Everything in `active_source` that shares `m`'s conversation, `m`
    /// included, in source order. Empty when `m` is not in the source.
    fn source_members(&self, m: &Message) -> Vec<Message> {
        let threads = self.source_threads();
        let source = self.active_source();
        threads
            .keys
            .get(&thread_slot(m))
            .and_then(|key| threads.members.get(key))
            .map(|idx| idx.iter().filter_map(|&i| source.get(i)).map(|m| Message::clone(m)).collect())
            .unwrap_or_default()
    }

    /// The conversation to show for a selected message: when `m` is the oldest
    /// (head) of a multi-message thread, every message in it (oldest first);
    /// otherwise just `m` (so opening an individual reply shows only that one).
    fn conversation_for(&self, m: &Message) -> (Vec<Message>, bool) {
        if !self.threading {
            return (vec![m.clone()], false);
        }
        // A part of the conversation from another folder, picked out of the
        // opened-out row: shown by itself, like any reply picked out (#309).
        if self.nested.contains_key(&(m.account_id, m.id)) {
            return (vec![m.clone()], true);
        }
        // Thread within whatever set is on screen (the search pool while searching,
        // otherwise the current folder) so the conversation matches the rows shown.
        let mut members = self.source_members(m);
        if members.len() <= 1 {
            // Nothing else here to thread with. It may still have siblings in
            // another folder, so this is *not* solo — the reader may look.
            return (vec![m.clone()], false);
        }
        // Oldest first, matching the rows: the head is the message that opened
        // the conversation, and opening it shows the whole thread in order.
        members.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.uid.cmp(&b.uid)));
        // Whether `m` is the row, judged over the window the rows came from
        // (#236): a conversation whose start lies past the rendered window is
        // still one row, and that row stands for all of it.
        if heads_its_row((m.account_id, m.id), &self.msg_thread, &self.thread_members) {
            (members, false)
        } else {
            // A reply picked out of a conversation that is on screen: show it by
            // itself and leave it that way.
            (vec![m.clone()], true)
        }
    }

    /// Select row `idx` and put the keyboard focus on it.
    ///
    /// Focus matters after a removal: GTK would otherwise pick a fallback of
    /// its own, which can be the top of the list — and moving focus scrolls
    /// the viewport with it, so the list appears to jump away from where the
    /// user was working (#19). Taking focus deliberately also means the
    /// single-key shortcuts carry on from the row that is now selected.
    fn select_and_focus(&self, idx: usize) {
        if idx < self.shown.len() {
            self.shared.selection.select_item(idx as u32, false);
            self.focus_row(idx);
        }
    }

    /// Bring row `idx` into view and give it the keyboard. Scrolling with
    /// FOCUS only moves the list's own focus item; the keyboard follows it
    /// only when the list already has it.
    fn focus_row(&self, idx: usize) {
        if idx < self.shown.len() {
            self.list_view.scroll_to(idx as u32, gtk::ListScrollFlags::FOCUS, None);
            self.list_view.grab_focus();
        }
    }

    /// Where deletion advances to, once the row at `idx` is gone: the row now
    /// occupying that slot when the user was moving down the list, the row
    /// above it when they were moving up. Clamped to the list either way.
    fn advance_index(&self, idx: usize) -> usize {
        let next = if self.nav_direction < 0 { idx.saturating_sub(1) } else { idx };
        next.min(self.shown.len().saturating_sub(1))
    }

    /// Put keyboard focus on row `idx` without touching selection — the
    /// counterpart to `select_and_focus` for a row that wasn't the one being
    /// viewed. Used when a removed row held focus but wasn't the viewed
    /// message (e.g. deleted via its own row action while browsing further
    /// down the list).
    fn focus_only(&self, idx: usize) {
        self.focus_row(idx);
        self.hide_focus_ring();
    }

    /// Drop the window's focus-visible flag after a *programmatic* focus grab:
    /// the reclaimed row keeps keyboard focus (arrow keys resume from it), but
    /// no accent focus ring appears around a row the user never navigated to.
    /// The next real key press turns the ring back on, as normal.
    fn hide_focus_ring(&self) {
        if let Some(win) = self.list_view.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
            win.set_focus_visible(false);
        }
    }

    /// Re-apply the whole selection (the viewed message plus any multi-selected
    /// rows) so it persists across rebuilds — background syncs included — until
    /// the user clicks away.
    fn select_current(&self) {
        let positions: Vec<usize> = self
            .selected_ids
            .iter()
            .filter_map(|key| self.shown.iter().position(|m| (m.account_id, m.id) == *key))
            .collect();
        self.select_positions(&positions);
    }

    /// Update the display-wide CSS that rings each account's avatar with its
    /// color (used in the unified "All Inboxes" view to identify the account).
    fn refresh_tint_css(&self) {
        // In account order, so the same colors read as the same rules.
        let colors: std::collections::BTreeMap<_, _> = self.account_colors.iter().collect();
        let mut css = String::new();
        for (id, color) in colors {
            css.push_str(&format!(
                ".vireo-acct-ring-{0} {{ border-radius: 9999px; box-shadow: 0 0 0 3px {1}; }}\n",
                id, color
            ));
        }
        self.color_provider.load(css);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        column_sort, compute_thread_keys, correspondents, heads_its_row, message_cmp, SortOrder, latest_elsewhere, nested_members, reader_conversation,
        row_for_reader_key, thread_slot, unasked_threads,
    };
    use crate::ui::message_row::{row_edits, swap_wanted, swipe_progress_px, RowEdit, SWIPE_ARM, SWIPE_MAX};
    use crate::models::Message;

    /// Apply `row_edits` to `old` the way the model applies them, then add
    /// the rest of `new`, and return the result alongside the number of rows
    /// put in or taken out.
    fn apply_edits(old: &[u32], new: &[u32]) -> (Vec<u32>, usize) {
        let (edits, tail) = row_edits(old, new);
        let mut rows = old.to_vec();
        let mut pos = 0;
        let mut cost = 0;
        for e in edits {
            match e {
                RowEdit::Keep => pos += 1,
                RowEdit::Remove => {
                    rows.remove(pos);
                    cost += 1;
                }
                RowEdit::Insert(i) => {
                    rows.insert(pos, new[i]);
                    pos += 1;
                    cost += 1;
                }
            }
        }
        cost += rows.len() - pos;
        rows.truncate(pos);
        rows.extend(&new[tail..]);
        (rows, cost)
    }

    /// #323: a rebuild keeps the rows that still show the same message and
    /// touches only what changed, so a long list is not built again for new
    /// mail, a lifted conversation or more of the folder.
    #[test]
    fn rebuilds_touch_only_the_rows_that_changed() {
        let old: Vec<u32> = (0..1000).collect();
        // More of the folder: nothing to touch, the rest joins after.
        let grown: Vec<u32> = (0..1500).collect();
        assert_eq!(apply_edits(&old, &grown), (grown.clone(), 0));
        // Two new messages at the top.
        let mut fresh = vec![5000, 5001];
        fresh.extend(0..1000);
        assert_eq!(apply_edits(&old, &fresh), (fresh.clone(), 2));
        // A reply lifts row 700's conversation to the top.
        let mut lifted = vec![700];
        lifted.extend((0..1000).filter(|&n| n != 700));
        assert_eq!(apply_edits(&old, &lifted), (lifted.clone(), 2));
        // Mail deleted elsewhere, from the middle.
        let gone: Vec<u32> = (0..1000).filter(|n| ![10, 11, 500].contains(n)).collect();
        assert_eq!(apply_edits(&old, &gone), (gone.clone(), 3));
        // The same rows: nothing to put in or take out.
        assert_eq!(apply_edits(&old, &old), (old.clone(), 0));
        // A different folder shares nothing, so everything goes.
        let other: Vec<u32> = (2000..2010).collect();
        let (rows, cost) = apply_edits(&old, &other);
        assert_eq!(rows, other);
        assert!(cost >= 1000);
    }

    /// #401: a list that is mostly another goes to the view as one swap, so
    /// it keeps its row widgets; small changes still go row by row.
    #[test]
    fn another_list_is_swapped_in_whole() {
        let swaps = |old: &[u32], new: &[u32]| {
            let (edits, tail) = row_edits(old, new);
            swap_wanted(&edits, old.len(), tail, new.len())
        };
        let old: Vec<u32> = (0..1000).collect();
        let mut fresh = vec![5000, 5001];
        fresh.extend(0..1000);
        assert!(!swaps(&old, &fresh));
        let mut lifted = vec![700];
        lifted.extend((0..1000).filter(|&n| n != 700));
        assert!(!swaps(&old, &lifted));
        let gone: Vec<u32> = (0..1000).filter(|n| ![10, 11, 500].contains(n)).collect();
        assert!(!swaps(&old, &gone));
        assert!(!swaps(&old, &old));
        assert!(!swaps(&old, &(0..1500).collect::<Vec<_>>()));
        assert!(!swaps(&[], &old));
        assert!(!swaps(&old, &[]));
        // Another account's folder.
        assert!(swaps(&old, &(2000..3000).collect::<Vec<_>>()));
        // All Inboxes to one of its accounts: every other row goes.
        let one: Vec<u32> = (0..1000).filter(|n| n % 2 == 0).collect();
        assert!(swaps(&old, &one));
    }

    /// #236: the row a conversation collapses to is judged over the rendered
    /// window's grouping, so a conversation whose oldest message lies past
    /// the window still opens whole from its row.
    #[test]
    fn a_row_heads_its_conversation_within_the_rendered_window() {
        use std::collections::HashMap;
        let key = (1u32, "root@x".to_string());
        // The window holds uids 30 and 40 of the conversation; uid 10, its
        // real start, is past the window and so in neither map.
        let mut msg_thread = HashMap::new();
        msg_thread.insert((1, 30), key.clone());
        msg_thread.insert((1, 40), key.clone());
        let mut thread_members = HashMap::new();
        thread_members.insert(key, vec![(1, 30), (1, 40)]);

        assert!(heads_its_row((1, 30), &msg_thread, &thread_members), "the oldest shown is the row");
        assert!(!heads_its_row((1, 40), &msg_thread, &thread_members), "a child row is not");
        // A message grouped with nothing in the window is its own row, and the
        // reader is still free to look for the rest of it in the cache.
        assert!(heads_its_row((1, 10), &msg_thread, &thread_members));
        assert!(heads_its_row((2, 7), &msg_thread, &thread_members));
    }

    /// #236 again, the second half: the row for a mail you have answered says
    /// so, because the cache hands down a reply this folder does not hold.
    #[test]
    fn a_row_speaks_for_the_reply_filed_in_sent() {
        use crate::models::{ThreadLatest, ThreadSummary};
        let reply = ThreadLatest {
            from_name: "Me".into(),
            from_addr: "me@example.com".into(),
            preview: "Looked, all good".into(),
            timestamp: 600,
            date: String::new(),
        };
        let summary = ThreadSummary { count: 2, latest: Some(reply.clone()), ..Default::default() };

        // The Inbox holds the message that came in at 500; the answer is later.
        assert_eq!(latest_elsewhere(Some(&summary), 500), Some(reply));
        // Once the folder itself holds something at least as new — the reply
        // is in this folder too, or a newer mail has arrived — the row keeps
        // describing what it can see.
        assert_eq!(latest_elsewhere(Some(&summary), 600), None);
        assert_eq!(latest_elsewhere(Some(&summary), 900), None);
        // A conversation the cache has said nothing about, or nothing but a
        // size, leaves the row alone.
        assert_eq!(latest_elsewhere(None, 500), None);
        assert_eq!(
            latest_elsewhere(Some(&ThreadSummary { count: 3, ..Default::default() }), 500),
            None
        );
    }

    fn msg(id: u32, message_id: &str, references: &str) -> Message {
        Message {
            id,
            account_id: 1,
            folder_id: 1,
            uid: id,
            from_name: "X".into(),
            from_addr: "x@example.com".into(),
            reply_to: String::new(),
            to: String::new(),
            cc: String::new(),
            subject: "S".into(),
            preview: String::new(),
            body: String::new(),
            date: String::new(),
            timestamp: 1000,
            unread: false,
            starred: false,
            keywords: Vec::new(),
            has_attachment: false,
            message_id: message_id.into(),
            references: references.into(),
            importance: Default::default(),
            due: 0,
        }
    }

    /// #334: every sort keeps its key, a heading sorts by its column (the
    /// recipients in Sent), and due dates sort soonest first, undated last.
    #[test]
    fn headings_sort_by_their_columns() {
        use crate::config::ListColumn;
        for c in ListColumn::ALL {
            if let Some(order) = column_sort(c, false) {
                assert_eq!(SortOrder::from_key(order.key()), order);
            }
        }
        assert_eq!(column_sort(ListColumn::Sender, true), Some(SortOrder::Recipients));
        assert_eq!(column_sort(ListColumn::Tags, false), None);
        let due = |id, due| Message { due, ..msg(id, "", "") };
        let mut list = vec![due(1, 0), due(2, 300), due(3, 100)];
        list.sort_by(|a, b| message_cmp(a, b, SortOrder::Due));
        assert_eq!(list.iter().map(|m| m.id).collect::<Vec<_>>(), vec![3, 2, 1]);
    }

    /// #334: who wrote, oldest first and each once; who it went to when
    /// nobody else has written.
    #[test]
    fn correspondents_name_each_writer_once() {
        let from = |id, name: &str, addr: &str, ts| Message {
            from_name: name.into(),
            from_addr: addr.into(),
            timestamp: ts,
            to: "Zoe <z@example.com>".into(),
            ..msg(id, "", "")
        };
        let (a, b, a2) = (from(1, "Ann", "a@example.com", 10), from(2, "", "b@example.com", 20), from(3, "Ann", "A@example.com", 30));
        assert_eq!(correspondents(&[&a2, &b, &a]), "Ann, b@example.com");
        assert_eq!(correspondents(&[&a]), "Ann");
        assert_eq!(correspondents(&[]), "");
    }

    /// #309: a conversation opens out into its parts in other folders, but
    /// not into what the list already shows, a second label's copy of the
    /// same mail, or what the list's filters leave out.
    #[test]
    fn a_row_nests_only_the_conversation_filed_elsewhere() {
        let in_folder = |id, mid: &str, folder| Message { folder_id: folder, ..msg(id, mid, "") };
        let own = vec![in_folder(1, "a@x", 1)];
        let found = vec![
            in_folder(1, "a@x", 1),          // the inbox message itself
            in_folder(90, "a@x", 9),         // its copy under All Mail
            in_folder(91, "b@x", 3),         // your reply in Sent
            in_folder(92, "b@x", 9),         // the reply's All Mail copy
            in_folder(93, "c@x", 2),         // in another listed folder
            Message { unread: true, ..in_folder(94, "d@x", 5) },
            in_folder(95, "", 5),            // no Message-ID to tell copies by
        ];
        let listed = [(1u32, 1u32), (1, 2)].into_iter().collect();
        let ids = |v: Vec<Message>| v.iter().map(|m| m.id).collect::<Vec<_>>();
        assert_eq!(ids(nested_members(&found, &own, &listed, &|_| true)), vec![91, 94]);
        assert_eq!(ids(nested_members(&found, &own, &listed, &|m| m.unread)), vec![94]);
    }

    /// Two replies in an Inbox each answer a different message in Sent, and
    /// reference nothing else — so within the Inbox they share no id at all.
    /// They are one conversation, and the messages that say so are the ones in
    /// Sent, which the folder on screen never shows.
    #[test]
    fn a_conversation_joined_through_another_folder_still_groups() {
        let shown = [
            msg(1, "reply-a@them", "sent-1@us"),
            msg(2, "reply-b@them", "sent-2@us"),
        ];
        // Without the Sent messages there is nothing to join them.
        let alone = compute_thread_keys(&shown, &[]);
        assert_ne!(
            alone.get(&(1, 1, 1)),
            alone.get(&(1, 1, 2)),
            "nothing on screen links these two"
        );

        // Sent 2 replied to reply-a, which replied to Sent 1: one conversation.
        let links = vec![
            (1u32, "sent-1@us".to_string(), String::new()),
            (1u32, "sent-2@us".to_string(), "sent-1@us reply-a@them".to_string()),
        ];
        let joined = compute_thread_keys(&shown, &links);
        assert_eq!(
            joined.get(&(1, 1, 1)),
            joined.get(&(1, 1, 2)),
            "the messages in Sent say they belong together"
        );
    }

    /// A re-added account re-downloads its whole mailbox, so every message in it
    /// is older than the moment the account was added. Threading reads the reply
    /// headers, which say the same thing whenever the mail was sent — the three
    /// messages here are the shape iCloud delivered: a root with no References,
    /// and two replies naming it.
    #[test]
    fn mail_older_than_the_account_still_threads() {
        let old = 1_787_565_140i64; // long before this list was ever built
        let mut root = msg(1, "root@dccma.com", "");
        root.timestamp = old;
        let mut first = msg(2, "r1@dccma.com", "root@dccma.com sent-1@me.com");
        first.timestamp = old + 48;
        let mut second = msg(3, "r2@dccma.com", "root@dccma.com sent-2@me.com");
        second.timestamp = old + 224;

        let shown = [root, first, second];
        let keys = compute_thread_keys(&shown, &[]);
        let root_key = keys.get(&(1, 1, 1)).cloned().expect("the root is threaded");
        assert_eq!(keys.get(&(1, 1, 2)), Some(&root_key), "first reply joins");
        assert_eq!(keys.get(&(1, 1, 3)), Some(&root_key), "second reply joins");
    }

    fn listed(items: &[(u32, &str)]) -> Vec<(u32, String, Vec<String>)> {
        items.iter().map(|(a, r)| (*a, r.to_string(), vec![format!("{r}@x")])).collect()
    }

    /// The counts come back as a rebuild, and a rebuild is what decides to ask.
    /// If asking were driven by the page alone, that would be a loop; it is
    /// driven by what has not been asked yet, so the second pass is silent.
    #[test]
    fn a_page_is_only_asked_about_once() {
        let page = listed(&[(1, "a"), (1, "b")]);
        let mut asked = std::collections::HashSet::new();

        let first = unasked_threads(&page, &asked);
        assert_eq!(first.len(), 2, "nothing counted yet, so ask about both");
        for (aid, root, _) in &first {
            asked.insert((*aid, root.clone()));
        }
        assert!(unasked_threads(&page, &asked).is_empty(), "the answer must not start another round");
    }

    /// A search re-filters the page on every keystroke, and each rebuild would
    /// otherwise be a fresh scan of the message index.
    #[test]
    fn narrowing_the_page_asks_nothing_further() {
        let page = listed(&[(1, "a"), (1, "b"), (1, "c")]);
        let asked: std::collections::HashSet<(u32, String)> =
            page.iter().map(|(a, r, _)| (*a, r.clone())).collect();

        let narrowed = listed(&[(1, "b")]);
        assert!(unasked_threads(&narrowed, &asked).is_empty());
    }

    /// New mail brings threads nobody has counted; only those are asked about.
    #[test]
    fn only_the_new_conversations_are_asked_about() {
        let asked: std::collections::HashSet<(u32, String)> =
            [(1u32, "a".to_string())].into_iter().collect();
        let page = listed(&[(1, "a"), (1, "new")]);

        let fresh = unasked_threads(&page, &asked);
        assert_eq!(fresh.iter().map(|(_, r, _)| r.as_str()).collect::<Vec<_>>(), vec!["new"]);
    }

    /// Two accounts can root a thread at the same Message-ID (the unified
    /// inbox shows both), and they are different conversations in different
    /// caches — asking about one must not silence the other.
    #[test]
    fn the_same_thread_root_in_two_accounts_is_two_questions() {
        let asked: std::collections::HashSet<(u32, String)> =
            [(1u32, "shared".to_string())].into_iter().collect();
        let page = listed(&[(1, "shared"), (2, "shared")]);

        let fresh = unasked_threads(&page, &asked);
        assert_eq!(fresh.iter().map(|(a, _, _)| *a).collect::<Vec<_>>(), vec![2]);
    }

    /// A search over every folder holds the same UID once per folder. Those
    /// are different messages, and each keeps its own conversation (#317).
    #[test]
    fn the_same_uid_in_two_folders_stays_two_messages() {
        let root = msg(5, "root@x", "");
        let reply = msg(6, "reply@x", "root@x");
        let mut other = msg(6, "other@y", "");
        other.folder_id = 2;
        let mut bare_here = msg(9, "", "");
        bare_here.folder_id = 1;
        let mut bare_there = msg(9, "", "");
        bare_there.folder_id = 2;
        let pool = [root, reply, other.clone(), bare_here, bare_there];
        let keys = compute_thread_keys(&pool, &[]);
        let conversation = keys.get(&(1, 1, 5)).cloned().expect("threaded");
        let members: Vec<(u32, u32)> = pool
            .iter()
            .filter(|m| keys.get(&thread_slot(m)) == Some(&conversation))
            .map(|m| (m.folder_id, m.uid))
            .collect();
        assert_eq!(members, vec![(1, 5), (1, 6)], "only the root and its reply");
        assert_ne!(keys.get(&thread_slot(&other)), Some(&conversation));
        assert_ne!(
            keys.get(&(1, 1, 9)),
            keys.get(&(1, 2, 9)),
            "no Message-ID and one UID in two folders: still two messages"
        );
    }

    /// Links are evidence, not glue: unrelated mail must not be pulled in.
    #[test]
    fn links_do_not_merge_unrelated_conversations() {
        let shown = [msg(1, "a@x", ""), msg(2, "b@x", "")];
        let links = vec![(1u32, "c@x".to_string(), "a@x".to_string())];
        let keys = compute_thread_keys(&shown, &links);
        assert_ne!(keys.get(&(1, 1, 1)), keys.get(&(1, 1, 2)), "still two conversations");
    }

    /// A long conversation groups whole. What it may drag in from *other*
    /// folders is bounded by the cache's own per-thread limit; what is already
    /// in the folder on screen is shown in full.
    #[test]
    fn a_long_conversation_groups_every_message() {
        let n = 60usize;
        let mut members: Vec<Message> = Vec::new();
        for i in 0..n {
            let mut m = msg(i as u32 + 1, &format!("m{i}@x"), "root@x");
            m.timestamp = 1000 + i as i64;
            members.push(m);
        }
        // All one conversation by their shared reference.
        let keys = compute_thread_keys(&members, &[]);
        let root = keys.get(&(1, 1, 1)).cloned().expect("threaded");
        assert!(
            members.iter().all(|m| keys.get(&(1, 1, m.id)) == Some(&root)),
            "one conversation"
        );
        assert_eq!(
            members.iter().filter(|m| keys.get(&(1, 1, m.id)) == Some(&root)).count(),
            n,
            "every message belongs to it, however long the thread runs"
        );
    }

    /// libadwaita's own scale for a touchpad's two-finger scroll: it spends a
    /// fixed 400px of horizontal delta on a full swipe, whatever the widget's
    /// `distance` says, which is why the preference exists at all.
    const TOUCHPAD_BASE: f64 = 400.0;

    #[test]
    fn mouse_drag_tracks_the_pointer_at_every_sensitivity() {
        // `AdwSwipeTracker` divides a drag by `SwipeSurface::distance`, which
        // is SWIPE_MAX * sensitivity, so the row must come back out at the
        // pointer's own px however the preference is set.
        for sensitivity in [1.0, 3.5, 10.0] {
            for dragged in [10.0, 72.0, 120.0] {
                let progress = dragged / (SWIPE_MAX * sensitivity);
                assert!(
                    (swipe_progress_px(progress, sensitivity) - dragged).abs() < 0.001,
                    "{dragged}px drag at {sensitivity}"
                );
            }
        }
    }

    #[test]
    fn sensitivity_shortens_the_trackpad_swipe() {
        // How much two-finger scroll it takes to reach the commit distance.
        let travel = |sensitivity: f64| {
            (1..=2000)
                .map(|px| px as f64)
                .find(|px| {
                    swipe_progress_px(px / TOUCHPAD_BASE, sensitivity).abs() >= SWIPE_ARM
                })
                .expect("armed eventually")
        };
        // Untuned, a trackpad has to travel further than most can in one go —
        // the complaint behind the setting.
        assert_eq!(travel(1.0), 240.0);
        // The default puts it within a comfortable swipe, and raising it
        // further keeps shortening it.
        assert!(travel(3.5) < 70.0, "default is a short swipe");
        assert!(travel(10.0) < travel(3.5), "higher is always shorter");
    }

    #[test]
    fn a_long_swipe_stops_at_the_action_strip() {
        for sensitivity in [1.0, 10.0] {
            assert_eq!(swipe_progress_px(1.0, sensitivity), SWIPE_MAX);
            assert_eq!(swipe_progress_px(-1.0, sensitivity), -SWIPE_MAX);
        }
    }

    #[test]
    fn preview_lines_clamp_but_keep_zero() {
        // 0 is "off"; anything above 3 is a hand-edited file, not a setting.
        for (asked, expected) in [(0u32, 0u32), (1, 1), (3, 3), (9, 3)] {
            assert_eq!(asked.min(3), expected, "for {asked}");
        }
    }

    /// Clicking a reply's card keeps the conversation's row selected even when
    /// the list never shows that reply a row of its own (#211).
    #[test]
    fn a_hidden_reply_selects_its_conversation_row() {
        use std::collections::HashMap;
        // The list shows the head of a three-message conversation, and one
        // unrelated message below it.
        let shown = [msg(1, "head@them", ""), msg(9, "other@them", "")];
        let thread = (1u32, "head@them".to_string());
        let msg_thread: HashMap<(u32, u32), (u32, String)> = [
            ((1, 1), thread.clone()),
            ((1, 2), thread.clone()),
            ((1, 3), thread.clone()),
        ]
        .into_iter()
        .collect();

        // The conversation as this list handed it over, opened from its head
        // row. A message of the user's own, pulled in from Sent, joins it
        // only later and only on the app's side: this folder has no row for
        // it, no thread entry, and never listed it at all (#220).
        let emitted = [(1, 1), (1, 2), (1, 3)];
        let merged = [(1, 1), (1, 2), (1, 3), (1, 77)];
        let conversation = reader_conversation(&emitted, &merged);
        assert_eq!(conversation, [(1, 1), (1, 2), (1, 3), (1, 77)]);
        let viewed = Some((1, 1));
        let row = |key: (u32, u32)| {
            row_for_reader_key(&key, &shown, &msg_thread, &conversation, viewed)
        };

        // The head has a row of its own.
        assert_eq!(row((1, 1)), Some(0));
        // Its replies do not, and land on the head's row rather than nowhere.
        assert_eq!(row((1, 2)), Some(0));
        assert_eq!(row((1, 3)), Some(0));
        // Neither does the copy from Sent, which this folder never lists.
        assert_eq!(row((1, 77)), Some(0));
        // Going by what the list handed over alone, that copy would land
        // nowhere — the bug behind #220.
        assert_eq!(row_for_reader_key(&(1, 77), &shown, &msg_thread, &emitted, viewed), None);
        // A message in no conversation still matches only itself.
        assert_eq!(row((1, 9)), Some(1));
        // And one from neither is no row at all.
        assert_eq!(row((1, 42)), None);
    }
}
