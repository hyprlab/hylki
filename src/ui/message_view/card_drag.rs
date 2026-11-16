//! The pointer over the reader's cards: a click on a card's text selects the
//! card, and a drag from a card's header carries the message to a folder in
//! the sidebar, as a drag of its row in the list does.
//!
//! Both are watched on the GTK side. The body frames run no scripts, so the
//! page never hears their clicks, and WebKit claims every press for itself,
//! so neither a GestureClick nor a DragSource on the web view ever sees one
//! through. A legacy controller sees the raw events before WebKit does and
//! asks the page what lies under the pointer.

use super::types::MessageViewInput;
use super::MessageView;
use crate::ui::drag_card;
use adw::prelude::*;
use gtk::gdk;
use gtk::glib::Propagation;
use relm4::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use webkit6::prelude::WebViewExt;

/// Pointer travel past which a press and release is no longer a click (it
/// selected text instead).
const CLICK_SLOP: f64 = 4.0;

/// One press of the primary button over the reader, and the drag it may
/// become.
#[derive(Debug, Default)]
pub(super) struct PointerPress {
    /// Where the press went down, in surface coordinates.
    at: Option<(f64, f64)>,
    device: Option<gdk::Device>,
    /// The card whose header the press went down on, once the page says.
    handle: Option<(u32, u32)>,
    /// Counts presses, so the page's answer about an earlier one is dropped.
    serial: u64,
    /// The press became a drag: the rest of it is the drag's, not WebKit's.
    dragging: bool,
    /// The drag under way, kept alive until it ends.
    drag: Option<gdk::Drag>,
}

pub(super) type Pointer = Rc<RefCell<PointerPress>>;

impl MessageView {
    pub(super) fn watch_pointer(&self, sender: &ComponentSender<Self>) {
        let pointer = self.pointer.clone();
        let sender = sender.input_sender().clone();
        let legacy = gtk::EventControllerLegacy::new();
        legacy.set_propagation_phase(gtk::PropagationPhase::Capture);
        legacy.connect_event(move |ctl, event| {
            let Some(view) = ctl.widget().and_downcast::<webkit6::WebView>() else {
                return Propagation::Proceed;
            };
            let Some(at) = event.position() else {
                return Propagation::Proceed;
            };
            let primary = event
                .downcast_ref::<gdk::ButtonEvent>()
                .is_some_and(|b| b.button() == gdk::BUTTON_PRIMARY);
            match event.event_type() {
                gdk::EventType::ButtonPress if primary => {
                    let serial = {
                        let mut p = pointer.borrow_mut();
                        let serial = p.serial + 1;
                        *p = PointerPress {
                            at: Some(at),
                            device: event.device(),
                            serial,
                            ..PointerPress::default()
                        };
                        serial
                    };
                    find_handle(&view, at, pointer.clone(), serial);
                    Propagation::Proceed
                }
                gdk::EventType::MotionNotify => {
                    let mut p = pointer.borrow_mut();
                    if p.dragging {
                        return Propagation::Stop;
                    }
                    let (Some(from), Some(key)) = (p.at, p.handle) else {
                        return Propagation::Proceed;
                    };
                    let held = event
                        .modifier_state()
                        .contains(gdk::ModifierType::BUTTON1_MASK);
                    let threshold = f64::from(
                        gtk::prelude::WidgetExt::settings(&view).gtk_dnd_drag_threshold(),
                    );
                    if !held || distance(from, at) < threshold {
                        return Propagation::Proceed;
                    }
                    p.dragging = true;
                    sender.emit(MessageViewInput::DragCard(key));
                    Propagation::Stop
                }
                gdk::EventType::ButtonRelease if primary => {
                    let (from, dragging) = {
                        let mut p = pointer.borrow_mut();
                        p.handle = None;
                        (p.at.take(), p.dragging)
                    };
                    if dragging {
                        return Propagation::Stop;
                    }
                    if from.is_some_and(|from| distance(from, at) <= CLICK_SLOP) {
                        pick_card_at(&view, at, event.modifier_state());
                    }
                    Propagation::Proceed
                }
                _ => Propagation::Proceed,
            }
        });
        self.webview.add_controller(legacy);
    }

    /// Start dragging the card `key` (or the selected cards, when it is one
    /// of several) out of the reader, carrying what a list row's drag does.
    pub(super) fn on_drag_card(&mut self, key: (u32, u32)) {
        if self.begin_card_drag(key).is_none() {
            self.pointer.borrow_mut().dragging = false;
        }
    }

