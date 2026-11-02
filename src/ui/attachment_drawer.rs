//! In-message attachment drawer: a resizable footer beneath the reader body that
//! shows every attachment on the open message as a wrapping grid of thumbnails
//! (images and PDFs) or color-coded type icons (everything else), each with the
//! filename beneath it.
//!
//! It reuses the gallery's thumbnail/icon/open helpers ([`texture_from`],
//! [`thumbnail_texture`], [`icon_for`], [`icon_color_class`], [`open_bytes`]).
//! Hovering a cell reveals
//! the same Download/Open quick actions used in the gallery; right-clicking opens
//! a matching context menu; double-clicking previews images and PDFs in the
//! app's full-window lightbox (via [`DrawerOutput::ShowLightbox`]) and opens
//! any other type in its default app. Single clicks do nothing — a first click
//! must never steal the second. The header's "Save All…" button writes every
//! attachment into a chosen folder.
//!
//! Sizing: the drawer *owns* a vertical `GtkPaned` whose top pane is the reader
//! body (passed in via [`DrawerInit`]) and whose bottom pane is the drawer. The
//! divider is an invisible grabbable strip — native drag, and because the
//! reader pane is allowed to shrink, resizing the drawer never grows the
//! window; while the drawer is collapsed a drag snaps back, so only the
//! expanded drawer resizes. The size slider in the header scales the
//! thumbnails only; a click on the grab pill — or on the header's
//! "N attachments" count, which is a button for exactly this — collapses the
//! grid to just the header, and clicking again brings it back. The dragged
//! height, collapsed state and view settings are persisted; thumbnail size is
//! per-session.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;

use crate::config::{self, DrawerState};
use crate::i18n::{i18n, ni18n_f};
use crate::models::Attachment;
use crate::ui::attachment_io::open_bytes;

mod actions;
mod cells;
mod motion;
mod setup;

pub(crate) use cells::previewable;
use setup::{
    activate_on_keys, grab_pill, remove_paned_gestures, showcase_clicks, wire_drawer_edge,
};

const MIN_THUMB: f64 = 56.0;
const MAX_THUMB: f64 = 220.0;
/// Minimum expanded drawer height in px (the drag can't go below this).
const MIN_HEIGHT: i32 = 96;

/// What the drawer needs at launch: its remembered state plus the reader widget
/// it docks beneath (which becomes the top pane of its internal `GtkPaned`).
pub(crate) struct DrawerInit {
    pub(crate) state: DrawerState,
    pub(crate) reader: gtk::Widget,
}

