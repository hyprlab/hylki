//! Left pane: an optional "All Inboxes" (unified) row, then one section per
//! account. Each account has a coloured avatar circle + header button (chevron
//! on the right) and an animated `gtk::Revealer` holding its folder list, so
//! expanding/collapsing slides smoothly. Exactly one thing is selected across
//! the unified row and all account folder lists.
//!
//! Account order is managed in the Accounts window, not here. Collapse state is
//! owned by the app (persisted); collapse is animated locally and reported for
//! persistence WITHOUT a full rebuild (which would interrupt the animation).

use std::collections::HashMap;

use adw::prelude::*;
use relm4::prelude::*;

use crate::models::{Account, Folder, FolderKind};
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use crate::i18n::{i18n, i18n_f, i18n_noop};

/// A per-account inbox shown in the expandable "All Inboxes" sub-list.
#[derive(Clone)]
struct InboxRef {
    account_id: u32,
    folder_id: u32,
    name: String,
    path: String,
}

/// A folder a filter rule files into, listed in the "Filtered Folders"
/// section inside All Inboxes. The app resolves the rules to these; the
/// sidebar only draws them (account pill + folder name + unread chip).
#[derive(Debug, Clone)]
pub struct UnifiedFolderRef {
    pub account_id: u32,
    pub folder: Folder,
}

/// Which copy of the Filtered Folders / Tags section a widget belongs to:
/// the unified section's, or an account's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    Unified,
    Account(u32),
}

impl Slot {
    /// The account a section is scoped to; `None` for the unified one.
    fn account(self) -> Option<u32> {
        match self {
            Slot::Unified => None,
            Slot::Account(id) => Some(id),
        }
    }
}

/// One folding section's widgets (a Filtered Folders or Tags section, in
/// one slot): rebuilt with the sidebar, toggled in place between rebuilds.
struct SectionWidgets {
    revealer: gtk::Revealer,
    chevron: gtk::Image,
    toggle: gtk::Button,
    list: gtk::ListBox,
    /// The header's folded-up total chip (Filtered Folders only).
    badge: Option<gtk::Label>,
}

/// One unified row — All Inboxes, or the unified section's Starred / Sent /
/// Drafts: the header list (one selectable row), the per-account list under
/// it, and their badges.
struct KindWidgets {
    header: gtk::ListBox,
    revealer: gtk::Revealer,
    chevron: Option<gtk::Image>,
    badge: Option<gtk::Label>,
    list: gtk::ListBox,
    rows: Vec<InboxRef>,
    row_badges: HashMap<(u32, u32), gtk::Label>,
}

/// A row of the unified section: All Inboxes and the Starred / Sent /
/// Drafts rows (one folder per account), the Filtered Folders row (every
/// rule's folder) and the Tags row (every tag) — all built alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum UnifiedRow {
    Kind(FolderKind),
    Filtered,
    Tags,
}

/// What a unified row is called.
fn row_title(row: UnifiedRow) -> String {
    match row {
        UnifiedRow::Kind(kind) => kind_label(kind),
        UnifiedRow::Filtered => i18n("Filters"),
        UnifiedRow::Tags => i18n("Tags"),
    }
}

/// A unified row's icon.
fn row_icon(row: UnifiedRow) -> &'static str {
    match row {
        UnifiedRow::Kind(kind) => kind.icon(),
        UnifiedRow::Filtered => "co.hyprlab.Hylki-filter-folder-symbolic",
        UnifiedRow::Tags => "co.hyprlab.Hylki-tag-outline-symbolic",
    }
}

/// The message a unified row's header sends when selected.
fn select_msg(row: UnifiedRow) -> SidebarInput {
    match row {
        UnifiedRow::Kind(FolderKind::Inbox) => SidebarInput::UnifiedRowSelected,
        UnifiedRow::Kind(kind) => SidebarInput::UnifiedKindRowSelected(kind),
        UnifiedRow::Filtered => SidebarInput::UnifiedFilteredSelected,
        UnifiedRow::Tags => SidebarInput::UnifiedTagsSelected,
    }
}

/// The message a unified row's chevron (or double-click) sends.
fn toggle_msg(row: UnifiedRow) -> SidebarInput {
    match row {
        UnifiedRow::Kind(FolderKind::Inbox) => SidebarInput::ToggleUnifiedExpand,
        UnifiedRow::Kind(kind) => SidebarInput::ToggleKindExpand(kind),
        UnifiedRow::Filtered => SidebarInput::ToggleFilteredExpand(Slot::Unified),
        UnifiedRow::Tags => SidebarInput::ToggleTagsExpand(Slot::Unified),
    }
}

/// The chevron glyph for a section that is open or folded.
fn chevron_icon(open: bool) -> &'static str {
    if open { "co.hyprlab.Hylki-pan-down-symbolic" } else { "co.hyprlab.Hylki-pan-end-symbolic" }
}

/// What a unified row is called.
fn kind_label(kind: FolderKind) -> String {
    match kind {
        FolderKind::Inbox => i18n("Inboxes"),
        FolderKind::Starred => i18n("Starred"),
        FolderKind::Sent => i18n("Sent"),
        FolderKind::Drafts => i18n("Drafts"),
        FolderKind::Archive => i18n("Archive"),
        _ => i18n("Folder"),
    }
}

/// One account's section data, as handed to the sidebar.
#[derive(Debug, Clone)]
pub struct SectionData {
    pub account: Account,
    pub folders: Vec<Folder>,
    pub collapsed: bool,
    /// Whether this account's custom-folders section is expanded (default hidden).
    pub custom_expanded: bool,
    /// Resolved avatar background colour ("#rrggbb").
    pub color: String,
    /// Avatar emoji; when absent, account-name initials are shown.
    pub emoji: Option<String>,
    /// Avatar picture (#162), shown before the emoji and the initials.
    pub avatar: Option<std::path::PathBuf>,
    /// Custom-folder paths whose tree node is collapsed (#51).
    pub tree_collapsed: Vec<String>,
    /// The folders this account's own "Filtered Folders" section lists
    /// (every rule's destination), and whether that section is open.
    pub filtered: Vec<Folder>,
    pub filtered_expanded: bool,
    /// Whether this account has any filter rule at all: only then is
    /// "Apply Filters" worth offering on its folders (#198).
    pub has_filters: bool,
    /// Whether this account's own "Tags" section is open.
    pub tags_expanded: bool,
}

/// Initial state for the sidebar.
pub struct SidebarInit {
    /// Icon-only mode: hide all text, show just icons and account pills.
    pub collapsed: bool,
    /// A mirror instance (the floating peek panel): it never picks a view
    /// on its own when nothing is selected — the app hands it the primary
    /// sidebar's choice through `SidebarInput::MirrorSelection`.
    pub mirror: bool,
    /// The three sections' open state as last left (persisted with the
    /// sidebar layout).
    pub unified_expanded: bool,
    pub filtered_expanded: bool,
    pub tags_expanded: bool,
    /// The unified Starred / Sent / Drafts rows' account lists.
    pub starred_expanded: bool,
    pub sent_expanded: bool,
    pub drafts_expanded: bool,
    pub archive_expanded: bool,
    /// Whether the "Attachments" row is shown.
    pub show_attachments: bool,
    /// Whether the "Contacts" row is shown.
    pub show_contacts: bool,
}

/// What is currently selected in the sidebar.
#[derive(Clone, PartialEq, Debug)]
pub enum Sel {
    None,
    Unified,
    /// The attachments gallery (all inboxes).
    Attachments,
    /// The in-app contacts view.
    Contacts,
    Outbox,
    Folder(u32, String),
    /// An account's inbox selected via the "All Inboxes" sub-list.
    UnifiedInbox(u32),
    /// A filtered folder (account, path) selected via the unified
    /// "Filtered Folders" section.
    UnifiedFolder(u32, String),
    /// A folder (account, path) selected in an account's own "Filtered
    /// Folders" section.
    AccountFiltered(u32, String),
    /// A unified Starred / Sent / Drafts row: the merged view.
    UnifiedKind(FolderKind),
    /// The unified Filtered Folders row: every rule's folder, merged.
    UnifiedFiltered,
    /// The unified Tags row: every tagged message.
    UnifiedTags,
    /// An account's folder picked in a unified Starred / Sent / Drafts
    /// row's account list.
    UnifiedKindRow(FolderKind, u32),
    /// A tag (its keyword) selected in a Tags section (#71): scoped to an
    /// account when picked from that account's own section.
    Tag(Option<u32>, String),
}

pub struct Sidebar {
    /// Last sections received, in display order.
    sections: Vec<SectionData>,
    /// Whether to show the unified "All Inboxes" row.
    show_unified: bool,
    /// Whether the collapsed-up "All Inboxes" row wears its total-unread chip
    /// (while expanded, the per-inbox sub-list carries the counts instead).
    unified_chips: crate::config::UnifiedChips,
    /// Whether the disclosure chevrons LEAD their rows (Settings: Chevron
    /// placement). Off restores the classic trailing position.
    chevrons_left: bool,
    /// Icon rail: a dot for unread mail in place of the count.
    rail_dots: bool,
    /// Icon rail: the sections that fold up by themselves when the sidebar
    /// collapses (Settings → Sidebar → Icon rail).
    rail_fold: crate::config::RailFold,
    /// Per-account widgets, rebuilt on each SetContents.
    revealers: HashMap<u32, gtk::Revealer>,
    chevrons: HashMap<u32, gtk::Image>,
    folder_lists: HashMap<u32, gtk::ListBox>,
    /// Per-account list box holding just the custom (user-created) folders, shown
    /// under a collapsible "Folders" section. Selection indices into these are
    /// offset past the account's essential folders.
    custom_folder_lists: HashMap<u32, gtk::ListBox>,
    /// Per-account custom folders, in row order, for tree-visibility math.
    custom_folders: HashMap<u32, Vec<Folder>>,
    /// Collapsed tree nodes per account (paths), mirrored from SectionData and
    /// flipped locally as chevrons are clicked (#51).
    tree_collapsed: HashMap<u32, std::collections::HashSet<String>>,
    /// Each parent row's expander image, for flipping without a rebuild.
    tree_chevrons: HashMap<(u32, String), gtk::Image>,
    /// Per-account custom-row revealers (row order), for animated tree
    /// collapse/expand.
    tree_row_revealers: HashMap<u32, Vec<gtk::Revealer>>,
    /// The rebuild freeze-frame Picture and its pending lift timer.
    freeze_frame: Option<gtk::Picture>,
    freeze_timer: std::rc::Rc<std::cell::RefCell<Option<gtk::glib::SourceId>>>,
    /// The freeze-frame's fade during a rail toggle (see `rebuild_normal`).
    freeze_fade: std::rc::Rc<std::cell::RefCell<Option<adw::TimedAnimation>>>,
    /// The rail state the rows on screen were built for, so a rebuild can
    /// tell a full ↔ rail toggle from an in-place refresh.
    built_collapsed: bool,
    /// The "Folders" section revealer and its chevron, per account.
    custom_revealers: HashMap<u32, gtk::Revealer>,
    custom_chevrons: HashMap<u32, gtk::Image>,
    /// The unified-row list box (one row), when shown.
    unified_list: Option<gtk::ListBox>,
    /// The pinned footer's single list box (Contacts + Attachments rows) and
    /// the rows themselves, for selection management.
    footer_list: Option<gtk::ListBox>,
    attachments_row: Option<gtk::ListBoxRow>,
    contacts_row: Option<gtk::ListBoxRow>,
    /// Refresh/spinner stack + spinner beside the "New Message" button.
    sync_stack: Option<gtk::Stack>,
    sync_spinner: Option<gtk::Spinner>,
    /// Whether any account is syncing (drives the refresh spinner).
    busy: bool,
    /// The "Outbox" row list box (one row), while anything is queued.
    outbox_list: Option<gtk::ListBox>,
    /// Display-wide provider holding each account's avatar colour rules.
    color_provider: gtk::CssProvider,
    selected: Sel,
    /// Icon-only mode: hide all text, show just icons and account pills.
    collapsed: bool,
    /// Set while rows are selected *programmatically* (restoring after a
    /// rebuild, following the app's navigation): the list boxes' selection
    /// signals then stay silent. Otherwise a signal-driven input would be
    /// queued and judged against a selection that has since moved on — two
    /// programmatic selections in a row used to oscillate forever through
    /// the app that way.
    quiet: std::rc::Rc<std::cell::Cell<bool>>,
    /// See `SidebarInit::mirror`.
    mirror: bool,
    /// Whether the "Attachments" row is shown (in the pinned footer).
    show_attachments: bool,
    /// Whether the "Contacts" row is shown (in the pinned footer).
    show_contacts: bool,
    /// How many messages are waiting in the Outbox across all accounts. The row
    /// only exists while this is non-zero — an empty Outbox is the normal state
    /// and does not deserve permanent furniture.
    outbox_count: u32,
    /// Total unread across all inboxes, for the "All Inboxes" badge.
    unified_unread: u32,
    /// Unread badge labels by (account_id, folder_id), updated in place.
    folder_badges: HashMap<(u32, u32), gtk::Label>,
    /// The "All Inboxes" unread badge label, when shown.
    unified_badge: Option<gtk::Label>,
    /// Whether the "All Inboxes" per-account inbox sub-list is expanded.
    unified_expanded: bool,
    unified_revealer: Option<gtk::Revealer>,
    unified_chevron: Option<gtk::Image>,
    unified_inbox_list: Option<gtk::ListBox>,
    /// Per-account inboxes shown under "All Inboxes", in sub-list row order.
    unified_inboxes: Vec<InboxRef>,
    /// Unread badges for the sub-list rows, by (account_id, inbox folder_id).
    unified_inbox_badges: HashMap<(u32, u32), gtk::Label>,
    /// The filtered folders listed in the unified section, in row order.
    unified_folders: Vec<UnifiedFolderRef>,
    /// Whether the unified "Filtered Folders" section is open.
    unified_folders_expanded: bool,
    /// The rows of every Filtered Folders section (the unified one and each
    /// account's own), in row order, with the sections' widgets and badges.
    filtered_rows: HashMap<Slot, Vec<UnifiedFolderRef>>,
    filtered_sections: HashMap<Slot, SectionWidgets>,
    filtered_badges: HashMap<Slot, HashMap<(u32, u32), gtk::Label>>,
    /// Inbox unread badge overlaid on each account's avatar circle, shown only
    /// while that account's section is collapsed (its Inbox row — and normal
    /// chip — is then hidden inside the revealer). Keyed by account_id.
    account_circle_badges: HashMap<u32, gtk::Label>,
    /// The tags (#71), listed in the unified Tags section and in each
    /// account's own; the sections exist only while there is a tag.
    tags: Vec<crate::config::Tag>,
    /// Whether the unified "Tags" section is open.
    tags_expanded: bool,
    tag_sections: HashMap<Slot, SectionWidgets>,
    /// The rail's own open states: anything opened or folded while the
    /// sidebar is the icon rail lands here, the saved state untouched, so
    /// the full sidebar comes back as it was left. Cleared whenever the
    /// sidebar changes width. An item absent here shows its saved state —
    /// or folded, when "Fold up expanded items" covers it.
    rail_open: HashMap<UnifiedRow, bool>,
    rail_open_accounts: HashMap<u32, bool>,
    /// Which unified Starred / Sent / Drafts rows are shown, whether their
    /// account lists are open, and their widgets.
    unified_kinds: crate::config::UnifiedKinds,
    kind_expanded: HashMap<FolderKind, bool>,
    kind_widgets: HashMap<UnifiedRow, KindWidgets>,
    /// Whether the unified Tags section is shown at all.
    unified_tags: bool,
    /// Whether the account sections are shown at all.
    show_accounts: bool,
    /// Where the two sections sit (Settings → Sidebar).
    filtered_placement: crate::config::SectionPlacement,
    tags_placement: crate::config::SectionPlacement,
}

#[derive(Debug, Clone)]
pub enum SidebarInput {
    SetContents {
        sections: Vec<SectionData>,
        show_unified: bool,
        /// The unified Starred / Sent / Drafts rows to show (already
        /// narrowed by the app to a multi-account setup).
        unified_kinds: crate::config::UnifiedKinds,
        /// Whether the unified Tags section is shown.
        unified_tags: bool,
        /// Whether the account sections are shown at all.
        show_accounts: bool,
        unified_chips: crate::config::UnifiedChips,
        chevrons_left: bool,
        rail_dots: bool,
        rail_fold: crate::config::RailFold,
        unified_unread: u32,
        /// Filter-rule folders to list inside All Inboxes (already
        /// narrowed to the rules that opt in and the Settings switch).
        unified_folders: Vec<UnifiedFolderRef>,
        /// The tags (#71), for the Tags section.
        tags: Vec<crate::config::Tag>,
        /// Where the Filtered Folders and Tags sections are drawn.
        filtered_placement: crate::config::SectionPlacement,
        tags_placement: crate::config::SectionPlacement,
    },
    /// A tag row in a Tags section was chosen.
    TagRowSelected { slot: Slot, index: i32 },
    /// Toggle a Tags section.
    ToggleTagsExpand(Slot),
    /// A unified Starred / Sent / Drafts row was chosen (the merged view).
    UnifiedKindRowSelected(FolderKind),
    /// The unified Filtered Folders row was chosen (every rule's folder).
    UnifiedFilteredSelected,
    /// The unified Tags row was chosen (every tagged message).
    UnifiedTagsSelected,
    /// A row in a unified Starred / Sent / Drafts row's account list.
    KindSubRowSelected { kind: FolderKind, index: i32 },
    /// Toggle a unified Starred / Sent / Drafts row's account list.
    ToggleKindExpand(FolderKind),
    UnifiedRowSelected,
    /// Select the "All Inboxes" row programmatically (the tray menu's
    /// "View all unread"): the highlight follows, and the selection goes
    /// out like a click. State is set before the row, so the row-selected
    /// signal hits the already-selected guard.
    SelectUnifiedRow,
    /// The "Attachments" row was chosen.
    AttachmentsRowSelected,
    /// The "Contacts" row was clicked (it acts as a launcher, not a selection).
    ContactsRowClicked,
    FolderRowSelected { account_id: u32, index: i32 },
    /// Select a folder row programmatically — the app navigated there itself
    /// ("Go to Message" from the gallery, a notification click) and the
    /// highlight must follow. State is set before the row, so the resulting
    /// row-selected signal hits the already-selected guard and stops.
    SelectFolderRow { account_id: u32, path: String },
    /// A per-account inbox row under "All Inboxes" was chosen.
    UnifiedInboxRowSelected(i32),
    /// Toggle the "All Inboxes" per-account inbox sub-list.
    ToggleUnifiedExpand,
    /// A row in a Filtered Folders section was chosen.
    FilteredRowSelected { slot: Slot, index: i32 },
    /// Toggle a Filtered Folders section.
    ToggleFilteredExpand(Slot),
    ToggleCollapseLocal(u32),
    /// Set icon-only mode outright (the app's narrow-window breakpoint) —
    /// unlike ToggleCollapsed this never reports CollapsedChanged, so it can't
    /// overwrite the user's own persisted choice.
    SetCollapsed(bool),
    /// Move the highlight to what the app is showing, without reporting it
    /// back (the docked rail and the floating peek panel are two instances
    /// of this component; the app pushes every navigation to both).
    MirrorSelection(Sel),
    /// Toggle the collapsible "Folders" (custom folders) section for an account.
    ToggleCustomFoldersLocal(u32),
    /// Collapse/expand one folder-tree node (a parent folder's chevron, #51).
    ToggleFolderNode { account_id: u32, path: String },
    /// A custom-folder row was clicked (fires every click, selected or not):
    /// parents toggle their sub-tree without needing the caret.
    FolderRowActivated { account_id: u32, index: i32 },
    ToggleCollapsed,
    /// Show/hide the "Attachments" row in the pinned footer.
    SetAttachmentsRow(bool),
    /// Show/hide the "Contacts" row in the pinned footer.
    SetContactsRow(bool),
    /// Whether any account is syncing — spins the refresh button.
    SetBusy(bool),
    /// How many messages are waiting to be sent; 0 hides the Outbox row.
    SetOutboxCount(u32),
    /// The "Outbox" row was chosen.
    OutboxRowSelected,
    /// Update unread badges in place without rebuilding the sidebar.
    SetUnread {
        folders: HashMap<(u32, u32), u32>,
        unified: u32,
    },
    /// A message drag was dropped on a folder row.
    DropOnFolder { account_id: u32, path: String, payload: String },
    /// A message drag has hovered a collapsed account long enough — expand it.
    ExpandForDrop(u32),
}

