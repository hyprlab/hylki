//! In-message attachment drawer: a resizable footer beneath the reader body that
//! shows every attachment on the open message as a wrapping grid of thumbnails
//! (images and PDFs) or colour-coded type icons (everything else), each with the
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
use crate::models::{is_image_name, Attachment};
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use crate::ui::attachments_gallery::{
    icon_color_class, icon_for, is_pdf_name, open_bytes, spawn_thumbnail_render, texture_from,
    thumbnail_texture, Thumbnail,
};
use crate::i18n::i18n;

/// Whether an attachment can be shown in the drawer's lightbox: a decodable
/// image, or a PDF (whose first page renders on demand).
pub(crate) fn previewable(att: &Attachment) -> bool {
    (is_image_name(&att.name) && texture_from(&att.data).is_some()) || is_pdf_name(&att.name)
}

/// The drawer's cover-cropped picture for a ready thumbnail texture.
fn drawer_picture(tex: &gtk::gdk::Texture) -> gtk::Widget {
    let pic = gtk::Picture::for_paintable(tex);
    pic.set_content_fit(gtk::ContentFit::Cover);
    pic.set_hexpand(true);
    pic.set_vexpand(true);
    pic.upcast()
}

/// The drawer's centred type icon, sized to the current thumbnail scale.
fn drawer_icon(name: &str, thumb: i32) -> gtk::Widget {
    let img = gtk::Image::from_icon_name(icon_for(name));
    img.set_pixel_size((thumb / 2).max(24));
    img.set_halign(gtk::Align::Center);
    img.set_valign(gtk::Align::Center);
    img.set_hexpand(true);
    img.set_vexpand(true);
    img.add_css_class("gallery-file-icon");
    img.add_css_class(icon_color_class(name));
    img.upcast()
}

const MIN_THUMB: f64 = 56.0;
const MAX_THUMB: f64 = 220.0;
/// Minimum expanded drawer height in px (the drag can't go below this).
const MIN_HEIGHT: i32 = 96;

/// What the drawer needs at launch: its remembered state plus the reader widget
/// it docks beneath (which becomes the top pane of its internal `GtkPaned`).
pub struct DrawerInit {
    pub state: DrawerState,
    pub reader: gtk::Widget,
}

pub struct AttachmentDrawer {
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
}

/// What the drawer asks the app to do.
#[derive(Debug)]
pub enum DrawerOutput {
    /// Show the app's full-window lightbox over these previewable
    /// attachments, starting at `start`.
    ShowLightbox { items: Vec<Attachment>, start: usize },
    /// Scroll the reader to the message this attachment came with (#213).
    ShowInMessage(Attachment),
}

#[derive(Debug)]
pub enum AttachmentDrawerInput {
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
}

