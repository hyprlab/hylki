//! The drop surfaces a composer shows while files are dragged over it: one to
//! attach them, one to place pictures in the text, one to upload them to
//! cloud storage and share a link. Each appears only when it can take the
//! files being dragged: the text only takes pictures, and only in a rich
//! message; the cloud only when an account is set up. The main window shows
//! the same three when no composer is open in it, each starting a new
//! message ([`DropContext::NewMessage`]).

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;

use crate::i18n::{i18n, i18n_f, ni18n, ni18n_f};

/// Which surface the files were let go on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropChoice {
    Attach,
    Inline,
    Cloud,
}

/// What the files are dropped into, which is what the cards say and how
/// they are laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropContext {
    /// A composer: the cards stack as rows, the reading pane being tall.
    Composer,
    /// The main window with no composer open: each card starts a new
    /// message, and the cards stand side by side across the wide window.
    NewMessage,
}

const FADE_MS: u32 = 160;
/// The veil's padding (`.drop-layer` in styles.css) and the gap between
/// its summary line and the cards.
const LAYER_PADDING: i32 = 18;
const LAYER_SPACING: i32 = 14;
/// A card's side padding (`.drop-zone`) plus its border.
const CARD_PADDING: i32 = 18;

pub struct DropZones {
    context: DropContext,
    layer: gtk::Box,
    zones: gtk::Box,
    summary: gtk::Label,
    attach: Zone,
    inline: Zone,
    cloud: Zone,
    /// Set by the composer as its format changes: pictures go into the text
    /// of a rich message only.
    allow_inline: Cell<bool>,
    /// The cloud accounts' names, for the upload surface's line; empty
    /// means there is nowhere to upload to.
    cloud_names: RefCell<Vec<String>>,
    /// Up, or on its way up; the layer stays visible a moment longer while
    /// it fades out.
    shown: Cell<bool>,
    fade: RefCell<Option<adw::TimedAnimation>>,
    /// Whether a drag may bring the cards up at all (the main window's
    /// stand down while a composer is open in it).
    gate: RefCell<Option<Box<dyn Fn() -> bool>>>,
    /// Run before the cards come up, to bring what they depend on (the
    /// format a new message starts in, the cloud accounts) up to date.
    refresh: RefCell<Option<Box<dyn Fn(&DropZones)>>>,
}

struct Zone {
    widget: gtk::Box,
    /// The icon beside the words (a row) or above them (a column).
    inner: gtk::Box,
    title: gtk::Label,
    subtitle: gtk::Label,
}

