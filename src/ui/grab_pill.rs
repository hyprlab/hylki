//! The grab pill: a thin rounded bar floated near a pane boundary, the iOS
//! home-indicator look, that drags a vertical `GtkPaned` divider. The Paned's
//! own separator is styled invisible where this is used — the pill is the
//! visible affordance, and dragging it moves the real divider.

use gtk::prelude::*;

/// Build a pill that drags `paned`'s divider. Dragging down always increases
/// the position (the top pane grows), which reads correctly from either side
/// of the divider. `clamp` bounds each candidate position; hand the Paned's
/// own limits back unchanged with `|_, p| p`.
///
/// The caller aligns the pill against its boundary (`set_valign` +
/// margin) and adds it to an Overlay over the pane it rides.
///
/// The pointer is tracked in ROOT coordinates: the pill rides a pane the drag
/// itself resizes, so pill-local offsets would feed back into the resize and
/// jitter (the console grip's trick).
pub(crate) fn paned_grab_pill(
    paned: &gtk::Paned,
    clamp: impl Fn(&gtk::Paned, i32) -> i32 + 'static,
) -> gtk::Box {
    let pill = pill_widget();
    let split = paned.clone();
    attach_drag(
        &pill,
        paned,
        move |_| {},
        move |want| {
            split.set_position(clamp(&split, want));
        },
    );
    pill
}

/// The pill, for a host that wires its own gesture semantics (the attachment
/// drawer's click-toggle) via [`attach_drag`]. What comes back is an
/// invisible, larger hit zone with the thin bar drawn centred inside it — the
/// visible bar is only 5px tall, far too small a target to press and release
/// reliably, so the gestures live on the zone (the iOS home indicator does
/// the same).
fn pill_widget() -> gtk::Box {
    let hit = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    hit.add_css_class("split-grab-hit");
    hit.set_size_request(120, 20);
    hit.set_halign(gtk::Align::Center);
    // row-resize (the arrows with the divider line), matching the split
    // reply's own Paned separator right below — one cursor across that
    // whole handle. The drawer's edge variant keeps plain ns-resize, since
    // its native separator is inert and shows no cursor of its own.
    hit.set_cursor_from_name(Some("row-resize"));
    hit.append(&pill_bar());
    hit
}

/// The pill stretched into a full-width edge zone: the bar stays centred,
/// but the gestures answer anywhere along the pane's edge — the attachment
/// drawer's whole seam clicks and drags, not just the bar.
pub(super) fn edge_pill_widget() -> gtk::Box {
    let hit = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    hit.add_css_class("split-grab-hit");
    hit.set_height_request(24);
    hit.set_hexpand(true);
    hit.set_halign(gtk::Align::Fill);
    hit.set_cursor_from_name(Some("ns-resize"));
    let bar = pill_bar();
    // Centred in the zone the bar floated 12px off the drawer's edge, too
    // far to read as its handle; the top margin brings it 4px closer while
    // the zone keeps its full height to press.
    bar.set_margin_top(8);
    hit.append(&bar);
    hit
}

fn pill_bar() -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bar.add_css_class("split-grab-pill");
    bar.set_size_request(100, 5);
    bar.set_hexpand(true);
    bar.set_halign(gtk::Align::Center);
    bar.set_valign(gtk::Align::Center);
    bar
}

/// The pointer travel below which a press-and-release still counts as a
/// click: `GtkGestureDrag` has no threshold of its own, so without this slop
/// a click's pixel of jitter would fire drag updates.
const DRAG_SLOP: i32 = 4;

/// Wire the pill's drag: `on_begin` fires as a press lands, then `on_want`
/// gets each update's candidate divider position (the position at drag start
/// plus the pointer's travel) — the host decides what to do with it. Updates
/// only start once the pointer has travelled past [`DRAG_SLOP`]; from then on
/// they flow continuously, back inside the slop included.
pub(super) fn attach_drag(
    pill: &gtk::Box,
    paned: &gtk::Paned,
    on_begin: impl Fn(i32) + 'static,
    on_want: impl Fn(i32) + 'static,
) {
    let drag = gtk::GestureDrag::new();
    let split = paned.clone();
    let armed = std::rc::Rc::new(std::cell::Cell::new(false));
    let start = std::rc::Rc::new(std::cell::Cell::new((0i32, 0f32)));
    let to_root_y = {
        let pill = pill.clone();
        move |x: f64, y: f64| -> Option<f32> {
            let root = pill.root()?;
            pill.compute_point(
                root.upcast_ref::<gtk::Widget>(),
                &gtk::graphene::Point::new(x as f32, y as f32),
            )
            .map(|p| p.y())
        }
    };
    {
        let start = start.clone();
        let armed = armed.clone();
        let to_root_y = to_root_y.clone();
        drag.connect_drag_begin(move |_, x, y| {
            if let Some(ry) = to_root_y(x, y) {
                let pos = split.position();
                start.set((pos, ry));
                armed.set(false);
                on_begin(pos);
            }
        });
    }
    drag.connect_drag_update(move |g, ox, oy| {
        let Some((sx, sy)) = g.start_point() else {
            return;
        };
        let Some(now) = to_root_y(sx + ox, sy + oy) else {
            return;
        };
        let (start_pos, start_y) = start.get();
        let want = start_pos + (now - start_y) as i32;
        if !armed.get() {
            if (want - start_pos).abs() < DRAG_SLOP {
                return;
            }
            armed.set(true);
        }
        on_want(want);
    });
    pill.add_controller(drag);
}