#[derive(Debug)]
pub enum SidebarOutput {
    /// The unified sections' open state changed (a click, or the rail's
    /// fold-up) — for persistence.
    SectionsOpen {
        all_inboxes: bool,
        filtered: bool,
        tags: bool,
        starred: bool,
        sent: bool,
        drafts: bool,
        archive: bool,
    },
    /// A unified Starred / Sent / Drafts row was chosen: every account's
    /// folder of that kind, merged.
    UnifiedKindSelected(FolderKind),
    /// The unified Filtered Folders row: every rule's folder, merged.
    UnifiedFilteredSelected,
    /// The unified Tags row: every tagged message.
    UnifiedTagsSelected,
    /// The user toggled an account's own Filtered Folders / Tags section.
    ToggleAccountFiltered(u32),
    ToggleAccountTags(u32),
    /// The "New message" row at the top of the sidebar.
    ComposeRequested,
    /// The refresh button beside it.
    RefreshRequested,
    /// Long-press on the rail's refresh button: reveal the status bar.
    StatusBarRequested,
    /// Right-click on the Contacts row: open the GNOME Contacts app.
    OpenGnomeContacts,
    /// A folder-tree node was collapsed or expanded (#51) — for persistence.
    FolderNodeCollapsed { account_id: u32, path: String, collapsed: bool },
    /// A folder was dropped onto a new parent ("" = the account's top level).
    MoveFolder { account_id: u32, path: String, dest: String },
    UnifiedSelected,
    /// A tag was selected (#71): its keyword, and the account it is scoped
    /// to (an account's own Tags section) or `None` for every account.
    TagSelected { keyword: String, account: Option<u32> },
    /// The attachments gallery was selected.
    AttachmentsSelected,
    /// The "Contacts" row was clicked — open the contacts browser.
    ContactsClicked,
    OutboxSelected,
    FolderSelected {
        account_id: u32,
        folder_id: u32,
        name: String,
        path: String,
    },
    ToggleCollapse(u32),
    /// The user toggled the collapsible custom-folders section for an account.
    ToggleCustomFolders(u32),
    /// The user toggled icon-only mode; `true` means collapsed.
    CollapsedChanged(bool),
    /// The empty-state "Add first account" button was clicked.
    AddAccount,
    /// A right-click context-menu action from a sidebar item.
    Context(CtxAction),
    /// Messages (identified by ids) were dropped on a folder to move them there.
    /// `items` is the whole dragged selection as (account, folder, uid, id) — it
    /// may include messages from other accounts when the drag started in the
    /// unified inbox; the app filters and reports those (#23).
    MoveMessages {
        dest_account: u32,
        dest: String,
        items: Vec<(u32, u32, u32, u32)>,
    },
}

/// Actions offered by sidebar right-click menus.
#[derive(Debug, Clone)]
pub enum CtxAction {
    MarkFolderRead { account_id: u32, folder_id: u32 },
    RefreshFolder { account_id: u32, folder_id: u32 },
    MarkAllInboxesRead,
    RefreshAllInboxes,
    /// Open Settings → Accounts with this account's editor up.
    OpenAccountSettings(u32),
    RemoveAccount(u32),
    /// Create a new custom folder under this account.
    NewFolder(u32),
    /// Delete a custom folder (its contents are moved to Trash first).
    DeleteFolder { account_id: u32, name: String, path: String },
    /// Rename a custom folder (its leaf name; children follow via RENAME).
    RenameFolder { account_id: u32, name: String, path: String },
    /// Erase everything in Trash or Junk (#152).
    EmptyFolder { account_id: u32, folder_id: u32, name: String, path: String },
    /// Open Settings on the filter rule that files into this folder.
    EditFilter { account_id: u32, path: String },
    /// Run this account's filter rules over the mail already in this
    /// folder (#198).
    ApplyFilters { account_id: u32, folder_id: u32 },
    /// Open Settings on this tag (by keyword).
    EditTag(String),
}

#[relm4::component(pub)]
impl Component for Sidebar {
    type Init = SidebarInit;
    type Input = SidebarInput;
    type Output = SidebarOutput;
    type CommandOutput = ();

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,

            #[name = "body_overlay"]
            gtk::Overlay {
                set_vexpand: true,

                // The top block (compose bar, All Inboxes) is pinned above
                // the scroller so it never scrolls away; only the per-account
                // sections below scroll. Contacts/Attachments pin below it,
                // in the footer.
                #[wrap(Some)]
                set_child = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,

                    #[name = "pinned_box"]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                    },

                    #[name = "sidebar_scroller"]
                    gtk::ScrolledWindow {
                        set_vexpand: true,
                        // External, not Never (see the message list's scroller): row
                        // content — a deeply indented folder tree, say — must not force
                        // the window's minimum width past what edge-tiling allows. The
                        // split view's min/max sidebar widths govern instead.
                        set_hscrollbar_policy: gtk::PolicyType::External,

                        #[name = "normal_box"]
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                        },
                    },

                    // Pinned footer, below the scroller: the Contacts and
                    // Attachments rows stay put against the sidebar's bottom
                    // edge no matter how tall the account list above grows.
                    #[name = "footer_box"]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                    },
                },

                // Freeze-frame for rebuilds: the sidebar's last-rendered pixels,
                // shown over the swap so recreating every row never shimmers.
                // Anchored at the start and sized to the pixels it holds, so
                // the pane animating to or from the rail beneath it can never
                // stretch or squash the picture (the overlay clips it instead).
                #[name = "freeze_frame"]
                add_overlay = &gtk::Picture {
                    set_visible: false,
                    set_can_target: false,
                    set_halign: gtk::Align::Start,
                    set_content_fit: gtk::ContentFit::Fill,
                },
            },

        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let color_provider = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &color_provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        let mut model = Sidebar {
            sections: Vec::new(),
            show_unified: false,
            unified_chips: crate::config::UnifiedChips::default(),
            chevrons_left: false,
            rail_dots: false,
            rail_fold: crate::config::RailFold::default(),
            revealers: HashMap::new(),
            chevrons: HashMap::new(),
            folder_lists: HashMap::new(),
            custom_folder_lists: HashMap::new(),
            custom_folders: HashMap::new(),
            tree_collapsed: HashMap::new(),
            tree_chevrons: HashMap::new(),
            tree_row_revealers: HashMap::new(),
            freeze_frame: None,
            freeze_timer: std::rc::Rc::new(std::cell::RefCell::new(None)),
            freeze_fade: std::rc::Rc::new(std::cell::RefCell::new(None)),
            built_collapsed: init.collapsed,
            custom_revealers: HashMap::new(),
            custom_chevrons: HashMap::new(),
            unified_list: None,
            footer_list: None,
            attachments_row: None,
            contacts_row: None,
            sync_stack: None,
            sync_spinner: None,
            busy: false,
            outbox_list: None,
            color_provider,
            selected: Sel::None,
            collapsed: init.collapsed,
            quiet: std::rc::Rc::new(std::cell::Cell::new(false)),
            mirror: init.mirror,
            show_attachments: init.show_attachments,
            show_contacts: init.show_contacts,
            outbox_count: 0,
            unified_unread: 0,
            folder_badges: HashMap::new(),
            unified_badge: None,
            unified_expanded: init.unified_expanded,
            unified_revealer: None,
            unified_chevron: None,
            unified_inbox_list: None,
            unified_inboxes: Vec::new(),
            unified_inbox_badges: HashMap::new(),
            unified_folders: Vec::new(),
            unified_folders_expanded: init.filtered_expanded,
            filtered_rows: HashMap::new(),
            filtered_sections: HashMap::new(),
            filtered_badges: HashMap::new(),
            account_circle_badges: HashMap::new(),
            tags: Vec::new(),
            tags_expanded: init.tags_expanded,
            tag_sections: HashMap::new(),
            unified_kinds: crate::config::UnifiedKinds::NONE,
            kind_expanded: HashMap::from([
                (FolderKind::Starred, init.starred_expanded),
                (FolderKind::Sent, init.sent_expanded),
                (FolderKind::Drafts, init.drafts_expanded),
                (FolderKind::Archive, init.archive_expanded),
            ]),
            kind_widgets: HashMap::new(),
            rail_open: HashMap::new(),
            rail_open_accounts: HashMap::new(),
            unified_tags: true,
            show_accounts: true,
            filtered_placement: crate::config::SectionPlacement::default(),
            tags_placement: crate::config::SectionPlacement::default(),
        };

        let widgets = view_output!();
        model.freeze_frame = Some(widgets.freeze_frame.clone());
        widgets.body_overlay.set_clip_overlay(&widgets.freeze_frame, true);
        // Never scroll-to-focus: a rebuild (folder drag-and-drop) destroys
        // the focused row, GTK hands focus to some early widget, and the
        // viewport would yank the sidebar to the top to show it — the
        // "jumps up then back down" on every drop. Sidebar scrolling is the
        // user's alone.
        if let Some(viewport) =
            widgets.sidebar_scroller.child().and_downcast::<gtk::Viewport>()
        {
            viewport.set_scroll_to_focus(false);
        }

        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        self.update_inner(widgets, msg, sender, root);
    }
}