impl DropZones {
    /// Lay the surfaces over `host`'s content (`overlay` is the host's
    /// overlay) and watch `host` for file drags. `on_drop` gets the files
    /// and the surface they landed on.
    pub fn install(
        context: DropContext,
        host: &impl IsA<gtk::Widget>,
        overlay: &gtk::Overlay,
        on_drop: impl Fn(DropChoice, Vec<PathBuf>) + 'static,
    ) -> Rc<Self> {
        let on_drop: Rc<dyn Fn(DropChoice, Vec<PathBuf>)> = Rc::new(on_drop);

        let layer = gtk::Box::new(gtk::Orientation::Vertical, LAYER_SPACING);
        layer.add_css_class("drop-layer");
        layer.set_visible(false);

        let summary = gtk::Label::new(None);
        summary.add_css_class("drop-summary");
        summary.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        layer.append(&summary);

        let zones = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        zones.set_homogeneous(true);
        zones.set_vexpand(true);
        layer.append(&zones);

        let (attach, inline, cloud) = match context {
            DropContext::Composer => (i18n("Attach"), i18n("Insert in Text"), i18n("Upload to Cloud")),
            DropContext::NewMessage => {
                (i18n("Attach to New Message"), i18n("Insert in New Message"), i18n("Share Link in New Message"))
            }
        };
        let attach = Zone::new("mail-attachment-symbolic", &attach);
        let inline = Zone::new("image-x-generic-symbolic", &inline);
        let cloud = Zone::new("cloud-symbolic", &cloud);
        for z in [&attach, &inline, &cloud] {
            zones.append(&z.widget);
        }

        let this = Rc::new(Self {
            context,
            layer: layer.clone(),
            zones,
            summary,
            attach,
            inline,
            cloud,
            allow_inline: Cell::new(true),
            cloud_names: RefCell::new(Vec::new()),
            shown: Cell::new(false),
            fade: RefCell::new(None),
            gate: RefCell::new(None),
            refresh: RefCell::new(None),
        });

        // The veil only shows; the target below does all the taking.
        layer.set_can_target(false);
        overlay.add_overlay(&layer);

        // One target over the whole composer, ahead of everything in it
        // (capture phase), rather than one per card: the files are read
        // once, as the drag comes in, and that one reading both decides
        // the cards and is what the drop delivers. With a target per card,
        // each read the files again when let go, and on some desktops that
        // second read never came back, so the drop never happened. Which
        // card was chosen is worked out from where the files were let go.
        let target = gtk::DropTarget::new(gtk::gdk::FileList::static_type(), gtk::gdk::DragAction::COPY);
        target.set_propagation_phase(gtk::PropagationPhase::Capture);
        target.set_preload(true);
        let host_widget = host.as_ref().clone();
        host_widget.add_css_class("drop-host");

        let weak = Rc::downgrade(&this);
        target.connect_accept(move |_, drop| {
            let open = weak.upgrade().is_some_and(|this| this.gate.borrow().as_ref().is_none_or(|gate| gate()));
            open && drop.formats().contains_type(gtk::gdk::FileList::static_type())
                && drop.actions().contains(gtk::gdk::DragAction::COPY)
        });

        let weak = Rc::downgrade(&this);
        let h = host_widget.clone();
        target.connect_value_notify(move |t| {
            let Some(this) = weak.upgrade() else { return };
            let Some(value) = t.value() else { return };
            let paths = value.get::<gtk::gdk::FileList>().map(|l| file_paths(&l)).unwrap_or_default();
            tracing::debug!("drop cards: {} file(s) dragged over the {:?}", paths.len(), this.context);
            if let Some(refresh) = this.refresh.borrow().as_ref() {
                refresh(&this);
            }
            if !paths.is_empty() {
                this.show(&paths, h.width(), h.height());
            }
        });

        let weak = Rc::downgrade(&this);
        let h = host_widget.clone();
        target.connect_motion(move |_, x, y| {
            if let Some(this) = weak.upgrade() {
                this.hover(this.card_at(&h, x, y));
            }
            gtk::gdk::DragAction::COPY
        });

        let weak = Rc::downgrade(&this);
        target.connect_leave(move |_| {
            if let Some(this) = weak.upgrade() {
                this.hide();
            }
        });

        let weak = Rc::downgrade(&this);
        let h = host_widget.clone();
        target.connect_drop(move |_, value, x, y| {
            let Ok(list) = value.get::<gtk::gdk::FileList>() else { return false };
            let paths = file_paths(&list);
            // Let go before the cards were up, or between them: attached,
            // which is what a drop on the composer always did.
            let choice = weak
                .upgrade()
                .and_then(|this| {
                    let choice = this.shown.get().then(|| this.card_at(&h, x, y)).flatten();
                    this.hide();
                    choice
                })
                .unwrap_or(DropChoice::Attach);
            tracing::info!("drop cards: {} file(s) let go on {choice:?}", paths.len());
            if paths.is_empty() {
                return false;
            }
            on_drop(choice, paths);
            true
        });
        host.add_controller(target);
        this
    }

    /// The card under `(x, y)` in `host`'s coordinates, if one is up there.
    fn card_at(&self, host: &gtk::Widget, x: f64, y: f64) -> Option<DropChoice> {
        [(&self.attach, DropChoice::Attach), (&self.inline, DropChoice::Inline), (&self.cloud, DropChoice::Cloud)]
            .into_iter()
            .filter(|(zone, _)| zone.widget.is_visible())
            .find(|(zone, _)| {
                host.compute_point(&zone.widget, &gtk::graphene::Point::new(x as f32, y as f32))
                    .is_some_and(|p| zone.widget.contains(p.x() as f64, p.y() as f64))
            })
            .map(|(_, choice)| choice)
    }

    /// Light the card under the pointer the way GTK lights a drop target
    /// that would take the drop (`:drop(active)` in the stylesheet).
    fn hover(&self, choice: Option<DropChoice>) {
        for (zone, c) in [(&self.attach, DropChoice::Attach), (&self.inline, DropChoice::Inline), (&self.cloud, DropChoice::Cloud)] {
            if choice == Some(c) {
                zone.widget.set_state_flags(gtk::StateFlags::DROP_ACTIVE, false);
            } else {
                zone.widget.unset_state_flags(gtk::StateFlags::DROP_ACTIVE);
            }
        }
    }

    pub fn set_allow_inline(&self, on: bool) {
        self.allow_inline.set(on);
    }

