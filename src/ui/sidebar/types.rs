//! The sidebar's data: what the app hands it per account, what is
//! selected, and the unified rows it draws.

use super::*;

/// A per-account inbox shown in the expandable "All Inboxes" sub-list.
#[derive(Clone)]
pub(super) struct InboxRef {
    pub(super) account_id: u32,
    pub(super) folder_id: u32,
    pub(super) name: String,
    pub(super) path: String,
}

/// A folder a filter rule files into, listed in the "Filtered Folders"
/// section inside All Inboxes. The app resolves the rules to these; the
/// sidebar only draws them (account pill + folder name + unread chip).
#[derive(Debug, Clone)]
pub(crate) struct UnifiedFolderRef {
    pub(crate) account_id: u32,
    pub(crate) folder: Folder,
}

/// Which copy of the Filtered Folders / Tags section a widget belongs to:
/// the unified section's, or an account's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Slot {
    Unified,
    Account(u32),
}

impl Slot {
    /// The account a section is scoped to; `None` for the unified one.
    pub(super) fn account(self) -> Option<u32> {
        match self {
            Slot::Unified => None,
            Slot::Account(id) => Some(id),
        }
    }
}

/// An account's Tags section's widgets: rebuilt with the sidebar, toggled
/// in place between rebuilds.
pub(super) struct SectionWidgets {
    pub(super) revealer: gtk::Revealer,
    pub(super) chevron: gtk::Image,
    pub(super) list: gtk::ListBox,
}

/// One unified row — All Inboxes, or the unified section's Starred / Sent /
/// Drafts: the header list (one selectable row), the per-account list under
/// it, and their badges.
pub(super) struct KindWidgets {
    pub(super) header: gtk::ListBox,
    pub(super) revealer: gtk::Revealer,
    pub(super) chevron: Option<gtk::Image>,
    pub(super) badge: Option<gtk::Label>,
    pub(super) list: gtk::ListBox,
    pub(super) rows: Vec<InboxRef>,
    pub(super) row_badges: HashMap<(u32, u32), gtk::Label>,
}

/// A row of the unified section: All Inboxes and the Starred / Sent /
/// Drafts rows (one folder per account), the Filtered Folders row (every
/// rule's folder) and the Tags row (every tag) — all built alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum UnifiedRow {
    Kind(FolderKind),
    /// The folders picked with Add to Favorites (#367), from any account.
    Favorites,
    Filtered,
    Tags,
    /// The smart folders (#417): saved searches, each a row of the list.
    Smart,
    /// Contacts: every contact, and under it each account's address
    /// books with their groups.
    Contacts,
}

/// What a unified row is called.
pub(super) fn row_title(row: UnifiedRow) -> String {
    match row {
        UnifiedRow::Kind(kind) => kind_label(kind),
        UnifiedRow::Favorites => i18n("Favorites"),
        UnifiedRow::Filtered => i18n("Filters"),
        UnifiedRow::Tags => i18n("Tags"),
        UnifiedRow::Smart => i18n("Smart Folders"),
        UnifiedRow::Contacts => i18n("Contacts"),
    }
}

/// A unified row's icon.
pub(super) fn row_icon(row: UnifiedRow) -> &'static str {
    match row {
        UnifiedRow::Kind(kind) => kind.icon(),
        UnifiedRow::Favorites => "user-bookmarks-symbolic",
        UnifiedRow::Filtered => "filter-folder-symbolic",
        UnifiedRow::Tags => "tag-outline-symbolic",
        UnifiedRow::Smart => "folder-saved-search-symbolic",
        UnifiedRow::Contacts => "x-office-address-book-symbolic",
    }
}

/// The message a unified row's header sends when selected.
pub(super) fn select_msg(row: UnifiedRow) -> SidebarInput {
    match row {
        UnifiedRow::Kind(FolderKind::Inbox) => SidebarInput::UnifiedRowSelected,
        UnifiedRow::Kind(kind) => SidebarInput::UnifiedKindRowSelected(kind),
        UnifiedRow::Favorites => SidebarInput::UnifiedFavoritesSelected,
        UnifiedRow::Filtered => SidebarInput::UnifiedFilteredSelected,
        UnifiedRow::Tags => SidebarInput::UnifiedTagsSelected,
        // Each smart folder is a view of its own; the header holds them.
        UnifiedRow::Smart => SidebarInput::ToggleSmartExpand,
        UnifiedRow::Contacts => SidebarInput::ContactsRowClicked,
    }
}

