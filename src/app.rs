//! Root component: the application window, three-pane adaptive layout, and the
//! routing between the sidebar, list, reader, and the per-account mail workers.

use std::collections::{HashMap, HashSet};

use adw::prelude::*;
use relm4::actions::{AccelsPlus, RelmAction, RelmActionGroup};
use relm4::prelude::*;
use tokio::sync::mpsc::UnboundedSender;

/// The contributors and translators shown in the About window's thanks lists.
/// Both come from the repository's metafiles, read in at build time: one
/// person per line, `Display Name <github-handle>`, with a translator's
/// languages after a dash. What each person contributed is credited in
/// `docs/CREDITS.md` and the changelog, not in the files.
const CONTRIBUTORS: &str = include_str!("../data/CONTRIBUTORS");
const TRANSLATORS: &str = include_str!("../data/TRANSLATORS");

/// One of those files as (display name, GitHub handle, note) rows. Comments,
/// blank lines and any line without a handle are skipped, so a typo in the
/// file costs that one row rather than the list.
fn credits(file: &'static str) -> Vec<(&'static str, &'static str, &'static str)> {
    file.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let (name, rest) = line.split_once('<')?;
            let (handle, note) = rest.split_once('>')?;
            Some((
                name.trim(),
                handle.trim(),
                note.trim().trim_start_matches('-').trim(),
            ))
        })
        .collect()
}

// The message list's opening width now comes from config (the remembered pane
// width, #28); its floor lives with the pane in message_list.rs
// (LIST_MIN_WIDTH).

/// The narrowest the reader pane may be squeezed. The header's right-hand
/// actions collapse into the overflow menu below READER_ACTIONS_BREAKPOINT
/// (the left group — Reply … Delete — stays), so the floor only needs a
/// usable body width plus that left group. Kept modest on purpose: the window's
/// total minimum width must stay under half of a 1920px screen, or GNOME
/// refuses to tile the window to the left/right screen edge (it only offers
/// the top-edge maximize).
const READER_MIN_WIDTH: i32 = 400;

/// How long a read/unread change sent to a worker keeps overriding what the
/// server reports for its message and folder, should the worker's
/// confirmation never come (a dropped connection mid-request).
const PENDING_SEEN_MAX: std::time::Duration = std::time::Duration::from_secs(20);

/// How long a manual "Apply Filters" run (#198) waits for the folders it asked
/// for before reporting on whatever came back. A folder that is offline, or
/// that errors, would otherwise leave the run without an answer for good.
const FILTER_RUN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// The one response of the manual run's progress dialog: "Run in Background"
/// while it works, "Close" once it holds the report.
const FILTER_RUN_RESPONSE: &str = "close";

/// What the rules did in one folder of a manual run.
#[derive(Default, Clone, Copy)]
struct FolderTally {
    /// The most messages one pass over the folder was held up against. A
    /// folder answers twice (cache, then server) over the same mail, so the
    /// larger listing is the count — adding them would count it twice.
    checked: usize,
    /// Messages at least one rule matched, whether or not anything was
    /// done about it (#201): mail tagged on an earlier sync matches again
    /// and needs nothing, and a report that only counted actions called
    /// that "none of them matched". Held the same way as `checked`.
    matched: usize,
    /// Tags put on, and messages filed away. Both only ever happen once per
    /// message (a second pass sees the tag, or the pending move), so these
    /// add up across passes.
    tagged: usize,
    filed: usize,
}

/// A manual "Apply Filters" run (#198): the folders it covers, what the rules
/// have done in each so far, and the dialog watching it.
struct FilterRun {
    /// Every folder the run covers, with its tally. A folder load answers
    /// twice (what the cache holds, then what the server sends), so the scope
    /// has to outlive the cache's answer or the server's would go unfiltered
    /// outside the Inbox. Cleared by [`FILTER_RUN_TIMEOUT`].
    folders: std::collections::HashMap<(u32, u32), FolderTally>,
    /// The folders that have not been to the server and back yet.
    pending: std::collections::HashSet<(u32, u32)>,
    /// Whether the run has already said what it did.
    reported: bool,
    /// The progress dialog, while it is up. Gone once the run was sent to the
    /// background, in which case the report goes to the status bar instead.
    dialog: Option<FilterRunDialog>,
}

impl FilterRun {
    /// The run's totals so far.
    fn totals(&self) -> FolderTally {
        self.folders.values().fold(FolderTally::default(), |mut t, f| {
            t.checked += f.checked;
            t.matched += f.matched;
            t.tagged += f.tagged;
            t.filed += f.filed;
            t
        })
    }

    /// How many of the run's folders are done.
    fn done_count(&self) -> usize {
        self.folders.len() - self.pending.len()
    }
}

/// The widgets of a manual run's progress dialog, kept so its live text can be
/// rewritten as folders report and so the finish can turn it into the report.
struct FilterRunDialog {
    dialog: adw::MessageDialog,
    spinner: gtk::Spinner,
    body: gtk::Label,
}

/// Fallback threshold for folding the reader header's right-hand actions
/// into the overflow menu. Normally the threshold is *measured* at startup from the
/// real row (see the breakpoint in init) so it tracks the user's decoration
/// layout; this value only stands in if that measurement comes back empty.
const READER_ACTIONS_BREAKPOINT: f64 = 490.0;

use crate::ui::FOCUS_ANIM_MS;

const SIDEBAR_RAIL_WIDTH: f64 = 80.0;

/// Byte budgets for the in-RAM body and attachment caches (issue #106). Bodies
/// average well under 1 MB even with inline images, so 64 MiB holds hundreds of
/// recently seen messages; attachments run larger, and 128 MiB still keeps a
/// handful of big ones for instant re-preview. Everything evicted re-reads from
/// the SQLite cache.
const BODY_CACHE_BUDGET: usize = 64 << 20;
const ATTACHMENT_CACHE_BUDGET: usize = 128 << 20;

/// The reader header's action buttons and fold breakpoint, kept for
/// re-packing when the layout changes (see relayout_reader_toolbar).
/// The message list header's controls that Focus Mode folds into a ⋯ menu:
/// each sits in a revealer so it can slide away, and the toggles are kept so
/// the menu can flip them (their own handlers then do the filtering).
struct ListHeaderWidgets {
    revealers: Vec<gtk::Revealer>,
    overflow: gtk::Revealer,
    unread: gtk::ToggleButton,
    starred: gtk::ToggleButton,
    sort: gtk::gio::SimpleAction,
}

struct ReaderToolbarWidgets {
    /// Each action button inside its Focus Mode revealer.
    buttons: Vec<(config::ToolbarItem, gtk::Widget)>,
    /// The Outbox's own buttons' revealers (Edit, Send, Send all).
    outbox: Vec<gtk::Revealer>,
    spinner: gtk::Widget,
    bin: adw::BreakpointBin,
    breakpoint: adw::Breakpoint,
    /// The row's cost with every action button gone (padding, spacing…).
    base: i32,
    /// One action button's natural width.
    button_w: i32,
    /// The window controls' width (re-measured when the decoration layout
    /// changes).
    controls: std::cell::Cell<i32>,
}

/// Slide a Focus Mode revealer's content away (and fade it, through the
/// `away` class's opacity transition) or bring it back.
fn focus_reveal(r: &gtk::Revealer, shown: bool) {
    if shown {
        r.remove_css_class("away");
    } else {
        r.add_css_class("away");
    }
    r.set_reveal_child(shown);
}

relm4::new_action_group!(WindowActionGroup, "win");
relm4::new_stateless_action!(AccountsAction, WindowActionGroup, "accounts");
relm4::new_stateless_action!(PreferencesAction, WindowActionGroup, "preferences");
relm4::new_stateless_action!(AboutAction, WindowActionGroup, "about");
relm4::new_stateless_action!(ShortcutsAction, WindowActionGroup, "shortcuts");
relm4::new_stateless_action!(PrintAction, WindowActionGroup, "print");
relm4::new_stateless_action!(PrintPreviewAction, WindowActionGroup, "print-preview");
relm4::new_stateless_action!(StatusBarAction, WindowActionGroup, "status-bar");
relm4::new_stateless_action!(ConsoleAction, WindowActionGroup, "console");
relm4::new_stateless_action!(FindAction, WindowActionGroup, "find");
relm4::new_stateless_action!(UndoAction, WindowActionGroup, "undo");
relm4::new_stateless_action!(RedoAction, WindowActionGroup, "redo");

use crate::config::{self, split_identity, AccountConfig};
use crate::models::{thread_ids, Account, Attachment, Folder, FolderKind, KeywordFinding, Message};
use crate::ui::accounts::{AccountsOutput, AccountsWindow};
use crate::ui::compose::{
    Compose, ComposeAccount, ComposeInit, ComposeInput, ComposeOutput, ComposePrefill,
};
use crate::ui::message_list::{
    BulkAction, MessageList, MessageListInput, MessageListOutput, RowAction,
};
use crate::ui::attachments_gallery::{
    AttachmentsGallery, GalleryAccount, GalleryFolder, GalleryInput, GalleryOutput, GalleryRequest,
};

/// Largest cached attachment whose bytes ride along with a gallery page, so a
/// thumbnail or preview is instant. Anything bigger is fetched when opened.
const GALLERY_DATA_CAP: i64 = 6 * 1024 * 1024;
use crate::ui::contacts_page::{ContactsPage, ContactsPageInput, ContactsPageOutput};
use crate::ui::attachment_drawer::{AttachmentDrawer, AttachmentDrawerInput};
use crate::ui::message_view::{MessageView, MessageViewInput, MessageViewOutput, UnsubState};
use crate::ui::message_window::{
    MessageWindow, MessageWindowInit, MessageWindowInput, MessageWindowOutput,
};
use crate::ui::notifications::{NotificationCenter, NotifyInput, NotifyOutput};
use crate::ui::preferences::{PrefInit, PrefInput, PrefOutput, Preferences};
use crate::ui::sidebar::{
    CtxAction, SectionData, Sidebar, SidebarInit, SidebarInput, SidebarOutput, UnifiedFolderRef,
};
use crate::worker::{self, MailRequest, OutgoingMessage, WorkerEvent};
use crate::i18n::{i18n, i18n_f, i18n_noop, ni18n_f};

/// The currently selected mailbox.
#[derive(Clone)]
struct SelectedFolder {
    account_id: u32,
    folder_id: u32,
    path: String,
}

/// A standalone compose window (New Message, compose-to, edit-draft, or a
/// popped-out reply) and its component. Both refs must stay alive: the window
/// holds the content, the controller holds the component root.
struct ComposeHost {
    id: u32,
    /// Held only to keep the component (and its widget tree) alive for the
    /// window's lifetime; dropped when the host is removed.
    #[allow(dead_code)]
    controller: Controller<Compose>,
    window: adw::Window,
}

/// The reader's inline reply/forward composer. `window` is `Some` only while the
/// pane has been promoted to a floating window (else it lives in the reader's
/// drop-down revealer).
/// Which flag a recorded change touched (#200).
#[derive(Clone, Debug, PartialEq)]
enum FlagKind {
    Read,
    Star,
    Tag(String),
}

/// One message a flag change covers. Folder ids are re-checked when the
/// change is applied — they are index-based and shift as folders come and go.
#[derive(Clone, Debug, PartialEq)]
struct FlagRef {
    folder_id: u32,
    uid: u32,
    /// The list row's id, which is how the setters find every copy of it.
    id: u32,
}

/// Put `kind` to `value` on each of `items`. Only messages the original
/// action actually changed are listed, so one value covers them all.
#[derive(Clone, Debug, PartialEq)]
struct FlagChange {
    kind: FlagKind,
    value: bool,
    items: Vec<FlagRef>,
}

/// One step of the undo/redo history (#200). A step is a thing to *do*: the
/// undo stack holds the step that reverses each action, and applying a step
/// pushes its [`UndoStep::inverse`] onto the other stack. Operations with no
/// inverse — a permanent delete, an emptied folder, a sent message — are
/// never recorded.
#[derive(Clone, Debug, PartialEq)]
enum UndoStep {
    /// Take `message_ids` out of `from` and put them in `to`. Symmetric, so
    /// redoing a move is the same request with the two folders swapped.
    Move { from: String, to: String, message_ids: Vec<String> },
    /// Set flags back to what they were.
    Flags(Vec<FlagChange>),
    CreateFolder { path: String },
    /// Only ever the inverse of a [`UndoStep::CreateFolder`]: deleting a
    /// folder the user made is undoing that, not a delete of its own. A
    /// folder the user deletes is not undoable at all and never recorded.
    DeleteFolder { path: String },
    RenameFolder { from: String, to: String },
}

impl UndoStep {
    /// The step that puts this one back.
    fn inverse(&self) -> UndoStep {
        match self {
            UndoStep::Move { from, to, message_ids } => UndoStep::Move {
                from: to.clone(),
                to: from.clone(),
                message_ids: message_ids.clone(),
            },
            UndoStep::Flags(changes) => UndoStep::Flags(
                changes
                    .iter()
                    .map(|c| FlagChange { value: !c.value, ..c.clone() })
                    .collect(),
            ),
            UndoStep::CreateFolder { path } => UndoStep::DeleteFolder { path: path.clone() },
            UndoStep::DeleteFolder { path } => UndoStep::CreateFolder { path: path.clone() },
            UndoStep::RenameFolder { from, to } => {
                UndoStep::RenameFolder { from: to.clone(), to: from.clone() }
            }
        }
    }
}

/// How long after an action its undo still repaints the list itself rather
/// than waiting for the server (#200). Long enough to cover "that was a
/// mistake", short enough that the rows it puts back are still what is
/// there — past this, another client or a sync may have moved on.
const INSTANT_UNDO: std::time::Duration = std::time::Duration::from_secs(10);

/// How many carried-over bodies to hold before giving up on them. They are
/// meant to be claimed by the very next reload; a build-up means the reloads
/// never came, and these are whole message bodies.
const CARRIED_BODY_LIMIT: usize = 64;

/// One entry on the undo (or redo) stack: the step to apply, and what the
/// user called the action it belongs to, so the menu can say "Undo Archive"
/// rather than just "Undo".
struct UndoEntry {
    account_id: u32,
    what: String,
    step: UndoStep,
    /// When the action happened, for the [`INSTANT_UNDO`] window.
    at: std::time::Instant,
    /// The rows a move took off screen, kept whole (bodies included) so a
    /// quick undo can put them straight back without a round trip.
    rows: Vec<Message>,
    /// The conversations those rows were part of, by Message-ID. Taken as
    /// the move is recorded: moving a message forgets its account's
    /// conversations, so by the time the undo runs they are gone.
    threads: Vec<(String, Vec<Message>)>,
    /// Whether applying this entry puts `rows` back on screen (undoing a
    /// move) or takes them away again (redoing it).
    rows_return: bool,
}

struct ReaderCompose {
    id: u32,
    controller: Controller<Compose>,
    window: Option<adw::Window>,
}

/// Files handed in from outside the app (GNOME Files' "Send with Hylki",
/// "Open With Hylki", a mailto: with attach=), on their way to a composer.
#[derive(Debug, Default)]
pub struct FileHandOff {
    /// The composer's other fields (a mailto's recipient, subject, body):
    /// used when the files go into a new message.
    base: ComposePrefill,
    /// The readable files.
    files: Vec<std::path::PathBuf>,
    /// The ones that were named but could not be read (reported once).
    dropped: Vec<std::path::PathBuf>,
}

/// Where a hand-off's files are going, once decided.
#[derive(Debug)]
pub enum HandOffTarget {
    New,
    Draft(Message),
    Reply(Message),
}

/// A hand-off's files split by how the composer takes them: attached, or
/// uploaded to cloud storage and linked (the ones over the size limit).
#[derive(Debug, Default, Clone)]
pub struct HandOffFiles {
    attach: Vec<std::path::PathBuf>,
    cloud: Vec<std::path::PathBuf>,
}

impl HandOffFiles {
    fn of(files: Vec<std::path::PathBuf>, cloud: bool) -> Self {
        if cloud {
            Self { attach: Vec::new(), cloud: files }
        } else {
            Self { attach: files, cloud: Vec::new() }
        }
    }
}

pub struct AppModel {
    /// One mail worker per account (account_id → request sender).
    workers: HashMap<u32, UnboundedSender<MailRequest>>,
    /// `mid:` lookups out at the servers (#130): the Message-ID and how many
    /// accounts have yet to answer, so a miss is reported once, at the end.
    mid_searches: HashMap<String, usize>,
    /// The tag finder's scan in progress (Settings → Tags → Find Tags…):
    /// how many accounts have yet to answer, and what the others found.
    tag_scan: Option<TagScan>,
    /// Counts scans, so a stale timeout cannot end a later one.
    tag_scan_gen: u32,
    config: Vec<AccountConfig>,
    /// Demo mode's stand-in account configs (the sample accounts live only
    /// at the backend): what the Accounts panel edits, kept in memory so a
    /// changed colour, emoji or picture shows without touching disk.
    demo_config: Vec<AccountConfig>,
    window: adw::ApplicationWindow,
    prefs: Option<Controller<Preferences>>,
    accounts_win: Option<Controller<AccountsWindow>>,
    /// What the accounts panel was built over (its init, printed), to tell
    /// whether a reopened Settings window needs a fresh one.
    accounts_seed: Option<String>,
    /// Standalone compose windows (multiple allowed at once). Pruned as they close.
    composers: Vec<ComposeHost>,
    /// The reader's inline reply/forward composer, if open.
    reader_compose: Option<ReaderCompose>,
    /// Superseded inline composers still finishing a save-if-dirty before closing.
    draining_composers: Vec<(u32, Controller<Compose>)>,
    /// SlideDown revealer under the reader toolbar that hosts the inline pane.
    reader_compose_revealer: gtk::Revealer,
    /// Split-reply slot (#86): a reply slides down from the pane's top and
    /// the message(s) stay below it, visible and interactive.
    reader_split_top: gtk::Revealer,
    /// The same slot beneath the reader (#212): the reply slides up from the
    /// pane's bottom edge when the reading order puts the newest message
    /// there, so the editor continues the conversation where it ends. Only
    /// one of the two slots is ever occupied.
    reader_split_bottom: gtk::Revealer,
    /// The vertical Paned dividing the split reply (start child, the slot
    /// above) from the reader (end child). A Paned allocates by divider
    /// position, so the composer holds the height it was given — a big paste
    /// can't push it down over the messages — and its divider is the grab
    /// handle that resizes the panel. Hidden slot = hidden divider.
    reader_split: gtk::Paned,
    /// The reader's own Paned, nested as `reader_split`'s end child: the
    /// reader (start child) over the bottom slot (end child). A second Paned
    /// rather than swapping children, so the reader is never reparented and
    /// the bottom slot has a divider of its own to be sized by.
    reader_split_lower: gtk::Paned,
    /// The running slide (open or close) of the split reply — held so the
    /// opposite motion can skip it to its end instead of fighting it over
    /// the divider.
    split_close_anim: std::rc::Rc<std::cell::RefCell<Option<adw::TimedAnimation>>>,
    /// The reader pane's own header bar: hidden while a split reply hosts,
    /// since it would show a second set of window decorations mid-window and
    /// spend vertical space the split needs.
    reader_header: std::cell::OnceCell<adw::HeaderBar>,
    /// Same idea over the contacts view's detail pane: composing from a
    /// contact slides down right there instead of yanking the mail view back.
    contacts_compose_revealer: gtk::Revealer,
    /// Monotonic id source for composers.
    next_compose_id: u32,
    menu: gtk::gio::Menu,
    /// The burger menu's help section, rebuilt when Console mode toggles.
    help_menu: gtk::gio::Menu,
    /// All known accounts, ordered by id.
    accounts: Vec<Account>,
    /// account_id → that account's folders.
    folders: HashMap<u32, Vec<Folder>>,
    /// Preferred sidebar account order (by email).
    account_order: Vec<String>,
    /// Accounts whose folder list is collapsed in the sidebar (by email).
    collapsed: Vec<String>,
    /// Accounts whose custom-folders section is expanded in the sidebar (by email).
    folders_expanded: Vec<String>,
    /// Collapsed folder-tree nodes ("email\tpath") — the sidebar's custom
    /// folders render as a collapsible hierarchy (#51).
    tree_collapsed: Vec<String>,
    selected: Option<SelectedFolder>,
    /// Attachments of the currently-open message (shown in the drawer).
    attachments: Vec<Attachment>,
    /// True while the current message's attachments are downloading.
    attachments_loading: bool,
    /// The reader header's right-hand actions are collapsed into the overflow menu
    /// (pane squeezed under READER_ACTIONS_BREAKPOINT).
    reader_actions_collapsed: bool,
    /// The collapsed header's ⋯ button — the anchor its menu pops from.
    reader_overflow_btn: gtk::Button,
    /// The reader header's layout: which buttons, which side, what order
    /// (Settings → Appearance → Toolbar).
    reader_toolbar: config::ReaderToolbar,
    /// The header's action buttons and their breakpoint, for re-packing when
    /// the layout changes. Set once after the view is built.
    reader_toolbar_widgets: std::cell::OnceCell<ReaderToolbarWidgets>,
    /// The toolbar's tag button (#71) — the anchor its menu pops from.
    reader_tag_btn: gtk::Button,
    /// The reader toolbar's Move To… button (#164): the folder picker
    /// anchors to it.
    reader_move_btn: gtk::Button,
    /// Cache of fetched attachments, keyed by (account_id, message_id), so
    /// revisiting a message doesn't re-download them. Byte-bounded — raw
    /// attachment bytes for every message ever opened added up to hundreds of
    /// megabytes over a long session (issue #106).
    attachment_cache: crate::ram_cache::RamCache<Vec<Attachment>>,
    /// The app-wide attachment lightbox (drawer previews): the
    /// previewable items on show, the current index, and its texture. The
    /// overlay fills the whole window — a separate window meant double chrome.
    lightbox_items: Vec<Attachment>,
    lightbox_pos: usize,
    lightbox_texture: Option<gtk::gdk::Texture>,
    /// Mirror of "the lightbox is open", readable synchronously by the
    /// window's key controller (closures only get a sender, not the model).
    lightbox_open: std::rc::Rc<std::cell::Cell<bool>>,
    /// Current lightbox zoom (1 or 3) — a click on the document toggles it,
    /// Escape unwinds it before closing.
    lightbox_zoom: i32,
    /// The lightbox picture + its scroller, for applying zoom sizes.
    lightbox_picture: Option<gtk::Picture>,
    lightbox_scroller: Option<gtk::ScrolledWindow>,
    /// True when a unified view is active (no single folder): the folders
    /// `unified_view` names, merged into one list.
    unified: bool,
    /// Which folders the unified view merges: every account's Inbox for All
    /// Inboxes, its Starred / Sent / Drafts for those rows, or every
    /// filter rule's folder for the Filtered Folders row.
    unified_view: UnifiedView,
    /// (account, folder) → that folder's latest messages, for the unified
    /// view's merge.
    unified_slices: HashMap<(u32, u32), Vec<Message>>,
    /// Accounts whose inbox has been requested for the unified view since
    /// launch (the cache-primed slices still need their catch-up sync).
    unified_boot_requested: HashSet<u32>,
    /// Accounts whose sent mail has been asked for this run (#236).
    sent_index_requested: HashSet<u32>,
    /// (account_id, folder_id) → last-seen message list, shown instantly on
    /// revisit while a fresh sync runs in the background.
    message_cache: HashMap<(u32, u32), Vec<Message>>,
    /// (account_id, folder_id) whose background backfill has fully finished, so the
    /// message list knows no more rows will stream in for them.
    indexed_folders: HashSet<(u32, u32)>,
    /// (account_id, message_id) → fetched body, so reopening a message renders
    /// instantly with no loading spinner. Byte-bounded: the background prefetch
    /// feeds this on every folder sync, and unbounded it grew past a gigabyte
    /// on a long-running session (issue #106) — evicted bodies re-read from the
    /// SQLite cache in a blink.
    body_cache: crate::ram_cache::RamCache<String>,
    /// Sender-authentication verdicts, keyed like `body_cache`. Prefetch delivers
    /// these well before a message is opened, and opening one renders from the
    /// in-memory body cache without a worker round-trip — so the verdict has to
    /// be held here or it would be lost by the time the reader needs it.
    sender_cache: HashMap<(u32, u32), Box<crate::models::SenderCheck>>,
    /// (account_id, folder_id) → server-side unread count, accurate beyond the
    /// loaded window (from IMAP STATUS/SEARCH). Drives the sidebar badges.
    folder_unread: HashMap<(u32, u32), u32>,
    /// Read/unread changes sent to a worker and not stored yet, keyed by
    /// (account, folder path, uid) → (seen, when sent). The worker serves
    /// one request at a time, so a folder list or unread count it fetched
    /// ahead of the STORE still shows the old state; while an entry is
    /// young the app's own state for that message and folder wins over it.
    pending_seen: HashMap<(u32, String, u32), (bool, std::time::Instant)>,
    /// The account-list split view, narrowed to icon-only width when collapsed.
    sidebar_split: Option<adw::OverlaySplitView>,
    /// The "Hylki" title label, hidden while the sidebar is collapsed.
    app_title: Option<gtk::Label>,
    /// Sidebar header. In the icon-only rail its window-control buttons are
    /// hidden so the header stops forcing a minimum width wider than the rail.
    sidebar_header: Option<adw::HeaderBar>,
    /// The main-menu button, which moves into the header's centre (the title
    /// slot) while the sidebar is a rail.
    sidebar_menu: Option<gtk::MenuButton>,
    /// The sidebar header's Refresh button — top-left, across from the menu —
    /// shown expanded and in the peek. The rail has no header room for it, so
    /// there the sidebar stacks its own refresh directly under the menu.
    sidebar_refresh: gtk::Button,
    /// The icon/spinner stack inside it, switched while any account syncs.
    sidebar_refresh_stack: gtk::Stack,
    sidebar_refresh_spinner: gtk::Spinner,
    /// Whether the sidebar is in icon-only (collapsed) mode.
    sidebar_collapsed: bool,
    /// The narrow-window breakpoint is currently applied (window is too narrow
    /// for the expanded sidebar + a full-width actions palette — e.g. tiled to
    /// half of a 1920px screen).
    auto_rail: bool,
    /// The rail's current on-screen state — the user's choice OR'd with the
    /// breakpoint, tracked so apply/unapply only animate real changes.
    rail_active: bool,
    /// While narrow: the sidebar is temporarily expanded as an overlay floating
    /// above the panes (so the list and reader keep their widths).
    sidebar_peek: bool,
    /// True while set_sidebar_peek is mutating the split view, so the
    /// show-sidebar notify (the scrim-dismiss detector) ignores the storm of
    /// notifies our own transition emits — collapsing the split auto-hides
    /// the sidebar, which read as an instant dismissal and closed every peek
    /// the moment it opened.
    peek_transition: std::rc::Rc<std::cell::Cell<bool>>,
    /// The peek panel's own split view: always collapsed, nested in the
    /// account split's content slot, so its overlay sidebar slides out from
    /// the rail's right edge over the panes while the docked rail itself is
    /// never touched — not collapsed, not re-laid, not snapshotted.
    peek_split: Option<adw::OverlaySplitView>,
    /// The icon/spinner stack in the peek panel's own Refresh button.
    peek_refresh_stack: gtk::Stack,
    peek_refresh_spinner: gtk::Spinner,
    /// Preference: hovering the icon rail opens the peek by itself.
    sidebar_hover_expand: bool,
    /// Preference: accounts, folders and sections reopen as they were left.
    remember_sidebar: bool,
    /// Preference: the sidebar reopens in the icon rail if left there.
    remember_rail: bool,
    /// The sections' open state, as the sidebar last reported it,
    /// persisted with the rest of the sidebar layout.
    unified_expanded: bool,
    filtered_expanded: bool,
    tags_expanded: bool,
    starred_expanded: bool,
    sent_expanded: bool,
    drafts_expanded: bool,
    archive_expanded: bool,
    /// Account emails whose own Filtered Folders / Tags sections are open.
    filtered_expanded_accounts: Vec<String>,
    tags_expanded_accounts: Vec<String>,
    /// Preference: the icon rail marks unread mail with a dot, not a count.
    rail_dots: bool,
    /// Preference: the sections the icon rail folds up when the sidebar
    /// collapses.
    rail_fold: config::RailFold,
    /// The app chrome's theme preference (follow system / light / dark).
    app_theme: config::AppTheme,
    /// The appearance theme: a bundled palette's id, or "system" for the
    /// stock GNOME colours (see `theme.rs`).
    theme: String,
    /// Held so the in-flight collapse/expand width animation isn't dropped.
    sidebar_anim: Option<adw::TimedAnimation>,
    current: Option<Message>,
    /// Sender addresses allowed to auto-load remote content (lowercased).
    allowed_senders: Vec<String>,
    /// The mailing lists left through the reader's Unsubscribe button
    /// (unsubscribed.toml), so a later message from one says so.
    unsubscribed: Vec<config::UnsubscribedList>,
    /// The meeting invitations answered through the reader's buttons
    /// (invites.toml, #223), so a meeting opened again says where it
    /// stands.
    invite_answers: Vec<config::InviteAnswer>,
    /// Whether remote content is auto-loaded for every new message.
    auto_remote_content: bool,
    /// Whether the blocked-remote-content banner is shown at all. Hiding it changes nothing about what
    /// is blocked — only whether the reader says so.
    show_remote_banner: bool,
    /// Addresses/domains whose incoming inbox mail is auto-deleted (lowercased).
    blacklist: Vec<String>,
    /// Seconds the message-list actions palette stays open after the cursor leaves.
    palette_collapse_secs: u64,
    /// Seconds a message card's actions palette stays open — the cards' own,
    /// separate from the list's above.
    card_palette_collapse_secs: u64,
    /// Whether to load sender avatars from Gravatar.
    gravatar: bool,
    /// Whether the coloured avatars are drawn at all (#29).
    avatars: bool,
    /// Whether the mail you sent wears its mailbox's face rather than the
    /// circle any other sender would get (#189).
    own_mailbox_face: bool,
    /// Whether a sender's site icon may fill their circle (#30).
    sender_logos: bool,
    /// How dates are written, and on what clock (#32).
    date_style: crate::config::DateStyle,
    clock_style: crate::config::ClockStyle,
    /// Seconds between automatic mail checks (0 = manual only).
    fetch_interval_secs: u64,
    /// Whether IMAP IDLE push is enabled.
    push: bool,
    /// Whether desktop notifications (new mail, error alerts) are posted.
    notifications_enabled: bool,
    /// Whether new-mail notifications may name the sender and subject.
    notification_content: bool,
    /// Which action buttons a new-mail notification carries (#244).
    notification_buttons: config::NotificationButtons,
    /// Whether the sidebar's footer shows the "Attachments" row.
    show_attachments: bool,
    /// Whether the sidebar's footer shows the "Contacts" shortcut row.
    show_contacts: bool,
    /// Whether the settings window opens on Accounts (vs Preferences).
    settings_open_accounts: bool,
    /// The Settings category last shown this session, to reopen on.
    last_settings_page: Option<String>,
    /// The list header's count text ("N" / "N of M"), from the message list.
    list_count: String,
    /// Lines of preview text per message-list row (1–3).
    preview_lines: u32,
    /// The keyboard-shortcut reference, while it is open — so the shortcut that
    /// opens it closes it again.
    shortcuts_win: Option<adw::Window>,
    /// Whether Hylki keeps running with no window. Shared with the window's
    /// close handler, which has to read it without the model.
    run_in_background: std::rc::Rc<std::cell::Cell<bool>>,
    /// Whether Hylki starts at login (background running only).
    autostart: bool,
    /// Tray icon (issue #116): the setting, the icon choice, and the running
    /// item while the setting is on.
    tray_enabled: bool,
    tray_icon: config::TrayIcon,
    tray_mail: bool,
    /// The chosen app icon (an `app_icon::catalog` id); the tray draws it.
    app_icon: String,
    /// A restart is on its way (Restart Now clicked, settle timer running).
    restart_pending: bool,
    tray: Option<crate::tray::TrayHandle>,
    /// What the tray menu last listed (account, id, sender, subject; `None`
    /// for no section), so an unchanged list isn't re-sent over D-Bus on
    /// every unread-count push.
    tray_mail_key: std::cell::RefCell<Option<Vec<(u32, u32, String, String)>>>,
    /// Whether single-key (modifier-free) shortcuts are enabled. The window's key
    /// controller needs to read this without the model, so it is shared: with the
    /// feature off, keystrokes must pass straight through rather than be consumed
    /// and dropped.
    single_key: std::rc::Rc<std::cell::Cell<bool>>,
    /// Whether messages are grouped into conversation threads.
    threading: bool,
    /// A conversation re-render is already scheduled, so further bodies arriving
    /// in the same burst don't each queue one of their own.
    thread_render_queued: bool,
    /// When the conversation on screen was opened. Its bodies arrive one at a
    /// time, and painting each arrival meant a stack of half-built renders
    /// flashing past; the reader holds its spinner until they are all in, or
    /// until this has been waiting [`THREAD_BODY_WAIT`].
    thread_opened_at: Option<std::time::Instant>,
    /// A cross-folder conversation lookup is still outstanding. Its answer can
    /// add messages (and bodies) to what is on screen, so the reader waits for
    /// it rather than painting a conversation it is about to paint again.
    thread_related_pending: bool,
    /// Whether the conversation on screen has had its one paint yet. Until it
    /// has, the reader shows its spinner — opening a thread is a wait, however
    /// short, and showing the previous message meanwhile reads as a glitch.
    thread_painted: bool,
    /// Conversations already assembled, keyed by the message that opens them.
    /// Returning to a thread paints from here rather than re-running the
    /// cross-folder lookup and re-gathering bodies: the wait belongs to the
    /// first open, not to every one. Bounded — a conversation holds its
    /// messages' bodies, which are not small.
    thread_cache: HashMap<(u32, u32), Vec<Message>>,
    /// Insertion order for `thread_cache`, oldest first.
    thread_cache_order: Vec<(u32, u32)>,
    /// Which conversation `current_thread` is, for storing it back.
    thread_key: Option<(u32, u32)>,
    /// Whether conversation threads start expanded in the message list.
    threads_expanded: bool,
    /// Reading pane shows conversations newest-message-first.
    thread_newest_first: bool,
    /// Reader always shows the recipients line under the sender.
    always_show_recipients: bool,
    /// Whether the sidebar offers the unified "All Inboxes" section at all.
    show_unified_pref: bool,
    /// Whether the collapsed "All Inboxes" row wears its total-unread chip.
    unified_chips: config::UnifiedChips,
    /// Whether All Inboxes lists the folders that opted-in filter rules file
    /// into, in its "Filtered Folders" section (Settings → Sidebar).
    unified_filtered: bool,
    /// Where the Filtered Folders and Tags sections sit (Settings → Sidebar).
    filtered_placement: config::SectionPlacement,
    tags_placement: config::SectionPlacement,
    /// The unified section's Starred / Sent / Drafts rows (Settings →
    /// Sidebar → Unified).
    unified_kinds: config::UnifiedKinds,
    /// Whether the unified section lists the tags.
    unified_tags: bool,
    /// Whether the account sections are shown at all.
    show_accounts: bool,
    /// The main menu's "Show Accounts" check item, kept in step with the
    /// setting wherever it is changed.
    show_accounts_action: gtk::gio::SimpleAction,
    /// Focus Mode (main menu, Ctrl+Shift+F, Settings → Appearance): the
    /// master switch and the parts it strips. Applied on top of the ordinary
    /// settings, which it never changes — off, everything comes back.
    focus: config::FocusMode,
    /// The main menu's "Focus Mode" check item.
    focus_action: gtk::gio::SimpleAction,
    /// The message list header's foldable controls and the ⋯ that stands in
    /// for them in Focus Mode (set in init).
    list_header_widgets: std::cell::OnceCell<ListHeaderWidgets>,
    /// Whether the sidebar's disclosure chevrons lead their rows.
    chevrons_left: bool,
    /// Console mode offered in the status bar (Settings → System & Appearance).
    console_mode: bool,
    /// Read-marking policy (#100).
    read_mark: config::ReadMark,
    /// Mail filter rules (#47), applied to inbox syncs.
    filters: Vec<config::FilterRule>,
    /// Tags (#71): a name and colour per keyword.
    tags: Vec<config::Tag>,
    /// The tag view, if that is the view — alongside `unified` and
    /// `selected`, never with: the account it is scoped to (`None` spans
    /// every account) and the keyword (`None` for every tag at once).
    tag_view: Option<(Option<u32>, Option<String>)>,
    /// Each tag view's last result, by (scope, keyword): shown the moment
    /// the view opens, while a fresh read of the index runs off the main
    /// thread and replaces it only if it differs.
    tag_view_cache: HashMap<(Option<u32>, Option<String>), Vec<Message>>,
    /// A read of the index for the open tag view is in flight; one more
    /// asked for meanwhile runs when it lands.
    tag_view_loading: bool,
    tag_view_dirty: bool,
    /// When each account's keywords were last re-synced from the server
    /// (#166), so reopening a tag view within a few seconds does not scan
    /// every folder again.
    keyword_sync_at: HashMap<u32, std::time::Instant>,
    /// The tag colours as `.tag-<keyword>` classes, app-wide (the list's
    /// chips, the sidebar's rows, the menus' swatches).
    tag_provider: gtk::CssProvider,
    /// The on-disk index, for reads the main thread makes itself: the tag
    /// views and folding locally-kept tags into synced summaries.
    cache: Option<crate::cache::Cache>,
    /// A manual "Apply Filters" run (#198), while one is under way.
    filter_run: Option<FilterRun>,
    /// Inbox UIDs whose filter move has been requested but not yet observed
    /// (the message still showed up in the last sync). A sync racing the
    /// server-side move must neither re-request the move nor re-notify.
    filter_moved: std::collections::HashMap<(u32, u32), std::collections::HashSet<u32>>,
    /// Per (account, folder): the body-search hits of the last sync (#191),
    /// uid → lowercased alternatives the server found in the message. What
    /// [`apply_filters`] consults for "Message body" conditions.
    body_hits: std::collections::HashMap<(u32, u32), std::collections::HashMap<u32, Vec<String>>>,
    /// Lone messages render as inset cards (#57).
    single_message_card: bool,
    /// Reader View: every message in the reader shown as its content alone,
    /// in the reader's own sheet (see `crate::reader`). The toggle sits in
    /// the reader's subject block; the choice is remembered across runs.
    /// The message zoom this session is at (Ctrl+ / Ctrl-). Starts at
    /// `zoom_default` and is not saved: a launch begins at the setting.
    zoom: u32,
    /// Settings → Reading: the zoom every launch starts at, in percent.
    zoom_default: u32,
    reader_mode: bool,
    /// The Reader View switch is shown in the reader header.
    reader_switch: bool,
    /// What Reader View does when a message is opened (Settings).
    reader_default: config::ReaderDefault,
    /// Each conversation message lists its own attachments (#213).
    card_attachments: bool,
    /// The attachment drawer beneath the reader is shown at all (#213).
    drawer_enabled: bool,
    /// Whether conversation rows may expand into their members in the list
    /// (the row keeps its chip and chevron either way).
    thread_expansion: bool,
    /// Whether deleting a whole selected conversation asks for confirmation.
    confirm_thread_delete: bool,
    /// Whether the current `list_selection` came from card clicks in the
    /// reader (as opposed to rows picked in the list). A lone list-row
    /// selection over an open conversation stands for the whole thread when
    /// deleting; a lone picked card never does.
    selection_from_cards: bool,
    /// Conversation card actions hide until hovered (preference).
    card_actions_hover: bool,
    /// With the ⋯ toggle off: card actions appear automatically on hover.
    card_actions_auto: bool,
    /// The list's actions palette opens on row hover (no ⋯ click).
    /// Whether the message list rows carry an actions palette at all.
    list_palette: bool,
    list_palette_hover: bool,
    /// A message card's ⋯ opens the card menu instead of sliding its
    /// actions palette out.
    card_palette_menu: bool,
    /// Message rows take a sideways swipe to archive / delete (#92).
    swipe_enabled: bool,
    /// The message list's swipe-gesture sides are swapped (#swipe).
    swipe_reversed: bool,
    /// How far a trackpad two-finger swipe has to travel to fire the action
    /// (higher = shorter swipe); mouse and touch drags are unaffected.
    swipe_sensitivity: f64,
    /// "New message" composes inline over the reading pane (vs a window).
    compose_inline: bool,
    reply_fields: bool,
    /// The identity new messages are sent from (#157); empty = the open
    /// folder's account.
    compose_default_from: String,
    paste_plain: bool,
    /// New messages start as plain text (#180).
    compose_format: crate::config::ComposeFormat,
    /// Where the split reply opens in the reading pane (#212).
    reply_position: config::ReplyPosition,
    /// Where the signature sits in a reply or forward (#237).
    signature_position: config::SignaturePosition,
    spellcheck: bool,
    spellcheck_langs: String,
    /// How email content is themed (message content only, not the app UI).
    message_theme: config::MessageTheme,
    /// Set every message in the reader's own font (#56).
    override_fonts: bool,
    /// That font, as a Pango description; empty = the interface font.
    reader_font: String,
    /// Ignore the senders' text and background colours (#56).
    override_colors: bool,
    /// Plain-text messages in monospace (#181), and the font ("" = the
    /// desktop's monospace font).
    plain_monospace: bool,
    plain_font: String,
    /// The repeating auto-fetch timer, if armed.
    auto_fetch_source: Option<gtk::glib::SourceId>,
    notifications: Controller<NotificationCenter>,
    /// The first-run welcome wizard, alive while it's on screen.
    welcome: Option<Controller<crate::ui::welcome::Welcome>>,
    notify_count: usize,
    /// Accounts currently performing network activity (drives the spinner).
    busy: HashSet<u32>,
    sidebar: Controller<Sidebar>,
    /// A second instance of the sidebar, always expanded, for the floating
    /// peek panel. It is fed the same contents and counts as the docked one
    /// (see `sidebars_emit`) and the two mirror each other's selection.
    peek_sidebar: Controller<Sidebar>,
    message_list: Controller<MessageList>,
    message_view: Controller<MessageView>,
    /// In-message attachment thumbnail drawer, docked below the reader body.
    attachment_drawer: Controller<AttachmentDrawer>,
    gallery: Controller<AttachmentsGallery>,
    /// The in-app contacts view behind the sidebar's Contacts row.
    contacts_page: Controller<ContactsPage>,
    /// Whether the content area shows the contacts view.
    showing_contacts: bool,
    /// True when the attachments gallery replaces the mail panes.
    showing_gallery: bool,
    /// Whether the Outbox is showing instead of the mail panes.
    showing_outbox: bool,
    /// Messages waiting to be sent, per account, as last reported by its worker.
    outbox_by_account: HashMap<u32, Vec<crate::models::OutboxItem>>,
    /// Gallery items per account inbox, merged for display.
    /// How many attachments the gallery's current query matches, carried
    /// between pages so only the first one pays for the COUNT.
    gallery_total: u32,
    /// Messages each in-scope folder has still to have described, so the
    /// gallery's footer can show one figure for the whole scan.
    gallery_scan_left: HashMap<(u32, String), u32>,
    /// Messages popped out into their own windows, keyed by (account, message).
    popouts: HashMap<(u32, u32), PopOut>,
    /// The conversation currently shown in the reader (newest first), with bodies
    /// filled in as they arrive. More than one entry = conversation/thread mode.
    current_thread: Vec<Message>,
    /// Every message currently selected (list rows or reader cards), newest
    /// report wins. Lets the toolbar's Delete act on the whole multi-selection.
    list_selection: Vec<(u32, u32)>,
    /// Undoable actions (#200): moves, flag changes, and folder creates and
    /// renames, newest last. Unlimited: entries are a few strings each.
    /// Ctrl+Z pops one, applies it, and pushes its inverse onto `redo_stack`.
    undo_stack: Vec<UndoEntry>,
    /// The other half of the history: what Ctrl+Shift+Z puts back. Cleared
    /// whenever a new action is recorded, as every undo history does — the
    /// branch it belonged to no longer exists.
    redo_stack: Vec<UndoEntry>,
    /// Conversations belonging to messages a move is bringing back (#200),
    /// keyed and re-filed exactly like [`carried_bodies`]. The reader paints
    /// an assembled conversation with no lookup and no spinner; without this
    /// a restored message rebuilt its thread from scratch, which means a
    /// round trip, which means the spinner.
    carried_threads: HashMap<(u32, String), Vec<Message>>,
    /// Bodies belonging to messages a move is bringing back (#200), by
    /// Message-ID. A move gives a message a new UID, which orphans its body in
    /// [`body_cache`] — that is keyed by the id the UID becomes. These are
    /// re-keyed onto the new ids as the folder's reload arrives, so the
    /// restored message renders from what is already here instead of blanking
    /// to a spinner while the server sends it over again. Drained on use.
    carried_bodies: HashMap<(u32, String), String>,
    /// Per-message remote-content choices: show (true) or block (false) this
    /// one message, whatever the standing policy says. Set from the reader's
    /// menu and its banner, and kept for the session only — a decision about
    /// one message is not a decision about a sender.
    remote_override: HashMap<(u32, u32), bool>,
    /// What the inline composer's own history can take back (#200), as it
    /// last reported: `(undo, redo)`, each `None` when that side is empty.
    /// While the composer holds keyboard focus the window's Undo and Redo
    /// are its, not the mail history's — the same split the keys already
    /// make, so the menu never offers to archive a message while someone is
    /// typing into a reply.
    compose_history: (Option<String>, Option<String>),
    /// The burger menu's Undo/Redo section, relabelled as the stacks change.
    undo_menu: gtk::gio::Menu,
    /// Their actions, kept so they can be greyed out when there is nothing
    /// on the stack.
    undo_action: Option<RelmAction<UndoAction>>,
    redo_action: Option<RelmAction<RedoAction>>,
    /// A draft awaiting its body before opening in the compose editor.
    /// A draft whose body is being fetched before its editor opens, and
    /// whether that editor goes in the reading pane (true) or a window.
    pending_draft: Option<(Message, bool, HandOffFiles)>,
    /// A message whose body is being fetched so a reply to it can open with
    /// handed-in files (Send with Hylki → Reply to a Message…).
    pending_reply: Option<(Message, HandOffFiles)>,
    /// A draft picker waiting for Drafts folders never listed this run
    /// (the hand-off, and the folders still to answer).
    pending_draft_pick: Option<(FileHandOff, HashSet<(u32, u32)>)>,
    /// Settings → System → GNOME Files: what handed-in files open into and
    /// what happens over the size limit.
    files_prefs: config::FilesPrefs,
    /// "Edit as New Message" (#232) waiting on the body, or the attachments,
    /// it is to be copied from.
    pending_edit_as_new: Option<Message>,
    /// A forward (#240) waiting on the body, or the attachments, it is to
    /// carry, and whether it opens inline or in a window.
    pending_forward: Option<(Message, bool)>,
    /// A reply started from a notification's button (#244), waiting for
    /// the body it quotes.
    pending_notified_reply: Option<Message>,
    /// Settings → System → Links: which browser a link in a message opens in
    /// (#232). Empty = the desktop's default, "ask" = its app chooser,
    /// otherwise a desktop entry id.
    link_browser: String,
    /// Outstanding bulk MoveMessages requests awaiting a worker `BulkComplete`.
    /// Outstanding server-side bulk operations; while > 0 the refresh spinner
    /// spins and the status bar narrates.
    bulk_pending: usize,
    /// Ids handed to conversation messages pulled in from another folder,
    /// allocated downwards from the top of the range. A message's id is its UID,
    /// which is only unique within its own folder — and the reader keys bodies by
    /// (account, id), so a Sent reply sharing a UID with the Inbox message it
    /// answers would otherwise be handed the wrong body.
    related_id_seq: u32,
    /// The id issued to each such message, keyed by where it really lives
    /// (account, folder, uid). Stable across re-opens so its body stays cached.
    related_ids: HashMap<(u32, u32, u32), u32>,
}

/// A message displayed in its own top-level window (double-click to pop out).
struct PopOut {
    window: adw::Window,
    controller: Controller<MessageWindow>,
}

/// Which folders a unified view merges (see `AppModel::unified_view`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnifiedView {
    /// Every account's folder of this kind.
    Kind(FolderKind),
    /// Every filter rule's folder listed under All Inboxes.
    Filtered,
}

#[derive(Debug)]
pub enum AppMsg {
    // User actions
    /// A unified row was chosen: All Inboxes, the unified section's Starred
    /// / Sent / Drafts row (every account's folder of that kind), or its
    /// Filtered Folders row (every rule's folder).
    UnifiedSelected(UnifiedView),
    /// Show the attachments gallery (sidebar "Attachments" row).
    ShowAttachments,
    /// Show the Outbox (queued, unsent messages).
    ShowOutbox,
    /// A worker reported its queue: replaces that account's entries.
    OutboxItems { account_id: u32, items: Vec<crate::models::OutboxItem> },
    /// A worker reported something noteworthy that isn't a failure.
    Notice(String),
    /// Edit the queued message currently open in the reader.
    EditCurrentOutbox,
    /// Try to send the queued message currently open in the reader.
    SendCurrentOutbox,
    /// Try to send everything waiting, across accounts.
    RetryAllOutbox,
    /// Send Later (#145): the half-minute clock that sends due queued mail.
    SendDueScheduled,
    /// The gallery wants a page of attachments matching this query.
    GalleryLoad(Box<GalleryRequest>),
    /// Download an attachment the gallery knows of but has never fetched.
    GalleryFetch { account_id: u32, folder_path: String, uid: u32 },
    /// A chunk of a folder's attachments was described by the scan.
    AttachmentsScanned { account_id: u32, folder_path: String, added: u32, remaining: u32 },
    /// Gallery "Go to Message" — open the attachment's source message.
    OpenAttachmentMessage { account_id: u32, folder_path: String, uid: u32 },
    FolderSelected { account_id: u32, folder_id: u32, name: String, path: String },
    ToggleCollapse(u32),
    /// A sidebar folder-tree node was collapsed/expanded (#51) — persist it.
    FolderNodeCollapsed { account_id: u32, path: String, collapsed: bool },
    ToggleCustomFolders(u32),
    SidebarCollapsed(bool),
    /// The message-pane header's sidebar button: flip the sidebar between the
    /// expanded pane and the icon rail. Routed through the sidebar component so
    /// its CollapsedChanged output drives the same peek/pin/persist logic as
    /// ever (see `SidebarCollapsed`).
    ToggleSidebar,
    /// The narrow-window breakpoint applied/unapplied — collapse the sidebar to
    /// its icon rail (and restore it) without touching the user's preference.
    AutoRail(bool),
    /// The floating sidebar overlay was dismissed from outside (a click on the
    /// dimmed content, an Escape) rather than via the sidebar's own button.
    SidebarPeekDismissed,
    SidebarContext(CtxAction),
    /// A folder's threading references were repaired — re-read it from the cache
    /// so what is on screen groups with what was found.
    RefsRepaired { account_id: u32, folder_id: u32 },
    /// The list's selection changed; the reader outlines the matching cards.
    SelectionKeys(Vec<(u32, u32)>),
    /// The reader's selection changed — mirror what the list can represent.
    SelectCards(Vec<(u32, u32)>),
    /// A conversation message was scrolled all the way through — mark it read on
    /// the server, in the caches and in the list.
    ThreadMessageSeen { account_id: u32, id: u32 },
    /// The conversation on screen has new bodies to show (coalesced).
    RenderThread,
    /// Messages were dropped on a sidebar folder — move them there. `items` is
    /// the dragged selection as (account, folder, uid, id).
    DropMoveMessages { dest_account: u32, dest: String, items: Vec<(u32, u32, u32, u32)> },
    /// Create a custom folder under an account (from the right-click menu).
    CreateFolder { account_id: u32, name: String },
    /// Move a folder under a new parent (sidebar drag-and-drop, #51).
    /// `dest` is the new parent's path, or "" for the account's top level.
    MoveFolder { account_id: u32, path: String, dest: String },
    /// Rename a folder's leaf name in place (context menu).
    RenameFolderTo { account_id: u32, path: String, new_name: String },
    /// Delete a custom folder (its contents are moved to Trash first).
    DeleteFolder { account_id: u32, path: String },
    /// Erase everything in a Trash or Junk folder (#152).
    EmptyFolder { account_id: u32, folder_id: u32, path: String },
    AccountsReordered(Vec<String>),
    /// `solo` marks a reply the user picked out of a conversation on screen:
    /// show that message alone and don't go looking for its siblings.
    MessageSelected { message: Message, thread: Vec<Message>, solo: bool },
    /// A new-mail desktop notification was clicked — open that message.
    OpenMessageFromNotification { account_id: u32, folder_id: u32, message_id: u32 },
    /// A notification action button (#38): mark the notified message read, or
    /// archive it, without raising the window.
    NotificationMarkRead { account_id: u32, folder_id: u32, message_id: u32 },
    NotificationArchive { account_id: u32, folder_id: u32, message_id: u32 },
    NotificationDelete { account_id: u32, folder_id: u32, message_id: u32 },
    NotificationReply { account_id: u32, folder_id: u32, message_id: u32 },
    NotificationForward { account_id: u32, folder_id: u32, message_id: u32 },
    NotificationSpam { account_id: u32, folder_id: u32, message_id: u32 },
    /// The search field became active/inactive — supply or drop the cross-folder
    /// search pool (every folder's messages, so search can span the mailbox).
    SearchActive(bool),
    /// The message list has no selection to show (e.g. the last message was
    /// removed), so the reader should clear.
    ClearReader,
    /// Double-click: open the message in its own standalone window.
    OpenMessageWindow { message: Message, thread: Vec<Message> },
    /// A popped-out message window was closed (remove it from the map).
    PopoutClosed((u32, u32)),
    /// Add a contact from a popout window's sender.
    AddContactFrom { name: String, email: String },
    /// Download a specific message's attachments (from a popout window).
    LoadAttachmentsFor(Box<Message>),
    /// A message's sender-authentication verdict arrived with its body.
    SenderChecked {
        account_id: u32,
        message_id: u32,
        check: Box<crate::models::SenderCheck>,
    },
    /// Open a single attachment delivered from a popout window.
    OpenAttachmentItem(Box<Attachment>),
    /// Save attachments delivered from a popout window.
    SaveAttachmentItems(Vec<Attachment>),
    ToggleStar,
    Archive,
    Delete,
    /// Erase messages for good (confirmed "Delete permanently" in Trash).
    PurgeMessages(Vec<Message>),
    /// The rest of an open message's conversation, found in other folders.
    Related { account_id: u32, message_id: u32, messages: Vec<Message> },
    /// True sizes for the conversations the list is showing, counted across
    /// the account's folders (#222). Tags are the list's own thread keys.
    ThreadCounts { account_id: u32, counts: Vec<(String, usize)> },
    /// The list rebuilt its rows: which conversations are on screen, and the
    /// Message-IDs each is threaded by, so their real sizes can be looked up.
    ThreadsListed { groups: Vec<(u32, String, Vec<String>)> },
    /// A row's palette or context-menu action. `conversation` holds the whole
    /// thread when the row stands for a collapsed conversation rather than for
    /// `message` alone — a reply started there answers the conversation's
    /// newest message, not the head it is filed under (#210). Empty otherwise.
    RowAction { action: RowAction, message: Box<Message>, conversation: Vec<Message> },
    /// A conversation card's own action pill. Reply/Reply all/Forward open the
    /// reader's inline composer (like the toolbar); the rest act like RowAction.
    CardAction { action: RowAction, message: Box<Message> },
    /// The open conversation gained a member (a reply synced in while it
    /// was on screen): show it in place, without a re-selection.
    ThreadGrew { message: Box<Message>, thread: Vec<Message> },
    /// A card's "Add sender to Contacts" button.
    CardContact(Box<Message>),
    /// A right-click on a reader card: that message's full menu (the list
    /// row's) at window point (x, y).
    CardMenu { message: Box<Message>, x: f64, y: f64 },
    /// The reader toolbar's Mark as Read/Unread toggle for the open message.
    ToggleReadCurrent,
    /// A bulk action applied to every selected message.
    Bulk { action: BulkAction, messages: Vec<Message> },
    /// A worker finished one bulk MoveMessages request; stops the busy
    /// indicator once all outstanding bulk moves are done.
    BulkComplete,
    Compose,
    OpenAbout,
    AllowSender(String),
    AddSender(String),
    RemoveSender(String),
    /// A card's Unsubscribe button: confirm, then leave the list the message
    /// came from by the handles its headers offered.
    Unsubscribe { message: Box<Message>, info: Box<crate::models::Unsubscribe> },
    /// Confirmed: send the request.
    UnsubscribeGo { message: Box<Message>, info: Box<crate::models::Unsubscribe> },
    /// The one-click route failed (or was not offered): write to the list's
    /// `mailto:` handle instead, from the account the message arrived in.
    UnsubscribeByMail { message: Box<Message>, info: Box<crate::models::Unsubscribe> },
    /// The request is over: `Ok(true)` left the list, `Ok(false)` only opened
    /// the list's web page, `Err` says what went wrong.
    UnsubscribeDone { message: Box<Message>, result: Result<bool, String> },
    /// A card's invitation button (#223): answer the organiser, or hand the
    /// meeting to whatever application opens calendar files.
    InviteAction {
        message: Box<Message>,
        invite: Box<crate::models::Invite>,
        action: crate::ui::message_view::InviteAction,
    },
    AddBlacklist(String),
    RemoveBlacklist(String),
    MarkSpam,
    SetAutoRemoteContent(bool),
    SetShowRemoteBanner(bool),
    /// The reader pane crossed the actions breakpoint (true = collapse the
    /// header's buttons into the overflow menu).
    SetReaderActionsCollapsed(bool),
    /// A new reader toolbar layout from Settings: save, re-pack, re-fold.
    SetReaderToolbar(config::ReaderToolbar),
    /// Showcase only: open a drop gap in the Settings toolbar editor.
    ShowcaseToolbarGap { zone: usize, index: usize },
    /// A right-click on the reader header's empty space: the menu that
    /// leads to the toolbar editor.
    ReaderToolbarMenu { x: f64, y: f64 },
    /// Open Settings on the Appearance page (its Toolbar section).
    CustomizeToolbar,
    /// The window controls were re-measured (decoration layout changed).
    ReaderControlsChanged(i32),
    /// The collapsed header's ⋯ button was clicked — pop its menu.
    ReaderOverflowMenu,
    SetGravatar(bool),
    /// Show the full-window attachment lightbox (from the drawer or the
    /// toolbar popover's Preview) over these previewable items.
    ShowLightbox { items: Vec<Attachment>, start: usize },
    LightboxPrev,
    LightboxNext,
    LightboxClose,
    /// Click on the document: toggle zoom 1x ↔ 3x, anchored at the clicked
    /// point (picture coordinates at the fitted size).
    LightboxZoomCycle { x: f64, y: f64 },
    /// Escape: unwind zoom first; close only from normal view.
    LightboxEscape,
    /// Open the shown lightbox item in its default application.
    LightboxOpenCurrent,
    /// Save the shown lightbox item via a file chooser.
    LightboxDownloadCurrent,
    /// A full-size PDF render finished (content hash) — show it if that item
    /// is still on screen.
    LightboxRendered(u64),
    /// The GNOME Contacts photo index changed (EDS sync, or the first load
    /// finished) — refresh the avatars that are on screen.
    ContactPhotosChanged,
    SetAvatars(bool),
    /// Your own mail wears its mailbox's face, or the circle any other sender
    /// would get (#189).
    SetOwnMailboxFace(bool),
    /// A Gravatar one of the accounts asked for came back (#189).
    OwnGravatarFetched {
        email: String,
        outcome: crate::avatar::FetchOutcome,
    },
    SetSenderLogos(bool),
    SetDateStyle(crate::config::DateStyle),
    SetClockStyle(crate::config::ClockStyle),
    /// Settings: the interface language code, "" for the system's (#179).
    SetLanguage(String),
    /// The welcome wizard's first page picked a language: save it and
    /// come back in it.
    WizardLanguage(String),
    SetThreading(bool),
    SetThreadExpansion(bool),
    SetConfirmThreadDelete(bool),
    /// Delete requested on a whole conversation (a lone selected thread-head
    /// row): confirm (per preference), then delete every member.
    DeleteThread(Vec<Message>),
    /// The thread-delete dialog was confirmed.
    DeleteThreadConfirmed(Vec<Message>),
    SetThreadsExpanded(bool),
    SetThreadNewestFirst(bool),
    SetAlwaysShowRecipients(bool),
    SetSingleMessageCard(bool),
    /// Reader View on or off (the header's switch).
    SetReaderMode(bool),
    /// Message zoom (Ctrl+ / Ctrl- / Ctrl+0): a step up, a step down, or
    /// back to the default, for this session.
    ZoomMessage(i8),
    /// Settings → Reading: the zoom every launch starts at.
    SetZoomDefault(u32),
    /// Settings: show the Reader View switch in the reader header.
    SetReaderSwitchShown(bool),
    /// Settings: what Reader View does when a message is opened.
    SetReaderDefault(config::ReaderDefault),
    SetCardActionsMode { hover_toggle: bool, hover_auto: bool },
    SetListPalette(bool),
    SetListPaletteHover(bool),
    SetCardPaletteMenu(bool),
    SetSwipeEnabled(bool),
    SetSwipeReversed(bool),
    SetSwipeSensitivity(f64),
    SetComposeInline(bool),
    /// Reply panel shows its From/To/Subject rows from the start (#154).
    SetReplyFields(bool),
    /// Settings → System → GNOME Files changed.
    SetFilesPrefs(config::FilesPrefs),
    /// Settings → System → Links: the browser links open in (#232).
    SetLinkBrowser(String),
    /// Copy the message the reader is on into a new one (#232).
    EditAsNewCurrent,
    /// A hand-off's files have a destination (the dialog answered, or the
    /// preference decided); `remember` writes the choice to Settings.
    HandOffAction { hand_off: FileHandOff, action: config::FilesAction, remember: bool },
    /// The draft picker answered (None: a new message instead).
    HandOffDraft { hand_off: FileHandOff, draft: Option<Message> },
    /// Show the draft picker with whatever Drafts lists have arrived.
    HandOffDraftPickNow,
    /// The reply picker answered (None: a new message instead).
    HandOffReply { hand_off: FileHandOff, message: Option<Message> },
    /// The size check answered: open the composer, attaching or uploading.
    HandOffOpen { hand_off: FileHandOff, target: HandOffTarget, cloud: bool, remember: bool },
    /// The identity new messages are sent from (#157); empty = the open
    /// folder's account.
    SetComposeDefaultFrom(String),
    /// The Settings window showed a category; remembered for reopening.
    SettingsPageShown(String),
    SetPastePlain(bool),
    SetSpellcheck(bool),
    SetSpellcheckLangs(String),
    /// Show or block remote content for one message, whatever the standing
    /// policy is — the reader menu's entry, and what the banner's Load does.
    SetRemoteContent { account_id: u32, id: u32, show: bool },
    /// Ctrl+Z: undo the most recent action.
    Undo,
    /// Ctrl+Shift+Z / Ctrl+Y: put back what undo took away.
    Redo,
    /// The inline composer's own history changed (#200): what its Undo and
    /// Redo would take back now, or `None` for a direction with nothing in
    /// it. The window's entries follow it while it holds focus.
    ComposeHistory { id: u32, undo: Option<String>, redo: Option<String> },
    SetFetchInterval(u64),
    SetPush(bool),
    SetNotifications(bool),
    SetNotificationContent(bool),
    SetNotificationButtons(config::NotificationButtons),
    SetAttachmentsRow(bool),
    SetContactsRow(bool),
    SetShowUnified(bool),
    SetUnifiedChips(config::UnifiedChips),
    SetUnifiedFiltered(bool),
    /// The unified section's Starred / Sent / Drafts rows.
    SetUnifiedKinds(config::UnifiedKinds),
    /// Whether the unified section lists the tags.
    SetUnifiedTags(bool),
    /// Whether the account sections are shown at all.
    SetShowAccounts(bool),
    /// Focus Mode's settings changed (the master switch or a part): apply
    /// whatever differs, animated, and save.
    SetFocusMode(config::FocusMode),
    /// The ⋯ of the message list header (Focus Mode): search, filters and
    /// sort as a menu.
    ListOverflowMenu,
    /// An account's own Filtered Folders / Tags section was opened or
    /// folded — record it with the layout.
    ToggleAccountFiltered(u32),
    ToggleAccountTags(u32),
    SetChevronsLeft(bool),
    /// Where the Filtered Folders / Tags sections sit (Settings → Sidebar).
    SetFilteredPlacement(config::SectionPlacement),
    SetTagsPlacement(config::SectionPlacement),
    /// The message list's visible-count text changed.
    ListCount(String),
    /// Preference: hovering the narrow-window rail floats the sidebar out.
    SetSidebarHoverExpand(bool),
    /// Preference: accounts, folders and sections reopen as they were left.
    SetRememberSidebar(bool),
    /// Preference: the sidebar reopens in the icon rail if left there.
    SetRememberRail(bool),
    /// Build the Settings window ahead of its first open (see the handler).
    PrewarmSettings,
    /// Close the Settings window as the user would (the showcase's reopen
    /// timing).
    DebugCloseSettings,
    /// The sidebar's All Inboxes / Filtered Folders / Tags sections were
    /// opened or folded — record it with the layout.
    SidebarSectionsOpen {
        all_inboxes: bool,
        filtered: bool,
        tags: bool,
        starred: bool,
        sent: bool,
        drafts: bool,
        archive: bool,
    },
    /// Preference: the icon rail shows unread dots rather than counts.
    SetRailDots(bool),
    /// A tag view's rows, read from the index off the main thread: the
    /// view they answer and, per message, its account and folder path.
    TagViewLoaded { key: (Option<u32>, Option<String>), rows: Vec<(u32, String, Message)> },
    /// Preference: which sections the icon rail folds up on collapse.
    SetRailFold(config::RailFold),
    /// Preference: the app chrome's theme (follow system / light / dark).
    SetAppTheme(config::AppTheme),
    /// Preference: the appearance theme (Settings gallery).
    SetTheme(String),
    /// The cursor entered the sidebar pane — open the hover peek (rail +
    /// preference permitting).
    SidebarHoverEnter,
    SetPreviewLines(u32),
    SetSingleKey(bool),
    SetRunInBackground(bool),
    SetAutostart(bool),
    SetTray(bool),
    SetTrayIcon(config::TrayIcon),
    SetTrayMail(bool),
    /// Preference: the app icon (Settings gallery or the wizard).
    SetAppIcon(String),
    /// "Restart Now" from the app-icon heads-up: quit into the restart
    /// helper once the desktop has taken the new launcher in.
    RestartApp,
    /// The settle time is up: hand off to the restart helper and quit.
    RestartNow,
    /// Quit was chosen from the tray icon's menu.
    QuitFromTray,
    /// "View all unread" was chosen from the tray icon's menu: go to All
    /// Inboxes, or without it to the first inbox holding unread mail.
    TrayViewUnread,
    /// A single-key shortcut fired.
    Shortcut(Shortcut),
    /// Show the keyboard-shortcut reference.
    ShowShortcuts,
    /// Print the message in the reader (issue #16).
    PrintMessage,
    /// Render it to a PDF and open that, to see what will come out.
    PrintPreview,
    SetPaletteCollapse(u64),
    SetCardPaletteCollapse(u64),
    SetMessageTheme(config::MessageTheme),
    SetOverrideFonts(bool),
    SetReaderFont(String),
    SetOverrideColors(bool),
    /// Settings: plain-text messages in monospace (#181), and the font.
    SetPlainMonospace(bool),
    SetPlainFont(String),
    /// Settings: new messages start as plain text (#180).
    SetComposeFormat(crate::config::ComposeFormat),
    /// Settings: where the split reply opens in the reading pane (#212).
    SetReplyPosition(config::ReplyPosition),
    SetSignaturePosition(config::SignaturePosition),
    /// Settings: each conversation message lists its own attachments (#213).
    SetCardAttachments(bool),
    /// Settings: the attachment drawer beneath the reader is shown (#213).
    SetAttachmentDrawer(bool),
    /// A card's attachment chip (#213): open the file, or save it.
    CardAttachment { account_id: u32, id: u32, index: usize, save: bool },
    /// The drawer's "Show in Message": scroll the reader to the message this
    /// attachment came with (#213).
    ShowAttachmentInMessage(Attachment),
    /// Showcase only: turn the inline composer's preview on.
    ShowcaseComposePreview,
    /// Pick entry `n` of the inline composer's From row (#237 capture).
    ShowcaseComposeFrom(u32),
    /// Forward one message of the open conversation by its id (#240), as
    /// its card's Forward button would.
    ShowcaseForward { account_id: u32, id: u32 },
    /// Showcase only (HYLKI_SHOWCASE_COMPOSE_CLOSE): cancel the inline
    /// composer, to check its web process goes with it.
    ShowcaseComposeClose,
    /// Showcase only (HYLKI_SHOWCASE_COMPOSE_UNDO): drive the inline
    /// composer's history through a scripted round of edits and undos.
    ShowcaseComposeUndo,
    /// Showcase only: open the window's burger menu, to read its Undo and
    /// Redo entries in the state they are actually seen in.
    ShowcaseBurger,
    /// Showcase only: put the inline composer into this composing format
    /// first, so the same round runs over a source-mode body.
    ShowcaseComposeFormat(config::ComposeFormat),
    /// Showcase only: open the inline composer's format chooser, or its
    /// overflow menu.
    ShowcaseComposeMenu { format: bool },
    /// Fetch a message's body again: its OpenPGP verdict changed (#133).
    ReloadBody(Box<crate::models::Message>),
    /// Select a settings category by id (the showcase hook).
    ShowSettingsPage(String),
    ComposeTo(String),
    /// Showcase only (HYLKI_SHOWCASE_FOLDER): switch to the first account's
    /// folder of this kind, so a capture can start from Drafts, Sent, etc.
    ShowcaseFolder { kind: FolderKind, account: Option<u32> },
    /// Showcase only (HYLKI_SHOWCASE_EDITOR_DIRTY): change the open account
    /// editor, so leaving an edited one can be captured.
    ShowcaseDirtyEditor,
    Reply,
    ReplyAll,
    Forward,
    AddToContacts,
    AddContactAddr(String),
    OpenMailto(String),
    /// Ctrl+C while the reader's view does not hold the keyboard.
    CopyReaderSelection,
    /// A `mid:` link (RFC 2392) from anywhere on the system (#130): open the
    /// message with that Message-ID.
    OpenMid(String),
    /// An account's answer to a server-side Message-ID search.
    MidLocated { account_id: u32, message_id: String, hit: Option<(String, u32)> },
    /// The tag finder: scan every account for keywords in use.
    FindTags,
    /// One account's answer to the scan.
    KeywordsFound { account_id: u32, findings: Vec<KeywordFinding> },
    /// A keyword re-sync (#166) changed the cached keywords of these folders.
    KeywordsSynced { account_id: u32, paths: Vec<String> },
    /// The scan's safety net: report what has come in, if the scan `gen` is
    /// still the one running.
    TagScanTimeout(u32),
    /// Files handed in from outside the app (a file manager's "Open With
    /// Hylki", or the command line): open a fresh composer with them attached
    /// (Isaac's PR #96).
    OpenWithFiles(Vec<std::path::PathBuf>),
    /// The click on a "message ready" desktop alert: raise the window and
    /// the composer it announced.
    PresentComposers,
    ContactAdded(Result<crate::contacts::AddOutcome, String>),
    ViewSource,
    /// User clicked "Load attachments" for a message whose attachments weren't
    /// pre-downloaded — fetch them from the server now.
    SendMessage(Box<OutgoingMessage>),
    SaveDraftMessage(Box<OutgoingMessage>),
    /// The composer's Delete Draft: trash the draft it was opened from and
    /// close that composer without saving.
    DeleteDraft { id: u32, origin: crate::models::DraftOrigin },
    DraftSaved,
    /// A composer (id) finished — tear down its host (window or inline revealer).
    ComposeClosed(u32),
    /// Promote/demote the reader's inline composer (id) between inline and window.
    ComposeToggleWindow(u32),
    Refresh,
    OpenAccounts,
    /// Open the accounts window straight to the "add account" form (empty state).
    AddFirstAccount,
    AccountSaved { original_email: Option<String>, account: Box<AccountConfig> },
    /// Show the keyring / Secret Service setup help. `problem: true` when a save
    /// actually failed to persist; `false` for the proactive one-time tip.
    ShowKeyringHelp { problem: bool },
    /// The one-time notice that an earlier install's data was carried over.
    ShowCarryOverNotice(&'static crate::legacy::Predecessor),
    AccountRemoved { email: String },
    AccountEnabledChanged { email: String, enabled: bool },
    ImportGoaAccount(Box<AccountConfig>),
    /// The welcome wizard's privacy/personalize choices: fan out through the
    /// existing Set* handlers so every side effect stays in one place.
    ApplyWelcomePrefs(crate::ui::welcome::WelcomePrefs),
    /// Raise and focus the main window (welcome wizard hand-off).
    PresentWindow,
    /// Open the status bar straight into console mode (button / burger menu).
    OpenConsole,
    /// Settings toggle for offering console mode at all.
    SetConsoleMode(bool),
    /// The list header's unread quick filter (#97).
    SetUnreadFilter(bool),
    /// Reveal (or toggle away) the message list's search bar (#102).
    OpenListSearch,
    /// Open the reader's in-message find bar (#103).
    OpenReaderFind,
    /// Delay-policy read marking (#100): fires a couple of seconds after a
    /// message opened; only applies if it is still the one on screen.
    DeferredMarkRead { message: Box<Message> },
    /// Settings → Reading → "Mark as read" changed.
    SetReadMark(config::ReadMark),
    /// The list header's starred quick filter.
    SetStarredFilter(bool),
    /// Backup (#50): save/load the whole configuration as one file.
    ExportSettings,
    /// Save the console's log to a file for a bug report (#132).
    ExportLog,
    /// Write the exported log straight to `path`, no chooser: the
    /// HYLKI_SHOWCASE_MEMORY hook, for reading the memory section of a
    /// running instance.
    ExportLogTo(std::path::PathBuf),
    ImportSettings,
    /// The filter rules changed in Settings (#47).
    SetFilters(Vec<config::FilterRule>),
    /// Run the filter rules over mail that is already in these (account,
    /// folder) pairs (#198), rather than waiting for the next arrival. An
    /// empty list means every account's Inbox.
    ApplyFilters(Vec<(u32, u32)>),
    /// A manual filter run has waited long enough: report what it managed.
    FilterRunTimeout,
    /// The progress dialog of a manual filter run was dismissed: it goes on
    /// in the background and reports in the status bar instead (#198).
    FilterRunToBackground,
    /// A folder's load has been to the server and back (#198).
    FolderSynced { account_id: u32, folder_id: u32 },
    /// The tags changed in Settings (#71).
    SetTags(Vec<config::Tag>),
    /// A tag row in the sidebar was chosen: list everything carrying it.
    /// A tag row was chosen: its keyword (`None` for the unified Tags row,
    /// every tag at once), and the account it is scoped to (an account
    /// section's own Tags row) or `None` for every account.
    TagSelected { keyword: Option<String>, account: Option<u32> },
    /// Put a tag on one message, or take it off (row menu, palette, card).
    SetTag { message: Box<Message>, keyword: String, add: bool },
    /// The reader toolbar's tag menu: toggle a tag on the reader's target.
    ToggleTagCurrent(String),
    /// Put the reader's target back in its Inbox (from Trash or Junk, #138).
    MoveToInbox,
    /// Pop the tag menu on the reader toolbar's tag button.
    ReaderTagMenu,
    /// Open the Move To… folder picker (#164) for the reader's target, or
    /// the whole list selection.
    MoveToMenu,
    /// The picker's answer: file the target or selection into `dest` —
    /// or, with `whole`, the open conversation entire (#171).
    MoveSelectionTo { account_id: u32, dest: String, whole: bool },
    /// "Move To…" from the message list's right-click menu: open the
    /// picker at window point (`x`, `y`) for `messages` (with
    /// `offer_whole`, a conversation row and its members: the picker
    /// offers all of them, or the row's own message alone).
    ListMoveTo { messages: Vec<Message>, offer_whole: bool, x: f64, y: f64 },
    /// That picker's answer.
    MoveMessagesTo { account_id: u32, dest: String, messages: Vec<Message> },
    /// Second stage of ImportSettings: the chosen file, applied on a clean
    /// main-loop turn (working inside the chooser's completion callback froze
    /// the app when the confirmation dialog presented there).
    ImportSettingsFrom(std::path::PathBuf),
    /// GNOME Online Accounts changed on the session bus. Carries the fresh live
    /// state, already fetched (debounced) on the watcher thread so the GTK main
    /// thread never does D-Bus I/O — re-reconcile against it.
    GoaChanged(crate::goa::GoaLiveState),
    /// The system resumed from sleep — worker IMAP sockets are stale, so
    /// reconnect every account and reload the visible folder.
    SystemResumed,
    /// Open the settings window on the user's preferred view (the menu entry).
    OpenSettings,
    OpenPreferences,
    ClosePreferences,
    /// The accounts editor subpage opened/closed in the settings window.
    SettingsEditorOpen(Option<&'static str>),
    /// A settings editor was asked whether it can be left: `ask` when it
    /// holds unsaved changes, otherwise it has already closed itself.
    SettingsLeaveEditor { page: String, ask: bool },
    /// The "settings window opens to" preference changed (true = Accounts).
    SetSettingsOpenAccounts(bool),
    // Worker events (each carries the account it came from)
    SetAccount(Account),
    SetFolders { account_id: u32, folders: Vec<Folder> },
    Messages { account_id: u32, folder_id: u32, messages: Vec<Message> },
    /// Showcase: open filter rule `i` in the current Accounts panel.
    ShowcaseEditFilter(usize),
    /// The message-body filter conditions the server confirmed for a folder's
    /// listed mail (#191): uid → the lowercased alternatives found in it.
    /// Arrives just ahead of the `Messages` it describes.
    BodyHits { account_id: u32, folder_id: u32, hits: std::collections::HashMap<u32, Vec<String>> },
    /// Additional indexed summaries from the background backfill (search index).
    MessagesAppend { account_id: u32, folder_id: u32, messages: Vec<Message> },
    UndoRestored { account_id: u32, folder_id: u32, message_ids: Vec<String> },
    /// A folder's background backfill finished — it's fully indexed now.
    BackfillDone { account_id: u32, folder_id: u32 },
    FolderUnread { account_id: u32, folder_id: u32, unread: u32 },
    /// From a per-folder IDLE watcher, which knows its folder only by path —
    /// folder ids are positional and may have shifted since it was spawned.
    FolderUnreadByPath { account_id: u32, path: String, unread: u32 },
    /// The worker stored (or failed to store) a read/unread change the app
    /// had applied ahead of it.
    SeenSettled { account_id: u32, path: String, uid: u32 },
    /// `path` is the folder the body was read from — a UID only identifies a
    /// message within its own folder, so applying a body to a message means
    /// checking the folder too.
    Body { account_id: u32, message_id: u32, path: String, body: String },
    Source { text: String },
    Attachments { account_id: u32, message_id: u32, items: Vec<Attachment> },
    AttachmentsPending { account_id: u32, message_id: u32 },
    /// A flagged message turned out to have no real attachments — drop its paperclip.
    NoAttachments { account_id: u32, message_id: u32 },
    /// An unflagged message turned out to carry attachments — give it one.
    HasAttachments { account_id: u32, message_id: u32 },
    Sent { account_id: u32 },
    Status { account_id: u32, text: String },
    Error { account_id: u32, text: String, connectivity: bool },
    NotifyCount(usize),
    ToggleNotifications,
    OpenContacts,
    /// The background EDS read for the contacts view finished.
    ContactsLoaded(Vec<crate::contacts::ContactDetails>),
    /// Right-click on the sidebar's Contacts row: the external app.
    LaunchGnomeContacts,
    /// Contact editor writes (run on a background thread against EDS).
    SaveContact { book_uid: String, vcard: String },
    CreateContact(String),
    DeleteContact { book_uid: String, uid: String },
    /// A contact write finished (`Some` = the error to show).
    ContactWriteDone(Option<String>),
}

#[relm4::component(pub)]
impl SimpleComponent for AppModel {
    type Init = ();
    type Input = AppMsg;
    type Output = ();

    view! {
        adw::ApplicationWindow {
            set_title: Some(crate::APP_NAME),
            set_icon_name: Some(crate::APP_ID),
            add_css_class: "hylki",

            // Persist the window size + maximized state on close. (Position and
            // which monitor can't be restored on Wayland — the compositor owns
            // placement — so only the geometry is saved.)
            connect_close_request[background = model.run_in_background.clone()] => move |w| {
                let maximized = w.is_maximized();
                let (width, height) = if maximized {
                    let (sw, sh, _) = crate::config::load_window_state();
                    (sw, sh)
                } else {
                    (w.width(), w.height())
                };
                crate::config::save_window_state(width, height, maximized);
                // Running in the background: hide the window and stay alive, so
                // mail keeps arriving and GNOME lists Hylki under Background Apps
                // (issue #3). The window is kept rather than rebuilt, so reopening
                // is instant and nothing is torn down — which is also what makes
                // this safe, given the exit below.
                if background.get() {
                    w.set_visible(false);
                    return gtk::glib::Propagation::Stop;
                }
                // Exit cleanly the moment window state is saved. Letting GTK,
                // WebKit and the per-account worker threads tear down the normal
                // way can abort — a Rust panic fired from a GObject dispose
                // callback becomes SIGABRT, which the Flatpak surfaces as a crash
                // notification. Nothing else needs persisting on quit: accounts
                // and settings are written as they change.
                std::process::exit(0)
            },

            // Escape/arrows drive the lightbox from anywhere (capture phase,
            // gated on the open flag so normal typing is untouched).
            add_controller = gtk::EventControllerKey {
                set_propagation_phase: gtk::PropagationPhase::Capture,
                connect_key_pressed[sender, open = model.lightbox_open.clone()] => move |_, key, _, _| {
                    if !open.get() {
                        return gtk::glib::Propagation::Proceed;
                    }
                    match key {
                        gtk::gdk::Key::Escape => sender.input(AppMsg::LightboxEscape),
                        gtk::gdk::Key::Left => sender.input(AppMsg::LightboxPrev),
                        gtk::gdk::Key::Right => sender.input(AppMsg::LightboxNext),
                        _ => return gtk::glib::Propagation::Proceed,
                    }
                    gtk::glib::Propagation::Stop
                },
            },

            #[wrap(Some)]
            set_content = &gtk::Overlay {

                #[wrap(Some)]
                set_child = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                append: model.notifications.widget(),

                // The peek panel's split view: permanently collapsed, so
                // showing its sidebar slides the expanded panel in from the
                // window's left edge over the rail and the panes — with
                // libadwaita's scrim, shadow and swipe — while the docked
                // rail underneath stays exactly as it is (see
                // set_sidebar_peek). The account split view is its content.
                #[name = "peek_split"]
                adw::OverlaySplitView {
                    set_vexpand: true,
                    set_collapsed: true,
                    set_show_sidebar: false,
                    set_min_sidebar_width: 280.0,
                    set_max_sidebar_width: 280.0,
                    // No edge-swipe to open: it would fight the message
                    // list's own swipe actions.
                    set_enable_show_gesture: false,

                    #[wrap(Some)]
                    set_sidebar = &adw::ToolbarView {
                        // Laid out like the expanded sidebar's header: Refresh
                        // top-left, the title centred, the menu top-right.
                        add_top_bar = &adw::HeaderBar {
                            add_css_class: "flat",
                            set_show_start_title_buttons: false,
                            set_show_end_title_buttons: false,
                            #[wrap(Some)]
                            set_title_widget = &gtk::Label {
                                set_label: crate::APP_NAME,
                                add_css_class: "app-title",
                            },
                            pack_start = &gtk::Button {
                                set_tooltip_text: Some(i18n("Refresh or long-press for Status Bar").as_str()),
                                add_css_class: "flat",
                                set_valign: gtk::Align::Center,
                                set_child: Some(&model.peek_refresh_stack),
                                connect_clicked[sender] => move |_| sender.input(AppMsg::Refresh),
                                add_controller = gtk::GestureLongPress {
                                    connect_pressed[sender] => move |gesture, _, _| {
                                        gesture.set_state(gtk::EventSequenceState::Claimed);
                                        sender.input(AppMsg::ToggleNotifications);
                                    },
                                },
                            },
                            pack_end = &gtk::MenuButton {
                                set_icon_name: "co.hyprlab.Hylki-open-menu-symbolic",
                                set_tooltip_text: Some(i18n("Main Menu").as_str()),
                                add_css_class: "flat",
                                set_menu_model: Some(&model.menu),
                            },
                        },
                        #[wrap(Some)]
                        set_content = model.peek_sidebar.widget(),
                    },

                    #[wrap(Some)]
                    #[name = "sidebar_split"]
                    set_content = &adw::OverlaySplitView {
                    set_vexpand: true,
                    set_max_sidebar_width: 280.0,
                    // Low enough (with the panes' own minimums) that the whole
                    // window fits half of a 1920px screen — see READER_MIN_WIDTH.
                    set_min_sidebar_width: 180.0,
                    set_sidebar_width_fraction: 0.2,

                    #[wrap(Some)]
                    set_sidebar = &adw::ToolbarView {
                        #[name = "sidebar_header"]
                        add_top_bar = &adw::HeaderBar {
                            add_css_class: "flat",
                            #[wrap(Some)]
                            #[name = "app_title"]
                            set_title_widget = &gtk::Label {
                                set_label: crate::APP_NAME,
                                add_css_class: "app-title",
                            },
                            pack_start: &model.sidebar_refresh,
                            #[name = "sidebar_menu"]
                            pack_end = &gtk::MenuButton {
                                set_icon_name: "co.hyprlab.Hylki-open-menu-symbolic",
                                set_tooltip_text: Some(i18n("Main Menu").as_str()),
                                add_css_class: "flat",
                                set_menu_model: Some(&model.menu),
                            },
                        },
                        #[wrap(Some)]
                        set_content = model.sidebar.widget(),
                    },

                    #[wrap(Some)]
                    set_content = &gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,

                    #[name = "content_stack"]
                    gtk::Stack {
                        set_hexpand: true,
                        set_transition_type: gtk::StackTransitionType::Crossfade,
                        // Swap the mail panes for the attachments gallery or
                        // the contacts view.
                        #[watch]
                        set_visible_child_name: if model.showing_gallery {
                            "gallery"
                        } else if model.showing_contacts {
                            "contacts"
                        } else {
                            "mail"
                        },

                    add_named[Some("mail")] = &gtk::Paned {
                        set_orientation: gtk::Orientation::Horizontal,
                        // Thin handle so the panes sit flush (just a 1px divider),
                        // no wide-handle gap between them. A theme paints that
                        // divider through this class (see theme::css); without
                        // one it keeps the stock separator.
                        add_css_class: "mail-split",
                        set_wide_handle: false,
                        // Launch wide enough for a row's actions palette. That is
                        // also the list's minimum while the avatars are on,
                        // so `shrink_start_child: false` clamps to the same figure
                        // either way. With the circles off the minimum drops to what
                        // the sender and subject need (#29) — a fine width to be
                        // able to drag down to, but a poor one to open at.
                        set_position: crate::config::load_list_pane_width(),
                        // Remember the width the user drags to (#28). Debounced:
                        // position-notify fires per pixel of a drag (and when the
                        // window squeezes the pane), one write once it settles.
                        connect_position_notify[
                            pending = std::rc::Rc::new(std::cell::RefCell::new(
                                None::<gtk::glib::SourceId>,
                            ))
                        ] => move |p| {
                            let pos = p.position();
                            if let Some(id) = pending.borrow_mut().take() {
                                id.remove();
                            }
                            let armed = pending.clone();
                            *pending.borrow_mut() = Some(gtk::glib::timeout_add_local_once(
                                std::time::Duration::from_millis(600),
                                move || {
                                    *armed.borrow_mut() = None;
                                    crate::config::save_list_pane_width(pos);
                                },
                            ));
                        },
                        // The list keeps its width as the window resizes (the
                        // reader absorbs the change), and can't be dragged narrower
                        // than its own minimum. The reader has a floor of its own:
                        // its header carries a full row of actions, and squeezing it
                        // pushed them off the right-hand edge of the window.
                        set_resize_start_child: false,
                        set_shrink_start_child: false,
                        set_resize_end_child: true,
                        set_shrink_end_child: false,

                        #[wrap(Some)]
                        set_start_child = &adw::ToolbarView {
                            add_top_bar = &adw::HeaderBar {
                                add_css_class: "flat",
                                // Middle pane: no window controls (the reader pane's
                                // header carries the window's close button).
                                set_show_start_title_buttons: false,
                                set_show_end_title_buttons: false,
                                // No folder title here — the sidebar's selection
                                // already names it. An empty label keeps the
                                // window's "Hylki" title from appearing instead.
                                #[wrap(Some)]
                                set_title_widget = &gtk::Label {
                                    set_label: "",
                                },
                                // Leftmost, mirroring the pane it acts on: the
                                // sidebar expand/collapse toggle (moved here from
                                // the sidebar's own footer).
                                pack_start = &gtk::Button {
                                    set_icon_name: "co.hyprlab.Hylki-sidebar-show-symbolic",
                                    #[watch]
                                    set_tooltip_text: Some(if model.rail_active { i18n("Expand sidebar") } else { i18n("Collapse sidebar") }.as_str()),
                                    add_css_class: "flat",
                                    connect_clicked[sender] => move |_| sender.input(AppMsg::ToggleSidebar),
                                },
                                // Search lives behind this button (#102);
                                // Ctrl+F and / open it too.
                                #[name = "lh_search"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "co.hyprlab.Hylki-system-search-symbolic",
                                        set_tooltip_text: Some(i18n("Search messages (Ctrl+F)").as_str()),
                                        add_css_class: "flat",
                                        connect_clicked[sender] => move |_| {
                                            sender.input(AppMsg::OpenListSearch);
                                        },
                                    },
                                },
                                // Across from the sidebar toggle: the visible
                                // message count and the sort menu (moved out of
                                // the list's own toolbar to reclaim a row).
                                // pack_end packs right-to-left: sort rightmost.
                                // Quick filters (#97): unread / starred only.
                                // Session state, like Mail.app's filter bar.
                                // Focus Mode: the ⋯ that stands in for the
                                // search, filters, count and sort while they
                                // are folded away. Rightmost, so packed first.
                                #[name = "lh_overflow"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    set_reveal_child: false,
                                    gtk::Button {
                                        set_icon_name: "co.hyprlab.Hylki-view-more-horizontal-symbolic",
                                        set_tooltip_text: Some(i18n("Search, filters and sort").as_str()),
                                        set_valign: gtk::Align::Center,
                                        add_css_class: "flat",
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::ListOverflowMenu),
                                    },
                                },
                                #[name = "lh_unread"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::ToggleButton {
                                        set_icon_name: "co.hyprlab.Hylki-mail-unread-symbolic",
                                        set_tooltip_text: Some(i18n("Show only unread").as_str()),
                                        set_valign: gtk::Align::Center,
                                        add_css_class: "flat",
                                        connect_toggled[sender] => move |btn| {
                                            sender.input(AppMsg::SetUnreadFilter(btn.is_active()));
                                        },
                                    },
                                },
                                #[name = "lh_starred"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::ToggleButton {
                                        set_icon_name: "co.hyprlab.Hylki-starred-symbolic",
                                        set_tooltip_text: Some(i18n("Show only starred").as_str()),
                                        set_valign: gtk::Align::Center,
                                        add_css_class: "flat",
                                        connect_toggled[sender] => move |btn| {
                                            sender.input(AppMsg::SetStarredFilter(btn.is_active()));
                                        },
                                    },
                                },

                                #[name = "lh_sort"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    #[name = "list_sort_btn"]
                                    gtk::MenuButton {
                                        set_icon_name: "co.hyprlab.Hylki-view-sort-descending-symbolic",
                                        set_tooltip_text: Some(i18n("Sort messages").as_str()),
                                        set_valign: gtk::Align::Center,
                                        add_css_class: "flat",
                                    },
                                },
                                #[name = "lh_count"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Label {
                                        #[watch]
                                        set_label: &model.list_count,
                                        set_valign: gtk::Align::Center,
                                        add_css_class: "list-count",
                                    },
                                },
                            },
                            #[wrap(Some)]
                            set_content = model.message_list.widget(),
                        },

                        #[wrap(Some)]
                        #[name = "reader_bin"]
                        set_end_child = &adw::BreakpointBin {
                            // The narrowest the reader may become. The header's
                            // actions collapse into the overflow menu before this
                            // matters (see the breakpoint added in init). The
                            // height floor exists because AdwBreakpointBin insists
                            // on an explicit one; the window's own minimum is what
                            // really binds vertically.
                            set_size_request: (READER_MIN_WIDTH, 200),
                            #[wrap(Some)]
                            set_child = &adw::ToolbarView {
                            #[name = "reader_header"]
                            add_top_bar = &adw::HeaderBar {
                                // Rightmost of the packed items, beside the window
                                // controls: the overflow ⋯ that stands in for the
                                // action buttons while collapsed.
                                pack_end: &model.reader_overflow_btn,
                                add_css_class: "flat",
                                // Tighter icon spacing than stock so the full
                                // action row fits a narrower pane (see
                                // READER_ACTIONS_BREAKPOINT and styles.css).
                                add_css_class: "reader-toolbar",
                                // Empty title so the window's "Hylki" title isn't
                                // shown here; the app title lives above the sidebar.
                                #[wrap(Some)]
                                set_title_widget = &gtk::Label {
                                    set_label: "",
                                },
                                // Outbox actions, in place of Reply/Forward/Flag —
                                // a message that hasn't been sent can't be replied
                                // to, and the questions worth asking about it are
                                // whether to edit, send or bin it.
                                #[name = "tb_outbox_edit"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "co.hyprlab.Hylki-document-edit-symbolic",
                                        set_tooltip_text: Some(i18n("Edit this message").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.showing_outbox
                                            && model.reader_compose.is_none(),
                                        #[watch]
                                        set_sensitive: model.current.is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::EditCurrentOutbox),
                                    },
                                },
                                #[name = "tb_outbox_send"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "co.hyprlab.Hylki-mail-send-symbolic",
                                        set_tooltip_text: Some(i18n("Try to send this message now").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.showing_outbox
                                            && model.reader_compose.is_none(),
                                        #[watch]
                                        set_sensitive: model.current.is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::SendCurrentOutbox),
                                    },
                                },
                                #[name = "tb_outbox_send_all"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_label: &i18n("Send all"),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.showing_outbox
                                            && model.reader_compose.is_none(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::RetryAllOutbox),
                                    },
                                },
                                // ---- The action buttons. Declared here in the
                                // default order, then re-packed in init (and on
                                // every change) in the user's saved order — see
                                // relayout_reader_toolbar. The left group stays
                                // at every width; the right group folds into
                                // the ⋯ overflow. Default left group: Reply,
                                // Reply All, Forward, Star, Archive, Delete.
                                #[name = "tb_reply"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "co.hyprlab.Hylki-mail-reply-sender-symbolic",
                                        set_tooltip_text: Some(i18n("Reply").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Reply),
                                        // In a conversation these act on the one
                                        // highlighted card; with none (or several)
                                        // highlighted they grey out — no way to say
                                        // which message they'd mean.
                                        #[watch]
                                        set_sensitive: model.reply_target().is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::Reply),
                                    },
                                },
                                #[name = "tb_reply_all"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "co.hyprlab.Hylki-mail-reply-all-symbolic",
                                        set_tooltip_text: Some(i18n("Reply All").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::ReplyAll),
                                        #[watch]
                                        set_sensitive: model.reply_target().is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::ReplyAll),
                                    },
                                },
                                #[name = "tb_forward"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "co.hyprlab.Hylki-mail-forward-symbolic",
                                        set_tooltip_text: Some(i18n("Forward").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Forward),
                                        #[watch]
                                        set_sensitive: model.reply_target().is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::Forward),
                                    },
                                },
                                #[name = "tb_star"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_tooltip_text: Some(i18n("Flag").as_str()),
                                        // One glyph in both states, like every other
                                        // icon; the flagged state carries colour only.
                                        set_icon_name: "co.hyprlab.Hylki-non-starred-symbolic",
                                        #[watch]
                                        set_css_classes: if model.toolbar_star_lit() {
                                            &["flat", "star-active"]
                                        } else {
                                            &["flat"]
                                        },
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Star),
                                        #[watch]
                                        set_sensitive: model.reply_target().is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::ToggleStar),
                                    },
                                },
                                #[name = "tb_archive"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "co.hyprlab.Hylki-mail-archive-symbolic",
                                        set_tooltip_text: Some(i18n("Archive").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Archive),
                                        #[watch]
                                        set_sensitive: model.reply_target().is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::Archive),
                                    },
                                },
                                #[name = "tb_delete"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "co.hyprlab.Hylki-user-trash-symbolic",
                                        #[watch]
                                        set_tooltip_text: Some(&model.delete_tooltip()),
                                        add_css_class: "flat",
                                        // Shown for the Outbox too (a queued message
                                        // can still be binned) — see toolbar_visible.
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Delete),
                                        #[watch]
                                        set_sensitive: model.reply_target().is_some()
                                            || model.list_selection.len() > 1,
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::Delete),
                                    },
                                },
                                // ---- Default right group, left to right: Tags,
                                // Read/Unread, Spam, Move To, Find, Print.
                                // (The sender-check seal lives in the
                                // message header now — #88; no Add-to-Contacts
                                // button either: right-click any address in a
                                // message header.)
                                #[name = "tb_print"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "co.hyprlab.Hylki-printer-symbolic",
                                        set_tooltip_text: Some(i18n("Print Preview (Ctrl+Shift+P)").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Print),
                                        #[watch]
                                        set_sensitive: model.current.is_some(),
                                        // The preview, not the print dialog: the button
                                        // shows what will come out and prints from
                                        // there, so nobody spends paper to find out.
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::PrintPreview),
                                    },
                                },
                                // In-message find (#103).
                                #[name = "tb_find"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "co.hyprlab.Hylki-loupe-with-arrow-symbolic",
                                        set_tooltip_text: Some(i18n("Find in message (Ctrl+F)").as_str()),
                                        add_css_class: "flat",
                                        // Greyed out, not hidden, with no message
                                        // open: the toolbar must not shift.
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Find),
                                        #[watch]
                                        set_sensitive: model.current.is_some(),
                                        connect_clicked[sender] => move |_| {
                                            sender.input(AppMsg::OpenReaderFind);
                                        },
                                    },
                                },
                                // Move To… (#164): a folder picker for the
                                // target, or the whole list selection.
                                #[name = "tb_move"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Box {
                                        #[local_ref]
                                        reader_move_btn -> gtk::Button {
                                            #[watch]
                                            set_visible: model.toolbar_visible(config::ToolbarItem::MoveTo),
                                            #[watch]
                                            set_sensitive: model.reply_target().is_some()
                                                || model.list_selection.len() > 1,
                                        },
                                    },
                                },
                                #[name = "tb_spam"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        #[watch]
                                        set_icon_name: if model.target_in_junk() {
                                            "co.hyprlab.Hylki-mail-mark-notjunk-symbolic"
                                        } else {
                                            "co.hyprlab.Hylki-mail-mark-junk-symbolic"
                                        },
                                        #[watch]
                                        set_tooltip_text: Some(if model.target_in_junk() { i18n("Not Spam") } else { i18n("Mark as Spam") }.as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Spam),
                                        #[watch]
                                        set_sensitive: model.reply_target().is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::MarkSpam),
                                    },
                                },
                                #[name = "tb_read"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: !model.showing_outbox && !model.reader_actions_collapsed
                                            && model.reader_compose.is_none(),
                                        // The icon shows the ACTION (read envelope =
                                        // "mark as read"), matching the menus.
                                        #[watch]
                                        set_icon_name: if model.reply_target().is_some_and(|m| m.unread) {
                                            "co.hyprlab.Hylki-mail-read-symbolic"
                                        } else {
                                            "co.hyprlab.Hylki-mail-unread-symbolic"
                                        },
                                        #[watch]
                                        set_tooltip_text: Some(if model.reply_target().is_some_and(|m| m.unread) { i18n("Mark as Read") } else { i18n("Mark as Unread") }.as_str()),
                                        #[watch]
                                        set_sensitive: model.reply_target().is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::ToggleReadCurrent),
                                    },
                                },
                                // Tags (#71): a menu of the tags, ticked where
                                // the target carries them. Only once a tag exists.
                                #[name = "tb_tags"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Box {
                                        #[local_ref]
                                        reader_tag_btn -> gtk::Button {
                                            #[watch]
                                            set_visible: model.toolbar_visible(config::ToolbarItem::Tags) && !model.tags.is_empty(),
                                            #[watch]
                                            set_sensitive: model.reply_target().is_some(),
                                        },
                                    },
                                },
                                #[name = "tb_spinner"]
                                pack_end = &gtk::Spinner {
                                    set_valign: gtk::Align::Center,
                                    set_tooltip_text: Some(i18n("Downloading attachments…").as_str()),
                                    #[watch]
                                    set_spinning: model.attachments_loading,
                                    #[watch]
                                    set_visible: model.attachments_loading
                                        && model.reader_compose.is_none(),
                                },
                            },
                            // Reader content: the inline reply/forward pane drops
                            // down (SlideDown revealer) above the message body,
                            // pushing it down to make room. The revealer is
                            // prepended in `init`. The drawer's widget is a Paned
                            // that holds the reader body + attachment footer.
                            #[wrap(Some)]
                            #[name = "reader_content_box"]
                            set_content = &gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                append: model.attachment_drawer.widget(),
                            },
                            },
                        },
                    },
                    },
                    },
                    },
                },
                },

                // ======== Full-window attachment lightbox ========
                // Covers all three panes; shown for images and PDFs coming
                // from the drawer. Same look as the gallery's own lightbox
                // (same CSS), no extra window.
                add_overlay = &gtk::Box {
                    add_css_class: "gallery-lightbox",
                    set_orientation: gtk::Orientation::Vertical,
                    #[watch]
                    set_visible: !model.lightbox_items.is_empty(),

                    gtk::CenterBox {
                        add_css_class: "gallery-lightbox-bar",
                        #[wrap(Some)]
                        set_start_widget = &gtk::Label {
                            #[watch]
                            set_label: model
                                .lightbox_items
                                .get(model.lightbox_pos)
                                .map(|a| a.name.as_str())
                                .unwrap_or(""),
                            set_ellipsize: gtk::pango::EllipsizeMode::Middle,
                            set_halign: gtk::Align::Start,
                            add_css_class: "gallery-lightbox-title",
                        },
                        #[wrap(Some)]
                        set_end_widget = &gtk::Button {
                            set_icon_name: "co.hyprlab.Hylki-window-close-symbolic",
                            set_tooltip_text: Some(i18n("Close").as_str()),
                            add_css_class: "circular",
                            add_css_class: "flat",
                            connect_clicked => AppMsg::LightboxClose,
                        },
                    },

                    gtk::Box {
                        set_orientation: gtk::Orientation::Horizontal,
                        set_vexpand: true,
                        set_spacing: 8,

                        gtk::Button {
                            set_icon_name: "co.hyprlab.Hylki-go-previous-symbolic",
                            set_tooltip_text: Some(i18n("Previous").as_str()),
                            set_valign: gtk::Align::Center,
                            add_css_class: "circular",
                            add_css_class: "osd",
                            #[watch]
                            set_visible: model.lightbox_items.len() > 1,
                            connect_clicked => AppMsg::LightboxPrev,
                        },

                        gtk::Stack {
                            set_hexpand: true,
                            set_vexpand: true,
                            #[watch]
                            set_visible_child_name: if model.lightbox_texture.is_some() {
                                "image"
                            } else {
                                "rendering"
                            },

                            #[name = "lightbox_scroller"]
                            add_named[Some("image")] = &gtk::ScrolledWindow {
                                set_hscrollbar_policy: gtk::PolicyType::Automatic,
                                set_vscrollbar_policy: gtk::PolicyType::Automatic,
                                set_hexpand: true,
                                set_vexpand: true,

                                #[name = "lightbox_picture"]
                                gtk::Picture {
                                    set_can_shrink: true,
                                    set_content_fit: gtk::ContentFit::Contain,
                                    #[watch]
                                    set_paintable: model.lightbox_texture.as_ref(),
                                    // Click-to-zoom and drag-to-pan are wired
                                    // in `init` (they share a movement
                                    // threshold, which view! closures can't).
                                },
                            },

                            add_named[Some("rendering")] = &gtk::Box {
                                set_halign: gtk::Align::Center,
                                set_valign: gtk::Align::Center,
                                gtk::Spinner {
                                    set_spinning: true,
                                    set_width_request: 36,
                                    set_height_request: 36,
                                },
                            },
                        },

                        gtk::Button {
                            set_icon_name: "co.hyprlab.Hylki-go-next-symbolic",
                            set_tooltip_text: Some(i18n("Next").as_str()),
                            set_valign: gtk::Align::Center,
                            add_css_class: "circular",
                            add_css_class: "osd",
                            #[watch]
                            set_visible: model.lightbox_items.len() > 1,
                            connect_clicked => AppMsg::LightboxNext,
                        },
                    },

                    gtk::CenterBox {
                        add_css_class: "gallery-lightbox-bar",
                        #[wrap(Some)]
                        set_start_widget = &gtk::Label {
                            #[watch]
                            set_label: &model.lightbox_caption(),
                            set_halign: gtk::Align::Start,
                            set_ellipsize: gtk::pango::EllipsizeMode::End,
                            add_css_class: "dim-label",
                        },
                        #[wrap(Some)]
                        set_end_widget = &gtk::Box {
                            set_spacing: 6,
                            gtk::Button {
                                set_icon_name: "co.hyprlab.Hylki-document-open-symbolic",
                                set_tooltip_text: Some(i18n("Open").as_str()),
                                add_css_class: "flat",
                                connect_clicked => AppMsg::LightboxOpenCurrent,
                            },
                            gtk::Button {
                                set_icon_name: "co.hyprlab.Hylki-folder-download-symbolic",
                                set_tooltip_text: Some(i18n("Download…").as_str()),
                                add_css_class: "flat",
                                connect_clicked => AppMsg::LightboxDownloadCurrent,
                            },
                        },
                    },
                },
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        relm4::set_global_css(include_str!("styles.css"));
        register_icons();
        // Before install_scheme_css and before the reader exists: both read
        // the theme's colours back, and both listen for the light/dark flip
        // that swaps a theme's two palettes — GTK runs those handlers in
        // connection order, so the palette has to be in place first.
        crate::theme::install(&config::load_theme());
        install_scheme_css(&root);

        let mut sidebar_state = config::load_sidebar_state();
        // Settings → Sidebar: "Remember the icon rail" off starts every
        // launch with the full sidebar; "Remember the sidebar layout" off
        // starts with every section folded up — and every account, folded as
        // it arrives (SetAccount) — keeping only the account order. The two
        // are independent.
        let remember_sidebar = config::load_remember_sidebar();
        let remember_rail = config::load_remember_rail();
        let icon_only = remember_rail && sidebar_state.icon_only;
        // Read here, ahead of the sidebar itself: Focus Mode's rail part
        // decides what the sidebar is built as. `icon_only` stays the user's
        // own choice — the one that is saved and the one that comes back
        // when Focus Mode ends.
        let mut focus = config::load_focus_mode();
        // "Start in Focus Mode" decides how the app opens, whatever the last
        // session left on. Written back at once so the file, the menu's check
        // item and the window all say the same thing from the first frame.
        if focus.enabled != focus.start_focused {
            focus.enabled = focus.start_focused;
            config::save_focus_mode(&focus);
        }
        let rail_now = icon_only || focus.active(config::FocusPart::RailSidebar);
        if !remember_sidebar {
            sidebar_state = config::SidebarState { order: sidebar_state.order, ..Default::default() };
        }
        let unified_expanded = sidebar_state.unified_expanded;
        let filtered_expanded = sidebar_state.filtered_expanded;
        let tags_expanded = sidebar_state.tags_expanded;
        let starred_expanded = sidebar_state.starred_expanded;
        let sent_expanded = sidebar_state.sent_expanded;
        let drafts_expanded = sidebar_state.drafts_expanded;
        let archive_expanded = sidebar_state.archive_expanded;

        // Load accounts, then reconcile against GNOME Online Accounts: drop any
        // imported account GOA no longer has, pause any whose Mail service is
        // switched off there. Reconciliation is skipped when GOA is unreachable,
        // so a momentary outage never wipes imported accounts. Live changes are
        // handled by the watcher below.
        let mut config = config::load().unwrap_or_default();
        // Snapshot before reconciling: a removed account's alias-password
        // keyring entries are keyed by addresses only its config knows.
        let config_before_goa = config.clone();
        let goa_outcome = match crate::goa::live_state() {
            Some(live) => reconcile_goa(&mut config, &live),
            None => GoaReconcile::default(),
        };
        let goa_removed = goa_outcome.removed;
        if !goa_removed.is_empty() {
            for email in &goa_removed {
                match config_before_goa.iter().find(|c| &c.email == email) {
                    Some(acc) => config::delete_account_secrets(acc),
                    None => config::delete_password(email),
                }
            }
            sidebar_state.order.retain(|e| !goa_removed.contains(e));
            sidebar_state.collapsed.retain(|e| !goa_removed.contains(e));
            sidebar_state.folders_expanded.retain(|e| !goa_removed.contains(e));
            sidebar_state.filtered_expanded_accounts.retain(|e| !goa_removed.contains(e));
            sidebar_state.tags_expanded_accounts.retain(|e| !goa_removed.contains(e));
            config::save_sidebar_state(&sidebar_state);
        }
        if !goa_removed.is_empty() || goa_outcome.paused_changed {
            let _ = config::save(&config);
        }
        let order = sidebar_state.order;
        let collapsed = sidebar_state.collapsed;
        let folders_expanded = sidebar_state.folders_expanded;
        let filtered_expanded_accounts = sidebar_state.filtered_expanded_accounts;
        let tags_expanded_accounts = sidebar_state.tags_expanded_accounts;
        let tree_collapsed = sidebar_state.tree_collapsed;

        // Whether this run serves the built-in sample data (see spawn_workers):
        // decided after the GOA reconcile, which can add accounts.
        let demo_data = demo_mode() && config.is_empty();
        let show_attachments = config::load_show_attachments();
        let show_contacts = config::load_show_contacts();
        let sidebar = Sidebar::builder()
            .launch(SidebarInit {
                collapsed: rail_now,
                mirror: false,
                unified_expanded,
                filtered_expanded,
                tags_expanded,
                starred_expanded,
                sent_expanded,
                drafts_expanded,
                archive_expanded,
                show_attachments,
                show_contacts,
            })
            .forward(sender.input_sender(), sidebar_output_msg);
        // The peek panel's rows: a second, always-expanded instance.
        let peek_sidebar = Sidebar::builder()
            .launch(SidebarInit {
                collapsed: false,
                mirror: true,
                unified_expanded,
                filtered_expanded,
                tags_expanded,
                starred_expanded,
                sent_expanded,
                drafts_expanded,
                archive_expanded,
                show_attachments,
                show_contacts,
            })
            .forward(sender.input_sender(), sidebar_output_msg);

        let message_list =
            MessageList::builder()
                .launch(())
                .forward(sender.input_sender(), |out| match out {
                    MessageListOutput::Selected { message, thread, solo } => {
                        AppMsg::MessageSelected { message, thread, solo }
                    }
                    MessageListOutput::SelectionKeys(keys) => AppMsg::SelectionKeys(keys),
                    MessageListOutput::DeleteThread { messages } => {
                        AppMsg::DeleteThread(messages)
                    }
                    MessageListOutput::ThreadGrew { message, thread } => {
                        AppMsg::ThreadGrew { message: Box::new(message), thread }
                    }
                    MessageListOutput::ThreadsListed { groups } => {
                        AppMsg::ThreadsListed { groups }
                    }
                    MessageListOutput::CountChanged(text) => AppMsg::ListCount(text),
                    MessageListOutput::Activated { message, thread } => {
                        AppMsg::OpenMessageWindow { message, thread }
                    }
                    MessageListOutput::Action { action, message, conversation } => {
                        AppMsg::RowAction { action, message, conversation }
                    }
                    MessageListOutput::SetTag { message, keyword, add } => {
                        AppMsg::SetTag { message, keyword, add }
                    }
                    MessageListOutput::Bulk { action, messages } => {
                        AppMsg::Bulk { action, messages }
                    }
                    MessageListOutput::MoveTo { messages, offer_whole, x, y } => {
                        AppMsg::ListMoveTo { messages, offer_whole, x, y }
                    }
                    MessageListOutput::SelectionCleared => AppMsg::ClearReader,
                    MessageListOutput::SearchActive(active) => AppMsg::SearchActive(active),
                });

        let message_view =
            MessageView::builder()
                .launch(())
                .forward(sender.input_sender(), |out| match out {
                    MessageViewOutput::AllowSender(addr) => AppMsg::AllowSender(addr),
                    MessageViewOutput::SetRemote { account_id, id, show } => {
                        AppMsg::SetRemoteContent { account_id, id, show }
                    }
                    MessageViewOutput::OpenWindow(m) => {
                        AppMsg::OpenMessageWindow { message: *m, thread: Vec::new() }
                    }
                    MessageViewOutput::CardAction { action, message } => {
                        AppMsg::CardAction { action, message }
                    }
                    MessageViewOutput::ContactSender(m) => AppMsg::CardContact(m),
                    MessageViewOutput::AttachmentAction { account_id, id, index, save } => {
                        AppMsg::CardAttachment { account_id, id, index, save }
                    }
                    MessageViewOutput::CardMenu { message, x, y } => {
                        AppMsg::CardMenu { message, x, y }
                    }
                    MessageViewOutput::CardMoveTo { message, x, y } => AppMsg::ListMoveTo {
                        messages: vec![*message],
                        offer_whole: false,
                        x,
                        y,
                    },
                    MessageViewOutput::MarkSeen { account_id, id } => {
                        AppMsg::ThreadMessageSeen { account_id, id }
                    }
                    MessageViewOutput::SelectCards(keys) => AppMsg::SelectCards(keys),
                    MessageViewOutput::ComposeTo(addr) => AppMsg::ComposeTo(addr),
                    MessageViewOutput::AddContactAddr(addr) => AppMsg::AddContactAddr(addr),
                    MessageViewOutput::ReloadBody(m) => AppMsg::ReloadBody(m),
                    MessageViewOutput::Notice(text) => AppMsg::Notice(text),
                    MessageViewOutput::ReaderMode(on) => AppMsg::SetReaderMode(on),
                    MessageViewOutput::ZoomReset => AppMsg::ZoomMessage(0),
                    MessageViewOutput::Unsubscribe { message, info } => {
                        AppMsg::Unsubscribe { message, info }
                    }
                    MessageViewOutput::InviteAction { message, invite, action } => {
                        AppMsg::InviteAction { message, invite, action }
                    }
                });

        // The drawer owns a Paned whose top pane is the reader body, so hand it
        // the message-view widget to dock beneath.
        let attachment_drawer = AttachmentDrawer::builder()
            .launch(crate::ui::attachment_drawer::DrawerInit {
                state: config::load_drawer_state(),
                reader: message_view.widget().clone().upcast(),
            })
            .forward(sender.input_sender(), |out| match out {
                crate::ui::attachment_drawer::DrawerOutput::ShowLightbox { items, start } => {
                    AppMsg::ShowLightbox { items, start }
                }
                crate::ui::attachment_drawer::DrawerOutput::ShowInMessage(att) => {
                    AppMsg::ShowAttachmentInMessage(att)
                }
            });

        let gallery =
            AttachmentsGallery::builder()
                .launch(())
                .forward(sender.input_sender(), |out| match out {
                    GalleryOutput::OpenMessage { account_id, folder_path, uid } => {
                        AppMsg::OpenAttachmentMessage { account_id, folder_path, uid }
                    }
                    GalleryOutput::Load(req) => AppMsg::GalleryLoad(Box::new(req)),
                    GalleryOutput::Fetch { account_id, folder_path, uid } => {
                        AppMsg::GalleryFetch { account_id, folder_path, uid }
                    }
                });

        // Built here (not in the model literal) because the contacts page
        // mounts it over its detail pane at launch.
        let contacts_compose_revealer = {
            let r = gtk::Revealer::new();
            r.set_transition_type(gtk::RevealerTransitionType::SlideDown);
            r.set_transition_duration(300);
            r.set_reveal_child(false);
            r.set_can_target(false);
            r
        };
        let contacts_page =
            ContactsPage::builder()
                .launch(contacts_compose_revealer.clone())
                .forward(sender.input_sender(), |out| match out {
                    ContactsPageOutput::Compose(email) => AppMsg::ComposeTo(email),
                    ContactsPageOutput::ToggleSidebar => AppMsg::ToggleSidebar,
                    ContactsPageOutput::SaveContact { book_uid, vcard } => {
                        AppMsg::SaveContact { book_uid, vcard }
                    }
                    ContactsPageOutput::CreateContact { vcard } => {
                        AppMsg::CreateContact(vcard)
                    }
                    ContactsPageOutput::DeleteContact { book_uid, uid } => {
                        AppMsg::DeleteContact { book_uid, uid }
                    }
                    ContactsPageOutput::ShowPhoto { name, data } => {
                        // The lightbox routes by extension — a bare contact
                        // name sent the JPEG down the PDF path, where poppler
                        // hung the UI trying to parse it. Name it by content.
                        let name = format!("{name}.{}", crate::models::image_ext(&data));
                        AppMsg::ShowLightbox {
                            items: vec![crate::models::Attachment { name, data }],
                            start: 0,
                        }
                    }
                });

        let notifications = NotificationCenter::builder().launch(()).forward(
            sender.input_sender(),
            |out| match out {
                NotifyOutput::CountChanged(n) => AppMsg::NotifyCount(n),
                NotifyOutput::ExportLog => AppMsg::ExportLog,
            },
        );

        // Sectioned: settings / printing / window & help / quit.
        let menu = gtk::gio::Menu::new();
        let help_menu = gtk::gio::Menu::new();
        let model_undo_menu = gtk::gio::Menu::new();
        {
            // #200: the only place undo and redo are visible. They name the
            // action they would reverse ("Undo Archive") and grey out when
            // there is nothing on the stack.
            menu.append_section(None, &model_undo_menu);

            let settings = gtk::gio::Menu::new();
            settings.append(Some(i18n("Settings").as_str()), Some("win.accounts"));
            menu.append_section(None, &settings);

            // The sidebar's account sections, for those who work from the
            // unified section alone (also Settings → Sidebar).
            let sidebar = gtk::gio::Menu::new();
            sidebar.append(Some(i18n("Show Accounts").as_str()), Some("app.show-accounts"));
            // Focus Mode: the distraction-free layout (also Ctrl+Shift+F
            // and Settings → Appearance, where its parts are chosen).
            sidebar.append(Some(i18n("Focus Mode").as_str()), Some("app.focus-mode"));
            menu.append_section(None, &sidebar);

            let printing = gtk::gio::Menu::new();
            printing.append(Some(i18n("Print Preview…").as_str()), Some("win.print-preview"));
            printing.append(Some(i18n("Print Message…").as_str()), Some("win.print"));
            menu.append_section(None, &printing);

            menu.append_section(None, &help_menu);

            // Last, where a Quit item belongs.
            let quit = gtk::gio::Menu::new();
            quit.append(Some(i18n("Quit").as_str()), Some("app.quit"));
            menu.append_section(None, &quit);
        }

        let show_accounts = config::load_show_accounts();
        let show_accounts_action = gtk::gio::SimpleAction::new_stateful(
            "show-accounts",
            None,
            &show_accounts.to_variant(),
        );
        {
            let s = sender.clone();
            show_accounts_action.connect_change_state(move |action, value| {
                if let Some(on) = value.and_then(|v| v.get::<bool>()) {
                    action.set_state(&on.to_variant());
                    s.input(AppMsg::SetShowAccounts(on));
                }
            });
        }

        let focus_action = gtk::gio::SimpleAction::new_stateful(
            "focus-mode",
            None,
            &focus.enabled.to_variant(),
        );
        {
            let s = sender.clone();
            focus_action.connect_change_state(move |action, value| {
                if let Some(on) = value.and_then(|v| v.get::<bool>()) {
                    action.set_state(&on.to_variant());
                    let mut focus = config::load_focus_mode();
                    focus.enabled = on;
                    s.input(AppMsg::SetFocusMode(focus));
                }
            });
        }

        let reader_override = config::load_reader_override();
        let plain_style = config::load_plain_style();
        let mut model = AppModel {
            workers: HashMap::new(),
            mid_searches: HashMap::new(),
            tag_scan: None,
            tag_scan_gen: 0,
            demo_config: if demo_mode() && config.is_empty() { demo_account_configs_saved() } else { Vec::new() },
            config,
            window: root.clone(),
            prefs: None,
            accounts_win: None,
            accounts_seed: None,
            composers: Vec::new(),
            reader_compose: None,
            draining_composers: Vec::new(),
            reader_split_top: {
                let r = gtk::Revealer::new();
                r.set_transition_type(gtk::RevealerTransitionType::SlideDown);
                r.set_transition_duration(300);
                r.set_reveal_child(false);
                r
            },
            reader_split_bottom: {
                let r = gtk::Revealer::new();
                r.set_transition_type(gtk::RevealerTransitionType::SlideUp);
                r.set_transition_duration(300);
                r.set_reveal_child(false);
                r
            },
            reader_split_lower: {
                let p = gtk::Paned::new(gtk::Orientation::Vertical);
                // Same invisible separator as the outer split: the bottom
                // panel's grab pill is its visible affordance too.
                p.add_css_class("reader-split");
                p
            },
            split_close_anim: std::rc::Rc::new(std::cell::RefCell::new(None)),
            reader_header: std::cell::OnceCell::new(),
            reader_split: {
                let p = gtk::Paned::new(gtk::Orientation::Vertical);
                // The separator itself is styled invisible — painted the
                // composer's own ground, so the panel reads as running to its
                // edge. The visible affordance is the floating grab pill the
                // split slot overlays on the composer (open_inline_reply).
                p.add_css_class("reader-split");
                p
            },
            reader_compose_revealer: {
                let r = gtk::Revealer::new();
                r.set_transition_type(gtk::RevealerTransitionType::SlideDown);
                r.set_transition_duration(300);
                r.set_reveal_child(false);
                r
            },
            contacts_compose_revealer,
            next_compose_id: 1,
            menu,
            help_menu,
            undo_menu: model_undo_menu.clone(),
            accounts: Vec::new(),
            folders: HashMap::new(),
            account_order: order,
            collapsed,
            folders_expanded,
            tree_collapsed,
            selected: None,
            attachments: Vec::new(),
            lightbox_items: Vec::new(),
            lightbox_pos: 0,
            lightbox_texture: None,
            lightbox_open: std::rc::Rc::new(std::cell::Cell::new(false)),
            lightbox_zoom: 1,
            lightbox_picture: None,
            lightbox_scroller: None,
            attachments_loading: false,
            reader_actions_collapsed: false,
            reader_toolbar: config::load_reader_toolbar(),
            reader_toolbar_widgets: std::cell::OnceCell::new(),
            reader_overflow_btn: {
                let b = gtk::Button::from_icon_name(
                    "co.hyprlab.Hylki-view-more-horizontal-symbolic",
                );
                b.set_tooltip_text(Some(i18n("Actions").as_str()));
                b.add_css_class("flat");
                b.set_visible(false);
                b
            },
            reader_tag_btn: {
                let b = gtk::Button::from_icon_name("co.hyprlab.Hylki-tag-outline-symbolic");
                b.set_tooltip_text(Some(i18n("Tags").as_str()));
                b.add_css_class("flat");
                b
            },
            reader_move_btn: {
                let b = gtk::Button::from_icon_name("co.hyprlab.Hylki-folder-symbolic");
                b.set_tooltip_text(Some(i18n("Move To…").as_str()));
                b.add_css_class("flat");
                b
            },
            attachment_cache: crate::ram_cache::RamCache::new(ATTACHMENT_CACHE_BUDGET),
            unified: false,
            unified_view: UnifiedView::Kind(FolderKind::Inbox),
            unified_slices: HashMap::new(),
            unified_boot_requested: HashSet::new(),
            sent_index_requested: HashSet::new(),
            message_cache: HashMap::new(),
            indexed_folders: HashSet::new(),
            body_cache: crate::ram_cache::RamCache::new(BODY_CACHE_BUDGET),
            sender_cache: HashMap::new(),
            pending_draft: None,
            pending_reply: None,
            pending_draft_pick: None,
            pending_edit_as_new: None,
            pending_forward: None,
            pending_notified_reply: None,
            files_prefs: config::load_files_prefs(),
            link_browser: {
                // The launcher reads its choice from here, not from disk, so
                // it is handed over before the first link can be clicked.
                let choice = config::load_link_browser();
                crate::ui::launch::set_browser(&choice);
                choice
            },
            popouts: HashMap::new(),
            current_thread: Vec::new(),
            list_selection: Vec::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            compose_history: (None, None),
            carried_bodies: HashMap::new(),
            remote_override: HashMap::new(),
            carried_threads: HashMap::new(),
            undo_action: None,
            redo_action: None,
            bulk_pending: 0,
            related_id_seq: u32::MAX,
            related_ids: HashMap::new(),
            folder_unread: HashMap::new(),
            pending_seen: HashMap::new(),
            sidebar_split: None,
            app_title: None,
            sidebar_menu: None,
            sidebar_header: None,
            sidebar_refresh: {
                let b = gtk::Button::new();
                b.set_tooltip_text(Some(i18n("Refresh or long-press for Status Bar").as_str()));
                b.add_css_class("flat");
                b.set_valign(gtk::Align::Center);
                b
            },
            sidebar_refresh_stack: {
                let s = gtk::Stack::new();
                s.set_transition_type(gtk::StackTransitionType::Crossfade);
                s
            },
            sidebar_refresh_spinner: gtk::Spinner::new(),
            sidebar_collapsed: icon_only,
            sidebar_anim: None,
            auto_rail: false,
            rail_active: rail_now,
            sidebar_peek: false,
            peek_transition: std::rc::Rc::new(std::cell::Cell::new(false)),
            peek_split: None,
            peek_refresh_stack: {
                let s = gtk::Stack::new();
                s.set_transition_type(gtk::StackTransitionType::Crossfade);
                s
            },
            peek_refresh_spinner: gtk::Spinner::new(),
            sidebar_hover_expand: config::load_sidebar_hover_expand(),
            remember_sidebar,
            remember_rail,
            unified_expanded,
            filtered_expanded,
            tags_expanded,
            starred_expanded,
            sent_expanded,
            drafts_expanded,
            archive_expanded,
            filtered_expanded_accounts,
            tags_expanded_accounts,
            rail_dots: config::load_rail_dots(),
            rail_fold: config::load_rail_fold(),
            app_theme: config::load_app_theme(),
            theme: config::load_theme(),
            current: None,
            allowed_senders: config::load_allowed_senders(),
            unsubscribed: config::load_unsubscribed(),
            invite_answers: config::load_invite_answers(),
            auto_remote_content: config::load_auto_remote_content(),
            show_remote_banner: config::load_show_remote_banner(),
            blacklist: config::load_blacklist(),
            palette_collapse_secs: config::load_palette_collapse(),
            card_palette_collapse_secs: config::load_card_palette_collapse(),
            gravatar: config::load_gravatar(),
            avatars: config::load_avatars(),
            own_mailbox_face: config::load_own_mailbox_face(),
            sender_logos: config::load_sender_logos(),
            date_style: config::load_date_format().0,
            clock_style: config::load_date_format().1,
            fetch_interval_secs: config::load_fetch_interval(),
            push: config::load_push(),
            notifications_enabled: config::load_notifications(),
            notification_content: config::load_notification_content(),
            notification_buttons: config::load_notification_buttons(),
            show_attachments,
            show_contacts,
            settings_open_accounts: config::load_settings_open_accounts(),
            last_settings_page: None,
            list_count: String::new(),
            preview_lines: config::load_preview_lines(),
            shortcuts_win: None,
            run_in_background: std::rc::Rc::new(std::cell::Cell::new(
                config::load_run_in_background(),
            )),
            autostart: config::load_autostart(),
            tray_enabled: config::load_tray(),
            tray_icon: config::load_tray_icon(),
            tray_mail: config::load_tray_mail(),
            app_icon: crate::app_icon::init_on_startup(),
            restart_pending: false,
            tray: None,
            tray_mail_key: std::cell::RefCell::new(None),
            single_key: std::rc::Rc::new(std::cell::Cell::new(
                config::load_single_key_shortcuts(),
            )),
            threading: config::load_threading(),
            thread_render_queued: false,
            thread_opened_at: None,
            thread_related_pending: false,
            thread_painted: false,
            thread_cache: HashMap::new(),
            thread_cache_order: Vec::new(),
            thread_key: None,
            threads_expanded: config::load_threads_expanded(),
            thread_newest_first: config::load_thread_newest_first(),
            always_show_recipients: config::load_always_show_recipients(),
            show_unified_pref: config::load_show_unified(),
            unified_chips: config::load_unified_chips(),
            unified_filtered: config::load_unified_filtered(),
            filtered_placement: config::load_filtered_placement(),
            tags_placement: config::load_tags_placement(),
            unified_kinds: config::load_unified_kinds(),
            unified_tags: config::load_unified_tags(),
            show_accounts,
            show_accounts_action,
            focus,
            focus_action,
            list_header_widgets: std::cell::OnceCell::new(),
            chevrons_left: config::load_chevrons_left(),
            console_mode: config::load_console_mode(),
            read_mark: config::load_read_mark(),
            // The demo (no accounts of its own) ships with tags and filter
            // rules, so its sidebar shows the Tags and Filtered Folders
            // sections; a staged tags.toml / filters.toml still wins.
            filters: {
                let filters = config::load_filters();
                if filters.is_empty() && demo_data { demo_filters() } else { filters }
            },
            tags: {
                let tags = config::load_tags();
                if tags.is_empty() && demo_data { demo_tags() } else { tags }
            },
            tag_view: None,
            tag_view_cache: HashMap::new(),
            tag_view_loading: false,
            tag_view_dirty: false,
            keyword_sync_at: HashMap::new(),
            tag_provider: gtk::CssProvider::new(),
            // Demo mode gets a cache that never touches disk, seeded with the
            // sample mail: the gallery's paging, search and sort are database
            // queries, so the demo has to run them against a real database or
            // it would be exercising nothing.
            cache: if demo_mode() {
                let c = crate::cache::Cache::in_memory().ok();
                if let Some(c) = c.as_ref() {
                    crate::backend::seed_demo_cache(c);
                }
                c
            } else {
                crate::cache::Cache::open().ok()
            },
            filter_run: None,
            filter_moved: Default::default(),
            body_hits: Default::default(),
            single_message_card: config::load_single_message_card(),
            reader_mode: config::load_reader_mode(),
            zoom: config::load_reader_zoom(),
            zoom_default: config::load_reader_zoom(),
            reader_switch: config::load_reader_switch(),
            reader_default: config::load_reader_default(),
            card_attachments: config::load_card_attachments(),
            drawer_enabled: config::load_attachment_drawer(),
            thread_expansion: config::load_thread_expansion(),
            confirm_thread_delete: config::load_confirm_thread_delete(),
            selection_from_cards: false,
            card_actions_hover: config::load_card_actions_hover(),
            card_actions_auto: config::load_card_actions_auto(),
            list_palette: config::load_list_palette(),
            list_palette_hover: config::load_list_palette_hover(),
            card_palette_menu: config::load_card_palette_menu(),
            swipe_enabled: config::load_swipe_enabled(),
            swipe_reversed: config::load_swipe_reversed(),
            swipe_sensitivity: config::load_swipe_sensitivity(),
            compose_inline: config::load_compose_inline(),
            reply_fields: config::load_reply_fields(),
            compose_default_from: config::load_compose_default_from(),
            paste_plain: config::load_paste_plain(),
            compose_format: config::load_compose_format(),
            reply_position: config::load_reply_position(),
            signature_position: config::load_signature_position(),
            spellcheck: config::load_spellcheck(),
            spellcheck_langs: config::load_spellcheck_langs(),
            message_theme: config::load_message_theme(),
            override_fonts: reader_override.0,
            reader_font: reader_override.1,
            override_colors: reader_override.2,
            plain_monospace: plain_style.0,
            plain_font: plain_style.1,
            auto_fetch_source: None,
            notifications,
            welcome: None,
            notify_count: 0,
            busy: HashSet::new(),
            sidebar,
            peek_sidebar,
            message_list,
            message_view,
            attachment_drawer,
            gallery,
            showing_gallery: false,
            contacts_page,
            showing_contacts: false,
            showing_outbox: false,
            outbox_by_account: HashMap::new(),
            gallery_total: 0,
            gallery_scan_left: HashMap::new(),
        };
        model.prime_from_cache();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &model.tag_provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        model.refresh_tag_css();
        model.message_list.emit(MessageListInput::SetTags(model.tags.clone()));
        model.message_view.emit(MessageViewInput::SetTags(model.tags.clone()));
        // What each mailbox of the user's own shows (#189), before the first
        // cached message is painted: the sidebar refreshes this whenever the
        // accounts change, but nothing has rebuilt it yet at this point.
        model.refresh_own_faces();
        model.warm_own_gravatars(&sender);
        model.spawn_workers(&sender);
        if model.tray_enabled {
            model.start_tray(&sender);
        }
        // Refresh visible avatars when the GNOME Contacts photo index
        // changes (first load finishing, or an EDS/CardDAV sync).
        crate::contacts::watch_photo_changes({
            let input = sender.input_sender().clone();
            move || {
                let _ = input.send(AppMsg::ContactPhotosChanged);
            }
        });
        // Watch GNOME Online Accounts so a change there (account removed, Mail
        // toggled) is reflected in Hylki live, no restart needed. The watcher
        // debounces signal bursts and snapshots GOA on its own thread;
        // reconciliation happens on GoaChanged.
        crate::goa::watch_changes({
            let s = sender.input_sender().clone();
            move |state| {
                let _ = s.send(AppMsg::GoaChanged(state));
            }
        });
        // Watch for resume-from-sleep: suspended IMAP sockets die silently, so
        // on wake we reconnect every worker and refresh, otherwise no new mail
        // arrives until the app is restarted.
        crate::power::watch_resume({
            let s = sender.input_sender().clone();
            move || {
                let _ = s.send(AppMsg::SystemResumed);
            }
        });
        // With no accounts, no worker events will populate the sidebar, so render
        // its empty state (the "Add first account" prompt) up front — and greet
        // a first run with the welcome wizard (src/ui/welcome.rs).
        // A restart the wizard's language pick asked for: the wizard comes
        // back once, whatever mode this is; the flag must not reach a
        // later restart (the icon change's), so it is cleared here.
        let wizard_again = std::env::var_os(crate::WIZARD_AGAIN_VAR).is_some();
        if wizard_again {
            std::env::remove_var(crate::WIZARD_AGAIN_VAR);
        }
        if model.config.is_empty() || std::env::var("HYLKI_WELCOME").is_ok() || wizard_again {
            model.rebuild_sidebar();
            // HYLKI_WELCOME=1 forces the wizard over an existing config, for
            // design review and screenshots. A wizard already completed once
            // (Start Reading pressed, even with no account added) doesn't
            // come back on its own — a restart right after it must not loop.
            if std::env::var("HYLKI_WELCOME").is_ok()
                || wizard_again
                || (!demo_mode() && !config::wizard_completed())
            {
                model.open_wizard(&sender);
            }
        }
        model
            .message_view
            .emit(MessageViewInput::SetReadMark(model.read_mark));
        model
            .message_list
            .emit(MessageListInput::SetGravatar(model.gravatar));
        model
            .message_list
            .emit(MessageListInput::SetAvatars(model.list_avatars()));
        // The formatter is a free function, so the preference has to be handed to
        // it before anything draws a date.
        model
            .message_list
            .emit(MessageListInput::SetSenderLogos(model.sender_logos));
        crate::datefmt::set_style(model.date_style, model.clock_style);
        model.message_list.emit(MessageListInput::SetLook {
            avatars: model.list_avatars(),
            preview_lines: model.list_preview_lines(),
            subject: model.list_show_subject(),
            animate: false,
        });
        model.sidebars_emit(SidebarInput::SetFocus {
            hide_accounts: model.focus.active(config::FocusPart::HideAccounts),
            fold_unified: model.focus.active(config::FocusPart::FoldUnified),
            animate: false,
        });
        model
            .message_list
            .emit(MessageListInput::SetThreading(model.threading));
        model
            .message_list
            .emit(MessageListInput::SetThreadExpansion(model.thread_expansion));
        model
            .message_list
            .emit(MessageListInput::SetListPalette(model.list_palette));
        model
            .message_list
            .emit(MessageListInput::SetThreadsExpanded(model.threads_expanded));
        model
            .message_list
            .emit(MessageListInput::SetPaletteCollapse(model.palette_collapse_secs));
        model
            .message_view
            .emit(MessageViewInput::SetContentTheme(model.message_theme.dark_override()));
        model.message_view.emit(MessageViewInput::SetReaderStyle(model.reader_style()));
        model
            .message_view
            .emit(MessageViewInput::SetAlwaysShowRecipients(model.always_show_recipients));
        model
            .message_view
            .emit(MessageViewInput::SetSingleMessageCard(model.single_message_card));
        model.message_view.emit(MessageViewInput::SetReaderMode(model.effective_reader_mode()));
        model.message_view.emit(MessageViewInput::SetZoomDefault(model.zoom_default));
        model.message_view.emit(MessageViewInput::SetZoom(model.zoom));
        model.message_view.emit(MessageViewInput::SetReaderSwitchShown(model.reader_switch));
        model.message_view.emit(MessageViewInput::SetReaderDefault(model.effective_reader_default()));
        model
            .message_view
            .emit(MessageViewInput::SetCardAttachmentsShown(model.card_attachments));
        model
            .message_view
            .emit(MessageViewInput::SetAttachmentDrawer(model.drawer_enabled));
        model.message_view.emit(MessageViewInput::SetUnsubscribed(model.unsubscribed_map()));
        model.message_view.emit(MessageViewInput::SetIdentities(model.identities_map()));
        model
            .message_view
            .emit(MessageViewInput::SetInviteAnswers(model.invite_answers_map()));
        model.arm_auto_fetch(&sender);

        // The app-wide theme choice must be in force before the first frame.
        apply_app_theme(model.app_theme);
        let reader_tag_btn = model.reader_tag_btn.clone();
        let reader_move_btn = model.reader_move_btn.clone();
        let widgets = view_output!();
        let _ = model.reader_header.set(widgets.reader_header.clone());
        // Right-click on the header's empty space (not a button) offers
        // the way to the toolbar editor.
        {
            let click = gtk::GestureClick::new();
            click.set_button(3);
            // Capture phase: the header's own window handle (a child, so
            // earlier in the bubble phase) would otherwise take the click
            // for the compositor's window menu and never let it through.
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            let header: gtk::Widget = widgets.reader_header.clone().upcast();
            let s = sender.input_sender().clone();
            click.connect_pressed(move |g, _, x, y| {
                // Anything clickable under the pointer keeps its own
                // right-click (or lack of one).
                let mut w = header.pick(x, y, gtk::PickFlags::DEFAULT);
                while let Some(cur) = w {
                    if cur == header {
                        break;
                    }
                    if cur.is::<gtk::Button>() || cur.is::<gtk::WindowControls>() {
                        return;
                    }
                    w = cur.parent();
                }
                g.set_state(gtk::EventSequenceState::Claimed);
                let _ = s.send(AppMsg::ReaderToolbarMenu { x, y });
            });
            widgets.reader_header.add_controller(click);
        }
        // Collapse the reader header's actions into the overflow menu when the
        // pane can no longer fit the full row — squeezing it further must never
        // push the window controls off the right edge. The threshold is
        // measured, not assumed: the window-controls cost depends on the
        // user's decoration layout (GNOME ships one button, others run three),
        // so the row's cost is taken from the real headerbar now — while every
        // action is still visible, before the breakpoint could hide any — and
        // the controls' share is re-derived if the decoration layout changes.
        {
            // The controls' width comes from the headerbar's own live
            // GtkWindowControls children (a detached WindowControls never
            // populates its buttons and measures 0).
            fn measure_controls(w: &gtk::Widget, sum: &mut i32) {
                if let Some(c) = w.downcast_ref::<gtk::WindowControls>() {
                    *sum += c.measure(gtk::Orientation::Horizontal, -1).1;
                    return;
                }
                let mut child = w.first_child();
                while let Some(c) = child {
                    measure_controls(&c, sum);
                    child = c.next_sibling();
                }
            }
            let header: gtk::Widget = widgets.reader_header.clone().upcast();
            let full = header.measure(gtk::Orientation::Horizontal, -1).1;
            let mut controls = 0;
            measure_controls(&header, &mut controls);
            use config::ToolbarItem as T;
            let buttons: Vec<(T, gtk::Widget)> = vec![
                (T::Reply, widgets.tb_reply.clone().upcast()),
                (T::ReplyAll, widgets.tb_reply_all.clone().upcast()),
                (T::Forward, widgets.tb_forward.clone().upcast()),
                (T::Star, widgets.tb_star.clone().upcast()),
                (T::Archive, widgets.tb_archive.clone().upcast()),
                (T::Delete, widgets.tb_delete.clone().upcast()),
                (T::Spam, widgets.tb_spam.clone().upcast()),
                (T::ReadUnread, widgets.tb_read.clone().upcast()),
                (T::Tags, widgets.tb_tags.clone().upcast()),
                (T::MoveTo, widgets.tb_move.clone().upcast()),
                (T::Find, widgets.tb_find.clone().upcast()),
                (T::Print, widgets.tb_print.clone().upcast()),
            ];
            // One button's cost, from the buttons themselves (a hidden one —
            // Tags before any tag exists — measures 0 and is skipped); what
            // is left of the row after them and the controls is the base.
            //
            // Two different widths, and the difference is load-bearing. Each
            // button sits in a Focus Mode revealer, and a revealer that has
            // not been revealed yet measures 0 however wide its child is —
            // which is exactly the state this runs in, before
            // `sync_focus_chrome` opens them. So a button's own cost comes
            // from the button inside the revealer, while what is taken off
            // `full` is what the row counted for it *just now*: the revealer.
            // Subtracting the buttons from a row that never included them is
            // how the base went negative (and the fold stopped happening) the
            // moment Focus Mode wrapped the row in revealers.
            let mut packed_sum = 0;
            let mut button_w = 0;
            for (_, w) in &buttons {
                packed_sum += w.measure(gtk::Orientation::Horizontal, -1).1;
                let inner = w.downcast_ref::<gtk::Revealer>().and_then(|r| r.child()).unwrap_or_else(|| w.clone());
                let nat = inner.measure(gtk::Orientation::Horizontal, -1).1;
                if nat > 0 {
                    button_w = button_w.max(nat);
                }
            }
            let base = if full <= 0 { 0 } else { (full - controls - packed_sum).max(0) };
            let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
                adw::BreakpointConditionLengthType::MaxWidth,
                READER_ACTIONS_BREAKPOINT,
                adw::LengthUnit::Px,
            ));
            let s = sender.input_sender().clone();
            bp.connect_apply(move |_| {
                let _ = s.send(AppMsg::SetReaderActionsCollapsed(true));
            });
            let s = sender.input_sender().clone();
            bp.connect_unapply(move |_| {
                let _ = s.send(AppMsg::SetReaderActionsCollapsed(false));
            });
            widgets.reader_bin.add_breakpoint(bp.clone());
            let _ = model.reader_toolbar_widgets.set(ReaderToolbarWidgets {
                buttons,
                outbox: vec![
                    widgets.tb_outbox_edit.clone(),
                    widgets.tb_outbox_send.clone(),
                    widgets.tb_outbox_send_all.clone(),
                ],
                spinner: widgets.tb_spinner.clone().upcast(),
                bin: widgets.reader_bin.clone(),
                breakpoint: bp,
                base,
                button_w,
                controls: std::cell::Cell::new(controls),
            });
            // Packs the saved order and sets the real threshold.
            model.relayout_reader_toolbar();
            if let Some(settings) = gtk::Settings::default() {
                let s = sender.input_sender().clone();
                settings.connect_notify_local(Some("gtk-decoration-layout"), move |_, _| {
                    // Re-measure in an idle: the headerbar's own controls
                    // rebuild on this same notify, in unspecified order.
                    let header = header.clone();
                    let s = s.clone();
                    gtk::glib::idle_add_local_once(move || {
                        let mut controls = 0;
                        measure_controls(&header, &mut controls);
                        let _ = s.send(AppMsg::ReaderControlsChanged(controls));
                    });
                });
            }
            let s = sender.input_sender().clone();
            model.reader_overflow_btn.connect_clicked(move |_| {
                let _ = s.send(AppMsg::ReaderOverflowMenu);
            });
            let s = sender.input_sender().clone();
            model.reader_tag_btn.connect_clicked(move |_| {
                let _ = s.send(AppMsg::ReaderTagMenu);
            });
            let s = sender.input_sender().clone();
            model.reader_move_btn.connect_clicked(move |_| {
                let _ = s.send(AppMsg::MoveToMenu);
            });
        }
        // The inline compose/reply pane is an overlay over the WHOLE reader
        // pane — its header bar included: revealed, it slides down from the
        // top and covers everything (the composer's own header carries the
        // window decorations meanwhile). While closed it must not eat the
        // reader's clicks. Wrapping happens here because the view macro's
        // deep nesting makes an in-place Overlay unwieldy.
        model.reader_compose_revealer.set_can_target(false);
        {
            let pane = widgets.reader_bin.child().expect("reader pane");
            widgets.reader_bin.set_child(None::<&gtk::Widget>);
            // Split-reply slot (#86) above the reader; the full-cover
            // revealer stays an overlay above the whole assembly. A Paned
            // rather than a Box so the composer's height is the divider
            // position — dragged by the user, immune to the editor's natural
            // height — instead of whatever the composer asks for.
            pane.set_vexpand(true);
            // The reader over the bottom slot (#212), the pair beneath the
            // top slot. Each slot is hidden while no split reply is open in
            // it, which hides its divider too.
            let lower = &model.reader_split_lower;
            lower.set_start_child(Some(&pane));
            lower.set_end_child(Some(&model.reader_split_bottom));
            lower.set_resize_start_child(true);
            lower.set_shrink_start_child(true);
            lower.set_resize_end_child(false);
            lower.set_shrink_end_child(false);
            lower.set_vexpand(true);
            model.reader_split_bottom.set_visible(false);
            let split = &model.reader_split;
            split.set_start_child(Some(&model.reader_split_top));
            split.set_end_child(Some(lower));
            // Window resizes go to the reader; the composer keeps its set
            // height and never shrinks below its minimum. The reader may
            // shrink — the divider clamp at open time is what keeps a slice
            // of it on screen.
            split.set_resize_start_child(false);
            split.set_shrink_start_child(false);
            split.set_resize_end_child(true);
            split.set_shrink_end_child(true);
            model.reader_split_top.set_visible(false);
            let overlay = gtk::Overlay::new();
            overlay.set_child(Some(split));
            overlay.add_overlay(&model.reader_compose_revealer);
            widgets.reader_bin.set_child(Some(&overlay));
        }
        // The attachments gallery is the content stack's second page. Wrap it in a
        // ToolbarView + HeaderBar so it keeps the window controls (close/minimize)
        // that otherwise live only on the reader pane's header.
        {
            use gtk::prelude::*;
            let gallery_tv = adw::ToolbarView::new();
            let gallery_hb = adw::HeaderBar::new();
            gallery_hb.add_css_class("flat");
            let title = gtk::Label::new(Some(i18n("Attachments").as_str()));
            title.add_css_class("pane-title");
            gallery_hb.set_title_widget(Some(&title));
            // Leftmost, same spot as the message list header's: the sidebar
            // collapse/expand toggle.
            let sidebar_btn =
                gtk::Button::from_icon_name("co.hyprlab.Hylki-sidebar-show-symbolic");
            sidebar_btn.set_tooltip_text(Some(i18n("Toggle sidebar").as_str()));
            sidebar_btn.add_css_class("flat");
            let s = sender.input_sender().clone();
            sidebar_btn.connect_clicked(move |_| {
                let _ = s.send(AppMsg::ToggleSidebar);
            });
            gallery_hb.pack_start(&sidebar_btn);
            gallery_tv.add_top_bar(&gallery_hb);
            gallery_tv.set_content(Some(model.gallery.widget()));
            widgets.content_stack.add_named(&gallery_tv, Some("gallery"));

            // The contacts view brings its own per-pane headers (so its
            // divider runs to the very top, like the mail panes').
            widgets
                .content_stack
                .add_named(model.contacts_page.widget(), Some("contacts"));
        }
        // Desktop-notification click actions: raise the window (error alerts) and
        // raise + open a specific message (new-mail alerts). Registered here rather
        // than in `notify` because opening a message needs the app's channel.
        {
            use gtk::prelude::*;
            let app = relm4::main_application();
            let present = gtk::gio::SimpleAction::new(crate::notify::PRESENT_ACTION, None);
            let win = model.window.clone();
            present.connect_activate(move |_, _| {
                crate::desktop::present_window(&win);
            });
            app.add_action(&present);

            // "Message ready" alerts (a hand-off whose window stayed behind):
            // the click raises the composer too, and the alert goes away by
            // itself once any window of ours has the focus.
            let present_compose =
                gtk::gio::SimpleAction::new(crate::notify::PRESENT_COMPOSE_ACTION, None);
            let psender = sender.clone();
            present_compose.connect_activate(move |_, _| {
                psender.input(AppMsg::PresentComposers);
            });
            app.add_action(&present_compose);
            model.window.connect_is_active_notify(|w| {
                if w.is_active() {
                    crate::notify::withdraw_compose_ready();
                }
            });

            let ty = gtk::glib::VariantTy::new("(uuu)").unwrap();
            let open = gtk::gio::SimpleAction::new(crate::notify::OPEN_MESSAGE_ACTION, Some(ty));
            let win = model.window.clone();
            let osender = sender.clone();
            open.connect_activate(move |_, param| {
                crate::desktop::present_window(&win);
                if let Some((account_id, folder_id, message_id)) =
                    param.and_then(|v| v.get::<(u32, u32, u32)>())
                {
                    osender.input(AppMsg::OpenMessageFromNotification {
                        account_id,
                        folder_id,
                        message_id,
                    });
                }
            });
            app.add_action(&open);

            // Notification buttons (#38): act on the message where it lies,
            // without presenting the window — that's their point.
            for (name, mk) in [
                (
                    crate::notify::MARK_READ_ACTION,
                    (&|account_id, folder_id, message_id| AppMsg::NotificationMarkRead {
                        account_id,
                        folder_id,
                        message_id,
                    }) as &dyn Fn(u32, u32, u32) -> AppMsg,
                ),
                (crate::notify::ARCHIVE_ACTION, &|account_id, folder_id, message_id| {
                    AppMsg::NotificationArchive { account_id, folder_id, message_id }
                }),
                (crate::notify::DELETE_ACTION, &|account_id, folder_id, message_id| {
                    AppMsg::NotificationDelete { account_id, folder_id, message_id }
                }),
                (crate::notify::SPAM_ACTION, &|account_id, folder_id, message_id| {
                    AppMsg::NotificationSpam { account_id, folder_id, message_id }
                }),
            ] {
                let act = gtk::gio::SimpleAction::new(name, Some(ty));
                let asender = sender.clone();
                act.connect_activate(move |_, param| {
                    if let Some((account_id, folder_id, message_id)) =
                        param.and_then(|v| v.get::<(u32, u32, u32)>())
                    {
                        asender.input(mk(account_id, folder_id, message_id));
                    }
                });
                app.add_action(&act);
            }
            // Reply and Forward (#244) need the window: the message opens
            // as a click on the notification would open it, and the
            // composer follows once its body is in.
            for (name, mk) in [
                (
                    crate::notify::REPLY_ACTION,
                    (&|account_id, folder_id, message_id| AppMsg::NotificationReply {
                        account_id,
                        folder_id,
                        message_id,
                    }) as &dyn Fn(u32, u32, u32) -> AppMsg,
                ),
                (crate::notify::FORWARD_ACTION, &|account_id, folder_id, message_id| {
                    AppMsg::NotificationForward { account_id, folder_id, message_id }
                }),
            ] {
                let act = gtk::gio::SimpleAction::new(name, Some(ty));
                let win = model.window.clone();
                let asender = sender.clone();
                act.connect_activate(move |_, param| {
                    crate::desktop::present_window(&win);
                    if let Some((account_id, folder_id, message_id)) =
                        param.and_then(|v| v.get::<(u32, u32, u32)>())
                    {
                        asender.input(AppMsg::OpenMessageFromNotification {
                            account_id,
                            folder_id,
                            message_id,
                        });
                        asender.input(mk(account_id, folder_id, message_id));
                    }
                });
                app.add_action(&act);
            }
        }
        // Restore the last window size + maximized state (Wayland can't restore
        // position/monitor).
        let (win_w, win_h, win_max) = config::load_window_state();
        root.set_default_size(win_w, win_h);
        if win_max {
            root.maximize();
        }
        // Below this width the expanded sidebar and a full-width Actions
        // Palette can't both fit (280 sidebar + 324 list + 492 reader), so the
        // sidebar drops to its icon rail automatically — this is what keeps the
        // palette whole when the window is tiled to half of a 1920px screen.
        // With a breakpoint present the window no longer derives its minimum
        // size from its content, so pin an explicit floor: the sidebar rail
        // (80) + the list's palette floor (324) + the reader header (~492).
        root.set_size_request(904, 360);
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            1094.0,
            adw::LengthUnit::Px,
        ));
        {
            let s = sender.clone();
            narrow.connect_apply(move |_| s.input(AppMsg::AutoRail(true)));
            let s = sender.clone();
            narrow.connect_unapply(move |_| s.input(AppMsg::AutoRail(false)));
        }
        root.add_breakpoint(narrow);
        {
            let s = sender.clone();
            let guard = model.peek_transition.clone();
            widgets.peek_split.connect_show_sidebar_notify(move |split| {
                tracing::info!(
                    "peek: show-sidebar notify shown={} guarded={}",
                    split.shows_sidebar(), guard.get()
                );
                // Only a *user* dismissal (scrim click / swipe) counts — our
                // own open/close transitions notify too.
                if !guard.get() && !split.shows_sidebar() {
                    s.input(AppMsg::SidebarPeekDismissed);
                }
            });
        }
        // Pointer tracking drives the hover peek: with the hover-expand
        // preference on, moving the pointer over the docked rail floats the
        // panel out. The peek never folds back on its own — the pointer
        // leaving used to arm a one-second dismissal, but the panel's menu
        // popover is its own surface, so merely opening it counted as
        // leaving and the panel slid away under the menu. Now only a click
        // outside the panel (the scrim), a swipe, or a navigation closes it.
        // The handler fires in every mode — the guards in the AppMsg handler
        // keep it meaningless outside a rail.
        if let Some(rail) = widgets.sidebar_split.sidebar() {
            let motion = gtk::EventControllerMotion::new();
            let armed = std::rc::Rc::new(std::cell::Cell::new(false));
            {
                let armed = armed.clone();
                motion.connect_enter(move |_, _, _| armed.set(true));
            }
            // Hover-open waits for the pointer to actually move over the
            // rail. GTK also synthesises an "enter" when the rail reappears
            // under a resting pointer as the panel slides away — opening on
            // that would fold and float forever.
            let s = sender.input_sender().clone();
            motion.connect_motion(move |_, _, _| {
                if armed.replace(false) {
                    let _ = s.send(AppMsg::SidebarHoverEnter);
                }
            });
            rail.add_controller(motion);
        }
        model.sidebar_split = Some(widgets.sidebar_split.clone());
        model.peek_split = Some(widgets.peek_split.clone());
        model.app_title = Some(widgets.app_title.clone());
        model.sidebar_header = Some(widgets.sidebar_header.clone());
        model.sidebar_menu = Some(widgets.sidebar_menu.clone());
        // The header Refresh's icon/spinner faces, and its click.
        {
            let icon = gtk::Image::from_icon_name("co.hyprlab.Hylki-view-refresh-symbolic");
            model.sidebar_refresh_stack.add_named(&icon, Some("icon"));
            model
                .sidebar_refresh_stack
                .add_named(&model.sidebar_refresh_spinner, Some("spinner"));
            model.sidebar_refresh_stack.set_visible_child_name("icon");
            model.sidebar_refresh.set_child(Some(&model.sidebar_refresh_stack));
            let s = sender.input_sender().clone();
            model.sidebar_refresh.connect_clicked(move |_| {
                let _ = s.send(AppMsg::Refresh);
            });
            // Long-press slides the status bar down — the place to see what
            // the spinner is spinning about. Claiming the sequence keeps the
            // release from also firing the button's click (a refresh).
            let long = gtk::GestureLongPress::new();
            let s = sender.input_sender().clone();
            long.connect_pressed(move |gesture, _, _| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let _ = s.send(AppMsg::ToggleNotifications);
            });
            model.sidebar_refresh.add_controller(long);
        }
        // The peek panel's Refresh: the same icon/spinner faces.
        {
            let icon = gtk::Image::from_icon_name("co.hyprlab.Hylki-view-refresh-symbolic");
            model.peek_refresh_stack.add_named(&icon, Some("icon"));
            model
                .peek_refresh_stack
                .add_named(&model.peek_refresh_spinner, Some("spinner"));
            model.peek_refresh_stack.set_visible_child_name("icon");
        }
        if model.rail_active {
            widgets.sidebar_split.set_min_sidebar_width(SIDEBAR_RAIL_WIDTH);
            widgets.sidebar_split.set_max_sidebar_width(SIDEBAR_RAIL_WIDTH);
            set_sidebar_header_compact(
                &widgets.sidebar_header,
                &widgets.app_title,
                &widgets.sidebar_menu,
                &model.sidebar_refresh,
                true,
            );
        }

        // GNOME's Background Apps menu quits an app by activating its `quit`
        // action over D-Bus, falling back to `flatpak kill` if nothing answers
        // within five seconds. Without this the ✕ there would be a hard kill.
        {
            let app = relm4::main_application();
            app.add_action(&model.show_accounts_action);
            app.add_action(&model.focus_action);
            let window = root.clone();
            let quit = gtk::gio::SimpleAction::new("quit", None);
            quit.connect_activate(move |_, _| {
                // Same teardown as closing the last window: save the geometry,
                // then exit rather than unwinding through GTK and WebKit.
                let maximized = window.is_maximized();
                let (width, height) = if maximized {
                    let (sw, sh, _) = crate::config::load_window_state();
                    (sw, sh)
                } else {
                    (window.width(), window.height())
                };
                crate::config::save_window_state(width, height, maximized);
                crate::background::set_status("");
                std::process::exit(0)
            });
            app.add_action(&quit);
            gtk::prelude::GtkApplicationExt::set_accels_for_action(&app, "app.quit", &["<Ctrl>q"]);
            // Ctrl+Shift+A shows or hides the sidebar's account sections
            // (the same switch as the main menu's and Settings'): activating
            // the stateful action without a value toggles it.
            gtk::prelude::GtkApplicationExt::set_accels_for_action(
                &app,
                "app.show-accounts",
                &["<Ctrl><Shift>a"],
            );
            // Ctrl+Shift+F switches Focus Mode on and off.
            gtk::prelude::GtkApplicationExt::set_accels_for_action(
                &app,
                "app.focus-mode",
                &["<Ctrl><Shift>f"],
            );
            // Ctrl+ / Ctrl- / Ctrl+0: message zoom, the bodies alone. The
            // = key stands in for + on layouts where + needs Shift, and the
            // keypad's keys count too, as in every browser.
            for (name, step, keys) in [
                ("zoom-in", 1i8, &["<Ctrl>plus", "<Ctrl>equal", "<Ctrl>KP_Add"][..]),
                ("zoom-out", -1, &["<Ctrl>minus", "<Ctrl>KP_Subtract"][..]),
                ("zoom-reset", 0, &["<Ctrl>0", "<Ctrl>KP_0"][..]),
            ] {
                let action = gtk::gio::SimpleAction::new(name, None);
                let s = sender.clone();
                action.connect_activate(move |_, _| s.input(AppMsg::ZoomMessage(step)));
                app.add_action(&action);
                gtk::prelude::GtkApplicationExt::set_accels_for_action(&app, &format!("app.{name}"), keys);
            }
            // Ctrl+W closes the window only (issue #64): with "run in the
            // background" on, mail keeps arriving — unlike Ctrl+Q, which
            // quits outright. GTK's built-in window.close action does
            // exactly the same as the titlebar's close button.
            gtk::prelude::GtkApplicationExt::set_accels_for_action(
                &app,
                "window.close",
                &["<Ctrl>w"],
            );

            // Activating the app again — its icon, a notification, or the
            // autostart entry — brings the hidden window back rather than doing
            // nothing.
            let window = root.clone();
            // Started hidden? Then the first activation is this very launch, and
            // presenting here would undo it. Every activation after that is a
            // person asking for the window.
            let pending_hidden_start =
                std::cell::Cell::new(crate::HIDDEN_START.load(std::sync::atomic::Ordering::Relaxed));
            app.connect_activate(move |_| {
                if pending_hidden_start.replace(false) {
                    return;
                }
                crate::desktop::present_window(&window);
            });
        }

        let mut group = RelmActionGroup::<WindowActionGroup>::new();
        let accounts_sender = sender.clone();
        group.add_action(RelmAction::<AccountsAction>::new_stateless(move |_| {
            accounts_sender.input(AppMsg::OpenSettings);
        }));
        let prefs_sender = sender.clone();
        group.add_action(RelmAction::<PreferencesAction>::new_stateless(move |_| {
            prefs_sender.input(AppMsg::OpenPreferences);
        }));
        let about_sender = sender.clone();
        group.add_action(RelmAction::<AboutAction>::new_stateless(move |_| {
            about_sender.input(AppMsg::OpenAbout);
        }));
        let shortcuts_sender = sender.clone();
        group.add_action(RelmAction::<ShortcutsAction>::new_stateless(move |_| {
            shortcuts_sender.input(AppMsg::ShowShortcuts);
        }));
        let print_sender = sender.clone();
        group.add_action(RelmAction::<PrintAction>::new_stateless(move |_| {
            print_sender.input(AppMsg::PrintMessage);
        }));
        let preview_sender = sender.clone();
        group.add_action(RelmAction::<PrintPreviewAction>::new_stateless(move |_| {
            preview_sender.input(AppMsg::PrintPreview);
        }));
        // The status bar reveals itself for errors; this is the manual path
        // (the dedicated button is gone from the sidebar).
        let status_sender = sender.clone();
        {
            let s = sender.clone();
            group.add_action(RelmAction::<ConsoleAction>::new_stateless(move |_| {
                s.input(AppMsg::OpenConsole);
            }));
        }
        {
            let s = sender.clone();
            group.add_action(RelmAction::<FindAction>::new_stateless(move |_| {
                s.input(AppMsg::OpenListSearch);
            }));
        }
        let undo_action = {
            let s = sender.clone();
            RelmAction::<UndoAction>::new_stateless(move |_| s.input(AppMsg::Undo))
        };
        let redo_action = {
            let s = sender.clone();
            RelmAction::<RedoAction>::new_stateless(move |_| s.input(AppMsg::Redo))
        };
        group.add_action(undo_action.clone());
        group.add_action(redo_action.clone());
        group.add_action(RelmAction::<StatusBarAction>::new_stateless(move |_| {
            status_sender.input(AppMsg::ToggleNotifications);
        }));
        group.register_for_widget(&root);

        // A real accelerator rather than a key handler: GTK matches these before
        // the keystroke reaches whatever has focus, so Ctrl+? works while reading
        // a message (the web view would otherwise swallow it). Both spellings are
        // bound because layouts disagree about whether Ctrl+Shift+/ arrives as
        // `question` or as `slash`, and F1 is the GNOME convention.
        relm4::main_application().set_accelerators_for_action::<PrintAction>(&["<Ctrl>p"]);
        relm4::main_application()
            .set_accelerators_for_action::<PrintPreviewAction>(&["<Ctrl><Shift>p"]);
        relm4::main_application()
            .set_accelerators_for_action::<StatusBarAction>(&["<Ctrl><Shift>s"]);
        relm4::main_application()
            .set_accelerators_for_action::<ConsoleAction>(&["<Ctrl><Shift>c"]);
        relm4::main_application().set_accelerators_for_action::<FindAction>(&["<Ctrl>f"]);
        relm4::main_application().set_accelerators_for_action::<ShortcutsAction>(&[
            "<Ctrl>question",
            "<Ctrl><Shift>question",
            "<Ctrl>slash",
            "<Ctrl><Shift>slash",
            "F1",
        ]);

        // Single-key shortcuts. The controller is on the window in the bubble
        // phase, so the focused widget always gets first refusal: a search entry
        // or the composer consumes the letter itself and nothing here fires.
        // `focus_takes_keys` covers the rest — chiefly the reader's web view,
        // which handles keys without consuming them.
        {
            let keys = gtk::EventControllerKey::new();
            let s = sender.clone();
            let window = root.clone();
            let enabled = model.single_key.clone();
            keys.connect_key_pressed(move |_, keyval, _, state| {
                let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
                let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
                // Ctrl+? opens the reference whether or not the shortcuts
                // themselves are switched on.
                if ctrl && matches!(keyval, gtk::gdk::Key::question | gtk::gdk::Key::slash) {
                    s.input(AppMsg::ShowShortcuts);
                    return gtk::glib::Propagation::Stop;
                }
                // Ctrl+Z undoes the last action, Ctrl+Shift+Z and Ctrl+Y put
                // it back (#200). Text fields and the composer keep all three
                // for their own text history.
                let undo_key = ctrl && !shift && keyval == gtk::gdk::Key::z;
                let redo_key = ctrl
                    && ((shift && matches!(keyval, gtk::gdk::Key::z | gtk::gdk::Key::Z))
                        || keyval == gtk::gdk::Key::y);
                if undo_key || redo_key {
                    if focus_is_text(&window) || focus_in_compose(&window) {
                        return gtk::glib::Propagation::Proceed;
                    }
                    s.input(if redo_key { AppMsg::Redo } else { AppMsg::Undo });
                    return gtk::glib::Propagation::Stop;
                }
                // Ctrl+C with the keyboard on the list or the window itself:
                // a selection made in the reader still wants copying (the
                // list takes focus back after a click over a message body).
                // The reader's own document handles the key when it has focus;
                // a text field or composer keeps its native copy.
                if ctrl
                    && !shift
                    && keyval == gtk::gdk::Key::c
                    && !focus_takes_keys(&window)
                    && !focus_in_compose(&window)
                {
                    s.input(AppMsg::CopyReaderSelection);
                    return gtk::glib::Propagation::Proceed;
                }
                if ctrl || state.contains(gtk::gdk::ModifierType::ALT_MASK) {
                    return gtk::glib::Propagation::Proceed;
                }
                // Escape backs out to the message list whatever else is going on,
                // and without needing single-key shortcuts switched on. A text
                // field keeps it, though — there it clears the search.
                if keyval == gtk::gdk::Key::Escape {
                    if focus_is_text(&window) {
                        return gtk::glib::Propagation::Proceed;
                    }
                    s.input(AppMsg::Shortcut(Shortcut::BackToList));
                    return gtk::glib::Propagation::Stop;
                }
                if !enabled.get() || focus_takes_keys(&window) {
                    return gtk::glib::Propagation::Proceed;
                }
                match shortcut_for(keyval, shift) {
                    Some(action) => {
                        s.input(AppMsg::Shortcut(action));
                        gtk::glib::Propagation::Stop
                    }
                    None => gtk::glib::Propagation::Proceed,
                }
            });
            root.add_controller(keys);
        }

        // One-time, dismissible keyring setup tip for Linux Mint / Cinnamon, where
        // the Secret Service often needs configuring so passwords persist and the
        // keyring auto-unlocks at login. Only shown once the user actually has an
        // account (so it isn't the very first thing a new user sees), and never
        // again after "Don't show again".
        if !model.config.is_empty()
            && crate::platform::is_mint_cinnamon()
            && !config::mint_keyring_help_dismissed()
        {
            sender.input(AppMsg::ShowKeyringHelp { problem: false });
        }

        // An earlier install's data came across at this start (see
        // `legacy`): say so once, and take its start-at-login over — the
        // portal's autostart entry is per app ID, so it has to be asked
        // for again under this one.
        if let Some(from) = crate::legacy::migrated_from() {
            if model.run_in_background.get() && model.autostart {
                crate::background::request(true);
            }
            sender.input(AppMsg::ShowCarryOverNotice(from));
        } else if std::env::var_os("HYLKI_SHOWCASE_CARRY_OVER").is_some() {
            // Opens the notice as if Vireo's data had just come across, for
            // checking it by hand.
            sender.input(AppMsg::ShowCarryOverNotice(&crate::legacy::PREDECESSORS[0]));
        }

        model.lightbox_picture = Some(widgets.lightbox_picture.clone());
        model.lightbox_scroller = Some(widgets.lightbox_scroller.clone());

        // Lightbox pointer behaviour: dragging pans the zoomed document (the
        // scroller's adjustments move opposite the pointer); a clean click —
        // release with no meaningful movement — cycles the zoom. The shared
        // `moved` cell is what keeps a pan from also zooming.
        {
            let hadj = widgets.lightbox_scroller.hadjustment();
            let vadj = widgets.lightbox_scroller.vadjustment();
            let start = std::rc::Rc::new(std::cell::Cell::new((0.0_f64, 0.0_f64)));
            let moved = std::rc::Rc::new(std::cell::Cell::new(0.0_f64));

            let drag = gtk::GestureDrag::new();
            drag.set_button(gtk::gdk::BUTTON_PRIMARY);
            {
                let start = start.clone();
                let moved = moved.clone();
                let (h, v) = (hadj.clone(), vadj.clone());
                drag.connect_drag_begin(move |_, _, _| {
                    start.set((h.value(), v.value()));
                    moved.set(0.0);
                });
            }
            {
                let start = start.clone();
                let moved = moved.clone();
                drag.connect_drag_update(move |_, dx, dy| {
                    moved.set(moved.get().max(dx.abs().max(dy.abs())));
                    let (h0, v0) = start.get();
                    hadj.set_value(h0 - dx);
                    vadj.set_value(v0 - dy);
                });
            }
            // On the SCROLLER, not the picture: the picture's own coordinate
            // space moves with every pan, so offsets measured in it oscillate
            // — scroll, shift, un-scroll — and the drag jitters. The scroller
            // stays put, so its offsets are stable.
            widgets.lightbox_scroller.add_controller(drag);

            let click = gtk::GestureClick::new();
            click.set_button(gtk::gdk::BUTTON_PRIMARY);
            let s = sender.clone();
            click.connect_released(move |_, n, x, y| {
                if n == 1 && moved.get() < 8.0 {
                    s.input(AppMsg::LightboxZoomCycle { x, y });
                }
            });
            widgets.lightbox_picture.add_controller(click);
        }

        // Screenshot showcase (HYLKI_DEMO + HYLKI_SHOWCASE=/path.png): stage
        // the demo the way the marketing shots want it — first row (the demo
        // conversation, expanded via the threads_expanded preference) selected,
        // one mid-thread card highlighted — then render the window to a PNG.
        // Return freed heap to the system now and then. What the gallery,
        // the composer or a big conversation allocated and dropped otherwise
        // stays held by the allocator, and a system monitor counts it against
        // the app: a user's log showed 1.73 GB held with 270 MB live.
        gtk::glib::timeout_add_seconds_local(30, || {
            if let Some(held) = crate::memory_report::trim_if_worthwhile() {
                tracing::debug!(target: "hylki::memory", "trimmed the heap: {} was freed but held", crate::memory_report::human_bytes(held as u64));
            }
            gtk::glib::ControlFlow::Continue
        });
        // HYLKI_SHOWCASE_MEMORY=/path.txt writes the exported log (memory
        // section included) there after HYLKI_SHOWCASE_MEMORY_AT seconds
        // (default 20), on a real mailbox as much as the demo: a memory
        // reading is only worth having once the index has loaded. With
        // HYLKI_SHOWCASE_MEMORY_EVERY=N it is rewritten every N seconds
        // after that, for watching a session grow.
        if let Some(path) = std::env::var_os("HYLKI_SHOWCASE_MEMORY") {
            let secs = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u32>().ok());
            let at = secs("HYLKI_SHOWCASE_MEMORY_AT").unwrap_or(20);
            let every = secs("HYLKI_SHOWCASE_MEMORY_EVERY").filter(|n| *n > 0);
            let s = sender.clone();
            let path = std::path::PathBuf::from(path);
            gtk::glib::timeout_add_seconds_local_once(at, move || {
                s.input(AppMsg::ExportLogTo(path.clone()));
                if let Some(every) = every {
                    gtk::glib::timeout_add_seconds_local(every, move || {
                        s.input(AppMsg::ExportLogTo(path.clone()));
                        gtk::glib::ControlFlow::Continue
                    });
                }
            });
        }
        // HYLKI_SHOWCASE_SELECT=<account>:<id> opens that message at 5 s
        // (HYLKI_SHOWCASE_SELECT_AT overrides the seconds) through its row,
        // or the head of its conversation, real accounts included: a way to
        // open a conversation the reader assembles from the cache without a
        // pointer, so its log can be read (#236).
        // HYLKI_SHOWCASE_ZOOM=N presses Ctrl+ N times (Ctrl- for a negative
        // N) at 8 s, so the live zoom path can be captured.
        if let Some(n) = std::env::var("HYLKI_SHOWCASE_ZOOM").ok().and_then(|v| v.parse::<i8>().ok()) {
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(8, move || {
                for _ in 0..n.unsigned_abs() {
                    s.input(AppMsg::ZoomMessage(n.signum()));
                }
            });
        }
        if let Some((a, id)) = std::env::var("HYLKI_SHOWCASE_SELECT").ok().and_then(|v| {
            let (a, id) = v.split_once(':')?;
            Some((a.parse::<u32>().ok()?, id.parse::<u32>().ok()?))
        }) {
            let at: u32 = std::env::var("HYLKI_SHOWCASE_SELECT_AT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5);
            let list = model.message_list.sender().clone();
            gtk::glib::timeout_add_seconds_local_once(at, move || {
                let _ = list.send(MessageListInput::SelectAndLoad((a, id)));
            });
        }
        // HYLKI_SHOWCASE_INBOX=<account> switches to that account's Inbox
        // at 3 s (the first account's when it is not a number), real
        // accounts included, for the same probe from a folder view.
        if let Ok(v) = std::env::var("HYLKI_SHOWCASE_INBOX") {
            let account = v.parse::<u32>().ok().filter(|a| *a > 0);
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(3, move || {
                s.input(AppMsg::ShowcaseFolder { kind: FolderKind::Inbox, account });
            });
        }
        // Timers leave room for the WebViews to load and settle between steps.
        if demo_mode() {
            if let Some(shot) = std::env::var("HYLKI_SHOWCASE").ok() {
                // HYLKI_SHOWCASE_STAGE=0 skips the staging (capture-only, for
                // testing arbitrary states); HYLKI_SHOWCASE_DELAY overrides the
                // capture time (seconds, default 9).
                let stage = std::env::var("HYLKI_SHOWCASE_STAGE").as_deref() != Ok("0");
                let delay: u32 = std::env::var("HYLKI_SHOWCASE_DELAY")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(9);
                // HYLKI_SHOWCASE_HEIGHTS=1 logs the allocated height of the
                // sidebar's section headings and their rows just before the
                // capture, for checking pill geometry without a pointer.
                if std::env::var("HYLKI_SHOWCASE_HEIGHTS").is_ok() {
                    let sidebar = model.sidebar.widget().clone().upcast::<gtk::Widget>();
                    gtk::glib::timeout_add_seconds_local_once(delay.saturating_sub(1).max(1), move || {
                        fn walk(w: &gtk::Widget, depth: usize, root: &gtk::Widget) {
                            let classes = w.css_classes();
                            let heading = classes.iter().any(|c| c == "unified-folders-toggle");
                            let child_row = w.is::<gtk::ListBoxRow>()
                                && w.parent().is_some_and(|p| p.css_classes().iter().any(|c| c == "section-child-list"));
                            if heading || child_row {
                                let y = w.compute_bounds(root).map(|b| b.y() as i32).unwrap_or(-1);
                                tracing::info!(
                                    "showcase heights: {} y={y} h={} margins={}/{} classes={:?}",
                                    if heading { "heading" } else { "row" },
                                    w.height(),
                                    w.margin_top(),
                                    w.margin_bottom(),
                                    classes,
                                );
                            }
                            let mut c = w.first_child();
                            while let Some(ch) = c {
                                walk(&ch, depth + 1, root);
                                c = ch.next_sibling();
                            }
                        }
                        walk(&sidebar, 0, &sidebar);
                    });
                }
                if stage {
                    let list = model.message_list.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        let _ = list.send(MessageListInput::MoveSelection(1));
                    });
                    // HYLKI_SHOWCASE_PALETTE=N opens row N's actions palette
                    // (so a capture can verify the floating palette's look).
                    if let Some(Ok(idx)) =
                        std::env::var("HYLKI_SHOWCASE_PALETTE").ok().map(|v| v.parse())
                    {
                        let list = model.message_list.sender().clone();
                        let burst = std::env::var("HYLKI_SHOWCASE_BURST").ok();
                        let win = root.clone();
                        {
                            // An occluded window's frame clock is suspended
                            // and animations cannot progress — raise it well
                            // ahead so the slide actually runs for the burst.
                            let win = win.clone();
                            gtk::glib::timeout_add_seconds_local_once(5, move || {
                                win.present();
                            });
                        }
                        gtk::glib::timeout_add_seconds_local_once(7, move || {
                            let _ = list.send(MessageListInput::DebugOpenPalette(idx));
                            // TEMP: frame burst right after the open, to see
                            // the slide animation (or its absence) in stills.
                            if let Some(base) = burst.clone() {
                                for (i, ms) in [40u64, 90, 140, 240].iter().enumerate() {
                                    let win = win.clone();
                                    let path = format!("{base}.{i}.png");
                                    gtk::glib::timeout_add_local_once(
                                        std::time::Duration::from_millis(*ms),
                                        move || {
                                            showcase_capture(
                                                win.upcast_ref::<gtk::Widget>(),
                                                &path,
                                            );
                                        },
                                    );
                                }
                            }
                        });
                    }
                    let view = model.message_view.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        let _ = view.send(crate::ui::message_view::MessageViewInput::CardClicked {
                            account_id: 1,
                            id: 2,
                            mode: crate::ui::message_view::SelectMode::Plain,
                        });
                    });
                }
                // HYLKI_SHOWCASE_SWIPE=N[:left|right] swipes row N fully and
                // releases it at 7s, with HYLKI_SHOWCASE_BURST=<path> writing
                // stills through the commit exit — the only way to see the
                // gesture's animation here (no input injection).
                if let Ok(spec) = std::env::var("HYLKI_SHOWCASE_SWIPE") {
                    let (idx, side) = spec.split_once(':').unwrap_or((spec.as_str(), "left"));
                    let index: usize = idx.trim().parse().unwrap_or(0);
                    let left = side != "right";
                    let list = model.message_list.sender().clone();
                    let burst = std::env::var("HYLKI_SHOWCASE_BURST").ok();
                    let win = root.clone();
                    {
                        // An occluded window's frame clock is suspended, so
                        // raise it well before the animation runs.
                        let win = win.clone();
                        gtk::glib::timeout_add_seconds_local_once(5, move || win.present());
                    }
                    gtk::glib::timeout_add_seconds_local_once(7, move || {
                        let _ = list.send(MessageListInput::DebugSwipe { index, left });
                        if let Some(base) = burst.clone() {
                            for (i, ms) in [40u64, 120, 200, 300, 500].iter().enumerate() {
                                let win = win.clone();
                                let path = format!("{base}.{i}.png");
                                gtk::glib::timeout_add_local_once(
                                    std::time::Duration::from_millis(*ms),
                                    move || {
                                        showcase_capture(win.upcast_ref::<gtk::Widget>(), &path);
                                    },
                                );
                            }
                        }
                    });
                }
                // HYLKI_SHOWCASE_ROW=N selects the list's row N at 4s (with
                // HYLKI_SHOWCASE_STAGE=0), for capturing or probing one message.
                if let Some(Ok(n)) = std::env::var("HYLKI_SHOWCASE_ROW").ok().map(|v| v.parse::<u32>()) {
                    let list = model.message_list.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(4, move || {
                        for _ in 0..=n {
                            let _ = list.send(MessageListInput::MoveSelection(1));
                        }
                    });
                }
                // HYLKI_SHOWCASE_UNSUB=ask|go presses the open card's
                // Unsubscribe button at 7s (after HYLKI_SHOWCASE_ROW opened
                // it): `ask` stops at the confirmation, `go` skips it.
                if std::env::var("HYLKI_SHOWCASE_UNSUB").is_ok() {
                    let view = model.message_view.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(7, move || {
                        let _ = view.send(MessageViewInput::Unsubscribe { account_id: 1, id: 10 });
                    });
                }
                // HYLKI_SHOWCASE_INVITE=accept|maybe|decline|calendar presses
                // that button on the open card's invitation banner at 7s
                // (after HYLKI_SHOWCASE_ROW=8 opened the meeting request).
                if let Ok(which) = std::env::var("HYLKI_SHOWCASE_INVITE") {
                    use crate::models::Rsvp;
                    use crate::ui::message_view::InviteAction;
                    let action = match which.as_str() {
                        "maybe" => InviteAction::Answer(Rsvp::Tentative),
                        "decline" => InviteAction::Answer(Rsvp::Declined),
                        "calendar" => InviteAction::AddToCalendar,
                        _ => InviteAction::Answer(Rsvp::Accepted),
                    };
                    let view = model.message_view.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(7, move || {
                        let _ = view.send(MessageViewInput::InviteAction {
                            account_id: 1,
                            id: 13,
                            action,
                        });
                    });
                }
                // HYLKI_SHOWCASE_UNIFIED=sent|starred|drafts opens that unified
                // row at 3s, All Inboxes at 6s and the row again at 9s, so the
                // timing logs show a cold and a warm open.
                if let Ok(which) = std::env::var("HYLKI_SHOWCASE_UNIFIED") {
                    let pick = move || match which.as_str() {
                        "starred" => SidebarInput::UnifiedKindRowSelected(FolderKind::Starred),
                        "drafts" => SidebarInput::UnifiedKindRowSelected(FolderKind::Drafts),
                        "archive" => SidebarInput::UnifiedKindRowSelected(FolderKind::Archive),
                        "filtered" => SidebarInput::UnifiedFilteredSelected,
                        "tags" => SidebarInput::UnifiedTagsSelected,
                        _ => SidebarInput::UnifiedKindRowSelected(FolderKind::Sent),
                    };
                    let sb = model.sidebar.sender().clone();
                    let p = pick.clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        let _ = sb.send(p());
                    });
                    let sb = model.sidebar.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        let _ = sb.send(SidebarInput::SelectUnifiedRow);
                    });
                    let sb = model.sidebar.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(9, move || {
                        let _ = sb.send(pick());
                    });
                }
                // HYLKI_SHOWCASE_ROW_MENU=1 opens the first row's context
                // menu at 5s (pair with HYLKI_SHOWCASE_MENU to capture it).
                if std::env::var("HYLKI_SHOWCASE_ROW_MENU").is_ok() {
                    let ml = model.message_list.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let _ = ml.send(MessageListInput::ContextMenu { x: 120.0, y: 40.0 });
                    });
                }
                // HYLKI_SHOWCASE_APPLY_FILTERS=inboxes|<account>:<folder>
                // starts a manual filter run at 5s (#198), the way Apply Now
                // and the folder menu's Apply Filters do, and the run's
                // report lands in the status bar a beat later.
                if let Ok(spec) = std::env::var("HYLKI_SHOWCASE_APPLY_FILTERS") {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let targets = spec
                            .split_once(':')
                            .and_then(|(a, f)| Some(vec![(a.parse().ok()?, f.parse().ok()?)]))
                            .unwrap_or_default();
                        s.input(AppMsg::ApplyFilters(targets));
                    });
                }
                // HYLKI_SHOWCASE_TOOLBAR_GAP=<zone>:<index> opens a drop gap
                // in the Settings toolbar editor at 6s (pair with
                // HYLKI_SHOWCASE_SETTINGS=appearance), as a hovering drag
                // would.
                if let Ok(spec) = std::env::var("HYLKI_SHOWCASE_TOOLBAR_GAP") {
                    if let Some((z, i)) = spec.split_once(':') {
                        if let (Ok(zone), Ok(index)) = (z.parse::<usize>(), i.parse::<usize>()) {
                            let s = sender.input_sender().clone();
                            gtk::glib::timeout_add_seconds_local_once(6, move || {
                                let _ = s.send(AppMsg::ShowcaseToolbarGap { zone, index });
                            });
                        }
                    }
                }
                // HYLKI_SHOWCASE_TOOLBAR_MENU=1 opens the header's right-click
                // menu (Customize Toolbar…) at 5s, as a click on its empty
                // middle would.
                if std::env::var("HYLKI_SHOWCASE_TOOLBAR_MENU").is_ok() {
                    let s = sender.input_sender().clone();
                    let header: gtk::Widget = widgets.reader_header.clone().upcast();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let _ = s.send(AppMsg::ReaderToolbarMenu {
                            x: header.width() as f64 / 2.0,
                            y: header.height() as f64 / 2.0,
                        });
                    });
                }
                // HYLKI_SHOWCASE_READER_MENU=1 opens the reader header's ⋯
                // overflow menu at 5s (pair with HYLKI_SHOWCASE_MENU to
                // capture it; the window must be narrow enough to collapse).
                if std::env::var("HYLKI_SHOWCASE_READER_MENU").is_ok() {
                    let s = sender.input_sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let _ = s.send(AppMsg::ReaderOverflowMenu);
                    });
                }
                // HYLKI_SHOWCASE_EDIT_AS_NEW=1 copies the open message into
                // the composer at 6s (#232), as its menu entry does.
                if std::env::var("HYLKI_SHOWCASE_EDIT_AS_NEW").is_ok() {
                    let s = sender.input_sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        let _ = s.send(AppMsg::EditAsNewCurrent);
                    });
                }
                // HYLKI_SHOWCASE_MOVE=1 opens the Move To… picker at 5s
                // (it captures itself a second later).
                if std::env::var("HYLKI_SHOWCASE_MOVE").is_ok() {
                    let s = sender.input_sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let _ = s.send(AppMsg::MoveToMenu);
                    });
                }
                // HYLKI_SHOWCASE_GALLERY=1 opens the attachments gallery at
                // 3s; add HYLKI_SHOWCASE_GALLERY_FOLDERS=1 to drop its folder
                // popover open a second later, or HYLKI_SHOWCASE_GALLERY_ACCOUNT=<row>
                // to pick that row of its account filter (0 = all accounts).
                if std::env::var("HYLKI_SHOWCASE_GALLERY").is_ok() {
                    let s = sender.input_sender().clone();
                    let g = model.gallery.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        let _ = s.send(AppMsg::ShowAttachments);
                        let row: Option<u32> = std::env::var("HYLKI_SHOWCASE_GALLERY_ACCOUNT")
                            .ok()
                            .and_then(|v| v.parse().ok());
                        let folders = std::env::var("HYLKI_SHOWCASE_GALLERY_FOLDERS").is_ok();
                        // HYLKI_SHOWCASE_GALLERY_SEARCH=<text> types into the
                        // search box and reports whether it keeps the focus.
                        let search = std::env::var("HYLKI_SHOWCASE_GALLERY_SEARCH").ok();
                        // HYLKI_SHOWCASE_GALLERY_MORE=<n> pages down n times,
                        // as scrolling to the end would.
                        let more: u32 = std::env::var("HYLKI_SHOWCASE_GALLERY_MORE")
                            .ok()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                        gtk::glib::timeout_add_seconds_local_once(1, move || {
                            if let Some(row) = row {
                                let _ = g.send(GalleryInput::SetAccountFilter(row));
                            }
                            for i in 0..more {
                                let g = g.clone();
                                gtk::glib::timeout_add_seconds_local_once(1 + i, move || {
                                    let _ = g.send(GalleryInput::LoadMore);
                                });
                            }
                            if let Some(text) = search {
                                let _ = g.send(GalleryInput::ShowcaseSearch(text));
                            }
                            if folders {
                                let _ = g.send(GalleryInput::ShowcaseFolders);
                            }
                        });
                    });
                }
                // HYLKI_SHOWCASE_RAIL=1 collapses the sidebar to the rail
                // at 3s (the user's own toggle, fold-ups and all).
                if std::env::var("HYLKI_SHOWCASE_RAIL").is_ok() {
                    let sb = model.sidebar.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        let _ = sb.send(SidebarInput::ToggleCollapsed);
                    });
                }
                // HYLKI_SHOWCASE_PEEK=<seconds>[,<seconds>…] works the header's
                // sidebar toggle at each of those moments (1 or anything
                // unparseable means 8 s), with the window raised first so the
                // slide really animates; HYLKI_SHOWCASE_BURST writes stills
                // through the first one. In a narrow window (a window.toml
                // under the 1094px breakpoint) that is the floating peek; at
                // full width, or under Focus Mode's rail, it is the rail
                // expanding and folding again.
                if let Ok(spec) = std::env::var("HYLKI_SHOWCASE_PEEK") {
                    let mut at: Vec<f64> =
                        spec.split(',').filter_map(|p| p.trim().parse::<f64>().ok()).filter(|s| *s > 1.5).collect();
                    if at.is_empty() {
                        at.push(8.0);
                    }
                    let burst = std::env::var("HYLKI_SHOWCASE_BURST").ok();
                    let win = root.clone();
                    {
                        let win = win.clone();
                        let first = at[0];
                        gtk::glib::timeout_add_local_once(
                            std::time::Duration::from_millis(((first - 1.5) * 1000.0) as u64),
                            move || win.present(),
                        );
                    }
                    for (n, secs) in at.into_iter().enumerate() {
                        let s = sender.clone();
                        let win = win.clone();
                        let burst = (n == 0).then(|| burst.clone()).flatten();
                        gtk::glib::timeout_add_local_once(
                            std::time::Duration::from_millis((secs * 1000.0) as u64),
                            move || {
                                s.input(AppMsg::ToggleSidebar);
                                if let Some(base) = burst {
                                    for (i, ms) in [60u64, 120, 200, 400].iter().enumerate() {
                                        let win = win.clone();
                                        let path = format!("{base}.{i}.png");
                                        gtk::glib::timeout_add_local_once(
                                            std::time::Duration::from_millis(*ms),
                                            move || {
                                                showcase_capture(win.upcast_ref::<gtk::Widget>(), &path);
                                            },
                                        );
                                    }
                                }
                            },
                        );
                    }
                }
                // HYLKI_SHOWCASE_TOGGLE=starred|sent|drafts toggles that
                // unified row's list at 5s (what a long-press does).
                if let Ok(which) = std::env::var("HYLKI_SHOWCASE_TOGGLE") {
                    let kind = match which.as_str() {
                        "sent" => FolderKind::Sent,
                        "drafts" => FolderKind::Drafts,
                        "archive" => FolderKind::Archive,
                        _ => FolderKind::Starred,
                    };
                    let sb = model.sidebar.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let _ = sb.send(SidebarInput::ToggleKindExpand(kind));
                    });
                }
                // HYLKI_SHOWCASE_FOLD_FILTERED folds All Inboxes' Filtered
                // Folders section, to check its folded header.
                if std::env::var("HYLKI_SHOWCASE_FOLD_FILTERED").is_ok() {
                    let sb = model.sidebar.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        let _ = sb.send(SidebarInput::ToggleFilteredExpand(crate::ui::sidebar::Slot::Unified));
                    });
                }
                // HYLKI_SHOWCASE_REPLY opens the inline reply composer on the
                // selected message, to check the composer's grounds (#148).
                // HYLKI_SHOWCASE_FLIP=dark|light then switches the app theme
                // at 6 s, to check a live flip re-resolves those grounds.
                // HYLKI_SHOWCASE_FOLDER=drafts|sent|archive|junk|trash switches
                // to that folder at 2 s, before the staging's 3 s selection
                // moves onto its first row.
                // HYLKI_SHOWCASE_FOCUS=<seconds> switches Focus Mode on at
                // that moment (its parts as saved in focus.toml), so a
                // capture a little later catches the slide, and one later
                // still the settled layout. HYLKI_SHOWCASE_FOCUS_OFF=<seconds>
                // switches it off again.
                for (var, on) in [("HYLKI_SHOWCASE_FOCUS", true), ("HYLKI_SHOWCASE_FOCUS_OFF", false)] {
                    if let Some(at) = std::env::var(var).ok().and_then(|v| v.parse::<f64>().ok()) {
                        let s = sender.clone();
                        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis((at * 1000.0) as u64), move || {
                            let mut focus = config::load_focus_mode();
                            focus.enabled = on;
                            s.input(AppMsg::SetFocusMode(focus));
                        });
                    }
                }
                if let Ok(kind) = std::env::var("HYLKI_SHOWCASE_FOLDER") {
                    let kind = match kind.as_str() {
                        "drafts" => Some(FolderKind::Drafts),
                        "sent" => Some(FolderKind::Sent),
                        "archive" => Some(FolderKind::Archive),
                        "junk" => Some(FolderKind::Junk),
                        "trash" => Some(FolderKind::Trash),
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        let s = sender.clone();
                        gtk::glib::timeout_add_seconds_local_once(2, move || {
                            s.input(AppMsg::ShowcaseFolder { kind, account: None });
                        });
                    }
                }
                if std::env::var("HYLKI_SHOWCASE_REPLY").is_ok() {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(4, move || {
                        s.input(AppMsg::Reply);
                    });
                }
                // HYLKI_SHOWCASE_FORWARD=<seconds> does the same with
                // Forward at that moment (4 s unless it parses), to check
                // the original's attachments come along (#240). As
                // <seconds>:<account>:<id> it forwards that message of the
                // open conversation, the way its card's button would, so a
                // message that is not the conversation's newest can be
                // picked.
                if let Ok(v) = std::env::var("HYLKI_SHOWCASE_FORWARD") {
                    let mut parts = v.split(':');
                    let at = parts.next().and_then(|p| p.parse::<u32>().ok()).unwrap_or(4);
                    let target = parts
                        .next()
                        .zip(parts.next())
                        .and_then(|(a, id)| Some((a.parse::<u32>().ok()?, id.parse::<u32>().ok()?)));
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(at, move || match target {
                        Some((account_id, id)) => s.input(AppMsg::ShowcaseForward { account_id, id }),
                        None => s.input(AppMsg::Forward),
                    });
                }
                // HYLKI_SHOWCASE_COMPOSE_PREVIEW=1 turns the inline
                // composer's preview on a beat after it opens, so a
                // capture can show the rendered message rather than the
                // source it was written in.
                // HYLKI_SHOWCASE_COMPOSE_MENU=format opens the inline
                // composer's format chooser; any other value opens its
                // overflow menu. Pair either with HYLKI_SHOWCASE_MENU=main.
                if let Ok(which) = std::env::var("HYLKI_SHOWCASE_COMPOSE_MENU") {
                    let s = sender.clone();
                    let format = which == "format";
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        s.input(AppMsg::ShowcaseComposeMenu { format });
                    });
                }
                if std::env::var("HYLKI_SHOWCASE_COMPOSE_PREVIEW").is_ok() {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        s.input(AppMsg::ShowcaseComposePreview);
                    });
                }
                // HYLKI_SHOWCASE_COMPOSE_FROM=N picks the inline composer's
                // From entry N at 7 s, to check where the new account's
                // signature lands (#237).
                if let Some(Ok(n)) = std::env::var("HYLKI_SHOWCASE_COMPOSE_FROM").ok().map(|v| v.parse::<u32>()) {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(7, move || {
                        s.input(AppMsg::ShowcaseComposeFrom(n));
                    });
                }
                // HYLKI_SHOWCASE_COMPOSE_CLOSE=N cancels the inline composer
                // N seconds in (pair with HYLKI_SHOWCASE_REPLY), to check
                // that a closed composer's web view, and so its web process,
                // actually goes away (#221).
                if let Some(at) = std::env::var("HYLKI_SHOWCASE_COMPOSE_CLOSE").ok().and_then(|v| v.parse::<u32>().ok()) {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(at, move || {
                        s.input(AppMsg::ShowcaseComposeClose);
                    });
                }
                // HYLKI_SHOWCASE_COMPOSE_UNDO=1 runs the composer's history
                // (#200) through a scripted round of edits and undos on the
                // `hylki::compose::undo` log target: the one way to exercise
                // it without a keyboard, since the keys cannot be injected
                // on this desktop. Pair with HYLKI_SHOWCASE_REPLY.
                // Set it to markdown, html or plain to walk a source-mode
                // body (a textarea) instead of the rich document, to
                // `backspace` to trim text rather than type it, or to
                // `pause` to leave a long gap between two runs of typing.
                if let Ok(which) = std::env::var("HYLKI_SHOWCASE_COMPOSE_UNDO") {
                    let format = match which.as_str() {
                        "markdown" => Some(config::ComposeFormat::Markdown),
                        "html" => Some(config::ComposeFormat::Html),
                        "plain" => Some(config::ComposeFormat::Plain),
                        _ => None,
                    };
                    if let Some(format) = format {
                        let s = sender.clone();
                        gtk::glib::timeout_add_seconds_local_once(5, move || {
                            s.input(AppMsg::ShowcaseComposeFormat(format));
                        });
                    }
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(7, move || {
                        s.input(AppMsg::ShowcaseComposeUndo);
                    });
                    // …and drop the burger menu open over it near the end,
                    // which is the state the entries are actually read in.
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(16, move || {
                        s.input(AppMsg::ShowcaseBurger);
                    });
                }
                // HYLKI_SHOWCASE_FILES=/a:/b hands those files in at 4 s,
                // as GNOME Files' "Send with Hylki" would; with
                // HYLKI_SHOWCASE_TOP=1 the capture takes the newest window
                // (the dialog asking about them) instead of the main one.
                if let Ok(list) = std::env::var("HYLKI_SHOWCASE_FILES") {
                    let s = sender.clone();
                    let paths: Vec<std::path::PathBuf> = list.split(':').filter(|p| !p.is_empty()).map(Into::into).collect();
                    gtk::glib::timeout_add_seconds_local_once(4, move || {
                        s.input(AppMsg::OpenWithFiles(paths));
                    });
                }
                if let Ok(flip) = std::env::var("HYLKI_SHOWCASE_FLIP") {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        s.input(AppMsg::SetAppTheme(if flip == "dark" {
                            config::AppTheme::Dark
                        } else {
                            config::AppTheme::Light
                        }));
                    });
                }
                // HYLKI_SHOWCASE_THEME=<id> picks that appearance theme at
                // 6 s, exactly as the Settings gallery does, so the live
                // repaint (chrome, reader and composer) can be captured.
                if let Ok(id) = std::env::var("HYLKI_SHOWCASE_THEME") {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        s.input(AppMsg::SetTheme(id.clone()));
                    });
                }
                // HYLKI_SHOWCASE_ACCOUNT=N opens account N's editor a beat
                // after the Settings window (with HYLKI_SHOWCASE_SETTINGS),
                // so the editor itself can be captured.
                if let Some(Ok(n)) = std::env::var("HYLKI_SHOWCASE_ACCOUNT").ok().map(|v| v.parse::<u32>()) {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        s.input(AppMsg::SidebarContext(CtxAction::OpenAccountSettings(n + 1)));
                    });
                }
                // HYLKI_SHOWCASE_EDITOR_DIRTY=1 types into the open account
                // editor's Label field at 7s, so leaving an edited editor can
                // be exercised without a keyboard.
                if std::env::var("HYLKI_SHOWCASE_EDITOR_DIRTY").is_ok() {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(7, move || {
                        s.input(AppMsg::ShowcaseDirtyEditor);
                    });
                }
                // HYLKI_SHOWCASE_SETTINGS_GO=<category> picks that sidebar
                // category at 8s, exactly as a click on its row does.
                if let Ok(page) = std::env::var("HYLKI_SHOWCASE_SETTINGS_GO") {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(8, move || {
                        s.input(AppMsg::ShowSettingsPage(page.clone()));
                    });
                }
                // HYLKI_SHOWCASE_DIALOG=save|discard|cancel answers whatever
                // message dialog is on screen at 10s.
                if let Ok(answer) = std::env::var("HYLKI_SHOWCASE_DIALOG") {
                    gtk::glib::timeout_add_seconds_local_once(10, move || {
                        let tops = gtk::Window::toplevels();
                        let dialog = (0..tops.n_items())
                            .filter_map(|i| tops.item(i))
                            .filter_map(|o| o.downcast::<adw::MessageDialog>().ok())
                            .find(|d| d.is_visible());
                        match dialog {
                            Some(d) => {
                                // Answering is what a button click emits; the
                                // click also takes the dialog off screen, so
                                // hide it (closing would answer a second time).
                                d.response(&answer);
                                d.set_visible(false);
                            }
                            None => tracing::warn!("showcase: no dialog to answer"),
                        }
                    });
                }
                // HYLKI_SHOWCASE_SETTINGS=accounts|prefs opens the Settings
                // window on that panel and captures it instead of the main
                // window, so its pages can be checked in stills too.
                let settings = std::env::var("HYLKI_SHOWCASE_SETTINGS").ok();
                if let Some(panel) = settings.clone() {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        s.input(if panel == "accounts" {
                            AppMsg::OpenAccounts
                        } else {
                            AppMsg::OpenPreferences
                        });
                        // Any other value names a sidebar category (#141).
                        if panel != "accounts" && panel != "prefs" {
                            let s = s.clone();
                            gtk::glib::timeout_add_seconds_local_once(1, move || {
                                s.input(AppMsg::ShowSettingsPage(panel));
                            });
                        }
                    });
                    // HYLKI_SHOWCASE_SETTINGS_REOPEN=1 closes the window at
                    // 5s and opens it again at 7s, to time a reopen.
                    if std::env::var("HYLKI_SHOWCASE_SETTINGS_REOPEN").is_ok() {
                        let s = sender.clone();
                        gtk::glib::timeout_add_seconds_local_once(5, move || {
                            s.input(AppMsg::DebugCloseSettings);
                        });
                        let s = sender.clone();
                        gtk::glib::timeout_add_seconds_local_once(7, move || {
                            s.input(AppMsg::OpenAccounts);
                        });
                    }
                }
                let main: gtk::Window = root.clone().upcast();
                let settings_window = move || {
                    let tops = gtk::Window::toplevels();
                    (0..tops.n_items())
                        .filter_map(|i| tops.item(i))
                        .filter_map(|o| o.downcast::<gtk::Window>().ok())
                        .filter(|w| *w != main && w.is_visible())
                        // The newest: a dialog over Settings, when one is up.
                        .last()
                };
                // With HYLKI_SHOWCASE_SCROLL set, the Settings window is
                // made tall a beat after opening and every scroller in it
                // is run to its end just before the capture, so the lower
                // groups of a panel can be checked.
                // The value is the fraction of the way down (default 1).
                let scroll: Option<f64> = std::env::var("HYLKI_SHOWCASE_SCROLL")
                    .ok()
                    .map(|v| v.parse().unwrap_or(1.0));
                if let (Some(_), Some(frac)) = (&settings, scroll) {
                    // HYLKI_SHOWCASE_SCROLL_WIDTH overrides the 720px width,
                    // for checking a panel at the window's real width.
                    let width: i32 = std::env::var("HYLKI_SHOWCASE_SCROLL_WIDTH")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(720);
                    let find = settings_window.clone();
                    gtk::glib::timeout_add_seconds_local_once(4, move || {
                        if let Some(w) = find() {
                            w.set_default_size(width, 1300);
                        }
                    });
                    let find = settings_window.clone();
                    gtk::glib::timeout_add_seconds_local_once(delay.saturating_sub(1), move || {
                        if let Some(w) = find() {
                            scroll_all_to(w.upcast_ref::<gtk::Widget>(), frac);
                        }
                    });
                }
                let win = root.clone();
                let top = std::env::var_os("HYLKI_SHOWCASE_TOP").is_some();
                // HYLKI_SHOWCASE_DELAY_MS names the moment to the
                // millisecond instead (a frame inside an animation).
                let delay_ms: u64 = std::env::var("HYLKI_SHOWCASE_DELAY_MS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(u64::from(delay) * 1000);
                gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(delay_ms), move || {
                    let target = settings
                        .as_ref()
                        .map(|_| ())
                        .or(top.then_some(()))
                        .and_then(|_| settings_window())
                        .map(|w| w.upcast::<gtk::Widget>())
                        .unwrap_or_else(|| win.clone().upcast::<gtk::Widget>());
                    showcase_capture(&target, &shot);
                });
            }
        }

        // The list header's sort menu (moved out of the message list's own
        // toolbar): a stateful radio action feeding the list's SetSort.
        {
            use crate::ui::message_list::SortOrder;
            let sort_group = gtk::gio::SimpleActionGroup::new();
            let sort_action = gtk::gio::SimpleAction::new_stateful(
                "order",
                Some(gtk::glib::VariantTy::STRING),
                &"date_newest".to_variant(),
            );
            let list = model.message_list.sender().clone();
            sort_action.connect_activate(move |action, param| {
                if let Some(key) = param.and_then(|v| v.str()) {
                    action.set_state(&key.to_variant());
                    let _ = list.send(MessageListInput::SetSort(SortOrder::from_key(key)));
                }
            });
            sort_group.add_action(&sort_action);
            widgets.list_sort_btn.insert_action_group("sortmenu", Some(&sort_group));

            let menu = gtk::gio::Menu::new();
            for (label, key) in [
                (i18n_noop("Date (Newest first)"), "date_newest"),
                (i18n_noop("Date (Oldest first)"), "date_oldest"),
                (i18n_noop("Sender (A–Z)"), "sender"),
                (i18n_noop("Subject (A–Z)"), "subject"),
                (i18n_noop("Unread first"), "unread"),
                (i18n_noop("Flagged first"), "flagged"),
            ] {
                menu.append(Some(&i18n(label)), Some(&format!("sortmenu.order::{key}")));
            }
            widgets.list_sort_btn.set_menu_model(Some(&menu));

            let _ = model.list_header_widgets.set(ListHeaderWidgets {
                revealers: vec![
                    widgets.lh_search.clone(),
                    widgets.lh_unread.clone(),
                    widgets.lh_starred.clone(),
                    widgets.lh_sort.clone(),
                    widgets.lh_count.clone(),
                ],
                overflow: widgets.lh_overflow.clone(),
                unread: widgets.lh_unread.child().and_downcast::<gtk::ToggleButton>().expect("unread toggle"),
                starred: widgets.lh_starred.child().and_downcast::<gtk::ToggleButton>().expect("starred toggle"),
                sort: sort_action,
            });
            // Focus Mode's chrome as saved: nothing is mapped yet, so the
            // revealers jump to their state without animating.
            model.sync_focus_chrome();
            model.reader_overflow_btn.set_visible(model.reader_overflow_wanted());
        }

        // mailto: URIs can arrive (via GApplication `open`) before this init
        // ran — install the live sender and drain anything that queued early.
        model.rebuild_help_menu();
        model.undo_action = Some(undo_action);
        model.redo_action = Some(redo_action);
        // Both start empty, so both start greyed out.
        model.refresh_undo_menu();
        model
            .notifications
            .emit(NotifyInput::SetConsoleEnabled(model.console_mode));
        // Screenshot/dev hook: open the status bar console shortly after
        // launch (pref permitting) so captures can show it.
        // HYLKI_SHOWCASE_ABOUT=1 opens the About window a beat after
        // launch; pair it with HYLKI_SHOWCASE_TOP=1 to capture that window
        // instead of the main one.
        if std::env::var("HYLKI_SHOWCASE_ABOUT").is_ok() {
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(3, move || {
                s.input(AppMsg::OpenAbout);
            });
        }
        if std::env::var("HYLKI_SHOWCASE_CONSOLE").is_ok() {
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(3, move || {
                s.input(AppMsg::OpenConsole);
            });
        }
        let _ = MAILTO_SENDER.set(sender.input_sender().clone());
        for uri in MAILTO_PENDING.lock().unwrap().drain(..) {
            sender.input(AppMsg::OpenMailto(uri));
        }
        for uri in MID_PENDING.lock().unwrap().drain(..) {
            sender.input(AppMsg::OpenMid(uri));
        }
        for paths in ATTACH_PENDING.lock().unwrap().drain(..) {
            sender.input(AppMsg::OpenWithFiles(paths));
        }

        // Send Later (#145): the app's clock. Every half minute, any queued
        // message whose time has come is flushed by id, which sends it even
        // though the ordinary flush leaves scheduled mail alone. One try per
        // scheduling: a failure shows in the Outbox with Send Now to retry,
        // rather than a loud attempt every tick.
        {
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local(30, move || {
                s.input(AppMsg::SendDueScheduled);
                gtk::glib::ControlFlow::Continue
            });
        }
        {
            // Settings, built hidden a moment after startup (see PrewarmSettings).
            let s = sender.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
                s.input(AppMsg::PrewarmSettings);
            });
        }
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::ShowOutbox => {
                self.close_sidebar_peek();
                self.mirror_selection(crate::ui::sidebar::Sel::Outbox);
                // Treated as a folder: same list, same reader, nothing swapped
                // out from under the user. `selected` stays None so no sync or
                // server request is ever aimed at it.
                self.showing_outbox = true;
                self.leave_gallery();
                self.showing_contacts = false;
                self.unified = false;
                self.tag_view = None;
                self.selected = None;
                self.current = None;
                self.current_thread.clear();
                self.attachments.clear();
                self.attachments_loading = false;
                self.sync_attachment_drawer();
                self.message_list.emit(MessageListInput::SetSelected(None));
                self.message_list.emit(MessageListInput::SetColorize(self.accounts.len() > 1));
                self.message_list.emit(MessageListInput::ResetPaging);
                self.show_message(None, false);
                self.push_outbox();
            }

            AppMsg::OutboxItems { account_id, items } => {
                self.outbox_by_account.insert(account_id, items);
                self.push_outbox();
            }




            AppMsg::EditCurrentOutbox => {
                if let Some(m) = self.current.clone() {
                    self.compose_from_outbox(m.account_id, m.id, &sender);
                }
            }

            AppMsg::SendCurrentOutbox => {
                if let Some(m) = self.current.clone() {
                    self.send_to(m.account_id, MailRequest::FlushOutbox { id: Some(m.id) });
                }
            }

            AppMsg::SendDueScheduled => {
                let now = crate::datefmt::now();
                let due: Vec<(u32, u32)> = self
                    .outbox_by_account
                    .iter()
                    .flat_map(|(account_id, items)| {
                        items
                            .iter()
                            .filter(|i| i.send_at.is_some_and(|t| t <= now) && i.attempts <= 1)
                            .map(move |i| (*account_id, i.id))
                    })
                    .collect();
                for (account_id, id) in due {
                    self.send_to(account_id, MailRequest::FlushOutbox { id: Some(id) });
                }
            }

            AppMsg::RetryAllOutbox => {
                let accounts: Vec<u32> = self.outbox_by_account
                    .iter()
                    .filter(|(_, items)| !items.is_empty())
                    .map(|(id, _)| *id)
                    .collect();
                for account_id in accounts {
                    self.send_to(account_id, MailRequest::FlushOutbox { id: None });
                }
            }

            AppMsg::Notice(text) => {
                self.notifications.emit(NotifyInput::Push {
                    text,
                    error: false,
                    connectivity: false,
                });
            }

            AppMsg::ShowAttachments => {
                self.close_sidebar_peek();
                self.mirror_selection(crate::ui::sidebar::Sel::Attachments);
                self.showing_outbox = false;
                self.showing_gallery = true;
                self.showing_contacts = false;
                self.gallery.emit(GalleryInput::SetLoading(true));
                // Handing over the folder list is what starts the load: the
                // gallery asks for its first page once it knows the scope, so
                // the query is never run against an empty one.
                self.gallery.emit(GalleryInput::SetAccounts(self.gallery_scope()));
                // Keep the scan working through whatever is still unindexed, so
                // the archive fills in behind the user as they browse.
                self.start_attachment_scan();
            }

            AppMsg::GalleryFetch { account_id, folder_path, uid } => {
                // The gallery knows this file exists because the scan described
                // it, but its bytes were never downloaded. Reuse the reader's
                // own fetch: it stores what comes back, so the next visit finds
                // it cached and the thumbnail appears.
                self.send_to(
                    account_id,
                    MailRequest::LoadAttachments {
                        message_id: uid,
                        path: folder_path,
                        uid,
                        download: true,
                    },
                );
                self.gallery.emit(GalleryInput::SetFetching(true));
            }

            AppMsg::AttachmentsScanned { account_id, folder_path, added, remaining } => {
                let folder_path_key = folder_path.clone();
                // Keep going while this folder has more to describe. The reply
                // to each chunk is what asks for the next, so the scan runs at
                // whatever pace the server answers and never piles up requests.
                if remaining > 0 && self.showing_gallery {
                    self.send_to(account_id, MailRequest::ScanAttachments { folder_path });
                }
                // Only refresh when the scan actually found something, and only
                // while the gallery is on screen — a chunk of mail with no
                // attachments after all should not reshuffle what's on show.
                if added > 0 && self.showing_gallery {
                    self.gallery.emit(GalleryInput::ScanProgress);
                }
                self.gallery_scan_left.insert((account_id, folder_path_key), remaining);
                let left: u32 = self.gallery_scan_left.values().sum();
                self.gallery.emit(GalleryInput::ScanStatus(left));
            }

            AppMsg::GalleryLoad(req) => {
                let Some(cache) = self.cache.as_ref() else {
                    self.gallery.emit(GalleryInput::Page {
                        items: Vec::new(),
                        total: 0,
                        offset: req.offset,
                    });
                    return;
                };
                let q = crate::cache::GalleryQuery {
                    folders: &req.folders,
                    account_id: req.account_id,
                    tokens: &req.tokens,
                    bucket: req.bucket,
                    sort: req.sort,
                    limit: req.limit,
                    offset: req.offset,
                    data_cap: GALLERY_DATA_CAP,
                };
                let items = cache.gallery_page(&q);
                // Only worth counting for the first page: the total cannot
                // change while the user scrolls one set of results.
                let total = if req.offset == 0 {
                    cache.gallery_total(&q)
                } else {
                    self.gallery_total
                };
                self.gallery_total = total;
                self.gallery.emit(GalleryInput::Page { items, total, offset: req.offset });
            }

            AppMsg::OpenAttachmentMessage { account_id, folder_path, uid } => {
                self.leave_gallery();
                self.showing_contacts = false;
                self.showing_outbox = false;
                if let Some(folder) = self
                    .folders
                    .get(&account_id)
                    .and_then(|fs| fs.iter().find(|f| f.path == folder_path))
                    .cloned()
                {
                    self.select_folder(account_id, folder.id, folder.name.clone(), folder.path.clone());
                    // Messages use their UID as id, so select by (account, uid).
                    self.message_list
                        .emit(MessageListInput::SelectAndLoad((account_id, uid)));
                }
            }

            AppMsg::UnifiedSelected(view) => {
                self.open_unified(view);
            }

            AppMsg::FolderSelected { account_id, folder_id, name, path } => {
                self.close_sidebar_peek();
                self.select_folder(account_id, folder_id, name, path);
            }

            AppMsg::OpenMessageFromNotification { account_id, folder_id, message_id } => {
                // The user clicked a new-mail notification: they've engaged with
                // that account's mail, so clear its toast, then navigate to the
                // message's folder and open it in the reader.
                crate::notify::withdraw_mail(account_id);
                tracing::info!(
                    "notification: open account {account_id} folder {folder_id} message {message_id} (unified row: {})",
                    self.unified_inboxes_shown()
                );
                if let Some((name, path, kind)) = self
                    .folders
                    .get(&account_id)
                    .and_then(|fs| fs.iter().find(|f| f.id == folder_id))
                    .map(|f| (f.name.clone(), f.path.clone(), f.kind))
                {
                    // Ask for the body before anything else goes to the
                    // account's worker. Opening Inboxes below asks every
                    // account for its inbox list, and the worker serves one
                    // request at a time: the body request the selection
                    // sends afterwards would wait behind that whole fetch
                    // (and the IDLE hand-off before it), so the message the
                    // user clicked sat on a spinner while the list loaded.
                    // Sent first, it comes back into the body cache, which
                    // the selection reads before fetching. A body already
                    // there (prefetched on arrival) needs nothing.
                    if !self.body_cache.contains_key(&(account_id, message_id)) {
                        let uid = self
                            .message_cache
                            .get(&(account_id, folder_id))
                            .and_then(|msgs| msgs.iter().find(|m| m.id == message_id))
                            .map(|m| m.uid);
                        if let Some(uid) = uid {
                            self.send_to(account_id, MailRequest::LoadBody {
                                message_id,
                                path: path.clone(),
                                uid,
                            });
                        }
                    }
                    // Mail that landed in an inbox opens in the unified
                    // Inboxes when the sidebar has that row (the view the
                    // app opens with; the account's own Inbox row may sit
                    // inside a folded section, where a highlight has nowhere
                    // to show). Without the row — one account, or the
                    // preference off — or when a filter filed the message
                    // elsewhere, its folder opens directly and the sidebar
                    // unfolds the account to show where that is. Both emit
                    // the (cached) list synchronously, so the SelectAndLoad
                    // that follows finds the row and opens it.
                    if kind == FolderKind::Inbox && self.unified_inboxes_shown() {
                        self.open_unified(UnifiedView::Kind(FolderKind::Inbox));
                    } else {
                        self.select_folder(account_id, folder_id, name, path);
                    }
                    self.message_list
                        .emit(MessageListInput::SelectAndLoad((account_id, message_id)));
                }
            }

            AppMsg::NotificationMarkRead { account_id, folder_id, message_id } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    // set_read clears the account's notification itself.
                    self.user_set_flags(read_label(true), &[m], FlagKind::Read, true);
                }
            }

            AppMsg::NotificationArchive { account_id, folder_id, message_id } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    self.move_to(m, FolderKind::Archive);
                    crate::notify::withdraw_mail(account_id);
                }
            }

            AppMsg::NotificationDelete { account_id, folder_id, message_id } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    // The same path as the Delete key: to Trash, with the
                    // usual undo; a message already there asks first.
                    self.delete_messages(vec![m], &sender);
                    crate::notify::withdraw_mail(account_id);
                }
            }

            AppMsg::NotificationSpam { account_id, folder_id, message_id } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    self.mark_spam_msg(m);
                    crate::notify::withdraw_mail(account_id);
                }
            }

            // The message is opening in the reader (the action sent
            // OpenMessageFromNotification first); the composer waits for
            // the body it quotes when that is still on its way.
            AppMsg::NotificationReply { account_id, folder_id, message_id } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    let m = self.with_cached_body(m);
                    if m.body.is_empty() {
                        self.pending_notified_reply = Some(m);
                    } else {
                        self.open_inline_reply(
                            m.account_id,
                            self.reply_pgp(&m, reply_prefill(&m)),
                            Some((m.account_id, m.id)),
                            &sender,
                        );
                    }
                }
            }

            AppMsg::NotificationForward { account_id, folder_id, message_id } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    self.forward(m, true, &sender);
                }
            }

            AppMsg::ToggleCollapse(account_id) => {
                // The sidebar already animated the toggle locally; just record
                // the new state (a rebuild here would interrupt the animation).
                if let Some(email) = self.email_of(account_id) {
                    if let Some(pos) = self.collapsed.iter().position(|e| *e == email) {
                        self.collapsed.remove(pos);
                    } else {
                        self.collapsed.push(email);
                    }
                    self.save_sidebar_state();
                }
            }

            AppMsg::ToggleCustomFolders(account_id) => {
                // The sidebar animated the toggle locally; record the new state
                // (the "folders_expanded" list holds accounts whose custom
                // folders are revealed; absence means hidden, the default).
                if let Some(email) = self.email_of(account_id) {
                    if let Some(pos) = self.folders_expanded.iter().position(|e| *e == email) {
                        self.folders_expanded.remove(pos);
                    } else {
                        self.folders_expanded.push(email);
                    }
                    self.save_sidebar_state();
                }
            }

            AppMsg::FolderNodeCollapsed { account_id, path, collapsed } => {
                // The sidebar already reshaped its rows; just remember it.
                if let Some(email) =
                    self.accounts.iter().find(|a| a.id == account_id).map(|a| a.email.clone())
                {
                    let key = format!("{email}\t{path}");
                    if collapsed {
                        if !self.tree_collapsed.contains(&key) {
                            self.tree_collapsed.push(key);
                        }
                    } else {
                        self.tree_collapsed.retain(|k| *k != key);
                    }
                    self.save_sidebar_state();
                }
            }

            AppMsg::SidebarCollapsed(collapsed) => {
                tracing::info!(
                    "peek: sidebar reports collapsed={collapsed} (peek={} auto_rail={} rail_active={})",
                    self.sidebar_peek, self.auto_rail, self.rail_active
                );
                // The sidebar component has already switched its own rows; this
                // is the app-side reaction (split widths, header, persistence).
                // At a width that can host the full sidebar, the arrow inside a
                // floating peek PINS it: the sidebar becomes the normal
                // side-by-side pane (persisted) and hover-expand goes dormant
                // until it is collapsed again. In the narrow window the toggle
                // only opens/closes the overlay — there is no room to pin.
                if self.sidebar_peek && !self.auto_rail && collapsed {
                    self.pin_sidebar_from_peek();
                } else if self.auto_rail {
                    // Narrow window: the rail never expands in place — put
                    // its rows straight back and float the peek panel out
                    // (or fold it) instead. Only a showcase toggle gets here;
                    // the header button goes through AppMsg::ToggleSidebar.
                    self.sidebar.emit(SidebarInput::SetCollapsed(true));
                    self.rail_active = true;
                    self.set_sidebar_peek(!collapsed, true);
                } else {
                    // Focus Mode holding the rail borrows the sidebar's
                    // layout: expanding it for a moment is a look, not a
                    // choice, so only what is on screen moves.
                    if !self.sidebar_layout_borrowed() {
                        self.sidebar_collapsed = collapsed;
                    }
                    self.rail_active = collapsed;
                    self.animate_sidebar(collapsed);
                    self.compact_sidebar_header(collapsed);
                    if !self.sidebar_layout_borrowed() {
                        self.save_sidebar_state();
                    }
                }
            }

            AppMsg::AutoRail(on) => {
                tracing::info!("peek: auto-rail {on} (peek={})", self.sidebar_peek);
                self.auto_rail = on;
                if !on && self.sidebar_peek {
                    // Widened with the panel open: fold it back before the
                    // split view returns to side-by-side.
                    self.set_sidebar_peek(false, false);
                    self.rail_active = true;
                }
                // The rail wins while the window is narrow; the user's own
                // choice comes back the moment there is room again. Nothing is
                // persisted here — this is the window's shape, not a preference.
                let want = self.rail_wanted();
                if want != self.rail_active {
                    self.rail_active = want;
                    self.sidebar.emit(SidebarInput::SetCollapsed(want));
                    self.animate_sidebar(want);
                    self.compact_sidebar_header(want);
                }
            }

            AppMsg::ToggleSidebar => {
                tracing::info!(
                    "peek: toggle button (peek={} auto_rail={} rail_active={})",
                    self.sidebar_peek, self.auto_rail, self.rail_active
                );
                // Self-healing: with no peek open, the split should always be
                // un-collapsed and showing (rail or side-by-side). If it ended
                // hidden — a lost race in the peek machinery left users with no
                // sidebar and a dead button until relaunch (seen on 1.17.1) —
                // the button's job is to bring the sidebar back, not to feed
                // a toggle into state that already lost the plot: repair to
                // the rail first, then toggle normally from there.
                if self.sidebar_peek {
                    if self.auto_rail {
                        // Narrow window: the button folds the floating peek
                        // back to the rail.
                        self.rail_active = true;
                        self.set_sidebar_peek(false, true);
                    } else {
                        // A hover peek at a width with room for the full
                        // sidebar: the button pins it side by side.
                        self.pin_sidebar_from_peek();
                    }
                } else {
                    if let Some(split) = self.sidebar_split.clone() {
                        if split.is_collapsed() || !split.shows_sidebar() {
                            tracing::info!(
                                "peek: toggle found split hidden (collapsed={} shown={}) — repairing",
                                split.is_collapsed(),
                                split.shows_sidebar()
                            );
                            self.rail_active = true;
                            self.sidebar.emit(SidebarInput::SetCollapsed(true));
                            self.compact_sidebar_header(true);
                            self.peek_transition.set(true);
                            split.set_min_sidebar_width(SIDEBAR_RAIL_WIDTH);
                            split.set_max_sidebar_width(SIDEBAR_RAIL_WIDTH);
                            split.set_collapsed(false);
                            split.set_show_sidebar(true);
                            self.peek_transition.set(false);
                        }
                    }
                    if self.auto_rail {
                        // Narrow window: the toggle floats the peek panel
                        // out beside the rail; the rail itself never expands.
                        self.rail_active = false;
                        self.set_sidebar_peek(true, true);
                    } else {
                        self.sidebar.emit(SidebarInput::ToggleCollapsed);
                    }
                }
            }

            AppMsg::SidebarPeekDismissed => {
                tracing::info!("peek: dismissed (peek={})", self.sidebar_peek);
                if self.sidebar_peek {
                    self.rail_active = true;
                    self.set_sidebar_peek(false, true);
                }
            }

            AppMsg::SidebarHoverEnter => {
                // Hover-expand (preference): the icon rail floats the full
                // sidebar out without a click — whether the rail comes from
                // the narrow-window breakpoint or the user's own collapse.
                // The same peek the expand button opens, dismissed the same
                // ways (navigation, or a click outside the panel).
                let rail_up = self.rail_wanted();
                if self.sidebar_hover_expand && rail_up && !self.sidebar_peek {
                    self.rail_active = false;
                    self.set_sidebar_peek(true, true);
                }
            }

            AppMsg::SetSidebarHoverExpand(on) => {
                if self.sidebar_hover_expand != on {
                    self.sidebar_hover_expand = on;
                    self.save_settings();
                }
            }

            AppMsg::SetRememberSidebar(on) => {
                if self.remember_sidebar != on {
                    self.remember_sidebar = on;
                    self.save_settings();
                }
            }

            AppMsg::SetRememberRail(on) => {
                if self.remember_rail != on {
                    self.remember_rail = on;
                    self.save_settings();
                }
            }

            AppMsg::SidebarSectionsOpen {
                all_inboxes,
                filtered,
                tags,
                starred,
                sent,
                drafts,
                archive,
            } => {
                let now = (all_inboxes, filtered, tags, starred, sent, drafts, archive);
                let was = (
                    self.unified_expanded,
                    self.filtered_expanded,
                    self.tags_expanded,
                    self.starred_expanded,
                    self.sent_expanded,
                    self.drafts_expanded,
                    self.archive_expanded,
                );
                if now != was {
                    self.unified_expanded = all_inboxes;
                    self.filtered_expanded = filtered;
                    self.tags_expanded = tags;
                    self.starred_expanded = starred;
                    self.sent_expanded = sent;
                    self.drafts_expanded = drafts;
                    self.archive_expanded = archive;
                    self.save_sidebar_state();
                }
            }

            AppMsg::SetRailDots(on) => {
                if self.rail_dots != on {
                    self.rail_dots = on;
                    self.save_settings();
                    self.rebuild_sidebar();
                }
            }

            AppMsg::SetRailFold(fold) => {
                if self.rail_fold != fold {
                    self.rail_fold = fold;
                    self.save_settings();
                    // The sidebar folds (or reopens) the sections concerned
                    // itself if the rail is up.
                    self.rebuild_sidebar();
                }
            }

            AppMsg::SetAppTheme(theme) => {
                if self.app_theme != theme {
                    self.app_theme = theme;
                    apply_app_theme(theme);
                    self.save_settings();
                }
            }

            AppMsg::SetTheme(id) => {
                if self.theme != id {
                    self.theme = id.clone();
                    // Repaints the chrome and tells the reader and any open
                    // composer to re-ground their documents. The colours the
                    // scheme-dependent CSS reads back are taken a main-loop
                    // pass later, as they are on a light/dark flip, so the
                    // lookups answer for the palette that just landed.
                    crate::theme::set(&id);
                    gtk::glib::idle_add_local_once(refresh_scheme_css);
                    self.save_settings();
                }
            }

            AppMsg::SidebarContext(action) => match action {
                CtxAction::MarkFolderRead { account_id, folder_id } => {
                    self.mark_folder_read(account_id, folder_id);
                }
                CtxAction::RefreshFolder { account_id, folder_id } => {
                    if let Some(path) = self
                        .folders
                        .get(&account_id)
                        .and_then(|fs| fs.iter().find(|f| f.id == folder_id))
                        .map(|f| f.path.clone())
                    {
                        self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
                    }
                }
                CtxAction::MarkAllInboxesRead => {
                    let inboxes: Vec<(u32, u32)> = self
                        .accounts
                        .iter()
                        .filter_map(|a| self.inbox_of(a.id).map(|f| (a.id, f.id)))
                        .collect();
                    for (account_id, folder_id) in inboxes {
                        self.mark_folder_read(account_id, folder_id);
                    }
                }
                CtxAction::RefreshAllInboxes => {
                    let reqs: Vec<(u32, u32, String)> = self
                        .accounts
                        .iter()
                        .filter_map(|a| self.inbox_of(a.id).map(|f| (a.id, f.id, f.path.clone())))
                        .collect();
                    for (account_id, folder_id, path) in reqs {
                        self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
                    }
                }
                CtxAction::ApplyFilters { account_id, folder_id } => {
                    self.start_filter_run(vec![(account_id, folder_id)], &sender);
                }
                CtxAction::EditFilter { account_id, path } => {
                    // The first rule filing into this folder, in the
                    // Filters page's editor.
                    let email = self.email_of(account_id);
                    let index = email.and_then(|email| {
                        self.filters.iter().position(|r| {
                            r.account_email.eq_ignore_ascii_case(&email) && r.dest_path == path
                        })
                    });
                    if let Some(i) = index {
                        self.open_settings_window(&sender, true, false);
                        if let Some(p) = &self.prefs {
                            p.emit(PrefInput::ShowPageById("filters".to_string()));
                        }
                        if let Some(acc) = &self.accounts_win {
                            acc.emit(crate::ui::accounts::AccountsInput::EditFilter(i));
                        }
                    }
                }
                CtxAction::EditTag(keyword) => {
                    if let Some(i) = self.tags.iter().position(|t| t.keyword == keyword) {
                        self.open_settings_window(&sender, true, false);
                        if let Some(p) = &self.prefs {
                            p.emit(PrefInput::ShowPageById("tags".to_string()));
                        }
                        if let Some(acc) = &self.accounts_win {
                            acc.emit(crate::ui::accounts::AccountsInput::EditTag(i));
                        }
                    }
                }
                CtxAction::OpenAccountSettings(account_id) => {
                    // Straight to this account's editor, not the accounts list.
                    let email = self
                        .accounts
                        .iter()
                        .find(|a| a.id == account_id)
                        .map(|a| a.email.clone());
                    self.open_settings_window(&sender, true, false);
                    if let (Some(email), Some(acc)) = (email, &self.accounts_win) {
                        acc.emit(crate::ui::accounts::AccountsInput::EditAccountByEmail(email));
                    }
                }
                CtxAction::RemoveAccount(account_id) => {
                    self.confirm_remove_account(account_id, &sender);
                }
                CtxAction::NewFolder(account_id) => {
                    self.prompt_new_folder(account_id, &sender);
                }
                CtxAction::DeleteFolder { account_id, name, path } => {
                    self.confirm_delete_folder(account_id, name, path, &sender);
                }
                CtxAction::RenameFolder { account_id, name, path } => {
                    self.prompt_rename_folder(account_id, name, path, &sender);
                }
                CtxAction::EmptyFolder { account_id, folder_id, name, path } => {
                    self.confirm_empty_folder(account_id, folder_id, name, path, &sender);
                }
                CtxAction::HideFolder { account_id, path } => {
                    self.hide_folders(account_id, vec![path]);
                }
            },

            AppMsg::DropMoveMessages { dest_account, dest, items } => {
                self.drop_move(dest_account, dest, items);
            }

            AppMsg::CreateFolder { account_id, name } => {
                let name = name.trim();
                if !name.is_empty() {
                    // The server names mailboxes in modified UTF-7, so a name
                    // with any non-ASCII character has to be encoded before it
                    // becomes a path (issue #1, in the other direction).
                    let path = format!(
                        "{}{}",
                        self.folder_namespace(account_id),
                        crate::mutf7::encode(name)
                    );
                    self.create_folder(account_id, path.clone());
                    self.push_undo(
                        account_id,
                        i18n("Create Folder"),
                        UndoStep::DeleteFolder { path },
                    );
                }
            }

            AppMsg::MoveFolder { account_id, path, dest } => {
                self.move_folder(account_id, path, dest);
            }

            AppMsg::RenameFolderTo { account_id, path, new_name } => {
                self.rename_folder_to(account_id, path, new_name);
            }

            AppMsg::EmptyFolder { account_id, folder_id, path } => {
                // Gone locally at once; the worker's empty message list and
                // zero count confirm it, or an error says why not.
                self.message_cache.remove(&(account_id, folder_id));
                self.folder_unread.insert((account_id, folder_id), 0);
                if self.selected.as_ref().is_some_and(|s| s.account_id == account_id && s.folder_id == folder_id) {
                    self.current = None;
                    self.current_thread.clear();
                    self.show_message(None, false);
                    self.message_list.emit(MessageListInput::SetLoading);
                }
                self.push_unread_counts();
                self.send_to(account_id, MailRequest::EmptyFolder { folder_id, path });
            }

            // Not recorded: deleting a folder cannot be taken back (#200).
            // The confirmation says so before it happens.
            AppMsg::DeleteFolder { account_id, path } => {
                self.delete_folder(account_id, path);
            }

            AppMsg::AccountsReordered(emails) => {
                // Display order only (by email) — no reconnect needed.
                if !emails.is_empty() {
                    self.account_order = emails;
                    self.save_sidebar_state();
                    self.rebuild_sidebar();
                }
            }

            AppMsg::ClearReader => {
                self.current = None;
                self.current_thread.clear();
                self.attachments.clear();
                self.attachments_loading = false;
                self.sync_attachment_drawer();
                self.show_message(None, false);
            }
            AppMsg::SearchActive(active) => {
                if active {
                    // Snapshot every folder's indexed messages so the search can
                    // span the whole mailbox.
                    self.message_list.emit(MessageListInput::SetSearchPool(
                        build_search_pool(&self.message_cache),
                    ));
                    // Results span accounts; tint rows by account (as in the unified
                    // inbox) so their origin is legible.
                    if self.accounts.len() > 1 {
                        self.message_list.emit(MessageListInput::SetColorize(true));
                    }
                } else {
                    self.message_list
                        .emit(MessageListInput::SetSearchPool(Vec::new()));
                    // Restore the tint state the underlying view wants.
                    self.message_list
                        .emit(MessageListInput::SetColorize(self.unified));
                }
            }
            AppMsg::MessageSelected { message: m, thread, solo } => {
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
                // A queued message reads like any other, straight from the bytes
                // already on disk — no folder, no UID, nothing to ask the server.
                if let Some(item) = self.outbox_item(m.account_id, m.id) {
                    self.show_outbox_message(&item);
                    return;
                }
                // Selecting a draft opens it in the compose editor, in the
                // reading pane where the message would otherwise show.
                if self.is_drafts_folder(m.account_id, m.folder_id) {
                    self.open_draft(m, true, HandOffFiles::default(), &sender);
                    return;
                }
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
                    self.body_cache.get(&(account_id, m.id)).cloned()
                };
                let needs_body = cached_body.is_none();

                if m.unread {
                    match self.read_mark {
                        config::ReadMark::Shown => self.mark_opened_read(&m),
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

                let mut current = m.clone();
                current.unread = false;
                if let Some(body) = cached_body {
                    current.body = body;
                }
                self.current = Some(current.clone());
                // A new selection: whatever the last one was waiting for no
                // longer holds this one back.
                self.thread_related_pending = false;

                // The folder being read holds only its half of the conversation:
                // your own replies are filed in Sent, and older parts may have
                // been archived. Ask for the rest from the cache (#21).
                // …unless the user picked this reply out of a conversation already
                // on screen. They asked for one message; assembling its thread
                // again would swap the conversation back in under them.
                if self.threading && !solo {
                    let only = [m.clone()];
                    let ids = thread_ids(if thread.is_empty() { &only[..] } else { &thread });
                    if !ids.is_empty() {
                        self.send_to(account_id, MailRequest::LoadRelated {
                            message_id: m.id,
                            ids,
                        });
                        self.thread_related_pending = true;
                    }
                }

                if thread.len() > 1 {
                    self.thread_key = Some((account_id, m.id));
                    // Already assembled: paint it now. No lookup, no body
                    // gathering, no spinner — returning to a thread shouldn't
                    // cost what opening it did.
                    tracing::debug!(
                        target: "hylki::undo",
                        "select {}: thread of {}, remembered={}, needs_body={needs_body}",
                        m.id,
                        thread.len(),
                        self.thread_cache.contains_key(&(account_id, m.id)),
                    );
                    if let Some(cached) = self.thread_cache.get(&(account_id, m.id)).cloned() {
                        self.current_thread = cached;
                        // The message just opened is read, whatever the stored
                        // copy said when it was put away.
                        for tm in self.current_thread.iter_mut() {
                            if tm.id == m.id && tm.account_id == account_id {
                                tm.unread = false;
                                tm.body = current.body.clone();
                            }
                        }
                        self.thread_painted = true;
                        self.thread_related_pending = false;
                        self.show_thread();
                        self.load_thread_attachments();
                    } else {
                        // Conversation: assemble the thread with any cached bodies,
                        // request the rest, and render it as a scrollable conversation.
                        let mut conv: Vec<Message> = Vec::with_capacity(thread.len());
                        for tm in &thread {
                            let mut tm = tm.clone();
                            // The others keep their real unread state: each is marked
                            // in the reader until it is scrolled through. Only the one
                            // just opened is read outright.
                            if tm.id == m.id && tm.account_id == account_id {
                                tm.unread = false;
                                tm.body = current.body.clone();
                            } else if tm.body.is_empty() {
                                if let Some(b) = self.body_cache.get(&(tm.account_id, tm.id)) {
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
                        self.queue_thread_render(&sender);
                        // Fetch the bodies we don't have yet (primary first via order).
                        let to_load: Vec<MissingBody> = self
                            .current_thread
                            .iter()
                            .filter(|tm| tm.body.is_empty())
                            .filter_map(|tm| {
                                self.resolve_folder_path(tm)
                                    .map(|p| (tm.account_id, tm.id, tm.uid, p))
                            })
                            .collect();
                        for ((aid, path), items) in batch_bodies_by_folder(to_load) {
                            self.send_to(aid, MailRequest::LoadBodies { items, path });
                        }
                        self.show_thread();
                        self.load_thread_attachments();
                    }
                } else {
                    self.current_thread.clear();
                    self.thread_key = None;
                    let display = current;
                    // Already on screen, pixel for pixel: leave it alone.
                    let unchanged = !needs_body
                        && !was_thread
                        && showing.is_some_and(|(aid, mid, body, unread, starred, keywords)| {
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
                        });
                    tracing::debug!(
                        target: "hylki::undo",
                        "select {}: single message, needs_body={needs_body}, unchanged={unchanged}",
                        m.id,
                    );
                    if unchanged {
                        return;
                    }
                    // Request the body FIRST so it renders before attachments — the
                    // worker processes requests in order, so the body must come first.
                    if needs_body {
                        if let Some(path) = folder_path.clone() {
                            self.send_to(account_id, MailRequest::LoadBody {
                                message_id: m.id,
                                path,
                                uid: m.uid,
                            });
                        }
                    }
                    self.show_message(Some(display), needs_body);
                }

                // Attachments: use the in-memory cache if present; otherwise
                // fetch them (disk cache first, then the server). Opening the
                // message is the request — the paperclip appears when they
                // land, with no "load attachments" click in between.
                if m.has_attachment {
                    if let Some(cached) = self.attachment_cache.get(&(account_id, m.id)).cloned() {
                        self.attachments = cached;
                        self.sync_attachment_drawer();
                    } else if let Some(path) = folder_path {
                        self.attachments_loading = true;
                        self.send_to(account_id, MailRequest::LoadAttachments {
                            message_id: m.id,
                            path,
                            uid: m.uid,
                            download: true,
                        });
                    }
                }
            }

            AppMsg::ThreadGrew { message: m, thread } => {
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
                        if let Some(b) = self.body_cache.get(&k) {
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
                self.thread_key = Some(key);
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
                    self.message_view.emit(MessageViewInput::HoldPlace { account_id, id });
                }
                let to_load: Vec<MissingBody> = self
                    .current_thread
                    .iter()
                    .filter(|tm| tm.body.is_empty())
                    .filter_map(|tm| {
                        self.resolve_folder_path(tm).map(|p| (tm.account_id, tm.id, tm.uid, p))
                    })
                    .collect();
                if to_load.is_empty() {
                    self.show_thread();
                } else {
                    for ((aid, path), items) in batch_bodies_by_folder(to_load) {
                        self.send_to(aid, MailRequest::LoadBodies { items, path });
                    }
                }
                self.load_thread_attachments();
            }

            AppMsg::OpenMessageWindow { message: m, thread } => {
                // Drafts open in the editor rather than a read-only window.
                if self.is_drafts_folder(m.account_id, m.folder_id) {
                    self.open_draft(m, false, HandOffFiles::default(), &sender);
                } else {
                    // Popouts follow the reading pane's display order (#70).
                    let mut thread = thread;
                    if self.thread_newest_first {
                        thread.reverse();
                    }
                    self.open_message_window(m, thread, &sender);
                }
            }

            AppMsg::PopoutClosed(key) => {
                self.popouts.remove(&key);
            }

            AppMsg::AddContactFrom { name, email } => {
                self.show_add_contact_dialog(&name, &email, &sender);
            }

            AppMsg::LoadAttachmentsFor(m) => {
                let m = *m;
                if let Some(path) = self.resolve_folder_path(&m) {
                    self.send_to(m.account_id, MailRequest::LoadAttachments {
                        message_id: m.id,
                        path,
                        uid: m.uid,
                        download: true,
                    });
                }
            }

            AppMsg::OpenAttachmentItem(att) => {
                crate::ui::attachments_gallery::open_bytes(
                    &att.name,
                    &att.data,
                    Some(self.window.upcast_ref()),
                );
            }

            AppMsg::SaveAttachmentItems(items) => {
                save_all_attachments(items, Some(self.window.clone()));
            }

            AppMsg::ToggleStar => {
                if let Some(m) = self.reply_target() {
                    if self.thread_star_target(&m) {
                        // The conversation is the target: any member starred
                        // reads as a starred thread, and the toggle clears or
                        // sets the whole set (same semantics as the list row).
                        let any = self.current_thread.iter().any(|t| t.starred);
                        let members = self.current_thread.clone();
                        self.user_set_flags(
                            star_label(!any),
                            &members,
                            FlagKind::Star,
                            !any,
                        );
                    } else {
                        let starred = !m.starred;
                        self.user_set_flags(star_label(starred), &[m], FlagKind::Star, starred);
                    }
                }
            }

            AppMsg::Archive => {
                if let Some(m) = self.reply_target() {
                    self.move_to(m, FolderKind::Archive);
                }
            }
            AppMsg::Delete => {
                // More than one message selected: the trash button deletes the
                // whole selection, exactly as the bulk bar's Delete would.
                if self.list_selection.len() > 1 {
                    self.message_list.emit(MessageListInput::Bulk(BulkAction::Delete));
                    return;
                }
                // A lone list-row selection over an open conversation may be
                // its thread head, which stands for the whole thread — the
                // list resolves that (DeleteThread) or falls back to a plain
                // bulk delete. A picked card is always just that message.
                if !self.selection_from_cards
                    && self.list_selection.len() == 1
                    && self.current_thread.len() > 1
                {
                    self.message_list.emit(MessageListInput::ResolveDelete);
                    return;
                }
                if let Some(m) = self.reply_target() {
                    // In the Outbox, deleting means giving up on sending it —
                    // there is no server-side copy to move to Trash.
                    if self.outbox_item(m.account_id, m.id).is_some() {
                        self.send_to(m.account_id, MailRequest::DeleteOutbox { id: m.id });
                        self.current = None;
                        self.show_message(None, false);
                    } else {
                        self.delete_messages(vec![m], &sender);
                    }
                }
            }

            AppMsg::Related { account_id, message_id, messages } => {
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
                }
            }

            AppMsg::ThreadsListed { groups } => {
                self.request_thread_counts(groups);
            }

            AppMsg::ThreadCounts { account_id, counts } => {
                let counts: Vec<((u32, String), usize)> =
                    counts.into_iter().map(|(root, n)| ((account_id, root), n)).collect();
                if !counts.is_empty() {
                    self.message_list.emit(MessageListInput::SetThreadCounts(counts));
                }
            }

            AppMsg::PurgeMessages(messages) => {
                self.purge_messages(messages);
            }

            AppMsg::CardAction { action, message } => {
                // A reply started from a card belongs in the main window's
                // inline composer, exactly like the toolbar's buttons — not in
                // a separate compose window.
                let m = self.with_cached_body(*message);
                tracing::debug!(
                    target: "hylki::reply",
                    "card {:?}: {}:{} from={}",
                    action, m.account_id, m.id, m.from_addr,
                );
                match action {
                    RowAction::Reply => {
                        self.open_inline_reply(m.account_id, self.reply_pgp(&m, reply_prefill(&m)), Some((m.account_id, m.id)), &sender);
                    }
                    RowAction::ReplyAll => {
                        let self_email = self.email_of(m.account_id).unwrap_or_default();
                        self.open_inline_reply(
                            m.account_id,
                            self.reply_pgp(&m, reply_all_prefill(&m, &self_email)),
                            Some((m.account_id, m.id)),
                            &sender,
                        );
                    }
                    RowAction::Forward => {
                        self.forward(m, true, &sender);
                    }
                    // Cards only carry the three above; anything else falls
                    // through to the ordinary row behaviour.
                    other => sender.input(AppMsg::RowAction {
                        action: other,
                        message: Box::new(m),
                        // A card is always its own message.
                        conversation: Vec::new(),
                    }),
                }
            }

            AppMsg::ToggleReadCurrent => {
                if let Some(m) = self.reply_target() {
                    let read = m.unread;
                    self.user_set_flags(read_label(read), &[m], FlagKind::Read, read);
                }
            }

            AppMsg::CardContact(message) => {
                self.show_add_contact_dialog(&message.from_name, &message.from_addr, &sender);
            }

            AppMsg::RowAction { action, message, conversation } => {
                let m = *message;
                if matches!(action, RowAction::Reply | RowAction::ReplyAll | RowAction::Forward) {
                    tracing::debug!(
                        target: "hylki::reply",
                        "row {:?}: {}:{} from={} conversation={:?}",
                        action, m.account_id, m.id, m.from_addr,
                        conversation.iter().map(|c| c.id).collect::<Vec<_>>(),
                    );
                }
                if self.outbox_item(m.account_id, m.id).is_some() {
                    // Nothing else in the palette applies to an unsent message,
                    // and every other action would aim an IMAP command at a UID
                    // that doesn't exist on any server.
                    if matches!(action, RowAction::Delete) {
                        self.send_to(m.account_id, MailRequest::DeleteOutbox { id: m.id });
                    }
                    return;
                }
                match action {
                    // A row that stands for a whole conversation answers the
                    // conversation's newest message, exactly as the reader's
                    // own reply does (#210) — the row is filed under the
                    // thread's head, which is its oldest message.
                    RowAction::Reply => {
                        let m = self.newest_to_answer(&conversation, m);
                        let m = self.with_cached_body(m);
                        self.open_compose(m.account_id, self.reply_pgp(&m, reply_prefill(&m)), &sender);
                    }
                    RowAction::ReplyAll => {
                        let m = self.newest_to_answer(&conversation, m);
                        let m = self.with_cached_body(m);
                        let self_email = self.email_of(m.account_id).unwrap_or_default();
                        self.open_compose(
                            m.account_id,
                            self.reply_pgp(&m, reply_all_prefill(&m, &self_email)),
                            &sender,
                        );
                    }
                    RowAction::Forward => {
                        let m = self.newest_to_answer(&conversation, m);
                        self.forward(m, false, &sender);
                    }
                    // A conversation row stands for its newest message here
                    // too: that is the one the row is showing.
                    RowAction::EditAsNew => {
                        let m = self.newest_to_answer(&conversation, m);
                        self.edit_as_new(m, &sender);
                    }
                    RowAction::ToggleStar => {
                        let starred = !m.starred;
                        self.user_set_flags(star_label(starred), &[m], FlagKind::Star, starred);
                    }
                    RowAction::ToggleRead => {
                        let read = m.unread;
                        self.user_set_flags(read_label(read), &[m], FlagKind::Read, read);
                    }
                    RowAction::Spam => self.mark_spam_msg(m),
                    RowAction::NotSpam => self.mark_ham_msg(m),
                    RowAction::Archive => self.move_to(m, FolderKind::Archive),
                    RowAction::MoveToInbox => self.move_to(m, FolderKind::Inbox),
                    RowAction::Delete => self.delete_messages(vec![m], &sender),
                    RowAction::ViewSource => {
                        if let Some(path) = self.resolve_folder_path(&m) {
                            self.send_to(m.account_id, MailRequest::LoadSource {
                                message_id: m.id,
                                path,
                                uid: m.uid,
                            });
                        }
                    }
                    RowAction::AddContact => {
                        self.show_add_contact_dialog(&m.from_name, &m.from_addr, &sender);
                    }
                }
            }

            AppMsg::Bulk { action, messages } => {
                if self.showing_outbox {
                    if matches!(action, BulkAction::Delete) {
                        for m in &messages {
                            self.send_to(m.account_id, MailRequest::DeleteOutbox { id: m.id });
                        }
                    }
                    self.message_list.emit(MessageListInput::ClearSelection);
                    return;
                }
                match action {
                    // Flag/read changes update rows in place (no removal).
                    BulkAction::MarkRead => {
                        self.user_set_flags(read_label(true), &messages, FlagKind::Read, true)
                    }
                    BulkAction::MarkUnread => {
                        self.user_set_flags(read_label(false), &messages, FlagKind::Read, false)
                    }
                    BulkAction::Flag => {
                        self.user_set_flags(star_label(true), &messages, FlagKind::Star, true)
                    }
                    BulkAction::Unflag => {
                        self.user_set_flags(star_label(false), &messages, FlagKind::Star, false)
                    }
                    // Archive/Delete/Spam remove every selected row. Doing that one
                    // at a time blocks the UI thread (a render cycle per message) and
                    // trips GTK's "app is not responding" dialog for large selections.
                    // Batch it; for big selections show a spinner and defer the apply
                    // one tick so the spinner paints before the blocking work runs.
                    BulkAction::Archive
                    | BulkAction::Spam
                    | BulkAction::NotSpam
                    | BulkAction::Delete
                    | BulkAction::MoveToInbox => {
                        // Deleting in Trash means erasing, not moving — split the
                        // selection so each half takes the right path (and the
                        // erasures get their confirmation).
                        if matches!(action, BulkAction::Delete)
                            && messages.iter().any(|m| self.in_trash(m))
                        {
                            self.delete_messages(messages, &sender);
                            return;
                        }
                        // The apply is one batched widget update, so no overlay
                        // needed at any size: rows vanish at once, the server
                        // work runs invisibly in the workers, and the refresh
                        // spinner + status bar carry the progress story.
                        self.apply_bulk_move(action, messages);
                    }
                }
            }

            AppMsg::BulkComplete => {
                self.bulk_pending = self.bulk_pending.saturating_sub(1);
                self.update_busy_indicator();
                if self.bulk_pending == 0 && self.busy.is_empty() {
                    self.notifications.emit(NotifyInput::SetStatus(String::new()));
                }
            }

            AppMsg::Refresh => {
                if self.unified {
                    let reqs = self.unified_targets();
                    for (account_id, folder_id, path) in reqs {
                        self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
                    }
                } else {
                    match self.selected.clone() {
                        Some(sel) => self.send_to(sel.account_id, MailRequest::LoadMessages {
                            folder_id: sel.folder_id,
                            path: sel.path,
                        }),
                        None => {
                            for w in self.workers.values() {
                                let _ = w.send(MailRequest::Reconnect);
                            }
                        }
                    }
                }
                // Only the visible folder (and the IDLE-watched inbox) re-synced
                // above; every other folder's unread chip would drift until it
                // was selected. Have each account re-check them all.
                for w in self.workers.values() {
                    let _ = w.send(MailRequest::RefreshUnread);
                }
                // A tag view lists what the index holds, and the index only
                // learns a folder's flags when that folder syncs: have the
                // accounts in scope re-read their keywords (#166).
                self.sync_tag_keywords();
            }

            AppMsg::ShowcaseFolder { kind, account } => {
                let account = account.unwrap_or_else(|| self.active_account());
                let found = self
                    .folders
                    .get(&account)
                    .and_then(|fs| fs.iter().find(|f| f.kind == kind))
                    .map(|f| (f.id, f.name.clone(), f.path.clone()));
                if let Some((id, name, path)) = found {
                    self.select_folder(account, id, name, path);
                }
            }

            AppMsg::Compose => {
                let (account, prefill) =
                    self.new_message_from(self.active_account(), ComposePrefill::default());
                if self.compose_inline {
                    // The new-message pane slides down over the reader,
                    // exactly like an inline reply — same composer, same
                    // pop-out-to-window toggle in its header.
                    self.open_inline_reply(account, prefill, None, &sender);
                } else {
                    self.open_compose(account, prefill, &sender);
                }
            }

            AppMsg::Reply => {
                if let Some(m) = self.compose_target() {
                    self.open_inline_reply(m.account_id, self.reply_pgp(&m, reply_prefill(&m)), Some((m.account_id, m.id)), &sender);
                }
            }

            AppMsg::ReplyAll => {
                if let Some(m) = self.compose_target() {
                    let self_email = self.email_of(m.account_id).unwrap_or_default();
                    self.open_inline_reply(
                        m.account_id,
                        self.reply_pgp(&m, reply_all_prefill(&m, &self_email)),
                        Some((m.account_id, m.id)),
                        &sender,
                    );
                }
            }

            AppMsg::Forward => {
                if let Some(m) = self.compose_target() {
                    self.forward(m, true, &sender);
                }
            }

            AppMsg::AddToContacts => {
                if let Some(m) = self.reply_target() {
                    self.show_add_contact_dialog(&m.from_name, &m.from_addr, &sender);
                }
            }

            AppMsg::AddContactAddr(addr) => {
                // From an address's right-click menu: only the address is
                // known; the dialog's name field starts blank for the user.
                self.show_add_contact_dialog("", &addr, &sender);
            }

            AppMsg::ContactAdded(result) => {
                use crate::contacts::AddOutcome;
                let (text, error) = match result {
                    Ok(AddOutcome::Created) => (i18n("Added to Contacts"), false),
                    Ok(AddOutcome::Merged(name)) => (i18n_f("Added email to {name}", &[("name", &name)]), false),
                    Ok(AddOutcome::AlreadyPresent(name)) => {
                        (i18n_f("Already in Contacts ({name})", &[("name", &name)]), false)
                    }
                    Err(e) => (i18n_f("Could not add contact: {e}", &[("e", &e.to_string())]), true),
                };
                self.notifications.emit(NotifyInput::Push { text, error, connectivity: false });
            }

            AppMsg::ViewSource => {
                if let Some(m) = self.current.clone() {
                    if let Some(path) = self.resolve_folder_path(&m) {
                        self.send_to(m.account_id, MailRequest::LoadSource {
                            message_id: m.id,
                            path,
                            uid: m.uid,
                        });
                    }
                }
            }

            AppMsg::OpenAbout => {
                self.open_about(&sender);
            }

            AppMsg::AllowSender(addr) | AppMsg::AddSender(addr) => {
                let addr = addr.trim().to_lowercase();
                if !addr.is_empty() && !self.allowed_senders.contains(&addr) {
                    self.allowed_senders.push(addr);
                    self.save_settings();
                }
            }

            AppMsg::Unsubscribe { message, info } => {
                if std::env::var("HYLKI_SHOWCASE_UNSUB").is_ok_and(|v| v == "go" || v == "fail") {
                    self.unsubscribe_go(*message, *info, &sender);
                } else {
                    self.confirm_unsubscribe(*message, *info, &sender);
                }
            }
            AppMsg::UnsubscribeGo { message, info } => {
                self.unsubscribe_go(*message, *info, &sender);
            }
            AppMsg::UnsubscribeByMail { message, info } => {
                self.unsubscribe_by_mail(*message, &info, &sender);
            }
            AppMsg::InviteAction { message, invite, action } => {
                self.invite_action(*message, *invite, action);
            }
            AppMsg::UnsubscribeDone { message, result } => {
                let key = (message.account_id, message.id);
                match result {
                    Ok(left) => {
                        if left {
                            self.record_unsubscribed(&message);
                            self.notifications.emit(NotifyInput::Push {
                                text: i18n_f(
                                    "Unsubscribed from {name}.",
                                    &[("name", &sender_label(&message))],
                                ),
                                error: false,
                                connectivity: false,
                            });
                        } else {
                            self.notifications.emit(NotifyInput::Push {
                                text: i18n("The list's unsubscribe page is open in your browser."),
                                error: false,
                                connectivity: false,
                            });
                        }
                        self.push_unsubscribe_state(key, None);
                    }
                    Err(why) => {
                        tracing::warn!("unsubscribe failed for {}: {why}", message.from_addr);
                        self.push_unsubscribe_state(key, Some(UnsubState::Failed(why)));
                    }
                }
            }

            AppMsg::RemoveSender(addr) => {
                let addr = addr.to_lowercase();
                self.allowed_senders.retain(|s| *s != addr);
                self.save_settings();
            }

            AppMsg::AddBlacklist(addr) => {
                let addr = addr.trim().trim_start_matches('@').to_lowercase();
                if !addr.is_empty() && !self.blacklist.contains(&addr) {
                    self.blacklist.push(addr);
                    self.save_settings();
                    // Sweep mail already in view from the newly-blocked sender.
                    self.sweep_blacklisted();
                }
            }

            AppMsg::RemoveBlacklist(addr) => {
                let addr = addr.to_lowercase();
                self.blacklist.retain(|s| *s != addr);
                self.save_settings();
            }

            AppMsg::MarkSpam => {
                // In Junk the same button, shortcut and menu entry mean the
                // reverse (#168).
                if let Some(m) = self.reply_target() {
                    if self.in_junk(&m) {
                        self.mark_ham_msg(m);
                    } else {
                        self.mark_spam_msg(m);
                    }
                }
            }

            AppMsg::SetAvatars(on) => {
                if self.avatars != on {
                    self.avatars = on;
                    self.save_settings();
                    self.push_list_look(false);
                }
            }

            AppMsg::SetFocusMode(focus) => self.set_focus_mode(focus),

            AppMsg::ListOverflowMenu => self.show_list_overflow_menu(&sender),

            AppMsg::SetOwnMailboxFace(on) => {
                if self.own_mailbox_face != on {
                    self.own_mailbox_face = on;
                    self.save_settings();
                    self.refresh_own_faces();
                    // Switched on, the accounts that want a Gravatar may never
                    // have been asked about (the switch was off at startup).
                    if on {
                        self.warm_own_gravatars(&sender);
                    }
                    self.refresh_faces();
                }
            }

            AppMsg::OwnGravatarFetched { email, outcome } => {
                // Nothing to redraw unless the address turned out to have one.
                if crate::avatar::cache_own_gravatar(&email, outcome) {
                    self.rebuild_sidebar();
                    self.refresh_faces();
                }
            }

            AppMsg::SetSenderLogos(on) => {
                if self.sender_logos != on {
                    self.sender_logos = on;
                    self.save_settings();
                    self.message_list.emit(MessageListInput::SetSenderLogos(on));
                }
            }

            AppMsg::SetReaderActionsCollapsed(on) => {
                self.reader_actions_collapsed = on;
                self.reader_overflow_btn.set_visible(self.reader_overflow_wanted());
            }

            AppMsg::SetReaderToolbar(layout) => {
                if self.reader_toolbar != layout {
                    self.reader_toolbar = layout;
                    config::save_reader_toolbar(&self.reader_toolbar);
                    self.relayout_reader_toolbar();
                    // Not while the inline composer covers the header: that
                    // path hides the ⋯ by hand and restores it on close.
                    if !self.reader_compose.as_ref().is_some_and(|r| r.window.is_none()) {
                        self.reader_overflow_btn.set_visible(self.reader_overflow_wanted());
                    }
                }
            }

            AppMsg::ReaderControlsChanged(controls) => {
                if let Some(tb) = self.reader_toolbar_widgets.get() {
                    tb.controls.set(controls);
                }
                self.refresh_reader_breakpoint();
            }

            AppMsg::ReaderOverflowMenu => self.show_reader_overflow_menu(&sender),

            AppMsg::SetShowRemoteBanner(on) => {
                if self.show_remote_banner != on {
                    self.show_remote_banner = on;
                    self.save_settings();
                    self.message_view.emit(MessageViewInput::SetBannerShown(on));
                }
            }

            AppMsg::SetAutoRemoteContent(on) => {
                if self.auto_remote_content != on {
                    self.auto_remote_content = on;
                    self.save_settings();
                    // Re-render what is open so the change takes effect there too:
                    // on, the blocked content loads; off, it is stripped again.
                    if self.current_thread.len() > 1 {
                        self.show_thread();
                    } else {
                        let current = self.current.clone();
                        self.show_message(current, false);
                    }
                }
            }

            AppMsg::SetDateStyle(style) => {
                if self.date_style != style {
                    self.date_style = style;
                    self.save_settings();
                    self.apply_date_style();
                }
            }

            AppMsg::SetLanguage(code) => {
                // The combo also notifies as its model is set; only a real
                // change is saved and announced.
                if config::load_language() != code {
                    config::save_language(&code);
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n("The language applies the next time Hylki starts."),
                        error: false,
                        connectivity: false,
                    });
                }
            }

            AppMsg::WizardLanguage(code) => {
                // The drop-down also notifies as it is set up; only a real
                // change counts. Saved, then Hylki restarts through the same
                // helper the icon change uses, so every window — the wizard
                // first, since it is not completed yet — comes up in the
                // chosen language. Should the helper fail, the wizard alone
                // is rebuilt in it (gettext reads LANGUAGE on every lookup).
                if config::load_language() == code {
                    return;
                }
                config::save_language(&code);
                tracing::info!("wizard: language {code:?} saved, restarting");
                // The instance that comes back must open the wizard again,
                // review mode or not: the helper drops the review switch on
                // purpose, so a one-shot flag of its own goes along (init
                // takes it and clears it, so it never outlives that start).
                std::env::set_var(crate::WIZARD_AGAIN_VAR, "1");
                match crate::app_icon::launch_restart_helper() {
                    Ok(()) => {
                        // The quit action's own teardown, inline: nothing to
                        // look up, nothing to wait for.
                        tracing::info!("wizard: restart helper up, exiting");
                        let (w, h, maximized) = crate::config::load_window_state();
                        crate::config::save_window_state(w, h, maximized);
                        std::process::exit(0);
                    }
                    Err(e) => {
                        std::env::remove_var(crate::WIZARD_AGAIN_VAR);
                        tracing::warn!("restart for the wizard's language failed: {e}");
                        crate::i18n::apply_language(&code);
                        if let Some(old) = self.welcome.take() {
                            old.widget().set_visible(false);
                        }
                        self.open_wizard(&sender);
                    }
                }
            }

            AppMsg::SetClockStyle(style) => {
                if self.clock_style != style {
                    self.clock_style = style;
                    self.save_settings();
                    self.apply_date_style();
                }
            }

            AppMsg::SetGravatar(on) => {
                if self.gravatar != on {
                    self.gravatar = on;
                    self.save_settings();
                    self.message_list.emit(MessageListInput::SetGravatar(on));
                    // Refresh the reader's avatar for the open message.
                    let current = self.current.clone();
                    self.show_message(current, false);
                }
            }

            AppMsg::ShowLightbox { items, start } => {
                if !items.is_empty() {
                    self.lightbox_pos = start.min(items.len() - 1);
                    self.lightbox_items = items;
                    self.lightbox_open.set(true);
                    self.lightbox_set_zoom(1);
                    self.lightbox_refresh(&sender);
                }
            }
            AppMsg::LightboxPrev => self.lightbox_step(-1, &sender),
            AppMsg::LightboxNext => self.lightbox_step(1, &sender),
            AppMsg::LightboxClose => {
                self.lightbox_items.clear();
                self.lightbox_texture = None;
                self.lightbox_open.set(false);
                self.lightbox_set_zoom(1);
            }
            AppMsg::LightboxZoomCycle { x, y } => {
                if self.lightbox_zoom == 1 {
                    self.lightbox_zoom_to_point(x, y);
                } else {
                    self.lightbox_set_zoom(1);
                }
            }
            AppMsg::LightboxEscape => {
                // Zoomed in, Escape returns to the fitted view; from there it
                // closes the lightbox.
                if self.lightbox_zoom != 1 {
                    self.lightbox_set_zoom(1);
                } else {
                    sender.input(AppMsg::LightboxClose);
                }
            }
            AppMsg::LightboxOpenCurrent => {
                if let Some(att) = self.lightbox_items.get(self.lightbox_pos) {
                    crate::ui::attachments_gallery::open_bytes(
                        &att.name,
                        &att.data,
                        Some(self.window.upcast_ref()),
                    );
                }
            }
            AppMsg::LightboxDownloadCurrent => {
                if let Some(att) = self.lightbox_items.get(self.lightbox_pos).cloned() {
                    let dialog = gtk::FileDialog::builder()
                        .initial_name(&att.name)
                        .title(&i18n("Save Attachment"))
                        .build();
                    dialog.save(Some(&self.window), gtk::gio::Cancellable::NONE, move |res| {
                        if let Ok(file) = res {
                            if let Some(path) = file.path() {
                                let _ = std::fs::write(path, &att.data);
                            }
                        }
                    });
                }
            }
            AppMsg::LightboxRendered(key) => {
                let still_current = self
                    .lightbox_items
                    .get(self.lightbox_pos)
                    .is_some_and(|a| crate::ui::attachments_gallery::content_key(&a.data) == key);
                if still_current {
                    self.lightbox_refresh(&sender);
                }
            }
            AppMsg::ContactPhotosChanged => {
                // The list skips the work when avatars are off; the
                // reader's cards draw initials only, so it has nothing to do.
                self.message_list.emit(MessageListInput::ContactPhotosChanged);
            }

            AppMsg::SetFetchInterval(secs) => {
                if self.fetch_interval_secs != secs {
                    self.fetch_interval_secs = secs;
                    self.save_settings();
                    self.arm_auto_fetch(&sender);
                }
            }

            AppMsg::SetPush(on) => {
                if self.push != on {
                    self.push = on;
                    self.save_settings();
                    // Workers read the push setting at startup; restart to apply.
                    self.reconnect_all(&sender);
                }
            }

            AppMsg::SetNotifications(on) => {
                if self.notifications_enabled != on {
                    self.notifications_enabled = on;
                    self.save_settings();
                }
            }

            AppMsg::SetNotificationContent(on) => {
                if self.notification_content != on {
                    self.notification_content = on;
                    self.save_settings();
                }
            }

            AppMsg::SetNotificationButtons(buttons) => {
                if self.notification_buttons != buttons {
                    self.notification_buttons = buttons;
                    self.save_settings();
                }
            }

            AppMsg::SetRunInBackground(on) => {
                if self.run_in_background.get() != on {
                    self.run_in_background.set(on);
                    self.save_settings();
                    // Ask the portal as the setting changes, so the permission
                    // dialog appears while the user is looking at the switch they
                    // just moved rather than at some later, unexplained moment.
                    crate::background::request(on && self.autostart);
                    if !on {
                        crate::background::set_status("");
                    }
                }
            }

            AppMsg::SetAutostart(on) => {
                if self.autostart != on {
                    self.autostart = on;
                    self.save_settings();
                    crate::background::request(self.run_in_background.get() && on);
                }
            }

            AppMsg::SetTray(on) => {
                if self.tray_enabled != on {
                    self.tray_enabled = on;
                    self.save_settings();
                    if on {
                        self.start_tray(&sender);
                    } else if let Some(tray) = self.tray.take() {
                        tray.stop();
                    }
                }
            }

            AppMsg::SetTrayIcon(icon) => {
                if self.tray_icon != icon {
                    self.tray_icon = icon;
                    self.save_settings();
                    if let Some(tray) = &self.tray {
                        tray.set_icon(icon, crate::app_icon::png_for(&self.app_icon));
                    }
                }
            }

            AppMsg::SetAppIcon(id) => {
                let id = crate::app_icon::set(&id);
                if self.app_icon != id {
                    self.app_icon = id;
                    if let Some(tray) = &self.tray {
                        tray.set_icon(self.tray_icon, crate::app_icon::png_for(&self.app_icon));
                    }
                    self.offer_restart_for_icon(&sender);
                }
            }

            AppMsg::RestartApp => {
                if self.restart_pending {
                    return;
                }
                self.restart_pending = true;
                // GNOME Shell only replaces a launcher's app object some
                // seconds after the launcher changed; quitting before that
                // brings the new window up under the old one, old icon and
                // all. Wait it out with the window still up.
                let wait = crate::app_icon::launcher_settle_remaining();
                self.show_restarting_dialog();
                if wait.is_zero() {
                    sender.input(AppMsg::RestartNow);
                } else {
                    let s = sender.clone();
                    gtk::glib::timeout_add_local_once(wait, move || s.input(AppMsg::RestartNow));
                }
            }

            AppMsg::RestartNow => match crate::app_icon::launch_restart_helper() {
                Ok(()) => relm4::main_application().activate_action("quit", None),
                Err(e) => {
                    tracing::warn!("restart helper failed: {e}");
                    self.restart_pending = false;
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n("Couldn't restart automatically — quit and reopen Hylki \
                               to finish switching the icon."),
                        error: true,
                        connectivity: false,
                    });
                }
            },

            AppMsg::SetTrayMail(on) => {
                if self.tray_mail != on {
                    self.tray_mail = on;
                    self.save_settings();
                    // A sentinel no real list equals, so the change is sent
                    // whichever way the switch went.
                    *self.tray_mail_key.borrow_mut() =
                        Some(vec![(u32::MAX, u32::MAX, String::new(), String::new())]);
                    self.push_tray_mail();
                }
            }

            AppMsg::QuitFromTray => {
                // The same teardown as Ctrl+Q: geometry saved, then exit.
                relm4::main_application().activate_action("quit", None);
            }

            AppMsg::TrayViewUnread => {
                let unified = self.show_unified_pref
                    && (self.config.iter().filter(|c| c.enabled).count() > 1
                        || (demo_mode() && self.accounts.len() > 1));
                if unified {
                    // Through the sidebar, so its highlight moves too; it
                    // answers with UnifiedSelected.
                    self.sidebar.emit(SidebarInput::SelectUnifiedRow);
                } else if let Some((account_id, folder_id, name, path)) = self
                    .accounts
                    .iter()
                    .filter_map(|a| self.inbox_of(a.id))
                    .find(|f| self.folder_unread_of(f) > 0)
                    .map(|f| (f.account_id, f.id, f.name.clone(), f.path.clone()))
                {
                    self.select_folder(account_id, folder_id, name, path);
                }
            }


            AppMsg::SetSingleKey(on) => {
                if self.single_key.get() != on {
                    self.single_key.set(on);
                    self.save_settings();
                }
            }

            AppMsg::ShowShortcuts => self.show_shortcuts(),

            AppMsg::PrintPreview | AppMsg::PrintMessage => {
                // Only the reader can print: it holds the rendered message, and
                // printing from an empty reader would offer a blank page. Say so
                // rather than appearing to do nothing, which is indistinguishable
                // from a broken menu item.
                let preview = matches!(msg, AppMsg::PrintPreview);
                tracing::info!(preview, open = self.current.is_some(), "print requested");
                if self.current.is_none() {
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n("Open a message first, then print it."),
                        error: false,
                        connectivity: false,
                    });
                    return;
                }
                self.message_view.emit(if preview {
                    MessageViewInput::PrintPreview
                } else {
                    MessageViewInput::Print
                });
            }

            AppMsg::Shortcut(action) => self.run_shortcut(action, &sender),

            AppMsg::SetPreviewLines(lines) => {
                if self.preview_lines != lines {
                    let was_off = self.preview_lines == 0;
                    self.preview_lines = lines;
                    self.save_settings();
                    self.push_list_look(false);
                    // Previews switched back on: IMAP summaries fetched while
                    // they were off carry no preview text (the setting also
                    // stops the body slice being downloaded), so nothing would
                    // show until the next scheduled sync. Refresh now so they
                    // fill in right away. (Graph always has bodyPreview in
                    // hand, which is why Microsoft accounts showed instantly.)
                    if was_off && lines > 0 {
                        sender.input(AppMsg::Refresh);
                    }
                }
            }

            AppMsg::SetAttachmentsRow(show) => {
                if self.show_attachments != show {
                    self.show_attachments = show;
                    self.save_settings();
                    self.sidebars_emit(SidebarInput::SetAttachmentsRow(show));
                }
            }

            AppMsg::ListCount(text) => self.list_count = text,

            AppMsg::SetContactsRow(show) => {
                if self.show_contacts != show {
                    self.show_contacts = show;
                    self.save_settings();
                    self.sidebars_emit(SidebarInput::SetContactsRow(show));
                }
            }

            AppMsg::SetShowUnified(show) => {
                if self.show_unified_pref != show {
                    self.show_unified_pref = show;
                    self.save_settings();
                    // Rebuilds the sidebar with or without the unified section.
                    self.rebuild_sidebar();
                }
            }

            AppMsg::SetUnifiedChips(chips) => {
                if self.unified_chips != chips {
                    self.unified_chips = chips;
                    self.save_settings();
                    self.rebuild_sidebar();
                }
            }

            AppMsg::SetUnifiedFiltered(show) => {
                if self.unified_filtered != show {
                    self.unified_filtered = show;
                    self.save_settings();
                    // Adds or removes the Filtered Folders section.
                    self.rebuild_sidebar();
                }
            }

            AppMsg::SetUnifiedKinds(kinds) => {
                if self.unified_kinds != kinds {
                    self.unified_kinds = kinds;
                    self.save_settings();
                    self.rebuild_sidebar();
                }
            }

            AppMsg::SetUnifiedTags(show) => {
                if self.unified_tags != show {
                    self.unified_tags = show;
                    self.save_settings();
                    self.rebuild_sidebar();
                }
            }

            AppMsg::SetShowAccounts(show) => {
                if self.show_accounts != show {
                    self.show_accounts = show;
                    self.save_settings();
                    self.rebuild_sidebar();
                }
                // Both places that offer the switch stay in step.
                if self.show_accounts_action.state().and_then(|v| v.get::<bool>()) != Some(show) {
                    self.show_accounts_action.set_state(&show.to_variant());
                }
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::SetShowAccounts(show));
                }
            }

            AppMsg::ToggleAccountFiltered(account_id) => {
                // The sidebar animated the toggle locally; record the state.
                if let Some(email) = self.email_of(account_id) {
                    if let Some(pos) = self.filtered_expanded_accounts.iter().position(|e| *e == email) {
                        self.filtered_expanded_accounts.remove(pos);
                    } else {
                        self.filtered_expanded_accounts.push(email);
                    }
                    self.save_sidebar_state();
                }
            }

            AppMsg::ToggleAccountTags(account_id) => {
                if let Some(email) = self.email_of(account_id) {
                    if let Some(pos) = self.tags_expanded_accounts.iter().position(|e| *e == email) {
                        self.tags_expanded_accounts.remove(pos);
                    } else {
                        self.tags_expanded_accounts.push(email);
                    }
                    self.save_sidebar_state();
                }
            }

            AppMsg::SetFilteredPlacement(p) => {
                if self.filtered_placement != p {
                    self.filtered_placement = p;
                    self.save_settings();
                    self.rebuild_sidebar();
                }
            }

            AppMsg::SetTagsPlacement(p) => {
                if self.tags_placement != p {
                    self.tags_placement = p;
                    self.save_settings();
                    self.rebuild_sidebar();
                }
            }

            AppMsg::SetChevronsLeft(left) => {
                if self.chevrons_left != left {
                    self.chevrons_left = left;
                    self.save_settings();
                    self.rebuild_sidebar();
                }
            }

            AppMsg::SetThreading(on) => {
                if self.threading != on {
                    self.threading = on;
                    self.save_settings();
                    self.message_list.emit(MessageListInput::SetThreading(on));
                }
            }

            AppMsg::SetThreadExpansion(on) => {
                if self.thread_expansion != on {
                    self.thread_expansion = on;
                    self.save_settings();
                    self.message_list.emit(MessageListInput::SetThreadExpansion(on));
                }
            }

            AppMsg::SetConfirmThreadDelete(on) => {
                if self.confirm_thread_delete != on {
                    self.confirm_thread_delete = on;
                    self.save_settings();
                }
            }

            AppMsg::DeleteThread(messages) => {
                if messages.is_empty() {
                    return;
                }
                if self.confirm_thread_delete {
                    self.confirm_delete_thread(messages, &sender);
                } else {
                    self.delete_messages(messages, &sender);
                }
            }

            AppMsg::DeleteThreadConfirmed(messages) => {
                self.delete_messages(messages, &sender);
            }

            AppMsg::SetCardActionsMode { hover_toggle, hover_auto } => {
                if self.card_actions_hover != hover_toggle
                    || self.card_actions_auto != hover_auto
                {
                    self.card_actions_hover = hover_toggle;
                    self.card_actions_auto = hover_auto;
                    self.save_settings();
                    self.push_card_actions_mode();
                }
            }

            AppMsg::SetThreadsExpanded(on) => {
                if self.threads_expanded != on {
                    self.threads_expanded = on;
                    self.save_settings();
                    self.message_list.emit(MessageListInput::SetThreadsExpanded(on));
                }
            }

            AppMsg::SetThreadNewestFirst(on) => {
                if self.thread_newest_first != on {
                    self.thread_newest_first = on;
                    self.save_settings();
                    // Re-render an open conversation in the new order.
                    if self.current_thread.len() > 1 {
                        self.show_thread();
                    }
                }
            }

            AppMsg::SetAlwaysShowRecipients(on) => {
                if self.always_show_recipients != on {
                    self.always_show_recipients = on;
                    self.save_settings();
                    self.message_view.emit(MessageViewInput::SetAlwaysShowRecipients(on));
                    // Re-render whatever is open so the header reflects it.
                    if self.current_thread.len() > 1 {
                        self.show_thread();
                    } else if let Some(current) = self.current.clone() {
                        let m = self.with_cached_body(current);
                        self.show_message(Some(m), false);
                    }
                }
            }

            AppMsg::SetCardAttachments(on) => {
                if self.card_attachments != on {
                    self.card_attachments = on;
                    self.save_settings();
                    self.message_view.emit(MessageViewInput::SetCardAttachmentsShown(on));
                }
            }
            AppMsg::SetAttachmentDrawer(on) => {
                if self.drawer_enabled != on {
                    self.drawer_enabled = on;
                    self.save_settings();
                    self.sync_attachment_drawer();
                    self.message_view.emit(MessageViewInput::SetAttachmentDrawer(on));
                }
            }
            AppMsg::CardAttachment { account_id, id, index, save } => {
                let Some(items) = self.attachment_cache.get(&(account_id, id)).cloned() else {
                    return;
                };
                let Some(att) = items.get(index).cloned() else { return };
                if save {
                    save_attachment(att, Some(self.window.clone()));
                } else if crate::ui::attachment_drawer::previewable(&att) {
                    // The lightbox pages through this message's previewable
                    // files, starting at the one clicked.
                    let previewable: Vec<Attachment> = items
                        .iter()
                        .filter(|a| crate::ui::attachment_drawer::previewable(a))
                        .cloned()
                        .collect();
                    let start = items
                        .iter()
                        .take(index + 1)
                        .filter(|a| crate::ui::attachment_drawer::previewable(a))
                        .count()
                        .saturating_sub(1);
                    sender.input(AppMsg::ShowLightbox { items: previewable, start });
                } else {
                    crate::ui::attachments_gallery::open_bytes(
                        &att.name,
                        &att.data,
                        Some(self.window.upcast_ref::<gtk::Window>()),
                    );
                }
            }
            AppMsg::ShowAttachmentInMessage(att) => {
                // The drawer shows identical (name, size) pairs once, so the
                // first message carrying this one is the one to find.
                let owner = self
                    .conversation_members()
                    .into_iter()
                    .find(|key| {
                        self.attachment_cache.get(key).is_some_and(|items| {
                            items.iter().any(|a| a.name == att.name && a.data.len() == att.data.len())
                        })
                    });
                if let Some((account_id, id)) = owner {
                    self.message_view
                        .emit(MessageViewInput::ScrollToAttachments { account_id, id });
                }
            }
            AppMsg::ZoomMessage(step) => {
                use config::READER_ZOOM_STEPS as STEPS;
                let at = STEPS.iter().position(|&z| z == self.zoom).unwrap_or(5) as i32;
                let zoom = match step {
                    0 => self.zoom_default,
                    s => STEPS[(at + i32::from(s.signum())).clamp(0, STEPS.len() as i32 - 1) as usize],
                };
                self.set_zoom(zoom);
            }
            AppMsg::SetZoomDefault(zoom) => {
                if self.zoom_default != zoom {
                    self.zoom_default = zoom;
                    self.save_settings();
                    self.message_view.emit(MessageViewInput::SetZoomDefault(zoom));
                    for p in self.popouts.values() {
                        p.controller.emit(MessageWindowInput::SetZoomDefault(zoom));
                    }
                }
                // The setting is also what the user wants to see now.
                self.set_zoom(zoom);
            }
            AppMsg::SetReaderMode(on) => {
                // In Focus Mode with Reader View on, the switch changes the
                // message on screen alone: the saved choice is the ordinary
                // one, and it comes back with the rest of the layout.
                if self.focus.active(config::FocusPart::ReaderView) {
                    // Nothing to remember.
                } else if self.reader_mode != on {
                    self.reader_mode = on;
                    self.save_settings();
                }
                // Always pushed, equal or not: with a per-message default the
                // readers reset themselves on each open, so what they show
                // can differ from the saved state — and a switch flipped in
                // one of them must land there regardless.
                self.message_view.emit(MessageViewInput::SetReaderMode(on));
                for p in self.popouts.values() {
                    p.controller.emit(MessageWindowInput::SetReaderMode(on));
                }
            }
            AppMsg::SetReaderSwitchShown(on) => {
                if self.reader_switch != on {
                    self.reader_switch = on;
                    self.save_settings();
                    self.message_view.emit(MessageViewInput::SetReaderSwitchShown(on));
                    for p in self.popouts.values() {
                        p.controller.emit(MessageWindowInput::SetReaderSwitchShown(on));
                    }
                }
            }
            AppMsg::SetReaderDefault(policy) => {
                if self.reader_default != policy {
                    self.reader_default = policy;
                    self.save_settings();
                    // Focus Mode's Reader View outranks it while on.
                    let policy = self.effective_reader_default();
                    self.message_view.emit(MessageViewInput::SetReaderDefault(policy));
                    for p in self.popouts.values() {
                        p.controller.emit(MessageWindowInput::SetReaderDefault(policy));
                    }
                }
            }
            AppMsg::SetSingleMessageCard(on) => {
                if self.single_message_card != on {
                    self.single_message_card = on;
                    self.save_settings();
                    self.message_view.emit(MessageViewInput::SetSingleMessageCard(on));
                    // Only lone messages change; re-render one if it's open.
                    if self.current_thread.len() <= 1 {
                        if let Some(current) = self.current.clone() {
                            let m = self.with_cached_body(current);
                            self.show_message(Some(m), false);
                        }
                    }
                }
            }

            AppMsg::SetPaletteCollapse(secs) => {
                if self.palette_collapse_secs != secs {
                    self.palette_collapse_secs = secs;
                    self.save_settings();
                    self.message_list.emit(MessageListInput::SetPaletteCollapse(secs));
                }
            }

            AppMsg::SetCardPaletteCollapse(secs) => {
                if self.card_palette_collapse_secs != secs {
                    self.card_palette_collapse_secs = secs;
                    self.save_settings();
                    self.message_view.emit(MessageViewInput::SetPaletteCollapse(secs));
                }
            }

            AppMsg::SetListPalette(on) => {
                if self.list_palette != on {
                    self.list_palette = on;
                    self.save_settings();
                    self.message_list.emit(MessageListInput::SetListPalette(on));
                }
            }

            AppMsg::SetListPaletteHover(on) => {
                if self.list_palette_hover != on {
                    self.list_palette_hover = on;
                    self.save_settings();
                    self.message_list.emit(MessageListInput::SetPaletteHover(on));
                }
            }

            AppMsg::SetCardPaletteMenu(on) => {
                if self.card_palette_menu != on {
                    self.card_palette_menu = on;
                    self.save_settings();
                    self.message_view.emit(MessageViewInput::SetCardPaletteMenu(on));
                }
            }

            AppMsg::SetSwipeEnabled(on) => {
                if self.swipe_enabled != on {
                    self.swipe_enabled = on;
                    self.save_settings();
                    self.message_list.emit(MessageListInput::SetSwipeEnabled(on));
                }
            }

            AppMsg::SetSwipeReversed(on) => {
                if self.swipe_reversed != on {
                    self.swipe_reversed = on;
                    self.save_settings();
                    self.message_list.emit(MessageListInput::SetSwipeReversed(on));
                }
            }

            AppMsg::SetSwipeSensitivity(factor) => {
                let factor = factor.clamp(
                    config::SWIPE_SENSITIVITY_MIN,
                    config::SWIPE_SENSITIVITY_MAX,
                );
                if self.swipe_sensitivity != factor {
                    self.swipe_sensitivity = factor;
                    self.save_settings();
                    self.message_list
                        .emit(MessageListInput::SetSwipeSensitivity(factor));
                }
            }

            AppMsg::SetRemoteContent { account_id, id, show } => {
                self.remote_override.insert((account_id, id), show);
                // Re-show whatever is open so the decision takes effect now.
                // The reader re-reads `remote_allowed` for every message it
                // paints, so a conversation gets it for the card it applies to.
                if self.current_thread.len() > 1 {
                    self.show_thread();
                } else {
                    let current = self.current.clone();
                    self.show_message(current, false);
                }
            }

            AppMsg::Undo => self.undo_redo(false),

            AppMsg::Redo => self.undo_redo(true),

            AppMsg::ComposeHistory { id, undo, redo } => {
                // Only the inline composer's: a composer in a window of its
                // own answers its keys there and has no menu here to label.
                if self.reader_compose.as_ref().is_some_and(|r| r.id == id && r.window.is_none()) {
                    self.compose_history = (undo, redo);
                    self.refresh_undo_menu();
                }
            }


            AppMsg::SetComposeInline(on) => {
                if self.compose_inline != on {
                    self.compose_inline = on;
                    self.save_settings();
                }
            }

            AppMsg::SetReplyFields(on) => {
                if self.reply_fields != on {
                    self.reply_fields = on;
                    self.save_settings();
                }
            }

            AppMsg::SetFilesPrefs(prefs) => {
                if self.files_prefs != prefs {
                    self.files_prefs = prefs;
                    self.save_settings();
                }
            }

            AppMsg::EditAsNewCurrent => {
                if let Some(m) = self.compose_target() {
                    self.edit_as_new(m, &sender);
                }
            }

            AppMsg::SetLinkBrowser(choice) => {
                if self.link_browser != choice {
                    self.link_browser = choice;
                    crate::ui::launch::set_browser(&self.link_browser);
                    self.save_settings();
                }
            }

            AppMsg::HandOffAction { hand_off, action, remember } => {
                if remember && self.files_prefs.action != action {
                    self.files_prefs.action = action;
                    self.save_settings();
                    self.push_files_prefs();
                }
                self.hand_off_action(hand_off, action, &sender);
            }

            AppMsg::HandOffDraftPickNow => {
                if let Some((hand_off, _)) = self.pending_draft_pick.take() {
                    self.show_draft_picker(hand_off, &sender);
                }
            }

            AppMsg::HandOffDraft { hand_off, draft } => {
                let target = match draft {
                    Some(m) => HandOffTarget::Draft(m),
                    None => HandOffTarget::New,
                };
                self.hand_off_size_check(hand_off, target, &sender);
            }

            AppMsg::HandOffReply { hand_off, message } => {
                let target = match message {
                    Some(m) => HandOffTarget::Reply(m),
                    None => HandOffTarget::New,
                };
                self.hand_off_size_check(hand_off, target, &sender);
            }

            AppMsg::HandOffOpen { hand_off, target, cloud, remember } => {
                if remember {
                    let large = if cloud { config::FilesLarge::Cloud } else { config::FilesLarge::Attach };
                    if self.files_prefs.large != large {
                        self.files_prefs.large = large;
                        self.save_settings();
                        self.push_files_prefs();
                    }
                }
                self.hand_off_open(hand_off, target, cloud, &sender);
            }

            AppMsg::SetComposeDefaultFrom(addr) => {
                if self.compose_default_from != addr {
                    self.compose_default_from = addr;
                    self.save_settings();
                }
            }

            AppMsg::SetPastePlain(on) => {
                if self.paste_plain != on {
                    self.paste_plain = on;
                    self.save_settings();
                }
            }

            AppMsg::SetSpellcheck(on) => {
                if self.spellcheck != on {
                    self.spellcheck = on;
                    self.save_settings();
                    // Takes effect in already-open composers too: the shared
                    // web context is live.
                    crate::ui::rich_editor::apply_spellcheck();
                }
            }

            AppMsg::SetSpellcheckLangs(langs) => {
                if self.spellcheck_langs != langs {
                    self.spellcheck_langs = langs;
                    self.save_settings();
                    crate::ui::rich_editor::apply_spellcheck();
                }
            }

            AppMsg::SetMessageTheme(theme) => {
                if self.message_theme != theme {
                    self.message_theme = theme;
                    self.save_settings();
                    let dark = theme.dark_override();
                    // Message content only — the reader and any popped-out windows.
                    self.message_view.emit(MessageViewInput::SetContentTheme(dark));
                    for p in self.popouts.values() {
                        p.controller.emit(MessageWindowInput::SetContentTheme(dark));
                    }
                }
            }

            AppMsg::ShowSettingsPage(id) => {
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::ShowPageById(id));
                }
            }
            AppMsg::ReaderToolbarMenu { x, y } => {
                use crate::ui::context_menu::{show_context_menu, MenuEntry};
                if let Some(header) = self.reader_header.get() {
                    let s = sender.input_sender().clone();
                    let entry = MenuEntry::new(i18n("Customize Toolbar…"), move || {
                        let _ = s.send(AppMsg::CustomizeToolbar);
                    })
                    .icon("co.hyprlab.Hylki-preferences-desktop-appearance-symbolic");
                    show_context_menu(header, x, y, vec![vec![entry]]);
                }
            }
            AppMsg::CustomizeToolbar => {
                self.open_settings_window(&sender, false, false);
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::ShowPageById("appearance".to_string()));
                }
            }
            AppMsg::ShowcaseToolbarGap { zone, index } => {
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::ToolbarGapPreview { zone, index });
                }
            }
            AppMsg::ReloadBody(m) => {
                if let Some(path) = self.resolve_folder_path(&m) {
                    self.send_to(m.account_id, MailRequest::LoadBody { message_id: m.id, path, uid: m.uid });
                }
            }
            AppMsg::SetOverrideFonts(on) => {
                if self.override_fonts != on {
                    self.override_fonts = on;
                    self.save_settings();
                    self.push_reader_style();
                }
            }
            AppMsg::SetReaderFont(font) => {
                if self.reader_font != font {
                    self.reader_font = font;
                    self.save_settings();
                    self.push_reader_style();
                }
            }
            AppMsg::SetPlainMonospace(on) => {
                if self.plain_monospace != on {
                    self.plain_monospace = on;
                    self.save_settings();
                    self.push_reader_style();
                }
            }
            AppMsg::SetPlainFont(font) => {
                if self.plain_font != font {
                    self.plain_font = font;
                    self.save_settings();
                    self.push_reader_style();
                }
            }
            AppMsg::ShowcaseComposeMenu { format } => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(if format {
                        ComposeInput::FormatMenu
                    } else {
                        ComposeInput::OverflowMenu
                    });
                }
            }
            AppMsg::ShowcaseComposePreview => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(ComposeInput::TogglePreview(true));
                }
            }
            AppMsg::ShowcaseComposeFrom(n) => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(ComposeInput::ShowcaseFrom(n));
                }
            }
            AppMsg::ShowcaseForward { account_id, id } => {
                let found = self
                    .current_thread
                    .iter()
                    .find(|m| m.account_id == account_id && m.id == id)
                    .cloned()
                    .or_else(|| {
                        self.message_cache
                            .values()
                            .flatten()
                            .find(|m| m.account_id == account_id && m.id == id)
                            .cloned()
                    });
                match found {
                    Some(m) => self.forward(m, true, &sender),
                    None => tracing::warn!("showcase forward: {account_id}:{id} is not loaded"),
                }
            }
            AppMsg::ShowcaseComposeUndo => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(ComposeInput::ShowcaseHistory(0));
                }
            }
            AppMsg::ShowcaseBurger => {
                if let Some(m) = self.sidebar_menu.as_ref() {
                    // Focus first: a click on a MenuButton focuses it before
                    // the popover opens, and popup() alone skips that — the
                    // difference between the two is a bug's hiding place.
                    m.grab_focus();
                    m.popup();
                    let mut chain = Vec::new();
                    let mut node = gtk::prelude::GtkWindowExt::focus(&self.window);
                    while let Some(w) = node {
                        chain.push(format!("{}", w.type_()));
                        node = w.parent();
                    }
                    tracing::info!(
                        target: "hylki::compose::undo",
                        "burger: opened, focus now [{}]",
                        chain.join(" < ")
                    );
                }
            }
            AppMsg::ShowcaseComposeFormat(format) => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(ComposeInput::SetFormat(format));
                }
            }
            AppMsg::SetComposeFormat(format) => {
                if self.compose_format != format {
                    self.compose_format = format;
                    self.save_settings();
                }
            }
            AppMsg::SetReplyPosition(position) => {
                // A split reply already open stays where it is; the next
                // one opens in the new place.
                if self.reply_position != position {
                    self.reply_position = position;
                    self.save_settings();
                }
            }
            AppMsg::SetSignaturePosition(position) => {
                // Composers already open keep their signature where it is;
                // the next reply or forward opens with the new placement.
                if self.signature_position != position {
                    self.signature_position = position;
                    self.save_settings();
                }
            }
            AppMsg::SetOverrideColors(on) => {
                if self.override_colors != on {
                    self.override_colors = on;
                    self.save_settings();
                    self.push_reader_style();
                }
            }

            AppMsg::ComposeTo(addr) => {
                // The contacts view hosts its own compose slot (the composer
                // slides down over the contact card); from the gallery there is
                // no slot, so bring the mail panes back first — the composer
                // would otherwise open invisibly behind the stack.
                self.leave_gallery();
                let account = self
                    .current
                    .as_ref()
                    .map(|m| m.account_id)
                    .unwrap_or_else(|| self.active_account());
                let prefill = ComposePrefill {
                    to: addr,
                    ..Default::default()
                };
                let (account, prefill) = self.new_message_from(account, prefill);
                // Same preference as "New Message": slide down over the
                // reader, unless composing is set to open in a window.
                if self.compose_inline {
                    self.open_inline_reply(account, prefill, None, &sender);
                } else {
                    self.open_compose(account, prefill, &sender);
                }
            }

            AppMsg::OpenWithFiles(mut paths) => {
                // Same open-composer flow as OpenMailto, with the handed-in
                // files pre-attached. The relay normalizes every stray
                // command-line argument into a file URI, so keep only the
                // ones that name a real file.
                let named = paths.len();
                paths.retain(|p| {
                    let ok = p.is_file();
                    if !ok {
                        tracing::warn!("file hand-off ignored (not a readable file): {}", p.display());
                    }
                    ok
                });
                tracing::info!("file hand-off: {named} named, {} readable", paths.len());
                if paths.is_empty() {
                    return;
                }
                self.begin_hand_off(
                    FileHandOff { base: ComposePrefill::default(), files: paths, dropped: Vec::new() },
                    &sender,
                );
            }

            AppMsg::CopyReaderSelection => {
                self.message_view.emit(MessageViewInput::CopySelection);
            }

            AppMsg::OpenMid(uri) => {
                // The cache first: it holds every message Hylki has listed,
                // across accounts, and answers at once. Only a miss asks the
                // servers, one HEADER search per folder per account.
                let Some(id) = parse_mid(&uri) else {
                    tracing::warn!("mid: link not understood: {uri}");
                    return;
                };
                let hits: Vec<(u32, String, u32)> = crate::cache::Cache::open()
                    .map(|c| c.locate_by_message_id(&id))
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(acc, path, _)| {
                        self.folders.get(acc).is_some_and(|fs| fs.iter().any(|f| f.path == *path))
                    })
                    .collect();
                // The copy in the account currently in view wins when several
                // accounts hold the message (a mail sent to two of them).
                let preferred = self.current.as_ref().map(|m| m.account_id);
                if let Some((account_id, folder_path, uid)) = hits
                    .iter()
                    .find(|(acc, _, _)| Some(*acc) == preferred)
                    .or_else(|| hits.first())
                    .cloned()
                {
                    tracing::info!("mid: {id} is cached in account {account_id}, {folder_path} uid {uid}");
                    sender.input(AppMsg::OpenAttachmentMessage { account_id, folder_path, uid });
                    return;
                }
                if self.workers.is_empty() {
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n_f("No message with the ID {id} was found", &[("id", &id)]),
                        error: true,
                        connectivity: false,
                    });
                    return;
                }
                self.mid_searches.insert(id.clone(), self.workers.len());
                for w in self.workers.values() {
                    let _ = w.send(MailRequest::Locate { message_id: id.clone() });
                }
            }

            AppMsg::MidLocated { account_id, message_id, hit } => {
                let Some(remaining) = self.mid_searches.get_mut(&message_id) else { return };
                if let Some((folder_path, uid)) = hit {
                    tracing::info!("mid: {message_id} found on account {account_id}, {folder_path} uid {uid}");
                    self.mid_searches.remove(&message_id);
                    sender.input(AppMsg::OpenAttachmentMessage { account_id, folder_path, uid });
                    return;
                }
                *remaining = remaining.saturating_sub(1);
                if *remaining == 0 {
                    tracing::info!("mid: {message_id} not found on any account");
                    self.mid_searches.remove(&message_id);
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n_f("No message with the ID {id} was found", &[("id", &message_id)]),
                        error: true,
                        connectivity: false,
                    });
                }
            }

            AppMsg::FindTags => {
                if self.tag_scan.is_some() {
                    return;
                }
                if self.workers.is_empty() {
                    if let Some(a) = &self.accounts_win {
                        a.emit(crate::ui::accounts::AccountsInput::TagFindings(Vec::new()));
                    }
                    return;
                }
                self.tag_scan_gen = self.tag_scan_gen.wrapping_add(1);
                let gen = self.tag_scan_gen;
                self.tag_scan = Some(TagScan { gen, remaining: self.workers.len(), found: Vec::new() });
                for w in self.workers.values() {
                    let _ = w.send(MailRequest::FindKeywords);
                }
                if let Some(a) = &self.accounts_win {
                    a.emit(crate::ui::accounts::AccountsInput::TagScanning(true));
                }
                // An account that is offline never answers: report what the
                // others found rather than searching forever.
                let s = sender.clone();
                gtk::glib::timeout_add_seconds_local_once(120, move || {
                    s.input(AppMsg::TagScanTimeout(gen));
                });
            }

            AppMsg::KeywordsFound { account_id, findings } => {
                let Some(scan) = self.tag_scan.as_mut() else { return };
                scan.found.push((account_id, findings));
                scan.remaining = scan.remaining.saturating_sub(1);
                if scan.remaining == 0 {
                    self.finish_tag_scan(&sender);
                }
            }

            AppMsg::TagScanTimeout(gen) => {
                if self.tag_scan.as_ref().is_some_and(|s| s.gen == gen) {
                    tracing::warn!("find tags: not every account answered in time");
                    self.finish_tag_scan(&sender);
                }
            }

            AppMsg::OpenMailto(uri) => {
                // A mailto: link from anywhere on the system (the desktop file
                // registers the scheme). Same open-composer flow as ComposeTo,
                // with the URI's subject/cc/bcc/body carried along.
                let Some(mut prefill) = parse_mailto(&uri) else { return };
                // attach= names files (#90): keep only the ones that really
                // exist as regular files. Whatever attaches shows up as a
                // normal removable chip in the composer, so nothing rides
                // along invisibly if a web link (rather than Nautilus)
                // carried the parameter. The ones that don't are reported,
                // not dropped in silence: from inside the Flatpak sandbox a
                // path the file manager can see may be one Hylki cannot.
                let (attachments, dropped): (Vec<_>, Vec<_>) =
                    std::mem::take(&mut prefill.attachments).into_iter().partition(|p| p.is_file());
                for p in &dropped {
                    tracing::warn!("mailto attach ignored (not a readable file): {}", p.display());
                }
                tracing::info!(
                    "mailto hand-off: {} attachment(s) named, {} readable",
                    attachments.len() + dropped.len(),
                    attachments.len()
                );
                if !attachments.is_empty() || !dropped.is_empty() {
                    // Files came along (Files' own "Email…" entry): the same
                    // choice of destination as "Send with Hylki".
                    self.begin_hand_off(FileHandOff { base: prefill, files: attachments, dropped }, &sender);
                    return;
                }
                self.leave_gallery();
                let account = self
                    .current
                    .as_ref()
                    .map(|m| m.account_id)
                    .unwrap_or_else(|| self.active_account());
                let (account, prefill) = self.new_message_from(account, prefill);
                if self.compose_inline {
                    self.open_inline_reply(account, prefill, None, &sender);
                } else {
                    self.open_compose(account, prefill, &sender);
                }
                self.after_hand_off(0, Vec::new());
            }

            AppMsg::PresentComposers => {
                crate::desktop::present_window(&self.window);
                // The newest standalone composer is the one the alert was
                // about; transient for the main window, it sits above it.
                if let Some(h) = self.composers.last() {
                    crate::desktop::present_window(&h.window);
                }
            }

            AppMsg::SendMessage(out) => {
                let account_id = out.from_account_id;
                // Remember who this went to, now, whatever route it takes
                // from here (straight out, the Outbox, Send Later): the next
                // composer suggests them, and so do the ones already open.
                let recipients: Vec<(String, String)> = [&out.to, &out.cc, &out.bcc]
                    .into_iter()
                    .flat_map(|f| crate::worker::parse_recipients(f))
                    .collect();
                if let Some(c) = self.cache.as_ref() {
                    c.record_addresses(&recipients);
                }
                let own: HashSet<String> = self.config.iter().map(|c| c.email.to_lowercase()).collect();
                let fresh: Vec<crate::contacts::Suggestion> = recipients
                    .into_iter()
                    .filter(|(_, e)| e.contains('@'))
                    .map(|(name, email)| crate::contacts::Suggestion {
                        name: if name.is_empty() { email.clone() } else { name },
                        own: own.contains(&email.to_lowercase()),
                        email,
                        from_contacts: false,
                        score: 1,
                    })
                    .collect();
                if !fresh.is_empty() {
                    for host in &self.composers {
                        host.controller.emit(ComposeInput::AddSuggestions(fresh.clone()));
                    }
                    if let Some(rc) = &self.reader_compose {
                        rc.controller.emit(ComposeInput::AddSuggestions(fresh.clone()));
                    }
                }
                let sent_path = self.sent_copy_path(account_id);
                self.send_to(account_id, MailRequest::Send { message: out, sent_path });
            }

            AppMsg::SaveDraftMessage(out) => {
                let account_id = out.from_account_id;
                // Existing Drafts folder, else a default path (worker creates it).
                let drafts = self
                    .folders
                    .get(&account_id)
                    .and_then(|fs| fs.iter().find(|f| f.kind == FolderKind::Drafts))
                    .map(|f| (f.id, f.path.clone()))
                    .or_else(|| {
                        self.default_folder_path(account_id, FolderKind::Drafts)
                            .map(|p| (0, p))
                    });
                let Some((folder_id, path)) = drafts else {
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n("No Drafts folder available for this account"),
                        error: true,
                        connectivity: false,
                    });
                    return;
                };
                self.send_to(account_id, MailRequest::SaveDraft { message: out, folder_id, path });
            }

            AppMsg::DeleteDraft { id, origin } => {
                // The same move the list's trash button makes for a draft, so
                // it is undoable and the Drafts list updates in place. A draft
                // the list no longer holds (a stale window) is erased by uid.
                let cached = self
                    .message_cache
                    .get(&(origin.account_id, origin.folder_id))
                    .and_then(|msgs| msgs.iter().find(|m| m.uid == origin.uid).cloned());
                match cached {
                    Some(m) => self.move_to(m, FolderKind::Trash),
                    None => {
                        self.send_to(
                            origin.account_id,
                            MailRequest::PurgeMessages {
                                path: origin.path.clone(),
                                uids: vec![origin.uid],
                            },
                        );
                        self.send_to(
                            origin.account_id,
                            MailRequest::LoadMessages {
                                folder_id: origin.folder_id,
                                path: origin.path,
                            },
                        );
                    }
                }
                self.pending_draft = None;
                self.close_compose(id);
                self.message_list.emit(MessageListInput::ReclaimFocus);
            }

            AppMsg::DraftSaved => {
                // The Drafts folder reload already reflects the saved draft; the
                // compose window has closed. No notification (mirrors silent send).
            }

            AppMsg::ShowcaseComposeClose => {
                if let Some(r) = &self.reader_compose {
                    r.controller.emit(ComposeInput::Cancel);
                }
            }
            AppMsg::ComposeClosed(id) => {
                self.close_compose(id);
                self.message_list.emit(MessageListInput::ReclaimFocus);
            }

            AppMsg::ComposeToggleWindow(id) => self.toggle_compose_window(id, &sender),

            AppMsg::Sent { account_id } => {
                // No success notification — only send failures are surfaced (via
                // WorkerEvent::Error). Just refresh the folder the copy landed
                // in, if that folder is the open one — which is wherever the
                // account files its copies (#199), not always Sent.
                if let Some(sel) = self.selected.clone() {
                    let copied_to = self.sent_copy_path(account_id);
                    let viewing_sent = sel.account_id == account_id
                        && copied_to.is_some_and(|path| path == sel.path);
                    if viewing_sent {
                        self.send_to(account_id, MailRequest::LoadMessages {
                            folder_id: sel.folder_id,
                            path: sel.path,
                        });
                    }
                }
                // The reply just sent answers the conversation on screen, and
                // its copy is in the cache by now (the worker lists the copy's
                // folder before it says Sent): ask for the rest of the thread
                // again so the reply joins it here, not after the next visit
                // to the Sent folder (#199).
                self.reload_related(account_id);
            }

            AppMsg::OpenAccounts => self.open_settings_window(&sender, true, false),

            AppMsg::AddFirstAccount => self.open_settings_window(&sender, true, true),

            AppMsg::AccountSaved { original_email, account } => {
                // Demo mode: the edit lands on the in-memory stand-in (so a
                // new colour, emoji or picture shows in the sidebar and the
                // reader at once) and nothing is written or reconnected.
                if self.config.is_empty() && demo_mode() {
                    let slot = original_email
                        .as_ref()
                        .and_then(|orig| self.demo_config.iter_mut().find(|c| &c.email == orig));
                    match slot {
                        Some(slot) => *slot = *account,
                        None => self.demo_config.push(*account),
                    }
                    if let Err(e) = config::save_demo_accounts(&self.demo_config) {
                        tracing::warn!("could not save the demo accounts: {e}");
                    }
                    self.rebuild_sidebar();
                    self.warm_own_gravatars(&sender);
                    self.refresh_faces();
                    return;
                }
                let new_email = account.email.clone();
                // Remember the secret we expect to persist, so we can verify the
                // keyring actually stored it (a silent keyring failure would
                // otherwise leave the account unable to log in after a restart).
                let expected_secret = (!account.password.is_empty())
                    .then(|| account.password.clone());
                // An alias dropped (or switched back to the account's SMTP) in
                // this edit must not leave its SMTP password behind in the
                // keyring — and an email rename re-keys every alias entry, so
                // the old ones all go. config::save() below stores the current
                // set fresh.
                if let Some(orig) = &original_email {
                    if let Some(old) = self.config.iter().find(|c| &c.email == orig) {
                        for old_alias in &old.aliases {
                            let kept = old.email == new_email
                                && account.aliases.iter().any(|n| {
                                    n.has_own_smtp()
                                        && n.address().eq_ignore_ascii_case(&old_alias.address())
                                });
                            if !kept {
                                config::delete_alias_smtp_password(
                                    &old.email,
                                    &old_alias.address(),
                                );
                            }
                        }
                    }
                }
                match original_email {
                    // Editing an existing account (matched by its previous email).
                    Some(orig) => {
                        if let Some(slot) = self.config.iter_mut().find(|c| c.email == orig) {
                            *slot = *account;
                        } else {
                            self.config.push(*account);
                        }
                        // Track an email change in the display-order/collapsed lists.
                        if orig != new_email {
                            for e in self.account_order.iter_mut().chain(self.collapsed.iter_mut()) {
                                if *e == orig {
                                    *e = new_email.clone();
                                }
                            }
                        }
                    }
                    // Adding a new account.
                    None => self.config.push(*account),
                }
                match config::save(&self.config) {
                    Ok(()) => {
                        // config::save() only logs keyring errors, so confirm the
                        // password can actually be read back. If not, the Secret
                        // Service isn't persisting it — tell the user how to fix it
                        // instead of silently "saving" an account that won't stay
                        // logged in.
                        if let Some(secret) = expected_secret {
                            if config::load_password(&new_email).as_deref() != Some(secret.as_str()) {
                                sender.input(AppMsg::ShowKeyringHelp { problem: true });
                            }
                        }
                        self.save_sidebar_state();
                        // Changed Special Folders assignments (#82) land on the
                        // live lists at once; the reconnect's fresh LIST then
                        // confirms them (and restores auto-detection for any
                        // role set back to Automatic).
                        for (i, cfg) in self.config.iter().enumerate() {
                            if let Some(folders) = self.folders.get_mut(&(i as u32 + 1)) {
                                apply_folder_roles(&cfg.folder_roles, folders);
                            }
                        }
                        self.rebuild_sidebar();
                        // A picture, an emoji or the Gravatar switch may have
                        // changed with this edit (#189).
                        self.warm_own_gravatars(&sender);
                        self.refresh_faces();
                        self.reconnect_all(&sender);
                    }
                    Err(e) => self.notifications.emit(NotifyInput::Push {
                        text: i18n_f("Could not save account: {e}", &[("e", &(e).to_string())]),
                        error: true,
                        connectivity: false,
                    }),
                }
            }

            AppMsg::ShowKeyringHelp { problem } => self.show_keyring_help(problem),
            AppMsg::ShowCarryOverNotice(from) => crate::ui::carry_over::show(&self.window, from),

            AppMsg::AccountEnabledChanged { email, enabled } => {
                if let Some(slot) = self.config.iter_mut().find(|c| c.email == email) {
                    if slot.enabled != enabled {
                        slot.enabled = enabled;
                        if let Err(e) = config::save(&self.config) {
                            self.notifications.emit(NotifyInput::Push {
                                text: i18n_f("Could not save account: {e}", &[("e", &(e).to_string())]),
                                error: true,
                                connectivity: false,
                            });
                        }
                        // Respawn workers so the account starts/stops syncing.
                        self.reconnect_all(&sender);
                    }
                }
            }

            AppMsg::PresentWindow => {
                crate::desktop::present_window(&self.window);
            }

            AppMsg::OpenConsole => {
                if self.console_mode {
                    self.notifications.emit(NotifyInput::ShowConsole);
                } else {
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n("Enable Console mode in Settings → System & Appearance"),
                        error: false,
                        connectivity: false,
                    });
                }
            }

            AppMsg::SetFilters(rules) => {
                config::save_filters(&rules);
                let listed_before = self.unified_folder_keys();
                self.filters = rules;
                // A rule opting its folder in or out of All Inboxes changes
                // the sidebar's Filtered Folders section; nothing else
                // about the rules is drawn there.
                if self.unified_folder_keys() != listed_before {
                    self.rebuild_sidebar();
                }
                // File whatever already sits in the inboxes under the new
                // rules; reuses the blacklist's re-sync sweep.
                self.sweep_blacklisted();
                // The counted folders may have changed with the rules (#116):
                // the total follows at once, and a newly counted folder's
                // list is fetched for the tray menu.
                self.push_unread_counts();
                self.sync_unloaded_counted_folders();
            }

            AppMsg::ApplyFilters(targets) => {
                self.start_filter_run(targets, &sender);
            }

            AppMsg::FilterRunTimeout => {
                self.report_filter_run(true);
            }

            AppMsg::FilterRunToBackground => {
                self.filter_run_dismissed();
            }

            AppMsg::FolderSynced { account_id, folder_id } => {
                self.filter_run_folder_synced(account_id, folder_id);
            }

            AppMsg::SetTags(tags) => {
                config::save_tags(&tags);
                self.tags = tags;
                // The unified Tags view spans every tag: what it holds is
                // stale once the set changes.
                self.tag_view_cache.retain(|(_, kw), _| kw.is_some());
                self.refresh_tag_css();
                self.message_list.emit(MessageListInput::SetTags(self.tags.clone()));
                self.message_view.emit(MessageViewInput::SetTags(self.tags.clone()));
                for p in self.popouts.values() {
                    p.controller.emit(MessageWindowInput::SetTags(self.tags.clone()));
                }
                // The sidebar lists the tags; a removed one that was the open
                // view falls back to All Inboxes there (SetContents).
                self.rebuild_sidebar();
                // Rules may name a tag that now exists: tag what already sits
                // in the inboxes, through the same sweep the filters use.
                self.sweep_blacklisted();
            }

            AppMsg::TagSelected { keyword, account } => {
                self.close_sidebar_peek();
                self.mirror_selection(match keyword.clone() {
                    Some(k) => crate::ui::sidebar::Sel::Tag(account, k),
                    None => crate::ui::sidebar::Sel::UnifiedTags,
                });
                self.leave_gallery();
                self.showing_contacts = false;
                self.showing_outbox = false;
                self.unified = false;
                self.selected = None;
                self.tag_view = Some((account, keyword));
                self.current = None;
                self.current_thread.clear();
                self.attachments.clear();
                self.attachments_loading = false;
                self.sync_attachment_drawer();
                self.show_message(None, false);
                self.message_list.emit(MessageListInput::SetSelected(None));
                self.message_list.emit(MessageListInput::SetColorize(true));
                self.message_list.emit(MessageListInput::ResetPaging);
                self.message_list.emit(MessageListInput::SetShowRecipient(false));
                self.message_list.emit(MessageListInput::SetRestorable(false));
                self.message_list.emit(MessageListInput::SetInJunk(false));
                self.message_list.emit(MessageListInput::SetInDrafts(false));
                self.message_view.emit(MessageViewInput::SetDraftsView(false));
                self.show_tag_view();
                self.refresh_tag_view(&sender);
                self.sync_tag_keywords();
                self.push_index_complete();
            }

            AppMsg::KeywordsSynced { account_id, paths } => {
                if paths.is_empty() {
                    return;
                }
                // The index changed under the open views: the tag view
                // re-reads it, and an open folder among the changed ones
                // re-syncs so its rows' chips follow.
                if self.tag_view.is_some() {
                    self.refresh_tag_view(&sender);
                }
                if let Some(sel) = self.selected.clone() {
                    if sel.account_id == account_id && paths.contains(&sel.path) {
                        self.send_to(account_id, MailRequest::LoadMessages {
                            folder_id: sel.folder_id,
                            path: sel.path,
                        });
                    }
                }
            }

            AppMsg::TagViewLoaded { key, rows } => {
                self.tag_view_loading = false;
                let out = self.assemble_tag_view(&key, rows);
                let changed = self.tag_view_cache.get(&key) != Some(&out);
                if changed {
                    self.tag_view_cache.insert(key.clone(), out.clone());
                }
                if self.tag_view.as_ref() == Some(&key) && changed {
                    self.message_list.emit(MessageListInput::SetMessages { messages: out });
                }
                if std::mem::take(&mut self.tag_view_dirty) {
                    self.refresh_tag_view(&sender);
                }
            }

            AppMsg::SetTag { message, keyword, add } => {
                let what = self.tag_label(&keyword, add);
                self.user_set_flags(what, &[*message], FlagKind::Tag(keyword), add);
            }

            AppMsg::MoveToInbox => {
                if let Some(m) = self.reply_target() {
                    self.move_to(m, FolderKind::Inbox);
                }
            }

            AppMsg::ToggleTagCurrent(keyword) => {
                if let Some(m) = self.reply_target() {
                    let add = !m.has_keyword(&keyword);
                    let what = self.tag_label(&keyword, add);
                    self.user_set_flags(what, &[m], FlagKind::Tag(keyword), add);
                }
            }

            AppMsg::ReaderTagMenu => {
                if let Some(entries) = self.reader_tag_entries(&sender) {
                    let btn = &self.reader_tag_btn;
                    crate::ui::context_menu::show_context_menu(
                        btn,
                        (btn.width() / 2) as f64,
                        btn.height() as f64,
                        vec![entries],
                    );
                }
            }

            AppMsg::MoveToMenu => {
                // A multi-selection lists the folders of its first message's
                // account (the move reports anything from another account);
                // otherwise the target's. The picker leaves out the folder
                // the mail already sits in.
                let (account_id, exclude) = if self.list_selection.len() > 1 {
                    (self.list_selection[0].0, None)
                } else {
                    match self.reply_target() {
                        Some(m) => (m.account_id, self.resolve_folder_path(&m)),
                        None => return,
                    }
                };
                let folders = self.folders.get(&account_id).cloned().unwrap_or_default();
                if folders.is_empty() {
                    return;
                }
                // The reading pane shows a conversation (its row was opened,
                // not one reply of it): offer to move the whole of it.
                let conversation = (self.list_selection.len() <= 1
                    && self.current_thread.len() > 1)
                    .then_some(self.current_thread.len());
                // Anchored to the toolbar button, or to the overflow button
                // when the toolbar has folded into it.
                let btn = if self.reader_move_btn.is_mapped() {
                    self.reader_move_btn.clone()
                } else {
                    self.reader_overflow_btn.clone()
                };
                let s = sender.input_sender().clone();
                crate::ui::folder_picker::show_folder_picker(
                    &btn,
                    (btn.width() / 2) as f64,
                    btn.height() as f64,
                    folders,
                    exclude,
                    conversation,
                    move |dest, whole| {
                        let _ = s.send(AppMsg::MoveSelectionTo { account_id, dest, whole });
                    },
                );
            }

            AppMsg::CardMenu { message, x, y } => {
                self.show_card_menu(*message, x, y, &sender);
            }

            AppMsg::ListMoveTo { messages, offer_whole, x, y } => {
                let Some(first) = messages.first() else { return };
                let account_id = first.account_id;
                let folders = self.folders.get(&account_id).cloned().unwrap_or_default();
                if folders.is_empty() {
                    return;
                }
                // Leave out the folder the mail sits in, when it is one.
                let exclude = self
                    .resolve_folder_path(first)
                    .filter(|p| messages.iter().all(|m| self.resolve_folder_path(m).as_ref() == Some(p)));
                let conversation = offer_whole.then_some(messages.len());
                let s = sender.input_sender().clone();
                let window = self.window.clone();
                crate::ui::folder_picker::show_folder_picker(
                    &window,
                    x,
                    y,
                    folders,
                    exclude,
                    conversation,
                    move |dest, whole| {
                        let picked = if offer_whole && !whole {
                            messages[..1].to_vec()
                        } else {
                            messages.clone()
                        };
                        let _ = s.send(AppMsg::MoveMessagesTo { account_id, dest, messages: picked });
                    },
                );
            }

            AppMsg::MoveMessagesTo { account_id, dest, messages } => {
                let items: Vec<(u32, u32, u32, u32)> =
                    messages.iter().map(|m| (m.account_id, m.folder_id, m.uid, m.id)).collect();
                self.drop_move(account_id, dest, items);
            }

            AppMsg::MoveSelectionTo { account_id, dest, whole } => {
                if whole && self.current_thread.len() > 1 {
                    // The whole conversation, the way a dragged selection
                    // moves: grouped by source folder, undoable, and any
                    // member from another account (a conversation merged
                    // across accounts in a unified view) reported.
                    let items: Vec<(u32, u32, u32, u32)> = self
                        .current_thread
                        .iter()
                        .map(|m| (m.account_id, m.folder_id, m.uid, m.id))
                        .collect();
                    self.drop_move(account_id, dest, items);
                } else if self.list_selection.len() > 1 {
                    // The same path a drag onto the sidebar takes: grouped by
                    // source folder, undoable, foreign accounts reported.
                    let items: Vec<(u32, u32, u32, u32)> = self
                        .list_selection
                        .iter()
                        .filter_map(|(aid, id)| {
                            self.find_cached_message(*aid, *id)
                                .map(|m| (m.account_id, m.folder_id, m.uid, m.id))
                        })
                        .collect();
                    self.drop_move(account_id, dest, items);
                } else if let Some(m) = self.reply_target() {
                    self.move_to_path(m, dest);
                }
            }

            AppMsg::OpenListSearch => {
                // Ctrl+F routes by focus (#102/#103): in the reader it finds
                // within the message, everywhere else it searches the list.
                let reader = self.message_view.widget().clone().upcast::<gtk::Widget>();
                let reader_focused =
                    gtk::prelude::GtkWindowExt::focus(&self.window)
                        .is_some_and(|f| f == reader || f.is_ancestor(&reader));
                if reader_focused && self.current.is_some() {
                    self.message_view.emit(MessageViewInput::OpenFind);
                } else {
                    self.message_list.emit(MessageListInput::FocusSearch);
                }
            }

            AppMsg::OpenReaderFind => {
                self.message_view.emit(MessageViewInput::OpenFind);
            }

            AppMsg::SetUnreadFilter(on) => {
                self.message_list.emit(MessageListInput::SetUnreadOnly(on));
            }

            AppMsg::DeferredMarkRead { message } => {
                let still_current = self
                    .current
                    .as_ref()
                    .is_some_and(|c| c.id == message.id && c.account_id == message.account_id);
                if still_current {
                    self.mark_opened_read(&message);
                }
            }

            AppMsg::SetReadMark(policy) => {
                if self.read_mark != policy {
                    self.read_mark = policy;
                    self.save_settings();
                    self.message_view.emit(MessageViewInput::SetReadMark(policy));
                }
            }

            AppMsg::SetStarredFilter(on) => {
                self.message_list.emit(MessageListInput::SetStarredOnly(on));
            }

            AppMsg::ExportSettings => {
                let dialog = gtk::FileDialog::builder()
                    .title(&i18n("Export Settings"))
                    .initial_name("vireo-settings.toml")
                    .build();
                let win = self.window.clone();
                let notif = self.notifications.sender().clone();
                dialog.save(Some(&win), gtk::gio::Cancellable::NONE, move |res| {
                    let Ok(file) = res else { return };
                    let Some(path) = file.path() else { return };
                    let outcome = crate::config::export_bundle()
                        .and_then(|t| std::fs::write(&path, t).map_err(|e| e.to_string()));
                    let _ = notif.send(match outcome {
                        Ok(()) => NotifyInput::Push {
                            text: i18n_f("Settings exported to {display}", &[("display", &(path.display()).to_string())]),
                            error: false,
                            connectivity: false,
                        },
                        Err(e) => NotifyInput::Push {
                            text: i18n_f("Export failed: {e}", &[("e", &(e).to_string())]),
                            error: true,
                            connectivity: false,
                        },
                    });
                });
            }

            AppMsg::ExportLog => {
                let dialog = gtk::FileDialog::builder()
                    .title(&i18n("Export Log"))
                    .initial_name(&format!("hylki-log-{}.txt", chrono::Local::now().format("%Y%m%d-%H%M")))
                    .build();
                let win = self.window.clone();
                let notif = self.notifications.sender().clone();
                // Measured now, while the model is at hand: the chooser's
                // callback runs later, without it.
                let memory = self.memory_report();
                dialog.save(Some(&win), gtk::gio::Cancellable::NONE, move |res| {
                    let Ok(file) = res else { return };
                    let Some(path) = file.path() else { return };
                    let outcome = std::fs::write(&path, crate::console_log::export_text(&memory))
                        .map_err(|e| e.to_string());
                    let _ = notif.send(match outcome {
                        Ok(()) => NotifyInput::Push {
                            text: i18n_f("Log exported to {display}", &[("display", &(path.display()).to_string())]),
                            error: false,
                            connectivity: false,
                        },
                        Err(e) => NotifyInput::Push {
                            text: i18n_f("Export failed: {e}", &[("e", &(e).to_string())]),
                            error: true,
                            connectivity: false,
                        },
                    });
                });
            }

            AppMsg::ExportLogTo(path) => {
                let text = crate::console_log::export_text(&self.memory_report());
                match std::fs::write(&path, text) {
                    Ok(()) => tracing::info!("log exported to {}", path.display()),
                    Err(e) => tracing::warn!("log export to {} failed: {e}", path.display()),
                }
            }

            AppMsg::ImportSettings => {
                let dialog = gtk::FileDialog::builder().title(&i18n("Import Settings")).build();
                let win = self.window.clone();
                let s = sender.clone();
                dialog.open(Some(&win), gtk::gio::Cancellable::NONE, move |res| {
                    // Only carry the choice out of the chooser's callback —
                    // the work (and any dialog) runs on a clean loop turn.
                    if let Ok(file) = res {
                        if let Some(path) = file.path() {
                            s.input(AppMsg::ImportSettingsFrom(path));
                        }
                    }
                });
            }

            AppMsg::ImportSettingsFrom(path) => {
                let outcome = std::fs::read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|t| crate::config::import_bundle(&t));
                match outcome {
                    Ok(n) => {
                        // Imported files are on disk; the running model still
                        // holds the old world, so a clean restart is the
                        // honest way to apply everything at once.
                        // Parent on whichever window is focused (Settings,
                        // normally) — a modal behind the focused window reads
                        // as a freeze.
                        let parent = relm4::main_application()
                            .active_window()
                            .unwrap_or_else(|| self.window.clone().upcast());
                        let alert = adw::MessageDialog::new(
                            Some(&parent),
                            Some(i18n("Settings Imported").as_str()),
                            Some(&i18n_f(
                                "{n} mail account(s), the cloud storage accounts and all \
                                 preferences were imported. Restart Hylki to apply them. \
                                 Passwords and sign-ins are not part of a backup; re-enter \
                                 them on first use if this is a new machine.",
                                &[("n", &n.to_string())],
                            )),
                        );
                        alert.add_response("later", &i18n("Later"));
                        alert.add_response("restart", &i18n("Restart Hylki"));
                        alert
                            .set_response_appearance("restart", adw::ResponseAppearance::Suggested);
                        alert.connect_response(None, |_, resp| {
                            if resp == "restart" {
                                // A detached shell starts the next instance
                                // once this one has quit and released its
                                // D-Bus name (started immediately, the new
                                // instance would just relay to the dying
                                // primary and exit).
                                if let Ok(exe) = std::env::current_exe() {
                                    let _ = std::process::Command::new("sh")
                                        .arg("-c")
                                        .arg(format!("sleep 1.5; exec '{}'", exe.display()))
                                        .spawn();
                                }
                                relm4::main_application().quit();
                            }
                        });
                        alert.present();
                    }
                    Err(e) => {
                        self.notifications.emit(NotifyInput::Push {
                            text: i18n_f("Import failed: {e}", &[("e", &(e).to_string())]),
                            error: true,
                            connectivity: false,
                        });
                    }
                }
            }

            AppMsg::SetConsoleMode(on) => {
                if self.console_mode != on {
                    self.console_mode = on;
                    self.save_settings();
                    self.notifications.emit(NotifyInput::SetConsoleEnabled(on));
                    self.rebuild_help_menu();
                }
            }

            AppMsg::ApplyWelcomePrefs(p) => {
                config::mark_wizard_completed();
                // Route every choice through its normal handler (each saves and
                // updates the live UI); drop the wizard controller afterwards.
                sender.input(AppMsg::SetAutoRemoteContent(!p.block_remote));
                sender.input(AppMsg::SetGravatar(p.gravatar));
                sender.input(AppMsg::SetSenderLogos(p.sender_logos));
                sender.input(AppMsg::SetNotificationContent(p.notification_content));
                sender.input(AppMsg::SetPreviewLines(p.preview_lines));
                sender.input(AppMsg::SetAvatars(p.avatars));
                sender.input(AppMsg::SetThreading(p.threading));
                sender.input(AppMsg::SetAppIcon(p.app_icon));
                self.welcome = None;
            }

            AppMsg::ImportGoaAccount(account) => {
                // Enable a GNOME Online Account in Hylki (or re-enable if already
                // imported). Its password came from GOA and is stored in the keyring.
                let email = account.email.clone();
                if let Some(slot) = self.config.iter_mut().find(|c| c.email == email) {
                    slot.enabled = true;
                    // Re-importing refreshes the GOA-owned connection details in
                    // place — e.g. a broken pre-#36 Microsoft 365 import (empty
                    // hosts, IMAP) repairs into a Graph account — keeping the
                    // slot (account ids are config indices) and the cosmetics.
                    if account.goa_id.is_some() {
                        slot.protocol = account.protocol;
                        slot.imap_host = account.imap_host.clone();
                        slot.imap_port = account.imap_port;
                        slot.smtp_host = account.smtp_host.clone();
                        slot.smtp_port = account.smtp_port;
                        slot.username = account.username.clone();
                        slot.smtp_separate = account.smtp_separate;
                        slot.smtp_username = account.smtp_username.clone();
                        slot.oauth = account.oauth;
                        slot.oauth_settings = account.oauth_settings.clone();
                        slot.oauth_refresh = account.oauth_refresh.clone();
                        slot.goa_id = account.goa_id.clone();
                    }
                } else {
                    self.config.push(*account);
                }
                match config::save(&self.config) {
                    Ok(()) => self.reconnect_all(&sender),
                    Err(e) => self.notifications.emit(NotifyInput::Push {
                        text: i18n_f("Could not import account: {e}", &[("e", &(e).to_string())]),
                        error: true,
                        connectivity: false,
                    }),
                }
            }

            AppMsg::AccountRemoved { email } => {
                // Drop the keyring entries while the config (which knows the
                // alias addresses) is still around.
                match self.config.iter().find(|c| c.email == email) {
                    Some(acc) => config::delete_account_secrets(acc),
                    None => config::delete_password(&email),
                }
                self.config.retain(|c| c.email != email);
                self.account_order.retain(|e| *e != email);
                self.collapsed.retain(|e| *e != email);
                if let Err(e) = config::save(&self.config) {
                    tracing::error!("could not save config: {e}");
                }
                self.save_sidebar_state();
                self.reconnect_all(&sender);
            }

            AppMsg::GoaChanged(live) => {
                // GOA changed in GNOME Settings. Drop any imported account that
                // no longer exists there; pause/resume any whose Mail service
                // was toggled. (Adding an account to GOA never auto-imports —
                // that stays a manual choice.)
                // Snapshot first: reconcile removes accounts from the config,
                // and the alias-password keyring entries are keyed by data
                // (the alias addresses) only the config holds.
                let before = self.config.clone();
                let outcome = reconcile_goa(&mut self.config, &live);
                if !outcome.removed.is_empty() {
                    for email in &outcome.removed {
                        match before.iter().find(|c| &c.email == email) {
                            Some(acc) => config::delete_account_secrets(acc),
                            None => config::delete_password(email),
                        }
                        self.account_order.retain(|e| e != email);
                        self.collapsed.retain(|e| e != email);
                        self.folders_expanded.retain(|e| e != email);
                        self.filtered_expanded_accounts.retain(|e| e != email);
                        self.tags_expanded_accounts.retain(|e| e != email);
                    }
                    self.save_sidebar_state();
                }
                if !outcome.removed.is_empty() || outcome.paused_changed {
                    if let Err(e) = config::save(&self.config) {
                        tracing::error!("could not save config after GOA change: {e}");
                    }
                    self.reconnect_all(&sender);
                }
            }

            AppMsg::SystemResumed => {
                // Sockets left open across suspend are dead. Reconnect drops the
                // stale session, logs in fresh and re-arms IMAP IDLE — and it
                // unsticks any worker parked inside an IDLE wait, since the
                // request breaks its select loop. Then reload the visible folder
                // so new mail appears without waiting for the next auto-fetch.
                for w in self.workers.values() {
                    let _ = w.send(MailRequest::Reconnect);
                }
                sender.input(AppMsg::Refresh);
                // A Gravatar lookup that failed while the network was down
                // gets another go now that it is back (#189).
                self.warm_own_gravatars(&sender);
                // Realign the auto-fetch timer to now; its monotonic countdown
                // did not advance during sleep.
                self.arm_auto_fetch(&sender);
            }

            AppMsg::OpenSettings => {
                // The "opens to" preference decides the first open of the
                // session; after that the window returns to where it was.
                let on_accounts = self.settings_open_accounts && self.last_settings_page.is_none();
                self.open_settings_window(&sender, on_accounts, false);
            }

            AppMsg::SettingsPageShown(id) => {
                self.last_settings_page = Some(id);
            }

            AppMsg::OpenPreferences => self.open_settings_window(&sender, false, false),

            AppMsg::SetSettingsOpenAccounts(on) => {
                if self.settings_open_accounts != on {
                    self.settings_open_accounts = on;
                    self.save_settings();
                }
            }

            // Closing the combined Settings window hides it (the window's
            // own hide-on-close); both panels stay for the next open, which
            // then only rebuilds the accounts panel.
            AppMsg::ClosePreferences => {
                // The window only hides; let the signature editor's web
                // process go with it (#221).
                if let Some(a) = &self.accounts_win {
                    a.emit(crate::ui::accounts::AccountsInput::ReleaseSignatureEditor);
                }
            }

            // Build the Settings window ahead of its first open, hidden and
            // realized, a moment after startup: its first appearance is
            // then a present() rather than a build, a layout and a paint.
            AppMsg::DebugCloseSettings => {
                if let Some(p) = &self.prefs {
                    p.widget().close();
                }
            }

            AppMsg::PrewarmSettings => {
                if self.prefs.is_none() {
                    self.build_settings_window(&sender, self.settings_open_accounts);
                    if let Some(p) = &self.prefs {
                        let w = p.widget();
                        WidgetExt::realize(w);
                        // Measuring runs the first CSS pass and Pango layouts
                        // now, so the first frame after present has less to do.
                        let _ = w.measure(gtk::Orientation::Horizontal, -1);
                        let _ = w.measure(gtk::Orientation::Vertical, 920);
                    }
                }
            }

            AppMsg::ShowcaseDirtyEditor => {
                if let Some(acc) = &self.accounts_win {
                    acc.emit(crate::ui::accounts::AccountsInput::DebugEditLabel(
                        "Edited in the capture".into(),
                    ));
                }
            }

            AppMsg::SettingsEditorOpen(open) => {
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::EditorOpen(open));
                }
            }

            AppMsg::SettingsLeaveEditor { page, ask } => {
                if let Some(p) = &self.prefs {
                    p.emit(if ask {
                        PrefInput::LeaveEditorPrompt(page)
                    } else {
                        PrefInput::LeaveEditorTo(page)
                    });
                }
            }

            AppMsg::SetAccount(account) => {
                if let Some(existing) = self.accounts.iter_mut().find(|a| a.id == account.id) {
                    *existing = account;
                } else {
                    // Layout not remembered: an account starts folded up,
                    // wherever it came from (config, GOA, the demo). Only
                    // on arrival, so opening it by hand sticks for the
                    // session.
                    if !self.remember_sidebar && !self.collapsed.contains(&account.email) {
                        self.collapsed.push(account.email.clone());
                    }
                    self.accounts.push(account);
                    self.accounts.sort_by_key(|a| a.id);
                }
                self.rebuild_sidebar();
            }

            AppMsg::SetFolders { account_id, folders } => {
                self.notifications.emit(NotifyInput::ClearConnectivity);
                // Defence in depth against a wedged session's empty LIST (the
                // worker filters these too): never trade a real folder list
                // for nothing.
                if folders.is_empty()
                    && self.folders.get(&account_id).is_some_and(|old| !old.is_empty())
                {
                    return;
                }
                let mut folders = folders;
                // The first listing of an account is looked over once for
                // Exchange's non-mail folders (#239), which are hidden the
                // way any folder is: the account editor lists them, and can
                // bring any of them back. Noted as done either way, so a
                // folder brought back stays back.
                let seed = self
                    .config
                    .get(account_id as usize - 1)
                    .filter(|cfg| !cfg.folders_seeded)
                    .map(|_| crate::models::exchange_non_mail_folders(&folders));
                if let Some(seed) = seed {
                    if let Some(cfg) = self.config.get_mut(account_id as usize - 1) {
                        cfg.folders_seeded = true;
                        if let Err(e) = config::save(&self.config) {
                            tracing::warn!("could not note the folder look-over: {e}");
                        }
                    }
                    if !seed.is_empty() {
                        tracing::info!("account {account_id}: hiding Exchange's non-mail folders {seed:?}");
                        self.hide_folders(account_id, seed);
                    }
                }
                // A listing from before a hide (the cache at startup, an
                // answer already on its way) still carries what is hidden.
                if let Some(cfg) = self.config.get(account_id as usize - 1) {
                    if !cfg.hidden_folders.is_empty() {
                        let hidden = cfg.hidden_folders.clone();
                        folders.retain(|f| !crate::models::folder_is_hidden(&f.path, None, &hidden));
                    }
                }
                // Manual special-folder assignments (#82) ride over whatever
                // the worker detected.
                if let Some(cfg) = self.config.get(account_id as usize - 1) {
                    apply_folder_roles(&cfg.folder_roles.clone(), &mut folders);
                }
                // Keep the settings editor's folder choices current while open.
                if let Some(acc) = &self.accounts_win {
                    acc.emit(crate::ui::accounts::AccountsInput::SetFolderChoices(
                        self.folder_choice_map_with(account_id, &folders),
                    ));
                }
                // Merge unread counts by PATH, and never let a zero overwrite
                // a known count: a refresh's per-folder STATUS can fail
                // silently (Gmail right after a RENAME answers zeros), and
                // adopting them wholesale wiped every unread chip. A genuine
                // zero re-asserts through the per-folder sync events.
                let prev_by_path: std::collections::HashMap<String, u32> = self
                    .folders
                    .get(&account_id)
                    .map(|old| {
                        old.iter()
                            .map(|f| {
                                let n = self
                                    .folder_unread
                                    .get(&(account_id, f.id))
                                    .copied()
                                    .unwrap_or(f.unread);
                                (f.path.clone(), n)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.folder_unread.retain(|(a, _), _| *a != account_id);
                for f in &folders {
                    let known = prev_by_path.get(&f.path).copied().unwrap_or(0);
                    let n = if f.unread > 0 { f.unread } else { known };
                    self.folder_unread.insert((account_id, f.id), n);
                }
                // An identical list — above all the refresh confirming an
                // optimistic folder move, whose local reshape mirrors the
                // worker exactly — changes nothing on screen: skip the rebuild
                // so the sidebar's rows don't vanish and reappear for it.
                let unchanged = self.folders.get(&account_id).is_some_and(|old| {
                    old.len() == folders.len()
                        && old.iter().zip(&folders).all(|(a, b)| {
                            a.id == b.id
                                && a.path == b.path
                                && a.kind == b.kind
                                && a.name == b.name
                        })
                });
                self.folders.insert(account_id, folders);
                if unchanged {
                    self.push_unread_counts();
                } else {
                    self.rebuild_sidebar();
                }
                // Launch with "All Inboxes" already selected: UnifiedSelected
                // fired before any folder list existed, so its inbox requests
                // went nowhere and the list sat empty until the user visited a
                // folder by hand. The first time an account's folders arrive
                // while the unified view is up, ask for its inbox — the cache
                // priming already painted its slice, and this is what brings
                // in whatever changed since the app last ran.
                if self.unified && self.unified_boot_requested.insert(account_id) {
                    let mine: Vec<(u32, u32, String)> = self
                        .unified_targets()
                        .into_iter()
                        .filter(|(a, _, _)| *a == account_id)
                        .collect();
                    for (_, folder_id, path) in mine {
                        self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
                    }
                }
                self.index_sent_folders(account_id);
                // A unified view merges one folder per account, and this
                // account's folders may only now have arrived — so the set the
                // completeness flag is measured over just changed. It is
                // otherwise recomputed on `BackfillDone` alone, which meant an
                // account joining after every other one had finished left the
                // list claiming a complete index, or (once its own load
                // landed) spinning with no event left to clear it (#218).
                self.push_index_complete();
            }

            AppMsg::FolderUnread { account_id, folder_id, unread } => {
                // A count fetched ahead of a read mark still in the worker's
                // queue: the app's own count (adjusted when the mark was
                // made) stands until the mark is stored.
                if self.pending_seen_in_folder(account_id, folder_id) {
                    return;
                }
                let prev = self.folder_unread.insert((account_id, folder_id), unread);
                if prev != Some(unread) {
                    self.sync_background_folder(account_id, folder_id);
                }
                self.push_unread_counts();
            }

            AppMsg::SeenSettled { account_id, path, uid } => {
                self.pending_seen.remove(&(account_id, path, uid));
                self.pending_seen.retain(|_, (_, at)| at.elapsed() < PENDING_SEEN_MAX);
            }

            AppMsg::FolderUnreadByPath { account_id, path, unread } => {
                if self.pending_seen_in(account_id, &path) {
                    return;
                }
                // Resolve against the current list; a path the app no longer
                // knows (folder deleted/renamed under a live watcher) is
                // dropped rather than guessed at.
                let id = self
                    .folders
                    .get(&account_id)
                    .and_then(|fs| fs.iter().find(|f| f.path == path))
                    .map(|f| f.id);
                if let Some(folder_id) = id {
                    let prev = self.folder_unread.insert((account_id, folder_id), unread);
                    if prev != Some(unread) {
                        self.sync_background_folder(account_id, folder_id);
                    }
                    self.push_unread_counts();
                }
            }

            AppMsg::ShowcaseEditFilter(i) => {
                if let Some(a) = &self.accounts_win {
                    a.emit(crate::ui::accounts::AccountsInput::EditFilter(i));
                }
            }

            AppMsg::BodyHits { account_id, folder_id, hits } => {
                // The whole answer for the sync about to land; the previous
                // one described mail that sync lists again anyway.
                self.body_hits.insert((account_id, folder_id), hits);
            }

            AppMsg::Messages { account_id, folder_id, messages } => {
                self.notifications.emit(NotifyInput::ClearConnectivity);
                let messages = self.merge_local_tags(account_id, messages);
                // Auto-delete blacklisted senders from the inbox before anything
                // else sees them.
                let messages = self.apply_blacklist(account_id, folder_id, messages);
                let (messages, filed) = self.apply_filters(account_id, folder_id, messages);
                // A list fetched ahead of a read mark still in the worker's
                // queue shows the message unread again; keep the app's state.
                let messages = self.apply_pending_seen(account_id, folder_id, messages);
                // A message a move has just brought home arrives with a new
                // UID — that is what a move does — which leaves its body filed
                // under the id the old one became. Move the body across before
                // anything asks for it (#200).
                if !self.carried_bodies.is_empty() {
                    for m in messages.iter().filter(|m| !m.message_id.is_empty()) {
                        if let Some(body) =
                            self.carried_bodies.remove(&(account_id, m.message_id.clone()))
                        {
                            self.body_cache.insert((account_id, m.id), body);
                        }
                    }
                }
                // The reader may still be holding the copy from before the
                // move. Point it at the row that is really there now: every
                // action on it addresses a UID, and the check just below would
                // otherwise read it as a message deleted from under us.
                let renumbered: HashMap<&str, (u32, u32)> = messages
                    .iter()
                    .filter(|m| !m.message_id.is_empty())
                    .map(|m| (m.message_id.as_str(), (m.uid, m.id)))
                    .collect();
                let mut renumber = |m: &mut Message| {
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
                // Did this sync remove the message currently open in the reader
                // (deleted/moved on another device)? Scope the check to the reader's
                // own folder so a folder switch or another folder's sync doesn't
                // count. Capture where it sat so we can advance to that slot.
                let vanished = self.current.as_ref().is_some_and(|c| {
                    c.account_id == account_id
                        && c.folder_id == folder_id
                        && !messages.iter().any(|m| m.uid == c.uid)
                });

                let next_after_vanish = if vanished {
                    let cur_uid = self.current.as_ref().unwrap().uid;
                    next_after_vanish(
                        self.message_cache.get(&(account_id, folder_id)),
                        &messages,
                        cur_uid,
                    )
                } else {
                    None
                };
                // Desktop-notify for genuinely new inbox mail. Only when Hylki
                // isn't the active window (no point notifying about mail you're
                // watching arrive), only for the Inbox, and never on the first load
                // of a folder (no prior cache) — that would fire for every existing
                // message on startup. "New" = unread and not in the previous sync.
                // Mail a filter just filed elsewhere still counts (#47 feedback):
                // it is new mail even though it never lands in the inbox list.
                if self.notifications_enabled && !self.window.is_active() {
                    let is_inbox = self.folder_kind(account_id, folder_id) == Some(FolderKind::Inbox);
                    if let (true, Some(old)) =
                        (is_inbox, self.message_cache.get(&(account_id, folder_id)))
                    {
                        let old_uids: std::collections::HashSet<u32> =
                            old.iter().map(|m| m.uid).collect();
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
                            (Some(m), nf)
                                if nf.is_none_or(|(f, _)| f.timestamp <= m.timestamp) =>
                            {
                                crate::notify::new_mail(
                                    account_id,
                                    folder_id,
                                    m.id,
                                    &m.from_name,
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
                                    .folders
                                    .get(&account_id)
                                    .and_then(|fs| fs.iter().find(|f| &f.path == dest_path))
                                    .map_or(folder_id, |f| f.id);
                                crate::notify::new_mail(
                                    account_id,
                                    dest_id,
                                    m.id,
                                    &m.from_name,
                                    &m.subject,
                                    others,
                                    false,
                                );
                            }
                            _ => {}
                        }
                    }
                }
                // Cache for instant display when revisiting this folder.
                // The worker answers a load from its cache first and from the
                // server after, and the view was seeded from the same cache
                // when it opened — so most arrivals change nothing. Only a
                // changed list is worth re-threading and re-emitting: a
                // rebuild of a large (or merged) list is not free, and it
                // used to happen several times over per visit.
                let unchanged = self.message_cache.get(&(account_id, folder_id)) == Some(&messages);
                self.message_cache
                    .insert((account_id, folder_id), messages.clone());
                // A draft picker waiting on this folder's list (Send with
                // Hylki → Continue a Draft…) opens once every Drafts folder
                // has answered.
                if let Some((_, waiting)) = self.pending_draft_pick.as_mut() {
                    waiting.remove(&(account_id, folder_id));
                    if waiting.is_empty() {
                        sender.input(AppMsg::HandOffDraftPickNow);
                    }
                }
                if !unchanged {
                    // A sync can add a reply to a conversation already
                    // assembled, so what was stored is no longer necessarily
                    // the conversation.
                    self.forget_threads(account_id);
                    self.push_thread_links();
                    // A sync that brought in a reply also changed how big the
                    // conversations are; the badges have to be counted again
                    // rather than kept from before it (#222).
                    self.message_list
                        .emit(MessageListInput::ForgetThreadCounts(account_id));
                }
                // After that sweep, not before it: a conversation carried over
                // a move (#200) goes back under the id the message now has,
                // with the member that moved renumbered to match. The reader
                // paints a remembered conversation with no lookup and no
                // spinner, which is the whole point of carrying it.
                if !self.carried_threads.is_empty() {
                    for m in messages.iter().filter(|m| !m.message_id.is_empty()) {
                        let Some(mut thread) =
                            self.carried_threads.remove(&(account_id, m.message_id.clone()))
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
                        self.remember_thread_for((account_id, m.id), thread);
                    }
                }
                if self.unified {
                    // Accept only the folders the view merges, by recency.
                    if self.is_unified_target(account_id, folder_id) {
                        let same_slice = unchanged
                            && self
                                .unified_slices
                                .get(&(account_id, folder_id))
                                .is_some_and(|s| *s == messages);
                        if !same_slice {
                            self.unified_slices.insert((account_id, folder_id), messages);
                            self.emit_unified();
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
                    self.refresh_tag_view(&sender);
                }
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

            AppMsg::UndoRestored { account_id, folder_id, message_ids } => {
                // The undone move landed and the folder's reload has already
                // been processed (the worker sends Messages first): put the
                // user on the restored message — selected, loaded in the
                // reader, and scrolled into view — instead of wherever the
                // reload left the list. If the restored message isn't in the
                // current view (folder switched meanwhile), SelectAndLoad
                // finds no row and nothing moves.
                // Unless the reader is already on it, which it will be when
                // the undo put the row back itself (#200): selecting it again
                // would tear the message down and render it a second time for
                // no change the reader can see.
                let already_open = self
                    .current
                    .as_ref()
                    .is_some_and(|c| !c.message_id.is_empty() && message_ids.contains(&c.message_id));
                if already_open {
                    return;
                }
                let restored = self
                    .message_cache
                    .get(&(account_id, folder_id))
                    .and_then(|msgs| {
                        msgs.iter().find(|m| message_ids.contains(&m.message_id))
                    })
                    .map(|m| (m.account_id, m.id));
                if let Some(key) = restored {
                    self.message_list.emit(MessageListInput::SelectAndLoad(key));
                }
            }

            AppMsg::MessagesAppend { account_id, folder_id, messages } => {
                // Background backfill: grow the folder's search index without
                // disturbing the current view (no title/query reset).
                let messages = self.merge_local_tags(account_id, messages);
                let messages = self.apply_blacklist(account_id, folder_id, messages);
                let entry = self.message_cache.entry((account_id, folder_id)).or_default();
                let existing: std::collections::HashSet<u32> =
                    entry.iter().map(|m| m.uid).collect();
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
                        self.unified_slices
                            .entry((account_id, folder_id))
                            .or_default()
                            .extend(fresh.iter().cloned());
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

            AppMsg::BackfillDone { account_id, folder_id } => {
                self.indexed_folders.insert((account_id, folder_id));
                self.push_index_complete();
            }

            AppMsg::SenderChecked { account_id, message_id, check } => {
                // Remember it: prefetch delivers the verdict long before the
                // message is opened, and opening it renders from the in-memory
                // body cache without asking the worker for anything.
                self.sender_cache
                    .insert((account_id, message_id), check.clone());
                // Only the message actually on screen; a verdict that arrives
                // from a background prefetch must not relabel a different one.
                if self
                    .current
                    .as_ref()
                    .is_some_and(|c| c.id == message_id && c.account_id == account_id)
                {
                    self.message_view
                        .emit(MessageViewInput::SetSenderCheck(check.clone()));
                }
                // Light the header seal on whichever on-screen card this
                // verdict belongs to (#88) — the open single message included
                // (it never fills current_thread).
                if self
                    .current_thread
                    .iter()
                    .any(|m| m.account_id == account_id && m.id == message_id)
                    || self
                        .current
                        .as_ref()
                        .is_some_and(|c| c.id == message_id && c.account_id == account_id)
                {
                    self.message_view.emit(MessageViewInput::SenderCheckFor {
                        account_id,
                        id: message_id,
                        check: check.clone(),
                    });
                }
                if let Some(p) = self.popouts.get(&(account_id, message_id)) {
                    p.controller.emit(MessageWindowInput::SetSenderCheck(check));
                }
            }

            AppMsg::Body { account_id, message_id, path, body } => {
                // Interior NULs abort glib string conversion (labels and the
                // WebView document alike); decoded mail bodies can carry them.
                let body =
                    if body.contains('\0') { body.replace('\0', " ") } else { body };
                self.body_cache
                    .insert((account_id, message_id), body.clone());
                // If this body was fetched to open a draft, open the editor now.
                if let Some((pd, inline, extra)) = self.pending_draft.take() {
                    if pd.account_id == account_id && pd.id == message_id {
                        self.compose_from_draft(pd, body, inline, extra, &sender);
                        return;
                    }
                    self.pending_draft = Some((pd, inline, extra));
                }
                // Likewise a copy being edited as a new message: the body is
                // in the cache now, so the second pass finds it there.
                if let Some(pending) = self.pending_edit_as_new.take() {
                    if pending.account_id == account_id && pending.id == message_id {
                        self.edit_as_new(pending, &sender);
                        return;
                    }
                    self.pending_edit_as_new = Some(pending);
                }
                // And a forward waiting on the body it quotes.
                if let Some((pending, inline)) = self.pending_forward.take() {
                    if pending.account_id == account_id && pending.id == message_id {
                        self.forward(pending, inline, &sender);
                        return;
                    }
                    self.pending_forward = Some((pending, inline));
                }
                // A reply from a notification's button (#244): the message
                // is opening in the reader too, so the body goes on to be
                // shown as usual.
                if let Some(mut m) = self.pending_notified_reply.take() {
                    if m.account_id == account_id && m.id == message_id {
                        m.body = body.clone();
                        self.open_inline_reply(
                            m.account_id,
                            self.reply_pgp(&m, reply_prefill(&m)),
                            Some((m.account_id, m.id)),
                            &sender,
                        );
                    } else {
                        self.pending_notified_reply = Some(m);
                    }
                }
                // Likewise a reply picked for handed-in files.
                if let Some((mut m, extra)) = self.pending_reply.take() {
                    if m.account_id == account_id && m.id == message_id {
                        m.body = body.clone();
                        self.open_hand_off_reply(m, extra, &sender);
                        return;
                    }
                    self.pending_reply = Some((m, extra));
                }
                // A UID is unique only within its folder, and the background
                // prefetch pushes bodies from every folder it syncs. Matching on
                // the number alone would let one folder's body overwrite a
                // different message that happens to share it.
                let folder = self
                    .folders
                    .get(&account_id)
                    .and_then(|fs| fs.iter().find(|f| f.path == path))
                    .map(|f| f.id);
                let is_target = |m: &Message| {
                    m.account_id == account_id
                        && m.id == message_id
                        && folder.is_none_or(|fid| m.folder_id == fid)
                };
                // Keep the primary's body up to date in either mode.
                if let Some(current) = self.current.as_mut() {
                    if current.account_id == account_id
                        && current.id == message_id
                        && folder.is_none_or(|fid| current.folder_id == fid)
                    {
                        current.body = body.clone();
                    }
                }
                if self.current_thread.len() > 1 {
                    // Conversation mode: fill the matching message's body. The
                    // whole conversation renders as one document holding every
                    // body, so a burst of arrivals is coalesced into a single
                    // re-render rather than one per body.
                    let mut changed = false;
                    for tm in self.current_thread.iter_mut() {
                        if is_target(tm) {
                            tm.body = body.clone();
                            changed = true;
                        }
                    }
                    if changed {
                        self.queue_thread_render(&sender);
                    }
                } else if self.current.as_ref().is_some_and(is_target) {
                    let current = self.current.clone();
                    self.show_message(current, false);
                }
                // Re-render any popped-out window showing this message — a
                // conversation window may hold it as any member of its thread,
                // so every popout gets the offer (non-members ignore it).
                for p in self.popouts.values() {
                    p.controller.emit(MessageWindowInput::SetBody {
                        account_id,
                        id: message_id,
                        body: body.clone(),
                    });
                }
            }

            AppMsg::RefsRepaired { account_id, folder_id } => {
                // Only the folder on screen, and only its cached copy: the repair
                // rewrote rows on disk, and re-reading is what lets the list see
                // the conversations they now form.
                let showing = self
                    .selected
                    .as_ref()
                    .is_some_and(|sel| sel.account_id == account_id && sel.folder_id == folder_id);
                if showing || self.unified {
                    if let Some(path) = self
                        .folders
                        .get(&account_id)
                        .and_then(|fs| fs.iter().find(|f| f.id == folder_id))
                        .map(|f| f.path.clone())
                    {
                        self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
                    }
                }
            }

            AppMsg::SelectionKeys(keys) => {
                self.list_selection = keys.clone();
                self.selection_from_cards = false;
                self.message_view.emit(MessageViewInput::SetSelectedCards(keys));
            }

            AppMsg::SelectCards(keys) => {
                self.list_selection = keys.clone();
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
                let conversation =
                    self.current_thread.iter().map(|m| (m.account_id, m.id)).collect();
                self.message_list.emit(MessageListInput::SelectFromReader { keys, conversation });
            }

            AppMsg::ThreadMessageSeen { account_id, id } => {
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
                let Some((uid, folder_id)) = target else { return };
                if let Some(path) = self
                    .folders
                    .get(&account_id)
                    .and_then(|fs| fs.iter().find(|f| f.id == folder_id))
                    .map(|f| f.path.clone())
                {
                    self.send_to(account_id, MailRequest::SetSeen { path, uid, seen: true });
                }
                self.message_list.emit(MessageListInput::MarkRead(id));
                self.mark_cached_read(account_id, id);
                // The card's dot clears in place as the viewport observer
                // marks it (#100) — this path never told the view before.
                self.message_view.emit(MessageViewInput::ClearDot { account_id, id });
                if let Some(n) = self.folder_unread.get_mut(&(account_id, folder_id)) {
                    *n = n.saturating_sub(1);
                }
                self.remember_thread();
                self.push_unread_counts();
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

            AppMsg::Source { text } => {
                // Source is only fetched on explicit request (toolbar or context
                // menu), so always show it — even for a message that isn't open.
                self.show_source_window(&text);
            }

            AppMsg::Attachments { account_id, message_id, items } => {
                self.attachment_cache
                    .insert((account_id, message_id), items.clone());
                // A file the gallery asked for: hand the bytes straight over so
                // the thumbnail and the preview fill in without a reload, which
                // would throw away the user's place in the list.
                if self.showing_gallery {
                    self.gallery.emit(GalleryInput::SetFetching(false));
                    self.gallery.emit(GalleryInput::Fetched {
                        account_id,
                        uid: message_id,
                        items: items.clone(),
                    });
                }
                if self
                    .current
                    .as_ref()
                    .is_some_and(|c| c.id == message_id && c.account_id == account_id)
                {
                    self.attachments_loading = false;
                    self.attachments = items.clone();
                    self.sync_attachment_drawer();
                }
                // With a conversation open the drawer spans the whole thread, so
                // any member's arrival re-merges the union (this supersedes the
                // single-message assignment above when both apply).
                if self.current_thread.len() > 1
                    && self
                        .current_thread
                        .iter()
                        .any(|tm| tm.id == message_id && tm.account_id == account_id)
                {
                    self.attachments_loading = false;
                    self.refresh_thread_attachments();
                }
                if let Some(p) = self.popouts.get(&(account_id, message_id)) {
                    p.controller.emit(MessageWindowInput::SetAttachments(items));
                }
                // These files were fetched to be carried into a copy of the
                // message; they are cached now, so the second pass stages them.
                if let Some(pending) = self.pending_edit_as_new.take() {
                    if pending.account_id == account_id && pending.id == message_id {
                        self.edit_as_new(pending, &sender);
                    } else {
                        self.pending_edit_as_new = Some(pending);
                    }
                }
                // The same for a forward that carries them (#240).
                if let Some((pending, inline)) = self.pending_forward.take() {
                    if pending.account_id == account_id && pending.id == message_id {
                        self.forward(pending, inline, &sender);
                    } else {
                        self.pending_forward = Some((pending, inline));
                    }
                }
            }

            AppMsg::AttachmentsPending { account_id, message_id } => {
                // Attachments exist but weren't on disk. Opening the message
                // was the request — fetch them now rather than asking for a
                // click (the old "load attachments" button). Every reader path
                // now downloads outright, so this is a safety net for any
                // cache-only probe that still answers "present, not fetched".
                let msg = self
                    .current
                    .as_ref()
                    .filter(|c| c.id == message_id && c.account_id == account_id)
                    .cloned()
                    .or_else(|| {
                        self.current_thread
                            .iter()
                            .find(|m| m.id == message_id && m.account_id == account_id)
                            .cloned()
                    });
                if let Some(m) = msg {
                    if let Some(path) = self.resolve_folder_path(&m) {
                        self.attachments_loading = true;
                        self.send_to(account_id, MailRequest::LoadAttachments {
                            message_id,
                            path,
                            uid: m.uid,
                            download: true,
                        });
                    }
                }
                if let Some(p) = self.popouts.get(&(account_id, message_id)) {
                    p.controller.emit(MessageWindowInput::AttachmentsPending);
                }
            }

            AppMsg::NoAttachments { account_id, message_id } => {
                // Clear a false paperclip live. Update every cached folder for the
                // account (a UID is per-folder, but the same message copied across
                // folders shares its attachment status) and the visible row.
                for ((aid, _), msgs) in self.message_cache.iter_mut() {
                    if *aid == account_id {
                        for m in msgs.iter_mut().filter(|m| m.id == message_id) {
                            m.has_attachment = false;
                        }
                    }
                }
                if let Some(c) = self.current.as_mut() {
                    if c.id == message_id && c.account_id == account_id {
                        c.has_attachment = false;
                    }
                }
                self.message_list
                    .emit(MessageListInput::SetHasAttachment { id: message_id, has: false });
                // A copy waiting on files that do not exist: open it anyway,
                // rather than leave the menu entry looking dead.
                if let Some(mut pending) = self.pending_edit_as_new.take() {
                    if pending.account_id == account_id && pending.id == message_id {
                        pending.has_attachment = false;
                        self.edit_as_new(pending, &sender);
                    } else {
                        self.pending_edit_as_new = Some(pending);
                    }
                }
                if let Some((mut pending, inline)) = self.pending_forward.take() {
                    if pending.account_id == account_id && pending.id == message_id {
                        pending.has_attachment = false;
                        self.forward(pending, inline, &sender);
                    } else {
                        self.pending_forward = Some((pending, inline));
                    }
                }
            }

            AppMsg::HasAttachments { account_id, message_id } => {
                // The mirror of NoAttachments: a message whose structure didn't
                // advertise its attachments (an inline PDF, say — issue #9) is
                // given its paperclip once the body has proved they are there.
                for ((aid, _), msgs) in self.message_cache.iter_mut() {
                    if *aid == account_id {
                        for m in msgs.iter_mut().filter(|m| m.id == message_id) {
                            m.has_attachment = true;
                        }
                    }
                }
                self.message_list
                    .emit(MessageListInput::SetHasAttachment { id: message_id, has: true });
                // If it is the message on screen, fetch the files too: the reader
                // only asks for them when the flag was already set, which by
                // definition it wasn't.
                let already = self
                    .current
                    .as_ref()
                    .is_some_and(|c| c.id == message_id && c.account_id == account_id && c.has_attachment);
                let open = self
                    .current
                    .as_mut()
                    .filter(|c| c.id == message_id && c.account_id == account_id);
                if let (false, Some(current)) = (already, open) {
                    current.has_attachment = true;
                    let message = current.clone();
                    if let Some(cached) =
                        self.attachment_cache.get(&(account_id, message_id)).cloned()
                    {
                        self.attachments = cached;
                        self.sync_attachment_drawer();
                    } else if let Some(path) = self.resolve_folder_path(&message) {
                        self.attachments_loading = true;
                        self.send_to(account_id, MailRequest::LoadAttachments {
                            message_id,
                            path,
                            uid: message.uid,
                            download: true,
                        });
                    }
                }
            }

            AppMsg::Status { account_id, text } => {
                if text.is_empty() {
                    self.busy.remove(&account_id);
                } else {
                    self.busy.insert(account_id);
                }
                self.update_busy_indicator();
                // A sync going quiet must not blank the status while background
                // bulk operations are still reporting there.
                if !text.is_empty() || self.bulk_pending == 0 {
                    self.notifications.emit(NotifyInput::SetStatus(text));
                }
            }

            AppMsg::Error { account_id, text, connectivity } => {
                // The log gets the whole error (a parser dump included, which
                // is what diagnoses #226-style replies); the bar gets it
                // readable.
                tracing::error!("[account {account_id}] {text}");
                let text = crate::worker::readable_error(&text);
                let label = self.account_label(account_id);
                // Desktop-notify only genuine failures (not transient connectivity
                // blips that auto-recover), and only when unfocused — the in-app bar
                // already surfaces it while you're looking.
                if self.notifications_enabled && !connectivity && !self.window.is_active() {
                    crate::notify::error(account_id, &format!("{label}: mail error"), &text);
                }
                self.notifications.emit(NotifyInput::Push {
                    text: format!("{label}: {text}"),
                    error: true,
                    connectivity,
                });
            }

            AppMsg::NotifyCount(n) => self.notify_count = n,
            AppMsg::ToggleNotifications => self.notifications.emit(NotifyInput::TogglePanel),

            AppMsg::OpenContacts => {
                self.close_sidebar_peek();
                self.mirror_selection(crate::ui::sidebar::Sel::Contacts);
                self.showing_outbox = false;
                self.leave_gallery();
                self.showing_contacts = true;
                // Read EDS off the UI thread (SQLite + photo decoding); the
                // page shows its loading face until the list lands.
                self.contacts_page.emit(ContactsPageInput::SetLoading);
                let s = sender.clone();
                std::thread::spawn(move || {
                    let contacts = if demo_mode() {
                        crate::contacts::demo_contacts()
                    } else {
                        crate::contacts::read_contact_details()
                    };
                    s.input(AppMsg::ContactsLoaded(contacts));
                });
            }

            AppMsg::ContactsLoaded(contacts) => {
                self.contacts_page.emit(ContactsPageInput::SetContacts(contacts));
            }

            AppMsg::LaunchGnomeContacts => crate::ui::contacts_browser::launch_gnome_contacts(),

            AppMsg::SaveContact { book_uid, vcard } => {
                let s = sender.clone();
                std::thread::spawn(move || {
                    let result = crate::contacts::modify_contact(&book_uid, &vcard);
                    s.input(AppMsg::ContactWriteDone(result.err()));
                });
            }

            AppMsg::CreateContact(vcard) => {
                let s = sender.clone();
                std::thread::spawn(move || {
                    let result = match crate::contacts::writable_books().first() {
                        Some(book) => crate::contacts::create_contact(&book.uid, &vcard),
                        None => Err(i18n("No address book available")),
                    };
                    s.input(AppMsg::ContactWriteDone(result.err()));
                });
            }

            AppMsg::DeleteContact { book_uid, uid } => {
                let s = sender.clone();
                std::thread::spawn(move || {
                    let result = crate::contacts::delete_contact(&book_uid, &uid);
                    s.input(AppMsg::ContactWriteDone(result.err()));
                });
            }

            AppMsg::ContactWriteDone(err) => {
                if let Some(e) = err {
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n_f("Could not update contact: {e}", &[("e", &(e).to_string())]),
                        error: true,
                        connectivity: false,
                    });
                }
                // Success or not, re-read so the card shows what EDS holds.
                // A short pause lets EDS flush the write to its SQLite cache
                // (that is what the read goes through).
                let s = sender.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    let contacts = crate::contacts::read_contact_details();
                    s.input(AppMsg::ContactsLoaded(contacts));
                });
            }
        }
    }
}

/// How long a conversation waits for its outstanding bodies before painting what
/// it has. Long enough that a cached conversation — the common case — always
/// arrives whole, short enough that one unreachable message can't hold the
/// reader hostage.
const THREAD_BODY_WAIT: std::time::Duration = std::time::Duration::from_millis(1200);

/// How many messages' reply headers are held for joining conversations across
/// folders. Only a bound on memory and rebuild cost — never on correctness: a
/// conversation whose members are all on screen groups without consulting these
/// at all. They matter for the join that runs through a message that isn't
/// shown, most often your own reply in Sent. The newest are kept, since that is
/// where the mail being read lives.
const THREAD_LINK_LIMIT: usize = 20_000;



impl AppModel {
    /// The memory section of an exported log: the process tree as the
    /// kernel sees it, then what the main process is holding — the mail
    /// index (whose size follows the mailbox, not the session) apart from the
    /// session caches (which follow use). Written so a "Hylki is using N GB"
    /// report answers itself: a big index on a big mailbox is the accepted
    /// cost of keeping everything in RAM; a big cache is something to fix.
    fn memory_report(&self) -> String {
        use crate::memory_report::{human_bytes, human_count, messages_bytes};
        let count = |(n, b): (usize, usize)| format!("{} messages, {}", human_count(n), human_bytes(b as u64));
        let mut out = String::new();
        out.push_str("== Memory ==\n");
        if let Some(up) = crate::memory_report::uptime() {
            out.push_str(&format!("Running for {}\n", crate::memory_report::human_duration(up)));
        }
        out.push_str("Processes:\n");
        for line in crate::memory_report::process_lines() {
            out.push_str(&line);
            out.push('\n');
        }
        for line in crate::memory_report::web_view_lines() {
            out.push_str(&line);
            out.push('\n');
        }
        let (rust_live, rust_peak) = crate::memory_report::rust_heap();
        let (sql_live, sql_peak) = crate::memory_report::sqlite_heap();
        out.push_str(&format!(
            "  of which Rust code holds {} (peak {}); SQLite holds {} (peak {}); the rest is GTK, WebKit, the graphics driver and other libraries\n",
            human_bytes(rust_live as u64),
            human_bytes(rust_peak as u64),
            human_bytes(sql_live),
            human_bytes(sql_peak)
        ));
        // What GTK draws with. A software renderer path (lavapipe, llvmpipe)
        // runs LLVM inside this process: hundreds of megabytes of heap on a
        // VM or a machine whose GPU driver GTK could not use.
        let renderer = gtk::prelude::NativeExt::renderer(&self.window)
            .map(|r| r.type_().name().to_string())
            .unwrap_or_else(|| "none yet".to_string());
        let libs = crate::memory_report::graphics_libraries();
        // Mesa's Vulkan loader maps every installed driver to enumerate the
        // devices, so lavapipe (and with it LLVM) shows up on a machine that
        // draws with its GPU too; software rendering is the verdict only when
        // no hardware driver is there beside it.
        let hardware = libs.iter().any(|l| {
            let l = l.as_str();
            (l.starts_with("libvulkan_") && !l.starts_with("libvulkan_lvp") && !l.starts_with("libvulkan_dzn"))
                || l.contains("nvidia")
                || (l.ends_with("_dri.so") && !l.contains("swrast"))
        });
        let software = libs.iter().any(|l| {
            l.starts_with("libvulkan_lvp") || l.starts_with("libLLVM") || l.contains("swrast")
        });
        out.push_str(&format!(
            "Graphics: {renderer}; driver libraries: {}{}\n",
            if libs.is_empty() { "none loaded".to_string() } else { libs.join(", ") },
            match (hardware, software) {
                (false, true) => " (SOFTWARE rendering: shaders compile through LLVM in this process)",
                (true, true) => " (hardware driver in use; the software fallback is mapped alongside it, as Mesa's loader does)",
                _ => "",
            }
        ));

        out.push_str("Mail index (follows the mailbox size, kept in RAM by design):\n");
        let folders = self.message_cache.len();
        let index = messages_bytes(self.message_cache.values().flatten());
        out.push_str(&format!(
            "  {} accounts, {} folders indexed: {}\n",
            self.accounts.len(),
            human_count(folders),
            count(index)
        ));
        let unified = messages_bytes(self.unified_slices.values().flatten());
        out.push_str(&format!(
            "  unified slices ({} folders): {}\n",
            human_count(self.unified_slices.len()),
            count(unified)
        ));
        let tags = messages_bytes(self.tag_view_cache.values().flatten());
        out.push_str(&format!(
            "  tag views ({}): {}\n",
            human_count(self.tag_view_cache.len()),
            count(tags)
        ));
        let [all, shown, pool] = self.message_list.model().memory_stats();
        out.push_str(&format!(
            "  list: folder {}; rows {}; search pool {}\n",
            count(all),
            count(shown),
            count(pool)
        ));

        out.push_str("Session caches (follow use; each is bounded unless noted):\n");
        out.push_str(&format!(
            "  bodies: {} entries, {} of {} budget\n",
            human_count(self.body_cache.len()),
            human_bytes(self.body_cache.bytes() as u64),
            human_bytes(self.body_cache.budget() as u64)
        ));
        out.push_str(&format!(
            "  attachments: {} entries, {} of {} budget\n",
            human_count(self.attachment_cache.len()),
            human_bytes(self.attachment_cache.bytes() as u64),
            human_bytes(self.attachment_cache.budget() as u64)
        ));
        let threads = messages_bytes(self.thread_cache.values().flatten());
        out.push_str(&format!(
            "  conversations: {} of {} kept, {}; open conversation {}\n",
            self.thread_cache.len(),
            Self::THREAD_CACHE_MAX,
            count(threads),
            count(messages_bytes(&self.current_thread))
        ));
        let undo = messages_bytes(self.undo_stack.iter().chain(&self.redo_stack).flat_map(|e| &e.rows));
        out.push_str(&format!(
            "  undo/redo: {} entries, {}\n",
            human_count(self.undo_stack.len() + self.redo_stack.len()),
            count(undo)
        ));
        let carried = messages_bytes(self.carried_threads.values().flatten());
        let carried_bodies: usize = self.carried_bodies.values().map(String::len).sum();
        out.push_str(&format!(
            "  carried replies: {} threads ({}), {} bodies ({}) (unbounded)\n",
            human_count(self.carried_threads.len()),
            count(carried),
            human_count(self.carried_bodies.len()),
            human_bytes(carried_bodies as u64)
        ));
        out.push_str(&format!(
            "  sender checks: {} (unbounded); pop-outs: {}\n",
            human_count(self.sender_cache.len()),
            self.popouts.len()
        ));
        let (items, with_data, data_bytes) = self.gallery.model().memory_stats();
        let ((thumbs, thumbs_rendered, thumb_bytes), (pdfs, pdfs_rendered, pdf_bytes)) =
            crate::ui::attachments_gallery::cache_stats();
        out.push_str(&format!(
            "  gallery: {} items listed, {} with file bytes ({}); thumbnails {} ({} rendered, {} of pixels), PDF pages {} ({} rendered, {} of pixels) (unbounded)\n",
            human_count(items),
            human_count(with_data),
            human_bytes(data_bytes),
            human_count(thumbs),
            human_count(thumbs_rendered),
            human_bytes(thumb_bytes),
            human_count(pdfs),
            human_count(pdfs_rendered),
            human_bytes(pdf_bytes)
        ));
        let (logos, logo_bytes, logo_misses) = crate::logo::cache_stats();
        out.push_str(&format!(
            "  sender logos: {} ({} of pixels), {} domains without (unbounded)\n",
            human_count(logos),
            human_bytes(logo_bytes),
            human_count(logo_misses)
        ));
        let (contacts, contact_bytes, gravatars, gravatar_bytes) = crate::avatar::cache_stats();
        out.push_str(&format!(
            "  contact photos: {} ({} of pixels); gravatars: {} ({} of pixels) (unbounded)\n",
            human_count(contacts),
            human_bytes(contact_bytes),
            human_count(gravatars),
            human_bytes(gravatar_bytes)
        ));
        let (pictures, picture_bytes, circles, circle_bytes) = crate::ui::initials::cache_stats();
        out.push_str(&format!(
            "  account pictures: {} ({} of pixels); rendered circles: {} ({}) (unbounded)\n",
            human_count(pictures),
            human_bytes(picture_bytes),
            human_count(circles),
            human_bytes(circle_bytes as u64)
        ));
        let (lines, line_bytes) = crate::console_log::stats();
        out.push_str(&format!(
            "  console: {} lines, {}\n",
            human_count(lines),
            human_bytes(line_bytes as u64)
        ));
        out
    }

    /// Push the date and clock preference into the formatter and redraw whatever
    /// shows a date: every row carries one, as does the open message.
    fn apply_date_style(&self) {
        crate::datefmt::set_style(self.date_style, self.clock_style);
        self.message_list.emit(MessageListInput::RefreshDates);
        let current = self.current.clone();
        self.show_message(current, false);
    }

    /// Persist all app settings together.
    /// (Re)build the burger menu's help section: the Console entry appears
    /// only while Console mode is enabled in Settings.
    fn rebuild_help_menu(&self) {
        self.help_menu.remove_all();
        self.help_menu.append(Some(i18n("Reveal Status Bar").as_str()), Some("win.status-bar"));
        if self.console_mode {
            self.help_menu.append(Some(i18n("Console").as_str()), Some("win.console"));
        }
        self.help_menu.append(Some(i18n("Keyboard Shortcuts").as_str()), Some("win.shortcuts"));
        self.help_menu
            .append(Some(format!("{} {}", i18n("About"), crate::APP_NAME).as_str()), Some("win.about"));
    }

    /// The reader's font-and-colour override (#56) as the reader applies it:
    /// the font only when the switch is on, the interface font standing in
    /// where none was chosen.
    fn reader_style(&self) -> config::ReaderStyle {
        let font = self.override_fonts.then(|| {
            if self.reader_font.trim().is_empty() {
                interface_font()
            } else {
                self.reader_font.clone()
            }
        });
        let plain_font = self.plain_monospace.then(|| {
            if self.plain_font.trim().is_empty() {
                crate::desktop::monospace_font()
            } else {
                self.plain_font.clone()
            }
        });
        config::ReaderStyle { font, colors: self.override_colors, plain_font }
    }

    /// Hand the current reader style to the reader and every popped-out window.
    fn push_reader_style(&self) {
        let style = self.reader_style();
        self.message_view.emit(MessageViewInput::SetReaderStyle(style.clone()));
        for p in self.popouts.values() {
            p.controller.emit(MessageWindowInput::SetReaderStyle(style.clone()));
        }
    }

    fn save_settings(&self) {
        config::save_privacy(
            &self.allowed_senders,
            self.auto_remote_content,
            self.gravatar,
            self.avatars,
            self.own_mailbox_face,
            self.sender_logos,
            self.date_style,
            self.clock_style,
            self.fetch_interval_secs,
            self.push,
            &self.blacklist,
            self.palette_collapse_secs,
            self.card_palette_collapse_secs,
            self.threading,
            self.threads_expanded,
            self.thread_expansion,
            self.thread_newest_first,
            self.always_show_recipients,
            self.single_message_card,
            self.reader_mode,
            self.reader_switch,
            self.reader_default,
            self.zoom_default,
            self.card_attachments,
            self.drawer_enabled,
            self.confirm_thread_delete,
            self.message_theme,
            self.override_fonts,
            self.reader_font.clone(),
            self.override_colors,
            self.plain_monospace,
            self.plain_font.clone(),
            self.notifications_enabled,
            self.notification_content,
            self.notification_buttons,
            self.show_attachments,
            self.show_contacts,
            self.settings_open_accounts,
            self.card_actions_hover,
            self.card_actions_auto,
            self.list_palette,
            self.list_palette_hover,
            self.card_palette_menu,
            self.swipe_enabled,
            self.swipe_reversed,
            self.swipe_sensitivity,
            self.compose_inline,
            self.reply_fields,
            &self.compose_default_from,
            self.paste_plain,
            self.compose_format,
            self.reply_position,
            self.signature_position,
            self.spellcheck,
            self.spellcheck_langs.clone(),
            self.preview_lines,
            self.single_key.get(),
            self.run_in_background.get(),
            self.autostart,
            self.tray_enabled,
            self.tray_icon,
            self.tray_mail,
            self.show_remote_banner,
            self.sidebar_hover_expand,
            self.remember_sidebar,
            self.remember_rail,
            self.rail_dots,
            self.rail_fold,
            self.app_theme,
            self.theme.clone(),
            self.show_unified_pref,
            self.unified_chips,
            self.unified_filtered,
            self.unified_kinds,
            self.unified_tags,
            self.show_accounts,
            self.filtered_placement,
            self.tags_placement,
            self.chevrons_left,
            self.console_mode,
            self.read_mark,
            self.files_prefs,
            self.link_browser.clone(),
        );
    }

    /// Carry out a single-key shortcut.
    fn run_shortcut(&mut self, action: Shortcut, sender: &ComponentSender<Self>) {
        match action {
            Shortcut::NextMessage => self.message_list.emit(MessageListInput::MoveSelection(1)),
            Shortcut::PrevMessage => self.message_list.emit(MessageListInput::MoveSelection(-1)),
            Shortcut::ToggleSelect => self.message_list.emit(MessageListInput::ToggleSelection),
            Shortcut::BackToList => self.message_list.emit(MessageListInput::FocusList),
            Shortcut::Search => self.message_list.emit(MessageListInput::FocusSearch),
            Shortcut::OpenMessage => {
                if let Some(view) = self.message_view.widget().first_child() {
                    view.grab_focus();
                }
            }
            Shortcut::NextInThread => self.step_thread(1),
            Shortcut::PrevInThread => self.step_thread(-1),
            Shortcut::Reply => sender.input(AppMsg::Reply),
            Shortcut::ReplyAll => sender.input(AppMsg::ReplyAll),
            Shortcut::Forward => sender.input(AppMsg::Forward),
            Shortcut::Archive => sender.input(AppMsg::Archive),
            Shortcut::Delete => sender.input(AppMsg::Delete),
            Shortcut::Spam => sender.input(AppMsg::MarkSpam),
            Shortcut::Star => sender.input(AppMsg::ToggleStar),
            Shortcut::ToggleRead => {
                if let Some(m) = self.current.clone() {
                    let read = m.unread;
                    self.user_set_flags(read_label(read), &[m], FlagKind::Read, read);
                }
            }
            Shortcut::Compose => sender.input(AppMsg::Compose),
            Shortcut::Shortcuts => self.show_shortcuts(),
            Shortcut::Tag(n) => {
                // Settings order, first nine. A number past the list does
                // nothing rather than something surprising.
                if let Some(tag) = self.tags.get(usize::from(n).saturating_sub(1)) {
                    sender.input(AppMsg::ToggleTagCurrent(tag.keyword.clone()));
                }
            }
            Shortcut::ClearTags => {
                // Only configured tags come off: a message's other keywords
                // ($Forwarded, a client's own flags) are not ours to drop.
                // Re-read the message between removals, since set_tag
                // patches the copy the next call will read.
                let keywords: Vec<String> = self
                    .reply_target()
                    .map(|m| {
                        self.tags
                            .iter()
                            .filter(|t| m.has_keyword(&t.keyword))
                            .map(|t| t.keyword.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                let mut changes = Vec::new();
                let mut account_id = 0;
                for keyword in keywords {
                    if let Some(m) = self.reply_target() {
                        account_id = m.account_id;
                        changes.push(FlagChange {
                            kind: FlagKind::Tag(keyword.clone()),
                            // The step is the reversal: undo puts them back.
                            value: true,
                            items: vec![FlagRef { folder_id: m.folder_id, uid: m.uid, id: m.id }],
                        });
                        self.set_tag(&m, &keyword, false);
                    }
                }
                // One entry, so a single Ctrl+Z restores the whole set rather
                // than giving back one tag per press.
                if !changes.is_empty() {
                    self.push_undo(account_id, i18n("Remove Tags"), UndoStep::Flags(changes));
                }
            }
        }
    }

    /// Move to the next (or previous) message of the open conversation.
    fn step_thread(&mut self, delta: i32) {
        let Some(current) = self.current.clone() else {
            return;
        };
        let thread = self.current_thread.clone();
        let Some(index) = thread
            .iter()
            .position(|m| m.id == current.id && m.account_id == current.account_id)
        else {
            // Not in a conversation: fall back to moving through the list, which
            // is what someone pressing "next" almost certainly meant.
            self.message_list.emit(MessageListInput::MoveSelection(delta));
            return;
        };
        let next = index as i32 + delta;
        if next < 0 || next as usize >= thread.len() {
            return;
        }
        let target = &thread[next as usize];
        self.message_list
            .emit(MessageListInput::SelectAndLoad((target.account_id, target.id)));
    }

    /// Open the keyboard-shortcut reference, or close it if it is already up:
    /// the key that summons a cheatsheet is the obvious one to dismiss it with.
    fn show_shortcuts(&mut self) {
        if let Some(win) = self.shortcuts_win.take() {
            if win.is_visible() {
                win.close();
                return;
            }
            // Closed from its own titlebar; fall through and open a fresh one.
        }
        self.shortcuts_win = Some(self.build_shortcuts_window());
    }

    /// A plain window listing every single-key shortcut.
    fn build_shortcuts_window(&self) -> adw::Window {
        let win = adw::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title(&i18n("Keyboard Shortcuts"))
            .default_width(420)
            .default_height(560)
            .build();

        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.set_margin_top(12);
        page.set_margin_bottom(18);
        page.set_margin_start(18);
        page.set_margin_end(18);

        if !self.single_key.get() {
            let off = gtk::Label::new(Some(
                i18n("Single-key shortcuts are switched off. Turn them on in Settings → System & Appearance.").as_str(),
            ));
            off.add_css_class("dim-label");
            off.set_wrap(true);
            off.set_xalign(0.0);
            off.set_margin_bottom(12);
            page.append(&off);
        }

        for (section, keys) in SHORTCUT_HELP {
            let title = gtk::Label::new(Some(i18n(section).as_str()));
            title.add_css_class("heading");
            title.set_halign(gtk::Align::Start);
            title.set_margin_top(14);
            title.set_margin_bottom(6);
            page.append(&title);

            let list = gtk::ListBox::new();
            list.add_css_class("boxed-list");
            list.set_selection_mode(gtk::SelectionMode::None);
            for (key, what) in *keys {
                let row = adw::ActionRow::builder().title(i18n(what)).build();
                let label = gtk::Label::new(Some(key));
                label.add_css_class("shortcut-key");
                label.set_valign(gtk::Align::Center);
                row.add_suffix(&label);
                list.append(&row);
            }
            page.append(&list);
        }

        // Which number is which tag (#157): the first nine, in Settings
        // order, each with its swatch. Only when there are tags to list.
        if !self.tags.is_empty() {
            let title = gtk::Label::new(Some(i18n("Your tags").as_str()));
            title.add_css_class("heading");
            title.set_halign(gtk::Align::Start);
            title.set_margin_top(14);
            title.set_margin_bottom(6);
            page.append(&title);
            let list = gtk::ListBox::new();
            list.add_css_class("boxed-list");
            list.set_selection_mode(gtk::SelectionMode::None);
            for (i, tag) in self.tags.iter().take(9).enumerate() {
                let row = adw::ActionRow::builder()
                    .title(gtk::glib::markup_escape_text(&tag.name))
                    .build();
                row.add_prefix(&crate::ui::context_menu::swatch_widget(&tag.color, true));
                let label = gtk::Label::new(Some(&(i + 1).to_string()));
                label.add_css_class("shortcut-key");
                label.set_valign(gtk::Align::Center);
                row.add_suffix(&label);
                list.append(&row);
            }
            page.append(&list);
        }

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&page)
            .build();
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&adw::HeaderBar::new());
        tv.set_content(Some(&scroller));
        win.set_content(Some(&tv));

        // Escape closes it, as does the accelerator that opened it. A reference
        // you can't dismiss with the key you opened it with is a nuisance.
        let keys = gtk::EventControllerKey::new();
        let closer = win.clone();
        keys.connect_key_pressed(move |_, keyval, _, state| {
            let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let toggles = ctrl
                && matches!(keyval, gtk::gdk::Key::question | gtk::gdk::Key::slash);
            if keyval == gtk::gdk::Key::Escape || toggles || keyval == gtk::gdk::Key::F1 {
                closer.close();
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        win.add_controller(keys);

        win.present();
        win
    }

    /// Push the merged queue to the Outbox view and the sidebar's badge. Oldest
    /// first, which is the order the workers send in.
    fn push_outbox(&self) {
        let items = self.outbox_items();
        self.sidebars_emit(SidebarInput::SetOutboxCount(items.len() as u32));
        if self.showing_outbox {
            self.message_list.emit(MessageListInput::SetMessages {
                messages: items.iter().map(|i| i.as_message()).collect(),
            });
        }
    }

    /// Every account's queue merged, oldest first — the order they will send in.
    fn outbox_items(&self) -> Vec<crate::models::OutboxItem> {
        let mut items: Vec<crate::models::OutboxItem> = self
            .outbox_by_account
            .values()
            .flatten()
            .cloned()
            .collect();
        items.sort_by_key(|i| (i.queued_at, i.id));
        items
    }

    /// The queued message behind a list row, if the Outbox is what's shown.
    fn outbox_item(&self, account_id: u32, id: u32) -> Option<crate::models::OutboxItem> {
        if !self.showing_outbox {
            return None;
        }
        self.outbox_by_account
            .get(&account_id)?
            .iter()
            .find(|i| i.id == id)
            .cloned()
    }

    /// Tell the message list whether the folder(s) currently shown are fully
    /// indexed, so it knows whether to expect more rows while scrolling.
    fn push_index_complete(&self) {
        self.message_list
            .emit(MessageListInput::SetIndexComplete(self.current_index_complete()));
    }

    fn current_index_complete(&self) -> bool {
        if self.unified {
            self.unified_targets()
                .iter()
                .all(|(a, f, _)| self.indexed_folders.contains(&(*a, *f)))
        } else if let Some(sel) = &self.selected {
            self.indexed_folders.contains(&(sel.account_id, sel.folder_id))
        } else {
            true
        }
    }

    /// (Re)arm the repeating auto-fetch timer to the current interval.
    fn arm_auto_fetch(&mut self, sender: &ComponentSender<Self>) {
        if let Some(id) = self.auto_fetch_source.take() {
            id.remove();
        }
        if self.fetch_interval_secs > 0 {
            let input = sender.input_sender().clone();
            let secs = self.fetch_interval_secs.min(u32::MAX as u64) as u32;
            let id = gtk::glib::timeout_add_seconds_local(secs, move || {
                let _ = input.send(AppMsg::Refresh);
                gtk::glib::ControlFlow::Continue
            });
            self.auto_fetch_source = Some(id);
        }
    }

    /// Send a request to a specific account's worker.
    fn send_to(&self, account_id: u32, req: MailRequest) {
        if let Some(worker) = self.workers.get(&account_id) {
            let _ = worker.send(req);
        }
    }

    /// The account to act on by default (selected folder's account, else first).
    fn active_account(&self) -> u32 {
        self.selected
            .as_ref()
            .map(|s| s.account_id)
            .or_else(|| self.accounts.first().map(|a| a.id))
            .unwrap_or(1)
    }

    /// The account and identity a NEW message opens from (#157). With a
    /// default sender chosen in Settings → Composing, that identity — an
    /// account's own address or one of its aliases — as long as its account
    /// is still enabled; otherwise `fallback` (the open folder's or open
    /// message's account, as before) from its own address. Replies never
    /// come through here: they answer from the address written to.
    fn new_message_from(&self, fallback: u32, mut prefill: ComposePrefill) -> (u32, ComposePrefill) {
        let want = self.compose_default_from.trim();
        if want.is_empty() {
            return (fallback, prefill);
        }
        let owner = self.config.iter().enumerate().find(|(_, c)| {
            c.enabled
                && (c.email.eq_ignore_ascii_case(want)
                    || c.aliases.iter().any(|al| {
                        crate::config::split_identity(&al.identity).1.eq_ignore_ascii_case(want)
                    }))
        });
        match owner {
            Some((idx, _)) => {
                prefill.from_address = want.to_string();
                (idx as u32 + 1, prefill)
            }
            None => (fallback, prefill),
        }
    }

    /// Spawn one worker per configured account (or a single mock worker when no
    /// account is configured).
    /// Paint the mail panes straight from the disk cache, before any worker
    /// has spoken: every enabled account's folder list and inbox slice is
    /// loaded synchronously at startup, so "All Inboxes" (the launch view) is
    /// full the instant the window appears. The workers' own cache-first
    /// loads and syncs then replace each slice with whatever changed since
    /// the app last ran.
    fn prime_from_cache(&mut self) {
        let Ok(cache) = crate::cache::Cache::open() else { return };
        for (i, c) in self.config.iter().enumerate() {
            if !c.enabled {
                continue;
            }
            let account_id = (i + 1) as u32;
            let folders = cache.load_folders(account_id);
            if folders.is_empty() {
                continue;
            }
            if let Some(inbox) = folders.iter().find(|f| f.kind == FolderKind::Inbox) {
                let messages = cache.load_messages(account_id, &inbox.path, inbox.id);
                if !messages.is_empty() {
                    self.message_cache.insert((account_id, inbox.id), messages.clone());
                    self.unified_slices.insert((account_id, inbox.id), messages);
                }
            }
            // The folders counting filters file into (#116) as well, so the
            // tray menu can list their unread mail before any sync.
            let counted = folders.iter().filter(|f| {
                !matches!(f.kind, FolderKind::Inbox | FolderKind::Trash | FolderKind::Junk)
                    && self.filters.iter().any(|r| {
                        r.count_unread
                            && r.account_email.eq_ignore_ascii_case(&c.email)
                            && r.dest_path == f.path
                    })
            });
            for f in counted {
                let messages = cache.load_messages(account_id, &f.path, f.id);
                if !messages.is_empty() {
                    self.message_cache.insert((account_id, f.id), messages);
                }
            }
            for f in &folders {
                if f.unread > 0 {
                    self.folder_unread.insert((account_id, f.id), f.unread);
                }
            }
            self.folders.insert(account_id, folders);
        }
    }

    fn spawn_workers(&mut self, sender: &ComponentSender<Self>) {
        self.workers.clear();
        // account_id is the config index + 1 (a load-bearing invariant), so we keep
        // every account's slot but only spawn a worker for enabled ones — disabled
        // accounts simply have no worker (no sync, no sidebar presence). With no
        // accounts configured, the app is blank — the sample/demo data only appears
        // when explicitly requested via HYLKI_DEMO (so removing all real accounts
        // doesn't fall back to fake content).
        if self.config.is_empty() {
            if demo_mode() {
                for account_id in [1, 2, 3] {
                    self.workers.insert(account_id, Self::spawn_worker(account_id, None, sender));
                }
            }
        } else {
            for (i, account) in self.config.iter().enumerate() {
                if !account.enabled {
                    continue;
                }
                let account_id = i as u32 + 1;
                let worker = Self::spawn_worker(account_id, Some(account.clone()), sender);
                self.workers.insert(account_id, worker);
                // Anything left queued by a previous run has to show up now, not
                // only after the next failed send.
                self.send_to(account_id, MailRequest::LoadOutbox);
            }
        }
    }

    fn spawn_worker(
        account_id: u32,
        account: Option<AccountConfig>,
        sender: &ComponentSender<Self>,
    ) -> UnboundedSender<MailRequest> {
        let input = sender.input_sender().clone();
        worker::spawn(account_id, account, move |event| {
            let _ = input.send(map_event(account_id, event));
        })
    }

    /// Tear down all workers and reconnect from the current config.
    fn reconnect_all(&mut self, sender: &ComponentSender<Self>) {
        self.accounts.clear();
        self.folders.clear();
        self.selected = None;
        self.unified = false;
        self.tag_view = None;
        self.unified_slices.clear();
        self.message_cache.clear();
        self.body_cache.clear();
        self.attachments.clear();
        self.sync_attachment_drawer();
        self.attachments_loading = false;
        self.attachment_cache.clear();
        self.current = None;
        self.busy.clear();
        self.update_busy_indicator();
        self.show_message(None, false);
        self.message_list.emit(MessageListInput::SetLoading);
        self.rebuild_sidebar();
        self.spawn_workers(sender);
    }

    /// Resolved avatar/accent colour for an account (custom, else auto accent).
    fn account_color(&self, account_id: u32) -> String {
        self.effective_config()
            .get(account_id.saturating_sub(1) as usize)
            .and_then(|c| c.color.clone())
            .or_else(|| {
                self.accounts
                    .iter()
                    .find(|a| a.id == account_id)
                    .map(|a| a.accent.clone())
            })
            .unwrap_or_else(|| "#3584e4".to_string())
    }

    /// Spin the header's Refresh while any account syncs (the rail's own
    /// refresh button mirrors this via `SidebarInput::SetBusy`).
    fn set_header_refresh_busy(&self, busy: bool) {
        let face = if busy { "spinner" } else { "icon" };
        self.sidebar_refresh_spinner.set_spinning(busy);
        self.sidebar_refresh_stack.set_visible_child_name(face);
        self.peek_refresh_spinner.set_spinning(busy);
        self.peek_refresh_stack.set_visible_child_name(face);
    }

    /// Spin the refresh button (header and rail) while anything is happening
    /// in the background — an account sync, or bulk moves/deletions still
    /// being applied on the server. The at-a-glance "working on something"
    /// signal; the status bar has the words.
    fn update_busy_indicator(&self) {
        let busy = !self.busy.is_empty() || self.bulk_pending > 0;
        self.sidebars_emit(SidebarInput::SetBusy(busy));
        self.set_header_refresh_busy(busy);
    }

    /// An account's avatar picture (#162), when one is set and its file is
    /// still there.
    /// The configs the UI describes accounts from: the real ones, or the
    /// demo's in-memory stand-ins while no account is configured.
    fn effective_config(&self) -> &[AccountConfig] {
        if self.config.is_empty() && demo_mode() {
            &self.demo_config
        } else {
            &self.config
        }
    }

    fn account_avatar(&self, account_id: u32) -> Option<std::path::PathBuf> {
        self.effective_config()
            .get(account_id.saturating_sub(1) as usize)
            .and_then(|c| c.avatar.as_deref())
            .and_then(config::avatar_path)
    }

    /// Custom avatar emoji for an account, if set.
    fn account_emoji(&self, account_id: u32) -> Option<String> {
        // In demo mode the sample accounts' emoji come from the stand-in
        // configs, which the Accounts panel can edit.
        self.effective_config()
            .get(account_id.saturating_sub(1) as usize)
            .and_then(|c| c.emoji.clone())
    }

    /// Tell the face chain which addresses are the user's own, and what their
    /// accounts show for them (#189): a mailbox given a picture wears it on
    /// the mail it sent too — in a conversation's cards and in the list — and
    /// not only in the sidebar. Refreshed with the sidebar, which is rebuilt
    /// whenever the accounts change.
    fn refresh_own_faces(&self) {
        crate::avatar::set_faces_on_own_mail(self.own_mailbox_face);
        let mut faces: Vec<(String, crate::avatar::OwnFace)> = Vec::new();
        for (i, cfg) in self.effective_config().iter().enumerate() {
            let face = crate::avatar::OwnFace {
                gravatar: cfg.gravatar,
                picture: cfg.avatar.as_deref().and_then(config::avatar_path),
                emoji: cfg.emoji.clone().filter(|e| !e.trim().is_empty()),
                color: self.account_color(i as u32 + 1),
            };
            // An account showing its initials has nothing to add: the circle
            // its own messages already get is drawn from the same letters.
            if !face.is_set() {
                continue;
            }
            // Mail sent as a send-as alias (#34) is still from this mailbox.
            let addresses = std::iter::once(cfg.email.clone())
                .chain(cfg.aliases.iter().map(|al| config::split_identity(&al.identity).1));
            faces.extend(
                addresses
                    .filter(|address| !address.trim().is_empty())
                    .map(|address| (address, face.clone())),
            );
        }
        crate::avatar::set_own_faces(faces);
    }

    /// Redraw the faces on screen after what a mailbox shows has changed
    /// (#189): the list rebuilds its rows, and the reader — in the pane and
    /// in any popped-out window — its cards.
    fn refresh_faces(&self) {
        self.message_list.emit(MessageListInput::ContactPhotosChanged);
        self.message_view.emit(MessageViewInput::FacesChanged);
        for p in self.popouts.values() {
            p.controller.emit(MessageWindowInput::FacesChanged);
        }
    }

    /// Look up the Gravatars the accounts asked for (#189), one request per
    /// address per session, off the main thread. Done here rather than where
    /// a circle is drawn so every view — the sidebar, the list, an open
    /// conversation — is holding the same answer.
    fn warm_own_gravatars(&self, sender: &ComponentSender<Self>) {
        for cfg in self.effective_config() {
            if !cfg.gravatar {
                continue;
            }
            // An alias is an address of its own, and may have a Gravatar of
            // its own; each is asked about separately.
            let addresses = std::iter::once(cfg.email.clone())
                .chain(cfg.aliases.iter().map(|al| config::split_identity(&al.identity).1));
            for address in addresses.filter(|a| !a.trim().is_empty()) {
                if !crate::avatar::wants_own_gravatar(&address) {
                    continue;
                }
                let back = sender.input_sender().clone();
                std::thread::spawn(move || {
                    let outcome = crate::avatar::fetch_own_gravatar(&address);
                    let _ = back.send(AppMsg::OwnGravatarFetched { email: address, outcome });
                });
            }
        }
    }

    /// A label for an account in messages (name, else email, else "Account N").
    /// Uses config (available even before the account connects), then live data.
    fn account_label(&self, account_id: u32) -> String {
        let pick = |name: &str, email: &str| -> Option<String> {
            if !name.trim().is_empty() {
                Some(name.to_string())
            } else if !email.trim().is_empty() {
                Some(email.to_string())
            } else {
                None
            }
        };
        self.config
            .get(account_id.saturating_sub(1) as usize)
            .and_then(|c| pick(&c.name, &c.email))
            .or_else(|| {
                self.accounts
                    .iter()
                    .find(|a| a.id == account_id)
                    .and_then(|a| pick(&a.name, &a.email))
            })
            .unwrap_or_else(|| format!("Account {account_id}"))
    }

    /// Display name for an account (name, else email).
    fn account_name(&self, account_id: u32) -> String {
        // The account's UI label (how it's shown in All Inboxes / the reader chip).
        self.accounts
            .iter()
            .find(|a| a.id == account_id)
            .map(|a| a.label.clone())
            .unwrap_or_default()
    }

    /// Record a just-issued move so Ctrl+Z can bring it back. Skips silently
    /// when nothing identifies the messages (no Message-ID) — an unrecordable
    /// move simply isn't undoable.
    fn push_undo_move(
        &mut self,
        account_id: u32,
        moved_to: &str,
        restore_to: &str,
        rows: Vec<Message>,
    ) {
        let what = self.move_label(account_id, moved_to);
        self.push_undo_move_as(account_id, what, moved_to, restore_to, rows);
    }

    /// [`push_undo_move`], for the moves whose destination doesn't name them:
    /// Not Spam puts mail back in the Inbox, which is not "Move to Inbox".
    fn push_undo_move_as(
        &mut self,
        account_id: u32,
        what: String,
        moved_to: &str,
        restore_to: &str,
        rows: Vec<Message>,
    ) {
        // A message with no Message-ID can't be found again on the server, so
        // it can't be part of the step — and a step of nothing isn't undoable.
        let rows: Vec<Message> = rows.into_iter().filter(|m| !m.message_id.is_empty()).collect();
        if rows.is_empty() {
            return;
        }
        let ids: Vec<String> = rows.iter().map(|m| m.message_id.clone()).collect();
        // Keep each row's body with it. The reader renders from the message's
        // own body before it looks anywhere else, so a restored row opens
        // instantly instead of asking the server for a UID the move has
        // already retired.
        let rows: Vec<Message> = rows
            .into_iter()
            .map(|mut m| {
                if m.body.is_empty() {
                    if let Some(body) = self.body_cache.get(&(account_id, m.id)) {
                        m.body = body.clone();
                    }
                }
                m
            })
            .collect();
        let threads: Vec<(String, Vec<Message>)> = rows
            .iter()
            .filter_map(|m| {
                self.thread_cache
                    .get(&(account_id, m.id))
                    .map(|t| (m.message_id.clone(), t.clone()))
            })
            .collect();
        self.push_undo_entry(UndoEntry {
            account_id,
            what,
            step: UndoStep::Move {
                from: moved_to.to_string(),
                to: restore_to.to_string(),
                message_ids: ids,
            },
            at: std::time::Instant::now(),
            rows,
            threads,
            rows_return: true,
        });
    }

    /// What to call a move in the Undo menu, taken from where it landed:
    /// the named actions where the destination has a role, "Move to X"
    /// otherwise.
    fn move_label(&self, account_id: u32, dest: &str) -> String {
        let folder = self
            .folders
            .get(&account_id)
            .and_then(|fs| fs.iter().find(|f| f.path == dest));
        match folder.map(|f| f.kind) {
            Some(FolderKind::Trash) => i18n("Delete"),
            Some(FolderKind::Archive) => i18n("Archive"),
            Some(FolderKind::Junk) => i18n("Mark as Spam"),
            _ => {
                let name = folder.map(|f| f.name.clone()).unwrap_or_else(|| dest.to_string());
                i18n_f("Move to {name}", &[("name", &name)])
            }
        }
    }

    /// The same for a tag, named as the user knows it.
    fn tag_label(&self, keyword: &str, add: bool) -> String {
        let name = self
            .tags
            .iter()
            .find(|t| t.keyword.eq_ignore_ascii_case(keyword))
            .map(|t| t.name.clone())
            .unwrap_or_else(|| keyword.to_string());
        if add {
            i18n_f("Add Tag {name}", &[("name", &name)])
        } else {
            i18n_f("Remove Tag {name}", &[("name", &name)])
        }
    }

    /// Record one reversible action (#200). The step is what *undoes* it.
    /// A new action ends whatever redo branch was open, as undo histories do.
    fn push_undo(&mut self, account_id: u32, what: String, step: UndoStep) {
        self.push_undo_entry(UndoEntry {
            account_id,
            what,
            step,
            at: std::time::Instant::now(),
            rows: Vec::new(),
            threads: Vec::new(),
            rows_return: true,
        });
    }

    fn push_undo_entry(&mut self, entry: UndoEntry) {
        self.undo_stack.push(entry);
        self.redo_stack.clear();
        self.refresh_undo_menu();
    }

    /// The inline composer, while it holds keyboard focus: whoever the
    /// window's Undo and Redo belong to at this moment.
    /// Who the window's Undo and Redo entries belong to. Not the same
    /// question as [`AppModel::focused_compose`], and deliberately not
    /// decided by focus: opening the menu is itself a focus change, and
    /// whether it lands on the button, inside the popover, or somewhere in
    /// between is GTK's business and varies with how the menu was opened.
    /// An entry that reads correctly only until you reach for it is no use.
    ///
    /// So: an open inline composer with something in its history owns them.
    /// Anything it can take back is more recent than anything in the mail
    /// history — it is being written now — and while it has nothing the
    /// entries go back to the mail, which is what someone with an untouched
    /// reply on screen would expect. The keys keep following focus, because
    /// Ctrl+Z in the body must undo the body.
    fn menu_compose(&self) -> Option<&ReaderCompose> {
        self.reader_compose.as_ref().filter(|r| {
            r.window.is_none()
                && (self.compose_history.0.is_some() || self.compose_history.1.is_some())
        })
    }

    /// Ctrl+Z / Ctrl+Shift+Z: take the top of one stack, apply it, and put
    /// its inverse on the other so the move can be made again.
    ///
    /// Typing into the inline composer hands both over to it: its history
    /// covers the body and its attachments, and nobody writing a reply means
    /// "archive that message again" by Ctrl+Z. The menu entries follow the
    /// same rule, so what they say is always what the key would do.
    fn undo_redo(&mut self, redo: bool) {
        if let Some(compose) = self.menu_compose() {
            compose.controller.emit(ComposeInput::History { redo });
            return;
        }
        let entry = if redo { self.redo_stack.pop() } else { self.undo_stack.pop() };
        // Nothing to say when there is nothing to do: the menu entry is
        // already greyed out, and the key should just be inert (#200).
        let Some(entry) = entry else { return };
        let back = UndoEntry {
            account_id: entry.account_id,
            what: entry.what.clone(),
            step: entry.step.inverse(),
            // The window restarts: whatever the state is now, it is current.
            at: std::time::Instant::now(),
            rows: entry.rows.clone(),
            threads: entry.threads.clone(),
            rows_return: !entry.rows_return,
        };
        // Hold on to the bodies whatever happens: the move is about to change
        // these messages' UIDs, and the reload that follows would otherwise
        // find no body for them and go back to the server for one.
        if entry.rows_return {
            if self.carried_bodies.len() > CARRIED_BODY_LIMIT {
                self.carried_bodies.clear();
            }
            for row in entry.rows.iter().filter(|m| !m.body.is_empty()) {
                self.carried_bodies
                    .insert((entry.account_id, row.message_id.clone()), row.body.clone());
            }
            for (message_id, thread) in &entry.threads {
                self.carried_threads
                    .insert((entry.account_id, message_id.clone()), thread.clone());
            }
            tracing::debug!(
                target: "hylki::undo",
                "carrying {} bodies and {} conversations over the move",
                entry.rows.iter().filter(|m| !m.body.is_empty()).count(),
                entry.threads.len(),
            );
        }
        // Repaint the list now rather than a few round trips from now, while
        // the rows we took off it are still known to be what is there (#200).
        // The server request still goes out; its reload lands behind this and
        // replaces these rows with the real ones, UIDs and all.
        if entry.at.elapsed() <= INSTANT_UNDO && !entry.rows.is_empty() {
            self.instant_move(&entry);
        }
        // A step that could not be carried out is dropped rather than put
        // back: leaving it on top would jam the key on something that will
        // fail again every time. The entries under it are usually about other
        // messages entirely, so the rest of the history stands.
        if self.apply_undo_step(entry.account_id, &entry.step) {
            if redo {
                self.undo_stack.push(back);
            } else {
                self.redo_stack.push(back);
            }
        }
        self.refresh_undo_menu();
    }

    /// Put a move's rows back on screen (or take them away again), without
    /// waiting for the server (#200). Only ever called inside the
    /// [`INSTANT_UNDO`] window, where the rows are still the truth.
    fn instant_move(&mut self, entry: &UndoEntry) {
        let UndoStep::Move { from, to, .. } = &entry.step else { return };
        let account_id = entry.account_id;
        // The folder the rows are in right now: where the step will put them
        // when it is returning them, and where it will take them from when it
        // is not.
        let here = if entry.rows_return { to } else { from };
        let Some(folder_id) = self
            .folders
            .get(&account_id)
            .and_then(|fs| fs.iter().find(|f| &f.path == here))
            .map(|f| f.id)
        else {
            return;
        };
        if !entry.rows_return {
            // Redoing the move: take the rows off the list again, using the
            // copies the caches hold now — a reload may have landed since,
            // and those carry the UIDs the next action will need.
            for row in &entry.rows {
                let current = self
                    .message_cache
                    .get(&(account_id, folder_id))
                    .and_then(|ms| ms.iter().find(|c| c.message_id == row.message_id).cloned());
                if let Some(m) = current {
                    self.discard_message(&m);
                }
            }
            return;
        }
        let known: std::collections::HashSet<String> = self
            .message_cache
            .get(&(account_id, folder_id))
            .map(|ms| ms.iter().map(|m| m.message_id.clone()).collect())
            .unwrap_or_default();
        let mut added = false;
        for row in &entry.rows {
            // The reload may have beaten us to it, or the user may have
            // pressed twice; either way the row is already there.
            if known.contains(&row.message_id) {
                continue;
            }
            let mut m = row.clone();
            // Folder ids are positional and shift as folders come and go.
            m.folder_id = folder_id;
            if m.unread {
                *self.folder_unread.entry((account_id, folder_id)).or_default() += 1;
            }
            if self.is_unified_target(account_id, folder_id) {
                self.unified_slices.entry((account_id, folder_id)).or_default().push(m.clone());
            }
            self.message_cache.entry((account_id, folder_id)).or_default().push(m);
            added = true;
        }
        tracing::debug!(target: "hylki::undo", "instant restore: added={added} to folder {folder_id}");
        if !added {
            return;
        }
        for key in [(account_id, folder_id)] {
            if let Some(ms) = self.message_cache.get_mut(&key) {
                ms.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
            }
            if let Some(ms) = self.unified_slices.get_mut(&key) {
                ms.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
            }
        }
        self.forget_threads(account_id);
        self.refresh_list_display();
        self.push_unread_counts();
        // Undo means "put that back", and what you want back in front of you
        // is the message itself, not whatever the list moved on to when it
        // went away. Select it now, with the row, rather than when the server
        // gets round to confirming — which is the whole point of doing this
        // here (#200). The list processes SetMessages first, so the row is
        // there to select. Restoring several at once picks the newest, the
        // one nearest the top of the list.
        let newest = entry
            .rows
            .iter()
            .max_by_key(|m| m.timestamp)
            .map(|m| (account_id, m.id));
        tracing::debug!(target: "hylki::undo", "instant restore: selecting {newest:?}");
        if let Some(key) = newest {
            self.message_list.emit(MessageListInput::SelectAndLoad(key));
        }
    }

    /// Carry out one step. Returns false when it could not be done at all
    /// (the folder is gone, the messages are no longer where they were), in
    /// which case the step is dropped rather than left to fail again.
    fn apply_undo_step(&mut self, account_id: u32, step: &UndoStep) -> bool {
        match step {
            UndoStep::Move { from, to, message_ids } => {
                let Some(dest_folder_id) = self
                    .folders
                    .get(&account_id)
                    .and_then(|fs| fs.iter().find(|f| &f.path == to))
                    .map(|f| f.id)
                else {
                    tracing::info!("undo: {to:?} is no longer there, dropping the step");
                    return false;
                };
                self.send_to(account_id, MailRequest::UndoMove {
                    path: from.clone(),
                    dest: to.clone(),
                    dest_folder_id,
                    message_ids: message_ids.clone(),
                });
                // Finding, moving and reloading the messages takes a few round
                // trips — spin the refresh indicator until the worker's
                // BulkComplete says it has landed.
                self.bulk_pending += 1;
                self.update_busy_indicator();
                true
            }
            UndoStep::Flags(changes) => {
                let mut touched = 0usize;
                for change in changes {
                    for item in &change.items {
                        let Some(m) = self
                            .message_cache
                            .get(&(account_id, item.folder_id))
                            .and_then(|msgs| {
                                msgs.iter().find(|m| m.uid == item.uid && m.id == item.id).cloned()
                            })
                        else {
                            continue;
                        };
                        touched += 1;
                        match &change.kind {
                            FlagKind::Read => self.set_read(&m, change.value),
                            FlagKind::Star => self.set_star(&m, change.value),
                            FlagKind::Tag(keyword) => self.set_tag(&m, keyword, change.value),
                        }
                    }
                }
                if touched == 0 {
                    tracing::info!("undo: those messages are no longer here, dropping the step");
                }
                touched > 0
            }
            UndoStep::CreateFolder { path } => {
                self.create_folder(account_id, path.clone());
                true
            }
            UndoStep::DeleteFolder { path } => {
                self.delete_folder(account_id, path.clone());
                true
            }
            UndoStep::RenameFolder { from, to } => {
                if !self
                    .folders
                    .get(&account_id)
                    .is_some_and(|fs| fs.iter().any(|f| &f.path == from))
                {
                    tracing::info!("undo: {from:?} is no longer there, dropping the step");
                    return false;
                }
                self.apply_folder_rename(account_id, from.clone(), to.clone(), None);
                true
            }
        }
    }

    /// (Re)build the burger menu's Undo/Redo section, naming the action each
    /// one would reverse, and grey the entries out when a stack is empty.
    fn refresh_undo_menu(&self) {
        // The composer's history is the one on offer whenever it has one.
        let composing = self.menu_compose().is_some();
        let (undo_what, redo_what) = if composing {
            (self.compose_history.0.clone(), self.compose_history.1.clone())
        } else {
            (
                self.undo_stack.last().map(|e| e.what.clone()),
                self.redo_stack.last().map(|e| e.what.clone()),
            )
        };
        let label = |verb: &str, what: Option<&String>| match what {
            Some(w) => format!("{verb} {w}"),
            None => verb.to_string(),
        };
        // The shortcut is shown with the "accel" attribute rather than a real
        // accelerator: registering one would have GTK match Ctrl+Z before the
        // keystroke reached whatever has focus, taking text undo away from
        // entries and the composer. The key handler keeps that distinction,
        // and this only draws the reminder next to the entry.
        let item = |text: String, action: &str, accel: &str| {
            let item = gtk::gio::MenuItem::new(Some(&text), Some(action));
            item.set_attribute_value("accel", Some(&accel.to_variant()));
            item
        };
        self.undo_menu.remove_all();
        self.undo_menu.append_item(&item(
            label(&i18n("Undo"), undo_what.as_ref()),
            "win.undo",
            "<Control>z",
        ));
        self.undo_menu.append_item(&item(
            label(&i18n("Redo"), redo_what.as_ref()),
            "win.redo",
            "<Control><Shift>z",
        ));
        if std::env::var_os("HYLKI_SHOWCASE_COMPOSE_UNDO").is_some() {
            let mut chain = Vec::new();
            let mut node = gtk::prelude::GtkWindowExt::focus(&self.window);
            while let Some(w) = node {
                chain.push(format!("{}", w.type_()));
                node = w.parent();
            }
            tracing::info!(
                target: "hylki::compose::undo",
                "menu: composing={composing} undo={undo_what:?} redo={redo_what:?} focus=[{}]",
                chain.join(" < ")
            );
        }
        if let Some(a) = &self.undo_action {
            a.set_enabled(undo_what.is_some());
        }
        if let Some(a) = &self.redo_action {
            a.set_enabled(redo_what.is_some());
        }
    }

    /// A read/star/tag change the user asked for: applied, and recorded so
    /// Ctrl+Z puts it back (#200). Only messages the change actually moves
    /// are recorded, so one value covers every one of them. Automatic marks
    /// — reading a message marks it read — use the plain setters and are
    /// deliberately not recorded: they are not actions anyone took.
    fn user_set_flags(&mut self, what: String, msgs: &[Message], kind: FlagKind, value: bool) {
        let mut by_account: HashMap<u32, Vec<FlagRef>> = HashMap::new();
        for m in msgs {
            let changes = match &kind {
                // A draft is neither read nor unread, and a queued message
                // has no server flags at all: the setters decline both, so
                // recording them would leave dead entries in the history.
                FlagKind::Read => {
                    m.unread == value && !self.is_drafts_folder(m.account_id, m.folder_id)
                }
                FlagKind::Star => m.starred != value,
                FlagKind::Tag(keyword) => {
                    m.has_keyword(keyword) != value
                        && self.outbox_item(m.account_id, m.id).is_none()
                }
            };
            if !changes {
                continue;
            }
            by_account.entry(m.account_id).or_default().push(FlagRef {
                folder_id: m.folder_id,
                uid: m.uid,
                id: m.id,
            });
            match &kind {
                FlagKind::Read => self.set_read(m, value),
                FlagKind::Star => self.set_star(m, value),
                FlagKind::Tag(keyword) => self.set_tag(m, keyword, value),
            }
        }
        // One entry per account: undo sends its requests to one worker.
        for (account_id, items) in by_account {
            self.push_undo(
                account_id,
                what.clone(),
                UndoStep::Flags(vec![FlagChange { kind: kind.clone(), value: !value, items }]),
            );
        }
    }

    /// Tell the reader how card actions should show (⋯ toggle / auto on
    /// hover / always).
    fn push_card_actions_mode(&self) {
        self.message_view.emit(MessageViewInput::SetCardActionsMode {
            hover_toggle: self.card_actions_hover,
            hover_auto: self.card_actions_auto,
        });
    }

    /// The message the toolbar's per-message actions act on: the open message
    /// itself, or — in a conversation — the one highlighted card, when exactly
    /// one is highlighted. `None` greys the buttons out: with no card (or
    /// several) selected there is no way to say which message an action would
    /// mean.
    fn reply_target(&self) -> Option<Message> {
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
    fn compose_target(&self) -> Option<Message> {
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
    fn newest_to_answer(&self, thread: &[Message], fallback: Message) -> Message {
        if thread.len() <= 1 {
            return fallback;
        }
        let mut own = self.own_identities(fallback.account_id);
        if own.is_empty() {
            own.extend(self.email_of(fallback.account_id));
        }
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

    /// Launch (or re-present) the welcome wizard: the first run's greeting,
    /// the HYLKI_WELCOME review mode, and — on beta builds only — the burger
    /// menu's Welcome Wizard entry for testers.
    fn open_wizard(&mut self, sender: &ComponentSender<Self>) {
        use crate::ui::welcome::{Welcome, WelcomeOutput};
        if let Some(w) = self.welcome.as_ref().filter(|w| w.widget().is_visible()) {
            w.widget().present();
            return;
        }
        let welcome =
            Welcome::builder().launch(()).forward(sender.input_sender(), |out| match out {
                WelcomeOutput::AddAccount(account) => {
                    AppMsg::AccountSaved { original_email: None, account }
                }
                WelcomeOutput::ImportGoa(account) => AppMsg::ImportGoaAccount(account),
                WelcomeOutput::Prefs(p) => AppMsg::ApplyWelcomePrefs(p),
                WelcomeOutput::Done => AppMsg::PresentWindow,
                WelcomeOutput::Language(code) => AppMsg::WizardLanguage(code),
            });
        welcome.widget().set_transient_for(Some(&self.window));
        welcome.widget().set_modal(true);
        // On a true first run the main window stays hidden (see main.rs)
        // until the wizard finishes — or is dismissed.
        {
            let s = sender.clone();
            welcome.widget().connect_close_request(move |_| {
                s.input(AppMsg::PresentWindow);
                gtk::glib::Propagation::Proceed
            });
        }
        welcome.widget().present();
        self.welcome = Some(welcome);
    }

    /// The open-marks-read side effects for the just-selected message (#100):
    /// server flag, list row, cached copy, badges, notification.
    fn mark_opened_read(&mut self, m: &Message) {
        let account_id = m.account_id;
        if let Some(path) = self.resolve_folder_path(m) {
            self.send_to(account_id, MailRequest::SetSeen { path, uid: m.uid, seen: true });
        }
        // Reading new mail clears that account's new-mail notification.
        crate::notify::withdraw_mail(account_id);
        self.message_list.emit(MessageListInput::MarkRead(m.id));
        self.mark_cached_read(account_id, m.id);
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
    fn thread_star_target(&self, m: &Message) -> bool {
        self.current_thread.len() > 1
            && self
                .current
                .as_ref()
                .is_some_and(|c| c.account_id == m.account_id && c.id == m.id)
    }

    /// Whether the reader toolbar's star shows lit: the target message's own
    /// star, or any member's when the conversation is the target.
    fn toolbar_star_lit(&self) -> bool {
        self.reply_target().is_some_and(|m| {
            if self.thread_star_target(&m) {
                self.current_thread.iter().any(|t| t.starred)
            } else {
                m.starred
            }
        })
    }

    /// The account email for an id, if known.
    fn email_of(&self, account_id: u32) -> Option<String> {
        self.accounts
            .iter()
            .find(|a| a.id == account_id)
            .map(|a| a.email.clone())
    }

    /// Every address this account sends as: its own, plus its send-as
    /// aliases (#34). Lower-cased comparison is the caller's job.
    fn own_identities(&self, account_id: u32) -> Vec<String> {
        let Some(cfg) = self.effective_config().get(account_id.saturating_sub(1) as usize) else {
            return Vec::new();
        };
        std::iter::once(cfg.email.clone())
            .chain(cfg.aliases.iter().map(|al| config::split_identity(&al.identity).1))
            .filter(|a| !a.trim().is_empty())
            .collect()
    }

    /// Persist the sidebar's per-account state (order, collapse, custom-folders
    /// expansion, and icon-only mode).
    fn save_sidebar_state(&self) {
        config::save_sidebar_state(&config::SidebarState {
            order: self.account_order.clone(),
            collapsed: self.collapsed.clone(),
            folders_expanded: self.folders_expanded.clone(),
            icon_only: self.sidebar_collapsed,
            tree_collapsed: self.tree_collapsed.clone(),
            unified_expanded: self.unified_expanded,
            filtered_expanded: self.filtered_expanded,
            tags_expanded: self.tags_expanded,
            starred_expanded: self.starred_expanded,
            sent_expanded: self.sent_expanded,
            drafts_expanded: self.drafts_expanded,
            archive_expanded: self.archive_expanded,
            filtered_expanded_accounts: self.filtered_expanded_accounts.clone(),
            tags_expanded_accounts: self.tags_expanded_accounts.clone(),
        });
    }

    /// Account emails in display order: those listed in `account_order` first
    /// (in that order), then any remaining accounts by id.
    fn ordered_emails(&self) -> Vec<String> {
        let mut result: Vec<String> = Vec::new();
        for email in &self.account_order {
            if self.accounts.iter().any(|a| &a.email == email) && !result.contains(email) {
                result.push(email.clone());
            }
        }
        let mut rest: Vec<&Account> = self
            .accounts
            .iter()
            .filter(|a| !result.contains(&a.email))
            .collect();
        rest.sort_by_key(|a| a.id);
        for a in rest {
            result.push(a.email.clone());
        }
        result
    }

    /// Pin the floating hover-peek open as the normal side-by-side sidebar:
    /// the arrow inside the peek, clicked at a width that can host the full
    /// sidebar, persists the expanded state: drop the panel, expand the
    /// docked sidebar's rows, and save.
    fn pin_sidebar_from_peek(&mut self) {
        tracing::info!("peek: pinned to side-by-side");
        self.sidebar_peek = false;
        if !self.sidebar_layout_borrowed() {
            self.sidebar_collapsed = false;
        }
        self.rail_active = false;
        if let Some(peek) = self.peek_split.clone() {
            self.peek_transition.set(true);
            peek.set_show_sidebar(false);
            self.peek_transition.set(false);
        }
        self.sidebar.emit(SidebarInput::SetCollapsed(false));
        // Side-by-side again: settle at the normal expanded width.
        self.animate_sidebar(false);
        self.compact_sidebar_header(false);
        if !self.sidebar_layout_borrowed() {
            self.save_sidebar_state();
        }
    }

    /// Close the floating sidebar overlay if it is open — navigation picked in
    /// it is done with it (mirrors how GNOME's own adaptive sidebars behave).
    fn close_sidebar_peek(&mut self) {
        if self.sidebar_peek {
            self.rail_active = true;
            self.set_sidebar_peek(false, true);
        }
    }

    /// Hide or show the sidebar header's title and window controls (icon rail).
    fn compact_sidebar_header(&self, compact: bool) {
        if let (Some(header), Some(title), Some(menu)) = (
            self.sidebar_header.as_ref(),
            self.app_title.as_ref(),
            self.sidebar_menu.as_ref(),
        ) {
            set_sidebar_header_compact(header, title, menu, &self.sidebar_refresh, compact);
        }
    }

    /// Open or close the narrow-window sidebar *peek*: the expanded panel
    /// (the second sidebar instance, with its own header) sliding out over
    /// the panes from the rail's right edge, through the nested split view's
    /// overlay mode. The docked rail is not involved: it keeps its rows,
    /// header and width throughout and stays clickable beside the panel.
    /// `animate` is advisory — libadwaita animates the overlay itself.
    fn set_sidebar_peek(&mut self, open: bool, animate: bool) {
        let Some(peek) = self.peek_split.clone() else { return };
        tracing::info!(
            "peek: set open={open} animate={animate} (was peek={}, shown={})",
            self.sidebar_peek,
            peek.shows_sidebar()
        );
        self.sidebar_peek = open;
        // The notify our own transition emits must not read as a dismissal.
        self.peek_transition.set(true);
        peek.set_show_sidebar(open);
        self.peek_transition.set(false);
    }

    /// Send a state update to both sidebar instances — the docked rail and
    /// the peek panel show the same accounts, counts and rows.
    fn sidebars_emit(&self, msg: SidebarInput) {
        self.peek_sidebar.emit(msg.clone());
        self.sidebar.emit(msg);
    }

    /// Highlight what the app now shows in both sidebar instances. The one
    /// the user clicked already has it (its guard makes this a no-op), the
    /// other follows; neither reports back.
    fn mirror_selection(&self, sel: crate::ui::sidebar::Sel) {
        self.sidebars_emit(SidebarInput::MirrorSelection(sel));
    }

    /// Smoothly animate the sidebar rail between its expanded width and the
    /// narrow icon-only width by interpolating the split view's pinned width.
    fn animate_sidebar(&mut self, collapsing: bool) {
        let Some(split) = self.sidebar_split.clone() else {
            return;
        };
        // Start from the current on-screen width; fall back to sensible defaults.
        let from = split
            .sidebar()
            .map(|w| w.width() as f64)
            .filter(|w| *w > 1.0)
            .unwrap_or(if collapsing { 256.0 } else { SIDEBAR_RAIL_WIDTH });
        let expanded = (0.2 * self.window.width() as f64).clamp(220.0, 280.0);
        let to = if collapsing { SIDEBAR_RAIL_WIDTH } else { expanded };

        let s = split.clone();
        let target = adw::CallbackAnimationTarget::new(move |v| {
            s.set_min_sidebar_width(v);
            s.set_max_sidebar_width(v);
        });
        let anim = adw::TimedAnimation::new(&split, from, to, 200, target);
        anim.set_easing(adw::Easing::EaseOutCubic);
        if !collapsing {
            // Restore responsive sizing once expanded, so window resizes track again.
            let s2 = split.clone();
            anim.connect_done(move |_| {
                s2.set_min_sidebar_width(180.0);
                s2.set_max_sidebar_width(280.0);
                s2.set_sidebar_width_fraction(0.2);
            });
        }
        anim.play();
        self.sidebar_anim = Some(anim);
    }

    /// Update the sidebar's unread badges in place (no rebuild), derived from the
    /// loaded message lists. Cheap enough to call on every read/sync.
    fn push_unread_counts(&self) {
        let folders = self.folder_unread.clone();
        let unified = self.unified_unread();
        self.sidebars_emit(SidebarInput::SetUnread { folders, unified: self.inboxes_unread() });
        // The counted total is what GNOME shows beside Hylki in Background
        // Apps, so a process with no window still says what it is there for.
        if self.run_in_background.get() {
            crate::background::set_status(&crate::background::status_text(unified));
        }
        // And what the tray icon's dot answers to: the inboxes alone, as
        // its menu says.
        if let Some(tray) = &self.tray {
            tray.set_unread(self.inboxes_unread());
        }
        self.push_tray_mail();
    }

    /// Publish the tray item with the current icon, unread total and mail list.
    fn start_tray(&mut self, sender: &ComponentSender<Self>) {
        let unified = self.unified_unread();
        let mail = self.tray_mail.then(|| self.tray_mail_list());
        *self.tray_mail_key.borrow_mut() = mail.as_ref().map(tray_mail_key);
        self.tray = crate::tray::TrayHandle::start(
            sender.input_sender().clone(),
            self.tray_icon,
            crate::app_icon::png_for(&self.app_icon),
            unified,
            mail,
        );
    }

    /// The window a dialog about the icon change belongs over: Settings
    /// while it is open, else the main window.
    fn icon_dialog_parent(&self) -> gtk::Window {
        match self.prefs.as_ref().filter(|p| p.widget().is_visible()) {
            Some(p) => p.widget().clone().upcast(),
            None => self.window.clone().upcast(),
        }
    }

    /// While a restart waits for the desktop to take the launcher in: a
    /// modal with a spinner, so the pause reads as progress rather than a
    /// stuck app. It has no way to close — the app quits from under it.
    fn show_restarting_dialog(&self) {
        let dialog = adw::MessageDialog::new(
            Some(&self.icon_dialog_parent()),
            Some(i18n("Restarting Hylki").as_str()),
            Some(i18n("Applying your new app icon. Hylki will close and reopen in a moment.").as_str()),
        );
        let spinner = gtk::Spinner::new();
        spinner.set_size_request(32, 32);
        spinner.set_halign(gtk::Align::Center);
        spinner.set_margin_top(6);
        spinner.start();
        dialog.set_extra_child(Some(&spinner));
        dialog.present();
    }

    /// The heads-up after an icon change. The launcher already carries the
    /// new icon, so the app grid shows it — but GNOME Shell keeps a running
    /// app's windows bound to the app object it created for the old
    /// launcher (a changed launcher only replaces the object, it never
    /// re-tracks the windows), so the dock's running entry, like Hylki's
    /// own window icon, only switches once every window has closed: a
    /// restart. Offered, never forced, over whichever window the change
    /// came from.
    fn offer_restart_for_icon(&self, sender: &ComponentSender<Self>) {
        let dialog = adw::MessageDialog::new(
            Some(&self.icon_dialog_parent()),
            Some(i18n("Restart to finish switching icons?").as_str()),
            Some(i18n("The dock and Hylki's own windows keep the old icon until Hylki restarts.").as_str()),
        );
        dialog.add_responses(&[("later", i18n("Later").as_str()), ("restart", i18n("Restart Now").as_str())]);
        dialog.set_response_appearance("restart", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("later"));
        dialog.set_close_response("later");
        let s = sender.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "restart" {
                s.input(AppMsg::RestartApp);
            }
        });
        dialog.present();
    }

    /// Send the tray menu its mail list when it has changed.
    fn push_tray_mail(&self) {
        let Some(tray) = &self.tray else { return };
        let mail = self.tray_mail.then(|| self.tray_mail_list());
        let key = mail.as_ref().map(tray_mail_key);
        if *self.tray_mail_key.borrow() == key {
            return;
        }
        *self.tray_mail_key.borrow_mut() = key;
        tray.set_mail(mail);
    }

    /// The newest unread messages across accounts' inboxes, as tray cards,
    /// with the inboxes' unread total (the cards may be fewer). Filter
    /// destinations are left out: the menu says it lists inboxes.
    fn tray_mail_list(&self) -> crate::tray::TrayMailList {
        let unread = self.inboxes_unread();
        let items = self.tray_mail_cards();
        crate::tray::TrayMailList { items, unread }
    }

    fn tray_mail_cards(&self) -> Vec<crate::tray::TrayMail> {
        let several = self.accounts.len() > 1;
        let mut unread: Vec<(&Account, &Message)> = self
            .accounts
            .iter()
            .filter_map(|a| self.inbox_of(a.id).map(|f| (a, f)))
            .flat_map(|(a, f)| {
                self.message_cache
                    .get(&(a.id, f.id))
                    .into_iter()
                    .flat_map(|msgs| msgs.iter())
                    .filter(|m| m.unread)
                    .map(move |m| (a, m))
            })
            .collect();
        unread.sort_by(|x, y| y.1.timestamp.cmp(&x.1.timestamp));
        unread
            .into_iter()
            .take(crate::tray::MAIL_LIMIT)
            .map(|(a, m)| {
                let sender = if m.from_name.trim().is_empty() {
                    m.from_addr.clone()
                } else {
                    m.from_name.clone()
                };
                let mut heading = crate::tray::clip(&sender, 40);
                if several {
                    heading.push_str(" · ");
                    heading.push_str(&crate::tray::clip(&a.label, 24));
                }
                if !m.date.is_empty() {
                    heading.push_str(" · ");
                    heading.push_str(&m.date);
                }
                let texture = match crate::avatar::lookup(&m.from_addr, self.gravatar) {
                    crate::avatar::CacheLookup::Texture(t) => Some(t),
                    _ => None,
                };
                crate::tray::TrayMail {
                    account_id: m.account_id,
                    folder_id: m.folder_id,
                    message_id: m.id,
                    heading,
                    subject: crate::tray::clip(&m.subject, 60),
                    preview: crate::tray::clip(&m.preview, 70),
                    icon: crate::tray::sender_icon(&m.from_name, &m.from_addr, texture),
                }
            })
            .collect()
    }

    /// Push the current accounts + folders to the sidebar, in the user's chosen
    /// order and with each account's collapsed state.
    /// The Special Folders choices per account email (#82): every folder of
    /// every configured account, for the settings editor's combos.
    fn folder_choice_map(
        &self,
    ) -> std::collections::HashMap<String, Vec<(String, String)>> {
        let mut map = std::collections::HashMap::new();
        // The demo's accounts live only at the backend, so give the
        // Accounts panel's stand-in configs their folders as well.
        let cfgs: &[AccountConfig] = self.effective_config();
        for (i, cfg) in cfgs.iter().enumerate() {
            let id = i as u32 + 1;
            let list = self
                .folders
                .get(&id)
                .map(|fs| fs.iter().map(|f| (f.path.clone(), f.name.clone())).collect())
                .unwrap_or_default();
            map.insert(cfg.email.clone(), list);
        }
        map
    }

    /// [`folder_choice_map`], but with `account_id`'s list taken from `fresh`
    /// (a SetFolders payload not yet stored on self).
    fn folder_choice_map_with(
        &self,
        account_id: u32,
        fresh: &[Folder],
    ) -> std::collections::HashMap<String, Vec<(String, String)>> {
        let mut map = self.folder_choice_map();
        if let Some(cfg) = self.config.get(account_id as usize - 1) {
            map.insert(
                cfg.email.clone(),
                fresh.iter().map(|f| (f.path.clone(), f.name.clone())).collect(),
            );
        }
        map
    }

    fn rebuild_sidebar(&self) {
        // The accounts are being redrawn because something about them changed;
        // the faces their own mail wears follow from the same place (#189).
        self.refresh_own_faces();
        let order = self.ordered_emails();
        let sections: Vec<SectionData> = order
            .iter()
            .filter_map(|email| {
                let account = self.accounts.iter().find(|a| &a.email == email)?.clone();
                // Use the server-side unread count (accurate beyond the loaded
                // window) for each folder's badge.
                let folders = self
                    .folders
                    .get(&account.id)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|mut f| {
                        if let Some(n) = self.folder_unread.get(&(account.id, f.id)) {
                            f.unread = *n;
                        }
                        f
                    })
                    .collect();
                let color = self.account_color(account.id);
                let emoji = self.account_emoji(account.id);
                let avatar = self.account_avatar(account.id);
                // This account's collapsed tree nodes, keyed "email\tpath".
                let prefix = format!("{email}\t");
                let tree_collapsed = self
                    .tree_collapsed
                    .iter()
                    .filter_map(|k| k.strip_prefix(&prefix).map(String::from))
                    .collect();
                let filtered = self.account_filtered_folders(account.id);
                let has_filters = self
                    .filters
                    .iter()
                    .any(|r| r.account_email.eq_ignore_ascii_case(&account.email));
                Some(SectionData {
                    has_filters,
                    collapsed: self.collapsed.contains(email),
                    custom_expanded: self.folders_expanded.contains(email),
                    filtered_expanded: self.filtered_expanded_accounts.contains(email),
                    tags_expanded: self.tags_expanded_accounts.contains(email),
                    color,
                    emoji,
                    avatar,
                    account,
                    folders,
                    filtered,
                    tree_collapsed,
                })
            })
            .collect();
        // Count enabled accounts from config, not just the workers that have
        // reported in: at startup the accounts stream in one by one, and
        // counting only the connected ones made the first sidebar build look
        // single-account — its default selection then landed on that account's
        // inbox (possibly inside a collapsed section, so nothing visibly
        // highlighted) instead of the "All Inboxes" the app should open with.
        let multi_account = self.multi_account();
        let show_unified = self.unified_inboxes_shown();
        // The other unified rows are as pointless with one account.
        let unified_kinds =
            if multi_account { self.unified_kinds } else { config::UnifiedKinds::NONE };
        let unified_unread = self.inboxes_unread();
        let unified_folders = self.unified_folder_refs();
        self.sidebars_emit(SidebarInput::SetContents {
            sections,
            show_unified,
            unified_kinds,
            unified_tags: self.unified_tags,
            show_accounts: self.show_accounts,
            unified_chips: self.unified_chips,
            chevrons_left: self.chevrons_left,
            rail_dots: self.rail_dots_now(),
            rail_fold: self.rail_fold,
            unified_unread,
            unified_folders,
            tags: self.tags.clone(),
            filtered_placement: self.filtered_placement,
            tags_placement: self.tags_placement,
        });

        // Keep the list's per-account tint colours in sync.
        let colors: std::collections::HashMap<u32, String> = self
            .accounts
            .iter()
            .map(|a| (a.id, self.account_color(a.id)))
            .collect();
        self.message_list
            .emit(MessageListInput::SetAccountColors(colors));
    }

    fn remote_allowed(&self, m: &Message) -> bool {
        // A choice made for this one message wins over the standing policy,
        // in both directions: it is how remote content is shown (or put back)
        // for a single message when the banner is switched off.
        if let Some(&show) = self.remote_override.get(&(m.account_id, m.id)) {
            return show;
        }
        if self.auto_remote_content {
            return true;
        }
        let addr = m.from_addr.to_lowercase();
        self.allowed_senders.iter().any(|s| *s == addr)
    }

    /// Push the current attachments into the in-message thumbnail drawer (which
    /// hides itself when the list is empty). Called wherever `self.attachments`
    /// changes so the drawer always mirrors the open message.
    /// "name · n of m" for the lightbox's bottom bar.
    fn lightbox_caption(&self) -> String {
        match self.lightbox_items.get(self.lightbox_pos) {
            Some(att) => format!(
                "{} \u{b7} {} of {}",
                att.name,
                self.lightbox_pos + 1,
                self.lightbox_items.len()
            ),
            None => String::new(),
        }
    }

    fn lightbox_step(&mut self, delta: i32, sender: &ComponentSender<Self>) {
        let n = self.lightbox_items.len() as i32;
        if n == 0 {
            return;
        }
        self.lightbox_pos = (((self.lightbox_pos as i32 + delta) % n + n) % n) as usize;
        self.lightbox_set_zoom(1);
        self.lightbox_refresh(sender);
    }

    /// Zoom to 3x anchored at `(x, y)` — the clicked point in the fitted
    /// picture's coordinates. The whole box scales uniformly by 3, so the
    /// clicked content sits at exactly (3x, 3y) afterwards; once the resize
    /// has been laid out (the scroller's range exists), the adjustments put
    /// that point at the viewport's centre. Without this the view stayed at
    /// the top-left of the grown, mostly-letterboxed box — content apparently
    /// shoved off-screen.
    fn lightbox_zoom_to_point(&mut self, x: f64, y: f64) {
        self.lightbox_set_zoom(3);
        let (Some(picture), Some(scroller)) =
            (&self.lightbox_picture, &self.lightbox_scroller)
        else {
            return;
        };
        let hadj = scroller.hadjustment();
        let vadj = scroller.vadjustment();
        let target_x = x * 3.0 - f64::from(scroller.width()) / 2.0;
        let target_y = y * 3.0 - f64::from(scroller.height()) / 2.0;
        let tries = std::cell::Cell::new(0u8);
        picture.add_tick_callback(move |_, _| {
            let laid_out = hadj.upper() > hadj.page_size() + 1.0
                || vadj.upper() > vadj.page_size() + 1.0;
            if laid_out {
                hadj.set_value(target_x);
                vadj.set_value(target_y);
                return gtk::glib::ControlFlow::Break;
            }
            tries.set(tries.get() + 1);
            if tries.get() > 30 {
                return gtk::glib::ControlFlow::Break;
            }
            gtk::glib::ControlFlow::Continue
        });
    }

    /// Apply a lightbox zoom level. At 1x the picture fits its scroller; at
    /// 3x its box grows to that multiple of the viewport (Contain keeps the
    /// aspect) and the scroller pans the overflow.
    fn lightbox_set_zoom(&mut self, zoom: i32) {
        self.lightbox_zoom = zoom;
        let (Some(picture), Some(scroller)) =
            (&self.lightbox_picture, &self.lightbox_scroller)
        else {
            return;
        };
        if zoom <= 1 {
            picture.set_size_request(-1, -1);
        } else {
            picture.set_size_request(scroller.width() * zoom, scroller.height() * zoom);
        }
    }

    /// Work out the lightbox texture for the current item: images decode on
    /// the spot; a PDF's page comes from the shared full-size cache or a
    /// worker render that circles back via [`AppMsg::LightboxRendered`].
    fn lightbox_refresh(&mut self, sender: &ComponentSender<Self>) {
        use crate::ui::attachments_gallery as gallery;
        self.lightbox_texture = None;
        let Some(att) = self.lightbox_items.get(self.lightbox_pos) else { return };
        if crate::models::is_image_name(&att.name) {
            self.lightbox_texture = gallery::texture_from(&att.data);
            return;
        }
        // Cache hit paints immediately (and, crucially, spawns nothing — a
        // hit that re-entered via LightboxRendered would loop forever).
        if let Some(tex) = gallery::cached_pdf_preview(&att.data) {
            self.lightbox_texture = Some(tex);
            return;
        }
        let key = gallery::content_key(&att.data);
        let s = sender.clone();
        gallery::lightbox_pdf_texture(&att.data, move |_| {
            s.input(AppMsg::LightboxRendered(key));
        });
    }

    /// The tag entries of the reader's menus (#71): one per tag, its swatch
    /// filled where the reader's target message carries it. None without a
    /// target or a tag.
    fn reader_tag_entries(
        &self,
        sender: &ComponentSender<Self>,
    ) -> Option<Vec<crate::ui::context_menu::MenuEntry>> {
        let target = self.reply_target()?;
        if self.tags.is_empty() {
            return None;
        }
        let s = sender.input_sender().clone();
        Some(crate::ui::message_list::tag_menu_entries(&self.tags, &target, move |keyword, _add| {
            let _ = s.send(AppMsg::ToggleTagCurrent(keyword));
        }))
    }

    /// The collapsed reader header's overflow menu: the right group of the
    /// toolbar (Tags, Read/Unread, Spam, Move To, Find, Print), same icons,
    /// enabled under the same conditions. The left group never folds.
    fn show_reader_overflow_menu(&self, sender: &ComponentSender<Self>) {
        use crate::ui::context_menu::{show_context_menu, MenuEntry};

        // One entry per action, each with its own clone of the input sender.
        macro_rules! entry {
            ($label:expr, $icon:expr, $msg:expr, $enabled:expr) => {{
                let s = sender.input_sender().clone();
                MenuEntry::new($label, move || {
                    let _ = s.send($msg);
                })
                .icon(concat!("co.hyprlab.Hylki-", $icon, "-symbolic"))
                .enabled($enabled)
            }};
        }

        let has_current = self.current.is_some();
        let focus = self.focus.active(config::FocusPart::ReaderToolbar);
        let sections = if self.showing_outbox {
            // The Outbox's own buttons (Edit, Send, Send all, Delete) sit in
            // the always-visible left group; only View Source is left to
            // fold here (queued rows have no context menu to carry it) —
            // unless Focus Mode folded that group too.
            let mut sections = Vec::new();
            if focus {
                let many = has_current || self.list_selection.len() > 1;
                sections.push(vec![
                    entry!(i18n("Edit this message"), "document-edit", AppMsg::EditCurrentOutbox, has_current),
                    entry!(i18n("Try to send this message now"), "mail-send", AppMsg::SendCurrentOutbox, has_current),
                    entry!(i18n("Send all"), "mail-send", AppMsg::RetryAllOutbox, true),
                    entry!(i18n("Delete"), "user-trash", AppMsg::Delete, many),
                ]);
            }
            sections.push(vec![entry!(i18n("View Source"), "code", AppMsg::ViewSource, has_current)]);
            sections
        } else {
            // Only the right group folds in here, in its own order; the left
            // group stays on the bar at every width. Per-message actions act
            // on the reply target: the open message, or — in a conversation —
            // the one highlighted card. With none (or several) highlighted
            // they grey out.
            use config::ToolbarItem as T;
            let target = self.reply_target();
            let acts = target.is_some();
            let many = acts || self.list_selection.len() > 1;
            let starred = target.as_ref().is_some_and(|m| m.starred);
            let target_unread = target.as_ref().is_some_and(|m| m.unread);
            let restorable = target.as_ref().is_some_and(|m| {
                self.folder_kind(m.account_id, m.folder_id)
                    .is_some_and(|k| matches!(k, FolderKind::Trash | FolderKind::Junk))
            });
            // Focus Mode folds the left group in as well, as a section of
            // its own ahead of the right group.
            let groups: Vec<&[T]> = if focus {
                vec![&self.reader_toolbar.left, &self.reader_toolbar.right]
            } else {
                vec![&self.reader_toolbar.right]
            };
            let mut sections = Vec::new();
            for group in groups {
            let mut section = Vec::new();
            for item in group {
                match item {
                    T::Reply => section.push(entry!(i18n("Reply"), "mail-reply-sender", AppMsg::Reply, acts)),
                    T::ReplyAll => section.push(entry!(i18n("Reply All"), "mail-reply-all", AppMsg::ReplyAll, acts)),
                    T::Forward => section.push(entry!(i18n("Forward"), "mail-forward", AppMsg::Forward, acts)),
                    T::Star => section.push(if starred {
                        entry!(i18n("Remove Flag"), "non-starred", AppMsg::ToggleStar, acts)
                    } else {
                        entry!(i18n("Flag"), "starred", AppMsg::ToggleStar, acts)
                    }),
                    T::Archive => section.push(entry!(i18n("Archive"), "mail-archive", AppMsg::Archive, acts)),
                    T::Delete => section.push(entry!(i18n("Delete"), "user-trash", AppMsg::Delete, many)),
                    T::Spam => section.push(if self.target_in_junk() {
                        entry!(i18n("Not Spam"), "mail-mark-notjunk", AppMsg::MarkSpam, acts)
                    } else {
                        entry!(i18n("Mark as Spam"), "mail-mark-junk", AppMsg::MarkSpam, acts)
                    }),
                    T::ReadUnread => section.push(if target_unread {
                        entry!(i18n("Mark as Read"), "mail-read", AppMsg::ToggleReadCurrent, acts)
                    } else {
                        entry!(i18n("Mark as Unread"), "mail-unread", AppMsg::ToggleReadCurrent, acts)
                    }),
                    // Tags (#71), where there are any and something to tag —
                    // behind a submenu, as in the message list's menu.
                    T::Tags => {
                        if let Some(entries) = self.reader_tag_entries(sender) {
                            section.push(
                                MenuEntry::submenu(i18n("Tags"), vec![entries]).icon("co.hyprlab.Hylki-tag-outline-symbolic"),
                            );
                        }
                    }
                    T::MoveTo => {
                        if restorable {
                            section.push(entry!(i18n("Move to Inbox"), "mail-inbox", AppMsg::MoveToInbox, acts));
                        }
                        section.push(entry!(i18n("Move To…"), "folder", AppMsg::MoveToMenu, many));
                    }
                    T::Find => section.push(entry!(
                        i18n("Find in Message"),
                        "loupe-with-arrow",
                        AppMsg::OpenReaderFind,
                        has_current
                    )),
                    // View Source is deliberately absent: it lives in the
                    // message list's context menu only (the Outbox variant
                    // above keeps it — queued rows have no such menu).
                    T::Print => section.push(entry!(i18n("Print Preview"), "printer", AppMsg::PrintPreview, has_current)),
                }
            }
            if !section.is_empty() {
                sections.push(section);
            }
            }
            sections
        };

        let btn = &self.reader_overflow_btn;
        show_context_menu(btn, (btn.width() / 2) as f64, btn.height() as f64, sections);
    }

    /// The message list's search, filters, count and sort as a menu: what
    /// the ⋯ in its header offers while Focus Mode has them folded away.
    /// The toggles are flipped through their own buttons, so the filtering
    /// happens where it always does.
    fn show_list_overflow_menu(&self, sender: &ComponentSender<Self>) {
        use crate::ui::context_menu::{show_context_menu_with_header, MenuEntry};
        let Some(lh) = self.list_header_widgets.get() else {
            return;
        };
        let s = sender.input_sender().clone();
        let search = MenuEntry::new(i18n("Search Messages…"), move || {
            let _ = s.send(AppMsg::OpenListSearch);
        })
        .icon("co.hyprlab.Hylki-system-search-symbolic");
        let unread = lh.unread.clone();
        let unread_on = unread.is_active();
        let unread_entry = MenuEntry::new(i18n("Show only unread"), move || {
            unread.set_active(!unread.is_active());
        })
        .icon("co.hyprlab.Hylki-mail-unread-symbolic")
        .selected(unread_on);
        let starred = lh.starred.clone();
        let starred_on = starred.is_active();
        let starred_entry = MenuEntry::new(i18n("Show only starred"), move || {
            starred.set_active(!starred.is_active());
        })
        .icon("co.hyprlab.Hylki-starred-symbolic")
        .selected(starred_on);
        let current = lh.sort.state().and_then(|v| v.str().map(String::from)).unwrap_or_default();
        let orders: Vec<MenuEntry> = [
            (i18n_noop("Date (Newest first)"), "date_newest"),
            (i18n_noop("Date (Oldest first)"), "date_oldest"),
            (i18n_noop("Sender (A–Z)"), "sender"),
            (i18n_noop("Subject (A–Z)"), "subject"),
            (i18n_noop("Unread first"), "unread"),
            (i18n_noop("Flagged first"), "flagged"),
        ]
        .into_iter()
        .map(|(label, key)| {
            let action = lh.sort.clone();
            // The same route the sort menu takes, so the list and the
            // action's own state both follow.
            MenuEntry::new(i18n(label), move || action.activate(Some(&key.to_variant()))).selected(current == key)
        })
        .collect();
        let sort = MenuEntry::submenu(i18n("Sort"), vec![orders])
            .icon("co.hyprlab.Hylki-view-sort-descending-symbolic");
        let sections = vec![vec![search], vec![unread_entry, starred_entry], vec![sort]];
        // The message count heads the menu, where the header showed it.
        let header = (!self.list_count.is_empty()).then(|| self.list_count.clone());
        let btn = lh.overflow.clone();
        show_context_menu_with_header(&btn, (btn.width() / 2) as f64, btn.height() as f64, header.as_deref(), sections);
    }

    /// Focus Mode changed (the menu, the shortcut or Settings): save it,
    /// and animate whichever parts differ from what is on screen.
    fn set_focus_mode(&mut self, focus: config::FocusMode) {
        use config::FocusPart as F;
        let prev = self.focus;
        if prev != focus {
            self.focus = focus;
            config::save_focus_mode(&focus);
            let changed = |part: F| prev.active(part) != focus.active(part);
            if changed(F::ReaderToolbar) || changed(F::ListHeader) {
                self.sync_focus_chrome();
                // Not while the inline composer covers the header: that
                // path hides the ⋯ by hand and restores it on close.
                if !self.reader_compose.as_ref().is_some_and(|r| r.window.is_none()) {
                    self.reader_overflow_btn.set_visible(self.reader_overflow_wanted());
                }
            }
            if changed(F::HideAccounts) || changed(F::FoldUnified) {
                self.sidebars_emit(SidebarInput::SetFocus {
                    hide_accounts: focus.active(F::HideAccounts),
                    fold_unified: focus.active(F::FoldUnified),
                    animate: true,
                });
            }
            if changed(F::HideAvatars)
                || changed(F::OnePreviewLine)
                || changed(F::HidePreview)
                || changed(F::HideSubject)
            {
                self.push_list_look(true);
            }
            if changed(F::RailSidebar) {
                // The dots (or counts) are drawn with the rows, so the
                // sidebar is rebuilt before it folds.
                self.rebuild_sidebar();
                self.sync_rail();
            }
            if changed(F::ReaderView) {
                self.push_reader_focus();
            }
        }
        // The menu's check item and Settings stay in step wherever the
        // change came from.
        if self.focus_action.state().and_then(|v| v.get::<bool>()) != Some(focus.enabled) {
            self.focus_action.set_state(&focus.enabled.to_variant());
        }
        if let Some(p) = &self.prefs {
            p.emit(PrefInput::SetFocusMode(focus));
        }
    }

    /// Slide the reader toolbar's buttons and the list header's controls
    /// away or back, as Focus Mode has them. Unmapped widgets (at launch)
    /// jump straight to the state.
    fn sync_focus_chrome(&self) {
        use config::FocusPart as F;
        let shown = !self.focus.active(F::ReaderToolbar);
        if let Some(tb) = self.reader_toolbar_widgets.get() {
            for (_, w) in &tb.buttons {
                if let Some(r) = w.downcast_ref::<gtk::Revealer>() {
                    focus_reveal(r, shown);
                }
            }
            for r in &tb.outbox {
                focus_reveal(r, shown);
            }
        }
        let shown = !self.focus.active(F::ListHeader);
        if let Some(lh) = self.list_header_widgets.get() {
            for r in &lh.revealers {
                focus_reveal(r, shown);
            }
            focus_reveal(&lh.overflow, !shown);
        }
    }

    /// The avatars the list draws: the setting, unless Focus Mode hides them.
    fn list_avatars(&self) -> bool {
        self.avatars && !self.focus.active(config::FocusPart::HideAvatars)
    }

    /// The preview lines the list shows: the setting, hidden outright or
    /// capped at one by Focus Mode (previews switched off stay off).
    ///
    /// Only what is *drawn* changes — the setting itself, and with it what
    /// the workers fetch, is left alone. So the preview text is in hand the
    /// moment Focus Mode ends, rather than waiting for the next sync the way
    /// switching previews off in Settings does.
    fn list_preview_lines(&self) -> u32 {
        if self.focus.active(config::FocusPart::HidePreview) {
            0
        } else if self.focus.active(config::FocusPart::OnePreviewLine) {
            self.preview_lines.min(1)
        } else {
            self.preview_lines
        }
    }

    /// Whether the list draws subject lines: always, unless Focus Mode's
    /// subject part is in force (then a row is its sender and its date).
    fn list_show_subject(&self) -> bool {
        !self.focus.active(config::FocusPart::HideSubject)
    }

    /// Whether the icon rail marks unread folders with a dot instead of a
    /// count: the preference, or Focus Mode's rail part while it holds.
    fn rail_dots_now(&self) -> bool {
        self.rail_dots || self.focus.active(config::FocusPart::RailSidebar)
    }

    /// Whether the rail belongs on screen: the window is too narrow for the
    /// full sidebar, Focus Mode has folded it, or the user collapsed it
    /// themselves. Only the last of those is ever saved.
    fn rail_wanted(&self) -> bool {
        self.auto_rail || self.focus.active(config::FocusPart::RailSidebar) || self.sidebar_collapsed
    }

    /// Whether the sidebar's layout is Focus Mode's for the while: the rail
    /// part is in force, so collapsing or expanding it is a look for as long
    /// as the mode lasts and never the layout the user keeps. Nothing is
    /// saved while this holds, and ending the mode gives back exactly the
    /// sidebar it took over — whatever was done to it meanwhile.
    fn sidebar_layout_borrowed(&self) -> bool {
        self.focus.active(config::FocusPart::RailSidebar)
    }

    /// Put the rail up or take it down to match [`Self::rail_wanted`],
    /// animating the split the way the toggle does. Persists nothing.
    fn sync_rail(&mut self) {
        let want = self.rail_wanted();
        // A floating peek is the expanded sidebar by another name: fold it
        // before the rail goes up under it.
        if want && self.sidebar_peek {
            self.set_sidebar_peek(false, true);
        }
        if want != self.rail_active {
            self.rail_active = want;
            self.sidebar.emit(SidebarInput::SetCollapsed(want));
            self.animate_sidebar(want);
            self.compact_sidebar_header(want);
        }
    }

    /// Hand the list its look; `animate` slides the avatars away or back
    /// (a Focus Mode toggle) rather than rebuilding the rows outright.
    fn push_list_look(&self, animate: bool) {
        self.message_list.emit(MessageListInput::SetLook {
            avatars: self.list_avatars(),
            preview_lines: self.list_preview_lines(),
            subject: self.list_show_subject(),
            animate,
        });
    }

    /// What Reader View does on each open: Focus Mode's "on" while it holds,
    /// else the setting.
    /// The session's message zoom: every reader on screen follows.
    fn set_zoom(&mut self, zoom: u32) {
        if zoom == self.zoom {
            return;
        }
        self.zoom = zoom;
        self.message_view.emit(MessageViewInput::SetZoom(zoom));
        for p in self.popouts.values() {
            p.controller.emit(MessageWindowInput::SetZoom(zoom));
        }
    }

    fn effective_reader_default(&self) -> config::ReaderDefault {
        if self.focus.active(config::FocusPart::ReaderView) {
            config::ReaderDefault::On
        } else {
            self.reader_default
        }
    }

    /// Reader View for the message on screen: on in Focus Mode, else as saved.
    fn effective_reader_mode(&self) -> bool {
        self.reader_mode || self.focus.active(config::FocusPart::ReaderView)
    }

    /// Focus Mode's Reader View came or went: every reader follows, the
    /// message on screen included.
    fn push_reader_focus(&self) {
        let policy = self.effective_reader_default();
        let mode = self.effective_reader_mode();
        self.message_view.emit(MessageViewInput::SetReaderDefault(policy));
        self.message_view.emit(MessageViewInput::SetReaderMode(mode));
        for p in self.popouts.values() {
            p.controller.emit(MessageWindowInput::SetReaderDefault(policy));
            p.controller.emit(MessageWindowInput::SetReaderMode(mode));
        }
    }

    fn sync_attachment_drawer(&self) {
        // Switched off (#213), the drawer is simply never given anything:
        // empty hides it, seam and all.
        let items = if self.drawer_enabled { self.attachments.clone() } else { Vec::new() };
        self.attachment_drawer.emit(AttachmentDrawerInput::SetItems(items));
        self.push_card_attachments();
    }

    /// The messages on screen: the open conversation, or the lone message.
    fn conversation_members(&self) -> Vec<(u32, u32)> {
        if self.current_thread.is_empty() {
            self.current.iter().map(|c| (c.account_id, c.id)).collect()
        } else {
            self.current_thread.iter().map(|m| (m.account_id, m.id)).collect()
        }
    }

    /// Hand the reader what each message on screen has attached (#213), as
    /// far as the cache knows: names and sizes only, never the bytes — the
    /// document lists them, and the app opens or saves them on request.
    fn push_card_attachments(&self) {
        use crate::ui::message_view::CardAttachment;
        let mut map: HashMap<(u32, u32), Vec<CardAttachment>> = HashMap::new();
        for key in self.conversation_members() {
            if let Some(items) = self.attachment_cache.get(&key) {
                let atts: Vec<CardAttachment> = items
                    .iter()
                    .map(|a| CardAttachment { name: a.name.clone(), size: a.data.len() as u64 })
                    .collect();
                if !atts.is_empty() {
                    map.insert(key, atts);
                }
            }
        }
        self.message_view.emit(MessageViewInput::SetCardAttachments(map));
    }

    /// Fill the drawer for a whole conversation: ask the disk cache for every
    /// member's attachments (never the network) and show what's already in
    /// hand. Opening a thread used to load nothing at all — the drawer only
    /// appeared after clicking a member, which runs the single-message path.
    fn load_thread_attachments(&mut self) {
        let wanted: Vec<(u32, u32, u32, String)> = self
            .current_thread
            .iter()
            .filter(|tm| tm.has_attachment)
            .filter(|tm| !self.attachment_cache.contains_key(&(tm.account_id, tm.id)))
            .filter_map(|tm| {
                self.resolve_folder_path(tm)
                    .map(|p| (tm.account_id, tm.id, tm.uid, p))
            })
            .collect();
        if !wanted.is_empty() {
            self.attachments_loading = true;
        }
        for (account_id, message_id, uid, path) in wanted {
            self.send_to(account_id, MailRequest::LoadAttachments {
                message_id,
                path,
                uid,
                download: true,
            });
        }
        self.refresh_thread_attachments();
    }

    /// Point the drawer at the union of cached attachments across the open
    /// conversation. A reply pulled in from Sent can duplicate what it was
    /// sent with, so identical (name, size) pairs are shown once.
    fn refresh_thread_attachments(&mut self) {
        let mut seen = HashSet::new();
        let mut merged = Vec::new();
        for tm in &self.current_thread {
            if let Some(items) = self.attachment_cache.get(&(tm.account_id, tm.id)) {
                for a in items {
                    if seen.insert((a.name.clone(), a.data.len())) {
                        merged.push(a.clone());
                    }
                }
            }
        }
        self.attachments = merged;
        self.sync_attachment_drawer();
    }

    /// Present a read-only window showing raw message source (monospace).
    fn show_source_window(&self, text: &str) {
        let buffer = gtk::TextBuffer::new(None);
        buffer.set_text(text);
        let view = gtk::TextView::with_buffer(&buffer);
        view.set_editable(false);
        view.set_monospace(true);
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_left_margin(12);
        view.set_right_margin(12);
        view.set_top_margin(8);
        view.set_bottom_margin(8);

        let scroller = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hexpand(true)
            .child(&view)
            .build();

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&scroller));

        let window = adw::Window::builder()
            .transient_for(&self.window)
            .title(&i18n("Message Source"))
            .default_width(720)
            .default_height(620)
            .content(&toolbar)
            .build();
        window.present();
    }

    /// Set the attachment scan going over every folder in the gallery's scope,
    /// newest mail first. One chunk per folder per pass; each answer asks for
    /// the next, so the archive fills in behind the user while they browse and
    /// stops as soon as a folder is fully described.
    fn start_attachment_scan(&mut self) {
        let folders: Vec<(u32, String)> = self
            .gallery_scope()
            .iter()
            .flat_map(|a| a.folders.iter().map(|f| (a.id, f.path.clone())).collect::<Vec<_>>())
            .collect();
        for (account_id, folder_path) in folders {
            self.send_to(account_id, MailRequest::ScanAttachments { folder_path });
        }
    }

    /// The accounts and folders the attachments gallery may draw on: every
    /// folder the cache's gallery query can return, which is all of them bar
    /// Drafts, Junk and Trash. The folders are already in sidebar order.
    fn gallery_scope(&self) -> Vec<GalleryAccount> {
        self.accounts
            .iter()
            .map(|a| GalleryAccount {
                id: a.id,
                label: a.label.clone(),
                folders: self
                    .folders
                    .get(&a.id)
                    .map(|fs| {
                        fs.iter()
                            .filter(|f| {
                                !matches!(
                                    f.kind,
                                    FolderKind::Drafts | FolderKind::Junk | FolderKind::Trash
                                )
                            })
                            .map(|f| GalleryFolder {
                                path: f.path.clone(),
                                name: f.name.clone(),
                                kind: f.kind,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// Leave the attachments gallery, dropping its data. The gallery loads
    /// every item's bytes eagerly (up to 300 × 6 MiB per account) and reloads
    /// from scratch on each visit anyway, so keeping two copies of that around
    /// for the rest of the session bought nothing but memory (issue #106).
    fn leave_gallery(&mut self) {
        if std::mem::take(&mut self.showing_gallery) {
            self.gallery_total = 0;
            self.gallery.emit(GalleryInput::Page {
                items: Vec::new(),
                total: 0,
                offset: 0,
            });
        }
    }

    /// More than one account is switched on. Counted from config, not from
    /// the workers that have reported in: at startup the accounts stream in
    /// one by one, and counting only the connected ones made the first
    /// sidebar build look single-account — its default selection then
    /// landed on that account's inbox (possibly inside a collapsed section,
    /// so nothing visibly highlighted) instead of the "All Inboxes" the app
    /// should open with.
    fn multi_account(&self) -> bool {
        self.config.iter().filter(|c| c.enabled).count() > 1
            // The demo has no config-file accounts, but its two mock accounts
            // deserve the same All Inboxes opening as a real multi-account setup.
            || (demo_mode() && self.accounts.len() > 1)
    }

    /// The sidebar has an Inboxes row: the preference is on and there is
    /// more than one account to merge.
    fn unified_inboxes_shown(&self) -> bool {
        self.show_unified_pref && self.multi_account()
    }

    /// Open a unified view (Inboxes, Starred, Sent, …, Filtered): the merged
    /// list of every account's folder of that kind. What clicking the
    /// sidebar row does; a notification click lands here too.
    fn open_unified(&mut self, view: UnifiedView) {
        let t_open = std::time::Instant::now();
        self.close_sidebar_peek();
        self.mirror_selection(match view {
            UnifiedView::Kind(FolderKind::Inbox) => crate::ui::sidebar::Sel::Unified,
            UnifiedView::Kind(kind) => crate::ui::sidebar::Sel::UnifiedKind(kind),
            UnifiedView::Filtered => crate::ui::sidebar::Sel::UnifiedFiltered,
        });
        self.leave_gallery();
        self.showing_contacts = false;
        self.showing_outbox = false;
        // Another view's slices are another view's: start afresh
        // (the loads below refill them).
        if self.unified_view != view {
            self.unified_slices.clear();
            self.unified_boot_requested.clear();
        }
        self.unified_view = view;
        self.unified = true;
        self.tag_view = None;
        self.selected = None;
        self.current = None;
        self.current_thread.clear();
        self.attachments.clear();
        self.attachments_loading = false;
        self.sync_attachment_drawer();
        self.show_message(None, false);
        self.message_list.emit(MessageListInput::SetSelected(None));
        self.message_list.emit(MessageListInput::SetColorize(true));
        self.message_list.emit(MessageListInput::ResetPaging);
        // A Sent view's rows all come from you — name the recipients.
        self.message_list.emit(MessageListInput::SetShowRecipient(
            view == UnifiedView::Kind(FolderKind::Sent),
        ));
        self.message_list.emit(MessageListInput::SetRestorable(false));
        self.message_list.emit(MessageListInput::SetInJunk(false));
        self.message_list
            .emit(MessageListInput::SetInDrafts(view == UnifiedView::Kind(FolderKind::Drafts)));
        self.message_view
            .emit(MessageViewInput::SetDraftsView(view == UnifiedView::Kind(FolderKind::Drafts)));
        let reqs = self.unified_targets();
        // Keep every account's last known slice and top it up from the
        // folder caches, the way opening a single folder does. This used
        // to clear the lot and wait: an account whose worker was slow to
        // answer — busy backfilling a large mailbox, reconnecting, or
        // offline — was simply absent from "All Inboxes", while its own
        // Inbox, served from cache, still listed its mail. Each account's
        // slice is replaced when its load lands.
        for (account_id, folder_id, path) in &reqs {
            // A folder not seen since launch (only inboxes are primed
            // at startup): the on-disk index has it as last synced,
            // so the view opens at once — and after a restart — rather
            // than waiting on the worker.
            let seen = self
                .message_cache
                .get(&(*account_id, *folder_id))
                .is_some_and(|c| !c.is_empty());
            if !seen {
                let from_disk = self
                    .cache
                    .as_ref()
                    .map(|c| c.load_messages(*account_id, path, *folder_id))
                    .unwrap_or_default();
                if !from_disk.is_empty() {
                    let from_disk = self.merge_local_tags(*account_id, from_disk);
                    self.message_cache.insert((*account_id, *folder_id), from_disk);
                }
            }
            if let Some(cached) = self.message_cache.get(&(*account_id, *folder_id)) {
                if !cached.is_empty() {
                    self.unified_slices.insert((*account_id, *folder_id), cached.clone());
                }
            }
        }
        // Forget folders that no longer contribute (an account
        // removed or disabled, a rule gone, since the last visit).
        let live: std::collections::HashSet<(u32, u32)> =
            reqs.iter().map(|(a, f, _)| (*a, *f)).collect();
        self.unified_slices.retain(|k, _| live.contains(k));
        if self.unified_slices.is_empty() {
            self.message_list
                .emit(MessageListInput::SetLoading);
        } else {
            self.emit_unified();
        }
        // Request every account's inbox; each result replaces that
        // account's slice as it arrives.
        for (account_id, folder_id, path) in reqs {
            self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
        }
        self.push_index_complete();
        tracing::info!(
            "unified {view:?}: opened in {:?} with {} cached messages",
            t_open.elapsed(),
            self.unified_slices.values().map(Vec::len).sum::<usize>()
        );
    }

    /// The menu a right-click on a reader card opens (the message list
    /// row's menu, for that one message), anchored on the window at (x, y).
    fn show_card_menu(&self, m: Message, x: f64, y: f64, sender: &ComponentSender<Self>) {
        use crate::ui::context_menu::{show_context_menu, MenuEntry};
        // Through the card path: a reply started from a card belongs in the
        // pane's inline composer, like the card's own buttons — the row path
        // would open a compose window. Every other action falls through to
        // the row behaviour there.
        let item = |action: RowAction, label: String, icon: &str| -> MenuEntry {
            let s = sender.input_sender().clone();
            let message = m.clone();
            MenuEntry::new(label, move || {
                let _ = s.send(AppMsg::CardAction { action, message: Box::new(message.clone()) });
            })
            .icon(format!("co.hyprlab.Hylki-{icon}-symbolic"))
        };
        let kind = self.folder_kind(m.account_id, m.folder_id);
        let in_junk = kind == Some(FolderKind::Junk);
        let restorable = matches!(kind, Some(FolderKind::Trash | FolderKind::Junk));
        let mut sections = vec![
            vec![
                item(RowAction::Reply, i18n("Reply"), "mail-reply-sender"),
                item(RowAction::ReplyAll, i18n("Reply All"), "mail-reply-all"),
                item(RowAction::Forward, i18n("Forward"), "mail-forward"),
                item(RowAction::EditAsNew, i18n("Edit as New Message"), "document-edit"),
            ],
            vec![
                if m.starred {
                    item(RowAction::ToggleStar, i18n("Remove Star"), "non-starred")
                } else {
                    item(RowAction::ToggleStar, i18n("Star"), "starred")
                },
                if m.unread {
                    item(RowAction::ToggleRead, i18n("Mark as Read"), "mail-read")
                } else {
                    item(RowAction::ToggleRead, i18n("Mark as Unread"), "mail-unread")
                },
            ],
        ];
        if !self.tags.is_empty() {
            let s = sender.input_sender().clone();
            let message = m.clone();
            let entries =
                crate::ui::message_list::tag_menu_entries(&self.tags, &m, move |keyword, add| {
                    let _ = s.send(AppMsg::SetTag {
                        message: Box::new(message.clone()),
                        keyword,
                        add,
                    });
                });
            sections.push(vec![
                MenuEntry::submenu(i18n("Tags"), vec![entries]).icon("co.hyprlab.Hylki-tag-outline-symbolic"),
            ]);
        }
        let mut acts = Vec::new();
        if in_junk {
            acts.push(item(RowAction::NotSpam, i18n("Not Spam"), "mail-mark-notjunk"));
        } else {
            if restorable {
                acts.push(item(RowAction::MoveToInbox, i18n("Move to Inbox"), "mail-inbox"));
            }
            acts.push(item(RowAction::Spam, i18n("Mark as Spam"), "mail-mark-junk"));
        }
        {
            let s = sender.input_sender().clone();
            let message = m.clone();
            acts.push(
                MenuEntry::new(i18n("Move To…"), move || {
                    let _ = s.send(AppMsg::ListMoveTo {
                        messages: vec![message.clone()],
                        offer_whole: false,
                        x,
                        y,
                    });
                })
                .icon("co.hyprlab.Hylki-folder-symbolic"),
            );
        }
        acts.push(item(RowAction::Archive, i18n("Archive"), "mail-archive"));
        acts.push(item(RowAction::Delete, i18n("Delete"), "user-trash"));
        sections.push(acts);
        sections.push(vec![item(RowAction::AddContact, i18n("Add Sender to Contacts"), "contact-new")]);
        // Remote content, per message. The banner offers the same thing, but
        // it can be switched off (Settings → Privacy) and then there is
        // nothing to click — so the choice lives here too, and here it goes
        // both ways: the banner can only ever let content in.
        {
            let showing = self.remote_allowed(&m);
            let s = sender.input_sender().clone();
            let (account_id, id) = (m.account_id, m.id);
            let label = if showing {
                i18n("Block Remote Content")
            } else {
                i18n("Show Remote Content")
            };
            let icon = if showing { "security-high" } else { "image-x-generic" };
            sections.push(vec![MenuEntry::new(label, move || {
                let _ = s.send(AppMsg::SetRemoteContent { account_id, id, show: !showing });
            })
            .icon(format!("co.hyprlab.Hylki-{icon}-symbolic"))]);
        }
        sections.push(vec![item(RowAction::ViewSource, i18n("View Source"), "code")]);
        show_context_menu(&self.window, x, y, sections);
    }

    /// Whether a read/unread change for a message in `path` is still on its
    /// way to the server (and not so old that the worker must have lost it).
    fn pending_seen_in(&self, account_id: u32, path: &str) -> bool {
        self.pending_seen.iter().any(|((a, p, _), (_, at))| {
            *a == account_id && p == path && at.elapsed() < PENDING_SEEN_MAX
        })
    }

    fn pending_seen_in_folder(&self, account_id: u32, folder_id: u32) -> bool {
        self.folders
            .get(&account_id)
            .and_then(|fs| fs.iter().find(|f| f.id == folder_id))
            .is_some_and(|f| self.pending_seen_in(account_id, &f.path))
    }

    /// Overlay the read/unread changes still on their way to the server on
    /// a folder list the worker fetched before storing them.
    fn apply_pending_seen(
        &self,
        account_id: u32,
        folder_id: u32,
        mut messages: Vec<Message>,
    ) -> Vec<Message> {
        let Some(path) = self
            .folders
            .get(&account_id)
            .and_then(|fs| fs.iter().find(|f| f.id == folder_id))
            .map(|f| f.path.clone())
        else {
            return messages;
        };
        if !self.pending_seen_in(account_id, &path) {
            return messages;
        }
        for m in messages.iter_mut() {
            if let Some((seen, at)) = self.pending_seen.get(&(account_id, path.clone(), m.uid)) {
                if at.elapsed() < PENDING_SEEN_MAX {
                    m.unread = !seen;
                }
            }
        }
        messages
    }

    /// Switch the message list to a folder: reset the view, show its cached
    /// messages instantly (if any), and kick off a background sync. Shared by the
    /// sidebar selection and the "open message from notification" flow.
    fn select_folder(&mut self, account_id: u32, folder_id: u32, _name: String, path: String) {
        self.leave_gallery();
        self.showing_contacts = false;
        self.showing_outbox = false;
        // Mirror the selection in the sidebar. Navigation that starts in the
        // sidebar hits its already-selected guard; navigation from anywhere
        // else ("Go to Message", a notification) moves the highlight — which
        // also lets the Attachments row be clicked again to return.
        self.sidebars_emit(SidebarInput::SelectFolderRow {
            account_id,
            path: path.clone(),
        });
        self.unified = false;
        self.tag_view = None;
        self.attachments.clear();
        self.sync_attachment_drawer();
        self.attachments_loading = false;
        self.message_list.emit(MessageListInput::SetSelected(None));
        self.message_list.emit(MessageListInput::SetColorize(false));
        self.message_list.emit(MessageListInput::ResetPaging);
        // A Sent folder's rows all come from you — name the recipients (#27).
        let is_sent = self
            .folders
            .get(&account_id)
            .and_then(|fs| fs.iter().find(|f| f.id == folder_id))
            .is_some_and(|f| f.kind == FolderKind::Sent);
        self.message_list.emit(MessageListInput::SetShowRecipient(is_sent));
        // Trash and Junk offer the way back to the Inbox (#138); Junk's is
        // "Not Spam" (#168).
        let kind = self.folder_kind(account_id, folder_id);
        let restorable = kind.is_some_and(|k| matches!(k, FolderKind::Trash | FolderKind::Junk));
        self.message_list.emit(MessageListInput::SetRestorable(restorable));
        self.message_list.emit(MessageListInput::SetInJunk(kind == Some(FolderKind::Junk)));
        self.message_list.emit(MessageListInput::SetInDrafts(kind == Some(FolderKind::Drafts)));
        self.message_view.emit(MessageViewInput::SetDraftsView(kind == Some(FolderKind::Drafts)));
        self.selected = Some(SelectedFolder {
            account_id,
            folder_id,
            path: path.clone(),
        });
        self.current = None;
        self.current_thread.clear();
        self.show_message(None, false);
        match self.message_cache.get(&(account_id, folder_id)) {
            Some(cached) => self.message_list.emit(MessageListInput::SetMessages {
                messages: cached.clone(),
            }),
            None => self.message_list.emit(MessageListInput::SetLoading),
        }
        self.push_index_complete();
        self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
    }

    fn show_message(&self, message: Option<Message>, loading: bool) {
        let allow_remote = message.as_ref().is_some_and(|m| self.remote_allowed(m));
        let (account_name, account_color) = match message.as_ref() {
            Some(m) => (
                Some(self.account_name(m.account_id)),
                Some(self.account_color(m.account_id)),
            ),
            None => (None, None),
        };
        let stored_check = message
            .as_ref()
            .and_then(|m| self.sender_cache.get(&(m.account_id, m.id)))
            .cloned();
        self.message_view.emit(MessageViewInput::Show {
            thread: message.into_iter().collect(),
            allow_remote,
            account_name,
            account_color,
            loading,
            primary: None, // a single message is its own primary
            folder_labels: HashMap::new(),
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

    /// Render the current conversation (thread) in the reader, newest first.
    ///
    /// The header follows the message the user selected, not whatever sorted to
    /// the top: a conversation can now include replies of your own pulled in from
    /// Sent, and one of those being the newest shouldn't retitle the reader.
    /// Ask for one conversation re-render shortly from now, collapsing a burst of
    /// arriving bodies into a single one.
    ///
    /// A conversation is rendered as one document with every member's body
    /// inlined and handed to WebKit. Rendering per arriving body meant N loads of
    /// an N-body document, issued far faster than WebKit could retire them.
    fn queue_thread_render(&mut self, sender: &ComponentSender<Self>) {
        if self.thread_render_queued {
            return;
        }
        self.thread_render_queued = true;
        let s = sender.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
            s.input(AppMsg::RenderThread);
        });
    }

    fn show_thread(&self) {
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
            primary: Some(Box::new(primary)),
            // Nothing to wait for when the conversation was already assembled
            // and its bodies are in hand.
            instant: self.thread_painted && !loading,
        });
        self.push_member_checks();
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
                let name = self
                    .folders
                    .get(&m.account_id)?
                    .iter()
                    .find(|f| f.id == m.folder_id)
                    .map(|f| f.name.clone())?;
                Some(((m.account_id, m.id), name))
            })
            .collect()
    }

    /// Pop a message out into its own standalone window with a dedicated
    /// reader. `thread` is the whole conversation when `m` heads one (the
    /// window then shows every card); empty for a single message.
    fn open_message_window(
        &mut self,
        m: Message,
        thread: Vec<Message>,
        sender: &ComponentSender<Self>,
    ) {
        let key = (m.account_id, m.id);
        // Already open? Just bring it forward.
        if let Some(p) = self.popouts.get(&key) {
            p.window.present();
            return;
        }
        let account_id = m.account_id;

        // Reuse an already-fetched body so the window renders instantly.
        let cached_body = if !m.body.is_empty() {
            Some(m.body.clone())
        } else if self
            .current
            .as_ref()
            .is_some_and(|c| c.id == m.id && c.account_id == account_id && !c.body.is_empty())
        {
            self.current.as_ref().map(|c| c.body.clone())
        } else {
            self.body_cache.get(&key).cloned()
        };
        let needs_body = cached_body.is_none();

        let mut display = m.clone();
        display.unread = false;
        if let Some(body) = cached_body {
            display.body = body;
        }

        // Fetch the body unless the in-flight selection request already will
        // (single click precedes the double click), to avoid a duplicate fetch.
        let already_loading = self
            .current
            .as_ref()
            .is_some_and(|c| c.id == m.id && c.account_id == account_id);
        if needs_body && !already_loading {
            if let Some(path) = self.resolve_folder_path(&m) {
                self.send_to(account_id, MailRequest::LoadBody {
                    message_id: m.id,
                    path,
                    uid: m.uid,
                });
            }
        }

        // Attachments: use the in-memory cache if present; otherwise fetch
        // them (disk cache first, then the server) and route the reply to
        // this window, just like the main reader does.
        let mut atts: Vec<Attachment> = Vec::new();
        let mut atts_loading = false;
        if display.has_attachment {
            if let Some(cached) = self.attachment_cache.get(&key).cloned() {
                atts = cached;
            } else if let Some(path) = self.resolve_folder_path(&m) {
                atts_loading = true;
                self.send_to(account_id, MailRequest::LoadAttachments {
                    message_id: m.id,
                    path,
                    uid: m.uid,
                    download: true,
                });
            }
        }

        // The conversation for the window: the assembled cache when this
        // thread has been opened before (cross-folder members and bodies
        // included), else what the list handed over. Members still missing
        // bodies get them fetched; the replies land via SetBody below.
        let mut thread = if thread.len() > 1 {
            self.thread_cache.get(&key).cloned().unwrap_or(thread)
        } else {
            thread
        };
        for member in &mut thread {
            let mkey = (member.account_id, member.id);
            if mkey == key {
                member.body = display.body.clone();
                member.unread = false;
                continue;
            }
            if member.body.is_empty() {
                if let Some(body) = self.body_cache.get(&mkey) {
                    member.body = body.clone();
                } else if let Some(path) = self.resolve_folder_path(member) {
                    self.send_to(member.account_id, MailRequest::LoadBody {
                        message_id: member.id,
                        path,
                        uid: member.uid,
                    });
                }
            }
        }

        let allow_remote = self.remote_allowed(&display);
        let init = MessageWindowInit {
            message: display,
            thread,
            account_name: Some(self.account_name(account_id)),
            account_color: Some(self.account_color(account_id)),
            allow_remote,
            loading: needs_body,
            attachments: atts,
            attachments_available: false,
            attachments_loading: atts_loading,
            content_dark: self.message_theme.dark_override(),
            reader_style: self.reader_style(),
            reader_mode: self.effective_reader_mode(),
            zoom: self.zoom,
            zoom_default: self.zoom_default,
            reader_switch: self.reader_switch,
            reader_default: self.effective_reader_default(),
            tags: self.tags.clone(),
        };

        let controller = MessageWindow::builder()
            .launch(init)
            .forward(sender.input_sender(), move |out| match out {
                MessageWindowOutput::Action { action, message } => {
                    // The window's actions name the card they came from.
                    AppMsg::RowAction { action, message, conversation: Vec::new() }
                }
                MessageWindowOutput::AddToContacts { name, email } => {
                    AppMsg::AddContactFrom { name, email }
                }
                MessageWindowOutput::LoadAttachments(message) => AppMsg::LoadAttachmentsFor(message),
                MessageWindowOutput::OpenAttachment(att) => AppMsg::OpenAttachmentItem(att),
                MessageWindowOutput::SaveAllAttachments(items) => AppMsg::SaveAttachmentItems(items),
                MessageWindowOutput::AllowSender(addr) => AppMsg::AllowSender(addr),
                MessageWindowOutput::ReloadBody(m) => AppMsg::ReloadBody(m),
                MessageWindowOutput::Notice(text) => AppMsg::Notice(text),
                MessageWindowOutput::ReaderMode(on) => AppMsg::SetReaderMode(on),
                MessageWindowOutput::ZoomReset => AppMsg::ZoomMessage(0),
                MessageWindowOutput::Unsubscribe { message, info } => {
                    AppMsg::Unsubscribe { message, info }
                }
                MessageWindowOutput::InviteAction { message, invite, action } => {
                    AppMsg::InviteAction { message, invite, action }
                }
                MessageWindowOutput::ComposeTo(addr) => AppMsg::ComposeTo(addr),
                MessageWindowOutput::Closed => AppMsg::PopoutClosed(key),
            });

        let window = controller.widget().clone();
        window.set_transient_for(Some(&self.window));
        window.present();
        controller.emit(MessageWindowInput::SetUnsubscribed(self.unsubscribed_map()));
        controller.emit(MessageWindowInput::SetIdentities(self.identities_map()));
        controller.emit(MessageWindowInput::SetInviteAnswers(self.invite_answers_map()));

        self.popouts.insert(key, PopOut { window, controller });
    }

    /// Push every cached sender verdict for the on-screen conversation into
    /// the reader, so each card's header seal lights up (#88).
    fn push_member_checks(&self) {
        // A single message never fills current_thread — the open message
        // itself is the one card then.
        let mut keys: Vec<(u32, u32)> =
            self.current_thread.iter().map(|m| (m.account_id, m.id)).collect();
        if let Some(c) = &self.current {
            if keys.is_empty() {
                keys.push((c.account_id, c.id));
            }
        }
        for key in keys {
            if let Some(check) = self.sender_cache.get(&key) {
                self.message_view.emit(MessageViewInput::SenderCheckFor {
                    account_id: key.0,
                    id: key.1,
                    check: check.clone(),
                });
            }
        }
    }

    /// Tooltip for the toolbar's trash button: says when it will delete the
    /// whole multi-selection rather than just the open message.
    /// Whether the reader header shows `item` right now: it must be in the
    /// saved layout, and a right-group button also needs the pane wide
    /// enough (the left group never folds). Delete alone also serves the
    /// Outbox; the inline composer hides the whole row.
    fn toolbar_visible(&self, item: config::ToolbarItem) -> bool {
        use config::ToolbarSide;
        self.reader_compose.is_none()
            && (item == config::ToolbarItem::Delete || !self.showing_outbox)
            && match self.reader_toolbar.side(item) {
                Some(ToolbarSide::Left) => true,
                Some(ToolbarSide::Right) => !self.reader_actions_collapsed,
                None => false,
            }
    }

    /// The ⋯ overflow stands in for the folded right group: only while
    /// collapsed, and only when that group has something to offer (the
    /// Outbox keeps View Source there regardless).
    fn reader_overflow_wanted(&self) -> bool {
        // Focus Mode folds the whole row in, so the ⋯ is the toolbar.
        if self.focus.active(config::FocusPart::ReaderToolbar) {
            return true;
        }
        self.reader_actions_collapsed && (self.showing_outbox || !self.reader_toolbar.right.is_empty())
    }

    /// Pack the header's action buttons in the saved order: the left group
    /// via pack_start, the right group via pack_end (which fills right to
    /// left, so in reverse), then the attachments spinner so it keeps the
    /// innermost slot of the right group. Buttons on neither side are left
    /// unpacked. Re-folds the row for the new count afterwards.
    fn relayout_reader_toolbar(&self) {
        let (Some(header), Some(tb)) = (self.reader_header.get(), self.reader_toolbar_widgets.get()) else {
            return;
        };
        for (_, w) in &tb.buttons {
            if w.parent().is_some() {
                header.remove(w);
            }
        }
        if tb.spinner.parent().is_some() {
            header.remove(&tb.spinner);
        }
        let find = |item: config::ToolbarItem| tb.buttons.iter().find(|(i, _)| *i == item).map(|(_, w)| w);
        // Each button's Focus Mode revealer slides towards the edge its
        // group sits against: GTK's SlideRight moves a folding child off to
        // the left, SlideLeft keeps it anchored and shrinks it from its
        // right, which is what an end-packed group needs.
        for item in &self.reader_toolbar.left {
            if let Some(w) = find(*item) {
                if let Some(r) = w.downcast_ref::<gtk::Revealer>() {
                    r.set_transition_type(gtk::RevealerTransitionType::SlideRight);
                }
                header.pack_start(w);
            }
        }
        for item in self.reader_toolbar.right.iter().rev() {
            if let Some(w) = find(*item) {
                if let Some(r) = w.downcast_ref::<gtk::Revealer>() {
                    r.set_transition_type(gtk::RevealerTransitionType::SlideLeft);
                }
                header.pack_end(w);
            }
        }
        header.pack_end(&tb.spinner);
        self.refresh_reader_breakpoint();
    }

    /// Point the fold breakpoint at the width the current layout needs: the
    /// row's fixed cost, the window controls, and one button's worth per
    /// shown item (hidden-for-now buttons like Tags without tags count too —
    /// that slack keeps the fold a step ahead of a squeeze).
    fn refresh_reader_breakpoint(&self) {
        let Some(tb) = self.reader_toolbar_widgets.get() else {
            return;
        };
        let shown = (self.reader_toolbar.left.len() + self.reader_toolbar.right.len()) as i32;
        let threshold = if tb.button_w <= 0 {
            READER_ACTIONS_BREAKPOINT
        } else {
            (tb.base + tb.controls.get() + shown * tb.button_w) as f64 + 24.0
        };
        tracing::info!(
            "reader toolbar: {shown} buttons × {}px + base {}px + controls {}px → collapse below {threshold:.0}px",
            tb.button_w,
            tb.base,
            tb.controls.get()
        );
        tb.breakpoint.set_condition(Some(&adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            threshold,
            adw::LengthUnit::Px,
        )));
        tb.bin.queue_resize();
    }

    fn delete_tooltip(&self) -> String {
        match self.list_selection.len() {
            n if n > 1 => i18n_f("Delete {n} messages", &[("n", &n.to_string())]),
            _ => i18n("Delete"),
        }
    }

    /// Whether the given folder is the account's Drafts folder.
    fn is_drafts_folder(&self, account_id: u32, folder_id: u32) -> bool {
        self.folder_kind(account_id, folder_id) == Some(FolderKind::Drafts)
    }

    /// The kind of a folder by id, if known.
    fn folder_kind(&self, account_id: u32, folder_id: u32) -> Option<FolderKind> {
        self.folders
            .get(&account_id)?
            .iter()
            .find(|f| f.id == folder_id)
            .map(|f| f.kind)
    }

    /// Open a draft for editing: reuse a cached body if we have one, otherwise
    /// fetch it and open the editor once it arrives (see the `Body` handler).
    /// `inline` puts the editor in the reading pane (a selected draft);
    /// otherwise it gets a window (a draft opened by double-click or Enter).
    /// `extra` carries files handed in from GNOME Files to attach (or
    /// upload) on top of the draft's own.
    fn open_draft(&mut self, m: Message, inline: bool, extra: HandOffFiles, sender: &ComponentSender<Self>) {
        let body = if !m.body.is_empty() {
            Some(m.body.clone())
        } else {
            self.body_cache.get(&(m.account_id, m.id)).cloned()
        };
        match body {
            Some(html) => self.compose_from_draft(m, html, inline, extra, sender),
            None => {
                if let Some(path) = self.resolve_folder_path(&m) {
                    self.send_to(
                        m.account_id,
                        MailRequest::LoadBody { message_id: m.id, path, uid: m.uid },
                    );
                }
                self.pending_draft = Some((m, inline, extra));
            }
        }
    }

    /// Render a queued message in the reader. Its bytes are stored with it, so
    /// this is a local render — the same one the worker would produce.
    fn show_outbox_message(&mut self, item: &crate::models::OutboxItem) {
        let mut message = item.as_message();
        message.body = crate::worker::extract_body(&item.raw);
        message.date = crate::models::OutboxItem::waiting_label(item.queued_at);
        self.attachments = crate::worker::extract_attachments_of(&item.raw);
        self.attachments_loading = false;
        self.sync_attachment_drawer();
        self.current = Some(message.clone());
        self.current_thread.clear();
        self.show_message(Some(message), false);
    }

    /// Open a queued Outbox message in the composer.
    ///
    /// The queued copy stays put until the edited version is handed back to the
    /// worker: an editor that is closed again must not have destroyed the only
    /// copy of the message. Attachments are written to a private temp directory,
    /// because the composer attaches files by path and the originals are long
    /// gone (under Flatpak the portal's paths expire).
    fn compose_from_outbox(&mut self, account_id: u32, id: u32, sender: &ComponentSender<Self>) {
        let Some(item) = self
            .outbox_by_account
            .get(&account_id)
            .and_then(|items| items.iter().find(|i| i.id == id))
            .cloned()
        else {
            return;
        };
        let editable = crate::worker::editable_from_raw(&item.raw, &item.rcpts);

        let attachments =
            stage_attachments(&format!("hylki-outbox-{account_id}-{id}"), &editable.attachments);

        let prefill = ComposePrefill {
            to: editable.to,
            cc: editable.cc,
            bcc: editable.bcc,
            subject: editable.subject,
            body_html: editable.body_html,
            attachments,
            // Re-editing a queued message: it keeps whatever thread it was
            // already part of (carried in the raw MIME it was built from).
            in_reply_to: String::new(),
            references: String::new(),
            draft_origin: None,
            encrypt: false,
            outbox_origin: Some(id),
            reply_addressed_to: String::new(),
            from_address: String::new(),
            send_at: item.send_at,
            cloud_uploads: Vec::new(),
        };
        // The Outbox stays the folder on screen: its list is still what's listed,
        // so its toolbar has to stay too. Leaving it would strand the user in a
        // reader offering Reply and Forward for a message that hasn't been sent.
        self.open_compose(account_id, prefill, sender);
    }

    /// Open the compose editor pre-filled from a draft, remembering its origin so
    /// saving/sending replaces it. Inline, the editor covers the reading pane
    /// the way a new message does, with the pane cleared beneath it: the draft
    /// is what is selected, so nothing older should reappear when the editor
    /// closes.
    fn compose_from_draft(
        &mut self,
        m: Message,
        body_html: String,
        inline: bool,
        extra: HandOffFiles,
        sender: &ComponentSender<Self>,
    ) {
        let path = self.resolve_folder_path(&m).unwrap_or_default();
        let prefill = ComposePrefill {
            to: m.to.clone(),
            cc: m.cc.clone(),
            subject: m.subject.clone(),
            body_html,
            draft_origin: Some(crate::models::DraftOrigin {
                account_id: m.account_id,
                folder_id: m.folder_id,
                path,
                uid: m.uid,
            }),
            attachments: extra.attach,
            cloud_uploads: extra.cloud,
            ..Default::default()
        };
        if inline {
            self.attachments.clear();
            self.attachments_loading = false;
            self.sync_attachment_drawer();
            self.current = None;
            self.current_thread.clear();
            self.show_message(None, false);
            self.open_inline_reply(m.account_id, prefill, None, sender);
        } else {
            self.open_compose(m.account_id, prefill, sender);
        }
    }

    /// Forward a message with its attachments (#240): the quoted original
    /// in the body, and every file that came with it attached to the new
    /// message, as Thunderbird, Apple Mail and Gmail do. Much of what gets
    /// forwarded is forwarded for the file. A reply deliberately does not
    /// do this: the sender already has what they sent.
    ///
    /// Built like `edit_as_new`: the body and the attachments may still be
    /// on the server, so each is fetched at most once with the message held
    /// in `pending_forward`, and the reply re-enters here. `inline` says
    /// where the composer opens, over the reading pane or in a window.
    fn forward(&mut self, m: Message, inline: bool, sender: &ComponentSender<Self>) {
        let m = self.with_cached_body(m);
        let key = (m.account_id, m.id);
        if m.body.is_empty() {
            if let Some(path) = self.resolve_folder_path(&m) {
                self.send_to(
                    m.account_id,
                    MailRequest::LoadBody { message_id: m.id, path, uid: m.uid },
                );
                self.pending_forward = Some((m, inline));
                return;
            }
        }
        if m.has_attachment && !self.attachment_cache.contains_key(&key) {
            if let Some(path) = self.resolve_folder_path(&m) {
                self.send_to(m.account_id, MailRequest::LoadAttachments {
                    message_id: m.id,
                    path,
                    uid: m.uid,
                    download: true,
                });
                self.pending_forward = Some((m, inline));
                return;
            }
        }
        let attachments = self
            .attachment_cache
            .get(&key)
            .map(|items| stage_attachments(&format!("hylki-forward-{}-{}", m.account_id, m.id), items))
            .unwrap_or_default();
        tracing::info!(
            "forward: {}:{} with {} attachment(s)",
            m.account_id,
            m.id,
            attachments.len()
        );
        let mut prefill = forward_prefill(&m);
        prefill.attachments = attachments;
        if inline {
            self.open_inline_reply(m.account_id, prefill, Some((m.account_id, m.id)), sender);
        } else {
            self.open_compose(m.account_id, prefill, sender);
        }
    }

    /// "Edit as New Message" (#232): the message opened in the composer as a
    /// message of its own — the same recipients, subject, body and
    /// attachments, with none of the threading headers and no tie to what it
    /// was copied from. Sending it sends a new message; the original is left
    /// exactly where it was.
    ///
    /// The body and the attachments may still be on the server. Each is
    /// fetched at most once, and the reply re-enters here: the message is
    /// held in `pending_edit_as_new` meanwhile, so nothing opens until what
    /// is being copied is actually in hand.
    fn edit_as_new(&mut self, m: Message, sender: &ComponentSender<Self>) {
        let m = self.with_cached_body(m);
        let key = (m.account_id, m.id);
        if m.body.is_empty() {
            if let Some(path) = self.resolve_folder_path(&m) {
                self.send_to(
                    m.account_id,
                    MailRequest::LoadBody { message_id: m.id, path, uid: m.uid },
                );
                self.pending_edit_as_new = Some(m);
                return;
            }
        }
        if m.has_attachment && !self.attachment_cache.contains_key(&key) {
            if let Some(path) = self.resolve_folder_path(&m) {
                self.send_to(m.account_id, MailRequest::LoadAttachments {
                    message_id: m.id,
                    path,
                    uid: m.uid,
                    download: true,
                });
                self.pending_edit_as_new = Some(m);
                return;
            }
        }
        let attachments = self
            .attachment_cache
            .get(&key)
            .map(|items| {
                stage_attachments(&format!("hylki-copy-{}-{}", m.account_id, m.id), items)
            })
            .unwrap_or_default();
        tracing::info!(
            "edit as new: copying {}:{} ({} attachment(s))",
            m.account_id,
            m.id,
            attachments.len()
        );
        let prefill = ComposePrefill {
            to: m.to.clone(),
            cc: m.cc.clone(),
            subject: m.subject.clone(),
            body_html: editable_copy_html(&m.body),
            attachments,
            ..Default::default()
        };
        // A new message, so it opens where a new message opens.
        if self.compose_inline {
            self.open_inline_reply(m.account_id, prefill, None, sender);
        } else {
            self.open_compose(m.account_id, prefill, sender);
        }
    }

    /// Assemble the `ComposeInit` for a composer (from-accounts + signatures,
    /// autocomplete suggestions, a fresh id, host mode).
    fn build_compose_init(
        &mut self,
        account_id: u32,
        prefill: ComposePrefill,
        windowed: bool,
        can_toggle: bool,
    ) -> (u32, ComposeInit) {
        // Selectable "from" identities, in display order: each account, then
        // one entry per send-as alias it defines (#34) — same transport and
        // signature, a different From on the wire. Built from the CONFIG, not
        // the live account list: launched cold by a mailto/file hand-off
        // (Nautilus's "Send by email", #105) the composer opens before any
        // worker has connected, and the live list is still empty — which
        // hid the From row entirely.
        // The demo's stand-in accounts count here too, so a demo reply
        // carries a signature like a real one.
        let config = self.effective_config();
        let mut emails: Vec<String> = Vec::new();
        for email in &self.account_order {
            if config.iter().any(|c| c.enabled && &c.email == email)
                && !emails.contains(email)
            {
                emails.push(email.clone());
            }
        }
        for c in config.iter().filter(|c| c.enabled) {
            if !emails.contains(&c.email) {
                emails.push(c.email.clone());
            }
        }
        let accounts: Vec<ComposeAccount> = emails
            .iter()
            .flat_map(|email| {
                let Some((idx, cfg)) =
                    config.iter().enumerate().find(|(_, c)| &c.email == email)
                else {
                    return Vec::new();
                };
                let id = idx as u32 + 1;
                let label = if cfg.name.trim().is_empty() {
                    cfg.email.clone()
                } else {
                    format!("{} <{}>", cfg.name, cfg.email)
                };
                let cfg = Some(cfg);
                let signature = cfg.and_then(|c| c.signature.clone()).unwrap_or_default();
                let pgp_key = cfg.and_then(|c| c.pgp_key.clone());
                let mut identities = vec![ComposeAccount {
                    id,
                    label,
                    signature: signature.clone(),
                    email: email.clone(),
                    pgp_key: pgp_key.clone(),
                    alias_from: None,
                }];
                for alias in cfg.map(|c| c.aliases.as_slice()).unwrap_or_default() {
                    let (name, addr) = split_identity(&alias.identity);
                    if addr.is_empty() {
                        continue;
                    }
                    let display = if name.is_empty() {
                        addr.clone()
                    } else {
                        format!("{name} <{addr}>")
                    };
                    identities.push(ComposeAccount {
                        id,
                        label: display.clone(),
                        signature: signature.clone(),
                        email: addr,
                        pgp_key: pgp_key.clone(),
                        alias_from: Some(display),
                    });
                }
                identities
            })
            .collect();
        // Default identity: for a reply, whichever of the account's addresses
        // the original was sent to — mail to an alias is answered as the alias.
        // For a new message, the default sender chosen in Settings (#157), if
        // it is one of this account's addresses. Otherwise (and when nothing
        // matches) the account's own address.
        let hay = prefill.reply_addressed_to.to_lowercase();
        let want = prefill.from_address.trim().to_lowercase();
        let selected = (!hay.is_empty())
            .then(|| {
                accounts.iter().position(|c| {
                    c.id == account_id && hay.contains(c.email.to_lowercase().as_str())
                })
            })
            .flatten()
            .or_else(|| {
                (!want.is_empty()).then(|| {
                    accounts
                        .iter()
                        .position(|c| c.id == account_id && c.email.to_lowercase() == want)
                })
                .flatten()
            })
            .or_else(|| {
                accounts
                    .iter()
                    .position(|c| c.id == account_id && c.alias_from.is_none())
            })
            .unwrap_or(0);

        // The user's own addresses go last in the recipient suggestions.
        let own: Vec<(String, String)> = self.config.iter().map(|c| (c.name.clone(), c.email.clone())).collect();
        let id = self.next_compose_id;
        self.next_compose_id += 1;
        let init = ComposeInit {
            compose_id: id,
            prefill,
            accounts,
            selected,
            suggestions: crate::contacts::suggestions(&own),
            windowed,
            can_toggle,
            compact: false,
            decorations: true,
            format: self.compose_format,
            signature_position: self.signature_position,
        };
        (id, init)
    }

    /// Launch a `Compose` component, forwarding its outputs into `AppMsg`.
    fn spawn_compose(
        &self,
        init: ComposeInit,
        sender: &ComponentSender<Self>,
    ) -> Controller<Compose> {
        Compose::builder()
            .launch(init)
            .forward(sender.input_sender(), |out| match out {
                ComposeOutput::Send(msg) => AppMsg::SendMessage(msg),
                ComposeOutput::SaveDraft(msg) => AppMsg::SaveDraftMessage(msg),
                ComposeOutput::DeleteDraft { id, origin } => AppMsg::DeleteDraft { id, origin },
                ComposeOutput::ToggleWindow(id) => AppMsg::ComposeToggleWindow(id),
                ComposeOutput::Close(id) => AppMsg::ComposeClosed(id),
                ComposeOutput::History { id, undo, redo } => {
                    AppMsg::ComposeHistory { id, undo, redo }
                }
            })
    }

    /// Host a compose pane in a fresh standalone window, transient for the app.
    fn compose_window_host(
        &self,
        content: &impl IsA<gtk::Widget>,
        id: u32,
        sender: &ComponentSender<Self>,
    ) -> adw::Window {
        let win = adw::Window::builder()
            .modal(false)
            // Room for the full compose toolbar (Cancel, Save Draft, the
            // action icons, Send) before it folds into its ⋯ menu.
            .default_width(720)
            .default_height(760)
            .title(&i18n("New Message"))
            .transient_for(&self.window)
            .build();
        win.set_content(Some(content));
        let s = sender.input_sender().clone();
        win.connect_close_request(move |_| {
            let _ = s.send(AppMsg::ComposeClosed(id));
            gtk::glib::Propagation::Proceed
        });
        win.connect_is_active_notify(|w| {
            if w.is_active() {
                crate::notify::withdraw_compose_ready();
            }
        });
        win.present();
        win
    }

    /// The tail of a composer opened from outside the app (a mailto: link, a
    /// file manager's "Send by email" or "Open With"): say which files could
    /// not be attached, and if the window has not come to the front by the
    /// time the compositor would have let it, post a desktop alert whose
    /// click brings it up. The activation token a launcher passes along is
    /// what lets a window take the focus; a stale or missing one (a link
    /// relayed from a terminal, an older launcher) leaves the window where it
    /// was, behind whatever the user is looking at, and this is the next
    /// best thing.
    fn after_hand_off(&self, attached: usize, dropped: Vec<std::path::PathBuf>) {
        if !dropped.is_empty() {
            let names: Vec<String> = dropped
                .iter()
                .map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string()))
                .collect();
            let text = crate::i18n::ni18n_f(
                "Could not attach {names}: Hylki cannot read the file.",
                "Could not attach {n} files (Hylki cannot read them): {names}",
                dropped.len() as u32,
                &[("n", &dropped.len().to_string()), ("names", &names.join(", "))],
            );
            self.notifications.emit(NotifyInput::Push { text, error: true, connectivity: false });
        }
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(900), move || {
            // Any window of ours in front will do: the composer, the main
            // window under an inline composer, or the dialog asking where
            // the files should go.
            let tops = gtk::Window::toplevels();
            let ours = (0..tops.n_items())
                .filter_map(|i| tops.item(i))
                .filter_map(|o| o.downcast::<gtk::Window>().ok())
                .any(|w| w.is_active());
            if !ours {
                tracing::info!("hand-off: window did not get the focus, posting an alert");
                crate::notify::compose_ready(attached as u32);
            }
        });
    }

    /// Files handed in from outside (Send with Hylki, Open With, Email…):
    /// the first step. Reports the unreadable ones, then goes where
    /// Settings → System → GNOME Files says: straight into a new message, a
    /// draft or a reply, or a dialog offering the three.
    fn begin_hand_off(&mut self, hand_off: FileHandOff, sender: &ComponentSender<Self>) {
        self.leave_gallery();
        let dropped = hand_off.dropped.clone();
        let attached = hand_off.files.len();
        // The focus watch runs from here: whichever window opens next (the
        // dialog or the composer) is the one that should come to the front.
        self.after_hand_off(attached, dropped);
        let action = self.files_prefs.action;
        if action == config::FilesAction::Ask {
            self.ask_hand_off_action(hand_off, sender);
        } else {
            self.hand_off_action(hand_off, action, sender);
        }
    }

    /// Hand the current Files preferences to an open Settings window, so a
    /// "remember this" choice in a dialog shows there at once.
    fn push_files_prefs(&self) {
        if let Some(p) = self.prefs.as_ref() {
            p.emit(PrefInput::SetFilesPrefs(self.files_prefs));
        }
    }

    /// The "what should these files go into" dialog.
    fn ask_hand_off_action(&self, hand_off: FileHandOff, sender: &ComponentSender<Self>) {
        let n = hand_off.files.len() as u32;
        let names = hand_off_names(&hand_off.files);
        let size = crate::cloud::human_size(hand_off_size(&hand_off.files));
        let dialog = adw::MessageDialog::new(
            Some(&self.window),
            Some(ni18n_f("Send {n} file with Hylki", "Send {n} files with Hylki", n, &[("n", &n.to_string())]).as_str()),
            Some(i18n_f("{names} ({size}). What should the files go into?", &[("names", &names), ("size", &size)]).as_str()),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("reply", &i18n("Reply to a Message…"));
        dialog.add_response("draft", &i18n("Continue a Draft…"));
        dialog.add_response("new", &i18n("New Message"));
        dialog.set_response_appearance("new", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("new"));
        dialog.set_close_response("cancel");
        let remember = remember_check();
        dialog.set_extra_child(Some(&remember));
        let hand_off = std::rc::Rc::new(std::cell::RefCell::new(Some(hand_off)));
        let s = sender.clone();
        dialog.connect_response(None, move |_, resp| {
            let Some(hand_off) = hand_off.borrow_mut().take() else { return };
            let action = match resp {
                "new" => config::FilesAction::New,
                "draft" => config::FilesAction::Draft,
                "reply" => config::FilesAction::Reply,
                _ => {
                    tracing::info!("file hand-off: cancelled");
                    return;
                }
            };
            s.input(AppMsg::HandOffAction { hand_off, action, remember: remember.is_active() });
        });
        dialog.present();
    }

    /// A destination is known: a new message goes on to the size check;
    /// a draft or reply first needs one picked.
    fn hand_off_action(&mut self, hand_off: FileHandOff, action: config::FilesAction, sender: &ComponentSender<Self>) {
        match action {
            config::FilesAction::Ask | config::FilesAction::New => {
                self.hand_off_size_check(hand_off, HandOffTarget::New, sender);
            }
            config::FilesAction::Draft => {
                // Drafts folders never listed this run are fetched first,
                // so the picker does not miss a draft the cache has not
                // seen; a worker that stays silent is given three seconds.
                let mut waiting = HashSet::new();
                for (account_id, drafts) in self.drafts_folders() {
                    if !self.message_cache.contains_key(&(account_id, drafts.id)) {
                        waiting.insert((account_id, drafts.id));
                        self.send_to(account_id, MailRequest::SyncFolder { folder_id: drafts.id, path: drafts.path.clone() });
                    }
                }
                if waiting.is_empty() {
                    self.show_draft_picker(hand_off, sender);
                } else {
                    self.pending_draft_pick = Some((hand_off, waiting));
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || s.input(AppMsg::HandOffDraftPickNow));
                }
            }
            config::FilesAction::Reply => {
                let messages = self.messages_for_pick();
                let s = sender.clone();
                show_message_picker(
                    self.window.upcast_ref(),
                    &i18n("Reply to a Message"),
                    &i18n("The files go into a reply to the message you pick."),
                    &i18n("No messages to reply to yet."),
                    &i18n("Reply With the Files"),
                    messages,
                    hand_off,
                    move |hand_off, message| s.input(AppMsg::HandOffReply { hand_off, message }),
                );
            }
        }
    }

    /// Over the size limit, with cloud storage set up: attach anyway, or
    /// upload and link, per the preference or a dialog.
    fn hand_off_size_check(&mut self, hand_off: FileHandOff, target: HandOffTarget, sender: &ComponentSender<Self>) {
        let total = hand_off_size(&hand_off.files);
        let limit = self.files_prefs.limit_bytes();
        let cloud_ready = !crate::cloud::load_enabled_accounts().is_empty();
        if total <= limit || !cloud_ready {
            if total > limit {
                tracing::info!("file hand-off: {total} bytes over the {limit} limit, but no cloud storage is set up");
            }
            self.hand_off_open(hand_off, target, false, sender);
            return;
        }
        match self.files_prefs.large {
            config::FilesLarge::Attach => self.hand_off_open(hand_off, target, false, sender),
            config::FilesLarge::Cloud => self.hand_off_open(hand_off, target, true, sender),
            config::FilesLarge::Ask => {
                let n = hand_off.files.len() as u32;
                let names = hand_off_names(&hand_off.files);
                let dialog = adw::MessageDialog::new(
                    Some(&self.window),
                    Some(i18n("Large files").as_str()),
                    Some(
                        ni18n_f(
                            "{names} is {size}, over the {limit} limit. Upload it to cloud storage and put a download link in the message instead?",
                            "{names} add up to {size}, over the {limit} limit. Upload them to cloud storage and put download links in the message instead?",
                            n,
                            &[
                                ("names", &names),
                                ("size", &crate::cloud::human_size(total)),
                                ("limit", &crate::cloud::human_size(limit)),
                            ],
                        )
                        .as_str(),
                    ),
                );
                dialog.add_response("cancel", &i18n("Cancel"));
                dialog.add_response("attach", &i18n("Attach Anyway"));
                dialog.add_response("cloud", &i18n("Upload to Cloud Storage"));
                dialog.set_response_appearance("cloud", adw::ResponseAppearance::Suggested);
                dialog.set_default_response(Some("cloud"));
                dialog.set_close_response("cancel");
                let remember = remember_check();
                dialog.set_extra_child(Some(&remember));
                let pending = std::rc::Rc::new(std::cell::RefCell::new(Some((hand_off, target))));
                let s = sender.clone();
                dialog.connect_response(None, move |_, resp| {
                    let Some((hand_off, target)) = pending.borrow_mut().take() else { return };
                    let cloud = match resp {
                        "cloud" => true,
                        "attach" => false,
                        _ => {
                            tracing::info!("file hand-off: cancelled at the size check");
                            return;
                        }
                    };
                    s.input(AppMsg::HandOffOpen { hand_off, target, cloud, remember: remember.is_active() });
                });
                dialog.present();
            }
        }
    }

    /// The last step: open the composer the files were meant for.
    fn hand_off_open(&mut self, hand_off: FileHandOff, target: HandOffTarget, cloud: bool, sender: &ComponentSender<Self>) {
        let files = HandOffFiles::of(hand_off.files, cloud);
        tracing::info!(
            "file hand-off: {} to attach, {} to upload, into {}",
            files.attach.len(),
            files.cloud.len(),
            match &target {
                HandOffTarget::New => "a new message",
                HandOffTarget::Draft(_) => "a draft",
                HandOffTarget::Reply(_) => "a reply",
            }
        );
        match target {
            HandOffTarget::New => {
                let account = self
                    .current
                    .as_ref()
                    .map(|m| m.account_id)
                    .unwrap_or_else(|| self.active_account());
                let mut prefill = hand_off.base;
                prefill.attachments = files.attach;
                prefill.cloud_uploads = files.cloud;
                let (account, prefill) = self.new_message_from(account, prefill);
                if self.compose_inline {
                    self.open_inline_reply(account, prefill, None, sender);
                } else {
                    self.open_compose(account, prefill, sender);
                }
            }
            HandOffTarget::Draft(m) => {
                let inline = self.compose_inline;
                self.open_draft(m, inline, files, sender);
            }
            HandOffTarget::Reply(m) => {
                let body = if !m.body.is_empty() {
                    Some(m.body.clone())
                } else {
                    self.body_cache.get(&(m.account_id, m.id)).cloned()
                };
                match body {
                    Some(html) => {
                        let mut m = m;
                        m.body = html;
                        self.open_hand_off_reply(m, files, sender);
                    }
                    None => {
                        if let Some(path) = self.resolve_folder_path(&m) {
                            self.send_to(m.account_id, MailRequest::LoadBody { message_id: m.id, path, uid: m.uid });
                        }
                        self.pending_reply = Some((m, files));
                    }
                }
            }
        }
    }

    /// A reply to `m` (its body loaded) carrying handed-in files. Splits
    /// the reading pane when `m` is what it is showing, as Reply does; a
    /// message picked from elsewhere gets its reply in a window, the pane
    /// left as it was.
    fn open_hand_off_reply(&mut self, m: Message, files: HandOffFiles, sender: &ComponentSender<Self>) {
        let mut prefill = self.reply_pgp(&m, reply_prefill(&m));
        prefill.attachments = files.attach;
        prefill.cloud_uploads = files.cloud;
        let on_screen = self
            .current
            .as_ref()
            .is_some_and(|c| c.account_id == m.account_id && c.id == m.id)
            || self.current_thread.iter().any(|t| t.account_id == m.account_id && t.id == m.id);
        if on_screen {
            self.open_inline_reply(m.account_id, prefill, Some((m.account_id, m.id)), sender);
        } else {
            self.open_compose(m.account_id, prefill, sender);
        }
    }

    /// The "Continue a Draft" picker over every draft the app knows.
    fn show_draft_picker(&self, hand_off: FileHandOff, sender: &ComponentSender<Self>) {
        let drafts = self.drafts_for_pick();
        let s = sender.clone();
        show_message_picker(
            self.window.upcast_ref(),
            &i18n("Continue a Draft"),
            &i18n("The files go into the draft you pick."),
            &i18n("No drafts to continue."),
            &i18n("Attach to This Draft"),
            drafts,
            hand_off,
            move |hand_off, draft| s.input(AppMsg::HandOffDraft { hand_off, draft }),
        );
    }

    /// Each account's Drafts folder, in account order.
    fn drafts_folders(&self) -> Vec<(u32, Folder)> {
        let mut ids: Vec<u32> = self.folders.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter()
            .filter_map(|id| {
                let f = self.folders.get(&id)?.iter().find(|f| f.kind == FolderKind::Drafts)?;
                Some((id, f.clone()))
            })
            .collect()
    }

    /// Every account's saved drafts, newest first, each with its account's
    /// name for the picker.
    fn drafts_for_pick(&self) -> Vec<(Message, String)> {
        let mut out = Vec::new();
        for (account_id, drafts) in self.drafts_folders() {
            let list = match self.message_cache.get(&(account_id, drafts.id)) {
                Some(l) => l.clone(),
                None => self
                    .cache
                    .as_ref()
                    .map(|c| c.load_messages(account_id, &drafts.path, drafts.id))
                    .unwrap_or_default(),
            };
            let name = self.account_name(account_id);
            out.extend(list.into_iter().map(|m| (m, name.clone())));
        }
        out.sort_by_key(|(m, _)| std::cmp::Reverse(m.timestamp));
        out
    }

    /// Messages a reply could answer: what the reading pane shows first,
    /// then every account's Inbox (and the other folders listed this
    /// session, Drafts, Sent, Junk and Trash left out), newest first.
    fn messages_for_pick(&self) -> Vec<(Message, String)> {
        let mut seen: HashSet<(u32, u32)> = HashSet::new();
        let mut out = Vec::new();
        for m in self.current.iter().chain(self.current_thread.iter()) {
            if seen.insert((m.account_id, m.id)) {
                out.push((m.clone(), self.account_name(m.account_id)));
            }
        }
        let mut rest = Vec::new();
        let mut ids: Vec<u32> = self.folders.keys().copied().collect();
        ids.sort_unstable();
        for account_id in ids {
            let Some(folders) = self.folders.get(&account_id) else { continue };
            let name = self.account_name(account_id);
            for f in folders {
                if matches!(f.kind, FolderKind::Drafts | FolderKind::Sent | FolderKind::Junk | FolderKind::Trash) {
                    continue;
                }
                let list = match self.message_cache.get(&(account_id, f.id)) {
                    Some(l) => l.clone(),
                    None if f.kind == FolderKind::Inbox => self
                        .cache
                        .as_ref()
                        .map(|c| c.load_messages(account_id, &f.path, f.id))
                        .unwrap_or_default(),
                    None => Vec::new(),
                };
                for m in list {
                    if seen.insert((m.account_id, m.id)) {
                        rest.push((m, name.clone()));
                    }
                }
            }
        }
        rest.sort_by_key(|(m, _)| std::cmp::Reverse(m.timestamp));
        rest.truncate(500);
        out.extend(rest);
        out
    }

    /// Open a standalone compose window (New Message, compose-to, edit-draft).
    fn open_compose(
        &mut self,
        account_id: u32,
        prefill: ComposePrefill,
        sender: &ComponentSender<Self>,
    ) {
        let (id, init) = self.build_compose_init(account_id, prefill, true, false);
        let controller = self.spawn_compose(init, sender);
        let window = self.compose_window_host(controller.widget(), id, sender);
        self.composers.push(ComposeHost { id, controller, window });
    }

    /// Open (or replace) the reader's inline reply/forward drop-down pane.
    /// The revealer the inline composer should slide down in: the reader
    /// pane's, or — while the contacts view is up — the contact card's.
    fn compose_slot(&self) -> &gtk::Revealer {
        if self.showing_contacts {
            &self.contacts_compose_revealer
        } else {
            &self.reader_compose_revealer
        }
    }

    /// Hide and empty every inline-composer slot (only one ever holds it).
    fn clear_compose_slots(&self) {
        for r in [&self.reader_compose_revealer, &self.contacts_compose_revealer] {
            r.set_reveal_child(false);
            // Without this the hidden, pane-covering revealer keeps
            // swallowing every click and scroll over the pane beneath.
            r.set_can_target(false);
            r.set_child(None::<&gtk::Widget>);
        }
        // In-flow (not overlay) slot: no click-swallowing to disarm. Settle
        // any running slide, then remember the height the user dragged the
        // panel to before it goes.
        // Taken out of the cell before it is skipped: `if let` holds the
        // temporary `borrow_mut` guard alive for its whole body, and
        // skipping emits `done`, whose handler reaches for this same cell.
        let prev_anim = self.split_close_anim.borrow_mut().take();
        if let Some(prev) = prev_anim {
            prev.skip();
        }
        if let Some((slot, split, bottom)) = self.live_split() {
            if slot.is_visible() {
                config::save_split_reply_height(split_reply_height(&split, bottom));
            }
        }
        // The reader gets its header back with the pane.
        self.show_reader_header(true);
        for slot in [&self.reader_split_top, &self.reader_split_bottom] {
            slot.set_reveal_child(false);
            // The composer sits inside the grab-pill Overlay wrapper:
            // unparent it from there explicitly, or hosting it elsewhere
            // (pop-out, drain) would find it still parented.
            if let Some(wrap) = slot.child().and_downcast::<gtk::Overlay>() {
                wrap.set_child(None::<&gtk::Widget>);
            }
            slot.set_child(None::<&gtk::Widget>);
            // Hiding the slot hides the Paned divider with it.
            slot.set_visible(false);
        }
        // The split's reply-target outline goes with the composer.
        self.message_view.emit(MessageViewInput::BlurCard);
    }

    /// Slide the covering composer away instead of blinking it out: the
    /// revealer plays its slide-up, and the child is removed only once the
    /// transition has had its time — removing it at once (what
    /// `clear_compose_slots` does) skips the slide entirely. The removal
    /// stands down if a new composer has taken the slot meanwhile. Pop-out
    /// reparenting keeps using the instant clear: it needs the widget freed
    /// immediately.
    fn animate_cover_close(&self) {
        for r in [&self.reader_compose_revealer, &self.contacts_compose_revealer] {
            let Some(child) = r.child() else { continue };
            r.set_can_target(false);
            if !r.reveals_child() {
                r.set_child(None::<&gtk::Widget>);
                continue;
            }
            r.set_reveal_child(false);
            let r2 = r.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(340), move || {
                if r2.child().as_ref() == Some(&child) && !r2.reveals_child() {
                    r2.set_child(None::<&gtk::Widget>);
                }
            });
        }
        self.message_view.emit(MessageViewInput::BlurCard);
    }

    /// Slide the split reply up and out the way it slid in, then tear the
    /// slot down. The Paned allocates the slot by divider position — not by
    /// the revealer's animated height — so the position is animated to zero
    /// in step with the revealer's own slide-up, with the slot briefly
    /// allowed to shrink below the composer's minimum. The teardown at the
    /// end is skipped if a new reply has taken the slot meanwhile.
    fn animate_split_close(&self) {
        let Some((slot, split, bottom)) = self.live_split() else { return };
        let Some(wrap) = slot.child() else { return };
        // Settle any running slide first, so the height saved below is the
        // real one and not a mid-animation reading.
        // Taken out of the cell before it is skipped: `if let` holds the
        // temporary `borrow_mut` guard alive for its whole body, and
        // skipping emits `done`, whose handler reaches for this same cell.
        let prev_anim = self.split_close_anim.borrow_mut().take();
        if let Some(prev) = prev_anim {
            prev.skip();
        }
        if slot.is_visible() {
            config::save_split_reply_height(split_reply_height(&split, bottom));
        }
        // The reply-target outline goes with the composer, as on an
        // instant teardown.
        self.message_view.emit(MessageViewInput::BlurCard);

        set_split_shrink(&split, bottom, true);
        slot.set_reveal_child(false);
        // The reader's header comes back in step with the panel's exit: it
        // slides down while the panel slides up, its icons fading in, so the
        // reader below moves in one motion rather than jumping up as the
        // panel goes and back down as the header pops in after it. (Beneath
        // the reader the header never left; this is a no-op there.)
        self.show_reader_header(true);
        let from = split.position() as f64;
        // Out the way it came in: to nothing above the reader, to the
        // pane's full height beneath it.
        let to = if bottom { split.height() as f64 } else { 0.0 };
        let target = adw::CallbackAnimationTarget::new({
            let split = split.clone();
            move |v| split.set_position(v as i32)
        });
        let anim = adw::TimedAnimation::new(&split, from, to, 300, target);
        anim.set_easing(adw::Easing::EaseOutCubic);
        let cell = self.split_close_anim.clone();
        anim.connect_done(move |_| {
            cell.borrow_mut().take();
            if slot.child().as_ref() == Some(&wrap) {
                if let Some(w) = wrap.downcast_ref::<gtk::Overlay>() {
                    w.set_child(None::<&gtk::Widget>);
                }
                slot.set_child(None::<&gtk::Widget>);
                slot.set_visible(false);
            }
            // Only once the slot is empty (or a new panel owns it): with the
            // composer still in the slot, forbidding shrink first would
            // re-clamp the divider to the composer's minimum for a frame —
            // the bounce at the end of the slide.
            set_split_shrink(&split, bottom, false);
        });
        *self.split_close_anim.borrow_mut() = Some(anim.clone());
        anim.play();
    }

    /// Slide the reader's header bar in or out of its toolbar view, fading
    /// its icons with it. The toolbar view animates the bar's height (its
    /// reveal-top-bars transition), so the content below slides rather than
    /// jumping by the bar's height; the opacity animation keeps the icons
    /// from appearing at full strength on the first frame of the slide.
    /// Timed to match the split reply's own 300ms slide, which it accompanies.
    fn show_reader_header(&self, shown: bool) {
        // The header bar goes only while a split reply sits above the
        // reader; the subject block under it wants more air then.
        self.message_view.emit(MessageViewInput::SetUnderSplit(!shown));
        let Some(header) = self.reader_header.get() else { return };
        let tv = header
            .ancestor(adw::ToolbarView::static_type())
            .and_downcast::<adw::ToolbarView>();
        if let Some(tv) = tv {
            if tv.reveals_top_bars() == shown {
                return;
            }
            tv.set_reveal_top_bars(shown);
        }
        let from = header.opacity();
        let to = if shown { 1.0 } else { 0.0 };
        let target = adw::CallbackAnimationTarget::new({
            let header = header.clone();
            move |v| header.set_opacity(v)
        });
        let anim = adw::TimedAnimation::new(header, from, to, 300, target);
        anim.set_easing(adw::Easing::EaseOutCubic);
        anim.play();
    }

    /// The split reply's grab handle: the shared grab pill, floated at the
    /// panel's edge that meets the reader and bounded like the panel's
    /// opening height — a usable panel, a visible reader. Above the reader
    /// that edge is the panel's bottom; beneath it (#212), its top, where the
    /// divider position is the reader's height rather than the panel's.
    fn build_split_grab_pill(&self, bottom: bool) -> gtk::Box {
        let paned = if bottom { &self.reader_split_lower } else { &self.reader_split };
        let pill = crate::ui::grab_pill::paned_grab_pill(paned, move |split, want| {
            if bottom {
                want.clamp(200, (split.height() - 220).max(200))
            } else {
                want.clamp(220, (split.height() - 200).max(220))
            }
        });
        pill.set_valign(if bottom { gtk::Align::Start } else { gtk::Align::End });
        // Bias the centred bar 6px away from the editor inside its hit zone
        // (a margin shifts the centring by half its size): more air between
        // the editor card and the bar, matching the attachment drawer's
        // spacing.
        if let Some(bar) = pill.first_child() {
            if bottom {
                bar.set_margin_bottom(12);
            } else {
                bar.set_margin_top(12);
            }
        }
        pill
    }

    /// The split-reply slot in use, with the Paned that sizes it and whether
    /// it is the one beneath the reader; `None` while no split reply is open.
    fn live_split(&self) -> Option<(gtk::Revealer, gtk::Paned, bool)> {
        if self.reader_split_top.child().is_some() {
            Some((self.reader_split_top.clone(), self.reader_split.clone(), false))
        } else if self.reader_split_bottom.child().is_some() {
            Some((self.reader_split_bottom.clone(), self.reader_split_lower.clone(), true))
        } else {
            None
        }
    }

    /// Whether the next split reply opens beneath the reader (#212): by the
    /// setting outright, or following the reading order — beneath when the
    /// conversation reads downward to its newest message, above when the
    /// newest is at the top.
    fn split_reply_at_bottom(&self) -> bool {
        match self.reply_position {
            config::ReplyPosition::Top => false,
            config::ReplyPosition::Bottom => true,
            config::ReplyPosition::Follow => !self.thread_newest_first,
        }
    }

    fn open_inline_reply(
        &mut self,
        account_id: u32,
        prefill: ComposePrefill,
        focus: Option<(u32, u32)>,
        sender: &ComponentSender<Self>,
    ) {
        let contextual = focus.is_some();
        // Supersede any composer already in the reader slot first.
        self.release_reader_compose();
        let (id, mut init) = self.build_compose_init(account_id, prefill, false, true);
        // The split reply is compact: just the editor, fields behind pop-out.
        init.compact = contextual;
        // Beneath the reader (#212) the reader's header stays, so the
        // composer's must not carry a second set of window controls.
        let bottom = contextual && !self.showing_contacts && self.split_reply_at_bottom();
        init.decorations = !bottom;
        let controller = self.spawn_compose(init, sender);
        let widget = controller.widget();
        widget.set_hexpand(true);
        widget.add_css_class("inline-compose-surface");
        if contextual && !self.showing_contacts {
            // A reply/forward splits the pane instead of covering it (#86):
            // the composer slides down from the top and the message(s) stay
            // below, visible and interactive to refer to while writing. The
            // composer fills whatever the Paned divider grants it, and the
            // height it sets is final: pasting a novel into the body cannot
            // push the panel down.
            //
            // The grab handle is a floating pill at the panel's bottom edge
            // (the iOS home-indicator look) — the wrapper Overlay floats it
            // over the composer, and dragging it moves the Paned divider,
            // whose own separator is painted invisible.
            // A still-running close animation would fight the new panel for
            // the divider: jump it to its end (its teardown sees the slot's
            // new occupant and stands down).
            let prev_anim = self.split_close_anim.borrow_mut().take();
            if let Some(prev) = prev_anim {
                prev.skip();
            }
            // Above the reader, the reader's own header would show a second
            // set of window decorations mid-window (the composer header at
            // the pane's top already carries them) and spend vertical space
            // the split needs: gone while the split hosts, back when it
            // goes. What remains of the reader starts at the remote-content
            // banner, or the subject block when there is none. Beneath the
            // reader the header is nowhere near the panel and stays.
            self.show_reader_header(bottom);
            let (slot, split) = if bottom {
                (&self.reader_split_bottom, self.reader_split_lower.clone())
            } else {
                (&self.reader_split_top, self.reader_split.clone())
            };
            widget.set_vexpand(true);
            let wrap = gtk::Overlay::new();
            wrap.set_child(Some(widget));
            wrap.add_overlay(&self.build_split_grab_pill(bottom));
            slot.set_child(Some(&wrap));
            slot.set_visible(true);
            slot.set_reveal_child(true);
            let pane_h = split.height();
            let pane_h = if pane_h > 0 { pane_h } else { 900 };
            let saved = config::load_split_reply_height();
            let h =
                if saved > 0 { saved } else { ((pane_h as f64 * 0.45) as i32).clamp(300, 560) };
            // Keep a usable slice of reader on screen whatever was saved —
            // and slide the panel in the way it slides out: the divider IS
            // the animation, driven from zero to the opening height (the
            // revealer's own transition can't run — it starts unmapped, and
            // adw skips animations on unmapped widgets — so the divider
            // does all the moving). Beneath the reader the divider position
            // is the reader's height, so the same slide runs from the pane's
            // full height down to what is left once the panel has its share.
            let target = h.clamp(220, (pane_h - 200).max(220));
            let (from, to) = if bottom {
                (pane_h as f64, (pane_h - target) as f64)
            } else {
                (0.0, target as f64)
            };
            set_split_shrink(&split, bottom, true);
            split.set_position(from as i32);
            let anim = adw::TimedAnimation::new(
                &split,
                from,
                to,
                300,
                adw::CallbackAnimationTarget::new({
                    let split = split.clone();
                    move |v| split.set_position(v as i32)
                }),
            );
            anim.set_easing(adw::Easing::EaseOutCubic);
            let cell = self.split_close_anim.clone();
            anim.connect_done({
                let cell = cell.clone();
                move |_| {
                    cell.borrow_mut().take();
                    set_split_shrink(&split, bottom, false);
                }
            });
            *cell.borrow_mut() = Some(anim.clone());
            anim.play();
            if let Some((fa, fid)) = focus {
                // Put the card being answered at the top of the shortened
                // reader, wearing the selection outline.
                self.message_view
                    .emit(MessageViewInput::FocusCard { account_id: fa, id: fid });
            }
        } else {
            // Composing clears the reader toolbar too (decorations excepted);
            // the ⋯ overflow is managed by hand, so hide it by hand.
            self.reader_overflow_btn.set_visible(false);
            // Fill the pane and paint an opaque surface: the composer covers
            // the pane completely, not a partial panel.
            widget.set_vexpand(true);
            let slot = self.compose_slot();
            slot.set_child(Some(widget));
            slot.set_can_target(true);
            // Revealed a frame later: starting the transition in the same
            // cycle the child was set finds it unmapped, and the slide is
            // skipped — the composer pops in instead of sliding down.
            let s = slot.clone();
            gtk::glib::idle_add_local_once(move || s.set_reveal_child(true));
        }
        controller.emit(ComposeInput::FocusEditor);
        self.reader_compose = Some(ReaderCompose { id, controller, window: None });
        // A fresh composer has nothing to take back yet; the last one's
        // labels must not carry over into its menu.
        self.compose_history = (None, None);
        self.refresh_undo_menu();
    }

    /// Detach the reader's inline composer from the reader slot. If it was popped
    /// out to a window it lives on independently; if inline, ask it to save-if-
    /// dirty and let it drain closed.
    fn release_reader_compose(&mut self) {
        let Some(r) = self.reader_compose.take() else {
            return;
        };
        self.compose_history = (None, None);
        self.refresh_undo_menu();
        match r.window {
            Some(window) => {
                self.composers.push(ComposeHost { id: r.id, controller: r.controller, window });
            }
            None => {
                // Either kind of inline composer slides out the way it
                // slid in.
                if self.live_split().is_some() {
                    self.animate_split_close();
                } else {
                    self.animate_cover_close();
                }
                self.reader_overflow_btn.set_visible(self.reader_overflow_wanted());
                r.controller.emit(ComposeInput::SaveDraftIfDirty);
                self.draining_composers.push((r.id, r.controller));
            }
        }
    }

    /// Promote the reader's inline pane to a window, or collapse a window back
    /// inline — reparenting the live pane so the editor state survives the move.
    fn toggle_compose_window(&mut self, id: u32, sender: &ComponentSender<Self>) {
        let Some(mut r) = self.reader_compose.take() else {
            return;
        };
        if r.id != id {
            self.reader_compose = Some(r);
            return;
        }
        let widget = r.controller.widget().clone();
        match r.window.take() {
            None => {
                // inline → window: unparent from the revealer, then host in a window.
                self.clear_compose_slots();
                self.reader_overflow_btn.set_visible(self.reader_overflow_wanted());
                let window = self.compose_window_host(&widget, id, sender);
                r.window = Some(window);
                r.controller.emit(ComposeInput::SetWindowed(true));
            }
            Some(window) => {
                // window → inline: unparent from the window, drop it back in place.
                window.set_content(None::<&gtk::Widget>);
                window.destroy();
                widget.set_vexpand(true);
                widget.set_hexpand(true);
                widget.add_css_class("inline-compose-surface");
                self.reader_overflow_btn.set_visible(false);
                let slot = self.compose_slot();
                slot.set_child(Some(&widget));
                slot.set_can_target(true);
                let s = slot.clone();
                gtk::glib::idle_add_local_once(move || s.set_reveal_child(true));
                // Back inline it covers the reader, header and all, so it
                // carries the decorations whatever slot it left from.
                r.controller.emit(ComposeInput::SetDecorations(true));
                r.controller.emit(ComposeInput::SetWindowed(false));
            }
        }
        r.controller.emit(ComposeInput::FocusEditor);
        self.reader_compose = Some(r);
    }

    /// Tear down a composer by id (from a Close output or a window's close-request).
    fn close_compose(&mut self, id: u32) {
        tracing::debug!(target: "hylki::compose", "composer {id} closed");
        if let Some(pos) = self.composers.iter().position(|h| h.id == id) {
            let host = self.composers.remove(pos);
            host.window.set_content(None::<&gtk::Widget>);
            host.window.destroy();
            return;
        }
        if self.reader_compose.as_ref().is_some_and(|r| r.id == id) {
            let r = self.reader_compose.take().unwrap();
            match r.window {
                Some(window) => {
                    window.set_content(None::<&gtk::Widget>);
                    window.destroy();
                }
                None => {
                    // Cancel/send on either inline composer: slide it out,
                    // not blink it.
                    if self.live_split().is_some() {
                        self.animate_split_close();
                    } else {
                        self.animate_cover_close();
                    }
                    self.reader_overflow_btn.set_visible(self.reader_overflow_wanted());
                }
            }
            return;
        }
        self.draining_composers.retain(|(cid, _)| *cid != id);
    }

    /// Move a message to its account's folder of `kind` (archive/delete).
    /// Destination path for `kind` on an account: its existing folder of that kind,
    /// else a sensible default (the worker creates it on the server on first move).
    fn folder_path_for(&self, account_id: u32, kind: FolderKind) -> Option<String> {
        self.folders
            .get(&account_id)
            .and_then(|fs| fs.iter().find(|f| f.kind == kind))
            .map(|f| f.path.clone())
            .or_else(|| self.default_folder_path(account_id, kind))
    }

    /// Fold the rest of a conversation — messages from the account's other
    /// folders, chiefly the replies you sent — into the open one (#21).
    ///
    /// Ignored unless it answers the message still on screen: the lookup is
    /// asynchronous and the user may have moved on. Messages already in the
    /// conversation are skipped, so this is safe to apply more than once.
    /// Ask the cache again for the rest of the conversation on screen, when
    /// it belongs to `account_id`. The answer goes through [`merge_related`],
    /// which adds only what the conversation lacks.
    fn reload_related(&mut self, account_id: u32) {
        let Some(current) = self.current.clone() else { return };
        if current.account_id != account_id || !self.threading {
            return;
        }
        let only = [current.clone()];
        let ids =
            thread_ids(if self.current_thread.is_empty() { &only[..] } else { &self.current_thread });
        if ids.is_empty() {
            return;
        }
        self.send_to(account_id, MailRequest::LoadRelated { message_id: current.id, ids });
        self.thread_related_pending = true;
    }

    fn merge_related(&mut self, account_id: u32, message_id: u32, messages: Vec<Message>) {
        let Some(current) = self.current.clone() else { return };
        if current.account_id != account_id || current.id != message_id || !self.threading {
            return;
        }
        let mut conv = if self.current_thread.is_empty() {
            vec![current.clone()]
        } else {
            self.current_thread.clone()
        };
        let messages = dedupe_label_copies(messages, &conv, current.folder_id);
        let mut added = Vec::new();
        for mut r in messages {
            if conv.iter().any(|m| m.folder_id == r.folder_id && m.uid == r.uid) {
                continue; // already in the conversation, from this folder's own index
            }
            let key = (r.account_id, r.folder_id, r.uid);
            r.id = match self.related_ids.get(&key) {
                Some(id) => *id,
                None => {
                    self.related_id_seq = self.related_id_seq.saturating_sub(1);
                    self.related_ids.insert(key, self.related_id_seq);
                    self.related_id_seq
                }
            };
            r.unread = false;
            if let Some(b) = self.body_cache.get(&(r.account_id, r.id)) {
                r.body = b.clone();
            }
            added.push(r);
        }
        if added.is_empty() {
            return;
        }
        conv.extend(added);
        conv.sort_by(|a, b| a.timestamp.cmp(&b.timestamp)); // oldest first, as the list threads
        // Late arrivals bring bodies of their own; give them the same grace
        // period so they join the first paint instead of causing a second.
        self.thread_opened_at = Some(std::time::Instant::now());
        self.current_thread = conv;
        let to_load: Vec<MissingBody> = self
            .current_thread
            .iter()
            .filter(|tm| tm.body.is_empty())
            .filter_map(|tm| self.resolve_folder_path(tm).map(|p| (tm.account_id, tm.id, tm.uid, p)))
            .collect();
        for ((aid, path), items) in batch_bodies_by_folder(to_load) {
            self.send_to(aid, MailRequest::LoadBodies { items, path });
        }
    }

    /// Whether a message already lives in its account's Trash — where "delete"
    /// can't mean "move to Trash" any more, so it has to mean erase for good.
    fn in_trash(&self, m: &Message) -> bool {
        self.folders.get(&m.account_id).is_some_and(|fs| {
            fs.iter().any(|f| f.id == m.folder_id && f.kind == FolderKind::Trash)
        })
    }

    fn in_junk(&self, m: &Message) -> bool {
        self.folders.get(&m.account_id).is_some_and(|fs| {
            fs.iter().any(|f| f.id == m.folder_id && f.kind == FolderKind::Junk)
        })
    }

    /// Whether the reader's target sits in Junk: the spam button, shortcut
    /// and menu entry then read "Not Spam" (#168).
    fn target_in_junk(&self) -> bool {
        self.reply_target().is_some_and(|m| self.in_junk(&m))
    }

    /// Delete `messages`: the ones still outside Trash are moved there, and any
    /// already in Trash are erased for good once the user confirms.
    fn delete_messages(&mut self, messages: Vec<Message>, sender: &ComponentSender<Self>) {
        let (purge, move_out): (Vec<Message>, Vec<Message>) =
            messages.into_iter().partition(|m| self.in_trash(m));
        match move_out.len() {
            0 => {}
            1 => self.move_to(move_out.into_iter().next().unwrap(), FolderKind::Trash),
            _ => self.apply_bulk_move(BulkAction::Delete, move_out),
        }
        if !purge.is_empty() {
            self.confirm_purge(purge, sender);
        }
    }

    /// Confirm erasing messages that are already in Trash — there's no undo, so
    /// this always asks first.
    /// Ask before deleting a whole conversation (a selected thread head). Can
    /// be turned off in Preferences (`confirm_thread_delete`).
    fn confirm_delete_thread(&self, messages: Vec<Message>, sender: &ComponentSender<Self>) {
        let n = messages.len();
        let heading = i18n("Delete this conversation?");
        let body = format!(
            "All {n} messages in this conversation will be deleted. \
             You can turn this warning off in Preferences."
        );
        let dialog =
            adw::MessageDialog::new(Some(&self.window), Some(&heading), Some(body.as_str()));
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("delete", &i18n("Delete Conversation"));
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        let s = sender.clone();
        let messages = std::cell::RefCell::new(Some(messages));
        dialog.connect_response(None, move |_, resp| {
            if resp == "delete" {
                if let Some(msgs) = messages.borrow_mut().take() {
                    s.input(AppMsg::DeleteThreadConfirmed(msgs));
                }
            }
        });
        dialog.present();
    }

    fn confirm_purge(&self, messages: Vec<Message>, sender: &ComponentSender<Self>) {
        let n = messages.len();
        let heading = ni18n_f("Delete this message permanently?", "Delete {n} messages permanently?", n as u32, &[("n", &n.to_string())]);
        let body = if n == 1 {
            i18n("It is already in Trash, so it will be erased from the server. This can’t be undone.")
        } else {
            i18n("They are already in Trash, so they will be erased from the server. \
             This can’t be undone.")
        };
        let dialog = adw::MessageDialog::new(Some(&self.window), Some(&heading), Some(&body));
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("delete", &i18n("Delete"));
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        let s = sender.clone();
        let messages = std::cell::RefCell::new(Some(messages));
        dialog.connect_response(None, move |_, resp| {
            if resp == "delete" {
                if let Some(msgs) = messages.borrow_mut().take() {
                    s.input(AppMsg::PurgeMessages(msgs));
                }
            }
        });
        dialog.present();
    }

    /// Erase messages from the server for good (flag `\Deleted` + EXPUNGE),
    /// grouped by source folder so each folder takes a single request.
    fn purge_messages(&mut self, messages: Vec<Message>) {
        let mut groups: HashMap<(u32, String), Vec<u32>> = HashMap::new();
        let mut removed_ids = Vec::with_capacity(messages.len());
        for m in &messages {
            let Some(src) = self.resolve_folder_path(m) else { continue };
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
        self.message_list.emit(MessageListInput::RemoveMany(removed_ids));
        self.push_unread_counts();
    }

    fn move_to(&mut self, m: Message, kind: FolderKind) {
        let Some(dest) = self.folder_path_for(m.account_id, kind) else {
            self.notifications.emit(NotifyInput::Push {
                text: i18n_f("No {v1} folder available", &[("v1", &(kind_label(kind)).to_string())]),
                error: true,
                connectivity: false,
            });
            return;
        };
        self.move_to_path(m, dest);
    }

    /// Move a message to an explicit destination folder path (used by both the
    /// kind-based actions and drag-and-drop onto a folder).
    fn move_to_path(&mut self, m: Message, dest: String) {
        let Some(src) = self.resolve_folder_path(&m) else {
            return;
        };
        if src == dest {
            return; // already in that folder
        }
        self.push_undo_move(m.account_id, &dest, &src, vec![m.clone()]);
        self.send_to(m.account_id, MailRequest::MoveMessage { path: src, uid: m.uid, dest });
        self.discard_message(&m);
    }

    /// Move a dropped drag selection into `dest` on `dest_account`. Messages are
    /// grouped by source folder and each group moved in a single `MoveMessages`
    /// request, so dragging a multi-selection moves all of it (#23). IMAP can't
    /// move mail between accounts, so anything from another account — possible
    /// when the drag started in the unified inbox — stays put and is reported
    /// rather than silently dropped.
    fn drop_move(&mut self, dest_account: u32, dest: String, items: Vec<(u32, u32, u32, u32)>) {
        let mut groups: HashMap<String, (Vec<u32>, Vec<Message>)> = HashMap::new();
        let mut removed_ids: Vec<u32> = Vec::new();
        let mut foreign = 0usize;
        for (aid, fid, uid, id) in items {
            if aid != dest_account {
                foreign += 1;
                continue;
            }
            // Prefer the cached message: it knows its own source folder (in the
            // unified inbox that isn't the folder the row was listed under) and
            // lets the caches be cleaned up optimistically.
            let cached = self.find_cached_message(aid, id);
            let src = match &cached {
                Some(m) => self.resolve_folder_path(m),
                None => self
                    .folders
                    .get(&aid)
                    .and_then(|fs| fs.iter().find(|f| f.id == fid))
                    .map(|f| f.path.clone()),
            };
            let Some(src) = src else { continue };
            if src == dest {
                continue; // already in that folder
            }
            if let Some(m) = &cached {
                self.discard_message_local(m);
            }
            let slot = groups.entry(src).or_default();
            slot.0.push(uid);
            // A row with no cached message is moved, but not recorded: there
            // is nothing to find it by, and nothing to put back on the list.
            slot.1.extend(cached);
            removed_ids.push(id);
        }
        if foreign > 0 {
            self.notifications.emit(NotifyInput::Push {
                text: if foreign == 1 {
                    i18n("One message stayed put — mail can't be moved between accounts")
                } else {
                    i18n_f("{foreign} messages stayed put — mail can't be moved between accounts", &[("foreign", &foreign.to_string())])
                },
                error: true,
                connectivity: false,
            });
        }
        if removed_ids.is_empty() {
            return;
        }
        self.bulk_pending += groups.len();
        for (src, (uids, rows)) in groups {
            self.push_undo_move(dest_account, &dest, &src, rows);
            self.send_to(
                dest_account,
                MailRequest::MoveMessages { path: src, uids, dest: dest.clone() },
            );
        }
        self.message_list.emit(MessageListInput::RemoveMany(removed_ids));
        self.push_unread_counts();
    }

    /// Move a custom folder under a new parent (or "" = the account's top
    /// level) via IMAP RENAME, after checking the move makes sense. The server
    /// carries any sub-hierarchy along with it.
    /// Make a folder at `path`. Optimistic: it appears in the sidebar right
    /// away (a server round-trip took seconds); the worker's confirming
    /// refresh matches the prediction and repaints nothing.
    fn create_folder(&mut self, account_id: u32, path: String) {
        let delim = self.folder_delimiter(account_id);
        let leaf = path.rsplit(delim).next().unwrap_or(&path).to_string();
        if !self
            .folders
            .get(&account_id)
            .is_some_and(|fs| fs.iter().any(|f| f.path == path))
        {
            self.folders.entry(account_id).or_default().push(Folder {
                id: 0,
                account_id,
                name: crate::mutf7::decode(&leaf),
                path: path.clone(),
                kind: FolderKind::Custom,
                unread: 0,
            });
            self.normalize_folders(account_id);
            self.rebuild_sidebar();
        }
        self.send_to(account_id, MailRequest::CreateFolder { path });
    }

    /// Remove a folder, its messages going to Trash first so nothing is lost
    /// even when this is undoing a folder someone has since filed mail into.
    /// Hide folders (#239): remembered on the account, taken off the
    /// sidebar at once, and the worker told so its next listing, and every
    /// sync after it, leaves them out. A hidden folder that was open gives
    /// way to nothing, like a deleted one.
    fn hide_folders(&mut self, account_id: u32, paths: Vec<String>) {
        let Some(cfg) = self.config.get_mut(account_id as usize - 1) else { return };
        let mut changed = false;
        for path in paths {
            if !cfg.hidden_folders.contains(&path) {
                cfg.hidden_folders.push(path);
                changed = true;
            }
        }
        if !changed {
            return;
        }
        let hidden = cfg.hidden_folders.clone();
        if let Err(e) = config::save(&self.config) {
            self.notifications.emit(NotifyInput::Push {
                text: i18n_f("Could not save account: {e}", &[("e", &(e).to_string())]),
                error: true,
                connectivity: false,
            });
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|s| s.account_id == account_id && crate::models::folder_is_hidden(&s.path, None, &hidden))
        {
            self.current = None;
            self.current_thread.clear();
            self.show_message(None, false);
            self.message_list.emit(MessageListInput::SetLoading);
        }
        if let Some(folders) = self.folders.get_mut(&account_id) {
            folders.retain(|f| !crate::models::folder_is_hidden(&f.path, None, &hidden));
        }
        self.rebuild_sidebar();
        self.send_to(account_id, MailRequest::SetHiddenFolders { paths: hidden });
    }

    fn delete_folder(&mut self, account_id: u32, path: String) {
        let trash = self
            .folders
            .get(&account_id)
            .and_then(|fs| fs.iter().find(|f| f.kind == FolderKind::Trash))
            .map(|f| f.path.clone())
            .or_else(|| self.default_folder_path(account_id, FolderKind::Trash));
        // If the deleted folder is currently open, clear the view.
        if self.selected.as_ref().is_some_and(|s| s.account_id == account_id && s.path == path) {
            self.current = None;
            self.current_thread.clear();
            self.show_message(None, false);
            self.message_list.emit(MessageListInput::SetLoading);
        }
        self.send_to(account_id, MailRequest::DeleteFolder { path, trash });
    }

    fn move_folder(&mut self, account_id: u32, path: String, dest: String) {
        let complain = |me: &Self, text: &str| {
            me.notifications.emit(NotifyInput::Push {
                text: text.to_string(),
                error: true,
                connectivity: false,
            });
        };
        let delim = self.folder_delimiter(account_id);
        // Into itself or its own subtree: there is no such place.
        if dest == path || dest.starts_with(&format!("{path}{delim}")) {
            complain(self, "A folder can't be moved into itself.");
            return;
        }
        let folders = self.folders.get(&account_id).cloned().unwrap_or_default();
        // Only your own folders (or the top level) can hold other folders —
        // moving one under Sent or Trash is never what a drop meant.
        if !dest.is_empty()
            && !folders.iter().any(|f| f.path == dest && f.kind == FolderKind::Custom)
        {
            return;
        }
        let leaf = path.rsplit(delim).next().unwrap_or(&path).to_string();
        let new_path = if dest.is_empty() {
            format!("{}{leaf}", self.folder_namespace(account_id))
        } else {
            format!("{dest}{delim}{leaf}")
        };
        if new_path == path {
            return;
        }
        if folders.iter().any(|f| f.path == new_path) {
            complain(self, &format!("A folder named {leaf:?} is already there."));
            return;
        }
        self.apply_folder_rename(account_id, path, new_path, Some(i18n("Move Folder")));
    }

    /// Bring an account's local folder list back to exactly the shape the
    /// worker reports: pin the freshest unread counts onto the folders, apply
    /// the worker's sort, reassign index-based ids, and re-key the unread map
    /// and the selection. Run after any optimistic mutation (move, rename,
    /// create) so the confirming server refresh is a recognised no-op.
    fn normalize_folders(&mut self, account_id: u32) {
        let Some(folders) = self.folders.get_mut(&account_id) else { return };
        for f in folders.iter_mut() {
            if let Some(u) = self.folder_unread.get(&(account_id, f.id)) {
                f.unread = *u;
            }
        }
        folders.sort_by(|a, b| {
            crate::worker::folder_order(a.kind)
                .cmp(&crate::worker::folder_order(b.kind))
                .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
        });
        for (i, f) in folders.iter_mut().enumerate() {
            f.id = i as u32 + 1;
        }
        self.folder_unread.retain(|(a, _), _| *a != account_id);
        for f in folders.iter() {
            self.folder_unread.insert((account_id, f.id), f.unread);
        }
        if let Some(sel) = self.selected.as_mut() {
            if sel.account_id == account_id {
                if let Some(f) = folders.iter().find(|f| f.path == sel.path) {
                    sel.folder_id = f.id;
                }
            }
        }
    }

    /// The shared optimistic machinery behind both folder moves and renames:
    /// clear the view if the affected subtree is open, reshape the local
    /// folder list exactly as the worker will report it back, re-key
    /// everything id- or path-addressed, and hand the RENAME to the server.
    fn apply_folder_rename(
        &mut self,
        account_id: u32,
        path: String,
        new_path: String,
        record: Option<String>,
    ) {
        // Recorded as the rename that puts it back. `None` while an undo or
        // redo is doing exactly that — it keeps its own history.
        if let Some(what) = record {
            self.push_undo(account_id, what, UndoStep::RenameFolder {
                from: new_path.clone(),
                to: path.clone(),
            });
        }
        let delim = self.folder_delimiter(account_id);
        // If the affected folder (or one of its children) is open, clear the
        // view — its path is about to stop existing.
        // — its path is about to stop existing.
        if self.selected.as_ref().is_some_and(|s| {
            s.account_id == account_id
                && (s.path == path || s.path.starts_with(&format!("{path}{delim}")))
        }) {
            self.current = None;
            self.current_thread.clear();
            self.show_message(None, false);
            self.message_list.emit(MessageListInput::SetLoading);
        }

        // Optimistic: reshape the local tree NOW so the sidebar shows the move
        // instantly; the server rename confirms in the background. The reshape
        // mirrors the worker exactly — same subtree rewrite the server does,
        // same sort, same index-assigned ids — so the confirming refresh is an
        // identical list and SetFolders repaints nothing.
        let old_prefix = format!("{path}{delim}");
        let new_prefix = format!("{new_path}{delim}");
        if let Some(folders) = self.folders.get_mut(&account_id) {
            for f in folders.iter_mut() {
                if f.path == path {
                    f.path = new_path.clone();
                    let leaf = f.path.rsplit(delim).next().unwrap_or(&f.path);
                    f.name = crate::mutf7::decode(leaf);
                } else if let Some(rest) = f.path.strip_prefix(&old_prefix) {
                    f.path = format!("{new_prefix}{rest}");
                }
            }
        }
        self.normalize_folders(account_id);
        // Collapsed tree nodes follow the moved subtree to its new home.
        if let Some(email) =
            self.accounts.iter().find(|a| a.id == account_id).map(|a| a.email.clone())
        {
            let key_prefix = format!("{email}\t");
            for k in self.tree_collapsed.iter_mut() {
                if let Some(rest) = k.strip_prefix(&key_prefix) {
                    if rest == path {
                        *k = format!("{key_prefix}{new_path}");
                    } else if let Some(r) = rest.strip_prefix(&old_prefix) {
                        *k = format!("{key_prefix}{new_prefix}{r}");
                    }
                }
            }
            self.save_sidebar_state();
        }
        // The open folder follows its rename (or its parent's): the
        // selection used to keep the old path, and every auto-fetch asked
        // the server for a mailbox that no longer existed.
        let followed = self.selected.as_ref().and_then(|s| {
            if s.account_id != account_id {
                return None;
            }
            if s.path == path {
                Some(new_path.clone())
            } else {
                s.path.strip_prefix(&old_prefix).map(|rest| format!("{new_prefix}{rest}"))
            }
        });
        if let Some(moved) = followed {
            let id = self
                .folders
                .get(&account_id)
                .and_then(|fs| fs.iter().find(|f| f.path == moved))
                .map(|f| f.id);
            if let (Some(sel), Some(id)) = (self.selected.as_mut(), id) {
                sel.path = moved.clone();
                sel.folder_id = id;
            }
        }
        self.rebuild_sidebar();
        if let Some(sel) = self.selected.clone().filter(|s| s.account_id == account_id) {
            self.sidebars_emit(SidebarInput::SelectFolderRow {
                account_id,
                path: sel.path.clone(),
            });
        }
        self.send_to(account_id, MailRequest::RenameFolder { old_path: path, new_path });
        // The worker runs requests in order: the reload queues behind the
        // RENAME and refills the cleared view from the folder's new name.
        if let Some(sel) = self.selected.clone().filter(|s| s.account_id == account_id) {
            self.send_to(account_id, MailRequest::LoadMessages {
                folder_id: sel.folder_id,
                path: sel.path,
            });
        }
    }

    /// The account's hierarchy delimiter, inferred from its folder paths (the
    /// namespace prefix's last character when there is one, else the first
    /// common delimiter any path uses; '/' as the fallback).
    fn folder_delimiter(&self, account_id: u32) -> char {
        let ns = self.folder_namespace(account_id);
        if let Some(c) = ns.chars().last() {
            if matches!(c, '/' | '.' | '\\') {
                return c;
            }
        }
        // Prefer '/' over '.' over '\' across the whole list, so one folder
        // with a dotted display name can't masquerade as the hierarchy
        // separator on a slash-delimited server.
        let fs = self.folders.get(&account_id);
        for d in ['/', '.', '\\'] {
            if fs.is_some_and(|fs| fs.iter().any(|f| f.path.contains(d))) {
                return d;
            }
        }
        '/'
    }

    /// The mailbox namespace prefix for an account: "INBOX<delim>" on servers
    /// that nest everything under INBOX (Dovecot-style), otherwise "". Only
    /// the INBOX root counts — deriving it from any nested folder's parent
    /// (as this once did) made "top level" mean "under whichever sub-folder
    /// happened to be listed first" on servers like iCloud, so new folders
    /// and header-dropped moves landed inside a random folder.
    fn folder_namespace(&self, account_id: u32) -> String {
        let Some(folders) = self.folders.get(&account_id) else {
            return String::new();
        };
        let delim = folders
            .iter()
            .find_map(|f| f.path.chars().find(|c| matches!(c, '/' | '.' | '\\')))
            .unwrap_or('/');
        let root = format!("INBOX{delim}");
        let customs: Vec<&Folder> =
            folders.iter().filter(|f| f.kind == FolderKind::Custom).collect();
        if !customs.is_empty()
            && customs.iter().all(|f| {
                f.path.len() >= root.len() && f.path[..root.len()].eq_ignore_ascii_case(&root)
            })
        {
            root
        } else {
            String::new()
        }
    }

    /// A sensible destination path for a standard folder the account doesn't have
    /// yet (Archive/Trash/Junk), matching the account's folder namespace so the
    /// server creates it in the right place (e.g. "INBOX.Archive").
    fn default_folder_path(&self, account_id: u32, kind: FolderKind) -> Option<String> {
        let leaf = match kind {
            FolderKind::Archive => "Archive",
            FolderKind::Trash => "Trash",
            FolderKind::Junk => "Junk",
            FolderKind::Drafts => "Drafts",
            _ => return None,
        };
        self.folders.get(&account_id)?; // require folders to be loaded
        Some(format!("{}{leaf}", self.folder_namespace(account_id)))
    }

    /// Find a cached message by (account, id) for drag-and-drop moves.
    fn find_cached_message(&self, account_id: u32, id: u32) -> Option<Message> {
        for ((aid, _), msgs) in self.message_cache.iter() {
            if *aid == account_id {
                if let Some(m) = msgs.iter().find(|m| m.id == id) {
                    return Some(m.clone());
                }
            }
        }
        self.unified_slices
            .iter()
            .filter(|((a, _), _)| *a == account_id)
            .flat_map(|(_, msgs)| msgs.iter())
            .find(|m| m.id == id)
            .cloned()
    }

    /// Explain how to set up the system keyring (Secret Service) so passwords
    /// persist across restarts, and — on Linux Mint / Cinnamon — how to stop the
    /// keyring asking for an unlock password at every login.
    ///
    /// `problem` is true when this is shown because a save actually failed;
    /// false for the proactive one-time tip (which offers "Don't show again").
    fn show_keyring_help(&self, problem: bool) {
        let mint = crate::platform::is_mint_cinnamon();

        let heading = if problem {
            i18n("Hylki couldn’t save your password")
        } else {
            i18n("Keyring setup on Linux Mint")
        };

        let mut body = String::new();
        if problem {
            body.push_str(
                "Hylki stores account passwords in the system keyring (the Secret \
                 Service), never on disk. The keyring didn’t accept the password, so \
                 this account won’t stay signed in after you close Hylki.\n\n",
            );
        } else {
            body.push_str(
                "Hylki keeps your account passwords in the system keyring (the Secret \
                 Service) rather than on disk. On Linux Mint with Cinnamon the keyring \
                 sometimes needs a one-time setup so passwords persist — and so it \
                 doesn’t ask you to unlock it at every login.\n\n",
            );
        }

        if mint {
            body.push_str(
                "Set it up:\n\
                 1. Install the keyring tools if needed:\n\
                 \u{2003}sudo apt install gnome-keyring seahorse\n\
                 2. Open “Passwords and Keys” (Seahorse) and make sure a keyring named \
                 “Login” exists and is set as Default (right-click → Set as Default).\n\n\
                 Stop it asking for a password at each login — pick one:\n\
                 • Recommended: set the Login keyring’s password to match your user \
                 login password (right-click the Login keyring → Change Password), and \
                 log in with your password rather than using automatic login. The \
                 keyring then unlocks automatically when you log in.\n\
                 • Or, to remove the prompt entirely even with automatic login: set the \
                 Login keyring’s password to blank (Change Password → leave the new \
                 password empty). This is convenient, but your saved passwords are then \
                 stored unencrypted at rest — only do this on a machine you trust.",
            );
            if crate::platform::is_flatpak() {
                body.push_str(
                    "\n\nNote: run these steps on the host system (not inside the \
                     Flatpak) — Hylki uses whatever keyring your desktop provides.",
                );
            }
        } else {
            body.push_str(
                "Make sure a Secret Service keyring is installed, running, and \
                 unlocked — for example install “gnome-keyring” and “seahorse” \
                 (Passwords and Keys), then create a default “Login” keyring and set \
                 its password to your login password so it unlocks automatically.",
            );
        }

        let dialog = adw::MessageDialog::new(Some(&self.window), Some(&heading), Some(&body));
        dialog.add_response("ok", &i18n("Got it"));
        dialog.set_default_response(Some("ok"));
        // The proactive tip is a one-time thing: mark it seen once dismissed (by
        // any means) so it never nags again. A real save failure always shows.
        if !problem {
            dialog.connect_response(None, |_, _| config::dismiss_mint_keyring_help());
        }
        dialog.present();
    }

    /// Prompt for a new custom folder name and create it under `account_id`.
    fn prompt_new_folder(&self, account_id: u32, sender: &ComponentSender<Self>) {
        let dialog = adw::MessageDialog::new(
            Some(&self.window),
            Some(i18n("New Folder").as_str()),
            Some(i18n("Create a new folder for this account.").as_str()),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("ok", &i18n("Create"));
        dialog.set_default_response(Some("ok"));
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some(i18n("Folder name").as_str()));
        entry.set_activates_default(true);
        dialog.set_extra_child(Some(&entry));
        let s = sender.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "ok" {
                let name = entry.text().to_string();
                if !name.trim().is_empty() {
                    s.input(AppMsg::CreateFolder { account_id, name });
                }
            }
        });
        dialog.present();
    }

    /// Prompt for a folder's new name and rename it in place.
    fn prompt_rename_folder(
        &self,
        account_id: u32,
        name: String,
        path: String,
        sender: &ComponentSender<Self>,
    ) {
        let dialog = adw::MessageDialog::new(
            Some(&self.window),
            Some(&format!("Rename {name:?}")),
            Some(i18n("Sub-folders keep their place under the new name.").as_str()),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("ok", &i18n("Rename"));
        dialog.set_default_response(Some("ok"));
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        let entry = gtk::Entry::new();
        entry.set_text(&name);
        entry.select_region(0, -1);
        entry.set_activates_default(true);
        dialog.set_extra_child(Some(&entry));
        let s = sender.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "ok" {
                let new_name = entry.text().to_string();
                if !new_name.trim().is_empty() {
                    s.input(AppMsg::RenameFolderTo {
                        account_id,
                        path: path.clone(),
                        new_name,
                    });
                }
            }
        });
        dialog.present();
    }

    /// Rename a custom folder's leaf in place: same parent, new (encoded)
    /// name, the same optimistic machinery as a drag-and-drop move.
    fn rename_folder_to(&mut self, account_id: u32, path: String, new_name: String) {
        let delim = self.folder_delimiter(account_id);
        let new_name = new_name.trim();
        // The path's parent (everything up to and including the last
        // delimiter) stays; only the leaf changes.
        let parent = match path.rfind(delim) {
            Some(at) => &path[..=at],
            None => "",
        };
        let new_path = format!("{parent}{}", crate::mutf7::encode(new_name));
        if new_path == path {
            return;
        }
        if self
            .folders
            .get(&account_id)
            .is_some_and(|fs| fs.iter().any(|f| f.path == new_path))
        {
            self.notifications.emit(NotifyInput::Push {
                text: i18n_f("A folder named {new_name} is already there.", &[("new_name", &format!("{new_name:?}"))]),
                error: true,
                connectivity: false,
            });
            return;
        }
        self.apply_folder_rename(account_id, path, new_path, Some(i18n("Rename Folder")));
    }

    /// Confirm deleting a custom folder (contents moved to Trash). Deliberately
    /// outside the undo history (#200): the mailbox itself is gone from the
    /// server, so the dialog says as much instead of promising a way back.
    fn confirm_delete_folder(
        &self,
        account_id: u32,
        name: String,
        path: String,
        sender: &ComponentSender<Self>,
    ) {
        let dialog = adw::MessageDialog::new(
            Some(&self.window),
            Some(&format!("Delete “{name}”?")),
            Some(
                i18n("Its messages are moved to Trash and the folder is removed from the \
                      server. This cannot be undone.")
                    .as_str(),
            ),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("delete", &i18n("Delete"));
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        let s = sender.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "delete" {
                s.input(AppMsg::DeleteFolder {
                    account_id,
                    path: path.clone(),
                });
            }
        });
        dialog.present();
    }

    /// Empty Trash / Empty Junk (#152): there is no undo, so always ask.
    fn confirm_empty_folder(
        &self,
        account_id: u32,
        folder_id: u32,
        name: String,
        path: String,
        sender: &ComponentSender<Self>,
    ) {
        let dialog = adw::MessageDialog::new(
            Some(&self.window),
            Some(&i18n_f("Empty “{name}”?", &[("name", &name)])),
            Some(i18n("Every message in it will be erased from the server. This can’t be undone.").as_str()),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("empty", &i18n("Empty"));
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("empty", adw::ResponseAppearance::Destructive);
        let s = sender.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "empty" {
                s.input(AppMsg::EmptyFolder { account_id, folder_id, path: path.clone() });
            }
        });
        dialog.present();
    }

    /// Mark `m` as spam: tag `$Junk` and move it to the account's Junk folder
    /// (so the server's spam filter can learn from it).
    fn mark_spam_msg(&mut self, m: Message) {
        let Some(src) = self.resolve_folder_path(&m) else {
            return;
        };
        let dest = self
            .folders
            .get(&m.account_id)
            .and_then(|fs| fs.iter().find(|f| f.kind == FolderKind::Junk))
            .map(|f| f.path.clone())
            .or_else(|| self.default_folder_path(m.account_id, FolderKind::Junk));
        let Some(dest) = dest else {
            self.notifications.emit(NotifyInput::Push {
                text: i18n("No Junk folder available for this account"),
                error: true,
                connectivity: false,
            });
            return;
        };
        self.push_undo_move(m.account_id, &dest, &src, vec![m.clone()]);
        self.send_to(m.account_id, MailRequest::MarkSpam { path: src, uid: m.uid, dest });
        self.discard_message(&m);
    }

    /// Not spam (#168): tell the server (`$NotJunk`, `$Junk` cleared) and
    /// put the message back in its account's Inbox.
    fn mark_ham_msg(&mut self, m: Message) {
        let Some(src) = self.resolve_folder_path(&m) else {
            return;
        };
        let Some(dest) = self.folder_path_for(m.account_id, FolderKind::Inbox) else {
            self.notifications.emit(NotifyInput::Push {
                text: i18n("No Inbox folder available for this account"),
                error: true,
                connectivity: false,
            });
            return;
        };
        self.push_undo_move_as(m.account_id, i18n("Not Spam"), &dest, &src, vec![m.clone()]);
        self.send_to(m.account_id, MailRequest::MarkHam { path: src, uid: m.uid, dest });
        self.discard_message(&m);
    }

    /// Mark every message in a folder as read: update server, caches, badges,
    /// and the displayed list.
    fn mark_folder_read(&mut self, account_id: u32, folder_id: u32) {
        let Some(path) = self
            .folders
            .get(&account_id)
            .and_then(|fs| fs.iter().find(|f| f.id == folder_id))
            .map(|f| f.path.clone())
        else {
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
        crate::notify::withdraw_mail(account_id);
        self.refresh_list_display();
        self.push_unread_counts();
    }

    /// Re-emit the currently-visible folder/unified list from the caches, so
    /// in-place changes (e.g. mark-all-read) show without a server round-trip.
    /// How many assembled conversations are kept. Each holds its messages'
    /// bodies, so this is a memory budget as much as a cache size.
    const THREAD_CACHE_MAX: usize = 8;

    /// Store the conversation on screen so returning to it is instant.
    fn remember_thread(&mut self) {
        let Some(key) = self.thread_key else { return };
        let thread = self.current_thread.clone();
        self.remember_thread_for(key, thread);
    }

    /// The same, for a conversation that isn't the one on screen — carrying
    /// one over a move that changed its key (#200).
    fn remember_thread_for(&mut self, key: (u32, u32), thread: Vec<Message>) {
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
    fn forget_threads(&mut self, account_id: u32) {
        self.thread_cache.retain(|(aid, _), _| *aid != account_id);
        self.thread_cache_order.retain(|(aid, _)| *aid != account_id);
    }

    /// Reply headers for every cached message, so the list can see that two
    /// replies in a folder answer the same message in another one.
    fn push_thread_links(&self) {
        // Newest first, and capped: threading the whole mailbox would otherwise
        // put every message's reply headers here, to be cloned on each sync and
        // walked on each rebuild. A conversation is joined by mail near it in
        // time, so the newest links are the ones that do the joining.
        let mut recent: Vec<&Message> = self
            .message_cache
            .values()
            .flatten()
            .filter(|m| !(m.message_id.is_empty() && m.references.is_empty()))
            .collect();
        recent.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        recent.truncate(THREAD_LINK_LIMIT);
        let mut links: Vec<(u32, String, String)> = recent
            .into_iter()
            .map(|m| (m.account_id, m.message_id.clone(), m.references.clone()))
            .collect();
        links.sort();
        links.dedup();
        self.message_list.emit(MessageListInput::SetThreadLinks(links));
    }

    /// Ask each account's cache how big the conversations on the list's page
    /// really are (#222).
    ///
    /// The list counts what it lists, which in a folder view is one folder; the
    /// replies you sent are in Sent and the badge was short by exactly them.
    /// The lookup is the reader's own (`thread_counts` mirrors what
    /// `messages_by_thread_ids` would find), so the number on the chip is the
    /// number of cards the reader will show when the row is opened.
    ///
    /// Nothing here blocks the page: the counts arrive as
    /// [`AppMsg::ThreadCounts`] and settle the badges a beat later.
    fn request_thread_counts(&mut self, groups: Vec<(u32, String, Vec<String>)>) {
        if !self.threading || groups.is_empty() {
            return;
        }
        let mut by_account: HashMap<u32, Vec<(String, Vec<String>)>> = HashMap::new();
        for (account_id, root, ids) in groups {
            by_account.entry(account_id).or_default().push((root, ids));
        }
        for (account_id, groups) in by_account {
            self.send_to(account_id, MailRequest::LoadThreadCounts { groups });
        }
    }

    /// Push every account's inbox slice to the list as one date-sorted run. The
    /// slices arrive independently (cache seed, then each account's load), so the
    /// whole merged list is re-emitted each time one of them changes.
    fn emit_unified(&self) {
        let t = std::time::Instant::now();
        let mut merged: Vec<Message> =
            self.unified_slices.values().flatten().cloned().collect();
        merged.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        tracing::debug!("unified: merged {} messages in {:?}", merged.len(), t.elapsed());
        self.message_list.emit(MessageListInput::SetMessages { messages: merged });
    }

    /// The keywords a tag view answers: one, or — the unified Tags row —
    /// every configured tag.
    /// With a tag view open, ask the accounts in its scope to bring their
    /// cached keywords in line with the server (#166). An account scanned
    /// within the last few seconds is left alone: reopening a view, or a
    /// refresh right after opening one, must not walk every folder twice.
    fn sync_tag_keywords(&mut self) {
        const PAUSE: std::time::Duration = std::time::Duration::from_secs(15);
        let Some((scope, _)) = self.tag_view.clone() else { return };
        let keywords: Vec<String> = self.tags.iter().map(|t| t.keyword.clone()).collect();
        if keywords.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        let ids: Vec<u32> = self
            .accounts
            .iter()
            .map(|a| a.id)
            .filter(|id| scope.map_or(true, |s| s == *id))
            .collect();
        for id in ids {
            if self.keyword_sync_at.get(&id).is_some_and(|t| now.duration_since(*t) < PAUSE) {
                continue;
            }
            self.keyword_sync_at.insert(id, now);
            self.send_to(id, MailRequest::RefreshKeywords { keywords: keywords.clone() });
        }
    }

    fn tag_view_keywords(&self, kw: &Option<String>) -> Vec<String> {
        match kw {
            Some(k) => vec![k.clone()],
            None => self.tags.iter().map(|t| t.keyword.clone()).collect(),
        }
    }

    /// Show the open tag view from its last result at once (an empty
    /// loading list if it never ran); [`refresh_tag_view`] brings it up to
    /// date behind.
    fn show_tag_view(&self) {
        let Some(key) = self.tag_view.as_ref() else { return };
        match self.tag_view_cache.get(key) {
            Some(cached) => {
                self.message_list.emit(MessageListInput::SetMessages { messages: cached.clone() })
            }
            None => self.message_list.emit(MessageListInput::SetLoading),
        }
    }

    /// Read the open tag view's rows from the on-disk index off the main
    /// thread (reading tens of thousands of rows per account is what made
    /// a tag click pause), answering with [`AppMsg::TagViewLoaded`]. The
    /// demo's mail never reaches the database, so its view is assembled
    /// here from the folders loaded so far.
    fn refresh_tag_view(&mut self, sender: &ComponentSender<Self>) {
        let Some(key) = self.tag_view.clone() else { return };
        if demo_mode() && self.config.is_empty() {
            let out = self.assemble_tag_view(&key, Vec::new());
            self.tag_view_cache.insert(key, out.clone());
            self.message_list.emit(MessageListInput::SetMessages { messages: out });
            return;
        }
        if self.tag_view_loading {
            self.tag_view_dirty = true;
            return;
        }
        self.tag_view_loading = true;
        let (scope, kw) = key.clone();
        let keywords = self.tag_view_keywords(&kw);
        let accounts: Vec<u32> = self
            .accounts
            .iter()
            .map(|a| a.id)
            .filter(|id| scope.map_or(true, |s| s == *id))
            .collect();
        let s = sender.clone();
        std::thread::spawn(move || {
            let mut rows: Vec<(u32, String, Message)> = Vec::new();
            if let Ok(cache) = crate::cache::Cache::open() {
                for account_id in accounts {
                    for kw in &keywords {
                        for (path, m) in cache.messages_with_keyword(account_id, kw) {
                            rows.push((account_id, path, m));
                        }
                    }
                }
            }
            s.input(AppMsg::TagViewLoaded { key, rows });
        });
    }

    /// The tag view (#71) from its rows: every message of every account in
    /// scope carrying the tag, newest first. Trash and Junk keep their tags
    /// but stay out; Gmail's per-label copies of one message collapse to one
    /// row, the inbox copy where there is one (its actions land where
    /// expected).
    fn assemble_tag_view(
        &self,
        key: &(Option<u32>, Option<String>),
        rows: Vec<(u32, String, Message)>,
    ) -> Vec<Message> {
        let (scope, kw) = key;
        let in_scope = |account_id: u32| scope.map_or(true, |id| id == account_id);
        let keywords = self.tag_view_keywords(kw);
        let mut out: Vec<Message> = Vec::new();
        let mut seen_rows: std::collections::HashSet<(u32, u32, u32)> =
            std::collections::HashSet::new();
        for (account_id, path, mut m) in rows {
            let Some(folders) = self.folders.get(&account_id) else { continue };
            let Some(f) = folders.iter().find(|f| f.path == path) else { continue };
            if matches!(f.kind, FolderKind::Trash | FolderKind::Junk) {
                continue;
            }
            m.folder_id = f.id;
            // A message with several tags answers several keywords.
            if seen_rows.insert((m.account_id, m.folder_id, m.uid)) {
                out.push(m);
            }
        }
        // The demo's mail never reaches the database, so its tag view reads
        // the folders loaded so far instead.
        if out.is_empty() && demo_mode() && self.config.is_empty() {
            for ((account_id, folder_id), messages) in &self.message_cache {
                if !in_scope(*account_id)
                    || matches!(
                        self.folder_kind(*account_id, *folder_id),
                        Some(FolderKind::Trash | FolderKind::Junk)
                    )
                {
                    continue;
                }
                out.extend(
                    messages
                        .iter()
                        .filter(|m| keywords.iter().any(|k| m.has_keyword(k)))
                        .cloned(),
                );
            }
        }
        let inbox_first = |m: &Message| {
            self.folder_kind(m.account_id, m.folder_id) != Some(FolderKind::Inbox)
        };
        out.sort_by(|a, b| {
            b.timestamp.cmp(&a.timestamp).then_with(|| inbox_first(a).cmp(&inbox_first(b)))
        });
        let mut seen: std::collections::HashSet<(u32, String)> = std::collections::HashSet::new();
        out.retain(|m| m.message_id.is_empty() || seen.insert((m.account_id, m.message_id.clone())));
        out
    }

    /// Fold an account's locally-kept tags (POP3, keyword-less servers) into
    /// summaries fresh from its worker, so they show like server-side ones.
    fn merge_local_tags(&self, account_id: u32, mut messages: Vec<Message>) -> Vec<Message> {
        if let Some(cache) = self.cache.as_ref() {
            cache.apply_local_tags(account_id, &mut messages);
        }
        messages
    }

    /// The tag colours as CSS: `.tag-<keyword>` fills (chips), and the same
    /// class on a `.tag-tint` widget colours its glyph instead.
    fn refresh_tag_css(&self) {
        let mut css = String::new();
        for t in &self.tags {
            let class = t.css_class();
            let text = crate::color::readable_text(&t.color);
            css.push_str(&format!(
                ".{class} {{ background-color: {color}; color: {text}; }} \
                 .{class} label {{ color: {text}; }} \
                 .tag-tint.{class} {{ color: {color}; background-color: transparent; }}\n",
                color = t.color,
            ));
        }
        self.tag_provider.load_from_data(&css);
    }

    /// Put a tag on a message or take it off (#71): the server (or the local
    /// store, where the server keeps none), then every copy the UI holds —
    /// the list row, the reader, a pop-out — so nothing waits for a sync.
    fn set_tag(&mut self, m: &Message, keyword: &str, add: bool) {
        if self.outbox_item(m.account_id, m.id).is_some() {
            return;
        }
        let Some(path) = self.resolve_folder_path(m) else { return };
        self.send_to(m.account_id, MailRequest::SetKeyword {
            path,
            uid: m.uid,
            message_id: m.message_id.clone(),
            keyword: keyword.to_string(),
            add,
        });
        let mut patched = m.clone();
        patched.set_keyword(keyword, add);
        let keywords = patched.keywords.clone();
        let (aid, fid, id) = (m.account_id, m.folder_id, m.id);
        let same = |c: &Message| c.account_id == aid && c.id == id;
        if let Some(msgs) = self.message_cache.get_mut(&(aid, fid)) {
            for c in msgs.iter_mut().filter(|c| same(c)) {
                c.keywords = keywords.clone();
            }
        }
        if let Some(msgs) = self.unified_slices.get_mut(&(aid, fid)) {
            for c in msgs.iter_mut().filter(|c| same(c)) {
                c.keywords = keywords.clone();
            }
        }
        for tm in self.current_thread.iter_mut().filter(|c| same(c)) {
            tm.keywords = keywords.clone();
        }
        if let Some(cur) = self.current.as_mut().filter(|c| same(c)) {
            cur.keywords = keywords.clone();
        }
        self.message_list.emit(MessageListInput::SetKeywords { id, keywords: keywords.clone() });
        self.message_view.emit(MessageViewInput::SetCardKeywords {
            account_id: aid,
            id,
            keywords: keywords.clone(),
        });
        if let Some(p) = self.popouts.get(&(aid, id)) {
            p.controller.emit(MessageWindowInput::SetKeywords(keywords));
        }
        // In this tag's own view an untagged message has no row to keep.
        if !add
            && self
                .tag_view
                .as_ref()
                .is_some_and(|(_, v)| v.as_deref().is_some_and(|v| v.eq_ignore_ascii_case(keyword)))
        {
            self.message_list.emit(MessageListInput::Remove(id));
        }
    }

    fn refresh_list_display(&self) {
        if self.unified {
            self.emit_unified();
        } else if self.tag_view.is_some() {
            self.show_tag_view();
        } else if let Some(sel) = self.selected.as_ref() {
            if let Some(msgs) = self.message_cache.get(&(sel.account_id, sel.folder_id)) {
                self.message_list.emit(MessageListInput::SetMessages { messages: msgs.clone() });
            }
        }
    }

    /// Dialog to add an email to GNOME Contacts (choosing the address book).
    fn show_add_contact_dialog(&self, name: &str, email: &str, sender: &ComponentSender<Self>) {
        let books = crate::contacts::writable_books();
        if books.is_empty() || email.trim().is_empty() {
            self.notifications.emit(NotifyInput::Push {
                text: i18n("No address book available to add contacts"),
                error: true,
                connectivity: false,
            });
            return;
        }

        let dialog = adw::MessageDialog::new(
            Some(&self.window),
            Some(i18n("Add to Contacts").as_str()),
            None,
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("add", &i18n("Add"));
        dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("add"));
        dialog.set_close_response("cancel");

        let form = gtk::ListBox::new();
        form.add_css_class("boxed-list");
        form.set_selection_mode(gtk::SelectionMode::None);
        let name_row = adw::EntryRow::new();
        name_row.set_title(&i18n("Name"));
        name_row.set_text(name);
        let email_row = adw::EntryRow::new();
        email_row.set_title(&i18n("Email"));
        email_row.set_text(email);
        let book_row = adw::ComboRow::new();
        book_row.set_title(&i18n("Address book"));
        let labels: Vec<&str> = books.iter().map(|b| b.name.as_str()).collect();
        book_row.set_model(Some(&gtk::StringList::new(&labels)));
        form.append(&name_row);
        form.append(&email_row);
        form.append(&book_row);
        dialog.set_extra_child(Some(&form));

        let input = sender.input_sender().clone();
        dialog.connect_response(None, move |_, resp| {
            if resp != "add" {
                return;
            }
            let name = name_row.text().trim().to_string();
            let email = email_row.text().trim().to_string();
            let idx = book_row.selected() as usize;
            let Some(book) = books.get(idx).cloned() else {
                return;
            };
            if email.is_empty() {
                return;
            }
            // Writing talks to EDS over D-Bus (blocking) — do it off the UI thread.
            let input = input.clone();
            std::thread::spawn(move || {
                let result = crate::contacts::add_or_merge(&book.uid, &name, &email);
                let _ = input.send(AppMsg::ContactAdded(result));
            });
        });
        dialog.present();
    }

    /// Open (or focus) the combined Settings window on the
    /// requested panel. When `add_new`, jump straight to the "add account"
    /// form — used by the empty-state "Add first account" button.
    /// Build the Settings window and its accounts panel, hidden. It is
    /// kept for the life of the app once built: `open_settings_window`
    /// presents it, and closing it hides it, so opening never waits on
    /// hundreds of rows being built and laid out again.
    fn build_settings_window(&mut self, sender: &ComponentSender<Self>, on_accounts: bool) {
        let t_open = std::time::Instant::now();
        // Pass accounts in display order. Passwords stay in the keyring until
        // an account's editor opens (the panel fetches them then, off the
        // main thread): reading every account's secrets up front cost a
        // keyring round trip or three per account, which is what made this
        // window slow to appear.
        let init = self.accounts_panel_init();
        self.accounts_seed = Some(format!("{init:?}"));
        let accounts = Self::launch_accounts_panel(init, sender);
        let init = PrefInit {
            auto_remote_content: self.auto_remote_content,
            show_remote_banner: self.show_remote_banner,
            gravatar: self.gravatar,
            avatars: self.avatars,
            own_mailbox_face: self.own_mailbox_face,
            sender_logos: self.sender_logos,
            date_style: self.date_style,
            clock_style: self.clock_style,
            language: config::load_language(),
            fetch_interval_secs: self.fetch_interval_secs,
            push: self.push,
            palette_collapse_secs: self.palette_collapse_secs,
            card_palette_collapse_secs: self.card_palette_collapse_secs,
            threading: self.threading,
            threads_expanded: self.threads_expanded,
            thread_newest_first: self.thread_newest_first,
            always_show_recipients: self.always_show_recipients,
            single_message_card: self.single_message_card,
            reader_switch: self.reader_switch,
            reader_default: self.reader_default,
            reader_zoom: self.zoom_default,
            card_attachments: self.card_attachments,
            attachment_drawer: self.drawer_enabled,
            thread_expansion: self.thread_expansion,
            confirm_thread_delete: self.confirm_thread_delete,
            message_theme: self.message_theme,
            override_fonts: self.override_fonts,
            reader_font: self.reader_font.clone(),
            override_colors: self.override_colors,
            plain_monospace: self.plain_monospace,
            plain_font: self.plain_font.clone(),
            compose_format: self.compose_format,
            reply_position: self.reply_position,
            signature_position: self.signature_position,
            notifications: self.notifications_enabled,
            notification_content: self.notification_content,
            notification_buttons: self.notification_buttons,
            show_attachments: self.show_attachments,
            show_contacts: self.show_contacts,
            show_unified: self.show_unified_pref,
            unified_chips: self.unified_chips,
            unified_filtered: self.unified_filtered,
            unified_kinds: self.unified_kinds,
            unified_tags: self.unified_tags,
            show_accounts: self.show_accounts,
            filtered_placement: self.filtered_placement,
            tags_placement: self.tags_placement,
            chevrons_left: self.chevrons_left,
            console_mode: self.console_mode,
            read_mark: self.read_mark,
            settings_open_accounts: self.settings_open_accounts,
            sidebar_hover_expand: self.sidebar_hover_expand,
            remember_sidebar: self.remember_sidebar,
            remember_rail: self.remember_rail,
            rail_dots: self.rail_dots,
            rail_fold: self.rail_fold,
            reader_toolbar: self.reader_toolbar.clone(),
            focus: self.focus,
            card_actions_hover: self.card_actions_hover,
            card_actions_auto: self.card_actions_auto,
            list_palette: self.list_palette,
            list_palette_hover: self.list_palette_hover,
            card_palette_menu: self.card_palette_menu,
            swipe_enabled: self.swipe_enabled,
            swipe_reversed: self.swipe_reversed,
            swipe_sensitivity: self.swipe_sensitivity,
            compose_inline: self.compose_inline,
            reply_fields: self.reply_fields,
            files: self.files_prefs,
            link_browser: self.link_browser.clone(),
            compose_default_from: self.compose_default_from.clone(),
            paste_plain: self.paste_plain,
            spellcheck: self.spellcheck,
            spellcheck_langs: self.spellcheck_langs.clone(),
            app_theme: self.app_theme,
            theme: self.theme.clone(),
            preview_lines: self.preview_lines,
            single_key_shortcuts: self.single_key.get(),
            run_in_background: self.run_in_background.get(),
            autostart: self.autostart,
            tray: self.tray_enabled,
            tray_icon: self.tray_icon,
            tray_mail: self.tray_mail,
            app_icon: self.app_icon.clone(),
            accounts_panel: accounts.widget().clone().upcast::<gtk::Widget>(),
            accounts_sender: accounts.sender().clone(),
            identities: self
                .config
                .iter()
                .filter(|a| a.enabled)
                .flat_map(|a| {
                    std::iter::once((a.name.clone(), a.email.clone())).chain(a.aliases.iter().map(|al| {
                        let (name, addr) = crate::config::split_identity(&al.identity);
                        (if name.is_empty() { a.name.clone() } else { name }, addr)
                    }))
                })
                .collect(),
            start_on_accounts: on_accounts,
            start_page: if on_accounts { Some("accounts".to_string()) } else { self.last_settings_page.clone() },
        };
        let prefs = Preferences::builder()
            .transient_for(&self.window)
            .launch(init)
            .forward(sender.input_sender(), |out| match out {
                PrefOutput::SetAutoRemoteContent(on) => AppMsg::SetAutoRemoteContent(on),
                PrefOutput::SetShowRemoteBanner(on) => AppMsg::SetShowRemoteBanner(on),
                PrefOutput::SetGravatar(on) => AppMsg::SetGravatar(on),
                PrefOutput::SetAvatars(on) => AppMsg::SetAvatars(on),
                PrefOutput::SetOwnMailboxFace(on) => AppMsg::SetOwnMailboxFace(on),
                PrefOutput::SetSenderLogos(on) => AppMsg::SetSenderLogos(on),
                PrefOutput::SetDateStyle(style) => AppMsg::SetDateStyle(style),
                PrefOutput::SetClockStyle(style) => AppMsg::SetClockStyle(style),
                PrefOutput::SetLanguage(code) => AppMsg::SetLanguage(code),
                PrefOutput::SetThreading(on) => AppMsg::SetThreading(on),
                PrefOutput::SetThreadExpansion(on) => AppMsg::SetThreadExpansion(on),
                PrefOutput::SetConfirmThreadDelete(on) => {
                    AppMsg::SetConfirmThreadDelete(on)
                }
                PrefOutput::SetThreadsExpanded(on) => AppMsg::SetThreadsExpanded(on),
                PrefOutput::SetThreadNewestFirst(on) => AppMsg::SetThreadNewestFirst(on),
                PrefOutput::SetAlwaysShowRecipients(on) => AppMsg::SetAlwaysShowRecipients(on),
                PrefOutput::SetSingleMessageCard(on) => AppMsg::SetSingleMessageCard(on),
                PrefOutput::SetReaderSwitch(on) => AppMsg::SetReaderSwitchShown(on),
                PrefOutput::SetReaderDefault(p) => AppMsg::SetReaderDefault(p),
                PrefOutput::SetReaderZoom(z) => AppMsg::SetZoomDefault(z),
                PrefOutput::SetCardAttachments(on) => AppMsg::SetCardAttachments(on),
                PrefOutput::SetAttachmentDrawer(on) => AppMsg::SetAttachmentDrawer(on),
                PrefOutput::SetCardActionsMode { hover_toggle, hover_auto } => {
                    AppMsg::SetCardActionsMode { hover_toggle, hover_auto }
                }
                PrefOutput::SetListPalette(on) => AppMsg::SetListPalette(on),
                PrefOutput::SetListPaletteHover(on) => AppMsg::SetListPaletteHover(on),
                PrefOutput::SetCardPaletteMenu(on) => AppMsg::SetCardPaletteMenu(on),
                PrefOutput::SetSwipeEnabled(on) => AppMsg::SetSwipeEnabled(on),
                PrefOutput::SetSwipeReversed(on) => AppMsg::SetSwipeReversed(on),
                PrefOutput::SetSwipeSensitivity(v) => AppMsg::SetSwipeSensitivity(v),
                PrefOutput::SetComposeInline(on) => AppMsg::SetComposeInline(on),
                PrefOutput::SetReplyFields(on) => AppMsg::SetReplyFields(on),
                PrefOutput::SetFilesPrefs(p) => AppMsg::SetFilesPrefs(p),
                PrefOutput::SetLinkBrowser(id) => AppMsg::SetLinkBrowser(id),
                PrefOutput::SetComposeDefaultFrom(addr) => AppMsg::SetComposeDefaultFrom(addr),
                PrefOutput::SetPastePlain(on) => AppMsg::SetPastePlain(on),
                PrefOutput::SetSpellcheck(on) => AppMsg::SetSpellcheck(on),
                PrefOutput::SetSpellcheckLangs(l) => AppMsg::SetSpellcheckLangs(l),
                PrefOutput::SetFetchInterval(secs) => AppMsg::SetFetchInterval(secs),
                PrefOutput::SetPush(on) => AppMsg::SetPush(on),
                PrefOutput::SetNotifications(on) => AppMsg::SetNotifications(on),
                PrefOutput::SetNotificationContent(on) => {
                    AppMsg::SetNotificationContent(on)
                }
                PrefOutput::SetNotificationButtons(b) => AppMsg::SetNotificationButtons(b),
                PrefOutput::SetAttachmentsRow(show) => AppMsg::SetAttachmentsRow(show),
                PrefOutput::SetContactsRow(show) => AppMsg::SetContactsRow(show),
                PrefOutput::SetShowUnified(show) => AppMsg::SetShowUnified(show),
                PrefOutput::SetUnifiedChips(chips) => AppMsg::SetUnifiedChips(chips),
                PrefOutput::SetUnifiedFiltered(show) => AppMsg::SetUnifiedFiltered(show),
                PrefOutput::SetUnifiedKinds(kinds) => AppMsg::SetUnifiedKinds(kinds),
                PrefOutput::SetUnifiedTags(show) => AppMsg::SetUnifiedTags(show),
                PrefOutput::SetShowAccounts(show) => AppMsg::SetShowAccounts(show),
                PrefOutput::SetFilteredPlacement(p) => AppMsg::SetFilteredPlacement(p),
                PrefOutput::SetTagsPlacement(p) => AppMsg::SetTagsPlacement(p),
                PrefOutput::SetChevronsLeft(left) => AppMsg::SetChevronsLeft(left),
                PrefOutput::SetConsoleMode(on) => AppMsg::SetConsoleMode(on),
                PrefOutput::SetReadMark(policy) => AppMsg::SetReadMark(policy),
                PrefOutput::ExportSettings => AppMsg::ExportSettings,
                PrefOutput::ExportLog => AppMsg::ExportLog,
                PrefOutput::ImportSettings => AppMsg::ImportSettings,
                PrefOutput::PageShown(id) => AppMsg::SettingsPageShown(id),
                PrefOutput::SetSidebarHoverExpand(on) => {
                    AppMsg::SetSidebarHoverExpand(on)
                }
                PrefOutput::SetRememberSidebar(on) => AppMsg::SetRememberSidebar(on),
                PrefOutput::SetRememberRail(on) => AppMsg::SetRememberRail(on),
                PrefOutput::SetRailDots(on) => AppMsg::SetRailDots(on),
                PrefOutput::SetReaderToolbar(layout) => AppMsg::SetReaderToolbar(layout),
                PrefOutput::SetFocusMode(focus) => AppMsg::SetFocusMode(focus),
                PrefOutput::SetRailFold(fold) => AppMsg::SetRailFold(fold),
                PrefOutput::SetAppTheme(theme) => AppMsg::SetAppTheme(theme),
                PrefOutput::SetTheme(id) => AppMsg::SetTheme(id),
                PrefOutput::SetSettingsOpenAccounts(on) => {
                    AppMsg::SetSettingsOpenAccounts(on)
                }
                PrefOutput::SetPreviewLines(n) => AppMsg::SetPreviewLines(n),
                PrefOutput::SetSingleKey(on) => AppMsg::SetSingleKey(on),
                PrefOutput::SetRunInBackground(on) => AppMsg::SetRunInBackground(on),
                PrefOutput::SetAutostart(on) => AppMsg::SetAutostart(on),
                PrefOutput::SetTray(on) => AppMsg::SetTray(on),
                PrefOutput::SetTrayIcon(icon) => AppMsg::SetTrayIcon(icon),
                PrefOutput::SetTrayMail(on) => AppMsg::SetTrayMail(on),
                PrefOutput::SetAppIcon(id) => AppMsg::SetAppIcon(id),
                PrefOutput::SetPaletteCollapse(secs) => AppMsg::SetPaletteCollapse(secs),
                PrefOutput::SetCardPaletteCollapse(secs) => AppMsg::SetCardPaletteCollapse(secs),
                PrefOutput::SetMessageTheme(t) => AppMsg::SetMessageTheme(t),
                PrefOutput::SetOverrideFonts(on) => AppMsg::SetOverrideFonts(on),
                PrefOutput::SetReaderFont(font) => AppMsg::SetReaderFont(font),
                PrefOutput::SetOverrideColors(on) => AppMsg::SetOverrideColors(on),
                PrefOutput::SetPlainMonospace(on) => AppMsg::SetPlainMonospace(on),
                PrefOutput::SetPlainFont(font) => AppMsg::SetPlainFont(font),
                PrefOutput::SetComposeFormat(f) => AppMsg::SetComposeFormat(f),
                PrefOutput::SetReplyPosition(p) => AppMsg::SetReplyPosition(p),
                PrefOutput::SetSignaturePosition(p) => AppMsg::SetSignaturePosition(p),
                PrefOutput::Closed => AppMsg::ClosePreferences,
            });
        accounts.emit(crate::ui::accounts::AccountsInput::SetFolderChoices(
            self.folder_choice_map(),
        ));
        // Showcase hook: HYLKI_SHOWCASE_EDIT_FILTER=<index> opens that
        // filter rule's editor once the panel is up, for a capture.
        // Sent through the app so it reaches whichever panel is current
        // when it fires: an open right after the pre-warm may have swapped
        // in a fresh one.
        if let Some(Ok(i)) = std::env::var("HYLKI_SHOWCASE_EDIT_FILTER").ok().map(|v| v.parse::<usize>()) {
            if demo_mode() {
                let s = sender.clone();
                gtk::glib::timeout_add_seconds_local_once(2, move || {
                    s.input(AppMsg::ShowcaseEditFilter(i));
                });
            }
        }
        // HYLKI_SHOWCASE_FIND_TAGS=1 runs the tag finder once the panel is
        // up (the demo backend answers with a fixed set); the report dialog
        // captures itself two seconds after it appears.
        if std::env::var("HYLKI_SHOWCASE_FIND_TAGS").is_ok() && demo_mode() {
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(2, move || s.input(AppMsg::FindTags));
        }
        // HYLKI_SHOWCASE_EDIT_TAG=<index> likewise opens that tag's editor
        // (#147); an index past the end opens the Add Tag dialog.
        if let Some(Ok(i)) = std::env::var("HYLKI_SHOWCASE_EDIT_TAG").ok().map(|v| v.parse::<usize>()) {
            if demo_mode() {
                let a = accounts.sender().clone();
                let n = self.tags.len();
                gtk::glib::timeout_add_seconds_local_once(2, move || {
                    let _ = a.send(if i < n {
                        crate::ui::accounts::AccountsInput::EditTag(i)
                    } else {
                        crate::ui::accounts::AccountsInput::AddTag
                    });
                });
            }
        }
        self.accounts_win = Some(accounts);
        tracing::debug!("settings window: built in {:?}", t_open.elapsed());
        self.prefs = Some(prefs);
    }

    /// A fresh accounts panel over the current accounts (in display
    /// order), filters, tags and sender lists. Passwords stay in the keyring
    /// until an account's editor opens (the panel fetches them then, off
    /// the main thread): reading every account's secrets up front cost a
    /// keyring round trip or three per account, which is what made this
    /// window slow to appear with many accounts.
    fn accounts_panel_init(&self) -> crate::ui::accounts::AccountsInit {
        let order = self.ordered_emails();
        let mut accounts: Vec<AccountConfig> = Vec::new();
        for email in &order {
            if let Some(a) = self.config.iter().find(|c| &c.email == email) {
                accounts.push(a.clone());
            }
        }
        for c in &self.config {
            if !accounts.iter().any(|a| a.email == c.email) {
                accounts.push(c.clone());
            }
        }
        // Demo mode: the sample accounts exist only at the backend layer, so
        // the Accounts panel would sit empty in screenshots — hand it
        // matching stand-in configs instead.
        if accounts.is_empty() && demo_mode() {
            accounts = self.demo_config.clone();
        }
        // The accounts panel component (embedded behind the "Accounts" tab).
        crate::ui::accounts::AccountsInit {
                accounts,
                allowed_senders: self.allowed_senders.clone(),
                blacklist: self.blacklist.clone(),
                filters: self.filters.clone(),
                tags: self.tags.clone(),
            }
    }

    /// Launch an accounts panel over `init`.
    fn launch_accounts_panel(
        init: crate::ui::accounts::AccountsInit,
        sender: &ComponentSender<Self>,
    ) -> Controller<AccountsWindow> {
        let accounts = AccountsWindow::builder()
            .launch(init)
            .forward(sender.input_sender(), |out| match out {
                AccountsOutput::Saved { original_email, account } => {
                    AppMsg::AccountSaved { original_email, account }
                }
                AccountsOutput::Removed { email } => AppMsg::AccountRemoved { email },
                AccountsOutput::Reordered(emails) => AppMsg::AccountsReordered(emails),
                AccountsOutput::EnabledChanged { email, enabled } => {
                    AppMsg::AccountEnabledChanged { email, enabled }
                }
                AccountsOutput::ImportGoa(account) => AppMsg::ImportGoaAccount(account),
                AccountsOutput::EditorOpen(open) => AppMsg::SettingsEditorOpen(open),
                AccountsOutput::AddSender(addr) => AppMsg::AddSender(addr),
                AccountsOutput::RemoveSender(addr) => AppMsg::RemoveSender(addr),
                AccountsOutput::AddBlacklist(addr) => AppMsg::AddBlacklist(addr),
                AccountsOutput::RemoveBlacklist(addr) => AppMsg::RemoveBlacklist(addr),
                AccountsOutput::SetFilters(rules) => AppMsg::SetFilters(rules),
                AccountsOutput::ApplyFilters => AppMsg::ApplyFilters(Vec::new()),
                AccountsOutput::SetTags(tags) => AppMsg::SetTags(tags),
                AccountsOutput::FindTags => AppMsg::FindTags,
                AccountsOutput::LeftEditor(page) => AppMsg::SettingsLeaveEditor { page, ask: false },
                AccountsOutput::LeaveNeedsPrompt(page) => {
                    AppMsg::SettingsLeaveEditor { page, ask: true }
                }
            });

        // The host window: the preferences component, carrying the accounts
        // panel behind its other tab.
        accounts
    }

    /// The tag finder's scan is over: fold every account's findings into one
    /// list (a keyword seen on two accounts is one entry), drop the keywords
    /// that are tags already, propose a name and colour for each, and hand
    /// the report to the Tags page — bringing Settings back if it was closed
    /// meanwhile.
    fn finish_tag_scan(&mut self, sender: &ComponentSender<Self>) {
        let Some(scan) = self.tag_scan.take() else { return };
        let many = scan.found.len() > 1;
        let mut merged: Vec<(KeywordFinding, Vec<String>)> = Vec::new();
        for (account_id, findings) in scan.found {
            let label = self
                .accounts
                .iter()
                .find(|a| a.id == account_id)
                .map(|a| if a.name.trim().is_empty() { a.email.clone() } else { a.name.clone() })
                .unwrap_or_default();
            for f in findings {
                if crate::worker::is_system_keyword(&f.keyword)
                    || self.tags.iter().any(|t| t.keyword.eq_ignore_ascii_case(&f.keyword))
                {
                    continue;
                }
                // One entry per account: its folders, named by the account
                // when more than one took part.
                let places: Vec<String> = if f.folders.is_empty() {
                    Vec::new()
                } else if many {
                    vec![format!("{label}: {}", f.folders.join(", "))]
                } else {
                    vec![f.folders.join(", ")]
                };
                match merged.iter_mut().find(|(m, _)| m.keyword.eq_ignore_ascii_case(&f.keyword)) {
                    Some((m, p)) => {
                        m.count += f.count;
                        if m.name.is_none() {
                            m.name = f.name.clone();
                        }
                        if m.color.is_none() {
                            m.color = f.color.clone();
                        }
                        p.extend(places);
                    }
                    None => merged.push((f, places)),
                }
            }
        }
        let mut taken: Vec<String> = self.tags.iter().map(|t| t.color.clone()).collect();
        let proposals: Vec<crate::ui::accounts::TagProposal> = merged
            .into_iter()
            .map(|(f, places)| {
                let name = f.name.clone().unwrap_or_else(|| config::Tag::name_for_keyword(&f.keyword));
                let color = f
                    .color
                    .clone()
                    .unwrap_or_else(|| config::Tag::color_for_keyword(&f.keyword, &taken));
                taken.push(color.clone());
                crate::ui::accounts::TagProposal {
                    tag: config::Tag { name, keyword: f.keyword, color },
                    count: f.count,
                    places,
                }
            })
            .collect();
        self.open_settings_window(sender, true, false);
        if let Some(p) = &self.prefs {
            p.emit(PrefInput::ShowPageById("tags".to_string()));
        }
        if let Some(a) = &self.accounts_win {
            a.emit(crate::ui::accounts::AccountsInput::TagFindings(proposals));
        }
    }

    /// Show Settings: the window kept from a previous open (its accounts
    /// panel rebuilt over the current accounts) or a newly built one.
    fn open_settings_window(
        &mut self,
        sender: &ComponentSender<Self>,
        on_accounts: bool,
        add_new: bool,
    ) {
        let t_open = std::time::Instant::now();
        let hidden = self.prefs.as_ref().is_some_and(|p| !p.widget().is_visible());
        if self.prefs.is_none() {
            self.build_settings_window(sender, on_accounts);
        } else if hidden {
            // Accounts, filters, tags and senders may have changed since: a
            // fresh panel over today's state — or the kept one, when
            // nothing did, with any editor it was left in closed.
            let init = self.accounts_panel_init();
            let seed = format!("{init:?}");
            let page = if on_accounts {
                "accounts".to_string()
            } else {
                self.last_settings_page.clone().unwrap_or_else(|| "general".to_string())
            };
            if self.accounts_seed.as_deref() == Some(seed.as_str()) {
                if let Some(a) = &self.accounts_win {
                    a.emit(crate::ui::accounts::AccountsInput::CloseEditor);
                }
            } else {
                let accounts = Self::launch_accounts_panel(init, sender);
                // A fresh panel knows no folders until told: without this
                // the filter editor's "Move to" list held only "Leave in
                // Inbox" whenever Settings reopened after a change.
                accounts.emit(crate::ui::accounts::AccountsInput::SetFolderChoices(
                    self.folder_choice_map(),
                ));
                self.accounts_seed = Some(seed);
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::SetAccountsPanel {
                        panel: accounts.widget().clone().upcast(),
                        sender: accounts.sender().clone(),
                    });
                }
                self.accounts_win = Some(accounts);
            }
            if let Some(p) = &self.prefs {
                p.emit(PrefInput::EditorOpen(None));
                p.emit(PrefInput::ShowPageById(page));
            }
        } else if on_accounts {
            // Already showing: switch to Accounts if asked, else leave the
            // window on whatever category it is showing.
            if let Some(p) = &self.prefs {
                p.emit(PrefInput::ShowAccounts(true));
            }
        }
        if let Some(p) = &self.prefs {
            p.widget().present();
        }
        if add_new {
            if let Some(a) = &self.accounts_win {
                a.emit(crate::ui::accounts::AccountsInput::AddAccount);
            }
        }
        tracing::debug!("settings window: presented in {:?}", t_open.elapsed());
        {
            let t = t_open;
            gtk::glib::idle_add_local_once(move || {
                tracing::debug!("settings window: first idle after open at {:?}", t.elapsed());
            });
            if let Some(p) = &self.prefs {
                p.widget().add_tick_callback(move |_, _| {
                    tracing::debug!("settings window: first frame at {:?}", t.elapsed());
                    gtk::glib::ControlFlow::Break
                });
            }
        }
    }

    /// Confirm and remove an account (drops its keyring password too).
    fn confirm_remove_account(&self, account_id: u32, sender: &ComponentSender<Self>) {
        let Some(email) = self.email_of(account_id) else {
            return;
        };
        let label = self.account_label(account_id);
        let dialog = adw::MessageDialog::new(
            Some(&self.window),
            Some(i18n("Remove Account?").as_str()),
            Some(&format!(
                "Remove {label} from Hylki? Its saved password is deleted. \
                 Mail on the server is not affected."
            )),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("remove", &i18n("Remove"));
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let s = sender.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "remove" {
                s.input(AppMsg::AccountRemoved { email: email.clone() });
            }
        });
        dialog.present();
    }

    /// A custom, scrollable About window: app identity up top (icon, name, a
    /// version chip, and a one-line description under it), then the feature
    /// sections laid out on the page itself, and project links.
    fn open_about(&self, sender: &ComponentSender<Self>) {
        let win = adw::Window::builder()
            .transient_for(&self.window)
            .modal(false)
            .title(format!("{} {}", i18n("About"), crate::APP_NAME).as_str())
            .default_width(460)
            // Remembered vertical size (tall by default) — resizing sticks
            // across restarts via the save on close below.
            .default_height(config::load_about_height())
            .build();
        win.connect_close_request(|w| {
            config::save_about_height(w.height());
            gtk::glib::Propagation::Proceed
        });

        // A navigation stack so Release Notes / Changelog slide in (and back out)
        // within the same window instead of spawning separate ones.
        let nav = adw::NavigationView::new();

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let clamp = adw::Clamp::builder().maximum_size(420).build();
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.set_margin_top(18);
        page.set_margin_bottom(12);

        // Identity block: the wordmark, wizard-style.
        // Same Overlay-with-spacer cap as the wizard — a Picture's texture
        // wins over both width requests and clamps.
        let wm_pic = crate::ui::welcome::wordmark_picture(182);
        let wm_frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wm_frame.set_size_request(182, crate::ui::welcome::wordmark_height(182.0));
        let wm = gtk::Overlay::new();
        wm.set_child(Some(&wm_frame));
        wm.add_overlay(&wm_pic);
        wm.set_clip_overlay(&wm_pic, true);
        wm.set_halign(gtk::Align::Center);
        wm.set_margin_bottom(10);
        page.append(&wm);

        let version = gtk::Label::new(Some(crate::VERSION));
        version.add_css_class("about-version-chip");
        version.set_halign(gtk::Align::Center);
        version.set_margin_top(8);
        page.append(&version);

        // One-sentence description, directly under the version chip.
        let desc = gtk::Label::new(Some(i18n("A fast, GNOME-native email client").as_str()));
        desc.set_wrap(true);
        desc.set_justify(gtk::Justification::Center);
        desc.add_css_class("dim-label");
        desc.set_margin_top(12);
        page.append(&desc);

        if cfg!(feature = "beta") {
            let warn = gtk::Label::new(Some(
                i18n("This is a beta build for trying upcoming changes early. \
                 Expect bugs and instability — please report anything broken \
                 on GitHub. It shares your accounts and mail with the stable \
                 Hylki install.").as_str(),
            ));
            warn.set_wrap(true);
            warn.set_justify(gtk::Justification::Center);
            warn.add_css_class("warning");
            warn.set_margin_top(10);
            warn.set_margin_start(12);
            warn.set_margin_end(12);
            page.append(&warn);
        }

        // Release notes: slide in as a sub-page of this window.
        let info = gtk::ListBox::new();
        info.add_css_class("boxed-list");
        info.set_selection_mode(gtk::SelectionMode::None);
        info.set_margin_top(20);

        let notes_row = adw::ActionRow::builder()
            .title(&i18n("Release Notes"))
            .subtitle(format!("What's new in {}", crate::VERSION))
            .activatable(true)
            .build();
        notes_row.add_suffix(&gtk::Image::from_icon_name("co.hyprlab.Hylki-go-next-symbolic"));
        {
            let nav = nav.clone();
            notes_row.connect_activated(move |_| nav.push_by_tag("notes"));
        }
        info.append(&notes_row);

        // Linux Mint (Cinnamon): re-open the one-time keyring setup tip. Shown only
        // where that tip applies, so Mint users who dismissed it can find it again.
        if crate::platform::is_mint_cinnamon() {
            let keyring_row = adw::ActionRow::builder()
                .title(&i18n("Keyring Setup Help"))
                .subtitle(&i18n("Make account passwords persist on Linux Mint"))
                .activatable(true)
                .build();
            keyring_row.add_suffix(&gtk::Image::from_icon_name("co.hyprlab.Hylki-go-next-symbolic"));
            let sender = sender.clone();
            keyring_row.connect_activated(move |_| {
                sender.input(AppMsg::ShowKeyringHelp { problem: false });
            });
            info.append(&keyring_row);
        }

        page.append(&info);

        // Project links. Each row shows its URL as a hover tooltip.
        let links_title = gtk::Label::new(Some(i18n("Project").as_str()));
        links_title.add_css_class("heading");
        links_title.set_halign(gtk::Align::Start);
        links_title.set_margin_top(20);
        links_title.set_margin_bottom(6);
        page.append(&links_title);

        let links = gtk::ListBox::new();
        links.add_css_class("boxed-list");
        links.set_selection_mode(gtk::SelectionMode::None);
        let mk_row = |title: &str, url: &str| -> adw::ActionRow {
            let row = adw::ActionRow::builder().title(title).activatable(true).build();
            row.set_tooltip_text(Some(url));
            row.add_suffix(&gtk::Image::from_icon_name("co.hyprlab.Hylki-adw-external-link-symbolic"));
            let u = url.to_string();
            row.connect_activated(move |_| crate::oauth::open_uri(&u));
            row
        };
        links.append(&mk_row(&i18n("Website"), "https://hylki.hyprlab.co"));
        links.append(&mk_row(
            &i18n("Report an issue or feature request"),
            "https://github.com/hyprlab/hylki/issues",
        ));
        links.append(&mk_row("Discord", "https://discord.gg/YfEJ4b6PFW"));
        links.append(&mk_row("hyprlab@proton.me", "mailto:hyprlab@proton.me"));
        links.append(&mk_row(&i18n("Source Code"), "https://github.com/hyprlab/hylki"));
        links.append(&mk_row(&i18n("License (GNU AGPL v3)"), "https://www.gnu.org/licenses/agpl-3.0.html"));

        // Buy Me a Coffee — with a coffee-cup glyph as its leading icon.
        let coffee = adw::ActionRow::builder()
            .title(&i18n("Buy Me a Coffee"))
            .activatable(true)
            .build();
        coffee.set_tooltip_text(Some("https://buymeacoffee.com/hyprlab"));
        let cup = gtk::Label::new(Some("☕"));
        cup.add_css_class("about-coffee");
        coffee.add_prefix(&cup);
        coffee.add_suffix(&gtk::Image::from_icon_name("co.hyprlab.Hylki-adw-external-link-symbolic"));
        coffee.connect_activated(move |_| crate::oauth::open_uri("https://buymeacoffee.com/hyprlab"));
        links.append(&coffee);
        page.append(&links);

        // Contributors — people outside Hyprlab whose patches are in the app.
        let thanks_title = gtk::Label::new(Some(i18n("Special thanks to these contributors").as_str()));
        thanks_title.add_css_class("heading");
        thanks_title.set_halign(gtk::Align::Start);
        thanks_title.set_margin_top(20);
        thanks_title.set_margin_bottom(6);
        page.append(&thanks_title);

        let people = |rows: Vec<(&'static str, &'static str, &'static str)>| {
            let list = gtk::ListBox::new();
            list.add_css_class("boxed-list");
            list.set_selection_mode(gtk::SelectionMode::None);
            for (name, handle, note) in rows {
                let subtitle = if note.is_empty() {
                    format!("@{handle}")
                } else {
                    format!("{note} · @{handle}")
                };
                let row = adw::ActionRow::builder()
                    .title(name)
                    .subtitle(subtitle)
                    .activatable(true)
                    .build();
                let url = format!("https://github.com/{handle}");
                row.set_tooltip_text(Some(&url));
                row.add_suffix(&gtk::Image::from_icon_name("co.hyprlab.Hylki-adw-external-link-symbolic"));
                row.connect_activated(move |_| crate::oauth::open_uri(&url));
                list.append(&row);
            }
            list
        };
        page.append(&people(credits(CONTRIBUTORS)));

        // Translators — the same list, with the languages each one carried.
        let translators_title = gtk::Label::new(Some(i18n("Translated by").as_str()));
        translators_title.add_css_class("heading");
        translators_title.set_halign(gtk::Align::Start);
        translators_title.set_margin_top(20);
        translators_title.set_margin_bottom(6);
        page.append(&translators_title);
        page.append(&people(credits(TRANSLATORS)));

        // Footer.
        let footer = gtk::Label::new(Some(i18n("© 2026 Hyprlab").as_str()));
        footer.add_css_class("dim-label");
        footer.add_css_class("caption");
        footer.set_wrap(true);
        footer.set_justify(gtk::Justification::Center);
        footer.set_margin_top(20);
        page.append(&footer);

        clamp.set_child(Some(&page));
        scroller.set_child(Some(&clamp));

        // The root page holds the identity + links; the sub-pages slide over it.
        let main_tv = adw::ToolbarView::new();
        let main_header = adw::HeaderBar::new();
        main_header.add_css_class("flat");
        main_header.set_show_title(false);
        main_tv.add_top_bar(&main_header);
        main_tv.set_content(Some(&scroller));
        nav.add(
            &adw::NavigationPage::builder()
                .title(format!("{} {}", i18n("About"), crate::APP_NAME).as_str())
                .tag("main")
                .child(&main_tv)
                .build(),
        );
        nav.add(&release_notes_page());

        win.set_content(Some(&nav));
        win.present();

        // Screenshot hook: HYLKI_SHOWCASE_SCROLL runs the page down by that
        // fraction once it has been laid out, so a capture can show the
        // thanks lists at the bottom (the same variable does this for
        // Settings).
        if let Some(frac) = std::env::var("HYLKI_SHOWCASE_SCROLL")
            .ok()
            .map(|v| v.parse::<f64>().unwrap_or(1.0))
        {
            let vadj = scroller.vadjustment();
            gtk::glib::timeout_add_seconds_local_once(2, move || {
                vadj.set_value(vadj.lower() + (vadj.upper() - vadj.page_size()) * frac);
            });
        }
    }

    /// Star/unstar a message, updating the server, the list, and the reader.
    fn set_star(&mut self, m: &Message, starred: bool) {
        let Some(path) = self.resolve_folder_path(m) else {
            return;
        };
        self.send_to(m.account_id, MailRequest::SetFlagged { path, uid: m.uid, flagged: starred });
        self.message_list
            .emit(MessageListInput::SetStarred { id: m.id, starred });
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
        } else if self.current.as_ref().is_some_and(|c| c.id == m.id && c.account_id == m.account_id)
        {
            let current = self.current.clone();
            self.show_message(current, false);
        }
        if let Some(p) = self.popouts.get(&(m.account_id, m.id)) {
            p.controller.emit(MessageWindowInput::SetStarred(starred));
        }
    }

    /// Mark a message read/unread, updating the server, list, badges, and reader.
    fn set_read(&mut self, m: &Message, read: bool) {
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
        self.pending_seen
            .insert((m.account_id, path.clone(), m.uid), (read, std::time::Instant::now()));
        self.send_to(m.account_id, MailRequest::SetSeen { path, uid: m.uid, seen: read });
        if read {
            // The mail was read (or marked read) in the app — the desktop
            // notification for this account's new mail is answered, clear it
            // instead of leaving it stranded in the panel (issue #41).
            crate::notify::withdraw_mail(m.account_id);
        }
        self.message_list
            .emit(MessageListInput::SetRead { id: m.id, read });
        self.set_cached_unread(m.account_id, m.id, !read);
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
    fn with_cached_body(&self, mut m: Message) -> Message {
        if m.body.is_empty() {
            if let Some(b) = self.body_cache.get(&(m.account_id, m.id)) {
                m.body = b.clone();
            }
        }
        m
    }

    /// Remove a handled message from the list, caches, and badges. Clears the
    /// reader only if that message was the one open. Shared by archive/delete/spam.
    /// Apply a removing bulk action (archive/delete/spam) to many messages at once.
    /// Messages are grouped by (account, source folder) and each group is moved in a
    /// SINGLE `MoveMessages` request (one server-side UID MOVE) — far faster and more
    /// reliable than one request per message, which on a huge mailbox (e.g. Gmail's
    /// All Mail) is slow and drops moves when the connection blips. The list is
    /// updated once (`RemoveMany`); the spinner clears when every group's worker
    /// reports `BulkComplete`.
    fn apply_bulk_move(&mut self, action: BulkAction, messages: Vec<Message>) {
        let kind = match action {
            BulkAction::Archive => FolderKind::Archive,
            BulkAction::Delete => FolderKind::Trash,
            BulkAction::Spam => FolderKind::Junk,
            BulkAction::NotSpam | BulkAction::MoveToInbox => FolderKind::Inbox,
            // Non-removing actions never reach here (handled inline).
            BulkAction::MarkRead
            | BulkAction::MarkUnread
            | BulkAction::Flag
            | BulkAction::Unflag => return,
        };
        // (account, source path) → (dest path, uids, the rows for undo).
        // dest is per-account.
        let mut groups: HashMap<(u32, String), (String, Vec<u32>, Vec<Message>)> =
            HashMap::new();
        let mut removed_ids = Vec::with_capacity(messages.len());
        let mut missing_dest = false;
        for m in &messages {
            let Some(src) = self.resolve_folder_path(m) else { continue };
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
            self.discard_message_local(m);
            removed_ids.push(m.id);
        }
        if missing_dest {
            self.notifications.emit(NotifyInput::Push {
                text: i18n_f("No {v1} folder available for some messages", &[("v1", &(kind_label(kind)).to_string())]),
                error: true,
                connectivity: false,
            });
        }
        self.bulk_pending += groups.len();
        if !groups.is_empty() {
            self.notifications.emit(NotifyInput::SetStatus(format!(
                "Moving {} message{} to {} on the server…",
                removed_ids.len(),
                if removed_ids.len() == 1 { "" } else { "s" },
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
                MailRequest::MarkHamMany { path: src, uids, dest }
            } else {
                MailRequest::MoveMessages { path: src, uids, dest }
            };
            self.send_to(account_id, req);
        }
        self.update_busy_indicator();
        self.message_list.emit(MessageListInput::RemoveMany(removed_ids));
        self.push_unread_counts();
    }

    /// Optimistic local cleanup when a message leaves the current folder: close its
    /// popout, drop it from the in-memory caches and the unread count, and clear the
    /// reader if it was the viewed message. Does NOT touch the list widget or push
    /// unread counts — the caller does that (single delete via `discard_message`;
    /// bulk via one `RemoveMany` + one push in `apply_bulk_move`).
    fn discard_message_local(&mut self, m: &Message) {
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
        // The Drafts chip counts every draft, so leaving that folder always
        // drops it by one; elsewhere only unread mail is counted.
        if m.unread || self.is_drafts_folder(m.account_id, m.folder_id) {
            if let Some(n) = self.folder_unread.get_mut(&(m.account_id, m.folder_id)) {
                *n = n.saturating_sub(1);
            }
        }
        if self.current.as_ref().is_some_and(|c| c.id == m.id && c.account_id == m.account_id) {
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

    fn discard_message(&mut self, m: &Message) {
        self.discard_message_local(m);
        self.message_list.emit(MessageListInput::Remove(m.id));
        self.push_unread_counts();
    }

    /// Whether a sender address matches the blacklist (exact address, or a bare
    /// domain entry matching the sender's domain or any subdomain of it).
    fn is_blacklisted(&self, addr: &str) -> bool {
        let a = addr.trim().to_lowercase();
        if a.is_empty() {
            return false;
        }
        let domain = a.rsplit('@').next().unwrap_or("");
        self.blacklist.iter().any(|entry| {
            if entry.contains('@') {
                a == *entry
            } else {
                domain == entry.as_str() || domain.ends_with(&format!(".{entry}"))
            }
        })
    }

    /// Move any blacklisted senders in an inbox sync to Trash, returning the rest.
    fn apply_blacklist(
        &self,
        account_id: u32,
        folder_id: u32,
        messages: Vec<Message>,
    ) -> Vec<Message> {
        if self.blacklist.is_empty()
            || self.inbox_of(account_id).map(|f| f.id) != Some(folder_id)
        {
            return messages;
        }
        let folders = self.folders.get(&account_id);
        let trash = folders
            .and_then(|fs| fs.iter().find(|f| f.kind == FolderKind::Trash))
            .map(|f| f.path.clone());
        let src = folders
            .and_then(|fs| fs.iter().find(|f| f.id == folder_id))
            .map(|f| f.path.clone());
        let mut kept = Vec::with_capacity(messages.len());
        for m in messages {
            if self.is_blacklisted(&m.from_addr) {
                if let (Some(trash), Some(src)) = (&trash, &src) {
                    self.send_to(account_id, MailRequest::MoveMessage {
                        path: src.clone(),
                        uid: m.uid,
                        dest: trash.clone(),
                    });
                }
            } else {
                kept.push(m);
            }
        }
        kept
    }

    /// Apply the mail filter rules (#47) to an inbox sync, Evolution-style:
    /// the first matching rule files the message into its folder; everything
    /// else passes through. On-sight like the blacklist, so mail that arrived
    /// while Hylki was closed still gets filed on the next sync.
    ///
    /// Returns the messages staying in the inbox, plus the ones filed away on
    /// this sync (paired with their destination path) so the caller can still
    /// count them as new mail when notifying.
    ///
    /// A manual run (#198) adds the folder it was pointed at to the pass, even
    /// when that is not the Inbox, and tallies what the rules did there so the
    /// run can report it.
    fn apply_filters(
        &mut self,
        account_id: u32,
        folder_id: u32,
        messages: Vec<Message>,
    ) -> (Vec<Message>, Vec<(Message, String)>) {
        let manual = self
            .filter_run
            .as_ref()
            .is_some_and(|r| r.folders.contains_key(&(account_id, folder_id)));
        let (kept, filed, pass) = self.filter_pass(account_id, folder_id, messages, manual);
        if manual {
            self.filter_run_folder_pass(account_id, folder_id, pass);
        }
        (kept, filed)
    }

    /// One pass of the rules over a folder's messages. Returns what stays,
    /// what was filed elsewhere, and (for a manual run) the pass's count:
    /// how many messages were looked at, how many a rule matched, and how
    /// many tags were put on and messages filed.
    fn filter_pass(
        &mut self,
        account_id: u32,
        folder_id: u32,
        messages: Vec<Message>,
        manual: bool,
    ) -> (Vec<Message>, Vec<(Message, String)>, FolderTally) {
        if self.filters.is_empty()
            || (!manual && self.inbox_of(account_id).map(|f| f.id) != Some(folder_id))
        {
            return (messages, Vec::new(), FolderTally::default());
        }
        let Some(email) = self.email_of(account_id) else {
            return (messages, Vec::new(), FolderTally::default());
        };
        let rules: Vec<&config::FilterRule> = self
            .filters
            .iter()
            .filter(|r| r.account_email.eq_ignore_ascii_case(&email))
            .collect();
        if rules.is_empty() {
            return (messages, Vec::new(), FolderTally::default());
        }
        let checked = messages.len();
        let mut matched = 0usize;
        let mut tagged = 0usize;
        // How many messages each rule matched, said in the log at the end of
        // a manual run: the one place a rule that matches nothing can be
        // told apart from one whose matches needed nothing done (#201).
        let mut per_rule = vec![0usize; rules.len()];
        let folders = self.folders.get(&account_id);
        let src = folders
            .and_then(|fs| fs.iter().find(|f| f.id == folder_id))
            .map(|f| f.path.clone());
        let known = |path: &str| folders.is_some_and(|fs| fs.iter().any(|f| f.path == path));
        let pending = self
            .filter_moved
            .remove(&(account_id, folder_id))
            .unwrap_or_default();
        let mut still_pending = std::collections::HashSet::new();
        let mut kept = Vec::with_capacity(messages.len());
        let mut filed = Vec::new();
        // When the account files copies of what it sends into this folder
        // (#199), the user's own mail sitting here is one of those copies, not
        // an arrival. Rules are written about incoming mail, so a rule on a
        // subject or a recipient would otherwise pick up your own reply and
        // move it away from the thread it was filed beside. The Sent folder is
        // the exception: everything in it is your own mail, so running rules
        // over it (#198) plainly means to act on exactly that.
        let diverted = self.sent_copy_path(account_id).as_deref() == src.as_deref()
            && self.folder_of_kind(account_id, FolderKind::Sent).map(|f| f.path.as_str())
                != src.as_deref();
        let own: Vec<String> = if diverted {
            self.own_identities(account_id)
        } else {
            Vec::new()
        };
        let hits = self.body_hits.get(&(account_id, folder_id));
        for mut m in messages {
            if own.iter().any(|a| a.eq_ignore_ascii_case(&m.from_addr)) {
                kept.push(m);
                continue;
            }
            let recipients = [m.to.as_str(), m.cc.as_str()]
                .into_iter()
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            let body_hits = hits.and_then(|h| h.get(&m.uid)).map(Vec::as_slice).unwrap_or(&[]);
            let input = config::FilterInput {
                from_addr: &m.from_addr,
                from_name: &m.from_name,
                subject: &m.subject,
                recipients: &recipients,
                reply_to: &m.reply_to,
                // The list rarely carries a body (the demo's does); the
                // preview is what a sync brings for every message.
                body: if m.body.is_empty() { &m.preview } else { &m.body },
                body_hits,
            };
            let matching: Vec<&&config::FilterRule> = rules
                .iter()
                .enumerate()
                .filter(|(_, r)| r.matches(&input))
                .map(|(i, r)| {
                    per_rule[i] += 1;
                    r
                })
                .collect();
            if !matching.is_empty() {
                matched += 1;
            }
            // Tags first (#71), from every matching rule, and only where the
            // message lacks the tag — this runs on every sync, and the flag
            // comes back down with the next one. Tagged before any move, so
            // the move carries the keyword along. A tag that no longer
            // exists tags nothing.
            if let Some(src) = &src {
                let wanted: Vec<(String, String)> = matching
                    .iter()
                    .filter(|r| !r.tag.is_empty() && !m.has_keyword(&r.tag))
                    .filter(|r| self.tags.iter().any(|t| t.keyword.eq_ignore_ascii_case(&r.tag)))
                    .map(|r| (r.tag.clone(), r.label()))
                    .collect();
                for (tag, rule) in wanted {
                    if m.has_keyword(&tag) {
                        continue; // two rules naming one tag
                    }
                    tagged += 1;
                    tracing::info!("filter: {} tagged {tag} [{rule}]", m.from_addr);
                    self.send_to(account_id, MailRequest::SetKeyword {
                        path: src.clone(),
                        uid: m.uid,
                        message_id: m.message_id.clone(),
                        keyword: tag.clone(),
                        add: true,
                    });
                    m.keywords.push(tag);
                }
            }
            let hit = matching.iter().copied().find(|r| {
                !r.dest_path.is_empty()
                    // A destination that vanished from the server keeps the
                    // mail in the inbox rather than erroring it into limbo.
                    && known(&r.dest_path)
                    && src.as_deref() != Some(r.dest_path.as_str())
            });
            match (hit, &src) {
                (Some(rule), Some(src)) => {
                    still_pending.insert(m.uid);
                    if pending.contains(&m.uid) {
                        // The move was already requested on an earlier sync;
                        // this one raced the server. Requesting again would
                        // error once the first move lands, and re-notifying
                        // would repeat the toast.
                        continue;
                    }
                    tracing::info!(
                        "filter: {} → {} [{}]",
                        m.from_addr,
                        rule.dest_path,
                        rule.label(),
                    );
                    self.send_to(account_id, MailRequest::MoveMessage {
                        path: src.clone(),
                        uid: m.uid,
                        dest: rule.dest_path.clone(),
                    });
                    filed.push((m, rule.dest_path.clone()));
                }
                _ => kept.push(m),
            }
        }
        // UIDs absent from this sync have completed their move server-side;
        // dropping them keeps the set from growing without bound.
        if !still_pending.is_empty() {
            self.filter_moved.insert((account_id, folder_id), still_pending);
        }
        if manual {
            let folder = src.as_deref().unwrap_or("?");
            for (rule, n) in rules.iter().zip(&per_rule) {
                tracing::info!("filter: [{}] matched {n} of {checked} in {folder}", rule.label());
            }
        }
        let pass = FolderTally { checked, matched, tagged, filed: filed.len() };
        (kept, filed, pass)
    }

    /// The (account, folder) pairs a manual filter run should cover (#198).
    /// An empty `targets` means every account's Inbox; anything else is
    /// taken as given, minus accounts that have no rules at all.
    fn filter_run_targets(&self, targets: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
        let has_rules = |account_id: u32| {
            self.email_of(account_id).is_some_and(|email| {
                self.filters.iter().any(|r| r.account_email.eq_ignore_ascii_case(&email))
            })
        };
        let targets = if targets.is_empty() {
            self.accounts
                .iter()
                .filter_map(|a| self.inbox_of(a.id).map(|f| (a.id, f.id)))
                .collect()
        } else {
            targets
        };
        targets.into_iter().filter(|(a, _)| has_rules(*a)).collect()
    }

    /// Start a manual filter run over `targets`: re-load each folder so the
    /// rules meet the mail already in it. The load's `Messages` event is what
    /// actually runs them, through [`apply_filters`]; the folder counts as
    /// done once its load has been to the server and back
    /// (`AppMsg::FolderSynced`), not when the cache answers first.
    fn start_filter_run(&mut self, targets: Vec<(u32, u32)>, sender: &ComponentSender<Self>) {
        let targets = self.filter_run_targets(targets);
        if targets.is_empty() {
            self.set_filters_applying(false);
            self.notifications.emit(NotifyInput::Push {
                text: i18n("No filters to apply here yet"),
                error: false,
                connectivity: false,
            });
            return;
        }
        tracing::info!(
            "filter: manual run over {} folder(s), {} rule(s)",
            targets.len(),
            self.filters.len(),
        );
        self.set_filters_applying(true);
        self.filter_run = Some(FilterRun {
            folders: targets.iter().map(|t| (*t, FolderTally::default())).collect(),
            pending: targets.iter().copied().collect(),
            reported: false,
            dialog: None,
        });
        let dialog = self.build_filter_run_dialog(sender);
        if let Some(run) = self.filter_run.as_mut() {
            run.dialog = Some(dialog);
        }
        self.update_filter_run_dialog();
        for (account_id, folder_id) in targets {
            let path = self
                .folders
                .get(&account_id)
                .and_then(|fs| fs.iter().find(|f| f.id == folder_id))
                .map(|f| f.path.clone());
            match path {
                Some(path) => {
                    self.send_to(account_id, MailRequest::LoadMessages { folder_id, path })
                }
                // Nothing to load it from: count the folder as done so the
                // run can still finish.
                None => self.filter_run_folder_synced(account_id, folder_id),
            }
        }
        // A folder that never answers (offline, or an error) must not leave
        // the run hanging: report whatever landed after a decent wait.
        let sender = sender.clone();
        gtk::glib::timeout_add_local_once(FILTER_RUN_TIMEOUT, move || {
            sender.input(AppMsg::FilterRunTimeout);
        });
    }

    /// The dialog a manual run puts up: a spinner, a live count, and one
    /// button that sends the run to the background while it is working and
    /// closes the report once it is not.
    fn build_filter_run_dialog(&self, sender: &ComponentSender<Self>) -> FilterRunDialog {
        // Parented on Settings when the run was started there, so the dialog
        // is not stranded behind it.
        let parent: Option<gtk::Window> = self
            .prefs
            .as_ref()
            .map(|p| p.widget().clone())
            .filter(|w| w.is_visible())
            .map(|w| w.upcast())
            .or_else(|| Some(self.window.clone().upcast()));
        let dialog = adw::MessageDialog::new(parent.as_ref(), Some(&i18n("Applying Filters")), None);
        dialog.set_body_use_markup(false);
        dialog.add_response(FILTER_RUN_RESPONSE, &i18n("Run in Background"));
        dialog.set_close_response(FILTER_RUN_RESPONSE);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_halign(gtk::Align::Center);
        let spinner = gtk::Spinner::new();
        spinner.set_size_request(32, 32);
        spinner.set_spinning(true);
        content.append(&spinner);
        let body = gtk::Label::new(None);
        body.set_justify(gtk::Justification::Center);
        body.set_wrap(true);
        body.add_css_class("dim-label");
        content.append(&body);
        dialog.set_extra_child(Some(&content));
        // Every way out of the dialog is the same one: while the run is going
        // it means "keep going without me", and afterwards it is just Close.
        // Which of the two happened is read off `reported`, not from here.
        {
            let sender = sender.clone();
            dialog.connect_response(None, move |_, _| {
                sender.input(AppMsg::FilterRunToBackground);
            });
        }
        dialog.present();
        FilterRunDialog { dialog, spinner, body }
    }

    /// Rewrite the progress dialog's live count, if it is still up and still
    /// counting. Once the run has reported, the dialog holds that report and
    /// a later pass (the scope outlives the report, so an ordinary sync of
    /// one of these folders still runs the rules) must not write over it.
    fn update_filter_run_dialog(&self) {
        let Some(run) = self.filter_run.as_ref() else { return };
        if run.reported {
            return;
        }
        let Some(d) = run.dialog.as_ref() else { return };
        let t = run.totals();
        let mut lines = vec![ni18n_f(
            "Looked at {n} message so far",
            "Looked at {n} messages so far",
            t.checked as u32,
            &[("n", &t.checked.to_string())],
        )];
        if t.matched > 0 {
            lines.push(ni18n_f(
                "Matched {n} message",
                "Matched {n} messages",
                t.matched as u32,
                &[("n", &t.matched.to_string())],
            ));
        }
        if t.tagged > 0 {
            lines.push(ni18n_f(
                "Tagged {n} message",
                "Tagged {n} messages",
                t.tagged as u32,
                &[("n", &t.tagged.to_string())],
            ));
        }
        if t.filed > 0 {
            lines.push(ni18n_f(
                "Filed {n} message away",
                "Filed {n} messages away",
                t.filed as u32,
                &[("n", &t.filed.to_string())],
            ));
        }
        // Only worth saying which folder the run is on when there is more
        // than one of them.
        if run.folders.len() > 1 {
            lines.push(i18n_f(
                "Folder {done} of {total}",
                &[
                    ("done", &run.done_count().to_string()),
                    ("total", &run.folders.len().to_string()),
                ],
            ));
        }
        d.body.set_label(&lines.join("\n"));
    }

    /// Fold one pass's work into the run's tally for that folder.
    fn filter_run_folder_pass(&mut self, account_id: u32, folder_id: u32, pass: FolderTally) {
        if let Some(run) = self.filter_run.as_mut() {
            let tally = run.folders.entry((account_id, folder_id)).or_default();
            tally.checked = tally.checked.max(pass.checked);
            tally.matched = tally.matched.max(pass.matched);
            tally.tagged += pass.tagged;
            tally.filed += pass.filed;
        }
        self.update_filter_run_dialog();
    }

    /// One folder of a manual run has been to the server and back.
    fn filter_run_folder_synced(&mut self, account_id: u32, folder_id: u32) {
        let Some(run) = self.filter_run.as_mut() else { return };
        if !run.pending.remove(&(account_id, folder_id)) {
            return;
        }
        let done = run.pending.is_empty();
        self.update_filter_run_dialog();
        if done {
            self.report_filter_run(false);
        }
    }

    /// The progress dialog was dismissed. While the run is still going that
    /// means "carry on without me", and the report will go to the status bar;
    /// once it has reported, the dialog was only holding the report, so this
    /// is an ordinary close.
    fn filter_run_dismissed(&mut self) {
        let Some(run) = self.filter_run.as_mut() else { return };
        run.dialog = None;
        if run.reported {
            self.filter_run = None;
        }
    }

    /// Say what a manual filter run did: in the dialog when it is still up
    /// (the user waited), in the status bar when it is not (they sent the run
    /// to the background). `end` (the timeout) also drops the run, so the
    /// folders stop counting as manually swept; without it the scope stands,
    /// and the server's answer to the same load is filtered too. Reports once
    /// either way.
    fn report_filter_run(&mut self, end: bool) {
        let Some(run) = self.filter_run.as_mut() else { return };
        if std::mem::replace(&mut run.reported, true) {
            if end {
                self.filter_run = None;
            }
            return;
        }
        let t = run.totals();
        let unanswered = run.pending.len();
        tracing::info!(
            "filter: manual run done, {} checked, {} matched, {} tagged, {} filed, \
             {unanswered} folder(s) unanswered",
            t.checked,
            t.matched,
            t.tagged,
            t.filed,
        );
        self.set_filters_applying(false);
        self.notifications.emit(NotifyInput::SetStatus(String::new()));
        // Always says how many messages the rules were held up against: with
        // nothing matching, that is the whole answer to "are my rules working"
        // (#198). The plural follows that count throughout.
        let n = t.checked.to_string();
        let matched_s = t.matched.to_string();
        let tagged_s = t.tagged.to_string();
        let filed_s = t.filed.to_string();
        let text = match (t.matched, t.tagged, t.filed) {
            (0, _, _) => ni18n_f(
                "Filters looked at {n} message, and it matched nothing",
                "Filters looked at {n} messages, and none of them matched",
                t.checked as u32,
                &[("n", &n)],
            ),
            // Matches that needed nothing: the rules had already done their
            // work on an earlier sync (#201). Saying "none matched" here
            // made working rules look broken.
            (_, 0, 0) => ni18n_f(
                "Filters matched {matched} of {n} message, and it was already tagged or filed",
                "Filters matched {matched} of {n} messages, and all of them were already tagged or filed",
                t.checked as u32,
                &[("matched", &matched_s), ("n", &n)],
            ),
            (_, _, 0) => ni18n_f(
                "Filters tagged {tagged} of {n} message",
                "Filters tagged {tagged} of {n} messages",
                t.checked as u32,
                &[("tagged", &tagged_s), ("n", &n)],
            ),
            (_, 0, _) => ni18n_f(
                "Filters filed {filed} of {n} message away",
                "Filters filed {filed} of {n} messages away",
                t.checked as u32,
                &[("filed", &filed_s), ("n", &n)],
            ),
            _ => ni18n_f(
                "Filters tagged {tagged} and filed {filed} of {n} message",
                "Filters tagged {tagged} and filed {filed} of {n} messages",
                t.checked as u32,
                &[("tagged", &tagged_s), ("filed", &filed_s), ("n", &n)],
            ),
        };
        // Waited on the dialog: the report belongs there, and nowhere else.
        // Sent to the background: the status bar is the only place left. A
        // dialog that went down with its parent (Settings closed mid-run)
        // counts as the second case, or the report would go nowhere.
        match self
            .filter_run
            .as_ref()
            .and_then(|r| r.dialog.as_ref())
            .filter(|d| d.dialog.is_visible())
        {
            Some(d) => {
                tracing::info!("filter: manual run reported in its dialog");
                d.spinner.set_spinning(false);
                d.spinner.set_visible(false);
                d.body.remove_css_class("dim-label");
                d.body.set_label(&text);
                d.dialog.set_heading(Some(&i18n("Filters Applied")));
                d.dialog.set_response_label(FILTER_RUN_RESPONSE, &i18n("Close"));
                d.dialog.set_response_appearance(
                    FILTER_RUN_RESPONSE,
                    adw::ResponseAppearance::Suggested,
                );
            }
            None => {
                tracing::info!("filter: manual run reported in the status bar");
                self.notifications.emit(NotifyInput::Push {
                    text,
                    error: false,
                    connectivity: false,
                });
            }
        }
        if end {
            self.filter_run = None;
        }
    }

    /// Tell the Settings window whether a manual filter run is under way, so
    /// its Apply Now button can spin.
    fn set_filters_applying(&self, on: bool) {
        if let Some(a) = &self.accounts_win {
            a.emit(crate::ui::accounts::AccountsInput::FiltersApplying(on));
        }
    }

    /// Re-sync every inbox so a newly-blacklisted sender's existing mail is
    /// caught and deleted by [`apply_blacklist`].
    fn sweep_blacklisted(&self) {
        let reqs: Vec<(u32, u32, String)> = self
            .accounts
            .iter()
            .filter_map(|a| self.inbox_of(a.id).map(|f| (a.id, f.id, f.path.clone())))
            .collect();
        for (account_id, folder_id, path) in reqs {
            self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
        }
    }

    /// The IMAP folder path a message lives in (its account's folder by id).
    /// A reply to an encrypted message starts with Encrypt on (#133): the
    /// verdict the reader had for the original says whether it was.
    fn reply_pgp(&self, m: &Message, mut prefill: ComposePrefill) -> ComposePrefill {
        prefill.encrypt = self
            .sender_cache
            .get(&(m.account_id, m.id))
            .and_then(|c| c.pgp.as_ref())
            .is_some_and(|p| p.encrypted);
        prefill
    }

    fn resolve_folder_path(&self, m: &Message) -> Option<String> {
        self.folders
            .get(&m.account_id)?
            .iter()
            .find(|f| f.id == m.folder_id)
            .map(|f| f.path.clone())
    }

    /// An account's Inbox folder, if known.
    fn inbox_of(&self, account_id: u32) -> Option<&Folder> {
        self.folder_of_kind(account_id, FolderKind::Inbox)
    }

    /// Where a copy of outgoing mail is filed (#199): the folder chosen in the
    /// account editor if it still exists, else the Sent folder. `None` when
    /// the account has neither — the message is sent without a copy.
    /// The lists left, as the readers take them: key → when.
    fn unsubscribed_map(&self) -> HashMap<String, i64> {
        self.unsubscribed.iter().map(|l| (l.key.clone(), l.at)).collect()
    }

    /// Tell every reader (the main one and the pop-outs) where a card's
    /// unsubscribe request stands.
    fn push_unsubscribe_state(&self, key: (u32, u32), state: Option<UnsubState>) {
        self.message_view.emit(MessageViewInput::UnsubscribeState {
            account_id: key.0,
            id: key.1,
            state: state.clone(),
        });
        for p in self.popouts.values() {
            p.controller.emit(MessageWindowInput::UnsubscribeState {
                account_id: key.0,
                id: key.1,
                state: state.clone(),
            });
        }
    }

    /// Remember that this message's list was left, and tell the readers.
    fn record_unsubscribed(&mut self, message: &Message) {
        let Some(info) = self
            .sender_cache
            .get(&(message.account_id, message.id))
            .and_then(|c| c.unsubscribe.as_ref())
        else {
            return;
        };
        let key = info.key(&message.from_addr);
        self.unsubscribed.retain(|l| l.key != key);
        self.unsubscribed.push(config::UnsubscribedList {
            key,
            name: sender_label(message),
            at: crate::datefmt::now(),
        });
        config::save_unsubscribed(&self.unsubscribed);
        let map = self.unsubscribed_map();
        self.message_view.emit(MessageViewInput::SetUnsubscribed(map.clone()));
        for p in self.popouts.values() {
            p.controller.emit(MessageWindowInput::SetUnsubscribed(map.clone()));
        }
    }

    /// The addresses each account answers to: its own and every send-as
    /// alias, lowercased. What an invitation's attendee list is matched
    /// against, so the card can say where the reader's own answer stands
    /// (#223).
    fn identities_map(&self) -> HashMap<u32, Vec<String>> {
        let mut out: HashMap<u32, Vec<String>> = HashMap::new();
        for (i, cfg) in self.effective_config().iter().enumerate() {
            let mut addresses = vec![cfg.email.trim().to_lowercase()];
            for alias in &cfg.aliases {
                if let Some((_, addr)) =
                    crate::worker::parse_recipients(&alias.identity).into_iter().next()
                {
                    addresses.push(addr.to_lowercase());
                }
            }
            addresses.retain(|a| !a.is_empty());
            out.insert(i as u32 + 1, addresses);
        }
        out
    }

    /// The invitations answered so far, as the readers take them: event key
    /// → the PARTSTAT sent and when.
    fn invite_answers_map(&self) -> HashMap<String, (String, i64)> {
        self.invite_answers
            .iter()
            .map(|a| (a.key.clone(), (a.status.clone(), a.at)))
            .collect()
    }

    /// One of a card's invitation buttons (#223).
    fn invite_action(
        &mut self,
        message: Message,
        invite: crate::models::Invite,
        action: crate::ui::message_view::InviteAction,
    ) {
        use crate::ui::message_view::InviteAction;
        match action {
            InviteAction::AddToCalendar => self.open_invite_in_calendar(&invite),
            InviteAction::Answer(rsvp) => self.answer_invite(&message, &invite, rsvp),
        }
    }

    /// "Add to Calendar": hand the event, as it arrived, to whatever
    /// application opens calendar files — the same road an attachment takes
    /// (`.ics` and all its portal handling), because that is exactly what
    /// this is.
    fn open_invite_in_calendar(&self, invite: &crate::models::Invite) {
        if invite.ics.is_empty() {
            self.notifications.emit(NotifyInput::Push {
                text: i18n("This message carries no calendar file to open."),
                error: true,
                connectivity: false,
            });
            return;
        }
        // Named for the meeting so the calendar's import dialog says what it
        // is about, rather than "invite.ics".
        let stem: String = invite
            .summary
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
            .collect();
        let name = match stem.trim() {
            "" => "invite.ics".to_string(),
            s => format!("{}.ics", &s[..s.len().min(60)]),
        };
        crate::ui::attachments_gallery::open_bytes(
            &name,
            invite.ics.as_bytes(),
            Some(self.window.upcast_ref::<gtk::Window>()),
        );
    }

    /// Accept, Maybe or Decline: the answer RFC 5546 asks for, mailed to the
    /// organizer from the address that was invited.
    ///
    /// No copy is filed in Sent — an RSVP is machinery, not correspondence,
    /// and the same is true of the unsubscribe requests the reader sends. A
    /// send that fails is reported and queued in the Outbox like any other.
    fn answer_invite(
        &mut self,
        message: &Message,
        invite: &crate::models::Invite,
        rsvp: crate::models::Rsvp,
    ) {
        let Some(organizer) = invite.organizer.as_ref().map(|o| o.email.clone()) else {
            self.notifications.emit(NotifyInput::Push {
                text: i18n("This invitation names no organiser to answer."),
                error: true,
                connectivity: false,
            });
            return;
        };
        let (from_alias, name, address) = self.invite_identity(message, invite);
        let ics = crate::invite::reply_ics(invite, &name, &address, rsvp.partstat());
        let subject = match rsvp {
            crate::models::Rsvp::Accepted => {
                i18n_f("Accepted: {subject}", &[("subject", &invite.summary)])
            }
            crate::models::Rsvp::Tentative => {
                i18n_f("Tentative: {subject}", &[("subject", &invite.summary)])
            }
            crate::models::Rsvp::Declined => {
                i18n_f("Declined: {subject}", &[("subject", &invite.summary)])
            }
        };
        let who = if name.trim().is_empty() { address.clone() } else { name.clone() };
        let body = match rsvp {
            crate::models::Rsvp::Accepted => {
                i18n_f("{name} has accepted this invitation.", &[("name", &who)])
            }
            crate::models::Rsvp::Tentative => {
                i18n_f("{name} has tentatively accepted this invitation.", &[("name", &who)])
            }
            crate::models::Rsvp::Declined => {
                i18n_f("{name} has declined this invitation.", &[("name", &who)])
            }
        };
        let out = crate::worker::OutgoingMessage {
            from_account_id: message.account_id,
            from_alias,
            to: organizer,
            cc: String::new(),
            bcc: String::new(),
            reply_to: String::new(),
            subject,
            body,
            html: String::new(),
            attachments: Vec::new(),
            // An answer belongs to the conversation the invitation started,
            // so the organizer's client files it with the request.
            in_reply_to: message.message_id.clone(),
            references: String::new(),
            draft_origin: None,
            outbox_origin: None,
            sign: false,
            encrypt: false,
            send_at: None,
            calendar: Some(crate::worker::CalendarPart {
                ics,
                method: "REPLY".to_string(),
            }),
        };
        self.send_to(
            message.account_id,
            MailRequest::Send { message: Box::new(out), sent_path: None },
        );
        self.record_invite_answer(invite, rsvp);
        self.notifications.emit(NotifyInput::Push {
            text: match rsvp {
                crate::models::Rsvp::Accepted => i18n("Your answer has been sent: accepted."),
                crate::models::Rsvp::Tentative => i18n("Your answer has been sent: maybe."),
                crate::models::Rsvp::Declined => i18n("Your answer has been sent: declined."),
            },
            error: false,
            connectivity: false,
        });
    }

    /// The identity an answer leaves under: the address on the invitation's
    /// attendee list that belongs to this account (a calendar knows its
    /// guest by the address it invited), else the alias the message was
    /// addressed to, else the account itself. Returns the send-as alias for
    /// the wire (`None` for the account), the name to put on the attendee
    /// line, and the bare address.
    fn invite_identity(
        &self,
        message: &Message,
        invite: &crate::models::Invite,
    ) -> (Option<String>, String, String) {
        let cfg = self.effective_config();
        let Some(cfg) = cfg.get(message.account_id.saturating_sub(1) as usize) else {
            return (None, String::new(), String::new());
        };
        let invited: Vec<String> =
            invite.attendees.iter().map(|a| a.email.to_lowercase()).collect();
        for alias in &cfg.aliases {
            if let Some((name, addr)) =
                crate::worker::parse_recipients(&alias.identity).into_iter().next()
            {
                if invited.iter().any(|i| *i == addr.to_lowercase()) {
                    return (Some(alias.identity.clone()), name, addr);
                }
            }
        }
        if invited.iter().any(|i| *i == cfg.email.to_lowercase()) {
            return (None, cfg.name.clone(), cfg.email.clone());
        }
        // Not on the list under any address we know: answer as whoever the
        // message was addressed to, which is the unsubscribe rule and the
        // best guess there is.
        match self.unsubscribe_from(message) {
            (Some(alias), addr) => {
                let name = crate::worker::parse_recipients(&alias)
                    .into_iter()
                    .next()
                    .map(|(n, _)| n)
                    .unwrap_or_default();
                (Some(alias), name, addr)
            }
            (None, addr) => (None, cfg.name.clone(), addr),
        }
    }

    /// Remember an answer, and tell the readers so the banner says so.
    fn record_invite_answer(
        &mut self,
        invite: &crate::models::Invite,
        rsvp: crate::models::Rsvp,
    ) {
        let key = invite.key();
        if key.is_empty() {
            return;
        }
        self.invite_answers.retain(|a| a.key != key);
        self.invite_answers.push(config::InviteAnswer {
            key,
            status: rsvp.partstat().to_string(),
            at: crate::datefmt::now(),
            summary: invite.summary.clone(),
        });
        config::save_invite_answers(&self.invite_answers);
        let map = self.invite_answers_map();
        self.message_view.emit(MessageViewInput::SetInviteAnswers(map.clone()));
        for p in self.popouts.values() {
            p.controller.emit(MessageWindowInput::SetInviteAnswers(map.clone()));
        }
    }

    /// The identity an unsubscribe mail leaves under: the alias the message
    /// was addressed to when it was one (a list knows its subscriber by the
    /// address it writes to), else the account itself. Returns the send-as
    /// alias for the wire (`None` for the account) and the bare address.
    fn unsubscribe_from(&self, message: &Message) -> (Option<String>, String) {
        let cfg = self.effective_config();
        let Some(cfg) = cfg.get(message.account_id.saturating_sub(1) as usize) else {
            return (None, String::new());
        };
        let recipients: Vec<String> = [&message.to, &message.cc]
            .into_iter()
            .flat_map(|f| crate::worker::parse_recipients(f))
            .map(|(_, e)| e.to_lowercase())
            .collect();
        for alias in &cfg.aliases {
            if let Some((_, addr)) = crate::worker::parse_recipients(&alias.identity).into_iter().next() {
                if recipients.iter().any(|r| *r == addr.to_lowercase()) {
                    return (Some(alias.identity.clone()), addr);
                }
            }
        }
        (None, cfg.email.clone())
    }

    /// Where an unsubscribe mail goes: the list's `mailto:` handle when it
    /// gave one, else the "reply with UNSUBSCRIBE" it asked for. `None`
    /// when the message offers neither.
    fn unsubscribe_mail_target(
        info: &crate::models::Unsubscribe,
    ) -> Option<crate::unsubscribe::MailtoTarget> {
        if let Some(t) = info.mailto.as_deref().and_then(crate::unsubscribe::parse_mailto) {
            return Some(t);
        }
        info.reply.as_ref().map(|r| crate::unsubscribe::MailtoTarget {
            to: r.to.clone(),
            subject: r.subject.clone(),
            body: r.subject.clone(),
        })
    }

    /// Ask before leaving a list: what will happen depends on the route the
    /// list offers, and a mail sent in the user's name is said so up front.
    fn confirm_unsubscribe(
        &self,
        message: Message,
        info: crate::models::Unsubscribe,
        sender: &ComponentSender<Self>,
    ) {
        let heading = i18n_f("Unsubscribe from {name}?", &[("name", &sender_label(&message))]);
        let mut body = if info.one_click.is_some() {
            i18n("Hylki will ask the list to stop sending you mail. A list can take a few days to act on the request.")
        } else if let Some(t) = Self::unsubscribe_mail_target(&info) {
            let (_, from) = self.unsubscribe_from(&message);
            i18n_f(
                "An unsubscribe request will be sent to {addr} from {from}. A list can take a few days to act on it.",
                &[("addr", &t.to), ("from", &from)],
            )
        } else {
            i18n("This list offers no direct way to unsubscribe. Its unsubscribe page will open in your browser.")
        };
        // Read out of the message's words rather than its headers: a good
        // guess, but the user should know it is one before anything is sent.
        if !info.in_headers {
            body.push(' ');
            body.push_str(&i18n("This message carries no unsubscribe header, so Hylki is going by the link in it."));
        }
        let parent = relm4::main_application()
            .active_window()
            .unwrap_or_else(|| self.window.clone().upcast());
        let dialog = adw::MessageDialog::new(Some(&parent), Some(&heading), Some(body.as_str()));
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("unsub", &i18n("Unsubscribe"));
        dialog.set_default_response(Some("unsub"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("unsub", adw::ResponseAppearance::Suggested);
        let s = sender.clone();
        let message = Box::new(message);
        let info = Box::new(info);
        dialog.connect_response(None, move |_, resp| {
            if resp == "unsub" {
                s.input(AppMsg::UnsubscribeGo { message: message.clone(), info: info.clone() });
            }
        });
        dialog.present();
        // HYLKI_SHOWCASE_UNSUB=ask: the dialog is its own surface, so the
        // main window's capture never shows it — take one of it too.
        if std::env::var("HYLKI_SHOWCASE_UNSUB").is_ok_and(|v| v == "ask") {
            if let Ok(path) = std::env::var("HYLKI_SHOWCASE") {
                let d = dialog.clone();
                gtk::glib::timeout_add_seconds_local_once(1, move || {
                    showcase_capture(d.upcast_ref::<gtk::Widget>(), &format!("{path}.dialog.png"));
                });
            }
        }
    }

    /// Leave the list, by the best route it offers: the one-click POST
    /// (RFC 8058), falling back to its `mailto:` handle when that fails or
    /// is not there, and only then the web page in the browser.
    fn unsubscribe_go(
        &mut self,
        message: Message,
        info: crate::models::Unsubscribe,
        sender: &ComponentSender<Self>,
    ) {
        let key = (message.account_id, message.id);
        self.push_unsubscribe_state(key, Some(UnsubState::Working));
        let message = Box::new(message);
        if demo_mode() {
            // The demo has no list to ask: the request succeeds after a beat
            // (or fails, for a look at that banner: HYLKI_SHOWCASE_UNSUB=fail).
            let s = sender.clone();
            let fail = std::env::var("HYLKI_SHOWCASE_UNSUB").is_ok_and(|v| v == "fail");
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(900), move || {
                let result = if fail {
                    Err("the list's server answered 503".to_string())
                } else {
                    Ok(true)
                };
                s.input(AppMsg::UnsubscribeDone { message, result });
            });
            return;
        }
        if let Some(url) = info.one_click.clone() {
            let s = sender.clone();
            let info = Box::new(info);
            std::thread::spawn(move || match crate::unsubscribe::one_click_post(&url) {
                Ok(()) => s.input(AppMsg::UnsubscribeDone { message, result: Ok(true) }),
                Err(why) if info.mailto.is_some() || info.reply.is_some() => {
                    tracing::warn!("one-click unsubscribe failed ({why}); writing to the list instead");
                    s.input(AppMsg::UnsubscribeByMail { message, info });
                }
                Err(why) => s.input(AppMsg::UnsubscribeDone { message, result: Err(why) }),
            });
            return;
        }
        if info.mailto.is_some() || info.reply.is_some() {
            self.unsubscribe_by_mail(*message, &info, sender);
            return;
        }
        match info.web.as_deref() {
            Some(url) => {
                crate::ui::launch::open_link(url, Some(self.window.upcast_ref()));
                sender.input(AppMsg::UnsubscribeDone { message, result: Ok(false) });
            }
            None => sender.input(AppMsg::UnsubscribeDone {
                message,
                result: Err(i18n("the message offers no way to unsubscribe")),
            }),
        }
    }

    /// The `mailto:` route: a short message to the list's handle, sent
    /// through the account the message arrived in (as the alias it was
    /// addressed to, when it was one). No copy is filed in Sent. A send that
    /// fails is reported and queued in the Outbox like any other.
    fn unsubscribe_by_mail(
        &mut self,
        message: Message,
        info: &crate::models::Unsubscribe,
        sender: &ComponentSender<Self>,
    ) {
        let Some(target) = Self::unsubscribe_mail_target(info) else {
            sender.input(AppMsg::UnsubscribeDone {
                message: Box::new(message),
                result: Err(i18n("the list's mail address could not be read")),
            });
            return;
        };
        let (from_alias, _) = self.unsubscribe_from(&message);
        let out = crate::worker::OutgoingMessage {
            from_account_id: message.account_id,
            from_alias,
            to: target.to,
            cc: String::new(),
            bcc: String::new(),
            reply_to: String::new(),
            subject: target.subject,
            body: target.body,
            html: String::new(),
            attachments: Vec::new(),
            // A "reply with UNSUBSCRIBE" is a reply: the list matches it to
            // the message it sent. A `mailto:` handle is a fresh message.
            in_reply_to: if info.mailto.is_some() { String::new() } else { message.message_id.clone() },
            references: String::new(),
            draft_origin: None,
            outbox_origin: None,
            sign: false,
            encrypt: false,
            calendar: None,
            send_at: None,
        };
        self.send_to(message.account_id, MailRequest::Send { message: Box::new(out), sent_path: None });
        sender.input(AppMsg::UnsubscribeDone { message: Box::new(message), result: Ok(true) });
    }

    fn sent_copy_path(&self, account_id: u32) -> Option<String> {
        let cfg = self.effective_config().get(account_id.saturating_sub(1) as usize);
        // The server files its own copy: appending a second one is what makes
        // a Gmail account show every sent message twice.
        if cfg.is_some_and(|c| c.server_saves_sent) {
            return None;
        }
        let chosen = cfg.and_then(|c| c.sent_copy_path.as_deref());
        pick_sent_copy(chosen, self.folders.get(&account_id).map_or(&[], Vec::as_slice))
    }

    /// An account's folder of a kind, if it has one (the first, for the
    /// special kinds a server lists once).
    fn folder_of_kind(&self, account_id: u32, kind: FolderKind) -> Option<&Folder> {
        self.folders.get(&account_id)?.iter().find(|f| f.kind == kind)
    }

    /// The folders the open unified view merges: (account, folder id, path).
    fn unified_targets(&self) -> Vec<(u32, u32, String)> {
        match self.unified_view {
            UnifiedView::Kind(kind) => self
                .accounts
                .iter()
                .filter_map(|a| self.folder_of_kind(a.id, kind).map(|f| (a.id, f.id, f.path.clone())))
                .collect(),
            UnifiedView::Filtered => self
                .unified_folder_refs()
                .into_iter()
                .map(|r| (r.account_id, r.folder.id, r.folder.path))
                .collect(),
        }
    }

    /// Whether a folder is one the open unified view merges.
    fn is_unified_target(&self, account_id: u32, folder_id: u32) -> bool {
        match self.unified_view {
            UnifiedView::Kind(kind) => {
                self.folder_of_kind(account_id, kind).is_some_and(|f| f.id == folder_id)
            }
            UnifiedView::Filtered => self
                .unified_folder_refs()
                .iter()
                .any(|r| r.account_id == account_id && r.folder.id == folder_id),
        }
    }

    /// The folders an account's own "Filtered Folders" section lists: the
    /// destination of every one of its rules, in rule order, without
    /// repeats — whether or not the rule opted into the unified section.
    /// An inbox destination is the account's Inbox row already.
    fn account_filtered_folders(&self, account_id: u32) -> Vec<Folder> {
        let mut out: Vec<Folder> = Vec::new();
        let Some(folders) = self.folders.get(&account_id) else { return out };
        let Some(email) = self.email_of(account_id) else { return out };
        let rules = self.filters.iter().filter(|r| r.account_email.eq_ignore_ascii_case(&email));
        for r in rules {
            let Some(f) = folders.iter().find(|f| f.path == r.dest_path) else { continue };
            if f.kind == FolderKind::Inbox || out.iter().any(|o| o.id == f.id) {
                continue;
            }
            let mut folder = f.clone();
            folder.unread = self.folder_unread_of(f);
            out.push(folder);
        }
        out
    }

    /// Server-side unread count for an account's inbox.
    /// The folders whose unread mail makes up an account's unread total: its
    /// inbox, then the destination of every filter rule marked to count
    /// (#116), in rule order and without repeats. Trash and Junk never
    /// count, whatever a rule says: mail filed there is being discarded.
    fn counted_folders(&self, account_id: u32) -> Vec<&Folder> {
        let mut out: Vec<&Folder> = Vec::new();
        if let Some(inbox) = self.inbox_of(account_id) {
            out.push(inbox);
        }
        let Some(folders) = self.folders.get(&account_id) else { return out };
        let Some(email) = self.email_of(account_id) else { return out };
        let rules = self
            .filters
            .iter()
            .filter(|r| r.count_unread && r.account_email.eq_ignore_ascii_case(&email));
        for r in rules {
            let Some(f) = folders.iter().find(|f| f.path == r.dest_path) else { continue };
            if matches!(f.kind, FolderKind::Trash | FolderKind::Junk)
                || out.iter().any(|o| o.id == f.id)
            {
                continue;
            }
            out.push(f);
        }
        out
    }

    /// The folders the unified "Filtered Folders" section lists: the
    /// destination of every rule, in account order then rule order, without
    /// repeats; nothing when Settings → Sidebar has the section off. An
    /// inbox destination is already an All Inboxes row and is skipped.
    fn unified_folder_refs(&self) -> Vec<UnifiedFolderRef> {
        let mut out: Vec<UnifiedFolderRef> = Vec::new();
        if !self.unified_filtered {
            return out;
        }
        for email in self.ordered_emails() {
            let Some(account) = self.accounts.iter().find(|a| a.email == email) else { continue };
            let Some(folders) = self.folders.get(&account.id) else { continue };
            let rules = self.filters.iter().filter(|r| r.account_email.eq_ignore_ascii_case(&email));
            for r in rules {
                let Some(f) = folders.iter().find(|f| f.path == r.dest_path) else { continue };
                if f.kind == FolderKind::Inbox
                    || out.iter().any(|o| o.account_id == account.id && o.folder.id == f.id)
                {
                    continue;
                }
                let mut folder = f.clone();
                folder.unread = self.folder_unread_of(f);
                out.push(UnifiedFolderRef { account_id: account.id, folder });
            }
        }
        out
    }

    /// The identity of [`unified_folder_refs`](Self::unified_folder_refs),
    /// for telling whether a rule change moved anything in or out.
    fn unified_folder_keys(&self) -> Vec<(u32, u32)> {
        self.unified_folder_refs()
            .into_iter()
            .map(|r| (r.account_id, r.folder.id))
            .collect()
    }

    fn folder_unread_of(&self, f: &Folder) -> u32 {
        self.folder_unread.get(&(f.account_id, f.id)).copied().unwrap_or(0)
    }

    /// An account's unread total over its [`counted_folders`](Self::counted_folders).
    fn counted_unread(&self, account_id: u32) -> u32 {
        self.counted_folders(account_id).iter().map(|f| self.folder_unread_of(f)).sum()
    }

    /// The number behind the tray icon's dot and menu and the Background
    /// Apps status: every account's counted unread mail.
    fn unified_unread(&self) -> u32 {
        self.accounts.iter().map(|a| self.counted_unread(a.id)).sum()
    }

    /// The number behind the unified Inboxes chip: the inboxes alone. A
    /// filter's destination has its own row (under Filters, and in its
    /// account), with its own chip, so it is not counted here as well.
    fn inboxes_unread(&self) -> u32 {
        self.accounts
            .iter()
            .filter_map(|a| self.inbox_of(a.id))
            .map(|f| self.folder_unread_of(f))
            .sum()
    }

    /// A watcher or sweep reported a changed unread count for `folder_id`.
    /// When that is one of the account's counted folders (its inbox, or a
    /// folder a counting filter files into) and the worker's IDLE sits
    /// elsewhere, the folder's message list used to refresh only when it
    /// was next opened: the tray menu said "No unread mail" under a count
    /// that said otherwise, and mail arriving in the inbox raised no
    /// notification, since both read the list. Ask for a quiet resync so
    /// the list follows the count. An open inbox (alone or as All Inboxes)
    /// or an open folder is the IDLE folder, whose own resync covers it.
    fn sync_background_folder(&self, account_id: u32, folder_id: u32) {
        let Some(folder) =
            self.counted_folders(account_id).into_iter().find(|f| f.id == folder_id)
        else {
            return;
        };
        // A folder that is open — on its own or as part of a unified view —
        // is not skipped (#170). It used to be, on the assumption that the
        // worker's IDLE on it would deliver the new list; but only push
        // accounts IDLE, and a polled account's Inboxes slice then waited a
        // whole poll interval for mail its unread chip already counted.
        // When IDLE did deliver it, this re-fetch of the first page comes
        // back identical and changes nothing (the Messages handler skips an
        // unchanged list).
        let path = folder.path.clone();
        self.send_to(account_id, MailRequest::SyncFolder { folder_id, path });
    }

    /// Counted folders (#116) whose lists have never been loaded this run:
    /// Index an account's sent mail without waiting for its folder to be
    /// opened (#236). A conversation pulls the user's own replies in from
    /// the cache, and the cache only held a folder once that folder had been
    /// shown: a reply sent from another client stayed out of every
    /// conversation until Sent was visited by hand. Asked once per account
    /// per run, as soon as its folders are known; the worker lists the first
    /// page and indexes the rest behind it. Both the folder the server files
    /// sent mail in and the one Hylki copies to, when they differ.
    fn index_sent_folders(&mut self, account_id: u32) {
        if self.sent_index_requested.contains(&account_id) {
            return;
        }
        let mut targets: Vec<(u32, String)> = self
            .folder_of_kind(account_id, FolderKind::Sent)
            .map(|f| (f.id, f.path.clone()))
            .into_iter()
            .collect();
        if let Some(path) = self.sent_copy_path(account_id) {
            let copy = self
                .folders
                .get(&account_id)
                .and_then(|fs| fs.iter().find(|f| f.path == path))
                .map(|f| (f.id, f.path.clone()));
            if let Some(copy) = copy {
                if !targets.iter().any(|(id, _)| *id == copy.0) {
                    targets.push(copy);
                }
            }
        }
        // No sent folder listed yet: ask again when the next folder list
        // arrives rather than never.
        if targets.is_empty() {
            return;
        }
        self.sent_index_requested.insert(account_id);
        for (folder_id, path) in targets {
            self.send_to(account_id, MailRequest::SyncFolder { folder_id, path });
        }
    }

    /// fetch them quietly, so the tray menu can show their unread mail
    /// without waiting for their counts to move.
    fn sync_unloaded_counted_folders(&self) {
        for a in &self.accounts {
            for f in self.counted_folders(a.id) {
                if !self.message_cache.contains_key(&(a.id, f.id)) {
                    self.send_to(a.id, MailRequest::SyncFolder {
                        folder_id: f.id,
                        path: f.path.clone(),
                    });
                }
            }
        }
    }

    /// Mark a cached message read in every list that holds it, so unread badges
    /// update immediately without waiting for the next server sync.
    fn mark_cached_read(&mut self, account_id: u32, message_id: u32) {
        self.set_cached_unread(account_id, message_id, false);
    }

    /// Set a cached message's unread flag in every list that holds it.
    fn set_cached_unread(&mut self, account_id: u32, message_id: u32, unread: bool) {
        for ((aid, _), msgs) in self.message_cache.iter_mut() {
            if *aid == account_id {
                if let Some(m) = msgs.iter_mut().find(|m| m.id == message_id) {
                    m.unread = unread;
                }
            }
        }
        for ((aid, _), msgs) in self.unified_slices.iter_mut() {
            if *aid == account_id {
                if let Some(m) = msgs.iter_mut().find(|m| m.id == message_id) {
                    m.unread = unread;
                }
            }
        }
    }

}

/// The desktop's interface font ("Adwaita Sans 11") as a Pango description:
/// what the reader sets mail in when the font override is on but no font
/// was chosen (#56). Cantarell 11 when GTK has no settings (headless).
fn interface_font() -> String {
    gtk::Settings::default()
        .and_then(|s| s.gtk_font_name())
        .map(|f| f.to_string())
        .filter(|f| !f.trim().is_empty())
        .unwrap_or_else(|| "Cantarell 11".to_string())
}

/// Where the whole release history lives.
const RELEASE_NOTES_URL: &str = "https://github.com/hyprlab/hylki/blob/main/docs/RELEASE_NOTES.md";

/// This release's section of `RELEASE_NOTES.md` — the same text the GitHub
/// release carries — headed by the version.
fn current_release_notes() -> String {
    let md = include_str!("../docs/RELEASE_NOTES.md");
    let head = format!("## What's new in {}", crate::VERSION);
    let mut out = String::new();
    let mut on = false;
    for line in md.lines() {
        if line.starts_with("## ") {
            if on {
                break;
            }
            on = line == head || line.starts_with(&format!("{head} "));
            if on {
                out.push_str(&format!("## {}\n", crate::VERSION));
            }
            continue;
        }
        if on {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The About window's "Release Notes" page: this release's notes, with the
/// full history a button away on GitHub.
fn release_notes_page() -> adw::NavigationPage {
    notes_page("Release Notes", "notes", &current_release_notes(), Some((i18n("Full Release Notes on GitHub"), RELEASE_NOTES_URL)))
}

/// Inline Markdown → Pango markup: `**bold**`, `*italic*`, `` `code` `` and
/// `[text](url)`. Everything outside a marker is escaped, so the source may
/// contain `&` or `<` safely. Emphasis nests (a bold link, a link with code in
/// its text); an unclosed marker stays the literal character it is.
fn md_inline(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let joined = |from: usize, to: usize| -> String { chars[from..to].iter().collect() };
    // Where `needle` next starts, at or after `from`.
    let find = |from: usize, needle: &[char]| -> Option<usize> {
        if needle.len() > chars.len() {
            return None;
        }
        (from..=chars.len() - needle.len()).find(|&i| chars[i..i + needle.len()] == *needle)
    };

    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        // Code spans first: their contents are literal, never re-parsed.
        if chars[i] == '`' {
            if let Some(end) = find(i + 1, &['`']) {
                out.push_str("<tt>");
                out.push_str(&gtk::glib::markup_escape_text(&joined(i + 1, end)));
                out.push_str("</tt>");
                i = end + 1;
                continue;
            }
        } else if chars[i..].starts_with(&['*', '*']) {
            if let Some(end) = find(i + 2, &['*', '*']) {
                out.push_str("<b>");
                out.push_str(&md_inline(&joined(i + 2, end)));
                out.push_str("</b>");
                i = end + 2;
                continue;
            }
        } else if chars[i] == '*' {
            if let Some(end) = find(i + 1, &['*']) {
                out.push_str("<i>");
                out.push_str(&md_inline(&joined(i + 1, end)));
                out.push_str("</i>");
                i = end + 1;
                continue;
            }
        } else if chars[i] == '[' {
            if let Some(close) = find(i + 1, &[']']) {
                if chars.get(close + 1) == Some(&'(') {
                    if let Some(paren) = find(close + 2, &[')']) {
                        out.push_str(&format!(
                            "<a href=\"{}\">{}</a>",
                            gtk::glib::markup_escape_text(&joined(close + 2, paren)),
                            md_inline(&joined(i + 1, close)),
                        ));
                        i = paren + 1;
                        continue;
                    }
                }
            }
        }
        out.push_str(&gtk::glib::markup_escape_text(&chars[i].to_string()));
        i += 1;
    }
    out
}

/// A wrapped label carrying inline Markdown. Links are opened through the app's
/// own URI handler rather than GTK's default, which has no portal under Flatpak.
fn md_label(text: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_markup(&md_inline(text));
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_xalign(0.0);
    label.set_halign(gtk::Align::Start);
    for class in classes {
        label.add_css_class(class);
    }
    label.connect_activate_link(|_, uri| {
        crate::oauth::open_uri(uri);
        gtk::glib::Propagation::Stop
    });
    label
}

/// Render Markdown as a column of widgets rather than one long label: a bullet
/// gets its own column so wrapped lines align under the text instead of running
/// back under the bullet, and each heading carries its own spacing. Handles
/// headings, bullets (nested one level), indented continuation paragraphs, and
/// the inline syntax in [`md_inline`].
fn md_column(md: &str) -> gtk::Box {
    /// A logical Markdown block, with source lines coalesced — CHANGELOG.md
    /// is hard-wrapped at ~72 columns, and rendering each source line as its
    /// own label left a ragged right edge instead of flowing text.
    enum Block {
        H2(String),
        H3(String),
        Bullet { nested: bool, text: String },
        Para { indented: bool, text: String },
    }

    let mut blocks: Vec<Block> = Vec::new();
    let mut cur: Option<Block> = None;
    let mut flush = |cur: &mut Option<Block>, blocks: &mut Vec<Block>| {
        if let Some(b) = cur.take() {
            blocks.push(b);
        }
    };
    for raw in md.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if trimmed.is_empty() {
            flush(&mut cur, &mut blocks);
        } else if trimmed.starts_with("# ") {
            // The page's header bar already shows the document's title.
            flush(&mut cur, &mut blocks);
        } else if let Some(rest) = trimmed.strip_prefix("## ") {
            flush(&mut cur, &mut blocks);
            blocks.push(Block::H2(rest.to_string()));
        } else if let Some(rest) = trimmed.strip_prefix("### ") {
            flush(&mut cur, &mut blocks);
            blocks.push(Block::H3(rest.to_string()));
        } else if let Some(rest) =
            trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* "))
        {
            flush(&mut cur, &mut blocks);
            cur = Some(Block::Bullet { nested: indent >= 2, text: rest.to_string() });
        } else {
            // A plain line continues the open bullet/paragraph, or starts one.
            match &mut cur {
                Some(Block::Bullet { text, .. }) | Some(Block::Para { text, .. }) => {
                    text.push(' ');
                    text.push_str(trimmed);
                }
                _ => {
                    cur = Some(Block::Para { indented: indent >= 2, text: trimmed.to_string() })
                }
            }
        }
    }
    flush(&mut cur, &mut blocks);

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let mut first = true;
    // Anything before the first section heading is document front-matter
    // (RELEASE_NOTES' intro paragraph), not notes — skip it.
    let mut seen_section = false;
    for block in &blocks {
        if !seen_section {
            match block {
                Block::H2(_) | Block::H3(_) => seen_section = true,
                _ => continue,
            }
        }
        let widget: gtk::Widget = match block {
            Block::H3(text) => {
                let label = md_label(text, &["heading"]);
                label.set_margin_top(if first { 0 } else { 14 });
                label.into()
            }
            Block::H2(text) => {
                let label = md_label(text, &["title-4"]);
                label.set_margin_top(if first { 0 } else { 22 });
                label.set_margin_bottom(2);
                label.into()
            }
            Block::Bullet { nested, text } => {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                row.set_margin_top(6);
                row.set_margin_start(if *nested { 18 } else { 0 });
                let bullet = gtk::Label::new(Some(if *nested { "\u{25e6}" } else { "\u{2022}" }));
                bullet.set_valign(gtk::Align::Start);
                bullet.add_css_class("dim-label");
                row.append(&bullet);
                let label = md_label(text, &[]);
                label.set_hexpand(true);
                row.append(&label);
                row.into()
            }
            Block::Para { indented, text } => {
                let label = md_label(text, &[]);
                label.set_margin_top(8);
                label.set_margin_start(if *indented { 26 } else { 0 });
                label.into()
            }
        };
        column.append(&widget);
        first = false;
    }
    column
}

/// Build a scrollable About sub-page from Markdown for the navigation stack,
/// reachable by `tag`. Pushed pages get a back button and slide animation from
/// the parent `NavigationView`.
fn notes_page(title: &str, tag: &str, md: &str, link: Option<(String, &'static str)>) -> adw::NavigationPage {
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    let clamp = adw::Clamp::builder().maximum_size(460).build();

    let column = md_column(md);
    column.set_margin_top(18);
    column.set_margin_bottom(24);
    column.set_margin_start(18);
    column.set_margin_end(18);
    // A button at the foot, for the rest of the story elsewhere.
    if let Some((label, url)) = link {
        let button = gtk::Button::with_label(&label);
        button.add_css_class("pill");
        button.add_css_class("suggested-action");
        button.set_halign(gtk::Align::Center);
        button.set_margin_top(18);
        button.connect_clicked(move |b| {
            let parent = b.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            crate::ui::launch::open_link(url, parent.as_ref());
        });
        column.append(&button);
    }

    clamp.set_child(Some(&column));
    scroller.set_child(Some(&clamp));

    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&scroller));

    adw::NavigationPage::builder()
        .title(title)
        .tag(tag)
        .child(&tv)
        .build()
}

/// One single-key shortcut. Gmail-compatible where Gmail and the request agree
/// (issue #5); where they differ, the request wins — `a` archives here, rather
/// than replying to all as it does in Gmail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    NextMessage,
    PrevMessage,
    OpenMessage,
    BackToList,
    NextInThread,
    PrevInThread,
    ToggleSelect,
    Reply,
    ReplyAll,
    Forward,
    Archive,
    Delete,
    Spam,
    Star,
    ToggleRead,
    Compose,
    Search,
    Shortcuts,
    /// Add or remove the Nth tag (1-based, in Settings order) on the
    /// message being read (#157).
    Tag(u8),
    /// Take every tag off the message being read.
    ClearTags,
}

/// The shortcut for a key press, if any. `shift` distinguishes `r` from `R`.
fn shortcut_for(key: gtk::gdk::Key, shift: bool) -> Option<Shortcut> {
    use gtk::gdk::Key;
    let action = match key {
        Key::j | Key::Down => Shortcut::NextMessage,
        Key::k | Key::Up => Shortcut::PrevMessage,
        Key::l | Key::Right => Shortcut::OpenMessage,
        Key::h | Key::Left | Key::u => Shortcut::BackToList,
        Key::w => Shortcut::NextInThread,
        Key::b => Shortcut::PrevInThread,
        Key::x => Shortcut::ToggleSelect,
        Key::r if !shift => Shortcut::Reply,
        Key::R => Shortcut::ReplyAll,
        Key::f => Shortcut::Forward,
        Key::a => Shortcut::Archive,
        Key::d => Shortcut::Delete,
        Key::exclam => Shortcut::Spam,
        Key::s => Shortcut::Star,
        Key::m => Shortcut::ToggleRead,
        Key::c => Shortcut::Compose,
        Key::slash => Shortcut::Search,
        Key::question => Shortcut::Shortcuts,
        // Tags by number, the way Evolution labels mail (#157). 0 clears.
        Key::_1 | Key::KP_1 => Shortcut::Tag(1),
        Key::_2 | Key::KP_2 => Shortcut::Tag(2),
        Key::_3 | Key::KP_3 => Shortcut::Tag(3),
        Key::_4 | Key::KP_4 => Shortcut::Tag(4),
        Key::_5 | Key::KP_5 => Shortcut::Tag(5),
        Key::_6 | Key::KP_6 => Shortcut::Tag(6),
        Key::_7 | Key::KP_7 => Shortcut::Tag(7),
        Key::_8 | Key::KP_8 => Shortcut::Tag(8),
        Key::_9 | Key::KP_9 => Shortcut::Tag(9),
        Key::_0 | Key::KP_0 => Shortcut::ClearTags,
        _ => return None,
    };
    Some(action)
}

/// Every shortcut with its key and description, for the reference window.
const SHORTCUT_HELP: &[(&str, &[(&str, &str)])] = &[
    (
        i18n_noop("Move around"),
        &[
            ("j  or  ↓", i18n_noop("Next message")),
            ("k  or  ↑", i18n_noop("Previous message")),
            ("l  or  →", i18n_noop("Open the selected message")),
            ("h  or  ←  or  u", i18n_noop("Back to the message list")),
            ("w", i18n_noop("Next message in the conversation")),
            ("b", i18n_noop("Previous message in the conversation")),
            ("/", i18n_noop("Search")),
        ],
    ),
    (
        i18n_noop("Act on a message"),
        &[
            ("r", i18n_noop("Reply")),
            ("R", i18n_noop("Reply to all")),
            ("f", i18n_noop("Forward")),
            ("a", i18n_noop("Archive")),
            ("d", i18n_noop("Delete")),
            ("!", i18n_noop("Mark as spam")),
            ("s", i18n_noop("Star or unstar")),
            ("m", i18n_noop("Mark read or unread")),
            ("x", i18n_noop("Select this row (for a bulk action)")),
            ("1 … 9", i18n_noop("Add or remove a tag (the first nine, in Settings order)")),
            ("0", i18n_noop("Remove every tag")),
        ],
    ),
    (
        i18n_noop("Everything else"),
        &[
            ("c", i18n_noop("Compose")),
            ("Ctrl+Enter", i18n_noop("Send the message you are writing")),
            ("Esc", i18n_noop("Back out of a reply and return to the list")),
            ("Ctrl+Z", i18n_noop("Undo the last action, or the last edit while you are writing")),
            ("Ctrl+Shift+Z", i18n_noop("Redo it (Ctrl+Y does the same)")),
            ("Ctrl+P", i18n_noop("Print the message you are reading")),
            ("Ctrl+Shift+P", i18n_noop("Preview it as a PDF first")),
            ("Ctrl+Shift+S", i18n_noop("Reveal the status bar (also: long-press Refresh)")),
            ("Ctrl+Shift+A", i18n_noop("Show or hide the accounts in the sidebar")),
            ("Ctrl+Shift+F", i18n_noop("Focus Mode on or off")),
            ("Ctrl++  /  Ctrl+-", i18n_noop("Message zoom in or out")),
            ("Ctrl+0", i18n_noop("Message zoom back to the default")),
            ("Ctrl+Shift+C", i18n_noop("Console mode (when enabled in Settings)")),
            ("Ctrl+W", i18n_noop("Close the window (background sync keeps running)")),
            ("Ctrl+Q", i18n_noop("Quit Hylki entirely")),
            ("?", i18n_noop("This list")),
        ],
    ),
];

/// Whether a keystroke should be left to the widget that has focus. Typing in a
/// search field, an address row or the composer must never archive mail, and the
/// message view is a web view that handles its own keys (find, scrolling).
fn focus_takes_keys(window: &adw::ApplicationWindow) -> bool {
    focus_matches(window, true)
}

/// Whether focus is in something being typed into. Narrower than
/// [`focus_takes_keys`]: Escape means "back out" while reading a message, but in
/// a search field it means "clear the search", which is the field's business.
fn focus_is_text(window: &adw::ApplicationWindow) -> bool {
    focus_matches(window, false)
}

/// Whether keyboard focus sits inside a composer — whose editor must keep
/// Ctrl+Z for its own text undo.
fn focus_in_compose(window: &adw::ApplicationWindow) -> bool {
    let mut w = gtk::prelude::GtkWindowExt::focus(window);
    while let Some(cur) = w {
        if cur.has_css_class("compose-pane") || cur.has_css_class("inline-compose-surface") {
            return true;
        }
        w = cur.parent();
    }
    false
}

fn focus_matches(window: &adw::ApplicationWindow, include_web_view: bool) -> bool {
    let Some(focus) = gtk::prelude::GtkWindowExt::focus(window) else {
        return false;
    };
    let mut node = Some(focus);
    while let Some(widget) = node {
        if widget.is::<gtk::Editable>() || widget.is::<gtk::TextView>() {
            return true;
        }
        if include_web_view && widget.is::<webkit6::WebView>() {
            return true;
        }
        node = widget.parent();
    }
    false
}

/// Whether to serve the built-in sample/demo data (for screenshots). Off unless
/// The split reply's own height on its Paned: the divider position above
/// the reader, and what the position leaves of the pane beneath it, where
/// the position is the reader's height (#212).
fn split_reply_height(split: &gtk::Paned, bottom: bool) -> i32 {
    if bottom {
        split.height() - split.position()
    } else {
        split.position()
    }
}

/// Let (or forbid) the split reply's child of `split` shrink below the
/// composer's minimum — needed for the slide, which runs the panel through
/// heights the composer could not otherwise be allocated.
fn set_split_shrink(split: &gtk::Paned, bottom: bool, shrink: bool) {
    if bottom {
        split.set_shrink_end_child(shrink);
    } else {
        split.set_shrink_start_child(shrink);
    }
}

/// `HYLKI_DEMO` is set, so removing all real accounts leaves the app blank.
/// Stand-in [`AccountConfig`]s mirroring the demo backend's three accounts
/// (same names, colours and emoji), so the Accounts window has something to
/// show in demo screenshots.
/// The demo's stand-in accounts as last edited in the Accounts panel, or
/// the stock ones. The stand-in secret is not serialised, so it is put
/// back on load (the editor will not save an account without one).
fn demo_account_configs_saved() -> Vec<AccountConfig> {
    match config::load_demo_accounts() {
        Some(mut saved) => {
            for a in saved.iter_mut() {
                if a.password.is_empty() {
                    a.password = "demo".into();
                }
            }
            saved
        }
        None => demo_account_configs(),
    }
}

fn demo_account_configs() -> Vec<AccountConfig> {
    let mk = |name: &str, email: &str, color: &str, emoji: &str| AccountConfig {
        name: name.into(),
        email: email.into(),
        protocol: Default::default(),
        imap_host: format!("imap.{}", email.split('@').nth(1).unwrap_or("example.com")),
        imap_port: 993,
        smtp_host: format!("smtp.{}", email.split('@').nth(1).unwrap_or("example.com")),
        smtp_port: 587,
        username: email.into(),
        // A stand-in secret: the account editor refuses to save an account
        // without one, and the demo's private bus has no keyring to ask.
        password: "demo".into(),
        smtp_separate: false,
        tls_accept_hostname_mismatch: false,
        smtp_username: String::new(),
        smtp_password: String::new(),
        color: Some(color.into()),
        emoji: Some(emoji.into()),
        avatar: None,
        gravatar: false,
        signature: None,
        signature_html: false,
        label: None,
        aliases: Vec::new(),
        enabled: true,
        goa_id: None,
        goa_mail_disabled: false,
        goa_enabled_before_mail_disabled: true,
        oauth: false,
        oauth_settings: None,
        oauth_refresh: String::new(),
        push: None,
        folder_roles: Default::default(),
        hidden_folders: Vec::new(),
        folders_seeded: false,
        sent_copy_path: None,
        server_saves_sent: false,
        empty_junk_days: 0,
        empty_trash_days: 0,
        pgp_key: None,
    };
    vec![
        mk("Jason M.", "jason@hylki.hyprlab.co", "#3584e4", "🚀"),
        mk("Hyprlab", "hello@hyprlab.dev", "#2ec27e", "🦀"),
        mk("Jason (Personal)", "jason.m@fastmail.com", "#9141ac", "🌿"),
    ]
}

/// The demo's tags (#71): the keywords its sample messages carry, so the
/// Tags section has rows and the row chips have colours.
fn demo_tags() -> Vec<config::Tag> {
    let mk = |name: &str, keyword: &str, color: &str| config::Tag {
        name: name.into(),
        keyword: keyword.into(),
        color: color.into(),
    };
    vec![
        mk("Work", "Work", "#1c71d8"),
        mk("To Do", "To_Do", "#e66100"),
        mk("Personal", "Personal", "#2ec27e"),
    ]
}

/// The demo's filter rules, one per account, each filing into that
/// account's custom folder (backend.rs) and listed under All Inboxes. Their
/// matches are chosen to hit only the mail already in those folders: the
/// mock backend never moves anything, so a rule catching inbox mail would
/// only make it vanish from the list.
fn demo_filters() -> Vec<config::FilterRule> {
    use config::{FilterField, FilterMatch, FilterRule};
    let mk = |name: &str, email: &str, field: FilterField, matcher: FilterMatch, value: &str, dest: &str| FilterRule {
        name: name.into(),
        account_email: email.into(),
        field,
        matcher,
        value: value.into(),
        more: Vec::new(),
        any: false,
        dest_path: dest.into(),
        tag: String::new(),
        count_unread: true,
    };
    vec![
        mk("Substack", "jason@hylki.hyprlab.co", FilterField::FromAddress, FilterMatch::EndsWith, "substack.com", "Newsletters"),
        mk("Invoices", "hello@hyprlab.dev", FilterField::Subject, FilterMatch::Contains, "invoice", "Invoices"),
        mk("", "jason.m@fastmail.com", FilterField::Subject, FilterMatch::Contains, "order", "Orders"),
    ]
}

/// What the Undo menu calls a star change.
fn star_label(starred: bool) -> String {
    if starred { i18n("Star") } else { i18n("Unstar") }
}

/// The same for a read/unread change.
fn read_label(read: bool) -> String {
    if read { i18n("Mark as Read") } else { i18n("Mark as Unread") }
}

/// A sender as a dialog or toast names them: the display name, else the address.
fn sender_label(message: &Message) -> String {
    let name = message.from_name.trim();
    if name.is_empty() { message.from_addr.clone() } else { name.to_string() }
}

fn demo_mode() -> bool {
    std::env::var_os("HYLKI_DEMO").is_some()
}

/// The folder a sent copy is filed in (#199): the chosen one when the account
/// still has it, else the Sent folder, else nowhere. A choice whose folder was
/// renamed or removed server-side falls back rather than appending into a
/// mailbox the server would have to create under an old name.
fn pick_sent_copy(chosen: Option<&str>, folders: &[Folder]) -> Option<String> {
    chosen
        .filter(|path| folders.iter().any(|f| f.path == *path))
        .map(str::to_string)
        .or_else(|| {
            folders
                .iter()
                .find(|f| f.kind == FolderKind::Sent)
                .map(|f| f.path.clone())
        })
}

/// Apply an account's manual special-folder assignments (#82) over the
/// auto-detected kinds (see [`crate::models::assign_folder_roles`]), then
/// re-sort into the fixed role order. Folder ids are untouched — they are
/// referenced from cached messages. The worker already applies the same
/// assignments to every listing it sends; this keeps the live lists right
/// between a Settings save and the reconnect's fresh LIST.
fn apply_folder_roles(
    roles: &std::collections::BTreeMap<String, String>,
    folders: &mut Vec<Folder>,
) {
    if roles.is_empty() {
        return;
    }
    crate::models::assign_folder_roles(roles, folders);
    folders.sort_by(|a, b| {
        crate::worker::folder_order(a.kind)
            .cmp(&crate::worker::folder_order(b.kind))
            .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
    });
}

/// A `mailto:` handed to the app before its component was up. `queue_mailto`
/// runs on GApplication's `open` signal, which can fire before `AppModel::init`
/// installs the sender — anything early waits here and is drained by init.
static MAILTO_PENDING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
static MAILTO_SENDER: std::sync::OnceLock<relm4::Sender<AppMsg>> = std::sync::OnceLock::new();

/// Files handed in (via `connect_open`) before the app's component was up —
/// same early-arrival race as `MAILTO_PENDING`, queued separately since each
/// batch opens its own composer.
static ATTACH_PENDING: std::sync::Mutex<Vec<Vec<std::path::PathBuf>>> =
    std::sync::Mutex::new(Vec::new());

/// Route files to attach to a fresh composer (from main's `connect_open`).
pub fn queue_attach_files(paths: Vec<std::path::PathBuf>) {
    match MAILTO_SENDER.get() {
        Some(s) => {
            let _ = s.send(AppMsg::OpenWithFiles(paths));
        }
        None => ATTACH_PENDING.lock().unwrap().push(paths),
    }
}

/// `mid:` links that arrived before the component was up (see `MAILTO_PENDING`).
static MID_PENDING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Route a `mid:` URI to the app (from main's `connect_open`).
pub fn queue_mid(uri: String) {
    match MAILTO_SENDER.get() {
        Some(s) => {
            let _ = s.send(AppMsg::OpenMid(uri));
        }
        None => MID_PENDING.lock().unwrap().push(uri),
    }
}

/// The Message-ID a `mid:` URI (RFC 2392) names, normalized the way the cache
/// stores them: percent-decoded, angle brackets and whitespace stripped,
/// lowercased, and without any `/cid` part. `None` for anything else.
fn parse_mid(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("mid:").or_else(|| uri.strip_prefix("MID:"))?;
    // GLib normalizes a scheme it does not know to `mid:///…` on the way
    // through GFile (and decodes some of the escapes), so leading slashes
    // are not part of the id.
    let decoded = crate::ui::rich_editor::percent_decode(rest.trim_start_matches('/'));
    // An optional `/content-id` follows the message-id. Slashes are legal
    // inside a message-id's left part (GitHub's have several), so only a
    // slash after the `@`, in the domain part, ends the id.
    let at = decoded.find('@')?;
    let id = match decoded[at..].find('/') {
        Some(i) => &decoded[..at + i],
        None => &decoded,
    };
    let id = id.trim().trim_start_matches('<').trim_end_matches('>').trim().to_ascii_lowercase();
    (!id.is_empty()).then_some(id)
}

/// Route a `mailto:` URI to the app (from main's `connect_open`).
pub fn queue_mailto(uri: String) {
    match MAILTO_SENDER.get() {
        Some(s) => {
            let _ = s.send(AppMsg::OpenMailto(uri));
        }
        None => MAILTO_PENDING.lock().unwrap().push(uri),
    }
}

/// Resolve a notification's `(account_id, folder_id, message_id)` to the full
/// cached [`Message`] (#38): the notified message may not be in the current
/// view, but the worker upserts it into the cache before notifying, so the
/// cache always has it.
/// What identifies a tray mail list for change detection: the total, then
/// each card.
fn tray_mail_key(mail: &crate::tray::TrayMailList) -> Vec<(u32, u32, String, String)> {
    std::iter::once((u32::MAX, mail.unread, String::new(), String::new()))
        .chain(
            mail.items
                .iter()
                .map(|m| (m.account_id, m.message_id, m.heading.clone(), m.subject.clone())),
        )
        .collect()
}

fn notified_message(
    account_id: u32,
    folder_id: u32,
    message_id: u32,
    folders: &std::collections::HashMap<u32, Vec<Folder>>,
) -> Option<Message> {
    let path = folders.get(&account_id)?.iter().find(|f| f.id == folder_id)?.path.clone();
    let cache = crate::cache::Cache::open().ok()?;
    cache
        .load_messages(account_id, &path, folder_id)
        .into_iter()
        .find(|m| m.id == message_id)
}

/// Percent-decode for mailto components (RFC 6068): `%XX` only — `+` stays
/// literal, because plus-addressing (`user+tag@example.com`) is a real thing.
fn pct_decode_mailto(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Turn a `mailto:` URI into a composer prefill (RFC 6068: address part plus
/// to/cc/bcc/subject/body query keys, all percent-encoded).
fn parse_mailto(uri: &str) -> Option<crate::ui::compose::ComposePrefill> {
    let rest = uri.strip_prefix("mailto:")?;
    let (addr, query) = rest.split_once('?').unwrap_or((rest, ""));
    // Leading slashes aren't part of any address: Nautilus's "Send by email"
    // opens `mailto:///?attach=…` (#90), and sloppy generators write
    // `mailto://user@host` URL-style.
    let addr = addr.trim_start_matches('/');
    let mut to = pct_decode_mailto(addr);
    let (mut cc, mut bcc, mut subject, mut body) =
        (String::new(), String::new(), String::new(), String::new());
    let mut attachments: Vec<std::path::PathBuf> = Vec::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = pct_decode_mailto(v);
        match k.to_ascii_lowercase().as_str() {
            // A second `to` joins the address part, comma-separated.
            "to" if !v.is_empty() => {
                if !to.is_empty() {
                    to.push_str(", ");
                }
                to.push_str(&v);
            }
            "cc" => cc = v,
            "bcc" => bcc = v,
            "subject" => subject = v,
            "body" => body = v,
            // Nautilus's "Send by email" (and xdg-email) pass the files as
            // attach= parameters (#90) — an absolute path or a file:// URI.
            // Only absolute paths are accepted; the caller re-checks that
            // each names a real file before attaching.
            "attach" | "attachment" if !v.is_empty() => {
                let path = v.strip_prefix("file://").unwrap_or(&v);
                if path.starts_with('/') {
                    attachments.push(std::path::PathBuf::from(path));
                }
            }
            _ => {}
        }
    }
    // The rich editor takes HTML; the mailto body is plain text.
    let body_html = if body.is_empty() {
        String::new()
    } else {
        body.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace("\r\n", "\n")
            .replace('\n', "<br>")
    };
    Some(crate::ui::compose::ComposePrefill {
        to,
        cc,
        bcc,
        subject,
        body_html,
        attachments,
        ..Default::default()
    })
}

/// Render the window's widget tree to a PNG at 2x (crisp text for marketing
/// shots). Content only — the compositor's shadow/frame is not part of the
/// tree, which is exactly what the site and store screenshots want.
/// Showcase helper: scroll every scrolled window under `w` `frac` of the
/// way down (1.0 = its end).
fn scroll_all_to(w: &gtk::Widget, frac: f64) {
    if let Some(sw) = w.downcast_ref::<gtk::ScrolledWindow>() {
        let adj = sw.vadjustment();
        adj.set_value((adj.upper() - adj.page_size()) * frac.clamp(0.0, 1.0));
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        scroll_all_to(&c, frac);
        child = c.next_sibling();
    }
}

pub(crate) fn showcase_capture(win: &gtk::Widget, path: &str) {
    let (w, h) = (win.width(), win.height());
    if w == 0 || h == 0 {
        tracing::error!("showcase: window not realized");
        return;
    }
    let paintable = gtk::WidgetPaintable::new(Some(win));
    let snapshot = gtk::Snapshot::new();
    snapshot.scale(2.0, 2.0);
    gtk::prelude::PaintableExt::snapshot(&paintable, &snapshot, w as f64, h as f64);
    let Some(node) = snapshot.to_node() else {
        // A fully occluded window is suspended by the compositor and stops
        // producing frames, so the snapshot comes back empty. Raise it and
        // try again shortly.
        tracing::warn!("showcase: nothing to render (window suspended?) — presenting + retrying");
        if let Some(w) = win.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
            w.present();
        }
        let win = win.clone();
        let path = path.to_string();
        gtk::glib::timeout_add_seconds_local_once(2, move || {
            showcase_capture(&win, &path);
        });
        return;
    };
    let Some(renderer) = win.native().and_then(|n| n.renderer()) else {
        tracing::error!("showcase: no renderer");
        return;
    };
    let rect = gtk::graphene::Rect::new(0.0, 0.0, (w * 2) as f32, (h * 2) as f32);
    let texture = renderer.render_texture(&node, Some(&rect));
    match texture.save_to_png(path) {
        Ok(()) => tracing::info!("showcase: saved {path}"),
        Err(e) => tracing::error!("showcase: could not save {path}: {e}"),
    }
}

/// What [`reconcile_goa`] changed: accounts dropped outright (their GOA account
/// is gone), and whether any account was paused or resumed because its Mail
/// service was toggled in GNOME Settings.
#[derive(Default)]
struct GoaReconcile {
    removed: Vec<String>,
    paused_changed: bool,
}

/// Reconcile imported accounts against GNOME Online Accounts: drop the ones
/// whose GOA account no longer exists, and pause — rather than remove — the ones
/// whose Mail service is switched off there, restoring their previous enabled
/// state when it comes back on. Pausing keeps every local setting (label,
/// colour, signature, sidebar state) intact. `live` is a snapshot the caller
/// obtained while GOA was reachable — when it isn't, skip reconciliation
/// entirely, so a momentarily-unavailable GOA never wipes imported accounts.
fn reconcile_goa(config: &mut Vec<AccountConfig>, live: &crate::goa::GoaLiveState) -> GoaReconcile {
    let mut outcome = GoaReconcile::default();
    config.retain(|c| match &c.goa_id {
        Some(id) if !live.account_ids.contains(id) => {
            outcome.removed.push(c.email.clone());
            false
        }
        _ => true,
    });
    for c in config.iter_mut() {
        let Some(id) = &c.goa_id else { continue };
        let mail_disabled = live.disabled_mail_ids.contains(id);
        if mail_disabled && !c.goa_mail_disabled {
            c.goa_mail_disabled = true;
            c.goa_enabled_before_mail_disabled = c.enabled;
            c.enabled = false;
            outcome.paused_changed = true;
        } else if !mail_disabled && c.goa_mail_disabled {
            c.goa_mail_disabled = false;
            c.enabled = c.goa_enabled_before_mail_disabled;
            outcome.paused_changed = true;
        }
    }
    outcome
}

/// Trim the sidebar header in the icon-only rail so it no longer forces a minimum
/// width wider than the rail: hide the (redundant) window-control buttons and tag
/// the header so its Compose/Menu buttons shrink to fit (see `.rail-header` in the
/// stylesheet). The reader pane's header still carries the window's close button,
/// so nothing becomes unreachable.
fn set_sidebar_header_compact(
    header: &adw::HeaderBar,
    title: &gtk::Label,
    menu: &gtk::MenuButton,
    refresh: &gtk::Button,
    compact: bool,
) {
    header.set_show_start_title_buttons(!compact);
    header.set_show_end_title_buttons(!compact);
    // The menu is always in exactly one of three spots — packed end
    // (expanded), the title slot (rail), or packed start (peek) — and
    // HeaderBar::remove detaches it from any of them, so transitions are
    // free to start from whichever state is current. Refresh is either packed
    // start (expanded / peek) or unparented (rail, which stacks its own
    // refresh under the menu instead) — only remove it while it is a child.
    header.remove(menu);
    if refresh.parent().is_some() {
        header.remove(refresh);
    }
    menu.set_margin_start(0);
    // In the rail there is no title to show, so the menu button takes the title
    // slot — the only position a header bar centres — instead of hugging the
    // right edge of an 80px strip. Both widgets are held by the model, so the
    // one being displaced survives being unparented here.
    if compact {
        header.add_css_class("rail-header");
        header.set_title_widget(Some(menu));
    } else {
        header.remove_css_class("rail-header");
        header.set_title_widget(Some(title));
        header.pack_end(menu);
        header.pack_start(refresh);
    }
    title.set_visible(!compact);
}

/// Route a sidebar instance's output to the app — the docked rail and the
/// floating peek panel are two instances, driven alike.
fn sidebar_output_msg(out: SidebarOutput) -> AppMsg {
    match out {
        SidebarOutput::UnifiedSelected => {
            AppMsg::UnifiedSelected(UnifiedView::Kind(FolderKind::Inbox))
        }
        SidebarOutput::UnifiedKindSelected(kind) => {
            AppMsg::UnifiedSelected(UnifiedView::Kind(kind))
        }
        SidebarOutput::UnifiedFilteredSelected => {
            AppMsg::UnifiedSelected(UnifiedView::Filtered)
        }
        SidebarOutput::UnifiedTagsSelected => {
            AppMsg::TagSelected { keyword: None, account: None }
        }
        SidebarOutput::TagSelected { keyword, account } => {
            AppMsg::TagSelected { keyword: Some(keyword), account }
        }
        SidebarOutput::ToggleAccountFiltered(id) => AppMsg::ToggleAccountFiltered(id),
        SidebarOutput::ToggleAccountTags(id) => AppMsg::ToggleAccountTags(id),
        SidebarOutput::AttachmentsSelected => AppMsg::ShowAttachments,
        SidebarOutput::ContactsClicked => AppMsg::OpenContacts,
        SidebarOutput::RefreshRequested => AppMsg::Refresh,
        SidebarOutput::StatusBarRequested => AppMsg::ToggleNotifications,
        SidebarOutput::OpenGnomeContacts => AppMsg::LaunchGnomeContacts,
        SidebarOutput::OutboxSelected => AppMsg::ShowOutbox,
        SidebarOutput::FolderSelected { account_id, folder_id, name, path } => {
            AppMsg::FolderSelected { account_id, folder_id, name, path }
        }
        SidebarOutput::ToggleCollapse(id) => AppMsg::ToggleCollapse(id),
        SidebarOutput::ToggleCustomFolders(id) => AppMsg::ToggleCustomFolders(id),
        SidebarOutput::CollapsedChanged(collapsed) => AppMsg::SidebarCollapsed(collapsed),
        SidebarOutput::SectionsOpen {
            all_inboxes,
            filtered,
            tags,
            starred,
            sent,
            drafts,
            archive,
        } => AppMsg::SidebarSectionsOpen {
            all_inboxes,
            filtered,
            tags,
            starred,
            sent,
            drafts,
            archive,
        },
        SidebarOutput::FolderNodeCollapsed { account_id, path, collapsed } => {
            AppMsg::FolderNodeCollapsed { account_id, path, collapsed }
        }
        SidebarOutput::AddAccount => AppMsg::AddFirstAccount,
        SidebarOutput::ComposeRequested => AppMsg::Compose,
        SidebarOutput::Context(action) => AppMsg::SidebarContext(action),
        SidebarOutput::MoveMessages { dest_account, dest, items } => {
            AppMsg::DropMoveMessages { dest_account, dest, items }
        }
        SidebarOutput::MoveFolder { account_id, path, dest } => {
            AppMsg::MoveFolder { account_id, path, dest }
        }
    }
}

/// Ask for a folder and write every attachment into it.
/// Save one attachment via a file chooser (a card chip's save button, #213).
fn save_attachment(att: Attachment, parent: Option<adw::ApplicationWindow>) {
    let dialog = gtk::FileDialog::builder()
        .initial_name(&att.name)
        .title(&i18n("Save Attachment"))
        .build();
    dialog.save(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
        if let Ok(file) = res {
            if let Some(path) = file.path() {
                let _ = std::fs::write(path, &att.data);
            }
        }
    });
}

fn save_all_attachments(atts: Vec<Attachment>, parent: Option<adw::ApplicationWindow>) {
    let dialog = gtk::FileDialog::new();
    dialog.set_title(&i18n("Save All Attachments"));
    dialog.select_folder(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
        if let Ok(folder) = res {
            if let Some(dir) = folder.path() {
                for att in &atts {
                    let safe = att.name.replace(['/', '\\'], "_");
                    let _ = std::fs::write(dir.join(&safe), &att.data);
                }
            }
        }
    });
}

/// Force (or release) the app-wide colour scheme per the appearance
/// preference — the whole chrome, not just message content, which has its own
/// setting.
fn apply_app_theme(theme: config::AppTheme) {
    let scheme = match theme {
        config::AppTheme::System => adw::ColorScheme::Default,
        config::AppTheme::Light => adw::ColorScheme::ForceLight,
        config::AppTheme::Dark => adw::ColorScheme::ForceDark,
    };
    adw::StyleManager::default().set_color_scheme(scheme);
}

/// Register the app icon so windows and dialogs can find it by name.
///
/// Hylki's toolbar/list icons are shipped inside the binary as a GResource
/// (registered in `main`), so they no longer depend on the host icon theme.
/// GTK auto-adds the bundle's resource path (`/co/hyprlab/Hylki/icons`) to the
/// default theme; we add it explicitly too, so lookups work even if that
/// convention ever changes.
fn register_icons() {
    if let Some(display) = gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        theme.add_resource_path("/co/hyprlab/Hylki/icons");
        // Dev-only: lets the window/about app icon resolve when running from the
        // source tree (uninstalled). Silently ignored on installed systems.
        theme.add_search_path(concat!(env!("CARGO_MANIFEST_DIR"), "/data/icons"));
        // The user's icon directory, where a chosen app icon lives under
        // its own name (app_icon.rs). Inside Flatpak XDG_DATA_HOME is the
        // sandbox's, so GTK wouldn't look there on its own.
        if let Some(home) = std::env::var_os("HOME") {
            theme.add_search_path(std::path::Path::new(&home).join(".local/share/icons"));
        }
    }
    gtk::Window::set_default_icon_name(crate::APP_ID);
}

/// A running tag-finder scan (see `AppMsg::FindTags`).
struct TagScan {
    gen: u32,
    remaining: usize,
    found: Vec<(u32, Vec<KeywordFinding>)>,
}

fn map_event(account_id: u32, event: WorkerEvent) -> AppMsg {
    match event {
        WorkerEvent::AttachmentsScanned { folder_path, added, remaining } => {
            AppMsg::AttachmentsScanned { account_id, folder_path, added, remaining }
        }
        WorkerEvent::BulkComplete => AppMsg::BulkComplete,
        WorkerEvent::Related { message_id, messages } => {
            AppMsg::Related { account_id, message_id, messages }
        }
        WorkerEvent::ThreadCounts { counts } => AppMsg::ThreadCounts { account_id, counts },
        WorkerEvent::Account(a) => AppMsg::SetAccount(a),
        WorkerEvent::Folders(folders) => AppMsg::SetFolders { account_id, folders },
        WorkerEvent::Messages { folder_id, messages } => {
            AppMsg::Messages { account_id, folder_id, messages }
        }
        WorkerEvent::BodyHits { folder_id, hits } => AppMsg::BodyHits { account_id, folder_id, hits },
        WorkerEvent::MessagesAppend { folder_id, messages } => {
            AppMsg::MessagesAppend { account_id, folder_id, messages }
        }
        WorkerEvent::Located { message_id, hit } => AppMsg::MidLocated { account_id, message_id, hit },
        WorkerEvent::KeywordsFound(findings) => AppMsg::KeywordsFound { account_id, findings },
        WorkerEvent::KeywordsSynced { paths } => AppMsg::KeywordsSynced { account_id, paths },
        WorkerEvent::Restored { folder_id, message_ids } => {
            AppMsg::UndoRestored { account_id, folder_id, message_ids }
        }
        WorkerEvent::BackfillDone { folder_id } => AppMsg::BackfillDone { account_id, folder_id },
        WorkerEvent::FolderSynced { folder_id } => {
            AppMsg::FolderSynced { account_id, folder_id }
        }
        WorkerEvent::FolderUnread { folder_id, unread } => {
            AppMsg::FolderUnread { account_id, folder_id, unread }
        }
        WorkerEvent::FolderUnreadByPath { path, unread } => {
            AppMsg::FolderUnreadByPath { account_id, path, unread }
        }
        WorkerEvent::SeenSettled { path, uid } => AppMsg::SeenSettled { account_id, path, uid },
        WorkerEvent::RefsRepaired { folder_id } => {
            AppMsg::RefsRepaired { account_id, folder_id }
        }
        WorkerEvent::Body { message_id, path, body } => {
            AppMsg::Body { account_id, message_id, path, body }
        }
        WorkerEvent::SenderChecked { message_id, check } => {
            AppMsg::SenderChecked { account_id, message_id, check: Box::new(check) }
        }
        WorkerEvent::Source { text, .. } => AppMsg::Source { text },
        WorkerEvent::Attachments { message_id, items } => {
            AppMsg::Attachments { account_id, message_id, items }
        }
        WorkerEvent::AttachmentsPending { message_id } => {
            AppMsg::AttachmentsPending { account_id, message_id }
        }
        WorkerEvent::NoAttachments { message_id } => {
            AppMsg::NoAttachments { account_id, message_id }
        }
        WorkerEvent::HasAttachments { message_id } => {
            AppMsg::HasAttachments { account_id, message_id }
        }
        WorkerEvent::Sent => AppMsg::Sent { account_id },
        WorkerEvent::Outbox { items } => AppMsg::OutboxItems { account_id, items },
        WorkerEvent::Notice(text) => AppMsg::Notice(text),
        WorkerEvent::DraftSaved => AppMsg::DraftSaved,
        WorkerEvent::Status(text) => AppMsg::Status { account_id, text },
        WorkerEvent::Error { text, connectivity } => {
            AppMsg::Error { account_id, text, connectivity }
        }
    }
}

thread_local! {
    /// Reloads the scheme-dependent provider with the colours the live theme
    /// answers with now (see [`install_scheme_css`]).
    static SCHEME_REFRESH: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Re-read the theme's colours into the scheme-dependent CSS, for a change
/// that moves them without flipping the colour scheme.
fn refresh_scheme_css() {
    SCHEME_REFRESH.with(|slot| {
        if let Some(refresh) = slot.borrow().as_ref() {
            refresh();
        }
    });
}

/// Styles that branch on the colour scheme, which static CSS cannot do: a
/// dedicated provider (above the static stylesheet's priority) carries the
/// scheme-dependent values and reloads whenever the scheme flips.
///
/// - The message list's selection pill: the alpha that reads right on dark is
///   heavy on a light ground, so light mode runs 25% lighter.
/// - The remote-content banner's shield: amber on dark, a deeper orange on
///   light where amber washes out.
/// - The composer's ground: the reader's deeper page shade, read from the
///   live theme (#148) rather than the stock GNOME values, so a custom theme
///   carries through to the composer like it does to the reader.
///
/// `window` is only where the theme's named colours are read from.
fn install_scheme_css(window: &impl IsA<gtk::Widget>) {
    let provider = gtk::CssProvider::new();
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
    }
    let window = window.clone().upcast::<gtk::Widget>();
    let apply = move |provider: &gtk::CssProvider, dark: bool| {
        // The selection is the GNOME accent itself at full saturation, with
        // high-contrast white text — and it stays full whether or not the
        // list holds focus, so clicking into the reader never dims it.
        let shield = if dark { "#ffca28" } else { "#ff7800" };
        // The compose surface sits on the reader's deeper page ground — the
        // same shade the threaded cards float on, as the theme defines it.
        let (_, page, _) = crate::ui::message_view::theme_grounds_for(&window, dark);
        provider.load_from_string(&format!(
            ".message-listbox > row:selected .message-row, \
             .message-listbox > row.activatable:selected:hover .message-row, \
             .message-listbox > row.activatable:selected:active .message-row {{ \
               background-color: @accent_bg_color; color: white; }}\
             .remote-alert image {{ color: {shield}; }}\
             .inline-compose-surface, .compose-pane {{ background-color: {page}; }}\
             .reader-split > separator {{ background-color: {page}; }}"
        ));
    };
    let style = adw::StyleManager::default();
    apply(&provider, style.is_dark());
    // A theme change moves the same colours without any scheme flip, so the
    // provider has to be reloadable on demand as well (AppMsg::SetTheme).
    SCHEME_REFRESH.with(|slot| {
        let provider = provider.clone();
        let apply = apply.clone();
        *slot.borrow_mut() = Some(Box::new(move || {
            apply(&provider, adw::StyleManager::default().is_dark());
        }));
    });
    style.connect_dark_notify(move |sm| {
        // The theme's named colours are re-resolved after this signal, not
        // before it: read them now and the lookup answers for the scheme
        // just left (a light-grey composer in dark mode). Apply once the
        // main loop comes round, as the reader does through its own message.
        let dark = sm.is_dark();
        let provider = provider.clone();
        let apply = apply.clone();
        gtk::glib::idle_add_local_once(move || apply(&provider, dark));
    });
}

/// The handed-in files' names, joined, for a dialog.
fn hand_off_names(files: &[std::path::PathBuf]) -> String {
    files
        .iter()
        .map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What the handed-in files add up to on disk.
fn hand_off_size(files: &[std::path::PathBuf]) -> u64 {
    files.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum()
}

/// The "remember this" check a hand-off dialog carries: on, the answer
/// becomes the Settings → System → GNOME Files preference.
fn remember_check() -> gtk::CheckButton {
    let check = gtk::CheckButton::with_label(&i18n("Always do this (change it in Settings → System → GNOME Files)"));
    check.set_halign(gtk::Align::Start);
    check.set_margin_top(6);
    check
}

/// A dialog listing messages to pick one from (a draft to continue, a
/// message to reply to) for handed-in files. `items` pairs each message
/// with its account's name; a search box narrows them by sender, subject
/// or account. `on_pick` gets the hand-off back with the choice, or
/// `None` for "a new message instead"; Cancel drops the hand-off.
#[allow(clippy::too_many_arguments)]
fn show_message_picker(
    parent: &gtk::Window,
    heading: &str,
    body: &str,
    empty_text: &str,
    pick_label: &str,
    items: Vec<(Message, String)>,
    hand_off: FileHandOff,
    on_pick: impl Fn(FileHandOff, Option<Message>) + 'static,
) {
    let dialog = adw::MessageDialog::new(Some(parent), Some(heading), Some(body));
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("new", &i18n("New Message Instead"));
    dialog.add_response("pick", pick_label);
    dialog.set_response_appearance("pick", adw::ResponseAppearance::Suggested);
    dialog.set_response_enabled("pick", false);
    dialog.set_close_response("cancel");

    let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
    column.set_width_request(460);
    let items = std::rc::Rc::new(items);
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::Single);
    if items.is_empty() {
        let empty = gtk::Label::new(Some(empty_text));
        empty.add_css_class("dim-label");
        empty.set_margin_top(12);
        empty.set_margin_bottom(12);
        column.append(&empty);
    } else {
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some(i18n("Search by sender, subject or account").as_str()));
        column.append(&search);
        for (m, account) in items.iter() {
            let row = adw::ActionRow::new();
            let subject = if m.subject.trim().is_empty() { i18n("(no subject)") } else { m.subject.clone() };
            row.set_title(&gtk::glib::markup_escape_text(&subject));
            let who = if m.from_name.trim().is_empty() { m.from_addr.clone() } else { m.from_name.clone() };
            let who = if who.trim().is_empty() { m.to.clone() } else { who };
            let mut sub = vec![who, m.date.clone()];
            if !account.trim().is_empty() {
                sub.push(account.clone());
            }
            row.set_subtitle(&gtk::glib::markup_escape_text(&sub.join("  ·  ")));
            row.set_activatable(true);
            list.append(&row);
        }
        // The search narrows by sender, subject or account, case-folded.
        let filter_items = items.clone();
        list.set_filter_func(move |row| {
            let needle = row
                .parent()
                .and_then(|l| l.downcast::<gtk::ListBox>().ok())
                .and_then(|l| l.prev_sibling())
                .and_then(|w| w.prev_sibling().or(Some(w)))
                .and_then(|w| w.downcast::<gtk::SearchEntry>().ok())
                .map(|e| e.text().to_lowercase())
                .unwrap_or_default();
            if needle.trim().is_empty() {
                return true;
            }
            let Some((m, account)) = filter_items.get(row.index().max(0) as usize) else { return true };
            [&m.from_name, &m.from_addr, &m.subject, &m.to, account]
                .iter()
                .any(|f| f.to_lowercase().contains(needle.trim()))
        });
        {
            let list = list.clone();
            search.connect_search_changed(move |_| list.invalidate_filter());
        }
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_min_content_height(120);
        scroller.set_max_content_height(360);
        scroller.set_propagate_natural_height(true);
        scroller.set_child(Some(&list));
        column.append(&scroller);
        {
            let dialog = dialog.clone();
            list.connect_row_selected(move |_, row| dialog.set_response_enabled("pick", row.is_some()));
        }
        // The first row (the message on screen, or the newest draft) starts
        // selected, so Enter takes the likeliest answer.
        list.select_row(list.row_at_index(0).as_ref());
        {
            let dialog = dialog.clone();
            list.connect_row_activated(move |l, row| {
                l.select_row(Some(row));
                dialog.response("pick");
                dialog.set_visible(false);
            });
        }
    }
    dialog.set_extra_child(Some(&column));

    let hand_off = std::rc::Rc::new(std::cell::RefCell::new(Some(hand_off)));
    dialog.connect_response(None, move |_, resp| {
        let Some(hand_off) = hand_off.borrow_mut().take() else { return };
        match resp {
            "new" => on_pick(hand_off, None),
            "pick" => {
                let picked = list.selected_row().and_then(|r| items.get(r.index().max(0) as usize)).map(|(m, _)| m.clone());
                match picked {
                    Some(m) => on_pick(hand_off, Some(m)),
                    None => on_pick(hand_off, None),
                }
            }
            _ => tracing::info!("file hand-off: cancelled at the picker"),
        }
    });
    dialog.present();
}

fn reply_prefill(m: &Message) -> ComposePrefill {
    let subject = if m.subject.to_lowercase().starts_with("re:") {
        m.subject.clone()
    } else {
        format!("Re: {}", m.subject)
    };
    let attribution = format!("On {}, {} wrote:", m.date, m.from_name);
    ComposePrefill {
        // A Reply-To header is the sender saying "answer me here instead" —
        // a mailing list, a no-reply address with a monitored counterpart. It
        // wins over the From address whenever it is present.
        to: if m.reply_to.is_empty() { m.from_addr.clone() } else { m.reply_to.clone() },
        cc: String::new(),
        subject,
        // Who the original went to, so a mail addressed to one of the
        // account's send-as aliases is answered from that alias (#34).
        reply_addressed_to: format!("{}, {}", m.to, m.cc),
        // The original is quoted as it was written, the way a forward quotes
        // it (#52). Flattening it to text collapsed every level of quoting
        // into one bar, so an answer to an answer lost the shape of the
        // conversation it was answering (#248).
        body_html: quoted_original(&attribution, &m.body),
        // What makes this a reply rather than a new conversation: In-Reply-To
        // names the parent, References carries the chain it belongs to.
        in_reply_to: m.message_id.clone(),
        references: thread_chain(m),
        ..Default::default()
    }
}

/// The References chain for a reply to `m`: whatever `m` already referenced,
/// with `m` itself appended, de-duplicated and in order.
fn thread_chain(m: &Message) -> String {
    let mut chain: Vec<&str> = Vec::new();
    for id in m.references.split_whitespace().chain(std::iter::once(m.message_id.as_str())) {
        if !id.is_empty() && !chain.contains(&id) {
            chain.push(id);
        }
    }
    chain.join(" ")
}

/// Reply-all: To = original sender; Cc = every other recipient (original To +
/// Cc) minus the sender and our own address, de-duplicated.
fn reply_all_prefill(m: &Message, self_email: &str) -> ComposePrefill {
    let mut prefill = reply_prefill(m);
    let self_l = self_email.to_lowercase();
    let from_l = m.from_addr.to_lowercase();
    // Whoever is already in To (the Reply-To address when one exists) must not
    // be repeated in Cc.
    let to_l = prefill.to.to_lowercase();
    let mut cc: Vec<String> = Vec::new();
    for list in [m.to.as_str(), m.cc.as_str()] {
        for addr in list.split(',') {
            let a = addr.trim();
            let al = a.to_lowercase();
            if a.is_empty() || al == self_l || al == from_l || to_l.contains(&al) {
                continue;
            }
            if !cc.iter().any(|x| x.eq_ignore_ascii_case(a)) {
                cc.push(a.to_string());
            }
        }
    }
    prefill.cc = cc.join(", ");
    prefill
}

/// Write attachments out to a private temp directory: the composer attaches
/// files by path, and these exist only as bytes — in a queued message's
/// stored MIME, or in a cached message being copied.
fn stage_attachments(dir_name: &str, items: &[crate::models::Attachment]) -> Vec<std::path::PathBuf> {
    if items.is_empty() {
        return Vec::new();
    }
    let dir = std::env::temp_dir().join(dir_name);
    if std::fs::create_dir_all(&dir).is_err() {
        return Vec::new();
    }
    let mut staged = Vec::new();
    for (i, att) in items.iter().enumerate() {
        // The name came out of a message header; keep it to a single
        // path component.
        let safe = att.name.replace(['/', '\\'], "_");
        let name = if safe.trim().is_empty() { format!("attachment-{}", i + 1) } else { safe };
        let path = dir.join(&name);
        match std::fs::write(&path, &att.data) {
            Ok(()) => staged.push(path),
            Err(e) => tracing::warn!("could not stage {name} for editing: {e}"),
        }
    }
    staged
}

/// A message's own content, ready to be edited rather than quoted: the HTML
/// sanitized exactly as a forward's is (#52, it is the same untrusted mail),
/// or the plain text escaped into it.
fn editable_copy_html(body: &str) -> String {
    if body.contains('<') {
        sanitize_forward_html(&hard_wrap_plain_regions(body))
    } else {
        let text = message_text(body);
        format!(
            "<p>{}</p>",
            text.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('\n', "<br>")
        )
    }
}

fn forward_prefill(m: &Message) -> ComposePrefill {
    let subject = if m.subject.to_lowercase().starts_with("fwd:") {
        m.subject.clone()
    } else {
        format!("Fwd: {}", m.subject)
    };
    let header = format!(
        "---------- Forwarded message ----------\nFrom: {} <{}>\nDate: {}\nSubject: {}",
        m.from_name, m.from_addr, m.date, m.subject
    );
    // Forward the body with its formatting, sanitized (issue #52), so a
    // forwarded invoice still looks like the invoice.
    let body_html = quoted_original(&header, &m.body);
    ComposePrefill {
        to: String::new(),
        cc: String::new(),
        subject,
        body_html,
        ..Default::default()
    }
}

/// Sanitize untrusted message HTML for the editable composer. Ammonia's
/// (conservative) defaults, widened with the structural tags and presentation
/// attributes mail bodies lean on — tables above all. No style attributes, no
/// scripts/handlers ever, URLs limited to http/https/mailto.
fn sanitize_forward_html(html: &str) -> String {
    use std::collections::HashSet;
    let mut b = ammonia::Builder::default();
    b.add_tags(["table", "thead", "tbody", "tfoot", "tr", "td", "th", "font", "center", "u"])
        .add_tag_attributes("table", ["width", "border", "cellpadding", "cellspacing", "align", "bgcolor"])
        .add_tag_attributes("td", ["width", "height", "align", "valign", "colspan", "rowspan", "bgcolor"])
        .add_tag_attributes("th", ["width", "height", "align", "valign", "colspan", "rowspan", "bgcolor"])
        .add_tag_attributes("tr", ["align", "valign", "bgcolor"])
        .add_tag_attributes("font", ["face", "size", "color"])
        .add_tag_attributes("p", ["align"])
        .add_tag_attributes("div", ["align"])
        .add_tags(["img"])
        .add_tag_attributes("img", ["src", "width", "height", "alt"])
        .url_schemes(HashSet::from(["http", "https", "mailto"]));
    b.clean(html).to_string()
}

/// The quoted block a reply or a forward opens with: the attribution line,
/// then the original body as it was written.
///
/// The body is untrusted mail, so it goes through the same sanitizer a
/// forward has used since #52. A body that is plain text through and through
/// keeps the escaped-text path.
fn quoted_original(attribution: &str, body: &str) -> String {
    if !body.contains('<') {
        return quote_block(attribution, &message_text(body));
    }
    let quoted = sanitize_forward_html(&hard_wrap_plain_regions(body));
    quote_block_html(attribution, trim_empty_blocks(&quoted))
}

/// Line breaks that live in CSS, written out as `<br>`.
///
/// A plain-text part is rendered under `white-space: pre-wrap`, where its own
/// newlines *are* its line breaks. The sanitizer keeps no styling, so those
/// breaks have to be in the markup before it runs or the quote arrives in the
/// composer as one run-on paragraph.
fn hard_wrap_plain_regions(html: &str) -> String {
    // A whole document of plain text (the worker's `wrap_plain`): the rule is
    // on `body`, so every newline in it is a break.
    if body_is_pre_wrap(html) {
        let open = html.find("<body").and_then(|i| html[i..].find('>').map(|j| i + j + 1));
        let close = html.rfind("</body>");
        return match (open, close) {
            (Some(open), Some(close)) if open <= close => format!(
                "{}{}{}",
                &html[..open],
                html[open..close].replace('\n', "<br>"),
                &html[close..]
            ),
            _ => html.replace('\n', "<br>"),
        };
    }
    // A message of mixed parts (`wrap_fragment`): only its text parts are
    // pre-wrap, each one a `.vireo-plain` div the worker writes itself, with
    // nothing but escaped text and links inside it.
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find("class=\"vireo-plain\"") {
        let Some(open) = rest[at..].find('>').map(|i| at + i + 1) else { break };
        let Some(close) = rest[open..].find("</div>").map(|i| open + i) else { break };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open..close].replace('\n', "<br>"));
        rest = &rest[close..];
    }
    out.push_str(rest);
    out
}

/// Does this document set `white-space: pre-wrap` on `body` itself?
fn body_is_pre_wrap(html: &str) -> bool {
    let mut rest = html;
    while let Some(at) = rest.find("<style") {
        let Some(open) = rest[at..].find('>').map(|i| at + i + 1) else { return false };
        let close = rest[open..].find("</style>").map(|i| open + i).unwrap_or(rest.len());
        let css: String = rest[open..close].chars().filter(|c| !c.is_whitespace()).collect();
        for rule in css.split('}') {
            let Some((selectors, decls)) = rule.split_once('{') else { continue };
            if selectors.split(',').any(|s| s == "body") && decls.contains("white-space:pre-wrap") {
                return true;
            }
        }
        rest = &rest[close..];
    }
    false
}

/// Drop the empty blocks a quoted body opens or closes with. Every message
/// written in the composer starts on a blank line, and quoting one used to
/// carry that blank line in as the first line of the quote.
fn trim_empty_blocks(html: &str) -> &str {
    const EMPTY: [&str; 8] = [
        "<br>", "<br/>", "<br />", "<p></p>", "<div></div>", "<p><br></p>", "<div><br></div>",
        "&nbsp;",
    ];
    let mut out = html.trim();
    'again: loop {
        for empty in EMPTY {
            if let Some(rest) = out.strip_prefix(empty) {
                out = rest.trim_start();
                continue 'again;
            }
            if let Some(rest) = out.strip_suffix(empty) {
                out = rest.trim_end();
                continue 'again;
            }
        }
        return out;
    }
}

/// Like [`quote_block`], but the quoted body is already-sanitized HTML.
fn quote_block_html(attribution: &str, inner_html: &str) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('\n', "<br>")
    };
    format!(
        "<p class=\"vireo-quote-attr\">{}</p><blockquote>{}</blockquote>",
        esc(attribution),
        inner_html
    )
}

/// Build the HTML quoted block (attribution line + blockquote) for a reply or
/// forward, from plain text so no scripts/remote content leak into the editor.
fn quote_block(attribution: &str, text: &str) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('\n', "<br>")
    };
    format!(
        "<p class=\"vireo-quote-attr\">{}</p><blockquote>{}</blockquote>",
        esc(attribution),
        esc(text)
    )
}

/// A readable plain-text rendering of a message body, which may be HTML. Used to
/// build safe quoted replies/forwards (no scripts, styles or remote content).
pub fn message_text(body: &str) -> String {
    if !body.contains('<') {
        return body.trim().to_string();
    }
    let mut s = strip_block(body, "script");
    s = strip_block(&s, "style");
    s = strip_block(&s, "head");
    // Turn common block/line elements into newlines.
    for (tag, nl) in [
        ("<br>", "\n"), ("<br/>", "\n"), ("<br />", "\n"),
        ("</p>", "\n\n"), ("</div>", "\n"), ("</li>", "\n"),
        ("</tr>", "\n"), ("</h1>", "\n"), ("</h2>", "\n"), ("</h3>", "\n"),
    ] {
        s = s.replace(tag, nl);
        s = s.replace(&tag.to_uppercase(), nl);
    }
    // Strip remaining tags.
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    // Decode the handful of entities that matter for plain text.
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    // Collapse runs of blank lines.
    let mut result = String::new();
    let mut blanks = 0;
    for line in out.lines() {
        if line.trim().is_empty() {
            blanks += 1;
            if blanks <= 1 {
                result.push('\n');
            }
        } else {
            blanks = 0;
            result.push_str(line.trim_end());
            result.push('\n');
        }
    }
    result.trim().to_string()
}

/// Remove `<tag>…</tag>` blocks (case-insensitive) from HTML.
fn strip_block(html: &str, tag: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::new();
    let mut i = 0;
    while i < html.len() {
        if lower[i..].starts_with(&open) {
            if let Some(rel) = lower[i..].find(&close) {
                i += rel + close.len();
                continue;
            } else {
                break; // unterminated — drop the rest
            }
        }
        let ch = html[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn kind_label(kind: FolderKind) -> String {
    i18n(match kind {
        FolderKind::Archive => "archive",
        FolderKind::Trash => "trash",
        _ => "destination",
    })
}

/// Flatten every folder's indexed messages into one pool for cross-folder search.
/// The map is keyed by `(account_id, folder_id)`, so a flat concatenation already
/// spans every folder of every account with no duplicates.
fn build_search_pool(cache: &HashMap<(u32, u32), Vec<Message>>) -> Vec<Message> {
    cache.values().flatten().cloned().collect()
}

/// A conversation member still waiting for its body:
/// `(account_id, message_id, uid, folder_path)`.
type MissingBody = (u32, u32, u32, String);

/// One `LoadBodies` request: the `(account_id, folder_path)` it goes to, and the
/// `(message_id, uid)` of every member it covers.
type BodyBatch = ((u32, String), Vec<(u32, u32)>);

/// Whether two rows are the same mail stored twice, rather than two messages.
///
/// Message-ID is the identity, but it is written by whoever sent the mail and
/// spam reuses one across unrelated messages — so sender and timestamp have to
/// agree as well before one copy is allowed to stand for another. Gmail's label
/// copies agree on all three. Subject is deliberately left out: a re-decoded
/// encoded-word can rewrite it under one label and not another.
fn same_mail(a: &Message, b: &Message) -> bool {
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
fn dedupe_label_copies(messages: Vec<Message>, conv: &[Message], shown_folder: u32) -> Vec<Message> {
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
fn next_after_vanish(
    previous: Option<&Vec<Message>>,
    messages: &[Message],
    cur_uid: u32,
) -> Option<(u32, u32)> {
    let idx = previous?.iter().position(|m| m.uid == cur_uid)?;
    messages
        .get(idx.min(messages.len().checked_sub(1)?))
        .map(|m| (m.account_id, m.id))
}

#[cfg(test)]
mod tests {
    /// A copy to edit carries the message's own content, not a quote of it:
    /// the HTML survives its sanitizing, and plain text becomes a paragraph
    /// with its line breaks kept.
    #[test]
    fn a_copy_keeps_the_body_unquoted() {
        let html = super::editable_copy_html("<p>Hi <b>there</b></p><script>steal()</script>");
        assert!(html.contains("<b>there</b>"), "{html}");
        assert!(!html.contains("script"), "{html}");
        assert!(!html.contains("blockquote"), "{html}");

        let html = super::editable_copy_html("Line one\nLine two");
        assert_eq!(html, "<p>Line one<br>Line two</p>");
    }

    /// A `mid:` link names a Message-ID the way the cache stores it: no
    /// brackets, lowercase, percent-decoded, without a `/cid` part.
    #[test]
    fn mid_links_normalize_to_the_cached_form() {
        let want = Some("cah5b6q=abc123@example.com".to_string());
        assert_eq!(super::parse_mid("mid:CAH5B6Q=abc123@example.com"), want);
        assert_eq!(super::parse_mid("mid:CAH5B6Q%3Dabc123%40example.com"), want);
        assert_eq!(super::parse_mid("mid:%3CCAH5B6Q%3Dabc123%40example.com%3E"), want);
        assert_eq!(super::parse_mid("mid:<CAH5B6Q=abc123@example.com>/part1"), want);
        assert_eq!(super::parse_mid("MID:CAH5B6Q=abc123@example.com"), want);
        assert_eq!(super::parse_mid("mid:///%3CCAH5B6Q=abc123@example.com%3E"), want);
        // Slashes inside the id are part of it; one after the domain is a cid.
        assert_eq!(
            super::parse_mid("mid:hyprlab/hylki/issues/128/5558573653@github.com/att1"),
            Some("hyprlab/hylki/issues/128/5558573653@github.com".to_string())
        );
        assert_eq!(super::parse_mid("mid:"), None);
        assert_eq!(super::parse_mid("mid:not-an-id"), None);
        assert_eq!(super::parse_mid("mailto:a@b.c"), None);
    }

    #[test]
    fn folder_roles_override_detection_and_demote_the_old_holder() {
        use crate::models::{Folder, FolderKind};
        let f = |id: u32, path: &str, kind: FolderKind| Folder {
            id,
            account_id: 1,
            name: path.to_string(),
            path: path.to_string(),
            kind,
            unread: 0,
        };
        let mut folders = vec![
            f(1, "INBOX", FolderKind::Inbox),
            f(2, "Sent Items", FolderKind::Custom),
            f(3, "Sent", FolderKind::Sent),
            f(4, "Rubbish", FolderKind::Custom),
        ];
        let roles: std::collections::BTreeMap<String, String> = [
            ("sent".to_string(), "Sent Items".to_string()),
            ("trash".to_string(), "Rubbish".to_string()),
            // A mapping whose folder no longer exists changes nothing.
            ("junk".to_string(), "Gone".to_string()),
        ]
        .into();
        super::apply_folder_roles(&roles, &mut folders);
        let kind_of = |path: &str| folders.iter().find(|f| f.path == path).unwrap().kind;
        assert_eq!(kind_of("Sent Items"), FolderKind::Sent);
        assert_eq!(kind_of("Sent"), FolderKind::Custom, "old holder demotes");
        assert_eq!(kind_of("Rubbish"), FolderKind::Trash);
        assert_eq!(kind_of("INBOX"), FolderKind::Inbox);
        // Ids survive the re-sort (cached messages reference them).
        assert_eq!(folders.iter().find(|f| f.path == "Sent Items").unwrap().id, 2);
    }

    #[test]
    fn every_undo_step_is_its_own_inverse_twice_over() {
        use super::{FlagChange, FlagKind, FlagRef, UndoStep};
        let steps = vec![
            UndoStep::Move {
                from: "Archive".into(),
                to: "INBOX".into(),
                message_ids: vec!["a@b.c".into()],
            },
            UndoStep::Flags(vec![
                FlagChange {
                    kind: FlagKind::Read,
                    value: false,
                    items: vec![FlagRef { folder_id: 1, uid: 7, id: 7 }],
                },
                FlagChange {
                    kind: FlagKind::Tag("Work".into()),
                    value: true,
                    items: vec![FlagRef { folder_id: 1, uid: 8, id: 8 }],
                },
            ]),
            UndoStep::CreateFolder { path: "Projects".into() },
            UndoStep::DeleteFolder { path: "Projects".into() },
            UndoStep::RenameFolder { from: "Old".into(), to: "New".into() },
        ];
        for step in &steps {
            assert_eq!(&step.inverse().inverse(), step, "{step:?} round-trips");
            assert_ne!(&step.inverse(), step, "{step:?} actually reverses");
        }
        // A move undoes by swapping the two folders, which is what makes
        // redo the very same request back the other way.
        assert_eq!(
            steps[0].inverse(),
            UndoStep::Move {
                from: "INBOX".into(),
                to: "Archive".into(),
                message_ids: vec!["a@b.c".into()],
            }
        );
        // Undoing a folder someone made deletes it again; there is no step
        // that undoes a folder the user deleted.
        assert_eq!(
            UndoStep::CreateFolder { path: "Projects".into() }.inverse(),
            UndoStep::DeleteFolder { path: "Projects".into() }
        );
    }

    #[test]
    fn sent_copies_go_where_the_account_says_including_the_inbox() {
        let f = |id: u32, path: &str, kind: FolderKind| Folder {
            id,
            account_id: 1,
            name: path.to_string(),
            path: path.to_string(),
            kind,
            unread: 0,
        };
        let folders = vec![
            f(1, "INBOX", FolderKind::Inbox),
            f(2, "Sent", FolderKind::Sent),
            f(3, "Archive", FolderKind::Archive),
        ];
        // Nothing chosen: the Sent folder, as before.
        assert_eq!(super::pick_sent_copy(None, &folders).as_deref(), Some("Sent"));
        // The Inbox is a legitimate choice (#199) — unlike a role (#136), a
        // destination takes nothing away from the folder it names.
        assert_eq!(super::pick_sent_copy(Some("INBOX"), &folders).as_deref(), Some("INBOX"));
        assert_eq!(super::pick_sent_copy(Some("Archive"), &folders).as_deref(), Some("Archive"));
        // A folder that has gone from the server falls back to Sent.
        assert_eq!(super::pick_sent_copy(Some("Gone"), &folders).as_deref(), Some("Sent"));
        // No Sent folder and no choice: the message goes out uncopied.
        let bare = vec![f(1, "INBOX", FolderKind::Inbox)];
        assert_eq!(super::pick_sent_copy(None, &bare), None);
        assert_eq!(super::pick_sent_copy(Some("Gone"), &bare), None);
        assert_eq!(super::pick_sent_copy(Some("INBOX"), &bare).as_deref(), Some("INBOX"));
    }

    #[test]
    fn a_role_never_takes_the_inbox() {
        let mut folders = vec![
            Folder { id: 1, account_id: 1, name: "Inbox".into(), path: "INBOX".into(), kind: FolderKind::Inbox, unread: 0 },
            Folder { id: 2, account_id: 1, name: "Sent".into(), path: "Sent".into(), kind: FolderKind::Sent, unread: 0 },
        ];
        let mut roles = std::collections::BTreeMap::new();
        roles.insert("sent".to_string(), "INBOX".to_string());
        super::apply_folder_roles(&roles, &mut folders);
        assert_eq!(folders.iter().find(|f| f.path == "INBOX").unwrap().kind, FolderKind::Inbox);
        assert_eq!(folders.iter().find(|f| f.path == "Sent").unwrap().kind, FolderKind::Sent);
    }

    #[test]
    fn mailto_uris_become_composer_prefills() {
        let p = super::parse_mailto("mailto:ann@example.com").unwrap();
        assert_eq!(p.to, "ann@example.com");
        assert!(p.subject.is_empty() && p.body_html.is_empty());

        let p = super::parse_mailto(
            "mailto:ann@example.com?subject=Hi%20there&cc=bob@x.org&body=line%20one%0Aline%20two",
        )
        .unwrap();
        assert_eq!(p.to, "ann@example.com");
        assert_eq!(p.subject, "Hi there");
        assert_eq!(p.cc, "bob@x.org");
        assert_eq!(p.body_html, "line one<br>line two");

        // Plus-addressing survives: '+' is literal in mailto, never a space.
        let p = super::parse_mailto("mailto:user%2Btag@example.com?to=a+b@x.org").unwrap();
        assert_eq!(p.to, "user+tag@example.com, a+b@x.org");

        // A body with markup arrives escaped, not interpreted.
        let p = super::parse_mailto("mailto:a@b.c?body=%3Cscript%3E").unwrap();
        assert_eq!(p.body_html, "&lt;script&gt;");

        assert!(super::parse_mailto("https://example.com").is_none());

        // Nautilus-style attachments (#90): plain absolute paths and file://
        // URIs, percent-decoded, several allowed; relative paths refused.
        // Nautilus opens `mailto:///?…` — the slashes are not an address.
        let p = super::parse_mailto(
            "mailto:///?attach=/home/u/a%20b.pdf&attach=file:///tmp/c.png&attach=../etc/passwd",
        )
        .unwrap();
        assert_eq!(p.to, "");
        assert_eq!(
            p.attachments,
            vec![
                std::path::PathBuf::from("/home/u/a b.pdf"),
                std::path::PathBuf::from("/tmp/c.png"),
            ],
        );
    }

    #[test]
    fn quoted_reply_keeps_the_levels_of_quoting() {
        // An answer to an answer: the inner quote has to stay one level in
        // (#248), not be flattened into the outer one.
        let body = "<!doctype html><html><head></head><body><div>yada yada</div>\
            <p class=\"vireo-quote-attr\">On Monday, Ada wrote:</p>\
            <blockquote><div>tagada</div></blockquote></body></html>";
        let quoted = super::quoted_original("On Tuesday, Bo wrote:", body);
        let inner = quoted.split_once("</p>").expect("attribution line").1;
        assert!(inner.contains("<blockquote><div>tagada</div></blockquote>"), "{quoted}");
        assert_eq!(inner.matches("<blockquote>").count(), 2, "{quoted}");
    }

    #[test]
    fn quoted_plain_text_keeps_its_line_breaks() {
        // A plain-text part's breaks live in `white-space: pre-wrap`, which
        // the sanitizer strips: they have to become <br> first.
        let doc = "<!doctype html><html><head><meta charset=\"utf-8\"><style>\
            body{margin:0;font:14px/1.5 system-ui,sans-serif;\
            white-space:pre-wrap;word-wrap:break-word}\
            </style></head><body>Hi,\n\nTwo lines.\n</body></html>";
        let quoted = super::quoted_original("hdr", doc);
        assert!(quoted.contains("Hi,<br><br>Two lines."), "{quoted}");
        assert!(!quoted.contains('\n'), "{quoted}");

        // The same in a message of several parts, where only the text part
        // is pre-wrap and the HTML part's source newlines are not breaks.
        let mixed = "<!doctype html><html><head><style>.vireo-plain{white-space:pre-wrap}\
            </style></head><body><div class=\"vireo-plain\">One\nTwo</div>\
            <div>\n<b>Three</b>\n</div></body></html>";
        let quoted = super::quoted_original("hdr", mixed);
        assert!(quoted.contains("One<br>Two"), "{quoted}");
        assert!(quoted.contains("<b>Three</b>"), "{quoted}");
        assert!(!quoted.contains("<br>\n<b>"), "{quoted}");
    }

    #[test]
    fn quoted_body_drops_the_blank_line_it_starts_on() {
        // Every message written in the composer opens on a blank line; the
        // quote should not begin with it.
        let body = "<html><body><div><br></div><div>yada yada</div><br></body></html>";
        let quoted = super::quoted_original("hdr", body);
        let inner = quoted.split_once("</p>").expect("attribution line").1;
        assert!(inner.starts_with("<blockquote><div>yada yada</div>"), "{quoted}");
        assert!(!inner.contains("<br></blockquote>"), "{quoted}");
    }

    #[test]
    fn forward_sanitizer_strips_active_content() {
        use super::sanitize_forward_html;
        let dirty = r#"<table><tr><td onclick="evil()">Total: <b>$40</b></td></tr></table>
            <script>steal()</script>
            <img src="javascript:alert(1)">
            <a href="https://example.com" onmouseover="x()">pay</a>
            <div style="background:url(https://tracker.example/p.gif)">hi</div>"#;
        let clean = sanitize_forward_html(dirty);
        assert!(!clean.contains("script"), "{clean}");
        assert!(!clean.contains("onclick") && !clean.contains("onmouseover"), "{clean}");
        assert!(!clean.contains("javascript:"), "{clean}");
        assert!(!clean.contains("style"), "{clean}");
        // The structure and safe pieces survive.
        assert!(clean.contains("<table>") && clean.contains("<b>$40</b>"), "{clean}");
        assert!(clean.contains(r#"<a href="https://example.com""#), "{clean}");
    }

    #[test]
    fn forward_sanitizer_keeps_presentation_attributes() {
        use super::sanitize_forward_html;
        let html = r##"<table width="600" border="0"><tr><td align="right" bgcolor="#eeeeee">
            <font color="#333333" size="2">Invoice</font></td></tr></table>
            <img src="https://example.com/logo.png" width="120" alt="logo">"##;
        let clean = sanitize_forward_html(html);
        for keep in ["width=\"600\"", "align=\"right\"", "bgcolor=\"#eeeeee\"",
                     "color=\"#333333\"", "src=\"https://example.com/logo.png\""] {
            assert!(clean.contains(keep), "missing {keep} in {clean}");
        }
    }

    #[test]
    fn identities_split_into_name_and_address() {
        use super::split_identity;
        assert_eq!(
            split_identity("Ann Work <ann@work.example>"),
            ("Ann Work".into(), "ann@work.example".into())
        );
        assert_eq!(split_identity("ann@shop.example"), (String::new(), "ann@shop.example".into()));
        assert_eq!(
            split_identity("\"Quoted\" <q@x.example>"),
            ("Quoted".into(), "q@x.example".into())
        );
    }

    use super::*;

    /// A conversation member as the related-lookup hands it over: what matters
    /// here is which folder it came from and what Message-ID it carries.
    fn labelled(folder_id: u32, uid: u32, message_id: &str) -> Message {
        Message {
            id: uid,
            account_id: 1,
            folder_id,
            uid,
            from_name: String::new(),
            from_addr: String::new(),
            reply_to: String::new(),
            to: String::new(),
            cc: String::new(),
            subject: String::new(),
            preview: String::new(),
            body: String::new(),
            date: String::new(),
            timestamp: 0,
            unread: false,
            starred: false,
            keywords: Vec::new(),
            has_attachment: false,
            message_id: message_id.to_string(),
            references: String::new(),
        }
    }

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
        let a = kept.iter().find(|m| m.message_id == "<a@example.com>").unwrap();
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
        assert_eq!(kept.len(), 2, "a shared id from another sender is other mail");
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
        assert_eq!(batches[0], ((1, "INBOX".to_string()), vec![(10, 100), (12, 101)]));
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
    fn single_key_map_matches_the_request() {
        use gtk::gdk::Key;
        // The keys issue #5 asked for, by name.
        assert_eq!(shortcut_for(Key::j, false), Some(Shortcut::NextMessage));
        assert_eq!(shortcut_for(Key::k, false), Some(Shortcut::PrevMessage));
        assert_eq!(shortcut_for(Key::l, false), Some(Shortcut::OpenMessage));
        assert_eq!(shortcut_for(Key::h, false), Some(Shortcut::BackToList));
        assert_eq!(shortcut_for(Key::r, false), Some(Shortcut::Reply));
        assert_eq!(shortcut_for(Key::a, false), Some(Shortcut::Archive));
        assert_eq!(shortcut_for(Key::x, false), Some(Shortcut::ToggleSelect));
        assert_eq!(shortcut_for(Key::d, false), Some(Shortcut::Delete));
        assert_eq!(shortcut_for(Key::w, false), Some(Shortcut::NextInThread));
        assert_eq!(shortcut_for(Key::b, false), Some(Shortcut::PrevInThread));
        // Arrows do what h/j/k/l do.
        assert_eq!(shortcut_for(Key::Down, false), Some(Shortcut::NextMessage));
        assert_eq!(shortcut_for(Key::Up, false), Some(Shortcut::PrevMessage));
        assert_eq!(shortcut_for(Key::Right, false), Some(Shortcut::OpenMessage));
        assert_eq!(shortcut_for(Key::Left, false), Some(Shortcut::BackToList));
        // Shift distinguishes reply from reply-all.
        assert_eq!(shortcut_for(Key::R, true), Some(Shortcut::ReplyAll));
        // Digits toggle tags by position, on the keypad too; 0 clears (#157).
        assert_eq!(shortcut_for(Key::_1, false), Some(Shortcut::Tag(1)));
        assert_eq!(shortcut_for(Key::KP_5, false), Some(Shortcut::Tag(5)));
        assert_eq!(shortcut_for(Key::_9, false), Some(Shortcut::Tag(9)));
        assert_eq!(shortcut_for(Key::_0, false), Some(Shortcut::ClearTags));
        // Anything unmapped is left to the widget with focus.
        assert_eq!(shortcut_for(Key::z, false), None);
        assert_eq!(shortcut_for(Key::Return, false), None);
        assert_eq!(shortcut_for(Key::space, false), None);
    }

    #[test]
    fn every_shortcut_is_documented() {
        // The reference window is the only place the keys are written down, so a
        // new shortcut without a line there would be invisible.
        let documented: Vec<&str> = SHORTCUT_HELP
            .iter()
            .flat_map(|(_, keys)| keys.iter().map(|(key, _)| *key))
            .collect();
        for key in ["j  or  ↓", "r", "a", "d", "w", "b", "x", "?", "1 … 9", "0"] {
            assert!(documented.contains(&key), "{key} is not in the reference");
        }
        // Every documented line has a description.
        for (_, keys) in SHORTCUT_HELP {
            for (key, what) in *keys {
                assert!(!key.trim().is_empty() && !what.trim().is_empty());
            }
        }
    }

    #[test]
    fn md_inline_renders_the_syntax_the_changelog_uses() {
        assert_eq!(md_inline("**bold**"), "<b>bold</b>");
        assert_eq!(md_inline("routed *every* port"), "routed <i>every</i> port");
        assert_eq!(md_inline("`set_visible`"), "<tt>set_visible</tt>");
        assert_eq!(
            md_inline("[Chris](https://github.com/chrispouliot)"),
            "<a href=\"https://github.com/chrispouliot\">Chris</a>"
        );
        // Emphasis nests, as in the changelog's bolded contributor links.
        assert_eq!(
            md_inline("**[Chris](https://example.com)**"),
            "<b><a href=\"https://example.com\">Chris</a></b>"
        );
    }

    #[test]
    fn md_inline_escapes_markup_and_keeps_stray_markers() {
        // Pango would refuse the whole label if these went through unescaped.
        assert_eq!(md_inline("a < b & c"), "a &lt; b &amp; c");
        // A code span's contents are literal, never re-parsed as Markdown.
        assert_eq!(md_inline("`a <b> *c*`"), "<tt>a &lt;b&gt; *c*</tt>");
        // Unclosed markers stay the characters they are rather than eating the line.
        assert_eq!(md_inline("2 * 3 = 6"), "2 * 3 = 6");
        assert_eq!(md_inline("see [the docs"), "see [the docs");
    }

    #[test]
    fn current_release_has_notes() {
        let notes = current_release_notes();
        assert!(notes.lines().count() > 2, "RELEASE_NOTES.md has no section for {}", crate::VERSION);
        assert!(notes.starts_with(&format!("## {}\n", crate::VERSION)));
    }

    #[test]
    fn changelog_and_release_notes_render_without_pango_errors() {
        // Pango refuses to render a label whose markup is malformed, blanking the
        // whole page — so every line of both documents has to parse. Link tags are
        // GtkLabel's own extension and are not known to `parse_markup`, so they
        // are lifted out first (which also asserts each one is closed).
        fn without_links(markup: &str) -> String {
            let mut out = String::new();
            let mut rest = markup;
            while let Some(open) = rest.find("<a href=\"") {
                out.push_str(&rest[..open]);
                let tail = &rest[open..];
                let close = tail.find('>').expect("link tag is closed");
                rest = &tail[close + 1..];
            }
            out.push_str(rest);
            out.replace("</a>", "")
        }

        for md in [
            include_str!("../CHANGELOG.md"),
            include_str!("../docs/RELEASE_NOTES.md"),
        ] {
            for line in md.lines() {
                let markup = md_inline(line);
                assert!(
                    gtk::pango::parse_markup(&without_links(&markup), '\0').is_ok(),
                    "does not parse as Pango markup: {line}\n  -> {markup}"
                );
            }
        }
    }

    fn msg(account_id: u32, folder_id: u32, uid: u32) -> Message {
        Message {
            id: uid,
            account_id,
            folder_id,
            uid,
            from_name: String::new(),
            from_addr: String::new(),
            reply_to: String::new(),
            to: String::new(),
            cc: String::new(),
            subject: String::new(),
            preview: String::new(),
            body: String::new(),
            date: String::new(),
            timestamp: 0,
            unread: false,
            starred: false,
            keywords: Vec::new(),
            has_attachment: false,
            message_id: String::new(),
            references: String::new(),
        }
    }

    /// A sender's Reply-To wins over their From address, and Reply All must
    /// not smuggle the sender back in through Cc.
    #[test]
    fn reply_goes_where_the_sender_asked() {
        let mut m = msg(1, 1, 10);
        m.from_addr = "no-reply@list.example".into();
        m.reply_to = "editor@example.com".into();
        m.to = "me@example.com, other@example.com".into();

        let r = reply_prefill(&m);
        assert_eq!(r.to, "editor@example.com");

        let ra = reply_all_prefill(&m, "me@example.com");
        assert_eq!(ra.to, "editor@example.com");
        assert_eq!(ra.cc, "other@example.com", "no sender, no self, no repeat of To");
    }

    /// Without a Reply-To the sender's own address is still the reply target.
    #[test]
    fn reply_falls_back_to_the_sender() {
        let mut m = msg(1, 1, 10);
        m.from_addr = "ada@example.com".into();
        assert_eq!(reply_prefill(&m).to, "ada@example.com");
    }

    #[test]
    fn thread_ids_collect_own_and_referenced_ids_once() {
        let mut a = msg(1, 1, 10);
        a.message_id = "root@x".into();
        let mut b = msg(1, 1, 11);
        b.message_id = "reply@x".into();
        b.references = "root@x parent@x".into();
        let ids = thread_ids(&[a, b]);
        assert_eq!(ids, vec!["root@x", "reply@x", "parent@x"]);
    }

    #[test]
    fn thread_ids_skips_messages_with_no_headers() {
        // Nothing to search the cache by — better an empty list than a lookup
        // that would match every message with an empty message_id.
        assert!(thread_ids(&[msg(1, 1, 10)]).is_empty());
    }

    #[test]
    fn search_pool_spans_every_folder_and_account() {
        let mut cache: HashMap<(u32, u32), Vec<Message>> = HashMap::new();
        cache.insert((1, 10), vec![msg(1, 10, 1), msg(1, 10, 2)]); // acct 1, inbox
        cache.insert((1, 11), vec![msg(1, 11, 3)]); // acct 1, archive
        cache.insert((2, 20), vec![msg(2, 20, 4), msg(2, 20, 5)]); // acct 2, inbox

        let pool = build_search_pool(&cache);
        assert_eq!(pool.len(), 5, "pool must include every folder's messages");

        let folders: std::collections::HashSet<(u32, u32)> =
            pool.iter().map(|m| (m.account_id, m.folder_id)).collect();
        assert_eq!(folders.len(), 3, "pool must span all three folders");
    }

    #[test]
    fn search_pool_is_empty_when_nothing_indexed() {
        assert!(build_search_pool(&HashMap::new()).is_empty());
    }

    /// The credits metafiles are read at build time and shown in About, so a
    /// line that stops parsing loses a real person from the list silently.
    #[test]
    fn credits_files_parse() {
        for file in [CONTRIBUTORS, TRANSLATORS] {
            let rows = credits(file);
            let lines = file
                .lines()
                .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
                .count();
            assert_eq!(rows.len(), lines, "every non-comment line must parse");
            for (name, handle, _) in rows {
                assert!(!name.is_empty() && !handle.is_empty(), "{name}/{handle}");
                assert!(
                    !handle.contains(char::is_whitespace),
                    "{handle} is not a GitHub handle"
                );
            }
        }
    }

    /// Translators carry their languages after the dash; contributors carry
    /// nothing after the handle.
    #[test]
    fn credits_notes_are_translator_languages() {
        assert!(credits(CONTRIBUTORS).iter().all(|(_, _, note)| note.is_empty()));
        assert!(credits(TRANSLATORS).iter().all(|(_, _, note)| !note.is_empty()));
    }
}

