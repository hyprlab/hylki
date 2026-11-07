//! Appearance themes: whole palettes the app can wear, on top of the
//! light/dark preference.
//!
//! Hylki normally paints itself in the system's own colors — stock
//! libadwaita plus the desktop accent — and that is still the default
//! ([`SYSTEM_ID`]). A theme replaces those colors wholesale: each one
//! carries a light and a dark palette, so "Follow system", "Light" and
//! "Dark" keep working exactly as before and simply pick which of the two
//! is on screen.
//!
//! The palettes themselves are T3 Code's theme library (MIT), converted to
//! sRGB by `tools/gen-themes.py` into [`crate::desktop::theme_palettes`]. This module
//! maps their product roles (canvas, surface, sidebar…) onto libadwaita's
//! named colors and keeps a single CSS provider up to date with the active
//! palette.
//!
//! Everything downstream follows from those named colors: the static
//! stylesheet references them (`@window_bg_color` and friends), and the
//! reader, the composer and the scheme-dependent CSS all read them back off
//! the live theme (`message_view::theme_grounds_for`), so a theme reaches
//! message content too.

use std::cell::RefCell;

use crate::desktop::theme_custom::THEMES as CUSTOM_THEMES;
use crate::desktop::theme_palettes::THEMES;

/// The stock GNOME look: no palette of our own, just libadwaita and the
/// desktop's accent color. The default, and what "no theme" saves as.
pub(crate) const SYSTEM_ID: &str = "system";

/// One palette — a theme's colors for a single appearance (light or dark).
///
/// The roles are T3 Code's own; [`css`] is where they become libadwaita
/// colors. Fields are kept in the generator's order, and the whole palette
/// is carried even where nothing reads a role yet: it is a faithful copy of
/// the upstream theme, and the next surface that needs a color should find
/// it here rather than invent one.
#[allow(dead_code)]
pub(crate) struct Palette {
    pub(crate) canvas: &'static str,
    pub(crate) chrome: &'static str,
    pub(crate) toolbar: &'static str,
    pub(crate) toolbar_foreground: &'static str,
    pub(crate) toolbar_border: &'static str,
    pub(crate) toolbar_control: &'static str,
    pub(crate) toolbar_control_foreground: &'static str,
    pub(crate) toolbar_control_hover: &'static str,
    pub(crate) surface: &'static str,
    pub(crate) surface_raised: &'static str,
    pub(crate) surface_overlay: &'static str,
    pub(crate) text: &'static str,
    pub(crate) text_muted: &'static str,
    pub(crate) border: &'static str,
    pub(crate) input: &'static str,
    pub(crate) focus: &'static str,
    pub(crate) accent: &'static str,
    pub(crate) accent_foreground: &'static str,
    pub(crate) secondary: &'static str,
    pub(crate) secondary_foreground: &'static str,
    pub(crate) muted: &'static str,
    pub(crate) muted_foreground: &'static str,
    pub(crate) placeholder: &'static str,
    pub(crate) error: &'static str,
    pub(crate) error_foreground: &'static str,
    pub(crate) error_surface: &'static str,
    pub(crate) warning: &'static str,
    pub(crate) warning_foreground: &'static str,
    pub(crate) warning_surface: &'static str,
    pub(crate) accent_surface: &'static str,
    pub(crate) accent_surface_foreground: &'static str,
    pub(crate) message_surface: &'static str,
    pub(crate) code_background: &'static str,
    pub(crate) code_foreground: &'static str,
    pub(crate) sidebar: &'static str,
    pub(crate) sidebar_foreground: &'static str,
    pub(crate) sidebar_muted_foreground: &'static str,
    pub(crate) sidebar_control_surface: &'static str,
    pub(crate) sidebar_row_hover: &'static str,
    pub(crate) sidebar_row_active: &'static str,
    pub(crate) sidebar_row_selected: &'static str,
    pub(crate) sidebar_border: &'static str,
}

