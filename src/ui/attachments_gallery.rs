//! Attachments gallery: a grid of every cached attachment across the connected
//! inboxes, with a lightbox preview (prev/next, open, go to message).
//!
//! Data comes from the SQLite cache (what the background prefetch has already
//! downloaded), fed in via [`GalleryInput::SetItems`]. Image attachments and PDFs
//! (rendered from their first page) show as thumbnails; other files show a type
//! icon. Clicking a cell opens a large overlay preview.

use adw::prelude::*;
use gtk::gdk;
use gtk::glib;
use relm4::prelude::*;

use std::collections::HashMap;

use crate::models::{ext_of, is_image_name, FolderKind, GalleryItem, GallerySort};
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use crate::i18n::{i18n, i18n_f};

/// Width of the table's trailing quick-actions column (three icon buttons);
/// the header carries a spacer of the same width so the columns line up.
const TABLE_ACTIONS_WIDTH: i32 = 100;

/// How many attachments one page holds. Big enough that scrolling rarely waits,
/// small enough that the first screen appears at once and a page's worth of
/// cached thumbnails is not a burden.
const PAGE_SIZE: u32 = 120;

/// How close to the end of the list the scroll has to come before the next page
/// is asked for, in px. Roughly two rows of thumbnails, so the page is usually
/// already there by the time the user reaches it.
const LOAD_MORE_MARGIN: f64 = 700.0;

/// How long typing has to pause before the search runs, in ms. Each search is a
/// query over the whole archive and a rebuilt grid, so running one per keystroke
/// would spend most of its time on words half-typed.
const SEARCH_DEBOUNCE_MS: u32 = 250;

/// One page's worth of question for the cache: everything the database needs to
/// narrow and order the archive before cutting the page out of it.
#[derive(Debug, Clone)]
pub struct GalleryRequest {
    /// `(account id, folder path)` for every folder in scope.
    pub folders: Vec<(u32, String)>,
    pub account_id: Option<u32>,
    pub tokens: Vec<String>,
    pub bucket: u32,
    pub sort: GallerySort,
    pub offset: u32,
    pub limit: u32,
}

/// One folder offered in the gallery's folder list.
#[derive(Debug, Clone)]
pub struct GalleryFolder {
    pub path: String,
    /// Display name, as the sidebar spells it.
    pub name: String,
    pub kind: FolderKind,
}

/// One account's share of the folder list: the account's own label and the
/// folders whose attachments the gallery could draw on.
#[derive(Debug, Clone)]
pub struct GalleryAccount {
    pub id: u32,
    pub label: String,
    pub folders: Vec<GalleryFolder>,
}

/// Whether a folder of this kind feeds the gallery unless the user says
/// otherwise. Sent is the one eligible kind that starts off: what you sent is
/// rarely what you are looking for, and its attachments are usually copies of
/// files you already have. Drafts, Junk and Trash never reach the gallery at
/// all (the cache query drops them), so they are not offered here.
fn kind_default(kind: FolderKind) -> bool {
    !matches!(kind, FolderKind::Sent)
}