#[relm4::component(pub)]
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
                sender.input(AttachmentDrawerInput::PositionChanged);
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
                            sender.input(AttachmentDrawerInput::CountClicked);
                        },

                        #[wrap(Some)]
                        set_child = &gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 8,
                            gtk::Image {
                                set_icon_name: Some("co.hyprlab.Hylki-mail-attachment-symbolic"),
                                add_css_class: "dim-label",
                            },
                            gtk::Label {
                                #[watch]
                                set_label: &format!(
                                    "{} attachment{}",
                                    model.items.len(),
                                    if model.items.len() == 1 { "" } else { "s" },
                                ),
                                add_css_class: "heading",
                            },
                            gtk::Image {
                                #[watch]
                                set_icon_name: Some(if model.collapsed {
                                    "co.hyprlab.Hylki-pan-up-symbolic"
                                } else {
                                    "co.hyprlab.Hylki-pan-down-symbolic"
                                }),
                                add_css_class: "dim-label",
                                set_pixel_size: 12,
                            },
                        },
                    },
                    gtk::Box { set_hexpand: true },
                    gtk::Image {
                        set_icon_name: Some("co.hyprlab.Hylki-image-x-generic-symbolic"),
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
                            sender.input(AttachmentDrawerInput::SetThumbSize(s.value() as i32));
                        },
                    },
                    gtk::Image {
                        set_icon_name: Some("co.hyprlab.Hylki-image-x-generic-symbolic"),
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
                            "co.hyprlab.Hylki-view-sort-descending-symbolic"
                        } else {
                            "co.hyprlab.Hylki-view-sort-ascending-symbolic"
                        },
                        #[watch]
                        set_tooltip_text: Some(if model.sort_desc { i18n("Sorted Z to A — switch to A to Z") } else { i18n("Sorted A to Z — switch to Z to A") }.as_str()),
                        connect_clicked[sender] => move |_| {
                            sender.input(AttachmentDrawerInput::ToggleSortOrder);
                        },
                    },
                    gtk::Button {
                        add_css_class: "flat",
                        set_valign: gtk::Align::Center,
                        #[watch]
                        set_visible: !model.collapsed,
                        #[watch]
                        set_icon_name: if model.list_view {
                            "co.hyprlab.Hylki-view-grid-symbolic"
                        } else {
                            "co.hyprlab.Hylki-view-list-bullet-symbolic"
                        },
                        #[watch]
                        set_tooltip_text: Some(if model.list_view { i18n("Show as thumbnails") } else { i18n("Show as a list") }.as_str()),
                        connect_clicked[sender] => move |_| {
                            sender.input(AttachmentDrawerInput::ToggleListView);
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
                            sender.input(AttachmentDrawerInput::SaveAll);
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

        // Space (or Enter) previews the highlighted attachment — the keyboard
        // side of the single-click-then-preview flow (issue #37).
        {
            let key = gtk::EventControllerKey::new();
            let s = sender.clone();
            let flow_ref = flow.clone();
            key.connect_key_pressed(move |_, keyval, _, _| {
                use gtk::gdk::Key;
                if matches!(keyval, Key::space | Key::KP_Space | Key::Return | Key::KP_Enter) {
                    if let Some(child) = flow_ref.selected_children().first() {
                        s.input(AttachmentDrawerInput::Activate(child.index() as usize));
                        return gtk::glib::Propagation::Stop;
                    }
                }
                gtk::glib::Propagation::Proceed
            });
            flow.add_controller(key);
        }

        // The grab pill floats just above the seam, over the reader's bottom
        // edge — the same affordance, in the same place, as the split reply's.
        // Its gestures route through update(), which knows the collapsed
        // state: a plain click toggles the drawer, a drag resizes it live —
        // collapsed included, where the release point becomes the drawer's
        // new height.
        // The full-width edge variant: the whole seam clicks and drags, not
        // just the centred bar — hunting a 120px target over a busy reader
        // made the handle feel unreliable.
        let pill = crate::ui::grab_pill::edge_pill_widget();
        pill.set_valign(gtk::Align::End);
        pill.set_visible(false); // no seam until attachments arrive
        {
            let s = sender.clone();
            let s2 = sender.clone();
            crate::ui::grab_pill::attach_drag(
                &pill,
                &root,
                move |_| s.input(AttachmentDrawerInput::PillDragBegin),
                move |want| s2.input(AttachmentDrawerInput::PillDrag { want }),
            );
            let click = gtk::GestureClick::new();
            let s = sender.clone();
            click.connect_released(move |_, _, _, _| {
                s.input(AttachmentDrawerInput::PillReleased);
            });
            pill.add_controller(click);
        }

        let model = AttachmentDrawer {
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
        };

        let widgets = view_output!();
        // Dock the reader as the top pane (the drawer body is the bottom pane),
        // wrapped in the Overlay that floats the grab pill over its lower edge.
        let reader_wrap = gtk::Overlay::new();
        reader_wrap.set_child(Some(&init.reader));
        reader_wrap.add_overlay(&pill);
        root.set_start_child(Some(&reader_wrap));
        // A collapsed drawer starts with its divider locked.
        // The native separator is permanently inert: GTK gives it an
        // enlarged invisible input region that wears its own row-resize
        // cursor and steals presses landing near the seam — the pill zone
        // and the drawer's edge strip carry every click and drag instead.
        model.set_divider_draggable(false);
        // Untargeting the separator is not enough: GtkPaned's pan gesture is
        // attached to the PANED itself, in capture phase, hit-testing raw
        // coordinates against an enlarged handle area. It claims presses
        // near the seam before the zone or edge strip ever see them (hover
        // and cursor stay ours, the click dies, and a drag "works" because
        // the paned handles it natively). Remove the gesture outright.
        {
            // Both of them: the GestureDrag is what actually moves the
            // divider (and eats the clicks); the GesturePan is its touch
            // sibling. Only the keyboard ShortcutController stays.
            let controllers = root.observe_controllers();
            let mut doomed: Vec<gtk::EventController> = Vec::new();
            for i in 0..controllers.n_items() {
                let Some(c) =
                    controllers.item(i).and_then(|o| o.downcast::<gtk::EventController>().ok())
                else {
                    continue;
                };
                if c.is::<gtk::GestureDrag>() || c.is::<gtk::GesturePan>() {
                    doomed.push(c);
                }
            }
            tracing::debug!("removing {} paned gesture(s)", doomed.len());
            for c in doomed {
                root.remove_controller(&c);
            }
        }

        {
            // The drawer's top-edge strip: a click toggles, a drag resizes —
            // through the same inputs as the pill, so a drag out of the
            // collapse and the click/drag distinction behave identically.
            let click = gtk::GestureClick::new();
            let s = sender.clone();
            click.connect_released(move |_, _, _, _| {
                s.input(AttachmentDrawerInput::EdgeClicked);
            });
            widgets.drawer_edge.add_controller(click);
            // The bar lives in the zone above; hovering the strip is hovering
            // the same logical handle, so it lights the bar too (CSS :hover
            // cannot cross widgets — a class stands in for it).
            let motion = gtk::EventControllerMotion::new();
            {
                let zone = pill.clone();
                motion.connect_enter(move |_, _, _| zone.add_css_class("hot"));
            }
            {
                let zone = pill.clone();
                motion.connect_leave(move |_| zone.remove_css_class("hot"));
            }
            widgets.drawer_edge.add_controller(motion);
            let s = sender.clone();
            let s2 = sender.clone();
            crate::ui::grab_pill::attach_drag(
                &widgets.drawer_edge,
                &root,
                move |_| s.input(AttachmentDrawerInput::PillDragBegin),
                move |want| s2.input(AttachmentDrawerInput::PillDrag { want }),
            );
        }
        // HYLKI_SHOWCASE_DRAWER=N clicks the header's count button N times,
        // one a second from 6s — the real button press, so the capture proves
        // the click path and not just the input it sends.
        if let Some(Ok(n)) = std::env::var("HYLKI_SHOWCASE_DRAWER").ok().map(|v| v.parse::<u32>()) {
            let btn = widgets.count_btn.clone();
            for i in 0..n {
                let btn = btn.clone();
                glib::timeout_add_seconds_local_once(6 + i, move || btn.emit_clicked());
            }
        }
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            AttachmentDrawerInput::SetItems(items) => {
                let became_visible = self.items.is_empty() && !items.is_empty();
                self.items = items;
                self.update_pill_visibility();
                self.rebuild(&sender);
                // Apply the remembered split the first time the drawer appears
                // (the Paned is allocated only once both children are visible).
                if became_visible && !self.positioned.get() {
                    self.schedule_initial_position();
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
            AttachmentDrawerInput::PositionChanged => {
                if self.adjusting.get() || !self.positioned.get() {
                    return;
                }
                // Collapsed = not resizable: a drag on the (invisible) divider
                // snaps straight back to the header-only split. Expanding via
                // the chevron is what re-enables resizing.
                if self.collapsed {
                    self.apply_position();
                    return;
                }
                let drawer_h = self.paned.height() - self.paned.position();
                if drawer_h >= MIN_HEIGHT {
                    self.height = drawer_h;
                    self.schedule_height_save();
                }
            }
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
            AttachmentDrawerInput::ImportKey(i) => {
                if let Some(att) = self.item_at(i) {
                    let (title, body) = match crate::pgp::import_keys(&crate::pgp::Gpg::system(), &att.data) {
                        Ok(s) if s.imported > 0 => (
                            i18n("Key imported"),
                            i18n("The key is in your keyring now. Trust it under Settings, OpenPGP, once you have checked its fingerprint with its owner."),
                        ),
                        Ok(_) => (i18n("Already in your keyring"), i18n("That key was imported before.")),
                        Err(e) => (i18n("Could not import the key"), e),
                    };
                    let dialog = adw::MessageDialog::new(self.window().as_ref(), Some(&title), Some(&body));
                    dialog.add_response("ok", &i18n("OK"));
                    dialog.present();
                }
            }
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
            AttachmentDrawerInput::Activate(i) => {
                // Single clicks do nothing at all (a first click must never
                // steal the second — a modal lightbox on click one made the
                // double-click unreachable). Double-click previews images and
                // PDFs in the lightbox, whose Open button launches the default
                // app; anything else opens externally at once.
                let Some(orig) = self.display_order.get(i).copied() else { return };
                let Some(att) = self.items.get(orig) else { return };
                if previewable(att) {
                    self.show_lightbox(orig, &sender);
                } else {
                    open_bytes(&att.name, &att.data, self.window().as_ref());
                }
            }
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
            AttachmentDrawerInput::PillDrag { want } => {
                self.pill_dragged = true;
                if self.collapsed {
                    // A real drag pulls the drawer straight out of the
                    // collapse and follows the pointer from the seam; where
                    // it is released is the drawer's new height (recorded by
                    // position-notify like any other resize).
                    self.collapsed = false;
                    config::save_drawer_collapsed(self.collapsed);
                }
                // The pill moves the divider live, exactly like a drag on the
                // (invisible) separator. The height is recorded here directly
                // (as well as by position-notify) so a drag is remembered
                // whatever else is in flight when the divider settles.
                self.paned.set_position(want.max(0));
                let drawer_h = self.paned.height() - want.max(0);
                if drawer_h >= MIN_HEIGHT {
                    self.height = drawer_h;
                    self.schedule_height_save();
                }
            }
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
                // A skipped slide's settle arrives late; its work was already
                // done synchronously when it was superseded.
                if gen != self.toggle_gen {
                    return;
                }
                // Order matters: flip the state first (the view update right
                // after this hides the body, shrinking the drawer's minimum),
                // THEN allow the end child its minimum again — the other way
                // round the Paned jumps the divider to fit the still-visible
                // body before the settle pulls it back.
                if collapsed {
                    self.settle_collapsed();
                }
                self.paned.set_shrink_end_child(false);
                self.adjusting.set(false);
            }
        }
    }
}