pub(crate) struct AttachmentDrawer {
    items: Vec<Attachment>,
    /// Display position → index into `items`. Identity for the grid; the list
    /// view is sorted alphabetically (flipped by `sort_desc`).
    display_order: Vec<usize>,
    /// Show an alphabetical list instead of the thumbnail grid (persisted).
    list_view: bool,
    /// Sort the list Z→A instead of A→Z (persisted).
    sort_desc: bool,
    /// Thumbnail edge in px (size slider); does not affect drawer height.
    thumb: i32,
    /// Expanded drawer height in px (the Paned split); persisted.
    height: i32,
    /// Whether the thumbnail area is hidden, leaving only the header.
    collapsed: bool,
    /// The wrapping thumbnail grid (rebuilt on item/size changes).
    flow: gtk::FlowBox,
    /// Vertical split: reader body on top, drawer body below. The divider is the
    /// resize grip — native drag, and the reader shrinks rather than the window
    /// growing.
    paned: gtk::Paned,
    /// The visible resize affordance: the shared grab pill floated just above
    /// the seam (over the reader's bottom edge), like the split reply's.
    /// Shown whenever the drawer is. A click toggles collapsed/expanded; a
    /// drag resizes live — out of the collapse too, where the release point
    /// becomes the drawer's new height.
    pill: gtk::Box,
    /// True while we set the Paned position programmatically, so the resulting
    /// position-notify isn't mistaken for a user drag.
    adjusting: Rc<Cell<bool>>,
    /// Set once the initial split has been applied (needs a realized Paned).
    positioned: Rc<Cell<bool>>,
    /// Debounces persisting the dragged height: position-notify fires for
    /// every pixel of a drag, and each save is a config write.
    height_save: Rc<std::cell::RefCell<Option<glib::SourceId>>>,
    /// Whether the press currently on the pill has actually dragged (moved
    /// past the slop). Tells a release apart from a click: a click toggles
    /// the drawer, a drag's release must not.
    pill_dragged: bool,
    /// The running expand/collapse slide and its direction (`true` = closing
    /// toward the header). Held so a toggle or drag mid-slide can skip it to
    /// its end instead of fighting it over the divider.
    toggle_anim: Rc<std::cell::RefCell<Option<(adw::TimedAnimation, bool)>>>,
    /// Bumped whenever a slide is superseded, so a stale `ToggleSettled`
    /// from a skipped animation is recognised and ignored.
    toggle_gen: u64,
    /// Files being deleted from the server (#289), by name and size: drawn
    /// pale red with "Deleting…" until the server answers.
    deleting: Vec<(String, usize)>,
    /// Deleted files still fading out. A new list that arrives meanwhile
    /// waits in `pending_items`, so a fade is never cut short by a rebuild.
    fading: usize,
    pending_items: Option<Vec<Attachment>>,
    /// The drawer's body, faded in when a message with attachments opens.
    drawer_body: Option<gtk::Box>,
    /// The body in its own overlay, which carries `swapping`. What a still
    /// of the drawer is taken from: it is what is on screen, swap included.
    drawer_frame: Option<gtk::Overlay>,
    /// A still of the drawer, faded out where it stood when a message
    /// without attachments replaces one with them.
    leaving: gtk::Picture,
    /// The running fade in or out.
    presence_anim: Rc<std::cell::RefCell<Option<adw::TimedAnimation>>>,
    /// A still of the previous message's drawer, dissolving over this one's
    /// when both have attachments.
    swapping: gtk::Picture,
    swap_anim: Rc<std::cell::RefCell<Option<adw::TimedAnimation>>>,
    /// Bumped by every list, so an empty one held back is dropped when the
    /// next message's list follows it.
    empty_gen: u64,
}

/// What the drawer asks the app to do.
#[derive(Debug)]
pub(crate) enum DrawerOutput {
    /// Show the app's full-window lightbox over these previewable
    /// attachments, starting at `start`.
    ShowLightbox {
        items: Vec<Attachment>,
        start: usize,
    },
    /// Scroll the reader to the message this attachment came with (#213).
    ShowInMessage(Attachment),
    /// Take this attachment out of its message on the server (#289); the
    /// app asks first.
    DeleteFromServer(Attachment),
}

#[derive(Debug)]
pub(crate) enum AttachmentDrawerInput {
    /// Replace the shown attachments (empty hides the drawer).
    SetItems(Vec<Attachment>),
    /// New thumbnail edge from the size slider (thumbnails only, not height).
    SetThumbSize(i32),
    /// The Paned divider moved (user drag) — capture the new height.
    PositionChanged,
    /// Switch between the thumbnail grid and the alphabetical list.
    ToggleListView,
    /// Flip the list's sort direction (A→Z / Z→A).
    ToggleSortOrder,
    /// Open an attachment in its default application.
    Open(usize),
    /// Import an attached OpenPGP public key into the keyring (#133).
    ImportKey(usize),
    /// Activate a cell/row (display order) — double-click, or Space/Enter on
    /// the highlighted one: preview images and PDFs in the lightbox, open
    /// anything else in its default app.
    Activate(usize),
    /// Save an attachment to disk (file chooser).
    Download(usize),
    /// Scroll the reader to the message the attachment belongs to (#213).
    ShowInMessage(usize),
    /// Remove the attachment from its message on the server (#289).
    DeleteFromServer(usize),
    /// The app is deleting this file (name, size) from the server: show it
    /// as being deleted.
    MarkDeleting(String, usize),
    /// The server refused: show the file as it was.
    NotDeleted(String, usize),
    /// The server deleted it: fade it out.
    Deleted(String, usize),
    /// A fade finished: take the file away.
    FadeDone(String, usize),
    /// Save every attachment into a chosen folder ("Save All" header button).
    SaveAll,
    /// Right-click at (x, y) within the cell.
    ContextMenu { index: usize, x: f64, y: f64 },
    /// A press landed on the grab pill (a drag may or may not follow).
    PillDragBegin,
    /// The grab pill was dragged to this candidate divider position.
    PillDrag { want: i32 },
    /// The press on the grab pill was released. A plain click (no drag in
    /// between) toggles the drawer collapsed/expanded.
    PillReleased,
    /// A click landed on the seam itself (the Paned separator, or the
    /// drawer's topmost strip) — toggles like a pill click.
    EdgeClicked,
    /// The header's "N attachments" button was clicked — toggles the drawer
    /// like the seam does, from the one part of the header that is always
    /// visible (collapsed included).
    CountClicked,
    /// The expand/collapse slide finished. Carries the state it was heading
    /// for and the generation it belongs to (see `toggle_gen`).
    ToggleSettled { collapsed: bool, gen: u64 },
    /// Fold the drawer at once, its files out of the way (the reader below
    /// the list, #475).
    Fold,
    /// An empty list held back (see `SetItems`) is due, unless a newer list
    /// came since: carries the `empty_gen` it was held under.
    SettleEmpty(u64),
}