impl Sidebar {
    fn update_inner(
        &mut self,
        widgets: &mut <Self as Component>::Widgets,
        msg: SidebarInput,
        sender: ComponentSender<Self>,
        _root: &<Self as Component>::Root,
    ) {
        match msg {
            SidebarInput::MirrorSelection(sel) => {
                if self.selected != sel {
                    self.selected = sel.clone();
                    self.quiet.set(true);
                    self.clear_other_selections(sel);
                    self.quiet.set(false);
                    self.restore_selection();
                }
            }

            SidebarInput::SetContents {
                mut sections,
                show_unified,
                unified_kinds,
                unified_tags,
                show_accounts,
                unified_chips,
                chevrons_left,
                rail_dots,
                rail_fold,
                unified_unread,
                unified_folders,
                tags,
                filtered_placement,
                tags_placement,
            } => {
                // Order each account's folders essential-first, then custom, so
                // the essential/custom split lines up with row indices (the main
                // list holds indices 0..E, the custom list E..).
                for s in &mut sections {
                    s.folders.sort_by_key(|f| f.kind == FolderKind::Custom);
                }
                // Seed the tree's collapsed nodes from the persisted state;
                // later chevron clicks flip the local copy.
                self.tree_collapsed = sections
                    .iter()
                    .map(|s| {
                        (s.account.id, s.tree_collapsed.iter().cloned().collect())
                    })
                    .collect();
                self.sections = sections;
                self.show_unified = show_unified;
                self.unified_chips = unified_chips;
                self.chevrons_left = chevrons_left;
                self.rail_dots = rail_dots;
                // "Fold up expanded items" is a view of the rail, not a
                // change to what is saved: the rebuild below draws the
                // ticked items folded while the sidebar is collapsed and
                // as they are once it expands (see `locked_row`).
                self.rail_fold = rail_fold;
                self.unified_unread = unified_unread;
                self.unified_folders = unified_folders;
                self.tags = tags;
                self.filtered_placement = filtered_placement;
                self.tags_placement = tags_placement;
                self.unified_kinds = unified_kinds;
                self.unified_tags = unified_tags;
                self.show_accounts = show_accounts;
                // Every Filtered Folders section's rows: the unified one's
                // (narrowed by the app to the rules that opted in) and each
                // account's own (every rule's destination).
                self.filtered_rows.clear();
                self.filtered_rows.insert(Slot::Unified, self.unified_folders.clone());
                for s in &self.sections {
                    let rows = s
                        .filtered
                        .iter()
                        .map(|f| UnifiedFolderRef { account_id: s.account.id, folder: f.clone() })
                        .collect();
                    self.filtered_rows.insert(Slot::Account(s.account.id), rows);
                }
                // The open tag was removed (or its account): fall back to
                // the default view.
                if let Sel::Tag(acc, kw) = &self.selected {
                    let gone = !self.tags.iter().any(|t| t.keyword.eq_ignore_ascii_case(kw))
                        || acc.is_some_and(|a| !self.sections.iter().any(|s| s.account.id == a));
                    if gone {
                        self.selected = Sel::None;
                    }
                }
                // A unified row switched off in Settings (or with nothing
                // left to list): its view stays, nothing shows selected.
                if let Sel::UnifiedKind(kind) = &self.selected {
                    if !self.unified_kinds.has(*kind) {
                        self.selected = Sel::None;
                    }
                }
                if self.selected == Sel::UnifiedFiltered && self.unified_folders.is_empty() {
                    self.selected = Sel::None;
                }
                if self.selected == Sel::UnifiedTags && !self.unified_tags_shown() {
                    self.selected = Sel::None;
                }
                // A folder picked in a switched-off row's account list is
                // still the open folder: carry the highlight to the
                // account's own row.
                if let Sel::UnifiedKindRow(kind, acc) = self.selected.clone() {
                    if !self.unified_kinds.has(kind) {
                        let path = self
                            .sections
                            .iter()
                            .find(|s| s.account.id == acc)
                            .and_then(|s| s.folders.iter().find(|f| f.kind == kind))
                            .map(|f| f.path.clone());
                        self.selected = path.map_or(Sel::None, |p| Sel::Folder(acc, p));
                    }
                }
                // Likewise a folder that left an account's own Filtered
                // Folders section (its rule was removed).
                if let Sel::AccountFiltered(acc, path) = &self.selected {
                    let listed = self
                        .filtered_rows
                        .get(&Slot::Account(*acc))
                        .is_some_and(|rows| rows.iter().any(|r| r.folder.path == *path));
                    if !listed {
                        self.selected = Sel::Folder(*acc, path.clone());
                    }
                }
                // A selected filtered folder that just left the section (its
                // rule opted out, or the section was switched off) is still
                // the open folder: carry the highlight to the account
                // section's own row for it.
                if let Sel::UnifiedFolder(acc, path) = &self.selected {
                    let listed = self
                        .unified_folders
                        .iter()
                        .any(|r| r.account_id == *acc && r.folder.path == *path);
                    if !listed {
                        self.selected = Sel::Folder(*acc, path.clone());
                    }
                }
                self.rebuild_normal(
                    &widgets.pinned_box,
                    &widgets.normal_box,
                    &widgets.footer_box,
                    &sender,
                );
                self.restore_selection();
            }

            SidebarInput::UnifiedRowSelected => {
                if self.selected == Sel::Unified {
                    return;
                }
                self.selected = Sel::Unified;
                self.clear_other_selections(Sel::Unified);
                let _ = sender.output(SidebarOutput::UnifiedSelected);
            }

            SidebarInput::SelectUnifiedRow => {
                if self.selected == Sel::Unified {
                    return;
                }
                self.selected = Sel::Unified;
                self.quiet.set(true);
                self.clear_other_selections(Sel::Unified);
                if let Some(l) = &self.unified_list {
                    l.select_row(l.row_at_index(0).as_ref());
                }
                self.quiet.set(false);
                let _ = sender.output(SidebarOutput::UnifiedSelected);
            }

            SidebarInput::AttachmentsRowSelected => {
                if self.selected == Sel::Attachments {
                    return;
                }
                self.selected = Sel::Attachments;
                self.clear_other_selections(Sel::Attachments);
                let _ = sender.output(SidebarOutput::AttachmentsSelected);
            }

            SidebarInput::ContactsRowClicked => {
                if self.selected == Sel::Contacts {
                    return;
                }
                self.selected = Sel::Contacts;
                self.clear_other_selections(Sel::Contacts);
                let _ = sender.output(SidebarOutput::ContactsClicked);
            }

            SidebarInput::OutboxRowSelected => {
                if self.selected == Sel::Outbox {
                    return;
                }
                self.selected = Sel::Outbox;
                self.clear_other_selections(Sel::Outbox);
                let _ = sender.output(SidebarOutput::OutboxSelected);
            }

            SidebarInput::SetOutboxCount(count) => {
                if self.outbox_count == count {
                    return;
                }
                let appearing = (self.outbox_count == 0) != (count == 0);
                self.outbox_count = count;
                // The row itself comes and goes with the count, so the sidebar
                // only needs rebuilding when it crosses zero; otherwise just
                // refresh the badge in place.
                if appearing {
                    // Leaving the Outbox selected when its row disappears would
                    // strand the view on an empty list.
                    if count == 0 && self.selected == Sel::Outbox {
                        self.selected = Sel::None;
                    }
                    self.rebuild_normal(
                        &widgets.pinned_box,
                        &widgets.normal_box,
                        &widgets.footer_box,
                        &sender,
                    );
                    self.restore_selection();
                } else if let Some(list) = &self.outbox_list {
                    if let Some(row) = list.row_at_index(0) {
                        if let Some(badge) = row.child().and_downcast::<gtk::Box>() {
                            if let Some(label) =
                                badge.last_child().and_downcast::<gtk::Label>()
                            {
                                label.set_label(&count.to_string());
                            }
                        }
                    }
                }
            }

            SidebarInput::UnifiedInboxRowSelected(index) => {
                if let Some(r) = self.unified_inboxes.get(index as usize).cloned() {
                    let key = Sel::UnifiedInbox(r.account_id);
                    if self.selected == key {
                        return;
                    }
                    self.selected = key.clone();
                    self.clear_other_selections(key);
                    let _ = sender.output(SidebarOutput::FolderSelected {
                        account_id: r.account_id,
                        folder_id: r.folder_id,
                        name: r.name,
                        path: r.path,
                    });
                }
            }

            SidebarInput::ToggleUnifiedExpand => {
                self.toggle_row(UnifiedRow::Kind(FolderKind::Inbox), &sender)
            }

            SidebarInput::FilteredRowSelected { slot, index } => {
                let Some(r) = self
                    .filtered_rows
                    .get(&slot)
                    .and_then(|rows| rows.get(index as usize))
                    .cloned()
                else {
                    return;
                };
                let key = match slot {
                    Slot::Unified => Sel::UnifiedFolder(r.account_id, r.folder.path.clone()),
                    Slot::Account(_) => Sel::AccountFiltered(r.account_id, r.folder.path.clone()),
                };
                if self.selected == key {
                    return;
                }
                self.selected = key.clone();
                self.clear_other_selections(key);
                let _ = sender.output(SidebarOutput::FolderSelected {
                    account_id: r.account_id,
                    folder_id: r.folder.id,
                    name: r.folder.name,
                    path: r.folder.path,
                });
            }

            SidebarInput::TagRowSelected { slot, index } => {
                if let Some(t) = self.tags.get(index as usize).cloned() {
                    let account = slot.account();
                    let key = Sel::Tag(account, t.keyword.clone());
                    if self.selected == key {
                        return;
                    }
                    self.selected = key.clone();
                    self.clear_other_selections(key);
                    let _ = sender.output(SidebarOutput::TagSelected { keyword: t.keyword, account });
                }
            }

            // The unified slot is a unified row when placed in the unified
            // section, a heading (like an account's own) above or below the
            // accounts.
            SidebarInput::ToggleTagsExpand(slot) => {
                if slot == Slot::Unified && self.kind_widgets.contains_key(&UnifiedRow::Tags) {
                    self.toggle_row(UnifiedRow::Tags, &sender);
                } else {
                    let open = !self.tags_open(slot);
                    self.set_tags_open(slot, open, &sender);
                    if let Some(w) = self.tag_sections.get(&slot) {
                        w.revealer.set_reveal_child(open);
                        if slot == Slot::Unified {
                            w.toggle.set_margin_bottom(tags_toggle_gap(open));
                        }
                        w.chevron.set_icon_name(Some(chevron_icon(open)));
                    }
                }
            }

            SidebarInput::ToggleFilteredExpand(slot) => {
                if slot == Slot::Unified && self.kind_widgets.contains_key(&UnifiedRow::Filtered) {
                    self.toggle_row(UnifiedRow::Filtered, &sender);
                } else {
                    let open = !self.filtered_open(slot);
                    self.set_filtered_open(slot, open, &sender);
                    let unread = self.filtered_unread(slot);
                    if let Some(w) = self.filtered_sections.get(&slot) {
                        w.revealer.set_reveal_child(open);
                        if slot == Slot::Unified {
                            w.toggle.set_margin_bottom(unified_folders_toggle_gap(open));
                        }
                        // Folded: the header wears the section's total;
                        // open, each row carries its own count.
                        if let Some(b) = &w.badge {
                            b.set_visible(unread > 0 && !open);
                        }
                        w.chevron.set_icon_name(Some(chevron_icon(open)));
                    }
                }
            }

            SidebarInput::UnifiedKindRowSelected(kind) => {
                let key = Sel::UnifiedKind(kind);
                if self.selected == key {
                    return;
                }
                self.selected = key.clone();
                self.clear_other_selections(key);
                let _ = sender.output(SidebarOutput::UnifiedKindSelected(kind));
            }

            SidebarInput::UnifiedFilteredSelected => {
                if self.selected == Sel::UnifiedFiltered {
                    return;
                }
                self.selected = Sel::UnifiedFiltered;
                self.clear_other_selections(Sel::UnifiedFiltered);
                let _ = sender.output(SidebarOutput::UnifiedFilteredSelected);
            }

            SidebarInput::UnifiedTagsSelected => {
                if self.selected == Sel::UnifiedTags {
                    return;
                }
                self.selected = Sel::UnifiedTags;
                self.clear_other_selections(Sel::UnifiedTags);
                let _ = sender.output(SidebarOutput::UnifiedTagsSelected);
            }

            SidebarInput::KindSubRowSelected { kind, index } => {
                let Some(r) = self
                    .kind_widgets
                    .get(&UnifiedRow::Kind(kind))
                    .and_then(|w| w.rows.get(index as usize))
                    .cloned()
                else {
                    return;
                };
                let key = Sel::UnifiedKindRow(kind, r.account_id);
                if self.selected == key {
                    return;
                }
                self.selected = key.clone();
                self.clear_other_selections(key);
                let _ = sender.output(SidebarOutput::FolderSelected {
                    account_id: r.account_id,
                    folder_id: r.folder_id,
                    name: r.name,
                    path: r.path,
                });
            }

            SidebarInput::ToggleKindExpand(kind) => self.toggle_row(UnifiedRow::Kind(kind), &sender),

            SidebarInput::FolderRowSelected { account_id, index } => {
                let folder = self
                    .sections
                    .iter()
                    .find(|s| s.account.id == account_id)
                    .and_then(|s| s.folders.get(index as usize))
                    .cloned();
                if let Some(folder) = folder {
                    let key = Sel::Folder(account_id, folder.path.clone());
                    if self.selected == key {
                        return;
                    }
                    self.selected = key.clone();
                    self.clear_other_selections(key);
                    let _ = sender.output(SidebarOutput::FolderSelected {
                        account_id,
                        folder_id: folder.id,
                        name: folder.name.clone(),
                        path: folder.path.clone(),
                    });
                }
            }

            SidebarInput::SelectFolderRow { account_id, path } => {
                // A click on a per-account inbox under "All Inboxes" echoes
                // back here as its plain folder path; that sub-row already
                // shows this exact folder, so keep its highlight instead of
                // jumping to the account section's own Inbox row.
                if self.selected == Sel::UnifiedInbox(account_id)
                    && self
                        .unified_inboxes
                        .iter()
                        .any(|r| r.account_id == account_id && r.path == path)
                {
                    return;
                }
                // Likewise a click on a filtered folder in either kind of
                // Filtered Folders section, or on an account's folder in a
                // unified Starred / Sent / Drafts row's list.
                if self.selected == Sel::UnifiedFolder(account_id, path.clone())
                    || self.selected == Sel::AccountFiltered(account_id, path.clone())
                {
                    return;
                }
                if let Sel::UnifiedKindRow(kind, acc) = &self.selected {
                    if *acc == account_id
                        && self.kind_widgets.get(&UnifiedRow::Kind(*kind)).is_some_and(|w| {
                            w.rows.iter().any(|r| r.account_id == account_id && r.path == path)
                        })
                    {
                        return;
                    }
                }
                let key = Sel::Folder(account_id, path.clone());
                if self.selected != key {
                    self.selected = key.clone();
                    self.quiet.set(true);
                    self.clear_other_selections(key);
                    self.select_folder(account_id, &path);
                    self.quiet.set(false);
                }
                // A highlight inside a folded account shows nothing (#170:
                // a notification click seemed to land nowhere); unfold it.
                self.reveal_account(account_id, &sender);
            }

            SidebarInput::SetUnread { folders, unified } => {
                // Mirror the fresh counts into every row list too, so a
                // rebuild draws them right (the account folders are done
                // below).
                let fresh = |aid: u32, fid: u32, cur: u32| folders.get(&(aid, fid)).copied().unwrap_or(cur);
                for r in self.unified_folders.iter_mut() {
                    r.folder.unread = fresh(r.account_id, r.folder.id, r.folder.unread);
                }
                for rows in self.filtered_rows.values_mut() {
                    for r in rows.iter_mut() {
                        r.folder.unread = fresh(r.account_id, r.folder.id, r.folder.unread);
                    }
                }
                for s in &mut self.sections {
                    let aid = s.account.id;
                    for f in &mut s.filtered {
                        f.unread = fresh(aid, f.id, f.unread);
                    }
                }
                for ((aid, fid), label) in self
                    .folder_badges
                    .iter()
                    .chain(&self.unified_inbox_badges)
                    .chain(self.filtered_badges.values().flatten())
                    .chain(self.kind_widgets.values().flat_map(|w| &w.row_badges))
                {
                    let n = folders.get(&(*aid, *fid)).copied().unwrap_or(0);
                    label.set_text(&n.to_string());
                    label.set_visible(n > 0);
                }
                if let Some(label) = &self.unified_badge {
                    label.set_text(&unified.to_string());
                    label.set_visible(
                        unified > 0
                            && !self.row_shown_open(UnifiedRow::Kind(FolderKind::Inbox))
                            && self.unified_chips.all_inboxes,
                    );
                }
                for (slot, badges) in &self.filtered_badges {
                    let total: u32 = badges.keys().map(|k| folders.get(k).copied().unwrap_or(0)).sum();
                    if let Some(b) = self.filtered_sections.get(slot).and_then(|w| w.badge.as_ref()) {
                        b.set_text(&total.to_string());
                        b.set_visible(total > 0 && !self.filtered_open(*slot) && self.unified_chips.filtered);
                    }
                }
                for (row, w) in &self.kind_widgets {
                    let total: u32 = w.row_badges.keys().map(|k| folders.get(k).copied().unwrap_or(0)).sum();
                    if let Some(b) = &w.badge {
                        b.set_text(&total.to_string());
                        b.set_visible(total > 0 && !self.row_shown_open(*row) && self.chip_shown(*row));
                    }
                }
                // Keep the avatar-circle badges in sync too. They only show while
                // the account is collapsed (toggled live in ToggleCollapseLocal),
                // so we just refresh the number and re-apply that visibility rule.
                for section in &self.sections {
                    if let Some(label) = self.account_circle_badges.get(&section.account.id) {
                        let n = section
                            .folders
                            .iter()
                            .find(|f| f.kind == FolderKind::Inbox)
                            .and_then(|f| folders.get(&(section.account.id, f.id)))
                            .copied()
                            .unwrap_or(0);
                        label.set_text(&n.to_string());
                        label.set_visible(self.account_shown_folded(section) && n > 0);
                    }
                }
                self.unified_unread = unified;
                // Persist the fresh counts into `sections` as well. Otherwise the
                // next rebuild_normal (e.g. toggling the sidebar collapse) recreates
                // every badge from the folder unread values captured at the last
                // SetContents, reverting in-place updates — so a read inbox's chip
                // reappears on collapse. Keep `sections` a faithful mirror.
                for section in &mut self.sections {
                    for folder in &mut section.folders {
                        folder.unread = folders
                            .get(&(section.account.id, folder.id))
                            .copied()
                            .unwrap_or(0);
                    }
                }
            }

            SidebarInput::SetAttachmentsRow(show) => {
                self.show_attachments = show;
                // The row is about to disappear from under the selection; fall
                // back the same way an emptied selection does.
                if !show && self.selected == Sel::Attachments {
                    self.selected = Sel::None;
                }
                self.rebuild_normal(
                    &widgets.pinned_box,
                    &widgets.normal_box,
                    &widgets.footer_box,
                    &sender,
                );
                self.restore_selection();
            }

            SidebarInput::SetContactsRow(show) => {
                self.show_contacts = show;
                // The row is about to disappear from under the selection; fall
                // back the same way an emptied selection does.
                if !show && self.selected == Sel::Contacts {
                    self.selected = Sel::None;
                }
                self.rebuild_normal(
                    &widgets.pinned_box,
                    &widgets.normal_box,
                    &widgets.footer_box,
                    &sender,
                );
                self.restore_selection();
            }

            SidebarInput::SetBusy(busy) => {
                self.busy = busy;
                if let Some(sp) = &self.sync_spinner {
                    sp.set_spinning(busy);
                }
                if let Some(stack) = &self.sync_stack {
                    stack.set_visible_child_name(if busy { "spinner" } else { "icon" });
                }
            }


            SidebarInput::SetCollapsed(collapsed) => {
                // Driven by the app's narrow-window breakpoint: same visual
                // change as the user's own toggle, but no CollapsedChanged
                // output — automatic switches must not overwrite the user's
                // persisted preference.
                if self.collapsed != collapsed {
                    self.collapsed = collapsed;
                    self.rail_open.clear();
                    self.rail_open_accounts.clear();
                    self.rebuild_normal(
                        &widgets.pinned_box,
                        &widgets.normal_box,
                        &widgets.footer_box,
                        &sender,
                    );
                    self.restore_selection();
                }
            }

            SidebarInput::ToggleCollapsed => {
                self.collapsed = !self.collapsed;
                self.rail_open.clear();
                self.rail_open_accounts.clear();
                self.rebuild_normal(
                    &widgets.pinned_box,
                    &widgets.normal_box,
                    &widgets.footer_box,
                    &sender,
                );
                self.restore_selection();
                let _ = sender.output(SidebarOutput::CollapsedChanged(self.collapsed));
            }

            SidebarInput::ToggleCollapseLocal(id) => {
                // In the rail this opens or folds the account for the rail
                // alone — the saved state stays as it is for the full
                // sidebar, which comes back as it was left.
                let rail_only = self.collapsed;
                if let Some(rev) = self.revealers.get(&id) {
                    let expanded = !rev.reveals_child();
                    rev.set_reveal_child(expanded);
                    if let Some(ch) = self.chevrons.get(&id) {
                        ch.set_icon_name(Some(if expanded { "co.hyprlab.Hylki-pan-down-symbolic" } else { "co.hyprlab.Hylki-pan-end-symbolic" }));
                    }
                    if rail_only {
                        self.rail_open_accounts.insert(id, expanded);
                    } else if let Some(s) = self.sections.iter_mut().find(|s| s.account.id == id) {
                        s.collapsed = !expanded;
                    }
                    // The Inbox chip lives inside the folder list we just hid/shown,
                    // so mirror it onto the avatar: visible only while collapsed.
                    if let Some(label) = self.account_circle_badges.get(&id) {
                        let n = self
                            .sections
                            .iter()
                            .find(|s| s.account.id == id)
                            .and_then(|s| s.folders.iter().find(|f| f.kind == FolderKind::Inbox))
                            .map(|f| f.unread)
                            .unwrap_or(0);
                        label.set_text(&n.to_string());
                        label.set_visible(!expanded && n > 0);
                    }
                    if !rail_only {
                        let _ = sender.output(SidebarOutput::ToggleCollapse(id));
                    }
                }
            }

            SidebarInput::ToggleCustomFoldersLocal(id) => {
                if let Some(rev) = self.custom_revealers.get(&id) {
                    let expanded = !rev.reveals_child();
                    rev.set_reveal_child(expanded);
                    if let Some(ch) = self.custom_chevrons.get(&id) {
                        ch.set_icon_name(Some(if expanded { "co.hyprlab.Hylki-pan-down-symbolic" } else { "co.hyprlab.Hylki-pan-end-symbolic" }));
                    }
                    if let Some(s) = self.sections.iter_mut().find(|s| s.account.id == id) {
                        s.custom_expanded = expanded;
                    }
                    let _ = sender.output(SidebarOutput::ToggleCustomFolders(id));
                }
            }

            SidebarInput::ToggleFolderNode { account_id, path } => {
                self.toggle_folder_node(account_id, path, &sender);
            }

            SidebarInput::FolderRowActivated { account_id, index } => {
                // Single-clicking a folder that has sub-folders toggles them —
                // no need to aim for the caret. Leaves just select as before.
                let target = self.custom_folders.get(&account_id).and_then(|folders| {
                    folders.get(index as usize).and_then(|f| {
                        folders
                            .iter()
                            .any(|g| path_is_under(&g.path, &f.path))
                            .then(|| f.path.clone())
                    })
                });
                if let Some(path) = target {
                    self.toggle_folder_node(account_id, path, &sender);
                }
            }

            SidebarInput::ExpandForDrop(id) => {
                // Expand a collapsed account so its folders become drop targets.
                self.reveal_account(id, &sender);
            }

            SidebarInput::DropOnFolder { account_id: dest_account, path: dest, payload } => {
                // A dragged folder, not messages: reparent it (#51). Folders
                // never cross accounts — mailboxes belong to one server.
                if let Some(rest) = payload.strip_prefix("vireo-folder\t") {
                    let mut it = rest.splitn(2, '\t');
                    let src_account = it.next().and_then(|s| s.parse::<u32>().ok());
                    let src_path = it.next().map(String::from);
                    if let (Some(src_account), Some(src_path)) = (src_account, src_path) {
                        if src_account == dest_account && src_path != dest {
                            let _ = sender.output(SidebarOutput::MoveFolder {
                                account_id: dest_account,
                                path: src_path,
                                dest,
                            });
                        }
                    }
                    return;
                }
                let items = parse_move_payload(&payload);
                // "" is the Folders header (a folder-move destination only);
                // messages need a real mailbox.
                if !items.is_empty() && !dest.is_empty() {
                    let _ = sender.output(SidebarOutput::MoveMessages { dest_account, dest, items });
                }
            }
        }
    }
}

impl Sidebar {
    /// Rebuild the list: optional unified row, then per-account headers with
    /// animated folder revealers, and refresh the per-account colour rules.
    /// Flip one tree node, restyle its caret, and re-apply visibility across
    /// the account's tree — no rebuild, so nothing flickers. Reports the new
    /// state for persistence.
    fn toggle_folder_node(
        &mut self,
        account_id: u32,
        path: String,
        sender: &ComponentSender<Self>,
    ) {
        let nodes = self.tree_collapsed.entry(account_id).or_default();
        let collapsed = if nodes.contains(&path) {
            nodes.remove(&path);
            false
        } else {
            nodes.insert(path.clone());
            true
        };
        if let Some(img) = self.tree_chevrons.get(&(account_id, path.clone())) {
            if collapsed {
                img.remove_css_class("open");
            } else {
                img.add_css_class("open");
            }
        }
        self.apply_tree_visibility(account_id);
        let _ = sender.output(SidebarOutput::FolderNodeCollapsed { account_id, path, collapsed });
    }

    /// Re-apply row visibility across one account's custom-folder tree (#51):
    /// a row shows unless some ancestor node is collapsed. Rows are never
    /// removed, so selection indices hold still.
    fn apply_tree_visibility(&self, account_id: u32) {
        let (Some(list), Some(folders)) = (
            self.custom_folder_lists.get(&account_id),
            self.custom_folders.get(&account_id),
        ) else {
            return;
        };
        let collapsed = self.tree_collapsed.get(&account_id).cloned().unwrap_or_default();
        let revealers = self.tree_row_revealers.get(&account_id);
        for (i, folder) in folders.iter().enumerate() {
            let Some(row) = list.row_at_index(i as i32) else { continue };
            let hidden = hidden_by_collapse(&folder.path, &collapsed);
            let rev = revealers.and_then(|r| r.get(i));
            match (hidden, rev) {
                (false, Some(rev)) => {
                    // Show the row first, then slide its content open.
                    row.set_visible(true);
                    rev.set_reveal_child(true);
                }
                (true, Some(rev)) => {
                    if row.get_visible() {
                        // Slide closed, then drop the row itself once the
                        // animation is done — an empty visible row still
                        // paints its chrome. Skipped if it was re-expanded
                        // inside the window.
                        rev.set_reveal_child(false);
                        let row = row.clone();
                        let rev = rev.clone();
                        gtk::glib::timeout_add_local_once(
                            std::time::Duration::from_millis(220),
                            move || {
                                if !rev.reveals_child() {
                                    row.set_visible(false);
                                }
                            },
                        );
                    }
                }
                (hidden, None) => row.set_visible(!hidden),
            }
        }
    }

    fn rebuild_normal(
        &mut self,
        pinned: &gtk::Box,
        container: &gtk::Box,
        footer: &gtk::Box,
        sender: &ComponentSender<Self>,
    ) {
        use crate::config::SectionPlacement::{self, AboveAccounts, AllInboxes, BelowAccounts};
        // Keep the scroll offset: rebuilding otherwise snaps the sidebar to
        // the top — felt on every folder drag-and-drop, whose optimistic move
        // rebuilds immediately under the pointer.
        let scroller = container
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_downcast::<gtk::ScrolledWindow>();
        let saved_scroll = scroller.as_ref().map(|s| s.vadjustment().value());
        // Freeze the sidebar's last-rendered pixels over the swap: even with
        // the scroll pinned, tearing down and recreating every row can
        // shimmer for a frame. The snapshot covers the rebuild and lifts a
        // few frames later, once the fresh tree has painted beneath it —
        // identical pixels, so the crossover is invisible. The snapshot is of
        // the overlay's whole child (pinned block + scroller + footer),
        // matching the area the freeze-frame Picture stretches over.
        //
        // A toggle between the full sidebar and the icon rail is different:
        // the pane's width animates underneath the snapshot for 200ms (the
        // app's `animate_sidebar`), so the snapshot keeps the width it was
        // taken at (never stretched to the moving pane — that was a visible
        // smear of every icon) and fades out over the same 200ms while the
        // fresh rows, centred in the pane as it moves, come through beneath.
        let rail_toggle = self.built_collapsed != self.collapsed;
        self.built_collapsed = self.collapsed;
        if let (Some(freeze), Some(area)) = (self.freeze_frame.clone(), pinned.parent()) {
            if container.first_child().is_some() || pinned.first_child().is_some() {
                use gtk::gdk::prelude::PaintableExt;
                // Take before skipping: the fade's done handler clears the
                // same slot, so the borrow must be over by then.
                let fading = self.freeze_fade.borrow_mut().take();
                if let Some(prev) = fading {
                    prev.skip();
                }
                let pending = self.freeze_timer.borrow_mut().take();
                if let Some(prev) = pending {
                    prev.remove();
                }
                let live = gtk::WidgetPaintable::new(Some(&area));
                freeze.set_paintable(Some(&live.current_image()));
                freeze.set_size_request(area.width().max(1), -1);
                freeze.set_opacity(1.0);
                freeze.set_visible(true);
                if rail_toggle {
                    let target = adw::CallbackAnimationTarget::new({
                        let freeze = freeze.clone();
                        move |v| freeze.set_opacity(v)
                    });
                    let anim = adw::TimedAnimation::new(&freeze, 1.0, 0.0, 200, target);
                    anim.set_easing(adw::Easing::EaseOutCubic);
                    anim.connect_done({
                        let freeze = freeze.clone();
                        let slot = self.freeze_fade.clone();
                        move |_| {
                            slot.borrow_mut().take();
                            freeze.set_visible(false);
                            freeze.set_opacity(1.0);
                        }
                    });
                    *self.freeze_fade.borrow_mut() = Some(anim.clone());
                    anim.play();
                } else {
                    let timer = gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(80),
                        {
                            let freeze = freeze.clone();
                            let slot = self.freeze_timer.clone();
                            move || {
                                slot.borrow_mut().take();
                                freeze.set_visible(false);
                            }
                        },
                    );
                    *self.freeze_timer.borrow_mut() = Some(timer);
                }
            }
        }
        while let Some(child) = pinned.first_child() {
            pinned.remove(&child);
        }
        while let Some(child) = container.first_child() {
            container.remove(&child);
        }
        while let Some(child) = footer.first_child() {
            footer.remove(&child);
        }
        if self.collapsed {
            pinned.add_css_class("rail-collapsed");
            container.add_css_class("rail-collapsed");
            footer.add_css_class("rail-collapsed");
        } else {
            pinned.remove_css_class("rail-collapsed");
            container.remove_css_class("rail-collapsed");
            footer.remove_css_class("rail-collapsed");
        }
        // Unread dots in place of counts (Settings → Sidebar → Icon rail):
        // a style on the rail's containers, so every mini chip built below
        // shrinks to a dot without each builder knowing.
        for w in [pinned, container, footer] {
            if self.collapsed && self.rail_dots {
                w.add_css_class("rail-dots");
            } else {
                w.remove_css_class("rail-dots");
            }
        }
        self.revealers.clear();
        self.chevrons.clear();
        self.folder_lists.clear();
        self.custom_folder_lists.clear();
        self.custom_revealers.clear();
        self.custom_chevrons.clear();
        self.unified_list = None;
        self.footer_list = None;
        self.attachments_row = None;
        self.contacts_row = None;
        self.outbox_list = None;
        self.folder_badges.clear();
        self.tree_chevrons.clear();
        self.tree_row_revealers.clear();
        self.unified_badge = None;
        self.unified_revealer = None;
        self.unified_chevron = None;
        self.unified_inbox_list = None;
        self.unified_inboxes.clear();
        self.unified_inbox_badges.clear();
        self.filtered_sections.clear();
        self.filtered_badges.clear();
        self.account_circle_badges.clear();
        self.tag_sections.clear();
        self.kind_widgets.clear();