/// A theme: a name and its two palettes.
pub(crate) struct Theme {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) light: Palette,
    pub(crate) dark: Palette,
}

impl Theme {
    /// The palette for the appearance the app is in.
    pub(crate) fn variant(&self, dark: bool) -> &Palette {
        if dark {
            &self.dark
        } else {
            &self.light
        }
    }
}

/// Every bundled theme, in picker order. The stock look is not in here: it
/// is the absence of a theme, and the picker lists it first itself.
pub(crate) fn catalog() -> impl Iterator<Item = &'static Theme> {
    THEMES.iter().chain(CUSTOM_THEMES.iter())
}

/// The theme with this id, or `None` for the stock look (and for an id from
/// a newer version's catalogue, which falls back to it).
fn find(id: &str) -> Option<&'static Theme> {
    if id.is_empty() || id == SYSTEM_ID {
        return None;
    }
    catalog().find(|t| t.id == id)
}

thread_local! {
    /// The provider carrying the active palette. Empty for the stock look.
    static PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
    /// The active theme's id, as saved.
    static CURRENT: RefCell<String> = const { RefCell::new(String::new()) };
    /// Who wants telling when the theme changes — the reader and the
    /// composer, which bake colors into their documents. A watcher that
    /// returns `false` (its widget is gone) is dropped.
    static WATCHERS: RefCell<Vec<Box<dyn Fn() -> bool>>> = const { RefCell::new(Vec::new()) };
}

/// Install the theme provider and paint `id`'s palette.
///
/// Call once, early in app startup and *before* anything else listens for
/// the color scheme: the palette has to be in place by the time the
/// scheme-dependent CSS and the reader read the theme's colors back on a
/// light/dark flip, and GTK runs `notify::dark` handlers in the order they
/// were connected.
pub(crate) fn install(id: &str) {
    let provider = gtk::CssProvider::new();
    if let Some(display) = gtk::gdk::Display::default() {
        // Above the static stylesheet and the scheme-dependent provider, so
        // a theme's colors win wherever they are defined, and above the
        // user's gtk.css, whose named colors from a theming tool would
        // otherwise hide the theme picked here (#593). Under the stock look
        // this provider is empty, so that file still colors the app.
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER + 1,
        );
    }
    PROVIDER.with(|p| *p.borrow_mut() = Some(provider));
    CURRENT.with(|c| *c.borrow_mut() = id.to_string());
    repaint();
    // A light/dark flip swaps which of the theme's two palettes is on.
    adw::StyleManager::default().connect_dark_notify(|_| repaint());
}

/// Switch to `id` (or [`SYSTEM_ID`] for the stock look) and tell everything
/// that has colors baked into it.
pub(crate) fn set(id: &str) {
    let changed = CURRENT.with(|c| {
        if *c.borrow() == id {
            return false;
        }
        *c.borrow_mut() = id.to_string();
        true
    });
    if !changed {
        return;
    }
    repaint();
    WATCHERS.with(|w| w.borrow_mut().retain(|watch| watch()));
}

/// The active theme's id (`""` before `install`, [`SYSTEM_ID`] for stock).
fn current() -> String {
    CURRENT.with(|c| c.borrow().clone())
}

/// The palette on screen for `dark`, or `None` under the stock look.
fn palette(dark: bool) -> Option<&'static Palette> {
    find(&current()).map(|t| t.variant(dark))
}

/// Run `f` whenever the theme changes; it is dropped once it returns
/// `false`, which is how a widget that has since been destroyed drops out.
/// A light/dark flip does *not* call these — listeners for that already
/// have libadwaita's own `notify::dark`.
pub(crate) fn connect_changed(f: impl Fn() -> bool + 'static) {
    WATCHERS.with(|w| w.borrow_mut().push(Box::new(f)));
}