    fn begin_card_drag(&mut self, key: (u32, u32)) -> Option<()> {
        let (from, device) = {
            let p = self.pointer.borrow();
            (p.at?, p.device.clone()?)
        };
        let surface = self.webview.native()?.surface()?;
        let (px, py, _) = surface.device_position(&device)?;
        let several = self.selected_cards.len() > 1 && self.selected_cards.contains(&key);
        let carried: Vec<_> = self
            .thread
            .iter()
            .filter(|m| {
                let k = (m.account_id, m.id);
                if several {
                    self.selected_cards.contains(&k)
                } else {
                    k == key
                }
            })
            .collect();
        let front = carried.iter().find(|m| (m.account_id, m.id) == key)?;
        let items: Vec<_> = carried
            .iter()
            .map(|m| (m.account_id, m.folder_id, m.uid, m.id))
            .collect();
        let card = |m: &crate::models::Message| {
            drag_card::card(&m.subject, &m.shown_from_name(), m.shown_from_addr(), None)
        };
        let icon = if carried.len() > 1 {
            let behind = carried
                .iter()
                .filter(|m| (m.account_id, m.id) != key)
                .take(drag_card::DRAG_STACK - 1)
                .map(|m| card(m))
                .collect();
            drag_card::stack(card(front), behind, carried.len())
        } else {
            card(front).upcast()
        };
        let content = drag_card::move_payload(&items);
        let drag = gdk::Drag::begin(
            &surface,
            &device,
            &content,
            gdk::DragAction::MOVE,
            from.0 - px,
            from.1 - py,
        )?;
        gtk::DragIcon::for_drag(&drag).set_child(Some(&icon));
        drag.set_hotspot(0, 0);
        // Done or given up, the drag says so to GDK, which takes its icon
        // away, and the pointer goes back to WebKit.
        let done = {
            let pointer = self.pointer.clone();
            move |drag: &gdk::Drag, success: bool| {
                drag.drop_done(success);
                let mut p = pointer.borrow_mut();
                p.dragging = false;
                p.drag = None;
            }
        };
        {
            let done = done.clone();
            drag.connect_dnd_finished(move |d| done(d, true));
        }
        drag.connect_cancel(move |d, _| done(d, false));
        self.pointer.borrow_mut().drag = Some(drag);
        Some(())
    }
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// A point in the view's surface coordinates, in the view's own.
fn widget_point(view: &webkit6::WebView, surface: (f64, f64)) -> Option<(f64, f64)> {
    let native = view.native()?;
    let (dx, dy) = native.surface_transform();
    let at = native.compute_point(
        view,
        &gtk::graphene::Point::new((surface.0 - dx) as f32, (surface.1 - dy) as f32),
    )?;
    Some((f64::from(at.x()), f64::from(at.y())))
}

/// Ask the page whether the press at `surface` landed on a card's header,
/// the part of a card a drag takes it by, and keep the answer for the drag.
fn find_handle(view: &webkit6::WebView, surface: (f64, f64), pointer: Pointer, serial: u64) {
    let Some((x, y)) = widget_point(view, surface) else {
        return;
    };
    let width = view.width().max(1);
    view.evaluate_javascript(
        &format!("window.__hylkiDragAt({x},{y},{width})"),
        None,
        None,
        None::<&gtk::gio::Cancellable>,
        move |r| {
            let Ok(v) = r else { return };
            let text = v.to_str();
            let Some((a, i)) = text.split_once(':') else {
                return;
            };
            let (Ok(a), Ok(i)) = (a.parse(), i.parse()) else {
                return;
            };
            let mut p = pointer.borrow_mut();
            if p.serial == serial && p.at.is_some() {
                p.handle = Some((a, i));
            }
        },
    );
}

/// Ask the page to select the card whose body frame is under `surface`, as a
/// click on the rest of the card does.
fn pick_card_at(view: &webkit6::WebView, surface: (f64, f64), state: gdk::ModifierType) {
    let Some((x, y)) = widget_point(view, surface) else {
        return;
    };
    let mode = if state.contains(gdk::ModifierType::SHIFT_MASK) {
        'r'
    } else if state.contains(gdk::ModifierType::CONTROL_MASK) {
        't'
    } else {
        'p'
    };
    let width = view.width().max(1);
    view.evaluate_javascript(
        &format!("window.__hylkiPickAt({x},{y},{width},'{mode}')"),
        None,
        None,
        None::<&gtk::gio::Cancellable>,
        |_| {},
    );
}
