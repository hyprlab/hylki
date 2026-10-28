//! The sidebar's messages: what the app and its own rows send it, and what
//! it reports back (selections, persistence, server actions, menus).

use super::*;

#[derive(Debug, Clone)]
pub(crate) enum SidebarInput {
    SetContents {
        sections: Vec<SectionData>,
        show_unified: bool,
        /// The unified Starred / Sent / Drafts rows to show (already
        /// narrowed by the app to a multi-account setup).
        unified_kinds: crate::config::UnifiedKinds,
        /// Whether the account sections are shown at all.
        show_accounts: bool,
        unified_chips: crate::config::UnifiedChips,
        chevrons_left: bool,
        /// A click anywhere on an account's header folds it.
        account_click_folds: bool,
        rail_dots: bool,
        rail_fold: crate::config::RailFold,
        unified_unread: u32,
        /// Filter-rule folders to list inside All Inboxes (already
        /// narrowed to the rules that opt in and the Settings switch).
        unified_folders: Vec<UnifiedFolderRef>,
        /// The favorite folders (#367), from every account.
        favorite_folders: Vec<UnifiedFolderRef>,
        /// The tags (#71), for the Tags section.
        tags: Vec<crate::config::Tag>,
        /// Where the Filters, Tags, Favorites and Contacts sections are
        /// drawn, and how they look.
        section_style: crate::config::SectionStyle,
        filtered_placement: crate::config::SectionPlacement,
        tags_placement: crate::config::SectionPlacement,
        favorites_placement: crate::config::SectionPlacement,
        contacts_placement: crate::config::SectionPlacement,
        /// The smart folders (#417) as (id, name).
        smart_folders: Vec<(String, String)>,
    },
    /// Focus Mode's sidebar parts: hide the accounts, fold the unified
    /// rows. `animate` slides what goes (or comes back) and rebuilds once it
    /// has gone; off, the rebuild is immediate (launch).
    SetFocus {
        hide_accounts: bool,
        fold_unified: bool,
        animate: bool,
    },
    /// The Focus Mode slide finished: draw the sidebar as it now is.
    FocusSettled,
    /// A tag row in a Tags section was chosen.
    TagRowSelected {
        slot: Slot,
        index: i32,
    },
    /// Toggle a Tags section.
    ToggleTagsExpand(Slot),
    /// A unified Starred / Sent / Drafts row was chosen (the merged view).
    UnifiedKindRowSelected(FolderKind),
    /// The unified Filtered Folders row was chosen (every rule's folder).
    UnifiedFilteredSelected,
    /// The Favorites row was chosen (every favorite folder).
    UnifiedFavoritesSelected,
    /// The unified Tags row was chosen (every tagged message).
    UnifiedTagsSelected,
    /// A row in a unified Starred / Sent / Drafts row's account list.
    KindSubRowSelected {
        kind: FolderKind,
        index: i32,
    },
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
    /// The address books and groups the Contacts row lists.
    SetContactTree(crate::ui::contacts_page::ContactTree),
    /// Open or fold the Contacts row's list.
    ToggleContactsExpand,
    /// Fold or open an account or book under it, by key.
    ToggleContactNode(String),
    /// A book or group (or every contact) was picked in that list.
    ContactTreeSelected(crate::ui::contacts_page::ContactScope),
    /// Contacts were dropped on a book or group there.
    ContactTreeDrop {
        keys: Vec<(String, String)>,
        on: crate::ui::contacts_page::DropOn,
        moving: bool,
    },
    /// A right-click choice on a book or group there.
    ContactTreeMenu(crate::ui::contacts_page::TreeAction),
    FolderRowSelected {
        account_id: u32,
        index: i32,
    },
    /// Select a folder row programmatically — the app navigated there itself
    /// ("Go to Message" from the gallery, a notification click) and the
    /// highlight must follow. State is set before the row, so the resulting
    /// row-selected signal hits the already-selected guard and stops.
    SelectFolderRow {
        account_id: u32,
        path: String,
    },
    /// A per-account inbox row under "All Inboxes" was chosen.
    UnifiedInboxRowSelected(i32),
    /// Toggle the "All Inboxes" per-account inbox sub-list.
    ToggleUnifiedExpand,
    /// A row in a Filtered Folders section was chosen.
    FilteredRowSelected {
        slot: Slot,
        index: i32,
    },
    /// Toggle a Filtered Folders section.
    ToggleFilteredExpand(Slot),
    /// Open or fold the Favorites list (#367).
    ToggleFavoritesExpand,
    /// A folder picked in the Favorites list, by its index there.
    FavoriteRowSelected(i32),
    /// Open or fold the Smart Folders list (#417).
    ToggleSmartExpand,
    /// A smart folder picked, by its index in the list.
    SmartRowSelected(i32),
    /// Each smart folder's unread count, by id.
    SetSmartUnread(HashMap<String, u32>),
    ToggleCollapseLocal(u32),
    /// Set icon-only mode outright (the app's narrow-window breakpoint) —
    /// unlike ToggleCollapsed this never reports CollapsedChanged, so it can't
    /// overwrite the user's own persisted choice.
    SetCollapsed(bool),
    /// Move the highlight to what the app is showing, without reporting it
    /// back (the docked rail and the floating peek panel are two instances
    /// of this component; the app pushes every navigation to both).
    MirrorSelection(Sel),
    /// Stop waiting for the launch view's account (#256): its folders did
    /// not arrive in time, so the usual first view is picked instead.
    DropStart,
    /// The accounts are reconnecting and the app has let go of the view on
    /// screen: forget the selection, and pick it again (telling the app)
    /// once its account has listed its folders.
    Reannounce,
    /// Toggle the collapsible "Folders" (custom folders) section for an account.
    ToggleCustomFoldersLocal(u32),
    /// Collapse/expand one folder-tree node (a parent folder's chevron, #51).
    ToggleFolderNode {
        account_id: u32,
        path: String,
    },
    /// A folder was dropped on the top or bottom edge of a sibling: put it
    /// just before or after that one.
    ReorderFolder {
        account_id: u32,
        path: String,
        target: String,
        after: bool,
    },
    ToggleCollapsed,
    /// Show/hide the "Attachments" row in the pinned footer.
    SetAttachmentsRow(bool),
    /// Whether any account is syncing — spins the refresh button.
    SetBusy(bool),
    /// Whether the network is down: the refresh button turns orange (#442).
    SetOffline(bool),
    /// How many messages are waiting to be sent; 0 hides the Outbox row.
    SetOutboxCount(u32),
    /// The "Outbox" row was chosen.
    OutboxRowSelected,
    /// Update unread badges in place without rebuilding the sidebar.
    SetUnread {
        folders: HashMap<(u32, u32), u32>,
        unified: u32,
        /// With "Highlight only new unread mail" on (#343): the folders
        /// with mail come in since they were last looked at. Every other
        /// count is grey. `None`: every count in the accent color.
        fresh: Option<std::collections::HashSet<(u32, u32)>>,
    },
    /// A message drag was dropped on a folder row.
    DropOnFolder {
        account_id: u32,
        path: String,
        payload: String,
    },
    /// A message drag has hovered a collapsed account long enough — expand it.
    ExpandForDrop(u32),
}