/// Load the active palette (or nothing at all) into the provider.
fn repaint() {
    let dark = adw::StyleManager::default().is_dark();
    let css = palette(dark).map(|p| css(p, dark)).unwrap_or_default();
    PROVIDER.with(|p| {
        if let Some(provider) = p.borrow().as_ref() {
            provider.load_from_string(&css);
        }
    });
}

/// A palette as libadwaita's named colors, plus the one rule that needs
/// writing out.
///
/// Only the backgrounds and foregrounds are set: `accent_color`,
/// `destructive_color` and the other "on a normal background" variants are
/// derived by libadwaita from the ones below, which keeps text legible on
/// palettes whose accent is very light or very dark.
///
/// The divider between the message list and the reading pane is the
/// exception: a GtkPaned separator takes no named color, so it is painted
/// here by class. (The sidebar's own divider needs nothing — it is an
/// Adwaita split view, and already follows `sidebar_border_color`.) The
/// border and shadow resets are load-bearing: libadwaita draws the
/// separator's hairline as a shadow, which would otherwise sit over the
/// palette color and lighten it. It is painted a fifth darker than the
/// palette's border role, which every theme pitches brighter than the list
/// and the reader on either side of it. Because this string is empty under
/// the stock look, none of it reaches an unthemed window.
///
/// The sidebar's divider keeps libadwaita's own values rather than the
/// palette's `sidebar_border`: every theme's tint is lighter than the
/// sidebar behind it, which drew a bright line down the window beside the
/// rail. Black at the stock opacity reads as a shadow, the way the system
/// look does.
///
/// `shade_color` is black at libadwaita's own opacity for the same reason,
/// and it is not optional: it is not a line color at all but the color
/// libadwaita dims and shadows *with*. It paints the scrim behind the
/// hovering sidebar (`overlay-split-view > dimming`) and that panel's drop
/// shadow, so a palette's opaque `border` there slid a solid sheet over the
/// whole window instead of letting it show through.
fn css(p: &Palette, dark: bool) -> String {
    // libadwaita's sidebar-border and shade colors, light and dark. The
    // sidebar's shade and the general one happen to share a value.
    let (sidebar_border, shade) = if dark {
        ("rgba(0, 0, 0, 0.36)", "rgba(0, 0, 0, 0.25)")
    } else {
        ("rgba(0, 0, 0, 0.07)", "rgba(0, 0, 0, 0.07)")
    };
    format!(
        "\
@define-color window_bg_color {chrome};\
@define-color window_fg_color {text};\
@define-color view_bg_color {surface};\
@define-color view_fg_color {text};\
@define-color headerbar_bg_color {toolbar};\
@define-color headerbar_fg_color {toolbar_fg};\
@define-color headerbar_backdrop_color {chrome};\
@define-color headerbar_border_color {toolbar_fg};\
@define-color headerbar_shade_color {border};\
@define-color headerbar_darker_shade_color {border};\
@define-color sidebar_bg_color {sidebar};\
@define-color sidebar_fg_color {sidebar_fg};\
@define-color sidebar_backdrop_color {sidebar};\
@define-color sidebar_border_color {sidebar_border};\
@define-color sidebar_shade_color {shade};\
@define-color secondary_sidebar_bg_color {surface};\
@define-color secondary_sidebar_fg_color {text};\
@define-color secondary_sidebar_backdrop_color {surface};\
@define-color secondary_sidebar_border_color {border};\
@define-color secondary_sidebar_shade_color {border};\
@define-color card_bg_color {raised};\
@define-color card_fg_color {text};\
@define-color card_shade_color {border};\
@define-color dialog_bg_color {overlay};\
@define-color dialog_fg_color {text};\
@define-color popover_bg_color {overlay};\
@define-color popover_fg_color {text};\
@define-color popover_shade_color {border};\
@define-color thumbnail_bg_color {raised};\
@define-color thumbnail_fg_color {text};\
@define-color accent_bg_color {accent};\
@define-color accent_fg_color {accent_fg};\
@define-color destructive_bg_color {error};\
@define-color destructive_fg_color {on_error};\
@define-color error_bg_color {error};\
@define-color error_fg_color {on_error};\
@define-color warning_bg_color {warning};\
@define-color warning_fg_color {on_warning};\
@define-color shade_color {shade};\
@define-color scrollbar_outline_color {surface};\
.mail-split > separator {{background-color: {separator};\
border: none;box-shadow: none;outline: none;}}",
        chrome = p.chrome,
        text = p.text,
        surface = p.surface,
        toolbar = p.toolbar,
        toolbar_fg = p.toolbar_foreground,
        border = p.border,
        separator = crate::color::darken(p.border, 0.2),
        sidebar = p.sidebar,
        sidebar_fg = p.sidebar_foreground,
        raised = p.surface_raised,
        overlay = p.surface_overlay,
        accent = p.accent,
        accent_fg = p.accent_foreground,
        error = p.error,
        on_error = crate::color::readable_text(p.error),
        warning = p.warning,
        on_warning = crate::color::readable_text(p.warning),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{catalog, CUSTOM_THEMES};

    #[test]
    fn the_theme_picker_has_unique_themes_and_both_appearances() {
        let ids: HashSet<_> = catalog().map(|theme| theme.id).collect();
        assert_eq!(ids.len(), catalog().count());
        assert!(ids.contains("mist"));
        assert!(ids.contains("sage"));
        assert!(ids.contains("stone"));
        assert!(ids.contains("cloud"));
        assert!(ids.contains("alabaster"));
        assert!(ids.contains("graphite"));
        assert!(ids.contains("harbor"));
        assert!(ids.contains("linen"));
        for theme in CUSTOM_THEMES {
            assert_ne!(theme.light.canvas, theme.dark.canvas);
            assert_ne!(theme.light.text, theme.dark.text);
            assert_ne!(theme.light.accent, theme.dark.accent);
        }
    }

    #[test]
    fn working_themes_keep_their_surfaces_neutral() {
        for id in ["cloud", "alabaster", "graphite", "harbor", "linen"] {
            let theme = CUSTOM_THEMES
                .iter()
                .find(|theme| theme.id == id)
                .unwrap_or_else(|| panic!("missing {id}"));
            for palette in [&theme.light, &theme.dark] {
                for (role, hex) in [
                    ("canvas", palette.canvas),
                    ("chrome", palette.chrome),
                    ("toolbar", palette.toolbar),
                    ("surface", palette.surface),
                    ("sidebar", palette.sidebar),
                ] {
                    let channels = [
                        u8::from_str_radix(&hex[1..3], 16).unwrap(),
                        u8::from_str_radix(&hex[3..5], 16).unwrap(),
                        u8::from_str_radix(&hex[5..7], 16).unwrap(),
                    ];
                    let spread = channels.iter().max().unwrap() - channels.iter().min().unwrap();
                    assert!(spread <= 8, "{id} {role} is tinted too strongly: {hex}");
                }
            }
        }
    }

    #[test]
    fn calm_themes_keep_text_and_buttons_legible() {
        fn luminance(hex: &str) -> f64 {
            let channel = |at: usize| {
                let value = u8::from_str_radix(&hex[at..at + 2], 16).unwrap() as f64 / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5)
        }
        fn contrast(a: &str, b: &str) -> f64 {
            let (bright, dark) = {
                let (a, b) = (luminance(a), luminance(b));
                if a > b {
                    (a, b)
                } else {
                    (b, a)
                }
            };
            (bright + 0.05) / (dark + 0.05)
        }

        for theme in CUSTOM_THEMES {
            for palette in [&theme.light, &theme.dark] {
                assert!(contrast(palette.text, palette.canvas) >= 7.0);
                assert!(contrast(palette.accent, palette.accent_foreground) >= 4.5);
            }
        }
    }
}