    pub fn set_cloud_names(&self, names: Vec<String>) {
        *self.cloud_names.borrow_mut() = names;
    }

    /// Bring the cards up only while `gate` says so.
    pub fn set_gate(&self, gate: impl Fn() -> bool + 'static) {
        *self.gate.borrow_mut() = Some(Box::new(gate));
    }

    /// Run `refresh` each time the cards are about to come up.
    pub fn set_refresh(&self, refresh: impl Fn(&DropZones) + 'static) {
        *self.refresh.borrow_mut() = Some(Box::new(refresh));
    }

    /// HYLKI_SHOWCASE_DROP_ZONES: bring the surfaces up for `paths` as a
    /// drag would, with `hover`'s card marked as under the pointer, since
    /// no drag can be made from a script.
    pub fn showcase(&self, paths: &[PathBuf], host: &gtk::Widget, hover: Option<DropChoice>) {
        self.show(paths, host.width(), host.height());
        // A capture's window is never drawn on screen, so its animations
        // do not advance: the end of the fade is what there is to see.
        if let Some(fade) = self.fade.borrow().as_ref() {
            fade.skip();
        }
        self.hover(hover);
    }

    fn show(&self, paths: &[PathBuf], width: i32, height: i32) {
        let n = paths.len() as u32;
        let pictures = paths.iter().filter(|p| crate::ui::rich_editor::is_inline_image(p)).count() as u32;
        let size: u64 = paths.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
        let size = crate::models::human_size(size);
        let names = self.cloud_names.borrow();

        self.summary.set_label(&if n == 1 {
            let name = paths[0].file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            format!("{name} · {size}")
        } else {
            ni18n_f("{n} file · {size}", "{n} files · {size}", n, &[("n", &n.to_string()), ("size", &size)])
        });

        let inline = self.allow_inline.get() && pictures > 0;
        self.inline.widget.set_visible(inline);
        self.cloud.widget.set_visible(!names.is_empty());
        match self.context {
            DropContext::Composer => {
                self.attach.subtitle.set_label(&ni18n("Send as a normal attachment", "Send as normal attachments", n));
                self.inline.subtitle.set_label(&if pictures == n {
                    ni18n("Place the picture where the cursor is", "Place the pictures where the cursor is", n)
                } else {
                    i18n("Pictures insert where the cursor is placed, other files attach normally")
                });
                self.cloud.subtitle.set_label(&match names.as_slice() {
                    [one] => i18n_f("Share a download link from {name}", &[("name", one)]),
                    _ => i18n("Share a download link"),
                });
            }
            DropContext::NewMessage => {
                self.cloud.title.set_label(&ni18n("Share Link in New Message", "Share Links in New Message", n));
                self.attach.subtitle.set_label(&ni18n(
                    "Start a message with the file attached",
                    "Start a message with the files attached",
                    n,
                ));
                self.inline.subtitle.set_label(&if pictures == n {
                    ni18n(
                        "Start a message with the picture in the text",
                        "Start a message with the pictures in the text",
                        n,
                    )
                } else {
                    i18n("Start a message with the pictures in the text and the other files attached")
                });
                self.cloud.subtitle.set_label(&match names.as_slice() {
                    [one] => ni18n_f(
                        "Start a message with a download link from {name}",
                        "Start a message with download links from {name}",
                        n,
                        &[("name", one)],
                    ),
                    _ => ni18n(
                        "Start a message with a download link",
                        "Start a message with download links",
                        n,
                    ),
                });
            }
        }

        // Up (still transparent) before anything is measured: a hidden
        // widget's style is not worked out, and it measures as nothing.
        if !self.layer.is_visible() {
            self.layer.set_opacity(0.0);
            self.layer.set_visible(true);
        }

        // A composer stacks them as rows, which the tall reading pane has
        // room for, and puts them side by side only where it is too short
        // to stack them (a split reply dragged small). The main window puts
        // them side by side, stacking them only in a window too narrow for
        // that. Measured, since the words wrap to the room there is.
        let rows = match self.context {
            DropContext::Composer => {
                self.lay_out(true);
                let (need, _, _, _) = self.layer.measure(gtk::Orientation::Vertical, width);
                need <= height
            }
            DropContext::NewMessage => {
                self.lay_out(false);
                let (need, _, _, _) = self.layer.measure(gtk::Orientation::Horizontal, height);
                need > width
            }
        };
        self.lay_out(rows);
        if rows {
            self.align_rows(width - 2 * LAYER_PADDING);
        }

        if !self.shown.replace(true) {
            self.fade_to(1.0);
        }
    }

