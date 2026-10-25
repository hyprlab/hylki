//! The list's messages in and out, and the bulk actions.

use super::filters::{CheckFilter, QuickFilters};
use super::search::{PoolFolder, SearchScope};
use super::sort::SortOrder;
use super::threading::{thread_slot, SourceThreads};
use super::RowAction;
use crate::models::{Message, ThreadSummary};
use crate::ui::message_row::Slot;
use adw::prelude::*;
use relm4::prelude::*;

/// A bulk action applied to every selected message at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BulkAction {
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
    /// Erase from the server, skipping Trash (#471). The app asks first.
    DeleteForever,
    /// Back to the Inbox, for a selection in Trash or Junk (#138).
    MoveToInbox,
    /// Into the Snoozed folder until a time the app holds (#527).
    Snooze,
    /// Out of the Snoozed folder now.
    Unsnooze,
}

#[derive(Debug)]
pub(crate) enum MessageListInput {
    /// The header's quick filters (#97, #441), with your contacts'
    /// addresses when "From Contacts" is among them.
    SetQuickFilters {
        filters: QuickFilters,
        contacts: Option<std::sync::Arc<std::collections::HashSet<String>>>,
    },
    SetMessages {
        messages: Vec<Message>,
    },
    /// The view now on show (`None` for any other), and its content's
    /// fingerprint. With `restore`, the list shows the view as kept when it
    /// was last left (the app checked with `has_view`) and no messages
    /// follow.
    SetView {
        key: Option<u64>,
        fp: u64,
        restore: bool,
    },
    /// The fingerprint of the content about to be set for the view on show.
    SetViewFp(u64),
    /// Build a view that is not on show and keep it, so its first visit
    /// is shown as kept: the unified rows, once a start has read their
    /// folders.
    Prebuild {
        key: u64,
        fp: u64,
        messages: Vec<Message>,
    },
    /// The conversations of the view being built ahead.
    PrebuildThreads {
        gen: u64,
        threads: Box<SourceThreads>,
    },
    /// Merge more indexed messages into the current list (background backfill),
    /// preserving the current search query and view.
    AppendMessages {
        messages: Vec<Message>,
    },
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
    ConversationSize {
        account_id: u32,
        id: u32,
        size: usize,
    },
    /// Whether a conversation's row speaks for the newest message anywhere in
    /// the account, the replies you sent included (#236).
    SetThreadRowNewest(bool),
    /// Gmail's keys (#392): Enter on a row the cursor is on opens it in the
    /// reader rather than a window of its own.
    SetGmailKeys(bool),
    /// Move the keyboard cursor `delta` rows without selecting or opening
    /// anything (Gmail's j/k, #392).
    MoveCursor(i32),
    /// Open the row the cursor is on in the reader (Gmail's o, #392).
    OpenCursor,
    /// A click in the list: the keyboard cursor's outline goes, since the
    /// pointer, not j or k, says where the user is now. A plain click also
    /// ends Gmail's checking; `extend` is a Ctrl or Shift click, which adds
    /// to it.
    PointerPressed {
        extend: bool,
    },
    /// File the selected rows into `dest`, a conversation's row with all
    /// of it (the picker's answer for a selection, #392).
    MoveSelectedTo {
        account_id: u32,
        dest: String,
    },
    /// Close the open conversation and leave the keyboard on its row
    /// (Gmail's u and Esc, and after Shift+U or _, #392).
    CloseOpen,
    /// Open the next (or previous) conversation, from the one open, while
    /// the keyboard stays in the reader (Gmail's j/k there, #392).
    StepConversation(i32),
    /// Select every row shown (Gmail's * a).
    SelectAllShown,
    /// Select the conversations shown that match, and only those (Gmail's
    /// * r, * u, * s and * t).
    SelectWhere(CheckFilter),
    /// Which way to go on when the open row leaves the list: down (1) or
    /// up (-1). Gmail's ] and [ archive and go on that way (#392).
    SetNavDirection(i32),
    /// The addresses you send from, for leaving your own replies out of a
    /// conversation's nested rows (#388).
    SetOwnAddresses(std::collections::HashSet<String>),
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
    SetLook {
        avatars: bool,
        preview_lines: u32,
        subject: bool,
        animate: bool,
    },
    /// The Focus Mode slide finished: rebuild the rows as they now are.
    LookSettled,
    /// Lay the rows out on one line, or as cards (#334).
    SetSingleLine(bool),
    /// The single-line columns, in order (#334).
    SetColumns(Vec<crate::config::ListColumn>),
    /// Show the column headings over a single-line list, or not (#334).
    SetHeadings(bool),
    /// Divide a list sorted by date under Today, Yesterday and so on (#435).
    SetGroupByDate(bool),
    /// Put a card's tags and labels on a line under its text (#374).
    SetPillsBelow(bool),
    /// A column's heading was clicked: sort by it, or the other way (#334).
    SortByColumn(crate::config::ListColumn),
    /// A heading's edge was dragged to `width`, or double-clicked (`None`:
    /// the column's own width). `done` once the drag is over (#334).
    ResizeColumn {
        column: crate::config::ListColumn,
        width: Option<i32>,
        done: bool,
    },
    /// The saved column widths (#334).
    SetColumnWidths(std::collections::HashMap<crate::config::ListColumn, i32>),
    /// Fill the headings again, once a handler that set them going is over.
    RefreshHeadings,
    /// Scroll a single line wider than the pane, or fit its columns (#443).
    SetColumnScroll(bool),
    /// Keep the search toolbar open (#446).
    SetSearchPinned(bool),
    /// Where the search field and the bulk bar open: under the rows when
    /// true, above them when false (#496).
    SetBarsPlacement {
        search_bottom: bool,
        bulk_bottom: bool,
    },
    /// The rows are this wide now.
    PaneWidth(i32),
    /// Each account's name for the Account column, and which accounts are
    /// Microsoft 365 ones, for the Due column (#334).
    SetAccountNames {
        names: std::collections::HashMap<u32, String>,
        graph: std::collections::HashSet<u32>,
    },
    /// When each snoozed message comes back (#527): its row in the Snoozed
    /// folder shows that in place of its date.
    SetSnoozed(crate::ui::message_row::SnoozedRows),
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
    /// the app when a search begins; cleared to empty when it ends. The
    /// pool's [`search_text_of`], worked out where the pool was put together,
    /// comes with it.
    SetSearchPool(
        Vec<Message>,
        std::collections::HashMap<(u32, u32), PoolFolder>,
        Vec<Box<str>>,
    ),
    /// What a search matched on another thread ([`MessageList::start_search`]).
    SearchMatched {
        gen: u64,
        query: String,
        text: std::sync::Arc<Vec<Box<str>>>,
        hits: Vec<bool>,
    },
    /// The reader's selection: mirror whichever of these the list has rows for,
    /// so everything that acts on the list's selection acts on the messages the
    /// user pointed at. The reader is already showing them, so this must not
    /// re-open anything. Messages the list cannot represent — a reply read in
    /// from Sent, which belongs to another folder — keep the row of the
    /// conversation they were read in with (#211, #220). `conversation` is that
    /// conversation as the reader has it, which is wider than what this list
    /// handed over: the app pulls the user's own replies in from Sent after
    /// the fact, under ids the list never sees.
    SelectFromReader {
        keys: Vec<(u32, u32)>,
        conversation: Vec<(u32, u32)>,
    },
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
    /// Showcase captures (#374): open the search, pick the scope by its
    /// place in the dropdown and type the text.
    ShowcaseSearch(String, u32),
    /// Close and clear the search toolbar (#102): Esc, empty focus-out, or
    /// the header button while open.
    CloseSearch,
    /// Showcase staging: open row N's actions palette (screenshot hook only —
    /// see HYLKI_SHOWCASE_PALETTE in app/showcase/list.rs).
    DebugOpenPalette(usize),
    /// Showcase only (HYLKI_SHOWCASE_SWIPE): drive row `index` through a full
    /// swipe and release, so the commit exit can be caught in stills — there
    /// is no way to inject a real gesture on this desktop.
    DebugSwipe {
        index: usize,
        left: bool,
    },
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
    SetRead {
        slot: (u32, u32, u32),
        read: bool,
    },
    /// A hover-palette action for a specific message (forwarded to the app).
    RowAction {
        action: RowAction,
        message: Box<Message>,
    },
    SetStarred {
        slot: (u32, u32, u32),
        starred: bool,
    },
    /// A message was replied to or forwarded (#454), or the send that
    /// marked it ahead of time failed (`on` false).
    SetAnswered {
        slot: (u32, u32, u32),
        forwarded: bool,
        on: bool,
    },
    /// A message's keywords and Gmail labels changed (a tag put on or
    /// taken off, #374).
    SetKeywords {
        slot: (u32, u32, u32),
        keywords: Vec<String>,
        labels: Vec<String>,
    },
    /// The tag definitions changed: rows rebuild their chips.
    SetTags(Vec<crate::config::Tag>),
    /// A row's tag menu toggled a tag — passed up to the app.
    SetTagFor {
        message: Box<Message>,
        keyword: String,
        add: bool,
    },
    /// A message's paperclip, found by where it lives: its own folder and
    /// UID, since a UID is unique only within its folder.
    SetHasAttachment {
        account_id: u32,
        folder_id: u32,
        uid: u32,
        has: bool,
    },
    Remove(u32),
    /// `gone` left the open conversation, which stays open: its row stays,
    /// now headed by `head`.
    LeaveConversation {
        gone: (u32, u32),
        head: (u32, u32),
    },
    /// Remove many messages in a single batch (bulk archive/delete/spam), so the
    /// list updates in one render pass instead of one per message.
    RemoveMany(Vec<u32>),
    /// Secondary-click on the row showing `key`, at (x, y) in the list's
    /// coordinates: open the context menu.
    ContextMenu {
        x: f64,
        y: f64,
        key: (u32, u32),
    },
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
    /// Whether the menus offer Snooze (#527): `None` where nothing can be
    /// snoozed, `Some(true)` in the Snoozed folder, which offers Unsnooze.
    SetSnoozeOffer(Option<bool>),
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
    ThreadsReady {
        gen: u64,
        threads: Box<SourceThreads>,
    },
    /// Mark which message is being viewed so it stays highlighted across
    /// rebuilds; `None` clears the selection (e.g. on folder switch).
    SetSelected(Option<u32>),
    /// Select a message by `(account_id, id)` AND load it in the reader — used to
    /// advance after the viewed message is removed by a background sync.
    SelectAndLoad((u32, u32)),
    /// A row palette's Move to…: open the picker for that row (and, for a
    /// conversation head, its thread) at window point (x, y).
    RowMoveTo {
        message: Box<Message>,
        x: f64,
        y: f64,
    },
}

