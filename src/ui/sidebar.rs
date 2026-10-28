//! Left pane: an optional "All Inboxes" (unified) row, then one section per
//! account. Each account has a colored avatar circle + header button (chevron
//! on the right) and an animated `gtk::Revealer` holding its folder list, so
//! expanding/collapsing slides smoothly. Exactly one thing is selected across
//! the unified row and all account folder lists.
//!
//! Account order is managed in the Accounts window, not here; folder order is
//! set here (a folder dragged between its siblings) and handed to the app to keep, which sends it back in `SectionData`. Collapse state is
//! owned by the app (persisted); collapse is animated locally and reported for
//! persistence WITHOUT a full rebuild (which would interrupt the animation).

use std::collections::{HashMap, HashSet};

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::{i18n, i18n_f, i18n_noop};
use crate::models::{Account, Folder, FolderKind};
use crate::ui::context_menu::{show_context_menu, show_context_menu_with_header, MenuEntry};

mod account_section;
mod contents;
mod dnd;
mod menus;
mod messages;
mod open_state;
mod picks;
mod rail;
mod rebuild;
mod selection;
mod tree;
mod types;
mod unified;
mod widgets;

pub(crate) use messages::{CtxAction, SidebarInput, SidebarOutput};
pub(crate) use tree::Hierarchy;
pub(crate) use tree::{folder_depth, order_folders, placed_run, TREE_EXPANDER_WIDTH};
pub(crate) use types::kind_label;
use types::{InboxRef, KindWidgets, SectionWidgets, UnifiedRow};
pub(crate) use types::{SectionData, Sel, SidebarInit, Slot, StartTarget, UnifiedFolderRef};
pub(crate) use widgets::{account_initials, build_unified_sub_row, pin_icon_size};

