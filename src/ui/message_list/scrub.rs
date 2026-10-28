//! The scrollbar's date: while the list's scrollbar is dragged, a label by
//! its thumb names the month and year of the rows in the middle of the
//! view, as Photos does, so a long folder can be browsed back by the year.

use super::types::MessageListInput;
use super::MessageList;
use crate::ui::message_row::RowShared;
use adw::prelude::*;
use gtk::{gdk, glib};
use relm4::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// How long the label stays once the thumb is let go.
const LINGER: std::time::Duration = std::time::Duration::from_millis(700);

pub(super) fn watch_scrollbar(
    scroller: &gtk::ScrolledWindow,
    shared: &Rc<RowShared>,
    sender: &ComponentSender<MessageList>,
) {
    let Some(overlay) = scroller.parent().and_downcast::<gtk::Overlay>() else {
        return;
    };
    let bar = scroller.vscrollbar();
    let label = gtk::Label::new(None);
    label.add_css_class("list-scrub-date");
    label.set_halign(gtk::Align::End);
    label.set_valign(gtk::Align::Start);
    label.set_margin_end(24);
    label.set_can_target(false);
    overlay.add_overlay(&label);

    // The list holds the whole folder once the pointer reaches the bar,
    // before it is pressed: a thumb shrinking under a drag would throw the
    // list far down.
    let motion = gtk::EventControllerMotion::new();
    let input = sender.input_sender().clone();
    motion.connect_enter(move |_, _, _| {
        let _ = input.send(MessageListInput::ShowWholeList);
    });
    bar.add_controller(motion);

    let scrub = Rc::new(Scrub {
        scroller: scroller.clone(),
        overlay,
        bar: bar.clone(),
        label,
        shared: shared.clone(),
        tick: RefCell::new(None),
        gen: Cell::new(0),
    });
    // Watched, not handled: the bar drags as it always does.
    let events = gtk::EventControllerLegacy::new();
    events.set_propagation_phase(gtk::PropagationPhase::Capture);
    let input = sender.input_sender().clone();
    events.connect_event(move |_, event| {
        match event.event_type() {
            gdk::EventType::ButtonPress | gdk::EventType::TouchBegin => {
                let _ = input.send(MessageListInput::ShowWholeList);
                scrub.start();
            }
            gdk::EventType::ButtonRelease
            | gdk::EventType::TouchEnd
            | gdk::EventType::TouchCancel => scrub.stop(),
            _ => {}
        }
        glib::Propagation::Proceed
    });
    bar.add_controller(events);
}

struct Scrub {
    scroller: gtk::ScrolledWindow,
    overlay: gtk::Overlay,
    bar: gtk::Widget,
    label: gtk::Label,
    shared: Rc<RowShared>,
    /// Following the drag, a frame at a time.
    tick: RefCell<Option<gtk::TickCallbackId>>,
    /// Bumped by each press, so a fade from an earlier one leaves it be.
    gen: Cell<u64>,
}

impl Scrub {
    fn start(self: &Rc<Self>) {
        self.gen.set(self.gen.get() + 1);
        if self.tick.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(self);
        let id = self.label.add_tick_callback(move |_, _| {
            match weak.upgrade() {
                Some(scrub) => scrub.follow(),
                None => return glib::ControlFlow::Break,
            }
            glib::ControlFlow::Continue
        });
        self.tick.replace(Some(id));
    }

    fn stop(self: &Rc<Self>) {
        if let Some(id) = self.tick.take() {
            id.remove();
        }
        let gen = self.gen.get();
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(LINGER, move || {
            if let Some(scrub) = weak.upgrade() {
                if scrub.gen.get() == gen {
                    scrub.label.remove_css_class("shown");
                }
            }
        });
    }

    /// Name the month the middle of the view is in, beside the thumb. A
    /// list in another order than by date has no month to name.
    fn follow(&self) {
        let date = self
            .shared
            .by_date
            .get()
            .then(|| self.middle_date())
            .flatten();
        let Some(ts) = date else {
            self.label.remove_css_class("shown");
            return;
        };
        let text = crate::datefmt::month_year(ts);
        if self.label.label() != text {
            self.label.set_label(&text);
        }
        self.place();
        self.label.add_css_class("shown");
    }

    /// The date of the row across the middle of the view.
    fn middle_date(&self) -> Option<i64> {
        let view = self.scroller.child()?;
        let middle = view.height() as f32 / 2.0;
        self.shared.bound_rows().into_iter().find_map(|row| {
            let widget = row.widget()?;
            let top = widget.compute_point(&view, &gtk::graphene::Point::new(0.0, 0.0))?;
            let inside = top.y() <= middle && middle < top.y() + widget.height() as f32;
            let data = row.data()?;
            inside.then(|| {
                data.meta
                    .latest_at
                    .as_ref()
                    .map_or(data.msg.timestamp, |l| l.0)
            })
        })
    }

    /// Level with the middle of the thumb, worked out as the bar draws it,
    /// kept inside the view.
    fn place(&self) {
        let adj = self.scroller.vadjustment();
        let track = self.bar.height() as f64;
        let (value, page, upper) = (adj.value(), adj.page_size(), adj.upper());
        if track <= 0.0 || upper <= page {
            return;
        }
        let thumb = (page / upper * track).clamp(40.0_f64.min(track), track);
        let middle = value / (upper - page) * (track - thumb) + thumb / 2.0;
        let Some(at) = self.bar.compute_point(
            &self.overlay,
            &gtk::graphene::Point::new(0.0, middle as f32),
        ) else {
            return;
        };
        let height = self.label.measure(gtk::Orientation::Vertical, -1).1;
        let room = self.overlay.height() - height - 6;
        let top = (at.y() as i32 - height / 2).clamp(6, room.max(6));
        if self.label.margin_top() != top {
            self.label.set_margin_top(top);
        }
    }
}