impl AttachmentDrawer {
    /// The top-level window this drawer lives in (for modal children / dialogs).
    fn window(&self) -> Option<gtk::Window> {
        self.flow.root().and_downcast::<gtk::Window>()
    }

    /// The grab pill shows whenever the drawer does — collapsed included,
    /// where a click or a drag brings the drawer back out. Only a message
    /// with no attachments (no drawer, no seam) hides it.
    fn update_pill_visibility(&self) {
        self.pill.set_visible(!self.items.is_empty());
    }

    /// Flip collapsed/expanded — a plain click on the grab pill — sliding the
    /// divider there like the split reply's entrance rather than jumping.
    ///
    /// Expanding shows the body at once so it rides the slide open; the
    /// collapse keeps the body visible for the ride down and hides it when
    /// the slide settles (`ToggleSettled`), else the thumbnails would vanish
    /// a beat before the panel. Either way the drawer may momentarily be
    /// squeezed below its minimum, so end-child shrink is allowed for the
    /// duration.
    fn toggle_collapsed(&mut self, sender: &ComponentSender<Self>) {
        self.finish_running_toggle();
        let total = self.paned.height();
        if total <= 1 {
            // Not allocated (never shown): the old instant flip.
            self.collapsed = !self.collapsed;
            self.apply_position();
            config::save_drawer_collapsed(self.collapsed);
            return;
        }
        let closing = !self.collapsed;
        if !closing {
            self.collapsed = false;
        }
        config::save_drawer_collapsed(closing);
        let target_h = if closing { Self::header_height(&self.paned) } else { self.height };

        let paned = self.paned.clone();
        // The whole ride is programmatic — position-notify must not read it
        // as a user drag.
        self.adjusting.set(true);
        paned.set_shrink_end_child(true);
        let anim = adw::TimedAnimation::new(
            &paned,
            paned.position() as f64,
            (total - target_h).max(0) as f64,
            300,
            adw::CallbackAnimationTarget::new({
                let paned = paned.clone();
                move |v| paned.set_position(v as i32)
            }),
        );
        anim.set_easing(adw::Easing::EaseOutCubic);
        self.toggle_gen += 1;
        let gen = self.toggle_gen;
        let cell = self.toggle_anim.clone();
        let s = sender.clone();
        // NOTHING is restored here: un-shrinking the drawer while its body is
        // still visible makes the Paned jump the divider up to the body's
        // minimum, then the settle snaps it back — a visible flash (and a
        // window where the jump could be recorded as a height). The settle
        // handler restores state in the right order instead.
        anim.connect_done(move |_| {
            cell.borrow_mut().take();
            s.input(AttachmentDrawerInput::ToggleSettled { collapsed: closing, gen });
        });
        *self.toggle_anim.borrow_mut() = Some((anim.clone(), closing));
        anim.play();
    }

