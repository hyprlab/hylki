//! The root component's state.

use std::collections::{HashMap, HashSet};

use relm4::actions::RelmAction;
use relm4::prelude::*;
use tokio::sync::mpsc::UnboundedSender;

use crate::config::{self, AccountConfig};
use crate::models::{Account, Attachment, Folder, Message};
use crate::ui::accounts::AccountsWindow;
use crate::ui::attachment_drawer::AttachmentDrawer;
use crate::ui::attachments_gallery::AttachmentsGallery;
use crate::ui::compose::Compose;
use crate::ui::contacts_page::ContactsPage;
use crate::ui::message_list::{MessageList, QuickFilters};
use crate::ui::message_view::MessageView;
use crate::ui::notifications::NotificationCenter;
use crate::ui::preferences::Preferences;
use crate::ui::sidebar::Sidebar;
use crate::worker::MailRequest;

use super::attachments::AttachmentTarget;
use super::compose_host::{ComposeHost, ReaderCompose};
use super::conversation::BodyKey;
use super::filter_run::FilterRun;
use super::hand_off::{FileHandOff, HandOffFiles};
use super::list_header::ListHeaderWidgets;
use super::msg::AppMsg;
use super::popout::PopOut;
use super::reader_toolbar::ReaderToolbarWidgets;
use super::send_morph::{MorphPending, Provisional, SentFile};
use super::tags::TagScan;
use super::transfer::{Transfer, TransferTally};
use super::types::{FolderMessages, SelectedFolder, UnifiedView};
use super::undo::UndoEntry;
use super::{RedoAction, UndoAction};

/// A pending tag change: (account, folder path, uid, lower-cased keyword).
type PendingTagKey = (u32, String, u32, String);

/// One row of the tray's mail section: (account, message id, sender,
/// subject).
type TrayMailRow = (u32, u32, String, String);