/// The message a unified row's chevron (or, in the rail, a long press) sends.
pub(super) fn toggle_msg(row: UnifiedRow) -> SidebarInput {
    match row {
        UnifiedRow::Kind(FolderKind::Inbox) => SidebarInput::ToggleUnifiedExpand,
        UnifiedRow::Kind(kind) => SidebarInput::ToggleKindExpand(kind),
        UnifiedRow::Favorites => SidebarInput::ToggleFavoritesExpand,
        UnifiedRow::Filtered => SidebarInput::ToggleFilteredExpand(Slot::Unified),
        UnifiedRow::Tags => SidebarInput::ToggleTagsExpand(Slot::Unified),
        UnifiedRow::Smart => SidebarInput::ToggleSmartExpand,
        UnifiedRow::Contacts => SidebarInput::ToggleContactsExpand,
    }
}

/// The chevron glyph for a section that is open or folded.
pub(super) fn chevron_icon(open: bool) -> &'static str {
    if open {
        "pan-down-symbolic"
    } else {
        "pan-end-symbolic"
    }
}

/// What a unified row is called.
pub(crate) fn kind_label(kind: FolderKind) -> String {
    match kind {
        FolderKind::Inbox => i18n("Inboxes"),
        FolderKind::Starred => i18n("Starred"),
        FolderKind::Sent => i18n("Sent"),
        FolderKind::Drafts => i18n("Drafts"),
        FolderKind::Archive => i18n("Archive"),
        FolderKind::Junk => i18n("Junk"),
        FolderKind::Trash => i18n("Trash"),
        FolderKind::Snoozed => i18n("Snoozed"),
        _ => i18n("Folder"),
    }
}

/// One account's section data, as handed to the sidebar.
#[derive(Debug, Clone)]
pub(crate) struct SectionData {
    pub(crate) account: Account,
    pub(crate) folders: Vec<Folder>,
    pub(crate) collapsed: bool,
    /// Whether this account's custom-folders section is expanded (default hidden).
    pub(crate) custom_expanded: bool,
    /// Resolved avatar background color ("#rrggbb").
    pub(crate) color: String,
    /// Avatar emoji; when absent, account-name initials are shown.
    pub(crate) emoji: Option<String>,
    /// Avatar picture (#162), shown before the emoji and the initials.
    pub(crate) avatar: Option<std::path::PathBuf>,
    /// Custom-folder paths whose tree node is collapsed (#51).
    pub(crate) tree_collapsed: Vec<String>,
    /// The user's own folder order (paths): a folder sorts among its
    /// siblings by where it stands here, unlisted ones after, as the server
    /// sorted them. Empty keeps the default order.
    pub(crate) folder_order: Vec<String>,
    /// How the server spells its folder hierarchy, to tell a folder's
    /// siblings on the server from the folders merely drawn beside it.
    pub(crate) hierarchy: Hierarchy,
    /// How the custom folders are sorted: the account's own choice
    /// (`own_folder_sort`) or else Settings' (`default_folder_sort`).
    pub(crate) folder_sort: crate::config::FolderSort,
    pub(crate) own_folder_sort: Option<crate::config::FolderSort>,
    pub(crate) default_folder_sort: crate::config::FolderSort,
    /// The folders this account's filter rules file into (every rule's
    /// destination).
    pub(crate) filtered: Vec<Folder>,
    /// Whether this account has any filter rule at all: only then is
    /// "Apply Filters" worth offering on its folders (#198).
    pub(crate) has_filters: bool,
    /// Whether this account's own "Tags" section is open.
    pub(crate) tags_expanded: bool,
    /// Whether the account takes part in the unified section (#267): off,
    /// its folders are left out of every unified row and chip, and only
    /// its own section lists them.
    pub(crate) in_unified: bool,
    /// A Gmail account, whose own folders are labels and say so (#374).
    pub(crate) gmail: bool,
}