pub(crate) struct Sidebar {
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
    /// Whether a click anywhere on an account's header folds it (Settings →
    /// Sidebar); off, its chevron or a double click does.
    account_click_folds: bool,
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
    /// The folders inside the Inbox, listed under its row (#345): each
    /// account's main-list folders in row order, and the revealers of the
    /// nested ones by row.
    main_folders: HashMap<u32, Vec<Folder>>,
    main_row_revealers: HashMap<u32, HashMap<usize, gtk::Revealer>>,
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
    /// The pinned footer's single list box (Contacts + Attachments rows),
    /// for selection management.
    footer_list: Option<gtk::ListBox>,
    /// The address books and groups the Contacts row lists.
    contact_tree: Option<crate::ui::contacts_page::ContactTree>,
    /// That list's rows, by the scope each picks; shared with its
    /// selection handler.
    tree_rows: std::rc::Rc<
        std::cell::RefCell<Vec<(crate::ui::contacts_page::ContactScope, gtk::ListBoxRow)>>,
    >,
    attachments_row: Option<gtk::ListBoxRow>,
    /// The footer's Contacts row, when Contacts is placed there.
    contacts_row: Option<gtk::ListBoxRow>,
    /// Refresh/spinner stack + spinner beside the "New Message" button.
    sync_stack: Option<gtk::Stack>,
    sync_spinner: Option<gtk::Spinner>,
    /// Whether any account is syncing (drives the refresh spinner).
    busy: bool,
    /// Whether the network is down (turns the refresh button orange).
    offline: bool,
    /// The rail's refresh button, for its offline face.
    sync_button: Option<gtk::Button>,
    /// The "Outbox" row list box (one row), while anything is queued.
    outbox_list: Option<gtk::ListBox>,
    /// Display-wide provider holding each account's avatar color rules.
    color_provider: crate::ui::DisplayCss,
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
    /// The launch view still to be picked (#256): kept until the account
    /// it names has its folders listed, then taken by the first pick.
    start: Option<StartTarget>,
    /// The view to pick again, and announce, once its account is listed
    /// again: the app dropped what it showed when the accounts reconnected
    /// (an account saved), while this list still showed it selected. The
    /// account is held by address: ids follow the account order, which a
    /// removal shifts.
    reannounce: Option<(Sel, Option<String>)>,
    /// Whether the "Attachments" row is shown (in the pinned footer).
    show_attachments: bool,
    /// How many messages are waiting in the Outbox across all accounts. The row
    /// only exists while this is non-zero — an empty Outbox is the normal state
    /// and does not deserve permanent furniture.
    outbox_count: u32,
    /// Total unread across all inboxes, for the "All Inboxes" badge.
    unified_unread: u32,
    /// The folders whose unread count is new since last looked at, while
    /// "Highlight only new unread mail" is on (#343); `None` when it is off.
    fresh: Option<std::collections::HashSet<(u32, u32)>>,
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
    /// The favorite folders (#367), in the order they were added.
    favorite_folders: Vec<UnifiedFolderRef>,
    favorites_expanded: bool,
    /// The smart folders (#417) as (id, name), in their saved order, with
    /// each one's unread count and the badge showing it.
    smart_folders: Vec<(String, String)>,
    smart_expanded: bool,
    /// Whether the Contacts row's address books are shown.
    contacts_expanded: bool,
    /// The accounts and books folded under it, by key.
    contacts_folded: HashSet<String>,
    /// Its rows that fold, and the carets that fold them.
    contact_layout: Vec<crate::ui::contact_tree::TreeRow>,
    contact_chevrons: Vec<(String, gtk::Image)>,
    smart_unread: HashMap<String, u32>,
    smart_badges: HashMap<String, gtk::Label>,
    /// The rows of every Filtered Folders section (the unified one and each
    /// account's own), in row order, with the sections' widgets and badges.
    filtered_rows: HashMap<Slot, Vec<UnifiedFolderRef>>,
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
    /// Whether the account sections are shown at all.
    show_accounts: bool,
    /// Where the Filters, Tags, Favorites and Contacts sections sit
    /// (Settings → Sidebar), and whether they are drawn as unified rows or
    /// as a tree.
    section_style: crate::config::SectionStyle,
    filtered_placement: crate::config::SectionPlacement,
    tags_placement: crate::config::SectionPlacement,
    favorites_placement: crate::config::SectionPlacement,
    contacts_placement: crate::config::SectionPlacement,
    /// Focus Mode: the account sections are hidden.
    focus_hide_accounts: bool,
    /// Focus Mode: the unified rows show folded, whatever their saved
    /// state; a click opens one for the mode alone (`rail_open`), the way
    /// the icon rail does.
    focus_fold_unified: bool,
    /// The revealer holding every account section, so Focus Mode can slide
    /// them away as one and bring them back the same way.
    accounts_revealer: Option<gtk::Revealer>,
    /// The next rebuild draws the accounts folded away and slides them in
    /// (Focus Mode has just given them back).
    reveal_accounts_late: bool,
}