/// Which master switch governs a folder kind: Archive folders follow "Include
/// Archive", anything that is neither an inbox nor an archive follows "Include
/// other folders", and inboxes follow neither (they are always on).
fn kind_master(kind: FolderKind) -> Option<Master> {
    match kind {
        FolderKind::Inbox => None,
        FolderKind::Archive => Some(Master::Archive),
        _ => Some(Master::Other),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Master {
    Archive,
    Other,
}

/// Whether a folder's attachments are in scope: its master switch has to be on
/// *and* it has to be ticked. A master that is off wins over a tick rather than
/// clearing it, so turning the master back on restores each folder to whatever
/// the user had chosen. `tick` is the user's own choice, or `None` when they
/// have not touched this folder and its kind's default stands.
fn in_scope(kind: FolderKind, include_archive: bool, include_other: bool, tick: Option<bool>) -> bool {
    let master_on = match kind_master(kind) {
        Some(Master::Archive) => include_archive,
        Some(Master::Other) => include_other,
        None => true,
    };
    master_on && tick.unwrap_or_else(|| kind_default(kind))
}

pub struct AttachmentsGallery {
    /// The pages loaded so far, already filtered and ordered by the database.
    /// This is a window onto `total`, not the whole set: an archive of two
    /// decades holds far more attachments than the UI can hold widgets for, so
    /// the list grows a page at a time as the view is scrolled.
    all_items: Vec<GalleryItem>,
    /// How many attachments match the current query in total, across every
    /// page — what the footer counts and how `has_more` is known.
    total: u32,
    /// A page request is in flight; the footer shows its spinner and no second
    /// request is sent until it lands.
    loading_more: bool,
    /// Messages the scan has still to describe across the folders in scope.
    /// Non-zero means the gallery is not yet showing everything there is.
    scan_remaining: u32,
    /// An attachment that was never downloaded is on its way from the server.
    fetching: bool,
    query: String,
    sort: GallerySort,
    /// Index into `items` currently shown in the lightbox, if any.
    preview: Option<usize>,
    /// What the lightbox shows for the current item: a decoded image, or a
    /// PDF's first page once its full-size render lands (None while a PDF
    /// render is in flight, and for types with nothing to show).
    preview_texture: Option<gdk::Texture>,
    loading: bool,
    /// Show the sortable table instead of the thumbnail grid (persisted).
    view_table: bool,
    /// Grid thumbnail cell width in px, driven by the footer slider (persisted).
    thumb_width: i32,
    /// The footer type dropdown's row: 0 = all, then one bucket per row.
    type_filter: u32,
    /// Show only one account's attachments; `None` shows every account. Unlike
    /// the folder scope this is a view filter, not a setting, so it resets to
    /// "All accounts" each time the gallery is opened.
    account_filter: Option<u32>,
    /// Every account and the folders it offers, as fed by the app.
    accounts: Vec<GalleryAccount>,
    /// Pull from Archive folders (persisted master switch).
    include_archive: bool,
    /// Pull from folders that are neither Inbox nor Archive (persisted).
    include_other: bool,
    /// The folders the user ticked or unticked by hand, overriding
    /// [`kind_default`]: account id -> path -> on. Persisted.
    overrides: HashMap<u32, HashMap<String, bool>>,
    /// Effective per-folder verdict, recomputed from the master switches and
    /// `overrides` whenever either changes, so filtering is a lookup.
    included: HashMap<u32, HashMap<String, bool>>,
    /// Debounce for the size slider — one rebuild after the drag settles.
    resize_timer: Option<glib::SourceId>,
    /// Debounce for the search box — one query after the typing settles.
    query_timer: Option<glib::SourceId>,
    flow: gtk::FlowBox,
    /// The table view's rows (the grid's sibling stack page).
    table: gtk::ListBox,
    /// The folder list inside the footer's "Folders" popover, rebuilt whenever
    /// the account/folder set changes.
    folder_list: gtk::ListBox,
    /// The account filter dropdown's model: "All accounts" then one row per
    /// account, in `accounts` order.
    account_names: gtk::StringList,
    /// The component's root widget, used to anchor the right-click context menu.
    root: gtk::Widget,
}

#[derive(Debug)]
pub enum GalleryInput {
    /// A page came back from the cache: `offset` 0 replaces the list, anything
    /// else appends. `total` is how many the query matches altogether.
    Page { items: Vec<GalleryItem>, total: u32, offset: u32 },
    SetLoading(bool),
    /// The scroll came near the end — ask for the next page if there is one.
    LoadMore,
    /// The scan described more attachments. Refreshes the view only while the
    /// user is still on the first page, so a scan landing mid-scroll cannot
    /// pull the ground out from under them.
    ScanProgress,
    /// How many messages the scan still has to describe, across every folder in
    /// scope; 0 once the archive is fully indexed.
    ScanStatus(u32),
    /// A file the gallery knew of but had not downloaded is being fetched, so
    /// the lightbox can show a spinner instead of an empty frame.
    SetFetching(bool),
    /// A message's attachments arrived: fill in the bytes for every loaded row
    /// of that message, so its thumbnail and preview appear in place.
    Fetched { account_id: u32, uid: u32, items: Vec<crate::models::Attachment> },
    /// Filter the grid to items matching this search text (sender, subject,
    /// filename, folder, and type keywords like "pdf" or "spreadsheet").
    /// Debounced — the query itself runs on [`GalleryInput::ApplyQuery`].
    SetQuery(String),
    /// The typing settled — run the search.
    ApplyQuery,
    /// Re-sort the grid; the value is the sort dropdown's selected row index.
    SetSort(u32),
    /// A table column header was clicked: sort by that column, or flip its
    /// direction when it is already the active column (0 name, 1 sender,
    /// 2 date, 3 size, 4 type).
    SortColumn(u8),
    /// Switch between the thumbnail grid and the table.
    SetViewTable(bool),
    /// The footer size slider moved (grid thumbnail width, px).
    SetThumbWidth(f64),
    /// The size slider settled — rebuild the grid at the new width.
    ApplyThumbWidth,
    /// Show only one type bucket (the footer type dropdown's row; 0 = all).
    SetTypeFilter(u32),
    /// The accounts and folders the gallery may draw on, sent by the app when
    /// the gallery opens. Rebuilds the folder list and the account dropdown.
    SetAccounts(Vec<GalleryAccount>),
    /// Show only one account (the footer account dropdown's row; 0 = all).
    SetAccountFilter(u32),
    /// Master switch: pull from Archive folders.
    SetIncludeArchive(bool),
    /// Master switch: pull from folders that are neither Inbox nor Archive.
    SetIncludeOther(bool),
    /// One folder was ticked or unticked in the folder list.
    SetFolder { account_id: u32, path: String, on: bool },
    /// A grid cell was activated (single click) — open the lightbox on that item.
    Activate(u32),
    Prev,
    Next,
    ClosePreview,
    /// Open the current (previewed) item's file in its default application.
    OpenCurrent,
    /// Jump to the current (previewed) item's source message.
    GoToCurrent,
    /// Open item `index` externally (double-click / context menu / lightbox).
    OpenItem(usize),
    /// Save item `index` to a file the user picks.
    DownloadItem(usize),
    /// Jump to item `index`'s source message.
    GoToItem(usize),
    /// A cell was double-clicked: open it externally, closing any preview.
    OpenExternal(usize),
    /// Right-click on cell `index` at `(x, y)` (cell-relative) — show its menu there.
    ContextMenu { index: usize, x: f64, y: f64 },
    /// A lightbox-size PDF render finished (keyed by content hash) — show it
    /// if that PDF is still the one being previewed.
    PreviewRendered(u64),
    /// Showcase only (VIREO_SHOWCASE_GALLERY_FOLDERS): drop the footer's
    /// folder popover open so a capture can see it.
    ShowcaseFolders,
    /// Showcase only (VIREO_SHOWCASE_GALLERY_SEARCH): focus the search box and
    /// put text in it, as typing would, then report whether the box still has
    /// the focus once the search has run. Guards the bug where a search that
    /// matched nothing hid the toolbar the box lives in.
    ShowcaseSearch(String),
}

#[derive(Debug)]
pub enum GalleryOutput {
    /// Open the source message of an attachment in the reader.
    OpenMessage { account_id: u32, folder_path: String, uid: u32 },
    /// Run this query against the cache and send the page back.
    Load(GalleryRequest),
    /// Fetch an attachment whose bytes were never downloaded, so it can be
    /// opened or previewed.
    Fetch { account_id: u32, folder_path: String, uid: u32 },
}

#[relm4::component(pub)]
impl Component for AttachmentsGallery {
    type Init = ();
    type Input = GalleryInput;
    type Output = GalleryOutput;
    type CommandOutput = ();

    view! {
        gtk::Overlay {
            add_css_class: "attachments-gallery",

            // Base layer: a search/sort toolbar above the scrolling grid.
            #[wrap(Some)]
            set_child = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                gtk::Box {
                    add_css_class: "gallery-toolbar",
                    set_spacing: 8,
                    #[watch]
                    set_visible: model.show_chrome(),

                    #[name = "search_entry"]
                    gtk::SearchEntry {
                        set_hexpand: true,
                        set_placeholder_text: Some(i18n("Search by sender, subject, type, filename…").as_str()),
                        connect_search_changed[sender] => move |e| {
                            sender.input(GalleryInput::SetQuery(e.text().to_string()));
                        },
                    },
                },

                gtk::Stack {
                    set_vexpand: true,
                    set_transition_type: gtk::StackTransitionType::Crossfade,
                    #[watch]
                    set_visible_child_name: model.page(),

                    add_named[Some("loading")] = &gtk::Box {
                        set_halign: gtk::Align::Center,
                        set_valign: gtk::Align::Center,
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 14,
                        gtk::Spinner { set_spinning: true, set_width_request: 36, set_height_request: 36 },
                        gtk::Label { set_label: &i18n("Loading attachments…"), add_css_class: "dim-label" },
                    },

                    add_named[Some("empty")] = &adw::StatusPage {
                        set_icon_name: Some("co.hyprlab.Vireo-mail-attachment-symbolic"),
                        set_title: &i18n("No attachments"),
                        set_description: Some(i18n("Attachments from your inboxes will appear here.").as_str()),
                    },

                    add_named[Some("noresults")] = &adw::StatusPage {
                        set_icon_name: Some("co.hyprlab.Vireo-system-search-symbolic"),
                        set_title: &i18n("No matching attachments"),
                        set_description: Some(i18n("Try a different search, or check which accounts and folders the footer is pulling from.").as_str()),
                    },

                    add_named[Some("grid")] = &gtk::ScrolledWindow {
                    set_hscrollbar_policy: gtk::PolicyType::Never,
                    set_vexpand: true,
                    connect_edge_reached[sender] => move |_, pos| {
                        if pos == gtk::PositionType::Bottom {
                            sender.input(GalleryInput::LoadMore);
                        }
                    },
                    #[wrap(Some)]
                    set_vadjustment = &gtk::Adjustment {
                        connect_value_changed[sender] => move |a| {
                            if near_end(a) {
                                sender.input(GalleryInput::LoadMore);
                            }
                        },
                    },

                    #[local_ref]
                    flow -> gtk::FlowBox {
                        set_valign: gtk::Align::Start,
                        set_max_children_per_line: 8,
                        set_min_children_per_line: 3,
                        set_row_spacing: 14,
                        set_column_spacing: 14,
                        set_homogeneous: true,
                        set_selection_mode: gtk::SelectionMode::None,
                        set_activate_on_single_click: true,
                        add_css_class: "gallery-flow",
                        connect_child_activated[sender] => move |_, child| {
                            sender.input(GalleryInput::Activate(child.index() as u32));
                        },
                    },
                    },

                    // Table view: fixed sortable column headers over the rows.
                    add_named[Some("table")] = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,

                        gtk::Box {
                            add_css_class: "gallery-table-header",
                            set_spacing: 10,

                            // Aligns the headers with the rows' leading thumbnail.
                            gtk::Box { set_width_request: 28 },
                            gtk::Button {
                                add_css_class: "flat",
                                set_hexpand: true,
                                connect_clicked => GalleryInput::SortColumn(0),
                                gtk::Label {
                                    #[watch]
                                    set_label: &column_header("Name", model.sort, GallerySort::Name, GallerySort::NameDesc),
                                    set_xalign: 0.0,
                                },
                            },
                            gtk::Button {
                                add_css_class: "flat",
                                set_width_request: 170,
                                set_hexpand: false,
                                connect_clicked => GalleryInput::SortColumn(1),
                                gtk::Label {
                                    #[watch]
                                    set_label: &column_header("Sender", model.sort, GallerySort::Sender, GallerySort::SenderDesc),
                                    set_xalign: 0.0,
                                },
                            },
                            gtk::Button {
                                add_css_class: "flat",
                                set_width_request: 90,
                                set_hexpand: false,
                                connect_clicked => GalleryInput::SortColumn(4),
                                gtk::Label {
                                    #[watch]
                                    set_label: &column_header("Type", model.sort, GallerySort::Type, GallerySort::TypeDesc),
                                    set_xalign: 0.0,
                                },
                            },
                            gtk::Button {
                                add_css_class: "flat",
                                set_width_request: 110,
                                set_hexpand: false,
                                connect_clicked => GalleryInput::SortColumn(2),
                                gtk::Label {
                                    #[watch]
                                    set_label: &column_header("Date", model.sort, GallerySort::Oldest, GallerySort::Newest),
                                    set_xalign: 0.0,
                                },
                            },
                            // Only Name expands, as in the rows: a header
                            // that also took spare width (a child's hexpand
                            // propagates to its button) would shift every
                            // column between them off its cells.
                            gtk::Button {
                                add_css_class: "flat",
                                set_width_request: 90,
                                set_hexpand: false,
                                connect_clicked => GalleryInput::SortColumn(3),
                                gtk::Label {
                                    #[watch]
                                    set_label: &column_header("Size", model.sort, GallerySort::Smallest, GallerySort::Largest),
                                    set_xalign: 1.0,
                                },
                            },
                            // Aligns with the rows' trailing actions column.
                            gtk::Box { set_width_request: TABLE_ACTIONS_WIDTH },
                        },

                        gtk::ScrolledWindow {
                            set_hscrollbar_policy: gtk::PolicyType::Never,
                            set_vexpand: true,
                            connect_edge_reached[sender] => move |_, pos| {
                                if pos == gtk::PositionType::Bottom {
                                    sender.input(GalleryInput::LoadMore);
                                }
                            },
                            #[wrap(Some)]
                            set_vadjustment = &gtk::Adjustment {
                                connect_value_changed[sender] => move |a| {
                                    if near_end(a) {
                                        sender.input(GalleryInput::LoadMore);
                                    }
                                },
                            },

                            #[local_ref]
                            table -> gtk::ListBox {
                                set_selection_mode: gtk::SelectionMode::None,
                                set_activate_on_single_click: true,
                                add_css_class: "gallery-table",
                                connect_row_activated[sender] => move |_, row| {
                                    sender.input(GalleryInput::Activate(row.index() as u32));
                                },
                            },
                        },
                    },
                },

                // Footer: view toggle, filtering/ordering, size, count — the
                // gallery's controls in one place, out of the content's way.
                gtk::ActionBar {
                    add_css_class: "gallery-footer",
                    #[watch]
                    set_revealed: model.show_chrome(),

                    pack_start = &gtk::Box {
                        add_css_class: "linked",

                        gtk::ToggleButton {
                            set_icon_name: "co.hyprlab.Vireo-view-grid-symbolic",
                            set_tooltip_text: Some(i18n("Thumbnail grid").as_str()),
                            #[watch]
                            #[block_signal(grid_toggle)]
                            set_active: !model.view_table,
                            connect_clicked[sender] => move |_| {
                                sender.input(GalleryInput::SetViewTable(false));
                            } @grid_toggle,
                        },
                        gtk::ToggleButton {
                            set_icon_name: "co.hyprlab.Vireo-view-list-bullet-symbolic",
                            set_tooltip_text: Some(i18n("Table").as_str()),
                            #[watch]
                            #[block_signal(table_toggle)]
                            set_active: model.view_table,
                            connect_clicked[sender] => move |_| {
                                sender.input(GalleryInput::SetViewTable(true));
                            } @table_toggle,
                        },
                    },

                    // Scope: which account and which folders feed the gallery.
                    #[name = "account_dropdown"]
                    pack_start = &gtk::DropDown {
                        set_tooltip_text: Some(i18n("Show only this account").as_str()),
                        // Pointless with one account, and it would be the only
                        // control there that could never change anything.
                        #[watch]
                        set_visible: model.accounts.len() > 1,
                        set_model: Some(&model.account_names),
                        connect_selected_notify[sender] => move |d| {
                            sender.input(GalleryInput::SetAccountFilter(d.selected()));
                        },
                    },

                    #[name = "folders_button"]
                    pack_start = &gtk::MenuButton {
                        set_icon_name: "co.hyprlab.Vireo-folder-symbolic",
                        set_tooltip_text: Some(i18n("Folders to pull from").as_str()),

                        #[wrap(Some)]
                        set_popover = &gtk::Popover {
                            add_css_class: "gallery-folders-popover",

                            #[name = "folders_box"]
                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 10,
                                set_width_request: 320,

                                gtk::ListBox {
                                    add_css_class: "boxed-list",
                                    set_selection_mode: gtk::SelectionMode::None,

                                    #[name = "archive_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Include Archive"),
                                        #[watch]
                                        #[block_signal(archive_toggle)]
                                        set_active: model.include_archive,
                                        connect_active_notify[sender] => move |r| {
                                            sender.input(GalleryInput::SetIncludeArchive(r.is_active()));
                                        } @archive_toggle,
                                    },
                                    #[name = "other_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Include other folders"),
                                        set_subtitle: &i18n("Everything that is not an inbox or an archive"),
                                        #[watch]
                                        #[block_signal(other_toggle)]
                                        set_active: model.include_other,
                                        connect_active_notify[sender] => move |r| {
                                            sender.input(GalleryInput::SetIncludeOther(r.is_active()));
                                        } @other_toggle,
                                    },
                                },

                                gtk::Label {
                                    set_label: &i18n("Pulling attachments from"),
                                    set_xalign: 0.0,
                                    add_css_class: "heading",
                                },

                                gtk::ScrolledWindow {
                                    set_hscrollbar_policy: gtk::PolicyType::Never,
                                    set_propagate_natural_height: true,
                                    set_max_content_height: 380,

                                    #[local_ref]
                                    folder_list -> gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                    },
                                },
                            },
                        },
                    },

                    pack_start = &gtk::DropDown {
                        set_tooltip_text: Some(i18n("Show only this type").as_str()),
                        #[wrap(Some)]
                        set_model = &gtk::StringList::new(&[
                            i18n("All types").as_str(),
                            i18n("Images").as_str(),
                            i18n("PDFs").as_str(),
                            i18n("Documents").as_str(),
                            i18n("Archives").as_str(),
                            i18n("Audio & Video").as_str(),
                            i18n("Other").as_str(),
                        ]),
                        connect_selected_notify[sender] => move |d| {
                            sender.input(GalleryInput::SetTypeFilter(d.selected()));
                        },
                    },

                    #[name = "sort_dropdown"]
                    pack_start = &gtk::DropDown {
                        set_tooltip_text: Some(i18n("Sort").as_str()),
                        set_selected: model.sort.index(),
                        #[wrap(Some)]
                        set_model = &gtk::StringList::new(&[
                            i18n("Newest first").as_str(),
                            i18n("Oldest first").as_str(),
                            i18n("Name (A–Z)").as_str(),
                            i18n("Name (Z–A)").as_str(),
                            i18n("Sender (A–Z)").as_str(),
                            i18n("Sender (Z–A)").as_str(),
                            i18n("Largest first").as_str(),
                            i18n("Smallest first").as_str(),
                            i18n("Type (A–Z)").as_str(),
                            i18n("Type (Z–A)").as_str(),
                        ]),
                        connect_selected_notify[sender] => move |d| {
                            sender.input(GalleryInput::SetSort(d.selected()));
                        },
                    },

                    pack_end = &gtk::Label {
                        add_css_class: "dim-label",
                        #[watch]
                        set_label: &count_text(model.all_items.len(), model.total as usize),
                    },

                    // What the network is doing, in the one place the gallery
                    // keeps its status: a page on its way, or the scan still
                    // working back through the archive.
                    pack_end = &gtk::Box {
                        set_spacing: 6,
                        set_valign: gtk::Align::Center,
                        #[watch]
                        set_visible: model.loading_more || model.scan_remaining > 0,

                        gtk::Spinner {
                            #[watch]
                            set_spinning: model.loading_more || model.scan_remaining > 0,
                            set_width_request: 16,
                            set_height_request: 16,
                        },
                        gtk::Label {
                            add_css_class: "dim-label",
                            #[watch]
                            set_label: &scan_text(model.loading_more, model.scan_remaining),
                        },
                    },

                    pack_end = &gtk::Scale {
                        set_range: (140.0, 380.0),
                        set_value: model.thumb_width as f64,
                        set_increments: (10.0, 40.0),
                        set_width_request: 140,
                        set_draw_value: false,
                        set_tooltip_text: Some(i18n("Thumbnail size").as_str()),
                        #[watch]
                        set_visible: !model.view_table,
                        connect_value_changed[sender] => move |s| {
                            sender.input(GalleryInput::SetThumbWidth(s.value()));
                        },
                    },
                },
            },

            // Lightbox overlay, shown while previewing an item.
            add_overlay = &gtk::Box {
                add_css_class: "gallery-lightbox",
                set_orientation: gtk::Orientation::Vertical,
                #[watch]
                set_visible: model.preview.is_some(),

                // Top bar: title + close.
                gtk::CenterBox {
                    add_css_class: "gallery-lightbox-bar",
                    #[wrap(Some)]
                    set_start_widget = &gtk::Label {
                        #[watch]
                        set_label: &model.current().map(|i| i.name.clone()).unwrap_or_default(),
                        set_ellipsize: gtk::pango::EllipsizeMode::Middle,
                        set_halign: gtk::Align::Start,
                        add_css_class: "gallery-lightbox-title",
                    },
                    #[wrap(Some)]
                    set_end_widget = &gtk::Button {
                        set_icon_name: "co.hyprlab.Vireo-window-close-symbolic",
                        set_tooltip_text: Some(i18n("Close").as_str()),
                        add_css_class: "circular",
                        add_css_class: "flat",
                        connect_clicked => GalleryInput::ClosePreview,
                    },
                },

                // Middle: prev  |  image/icon  |  next.
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_vexpand: true,
                    set_spacing: 8,

                    gtk::Button {
                        set_icon_name: "co.hyprlab.Vireo-go-previous-symbolic",
                        set_tooltip_text: Some(i18n("Previous").as_str()),
                        set_valign: gtk::Align::Center,
                        add_css_class: "circular",
                        add_css_class: "osd",
                        #[watch]
                        set_sensitive: model.all_items.len() > 1,
                        connect_clicked => GalleryInput::Prev,
                    },

                    #[name = "preview_stack"]
                    gtk::Stack {
                        set_hexpand: true,
                        set_vexpand: true,
                        #[watch]
                        set_visible_child_name: if model.preview_texture.is_some() {
                            "image"
                        } else if model.current().is_some_and(|i| is_pdf_name(&i.name) && i.data.is_some()) {
                            // A PDF whose full-size render is still on its way.
                            "rendering"
                        } else {
                            "file"
                        },

                        #[name = "preview_picture"]
                        add_named[Some("image")] = &gtk::Picture {
                            set_can_shrink: true,
                            set_content_fit: gtk::ContentFit::Contain,
                            #[watch]
                            set_paintable: model.preview_texture.as_ref(),
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

                        add_named[Some("file")] = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_halign: gtk::Align::Center,
                            set_valign: gtk::Align::Center,
                            set_spacing: 12,
                            gtk::Image {
                                #[watch]
                                set_icon_name: model.current().map(|i| icon_for(&i.name)),
                                set_pixel_size: 96,
                                #[watch]
                                set_css_classes: &[
                                    "gallery-file-icon",
                                    model.current().map(|i| icon_color_class(&i.name)).unwrap_or("ftype-generic"),
                                ],
                            },
                            gtk::Label {
                                #[watch]
                                set_label: &model.current().map(|i| i.name.clone()).unwrap_or_default(),
                                set_ellipsize: gtk::pango::EllipsizeMode::Middle,
                                add_css_class: "title-3",
                            },
                        },
                    },

                    gtk::Button {
                        set_icon_name: "co.hyprlab.Vireo-go-next-symbolic",
                        set_tooltip_text: Some(i18n("Next").as_str()),
                        set_valign: gtk::Align::Center,
                        add_css_class: "circular",
                        add_css_class: "osd",
                        #[watch]
                        set_sensitive: model.all_items.len() > 1,
                        connect_clicked => GalleryInput::Next,
                    },
                },

                // Bottom bar: caption + actions.
                gtk::CenterBox {
                    add_css_class: "gallery-lightbox-bar",
                    #[wrap(Some)]
                    set_start_widget = &gtk::Label {
                        #[watch]
                        set_label: &model.current().map(caption).unwrap_or_default(),
                        set_halign: gtk::Align::Start,
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        add_css_class: "dim-label",
                    },
                    #[wrap(Some)]
                    set_end_widget = &gtk::Box {
                        set_spacing: 8,
                        gtk::Button {
                            set_label: &i18n("Open"),
                            set_tooltip_text: Some(i18n("Open in the default app").as_str()),
                            #[watch]
                            set_sensitive: model.current().is_some_and(|i| i.data.is_some()),
                            connect_clicked => GalleryInput::OpenCurrent,
                        },
                        gtk::Button {
                            set_label: &i18n("Go to Message"),
                            add_css_class: "suggested-action",
                            connect_clicked => GalleryInput::GoToCurrent,
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
        let (view_table, thumb_width, sort_index) = crate::config::load_gallery_view();
        let (include_archive, include_other, saved_folders) = crate::config::load_gallery_scope();
        let mut overrides: HashMap<u32, HashMap<String, bool>> = HashMap::new();
        for (id, path, on) in saved_folders {
            overrides.entry(id).or_default().insert(path, on);
        }
        let model = AttachmentsGallery {
            all_items: Vec::new(),
            total: 0,
            loading_more: false,
            scan_remaining: 0,
            fetching: false,
            query: String::new(),
            sort: GallerySort::from_index(sort_index),
            preview: None,
            preview_texture: None,
            loading: false,
            view_table,
            thumb_width,
            type_filter: 0,
            account_filter: None,
            accounts: Vec::new(),
            include_archive,
            include_other,
            overrides,
            included: HashMap::new(),
            resize_timer: None,
            query_timer: None,
            flow: gtk::FlowBox::new(),
            table: gtk::ListBox::new(),
            folder_list: gtk::ListBox::new(),
            account_names: gtk::StringList::new(&[i18n("All accounts").as_str()]),
            root: root.clone().upcast(),
        };
        let flow = &model.flow;
        let table = &model.table;
        let folder_list = &model.folder_list;
        let widgets = view_output!();

        // Double-clicking the preview opens the document in its external app.
        let dbl = gtk::GestureClick::new();
        dbl.set_button(gtk::gdk::BUTTON_PRIMARY);
        let ds = sender.clone();
        dbl.connect_pressed(move |_, n, _, _| {
            if n == 2 {
                ds.input(GalleryInput::OpenCurrent);
            }
        });
        widgets.preview_picture.add_controller(dbl);

        // Arrow keys navigate the lightbox; Escape closes it.
        let key = gtk::EventControllerKey::new();
        let ks = sender.clone();
        key.connect_key_pressed(move |_, keyval, _, _| {
            match keyval {
                gdk::Key::Left => ks.input(GalleryInput::Prev),
                gdk::Key::Right => ks.input(GalleryInput::Next),
                gdk::Key::Escape => ks.input(GalleryInput::ClosePreview),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        root.add_controller(key);

        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::Input,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            GalleryInput::Page { items, total, offset } => {
                if offset == 0 {
                    // The first page of a fresh query replaces everything.
                    self.all_items = items;
                } else if offset as usize == self.all_items.len() {
                    self.all_items.extend(items);
                } else {
                    // A page for a query that has since changed: its offset no
                    // longer lines up with what is loaded, so appending it
                    // would interleave rows from two different orderings. Drop
                    // it and wait for the page the current query asked for.
                    self.loading_more = false;
                    self.update_view(widgets, sender);
                    return;
                }
                self.total = total;
                self.loading = false;
                self.loading_more = false;
                self.rebuild_view(&sender);
            }
            GalleryInput::LoadMore => self.load_more(&sender),
            GalleryInput::ScanProgress => {
                if self.all_items.len() <= PAGE_SIZE as usize {
                    self.reload(&sender);
                }
            }
            GalleryInput::ScanStatus(remaining) => self.scan_remaining = remaining,
            GalleryInput::SetFetching(on) => self.fetching = on,
            GalleryInput::Fetched { account_id, uid, items } => {
                let mut touched = false;
                for row in self
                    .all_items
                    .iter_mut()
                    .filter(|i| i.account_id == account_id && i.uid == uid && i.data.is_none())
                {
                    // Match on the name rather than the index: the scan numbers
                    // parts as the server describes them and the fetch numbers
                    // them as the parser finds them, which need not agree.
                    if let Some(found) = items.iter().find(|a| a.name == row.name) {
                        row.data = Some(found.data.clone());
                        row.size = found.data.len() as u64;
                        row.downloaded = true;
                        touched = true;
                    }
                }
                if touched {
                    self.rebuild_view(&sender);
                    if self.preview.is_some() {
                        self.refresh_preview(&sender);
                    }
                }
            }
            GalleryInput::SetQuery(q) => {
                if self.query != q {
                    self.query = q;
                    if let Some(t) = self.query_timer.take() {
                        t.remove();
                    }
                    let s = sender.clone();
                    self.query_timer = Some(glib::timeout_add_local_once(
                        std::time::Duration::from_millis(SEARCH_DEBOUNCE_MS as u64),
                        move || s.input(GalleryInput::ApplyQuery),
                    ));
                }
            }
            GalleryInput::ApplyQuery => {
                self.query_timer = None;
                self.reload(&sender);
            }
            GalleryInput::SetSort(i) => {
                let sort = GallerySort::from_index(i);
                if self.sort != sort {
                    self.sort = sort;
                    crate::config::save_gallery_sort(i);
                    self.reload(&sender);
                }
            }
            GalleryInput::SortColumn(col) => {
                // First click sorts a column its natural way; a second flips it.
                let sort = match (col, self.sort) {
                    (0, GallerySort::Name) => GallerySort::NameDesc,
                    (0, _) => GallerySort::Name,
                    (1, GallerySort::Sender) => GallerySort::SenderDesc,
                    (1, _) => GallerySort::Sender,
                    (2, GallerySort::Newest) => GallerySort::Oldest,
                    (2, _) => GallerySort::Newest,
                    (3, GallerySort::Largest) => GallerySort::Smallest,
                    (3, _) => GallerySort::Largest,
                    (4, GallerySort::Type) => GallerySort::TypeDesc,
                    (4, _) | (_, _) => GallerySort::Type,
                };
                // The dropdown follows; its notify handler sees the same value
                // and does nothing further.
                widgets.sort_dropdown.set_selected(sort.index());
                if self.sort != sort {
                    self.sort = sort;
                    crate::config::save_gallery_sort(sort.index());
                    self.reload(&sender);
                }
            }
            GalleryInput::SetViewTable(table) => {
                if self.view_table != table {
                    self.view_table = table;
                    self.rebuild_view(&sender);
                    crate::config::save_gallery_table_view(table);
                }
            }
            GalleryInput::SetThumbWidth(v) => {
                let width = (v.round() as i32).clamp(140, 380);
                if width != self.thumb_width {
                    self.thumb_width = width;
                    // One rebuild once the drag settles, not one per pixel.
                    if let Some(id) = self.resize_timer.take() {
                        id.remove();
                    }
                    let s = sender.clone();
                    self.resize_timer = Some(glib::timeout_add_local_once(
                        std::time::Duration::from_millis(150),
                        move || s.input(GalleryInput::ApplyThumbWidth),
                    ));
                }
            }
            GalleryInput::ApplyThumbWidth => {
                self.resize_timer = None;
                if !self.view_table {
                    self.rebuild_view(&sender);
                }
                crate::config::save_gallery_thumb_width(self.thumb_width);
            }
            GalleryInput::SetTypeFilter(bucket) => {
                if self.type_filter != bucket {
                    self.type_filter = bucket;
                    self.reload(&sender);
                }
            }
            GalleryInput::SetAccounts(accounts) => {
                self.accounts = accounts;
                self.rebuild_account_names();
                // The dropdown's rows just changed under it; start from "All
                // accounts" rather than whichever row the old list had there.
                self.account_filter = None;
                widgets.account_dropdown.set_selected(0);
                self.recompute_scope();
                self.rebuild_folder_list(&sender);
                self.reload(&sender);
            }
            GalleryInput::SetAccountFilter(row) => {
                // Row 0 is "All accounts"; the rest index `accounts` in order.
                let filter = (row as usize)
                    .checked_sub(1)
                    .and_then(|i| self.accounts.get(i))
                    .map(|a| a.id);
                if self.account_filter != filter {
                    self.account_filter = filter;
                    // Keeps the dropdown honest when the message came from
                    // somewhere other than the dropdown itself. Setting it to
                    // the row it already holds emits nothing, so this cannot
                    // loop back round.
                    widgets.account_dropdown.set_selected(row);
                    self.reload(&sender);
                }
            }
            GalleryInput::SetIncludeArchive(on) => {
                if self.include_archive != on {
                    self.include_archive = on;
                    self.scope_changed(&sender);
                }
            }
            GalleryInput::SetIncludeOther(on) => {
                if self.include_other != on {
                    self.include_other = on;
                    self.scope_changed(&sender);
                }
            }
            GalleryInput::SetFolder { account_id, path, on } => {
                let entry = self.overrides.entry(account_id).or_default();
                if entry.insert(path, on) != Some(on) {
                    // The list already draws this tick; rebuilding it here
                    // would tear down the check button mid-signal.
                    self.recompute_scope();
                    self.save_scope();
                    self.reload(&sender);
                }
            }
            GalleryInput::SetLoading(on) => self.loading = on,
            GalleryInput::Activate(i) => {
                if (i as usize) < self.all_items.len() {
                    self.preview = Some(i as usize);
                    self.refresh_preview(&sender);
                }
            }
            GalleryInput::Prev => self.step(-1, &sender),
            GalleryInput::Next => self.step(1, &sender),
            GalleryInput::ClosePreview => {
                self.preview = None;
                self.preview_texture = None;
            }
            GalleryInput::PreviewRendered(key) => {
                // Only meaningful if the rendered PDF is still on show.
                let still_current = self
                    .current()
                    .and_then(|i| i.data.as_ref())
                    .is_some_and(|d| thumb_cache_key(d) == key);
                if still_current {
                    self.refresh_preview(&sender);
                }
            }
            GalleryInput::OpenCurrent => {
                if let Some(i) = self.preview {
                    self.open_item(i, &sender);
                }
            }
            GalleryInput::GoToCurrent => {
                if let Some(i) = self.preview {
                    self.goto_item(i, &sender);
                }
            }
            GalleryInput::OpenItem(i) => self.open_item(i, &sender),
            GalleryInput::DownloadItem(i) => self.download_item(i, &sender),
            GalleryInput::GoToItem(i) => self.goto_item(i, &sender),
            GalleryInput::OpenExternal(i) => {
                // Double-click: skip/close the preview and open the file directly.
                self.preview = None;
                self.open_item(i, &sender);
            }
            GalleryInput::ContextMenu { index, x, y } => {
                self.show_context_menu(index, x, y, &sender)
            }
            GalleryInput::ShowcaseSearch(text) => {
                let entry = widgets.search_entry.clone();
                entry.grab_focus();
                // Goes through `search-changed`, so this is the same path a
                // typed character takes, debounce and all.
                entry.set_text(&text);
                glib::timeout_add_seconds_local_once(2, move || {
                    // `has_focus` also needs the window to be the active one,
                    // which it need not be under a private bus; `is_focus` is
                    // the question actually being asked — is this still the
                    // toplevel's focus widget.
                    let focus = entry
                        .root()
                        .and_downcast::<gtk::Window>()
                        .and_then(|w| gtk::prelude::GtkWindowExt::focus(&w));
                    // A GtkSearchEntry is composite: the focus widget is the
                    // GtkText inside it, so ask whether the focus is anywhere
                    // within the entry rather than whether it *is* the entry.
                    let kept = focus
                        .as_ref()
                        .is_some_and(|f| f == entry.upcast_ref::<gtk::Widget>() || f.is_ancestor(&entry));
                    tracing::info!(
                        target: "vireo::showcase",
                        "gallery search focus: kept={kept} holder={} toolbar_visible={} text={:?}",
                        focus.map(|w| w.type_().name().to_string()).unwrap_or_else(|| "none".into()),
                        entry.parent().is_some_and(|p| p.is_visible()),
                        entry.text()
                    );
                });
            }
            GalleryInput::ShowcaseFolders => {
                widgets.folders_button.popup();
                // A popover is its own surface, so the window snapshot never
                // has it: capture its contents directly, as the context menu
                // does (see `ui::context_menu`).
                if let Ok(path) = std::env::var("VIREO_SHOWCASE") {
                    let content = widgets.folders_box.clone();
                    glib::timeout_add_seconds_local_once(1, move || {
                        crate::app::showcase_capture(content.upcast_ref(), &path);
                    });
                }
            }
        }
        self.update_view(widgets, sender);
    }
}

impl AttachmentsGallery {
    /// What the gallery holds in RAM, for the memory section of an export:
    /// items listed, of which carrying their file bytes, and those bytes.
    pub fn memory_stats(&self) -> (usize, usize, u64) {
        let with_data = self.all_items.iter().filter(|i| i.data.is_some()).count();
        let bytes = self.all_items.iter().filter_map(|i| i.data.as_ref()).map(|d| d.len() as u64).sum();
        (self.all_items.len(), with_data, bytes)
    }

    fn page(&self) -> &'static str {
        if self.loading && self.all_items.is_empty() {
            "loading"
        } else if !self.all_items.is_empty() {
            if self.view_table {
                "table"
            } else {
                "grid"
            }
        } else if self.is_narrowed() {
            // Nothing matched, but something would without the search or the
            // filters — a different message from an empty gallery.
            "noresults"
        } else {
            "empty"
        }
    }

    /// Whether the search bar and the footer are on show. They stay up while a
    /// search narrows the gallery to nothing: the entry being typed into lives
    /// in the toolbar, and a toolbar that hid itself the moment a query matched
    /// nothing would take the focus with it mid-word.
    fn show_chrome(&self) -> bool {
        !self.all_items.is_empty() || self.is_narrowed() || self.scan_remaining > 0
    }

    /// Whether the user has narrowed the view at all. Used only to choose
    /// between the two empty states, so an archive with no attachments in it
    /// doesn't tell the user to try a different search they never made.
    fn is_narrowed(&self) -> bool {
        !self.query.trim().is_empty() || self.type_filter != 0 || self.account_filter.is_some()
    }

    /// Ask for the first page again, dropping whatever is loaded: what every
    /// change to the scope, the search, the type filter or the sort has to do,
    /// because all four are applied by the database and not here.
    /// Ask for the first page again: what every change to the scope, the
    /// search, the type filter or the sort has to do, because all four are
    /// applied by the database and not here.
    ///
    /// What is already on screen deliberately stays there until the
    /// replacement page arrives. Emptying the list first would take the
    /// toolbar and footer down with it — and the search entry lives in the
    /// toolbar, so a search would lose the focus of whoever was typing it
    /// after the first letter.
    fn reload(&mut self, sender: &ComponentSender<Self>) {
        self.preview = None;
        self.preview_texture = None;
        self.loading = true;
        self.request_page(0, sender);
    }

    /// Ask for the next page, unless one is already on its way or the last page
    /// has already landed.
    fn load_more(&mut self, sender: &ComponentSender<Self>) {
        if self.loading_more || !self.has_more() {
            return;
        }
        let offset = self.all_items.len() as u32;
        self.request_page(offset, sender);
    }

    fn has_more(&self) -> bool {
        (self.all_items.len() as u32) < self.total
    }

    /// The query as it stands, for the app to run against the cache.
    fn request_page(&mut self, offset: u32, sender: &ComponentSender<Self>) {
        self.loading_more = true;
        let folders: Vec<(u32, String)> = self
            .accounts
            .iter()
            .flat_map(|a| {
                a.folders
                    .iter()
                    .filter(|f| self.folder_included(a.id, &f.path))
                    .map(|f| (a.id, f.path.clone()))
                    .collect::<Vec<_>>()
            })
            .collect();
        let _ = sender.output(GalleryOutput::Load(GalleryRequest {
            folders,
            account_id: self.account_filter,
            tokens: self
                .query
                .split_whitespace()
                .map(|t| t.to_lowercase())
                .collect(),
            bucket: self.type_filter,
            sort: self.sort,
            offset,
            limit: PAGE_SIZE,
        }));
    }

    /// Whether attachments from this folder are in scope. A folder the app has
    /// not listed (a new one on the server, or the list not having arrived yet)
    /// counts as in scope, matching the worker's own default.
    fn folder_included(&self, account_id: u32, path: &str) -> bool {
        self.included
            .get(&account_id)
            .and_then(|m| m.get(path))
            .copied()
            .unwrap_or(true)
    }

    /// Recompute the effective per-folder verdict from the two master switches
    /// and the user's own ticks. A master switch that is off wins over a tick,
    /// so turning "Include Archive" back on restores each archive folder to
    /// whatever the user had chosen for it.
    fn recompute_scope(&mut self) {
        self.included = self
            .accounts
            .iter()
            .map(|acct| {
                let ticks = self.overrides.get(&acct.id);
                let folders = acct
                    .folders
                    .iter()
                    .map(|f| {
                        let tick = ticks.and_then(|m| m.get(&f.path)).copied();
                        let on = in_scope(f.kind, self.include_archive, self.include_other, tick);
                        (f.path.clone(), on)
                    })
                    .collect();
                (acct.id, folders)
            })
            .collect();
    }

    /// The user's own tick for a folder, before the master switches have their
    /// say — what the folder list shows in its check box.
    fn folder_ticked(&self, account_id: u32, path: &str, kind: FolderKind) -> bool {
        self.overrides
            .get(&account_id)
            .and_then(|m| m.get(path))
            .copied()
            .unwrap_or_else(|| kind_default(kind))
    }

    /// Rebuild the footer popover's folder list: every account's folders, each
    /// with a check box, under the account's own label when there is more than
    /// one account. Rows whose master switch is off are shown insensitive, so
    /// it is clear the switch and not the tick is what is holding them back.
    fn rebuild_folder_list(&self, sender: &ComponentSender<Self>) {
        while let Some(row) = self.folder_list.first_child() {
            self.folder_list.remove(&row);
        }
        let multi = self.accounts.len() > 1;
        for acct in &self.accounts {
            if multi {
                let header = gtk::ListBoxRow::new();
                header.set_activatable(false);
                header.set_selectable(false);
                header.add_css_class("gallery-folder-heading");
                let label = gtk::Label::new(Some(&acct.label));
                label.set_xalign(0.0);
                label.add_css_class("heading");
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                header.set_child(Some(&label));
                self.folder_list.append(&header);
            }
            for folder in &acct.folders {
                let row = adw::ActionRow::new();
                row.set_title(&glib::markup_escape_text(&folder.name));
                row.add_prefix(&gtk::Image::from_icon_name(folder.kind.icon()));
                let check = gtk::CheckButton::new();
                check.set_active(self.folder_ticked(acct.id, &folder.path, folder.kind));
                check.set_valign(gtk::Align::Center);
                let s = sender.clone();
                let account_id = acct.id;
                let path = folder.path.clone();
                check.connect_toggled(move |c| {
                    s.input(GalleryInput::SetFolder {
                        account_id,
                        path: path.clone(),
                        on: c.is_active(),
                    });
                });
                row.add_suffix(&check);
                row.set_activatable_widget(Some(&check));
                // An inbox always counts; the rest answer to a master switch.
                row.set_sensitive(match kind_master(folder.kind) {
                    Some(Master::Archive) => self.include_archive,
                    Some(Master::Other) => self.include_other,
                    None => true,
                });
                self.folder_list.append(&row);
            }
        }
    }

    /// Refill the account dropdown's rows from `accounts`, keeping "All
    /// accounts" first.
    fn rebuild_account_names(&self) {
        while self.account_names.n_items() > 1 {
            self.account_names.remove(self.account_names.n_items() - 1);
        }
        for acct in &self.accounts {
            self.account_names.append(&acct.label);
        }
    }

    /// Persist the folder scope, dropping ticks that agree with their kind's
    /// default so the file only records genuine choices.
    fn save_scope(&self) {
        let kinds: HashMap<(u32, &str), FolderKind> = self
            .accounts
            .iter()
            .flat_map(|a| a.folders.iter().map(move |f| ((a.id, f.path.as_str()), f.kind)))
            .collect();
        let mut folders: Vec<(u32, String, bool)> = self
            .overrides
            .iter()
            .flat_map(|(id, m)| m.iter().map(move |(path, on)| (*id, path.clone(), *on)))
            .filter(|(id, path, on)| {
                // Keep a tick for a folder we have not been told about: it may
                // belong to an account that is offline right now.
                kinds
                    .get(&(*id, path.as_str()))
                    .is_none_or(|k| kind_default(*k) != *on)
            })
            .collect();
        folders.sort();
        crate::config::save_gallery_scope(self.include_archive, self.include_other, &folders);
    }

    /// Re-apply the scope to the loaded items and repaint both the grid/table
    /// and the folder list's tick marks.
    fn scope_changed(&mut self, sender: &ComponentSender<Self>) {
        self.recompute_scope();
        self.save_scope();
        self.rebuild_folder_list(sender);
        self.reload(sender);
    }

    /// The `GalleryItem` at display position `display`. The database returns
    /// rows already filtered and in order, so position is a direct index.
    fn item_at(&self, display: usize) -> Option<&GalleryItem> {
        self.all_items.get(display)
    }

    fn current(&self) -> Option<&GalleryItem> {
        self.preview.and_then(|i| self.item_at(i))
    }

    fn step(&mut self, delta: i32, sender: &ComponentSender<Self>) {
        if self.all_items.is_empty() {
            return;
        }
        if let Some(i) = self.preview {
            let n = self.all_items.len() as i32;
            self.preview = Some((((i as i32 + delta) % n + n) % n) as usize);
            self.refresh_preview(sender);
        }
    }

    /// Work out what the lightbox shows for the current item: an image decodes
    /// on the spot; a PDF's first page comes from the full-size render cache,
    /// or a worker renders it now and [`GalleryInput::PreviewRendered`] circles
    /// back here. Anything else has no texture — the file-icon page shows.
    fn refresh_preview(&mut self, sender: &ComponentSender<Self>) {
        self.preview_texture = None;
        let Some(item) = self.current() else { return };
        let Some(data) = item.data.as_ref() else {
            // Never downloaded (or too big to have ridden along with the page):
            // ask for it, and the spinner shows until `Fetched` circles back.
            if !self.fetching {
                self.fetch_item(item, sender);
            }
            return;
        };
        if item.is_image() {
            self.preview_texture = texture_from(data);
            return;
        }
        if !is_pdf_name(&item.name) {
            return;
        }
        let key = thumb_cache_key(data);
        match PDF_PREVIEWS.with(|c| c.borrow().get(&key).cloned()) {
            Some(texture) => self.preview_texture = texture,
            None => {
                let s = sender.clone();
                lightbox_pdf_texture(data, move |_| {
                    s.input(GalleryInput::PreviewRendered(key));
                });
            }
        }
    }

    /// Repopulate whichever view is showing. The other keeps stale children;
    /// switching to it rebuilds it, so only one view's widgets are ever built
    /// for a given change.
    fn rebuild_view(&mut self, sender: &ComponentSender<Self>) {
        if self.view_table {
            self.rebuild_table(sender);
        } else {
            self.rebuild_grid(sender);
        }
    }

    fn rebuild_grid(&mut self, sender: &ComponentSender<Self>) {
        // Remove existing cells; only FlowBoxChild children (not, say, a popover
        // that happens to be parented nearby).
        let mut child = self.flow.first_child();
        while let Some(c) = child {
            let next = c.next_sibling();
            if c.downcast_ref::<gtk::FlowBoxChild>().is_some() {
                self.flow.remove(&c);
            }
            child = next;
        }
        for display in 0..self.all_items.len() {
            if let Some(item) = self.item_at(display) {
                self.flow
                    .append(&build_cell(display, item, self.thumb_width, sender));
            }
        }
    }

    fn rebuild_table(&mut self, sender: &ComponentSender<Self>) {
        while let Some(row) = self.table.first_child() {
            self.table.remove(&row);
        }
        for display in 0..self.all_items.len() {
            if let Some(item) = self.item_at(display) {
                self.table.append(&build_row(display, item, sender));
            }
        }
    }

    /// Open item `index` in its default application. A file whose bytes were
    /// never downloaded — most of an archive — is fetched first, and opens when
    /// [`GalleryInput::Fetched`] brings it back.
    fn open_item(&self, index: usize, sender: &ComponentSender<Self>) {
        let Some(item) = self.item_at(index) else { return };
        match &item.data {
            Some(data) => {
                let parent = self.flow.root().and_downcast::<gtk::Window>();
                open_bytes(&item.name, data, parent.as_ref());
            }
            None => self.fetch_item(item, sender),
        }
    }

    /// Ask the app to download the message this item belongs to.
    fn fetch_item(&self, item: &GalleryItem, sender: &ComponentSender<Self>) {
        let _ = sender.output(GalleryOutput::Fetch {
            account_id: item.account_id,
            folder_path: item.folder_path.clone(),
            uid: item.uid,
        });
    }

    /// Save item `index` to a file the user chooses.
    fn download_item(&self, index: usize, sender: &ComponentSender<Self>) {
        let Some(item) = self.item_at(index) else { return };
        let Some(data) = item.data.clone() else {
            // Not in hand yet: fetch it, and the user can save it once the
            // thumbnail fills in.
            self.fetch_item(item, sender);
            return;
        };
        let dialog = gtk::FileDialog::builder()
            .title(&i18n("Save Attachment"))
            .initial_name(&item.name)
            .modal(true)
            .build();
        let parent = self.flow.root().and_downcast::<gtk::Window>();
        dialog.save(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
            if let Ok(file) = res {
                if let Some(path) = file.path() {
                    if let Err(e) = std::fs::write(&path, &data) {
                        tracing::warn!("could not save attachment: {e}");
                    }
                }
            }
        });
    }

    fn goto_item(&mut self, index: usize, sender: &ComponentSender<Self>) {
        if let Some(item) = self.item_at(index) {
            let _ = sender.output(GalleryOutput::OpenMessage {
                account_id: item.account_id,
                folder_path: item.folder_path.clone(),
                uid: item.uid,
            });
            self.preview = None;
        }
    }

    /// Pop up the right-click menu (Download / Open / Go to Message) at the click
    /// point `(x, y)` (relative to cell `index`). Download/Open are only enabled
    /// when the file's bytes are cached.
    fn show_context_menu(&self, index: usize, x: f64, y: f64, sender: &ComponentSender<Self>) {
        if self.item_at(index).is_none() {
            return;
        }

        // Both act on a file that was never downloaded too: they fetch it
        // first. Greying them out would be wrong now that most of an archive's
        // attachments are known but not held.
        let s = sender.clone();
        let open = MenuEntry::new(i18n("Open"), move || s.input(GalleryInput::OpenItem(index)))
            .icon("co.hyprlab.Vireo-document-open-symbolic");
        let s = sender.clone();
        let download = MenuEntry::new(i18n("Download…"), move || s.input(GalleryInput::DownloadItem(index)))
            .icon("co.hyprlab.Vireo-folder-download-symbolic");
        let s = sender.clone();
        let goto = MenuEntry::new(i18n("Go to Message"), move || s.input(GalleryInput::GoToItem(index)))
            .icon("co.hyprlab.Vireo-mail-unread-symbolic");
        let sections = vec![vec![open, download, goto]];

        // Anchor on the clicked cell/row itself so the click point (already
        // relative to it) needs no coordinate translation.
        let source: Option<gtk::Widget> = if self.view_table {
            self.table.row_at_index(index as i32).map(|r| r.upcast())
        } else {
            self.flow.child_at_index(index as i32).map(|c| c.upcast())
        };
        show_context_menu(source.as_ref().unwrap_or(&self.root), x, y, sections);
    }
}

/// One grid cell: a thumbnail (image or PDF first page) or type icon, plus name
/// + size.
fn build_cell(
    index: usize,
    item: &GalleryItem,
    width: i32,
    sender: &ComponentSender<AttachmentsGallery>,
) -> gtk::Widget {
    let cell = gtk::Box::new(gtk::Orientation::Vertical, 6);
    cell.add_css_class("gallery-cell");
    cell.set_hexpand(true);
    cell.set_halign(gtk::Align::Fill);
    cell.set_tooltip_text(Some(&format!("{} — {}", item.name, item.human_size())));

    let thumb_holder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    thumb_holder.add_css_class("gallery-thumb");
    thumb_holder.set_halign(gtk::Align::Fill);
    thumb_holder.set_valign(gtk::Align::Fill);

    let thumb = match item.data.as_ref() {
        Some(d) => thumbnail_texture(&item.name, d),
        None => Thumbnail::Fallback,
    };
    match thumb {
        Thumbnail::Ready(tex) => thumb_holder.append(&gallery_picture(&tex)),
        Thumbnail::Fallback => thumb_holder.append(&gallery_icon(&item.name)),
        Thumbnail::Pending => {
            thumb_holder.append(&thumbnail_spinner());
            let holder = thumb_holder.downgrade();
            let name = item.name.clone();
            let data = item.data.clone().unwrap_or_default();
            spawn_thumbnail_render(&item.name, data, move |tex| {
                // The cell may be gone by now (search narrowed, list rebuilt);
                // the render still landed in the cache for the next build.
                let Some(holder) = holder.upgrade() else { return };
                while let Some(child) = holder.first_child() {
                    holder.remove(&child);
                }
                match tex {
                    Some(tex) => holder.append(&gallery_picture(&tex)),
                    None => holder.append(&gallery_icon(&name)),
                }
            });
        }
    }

    // Lock the thumbnail section to a 4:3 aspect ratio; its width tracks the
    // (responsive) column width and the height follows, filling the cell.
    let aspect = RatioBox::new(&thumb_holder);
    // Preferred column width (the footer slider's value) — the FlowBox packs at
    // least 3 per row and adds more as the window widens.
    aspect.set_width_request(width);
    aspect.set_hexpand(true);

    // Overlay quick-action buttons at the thumbnail's bottom-right corner; they
    // fade in on hover (via CSS). Download and Open need the file's bytes cached;
    // "Go to Message" always works, so it shows even for uncached attachments.
    let thumb_overlay = gtk::Overlay::new();
    thumb_overlay.set_child(Some(&aspect));

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    actions.set_valign(gtk::Align::End);
    actions.set_margin_end(6);
    actions.set_margin_bottom(6);
    let action_btn = |icon: &str, tip: &str| {
        let b = gtk::Button::from_icon_name(icon);
        b.add_css_class("gallery-open");
        b.add_css_class("circular");
        b.add_css_class("osd");
        b.set_tooltip_text(Some(tip));
        b
    };
    if item.data.is_some() {
        let download = action_btn("co.hyprlab.Vireo-folder-download-symbolic", "Download");
        let s = sender.clone();
        download.connect_clicked(move |_| s.input(GalleryInput::DownloadItem(index)));
        actions.append(&download);

        let open = action_btn("co.hyprlab.Vireo-document-open-symbolic", "Open");
        let s = sender.clone();
        open.connect_clicked(move |_| s.input(GalleryInput::OpenItem(index)));
        actions.append(&open);
    }
    let goto = action_btn("co.hyprlab.Vireo-mail-unread-symbolic", &i18n("Go to Message"));
    let s = sender.clone();
    goto.connect_clicked(move |_| s.input(GalleryInput::GoToItem(index)));
    actions.append(&goto);
    thumb_overlay.add_overlay(&actions);

    cell.append(&thumb_overlay);

    let name = gtk::Label::new(Some(&item.name));
    name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    name.set_max_width_chars(18);
    name.add_css_class("gallery-name");
    cell.append(&name);

    let from = item.from_name.trim();
    if !from.is_empty() {
        let sender = gtk::Label::new(Some(from));
        sender.set_ellipsize(gtk::pango::EllipsizeMode::End);
        sender.set_max_width_chars(18);
        sender.add_css_class("gallery-from");
        cell.append(&sender);
    }

    let subject = item.subject.trim();
    if !subject.is_empty() {
        let subj = gtk::Label::new(Some(subject));
        subj.set_ellipsize(gtk::pango::EllipsizeMode::End);
        subj.set_max_width_chars(18);
        subj.add_css_class("gallery-subject");
        subj.add_css_class("dim-label");
        cell.append(&subj);
    }

    let mut meta = vec![folder_label(&item.folder_path)];
    let date = item.date_label();
    if !date.is_empty() {
        meta.push(date);
    }
    meta.push(item.human_size());
    let sub = gtk::Label::new(Some(&meta.join(" · ")));
    sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
    sub.set_max_width_chars(18);
    sub.add_css_class("gallery-size");
    sub.add_css_class("dim-label");
    cell.append(&sub);

    let child = gtk::FlowBoxChild::new();
    child.set_child(Some(&cell));

    // Right-click → context menu at the click point.
    let right = gtk::GestureClick::new();
    right.set_button(gtk::gdk::BUTTON_SECONDARY);
    let s = sender.clone();
    right.connect_pressed(move |_, _, x, y| {
        s.input(GalleryInput::ContextMenu { index, x, y });
    });
    child.add_controller(right);

    // Double-click (primary) → open externally. Single click keeps the FlowBox's
    // built-in activation (which opens the preview).
    let dbl = gtk::GestureClick::new();
    dbl.set_button(gtk::gdk::BUTTON_PRIMARY);
    let s = sender.clone();
    dbl.connect_pressed(move |_, n, _, _| {
        if n == 2 {
            s.input(GalleryInput::OpenExternal(index));
        }
    });
    child.add_controller(dbl);

    child.upcast()
}

fn caption(item: &GalleryItem) -> String {
    let who = if item.from_name.trim().is_empty() { "Unknown" } else { item.from_name.trim() };
    let subject = item.subject.trim();
    let mut parts = vec![who.to_string()];
    if !subject.is_empty() {
        parts.push(subject.to_string());
    }
    parts.push(folder_label(&item.folder_path));
    let date = item.date_label();
    if !date.is_empty() {
        parts.push(date);
    }
    parts.push(item.human_size());
    parts.join(" · ")
}

/// A friendly folder name from a mailbox path (the last path segment).
fn folder_label(path: &str) -> String {
    let name = path.rsplit(['/', '.']).next().unwrap_or(path);
    if name.eq_ignore_ascii_case("inbox") {
        "Inbox".to_string()
    } else {
        // Paths are stored as the server names them, in modified UTF-7.
        crate::mutf7::decode(name)
    }
}


/// One table row: mini thumbnail/type icon, name, sender, type, date, size —
/// the same widths as the header buttons, so the columns line up.
fn build_row(
    index: usize,
    item: &GalleryItem,
    sender: &ComponentSender<AttachmentsGallery>,
) -> gtk::ListBoxRow {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    line.add_css_class("gallery-table-row");

    // Cache-only mini thumbnail: a decode already paid for is shown, but a
    // 24px row never spawns a render of its own.
    let lead: gtk::Widget = match item.data.as_ref().map(|d| thumbnail_texture(&item.name, d)) {
        Some(Thumbnail::Ready(tex)) => {
            let pic = gtk::Picture::for_paintable(&tex);
            pic.set_content_fit(gtk::ContentFit::Cover);
            pic.set_size_request(28, 28);
            pic.add_css_class("gallery-table-thumb");
            pic.upcast()
        }
        _ => {
            let img = gtk::Image::from_icon_name(icon_for(&item.name));
            img.set_pixel_size(20);
            img.set_size_request(28, 28);
            img.add_css_class("gallery-file-icon");
            img.add_css_class(icon_color_class(&item.name));
            img.upcast()
        }
    };
    line.append(&lead);

    let name = gtk::Label::new(Some(&item.name));
    name.set_xalign(0.0);
    name.set_hexpand(true);
    name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    name.set_tooltip_text(Some(&format!("{} — {}", item.subject, item.human_size())));
    line.append(&name);

    let sender_label = gtk::Label::new(Some(&item.from_name));
    sender_label.set_xalign(0.0);
    sender_label.set_width_request(170);
    sender_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    sender_label.set_max_width_chars(1);
    line.append(&sender_label);

    // Every fixed-width cell is clipped to its column (the width request is
    // the floor, the one-char max the ceiling): a value wider than its
    // column would otherwise widen the cell, and with the row's width
    // fixed, the expanding Name cell would give up the difference — sliding
    // Sender left of its header on that row alone.
    let fixed = |text: &str, width: i32, xalign: f32| {
        let l = gtk::Label::new(Some(text));
        l.set_xalign(xalign);
        l.set_width_request(width);
        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
        l.set_max_width_chars(1);
        l.add_css_class("dim-label");
        l
    };
    // No extension: say so, rather than showing the name again.
    let ext = ext_of(&item.name);
    let kind_text = if ext.is_empty() { "File".to_string() } else { ext.to_ascii_uppercase() };
    let kind = fixed(&kind_text, 90, 0.0);
    line.append(&kind);

    let date = fixed(&date_text(item.timestamp), 110, 0.0);
    line.append(&date);

    let size = fixed(&item.human_size(), 90, 1.0);
    size.add_css_class("numeric");
    line.append(&size);

    // The same quick actions the grid cells carry, as a trailing column.
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    actions.set_width_request(TABLE_ACTIONS_WIDTH);
    actions.set_halign(gtk::Align::End);
    let act = |icon: &str, tip: &str| {
        let b = gtk::Button::from_icon_name(icon);
        b.add_css_class("flat");
        b.set_valign(gtk::Align::Center);
        b.set_tooltip_text(Some(tip));
        b
    };
    if item.data.is_some() {
        let download = act("co.hyprlab.Vireo-folder-download-symbolic", "Download");
        let s = sender.clone();
        download.connect_clicked(move |_| s.input(GalleryInput::DownloadItem(index)));
        actions.append(&download);
        let open = act("co.hyprlab.Vireo-document-open-symbolic", "Open");
        let s = sender.clone();
        open.connect_clicked(move |_| s.input(GalleryInput::OpenItem(index)));
        actions.append(&open);
    }
    let goto = act("co.hyprlab.Vireo-mail-unread-symbolic", &i18n("Go to Message"));
    let s = sender.clone();
    goto.connect_clicked(move |_| s.input(GalleryInput::GoToItem(index)));
    actions.append(&goto);
    line.append(&actions);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&line));

    // The same gestures the grid cells carry: right-click for the context
    // menu, double-click to open externally (single click previews).
    let right = gtk::GestureClick::new();
    right.set_button(gtk::gdk::BUTTON_SECONDARY);
    let s = sender.clone();
    right.connect_pressed(move |_, _, x, y| {
        s.input(GalleryInput::ContextMenu { index, x, y });
    });
    row.add_controller(right);
    let dbl = gtk::GestureClick::new();
    dbl.set_button(gtk::gdk::BUTTON_PRIMARY);
    let s = sender.clone();
    dbl.connect_pressed(move |_, n, _, _| {
        if n == 2 {
            s.input(GalleryInput::OpenExternal(index));
        }
    });
    row.add_controller(dbl);
    row
}

