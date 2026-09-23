//! The app-icon gallery: a grid of every icon in `app_icon::catalog`, with
//! the current choice ringed. Shared by Settings (System & Appearance) and
//! the welcome wizard's personalize page; the caller hears each pick.

use std::rc::Rc;

use gtk::prelude::*;
use crate::i18n::i18n;

/// Build the gallery as a fixed six-wide grid (three even rows) with
/// `selected` ringed (none when it is empty); `on_pick` runs for every change the user makes (not
/// for the initial selection).
pub fn gallery(selected: &str, tile: i32, on_pick: Rc<dyn Fn(&str)>) -> gtk::FlowBox {
    build(selected, tile, 6, on_pick)
}

/// The gallery as one row that scrolls sideways, its edges fading into the
/// card wherever there is more to scroll to.
pub fn strip(selected: &str, tile: i32, on_pick: Rc<dyn Fn(&str)>) -> gtk::Overlay {
    let n = crate::app_icon::catalog().count() as u32;
    let row = build(selected, tile, n.max(1), on_pick);
    row.set_halign(gtk::Align::Start);
    row.set_hexpand(false);
    // Natural height only: in a taller host (the wizard's card) the cells
    // would otherwise stretch, and the selection ring with them.
    row.set_valign(gtk::Align::Start);
    row.set_vexpand(false);
    // Room under the row for the scrollbar, so it never sits on the labels.
    row.set_margin_bottom(8);
    let sw = gtk::ScrolledWindow::new();
    sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
    // A fixed height: the cell (padding, tile, gap, two label lines,
    // padding) plus the scrollbar margin. A flow box asked for its
    // natural height at a width narrower than its one row over-reports
    // it, which left a band of empty card under the icons.
    let height = 8 + tile + 4 + 32 + 6 + 8;
    sw.set_min_content_height(height);
    sw.set_max_content_height(height);
    sw.set_overlay_scrolling(true);
    sw.set_vexpand(false);
    sw.set_valign(gtk::Align::Start);
    sw.set_child(Some(&row));
    // Open on the current choice rather than the start of the row.
    if let Some(child) = row.selected_children().into_iter().next() {
        let sw2 = sw.clone();
        gtk::glib::idle_add_local_once(move || {
            let x = child.allocation().x() as f64;
            let w = child.allocation().width() as f64;
            let page = sw2.hadjustment().page_size();
            sw2.hadjustment().set_value((x + w / 2.0 - page / 2.0).max(0.0));
        });
    }

    // The fades: a gradient from the card's colour to nothing over each
    // edge, drawn only as far as there is content hidden beyond it, so a
    // row scrolled fully to one end shows a clean edge there.
    const FADE: f64 = 36.0;
    let fade = |side: &str, align: gtk::Align| {
        let f = gtk::Box::new(gtk::Orientation::Vertical, 0);
        f.add_css_class("icon-strip-fade");
        f.add_css_class(side);
        f.set_can_target(false);
        f.set_width_request(FADE as i32);
        f.set_halign(align);
        f.set_valign(gtk::Align::Fill);
        f.set_opacity(0.0);
        f
    };
    let left = fade("left", gtk::Align::Start);
    let right = fade("right", gtk::Align::End);
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&sw));
    overlay.add_overlay(&left);
    overlay.add_overlay(&right);
    let update = {
        let (left, right) = (left.clone(), right.clone());
        move |a: &gtk::Adjustment| {
            let v = a.value();
            let end = (a.upper() - a.page_size()).max(0.0);
            left.set_opacity((v / FADE).clamp(0.0, 1.0));
            right.set_opacity(((end - v) / FADE).clamp(0.0, 1.0));
        }
    };
    let adj = sw.hadjustment();
    adj.connect_value_changed(update.clone());
    adj.connect_changed(update);
    overlay
}

fn build(selected: &str, tile: i32, per_line: u32, on_pick: Rc<dyn Fn(&str)>) -> gtk::FlowBox {
    let grid = gtk::FlowBox::new();
    grid.add_css_class("icon-gallery");
    grid.set_selection_mode(gtk::SelectionMode::Single);
    grid.set_homogeneous(true);
    grid.set_activate_on_single_click(true);
    grid.set_min_children_per_line(per_line);
    grid.set_max_children_per_line(per_line);
    grid.set_column_spacing(2);
    grid.set_row_spacing(2);
    grid.set_halign(gtk::Align::Fill);
    grid.set_hexpand(true);

    let mut to_select = None;
    for choice in crate::app_icon::catalog() {
        let cell = gtk::Box::new(gtk::Orientation::Vertical, 4);
        cell.set_halign(gtk::Align::Center);
        let pic = gtk::Picture::new();
        // 2x for HiDPI; still far cheaper than decoding the full 512.
        if let Some(tex) = crate::app_icon::texture(choice.id, tile * 2) {
            pic.set_paintable(Some(&tex));
        }
        pic.set_size_request(tile, tile);
        pic.set_can_shrink(true);
        pic.set_content_fit(gtk::ContentFit::Contain);
        cell.append(&pic);
        let label = gtk::Label::new(Some(&i18n(choice.label)));
        label.add_css_class("icon-gallery-label");
        // Two-word names ("Yellow & blue") wrap onto a second line where
        // the cell is narrow (the wizard's card) rather than ellipsizing.
        label.set_wrap(true);
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.set_lines(2);
        // Reserve both lines up front: a sideways-scrolling row measures
        // its height before the labels have a width to wrap in, and a
        // second line that appears later is cut off.
        label.set_size_request(-1, 32);
        label.set_valign(gtk::Align::Start);
        label.set_max_width_chars(11);
        label.set_justify(gtk::Justification::Center);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        cell.append(&label);
        cell.set_tooltip_text(Some(&i18n(choice.label)));

        let child = gtk::FlowBoxChild::new();
        child.set_child(Some(&cell));
        child.set_widget_name(choice.id);
        grid.append(&child);
        if choice.id == selected {
            to_select = Some(child);
        }
    }
    // An empty `selected` rings nothing: the launcher shows an icon set
    // outside Hylki (#252), and every tile, the default's too, is a pick.
    let fallback = || if selected.is_empty() { None } else { grid.child_at_index(0) };
    if let Some(child) = to_select.or_else(fallback) {
        grid.select_child(&child);
    }

    // Connected after the initial selection, so only the user's picks report.
    let current = std::cell::RefCell::new(selected.to_string());
    grid.connect_selected_children_changed(move |g| {
        let Some(child) = g.selected_children().into_iter().next() else { return };
        let id = child.widget_name().to_string();
        if *current.borrow() == id {
            return;
        }
        *current.borrow_mut() = id.clone();
        on_pick(&id);
    });
    grid
}
