//! Small color helpers for per-account theming (avatar circles, the unified
//! list tint, and the reader account chip).

fn rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let c = gtk::gdk::RGBA::parse(hex).ok()?;
    Some((
        (c.red() * 255.0).round() as u8,
        (c.green() * 255.0).round() as u8,
        (c.blue() * 255.0).round() as u8,
    ))
}

/// A translucent ("pale") version of `hex` as a CSS color, e.g. for row tints.
pub(crate) fn pale(hex: &str, alpha: f64) -> String {
    match rgb(hex) {
        Some((r, g, b)) => format!("rgba({r},{g},{b},{alpha})"),
        None => "transparent".to_string(),
    }
}

/// A readable text color (black or white) for text drawn on a `hex` background.
pub(crate) fn readable_text(hex: &str) -> &'static str {
    if let Some((r, g, b)) = rgb(hex) {
        let lum = 0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64;
        if lum > 150.0 {
            return "rgba(0,0,0,0.85)";
        }
    }
    "#ffffff"
}

/// Format a `gdk::RGBA` as a `#rrggbb` string.
pub(crate) fn to_hex(c: &gtk::gdk::RGBA) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        (c.red() * 255.0).round() as u8,
        (c.green() * 255.0).round() as u8,
        (c.blue() * 255.0).round() as u8,
    )
}

/// `hex` pulled towards black by `amount` (0.0 leaves it alone, 1.0 is black).
///
/// Used for hairlines that a palette pitches brighter than the surfaces
/// they divide — see the theme's list/reader separator.
pub(crate) fn darken(hex: &str, amount: f64) -> String {
    let Some((r, g, b)) = rgb(hex) else {
        return hex.to_string();
    };
    let keep = (1.0 - amount).clamp(0.0, 1.0);
    let dim = |c: u8| (c as f64 * keep).round() as u8;
    format!("#{:02x}{:02x}{:02x}", dim(r), dim(g), dim(b))
}

/// WCAG relative luminance of an sRGB color, 0.0 (black) to 1.0 (white).
fn luminance((r, g, b): (u8, u8, u8)) -> f64 {
    let lin = |c: u8| {
        let c = c as f64 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// WCAG contrast ratio of two colors, 1.0 to 21.0.
fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `a` laid over `b` at `alpha`.
fn mix(a: (u8, u8, u8), b: (u8, u8, u8), alpha: f64) -> (u8, u8, u8) {
    let m = |x: u8, y: u8| (x as f64 * alpha + y as f64 * (1.0 - alpha)).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// Text in `hex` on a tint of itself at `alpha` over `ground`, kept
/// readable: the color is pulled towards black on a light ground, or white
/// on a dark one, only as far as it takes to reach `min` contrast. An
/// account color picked pale still names its account (#530).
pub(crate) fn tinted_text(hex: &str, alpha: f64, ground: &str, min: f64) -> String {
    let (Some(c), Some(g)) = (rgb(hex), rgb(ground)) else {
        return hex.to_string();
    };
    let tint = mix(c, g, alpha);
    let pole = if luminance(tint) > 0.18 {
        (0, 0, 0)
    } else {
        (255, 255, 255)
    };
    let mut text = c;
    for step in 1..=20 {
        if contrast(text, tint) >= min {
            break;
        }
        text = mix(pole, c, step as f64 / 20.0);
    }
    format!("#{:02x}{:02x}{:02x}", text.0, text.1, text.2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pale_is_the_color_at_an_alpha_or_transparent() {
        assert_eq!(pale("#ff8000", 0.5), "rgba(255,128,0,0.5)");
        assert_eq!(pale("rgb(1,2,3)", 1.0), "rgba(1,2,3,1)");
        assert_eq!(pale("no color", 0.2), "transparent");
    }

    #[test]
    fn readable_text_is_dark_only_on_a_light_ground() {
        assert_eq!(readable_text("#ffffff"), "rgba(0,0,0,0.85)");
        assert_eq!(readable_text("#ffff00"), "rgba(0,0,0,0.85)");
        assert_eq!(readable_text("#808080"), "#ffffff");
        assert_eq!(readable_text("#000000"), "#ffffff");
        assert_eq!(readable_text("no color"), "#ffffff");
    }

    #[test]
    fn darken_scales_each_channel_and_clamps_the_amount() {
        assert_eq!(darken("#ff8000", 0.5), "#804000");
        assert_eq!(darken("#102030", 2.0), "#000000");
        assert_eq!(darken("#102030", -1.0), "#102030");
        assert_eq!(darken("#102030", 0.0), "#102030");
        // Unparseable input comes back as it was.
        assert_eq!(darken("no color", 0.5), "no color");
    }

    #[test]
    fn tinted_text_keeps_a_dark_color_and_deepens_a_pale_one() {
        // Already readable: left alone.
        assert_eq!(tinted_text("#1a5fb4", 0.18, "#fafafb", 4.5), "#1a5fb4");
        // A pale yellow on a light ground goes dark enough to read.
        let t = tinted_text("#f9f06b", 0.18, "#fafafb", 4.5);
        let tint = mix(rgb("#f9f06b").unwrap(), rgb("#fafafb").unwrap(), 0.18);
        assert!(contrast(rgb(&t).unwrap(), tint) >= 4.5, "{t}");
        // A dark color on a dark ground goes light.
        let t = tinted_text("#1a2b6d", 0.18, "#222226", 4.5);
        let tint = mix(rgb("#1a2b6d").unwrap(), rgb("#222226").unwrap(), 0.18);
        assert!(contrast(rgb(&t).unwrap(), tint) >= 4.5, "{t}");
        assert_eq!(tinted_text("no color", 0.18, "#ffffff", 4.5), "no color");
    }

    #[test]
    fn to_hex_rounds_each_channel() {
        assert_eq!(to_hex(&gtk::gdk::RGBA::new(1.0, 0.5, 0.0, 0.3)), "#ff8000");
        assert_eq!(to_hex(&gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 1.0)), "#000000");
    }
}
