//! The attachment lightbox: images and PDFs shown over a whole window, from
//! the attachment drawer or a card's attachment row.
//!
//! A component of its own so that a message's own window can have one as
//! well as the main window. Each lays the lightbox over its content with a
//! `gtk::Overlay`; it looks like the gallery's own lightbox (same CSS).

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::{i18n, i18n_f};
use crate::models::Attachment;
use crate::ui::thumbnails;

pub(crate) struct Lightbox {
    items: Vec<Attachment>,
    pos: usize,
    texture: Option<gtk::gdk::Texture>,
    /// The current item has nothing to show: a PDF poppler cannot open, or
    /// an image that does not decode.
    no_preview: bool,
    /// Current zoom (1 or 3): a click on the document toggles it, Escape
    /// unwinds it before closing.
    zoom: i32,
    picture: gtk::Picture,
    scroller: gtk::ScrolledWindow,
    /// The lightbox itself: zoomed, the scroller takes all of it.
    root: gtk::Box,
    /// Whether the window's key handler has been added: once, the first
    /// time the lightbox opens, when the window it sits in is known.
    keys_added: bool,
    /// Mirror of "the lightbox is open", read by that key handler.
    open: Rc<Cell<bool>>,
}

#[derive(Debug)]
pub(crate) enum LightboxInput {
    /// Show these previewable attachments, starting at `start`.
    Show {
        items: Vec<Attachment>,
        start: usize,
    },
    Prev,
    Next,
    Close,
    /// Click on the document: toggle zoom 1x ↔ 3x, anchored at the clicked
    /// point (picture coordinates at the fitted size).
    ZoomCycle {
        x: f64,
        y: f64,
    },
    /// Escape: unwind zoom first; close only from normal view.
    Escape,
    /// Open the shown item in its default application.
    OpenCurrent,
    /// Save the shown item via a file chooser.
    DownloadCurrent,
    /// Print the shown item: Ctrl+P while the lightbox is open (#632).
    PrintCurrent,
    /// A full-size PDF render finished (content hash, and the page unless
    /// it failed): show it if that item is still on screen.
    Rendered(u64, Option<gtk::gdk::Texture>),
}

#[relm4::component(pub(crate))]
impl Component for Lightbox {
    type Init = ();
    type Input = LightboxInput;
    type Output = ();
    type CommandOutput = ();