/// A column header's label, carrying the sort arrow when it is the active
/// column: `asc`/`desc` are the two criteria that column maps to.
fn column_header(label: &str, current: GallerySort, asc: GallerySort, desc: GallerySort) -> String {
    if current == asc {
        format!("{label} \u{2191}")
    } else if current == desc {
        format!("{label} \u{2193}")
    } else {
        label.to_string()
    }
}

/// Whether a scroller has come close enough to its end to be worth asking for
/// the next page. `upper - page_size` is the furthest the view can scroll, so
/// this is "within a couple of rows of the bottom".
fn near_end(a: &gtk::Adjustment) -> bool {
    a.upper() - a.page_size() - a.value() < LOAD_MORE_MARGIN
}

/// The footer's network status: a page arriving, or how much of the archive the
/// scan has still to describe. Empty when there is nothing happening.
fn scan_text(loading_more: bool, scan_remaining: u32) -> String {
    if scan_remaining > 0 {
        // Deliberately counts messages, not attachments: until a message has
        // been described there is no telling how many files it holds.
        i18n_f("Indexing {n} messages…", &[("n", &scan_remaining.to_string())])
    } else if loading_more {
        i18n("Loading more…")
    } else {
        String::new()
    }
}

/// The footer's item count: what's shown of what's there.
fn count_text(shown: usize, total: usize) -> String {
    if shown == total {
        format!("{total} attachment{}", if total == 1 { "" } else { "s" })
    } else {
        format!("{shown} of {total}")
    }
}

