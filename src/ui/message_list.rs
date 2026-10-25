//! Middle pane: the scrollable list of messages in the selected folder,
//! with a search field and live filtering.

use self::layout::BarSlots;
use self::search::{ScopePicker, SearchHits, SearchText};
use self::threading::SourceThreads;
use self::views::ViewSnapshot;

use std::rc::Rc;

use relm4::prelude::*;

use crate::models::{Message, ThreadSummary};
pub(crate) use crate::ui::message_row::{tag_menu_entries, RowAction};
use crate::ui::message_row::{RowData, RowShared};

mod component;
mod conversations;
mod filters;
mod init;
mod layout;
mod menus;
mod nav;
mod on_look;
mod on_paging;
mod on_remove;
mod on_rows;
mod on_search;
mod on_selection;
mod on_threads;
mod rows;
mod search;
mod sort;
mod threading;
mod types;
mod views;

pub(crate) use filters::{CheckFilter, QuickFilter, QuickFilters};
pub(crate) use layout::after_next_paint;
pub(crate) use search::search_text_of;
pub(crate) use search::{PoolFolder, SearchScope};
pub(crate) use sort::SortOrder;
pub(crate) use types::{BulkAction, MessageListInput, MessageListOutput};
pub(crate) use views::{KEEP_AT_MOST, KEPT_VIEWS};

pub(crate) struct MessageList {
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
    /// The view on show, as the app named it (`SetView`), and its content's
    /// fingerprint: a unified view is kept as built when left, and shown
    /// again from that if nothing in it changed.
    view_key: Option<u64>,
    view_fp: u64,
    views: std::collections::VecDeque<ViewSnapshot>,
    /// A kept view to show at the next queued rebuild, in place of one.
    pending_restore: Option<ViewSnapshot>,
    /// A view being built ahead (`Prebuild`) while its conversations are
    /// worked out off the main thread: its number, key, fingerprint and
    /// messages.
    prebuild: Option<(u64, u64, u64, Vec<Rc<Message>>)>,
    prebuild_gen: u64,
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
    /// The pool's folders, by `(account_id, folder_id)`.
    pool_folders: Rc<std::collections::HashMap<(u32, u32), PoolFolder>>,
    /// What a search looks in, for the folder (`[0]`) and for the pool
    /// (`[1]`), made once for a search rather than at every keystroke.
    search_text: std::cell::RefCell<[Option<SearchText>; 2]>,
    /// The newest search sent to another thread. One still running for an
    /// older query sees it has been overtaken and stops (#515).
    search_gen: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The answer of the last search that ran on another thread.
    search_hits: Option<SearchHits>,
    /// Which messages the search field filters over.
    scope: SearchScope,
    /// The search scope's button and popover.
    scope_picker: Option<ScopePicker>,
    /// The search field widget, kept so a folder switch can clear its text.
    search_entry: Option<gtk::SearchEntry>,
    /// The search toolbar is hidden until asked for (#102).
    search_open: bool,
    /// When the search closed itself (empty entry losing focus): the button
    /// click that caused that blur arrives right after and must not reopen.
    search_closed_at: Option<std::time::Instant>,
    /// The search toolbar never hides (#446): closing only clears it.
    search_pinned: bool,
    /// The search field and the bulk bar open under the rows instead of
    /// above them (#496).
    search_bottom: bool,
    bulk_bottom: bool,
    /// The two bars and the boxes above and below the rows they move
    /// between.
    bars: Option<BarSlots>,
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
    /// Every address you send from, lower case: with `thread_row_newest`
    /// off, the mail you sent is not nested under a conversation's row
    /// either (#388).
    own_addresses: std::collections::HashSet<String>,
    /// Gmail's keys are on (#392): Enter opens a row in the reader.
    gmail_keys: bool,
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
    /// Gmail's x is checking rows for an action rather than opening them
    /// (#392): a selection of one stays closed too, and the reader says how
    /// many are selected. Opening a row, or a plain click, ends it.
    checking: bool,
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
    /// Whether the menus offer Snooze (#527), and Unsnooze (`Some(true)`).
    snooze_offer: Option<bool>,
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
    /// The header's quick filters (#97, #441).
    quick: QuickFilters,
    /// Your contacts' addresses, lower case, while "From Contacts" is on.
    contact_addresses: Option<std::sync::Arc<std::collections::HashSet<String>>>,
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
    /// Headings divide a list sorted by date into Today, Yesterday and so
    /// on (#435), and whether the view draws them now: only a date sort
    /// has them.
    group_by_date: bool,
    day_headings: bool,
    /// A card's pills on a line under its text (#374).
    pills_below: bool,
    /// Show the column headings over a single-line list (#334).
    headings: bool,
    headings_bar: gtk::Box,
    /// The widths columns were dragged to by their headings (#334).
    widths: std::collections::HashMap<crate::config::ListColumn, i32>,
    /// Columns keep their widths and a list too narrow for them scrolls
    /// sideways; off, the name columns narrow to fit the pane (#443).
    column_scroll: bool,
    /// How wide the rows are, for fitting the columns into it.
    pane_width: i32,
    /// The headings' sized cells, which a drag resizes in place.
    heading_bins: std::cell::RefCell<
        std::collections::HashMap<crate::config::ListColumn, crate::ui::column_bin::ColumnBin>,
    >,
}

#[cfg(test)]
mod characterization_tests;

#[cfg(test)]
mod tests;