        // No accounts yet: show a prompt to add the first one instead of an empty
        // sidebar (the app is blank in this state).
        if self.sections.is_empty() {
            let s = sender.clone();
            let add = gtk::Button::new();
            add.add_css_class("suggested-action");
            add.add_css_class("pill");
            add.set_valign(gtk::Align::Center);
            add.set_halign(gtk::Align::Center);
            add.connect_clicked(move |_| {
                let _ = s.output(SidebarOutput::AddAccount);
            });
            if self.collapsed {
                add.set_icon_name("co.hyprlab.Hylki-list-add-symbolic");
                add.set_tooltip_text(Some(i18n("Add account").as_str()));
                add.set_margin_top(12);
                container.append(&add);
            } else {
                let label_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                label_box.append(&gtk::Image::from_icon_name("co.hyprlab.Hylki-list-add-symbolic"));
                label_box.append(&gtk::Label::new(Some(i18n("Add first account").as_str())));
                add.set_child(Some(&label_box));
                let empty = gtk::Box::new(gtk::Orientation::Vertical, 12);
                empty.set_valign(gtk::Align::Start);
                empty.set_margin_top(36);
                empty.set_margin_start(16);
                empty.set_margin_end(16);
                let hint = gtk::Label::new(Some(i18n("No accounts yet").as_str()));
                hint.add_css_class("dim-label");
                empty.append(&hint);
                empty.append(&add);
                container.append(&empty);
            }
            return;
        }

        let sections = self.sections.clone();

        // "New message" — the compose action. Expanded, the pill sits alone
        // and centred (Refresh lives in the app's header bar, top-left across
        // from the menu). The collapsed rail's header only has room for the
        // menu button, so Refresh stacks here instead — directly below it.
        {
            let bar = gtk::Box::new(
                if self.collapsed {
                    gtk::Orientation::Vertical
                } else {
                    gtk::Orientation::Horizontal
                },
                0,
            );

            self.sync_stack = None;
            self.sync_spinner = None;
            if self.collapsed {
                // Refresh, showing a spinner while any account syncs.
                let refresh = gtk::Button::new();
                refresh.set_tooltip_text(Some(i18n("Refresh or long-press for Status Bar").as_str()));
                refresh.add_css_class("flat");
                refresh.set_valign(gtk::Align::Center);
                refresh.set_halign(gtk::Align::Center);
                let stack = gtk::Stack::new();
                stack.set_transition_type(gtk::StackTransitionType::Crossfade);
                let icon = gtk::Image::from_icon_name("co.hyprlab.Hylki-view-refresh-symbolic");
                stack.add_named(&icon, Some("icon"));
                let spinner = gtk::Spinner::new();
                spinner.set_spinning(self.busy);
                stack.add_named(&spinner, Some("spinner"));
                stack.set_visible_child_name(if self.busy { "spinner" } else { "icon" });
                refresh.set_child(Some(&stack));
                let s = sender.clone();
                refresh.connect_clicked(move |_| {
                    let _ = s.output(SidebarOutput::RefreshRequested);
                });
                // Long-press reveals the status bar (same as the header
                // refresh); claiming the sequence suppresses the click.
                let long = gtk::GestureLongPress::new();
                let s = sender.clone();
                long.connect_pressed(move |gesture, _, _| {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    let _ = s.output(SidebarOutput::StatusBarRequested);
                });
                refresh.add_controller(long);
                bar.append(&refresh);
                self.sync_stack = Some(stack);
                self.sync_spinner = Some(spinner);
            }

            // The compose button, drawn like a row so it matches the
            // sidebar's look. Expanded: full width like the rows below, icon
            // and label centred. The collapsed rail shows the icon alone.
            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::None);
            list.add_css_class("navigation-sidebar");
            let row = gtk::ListBoxRow::new();
            row.add_css_class("compose-row");
            let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            hbox.add_css_class("folder-row");
            if self.collapsed {
                // The rail has no room for a label; the icon carries it there.
                let img = gtk::Image::from_icon_name("co.hyprlab.Hylki-mail-message-new-symbolic");
                img.add_css_class("folder-icon");
                pin_icon_size(&img);
                hbox.set_halign(gtk::Align::Center);
                row.set_tooltip_text(Some(i18n("New Message").as_str()));
                hbox.append(&img);
            } else {
                hbox.set_halign(gtk::Align::Center);
                hbox.set_spacing(6);
                let icon =
                    gtk::Image::from_icon_name("co.hyprlab.Hylki-mail-message-new-symbolic");
                icon.add_css_class("folder-icon");
                hbox.append(&icon);
                let label = gtk::Label::new(Some(i18n("New Message").as_str()));
                label.add_css_class("account-name");
                // The pill must be able to shrink with the sidebar (down to its
                // 180px minimum) — otherwise the whole column's minimum width
                // exceeds the pane and every row highlight overflows the edge.
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                hbox.append(&label);
            }
            row.set_child(Some(&hbox));
            list.append(&row);
            let s = sender.clone();
            let quiet = self.quiet.clone();
            list.connect_row_activated(move |_, _| {
                if quiet.get() {
                    return;
                }
                let _ = s.output(SidebarOutput::ComposeRequested);
            });

