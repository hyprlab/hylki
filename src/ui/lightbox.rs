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

pub struct Lightbox {
    items: Vec<Attachment>,
    pos: usize,
    texture: Option<gtk::gdk::Texture>,
    /// Current zoom (1 or 3): a click on the document toggles it, Escape
    /// unwinds it before closing.
    zoom: i32,
    picture: gtk::Picture,
    scroller: gtk::ScrolledWindow,
    /// Whether the window's key handler has been added: once, the first
    /// time the lightbox opens, when the window it sits in is known.
    keys_added: bool,
    /// Mirror of "the lightbox is open", read by that key handler.
    open: Rc<Cell<bool>>,
}

#[derive(Debug)]
pub enum LightboxInput {
    /// Show these previewable attachments, starting at `start`.
    Show { items: Vec<Attachment>, start: usize },
    Prev,
    Next,
    Close,
    /// Click on the document: toggle zoom 1x ↔ 3x, anchored at the clicked
    /// point (picture coordinates at the fitted size).
    ZoomCycle { x: f64, y: f64 },
    /// Escape: unwind zoom first; close only from normal view.
    Escape,
    /// Open the shown item in its default application.
    OpenCurrent,
    /// Save the shown item via a file chooser.
    DownloadCurrent,
    /// A full-size PDF render finished (content hash): show it if that item
    /// is still on screen.
    Rendered(u64),
}

#[relm4::component(pub)]
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

            gtk::CenterBox {
                add_css_class: "gallery-lightbox-bar",
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
                set_spacing: 8,

                gtk::Button {
                    set_icon_name: "go-previous-symbolic",
                    set_tooltip_text: Some(i18n("Previous").as_str()),
                    set_valign: gtk::Align::Center,
                    add_css_class: "circular",
                    add_css_class: "osd",
                    #[watch]
                    set_visible: model.items.len() > 1,
                    connect_clicked => LightboxInput::Prev,
                },

                gtk::Stack {
                    set_hexpand: true,
                    set_vexpand: true,
                    #[watch]
                    set_visible_child_name: if model.texture.is_some() { "image" } else { "rendering" },

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
                },

                gtk::Button {
                    set_icon_name: "go-next-symbolic",
                    set_tooltip_text: Some(i18n("Next").as_str()),
                    set_valign: gtk::Align::Center,
                    add_css_class: "circular",
                    add_css_class: "osd",
                    #[watch]
                    set_visible: model.items.len() > 1,
                    connect_clicked => LightboxInput::Next,
                },
            },

            gtk::CenterBox {
                add_css_class: "gallery-lightbox-bar",
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
            zoom: 1,
            picture: gtk::Picture::new(),
            scroller: gtk::ScrolledWindow::new(),
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
                    s.input(LightboxInput::ZoomCycle { x, y });
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
                self.texture = None;
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
                    sender.input(LightboxInput::Close);
                }
            }
            LightboxInput::OpenCurrent => {
                if let Some(att) = self.items.get(self.pos) {
                    let window = root.root().and_downcast::<gtk::Window>();
                    crate::ui::attachments_gallery::open_bytes(&att.name, &att.data, window.as_ref());
                }
            }
            LightboxInput::DownloadCurrent => {
                if let Some(att) = self.items.get(self.pos).cloned() {
                    let dialog = gtk::FileDialog::builder()
                        .initial_name(&att.name)
                        .title(&i18n("Save Attachment"))
                        .build();
                    let window = root.root().and_downcast::<gtk::Window>();
                    dialog.save(window.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                        if let Ok(file) = res {
                            if let Some(path) = file.path() {
                                let _ = std::fs::write(path, &att.data);
                            }
                        }
                    });
                }
            }
            LightboxInput::Rendered(key) => {
                let still_current = self
                    .items
                    .get(self.pos)
                    .is_some_and(|a| crate::ui::attachments_gallery::content_key(&a.data) == key);
                if still_current {
                    self.refresh(&sender);
                }
            }
        }
    }
}

impl Lightbox {
    /// Escape and the arrow keys drive the lightbox from anywhere in its
    /// window: capture phase, and only while it is open, so typing
    /// elsewhere is untouched.
    fn add_keys(&mut self, root: &gtk::Box, sender: &ComponentSender<Self>) {
        if self.keys_added {
            return;
        }
        let Some(window) = root.root().and_downcast::<gtk::Window>() else { return };
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
    /// picture's coordinates. The whole box scales uniformly by 3, so the
    /// clicked content sits at exactly (3x, 3y) afterwards; once the resize
    /// has been laid out (the scroller's range exists), the adjustments put
    /// that point at the viewport's centre. Without this the view stayed at
    /// the top-left of the grown, mostly-letterboxed box, the content
    /// apparently shoved off-screen.
    fn zoom_to_point(&mut self, x: f64, y: f64) {
        self.set_zoom(3);
        let hadj = self.scroller.hadjustment();
        let vadj = self.scroller.vadjustment();
        let target_x = x * 3.0 - f64::from(self.scroller.width()) / 2.0;
        let target_y = y * 3.0 - f64::from(self.scroller.height()) / 2.0;
        let tries = Cell::new(0u8);
        self.picture.add_tick_callback(move |_, _| {
            let laid_out = hadj.upper() > hadj.page_size() + 1.0 || vadj.upper() > vadj.page_size() + 1.0;
            if laid_out {
                hadj.set_value(target_x);
                vadj.set_value(target_y);
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
    /// multiple of the viewport (Contain keeps the aspect) and the scroller
    /// pans the overflow.
    fn set_zoom(&mut self, zoom: i32) {
        self.zoom = zoom;
        if zoom <= 1 {
            self.picture.set_size_request(-1, -1);
        } else {
            self.picture
                .set_size_request(self.scroller.width() * zoom, self.scroller.height() * zoom);
        }
    }

    /// Work out the texture for the current item: images decode on the
    /// spot; a PDF's page comes from the shared full-size cache or a worker
    /// render that circles back via [`LightboxInput::Rendered`].
    fn refresh(&mut self, sender: &ComponentSender<Self>) {
        use crate::ui::attachments_gallery as gallery;
        self.texture = None;
        let Some(att) = self.items.get(self.pos) else { return };
        if crate::models::is_image_name(&att.name) {
            self.texture = gallery::texture_from(&att.data);
            return;
        }
        // A cache hit paints immediately (and, crucially, spawns nothing: a
        // hit that re-entered via Rendered would loop forever).
        if let Some(tex) = gallery::cached_pdf_preview(&att.data) {
            self.texture = Some(tex);
            return;
        }
        let key = gallery::content_key(&att.data);
        let s = sender.input_sender().clone();
        gallery::lightbox_pdf_texture(&att.data, move |_| {
            let _ = s.send(LightboxInput::Rendered(key));
        });
    }
}
