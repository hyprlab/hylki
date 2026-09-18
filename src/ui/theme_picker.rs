//! The appearance-theme gallery: one card per theme, each showing the two
//! palettes it carries — light on the left, dark on the right — with the
//! current choice ringed. The card is the theme; which of its two palettes
//! is on screen stays with the Style setting above it (follow system, light
//! or dark).
//!
//! Lives in Settings → Appearance; the caller hears every pick.

use std::rc::Rc;

use gtk::prelude::*;

use crate::i18n::i18n;
use crate::theme::{self, Palette};

/// The swatch circle's diameter, in points.
const SWATCH: i32 = 44;

/// The stock GNOME look's own swatch colours: the shades libadwaita paints
/// without a theme, and the GNOME blue it accents with. (The desktop's
/// chosen accent is not readable through libadwaita 1.4's API, which is what
/// Hylki builds against; the default is what the great majority run.)
const SYSTEM_LIGHT: (&str, &str) = ("#fafafa", "#3584e4");
const SYSTEM_DARK: (&str, &str) = ("#242424", "#3584e4");

/// Build the gallery with `selected` ringed. `on_pick` runs for every change
/// the user makes, not for the initial selection.
pub fn gallery(selected: &str, on_pick: Rc<dyn Fn(&str)>) -> gtk::FlowBox {
    let grid = gtk::FlowBox::new();
    grid.add_css_class("icon-gallery");
    grid.add_css_class("theme-gallery");
    grid.set_selection_mode(gtk::SelectionMode::Single);
    grid.set_homogeneous(true);
    grid.set_activate_on_single_click(true);
    grid.set_min_children_per_line(3);
    grid.set_max_children_per_line(3);
    grid.set_column_spacing(2);
    grid.set_row_spacing(2);
    grid.set_halign(gtk::Align::Fill);
    grid.set_hexpand(true);

    // The stock look leads: it is the default, and it is what a theme is
    // being chosen against.
    let mut cards = vec![(
        theme::SYSTEM_ID.to_string(),
        i18n("System"),
        SYSTEM_LIGHT,
        SYSTEM_DARK,
    )];
    cards.extend(theme::catalog().iter().map(|t| {
        (
            t.id.to_string(),
            // Theme names are names, not prose: they read the same in
            // every language.
            t.label.to_string(),
            swatch_colors(&t.light),
            swatch_colors(&t.dark),
        )
    }));

    let mut to_select = None;
    for (id, label, light, dark) in cards {
        let cell = gtk::Box::new(gtk::Orientation::Vertical, 6);
        cell.set_halign(gtk::Align::Center);
        let pair = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        pair.set_halign(gtk::Align::Center);
        pair.append(&swatch(light, false));
        pair.append(&swatch(dark, true));
        cell.append(&pair);

        let name = gtk::Label::new(Some(&label));
        name.add_css_class("icon-gallery-label");
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        name.set_max_width_chars(12);
        cell.append(&name);
        cell.set_tooltip_text(Some(&label));

        let child = gtk::FlowBoxChild::new();
        child.set_child(Some(&cell));
        child.set_widget_name(&id);
        grid.append(&child);
        if id == selected {
            to_select = Some(child);
        }
    }
    if let Some(child) = to_select.or_else(|| grid.child_at_index(0)) {
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

/// A palette as its swatch: the ground it paints windows with, and the
/// accent it picks things out in.
fn swatch_colors(p: &Palette) -> (&'static str, &'static str) {
    (p.canvas, p.accent)
}

/// One palette as a circle: its accent bloomed into the upper left, falling
/// away to the palette's own ground — enough of both to tell two themes
/// apart at a glance.
///
/// `dark_variant` says which of a theme's two palettes this is; the one the
/// Style setting is not currently showing fades back, so the pair reads as
/// "this is the half you are looking at, and this is the other half".
fn swatch((ground, accent): (&'static str, &'static str), dark_variant: bool) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_content_width(SWATCH);
    area.set_content_height(SWATCH);
    area.set_halign(gtk::Align::Center);
    area.set_valign(gtk::Align::Center);
    area.set_draw_func(move |area, cr, w, h| {
        let d = w.min(h) as f64;
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let base = rgb(ground);
        let bloom = mix(rgb(accent), base, 0.72);
        let edge = mix(rgb(accent), base, 0.22);
        let grad = gtk::cairo::RadialGradient::new(
            cx - d * 0.18,
            cy - d * 0.20,
            0.0,
            cx - d * 0.18,
            cy - d * 0.20,
            d,
        );
        grad.add_color_stop_rgb(0.0, bloom.0, bloom.1, bloom.2);
        grad.add_color_stop_rgb(0.62, base.0, base.1, base.2);
        grad.add_color_stop_rgb(1.0, edge.0, edge.1, edge.2);
        // Inset by the ring's width whether or not this half wears it, so
        // the two circles are always the same size.
        cr.arc(cx, cy, d / 2.0 - 4.0, 0.0, std::f64::consts::TAU);
        let _ = cr.set_source(&grad);
        let _ = cr.fill_preserve();
        // A hairline so a near-white swatch still reads as a circle on a
        // near-white card, and a near-black one on a dark card.
        let lum = 0.299 * base.0 + 0.587 * base.1 + 0.114 * base.2;
        let line = if lum > 0.5 { 0.0 } else { 1.0 };
        cr.set_source_rgba(line, line, line, 0.18);
        cr.set_line_width(1.0);
        let _ = cr.stroke();
        // The half the Style setting is showing wears a ring, so the pair
        // reads as "this is the one you are looking at, and this is the
        // other one".
        if adw::StyleManager::default().is_dark() == dark_variant {
            let fg = area.color();
            cr.arc(cx, cy, d / 2.0 - 1.5, 0.0, std::f64::consts::TAU);
            cr.set_source_rgba(fg.red() as f64, fg.green() as f64, fg.blue() as f64, 0.45);
            cr.set_line_width(2.0);
            let _ = cr.stroke();
        }
    });
    // The gallery outlives a scheme flip — the Settings window is kept warm
    // between openings — so the ring follows it. The handler goes with the
    // swatch rather than hanging off the style manager for ever.
    let handler = {
        let area = area.clone();
        adw::StyleManager::default().connect_dark_notify(move |_| area.queue_draw())
    };
    let handler = std::cell::RefCell::new(Some(handler));
    area.connect_destroy(move |_| {
        if let Some(handler) = handler.borrow_mut().take() {
            adw::StyleManager::default().disconnect(handler);
        }
    });
    area
}

/// `#rrggbb` as cairo's 0..1 components.
fn rgb(hex: &str) -> (f64, f64, f64) {
    match gtk::gdk::RGBA::parse(hex) {
        Ok(c) => (c.red() as f64, c.green() as f64, c.blue() as f64),
        Err(_) => (0.5, 0.5, 0.5),
    }
}

/// `a` over `b` at `t` (0 = all `b`).
fn mix(a: (f64, f64, f64), b: (f64, f64, f64), t: f64) -> (f64, f64, f64) {
    (
        b.0 + (a.0 - b.0) * t,
        b.1 + (a.1 - b.1) * t,
        b.2 + (a.2 - b.2) * t,
    )
}