            if !self.collapsed {
                // Full width, like the rows below it; the centred label
                // carries the action on its own.
                list.set_hexpand(true);
                list.set_halign(gtk::Align::Fill);
            }
            bar.append(&list);
            pinned.append(&bar);
        }

        // The unified section: the "All Inboxes" row, the Starred / Sent /
        // Drafts rows (Settings → Sidebar → Unified), then — placed here —
        // the Filtered Folders and Tags rows, all built alike: a header
        // row that opens the merged view and a caret that opens the list
        // beneath. It heads the scrolling sidebar rather than the pinned
        // area: with several lists open at once it can stand taller than a
        // short window, and pinned it would have forced the window taller
        // than the screen.
        let mut unified_rows: Vec<UnifiedRow> = Vec::new();
        if self.show_unified {
            unified_rows.push(UnifiedRow::Kind(FolderKind::Inbox));
        }
        unified_rows.extend(self.unified_kinds.listed().into_iter().map(UnifiedRow::Kind));
        let unified_shown = !unified_rows.is_empty();
        if unified_shown && !self.unified_folders.is_empty() && self.filtered_placement == AllInboxes {
            unified_rows.push(UnifiedRow::Filtered);
        }
        if unified_shown && self.unified_tags_shown() && self.tags_placement == AllInboxes {
            unified_rows.push(UnifiedRow::Tags);
        }
        self.build_unified_run(container, &unified_rows, &sections, sender);

        // Contacts and Attachments live in the pinned footer against the
        // sidebar's bottom edge — they keep out of the way of the account
        // list and never scroll off. A faint rule sets them apart from
        // whatever the scroller above ends on. ONE list box holds both rows,
        // so they read as one gapless section (beta 1.18.0b feedback) and
        // selecting one automatically clears the other.
        if self.show_contacts || self.show_attachments {
            let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
            sep.add_css_class("footer-separator");
            footer.append(&sep);

            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::Single);
            list.add_css_class("navigation-sidebar");

            // "Contacts" row — shows the in-app contacts view. Right-click
            // offers a jump straight to the GNOME Contacts app.
            if self.show_contacts {
                let row = gtk::ListBoxRow::new();
                let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                hbox.add_css_class("folder-row");
                let img =
                    gtk::Image::from_icon_name("co.hyprlab.Hylki-x-office-address-book-symbolic");
                img.add_css_class("folder-icon");
                pin_icon_size(&img);
                if self.collapsed {
                    hbox.set_halign(gtk::Align::Center);
                    row.set_tooltip_text(Some(i18n("Contacts").as_str()));
                    hbox.append(&img);
                } else {
                    if self.chevrons_left {
                        img.set_margin_start(ROW_LEFT_INSET);
                    }
                    hbox.append(&img);
                    let label = gtk::Label::new(Some(i18n("Contacts").as_str()));
                    label.set_hexpand(true);
                    label.set_halign(gtk::Align::Start);
                    label.add_css_class("account-name");
                    hbox.append(&label);
                }
                row.set_child(Some(&hbox));

                let right_click = gtk::GestureClick::new();
                right_click.set_button(3);
                let s = sender.clone();
                right_click.connect_pressed(move |gesture, _, x, y| {
                    let Some(widget) = gesture.widget() else { return };
                    let s2 = s.clone();
                    show_context_menu(
                        &widget,
                        x,
                        y,
                        vec![vec![MenuEntry::new(i18n("Open GNOME Contacts"), move || {
                            let _ = s2.output(SidebarOutput::OpenGnomeContacts);
                        })
                        .icon("co.hyprlab.Hylki-adw-external-link-symbolic")]],
                    );
                });
                row.add_controller(right_click);
                list.append(&row);
                self.contacts_row = Some(row);
            }

            // "Attachments" row — a gallery of every inbox attachment. The
            // very last row in the sidebar, below Contacts.
            if self.show_attachments {
                let row = gtk::ListBoxRow::new();
                let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                hbox.add_css_class("folder-row");
                let img = gtk::Image::from_icon_name("co.hyprlab.Hylki-mail-attachment-symbolic");
                img.add_css_class("folder-icon");
                pin_icon_size(&img);
                if self.collapsed {
                    hbox.set_halign(gtk::Align::Center);
                    row.set_tooltip_text(Some(i18n("Attachments").as_str()));
                    hbox.append(&img);
                } else {
                    if self.chevrons_left {
                        img.set_margin_start(ROW_LEFT_INSET);
                    }
                    hbox.append(&img);
                    let label = gtk::Label::new(Some(i18n("Attachments").as_str()));
                    label.set_hexpand(true);
                    label.set_halign(gtk::Align::Start);
                    label.add_css_class("account-name");
                    hbox.append(&label);
                }
                row.set_child(Some(&hbox));
                list.append(&row);
                self.attachments_row = Some(row);
            }

            let s = sender.clone();
            let contacts_row = self.contacts_row.clone();
            let attachments_row = self.attachments_row.clone();
            let quiet = self.quiet.clone();
            list.connect_row_selected(move |_, row| {
                if quiet.get() {
                    return;
                }
                let Some(row) = row else { return };
                if Some(row) == contacts_row.as_ref() {
                    s.input(SidebarInput::ContactsRowClicked);
                } else if Some(row) == attachments_row.as_ref() {
                    s.input(SidebarInput::AttachmentsRowSelected);
                }
            });
            footer.append(&list);
            self.footer_list = Some(list);
        }

        // "Outbox" row — only while something is waiting to be sent. It sits
        // directly above the accounts so a stuck message is impossible to miss.
        if self.outbox_count > 0 {
            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::Single);
            list.add_css_class("navigation-sidebar");

            let row = gtk::ListBoxRow::new();
            let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            hbox.add_css_class("folder-row");
            let img = gtk::Image::from_icon_name("co.hyprlab.Hylki-mail-send-symbolic");
            img.add_css_class("folder-icon");
            pin_icon_size(&img);
            let badge = gtk::Label::new(Some(&self.outbox_count.to_string()));
            style_badge(&badge, 5);
            if self.collapsed {
                hbox.set_halign(gtk::Align::Center);
                row.set_tooltip_text(Some(&format!(
                    "Outbox — {} waiting to be sent",
                    self.outbox_count
                )));
                hbox.append(&img);
            } else {
                if self.chevrons_left {
                    img.set_margin_start(ROW_LEFT_INSET);
                }
                hbox.append(&img);
                let label = gtk::Label::new(Some(i18n("Outbox").as_str()));
                label.set_hexpand(true);
                label.set_halign(gtk::Align::Start);
                label.add_css_class("account-name");
                hbox.append(&label);
                hbox.append(&badge);
            }
            row.set_child(Some(&hbox));
            list.append(&row);

            let s = sender.clone();
            let quiet = self.quiet.clone();
            list.connect_row_selected(move |_, row| {
                if quiet.get() {
                    return;
                }
                if row.is_some() {
                    s.input(SidebarInput::OutboxRowSelected);
                }
            });
            container.append(&list);
            self.outbox_list = Some(list);
        }

        // The sections placed in the scrolling sidebar above the accounts —
        // including those meant for All Inboxes when it is hidden (a single
        // account), which would otherwise have nowhere to be.
        let no_unified = !self.show_unified && !self.unified_kinds.any();
        let above = |p: SectionPlacement| p == AboveAccounts || (p == AllInboxes && no_unified);
        // These placements keep the heading style (a caret heading and its
        // rows); "in the unified section" draws them as unified rows.
        let filtered_above = !self.unified_folders.is_empty() && above(self.filtered_placement);
        let tags_above = self.unified_tags_shown() && above(self.tags_placement);
        if filtered_above {
            self.build_filtered_section(container, Slot::Unified, &sections, sender);
        }
        if tags_above {
            self.build_tags_section(container, Slot::Unified, filtered_above, sender);
        }

        // The account sections — unless Settings (or the main menu) has
        // them off, for those who work from the unified section alone.
        let account_sections: Vec<&SectionData> =
            if self.show_accounts { sections.iter().collect() } else { Vec::new() };
        for (section_idx, section) in account_sections.into_iter().enumerate() {
            let id = section.account.id;

            // Header: avatar circle + name/email on the left, chevron on the right.
            let header = gtk::Button::new();
            header.add_css_class("flat");
            header.add_css_class("account-header");
            let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 10);

            // A configured label wins (same as the All Inboxes sub-rows);
            // otherwise the account's name, then its address.
            let name_str = if !section.account.label.trim().is_empty()
                && section.account.label != section.account.email
            {
                section.account.label.clone()
            } else if section.account.name.trim().is_empty() {
                section.account.email.clone()
            } else {
                section.account.name.clone()
            };

            let circle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            circle.add_css_class("account-circle");
            circle.add_css_class(&format!("acct-color-{id}"));
            circle.set_valign(gtk::Align::Center);
            // Keep it a perfect circle: a fixed square that never stretches with
            // the row. (Without this the glyph's hexpand propagates up and the
            // circle widens into an oval whenever a name column sits beside it.)
            circle.set_halign(gtk::Align::Center);
            circle.set_hexpand(false);
            circle.set_size_request(30, 30);
            // Drawn ink-centred (see `ui::initials`), not a label: a lone
            // letter or an emoji sits exactly in the middle of the disc.
            // The account's own Gravatar leads when it asked for one and the
            // address has one (#189); the picture and the emoji are what it
            // falls back to.
            let gravatar = account_gravatar(&section.account.email);
            let glyph: gtk::Widget = match (&gravatar, &section.avatar, &section.emoji) {
                (Some(texture), ..) => {
                    circle.set_overflow(gtk::Overflow::Hidden);
                    crate::ui::initials::picture_from_texture(texture, 30).upcast()
                }
                (None, Some(path), _) => {
                    // A picture fills the disc; the disc's rounded corners
                    // clip it into a circle.
                    circle.set_overflow(gtk::Overflow::Hidden);
                    crate::ui::initials::avatar_picture(path, 30).upcast()
                }
                (None, None, Some(em)) if !em.is_empty() => {
                    crate::ui::initials::glyph_picture(em, &section.color, 0.55, 30).upcast()
                }
                _ => crate::ui::initials::glyph_picture(
                    &account_initials(&name_str, &section.account.email),
                    &section.color,
                    0.47,
                    30,
                )
                .upcast(),
            };
            circle.append(&glyph);
            // While this account's section is collapsed its Inbox row (and the
            // chip on it) is hidden inside the revealer, so surface the inbox
            // unread count on the avatar instead — mirroring how the collapsed
            // "All Inboxes" rail badges its icon.
            let inbox_unread = section
                .folders
                .iter()
                .find(|f| f.kind == FolderKind::Inbox)
                .map(|f| f.unread)
                .unwrap_or(0);
            let (circle_overlay, circle_badge) = with_unread_overlay(&circle, inbox_unread);
            circle_overlay.set_halign(gtk::Align::Center);
            circle_overlay.set_hexpand(false);
            // Folded for the rail (Settings → Sidebar → Icon rail) reads as
            // collapsed here, without touching the saved state.
            let folded = self.account_shown_folded(section);
            circle_badge.set_visible(folded && inbox_unread > 0);
            self.account_circle_badges.insert(id, circle_badge);
            if !self.collapsed && self.chevrons_left {
                // Leaves room for the overlaid disclosure chevron, which
                // would otherwise sit right on top of the avatar — a bit
                // more than the minimum, so the two aren't touching.
                circle_overlay.set_margin_start(ROW_LEFT_INSET + 8);
            }

            // Chevron is tracked even when collapsed so per-account toggles
            // still update an icon; it's only shown in the expanded layout —
            // leading (overlaid on the header's left edge, reserving no
            // layout space, like the All Inboxes row's) or classic trailing,
            // per Settings → Chevron placement.
            let chevron = gtk::Image::from_icon_name(if folded {
                "co.hyprlab.Hylki-pan-end-symbolic"
            } else {
                "co.hyprlab.Hylki-pan-down-symbolic"
            });
            chevron.set_valign(gtk::Align::Center);

            if self.collapsed {
                hbox.append(&circle_overlay);
                hbox.set_halign(gtk::Align::Center);
                header.set_tooltip_text(Some(&name_str));
            } else {
                hbox.append(&circle_overlay);
                let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
                vbox.set_hexpand(true);
                vbox.set_valign(gtk::Align::Center);
                // Clearance for the unread chip that overlays the circle's
                // corner while the section is collapsed.
                vbox.set_margin_start(6);
                let name = gtk::Label::new(Some(&name_str));
                name.set_halign(gtk::Align::Start);
                name.set_ellipsize(gtk::pango::EllipsizeMode::End);
                name.add_css_class("account-name");
                let email = gtk::Label::new(Some(&section.account.email));
                email.set_halign(gtk::Align::Start);
                email.set_ellipsize(gtk::pango::EllipsizeMode::End);
                email.add_css_class("account-email");
                vbox.append(&name);
                vbox.append(&email);
                hbox.append(&vbox);
                if self.chevrons_left {
                    chevron.add_css_class("row-disclosure-chevron");
                    chevron.set_pixel_size(15);
                    chevron.set_halign(gtk::Align::Start);
                    let overlay = gtk::Overlay::new();
                    overlay.set_child(Some(&hbox));
                    overlay.add_overlay(&chevron);
                    header.set_child(Some(&overlay));
                } else {
                    hbox.append(&chevron);
                }
            }

            if header.child().is_none() {
                header.set_child(Some(&hbox));
            }
            let s = sender.input_sender().clone();
            header.connect_clicked(move |_| {
                let _ = s.send(SidebarInput::ToggleCollapseLocal(id));
            });
            // Right-click an account: act on the account / its inbox.
            let inbox_id = section.folders.iter().find(|f| f.kind == FolderKind::Inbox).map(|f| f.id);
            let click = gtk::GestureClick::new();
            click.set_button(gtk::gdk::BUTTON_SECONDARY);
            let cs = sender.clone();
            let header_w = header.clone();
            click.connect_pressed(move |_, _, x, y| {
                let mut items: Vec<(&str, CtxAction)> = Vec::new();
                if let Some(fid) = inbox_id {
                    items.push((i18n_noop("Mark Inbox as Read"), CtxAction::MarkFolderRead {
                        account_id: id,
                        folder_id: fid,
                    }));
                    items.push((i18n_noop("Refresh"), CtxAction::RefreshFolder {
                        account_id: id,
                        folder_id: fid,
                    }));
                }
                items.push((i18n_noop("New Folder…"), CtxAction::NewFolder(id)));
                items.push((i18n_noop("Account Settings…"), CtxAction::OpenAccountSettings(id)));
                items.push((i18n_noop("Remove Account…"), CtxAction::RemoveAccount(id)));
                show_sidebar_menu(&header_w, x, y, items, &cs);
            });
            header.add_controller(click);

            // While dragging a message, hovering the account header for 500ms
            // expands a collapsed account so its folders become drop targets.
            let motion = gtk::DropControllerMotion::new();
            let ms = sender.input_sender().clone();
            let timer: std::rc::Rc<std::cell::RefCell<Option<gtk::glib::SourceId>>> =
                std::rc::Rc::new(std::cell::RefCell::new(None));
            let t_enter = timer.clone();
            motion.connect_enter(move |_, _, _| {
                if t_enter.borrow().is_some() {
                    return;
                }
                let ms = ms.clone();
                let t = t_enter.clone();
                let src = gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(500),
                    move || {
                        *t.borrow_mut() = None;
                        let _ = ms.send(SidebarInput::ExpandForDrop(id));
                    },
                );
                *t_enter.borrow_mut() = Some(src);
            });
            let t_leave = timer.clone();
            motion.connect_leave(move |_| {
                if let Some(src) = t_leave.borrow_mut().take() {
                    src.remove();
                }
            });
            header.add_controller(motion);
            // The first account sits 10px further below the unified block
            // above it (when there is one), so the two read as separate
            // groups (Jason, 2026-09-13).
            if section_idx == 0 && container.first_child().is_some() {
                header.set_margin_top(10);
            }
            container.append(&header);

            // Animated folder list.
            let revealer = gtk::Revealer::new();
            revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
            revealer.set_transition_duration(0);
            revealer.set_reveal_child(!folded);

            // Split folders: essential (Inbox/Sent/Trash/Archive/…) are always
            // shown; user-created "custom" folders are tucked under a collapsible
            // "Folders" section. `section.folders` is already essential-first, so
            // the essential list holds row indices 0..E and the custom list E..
            let essential: Vec<&Folder> = section
                .folders
                .iter()
                .filter(|f| f.kind != FolderKind::Custom)
                .collect();
            let custom: Vec<&Folder> = section
                .folders
                .iter()
                .filter(|f| f.kind == FolderKind::Custom)
                .collect();
            let e = essential.len() as i32;

            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::Single);
            list.add_css_class("navigation-sidebar");
            for folder in &essential {
                let icon = filter_icon(section, folder);
                let (row, badge) =
                    build_folder_row(folder, self.collapsed, 0, None, self.chevrons_left, icon);
                row.add_controller(folder_drop_target(id, folder.path.clone(), sender));
                list.append(&row);
                if let Some(badge) = badge {
                    self.folder_badges.insert((id, folder.id), badge);
                }
            }
            let s2 = sender.input_sender().clone();
            let quiet = self.quiet.clone();
            list.connect_row_selected(move |_, row| {
                if quiet.get() {
                    return;
                }
                if let Some(row) = row {
                    let _ = s2.send(SidebarInput::FolderRowSelected {
                        account_id: id,
                        index: row.index(),
                    });
                }
            });
            attach_folder_context_menu(
                &list,
                id,
                essential.iter().map(|f| (*f).clone()).collect(),
                section.filtered.iter().map(|f| f.path.clone()).collect(),
                section.has_filters,
                sender,
            );

            // The collapsible custom-folders list + its "Folders" toggle header.
            let custom_list = gtk::ListBox::new();
            custom_list.set_selection_mode(gtk::SelectionMode::Single);
            custom_list.add_css_class("navigation-sidebar");
            let custom_revealer = gtk::Revealer::new();
            let custom_chevron = gtk::Image::from_icon_name(if section.custom_expanded {
                "co.hyprlab.Hylki-pan-down-symbolic"
            } else {
                "co.hyprlab.Hylki-pan-end-symbolic"
            });
            let folders_toggle = gtk::Button::new();
            if !custom.is_empty() {
                let collapsed_nodes =
                    self.tree_collapsed.get(&id).cloned().unwrap_or_default();
                for folder in &custom {
                    let depth = folder_depth(folder, &custom);
                    // The expander slot (#51): a chevron for folders with
                    // sub-folders, an equal-width spacer for leaves so names
                    // at one depth stay aligned. Rail mode has no room for
                    // either.
                    let has_children =
                        custom.iter().any(|g| path_is_under(&g.path, &folder.path));
                    let lead: Option<gtk::Widget> = if self.collapsed {
                        None
                    } else if has_children {
                        // One right-pointing caret; the "open" class rotates it
                        // 90° via a CSS transition, so toggling spins smoothly
                        // instead of swapping glyphs.
                        let img = gtk::Image::from_icon_name("co.hyprlab.Hylki-pan-end-symbolic");
                        img.add_css_class("tree-expander-icon");
                        if !collapsed_nodes.contains(&folder.path) {
                            img.add_css_class("open");
                        }
                        img.set_pixel_size(12);
                        let btn = gtk::Button::new();
                        btn.set_child(Some(&img));
                        btn.add_css_class("flat");
                        btn.add_css_class("tree-expander");
                        btn.set_valign(gtk::Align::Center);
                        btn.set_tooltip_text(Some(i18n("Show or hide sub-folders").as_str()));
                        let st = sender.input_sender().clone();
                        let path = folder.path.clone();
                        btn.connect_clicked(move |_| {
                            let _ = st.send(SidebarInput::ToggleFolderNode {
                                account_id: id,
                                path: path.clone(),
                            });
                        });
                        self.tree_chevrons.insert((id, folder.path.clone()), img);
                        Some(btn.upcast())
                    } else {
                        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                        spacer.set_width_request(TREE_EXPANDER_WIDTH);
                        Some(spacer.upcast())
                    };
                    let (row, badge) =
                        build_folder_row(
                            folder,
                            self.collapsed,
                            depth,
                            lead.as_ref(),
                            self.chevrons_left,
                            filter_icon(section, folder),
                        );
                    // Hidden while any ancestor is collapsed; the row still
                    // exists, so selection indices stay stable. Its content
                    // sits in a revealer so user toggles slide open/closed —
                    // built with no transition (rebuilds must reach full
                    // height in one pass; see the scroll-jump saga), armed to
                    // 200ms alongside the section revealers below.
                    let hidden = hidden_by_collapse(&folder.path, &collapsed_nodes);
                    if let Some(content) = row.child() {
                        row.set_child(gtk::Widget::NONE);
                        let rev = gtk::Revealer::new();
                        rev.set_transition_type(gtk::RevealerTransitionType::SlideDown);
                        rev.set_transition_duration(0);
                        rev.set_child(Some(&content));
                        rev.set_reveal_child(!hidden);
                        row.set_child(Some(&rev));
                        self.tree_row_revealers
                            .entry(id)
                            .or_default()
                            .push(rev);
                    }
                    row.set_visible(!hidden);
                    row.add_controller(folder_drop_target(id, folder.path.clone(), sender));
                    // Custom folders can be picked up and dropped on a new
                    // parent (#51); essential folders stay where the server
                    // put them.
                    if !self.collapsed {
                        let drag = gtk::DragSource::new();
                        drag.set_actions(gtk::gdk::DragAction::MOVE);
                        let payload = format!("vireo-folder\t{id}\t{}", folder.path);
                        drag.connect_prepare(move |_, _, _| {
                            Some(gtk::gdk::ContentProvider::for_value(&payload.to_value()))
                        });
                        row.add_controller(drag);
                    }
                    custom_list.append(&row);
                    if let Some(badge) = badge {
                        self.folder_badges.insert((id, folder.id), badge);
                    }
                }
                self.custom_folders
                    .insert(id, custom.iter().map(|f| (*f).clone()).collect());
                let s3 = sender.input_sender().clone();
                let quiet = self.quiet.clone();
                custom_list.connect_row_selected(move |_, row| {
                    if quiet.get() {
                        return;
                    }
                    if let Some(row) = row {
                        // Offset past the essential folders into `section.folders`.
                        let _ = s3.send(SidebarInput::FolderRowSelected {
                            account_id: id,
                            index: e + row.index(),
                        });
                    }
                });
                let s4 = sender.input_sender().clone();
                let quiet = self.quiet.clone();
                custom_list.connect_row_activated(move |_, row| {
                    if quiet.get() {
                        return;
                    }
                    let _ = s4.send(SidebarInput::FolderRowActivated {
                        account_id: id,
                        index: row.index(),
                    });
                });
                attach_folder_context_menu(
                    &custom_list,
                    id,
                    custom.iter().map(|f| (*f).clone()).collect(),
                    section.filtered.iter().map(|f| f.path.clone()).collect(),
                    section.has_filters,
                    sender,
                );

                custom_revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
                custom_revealer.set_transition_duration(0);
                custom_revealer.set_reveal_child(section.custom_expanded);
                custom_revealer.set_child(Some(&custom_list));

                folders_toggle.add_css_class("flat");
                folders_toggle.add_css_class("folders-toggle");
                let hb = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                hb.add_css_class("folder-row");
                if self.collapsed {
                    hb.set_halign(gtk::Align::Center);
                    hb.append(&gtk::Image::from_icon_name("co.hyprlab.Hylki-folder-symbolic"));
                    folders_toggle.set_tooltip_text(Some(i18n("Folders").as_str()));
                } else {
                    if self.chevrons_left {
                        // A chevron glyph's ink sits further into its canvas
                        // than a regular icon's, so the folder rows' full
                        // inset would land it visually right of the icons
                        // above — a smaller value keeps the same column.
                        custom_chevron.set_margin_start(2);
                    }
                    hb.append(&custom_chevron);
                    let lbl = gtk::Label::new(Some(&i18n_f("Folders ({len})", &[("len", &(custom.len()).to_string())])));
                    lbl.set_halign(gtk::Align::Start);
                    lbl.set_hexpand(true);
                    hb.append(&lbl);
                }
                folders_toggle.set_child(Some(&hb));
                let st = sender.input_sender().clone();
                folders_toggle.connect_clicked(move |_| {
                    let _ = st.send(SidebarInput::ToggleCustomFoldersLocal(id));
                });
                // Dropping a folder on the section header moves it to the
                // account's top level ("" — resolved to the namespace root).
                folders_toggle.add_controller(folder_drop_target(id, String::new(), sender));
            }

            // "+ Add Folder" button at the bottom of the list for quick creation.
            let add_btn = gtk::Button::new();
            add_btn.add_css_class("flat");
            add_btn.add_css_class("add-folder-btn");
            let add_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            add_box.add_css_class("folder-row");
            let add_img = gtk::Image::from_icon_name("co.hyprlab.Hylki-list-add-symbolic");
            pin_icon_size(&add_img);
            add_box.append(&add_img);
            if self.collapsed {
                add_box.set_halign(gtk::Align::Center);
                add_btn.set_tooltip_text(Some(i18n("Add Folder").as_str()));
            } else {
                if self.chevrons_left {
                    add_img.set_margin_start(4);
                }
                let lbl = gtk::Label::new(Some(i18n("Add Folder").as_str()));
                lbl.set_halign(gtk::Align::Start);
                lbl.set_hexpand(true);
                add_box.append(&lbl);
            }
            add_btn.set_child(Some(&add_box));
            let cs = sender.clone();
            add_btn.connect_clicked(move |_| {
                let _ = cs.output(SidebarOutput::Context(CtxAction::NewFolder(id)));
            });

            let wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);
            wrap.append(&list);
            // The account's own Tags section, above its folder hierarchy:
            // every tag scoped to this account, there whatever the unified
            // section shows. (Its filtered folders are marked in place in
            // the hierarchy instead — see `filter_icon`.)
            if !self.tags.is_empty() {
                self.build_tags_section(&wrap, Slot::Account(id), false, sender);
            }
            if !custom.is_empty() {
                wrap.append(&folders_toggle);
                wrap.append(&custom_revealer);
            }
            wrap.append(&add_btn);
            revealer.set_child(Some(&wrap));
            container.append(&revealer);

            self.revealers.insert(id, revealer);
            self.chevrons.insert(id, chevron);
            self.folder_lists.insert(id, list);
            self.custom_folder_lists.insert(id, custom_list);
            self.custom_revealers.insert(id, custom_revealer);
            self.custom_chevrons.insert(id, custom_chevron);
        }

        // And the sections placed after the last account.
        let filtered_below =
            !self.unified_folders.is_empty() && self.filtered_placement == BelowAccounts;
        if filtered_below {
            self.build_filtered_section(container, Slot::Unified, &sections, sender);
        }
        if self.unified_tags_shown() && self.tags_placement == BelowAccounts {
            self.build_tags_section(container, Slot::Unified, filtered_below, sender);
        }

        // Per-account avatar colours (background + readable text).
        let mut css = String::new();
        for s in &sections {
            let text = crate::color::readable_text(&s.color);
            css.push_str(&format!(
                ".acct-color-{0} {{ background-color: {1}; }} \
                 .acct-color-{0} label {{ color: {2}; }} \
                 .acct-tint-{0} {{ color: {1}; }}\n",
                s.account.id, s.color, text
            ));
        }
        self.color_provider.load_from_data(&css);

        // The revealers were built with no transition so the rebuilt content
        // reaches full height in the very first layout pass; hand them their
        // real animation back once that pass is done, for user toggles.
        {
            let revs: Vec<gtk::Revealer> = self
                .revealers
                .values()
                .chain(self.custom_revealers.values())
                .chain(self.tree_row_revealers.values().flatten())
                .cloned()
                .chain(self.unified_revealer.clone())
                .chain(self.filtered_sections.values().map(|w| w.revealer.clone()))
                .chain(self.tag_sections.values().map(|w| w.revealer.clone()))
                .chain(self.kind_widgets.values().map(|w| w.revealer.clone()))
                .collect();
            gtk::glib::idle_add_local_once(move || {
                for r in &revs {
                    r.set_transition_duration(200);
                }
            });
        }

        // Restore the scroll offset before anything paints: an idle-time
        // restore let one frame render at the top first — a visible flash on
        // every rebuild. The adjustment's `changed` signal fires while the
        // fresh rows are being measured (same layout pass), so pinning the
        // value there means no frame ever shows the wrong offset. The pin
        // holds through the freeze-frame window — layout can keep settling
        // for a few frames — then a timer finalises and disconnects.
        if let (Some(pos), Some(scroller)) = (saved_scroll, scroller) {
            if pos > 0.0 {
                let adj = scroller.vadjustment();
                adj.set_value(pos);
                let handler: std::rc::Rc<std::cell::RefCell<Option<gtk::glib::SignalHandlerId>>> =
                    std::rc::Rc::new(std::cell::RefCell::new(None));
                *handler.borrow_mut() = Some(adj.connect_changed(move |adj| {
                    adj.set_value(pos);
                }));
                let adj = scroller.vadjustment();
                let handler = handler.clone();
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(120),
                    move || {
                        adj.set_value(pos);
                        if let Some(id) = handler.borrow_mut().take() {
                            adj.disconnect(id);
                        }
                    },
                );
            }
        }
    }

    /// Re-apply the current selection after a rebuild; on first populate (no
    /// prior selection) default to the unified row if shown, else the first folder.
    /// One row of the unified section — All Inboxes, Starred / Sent /
    /// Drafts, Filtered Folders or Tags — with its expandable list beneath:
    /// the header row selects the merged view, the caret opens the list
    /// (each account's folder of that kind, each filtered folder, each
    /// tag). In the icon-only rail a small toggle button under the icon
    /// stands in for the in-row chevron.
    fn build_unified_row(
        &mut self,
        parent: &gtk::Box,
        row_kind: UnifiedRow,
        sections: &[SectionData],
        sender: &ComponentSender<Self>,
    ) -> KindWidgets {
        let is_inbox = row_kind == UnifiedRow::Kind(FolderKind::Inbox);
        let title = row_title(row_kind);
        // Sent wears no unread chip anywhere: what you sent is not new mail.
        let counted = row_kind != UnifiedRow::Kind(FolderKind::Sent);
        let unread = self.row_unread(row_kind);
        let expanded = self.row_shown_open(row_kind);
        let show_chip = unread > 0 && !expanded && self.chip_shown(row_kind);
        let toggle_tip = match row_kind {
            UnifiedRow::Kind(FolderKind::Inbox) => i18n("Show each inbox"),
            UnifiedRow::Kind(_) => i18n("Show each account"),
            UnifiedRow::Filtered => i18n("Show each folder"),
            UnifiedRow::Tags => i18n("Show each tag"),
        };

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.add_css_class("navigation-sidebar");
        // The unified rows stack with no gap between their pills, as
        // folder rows in one list do (see styles.css).
        list.add_css_class("unified-item");

        let row = gtk::ListBoxRow::new();
        // Tagged so the disclosure chevron can be lined up with the
        // account headers' (see styles.css).
        row.add_css_class("unified-row");
        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        hbox.add_css_class("folder-row");
        let img = gtk::Image::from_icon_name(row_icon(row_kind));
        img.add_css_class("folder-icon");
        let mut chevron_img: Option<gtk::Image> = None;
        let badge: gtk::Label;
        if self.collapsed {
            hbox.set_halign(gtk::Align::Center);
            let mut tip = if unread > 0 { format!("{title} ({unread})") } else { title.clone() };
            // The rail has no room for a chevron: a long press on the icon
            // opens or folds the list instead.
            tip.push('\n');
            tip.push_str(&i18n("Long-press to expand or collapse"));
            row.set_tooltip_text(Some(&tip));
            // Total-unread chip overlaid on the icon so the count stays
            // visible in the icon-only rail.
            let (overlay, b) = with_unread_overlay(&img, unread);
            b.set_visible(show_chip);
            hbox.append(&overlay);
            badge = b;
        } else {
            // The disclosure chevron toggling the list — its own button
            // either way (selecting the merged view and expanding it stay
            // separate actions). Placement follows Settings → Chevron
            // placement: leading (overlaid on the row's left edge — see
            // below), or classic trailing.
            let chevron = gtk::Image::from_icon_name(chevron_icon(expanded));
            let chev_btn = gtk::Button::new();
            chev_btn.set_child(Some(&chevron));
            chev_btn.add_css_class("flat");
            chev_btn.add_css_class("chevron-btn");
            chev_btn.set_valign(gtk::Align::Center);
            chev_btn.set_tooltip_text(Some(toggle_tip.as_str()));
            let cs = sender.input_sender().clone();
            chev_btn.connect_clicked(move |_| {
                let _ = cs.send(toggle_msg(row_kind));
            });
            row.add_css_class(if self.chevrons_left { "chev-left" } else { "chev-right" });
            pin_icon_size(&img);
            if self.chevrons_left {
                // Centers this 16px icon on the avatar circles below it
                // (rather than matching left edges) — a small icon flush
                // with a much wider circle's left edge reads as
                // off-centre next to it (PR #95).
                img.set_margin_start(ROW_LEFT_INSET + 9);
            }
            hbox.append(&img);
            let label = gtk::Label::new(Some(&title));
            label.set_halign(gtk::Align::Start);
            label.set_hexpand(true);
            // Squeezed by a wide chip, the title shortens; the chip and the
            // chevron never leave the row.
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.add_css_class("account-name");
            if self.chevrons_left {
                label.set_margin_start(-2);
            }
            hbox.append(&label);
            row.set_tooltip_text(Some(i18n("Long-press to expand or collapse").as_str()));
            chevron_img = Some(chevron.clone());
            // The total-unread chip right-aligns like every folder row's,
            // one shared column down the sidebar. While the list is
            // expanded its rows carry the counts, so the total is
            // redundant and hidden.
            let b = gtk::Label::new(Some(&unread.to_string()));
            style_badge(&b, 5);
            b.set_visible(show_chip);
            hbox.append(&b);
            badge = b;
            if self.chevrons_left {
                // Overlaid on the row's left edge rather than packed into
                // the layout — the same trick as the rail's unread badges
                // — so it reserves no space of its own: icon and label
                // keep their normal position and the chip keeps the
                // shared flush-right column (Isaac's PR #95 mechanism).
                chevron.set_pixel_size(15);
                chev_btn.add_css_class("row-disclosure-chevron");
                chev_btn.set_halign(gtk::Align::Start);
                let overlay = gtk::Overlay::new();
                overlay.set_child(Some(&hbox));
                overlay.add_overlay(&chev_btn);
                row.set_child(Some(&overlay));
            } else {
                hbox.append(&chev_btn);
            }
        }
        if row.child().is_none() {
            row.set_child(Some(&hbox));
        }
        list.append(&row);

        let s = sender.input_sender().clone();
        let quiet = self.quiet.clone();
        list.connect_row_selected(move |_, row| {
            if quiet.get() {
                return;
            }
            if row.is_some() {
                let _ = s.send(select_msg(row_kind));
            }
        });
        // Double-click toggles the list, same as the chevron (single click
        // only selects; activation needs the double-click once single-click
        // activation is off).
        list.set_activate_on_single_click(false);
        let s2 = sender.input_sender().clone();
        let quiet = self.quiet.clone();
        list.connect_row_activated(move |_, _| {
            if quiet.get() {
                return;
            }
            let _ = s2.send(toggle_msg(row_kind));
        });
        if is_inbox {
            // Right-click "All Inboxes": act on every inbox at once.
            let click = gtk::GestureClick::new();
            click.set_button(gtk::gdk::BUTTON_SECONDARY);
            let cs = sender.clone();
            let list_w = list.clone();
            click.connect_pressed(move |_, _, x, y| {
                show_sidebar_menu(
                    &list_w,
                    x,
                    y,
                    vec![
                        (i18n_noop("Mark All as Read"), CtxAction::MarkAllInboxesRead),
                        (i18n_noop("Refresh"), CtxAction::RefreshAllInboxes),
                    ],
                    &cs,
                );
            });
            list.add_controller(click);
        }
        parent.append(&list);

        // A long press on the row opens or folds the list in either layout
        // (a plain click still selects the merged view): in the rail it is
        // the only way, there being no room for a chevron; in the full
        // sidebar it sits alongside the chevron.
        {
            let press = gtk::GestureLongPress::new();
            press.set_touch_only(false);
            let cs = sender.input_sender().clone();
            press.connect_pressed(move |g, _, _| {
                // Claimed, so the release doesn't also count as a click.
                g.set_state(gtk::EventSequenceState::Claimed);
                let _ = cs.send(toggle_msg(row_kind));
            });
            list.add_controller(press);
        }

        // The list beneath: the same row shape for all — a lead (account
        // pill, tinted folder glyph, tag disc), a name, an unread chip.
        let sub = gtk::ListBox::new();
        sub.set_selection_mode(gtk::SelectionMode::Single);
        sub.add_css_class("navigation-sidebar");
        sub.add_css_class("unified-item");
        let mut rows: Vec<InboxRef> = Vec::new();
        let mut row_badges: HashMap<(u32, u32), gtk::Label> = HashMap::new();
        match row_kind {
            UnifiedRow::Kind(kind) => {
                for section in sections {
                    let Some(folder) = section.folders.iter().find(|f| f.kind == kind) else {
                        continue;
                    };
                    let aid = section.account.id;
                    let (row, badge) =
                        build_unified_inbox_row(section, folder, self.collapsed, self.chevrons_left);
                    // A message dropped here moves to that account's folder
                    // (the app declines mail from other accounts, #23).
                    row.add_controller(folder_drop_target(aid, folder.path.clone(), sender));
                    sub.append(&row);
                    if counted {
                        row_badges.insert((aid, folder.id), badge);
                    } else {
                        badge.set_visible(false);
                        row.set_tooltip_text(None);
                    }
                    rows.push(InboxRef {
                        account_id: aid,
                        folder_id: folder.id,
                        name: folder.name.clone(),
                        path: folder.path.clone(),
                    });
                }
                let ss = sender.input_sender().clone();
                let quiet = self.quiet.clone();
                sub.connect_row_selected(move |_, row| {
                    if quiet.get() {
                        return;
                    }
                    if let Some(row) = row {
                        let _ = ss.send(if is_inbox {
                            SidebarInput::UnifiedInboxRowSelected(row.index())
                        } else {
                            SidebarInput::KindSubRowSelected { kind, index: row.index() }
                        });
                    }
                });
                // Right-click a row: act on that account's folder.
                let click = gtk::GestureClick::new();
                click.set_button(gtk::gdk::BUTTON_SECONDARY);
                let cs = sender.clone();
                let sub_w = sub.clone();
                let refs = rows.clone();
                click.connect_pressed(move |_, _, x, y| {
                    if let Some(r) = sub_w
                        .row_at_y(y as i32)
                        .and_then(|row| refs.get(row.index() as usize))
                    {
                        show_sidebar_menu(
                            &sub_w,
                            x,
                            y,
                            vec![
                                (i18n_noop("Mark as Read"), CtxAction::MarkFolderRead {
                                    account_id: r.account_id,
                                    folder_id: r.folder_id,
                                }),
                                (i18n_noop("Refresh"), CtxAction::RefreshFolder {
                                    account_id: r.account_id,
                                    folder_id: r.folder_id,
                                }),
                                (i18n_noop("Account Settings…"), CtxAction::OpenAccountSettings(r.account_id)),
                            ],
                            &cs,
                        );
                    }
                });
                sub.add_controller(click);
            }
            UnifiedRow::Filtered => {
                let refs = self.unified_folders.clone();
                for r in &refs {
                    let Some(section) = sections.iter().find(|s| s.account.id == r.account_id) else {
                        continue;
                    };
                    // The glyph in the account's colour says whose folder
                    // this is; the tooltip names the account.
                    let icon = filtered_folder_icon(&r.folder, section.account.id);
                    pin_icon_size(&icon);
                    let tip = format!("{} \u{2014} {}", r.folder.name, section.account.label);
                    let (row, badge) = build_unified_sub_row(
                        &icon,
                        &r.folder.name,
                        &tip,
                        r.folder.unread,
                        self.collapsed,
                        self.chevrons_left,
                        false,
                    );
                    // Filtered folders take drops like any folder of their account.
                    row.add_controller(folder_drop_target(r.account_id, r.folder.path.clone(), sender));
                    sub.append(&row);
                    row_badges.insert((r.account_id, r.folder.id), badge);
                }
                self.filtered_badges.insert(Slot::Unified, row_badges.clone());
                let ss = sender.input_sender().clone();
                let quiet = self.quiet.clone();
                sub.connect_row_selected(move |_, row| {
                    if quiet.get() {
                        return;
                    }
                    if let Some(row) = row {
                        let _ = ss.send(SidebarInput::FilteredRowSelected {
                            slot: Slot::Unified,
                            index: row.index(),
                        });
                    }
                });
                // Right-click a filtered folder: act on that folder alone.
                let click = gtk::GestureClick::new();
                click.set_button(gtk::gdk::BUTTON_SECONDARY);
                let cs = sender.clone();
                let sub_w = sub.clone();
                click.connect_pressed(move |_, _, x, y| {
                    if let Some(r) = sub_w
                        .row_at_y(y as i32)
                        .and_then(|row| refs.get(row.index() as usize))
                    {
                        // The same menu the folder has under its account.
                        let items = folder_menu_items(r.account_id, &r.folder, true, true);
                        show_sidebar_menu(&sub_w, x, y, items, &cs);
                    }
                });
                sub.add_controller(click);
            }
            UnifiedRow::Tags => {
                for t in &self.tags {
                    let disc = crate::ui::context_menu::swatch_widget(&t.color, true);
                    let (row, badge) = build_unified_sub_row(
                        &disc,
                        &t.name,
                        &t.name,
                        0,
                        self.collapsed,
                        self.chevrons_left,
                        false,
                    );
                    badge.set_visible(false);
                    sub.append(&row);
                }
                let ss = sender.input_sender().clone();
                let quiet = self.quiet.clone();
                sub.connect_row_selected(move |_, row| {
                    if quiet.get() {
                        return;
                    }
                    if let Some(row) = row {
                        let _ = ss.send(SidebarInput::TagRowSelected {
                            slot: Slot::Unified,
                            index: row.index(),
                        });
                    }
                });
                attach_tag_context_menu(
                    &sub,
                    self.tags.iter().map(|t| t.keyword.clone()).collect(),
                    sender,
                );
            }
        }

        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        // 0 during the rebuild: an animated reveal grows the content's
        // height over 200ms, which drags the scroll toward the top
        // mid-rebuild. Real duration restored one frame later.
        revealer.set_transition_duration(0);
        revealer.set_reveal_child(expanded);
        revealer.set_child(Some(&sub));
        parent.append(&revealer);

        KindWidgets {
            header: list,
            revealer,
            chevron: chevron_img,
            badge: Some(badge),
            list: sub,
            rows,
            row_badges,
        }
    }

    /// Whether a unified row's list is open. All Inboxes, Filtered Folders
    /// and Tags keep their own state (persisted, folded by the rail); the
    /// Starred / Sent / Drafts rows live in `kind_expanded`.
    fn row_open(&self, row: UnifiedRow) -> bool {
        match row {
            UnifiedRow::Kind(FolderKind::Inbox) => self.unified_expanded,
            UnifiedRow::Kind(kind) => self.kind_open(kind),
            UnifiedRow::Filtered => self.unified_folders_expanded,
            UnifiedRow::Tags => self.tags_expanded,
        }
    }

    /// Whether "Fold up expanded items" starts this row folded in the rail:
    /// the sidebar is the icon rail and the row's switch is on. Such a row
    /// keeps its own rail-only open state (`rail_open`).
    fn locked_row(&self, row: UnifiedRow) -> bool {
        self.collapsed
            && match row {
                UnifiedRow::Kind(kind) => self.rail_fold.folds_kind(kind),
                UnifiedRow::Filtered => self.rail_fold.folds_filtered(),
                UnifiedRow::Tags => self.rail_fold.folds_tags(),
            }
    }

    /// Whether "Fold up expanded items" holds the accounts folded.
    fn locked_accounts(&self) -> bool {
        self.collapsed && self.rail_fold.folds_accounts()
    }

    /// Whether a unified row's list shows open. In the rail: what it was
    /// last set to there, else folded when "Fold up expanded items" covers
    /// it, else its saved state. In the full sidebar: its saved state.
    fn row_shown_open(&self, row: UnifiedRow) -> bool {
        if !self.collapsed {
            return self.row_open(row);
        }
        match self.rail_open.get(&row) {
            Some(open) => *open,
            None => !self.locked_row(row) && self.row_open(row),
        }
    }

    /// Whether an account's section shows folded: the same rule, with the
    /// rail-only state in `rail_open_accounts`.
    fn account_shown_folded(&self, section: &SectionData) -> bool {
        if !self.collapsed {
            return section.collapsed;
        }
        match self.rail_open_accounts.get(&section.account.id) {
            Some(open) => !*open,
            None => self.locked_accounts() || section.collapsed,
        }
    }

    fn set_row_open(&mut self, row: UnifiedRow, open: bool) {
        match row {
            UnifiedRow::Kind(FolderKind::Inbox) => self.unified_expanded = open,
            UnifiedRow::Kind(kind) => {
                self.kind_expanded.insert(kind, open);
            }
            UnifiedRow::Filtered => self.unified_folders_expanded = open,
            UnifiedRow::Tags => self.tags_expanded = open,
        }
    }

    /// The unread total a unified row's folded chip shows.
    fn row_unread(&self, row: UnifiedRow) -> u32 {
        match row {
            UnifiedRow::Kind(FolderKind::Inbox) => self.unified_unread,
            UnifiedRow::Kind(kind) => self.kind_unread(kind),
            UnifiedRow::Filtered => self.filtered_unread(Slot::Unified),
            UnifiedRow::Tags => 0,
        }
    }

    /// Open or fold a unified row's list in place (a click on its chevron,
    /// a double-click on the row), and report the section states.
    fn toggle_row(&mut self, row: UnifiedRow, sender: &ComponentSender<Self>) {
        let open = !self.row_shown_open(row);
        if self.collapsed {
            // Opened or folded for the rail alone: the saved state is
            // untouched, and the full sidebar comes back as it was left.
            self.rail_open.insert(row, open);
        } else {
            self.set_row_open(row, open);
            self.report_sections(sender);
        }
        let unread = self.row_unread(row);
        if row == UnifiedRow::Kind(FolderKind::Inbox) {
            if let Some(rev) = &self.unified_revealer {
                rev.set_reveal_child(open);
            }
            if let Some(label) = &self.unified_badge {
                label.set_visible(unread > 0 && !open && self.unified_chips.all_inboxes);
            }
            if let Some(ch) = &self.unified_chevron {
                ch.set_icon_name(Some(chevron_icon(open)));
            }
        } else if let Some(w) = self.kind_widgets.get(&row) {
            w.revealer.set_reveal_child(open);
            // Expanded: the list shows each count, so the total chip bows
            // out; it returns when folded back up.
            if let Some(b) = &w.badge {
                b.set_visible(unread > 0 && !open && self.chip_shown(row));
            }
            if let Some(ch) = &w.chevron {
                ch.set_icon_name(Some(chevron_icon(open)));
            }
        }
    }

    /// Build a run of unified rows into `parent`, the first row keeping the
    /// list's top padding as the gap above the run and the last row's list
    /// carrying the gap to whatever follows.
    fn build_unified_run(
        &mut self,
        parent: &gtk::Box,
        rows: &[UnifiedRow],
        sections: &[SectionData],
        sender: &ComponentSender<Self>,
    ) {
        let n = rows.len();
        for (i, row) in rows.iter().copied().enumerate() {
            let built = self.build_unified_row(parent, row, sections, sender);
            if i == 0 {
                built.header.add_css_class("unified-first");
            }
            if i + 1 == n {
                // 14 plus the 6 the list's own padding used to give.
                built.list.set_margin_bottom(20);
            }
            if row == UnifiedRow::Kind(FolderKind::Inbox) {
                // All Inboxes keeps its own fields: the in-place updates and
                // the tray's "View all unread" address it directly.
                self.unified_list = Some(built.header);
                self.unified_revealer = Some(built.revealer);
                self.unified_chevron = built.chevron;
                self.unified_badge = built.badge;
                self.unified_inbox_list = Some(built.list);
                self.unified_inboxes = built.rows;
                self.unified_inbox_badges = built.row_badges;
            } else {
                self.kind_widgets.insert(row, built);
            }
        }
    }

    /// A Filtered Folders section — the unified one, or an account's own:
    /// a toggle header and, under an animated revealer, one row per folder
    /// (the filter-folder glyph in the account's colour, the folder name,
    /// its unread chip). In the rail the header is a glyph button and the
    /// rows are glyphs. The unified heading reads like the All Inboxes row
    /// (full-strength label); an account's reads like its "Folders" heading.
    fn build_filtered_section(
        &mut self,
        parent: &gtk::Box,
        slot: Slot,
        sections: &[SectionData],
        sender: &ComponentSender<Self>,
    ) {
        let rows = self.filtered_rows.get(&slot).cloned().unwrap_or_default();
        let open = self.filtered_open(slot);
        let unread: u32 = rows.iter().map(|r| r.folder.unread).sum();
        let unified = slot == Slot::Unified;
        let chevron = gtk::Image::from_icon_name(chevron_icon(open));
        let toggle = gtk::Button::new();
        toggle.add_css_class("flat");
        toggle.add_css_class("folders-toggle");
        if unified {
            toggle.add_css_class("unified-folders-toggle");
        }
        let hb = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        hb.add_css_class("folder-row");
        // The header's unread chip — the section's total — shows only while
        // the section is folded up, as All Inboxes' does, and only with its
        // switch on (Settings → Sidebar → Unread counts).
        let show_chip = unread > 0 && !open && self.unified_chips.filtered;
        let badge: gtk::Label;
        if self.collapsed {
            // The rail has no room for a label: Jason's filter-folder glyph
            // (a folder with a funnel's bars) carries the toggle alone.
            let icon = gtk::Image::from_icon_name("co.hyprlab.Hylki-filter-folder-symbolic");
            hb.set_halign(gtk::Align::Center);
            let (overlay, b) = with_unread_overlay(&icon, unread);
            b.set_visible(show_chip);
            hb.append(&overlay);
            badge = b;
            toggle.set_tooltip_text(Some(&if unread > 0 {
                format!("{} ({unread})", i18n("Filters"))
            } else {
                i18n("Filters")
            }));
        } else {
            // A leading caret and the label, like the accounts' "Folders"
            // heading; the glyph belongs to the rows beneath.
            if self.chevrons_left {
                // Same nudge as that heading: a chevron's ink sits deeper
                // in its canvas than an icon's.
                chevron.set_margin_start(2);
            }
            hb.append(&chevron);
            let lbl = gtk::Label::new(Some(i18n("Filters").as_str()));
            if unified {
                lbl.add_css_class("account-name");
            }
            lbl.set_halign(gtk::Align::Start);
            lbl.set_hexpand(true);
            lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            hb.append(&lbl);
            let b = gtk::Label::new(Some(&unread.to_string()));
            style_badge(&b, 5);
            b.set_visible(show_chip);
            hb.append(&b);
            badge = b;
        }
        toggle.set_child(Some(&hb));
        let st = sender.input_sender().clone();
        toggle.connect_clicked(move |_| {
            let _ = st.send(SidebarInput::ToggleFilteredExpand(slot));
        });
        if unified {
            toggle.set_margin_bottom(unified_folders_toggle_gap(open));
        }
        parent.append(&toggle);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.add_css_class("navigation-sidebar");
        list.add_css_class("section-child-list");
        // The gap to the first account section below (the inbox sub-list
        // carries 14 when this section is absent; the folder rows' own
        // bottom padding makes 10 read the same here). With a Tags section
        // beneath, 10px keeps these rows off its heading (Jason,
        // 2026-09-07); folded up, the list takes the gap away with it.
        if unified {
            list.set_margin_bottom(10);
        }
        let mut badges: HashMap<(u32, u32), gtk::Label> = HashMap::new();
        for r in &rows {
            let Some(section) = sections.iter().find(|s| s.account.id == r.account_id) else {
                continue;
            };
            let tip = if unified {
                format!("{} \u{2014} {}", r.folder.name, section.account.label)
            } else {
                r.folder.name.clone()
            };
            // Laid out exactly like a folder under an account's "Folders"
            // heading — same builder, same leaf expander slot — so folders
            // read the same wherever they sit in the sidebar. Only the
            // colour differs: the account's, which says whose folder this is.
            let icon = filtered_folder_icon(&r.folder, section.account.id);
            let lead: Option<gtk::Widget> = if self.collapsed {
                None
            } else {
                let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                spacer.set_width_request(TREE_EXPANDER_WIDTH);
                Some(spacer.upcast())
            };
            let (row, badge) = build_folder_row(
                &r.folder,
                self.collapsed,
                0,
                lead.as_ref(),
                self.chevrons_left,
                FolderGlyph::Icon(icon),
            );
            let Some(badge) = badge else { continue };
            // Name the account too: the tint alone is a hint.
            row.set_tooltip_text(Some(&if self.collapsed && r.folder.unread > 0 {
                format!("{tip} ({})", r.folder.unread)
            } else {
                tip.clone()
            }));
            // Filtered folders take drops like any folder of their account.
            row.add_controller(folder_drop_target(r.account_id, r.folder.path.clone(), sender));
            list.append(&row);
            badges.insert((r.account_id, r.folder.id), badge);
        }
        let ss = sender.input_sender().clone();
        let quiet = self.quiet.clone();
        list.connect_row_selected(move |_, row| {
            if quiet.get() {
                return;
            }
            if let Some(row) = row {
                let _ = ss.send(SidebarInput::FilteredRowSelected { slot, index: row.index() });
            }
        });
        // Right-click a filtered folder: the same menu it has under its
        // account.
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_SECONDARY);
        let cs = sender.clone();
        let list_w = list.clone();
        let refs = rows.clone();
        click.connect_pressed(move |_, _, x, y| {
            if let Some(r) = list_w
                .row_at_y(y as i32)
                .and_then(|row| refs.get(row.index() as usize))
            {
                let items = folder_menu_items(r.account_id, &r.folder, true, true);
                show_sidebar_menu(&list_w, x, y, items, &cs);
            }
        });
        list.add_controller(click);

        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        revealer.set_transition_duration(0);
        revealer.set_reveal_child(open);
        revealer.set_child(Some(&list));
        parent.append(&revealer);
        self.filtered_badges.insert(slot, badges);
        self.filtered_sections.insert(
            slot,
            SectionWidgets { revealer, chevron, toggle, list, badge: Some(badge) },
        );
    }

    /// A Tags section (#71) — the unified one, or an account's own: a
    /// toggle header and, under an animated revealer, one row per tag
    /// (colour disc, its name). From the unified section a tag shows every
    /// account's mail with it; from an account's, that account's alone.
    /// In the rail the header is a tag glyph and the rows are discs.
    fn build_tags_section(
        &mut self,
        parent: &gtk::Box,
        slot: Slot,
        after_filtered: bool,
        sender: &ComponentSender<Self>,
    ) {
        let open = self.tags_open(slot);
        let unified = slot == Slot::Unified;
        let chevron = gtk::Image::from_icon_name(chevron_icon(open));
        let toggle = gtk::Button::new();
        toggle.add_css_class("flat");
        toggle.add_css_class("folders-toggle");
        if unified {
            toggle.add_css_class("unified-folders-toggle");
            // Rides 16px up onto the Filtered Folders rows (Jason,
            // 2026-09-07) when that section sits right above.
            if after_filtered {
                toggle.add_css_class("tags-toggle");
            }
        }
        let hb = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        hb.add_css_class("folder-row");
        if self.collapsed {
            let icon = gtk::Image::from_icon_name("co.hyprlab.Hylki-tag-outline-symbolic");
            pin_icon_size(&icon);
            hb.set_halign(gtk::Align::Center);
            hb.append(&icon);
            toggle.set_tooltip_text(Some(i18n("Tags").as_str()));
        } else {
            if self.chevrons_left {
                chevron.set_margin_start(2);
            }
            hb.append(&chevron);
            let lbl = gtk::Label::new(Some(i18n("Tags").as_str()));
            if unified {
                lbl.add_css_class("account-name");
            }
            lbl.set_halign(gtk::Align::Start);
            lbl.set_hexpand(true);
            lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            hb.append(&lbl);
        }
        toggle.set_child(Some(&hb));
        let st = sender.input_sender().clone();
        toggle.connect_clicked(move |_| {
            let _ = st.send(SidebarInput::ToggleTagsExpand(slot));
        });
        if unified {
            toggle.set_margin_bottom(tags_toggle_gap(open));
        }
        parent.append(&toggle);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.add_css_class("navigation-sidebar");
        list.add_css_class("section-child-list");
        if unified {
            list.set_margin_bottom(10);
        }
        for t in &self.tags {
            let row = gtk::ListBoxRow::new();
            let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            hbox.add_css_class("folder-row");
            let disc = crate::ui::context_menu::swatch_widget(&t.color, true);
            if self.collapsed {
                hbox.set_halign(gtk::Align::Center);
                hbox.append(&disc);
                row.set_tooltip_text(Some(&t.name));
            } else {
                // Laid out like the Filtered Folders rows: the same leaf
                // expander slot, then the disc exactly where their glyph
                // sits (both 16px), so the two sections' rows line up.
                let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                spacer.set_width_request(TREE_EXPANDER_WIDTH);
                hbox.append(&spacer);
                if self.chevrons_left {
                    disc.set_margin_start(ROW_LEFT_INSET);
                }
                hbox.append(&disc);
                // Regular weight, like the folder names above (the
                // heading's bold is the section's, not its rows').
                let label = gtk::Label::new(Some(&t.name));
                label.set_hexpand(true);
                label.set_halign(gtk::Align::Start);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                hbox.append(&label);
            }
            row.set_child(Some(&hbox));
            list.append(&row);
        }
        let ss = sender.input_sender().clone();
        let quiet = self.quiet.clone();
        list.connect_row_selected(move |_, row| {
            if quiet.get() {
                return;
            }
            if let Some(row) = row {
                let _ = ss.send(SidebarInput::TagRowSelected { slot, index: row.index() });
            }
        });
        attach_tag_context_menu(&list, self.tags.iter().map(|t| t.keyword.clone()).collect(), sender);

        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        revealer.set_transition_duration(0);
        revealer.set_reveal_child(open);
        revealer.set_child(Some(&list));
        parent.append(&revealer);
        self.tag_sections.insert(slot, SectionWidgets { revealer, chevron, toggle, list, badge: None });
    }

    /// Whether a unified row's total-unread chip is switched on (Settings →
    /// Sidebar → Unified → Unread counts). Sent and Tags never carry one.
    fn chip_shown(&self, row: UnifiedRow) -> bool {
        match row {
            UnifiedRow::Kind(kind) => self.unified_chips.has(kind),
            UnifiedRow::Filtered => self.unified_chips.filtered,
            UnifiedRow::Tags => false,
        }
    }

    /// Whether the unified Tags section has anything to show.
    fn unified_tags_shown(&self) -> bool {
        self.unified_tags && !self.tags.is_empty()
    }

    /// Whether a Filtered Folders section is open: the unified one keeps its
    /// own state, an account's lives with the account.
    fn filtered_open(&self, slot: Slot) -> bool {
        match slot {
            Slot::Unified => self.unified_folders_expanded,
            Slot::Account(id) => {
                self.sections.iter().find(|s| s.account.id == id).is_some_and(|s| s.filtered_expanded)
            }
        }
    }

    fn set_filtered_open(&mut self, slot: Slot, open: bool, sender: &ComponentSender<Self>) {
        match slot {
            Slot::Unified => {
                self.unified_folders_expanded = open;
                self.report_sections(sender);
            }
            Slot::Account(id) => {
                if let Some(s) = self.sections.iter_mut().find(|s| s.account.id == id) {
                    s.filtered_expanded = open;
                }
                let _ = sender.output(SidebarOutput::ToggleAccountFiltered(id));
            }
        }
    }

    fn tags_open(&self, slot: Slot) -> bool {
        match slot {
            Slot::Unified => self.tags_expanded,
            Slot::Account(id) => {
                self.sections.iter().find(|s| s.account.id == id).is_some_and(|s| s.tags_expanded)
            }
        }
    }

    fn set_tags_open(&mut self, slot: Slot, open: bool, sender: &ComponentSender<Self>) {
        match slot {
            Slot::Unified => {
                self.tags_expanded = open;
                self.report_sections(sender);
            }
            Slot::Account(id) => {
                if let Some(s) = self.sections.iter_mut().find(|s| s.account.id == id) {
                    s.tags_expanded = open;
                }
                let _ = sender.output(SidebarOutput::ToggleAccountTags(id));
            }
        }
    }

    /// Whether a unified Starred / Sent / Drafts row's account list is open.
    fn kind_open(&self, kind: FolderKind) -> bool {
        self.kind_expanded.get(&kind).copied().unwrap_or(false)
    }

    /// The unread total a unified row shows: every account's folder of that
    /// kind (Drafts counts every draft, as its chips do).
    fn kind_unread(&self, kind: FolderKind) -> u32 {
        if kind == FolderKind::Sent {
            return 0;
        }
        self.sections
            .iter()
            .flat_map(|s| s.folders.iter())
            .filter(|f| f.kind == kind)
            .map(|f| f.unread)
            .sum()
    }

    /// A Filtered Folders section's unread total, from its rows.
    fn filtered_unread(&self, slot: Slot) -> u32 {
        self.filtered_rows
            .get(&slot)
            .map(|rows| rows.iter().map(|r| r.folder.unread).sum())
            .unwrap_or(0)
    }

    fn select_tag(&self, account: Option<u32>, keyword: &str) {
        let Some(idx) = self.tags.iter().position(|t| t.keyword.eq_ignore_ascii_case(keyword)) else {
            return;
        };
        let list = match account {
            None => self
                .kind_widgets
                .get(&UnifiedRow::Tags)
                .map(|w| w.list.clone())
                .or_else(|| self.tag_sections.get(&Slot::Unified).map(|w| w.list.clone())),
            Some(id) => self.tag_sections.get(&Slot::Account(id)).map(|w| w.list.clone()),
        };
        if let Some(list) = list {
            if let Some(row) = list.row_at_index(idx as i32) {
                list.select_row(Some(&row));
            }
        }
    }

    /// Deselect every list except the one owning `keep` (whose own list keeps its
    /// selection). Used when a selection moves between sections.
    fn clear_other_selections(&self, keep: Sel) {
        if keep != Sel::Unified {
            if let Some(l) = &self.unified_list {
                l.unselect_all();
            }
        }
        for (row, w) in &self.kind_widgets {
            let keep_header = match row {
                UnifiedRow::Kind(k) => keep == Sel::UnifiedKind(*k),
                UnifiedRow::Filtered => keep == Sel::UnifiedFiltered,
                UnifiedRow::Tags => keep == Sel::UnifiedTags,
            };
            if !keep_header {
                w.header.unselect_all();
            }
            let keep_list = match row {
                UnifiedRow::Kind(k) => matches!(&keep, Sel::UnifiedKindRow(kk, _) if kk == k),
                UnifiedRow::Filtered => matches!(keep, Sel::UnifiedFolder(..)),
                UnifiedRow::Tags => matches!(keep, Sel::Tag(None, _)),
            };
            if !keep_list {
                w.list.unselect_all();
            }
        }
        for (slot, w) in &self.tag_sections {
            if !matches!(&keep, Sel::Tag(acc, _) if *acc == slot.account()) {
                w.list.unselect_all();
            }
        }
        for (slot, w) in &self.filtered_sections {
            let keep_here = match slot {
                Slot::Unified => matches!(keep, Sel::UnifiedFolder(..)),
                Slot::Account(id) => matches!(&keep, Sel::AccountFiltered(a, _) if a == id),
            };
            if !keep_here {
                w.list.unselect_all();
            }
        }
        // One list holds both footer rows; single-selection makes them
        // mutually exclusive, so it only needs clearing when neither is kept.
        if keep != Sel::Attachments && keep != Sel::Contacts {
            if let Some(l) = &self.footer_list {
                l.unselect_all();
            }
        }
        if keep != Sel::Outbox {
            if let Some(l) = &self.outbox_list {
                l.unselect_all();
            }
        }
        if !matches!(keep, Sel::UnifiedInbox(_)) {
            if let Some(l) = &self.unified_inbox_list {
                l.unselect_all();
            }
        }
        // A folder selection lives in exactly one of the two lists (essential or
        // custom) of one account; unselect every other list, including the
        // sibling list of the same account.
        let keep_is_custom = if let Sel::Folder(kaid, kpath) = &keep {
            self.folder_kind(*kaid, kpath) == Some(FolderKind::Custom)
        } else {
            false
        };
        for (aid, lb) in &self.folder_lists {
            let keep_here = matches!(&keep, Sel::Folder(kaid, _) if kaid == aid) && !keep_is_custom;
            if !keep_here {
                lb.unselect_all();
            }
        }
        for (aid, lb) in &self.custom_folder_lists {
            let keep_here = matches!(&keep, Sel::Folder(kaid, _) if kaid == aid) && keep_is_custom;
            if !keep_here {
                lb.unselect_all();
            }
        }
    }

    fn select_attachments(&self) {
        if let (Some(list), Some(row)) = (&self.footer_list, &self.attachments_row) {
            list.select_row(Some(row));
        }
    }

    fn select_contacts(&self) {
        if let (Some(list), Some(row)) = (&self.footer_list, &self.contacts_row) {
            list.select_row(Some(row));
        }
    }

    fn select_outbox(&self) {
        if let Some(list) = &self.outbox_list {
            if let Some(row) = list.row_at_index(0) {
                list.select_row(Some(&row));
            }
        }
    }

    /// Highlight a unified row's header (row 0 of its own list).
    fn select_row_header(&self, row: UnifiedRow) {
        if let Some(w) = self.kind_widgets.get(&row) {
            w.header.select_row(w.header.row_at_index(0).as_ref());
        }
    }

    fn restore_selection(&mut self) {
        // With nothing selected the primary instance picks the opening view
        // (All Inboxes, else the first inbox) and must say so: that first
        // pick reaches the app through the list boxes' selection signals.
        // Everything else is a restore of what is already shown — silent.
        let announce = self.selected == Sel::None && !self.mirror;
        self.quiet.set(!announce);
        self.restore_selection_inner();
        self.quiet.set(false);
    }

    fn restore_selection_inner(&mut self) {
        match self.selected.clone() {
            Sel::Unified => self.select_unified(),
            Sel::Attachments => self.select_attachments(),
            Sel::Contacts => self.select_contacts(),
            Sel::Outbox => self.select_outbox(),
            Sel::Folder(acc, path) => self.select_folder(acc, &path),
            Sel::UnifiedInbox(acc) => self.select_unified_inbox(acc),
            Sel::UnifiedFolder(acc, path) => self.select_filtered_row(Slot::Unified, acc, &path),
            Sel::AccountFiltered(acc, path) => {
                self.select_filtered_row(Slot::Account(acc), acc, &path)
            }
            Sel::Tag(acc, kw) => self.select_tag(acc, &kw),
            Sel::UnifiedKind(kind) => self.select_row_header(UnifiedRow::Kind(kind)),
            Sel::UnifiedFiltered => self.select_row_header(UnifiedRow::Filtered),
            Sel::UnifiedTags => self.select_row_header(UnifiedRow::Tags),
            Sel::UnifiedKindRow(kind, acc) => {
                if let Some(w) = self.kind_widgets.get(&UnifiedRow::Kind(kind)) {
                    if let Some(idx) = w.rows.iter().position(|r| r.account_id == acc) {
                        w.list.select_row(w.list.row_at_index(idx as i32).as_ref());
                    }
                }
            }
            Sel::None => {
                if self.show_unified {
                    self.select_unified();
                } else if let Some(acc) = self
                    .sections
                    .iter()
                    .find(|s| !s.folders.is_empty())
                    .map(|s| s.account.id)
                    .filter(|_| self.show_accounts)
                {
                    self.select_folder_index(acc, 0);
                }
            }
        }
    }

    fn select_unified(&self) {
        if let Some(list) = &self.unified_list {
            if let Some(row) = list.row_at_index(0) {
                list.select_row(Some(&row));
            }
        }
    }

    fn select_unified_inbox(&self, account_id: u32) {
        if let Some(list) = &self.unified_inbox_list {
            if let Some(idx) = self
                .unified_inboxes
                .iter()
                .position(|r| r.account_id == account_id)
            {
                if let Some(row) = list.row_at_index(idx as i32) {
                    list.select_row(Some(&row));
                }
            }
        }
    }

    /// Highlight a filtered folder's row: in the unified row's list, or an
    /// account's own section.
    fn select_filtered_row(&self, slot: Slot, account_id: u32, path: &str) {
        let list = match slot {
            Slot::Unified => self
                .kind_widgets
                .get(&UnifiedRow::Filtered)
                .map(|w| w.list.clone())
                .or_else(|| self.filtered_sections.get(&slot).map(|w| w.list.clone())),
            Slot::Account(_) => self.filtered_sections.get(&slot).map(|w| w.list.clone()),
        };
        let Some(list) = list else { return };
        if let Some(idx) = self
            .filtered_rows
            .get(&slot)
            .and_then(|rows| rows.iter().position(|r| r.account_id == account_id && r.folder.path == path))
        {
            if let Some(row) = list.row_at_index(idx as i32) {
                list.select_row(Some(&row));
            }
        }
    }

    /// Unfold an account's section if it is folded, the way its chevron
    /// would: the rail keeps its own fold state, the full sidebar's is
    /// reported (and persisted) as a toggle. Nothing happens to an account
    /// already open.
    fn reveal_account(&mut self, id: u32, sender: &ComponentSender<Self>) {
        let Some(rev) = self.revealers.get(&id) else { return };
        if rev.reveals_child() {
            return;
        }
        rev.set_reveal_child(true);
        if let Some(ch) = self.chevrons.get(&id) {
            ch.set_icon_name(Some("co.hyprlab.Hylki-pan-down-symbolic"));
        }
        if self.collapsed {
            self.rail_open_accounts.insert(id, true);
        } else if let Some(s) = self.sections.iter_mut().find(|s| s.account.id == id) {
            s.collapsed = false;
        }
        // The avatar badge stands in for the Inbox chip only while folded.
        if let Some(label) = self.account_circle_badges.get(&id) {
            label.set_visible(false);
        }
        if !self.collapsed {
            let _ = sender.output(SidebarOutput::ToggleCollapse(id));
        }
    }

    fn select_folder(&self, account_id: u32, path: &str) {
        let idx = self
            .sections
            .iter()
            .find(|s| s.account.id == account_id)
            .and_then(|s| s.folders.iter().position(|f| f.path == path));
        if let Some(idx) = idx {
            self.select_folder_index(account_id, idx);
        }
    }

    fn select_folder_index(&self, account_id: u32, idx: usize) {
        // Essential folders live in the main list (rows 0..E); custom folders in
        // the collapsible list (rows E..). Route to the right one.
        let e = self.essential_count(account_id);
        let (list, row_idx) = if idx < e {
            (self.folder_lists.get(&account_id), idx)
        } else {
            (self.custom_folder_lists.get(&account_id), idx - e)
        };
        if let Some(list) = list {
            if let Some(row) = list.row_at_index(row_idx as i32) {
                list.select_row(Some(&row));
            }
        }
    }

    /// Number of essential (non-custom) folders for an account — the boundary
    /// between the main and custom folder lists in `section.folders`.
    fn essential_count(&self, account_id: u32) -> usize {
        self.sections
            .iter()
            .find(|s| s.account.id == account_id)
            .map(|s| s.folders.iter().filter(|f| f.kind != FolderKind::Custom).count())
            .unwrap_or(0)
    }

    /// The kind of the folder at `path` in `account_id`, if known.
    fn folder_kind(&self, account_id: u32, path: &str) -> Option<FolderKind> {
        self.sections
            .iter()
            .find(|s| s.account.id == account_id)
            .and_then(|s| s.folders.iter().find(|f| f.path == path))
            .map(|f| f.kind)
    }
}