#[relm4::component(pub(crate))]
impl SimpleComponent for AttachmentDrawer {
    type Init = DrawerInit;
    type Input = AttachmentDrawerInput;
    type Output = DrawerOutput;

    view! {
        #[root]
        gtk::Paned {
            set_orientation: gtk::Orientation::Vertical,
            // No wide handle: its stock chrome is a bordered band with a
            // grip. The plain separator is a permanently inert hairline (see
            // `.drawer-split` and set_divider_draggable) — the grab pill
            // floated over the reader's bottom edge and the drawer's own
            // edge strip carry every click and drag.
            add_css_class: "drawer-split",
            // The reader (start child) shrinks to make room; the drawer keeps its
            // set size. This is what prevents the window from growing.
            set_resize_start_child: true,
            set_shrink_start_child: true,
            set_resize_end_child: false,
            set_shrink_end_child: false,
            connect_position_notify[sender] => move |_| {
                sender.input_sender().emit(AttachmentDrawerInput::PositionChanged);
            },

            #[wrap(Some)]
            #[name = "drawer_body"]
            set_end_child = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                add_css_class: "attachment-drawer",
                // Hidden (and the divider disappears) when there's nothing to show.
                #[watch]
                set_visible: !model.items.is_empty(),

                // The drawer's own top edge as a real widget: it clicks
                // (toggle), drags (resize), and carries the same ns-resize
                // cursor as the pill zone above, so the seam feels like one
                // continuous handle. The native Paned separator is shrunk to
                // a hairline — its drag gesture claims every press, which
                // made it a click-dead band with its own cursor.
                #[name = "drawer_edge"]
                gtk::Box {
                    set_height_request: 8,
                    set_cursor_from_name: Some("ns-resize"),
                },

                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 8,
                    add_css_class: "attachment-drawer-header",
                    // The count is the drawer's other handle: clicking it
                    // expands or collapses exactly like a click on the seam,
                    // with the chevron showing which way it goes next.
                    #[name = "count_btn"]
                    gtk::Button {
                        add_css_class: "flat",
                        // GTK hands a button with a plain label the
                        // `text-button` padding and one with an icon the
                        // `image-button` padding; a button with a child of its
                        // own (this one: clip, count, chevron) gets neither and
                        // ends up tighter than "Save All…" beside it. Ask for
                        // the text one by hand so the two pills match.
                        add_css_class: "text-button",
                        set_valign: gtk::Align::Center,
                        // A hair off the header's own inset at each end
                        // (Jason, 2026-09-17): the count and "Save All…" are
                        // the row's two pills, so they sit the same distance
                        // from the edge they face.
                        set_margin_start: 3,
                        #[watch]
                        set_tooltip_text: Some(if model.collapsed {
                            i18n("Show the attachments")
                        } else {
                            i18n("Hide the attachments")
                        }.as_str()),
                        connect_clicked[sender] => move |_| {
                            sender.input_sender().emit(AttachmentDrawerInput::CountClicked);
                        },

                        #[wrap(Some)]
                        set_child = &gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 8,
                            gtk::Image {
                                set_icon_name: Some("mail-attachment-symbolic"),
                                add_css_class: "dim-label",
                            },
                            gtk::Label {
                                #[watch]
                                set_label: &ni18n_f(
                                    "{n} attachment",
                                    "{n} attachments",
                                    model.items.len() as u32,
                                    &[("n", &model.items.len().to_string())],
                                ),
                                add_css_class: "heading",
                            },
                            gtk::Image {
                                #[watch]
                                set_icon_name: Some(if model.collapsed {
                                    "pan-up-symbolic"
                                } else {
                                    "pan-down-symbolic"
                                }),
                                add_css_class: "dim-label",
                                set_pixel_size: 12,
                            },
                        },
                    },
                    gtk::Box { set_hexpand: true },
                    gtk::Image {
                        set_icon_name: Some("image-x-generic-symbolic"),
                        add_css_class: "dim-label",
                        set_pixel_size: 12,
                        // The size slider is meaningless with the grid hidden
                        // (and sizes thumbnails only, so also in the list view).
                        #[watch]
                        set_visible: !model.collapsed && !model.list_view,
                    },
                    gtk::Scale {
                        set_orientation: gtk::Orientation::Horizontal,
                        set_width_request: 130,
                        set_range: (MIN_THUMB, MAX_THUMB),
                        set_value: model.thumb as f64,
                        set_round_digits: 0,
                        set_draw_value: false,
                        set_tooltip_text: Some(i18n("Thumbnail size").as_str()),
                        set_valign: gtk::Align::Center,
                        #[watch]
                        set_visible: !model.collapsed && !model.list_view,
                        connect_value_changed[sender] => move |s| {
                            sender.input_sender().emit(AttachmentDrawerInput::SetThumbSize(s.value() as i32));
                        },
                    },
                    gtk::Image {
                        set_icon_name: Some("image-x-generic-symbolic"),
                        add_css_class: "dim-label",
                        set_pixel_size: 22,
                        #[watch]
                        set_visible: !model.collapsed && !model.list_view,
                    },
                    gtk::Button {
                        add_css_class: "flat",
                        set_valign: gtk::Align::Center,
                        // Only the list has an order to flip.
                        #[watch]
                        set_visible: !model.collapsed && model.list_view,
                        #[watch]
                        set_icon_name: if model.sort_desc {
                            "view-sort-descending-symbolic"
                        } else {
                            "view-sort-ascending-symbolic"
                        },
                        #[watch]
                        set_tooltip_text: Some(if model.sort_desc { i18n("Sorted Z to A — switch to A to Z") } else { i18n("Sorted A to Z — switch to Z to A") }.as_str()),
                        connect_clicked[sender] => move |_| {
                            sender.input_sender().emit(AttachmentDrawerInput::ToggleSortOrder);
                        },
                    },
                    gtk::Button {
                        add_css_class: "flat",
                        set_valign: gtk::Align::Center,
                        #[watch]
                        set_visible: !model.collapsed,
                        #[watch]
                        set_icon_name: if model.list_view {
                            "view-grid-symbolic"
                        } else {
                            "view-list-bullet-symbolic"
                        },
                        #[watch]
                        set_tooltip_text: Some(if model.list_view { i18n("Show as thumbnails") } else { i18n("Show as a list") }.as_str()),
                        connect_clicked[sender] => move |_| {
                            sender.input_sender().emit(AttachmentDrawerInput::ToggleListView);
                        },
                    },
                    // Rightmost by design: the header's one real action, with
                    // the view controls (slider, sort, list/grid) to its left.
                    gtk::Button {
                        set_label: &i18n("Save All…"),
                        // A standing button, not a flat hover-reveal.
                        set_valign: gtk::Align::Center,
                        // Matching the count's inset at the other end.
                        set_margin_end: 3,
                        set_tooltip_text: Some(i18n("Save every attachment to a folder").as_str()),
                        connect_clicked[sender] => move |_| {
                            sender.input_sender().emit(AttachmentDrawerInput::SaveAll);
                        },
                    },
                },

