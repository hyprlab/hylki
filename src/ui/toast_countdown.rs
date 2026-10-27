//! The Undo Send toast's countdown (#534): a line along the bottom edge of
//! the pill that fills from left to right until the message really goes.
//!
//! A toast has no slot that spans the pill; a custom title sits inside its
//! padding. So the line is a widget with no height of its own, set under the
//! title, that draws in the toast's coordinates, clipped to its rounded
//! outline so it follows the curve at either end like a border.
//!
//! Its color is `@accent_bg_color`: the desktop's accent with the System
//! theme, or an Appearance theme's own.

use std::cell::Cell;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{glib, graphene, gsk, subclass::prelude::*};

/// Thickness of the line.
const LINE: f32 = 3.0;

mod imp {
    use super::*;

    pub(crate) struct ToastCountdown {
        pub(super) started: Cell<Instant>,
        pub(super) total: Cell<Duration>,
    }

    impl Default for ToastCountdown {
        fn default() -> Self {
            Self {
                started: Cell::new(Instant::now()),
                total: Cell::new(Duration::ZERO),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ToastCountdown {
        const NAME: &'static str = "HylkiToastCountdown";
        type Type = super::ToastCountdown;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("toast-countdown");
        }
    }

    impl ObjectImpl for ToastCountdown {}

    impl WidgetImpl for ToastCountdown {
        fn measure(&self, _: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().draw(snapshot);
        }
    }
}

glib::wrapper! {
    pub(crate) struct ToastCountdown(ObjectSubclass<imp::ToastCountdown>)
        @extends gtk::Widget;
}

impl ToastCountdown {
    /// A line that is full `total` from now. Time is read off the clock, not
    /// counted in frames, so a toast that waited behind another, or a window
    /// that was hidden, shows how much is really left.
    fn new(total: Duration) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().total.set(total);
        obj.add_tick_callback(|w, _| {
            w.queue_draw();
            if w.fraction() < 1.0 {
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
        obj
    }

    fn fraction(&self) -> f32 {
        let total = self.imp().total.get().as_secs_f32();
        if total <= 0.0 {
            return 1.0;
        }
        (self.imp().started.get().elapsed().as_secs_f32() / total).min(1.0)
    }

    fn draw(&self, snapshot: &gtk::Snapshot) {
        let mut toast = self.parent();
        while let Some(w) = toast.as_ref().filter(|w| w.css_name() != "toast") {
            toast = w.parent();
        }
        let Some(pill) = toast.and_then(|t| t.compute_bounds(self)) else {
            return;
        };
        let radius = pill.height().min(pill.width()) / 2.0;
        let corner = graphene::Size::new(radius, radius);
        snapshot.push_rounded_clip(&gsk::RoundedRect::new(pill, corner, corner, corner, corner));
        snapshot.append_color(
            &self.color(),
            &graphene::Rect::new(
                pill.x(),
                pill.y() + pill.height() - LINE,
                pill.width() * self.fraction(),
                LINE,
            ),
        );
        snapshot.pop();
    }
}

/// The toast's title while the Undo Send delay runs: the message's line,
/// drawn as libadwaita draws a plain title (which a custom title replaces),
/// over the countdown. Being plain text, a subject with `&` or `<` shows as
/// written.
pub(crate) fn undo_countdown(title: &str, delay_secs: u32) -> gtk::Box {
    let label = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .single_line_mode(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["heading"])
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    // The title slot is as tall as the pill; the stock label centers in it.
    content.set_valign(gtk::Align::Center);
    content.append(&label);
    content.append(&ToastCountdown::new(Duration::from_secs(delay_secs.into())));
    content
}