/// "Aug 26, 2026" for a table row, or nothing when the timestamp is unknown.
fn date_text(timestamp: i64) -> String {
    glib::DateTime::from_unix_local(timestamp)
        .ok()
        .and_then(|d| d.format("%b %e, %Y").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// The footer type dropdown's bucket for a filename. Row 0 is "All types";
/// the rest must match the `StringList` built in the view.
/// The gallery's cover-cropped thumbnail picture for a ready texture.
fn gallery_picture(tex: &gdk::Texture) -> gtk::Picture {
    let pic = gtk::Picture::for_paintable(tex);
    pic.set_content_fit(gtk::ContentFit::Cover);
    pic.set_hexpand(true);
    pic.set_vexpand(true);
    pic.add_css_class("gallery-thumb-image");
    pic
}

/// The gallery's centred type icon for anything without a thumbnail.
fn gallery_icon(name: &str) -> gtk::Image {
    let img = gtk::Image::from_icon_name(icon_for(name));
    img.set_pixel_size(56);
    img.set_hexpand(true);
    img.add_css_class("gallery-file-icon");
    img.add_css_class(icon_color_class(name));
    img
}

/// A `gdk::Texture` from raw image bytes, or `None` if the format isn't loadable.
pub(crate) fn texture_from(data: &[u8]) -> Option<gdk::Texture> {
    gdk::Texture::from_bytes(&glib::Bytes::from(data)).ok()
}

/// What a grid cell can show for an attachment right now.
pub(crate) enum Thumbnail {
    /// A texture is available immediately: a decoded image, or a PDF page
    /// already in the render cache.
    Ready(gdk::Texture),
    /// An image or PDF not decoded/rendered yet. Show a spinner and call
    /// [`spawn_thumbnail_render`] — decoding on the main thread while cells
    /// were being built froze the whole window (the "Force Quit" dialog).
    Pending,
    /// No thumbnail for this type (or it wouldn't decode): type icon.
    Fallback,
}

thread_local! {
    /// Finished thumbnail renders — decoded images and PDF pages alike,
    /// successes and failures, keyed by content hash — so a gallery rebuild or
    /// a revisit this session never decodes the same attachment twice.
    /// Main-thread only; results land here from `spawn_thumbnail_render`.
    static THUMB_CACHE: std::cell::RefCell<std::collections::HashMap<u64, Option<gdk::Texture>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    /// Lightbox-size PDF page renders, cached separately from the thumbnails
    /// (same key, much bigger pixels). Failures cache too.
    static PDF_PREVIEWS: std::cell::RefCell<std::collections::HashMap<u64, Option<gdk::Texture>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// The session's render caches, for the memory section of an export:
/// thumbnails (entries, of which rendered, pixel bytes) then the lightbox
/// PDF pages the same way.
pub(crate) fn cache_stats() -> ((usize, usize, u64), (usize, usize, u64)) {
    fn measure(c: &std::collections::HashMap<u64, Option<gdk::Texture>>) -> (usize, usize, u64) {
        let rendered: Vec<&gdk::Texture> = c.values().flatten().collect();
        let bytes = rendered.iter().map(|t| crate::memory_report::texture_bytes(t)).sum();
        (c.len(), rendered.len(), bytes)
    }
    (
        THUMB_CACHE.with(|c| measure(&c.borrow())),
        PDF_PREVIEWS.with(|c| measure(&c.borrow())),
    )
}

/// Whether a filename names a PDF (by extension).
pub(crate) fn is_pdf_name(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".pdf")
}

/// The content hash the thumbnail/preview caches key on — for callers that
/// need to correlate an async render with the item it belongs to.
pub(crate) fn content_key(data: &[u8]) -> u64 {
    thumb_cache_key(data)
}

/// A full-size PDF page already in the preview cache, if any.
pub(crate) fn cached_pdf_preview(data: &[u8]) -> Option<gdk::Texture> {
    PDF_PREVIEWS
        .with(|c| c.borrow().get(&thumb_cache_key(data)).cloned())
        .flatten()
}

/// A lightbox-size render of a PDF's first page: handed to `on_done` on the
/// main thread — at once from the cache, or after a worker renders it.
/// Failures cache too, so a broken PDF is rendered at most once.
pub(crate) fn lightbox_pdf_texture(
    data: &[u8],
    on_done: impl FnOnce(Option<gdk::Texture>) + 'static,
) {
    let key = thumb_cache_key(data);
    if let Some(cached) = PDF_PREVIEWS.with(|c| c.borrow().get(&key).cloned()) {
        on_done(cached);
        return;
    }
    let data = data.to_vec();
    glib::spawn_future_local(async move {
        let tex =
            gtk::gio::spawn_blocking(move || pdf_page_texture(&data, PREVIEW_RENDER_WIDTH))
                .await
                .ok()
                .flatten();
        PDF_PREVIEWS.with(|c| {
            c.borrow_mut().insert(key, tex.clone());
        });
        on_done(tex);
    });
}

fn thumb_cache_key(data: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    data.len().hash(&mut h);
    data.hash(&mut h);
    h.finish()
}

/// A grid-cell thumbnail for any attachment type that has one — a decoded
/// image or a PDF's first page — answered from the render cache, or `Pending`
/// until a worker produces it.
pub(crate) fn thumbnail_texture(name: &str, data: &[u8]) -> Thumbnail {
    if !is_image_name(name) && !is_pdf_name(name) {
        return Thumbnail::Fallback;
    }
    match THUMB_CACHE.with(|c| c.borrow().get(&thumb_cache_key(data)).cloned()) {
        Some(Some(tex)) => Thumbnail::Ready(tex),
        Some(None) => Thumbnail::Fallback,
        None => Thumbnail::Pending,
    }
}

/// Decode an image, or render a PDF's first page, off the main thread; hand
/// the result to `on_done` back on it and record it in the cache either way.
/// The GTK loop keeps running — cells show their spinner instead of the
/// window freezing.
pub(crate) fn spawn_thumbnail_render(
    name: &str,
    data: Vec<u8>,
    on_done: impl FnOnce(Option<gdk::Texture>) + 'static,
) {
    let key = thumb_cache_key(&data);
    let image = is_image_name(name);
    glib::spawn_future_local(async move {
        let tex = gtk::gio::spawn_blocking(move || {
            if image {
                texture_from(&data)
            } else {
                pdf_page_texture(&data, THUMB_RENDER_WIDTH)
            }
        })
        .await
        .ok()
        .flatten();
        THUMB_CACHE.with(|c| {
            c.borrow_mut().insert(key, tex.clone());
        });
        on_done(tex);
    });
}

/// The centred spinner a cell shows while its PDF page renders.
pub(crate) fn thumbnail_spinner() -> gtk::Spinner {
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    spinner.set_width_request(28);
    spinner.set_height_request(28);
    spinner.set_halign(gtk::Align::Center);
    spinner.set_valign(gtk::Align::Center);
    spinner.set_hexpand(true);
    spinner.set_vexpand(true);
    spinner
}

/// Width thumbnails render at — soft would do, but sharp is cheap at 360px.
const THUMB_RENDER_WIDTH: f64 = 360.0;
/// Width the lightbox renders a PDF page at: sharp on a large window without
/// tripping the decoder's pixel ceiling (A4 portrait at 1600 ≈ 3.6M pixels).
const PREVIEW_RENDER_WIDTH: f64 = 1600.0;

/// Render a PDF's first page to a texture at `target_width`, by way of an
/// in-memory PNG — the same route every other thumbnail here already goes
/// through, so cropping, caching, and format all stay uniform.
fn pdf_page_texture(data: &[u8], target_width: f64) -> Option<gdk::Texture> {
    // One PDF render at a time, process-wide. Poppler's colour management
    // (lcms2) shares state across documents: two thumbnail threads rendering
    // concurrently crashed with heap corruption — one thread tearing down its
    // Gfx (cmsCloseProfile) while the other still rendered. This is the sole
    // poppler entry point, so serialising here covers every caller.
    static PDF_RENDER: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = PDF_RENDER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let doc = poppler::Document::from_bytes(&glib::Bytes::from(data), None).ok()?;
    let page = doc.page(0)?;
    let (w, h) = page.size();
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    // Poppler's default is one pixel per point (72 dpi) — fine for text but
    // soft on screen, so render at the caller's width instead.
    let scale = target_width / w;
    let surface = gtk::cairo::ImageSurface::create(
        gtk::cairo::Format::ARgb32,
        target_width.round() as i32,
        (h * scale).round() as i32,
    )
    .ok()?;
    let cr = gtk::cairo::Context::new(&surface).ok()?;
    // A PDF page's own background is transparent; without painting white first,
    // the thumbnail would show through to whatever is behind it (a hole in
    // dark mode rather than a page).
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.paint().ok()?;
    cr.scale(scale, scale);
    page.render(&cr);
    drop(cr);
    let mut png = Vec::new();
    surface.write_to_png(&mut png).ok()?;
    texture_from(&png)
}

/// A symbolic icon name for a filename by extension.
pub(crate) fn icon_for(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "pdf" => "co.hyprlab.Vireo-x-office-document-symbolic",
        "doc" | "docx" | "odt" | "rtf" | "txt" | "md" => "co.hyprlab.Vireo-x-office-document-symbolic",
        "xls" | "xlsx" | "ods" | "csv" => "co.hyprlab.Vireo-x-office-spreadsheet-symbolic",
        "ppt" | "pptx" | "odp" => "co.hyprlab.Vireo-x-office-presentation-symbolic",
        "zip" | "gz" | "tar" | "7z" | "rar" | "xz" | "bz2" => "co.hyprlab.Vireo-package-x-generic-symbolic",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => "co.hyprlab.Vireo-audio-x-generic-symbolic",
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" => "co.hyprlab.Vireo-video-x-generic-symbolic",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "heic" | "heif" | "avif" | "ico" => {
            "co.hyprlab.Vireo-image-x-generic-symbolic"
        }
        "ics" => "co.hyprlab.Vireo-x-office-calendar-symbolic",
        _ => "co.hyprlab.Vireo-text-x-generic-symbolic",
    }
}