                #[local_ref]
                scroller -> gtk::ScrolledWindow {
                    set_vexpand: true,
                    set_hscrollbar_policy: gtk::PolicyType::Never,
                    set_vscrollbar_policy: gtk::PolicyType::Automatic,
                    #[watch]
                    set_visible: !model.collapsed,

                    #[local_ref]
                    flow -> gtk::FlowBox {
                        set_orientation: gtk::Orientation::Horizontal,
                        // Single-click highlights an attachment; Space or Enter
                        // then previews it, Nautilus-style (issue #37).
                        set_selection_mode: gtk::SelectionMode::Single,
                        set_activate_on_single_click: true,
                        set_homogeneous: false,
                        set_valign: gtk::Align::Start,
                        set_row_spacing: 6,
                        set_column_spacing: 6,
                        set_margin_top: 8,
                        set_margin_bottom: 8,
                        set_margin_start: 8,
                        set_margin_end: 8,
                        set_min_children_per_line: 1,
                        #[watch]
                        set_max_children_per_line: if model.list_view { 1 } else { 40 },
                        add_css_class: "attachment-drawer-flow",

                    },
                },
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let flow = gtk::FlowBox::default();
        let scroller = gtk::ScrolledWindow::default();

        activate_on_keys(&flow, &sender);

