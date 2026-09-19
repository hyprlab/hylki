//! The appearance-theme gallery: one card per theme, each showing the two
//! palettes it carries as a pair of window thumbnails — light on the left,
//! dark on the right — with the current choice highlighted. The card is
//! the theme; which of its two palettes is on screen stays with the Style
//! setting above it (follow system, light or dark).
//!
//! The thumbnails follow the GNOME Settings appearance chooser: a tiny
//! window with a header bar, a sidebar with a selected row, and a raised
//! card in the content pane, so every surface a theme colours is on show
//! at once.
//!
//! Lives in Settings → Appearance; the caller hears every pick.

use std::rc::Rc;

use gtk::prelude::*;

use crate::i18n::i18n;
use crate::theme::{self, Palette};

/// The thumbnail window's size, in points.
const THUMB_W: f64 = 70.0;
const THUMB_H: f64 = 50.0;
/// Its corner radius.
const THUMB_RADIUS: f64 = 6.0;
/// Air around the thumbnail for its shadow to fall in: the card shadow's
/// deepest layer reaches 10pt below the edge, but is all but gone past 6.
const THUMB_INSET: f64 = 6.0;

/// libadwaita's `.card` shadow — the one the Settings groups around this
/// gallery wear — as (dy, blur, spread, alpha) layers, so a thumbnail lifts
/// off the card exactly as the card lifts off the window. Black at the
/// stylesheet's `rgb(0 0 6)`.
const CARD_SHADOW: [(f64, f64, f64, f64); 3] = [
    (0.0, 0.0, 1.0, 0.03),
    (1.0, 3.0, 1.0, 0.07),
    (2.0, 6.0, 2.0, 0.03),
];

/// The surfaces one thumbnail paints, as `#rrggbb`, in the roles the
/// thumbnail draws them in.
#[derive(Clone, Copy)]
struct Mini {
    /// The window ground behind everything.
    window: &'static str,
    /// The header bar.
    header: &'static str,
    /// The sidebar.
    sidebar: &'static str,
    /// The content pane, and the raised card on it.
    view: &'static str,
    card: &'static str,
    /// The hairlines between surfaces.
    border: &'static str,
    /// The accent: a theme's own, or `None` for whatever accent the desktop
    /// is set to, read as the thumbnail draws.
    accent: Option<&'static str>,
    accent_fg: &'static str,
}

/// The stock GNOME look, light and dark: the colours libadwaita paints
/// without a theme (translucent ones flattened onto their grounds). The
/// accent is not one of them — the System card shows whatever accent the
/// desktop is set to, so the card shows the look it stands for rather than
/// always GNOME blue.
const SYSTEM_LIGHT: Mini = Mini {
    window: "#fafafa",
    header: "#ffffff",
    sidebar: "#ebebed",
    view: "#ffffff",
    card: "#ffffff",
    border: "#d9d9d9",
    accent: None,
    accent_fg: "#ffffff",
};
const SYSTEM_DARK: Mini = Mini {
    window: "#242424",
    header: "#303030",
    sidebar: "#303030",
    view: "#1e1e1e",
    card: "#363636",
    border: "#454545",
    accent: None,
    accent_fg: "#ffffff",
};

