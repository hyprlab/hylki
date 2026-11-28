//! Drawing shared by the calendar views: the theme's colors, an event's
//! colors from its calendar's, rounded boxes and text.
//!
//! The views are drawn with Cairo rather than built from widgets: a month
//! can hold hundreds of events and a week a few dozen that move under the
//! pointer while they are dragged, and one drawing area per view keeps that
//! smooth. Colors come from the widget, so a theme or the dark style applies.

use adw::prelude::*;
use gtk::{cairo, gdk, pango};

/// A color as Cairo takes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Rgba {
    pub(super) r: f64,
    pub(super) g: f64,
    pub(super) b: f64,
    pub(super) a: f64,
}

impl Rgba {
    pub(super) const fn new(r: f64, g: f64, b: f64, a: f64) -> Rgba {
        Rgba { r, g, b, a }
    }

    pub(super) fn with_alpha(self, a: f64) -> Rgba {
        Rgba { a, ..self }
    }

    /// `#rrggbb` (or `#rgb`); grey for anything else.
    pub(super) fn hex(text: &str) -> Rgba {
        gdk::RGBA::parse(text)
            .map(Rgba::from)
            .unwrap_or(Rgba::new(0.5, 0.5, 0.5, 1.0))
    }

    /// Mixed with `other` by `t` (0 is self).
    pub(super) fn mix(self, other: Rgba, t: f64) -> Rgba {
        Rgba {
            r: self.r + (other.r - self.r) * t,
            g: self.g + (other.g - self.g) * t,
            b: self.b + (other.b - self.b) * t,
            a: self.a + (other.a - self.a) * t,
        }
    }

    pub(super) fn set(self, cr: &cairo::Context) {
        cr.set_source_rgba(self.r, self.g, self.b, self.a);
    }

    /// Relative luminance, for choosing text that reads on it.
    pub(super) fn luminance(self) -> f64 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }
}

impl From<gdk::RGBA> for Rgba {
    fn from(c: gdk::RGBA) -> Rgba {
        Rgba::new(
            f64::from(c.red()),
            f64::from(c.green()),
            f64::from(c.blue()),
            f64::from(c.alpha()),
        )
    }
}

pub(super) const WHITE: Rgba = Rgba::new(1.0, 1.0, 1.0, 1.0);
pub(super) const BLACK: Rgba = Rgba::new(0.0, 0.0, 0.0, 1.0);
/// The current-time line: GNOME's red 3.
pub(super) const NOW: Rgba = Rgba::new(0.878, 0.106, 0.141, 1.0);

/// The theme's colors, read from the widget being drawn.
#[derive(Debug, Clone, Copy)]
pub(super) struct Theme {
    pub(super) fg: Rgba,
    pub(super) dim: Rgba,
    pub(super) faint: Rgba,
    pub(super) line: Rgba,
    pub(super) shade: Rgba,
    pub(super) accent: Rgba,
    pub(super) accent_fg: Rgba,
    pub(super) dark: bool,
}

impl Theme {
    pub(super) fn of(widget: &impl IsA<gtk::Widget>) -> Theme {
        let fg = Rgba::from(widget.as_ref().color());
        let style = adw::StyleManager::default();
        let dark = style.is_dark();
        let accent = Rgba::from(style.accent_color_rgba());
        Theme {
            fg,
            dim: fg.with_alpha(0.55),
            faint: fg.with_alpha(0.32),
            line: fg.with_alpha(if dark { 0.12 } else { 0.10 }),
            shade: fg.with_alpha(if dark { 0.035 } else { 0.03 }),
            accent,
            accent_fg: if accent.luminance() > 0.6 { BLACK } else { WHITE },
            dark,
        }
    }

