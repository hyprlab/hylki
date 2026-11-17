//! The slot a composer slides down in over a whole pane (the reader's, or
//! the contact card's): a revealer over the pane, and between it and the
//! composer a bin that keeps the composer the pane's height (#669).
//!
//! A revealer mid-slide sizes its child at its own height divided by how
//! far the slide has got, so that the child keeps its full size while only
//! part of it shows. Stretched over the pane, the revealer kept the pane's
//! full height throughout, so the child's grew without bound as the slide
//! neared its end: 8,406,485 pixels on the last frame of a close. The
//! composer's WebView makes a drawing surface of each size it is given,
//! and on the NVIDIA driver such a surface fell back to shared memory: one
//! send took about 30 GB, and the machine with it.
//!
//! So the revealer asks the pane for no more than the slide shows, and the
//! bin both asks for the pane's height and never gives the composer more.
//! The composer stays the pane's size and moves, instead of stretching.

use adw::prelude::*;
use gtk::{glib, subclass::prelude::*};

mod imp {
    use super::*;

    #[derive(Default)]
    pub(crate) struct CoverBin;

    #[glib::object_subclass]
    impl ObjectSubclass for CoverBin {
        const NAME: &'static str = "HylkiCoverBin";
        type Type = super::CoverBin;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for CoverBin {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for CoverBin {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.obj().first_child() else {
                return (0, 0, -1, -1);
            };
            let (min, nat, _, _) = child.measure(orientation, for_size);
            match self.pane_height() {
                Some(pane) if orientation == gtk::Orientation::Vertical => {
                    (min.min(pane), pane, -1, -1)
                }
                _ => (min, nat, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let Some(child) = self.obj().first_child() else {
                return;
            };
            let shown = self.pane_height().map_or(height, |pane| height.min(pane));
            // At the bottom of whatever the revealer gives: a slide down
            // shows the bottom of its child first.
            let at = gtk::gsk::Transform::new()
                .translate(&gtk::graphene::Point::new(0.0, (height - shown) as f32));
            child.allocate(width, shown, -1, Some(at));
        }
    }

    impl CoverBin {
        /// The height of the pane the revealer covers, once it has one.
        fn pane_height(&self) -> Option<i32> {
            let pane = self.obj().parent()?.parent()?;
            Some(pane.height()).filter(|h| *h > 0)
        }
    }
}

glib::wrapper! {
    pub(crate) struct CoverBin(ObjectSubclass<imp::CoverBin>)
        @extends gtk::Widget;
}

/// An empty covering slot, hidden. Its parent is the pane it covers.
pub(crate) fn new() -> gtk::Revealer {
    let r = gtk::Revealer::new();
    r.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    r.set_transition_duration(300);
    // As tall as the slide shows, not the pane: see the module comment.
    r.set_valign(gtk::Align::Start);
    r.set_reveal_child(false);
    r.set_child(Some(&glib::Object::new::<CoverBin>()));
    r
}

/// The composer in `slot`, if one is there.
pub(crate) fn child(slot: &gtk::Revealer) -> Option<gtk::Widget> {
    slot.child()?.first_child()
}

/// Put `widget` in `slot`, or empty it with `None`.
pub(crate) fn set_child(slot: &gtk::Revealer, widget: Option<&gtk::Widget>) {
    let Some(bin) = slot.child() else { return };
    while let Some(old) = bin.first_child() {
        old.unparent();
    }
    if let Some(w) = widget {
        w.set_parent(&bin);
    }
}