        let pill = grab_pill(&root, &sender);

        let leaving = gtk::Picture::new();
        leaving.set_content_fit(gtk::ContentFit::Fill);
        leaving.set_can_shrink(true);
        leaving.set_can_target(false);
        leaving.set_valign(gtk::Align::End);
        leaving.add_css_class("attachment-drawer-still");
        let swapping = gtk::Picture::new();
        swapping.set_content_fit(gtk::ContentFit::Fill);
        swapping.set_can_shrink(true);
        swapping.set_can_target(false);
        swapping.set_visible(false);
        swapping.add_css_class("attachment-drawer-still");
        leaving.set_visible(false);

        let mut model = AttachmentDrawer {
            items: Vec::new(),
            display_order: Vec::new(),
            list_view: init.state.list_view,
            sort_desc: init.state.sort_desc,
            thumb: init.state.thumb.clamp(MIN_THUMB as i32, MAX_THUMB as i32),
            height: init.state.height.max(MIN_HEIGHT),
            collapsed: init.state.collapsed,
            flow: flow.clone(),
            // The root of this component IS the Paned.
            paned: root.clone(),
            pill: pill.clone(),
            adjusting: Rc::new(Cell::new(false)),
            positioned: Rc::new(Cell::new(false)),
            height_save: Rc::new(std::cell::RefCell::new(None)),
            pill_dragged: false,
            toggle_anim: Rc::new(std::cell::RefCell::new(None)),
            toggle_gen: 0,
            deleting: Vec::new(),
            fading: 0,
            pending_items: None,
            drawer_body: None,
            drawer_frame: None,
            leaving: leaving.clone(),
            presence_anim: Rc::new(std::cell::RefCell::new(None)),
            swapping: swapping.clone(),
            swap_anim: Rc::new(std::cell::RefCell::new(None)),
            empty_gen: 0,
        };

        let widgets = view_output!();
        model.drawer_body = Some(widgets.drawer_body.clone());
        // The body moves into an overlay of its own, where the previous
        // message's drawer dissolves over it. The overlay comes and goes
        // with the body, so the split still hides its seam with no drawer.
        {
            let body = widgets.drawer_body.clone();
            let frame = gtk::Overlay::new();
            root.set_end_child(None::<&gtk::Widget>);
            frame.set_child(Some(&body));
            frame.add_overlay(&swapping);
            body.bind_property("visible", &frame, "visible")
                .sync_create()
                .build();
            root.set_end_child(Some(&frame));
            root.set_resize_end_child(false);
            root.set_shrink_end_child(false);
            model.drawer_frame = Some(frame);
        }
        // Dock the reader as the top pane (the drawer body is the bottom pane),
        // wrapped in the Overlay that floats the grab pill over its lower edge.
        let reader_wrap = gtk::Overlay::new();
        reader_wrap.set_child(Some(&init.reader));
        reader_wrap.add_overlay(&leaving);
        reader_wrap.add_overlay(&pill);
        root.set_start_child(Some(&reader_wrap));
        // A collapsed drawer starts with its divider locked.
        // The native separator is permanently inert: GTK gives it an
        // enlarged invisible input region that wears its own row-resize
        // cursor and steals presses landing near the seam — the pill zone
        // and the drawer's edge strip carry every click and drag instead.
        model.set_divider_draggable(false);
        remove_paned_gestures(&root);