    /// If an expand/collapse slide is running, jump it to its end and settle
    /// its target state synchronously. The `ToggleSettled` it queues on the
    /// way out is voided by bumping the generation.
    fn finish_running_toggle(&mut self) {
        let running = self.toggle_anim.borrow_mut().take();
        let Some((anim, closing)) = running else { return };
        self.toggle_gen += 1;
        anim.skip();
        if closing {
            self.settle_collapsed();
        }
        self.paned.set_shrink_end_child(false);
        self.adjusting.set(false);
    }

    /// The end state of a collapse: body hidden, divider locked, position
    /// pinned to exactly the header.
    fn settle_collapsed(&mut self) {
        self.collapsed = true;
        self.apply_position();
    }

    /// Natural height of the drawer's header row (the first child of the end
    /// pane) — the drawer's height when collapsed.
    fn header_height(paned: &gtk::Paned) -> i32 {
        // The collapsed drawer keeps everything above the scroller: the edge
        // strip and the header row.
        let Some(body) = paned.end_child() else { return 44 };
        let mut sum = 0;
        let mut child = body.first_child();
        while let Some(c) = child {
            if c.is::<gtk::ScrolledWindow>() {
                break;
            }
            sum += c.measure(gtk::Orientation::Vertical, -1).1;
            child = c.next_sibling();
        }
        if sum > 0 { sum } else { 44 }
    }