/// The dragged messages in a drop payload: the marker "vireo-move" followed by
/// one tab-separated (account, folder, uid, id) group per message. A drag from a
/// multi-selection carries every selected message (#23); anything malformed
/// yields nothing rather than a partial move.
fn parse_move_payload(payload: &str) -> Vec<(u32, u32, u32, u32)> {
    let parts: Vec<&str> = payload.split('\t').collect();
    if parts.first() != Some(&"vireo-move") || parts.len() < 5 || parts.len() % 4 != 1 {
        return Vec::new();
    }
    let items: Vec<(u32, u32, u32, u32)> = parts[1..]
        .chunks(4)
        .filter_map(|c| {
            Some((c[0].parse().ok()?, c[1].parse().ok()?, c[2].parse().ok()?, c[3].parse().ok()?))
        })
        .collect();
    // All or nothing: a group that won't parse means the payload isn't ours.
    if items.len() * 4 + 1 == parts.len() {
        items
    } else {
        Vec::new()
    }
}

/// A drop target that moves a dragged message into `dest_path` on account `id`.
fn folder_drop_target(
    id: u32,
    dest_path: String,
    sender: &ComponentSender<Sidebar>,
) -> gtk::DropTarget {
    let drop = gtk::DropTarget::new(gtk::glib::types::Type::STRING, gtk::gdk::DragAction::MOVE);
    let ds = sender.input_sender().clone();
    drop.connect_drop(move |_, value, _, _| {
        if let Ok(payload) = value.get::<String>() {
            let _ = ds.send(SidebarInput::DropOnFolder {
                account_id: id,
                path: dest_path.clone(),
                payload,
            });
            return true;
        }
        false
    });
    drop
}

