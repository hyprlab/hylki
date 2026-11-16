//! Steps of the drawer's `init`: the widgets and gestures built around
//! the view.

use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;

use super::{AttachmentDrawer, AttachmentDrawerInput};

/// Space previews the highlighted attachment and Enter opens it, as a
/// double click does: the keyboard side of the single-click-then-preview
/// flow (issue #37). They differ only for what the lightbox cannot show,
/// which Space hands to the file previewer as Files does (#625).
pub(super) fn activate_on_keys(flow: &gtk::FlowBox, sender: &ComponentSender<AttachmentDrawer>) {
    let key = gtk::EventControllerKey::new();
    let s = sender.clone();
    let flow_ref = flow.clone();
    key.connect_key_pressed(move |key, keyval, _, _| {
        use gtk::gdk::Key;
        let Some(i) = flow_ref
            .selected_children()
            .first()
            .map(|c| c.index() as usize)
        else {
            return gtk::glib::Propagation::Proceed;
        };
        let input = match keyval {
            // The previewer's focus is asked for with the key press's
            // time, which the compositor weighs against the last input.
            Key::space | Key::KP_Space => {
                AttachmentDrawerInput::Preview(i, key.current_event_time())
            }
            Key::Return | Key::KP_Enter => AttachmentDrawerInput::Activate(i),
            _ => return gtk::glib::Propagation::Proceed,
        };
        s.input_sender().emit(input);
        gtk::glib::Propagation::Stop
    });
    flow.add_controller(key);
}

/// The grab pill floats just above the seam, over the reader's bottom
/// edge — the same affordance, in the same place, as the split reply's.
/// Its gestures route through update(), which knows the collapsed
/// state: a plain click toggles the drawer, a drag resizes it live —
/// collapsed included, where the release point becomes the drawer's
/// new height.
///
/// The full-width edge variant: the whole seam clicks and drags, not
/// just the centred bar — hunting a 120px target over a busy reader
/// made the handle feel unreliable.
pub(super) fn grab_pill(
    paned: &gtk::Paned,
    sender: &ComponentSender<AttachmentDrawer>,
) -> gtk::Box {
    let pill = crate::ui::grab_pill::edge_pill_widget();
    pill.set_valign(gtk::Align::End);
    pill.set_visible(false); // no seam until attachments arrive
    let s = sender.clone();
    let s2 = sender.clone();
    crate::ui::grab_pill::attach_drag(
        &pill,
        paned,
        move |_| s.input_sender().emit(AttachmentDrawerInput::PillDragBegin),
        move |want| {
            s2.input_sender()
                .emit(AttachmentDrawerInput::PillDrag { want })
        },
    );
    let click = gtk::GestureClick::new();
    let s = sender.clone();
    click.connect_released(move |_, _, _, _| {
        s.input_sender().emit(AttachmentDrawerInput::PillReleased);
    });
    pill.add_controller(click);
    pill
}

/// Untargeting the separator is not enough: GtkPaned's pan gesture is
/// attached to the PANED itself, in capture phase, hit-testing raw
/// coordinates against an enlarged handle area. It claims presses
/// near the seam before the zone or edge strip ever see them (hover
/// and cursor stay ours, the click dies, and a drag "works" because
/// the paned handles it natively). Remove the gesture outright.
pub(super) fn remove_paned_gestures(paned: &gtk::Paned) {
    // Both of them: the GestureDrag is what actually moves the
    // divider (and eats the clicks); the GesturePan is its touch
    // sibling. Only the keyboard ShortcutController stays.
    let controllers = paned.observe_controllers();
    let mut doomed: Vec<gtk::EventController> = Vec::new();
    for i in 0..controllers.n_items() {
        let Some(c) = controllers
            .item(i)
            .and_then(|o| o.downcast::<gtk::EventController>().ok())
        else {
            continue;
        };
        if c.is::<gtk::GestureDrag>() || c.is::<gtk::GesturePan>() {
            doomed.push(c);
        }
    }
    tracing::debug!("removing {} paned gesture(s)", doomed.len());
    for c in doomed {
        paned.remove_controller(&c);
    }
}

/// The drawer's top-edge strip: clicks and drags like the grab pill.
pub(super) fn wire_drawer_edge(
    edge: &gtk::Box,
    pill: &gtk::Box,
    paned: &gtk::Paned,
    sender: &ComponentSender<AttachmentDrawer>,
) {
    // The drawer's top-edge strip: a click toggles, a drag resizes —
    // through the same inputs as the pill, so a drag out of the
    // collapse and the click/drag distinction behave identically.
    let click = gtk::GestureClick::new();
    let s = sender.clone();
    click.connect_released(move |_, _, _, _| {
        s.input_sender().emit(AttachmentDrawerInput::EdgeClicked);
    });
    edge.add_controller(click);
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
    edge.add_controller(motion);
    let s = sender.clone();
    let s2 = sender.clone();
    crate::ui::grab_pill::attach_drag(
        edge,
        paned,
        move |_| s.input_sender().emit(AttachmentDrawerInput::PillDragBegin),
        move |want| {
            s2.input_sender()
                .emit(AttachmentDrawerInput::PillDrag { want })
        },
    );
}

/// HYLKI_SHOWCASE_DRAWER=N clicks the header's count button N times,
/// one a second from 6s — the real button press, so the capture proves
/// the click path and not just the input it sends.
pub(super) fn showcase_clicks(btn: &gtk::Button) {
    if let Some(Ok(n)) = std::env::var("HYLKI_SHOWCASE_DRAWER")
        .ok()
        .map(|v| v.parse::<u32>())
    {
        let btn = btn.clone();
        for i in 0..n {
            let btn = btn.clone();
            glib::timeout_add_seconds_local_once(6 + i, move || btn.emit_clicked());
        }
    }
}