    /// Move the Paned divider so the drawer body is the desired height: the
    /// header alone when collapsed, else `self.height`. Guarded so the resulting
    /// position-notify isn't taken for a user drag.
    fn apply_position(&self) {
        let total = self.paned.height();
        if total <= 1 {
            return; // not allocated yet
        }
        let drawer_h = if self.collapsed {
            Self::header_height(&self.paned)
        } else {
            self.height
        };
        self.adjusting.set(true);
        self.paned.set_position((total - drawer_h).max(0));
        self.adjusting.set(false);
    }

    /// Make the Paned's own separator inert (there is no lock API on
    /// GtkPaned, so this walks its internal children and untargets the child
    /// that is neither pane). Called once at init and left off for good: the
    /// separator's enlarged input region otherwise steals presses near the
    /// seam and wears its own cursor. The pill zone and the drawer's edge
    /// strip do all the clicking and dragging.
    fn set_divider_draggable(&self, on: bool) {
        let start = self.paned.start_child();
        let end = self.paned.end_child();
        let mut child = self.paned.first_child();
        while let Some(c) = child {
            if Some(&c) != start.as_ref() && Some(&c) != end.as_ref() {
                c.set_can_target(on);
            }
            child = c.next_sibling();
        }
    }

    /// Persist the dragged height, debounced past the end of the drag.
    fn schedule_height_save(&self) {
        let height = self.height;
        let slot = self.height_save.clone();
        if let Some(prev) = slot.borrow_mut().take() {
            prev.remove();
        }
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(400), {
            let slot = slot.clone();
            move || {
                slot.borrow_mut().take();
                config::save_drawer_height(height);
            }
        });
        *slot.borrow_mut() = Some(id);
    }

    /// Apply the remembered split once the Paned has been allocated. The drawer
    /// has just become visible, so defer one loop iteration for layout to settle.
    fn schedule_initial_position(&self) {
        let paned = self.paned.clone();
        let adjusting = self.adjusting.clone();
        let positioned = self.positioned.clone();
        let height = self.height;
        let collapsed = self.collapsed;
        glib::idle_add_local_once(move || {
            if positioned.get() {
                return;
            }
            let total = paned.height();
            if total <= 1 {
                return; // still unallocated; a later SetItems will retry
            }
            let drawer_h = if collapsed { Self::header_height(&paned) } else { height };
            adjusting.set(true);
            paned.set_position((total - drawer_h).max(0));
            adjusting.set(false);
            positioned.set(true);
        });
    }

    /// Rebuild the thumbnail grid from the current items and thumbnail size.
    fn rebuild(&mut self, sender: &ComponentSender<Self>) {
        // Remove existing cells only, skipping anything that isn't a
        // FlowBoxChild — and walk siblings captured before removal so this can
        // never loop (removing a non-child is a no-op, which would spin
        // `first_child()` forever).
        let mut child = self.flow.first_child();
        while let Some(c) = child {
            let next = c.next_sibling();
            if c.downcast_ref::<gtk::FlowBoxChild>().is_some() {
                self.flow.remove(&c);
            }
            child = next;
        }
        // The grid keeps the message's own attachment order; the list is
        // alphabetical (case-insensitive), flipped by the sort switch. Cells are
        // handed their *display* index — `item_at` maps it back to `items`.
        let mut order: Vec<usize> = (0..self.items.len()).collect();
        if self.list_view {
            order.sort_by(|&a, &b| {
                self.items[a].name.to_lowercase().cmp(&self.items[b].name.to_lowercase())
            });
            if self.sort_desc {
                order.reverse();
            }
        }
        self.display_order = order;
        for (i, &orig) in self.display_order.iter().enumerate() {
            let att = &self.items[orig];
            let cell = if self.list_view {
                build_list_row(i, att, sender)
            } else {
                build_cell(i, att, self.thumb, sender)
            };
            self.flow.insert(&cell, -1);
        }
    }

    /// The attachment shown at display position `i` (grid or sorted list).
    fn item_at(&self, i: usize) -> Option<&Attachment> {
        self.items.get(*self.display_order.get(i)?)
    }

    /// Save one attachment via a file chooser.
    fn save_attachment(&self, att: &Attachment) {
        let dialog = gtk::FileDialog::builder()
            .initial_name(&att.name)
            .title(&i18n("Save Attachment"))
            .build();
        let data = att.data.clone();
        dialog.save(self.window().as_ref(), gtk::gio::Cancellable::NONE, move |res| {
            if let Ok(file) = res {
                if let Some(path) = file.path() {
                    let _ = std::fs::write(path, &data);
                }
            }
        });
    }

    /// Ask for a folder and write every attachment into it ("Save All").
    fn save_all(&self) {
        if self.items.is_empty() {
            return;
        }
        let atts = self.items.clone();
        let dialog = gtk::FileDialog::builder().title(&i18n("Save All Attachments")).build();
        dialog.select_folder(self.window().as_ref(), gtk::gio::Cancellable::NONE, move |res| {
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

    /// Right-click menu: Open / Download, matching the gallery's actions.
    fn show_context_menu(&self, index: usize, x: f64, y: f64, sender: &ComponentSender<Self>) {
        let s = sender.clone();
        let open = MenuEntry::new(i18n("Open"), move || s.input(AttachmentDrawerInput::Open(index)))
            .icon("co.hyprlab.Hylki-document-open-symbolic");
        let s = sender.clone();
        let download =
            MenuEntry::new(i18n("Download…"), move || s.input(AttachmentDrawerInput::Download(index)))
                .icon("co.hyprlab.Hylki-folder-download-symbolic");
        // The drawer gathers the whole conversation's files; this finds the
        // message a file came with (#213).
        let s = sender.clone();
        let show = MenuEntry::new(i18n("Show in Message"), move || {
            s.input(AttachmentDrawerInput::ShowInMessage(index))
        })
        .icon("co.hyprlab.Hylki-mail-unread-symbolic");

        // Anchor on the clicked cell itself so the click point (already
        // relative to it) needs no coordinate translation.
        let parent: gtk::Widget = self
            .flow
            .child_at_index(index as i32)
            .map(|c| c.upcast())
            .unwrap_or_else(|| self.flow.clone().upcast());
        show_context_menu(&parent, x, y, vec![vec![open, download], vec![show]]);
    }

    /// Ask the app to show its full-window lightbox over the message's
    /// previewable attachments (images and PDFs), starting at `start`
    /// (attachments order). The overlay lives at the window level so it fills
    /// Hylki itself — a separate preview window meant double chrome.
    fn show_lightbox(&self, start: usize, sender: &ComponentSender<Self>) {
        let items: Vec<Attachment> =
            self.items.iter().filter(|a| previewable(a)).cloned().collect();
        if items.is_empty() {
            return;
        }
        let start_pos = self
            .items
            .iter()
            .take(start + 1)
            .filter(|a| previewable(a))
            .count()
            .saturating_sub(1);
        let _ = sender.output(DrawerOutput::ShowLightbox { items, start: start_pos });
    }
}

/// Build one grid cell: a square thumbnail (image/PDF texture or type icon)
/// with hover Download/Open actions and the filename beneath it.
/// Whether an attachment is an OpenPGP public key: by its armour, or by a
/// key file's usual name.
fn is_key_attachment(att: &Attachment) -> bool {
    let head = &att.data[..att.data.len().min(64)];
    if head.starts_with(b"-----BEGIN PGP PUBLIC KEY BLOCK-----") {
        return true;
    }
    let name = att.name.to_ascii_lowercase();
    (name.ends_with(".asc") || name.ends_with(".gpg") || name.ends_with(".pgp") || name.ends_with(".key"))
        && !head.starts_with(b"-----BEGIN PGP MESSAGE-----")
        && !head.starts_with(b"-----BEGIN PGP SIGNATURE-----")
}

fn build_cell(
    index: usize,
    att: &Attachment,
    thumb: i32,
    sender: &ComponentSender<AttachmentDrawer>,
) -> gtk::FlowBoxChild {
    let cell = gtk::Box::new(gtk::Orientation::Vertical, 4);
    cell.add_css_class("drawer-cell");
    cell.set_halign(gtk::Align::Center);

    // The thumbnail content: a cover-cropped image/PDF-page render, or a
    // centred type icon. Anything not yet in the render cache gets a spinner
    // while a worker decodes it — never a frozen window.
    let content: gtk::Widget = match thumbnail_texture(&att.name, &att.data) {
        Thumbnail::Ready(tex) => drawer_picture(&tex),
        Thumbnail::Fallback => drawer_icon(&att.name, thumb),
        Thumbnail::Pending => {
            let holder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            holder.set_hexpand(true);
            holder.set_vexpand(true);
            holder.append(&crate::ui::attachments_gallery::thumbnail_spinner());
            let weak = holder.downgrade();
            let name = att.name.clone();
            spawn_thumbnail_render(&att.name, att.data.clone(), move |tex| {
                let Some(holder) = weak.upgrade() else { return };
                while let Some(child) = holder.first_child() {
                    holder.remove(&child);
                }
                match tex {
                    Some(tex) => holder.append(&drawer_picture(&tex)),
                    None => holder.append(&drawer_icon(&name, thumb)),
                }
            });
            holder.upcast()
        }
    };

    // Force a fixed 1:1 square regardless of the image's intrinsic size, so cells
    // stay uniform and the grid flows left-to-right, wrapping into rows.
    let square = SquareBox::new(&content, thumb);
    square.set_overflow(gtk::Overflow::Hidden);
    square.add_css_class("drawer-thumb");

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&square));

    // Hover quick actions (Download, Open) — same style as the gallery.
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    actions.set_valign(gtk::Align::End);
    actions.set_margin_end(6);
    actions.set_margin_bottom(6);
    let action_btn = |icon: &str, tip: &str| {
        let b = gtk::Button::from_icon_name(icon);
        b.add_css_class("gallery-open");
        b.add_css_class("drawer-action");
        b.add_css_class("circular");
        b.add_css_class("osd");
        b.set_tooltip_text(Some(tip));
        b
    };
    let download = action_btn("co.hyprlab.Hylki-folder-download-symbolic", "Download");
    let s = sender.clone();
    download.connect_clicked(move |_| s.input(AttachmentDrawerInput::Download(index)));
    actions.append(&download);
    let open = action_btn("co.hyprlab.Hylki-document-open-symbolic", "Open");
    let s = sender.clone();
    open.connect_clicked(move |_| s.input(AttachmentDrawerInput::Open(index)));
    actions.append(&open);
    // An attached public key (#133): one click puts it in the keyring.
    if is_key_attachment(att) {
        let import = action_btn("co.hyprlab.Hylki-channel-secure-symbolic", &i18n("Import OpenPGP key"));
        let s = sender.clone();
        import.connect_clicked(move |_| s.input(AttachmentDrawerInput::ImportKey(index)));
        actions.append(&import);
    }
    overlay.add_overlay(&actions);

    cell.append(&overlay);

    let name = gtk::Label::new(Some(&att.name));
    name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    name.set_max_width_chars(1);
    name.set_width_request(thumb);
    name.set_justify(gtk::Justification::Center);
    name.set_tooltip_text(Some(&att.name));
    name.add_css_class("drawer-filename");
    name.add_css_class("caption");
    cell.append(&name);

    let child = gtk::FlowBoxChild::new();
    child.set_child(Some(&cell));
    // The full filename on hover anywhere in the cell — the label below the
    // thumbnail is ellipsized, so the thumbnail itself must answer too.
    child.set_tooltip_text(Some(&att.name));

    // Right-click → context menu at the click point.
    let right = gtk::GestureClick::new();
    right.set_button(gtk::gdk::BUTTON_SECONDARY);
    let s = sender.clone();
    right.connect_pressed(move |_, _, x, y| {
        s.input(AttachmentDrawerInput::ContextMenu { index, x, y });
    });
    child.add_controller(right);
    add_double_click_open(&child, index, sender);

    child
}

