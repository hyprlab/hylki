//! Small colour helpers for per-account theming (avatar circles, the unified
//! list tint, and the reader account chip).

fn rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let c = gtk::gdk::RGBA::parse(hex).ok()?;
    Some((
        (c.red() * 255.0).round() as u8,
        (c.green() * 255.0).round() as u8,
        (c.blue() * 255.0).round() as u8,
    ))
}

/// A translucent ("pale") version of `hex` as a CSS colour, e.g. for row tints.
pub fn pale(hex: &str, alpha: f64) -> String {
    match rgb(hex) {
        Some((r, g, b)) => format!("rgba({r},{g},{b},{alpha})"),
        None => "transparent".to_string(),
    }
}

/// A readable text colour (black or white) for text drawn on a `hex` background.
pub fn readable_text(hex: &str) -> &'static str {
    if let Some((r, g, b)) = rgb(hex) {
        let lum = 0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64;
        if lum > 150.0 {
            return "rgba(0,0,0,0.85)";
        }
    }
    "#ffffff"
}

/// Format a `gdk::RGBA` as a `#rrggbb` string.
pub fn to_hex(c: &gtk::gdk::RGBA) -> String {
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
pub fn darken(hex: &str, amount: f64) -> String {
    let Some((r, g, b)) = rgb(hex) else {
        return hex.to_string();
    };
    let keep = (1.0 - amount).clamp(0.0, 1.0);
    let dim = |c: u8| (c as f64 * keep).round() as u8;
    format!("#{:02x}{:02x}{:02x}", dim(r), dim(g), dim(b))
}
