//! Opening a message as one change on screen: the reading pane is frozen
//! (a still of it laid over it), everything under the still changes as it
//! arrives, and the still is lifted in one frame once the new message is
//! ready to look at.
//!
//! Opening a message changes many things that arrive one by one: the
//! subject, the banners, the attachment drawer and its room, the document
//! and where it is scrolled to. Applied as they came, each showed on its
//! own: a subject over the last message's body, a drawer that went and
//! came back, a page that showed and then jumped. Under the still they
//! settle unseen, at the pane's final size, and the reader cuts from one
//! message to the next, the way a browser holds a page until the next one
//! can paint.
//!
//! The still is taken at once, from what GTK draws for the pane (the page
//! included: the WebView draws its last frame), and rendered to a texture
//! there and then, so nothing that changes afterwards can show in it. An
//! asynchronous snapshot from WebKit came too late on real hardware, where
//! the app is busy right after a click, and the pane went uncovered.

use adw::prelude::*;
use relm4::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// A freeze nothing lifts (a page that never reports in) ends on its own.
const BACKSTOP_MS: u64 = 1500;

/// How long the still takes to fade out over the new message, so one
/// message dissolves into the next rather than cutting to it.
const FADE_MS: u32 = 80;

struct Inner {
    /// Holds the pane, with the still laid over it.
    host: gtk::Overlay,
    still: gtk::Picture,
    frozen: Cell<bool>,
    /// Bumped per freeze, so what was meant to end one (a reveal, the
    /// backstop) can't end the next.
    generation: Cell<u64>,
    /// The still fading out after a reveal.
    fade: RefCell<Option<adw::TimedAnimation>>,
}

/// One reading pane's freeze. Cheap to clone; the reader, its attachment
/// drawer and their owner share it.
#[derive(Clone)]
pub(crate) struct ReaderSwap(Rc<Inner>);

impl ReaderSwap {
    pub(crate) fn new() -> Self {
        let still = gtk::Picture::new();
        // The pane keeps its size while frozen (the window does), so the
        // still lies exactly over it.
        still.set_content_fit(gtk::ContentFit::Fill);
        still.set_can_shrink(true);
        still.set_can_target(false);
        still.set_can_focus(false);
        still.set_visible(false);
        // Opaque: where the pane draws no ground of its own (around the
        // subject), what changes under the still must not show through.
        still.add_css_class("reader-swap-still");
        let host = gtk::Overlay::new();
        host.add_overlay(&still);
        Self(Rc::new(Inner {
            host,
            still,
            frozen: Cell::new(false),
            generation: Cell::new(0),
            fade: RefCell::new(None),
        }))
    }

    /// The widget that holds the pane: placed where the pane would be.
    pub(crate) fn host(&self) -> &gtk::Overlay {
        &self.0.host
    }

    /// The pane that is frozen: the reader with its attachment drawer.
    pub(crate) fn set_pane(&self, pane: &impl IsA<gtk::Widget>) {
        self.0.host.set_child(Some(pane));
    }

    pub(crate) fn generation(&self) -> u64 {
        self.0.generation.get()
    }

    /// The pane is held: what changes on it is not seen.
    pub(crate) fn held(&self) -> bool {
        self.0.frozen.get()
    }