    /// An event's colors from its calendar's: a tinted fill, the full color
    /// for its edge, and text dark enough (or light enough) to read.
    pub(super) fn event(&self, base: Rgba, selected: bool, faded: bool) -> EventColors {
        if selected {
            let text = if base.luminance() > 0.62 { BLACK } else { WHITE };
            return EventColors {
                fill: base,
                edge: base.mix(BLACK, 0.15),
                text,
                sub: text.with_alpha(0.85),
            };
        }
        let fill_alpha = if self.dark { 0.30 } else { 0.18 };
        let text = if self.dark {
            base.mix(WHITE, 0.62)
        } else {
            base.mix(BLACK, 0.48)
        };
        let fade = if faded { 0.5 } else { 1.0 };
        EventColors {
            fill: base.with_alpha(fill_alpha * fade),
            edge: base.with_alpha(fade),
            text: text.with_alpha(fade),
            sub: text.with_alpha(0.78 * fade),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct EventColors {
    pub(super) fill: Rgba,
    pub(super) edge: Rgba,
    pub(super) text: Rgba,
    pub(super) sub: Rgba,
}

/// A rounded rectangle path.
pub(super) fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let pi = std::f64::consts::PI;
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -pi / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, pi / 2.0);
    cr.arc(x + r, y + h - r, r, pi / 2.0, pi);
    cr.arc(x + r, y + r, r, pi, 1.5 * pi);
    cr.close_path();
}

/// How a piece of text is set.
#[derive(Debug, Clone, Copy)]
pub(super) struct Font {
    /// Relative to the widget's font.
    pub(super) scale: f64,
    pub(super) weight: pango::Weight,
    /// Lines it may wrap onto; 1 ellipsizes.
    pub(super) lines: i32,
    pub(super) strike: bool,
    pub(super) tabular: bool,
}

impl Font {
    pub(super) const fn new(scale: f64) -> Font {
        Font {
            scale,
            weight: pango::Weight::Normal,
            lines: 1,
            strike: false,
            tabular: false,
        }
    }
    pub(super) const fn bold(mut self) -> Font {
        self.weight = pango::Weight::Semibold;
        self
    }
    pub(super) const fn heavy(mut self) -> Font {
        self.weight = pango::Weight::Bold;
        self
    }
    pub(super) const fn lines(mut self, n: i32) -> Font {
        self.lines = n;
        self
    }
    pub(super) const fn strike(mut self, on: bool) -> Font {
        self.strike = on;
        self
    }
    pub(super) const fn tabular(mut self) -> Font {
        self.tabular = true;
        self
    }
}

/// Lay `text` out in the widget's font.
pub(super) fn layout(
    widget: &impl IsA<gtk::Widget>,
    text: &str,
    font: Font,
    width: Option<f64>,
) -> pango::Layout {
    let layout = widget.as_ref().create_pango_layout(Some(text));
    let mut desc = widget
        .as_ref()
        .pango_context()
        .font_description()
        .unwrap_or_default();
    let size = if desc.size() > 0 {
        desc.size()
    } else {
        10 * pango::SCALE
    };
    desc.set_size((f64::from(size) * font.scale) as i32);
    desc.set_weight(font.weight);
    layout.set_font_description(Some(&desc));
    let attrs = pango::AttrList::new();
    if font.strike {
        attrs.insert(pango::AttrInt::new_strikethrough(true));
    }
    if font.tabular {
        attrs.insert(pango::AttrFontFeatures::new("tnum"));
    }
    layout.set_attributes(Some(&attrs));
    if let Some(w) = width {
        layout.set_width((w.max(1.0) * f64::from(pango::SCALE)) as i32);
        layout.set_ellipsize(pango::EllipsizeMode::End);
        if font.lines > 1 {
            layout.set_wrap(pango::WrapMode::WordChar);
            layout.set_height(-font.lines);
        }
    }
    layout
}

/// Draw `text` with its top left at `(x, y)`; answers its height.
#[allow(clippy::too_many_arguments)]
pub(super) fn text(
    cr: &cairo::Context,
    widget: &impl IsA<gtk::Widget>,
    text: &str,
    x: f64,
    y: f64,
    width: Option<f64>,
    font: Font,
    color: Rgba,
) -> f64 {
    let l = layout(widget, text, font, width);
    color.set(cr);
    cr.move_to(x, y);
    pangocairo::functions::show_layout(cr, &l);
    f64::from(l.pixel_size().1)
}

/// The pixel size of `text` as [`text`] would draw it.
pub(super) fn measure(widget: &impl IsA<gtk::Widget>, text: &str, font: Font) -> (f64, f64) {
    let (w, h) = layout(widget, text, font, None).pixel_size();
    (f64::from(w), f64::from(h))
}

/// Draw `text` centred on `(cx, y)`.
pub(super) fn centred(
    cr: &cairo::Context,
    widget: &impl IsA<gtk::Widget>,
    text_: &str,
    cx: f64,
    y: f64,
    font: Font,
    color: Rgba,
) -> (f64, f64) {
    let (w, h) = measure(widget, text_, font);
    text(cr, widget, text_, cx - w / 2.0, y, None, font, color);
    (w, h)
}

/// A filled circle.
pub(super) fn dot(cr: &cairo::Context, cx: f64, cy: f64, r: f64, color: Rgba) {
    color.set(cr);
    cr.arc(cx, cy, r, 0.0, 2.0 * std::f64::consts::PI);
    let _ = cr.fill();
}

/// Diagonal hatching over a box: a tentative or unanswered event.
pub(super) fn hatch(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, color: Rgba) {
    let _ = cr.save();
    cr.rectangle(x, y, w, h);
    cr.clip();
    color.set(cr);
    cr.set_line_width(1.5);
    let mut t = -h;
    while t < w {
        cr.move_to(x + t, y + h);
        cr.line_to(x + t + h, y);
        t += 7.0;
    }
    let _ = cr.stroke();
    let _ = cr.restore();
}

/// An event box: its fill, the stripe down its leading edge and any marks
/// for how it stands.
#[allow(clippy::too_many_arguments)]
pub(super) fn event_box(
    cr: &cairo::Context,
    colors: &EventColors,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    tentative: bool,
    stripe: bool,
) {
    rounded(cr, x, y, w, h, 5.0);
    colors.fill.set(cr);
    let _ = cr.fill();
    if tentative {
        hatch(cr, x, y, w, h, colors.edge.with_alpha(0.22));
        rounded(cr, x + 0.5, y + 0.5, w - 1.0, h - 1.0, 5.0);
        colors.edge.with_alpha(0.7).set(cr);
        cr.set_line_width(1.0);
        cr.set_dash(&[3.0, 2.0], 0.0);
        let _ = cr.stroke();
        cr.set_dash(&[], 0.0);
    }
    if stripe && w > 6.0 {
        let _ = cr.save();
        rounded(cr, x, y, w, h, 5.0);
        cr.clip();
        colors.edge.set(cr);
        let lead = if crate::i18n::ui_is_rtl() { x + w - 3.0 } else { x };
        cr.rectangle(lead, y, 3.0, h);
        let _ = cr.fill();
        let _ = cr.restore();
    }
}
