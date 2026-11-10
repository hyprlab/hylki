//! Attachments gallery: a grid of every cached attachment across the connected
//! inboxes, with a lightbox preview (prev/next, open, go to message).
//!
//! Data comes from the SQLite cache (what the background prefetch has already
//! downloaded), fed in a page at a time via [`GalleryInput::Page`]. Image attachments and PDFs
//! (rendered from their first page) show as thumbnails; other files show a type
//! icon. Clicking a cell opens a large overlay preview.

use adw::prelude::*;
use gtk::gdk;
use gtk::glib;
use relm4::prelude::*;

use std::collections::HashMap;

use crate::i18n::i18n;
use crate::models::{GalleryItem, GallerySort};

mod cell;
mod items;
mod labels;
mod msg;
mod ratio_box;
mod scope;
mod table;
mod update;

use cell::build_cell;
pub(crate) use cell::{deleting_label, deleting_over, deleting_veil, fade_out};
use labels::{caption, column_header, count_text, scan_text};
use msg::key_of;
pub(crate) use msg::{FileKey, GalleryInput, GalleryOutput, GalleryRequest};

use crate::ui::attachment_io::open_bytes;
use crate::ui::thumbnails::{
    icon_color_class, icon_for, is_pdf_name, lightbox_pdf_texture, spawn_thumbnail_render,
    texture_from, thumb_cache_key, thumbnail_spinner, thumbnail_texture, Thumbnail, PDF_PREVIEWS,
};
use ratio_box::RatioBox;
pub(crate) use scope::{GalleryAccount, GalleryFolder};
use table::build_row;

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

pub(crate) struct AttachmentsGallery {
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
    /// The PDF being previewed could not be rendered, such as one locked
    /// with a password: its page is not on its way.
    preview_failed: bool,
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
    /// Files being deleted from the server (#289), drawn pale red with
    /// "Deleting…" until the server answers.
    deleting: Vec<FileKey>,
}