    view! {
        gtk::Box {
            add_css_class: "gallery-lightbox",
            set_orientation: gtk::Orientation::Vertical,
            #[watch]
            set_visible: !model.items.is_empty(),
            // Zoomed, the bars, arrows and padding step aside and the
            // document fills the window.
            #[watch]
            set_class_active: ("zoomed", model.zoom != 1),

            gtk::CenterBox {
                add_css_class: "gallery-lightbox-bar",
                #[watch]
                set_visible: model.zoom == 1,
                #[wrap(Some)]
                set_start_widget = &gtk::Label {
                    #[watch]
                    set_label: model.items.get(model.pos).map(|a| a.name.as_str()).unwrap_or(""),
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
                    connect_clicked => LightboxInput::Close,
                },
            },

            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_vexpand: true,
                #[watch]
                set_spacing: if model.zoom == 1 { 8 } else { 0 },

                gtk::Button {
                    set_icon_name: "go-previous-symbolic",
                    set_tooltip_text: Some(i18n("Previous").as_str()),
                    set_valign: gtk::Align::Center,
                    add_css_class: "circular",
                    add_css_class: "osd",
                    #[watch]
                    set_visible: model.items.len() > 1 && model.zoom == 1,
                    connect_clicked => LightboxInput::Prev,
                },

                gtk::Stack {
                    set_hexpand: true,
                    set_vexpand: true,
                    #[watch]
                    set_visible_child_name: if model.texture.is_some() {
                        "image"
                    } else if model.no_preview {
                        "no-preview"
                    } else {
                        "rendering"
                    },

                    #[name = "scroller"]
                    add_named[Some("image")] = &gtk::ScrolledWindow {
                        set_hscrollbar_policy: gtk::PolicyType::Automatic,
                        set_vscrollbar_policy: gtk::PolicyType::Automatic,
                        set_hexpand: true,
                        set_vexpand: true,

                        #[name = "picture"]
                        gtk::Picture {
                            set_can_shrink: true,
                            set_content_fit: gtk::ContentFit::Contain,
                            #[watch]
                            set_paintable: model.texture.as_ref(),
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

                    add_named[Some("no-preview")] = &gtk::Box {
                        add_css_class: "gallery-lightbox-file",
                        set_orientation: gtk::Orientation::Vertical,
                        set_halign: gtk::Align::Center,
                        set_valign: gtk::Align::Center,
                        set_spacing: 12,
                        gtk::Image {
                            #[watch]
                            set_icon_name: model.items.get(model.pos).map(|a| thumbnails::icon_for(&a.name)),
                            set_pixel_size: 96,
                            #[watch]
                            set_css_classes: &[
                                "gallery-file-icon",
                                model.items.get(model.pos).map(|a| thumbnails::icon_color_class(&a.name)).unwrap_or("ftype-generic"),
                            ],
                        },
                        gtk::Label {
                            set_label: &i18n("No preview available"),
                            add_css_class: "title-3",
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
                    set_visible: model.items.len() > 1 && model.zoom == 1,
                    connect_clicked => LightboxInput::Next,
                },
            },

            gtk::CenterBox {
                add_css_class: "gallery-lightbox-bar",
                #[watch]
                set_visible: model.zoom == 1,
                #[wrap(Some)]
                set_start_widget = &gtk::Label {
                    #[watch]
                    set_label: &model.caption(),
                    set_halign: gtk::Align::Start,
                    set_ellipsize: gtk::pango::EllipsizeMode::End,
                    add_css_class: "dim-label",
                },
                #[wrap(Some)]
                set_end_widget = &gtk::Box {
                    set_spacing: 6,
                    gtk::Button {
                        set_icon_name: "document-open-symbolic",
                        set_tooltip_text: Some(i18n("Open").as_str()),
                        add_css_class: "flat",
                        connect_clicked => LightboxInput::OpenCurrent,
                    },
                    gtk::Button {
                        set_icon_name: "printer-symbolic",
                        set_tooltip_text: Some(i18n("Print…").as_str()),
                        add_css_class: "flat",
                        #[watch]
                        set_sensitive: model.printable(),
                        connect_clicked => LightboxInput::PrintCurrent,
                    },
                    gtk::Button {
                        set_icon_name: "folder-download-symbolic",
                        set_tooltip_text: Some(i18n("Download…").as_str()),
                        add_css_class: "flat",
                        connect_clicked => LightboxInput::DownloadCurrent,
                    },
                },
            },
        }
    }

    fn init(_: (), root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let mut model = Lightbox {
            items: Vec::new(),
            pos: 0,
            texture: None,
            no_preview: false,
            zoom: 1,
            picture: gtk::Picture::new(),
            scroller: gtk::ScrolledWindow::new(),
            root: root.clone(),
            keys_added: false,
            open: Rc::new(Cell::new(false)),
        };
        let widgets = view_output!();
        model.picture = widgets.picture.clone();
        model.scroller = widgets.scroller.clone();

        // Pointer behaviour: dragging pans the zoomed document (the
        // scroller's adjustments move opposite the pointer); a clean click,
        // a release with no meaningful movement, cycles the zoom. The shared
        // `moved` cell is what keeps a pan from also zooming.
        {
            let hadj = model.scroller.hadjustment();
            let vadj = model.scroller.vadjustment();
            let start = Rc::new(Cell::new((0.0_f64, 0.0_f64)));
            let moved = Rc::new(Cell::new(0.0_f64));

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
            // (scroll, shift, un-scroll) and the drag jitters. The scroller
            // stays put, so its offsets are stable.
            model.scroller.add_controller(drag);

            let click = gtk::GestureClick::new();
            click.set_button(gtk::gdk::BUTTON_PRIMARY);
            let s = sender.clone();
            click.connect_released(move |_, n, x, y| {
                if n == 1 && moved.get() < 8.0 {
                    s.input_sender().emit(LightboxInput::ZoomCycle { x, y });
                }
            });
            model.picture.add_controller(click);
        }

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: LightboxInput, sender: ComponentSender<Self>, root: &Self::Root) {
        match msg {
            LightboxInput::Show { items, start } => {
                if items.is_empty() {
                    return;
                }
                self.add_keys(root, &sender);
                self.pos = start.min(items.len() - 1);
                self.items = items;
                self.open.set(true);
                self.set_zoom(1);
                self.refresh(&sender);
            }
            LightboxInput::Prev => self.step(-1, &sender),
            LightboxInput::Next => self.step(1, &sender),
            LightboxInput::Close => {
                self.items.clear();
                self.show_page(None);
                self.open.set(false);
                self.set_zoom(1);
            }
            LightboxInput::ZoomCycle { x, y } => {
                if self.zoom == 1 {
                    self.zoom_to_point(x, y);
                } else {
                    self.set_zoom(1);
                }
            }
            LightboxInput::Escape => {
                // Zoomed in, Escape returns to the fitted view; from there it
                // closes the lightbox.
                if self.zoom != 1 {
                    self.set_zoom(1);
                } else {
                    sender.input_sender().emit(LightboxInput::Close);
                }
            }
            LightboxInput::OpenCurrent => {
                if let Some(att) = self.items.get(self.pos) {
                    let window = root.root().and_downcast::<gtk::Window>();
                    crate::ui::attachment_io::open_bytes(&att.name, &att.data, window.as_ref());
                }
            }
            LightboxInput::DownloadCurrent => {
                if let Some(att) = self.items.get(self.pos).cloned() {
                    let window = root.root().and_downcast::<gtk::Window>();
                    // A failed write is dropped.
                    crate::ui::attachment_io::save_attachment_as(
                        &att.name,
                        att.data,
                        window.as_ref(),
                        |_| {},
                    );
                }
            }
            LightboxInput::PrintCurrent => {
                if let Some(att) = self.items.get(self.pos) {
                    let window = root.root().and_downcast::<gtk::Window>();
                    crate::ui::print_preview::print_attachment(&att.name, &att.data, window);
                }
            }
            LightboxInput::Rendered(key, page) => {
                let still_current = self
                    .items
                    .get(self.pos)
                    .is_some_and(|a| thumbnails::content_key(&a.data) == key);
                if still_current {
                    self.show_page(page);
                }
            }
        }
    }
}

impl Lightbox {
    /// Whether the lightbox covers its window: the window's Ctrl+P then
    /// prints the attachment rather than the message under it.
    pub(crate) fn is_open(&self) -> bool {
        !self.items.is_empty()
    }

    /// An image that does not decode has nothing to print; a PDF poppler
    /// cannot open may still print, as the printer reads it, not poppler.
    fn printable(&self) -> bool {
        self.items
            .get(self.pos)
            .is_some_and(|a| !(self.no_preview && crate::models::is_image_name(&a.name)))
    }

    /// Escape and the arrow keys drive the lightbox from anywhere in its
    /// window: capture phase, and only while it is open, so typing
    /// elsewhere is untouched.
    fn add_keys(&mut self, root: &gtk::Box, sender: &ComponentSender<Self>) {
        if self.keys_added {
            return;
        }
        let Some(window) = root.root().and_downcast::<gtk::Window>() else {
            return;
        };
        self.keys_added = true;
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let open = self.open.clone();
        let s = sender.input_sender().clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if !open.get() {
                return gtk::glib::Propagation::Proceed;
            }
            let msg = match key {
                gtk::gdk::Key::Escape => LightboxInput::Escape,
                gtk::gdk::Key::Left => LightboxInput::Prev,
                gtk::gdk::Key::Right => LightboxInput::Next,
                _ => return gtk::glib::Propagation::Proceed,
            };
            let _ = s.send(msg);
            gtk::glib::Propagation::Stop
        });
        window.add_controller(keys);
    }

    /// "name · n of m" for the bottom bar.
    fn caption(&self) -> String {
        match self.items.get(self.pos) {
            Some(att) => i18n_f(
                "{name} · {current} of {total}",
                &[
                    ("name", &att.name),
                    ("current", &(self.pos + 1).to_string()),
                    ("total", &self.items.len().to_string()),
                ],
            ),
            None => String::new(),
        }
    }

    fn step(&mut self, delta: i32, sender: &ComponentSender<Self>) {
        let n = self.items.len() as i32;
        if n == 0 {
            return;
        }
        self.pos = (((self.pos as i32 + delta) % n + n) % n) as usize;
        self.set_zoom(1);
        self.refresh(sender);
    }

    /// Zoom to 3x anchored at `(x, y)`, the clicked point in the fitted
    /// picture's coordinates. The zoomed scroller is the whole lightbox,
    /// not the fitted one between the bars, so the scale is not exactly 3:
    /// the click is turned into a point on the document first, and once the
    /// resize has been laid out (the scroller's range exists) the
    /// adjustments put that point at the viewport's centre. Without this the
    /// view stayed at the top-left of the grown, mostly-letterboxed box, the
    /// content apparently shoved off-screen.
    fn zoom_to_point(&mut self, x: f64, y: f64) {
        let Some((tw, th)) = self
            .texture
            .as_ref()
            .map(|t| (f64::from(t.width()), f64::from(t.height())))
        else {
            return;
        };
        if tw <= 0.0 || th <= 0.0 {
            return;
        }
        // Where the click lands on the document, 0..1 each way.
        let fit = move |w: f64, h: f64| {
            let s = (w / tw).min(h / th);
            (s, (w - tw * s) / 2.0, (h - th * s) / 2.0)
        };
        let (s0, ox0, oy0) = fit(
            f64::from(self.picture.width()),
            f64::from(self.picture.height()),
        );
        let u = ((x - ox0) / (tw * s0)).clamp(0.0, 1.0);
        let v = ((y - oy0) / (th * s0)).clamp(0.0, 1.0);

        self.set_zoom(3);
        let hadj = self.scroller.hadjustment();
        let vadj = self.scroller.vadjustment();
        let tries = Cell::new(0u8);
        self.picture.add_tick_callback(move |_, _| {
            let laid_out =
                hadj.upper() > hadj.page_size() + 1.0 || vadj.upper() > vadj.page_size() + 1.0;
            if laid_out {
                let (s1, ox1, oy1) = fit(hadj.upper(), vadj.upper());
                hadj.set_value(ox1 + u * tw * s1 - hadj.page_size() / 2.0);
                vadj.set_value(oy1 + v * th * s1 - vadj.page_size() / 2.0);
                return gtk::glib::ControlFlow::Break;
            }
            tries.set(tries.get() + 1);
            if tries.get() > 30 {
                return gtk::glib::ControlFlow::Break;
            }
            gtk::glib::ControlFlow::Continue
        });
    }

    /// At 1x the picture fits its scroller; at 3x its box grows to that
    /// multiple of the whole lightbox, which the scroller fills once the
    /// bars have gone (Contain keeps the aspect), and the scroller pans the
    /// overflow.
    fn set_zoom(&mut self, zoom: i32) {
        self.zoom = zoom;
        if zoom <= 1 {
            self.picture.set_size_request(-1, -1);
        } else {
            self.picture
                .set_size_request(self.root.width() * zoom, self.root.height() * zoom);
        }
    }

    /// The outcome for the current item; no page at all means it has none
    /// to show, which is not the same as one still on its way.
    fn show_page(&mut self, page: Option<gtk::gdk::Texture>) {
        self.no_preview = page.is_none();
        self.texture = page;
    }

    /// Work out the texture for the current item: images decode on the
    /// spot; a PDF's page comes from the shared full-size cache or a worker
    /// render that circles back via [`LightboxInput::Rendered`].
    fn refresh(&mut self, sender: &ComponentSender<Self>) {
        self.texture = None;
        self.no_preview = false;
        let Some(att) = self.items.get(self.pos) else {
            return;
        };
        if crate::models::is_image_name(&att.name) {
            let image = thumbnails::texture_from(&att.data);
            self.show_page(image);
            return;
        }
        // The result travels in the message and does not come back through
        // here: a render that answered this function again looped for ever on
        // a PDF that fails, with the window frozen (#582).
        if let Some(page) = thumbnails::cached_pdf_preview(&att.data) {
            self.show_page(page);
            return;
        }
        let key = thumbnails::content_key(&att.data);
        let s = sender.input_sender().clone();
        thumbnails::lightbox_pdf_texture(&att.data, move |page| {
            let _ = s.send(LightboxInput::Rendered(key, page));
        });
    }
}