/// CSS class that tints a type icon by file kind: PDFs red, Word docs blue,
/// spreadsheets green, and so on (see `styles.css`). Symbolic icons pick up the
/// class's `color`, so an unthumbnailed attachment reads at a glance.
pub(crate) fn icon_color_class(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "pdf" => "ftype-pdf",
        "doc" | "docx" | "odt" | "rtf" => "ftype-doc",
        "xls" | "xlsx" | "ods" | "csv" => "ftype-sheet",
        "ppt" | "pptx" | "odp" => "ftype-slides",
        "zip" | "gz" | "tar" | "7z" | "rar" | "xz" | "bz2" => "ftype-archive",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => "ftype-audio",
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" => "ftype-video",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "heic" | "heif" | "avif" | "ico" => {
            "ftype-image"
        }
        "ics" => "ftype-calendar",
        _ => "ftype-generic",
    }
}

/// Write bytes to a temp file and open it in the default application.
///
/// The filename comes from an email, so it comes from whoever sent it. The
/// sanitizer below removes every path separator, which is what stops traversal;
/// the rest of the care here is about `/tmp` being shared on a native install
/// (under Flatpak it is per-app): the directory is created private and its
/// ownership checked, and each file is created fresh rather than written
/// through whatever already sits at a guessable path.
///
/// Launched through the portal's `UriLauncher` rather than
/// `AppInfo::launch_default_for_uri`: for a type with no registered default
/// (common for attachments — nothing may ever have been "set as default" for
/// a `.pdf`) the portal falls back to GNOME's own app-chooser dialog instead
/// of silently doing nothing.
pub(crate) fn open_bytes(name: &str, data: &[u8], parent: Option<&gtk::Window>) {
    let safe: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect();
    let Some(dir) = attachment_dir() else { return };
    let base = if safe.is_empty() { "attachment".to_string() } else { safe };
    let Some((mut file, path)) = create_private(&dir, &base) else {
        tracing::warn!("could not open a temporary file for the attachment");
        return;
    };
    // Write through the handle rather than reopening by path: the checks below
    // are about what is at that name, and going back through it would hand the
    // result to whatever is there by the time we look again.
    if let Err(e) = std::io::Write::write_all(&mut file, data) {
        tracing::warn!("could not write the attachment: {e}");
        return;
    }
    drop(file);
    let uri = format!("file://{}", path.to_string_lossy());
    // Outside Flatpak, GIO launches the default handler directly; the portal
    // route has been seen accepting an OpenURI request and then launching
    // nothing — reported success, no app, click looked dead. Inside the
    // sandbox the portal is the only road out and GIO's launcher is the one
    // that can't work. So each side leads with the road that works for it
    // and keeps the other as the fallback.
    if std::path::Path::new("/.flatpak-info").exists() {
        // Inside the sandbox the file was staged into the app's PRIVATE /tmp —
        // a path the host cannot read. Handing the portal a file:// URI string
        // therefore launches a handler pointed at a file that, host-side, does
        // not exist. FileLauncher instead passes the file as a descriptor
        // through the document portal, which exports it where the handler can
        // read it. When even that fails (a broken portal), say so instead of
        // doing nothing — a host-side AppInfo fallback can't work here.
        // Not GTK's FileLauncher: in this runtime it mis-finishes its own
        // async task in the sandboxed path (task-tag assertion), so its
        // callback never fires and every failure vanishes. Speaking the
        // portal protocol directly restores the contract.
        portal_open_file(path, false, parent.cloned());
    } else if let Err(e) =
        gtk::gio::AppInfo::launch_default_for_uri(&uri, gtk::gio::AppLaunchContext::NONE)
    {
        tracing::warn!("gio launch failed ({e}), trying the portal");
        let owned = parent.cloned();
        gtk::UriLauncher::new(&uri).launch(parent, gtk::gio::Cancellable::NONE, move |res| {
            if let Err(e) = res {
                tracing::warn!("portal launch also failed: {e}");
                launch_failed_dialog(owned.as_ref(), &e.to_string());
            }
        });
    }
}