/// Initial state for the sidebar.
pub(crate) struct SidebarInit {
    /// Icon-only mode: hide all text, show just icons and account pills.
    pub(crate) collapsed: bool,
    /// A mirror instance (the floating peek panel): it never picks a view
    /// on its own when nothing is selected — the app hands it the primary
    /// sidebar's choice through `SidebarInput::MirrorSelection`.
    pub(crate) mirror: bool,
    /// The three sections' open state as last left (persisted with the
    /// sidebar layout).
    pub(crate) unified_expanded: bool,
    pub(crate) filtered_expanded: bool,
    pub(crate) tags_expanded: bool,
    /// The unified Starred / Sent / Drafts rows' account lists.
    pub(crate) starred_expanded: bool,
    pub(crate) sent_expanded: bool,
    pub(crate) drafts_expanded: bool,
    pub(crate) archive_expanded: bool,
    pub(crate) junk_expanded: bool,
    pub(crate) trash_expanded: bool,
    pub(crate) snoozed_expanded: bool,
    pub(crate) favorites_expanded: bool,
    pub(crate) smart_expanded: bool,
    pub(crate) contacts_expanded: bool,
    pub(crate) contacts_folded: Vec<String>,
    /// Whether the "Attachments" row is shown.
    pub(crate) show_attachments: bool,
    /// Where the first pick lands instead of All Inboxes (#256).
    pub(crate) start: Option<StartTarget>,
}

/// A view to open at launch (#256), by account address: an account's
/// inbox, or one of its folders by path.
#[derive(Clone, Debug)]
pub(crate) enum StartTarget {
    Inbox(String),
    Folder(String, String),
}

/// What is currently selected in the sidebar.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Sel {
    None,
    Unified,
    /// The attachments gallery (all inboxes).
    Attachments,
    /// The in-app contacts view, every contact.
    Contacts,
    /// An address book or group picked under the Contacts row.
    ContactScope(crate::ui::contacts_page::ContactScope),
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
    /// The Favorites row: every favorite folder, merged.
    UnifiedFavorites,
    /// The unified Tags row: every tagged message.
    UnifiedTags,
    /// An account's folder picked in a unified Starred / Sent / Drafts
    /// row's account list.
    UnifiedKindRow(FolderKind, u32),
    /// A folder (account, path) picked in the Favorites list (#367).
    Favorite(u32, String),
    /// A tag (its keyword) selected in a Tags section (#71): scoped to an
    /// account when picked from that account's own section.
    Tag(Option<u32>, String),
    /// A smart folder (#417), by its id.
    Smart(String),
}

impl Sel {
    /// The account the selection belongs to, if it belongs to one.
    pub(super) fn account(&self) -> Option<u32> {
        match self {
            Sel::Folder(acc, _)
            | Sel::UnifiedInbox(acc)
            | Sel::UnifiedFolder(acc, _)
            | Sel::AccountFiltered(acc, _)
            | Sel::Tag(Some(acc), _)
            | Sel::Favorite(acc, _)
            | Sel::UnifiedKindRow(_, acc) => Some(*acc),
            _ => None,
        }
    }

    /// The same selection for the account now numbered `id`.
    pub(super) fn with_account(self, id: u32) -> Sel {
        match self {
            Sel::Folder(_, p) => Sel::Folder(id, p),
            Sel::UnifiedInbox(_) => Sel::UnifiedInbox(id),
            Sel::UnifiedFolder(_, p) => Sel::UnifiedFolder(id, p),
            Sel::AccountFiltered(_, p) => Sel::AccountFiltered(id, p),
            Sel::Tag(Some(_), k) => Sel::Tag(Some(id), k),
            Sel::Favorite(_, p) => Sel::Favorite(id, p),
            Sel::UnifiedKindRow(kind, _) => Sel::UnifiedKindRow(kind, id),
            other => other,
        }
    }
}