/// Wire a right-click menu (Mark as Read / Refresh / Delete) onto a folder list;
/// `folders` maps the list's row indices to their folders.
/// The context menu of one folder — the same wherever the folder is
/// listed (under its account, or as a filtered folder in the unified
/// section): `filtered` says a filter rule files into it.
fn folder_menu_items(
    id: u32,
    f: &Folder,
    filtered: bool,
    has_filters: bool,
) -> Vec<(&'static str, CtxAction)> {
    let mut items = vec![
        (i18n_noop("Mark as Read"), CtxAction::MarkFolderRead { account_id: id, folder_id: f.id }),
        (i18n_noop("Refresh"), CtxAction::RefreshFolder { account_id: id, folder_id: f.id }),
    ];
    // Rules normally only meet mail arriving in the Inbox; from here they
    // can be held up against whatever is already in this folder (#198).
    // Not on Drafts, Junk or Trash: filing mail *out* of those is never what
    // a rule about incoming mail meant.
    let filterable = !matches!(
        f.kind,
        FolderKind::Drafts | FolderKind::Junk | FolderKind::Trash | FolderKind::Starred
    );
    if has_filters && filterable {
        items.push((i18n_noop("Apply Filters"), CtxAction::ApplyFilters {
            account_id: id,
            folder_id: f.id,
        }));
    }
    // A filter files into this folder: its rule is a click away.
    if filtered {
        items.push((i18n_noop("Edit Filter…"), CtxAction::EditFilter {
            account_id: id,
            path: f.path.clone(),
        }));
    }
    // Only user-created folders can be renamed or deleted.
    if f.kind == FolderKind::Custom {
        items.push((i18n_noop("Rename Folder…"), CtxAction::RenameFolder {
            account_id: id,
            name: f.name.clone(),
            path: f.path.clone(),
        }));
        items.push((i18n_noop("Delete Folder…"), CtxAction::DeleteFolder {
            account_id: id,
            name: f.name.clone(),
            path: f.path.clone(),
        }));
    }
    // Trash and Junk can be emptied outright (#152).
    let empty_label = match f.kind {
        FolderKind::Trash => Some(i18n_noop("Empty Trash…")),
        FolderKind::Junk => Some(i18n_noop("Empty Junk…")),
        _ => None,
    };
    if let Some(label) = empty_label {
        items.push((label, CtxAction::EmptyFolder {
            account_id: id,
            folder_id: f.id,
            name: f.name.clone(),
            path: f.path.clone(),
        }));
    }
    items
}