/// The root component's state: the accounts and their workers, what the
/// list and the reader show, the session caches, every setting the app
/// applies, and the child components.
pub(crate) struct AppModel {
    /// One mail worker per account (account_id → request sender).
    pub(super) workers: HashMap<u32, UnboundedSender<MailRequest>>,
    /// `mid:` lookups out at the servers (#130): the Message-ID and how many
    /// accounts have yet to answer, so a miss is reported once, at the end.
    pub(super) mid_searches: HashMap<String, usize>,
    /// The tag finder's scan in progress (Settings → Tags → Find Tags…):
    /// how many accounts have yet to answer, and what the others found.
    pub(super) tag_scan: Option<TagScan>,
    /// Counts scans, so a stale timeout cannot end a later one.
    pub(super) tag_scan_gen: u32,
    pub(super) config: Vec<AccountConfig>,
    /// Demo mode's stand-in account configs (the sample accounts live only
    /// at the backend): what the Accounts panel edits, kept in memory so a
    /// changed color, emoji or picture shows without touching disk.
    pub(super) demo_config: Vec<AccountConfig>,
    pub(super) window: adw::ApplicationWindow,
    pub(super) prefs: Option<Controller<Preferences>>,
    pub(super) accounts_win: Option<Controller<AccountsWindow>>,
    /// What the accounts panel was built over (its init, printed), to tell
    /// whether a reopened Settings window needs a fresh one.
    pub(super) accounts_seed: Option<String>,
    /// Standalone compose windows (multiple allowed at once). Pruned as they close.
    pub(super) composers: Vec<ComposeHost>,
    /// The reader's inline reply/forward composer, if open.
    pub(super) reader_compose: Option<ReaderCompose>,
    /// The stage over the reading pane that turns a sent split reply into
    /// its card in the conversation (send_morph.rs).
    pub(super) send_morph: crate::ui::send_morph::SendMorph,
    /// A reply just sent from the split pane, with the card it gets in the
    /// conversation and whether it moves there: (composer id, message,
    /// animated), until that composer closes.
    pub(super) arriving: Option<(u32, Message, bool)>,
    /// A sent reply whose morph is under way, until the pane changes under
    /// the stage.
    pub(super) morph_pending: Option<MorphPending>,
    /// The app's own input, for the morph's steps that answer later.
    pub(super) self_sender: relm4::Sender<AppMsg>,
    /// The cards standing in for replies sent from here until their copies
    /// are read back from Sent, by (account, id).
    pub(super) provisionals: HashMap<(u32, u32), Provisional>,
    /// The files each reply sent from here went with, by its card, which
    /// lists them until its copy's own attachments are read back.
    pub(super) sent_files: HashMap<(u32, u32), Vec<SentFile>>,
    /// Superseded inline composers still finishing a save-if-dirty before closing.
    pub(super) draining_composers: Vec<(u32, Controller<Compose>)>,
    /// SlideDown revealer under the reader toolbar that hosts the inline pane.
    pub(super) reader_compose_revealer: gtk::Revealer,
    /// The cards files dragged onto the window bring up when no composer is
    /// open in it, each starting a new message (#293).
    pub(super) window_drop_zones: Option<std::rc::Rc<crate::ui::drop_zones::DropZones>>,
    /// Split-reply slot (#86): a reply slides down from the pane's top and
    /// the message(s) stay below it, visible and interactive.
    pub(super) reader_split_top: gtk::Revealer,
    /// The same slot beneath the reader (#212): the reply slides up from the
    /// pane's bottom edge when the reading order puts the newest message
    /// there, so the editor continues the conversation where it ends. Only
    /// one of the two slots is ever occupied.
    pub(super) reader_split_bottom: gtk::Revealer,
    /// The vertical Paned dividing the split reply (start child, the slot
    /// above) from the reader (end child). A Paned allocates by divider
    /// position, so the composer holds the height it was given — a big paste
    /// can't push it down over the messages — and its divider is the grab
    /// handle that resizes the panel. Hidden slot = hidden divider.
    pub(super) reader_split: gtk::Paned,
    /// The reader's own Paned, nested as `reader_split`'s end child: the
    /// reader (start child) over the bottom slot (end child). A second Paned
    /// rather than swapping children, so the reader is never reparented and
    /// the bottom slot has a divider of its own to be sized by.
    pub(super) reader_split_lower: gtk::Paned,
    /// The running slide (open or close) of the split reply — held so the
    /// opposite motion can skip it to its end instead of fighting it over
    /// the divider.
    pub(super) split_close_anim: std::rc::Rc<std::cell::RefCell<Option<adw::TimedAnimation>>>,
    /// The reader pane's own header bar: hidden while a split reply hosts,
    /// since it would show a second set of window decorations mid-window and
    /// spend vertical space the split needs.
    pub(super) reader_header: std::cell::OnceCell<adw::HeaderBar>,
    /// Where Send says what it is doing, with Undo while it can (#434).
    pub(super) toasts: std::cell::OnceCell<adw::ToastOverlay>,
    /// Messages on their way out (#434), shared with the quit paths, which
    /// park any still waiting out the Undo Send delay in the Outbox.
    pub(super) outgoing: super::outgoing::OutgoingList,
    /// The last ticket handed to a send; 0 is never used.
    pub(super) send_ticket: u64,
    /// Same idea over the contacts view's detail pane: composing from a
    /// contact slides down right there instead of yanking the mail view back.
    pub(super) contacts_compose_revealer: gtk::Revealer,
    /// Monotonic id source for composers.
    pub(super) next_compose_id: u32,
    pub(super) menu: gtk::gio::Menu,
    /// The burger menu's help section, rebuilt when Console mode toggles.
    pub(super) help_menu: gtk::gio::Menu,
    /// Whether the status bar is down, so the menu offers to hide it (#294).
    pub(super) status_bar_shown: bool,
    /// All known accounts, ordered by id.
    pub(super) accounts: Vec<Account>,
    /// account_id → that account's folders.
    pub(super) folders: HashMap<u32, Vec<Folder>>,
    /// Preferred sidebar account order (by email).
    pub(super) account_order: Vec<String>,
    /// Accounts whose folder list is collapsed in the sidebar (by email).
    pub(super) collapsed: Vec<String>,
    /// Accounts whose custom-folders section is expanded in the sidebar (by email).
    pub(super) folders_expanded: Vec<String>,
    /// Collapsed folder-tree nodes ("email\tpath") — the sidebar's custom
    /// folders render as a collapsible hierarchy (#51).
    pub(super) tree_collapsed: Vec<String>,
    /// The folders the user has put in order in the sidebar ("email\tpath"):
    /// each sorts among its siblings by its place in this list.
    pub(super) folder_order: Vec<String>,
    pub(super) selected: Option<SelectedFolder>,
    /// Attachments of the currently-open message (shown in the drawer).
    pub(super) attachments: Vec<Attachment>,
    /// True while the current message's attachments are downloading.
    pub(super) attachments_loading: bool,
    /// The reader header's right-hand actions are collapsed into the overflow menu
    /// (pane squeezed under READER_ACTIONS_BREAKPOINT).
    pub(super) reader_actions_collapsed: bool,
    /// The pane is too narrow for the left group beside the overflow button
    /// and the window controls at full size: the buttons draw narrower.
    pub(super) reader_compact: bool,
    /// The right group stays in the ⋯ menu at every width.
    pub(super) toolbar_fold_right: bool,
    /// The collapsed header's ⋯ button — the anchor its menu pops from.
    pub(super) reader_overflow_btn: gtk::Button,
    /// The reader header's layout: which buttons, which side, what order
    /// (Settings → Appearance → Toolbar).
    pub(super) reader_toolbar: config::ReaderToolbar,
    /// The header's action buttons and their breakpoint, for re-packing when
    /// the layout changes. Set once after the view is built.
    pub(super) reader_toolbar_widgets: std::cell::OnceCell<ReaderToolbarWidgets>,
    /// The toolbar's tag button (#71) — the anchor its menu pops from.
    pub(super) reader_tag_btn: gtk::Button,
    /// The toolbar's Snooze button (#527), the anchor its menu pops from.
    pub(super) reader_snooze_btn: gtk::Button,
    /// The reader toolbar's Move To… button (#164): the folder picker
    /// anchors to it.
    pub(super) reader_move_btn: gtk::Button,
    /// Cache of fetched attachments, keyed by (account_id, message_id), so
    /// revisiting a message doesn't re-download them. Byte-bounded — raw
    /// attachment bytes for every message ever opened added up to hundreds of
    /// megabytes over a long session (issue #106).
    pub(super) attachment_cache: crate::ram_cache::RamCache<BodyKey, Vec<Attachment>>,
    /// The attachment lightbox over the whole window (drawer previews and
    /// card attachment rows).
    pub(super) lightbox: Controller<crate::ui::lightbox::Lightbox>,
    /// True when a unified view is active (no single folder): the folders
    /// `unified_view` names, merged into one list.
    pub(super) unified: bool,
    /// Which folders the unified view merges: every account's Inbox for All
    /// Inboxes, its Starred / Sent / Drafts for those rows, or every
    /// filter rule's folder for the Filtered Folders row.
    pub(super) unified_view: UnifiedView,
    /// The one account the unified view is narrowed to: an account's row
    /// under the unified Starred row, for one with no Starred folder (#482).
    pub(super) unified_account: Option<u32>,
    /// (account, folder) → that folder's latest messages, for the unified
    /// view's merge.
    pub(super) unified_slices: HashMap<(u32, u32), Vec<Message>>,
    /// Accounts whose inbox has been requested for the unified view since
    /// launch (the cache-primed slices still need their catch-up sync).
    pub(super) unified_boot_requested: HashSet<u32>,
    /// Accounts whose sent mail has been asked for this run (#236).
    pub(super) sent_index_requested: HashSet<u32>,
    /// (account_id, folder_id) → last-seen message list, shown instantly on
    /// revisit while a fresh sync runs in the background. Only folders
    /// listed whole this run: a cross-folder search reads the rest from disk.
    pub(super) message_cache: HashMap<(u32, u32), FolderMessages>,
    /// Bumped as a search starts and ends, so a pool read from disk for a
    /// search already over is dropped (#472).
    pub(super) search_gen: u64,
    /// The search of the servers' text under way (#509).
    pub(super) server_search: super::text_search::ServerSearch,
    /// (account_id, folder_id) whose background backfill has fully finished, so the
    /// message list knows no more rows will stream in for them.
    pub(super) indexed_folders: HashSet<(u32, u32)>,
    /// Fetched bodies by (account, folder, UID) (see [`body_key`]), so
    /// reopening a message renders instantly with no loading spinner. The
    /// folder is part of the key because a UID is unique only within its
    /// folder: keyed by account and UID alone, a Sent message's body filed by
    /// the prefetch replaced an Inbox message's with the same UID, and the
    /// Inbox message then opened showing the other one's text. Byte-bounded: the background prefetch
    /// feeds this on every folder sync, and unbounded it grew past a gigabyte
    /// on a long-running session (issue #106) — evicted bodies re-read from the
    /// SQLite cache in a blink.
    pub(super) body_cache: crate::ram_cache::RamCache<BodyKey, String>,
    /// Sender-authentication verdicts, keyed like `body_cache` (account,
    /// folder, UID). Prefetch delivers
    /// these well before a message is opened, and opening one renders from the
    /// in-memory body cache without a worker round-trip — so the verdict has to
    /// be held here or it would be lost by the time the reader needs it.
    pub(super) sender_cache: HashMap<BodyKey, Box<crate::models::SenderCheck>>,
    /// (account_id, folder_id) → server-side unread count, accurate beyond the
    /// loaded window (from IMAP STATUS/SEARCH). Drives the sidebar badges.
    pub(super) folder_unread: HashMap<(u32, u32), u32>,
    /// Read/unread changes sent to a worker and not stored yet, keyed by
    /// (account, folder path, uid) → (seen, when sent). The worker serves
    /// one request at a time, so a folder list or unread count it fetched
    /// ahead of the STORE still shows the old state; while an entry is
    /// young the app's own state for that message and folder wins over it.
    pub(super) pending_seen: HashMap<(u32, String, u32), (bool, std::time::Instant)>,
    /// (account, folder path, uid, lower-cased keyword) → (on, held until,
    /// keyword): the same for tags. A folder list fetched ahead of the STORE took a
    /// tag just set off the row until the next sync put it back.
    pub(super) pending_tags: HashMap<PendingTagKey, (bool, std::time::Instant, String)>,
    /// The messages a reply or forward on its way out marked as answered
    /// ahead of time (#454), by send ticket: (account, folder id, uid,
    /// forwarded). Undone or failed, the marks come off again.
    pub(super) pending_answers: HashMap<u64, Vec<(u32, u32, u32, bool)>>,
    /// Folders (account, folder) whose server unread count arrived while a
    /// read mark or a move was on its way and was set aside: synced again
    /// once those are stored. The count may also have been the only word of
    /// new mail, so a fresh count alone is not enough (#336).
    pub(super) dropped_unread: std::collections::HashSet<(u32, u32)>,
    /// Mail taken out of a folder whose move the worker has not reached
    /// yet, keyed by (account, folder path, uid) → when it was sent. A list
    /// the worker fetched ahead of the move still holds it, and putting it
    /// back on screen made deleted mail come back, to be deleted again for
    /// a request that had already run (#255). Filled by [`Self::send_to`],
    /// which every such request goes through.
    pub(super) pending_moves: std::cell::RefCell<HashMap<(u32, String, u32), std::time::Instant>>,
    /// Messages on their way to another account (#265), by token.
    pub(super) transfers: HashMap<u64, Transfer>,
    pub(super) next_transfer: u64,
    /// Ids for messages opened from files (#398), counting down from the
    /// top so they never meet a server's UIDs.
    pub(super) next_local_id: u32,
    /// The `ExportRaw` tokens of Save as File, with the name to offer.
    pub(super) pending_saves: HashMap<u64, String>,
    /// How the moves to other accounts under way are going, for the status
    /// line and the one report at the end.
    pub(super) transfer_tally: TransferTally,
    /// The account-list split view, narrowed to icon-only width when collapsed.
    pub(super) sidebar_split: Option<adw::OverlaySplitView>,
    /// The "Hylki" title label, hidden while the sidebar is collapsed.
    pub(super) app_title: Option<gtk::Label>,
    /// Sidebar header. In the icon-only rail its window-control buttons are
    /// hidden so the header stops forcing a minimum width wider than the rail.
    pub(super) sidebar_header: Option<adw::HeaderBar>,
    /// The main-menu button, which moves into the header's centre (the title
    /// slot) while the sidebar is a rail.
    pub(super) sidebar_menu: Option<gtk::MenuButton>,
    /// The sidebar header's Refresh button — top-left, across from the menu —
    /// shown expanded and in the peek. The rail has no header room for it, so
    /// there the sidebar stacks its own refresh directly under the menu.
    pub(super) sidebar_refresh: gtk::Button,
    /// The icon/spinner stack inside it, switched while any account syncs.
    pub(super) sidebar_refresh_stack: gtk::Stack,
    pub(super) sidebar_refresh_spinner: gtk::Spinner,
    /// Whether the sidebar is in icon-only (collapsed) mode.
    pub(super) sidebar_collapsed: bool,
    /// The narrow-window breakpoint is currently applied (window is too narrow
    /// for the expanded sidebar + a full-width actions palette — e.g. tiled to
    /// half of a 1920px screen).
    pub(super) auto_rail: bool,
    /// The rail's current on-screen state — the user's choice OR'd with the
    /// breakpoint, tracked so apply/unapply only animate real changes.
    pub(super) rail_active: bool,
    /// While narrow: the sidebar is temporarily expanded as an overlay floating
    /// above the panes (so the list and reader keep their widths).
    pub(super) sidebar_peek: bool,
    /// True while set_sidebar_peek is mutating the split view, so the
    /// show-sidebar notify (the scrim-dismiss detector) ignores the storm of
    /// notifies our own transition emits — collapsing the split auto-hides
    /// the sidebar, which read as an instant dismissal and closed every peek
    /// the moment it opened.
    pub(super) peek_transition: std::rc::Rc<std::cell::Cell<bool>>,
    /// The peek panel's own split view: always collapsed, nested in the
    /// account split's content slot, so its overlay sidebar slides out from
    /// the rail's right edge over the panes while the docked rail itself is
    /// never touched — not collapsed, not re-laid, not snapshotted.
    pub(super) peek_split: Option<adw::OverlaySplitView>,
    /// The icon/spinner stack in the peek panel's own Refresh button.
    pub(super) peek_refresh_stack: gtk::Stack,
    pub(super) peek_refresh_spinner: gtk::Spinner,
    /// Preference: hovering the icon rail opens the peek by itself.
    pub(super) sidebar_hover_expand: bool,
    /// Preference: accounts, folders and sections reopen as they were left.
    pub(super) remember_sidebar: bool,
    /// Preference: the sidebar reopens in the icon rail if left there.
    pub(super) remember_rail: bool,
    /// The sections' open state, as the sidebar last reported it,
    /// persisted with the rest of the sidebar layout.
    pub(super) unified_expanded: bool,
    pub(super) filtered_expanded: bool,
    pub(super) tags_expanded: bool,
    pub(super) starred_expanded: bool,
    pub(super) sent_expanded: bool,
    pub(super) drafts_expanded: bool,
    pub(super) archive_expanded: bool,
    pub(super) junk_expanded: bool,
    pub(super) trash_expanded: bool,
    pub(super) snoozed_expanded: bool,
    /// Favorite folders (#367), as "email\tpath" entries in the order they
    /// were added, and whether their list is open.
    pub(super) favorites: Vec<String>,
    pub(super) favorites_expanded: bool,
    pub(super) smart_expanded: bool,
    pub(super) contacts_expanded: bool,
    /// The accounts and books folded under the sidebar's Contacts row.
    pub(super) contacts_folded: Vec<String>,
    /// Account emails whose own Filtered Folders / Tags sections are open.
    pub(super) filtered_expanded_accounts: Vec<String>,
    pub(super) tags_expanded_accounts: Vec<String>,
    /// Preference: the icon rail marks unread mail with a dot, not a count.
    pub(super) rail_dots: bool,
    /// Preference: unread counts are accented only for mail that came in
    /// since the folder was last looked at (#343).
    pub(super) seen_counts: bool,
    /// Each folder's unread count when last looked at, keyed as in
    /// state.toml ("address␟path").
    pub(super) unread_seen: std::cell::RefCell<std::collections::BTreeMap<String, u32>>,
    /// Preference: the sections the icon rail folds up when the sidebar
    /// collapses.
    pub(super) rail_fold: config::RailFold,
    /// The app chrome's theme preference (follow system / light / dark).
    pub(super) app_theme: config::AppTheme,
    /// The app's text size in percent of the desktop's (#267).
    pub(super) text_scale: u32,
    /// The appearance theme: a bundled palette's id, or "system" for the
    /// stock GNOME colors (see `theme.rs`).
    pub(super) theme: String,
    /// Held so the in-flight collapse/expand width animation isn't dropped.
    pub(super) sidebar_anim: Option<adw::TimedAnimation>,
    pub(super) current: Option<Message>,
    /// Sender addresses allowed to auto-load remote content (lowercased).
    pub(super) allowed_senders: Vec<String>,
    /// The mailing lists left through the reader's Unsubscribe button
    /// (unsubscribed.toml), so a later message from one says so.
    pub(super) unsubscribed: Vec<config::UnsubscribedList>,
    /// The meeting invitations answered through the reader's buttons
    /// (invites.toml, #223), so a meeting opened again says where it
    /// stands.
    pub(super) invite_answers: Vec<config::InviteAnswer>,
    /// Whether remote content is auto-loaded for every new message.
    pub(super) auto_remote_content: bool,
    /// Whether the next messages' remote content is fetched ahead (#474).
    pub(super) remote_ahead: bool,
    /// Whether the blocked-remote-content banner is shown at all. Hiding it changes nothing about what
    /// is blocked — only whether the reader says so.
    pub(super) show_remote_banner: bool,
    /// Whether a "Check this sender" verdict gets the red banner over the
    /// message. A failed check gets it regardless.
    pub(super) show_spoof_banner: bool,
    /// Answering requests for read receipts (#520), and the requests
    /// already answered or ignored here (receipts.toml).
    pub(super) receipts: config::ReceiptPolicy,
    pub(super) receipts_answered: Vec<config::ReceiptAnswer>,
    /// New messages ask for a read receipt (#520).
    pub(super) request_receipts: bool,
    /// The sent mail waiting for a read receipt and the receipts that came
    /// back (read_by.toml), where the receipt mail goes, and the receipt
    /// mail already filed this session.
    pub(super) read_by: config::ReadByFile,
    pub(super) returned_receipts: config::ReturnedReceipts,
    pub(super) notify_receipts: bool,
    pub(super) receipts_filed: HashSet<BodyKey>,
    /// Addresses/domains whose incoming inbox mail is auto-deleted (lowercased).
    pub(super) blacklist: Vec<String>,
    /// Seconds the message-list actions palette stays open after the cursor leaves.
    pub(super) palette_collapse_secs: u64,
    /// Seconds a message card's actions palette stays open — the cards' own,
    /// separate from the list's above.
    pub(super) card_palette_collapse_secs: u64,
    /// Whether to load sender avatars from Gravatar.
    pub(super) gravatar: bool,
    /// Whether the colored avatars are drawn at all (#29).
    pub(super) avatars: bool,
    /// Whether tagged rows wear their tag's color (PR #383).
    pub(super) tag_row_tint: bool,
    /// The address book new contacts go to (empty: automatic).
    pub(super) contact_book: String,
    /// Whether the mail you sent wears its mailbox's face rather than the
    /// circle any other sender would get (#189).
    pub(super) own_mailbox_face: bool,
    /// Whether a sender's site icon may fill their circle (#30).
    pub(super) sender_logos: bool,
    /// How dates are written, and on what clock (#32).
    pub(super) date_style: crate::config::DateStyle,
    pub(super) clock_style: crate::config::ClockStyle,
    /// Seconds between automatic mail checks (0 = manual only).
    pub(super) fetch_interval_secs: u64,
    /// Whether IMAP IDLE push is enabled.
    pub(super) push: bool,
    /// Whether a metered connection holds back the work done ahead of the
    /// user (#442).
    pub(super) metered_save_data: bool,
    /// Whether desktop notifications (new mail, error alerts) are posted.
    pub(super) notifications_enabled: bool,
    /// Whether new-mail notifications may name the sender and subject.
    pub(super) notification_content: bool,
    /// Which action buttons a new-mail notification carries (#244).
    pub(super) notification_buttons: config::NotificationButtons,
    /// Whether the sidebar's footer shows the "Attachments" row.
    pub(super) show_attachments: bool,
    /// Whether the settings window opens on Accounts (vs Preferences).
    pub(super) settings_open_accounts: bool,
    /// The Settings category last shown this session, to reopen on.
    pub(super) last_settings_page: Option<String>,
    /// The list header's count text ("N" / "N of M"), from the message list.
    pub(super) list_count: String,
    /// Lines of preview text per message-list row (1–3).
    pub(super) preview_lines: u32,
    /// The message list's layout setting (#334).
    pub(super) list_layout: config::ListLayout,
    /// The columns of a single-line list, in order (#334).
    pub(super) list_columns: Vec<config::ListColumn>,
    /// Headings over them, which sort the list when clicked (#334).
    pub(super) list_headings: bool,
    /// A single line too wide for the pane scrolls rather than fitting its
    /// columns (#443).
    pub(super) list_column_scroll: bool,
    /// Date headings over a list sorted by date (#435).
    pub(super) list_group_by_date: bool,
    /// A card's pills on a line under its text (#374).
    pub(super) list_pills_below: bool,
    /// The list's search field stays open (#446).
    pub(super) list_search_open: bool,
    /// Contacts' search field stays open.
    pub(super) contacts_search_open: bool,
    /// Saved contacts go by their address-book names (#541).
    pub(super) contact_names: bool,
    /// The list's search field and bulk bar open under the rows (#496).
    pub(super) list_search_bottom: bool,
    pub(super) list_bulk_bar_bottom: bool,
    /// The widths columns were dragged to by their headings (#334).
    pub(super) list_column_widths: std::collections::HashMap<config::ListColumn, i32>,
    /// The list pane is wide enough for one line per message, which
    /// Automatic follows.
    pub(super) list_wide: bool,
    /// The keyboard-shortcut reference, while it is open — so the shortcut that
    /// opens it closes it again.
    pub(super) shortcuts_win: Option<adw::Window>,
    /// Whether Hylki keeps running with no window. Shared with the window's
    /// close handler, which has to read it without the model.
    pub(super) run_in_background: std::rc::Rc<std::cell::Cell<bool>>,
    /// Whether Hylki starts at login (background running only).
    pub(super) autostart: bool,
    /// The window opens fullscreen (#511).
    pub(super) start_fullscreen: bool,
    /// Tray icon (issue #116): the setting, the icon choice, and the running
    /// item while the setting is on.
    pub(super) tray_enabled: bool,
    pub(super) tray_icon: config::TrayIcon,
    pub(super) tray_mail: bool,
    /// What the launcher icon's badge counts (#271, #431).
    pub(super) launcher_badge: config::LauncherBadge,
    /// Inbox mail that arrived while the window was not in front (#431).
    pub(super) arrivals: crate::desktop::launcher_badge::Arrivals,
    /// The chosen app icon (an `app_icon::catalog` id); the tray draws it.
    pub(super) app_icon: String,
    /// A restart is on its way (Restart Now clicked, settle timer running).
    pub(super) restart_pending: bool,
    pub(super) tray: Option<super::tray::TrayHandle>,
    /// What the tray menu last listed (account, id, sender, subject; `None`
    /// for no section), so an unchanged list isn't re-sent over D-Bus on
    /// every unread-count push.
    pub(super) tray_mail_key: std::cell::RefCell<Option<Vec<TrayMailRow>>>,
    /// Whether single-key (modifier-free) shortcuts are enabled. The window's key
    /// controller needs to read this without the model, so it is shared: with the
    /// feature off, keystrokes must pass straight through rather than be consumed
    /// and dropped.
    pub(super) single_key: std::rc::Rc<std::cell::Cell<bool>>,
    /// Gmail's single-key map in place of Hylki's (#392), shared with the
    /// key controller like `single_key`.
    pub(super) gmail_keys: std::rc::Rc<std::cell::Cell<bool>>,
    /// Settings → System → Keep downloaded mail, in days; 0 keeps it (#394).
    pub(super) keep_downloads_days: u32,
    /// Whether messages are grouped into conversation threads.
    pub(super) threading: bool,
    /// A conversation re-render is already scheduled, so further bodies arriving
    /// in the same burst don't each queue one of their own.
    pub(super) thread_render_queued: bool,
    /// When the conversation on screen was opened. Its bodies arrive one at a
    /// time, and painting each arrival meant a stack of half-built renders
    /// flashing past; the reader holds its spinner until they are all in, or
    /// until this has been waiting [`THREAD_BODY_WAIT`].
    pub(super) thread_opened_at: Option<std::time::Instant>,
    /// A cross-folder conversation lookup is still outstanding. Its answer can
    /// add messages (and bodies) to what is on screen, so the reader waits for
    /// it rather than painting a conversation it is about to paint again.
    pub(super) thread_related_pending: bool,
    /// Whether the conversation on screen has had its one paint yet. Until it
    /// has, the reader shows its spinner — opening a thread is a wait, however
    /// short, and showing the previous message meanwhile reads as a glitch.
    pub(super) thread_painted: bool,
    /// Conversations already assembled, keyed by the message that opens them.
    /// Returning to a thread paints from here rather than re-running the
    /// cross-folder lookup and re-gathering bodies: the wait belongs to the
    /// first open, not to every one. Bounded — a conversation holds its
    /// messages' bodies, which are not small. Keyed by the head's folder and
    /// UID: a UID alone names a different message in every folder (#317).
    pub(super) thread_cache: HashMap<BodyKey, Vec<Message>>,
    /// Attachments being deleted from the server (#289), shown as such in
    /// the drawer and the gallery until the worker answers.
    pub(super) deleting_attachments: Vec<AttachmentTarget>,
    /// Showcase only: answer the next delete question with Delete.
    pub(super) showcase_confirm_delete: bool,
    /// Insertion order for `thread_cache`, oldest first.
    pub(super) thread_cache_order: Vec<BodyKey>,
    /// Which conversation `current_thread` is, for storing it back.
    pub(super) thread_key: Option<BodyKey>,
    /// Whether conversation threads start expanded in the message list.
    pub(super) threads_expanded: bool,
    /// Reading pane shows conversations newest-message-first.
    pub(super) thread_newest_first: bool,
    /// Which messages of a conversation open folded (#326).
    pub(super) fold_messages: config::FoldMessages,
    /// Reader always shows the recipients line under the sender.
    pub(super) always_show_recipients: bool,
    /// The OpenPGP chip says its verdict in words (#300).
    pub(super) pgp_labels: bool,
    /// The reader's find bar opens under the message (#392).
    pub(super) reader_find_bottom: bool,
    /// Whether the sidebar offers the unified "All Inboxes" section at all.
    pub(super) show_unified_pref: bool,
    /// Whether the collapsed "All Inboxes" row wears its total-unread chip.
    pub(super) unified_chips: config::UnifiedChips,
    /// Where the Filters, Tags, Favorites and Contacts sections sit
    /// (Settings → Sidebar), and how they are drawn.
    pub(super) section_style: config::SectionStyle,
    pub(super) filtered_placement: config::SectionPlacement,
    pub(super) tags_placement: config::SectionPlacement,
    pub(super) favorites_placement: config::SectionPlacement,
    pub(super) contacts_placement: config::SectionPlacement,
    /// The unified section's Starred / Sent / Drafts rows (Settings →
    /// Sidebar → Unified).
    pub(super) unified_kinds: config::UnifiedKinds,
    /// Whether the account sections are shown at all.
    pub(super) show_accounts: bool,
    /// The main menu's "Show Accounts" check item, kept in step with the
    /// setting wherever it is changed.
    pub(super) show_accounts_action: gtk::gio::SimpleAction,
    /// Focus Mode (main menu, Ctrl+Shift+F, Settings → Appearance): the
    /// master switch and the parts it strips. Applied on top of the ordinary
    /// settings, which it never changes — off, everything comes back.
    pub(super) focus: config::FocusMode,
    /// The main menu's "Focus Mode" check item.
    pub(super) focus_action: gtk::gio::SimpleAction,
    /// The message list header's foldable controls and the ⋯ that stands in
    /// for them in Focus Mode (set in init).
    pub(super) list_header_widgets: std::cell::OnceCell<ListHeaderWidgets>,
    /// Whether the sidebar's disclosure chevrons lead their rows.
    pub(super) chevrons_left: bool,
    /// Whether a click anywhere on an account's header folds it.
    pub(super) account_click_folds: bool,
    /// What the window shows at launch (#256).
    pub(super) start_view: config::StartView,
    /// How accounts' folders are sorted, unless an account chooses
    /// (Settings → Sidebar).
    pub(super) folder_sort: config::FolderSort,
    /// Console mode offered in the status bar (Settings → System & Appearance).
    pub(super) console_mode: bool,
    /// The status bar at the top of the window, not its foot (#433).
    pub(super) status_bar_top: bool,
    /// Read-marking policy (#100).
    pub(super) read_mark: config::ReadMark,
    /// Mail filter rules (#47), applied to inbox syncs.
    pub(super) filters: Vec<config::FilterRule>,
    /// The saved contacts' addresses, for rules on whether a sender is in
    /// Contacts (PR #384).
    pub(super) saved_addresses: crate::contacts::SavedAddresses,
    /// The list header's quick filters that are on (#441).
    pub(super) quick_filters: QuickFilters,
    /// Tags (#71): a name and color per keyword.
    pub(super) tags: Vec<config::Tag>,
    /// The tag view, if that is the view — alongside `unified` and
    /// `selected`, never with: the account it is scoped to (`None` spans
    /// every account) and the keyword (`None` for every tag at once).
    pub(super) tag_view: Option<(Option<u32>, Option<String>)>,
    /// Each tag view's last result, by (scope, keyword): shown the moment
    /// the view opens, while a fresh read of the index runs off the main
    /// thread and replaces it only if it differs.
    pub(super) tag_view_cache: HashMap<(Option<u32>, Option<String>), Vec<Message>>,
    /// A read of the index for the open tag view is in flight; one more
    /// asked for meanwhile runs when it lands.
    pub(super) tag_view_loading: bool,
    pub(super) tag_view_dirty: bool,
    /// Smart folders (#417): the saved searches, and the one open, by id,
    /// read and cached the way the tag view is.
    pub(super) smart_folders: Vec<config::SmartFolder>,
    pub(super) smart_view: Option<String>,
    pub(super) smart_view_cache: HashMap<String, Vec<Message>>,
    pub(super) smart_view_loading: bool,
    pub(super) smart_view_dirty: bool,
    /// The unified Starred view's read of the index (#482) is running, and
    /// another is owed when it lands.
    pub(super) unified_starred_loading: bool,
    pub(super) unified_starred_dirty: bool,
    /// The starred mail of the accounts with no Starred folder, by account,
    /// with the folder path each message sits in: kept for the session, so
    /// the unified Starred view opens at once (#482).
    pub(super) unified_starred_rows: HashMap<u32, Vec<(String, Message)>>,
    pub(super) unified_starred_due: bool,
    /// A read of the unified rows' folders into memory is due (`Preload`).
    pub(super) preload_due: bool,
    /// Unified rows still to build ahead for the message list (`PrebuildNext`).
    pub(super) prebuild_queue: Vec<UnifiedView>,
    pub(super) prebuild_due: bool,
    /// A count of the smart folders' unread mail is waiting to run, or
    /// running: syncs come in bursts, and one count covers a burst.
    pub(super) smart_counts_due: bool,
    pub(super) smart_counts_running: bool,
    /// When each account's keywords were last re-synced from the server
    /// (#166), so reopening a tag view within a few seconds does not scan
    /// every folder again.
    pub(super) keyword_sync_at: HashMap<u32, std::time::Instant>,
    /// When each folder's stars and read state were last checked against
    /// the server for an open Smart Folder or Starred view (#532), by
    /// account and path.
    pub(super) flag_sync_at: HashMap<(u32, String), std::time::Instant>,
    /// The tag colors as `.tag-<keyword>` classes, app-wide (the list's
    /// chips, the sidebar's rows, the menus' swatches).
    pub(super) tag_provider: crate::ui::DisplayCss,
    /// The on-disk index, for reads the main thread makes itself: the tag
    /// views and folding locally-kept tags into synced summaries.
    pub(super) cache: Option<crate::cache::Cache>,
    /// A manual "Apply Filters" run (#198), while one is under way.
    pub(super) filter_run: Option<FilterRun>,
    /// Inbox UIDs whose filter move has been requested but not yet observed
    /// (the message still showed up in the last sync). A sync racing the
    /// server-side move must neither re-request the move nor re-notify.
    pub(super) filter_moved: std::collections::HashMap<(u32, u32), std::collections::HashSet<u32>>,
    /// The messages filters have forwarded, so none goes out twice.
    pub(super) forwarded_log: config::ForwardedLog,
    /// Forwards waiting for the body or the files of their message.
    pub(super) auto_forwards: Vec<super::filter_forward::AutoForward>,
    /// Keys of the forwards under way (waiting or sent, not yet answered).
    pub(super) auto_forward_inflight: std::collections::HashSet<String>,
    /// Send ticket to the forward a filter sent under it.
    pub(super) auto_forward_tickets:
        std::collections::HashMap<u64, super::filter_forward::SentForward>,
    /// Folders other than an Inbox that the filters run over on their next
    /// load, (account, folder): where a forward a manual run started has
    /// just gone, so the rest of its rule can follow.
    pub(super) filter_followups: std::collections::HashSet<(u32, u32)>,
    /// Per (account, folder): the body-search hits of the last sync (#191),
    /// uid → lowercased alternatives the server found in the message. What
    /// [`apply_filters`] consults for "Message body" conditions.
    pub(super) body_hits:
        std::collections::HashMap<(u32, u32), std::collections::HashMap<u32, Vec<String>>>,
    /// Lone messages render as inset cards (#57).
    pub(super) single_message_card: bool,
    /// The message zoom this session is at (Ctrl+ / Ctrl-). Starts at
    /// `zoom_default` and is not saved: a launch begins at the setting.
    pub(super) zoom: u32,
    /// Settings → Reading: the zoom every launch starts at, in percent.
    pub(super) zoom_default: u32,
    /// Reader View: every message in the reader shown as its content alone,
    /// in the reader's own sheet (see `crate::mail::reader`). The toggle sits in
    /// the reader's subject block; the choice is remembered across runs.
    pub(super) reader_mode: bool,
    /// The Reader View switch is shown in the reader header.
    pub(super) reader_switch: bool,
    /// The Light / Dark Mode switch is shown in the reader header (PR #386).
    pub(super) theme_switch: bool,
    /// The account chip is shown in the reader header (Settings).
    pub(super) account_chip: bool,
    /// Icons come from the desktop's icon theme, not Hylki's own (Settings).
    pub(super) desktop_icons: bool,
    /// What Reader View does when a message is opened (Settings).
    pub(super) reader_default: config::ReaderDefault,
    /// Each conversation message lists its own attachments (#213).
    pub(super) card_attachments: bool,
    /// The attachment drawer beneath the reader is shown at all (#213).
    pub(super) drawer_enabled: bool,
    /// Whether conversation rows may expand into their members in the list
    /// (the row keeps its chip and chevron either way).
    pub(super) thread_expansion: bool,
    /// Whether a conversation's row speaks for the newest message anywhere in
    /// the account, the replies you sent included (#236). Off by default.
    pub(super) thread_row_newest: bool,
    /// Whether deleting a whole selected conversation asks for confirmation.
    pub(super) confirm_thread_delete: bool,
    /// Whether the current `list_selection` came from card clicks in the
    /// reader (as opposed to rows picked in the list). A lone list-row
    /// selection over an open conversation stands for the whole thread when
    /// deleting; a lone picked card never does.
    pub(super) selection_from_cards: bool,
    /// Conversation card actions hide until hovered (preference).
    pub(super) card_actions_hover: bool,
    /// With the ⋯ toggle off: card actions appear automatically on hover.
    pub(super) card_actions_auto: bool,
    /// Whether the message list rows carry an actions palette at all.
    pub(super) list_palette: bool,
    /// The list's actions palette opens on row hover (no ⋯ click).
    pub(super) list_palette_hover: bool,
    /// A message card's ⋯ opens the card menu instead of sliding its
    /// actions palette out.
    pub(super) card_palette_menu: bool,
    /// Message rows take a sideways swipe to archive / delete (#92).
    pub(super) swipe_enabled: bool,
    /// The message list's swipe-gesture sides are swapped (#swipe).
    pub(super) swipe_reversed: bool,
    /// How far a trackpad two-finger swipe has to travel to fire the action
    /// (higher = shorter swipe); mouse and touch drags are unaffected.
    pub(super) swipe_sensitivity: f64,
    /// "New message" composes inline over the reading pane (vs a window).
    pub(super) compose_inline: bool,
    /// Reply, Reply All and Forward open in the reading pane (vs a window, #295).
    pub(super) reply_inline: bool,
    pub(super) reply_fields: bool,
    /// The identity new messages are sent from (#157); empty = the open
    /// folder's account.
    pub(super) compose_default_from: String,
    pub(super) paste_plain: bool,
    /// Return starts a new paragraph in the composer; off, a new line.
    pub(super) return_paragraph: bool,
    pub(super) toolbar_expanded: bool,
    /// The font and size rich-text messages are written in; empty and 0
    /// leave them to the recipient.
    pub(super) compose_font: String,
    pub(super) compose_font_size: u32,
    /// Settings → Composing → Default zoom; 0 = the reader's (#535).
    pub(super) compose_zoom: u32,
    /// What a printed page carries besides the message (#359).
    pub(super) print_options: config::PrintOptions,
    /// New messages start as plain text (#180).
    pub(super) compose_format: crate::config::ComposeFormat,
    /// Replies and forwards to plain-text mail open in plain text (#436).
    pub(super) follow_plain: bool,
    /// Where the split reply opens in the reading pane (#212).
    pub(super) reply_position: config::ReplyPosition,
    /// Where the signature sits in a reply or forward (#237).
    pub(super) signature_position: config::SignaturePosition,
    /// A `-- ` line goes above the signature in new messages.
    pub(super) signature_dashes: bool,
    /// The line above a reply's quote, "" for the default (#528).
    pub(super) reply_header: String,
    /// Undo Send delay in seconds, 0 for off (#434).
    pub(super) undo_send_secs: u32,
    pub(super) spellcheck: bool,
    pub(super) spellcheck_langs: String,
    /// How email content is themed (message content only, not the app UI).
    pub(super) message_theme: config::MessageTheme,
    /// Set every message in the reader's own font (#56).
    pub(super) override_fonts: bool,
    /// That font, as a Pango description; empty = the interface font.
    pub(super) reader_font: String,
    /// Ignore the senders' text and background colors (#56).
    pub(super) override_colors: bool,
    /// Plain-text messages in monospace (#181), and the font ("" = the
    /// desktop's monospace font).
    pub(super) plain_monospace: bool,
    pub(super) plain_font: String,
    /// The repeating auto-fetch timer, if armed.
    pub(super) auto_fetch_source: Option<gtk::glib::SourceId>,
    /// When the last wake from sleep was handled, so the second report of
    /// the same wake is ignored.
    pub(super) last_wake: Option<std::time::Instant>,
    pub(super) notifications: Controller<NotificationCenter>,
    /// The first-run welcome wizard, alive while it's on screen.
    pub(super) welcome: Option<Controller<crate::ui::welcome::Welcome>>,
    pub(super) notify_count: usize,
    /// Accounts currently performing network activity (drives the spinner).
    pub(super) busy: HashSet<u32>,
    pub(super) sidebar: Controller<Sidebar>,
    /// A second instance of the sidebar, always expanded, for the floating
    /// peek panel. It is fed the same contents and counts as the docked one
    /// (see `sidebars_emit`) and the two mirror each other's selection.
    pub(super) peek_sidebar: Controller<Sidebar>,
    pub(super) message_list: Controller<MessageList>,
    pub(super) message_view: Controller<MessageView>,
    /// In-message attachment thumbnail drawer, docked below the reader body.
    pub(super) attachment_drawer: Controller<AttachmentDrawer>,
    pub(super) gallery: Controller<AttachmentsGallery>,
    /// The in-app contacts view behind the sidebar's Contacts row.
    pub(super) contacts_page: Controller<ContactsPage>,
    /// Whether the content area shows the contacts view.
    pub(super) showing_contacts: bool,
    /// True when the attachments gallery replaces the mail panes.
    pub(super) showing_gallery: bool,
    /// Whether the Outbox is showing instead of the mail panes.
    pub(super) showing_outbox: bool,
    /// Messages waiting to be sent, per account, as last reported by its worker.
    pub(super) outbox_by_account: HashMap<u32, Vec<crate::models::OutboxItem>>,
    /// Snoozed mail (#527), as snoozed.toml keeps it.
    pub(super) snoozed: Vec<config::Snooze>,
    /// Whether the sidebar was last given the unified Snoozed row, which
    /// it has only while something is snoozed (#542).
    pub(super) snoozed_row: std::cell::Cell<bool>,
    /// How many messages each Snoozed folder holds on the server, by
    /// (account, path), as the last recount or listing found (#542).
    pub(super) snoozed_held: HashMap<(u32, String), u32>,
    /// Snoozed mail a worker has been asked to bring back, by (account
    /// address, Message-ID), with when to ask again if no answer comes.
    pub(super) snooze_waking: HashMap<(String, String), std::time::Instant>,
    /// Snoozes an undo took back, by (account address, Message-ID), for a
    /// redo to restore (#542).
    pub(super) snooze_undone: HashMap<(String, String), config::Snooze>,
    /// The time a Snooze of the list's selection is for, while the list
    /// resolves which messages that is.
    pub(super) snooze_until: Option<i64>,
    /// The sending identities a JMAP server keeps, per account (#346):
    /// offered as From addresses beside the account's own aliases.
    pub(super) server_identities: HashMap<u32, Vec<(String, String)>>,
    /// A merge of the unified view is on its way (`queue_unified_emit`),
    /// with new slices to merge, and new thread links to hand the list.
    pub(super) unified_emit_queued: std::cell::Cell<bool>,
    pub(super) unified_dirty: std::cell::Cell<bool>,
    pub(super) thread_links_due: std::cell::Cell<bool>,
    pub(super) unified_emitted_at: std::cell::Cell<std::time::Instant>,
    /// The fingerprint of the unified view's content the list has, when it
    /// has the open view's: a merge that would hand it the same is skipped.
    pub(super) list_fp: std::cell::Cell<Option<u64>>,
    /// The open unified view's folders whose slice is not copied from the
    /// folder's cache yet: a view the list kept is shown first, and the
    /// copies (tens of thousands of messages for an Archive) made after.
    /// Until then the folder's cache stands for the slice.
    pub(super) unified_unfilled: std::collections::HashSet<(u32, u32)>,
    /// How many attachments the gallery's current query matches, carried
    /// between pages so only the first one pays for the COUNT.
    pub(super) gallery_total: u32,
    /// Messages each in-scope folder has still to have described, so the
    /// gallery's footer can show one figure for the whole scan.
    pub(super) gallery_scan_left: HashMap<(u32, String), u32>,
    /// Messages popped out into their own windows, keyed by (account, message).
    pub(super) popouts: HashMap<(u32, u32), PopOut>,
    /// The conversation currently shown in the reader (newest first), with bodies
    /// filled in as they arrive. More than one entry = conversation/thread mode.
    pub(super) current_thread: Vec<Message>,
    /// The members of the open conversation the list itself holds, as
    /// opposed to the ones the reader brought in from Sent or another
    /// folder: what archiving the conversation moves (#392).
    pub(super) listed_thread: Vec<(u32, u32)>,
    /// The next message opened takes the keyboard into the reader (Gmail's
    /// o and Enter, #392).
    pub(super) reader_focus_pending: bool,
    /// Every message currently selected (list rows or reader cards), newest
    /// report wins. Lets the toolbar's Delete act on the whole multi-selection.
    pub(super) list_selection: Vec<(u32, u32)>,
    /// The folder of each message in `list_selection` when the list made
    /// it, `(account_id, folder_id, id)`. The id is a UID, which several
    /// folders of the Filters view can share, so it is these that say which
    /// message each row is (#481). Empty for cards picked in the reader.
    pub(super) list_slots: Vec<crate::ui::message_row::Slot>,
    /// How many rows Gmail's x has checked, while it is checking them
    /// (#392): they are acted on, not opened.
    pub(super) checked: Option<usize>,
    /// A composer is waiting for the busy pointer to be drawn, and one is
    /// being built (see `AppMsg::ComposeNow`).
    pub(super) compose_waiting: bool,
    pub(super) compose_now: bool,
    /// The list's own selection as it last reported it; `list_selection`
    /// also follows the reader's cards.
    pub(super) list_heard: Vec<(u32, u32)>,
    /// Undoable actions (#200): moves, flag changes, and folder creates and
    /// renames, newest last. Unlimited: entries are a few strings each.
    /// Ctrl+Z pops one, applies it, and pushes its inverse onto `redo_stack`.
    pub(super) undo_stack: Vec<UndoEntry>,
    /// The other half of the history: what Ctrl+Shift+Z puts back. Cleared
    /// whenever a new action is recorded, as every undo history does — the
    /// branch it belonged to no longer exists.
    pub(super) redo_stack: Vec<UndoEntry>,
    /// Conversations belonging to messages a move is bringing back (#200),
    /// keyed and re-filed exactly like [`carried_bodies`]. The reader paints
    /// an assembled conversation with no lookup and no spinner; without this
    /// a restored message rebuilt its thread from scratch, which means a
    /// round trip, which means the spinner.
    pub(super) carried_threads: HashMap<(u32, String), Vec<Message>>,
    /// Bodies belonging to messages a move is bringing back (#200), by
    /// Message-ID. A move gives a message a new UID, which orphans its body in
    /// [`body_cache`] — that is keyed by the folder and UID it had. These are
    /// re-keyed onto the new ids as the folder's reload arrives, so the
    /// restored message renders from what is already here instead of blanking
    /// to a spinner while the server sends it over again. Drained on use.
    pub(super) carried_bodies: HashMap<(u32, String), String>,
    /// Per-message remote-content choices: show (true) or block (false) this
    /// one message, whatever the standing policy says. Set from the reader's
    /// menu and its banner, and kept for the session only — a decision about
    /// one message is not a decision about a sender.
    pub(super) remote_override: HashMap<(u32, u32), bool>,
    /// What the inline composer's own history can take back (#200), as it
    /// last reported: `(undo, redo)`, each `None` when that side is empty.
    /// While the composer holds keyboard focus the window's Undo and Redo
    /// are its, not the mail history's — the same split the keys already
    /// make, so the menu never offers to archive a message while someone is
    /// typing into a reply.
    pub(super) compose_history: (Option<String>, Option<String>),
    /// The burger menu's Undo/Redo section, relabelled as the stacks change.
    pub(super) undo_menu: gtk::gio::Menu,
    /// Their actions, kept so they can be greyed out when there is nothing
    /// on the stack.
    pub(super) undo_action: Option<RelmAction<UndoAction>>,
    pub(super) redo_action: Option<RelmAction<RedoAction>>,
    /// A draft whose content is being fetched before its editor opens, and
    /// whether that editor goes in the reading pane (true) or a window.
    pub(super) pending_draft: Option<(Message, bool, HandOffFiles)>,
    /// The `ExportRaw` token the pending draft's bytes come back under.
    /// While it is set, the draft waits for those and not for its body.
    pub(super) pending_draft_token: Option<u64>,
    /// A message whose body is being fetched so a reply to it can open with
    /// handed-in files (Send with Hylki → Reply to a Message…).
    pub(super) pending_reply: Option<(Message, HandOffFiles)>,
    /// A draft picker waiting for Drafts folders never listed this run
    /// (the hand-off, and the folders still to answer).
    pub(super) pending_draft_pick: Option<(FileHandOff, HashSet<(u32, u32)>)>,
    /// The template picker, waiting on these Templates folders' lists.
    pub(super) pending_template_pick: Option<HashSet<(u32, u32)>>,
    /// Settings → System → GNOME Files: what handed-in files open into and
    /// what happens over the size limit.
    pub(super) files_prefs: config::FilesPrefs,
    /// "Edit as New Message" (#232) waiting on the body, or the attachments,
    /// it is to be copied from.
    pub(super) pending_edit_as_new: Option<Message>,
    /// A forward (#240) waiting on the body, or the attachments, it is to
    /// carry, and whether it opens inline or in a window.
    pub(super) pending_forward: Option<(Message, bool)>,
    /// A reply started from a notification's button (#244), waiting for
    /// the body it quotes.
    pub(super) pending_notified_reply: Option<Message>,
    /// Settings → System → Links: which browser a link in a message opens in
    /// (#232). Empty = the desktop's default, "ask" = its app chooser,
    /// otherwise a desktop entry id.
    pub(super) link_browser: String,
    /// Outstanding bulk MoveMessages requests awaiting a worker `BulkComplete`.
    /// Outstanding server-side bulk operations; while > 0 the refresh spinner
    /// spins and the status bar narrates.
    pub(super) bulk_pending: usize,
    /// Ids handed to conversation messages pulled in from another folder,
    /// allocated downwards from the top of the range. A message's id is its UID,
    /// which is only unique within its own folder — and the reader keys bodies by
    /// (account, id), so a Sent reply sharing a UID with the Inbox message it
    /// answers would otherwise be handed the wrong body.
    pub(super) related_id_seq: u32,
    /// The id issued to each such message, keyed by where it really lives
    /// (account, folder, uid). Stable across re-opens so its body stays cached.
    pub(super) related_ids: HashMap<(u32, u32, u32), u32>,
}