    /// Hold what the pane shows now until `reveal`. A freeze already under
    /// way carries on: the still up is what the reader last saw.
    pub(crate) fn freeze(&self) {
        let inner = &self.0;
        if inner.frozen.replace(true) {
            return;
        }
        let generation = inner.generation.get().wrapping_add(1);
        inner.generation.set(generation);
        let backstop = self.clone();
        gtk::glib::timeout_add_local_once(
            std::time::Duration::from_millis(BACKSTOP_MS),
            move || backstop.reveal(generation, "backstop"),
        );
        let Some(pane) = inner.host.child().filter(|p| p.is_mapped()) else {
            // Nothing on screen: nothing to hold.
            tracing::debug!(target: "hylki::swap", "freeze {generation}: pane not on screen");
            return;
        };
        let started = std::time::Instant::now();
        match still_of(&inner.host, &pane) {
            Ok(still) => {
                // A still still fading from the last reveal gives way to
                // this one, at full strength.
                if let Some(fade) = inner.fade.take() {
                    fade.pause();
                }
                inner.still.set_opacity(1.0);
                inner.still.set_paintable(Some(&still));
                inner.still.set_visible(true);
                tracing::debug!(
                    target: "hylki::swap",
                    "freeze {generation}: still up in {:?}",
                    started.elapsed()
                );
            }
            // The pane changes in view, as it did before the freeze.
            Err(why) => {
                tracing::debug!(target: "hylki::swap", "freeze {generation}: no still: {why}")
            }
        }
    }

    /// Show the pane as it now is: the still fades out over it in
    /// `FADE_MS`. Only for the freeze it was meant for: a later one has a
    /// still of its own. `why` is for the log.
    pub(crate) fn reveal(&self, generation: u64, why: &str) {
        let inner = &self.0;
        if inner.generation.get() != generation || !inner.frozen.get() {
            return;
        }
        tracing::debug!(target: "hylki::swap", "reveal {generation}: {why}");
        inner.frozen.set(false);
        if !inner.still.is_visible() {
            return;
        }
        let still = inner.still.clone();
        let target = adw::CallbackAnimationTarget::new(move |v| still.set_opacity(v));
        let fade = adw::TimedAnimation::new(&inner.still, 1.0, 0.0, FADE_MS, target);
        // Five frames at 60 Hz: an eased curve spends most of its change in
        // the first, which reads as a cut. Even steps dissolve.
        fade.set_easing(adw::Easing::Linear);
        // Weak: the animation is kept in `Inner`.
        let weak = Rc::downgrade(&self.0);
        fade.connect_done(move |_| {
            let Some(inner) = weak.upgrade() else { return };
            // A freeze since has put up a still of its own.
            if inner.frozen.get() || inner.generation.get() != generation {
                return;
            }
            inner.still.set_visible(false);
            inner.still.set_paintable(None::<&gtk::gdk::Paintable>);
            inner.still.set_opacity(1.0);
            inner.fade.replace(None);
        });
        if let Some(old) = inner.fade.replace(Some(fade.clone())) {
            old.pause();
        }
        fade.play();
    }
}

/// What `pane` shows in `host` now, as a texture: drawn the way the host
/// draws it and rendered at once, at the screen's scale. `Err` says why
/// there is none, for the log.
fn still_of(host: &gtk::Overlay, pane: &gtk::Widget) -> Result<gtk::gdk::Texture, String> {
    let native = host.native().ok_or("no native")?;
    let renderer = native.renderer().ok_or("no renderer")?;
    let scale = native.surface().map(|s| s.scale()).unwrap_or(1.0) as f32;
    let (width, height) = (host.width() as f32, host.height() as f32);
    if width <= 0.0 || height <= 0.0 {
        return Err(format!("host is {width}x{height}"));
    }
    // GTK lays widgets out at the next frame and will not draw one that is
    // waiting for it ("snapshot without a current allocation"): something
    // in the pane often has, just before a click. Lay it out now, at the
    // size it has (the host gives it all its room), as that frame would.
    pane.allocate(pane.width(), pane.height(), -1, None);
    let snapshot = gtk::Snapshot::new();
    snapshot.scale(scale, scale);
    host.snapshot_child(pane, &snapshot);
    let node = snapshot.to_node().ok_or_else(|| {
        format!(
            "the pane drew nothing (mapped {}, drawable {}, {}x{}, opacity {})",
            pane.is_mapped(),
            pane.is_drawable(),
            pane.width(),
            pane.height(),
            pane.opacity()
        )
    })?;
    let viewport = gtk::graphene::Rect::new(0.0, 0.0, width * scale, height * scale);
    Ok(renderer.render_texture(&node, Some(&viewport)))
}