/// Both launch roads failed — tell the user what happened and what still
/// works, rather than leaving a click that does nothing.
fn launch_failed_dialog(parent: Option<&gtk::Window>, error: &str) {
    let dialog = gtk::AlertDialog::builder()
        .message(&i18n("The file could not be opened"))
        // No Flatseal toggle can help here: portal access is not a
        // permission, and the failure is inside the portal's own launcher.
        // Offer the steps that actually work.
        .detail(format!(
            "The desktop portal reported: {error}\n\n\
             \u{2022} Download the file, then open it from Files\n\
             \u{2022} Updating \u{201c}xdg-desktop-portal\u{201d} and logging back in may fix direct opening"
        ))
        .modal(true)
        .build();
    dialog.show(parent);
}

/// Open a staged file through the OpenURI portal (`portal_request` in the
/// launch module speaks the protocol). Response 0 is success. On the quiet
/// attempt (`ask == false`) any failure retries once with the app chooser —
/// the backend's own dialog, whose launch machinery works even where the
/// direct default-handler launch is broken (seen on Fedora 44), and whose
/// "always open with" sticks in the permission store. A cancelled chooser
/// (1) is not an error; any other chooser failure gets the dialog.
fn portal_open_file(path: std::path::PathBuf, ask: bool, parent: Option<gtk::Window>) {
    use gtk::gio;
    use gtk::glib;
    use gtk::glib::prelude::*;
    use std::os::fd::AsFd;

    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("staged attachment vanished: {e}");
            return;
        }
    };
    let fd_list = gio::UnixFDList::new();
    let handle = match fd_list.append(file.as_fd()) {
        Ok(h) => h,
        Err(e) => {
            launch_failed_dialog(parent.as_ref(), &e.to_string());
            return;
        }
    };
    let retry_path = path.clone();
    let retry_parent = parent.clone();
    crate::ui::launch::portal_request(
        "OpenFile",
        Some(fd_list),
        move |token| {
            let options = glib::VariantDict::new(None);
            options.insert_value("handle_token", &token.to_variant());
            if ask {
                options.insert_value("ask", &true.to_variant());
            }
            // Not the tuple's ToVariant: that boxes the dict as a nested "v"
            // and the portal rejects "(shv)". tuple_from_iter splices each
            // child at its own type, producing the "(sha{sv})" the interface
            // declares.
            glib::Variant::tuple_from_iter([
                "".to_variant(),
                glib::variant::Handle(handle).to_variant(),
                options.end(),
            ])
        },
        move |res| match res {
            Err(e) => {
                tracing::warn!("portal OpenFile call failed: {e}");
                launch_failed_dialog(retry_parent.as_ref(), &e);
            }
            // The user cancelled — on EITHER attempt. When no default
            // handler is registered, the backend shows its chooser even on
            // the quiet attempt, so a cancel can arrive with ask == false
            // too; treating that as a failure re-asked and the dialog popped
            // right back up (issue #65). A cancel is an answer, never a
            // reason to ask again.
            Ok(0) | Ok(1) => {}
            Ok(code) if !ask => {
                tracing::warn!("portal open answered {code}; retrying with the chooser");
                portal_open_file(retry_path.clone(), true, retry_parent.clone());
            }
            Ok(code) => launch_failed_dialog(
                retry_parent.as_ref(),
                &format!("the portal answered response code {code}"),
            ),
        },
    );
}