#[relm4::component(pub(crate))]
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
        let color_provider = crate::ui::DisplayCss::new();

        let mut model = Sidebar {
            sections: Vec::new(),
            show_unified: false,
            unified_chips: crate::config::UnifiedChips::default(),
            chevrons_left: false,
            account_click_folds: false,
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
            main_folders: HashMap::new(),
            main_row_revealers: HashMap::new(),
            freeze_frame: None,
            freeze_timer: std::rc::Rc::new(std::cell::RefCell::new(None)),
            freeze_fade: std::rc::Rc::new(std::cell::RefCell::new(None)),
            built_collapsed: init.collapsed,
            custom_revealers: HashMap::new(),
            custom_chevrons: HashMap::new(),
            unified_list: None,
            footer_list: None,
            contact_tree: None,
            tree_rows: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
            attachments_row: None,
            contacts_row: None,
            sync_stack: None,
            sync_spinner: None,
            busy: false,
            offline: !crate::desktop::power::online(),
            sync_button: None,
            outbox_list: None,
            color_provider,
            selected: Sel::None,
            collapsed: init.collapsed,
            quiet: std::rc::Rc::new(std::cell::Cell::new(false)),
            mirror: init.mirror,
            start: if init.mirror { None } else { init.start },
            reannounce: None,
            show_attachments: init.show_attachments,
            outbox_count: 0,
            unified_unread: 0,
            fresh: None,
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
            favorite_folders: Vec::new(),
            favorites_expanded: init.favorites_expanded,
            smart_folders: Vec::new(),
            smart_expanded: init.smart_expanded,
            contacts_expanded: init.contacts_expanded,
            contacts_folded: init.contacts_folded.iter().cloned().collect(),
            contact_layout: Vec::new(),
            contact_chevrons: Vec::new(),
            smart_unread: HashMap::new(),
            smart_badges: HashMap::new(),
            filtered_rows: HashMap::new(),
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
                (FolderKind::Junk, init.junk_expanded),
                (FolderKind::Trash, init.trash_expanded),
                (FolderKind::Snoozed, init.snoozed_expanded),
            ]),
            kind_widgets: HashMap::new(),
            rail_open: HashMap::new(),
            rail_open_accounts: HashMap::new(),
            show_accounts: true,
            section_style: crate::config::SectionStyle::default(),
            filtered_placement: crate::config::SectionPlacement::AboveAccounts,
            tags_placement: crate::config::SectionPlacement::AboveAccounts,
            favorites_placement: crate::config::SectionPlacement::AboveAccounts,
            contacts_placement: crate::config::SectionPlacement::AboveAccounts,
            focus_hide_accounts: false,
            focus_fold_unified: false,
            accounts_revealer: None,
            reveal_accounts_late: false,
        };

        let widgets = view_output!();
        model.freeze_frame = Some(widgets.freeze_frame.clone());
        widgets
            .body_overlay
            .set_clip_overlay(&widgets.freeze_frame, true);
        // Never scroll-to-focus: a rebuild (folder drag-and-drop) destroys
        // the focused row, GTK hands focus to some early widget, and the
        // viewport would yank the sidebar to the top to show it — the
        // "jumps up then back down" on every drop. Sidebar scrolling is the
        // user's alone.
        if let Some(viewport) = widgets
            .sidebar_scroller
            .child()
            .and_downcast::<gtk::Viewport>()
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
            SidebarInput::DropStart => self.on_drop_start(),
            SidebarInput::Reannounce => self.on_reannounce(sender),
            SidebarInput::MirrorSelection(sel) => self.on_mirror_selection(sel),
            msg @ SidebarInput::SetContents { .. } => self.on_set_contents(msg, widgets, sender),
            SidebarInput::UnifiedRowSelected => self.on_unified_row_selected(sender),
            SidebarInput::SelectUnifiedRow => self.on_select_unified_row(sender),
            SidebarInput::AttachmentsRowSelected => self.on_attachments_row_selected(sender),
            SidebarInput::ContactsRowClicked => self.on_contacts_row_clicked(sender),
            SidebarInput::SetContactTree(tree) => self.on_set_contact_tree(tree, sender),
            SidebarInput::ToggleContactsExpand => {
                self.toggle_row(UnifiedRow::Contacts, &sender);
            }
            SidebarInput::ToggleContactNode(key) => self.on_toggle_contact_node(key, sender),
            SidebarInput::ContactTreeSelected(scope) => {
                self.on_contact_tree_selected(scope, sender)
            }
            SidebarInput::ContactTreeDrop { keys, on, moving } => {
                let _ = sender.output(SidebarOutput::ContactDrop { keys, on, moving });
            }
            SidebarInput::ContactTreeMenu(action) => {
                let _ = sender.output(SidebarOutput::ContactTreeAction(action));
            }
            SidebarInput::OutboxRowSelected => self.on_outbox_row_selected(sender),
            SidebarInput::SetOutboxCount(count) => self.on_set_outbox_count(count, widgets, sender),
            SidebarInput::UnifiedInboxRowSelected(index) => {
                self.on_unified_inbox_row_selected(index, sender)
            }
            SidebarInput::ToggleUnifiedExpand => {
                self.toggle_row(UnifiedRow::Kind(FolderKind::Inbox), &sender)
            }
            SidebarInput::FilteredRowSelected { slot, index } => {
                self.on_filtered_row_selected(slot, index, sender)
            }
            SidebarInput::TagRowSelected { slot, index } => {
                self.on_tag_row_selected(slot, index, sender)
            }
            // The unified slot is a unified row; an account's is a heading.
            SidebarInput::ToggleTagsExpand(slot) => self.on_toggle_tags_expand(slot, sender),
            SidebarInput::ToggleFavoritesExpand => {
                self.toggle_row(UnifiedRow::Favorites, &sender);
            }
            SidebarInput::ToggleSmartExpand => self.on_toggle_smart_expand(sender),
            SidebarInput::SmartRowSelected(index) => self.on_smart_row_selected(index, sender),
            SidebarInput::SetSmartUnread(counts) => self.on_set_smart_unread(counts),
            SidebarInput::FavoriteRowSelected(index) => {
                self.on_favorite_row_selected(index, sender)
            }
            SidebarInput::ToggleFilteredExpand(slot) => {
                self.on_toggle_filtered_expand(slot, sender)
            }
            SidebarInput::UnifiedKindRowSelected(kind) => {
                self.on_unified_kind_row_selected(kind, sender)
            }
            SidebarInput::UnifiedFavoritesSelected => self.on_unified_favorites_selected(sender),
            SidebarInput::UnifiedFilteredSelected => self.on_unified_filtered_selected(sender),
            SidebarInput::UnifiedTagsSelected => self.on_unified_tags_selected(sender),
            SidebarInput::KindSubRowSelected { kind, index } => {
                self.on_kind_sub_row_selected(kind, index, sender)
            }
            SidebarInput::ToggleKindExpand(kind) => {
                self.toggle_row(UnifiedRow::Kind(kind), &sender)
            }
            SidebarInput::FolderRowSelected { account_id, index } => {
                self.on_folder_row_selected(account_id, index, sender)
            }
            SidebarInput::SelectFolderRow { account_id, path } => {
                self.on_select_folder_row(account_id, path, sender)
            }
            SidebarInput::SetUnread {
                folders,
                unified,
                fresh,
            } => self.on_set_unread(folders, unified, fresh),
            SidebarInput::SetAttachmentsRow(show) => {
                self.on_set_attachments_row(show, widgets, sender)
            }
            SidebarInput::SetBusy(busy) => self.on_set_busy(busy),
            SidebarInput::SetOffline(offline) => self.on_set_offline(offline),
            SidebarInput::SetFocus {
                hide_accounts,
                fold_unified,
                animate,
            } => self.on_set_focus(hide_accounts, fold_unified, animate, widgets, sender),
            SidebarInput::FocusSettled => self.on_focus_settled(widgets, sender),
            SidebarInput::SetCollapsed(collapsed) => {
                self.on_set_collapsed(collapsed, widgets, sender)
            }
            SidebarInput::ToggleCollapsed => self.on_toggle_collapsed(widgets, sender),
            SidebarInput::ToggleCollapseLocal(id) => self.on_toggle_collapse_local(id, sender),
            SidebarInput::ToggleCustomFoldersLocal(id) => {
                self.on_toggle_custom_folders_local(id, sender)
            }
            SidebarInput::ToggleFolderNode { account_id, path } => {
                self.toggle_folder_node(account_id, path, &sender);
            }
            SidebarInput::ReorderFolder {
                account_id,
                path,
                target,
                after,
            } => self.on_reorder_folder(account_id, path, target, after, sender),
            SidebarInput::ExpandForDrop(id) => self.on_expand_for_drop(id, sender),
            SidebarInput::DropOnFolder {
                account_id: dest_account,
                path: dest,
                payload,
            } => self.on_drop_on_folder(dest_account, dest, payload, sender),
        }
    }
}

#[cfg(test)]
mod tests;