#[relm4::component(pub(crate))]
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
                        set_icon_name: Some("mail-attachment-symbolic"),
                        set_title: &i18n("No attachments"),
                        set_description: Some(i18n("Attachments from your inboxes will appear here.").as_str()),
                    },

                    add_named[Some("noresults")] = &adw::StatusPage {
                        set_icon_name: Some("system-search-symbolic"),
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
                                    set_label: &column_header(&i18n("Name"), model.sort, GallerySort::Name, GallerySort::NameDesc),
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
                                    set_label: &column_header(&i18n("Sender"), model.sort, GallerySort::Sender, GallerySort::SenderDesc),
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
                                    set_label: &column_header(&i18n("Type"), model.sort, GallerySort::Type, GallerySort::TypeDesc),
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
                                    set_label: &column_header(&i18n("Date"), model.sort, GallerySort::Oldest, GallerySort::Newest),
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
                                    set_label: &column_header(&i18n("Size"), model.sort, GallerySort::Smallest, GallerySort::Largest),
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
                            set_icon_name: "view-grid-symbolic",
                            set_tooltip_text: Some(i18n("Thumbnail grid").as_str()),
                            #[watch]
                            #[block_signal(grid_toggle)]
                            set_active: !model.view_table,
                            connect_clicked[sender] => move |_| {
                                sender.input(GalleryInput::SetViewTable(false));
                            } @grid_toggle,
                        },
                        gtk::ToggleButton {
                            set_icon_name: "view-list-bullet-symbolic",
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
                        set_icon_name: "folder-symbolic",
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
                        set_icon_name: "window-close-symbolic",
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
                        set_icon_name: "go-previous-symbolic",
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
                        } else if !model.preview_failed
                            && model.current().is_some_and(|i| is_pdf_name(&i.name) && i.data.is_some())
                        {
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
                            add_css_class: "gallery-lightbox-file",
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
                            gtk::Label {
                                set_label: &i18n("No preview available"),
                                add_css_class: "dim-label",
                                #[watch]
                                set_visible: model.preview_failed,
                            },
                        },
                    },

                    gtk::Button {
                        set_icon_name: "go-next-symbolic",
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
            preview_failed: false,
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
            deleting: Vec::new(),
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
            GalleryInput::Page {
                items,
                total,
                offset,
            } => self.on_page(items, total, offset, &sender),
            GalleryInput::LoadMore => self.load_more(&sender),
            GalleryInput::ScanProgress => {
                if self.all_items.len() <= PAGE_SIZE as usize {
                    self.reload(&sender);
                }
            }
            GalleryInput::ScanStatus(remaining) => self.scan_remaining = remaining,
            GalleryInput::SetFetching(on) => self.fetching = on,
            GalleryInput::Fetched {
                account_id,
                path,
                uid,
                items,
            } => self.on_fetched(account_id, path, uid, items, &sender),
            GalleryInput::SetDeleting(key, on) => self.on_set_deleting(key, on, &sender),
            GalleryInput::Removed(key) => self.on_removed(key, &sender),
            GalleryInput::RemoveNow(key) => {
                if !self.on_remove_now(key, &sender) {
                    return;
                }
            }
            GalleryInput::SetQuery(q) => self.on_set_query(q, &sender),
            GalleryInput::ApplyQuery => {
                self.query_timer = None;
                self.reload(&sender);
            }
            GalleryInput::SetSort(i) => self.on_set_sort(i, &sender),
            GalleryInput::SortColumn(col) => self.on_sort_column(col, widgets, &sender),
            GalleryInput::SetViewTable(table) => self.on_set_view_table(table, &sender),
            GalleryInput::SetThumbWidth(v) => self.on_set_thumb_width(v, &sender),
            GalleryInput::ApplyThumbWidth => self.on_apply_thumb_width(&sender),
            GalleryInput::SetTypeFilter(bucket) => {
                if self.type_filter != bucket {
                    self.type_filter = bucket;
                    self.reload(&sender);
                }
            }
            GalleryInput::SetAccounts(accounts) => self.on_set_accounts(accounts, widgets, &sender),
            GalleryInput::SetAccountFilter(row) => {
                self.on_set_account_filter(row, widgets, &sender)
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
            GalleryInput::SetFolder {
                account_id,
                path,
                on,
            } => self.on_set_folder(account_id, path, on, &sender),
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
            GalleryInput::PreviewRendered(key) => self.on_preview_rendered(key, &sender),
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
            GalleryInput::PreviewItem(i) => self.preview_item(i, &sender),
            GalleryInput::DownloadItem(i) => self.download_item(i, &sender),
            GalleryInput::GoToItem(i) => self.goto_item(i, &sender),
            GalleryInput::ShowcaseDelete(name) => self.on_showcase_delete(name, &sender),
            GalleryInput::DeleteItem(i) => self.on_delete_item(i, &sender),
            GalleryInput::OpenExternal(i) => {
                // Double-click: skip/close the preview and open the file directly.
                self.preview = None;
                self.open_item(i, &sender);
            }
            GalleryInput::ContextMenu { index, x, y } => {
                self.show_context_menu(index, x, y, &sender)
            }
            GalleryInput::ShowcaseSearch(text) => self.on_showcase_search(text, widgets),
            GalleryInput::ShowcaseFolders => self.on_showcase_folders(widgets),
        }
        self.update_view(widgets, sender);
    }
}

/// Whether a scroller has come close enough to its end to be worth asking for
/// the next page. `upper - page_size` is the furthest the view can scroll, so
/// this is "within a couple of rows of the bottom".
fn near_end(a: &gtk::Adjustment) -> bool {
    a.upper() - a.page_size() - a.value() < LOAD_MORE_MARGIN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_index_roundtrips_every_criterion() {
        for i in 0..10 {
            assert_eq!(GallerySort::from_index(i).index(), i);
        }
    }
}