    /// Give the rows' contents one width, the widest one's, so the icons
    /// line up down the stack however long each card's words are. (A size
    /// group would be the tool, but it does not hold with wrapping labels.)
    fn align_rows(&self, room: i32) {
        let visible: Vec<&Zone> =
            [&self.attach, &self.inline, &self.cloud].into_iter().filter(|z| z.widget.is_visible()).collect();
        let widest = visible
            .iter()
            .map(|z| z.inner.measure(gtk::Orientation::Horizontal, -1).1)
            .max()
            .unwrap_or(0)
            .min(room - 2 * CARD_PADDING);
        for zone in visible {
            zone.inner.set_size_request(widest, -1);
        }
    }

    fn lay_out(&self, rows: bool) {
        self.zones.set_orientation(if rows { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal });
        if rows {
            self.layer.add_css_class("rows");
        } else {
            self.layer.remove_css_class("rows");
        }
        for zone in [&self.attach, &self.inline, &self.cloud] {
            zone.inner.set_size_request(-1, -1);
            zone.lay_out(rows);
        }
    }

    fn hide(&self) {
        self.hover(None);
        if self.shown.replace(false) {
            self.fade_to(0.0);
        }
    }

    /// Fade the layer from wherever it is: a drag that leaves and comes
    /// back mid-fade turns the fade around rather than starting it over.
    fn fade_to(&self, to: f64) {
        if let Some(running) = self.fade.borrow_mut().take() {
            running.pause();
        }
        let from = self.layer.opacity();
        let layer = self.layer.clone();
        let target = adw::CallbackAnimationTarget::new(move |v| layer.set_opacity(v));
        let anim = adw::TimedAnimation::new(&self.layer, from, to, FADE_MS, target);
        anim.set_easing(adw::Easing::EaseOutCubic);
        let layer = self.layer.clone();
        anim.connect_done(move |_| {
            if to == 0.0 {
                layer.set_visible(false);
            }
        });
        anim.play();
        *self.fade.borrow_mut() = Some(anim);
    }
}

impl Zone {
    fn new(icon: &str, title: &str) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 6);
        widget.add_css_class("drop-zone");
        widget.set_hexpand(true);
        widget.set_vexpand(true);

        let inner = gtk::Box::new(gtk::Orientation::Vertical, 6);
        inner.set_valign(gtk::Align::Center);
        inner.set_halign(gtk::Align::Center);
        inner.set_vexpand(true);

        let badge = gtk::Image::from_icon_name(icon);
        badge.add_css_class("drop-badge");
        badge.set_pixel_size(30);
        badge.set_valign(gtk::Align::Center);
        badge.set_halign(gtk::Align::Center);
        inner.append(&badge);

        let words = gtk::Box::new(gtk::Orientation::Vertical, 4);
        words.set_valign(gtk::Align::Center);
        let title = gtk::Label::new(Some(title));
        title.add_css_class("drop-title");
        title.set_wrap(true);
        words.append(&title);

        let subtitle = gtk::Label::new(None);
        subtitle.add_css_class("drop-subtitle");
        subtitle.set_wrap(true);
        subtitle.set_max_width_chars(30);
        subtitle.set_lines(3);
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        subtitle.set_valign(gtk::Align::Start);
        words.append(&subtitle);
        inner.append(&words);

        widget.append(&inner);
        let zone = Self { widget, inner, title, subtitle };
        zone.lay_out(true);
        zone
    }

    /// A row: the icon to the left of the words, the pair centred in the
    /// card and as wide as the other cards' pairs, so the icons line up
    /// ([`DropZones::align_rows`]). A column: the icon over the words, all
    /// centred, the line under the title kept three lines tall (the
    /// stylesheet's `.rows` lifts that) so side by side the icons stay level.
    fn lay_out(&self, row: bool) {
        self.inner.set_orientation(if row { gtk::Orientation::Horizontal } else { gtk::Orientation::Vertical });
        self.inner.set_spacing(if row { 20 } else { 12 });
        let (xalign, justify) =
            if row { (0.0, gtk::Justification::Left) } else { (0.5, gtk::Justification::Center) };
        for label in [&self.title, &self.subtitle] {
            label.set_xalign(xalign);
            label.set_justify(justify);
        }
    }
}

/// The regular files in a dragged list; folders have nothing to attach.
fn file_paths(list: &gtk::gdk::FileList) -> Vec<PathBuf> {
    list.files().iter().filter_map(|f| f.path()).filter(|p| p.is_file()).collect()
}