/// Build the gallery with `selected` highlighted. `on_pick` runs for every change
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
            mini(&t.light),
            mini(&t.dark),
        )
    }));

    let mut to_select = None;
    for (id, label, light, dark) in cards {
        let cell = gtk::Box::new(gtk::Orientation::Vertical, 6);
        cell.set_halign(gtk::Align::Center);
        let pair = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        pair.set_halign(gtk::Align::Center);
        pair.append(&thumbnail(light));
        pair.append(&thumbnail(dark));
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

/// A palette's surfaces in the roles the thumbnail paints — the same
/// mapping `theme::css` hands libadwaita, so the thumbnail is the window
/// in miniature.
fn mini(p: &Palette) -> Mini {
    Mini {
        window: p.chrome,
        header: p.toolbar,
        sidebar: p.sidebar,
        view: p.surface,
        card: p.surface_raised,
        border: p.border,
        accent: Some(p.accent),
        accent_fg: p.accent_foreground,
    }
}

/// One palette as a window thumbnail: header bar, sidebar with a selected
/// row, and a card with an accent button in the content pane. Which half
/// the Style setting is showing needs no marking: a light window and a dark
/// one tell themselves apart.
fn thumbnail(m: Mini) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_content_width((THUMB_W + 2.0 * THUMB_INSET) as i32);
    area.set_content_height((THUMB_H + 2.0 * THUMB_INSET) as i32);
    area.set_halign(gtk::Align::Center);
    area.set_valign(gtk::Align::Center);
    area.set_draw_func(move |_, cr, w, h| {
        let (x0, y0) = (
            ((w as f64 - THUMB_W) / 2.0).round(),
            ((h as f64 - THUMB_H) / 2.0).round(),
        );
        let accent = match m.accent {
            Some(hex) => rgb(hex),
            // The desktop's own accent, as libadwaita paints its buttons.
            None => {
                let c = adw::StyleManager::default().accent_color().to_rgba();
                (c.red() as f64, c.green() as f64, c.blue() as f64)
            }
        };

        for (dy, blur, spread, alpha) in CARD_SHADOW {
            shadow(cr, x0, y0 + dy, THUMB_W, THUMB_H, THUMB_RADIUS, blur, spread, alpha);
        }

        // Everything inside is clipped to the window's rounded shape.
        let _ = cr.save();
        rounded(cr, x0, y0, THUMB_W, THUMB_H, THUMB_RADIUS);
        cr.clip();

        fill(cr, x0, y0, THUMB_W, THUMB_H, rgb(m.window), 1.0);

        // The header bar, over a hairline.
        let header_h = 11.0;
        fill(cr, x0, y0, THUMB_W, header_h, rgb(m.header), 1.0);
        fill(cr, x0, y0 + header_h - 0.5, THUMB_W, 0.5, rgb(m.border), 1.0);

        // The sidebar, with one row selected in the accent.
        let side_w = 24.0;
        let body_y = y0 + header_h;
        fill(cr, x0, body_y, side_w, THUMB_H - header_h, rgb(m.sidebar), 1.0);
        fill(cr, x0 + side_w - 0.5, body_y, 0.5, THUMB_H - header_h, rgb(m.border), 1.0);
        let row_h = 7.0;
        rounded(cr, x0 + 2.5, body_y + 4.0 + row_h - 1.5, side_w - 5.0, row_h - 1.0, 2.0);
        cr.set_source_rgba(accent.0, accent.1, accent.2, 0.22);
        let _ = cr.fill();

        // The content pane, with a raised card and an accent button.
        fill(cr, x0 + side_w, body_y, THUMB_W - side_w, THUMB_H - header_h, rgb(m.view), 1.0);
        let (cx, cy, cw, ch) = (x0 + side_w + 5.0, body_y + 5.0, THUMB_W - side_w - 10.0, THUMB_H - header_h - 10.0);
        rounded(cr, cx, cy, cw, ch, 3.5);
        let card = rgb(m.card);
        cr.set_source_rgb(card.0, card.1, card.2);
        let _ = cr.fill_preserve();
        let border = rgb(m.border);
        cr.set_source_rgba(border.0, border.1, border.2, 0.9);
        cr.set_line_width(0.75);
        let _ = cr.stroke();
        let (bw, bh) = (15.0, 6.5);
        rounded(cr, cx + cw - bw - 3.5, cy + ch - bh - 3.5, bw, bh, 3.25);
        cr.set_source_rgb(accent.0, accent.1, accent.2);
        let _ = cr.fill();
        pill(cr, cx + cw - bw - 3.5 + 4.0, cy + ch - bh - 3.5 + 2.5, bw - 8.0, 1.5, rgb(m.accent_fg), 0.9);

        let _ = cr.restore();

        // The window's own edge: a hairline so a light window still reads
        // as a window on a light card, and a dark one on a dark card.
        rounded(cr, x0 + 0.5, y0 + 0.5, THUMB_W - 1.0, THUMB_H - 1.0, THUMB_RADIUS - 0.5);
        let window = rgb(m.window);
        let lum = 0.299 * window.0 + 0.587 * window.1 + 0.114 * window.2;
        let line = if lum > 0.5 { 0.0 } else { 1.0 };
        cr.set_source_rgba(line, line, line, 0.16);
        cr.set_line_width(1.0);
        let _ = cr.stroke();
    });
    // The System card outlives an accent change — the Settings window is
    // kept warm between openings — so its thumbnails follow it. The handler
    // goes with the thumbnail rather than hanging off the style manager for
    // ever.
    let handler = {
        let area = area.clone();
        adw::StyleManager::default().connect_accent_color_notify(move |_| area.queue_draw())
    };
    let handler = std::cell::RefCell::new(Some(handler));
    area.connect_destroy(move |_| {
        if let Some(handler) = handler.borrow_mut().take() {
            adw::StyleManager::default().disconnect(handler);
        }
    });
    area
}

/// A rounded-rectangle path.
fn rounded(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::PI;
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
    cr.close_path();
}

/// One layer of a CSS box-shadow under a rounded rectangle: `spread` grows
/// the shape, `blur` fades its edge out over that many points. Cairo has
/// no blur, so the fade is stacked rings, each carrying a share of `alpha`
/// so the solid middle adds up to it exactly.
fn shadow(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64, blur: f64, spread: f64, alpha: f64) {
    let rings = (blur * 2.0).ceil().max(1.0) as usize;
    let share = 1.0 - (1.0 - alpha).powf(1.0 / rings as f64);
    for i in 0..rings {
        // From the outermost, faintest reach to the innermost, all overlapping.
        let reach = spread + blur * (1.0 - i as f64 / rings as f64);
        rounded(cr, x - reach, y - reach, w + 2.0 * reach, h + 2.0 * reach, r + reach);
        cr.set_source_rgba(0.0, 0.0, 6.0 / 255.0, share);
        let _ = cr.fill();
    }
}

/// A filled rectangle.
fn fill(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, c: (f64, f64, f64), alpha: f64) {
    cr.rectangle(x, y, w, h);
    cr.set_source_rgba(c.0, c.1, c.2, alpha);
    let _ = cr.fill();
}

/// A fully rounded bar — a line of text, in miniature.
fn pill(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, c: (f64, f64, f64), alpha: f64) {
    rounded(cr, x, y, w, h, h / 2.0);
    cr.set_source_rgba(c.0, c.1, c.2, alpha);
    let _ = cr.fill();
}

/// `#rrggbb` as cairo's 0..1 components.
fn rgb(hex: &str) -> (f64, f64, f64) {
    match gtk::gdk::RGBA::parse(hex) {
        Ok(c) => (c.red() as f64, c.green() as f64, c.blue() as f64),
        Err(_) => (0.5, 0.5, 0.5),
    }
}