/// The private directory opened attachments are staged in, created if needed.
///
/// Returns `None` if the path exists but is not a directory we own with nobody
/// else's access — an attacker who pre-creates it on a shared host would
/// otherwise get every attachment the user opens.
fn attachment_dir() -> Option<std::path::PathBuf> {
    // Inside Flatpak /tmp is PRIVATE to the sandbox: the document portal
    // validates an exported fd by re-opening its path in the HOST namespace,
    // so a /tmp-staged file fails validation and every portal open dies
    // silently before any UI. $XDG_RUNTIME_DIR/app/$FLATPAK_ID is
    // bind-mounted from the host at the SAME path — the one temp location
    // both sides of the sandbox agree on (and it's session-scoped tmpfs,
    // like /tmp). Native builds keep /tmp.
    let dir = match std::env::var("FLATPAK_ID") {
        Ok(app_id) => {
            let runtime = std::env::var("XDG_RUNTIME_DIR").ok()?;
            std::path::Path::new(&runtime)
                .join("app")
                .join(app_id)
                .join("vireo-attachments")
        }
        Err(_) => std::env::temp_dir().join("vireo-attachments"),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        if !dir.exists() {
            let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir);
        }
        // `symlink_metadata`, so a symlink pointing at somewhere friendlier is
        // seen for what it is rather than followed.
        let md = std::fs::symlink_metadata(&dir).ok()?;
        if !md.is_dir() || md.uid() != our_uid() {
            tracing::warn!("{} is not ours; not staging attachments", dir.display());
            return None;
        }
        if md.permissions().mode() & 0o077 != 0 {
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::create_dir_all(&dir);
    }
    Some(dir)
}