/// Right-click on a tag row, wherever tags are listed: edit it in Settings.
fn attach_tag_context_menu(
    list: &gtk::ListBox,
    keywords: Vec<String>,
    sender: &ComponentSender<Sidebar>,
) {
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_SECONDARY);
    let cs = sender.clone();
    let list_w = list.clone();
    click.connect_pressed(move |_, _, x, y| {
        if let Some(k) = list_w
            .row_at_y(y as i32)
            .and_then(|row| keywords.get(row.index() as usize))
        {
            show_sidebar_menu(
                &list_w,
                x,
                y,
                vec![(i18n_noop("Edit Tag…"), CtxAction::EditTag(k.clone()))],
                &cs,
            );
        }
    });
    list.add_controller(click);
}

fn attach_folder_context_menu(
    list: &gtk::ListBox,
    id: u32,
    folders: Vec<Folder>,
    filtered: Vec<String>,
    has_filters: bool,
    sender: &ComponentSender<Sidebar>,
) {
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_SECONDARY);
    let cs = sender.clone();
    let list_w = list.clone();
    click.connect_pressed(move |_, _, x, y| {
        if let Some(f) = list_w
            .row_at_y(y as i32)
            .and_then(|row| folders.get(row.index() as usize))
        {
            let items =
                folder_menu_items(id, f, filtered.iter().any(|p| *p == f.path), has_filters);
            show_sidebar_menu(&list_w, x, y, items, &cs);
        }
    });
    list.add_controller(click);
}

/// The Gravatar an account asked for (#189), once the app has looked it up.
/// `None` when it asked for none, when the address has none, or before the
/// answer is back — in every case the picture, emoji or initials stand.
fn account_gravatar(email: &str) -> Option<gtk::gdk::Texture> {
    crate::avatar::account_face(email)
        .filter(|face| face.gravatar)
        .and_then(|_| crate::avatar::own_gravatar(email))
}

pub(crate) fn account_initials(name: &str, email: &str) -> String {
    let mut it = name.split_whitespace();
    let a = it.next().and_then(|w| w.chars().next());
    let b = it.next().and_then(|w| w.chars().next());
    match (a, b) {
        (Some(a), Some(b)) => format!("{a}{b}").to_uppercase(),
        (Some(a), None) => a.to_uppercase().to_string(),
        _ => email
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string()),
    }
}

/// Width of the tree-expander slot: chevron button and leaf spacer alike, so
/// folder names at one depth line up whether or not they have children (#51).
const TREE_EXPANDER_WIDTH: i32 = 18;

/// Whether `child` sits under `parent` in the mailbox hierarchy — a strict
/// descendant, with a real delimiter at the boundary (any of the common ones;
/// see folder_depth for why the delimiter itself never reaches the UI).
fn path_is_under(child: &str, parent: &str) -> bool {
    child.len() > parent.len() + 1
        && child.starts_with(parent)
        && matches!(child.as_bytes()[parent.len()], b'/' | b'.' | b'\\')
}

impl Sidebar {
    /// Tell the app how the three sections stand, for the saved layout.
    fn report_sections(&self, sender: &ComponentSender<Self>) {
        let _ = sender.output(SidebarOutput::SectionsOpen {
            all_inboxes: self.unified_expanded,
            filtered: self.unified_folders_expanded,
            tags: self.tags_expanded,
            starred: self.kind_open(FolderKind::Starred),
            sent: self.kind_open(FolderKind::Sent),
            drafts: self.kind_open(FolderKind::Drafts),
            archive: self.kind_open(FolderKind::Archive),
        });
    }

}

/// Whether a folder row is hidden because some ancestor node is collapsed.
fn hidden_by_collapse(
    path: &str,
    collapsed: &std::collections::HashSet<String>,
) -> bool {
    collapsed.iter().any(|p| path_is_under(path, p))
}

/// Pop up a right-click context menu of `items` anchored at (x, y) in
/// `parent`, styled to GNOME HIG (sized to content, no scrollbar).
fn show_sidebar_menu(
    parent: &impl IsA<gtk::Widget>,
    x: f64,
    y: f64,
    items: Vec<(&str, CtxAction)>,
    sender: &ComponentSender<Sidebar>,
) {
    let entries = items
        .into_iter()
        .map(|(label, action)| {
            let s = sender.clone();
            MenuEntry::new(i18n(label), move || {
                let _ = s.output(SidebarOutput::Context(action.clone()));
            })
        })
        .collect();
    show_context_menu(parent, x, y, vec![entries]);
}

/// A row in the "All Inboxes" sub-list: a small account pill, the account name,
/// and that inbox's unread badge. In the compact rail only the pill is shown
/// (centred), with the unread count as a corner chip. Returns the badge for
/// in-place updates.
fn build_unified_inbox_row(
    section: &SectionData,
    inbox: &Folder,
    collapsed: bool,
    inset: bool,
) -> (gtk::ListBoxRow, gtk::Label) {
    // Show the account's configured label (defaults to its email) so accounts are
    // easy to tell apart in the All Inboxes view.
    let label = &section.account.label;

    // Small account pill (colour + initials/emoji), like the header circle.
    let id = section.account.id;
    let circle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    circle.add_css_class("account-circle-sm");
    circle.add_css_class(&format!("acct-color-{id}"));
    circle.set_valign(gtk::Align::Center);
    circle.set_halign(gtk::Align::Center);
    circle.set_hexpand(false);
    circle.set_size_request(21, 21);
    let gravatar = account_gravatar(&section.account.email);
    let glyph: gtk::Widget = match (&gravatar, &section.avatar, &section.emoji) {
        (Some(texture), ..) => {
            circle.set_overflow(gtk::Overflow::Hidden);
            crate::ui::initials::picture_from_texture(texture, 21).upcast()
        }
        (None, Some(path), _) => {
            circle.set_overflow(gtk::Overflow::Hidden);
            crate::ui::initials::avatar_picture(path, 21).upcast()
        }
        (None, None, Some(em)) if !em.is_empty() => {
            crate::ui::initials::glyph_picture(em, &section.color, 0.6, 21).upcast()
        }
        _ => crate::ui::initials::glyph_picture(
            &account_initials(label, &section.account.email),
            &section.color,
            0.5,
            21,
        )
        .upcast(),
    };
    circle.append(&glyph);

    build_unified_sub_row(&circle, label, label, inbox.unread, collapsed, inset, true)
}

/// A row nested under "All Inboxes": `lead` (the account's pill), `title`,
/// and an unread badge. In the compact rail only the lead is shown
/// (centred), with the count as a corner chip and `tip` (plus the count) as
/// the tooltip. Returns the badge for in-place updates.
fn build_unified_sub_row(
    lead: &impl IsA<gtk::Widget>,
    title: &str,
    tip: &str,
    unread: u32,
    collapsed: bool,
    inset: bool,
    pill: bool,
) -> (gtk::ListBoxRow, gtk::Label) {
    let row = gtk::ListBoxRow::new();
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    hbox.add_css_class("folder-row");
    // The 2px pull-in centres a 21px account pill on the 16px icon column;
    // a row led by a 16px icon or disc sits on that column as it is, and
    // pulled in it read as drifting left of the header's glyph.
    if !collapsed && pill {
        hbox.add_css_class("unified-subrow");
    }

    let badge = if collapsed {
        hbox.set_halign(gtk::Align::Center);
        let tip = if unread > 0 {
            format!("{tip} ({unread})")
        } else {
            tip.to_string()
        };
        row.set_tooltip_text(Some(&tip));
        let (overlay, badge) = with_unread_overlay(lead, unread);
        hbox.append(&overlay);
        badge
    } else {
        if inset {
            lead.set_margin_start(ROW_LEFT_INSET - 6);
        }
        hbox.append(lead);
        if tip != title {
            row.set_tooltip_text(Some(tip));
        }

        let name = gtk::Label::new(Some(title));
        // The label keeps the same column either way: the icon-led row
        // gave up the 2px pull-in, so its label gives up 2px here.
        name.set_margin_start(if pill { 6 } else { 4 });
        name.set_hexpand(true);
        name.set_halign(gtk::Align::Start);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        hbox.append(&name);

        let badge = gtk::Label::new(Some(&unread.to_string()));
        style_badge(&badge, 5);
        badge.set_visible(unread > 0);
        hbox.append(&badge);
        badge
    };

    row.set_child(Some(&hbox));
    (row, badge)
}

/// Pins a symbolic icon to an exact, deterministic 16px box so it centres on
/// the same column as the rail's avatar circles. Left to GTK's default
/// icon-size resolution, a plain `gtk::Image`'s natural size can round a
/// fraction of a pixel off from the circles' hand-pinned, always-even
/// `set_size_request` — `Align::Center` then centres that slightly-off box
/// exactly as asked, reading as the icon column drifting right of the
/// avatar column (PR #95).
fn pin_icon_size(icon: &gtk::Image) {
    icon.set_pixel_size(16);
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
}

/// Room under the "Filtered Folders" heading: folded up, its list (and the
/// list's gap to the first account section) is gone, so the heading itself
/// keeps the accounts at a balanced distance; open, the list carries it.
fn unified_folders_toggle_gap(expanded: bool) -> i32 {
    if expanded { 0 } else { 16 }
}

/// Room under the "Tags" heading: 5px when folded up (Jason, 2026-09-07);
/// open, its list carries the gap to the accounts.
fn tags_toggle_gap(expanded: bool) -> i32 {
    if expanded { 0 } else { 5 }
}

/// Extra left inset on every expanded row's leading icon in the leading-
/// chevron layout, so rows don't read as shoved flush against the sidebar's
/// edge (PR #95). The classic trailing layout keeps its original geometry.
const ROW_LEFT_INSET: i32 = 8;

/// Wrap `child` in an overlay with a small unread chip pinned to its top-right
/// corner — used in the compact rail, where there's no room for an inline badge.
/// Returns the overlay (to place in the tree) and the chip label (for updates).
fn with_unread_overlay(
    child: &impl IsA<gtk::Widget>,
    unread: u32,
) -> (gtk::Overlay, gtk::Label) {
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(child));
    let badge = gtk::Label::new(Some(&unread.to_string()));
    style_badge(&badge, 4);
    badge.add_css_class("unread-badge-mini");
    badge.set_halign(gtk::Align::End);
    badge.set_valign(gtk::Align::Start);
    badge.set_visible(unread > 0);
    overlay.add_overlay(&badge);
    (overlay, badge)
}

/// Nesting depth of a custom folder: how many of the *other listed* folders are
/// ancestors of its IMAP path. Working from listed ancestors (rather than
/// counting delimiters) keeps namespace prefixes honest — "INBOX.Clients" is
/// top-level on a Dovecot-style server because "INBOX" isn't in the custom
/// list, while "INBOX.Clients.Acme" is one level down because "INBOX.Clients"
/// is. The delimiter itself never reaches the UI, so any of the common ones is
/// accepted at the boundary.
pub(crate) fn folder_depth(folder: &Folder, all: &[&Folder]) -> usize {
    all.iter()
        .filter(|g| {
            g.id != folder.id
                && folder.path.len() > g.path.len() + 1
                && folder.path.starts_with(&g.path)
                && matches!(folder.path.as_bytes()[g.path.len()], b'/' | b'.' | b'\\')
        })
        .count()
}

/// The icon a folder row wears when a filter rule files into it: the
/// filter-folder glyph in the account's colour, in place in the hierarchy.
fn filter_icon(section: &SectionData, folder: &Folder) -> FolderGlyph {
    if !section.filtered.iter().any(|f| f.id == folder.id) {
        return FolderGlyph::Plain;
    }
    if folder.kind == FolderKind::Custom {
        FolderGlyph::Icon(filtered_folder_icon(folder, section.account.id))
    } else {
        FolderGlyph::Marked(section.account.id)
    }
}

/// What a folder row shows for its icon.
enum FolderGlyph {
    /// The folder kind's own icon.
    Plain,
    /// The caller's icon in place of it (a filter destination's tinted glyph).
    Icon(gtk::Image),
    /// The kind's own icon, grey as ever, with a small filter glyph in the
    /// account's colour riding its corner: a main folder (Archive, Junk…)
    /// that a filter files into.
    Marked(u32),
}

/// The icon of a folder a filter files into: a custom folder wears the
/// filter-folder glyph, a main folder (Archive, Junk…) keeps its own —
/// either in the account's colour, which is what says "part of a filter".
fn filtered_folder_icon(folder: &Folder, account_id: u32) -> gtk::Image {
    let name = if folder.kind == FolderKind::Custom {
        "co.hyprlab.Hylki-filter-folder-symbolic"
    } else {
        folder.kind.icon()
    };
    let icon = gtk::Image::from_icon_name(name);
    icon.add_css_class(&format!("acct-tint-{account_id}"));
    icon
}

/// An unread chip: the count in a pill that never pushes its row wider
/// than the sidebar — past `max_chars` digits it ends in an ellipsis.
fn style_badge(badge: &gtk::Label, max_chars: i32) {
    badge.add_css_class("unread-badge");
    badge.set_valign(gtk::Align::Center);
    badge.set_ellipsize(gtk::pango::EllipsizeMode::End);
    badge.set_max_width_chars(max_chars);
}

/// Build one folder row. `depth` indents sub-folders to mirror the server's
/// hierarchy (0 = top level; only meaningful for custom folders).
fn build_folder_row(
    folder: &Folder,
    collapsed: bool,
    depth: usize,
    lead: Option<&gtk::Widget>,
    inset: bool,
    glyph: FolderGlyph,
) -> (gtk::ListBoxRow, Option<gtk::Label>) {
    let row = gtk::ListBoxRow::new();
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    hbox.add_css_class("folder-row");
    // Custom folders show only their leaf name; the tooltip carries the whole
    // hierarchy, so nine folders all named "Archive" stay tellable apart (#51).
    if folder.kind == FolderKind::Custom && folder.path.contains(['/', '.', '\\']) {
        let pretty = folder
            .path
            .split(['/', '.', '\\'])
            .map(crate::mutf7::decode)
            .collect::<Vec<_>>()
            .join(" › ");
        row.set_tooltip_text(Some(&pretty));
    }
    if let Some(lead) = lead {
        hbox.append(lead);
    }
    if !collapsed && depth > 0 {
        // Indent nested folders; capped so a pathological hierarchy can't push
        // the name out of the sidebar.
        hbox.set_margin_start(14 * depth.min(4) as i32);
    }

    // The folder kind's icon, unless the caller brought its own (the
    // Filters rows' account-tinted glyph).
    let img = match &glyph {
        FolderGlyph::Icon(icon) => icon.clone(),
        _ => gtk::Image::from_icon_name(folder.kind.icon()),
    };
    img.add_css_class("folder-icon");
    pin_icon_size(&img);
    // A marked folder carries the filter glyph on the icon's corner: top
    // right, or bottom right in the rail where the unread badge has the
    // top. The mark takes no room of its own.
    let visual: gtk::Widget = match glyph {
        FolderGlyph::Marked(account_id) => {
            let overlay = gtk::Overlay::new();
            overlay.set_child(Some(&img));
            let mark = gtk::Image::from_icon_name("co.hyprlab.Hylki-filter-symbolic");
            mark.set_pixel_size(9);
            mark.add_css_class("filter-mark");
            mark.add_css_class(&format!("acct-tint-{account_id}"));
            mark.set_halign(gtk::Align::End);
            mark.set_valign(if collapsed { gtk::Align::End } else { gtk::Align::Start });
            overlay.add_overlay(&mark);
            overlay.set_measure_overlay(&mark, false);
            overlay.upcast()
        }
        _ => img.clone().upcast(),
    };

    // Sent wears no unread chip: what you sent is not new mail. `None` keeps
    // it off the in-place update lists too.
    let counted = folder.kind != FolderKind::Sent;
    let unread = if counted { folder.unread } else { 0 };
    let badge = if collapsed {
        hbox.set_halign(gtk::Align::Center);
        let tip = if unread > 0 {
            format!("{} ({})", folder.name, unread)
        } else {
            folder.name.clone()
        };
        row.set_tooltip_text(Some(&tip));
        // Every folder carries an unread chip; in the rail it rides the icon's
        // corner so new mail shows without expanding the sidebar.
        let (overlay, badge) = with_unread_overlay(&visual, unread);
        hbox.append(&overlay);
        counted.then_some(badge)
    } else {
        if inset {
            visual.set_margin_start(ROW_LEFT_INSET);
        }
        hbox.append(&visual);
        let name = gtk::Label::new(Some(&folder.name));
        name.set_hexpand(true);
        name.set_halign(gtk::Align::Start);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        hbox.append(&name);

        // Every folder shows an unread count chip — present but hidden when
        // zero so it can update in place.
        let badge = gtk::Label::new(Some(&unread.to_string()));
        style_badge(&badge, 5);
        badge.set_visible(unread > 0);
        hbox.append(&badge);
        counted.then_some(badge)
    };

    row.set_child(Some(&hbox));
    (row, badge)
}

#[cfg(test)]
mod tests {
    use super::folder_depth;
    use super::hidden_by_collapse;
    use super::parse_move_payload;
    use crate::models::{Folder, FolderKind};

    fn custom(id: u32, path: &str) -> Folder {
        Folder {
            id,
            account_id: 1,
            name: path.rsplit(['/', '.']).next().unwrap_or(path).to_string(),
            path: path.to_string(),
            kind: FolderKind::Custom,
            unread: 0,
        }
    }

    #[test]
    fn folder_depth_follows_listed_ancestors() {
        // A Dovecot-style namespace: everything lives under "INBOX.", which is
        // not itself a custom folder — so "INBOX.Clients" is top-level and only
        // real sub-folders are indented.
        let folders = vec![
            custom(1, "INBOX.Clients"),
            custom(2, "INBOX.Clients.Acme"),
            custom(3, "INBOX.Clients.Acme.Invoices"),
            custom(4, "INBOX.Travel"),
        ];
        let refs: Vec<&Folder> = folders.iter().collect();
        assert_eq!(folder_depth(&folders[0], &refs), 0);
        assert_eq!(folder_depth(&folders[1], &refs), 1);
        assert_eq!(folder_depth(&folders[2], &refs), 2);
        assert_eq!(folder_depth(&folders[3], &refs), 0);
    }

    #[test]
    fn folder_depth_needs_a_delimiter_not_just_a_prefix() {
        // "ClientsB" merely shares a prefix with "Clients" — it is a sibling,
        // not a child.
        let folders = vec![custom(1, "Clients"), custom(2, "ClientsB"), custom(3, "Clients/X")];
        let refs: Vec<&Folder> = folders.iter().collect();
        assert_eq!(folder_depth(&folders[1], &refs), 0);
        assert_eq!(folder_depth(&folders[2], &refs), 1);
    }

    #[test]
    fn a_collapsed_node_hides_descendants_and_nothing_else() {
        let mut collapsed = std::collections::HashSet::new();
        collapsed.insert("Clients".to_string());
        // Direct child and grandchild hide; a sibling sharing the prefix does
        // not, and neither does the collapsed node itself.
        assert!(hidden_by_collapse("Clients/Acme", &collapsed));
        assert!(hidden_by_collapse("Clients/Acme/Invoices", &collapsed));
        assert!(!hidden_by_collapse("ClientsB", &collapsed));
        assert!(!hidden_by_collapse("Clients", &collapsed));
        // Dotted hierarchies collapse the same way.
        collapsed.clear();
        collapsed.insert("INBOX.2025".to_string());
        assert!(hidden_by_collapse("INBOX.2025.Archive", &collapsed));
        assert!(!hidden_by_collapse("INBOX.2026.Archive", &collapsed));
    }

    #[test]
    fn a_drop_payload_carries_every_dragged_message() {
        // One message (the single-selection case).
        assert_eq!(parse_move_payload("vireo-move\t1\t2\t3\t4"), vec![(1, 2, 3, 4)]);
        // Three, as a multi-selection drag sends them — including a second
        // account, which the app filters out (mail can't cross accounts).
        assert_eq!(
            parse_move_payload("vireo-move\t1\t2\t3\t4\t1\t2\t5\t6\t7\t8\t9\t10"),
            vec![(1, 2, 3, 4), (1, 2, 5, 6), (7, 8, 9, 10)]
        );
    }

    #[test]
    fn a_payload_that_isnt_ours_moves_nothing() {
        for bad in [
            "",
            "some dragged text",
            "vireo-move",
            "vireo-move\t1\t2\t3",       // short group
            "vireo-move\t1\t2\t3\t4\t5",  // trailing partial group
            "vireo-move\t1\t2\tx\t4",     // unparsable field
        ] {
            assert!(parse_move_payload(bad).is_empty(), "{bad:?} should parse to nothing");
        }
    }
}