#[derive(Debug)]
pub(crate) enum SidebarOutput {
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
        junk: bool,
        trash: bool,
        snoozed: bool,
        favorites: bool,
        smart: bool,
        contacts: bool,
    },
    /// A unified Starred / Sent / Drafts row was chosen: every account's
    /// folder of that kind, merged.
    UnifiedKindSelected(FolderKind),
    /// An account's row under the unified Starred row, for an account with
    /// no Starred folder: its starred mail, wherever it is filed (#482).
    AccountStarredSelected(u32),
    /// The unified Filtered Folders row: every rule's folder, merged.
    UnifiedFilteredSelected,
    /// The Favorites row: every favorite folder, merged.
    UnifiedFavoritesSelected,
    /// The unified Tags row: every tagged message.
    UnifiedTagsSelected,
    /// A smart folder (#417), by its id.
    SmartSelected(String),
    /// The user toggled an account's own Tags section.
    ToggleAccountTags(u32),
    /// The "New message" row at the top of the sidebar.
    ComposeRequested,
    /// Its right-click menu's New Message from Template (#360).
    ComposeFromTemplateRequested,
    /// The refresh button beside it.
    RefreshRequested,
    /// Long-press on the rail's refresh button: reveal the status bar.
    StatusBarRequested,
    /// Right-click on the Contacts row: open the GNOME Contacts app.
    OpenGnomeContacts,
    /// What the contact list shows, picked in the sidebar.
    ContactScope(crate::ui::contacts_page::ContactScope),
    /// Contacts dropped on a book or group in the sidebar.
    ContactDrop {
        keys: Vec<(String, String)>,
        on: crate::ui::contacts_page::DropOn,
        moving: bool,
    },
    /// A right-click choice on a book or group in the sidebar.
    ContactTreeAction(crate::ui::contacts_page::TreeAction),
    /// The accounts and books folded under the Contacts row changed: for
    /// persistence.
    ContactsFolded(Vec<String>),
    /// A folder-tree node was collapsed or expanded (#51) — for persistence.
    FolderNodeCollapsed {
        account_id: u32,
        path: String,
        collapsed: bool,
    },
    /// A folder was dropped onto a new parent ("" = the account's top level).
    MoveFolder {
        account_id: u32,
        path: String,
        dest: String,
    },
    /// The user rearranged a run of sibling folders: `paths` is the whole
    /// run in its new order, for persistence.
    ReorderFolders {
        account_id: u32,
        paths: Vec<String>,
    },
    /// A folder was dropped beside a folder at another level: move it to
    /// `dest` (that level's parent, "" for the top) on the server, then put
    /// it just before or after `target`.
    PlaceFolder {
        account_id: u32,
        path: String,
        dest: String,
        target: String,
        after: bool,
    },
    /// The account's own folder order was chosen from its Folders heading;
    /// `None` follows Settings.
    SetFolderSort {
        account_id: u32,
        sort: Option<crate::config::FolderSort>,
    },
    UnifiedSelected,
    /// A tag was selected (#71): its keyword, and the account it is scoped
    /// to (an account's own Tags section) or `None` for every account.
    TagSelected {
        keyword: String,
        account: Option<u32>,
    },
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
    /// An account's section was opened or folded, by the user or to show a
    /// folder picked inside it: the state it is now in, not a toggle, since
    /// both sidebars (the pane and the peek) report the same reveal and two
    /// toggles cancelled out (the account then folded at the next rebuild).
    AccountFolded {
        id: u32,
        folded: bool,
    },
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
pub(crate) enum CtxAction {
    MarkFolderRead {
        account_id: u32,
        folder_id: u32,
    },
    /// Keep a folder in the Favorites list at the top of the sidebar (#367),
    /// or take it out.
    AddFavorite {
        account_id: u32,
        path: String,
    },
    RemoveFavorite {
        account_id: u32,
        path: String,
    },
    RefreshFolder {
        account_id: u32,
        folder_id: u32,
    },
    MarkAllInboxesRead,
    RefreshAllInboxes,
    /// Erase every unified account's Trash or Junk at once (#413).
    EmptyAll(FolderKind),
    /// Open Settings → Accounts with this account's editor up.
    OpenAccountSettings(u32),
    RemoveAccount(u32),
    /// Create a new custom folder under this account.
    NewFolder(u32),
    /// Delete a custom folder (its contents are moved to Trash first).
    DeleteFolder {
        account_id: u32,
        name: String,
        path: String,
    },
    /// Rename a custom folder (its leaf name; children follow via RENAME).
    RenameFolder {
        account_id: u32,
        name: String,
        path: String,
    },
    /// Erase everything in Trash or Junk (#152).
    EmptyFolder {
        account_id: u32,
        folder_id: u32,
        name: String,
        path: String,
    },
    /// Take a folder out of the sidebar and out of syncing (#239). The
    /// account editor's Hidden Folders list brings it back.
    HideFolder {
        account_id: u32,
        path: String,
    },
    /// Open Settings on the filter rule that files into this folder.
    EditFilter {
        account_id: u32,
        path: String,
    },
    /// Run this account's filter rules over the mail already in this
    /// folder (#198).
    ApplyFilters {
        account_id: u32,
        folder_id: u32,
    },
    /// Open Settings on this tag (by keyword).
    EditTag(String),
    /// Smart folders (#417): make one, or open Settings on one or take it
    /// away, by id.
    NewSmartFolder,
    EditSmartFolder(String),
    DeleteSmartFolder(String),
    /// Put the account's folders back in the server's order.
    ResetFolderOrder(u32),
}