#[derive(Debug)]
pub(crate) enum MessageListOutput {
    /// The view the app asked to be shown as kept was not: send its
    /// messages.
    ViewMissed,
    /// A message was opened. `thread` is the conversation to render (just
    /// `message` when it is not one). `solo` marks the deliberate case: the user
    /// picked one reply *inside* a conversation shown in the list, and wants
    /// only that message — so the reader must not go looking for its siblings.
    Selected {
        message: Message,
        thread: Vec<Message>,
        solo: bool,
    },
    /// Every selected message, whenever that changes — the reader outlines the
    /// matching cards. Each comes with its folder: a view of several folders
    /// can hold one UID twice in an account, and the app has to act on the
    /// row selected, not another folder's message (#481).
    SelectionKeys(Vec<Slot>),
    /// Gmail's o or Enter opened a row (#392); `already_open` when it was
    /// the one already in the reader, so no new selection follows.
    OpenedByKey { already_open: bool },
    /// The header-bar count changed — app/list_header.rs shows it.
    CountChanged(String),
    /// A row was double-clicked: open it in its own window. `thread` is the
    /// whole conversation when the row heads one (same shape as `Selected`),
    /// so the window shows every card — otherwise just `[message]`.
    Activated {
        message: Message,
        thread: Vec<Message>,
    },
    /// A context-menu or palette action chosen for a specific message.
    /// `conversation` holds the thread when the row stands for a collapsed
    /// conversation rather than for `message` alone, so a reply started there
    /// can answer the conversation instead of the head it is filed under
    /// (#210). Empty for an ordinary row, and for a head row shown alongside
    /// its expanded replies — there the row means only itself.
    Action {
        action: RowAction,
        message: Box<Message>,
        conversation: Vec<Message>,
    },
    /// A tag toggled on a specific message (#71).
    SetTag {
        message: Box<Message>,
        keyword: String,
        add: bool,
    },
    /// A tag toggled on every selected message from the bulk bar (#313).
    SetTagMany {
        messages: Vec<Message>,
        keyword: String,
        add: bool,
    },
    /// A bulk action chosen for every currently-selected message.
    Bulk {
        action: BulkAction,
        messages: Vec<Message>,
    },
    /// "Move To…" from a row's menu: open the folder picker for `messages`
    /// at window point (`x`, `y`). With `offer_whole`, the first message is
    /// the clicked conversation row and the rest its members: the picker
    /// offers moving them all (#171), or just the first.
    MoveTo {
        messages: Vec<Message>,
        offer_whole: bool,
        x: f64,
        y: f64,
    },
    /// A choice from the Snooze menu for these messages (#527).
    Snooze {
        messages: Vec<Message>,
        choice: crate::ui::later::SnoozeChoice,
    },
    /// The selected conversation gained a member since it was opened (a
    /// reply synced in): the head and the whole conversation as it now is.
    ThreadGrew {
        message: Message,
        thread: Vec<Message>,
    },
    /// Conversations rows have started showing and the Message-IDs each is
    /// threaded by, so the app can ask the cache how big they really are
    /// (#222). Each is asked about once.
    ThreadsListed {
        groups: Vec<(u32, String, Vec<String>)>,
    },
    /// Delete requested on a lone selected row that heads a whole conversation:
    /// every member of the thread, for the app to confirm and delete.
    DeleteThread { messages: Vec<Message> },
    /// The viewed message was removed and no row remains to advance to, so the
    /// reader should clear.
    SelectionCleared,
    /// The selected rows' messages to file into `dest` (#392).
    MoveMessages {
        account_id: u32,
        dest: String,
        messages: Vec<Message>,
    },
    /// How many rows Gmail's x has checked (#392), whenever that changes;
    /// `None` once checking ends.
    Checked(Option<usize>),
    /// A search across folders began or ended (a query in the field and a
    /// scope wider than this folder), so the app can supply or drop the
    /// cross-folder search pool.
    SearchActive(bool),
    /// A column heading changed the sort (#334), by its key: the sort
    /// menus follow.
    SortChanged(&'static str),
    /// A column was resized by its heading (#334): the widths to keep.
    ColumnWidths(std::collections::HashMap<crate::config::ListColumn, i32>),
    /// Customize Columns… from the headings' right-click menu (#443).
    CustomizeColumns,
}

/// The rows GTK has selected right now, read without a trip through the
/// component's queue: a key pressed straight after an arrow key belongs to
/// the row the arrow reached, even when the app has not heard of it yet
/// (#416).
#[derive(Clone)]
pub(crate) struct SelectionProbe {
    pub(super) selection: gtk::MultiSelection,
    pub(super) model: crate::ui::message_row::MessageModel,
}

impl SelectionProbe {
    /// Where each selected row's message sits, in list order.
    pub(crate) fn slots(&self) -> Vec<Slot> {
        let set = self.selection.selection();
        let Some((iter, first)) = gtk::BitsetIter::init_first(&set) else {
            return Vec::new();
        };
        std::iter::once(first)
            .chain(iter)
            .filter_map(|p| self.model.row(p as usize))
            .map(|r| thread_slot(&r.msg))
            .collect()
    }
}