        wire_drawer_edge(&widgets.drawer_edge, &pill, &root, &sender);
        showcase_clicks(&widgets.count_btn);
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            AttachmentDrawerInput::SetItems(items) if self.fading > 0 => {
                self.pending_items = Some(items);
            }
            AttachmentDrawerInput::SetItems(items) => self.set_items(items, &sender),
            AttachmentDrawerInput::SettleEmpty(gen) => {
                if gen == self.empty_gen {
                    self.apply_items(Vec::new(), &sender);
                }
            }
            AttachmentDrawerInput::SetThumbSize(size) => {
                // Thumbnail size is a live setting only — never persisted.
                let size = size.clamp(MIN_THUMB as i32, MAX_THUMB as i32);
                if size != self.thumb {
                    self.thumb = size;
                    self.rebuild(&sender);
                }
            }
            AttachmentDrawerInput::PositionChanged => self.position_changed(),
            AttachmentDrawerInput::ToggleListView => {
                self.list_view = !self.list_view;
                self.rebuild(&sender);
                config::save_drawer_list_view(self.list_view);
            }
            AttachmentDrawerInput::ToggleSortOrder => {
                self.sort_desc = !self.sort_desc;
                self.rebuild(&sender);
                config::save_drawer_sort_desc(self.sort_desc);
            }
            AttachmentDrawerInput::Open(i) => {
                if let Some(att) = self.item_at(i) {
                    open_bytes(&att.name, &att.data, self.window().as_ref());
                }
            }
            AttachmentDrawerInput::ImportKey(i) => self.import_key(i),
            AttachmentDrawerInput::Download(i) => {
                if let Some(att) = self.item_at(i).cloned() {
                    self.save_attachment(&att);
                }
            }
            AttachmentDrawerInput::ShowInMessage(i) => {
                if let Some(att) = self.item_at(i).cloned() {
                    let _ = sender.output(DrawerOutput::ShowInMessage(att));
                }
            }
            AttachmentDrawerInput::MarkDeleting(name, len) => {
                self.deleting.push((name, len));
                self.rebuild_unless_fading(&sender);
            }
            AttachmentDrawerInput::NotDeleted(name, len) => {
                if let Some(at) = self.deleting.iter().position(|d| *d == (name.clone(), len)) {
                    self.deleting.remove(at);
                    self.rebuild_unless_fading(&sender);
                }
            }
            AttachmentDrawerInput::Deleted(name, len) => self.deleted(name, len, &sender),
            AttachmentDrawerInput::FadeDone(name, len) => self.fade_done(name, len, &sender),
            AttachmentDrawerInput::DeleteFromServer(i) => {
                if let Some(att) = self.item_at(i).cloned() {
                    let _ = sender.output(DrawerOutput::DeleteFromServer(att));
                }
            }
            AttachmentDrawerInput::Activate(i) => self.activate(i, &sender),
            AttachmentDrawerInput::SaveAll => {
                self.save_all();
            }
            AttachmentDrawerInput::ContextMenu { index, x, y } => {
                self.show_context_menu(index, x, y, &sender);
            }
            AttachmentDrawerInput::PillDragBegin => {
                self.pill_dragged = false;
                // A drag takes over from any running slide: jump it to its
                // end (settling the collapse it was heading for) so the two
                // don't fight over the divider.
                self.finish_running_toggle();
            }
            AttachmentDrawerInput::PillDrag { want } => self.pill_drag(want),
            AttachmentDrawerInput::PillReleased => {
                // A plain click — the press never travelled past the drag
                // slop — toggles the drawer. A drag's release must not: the
                // user just set a height.
                if !self.pill_dragged {
                    self.toggle_collapsed(&sender);
                }
            }
            AttachmentDrawerInput::EdgeClicked => {
                // The edge strip shares the pill's drag plumbing: a release
                // that ended a drag just set a height and must not toggle.
                if !self.pill_dragged {
                    self.toggle_collapsed(&sender);
                }
            }
            AttachmentDrawerInput::CountClicked => {
                // A real button, never part of a drag: it toggles
                // unconditionally (the seam's `pill_dragged` guard would
                // otherwise swallow the first click after any resize).
                self.toggle_collapsed(&sender);
            }
            AttachmentDrawerInput::ToggleSettled { collapsed, gen } => {
                self.toggle_settled(collapsed, gen)
            }
            AttachmentDrawerInput::Fold => self.fold(),
        }
    }
}

impl AttachmentDrawer {}