/// Create a new 0600 file for `base` in `dir`, returning it open with its path.
///
/// `create_new` means an existing file — or a symlink planted at a guessable
/// name — makes the call fail rather than being written through, so the next
/// candidate name is tried instead. Overwriting the user's previous copy of the
/// same attachment silently would also be wrong.
fn create_private(
    dir: &std::path::Path,
    base: &str,
) -> Option<(std::fs::File, std::path::PathBuf)> {
    for n in 0..64 {
        let name = if n == 0 {
            base.to_string()
        } else {
            match base.rsplit_once('.') {
                Some((stem, ext)) if !stem.is_empty() => format!("{stem}-{n}.{ext}"),
                _ => format!("{base}-{n}"),
            }
        };
        let path = dir.join(&name);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
            // Refuse to follow a symlink instead of racing to check for one.
            // 0o400000 is O_NOFOLLOW on every Linux arch Vireo ships for; the
            // wrong constant here once passed O_DIRECTORY (0o200000) instead,
            // which made this open() fail and every attachment click a no-op.
            opts.custom_flags(0o400000 /* O_NOFOLLOW */);
        }
        if let Ok(file) = opts.open(&path) {
            return Some((file, path));
        }
    }
    None
}

/// This process's real user ID.
///
/// Vireo has no libc dependency; `getuid` is always available and cannot fail,
/// so declaring it directly is cheaper than taking one on.
#[cfg(unix)]
fn our_uid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

/// Delete anything left in the attachment staging directory.
///
/// Opened attachments used to accumulate in `/tmp` for the life of the machine —
/// decrypted, readable, and long past the point the user considers them gone.
/// Called once at startup, because the helper application the user opened a file
/// with may still have it open while Vireo is running.
pub fn purge_attachment_dir() {
    let dir = std::env::temp_dir().join("vireo-attachments");
    if !dir.exists() {
        return;
    }
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => tracing::debug!("cleared staged attachments in {}", dir.display()),
        Err(e) => tracing::warn!("could not clear {}: {e}", dir.display()),
    }
}

glib::wrapper! {
    /// A single-child container that forces a fixed 4:3 (width:height) aspect
    /// ratio via true height-for-width sizing, so the thumbnail fills the
    /// (responsive) column width and its height follows — no centred gaps and
    /// no continuous frame-clock ticking.
    pub struct RatioBox(ObjectSubclass<imp::RatioBox>) @extends gtk::Widget;
}

impl RatioBox {
    fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let obj: Self = glib::Object::new();
        child.set_parent(&obj);
        obj
    }
}

mod imp {
    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;

    /// Height as a fraction of width — 3/4 gives a 4:3 landscape thumbnail.
    const HEIGHT_OVER_WIDTH_NUM: i32 = 3;
    const HEIGHT_OVER_WIDTH_DEN: i32 = 4;

    #[derive(Default)]
    pub struct RatioBox;

    #[glib::object_subclass]
    impl ObjectSubclass for RatioBox {
        const NAME: &'static str = "VireoRatioBox";
        type Type = super::RatioBox;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for RatioBox {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for RatioBox {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Vertical => {
                    // Height follows the allocated width. Until the width is
                    // known (for_size < 0) request nothing and let the parent
                    // stretch us horizontally first.
                    let h = if for_size > 0 {
                        for_size * HEIGHT_OVER_WIDTH_NUM / HEIGHT_OVER_WIDTH_DEN
                    } else {
                        0
                    };
                    (h, h, -1, -1)
                }
                // Width is driven by the parent (hexpand + width-request); ask
                // for nothing intrinsic so we fill whatever the column offers.
                _ => (0, 0, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.obj().first_child() {
                child.allocate(width, height, baseline, None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gallery's default catch: inboxes, archives and custom folders, but
    /// not Sent.
    #[test]
    fn sent_is_the_only_kind_off_by_default() {
        for kind in [FolderKind::Inbox, FolderKind::Starred, FolderKind::Archive, FolderKind::Custom] {
            assert!(in_scope(kind, true, true, None), "{kind:?} should be on by default");
        }
        assert!(!in_scope(FolderKind::Sent, true, true, None));
    }

    #[test]
    fn a_tick_overrides_the_kind_default_either_way() {
        assert!(in_scope(FolderKind::Sent, true, true, Some(true)));
        assert!(!in_scope(FolderKind::Custom, true, true, Some(false)));
    }

    #[test]
    fn each_master_switch_governs_only_its_own_kinds() {
        // Archive off takes the archive down and leaves the rest alone.
        assert!(!in_scope(FolderKind::Archive, false, true, None));
        assert!(in_scope(FolderKind::Inbox, false, true, None));
        assert!(in_scope(FolderKind::Custom, false, true, None));
        // "Other" off takes everything that is neither inbox nor archive.
        assert!(!in_scope(FolderKind::Custom, true, false, None));
        assert!(!in_scope(FolderKind::Starred, true, false, None));
        assert!(in_scope(FolderKind::Inbox, true, false, None));
        assert!(in_scope(FolderKind::Archive, true, false, None));
    }

    /// An inbox answers to neither switch: there is always something to show.
    #[test]
    fn an_inbox_survives_both_switches_being_off() {
        assert!(in_scope(FolderKind::Inbox, false, false, None));
    }

    /// A master switch that is off beats a tick, but does not erase it: the
    /// tick is what comes back when the switch returns.
    #[test]
    fn a_master_switch_outranks_a_tick_without_clearing_it() {
        assert!(!in_scope(FolderKind::Archive, false, true, Some(true)));
        assert!(in_scope(FolderKind::Archive, true, true, Some(true)));
    }

    fn item(name: &str, from: &str, subject: &str, folder: &str, ts: i64, size: u64) -> GalleryItem {
        GalleryItem {
            account_id: 1,
            downloaded: true,
            folder_path: folder.into(),
            uid: 1,
            name: name.into(),
            size,
            from_name: from.into(),
            subject: subject.into(),
            timestamp: ts,
            data: None,
        }
    }



    /// A minimal one-page PDF, built by hand — just enough structure for
    /// poppler to load and render, with no dependency on an external file.
    fn minimal_pdf() -> Vec<u8> {
        let content = b"1 0 0 rg 100 100 300 300 re f";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 400] /Contents 4 0 R >>".to_string(),
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                content.len(),
                std::str::from_utf8(content).unwrap()
            ),
        ];
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = vec![0usize];
        for (i, obj) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(obj.as_bytes());
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets[1..] {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
                objects.len() + 1,
                xref_offset
            )
            .as_bytes(),
        );
        out
    }


    #[test]
    fn sort_index_roundtrips_every_criterion() {
        for i in 0..10 {
            assert_eq!(GallerySort::from_index(i).index(), i);
        }
    }

    #[test]
    fn column_headers_carry_the_sort_arrow() {
        use super::column_header;
        assert_eq!(column_header("Name", GallerySort::Name, GallerySort::Name, GallerySort::NameDesc), "Name ↑");
        assert_eq!(
            column_header("Name", GallerySort::NameDesc, GallerySort::Name, GallerySort::NameDesc),
            "Name ↓"
        );
        assert_eq!(column_header("Name", GallerySort::Newest, GallerySort::Name, GallerySort::NameDesc), "Name");
    }

    #[test]
    fn counts_read_naturally() {
        use super::count_text;
        assert_eq!(count_text(1, 1), "1 attachment");
        assert_eq!(count_text(42, 42), "42 attachments");
        assert_eq!(count_text(12, 42), "12 of 42");
    }

    #[test]
    fn pdf_thumbnail_renders_the_first_page() {
        let tex = pdf_page_texture(&minimal_pdf(), 360.0).expect("should render a page");
        assert!(tex.width() > 0 && tex.height() > 0);
        assert!(pdf_page_texture(b"not a real pdf", 360.0).is_none());
    }

    #[test]
    fn thumbnail_texture_classifies_attachment_types() {
        // No thumbnail for a type without one; an uncached PDF is rendered
        // asynchronously, so the immediate answer is Pending.
        assert!(matches!(
            thumbnail_texture("archive.zip", b"not a real zip"),
            Thumbnail::Fallback
        ));
        assert!(matches!(
            thumbnail_texture("notes.pdf", &minimal_pdf()),
            Thumbnail::Pending
        ));
    }


    #[test]
    fn icon_color_class_maps_types() {
        assert_eq!(icon_color_class("report.pdf"), "ftype-pdf");
        assert_eq!(icon_color_class("Notes.DOCX"), "ftype-doc"); // case-insensitive
        assert_eq!(icon_color_class("budget.xlsx"), "ftype-sheet");
        assert_eq!(icon_color_class("deck.pptx"), "ftype-slides");
        assert_eq!(icon_color_class("bundle.zip"), "ftype-archive");
        assert_eq!(icon_color_class("song.mp3"), "ftype-audio");
        assert_eq!(icon_color_class("clip.mov"), "ftype-video");
        assert_eq!(icon_color_class("invite.ics"), "ftype-calendar");
        assert_eq!(icon_color_class("photo.png"), "ftype-image");
        assert_eq!(icon_color_class("data.bin"), "ftype-generic");
        assert_eq!(icon_color_class("noext"), "ftype-generic");
    }
}
