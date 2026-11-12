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

/// `hex` as libadwaita makes a standalone color (text, an icon) from a
/// background one, the way `accent_color` comes from `accent_bg_color`: its
/// OKLab lightness capped at 0.5 on a light ground, raised to at least 0.85
/// on a dark one. The reader has no libadwaita stylesheet to do this (#651).
pub(crate) fn standalone(hex: &str, dark: bool) -> String {
    let Some((r, g, b)) = rgb(hex) else {
        return hex.to_string();
    };
    let lin = |c: u8| {
        let c = c as f64 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin(r), lin(g), lin(b));
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    let big_l = 0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s;
    let a = 1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s;
    let bb = 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s;
    let big_l = if dark {
        big_l.max(0.85)
    } else {
        big_l.min(0.5)
    };
    let l = (big_l + 0.396_337_777_4 * a + 0.215_803_757_3 * bb).powi(3);
    let m = (big_l - 0.105_561_345_8 * a - 0.063_854_172_8 * bb).powi(3);
    let s = (big_l - 0.089_484_177_5 * a - 1.291_485_548 * bb).powi(3);
    // Out of gamut is clipped per channel, as GTK does.
    let enc = |c: f64| {
        let c = c.clamp(0.0, 1.0);
        let c = if c <= 0.003_130_8 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (c * 255.0).round() as u8
    };
    format!(
        "#{:02x}{:02x}{:02x}",
        enc(4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s),
        enc(-1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s),
        enc(-0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701 * s),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_matches_libadwaita() {
        // What GTK computes for libadwaita's own blue accent and red error.
        assert_eq!(standalone("#3584e4", false), "#0461be");
        assert_eq!(standalone("#3584e4", true), "#81d0ff");
        assert_eq!(standalone("#e01b24", false), "#c30000");
        assert_eq!(standalone("#c01c28", true), "#ff938c");
        assert_eq!(standalone("no color", true), "no color");
    }

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