/// Double-click (primary): the drawer's activation gesture — previewable
/// types open the lightbox, everything else its default application.
fn add_double_click_open(
    child: &gtk::FlowBoxChild,
    index: usize,
    sender: &ComponentSender<AttachmentDrawer>,
) {
    let dbl = gtk::GestureClick::new();
    dbl.set_button(gtk::gdk::BUTTON_PRIMARY);
    let s = sender.clone();
    dbl.connect_pressed(move |_, n, _, _| {
        if n == 2 {
            s.input(AttachmentDrawerInput::Activate(index));
        }
    });
    child.add_controller(dbl);
}

/// One row of the drawer's list view: type icon, filename, size, and the same
/// Download/Open quick actions the grid shows on hover. Activation (click) and
/// the right-click context menu match the grid's behaviour.
fn build_list_row(
    index: usize,
    att: &Attachment,
    sender: &ComponentSender<AttachmentDrawer>,
) -> gtk::FlowBoxChild {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("drawer-list-row");

    let icon = gtk::Image::from_icon_name(icon_for(&att.name));
    icon.set_pixel_size(20);
    icon.add_css_class(icon_color_class(&att.name));
    row.append(&icon);

    let name = gtk::Label::new(Some(&att.name));
    name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    name.set_halign(gtk::Align::Start);
    name.set_hexpand(true);
    name.set_xalign(0.0);
    row.append(&name);

    let size = gtk::Label::new(Some(&att.human_size()));
    size.add_css_class("dim-label");
    size.add_css_class("caption");
    size.set_valign(gtk::Align::Center);
    row.append(&size);

    let action_btn = |icon: &str, tip: &str| {
        let b = gtk::Button::from_icon_name(icon);
        b.add_css_class("flat");
        b.set_valign(gtk::Align::Center);
        b.set_tooltip_text(Some(tip));
        b
    };
    let download = action_btn("co.hyprlab.Hylki-folder-download-symbolic", "Download");
    let s = sender.clone();
    download.connect_clicked(move |_| s.input(AttachmentDrawerInput::Download(index)));
    row.append(&download);
    let open = action_btn("co.hyprlab.Hylki-document-open-symbolic", "Open");
    let s = sender.clone();
    open.connect_clicked(move |_| s.input(AttachmentDrawerInput::Open(index)));
    row.append(&open);

    let child = gtk::FlowBoxChild::new();
    child.set_child(Some(&row));
    child.set_tooltip_text(Some(&att.name));

    let right = gtk::GestureClick::new();
    right.set_button(gtk::gdk::BUTTON_SECONDARY);
    let s = sender.clone();
    right.connect_pressed(move |_, _, x, y| {
        s.input(AttachmentDrawerInput::ContextMenu { index, x, y });
    });
    child.add_controller(right);
    add_double_click_open(&child, index, sender);

    child
}

glib::wrapper! {
    /// A single-child container that always measures to a fixed N×N square,
    /// ignoring the child's intrinsic size. This keeps every thumbnail uniformly
    /// sized (so images don't blow the cell out to their native pixel width) and
    /// lets the grid pack many per row, flowing left-to-right.
    pub struct SquareBox(ObjectSubclass<imp::SquareBox>) @extends gtk::Widget;
}

impl SquareBox {
    fn new(child: &impl IsA<gtk::Widget>, size: i32) -> Self {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        let obj: Self = glib::Object::new();
        obj.imp().size.set(size);
        child.set_parent(&obj);
        obj
    }
}

mod imp {
    use std::cell::Cell;

    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub struct SquareBox {
        pub size: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SquareBox {
        const NAME: &'static str = "HylkiSquareBox";
        type Type = super::SquareBox;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for SquareBox {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for SquareBox {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::ConstantSize
        }

        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let s = self.size.get().max(1);
            (s, s, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.obj().first_child() {
                child.allocate(width, height, baseline, None);
            }
        }
    }
}
