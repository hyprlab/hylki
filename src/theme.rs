//! Appearance themes: whole palettes the app can wear, on top of the
//! light/dark preference.
//!
//! Hylki normally paints itself in the system's own colours — stock
//! libadwaita plus the desktop accent — and that is still the default
//! ([`SYSTEM_ID`]). A theme replaces those colours wholesale: each one
//! carries a light and a dark palette, so "Follow system", "Light" and
//! "Dark" keep working exactly as before and simply pick which of the two
//! is on screen.
//!
//! The palettes themselves are T3 Code's theme library (MIT), converted to
//! sRGB by `tools/gen-themes.py` into [`crate::theme_palettes`]. This module
//! maps their product roles (canvas, surface, sidebar…) onto libadwaita's
//! named colours and keeps a single CSS provider up to date with the active
//! palette.
//!
//! Everything downstream follows from those named colours: the static
//! stylesheet references them (`@window_bg_color` and friends), and the
//! reader, the composer and the scheme-dependent CSS all read them back off
//! the live theme (`message_view::theme_grounds_for`), so a theme reaches
//! message content too.

use std::cell::RefCell;

use crate::theme_palettes::THEMES;

/// The stock GNOME look: no palette of our own, just libadwaita and the
/// desktop's accent colour. The default, and what "no theme" saves as.
pub const SYSTEM_ID: &str = "system";

/// One palette — a theme's colours for a single appearance (light or dark).
///
/// The roles are T3 Code's own; [`css`] is where they become libadwaita
/// colours. Fields are kept in the generator's order, and the whole palette
/// is carried even where nothing reads a role yet: it is a faithful copy of
/// the upstream theme, and the next surface that needs a colour should find
/// it here rather than invent one.
#[allow(dead_code)]
pub struct Palette {
    pub canvas: &'static str,
    pub chrome: &'static str,
    pub toolbar: &'static str,
    pub toolbar_foreground: &'static str,
    pub toolbar_border: &'static str,
    pub toolbar_control: &'static str,
    pub toolbar_control_foreground: &'static str,
    pub toolbar_control_hover: &'static str,
    pub surface: &'static str,
    pub surface_raised: &'static str,
    pub surface_overlay: &'static str,
    pub text: &'static str,
    pub text_muted: &'static str,
    pub border: &'static str,
    pub input: &'static str,
    pub focus: &'static str,
    pub accent: &'static str,
    pub accent_foreground: &'static str,
    pub secondary: &'static str,
    pub secondary_foreground: &'static str,
    pub muted: &'static str,
    pub muted_foreground: &'static str,
    pub placeholder: &'static str,
    pub error: &'static str,
    pub error_foreground: &'static str,
    pub error_surface: &'static str,
    pub warning: &'static str,
    pub warning_foreground: &'static str,
    pub warning_surface: &'static str,
    pub accent_surface: &'static str,
    pub accent_surface_foreground: &'static str,
    pub message_surface: &'static str,
    pub code_background: &'static str,
    pub code_foreground: &'static str,
    pub sidebar: &'static str,
    pub sidebar_foreground: &'static str,
    pub sidebar_muted_foreground: &'static str,
    pub sidebar_control_surface: &'static str,
    pub sidebar_row_hover: &'static str,
    pub sidebar_row_active: &'static str,
    pub sidebar_row_selected: &'static str,
    pub sidebar_border: &'static str,
}

/// A theme: a name and its two palettes.
pub struct Theme {
    pub id: &'static str,
    pub label: &'static str,
    pub light: Palette,
    pub dark: Palette,
}

impl Theme {
    /// The palette for the appearance the app is in.
    pub fn variant(&self, dark: bool) -> &Palette {
        if dark {
            &self.dark
        } else {
            &self.light
        }
    }
}

/// Every bundled theme, in picker order. The stock look is not in here: it
/// is the absence of a theme, and the picker lists it first itself.
pub fn catalog() -> &'static [Theme] {
    THEMES
}

/// The theme with this id, or `None` for the stock look (and for an id from
/// a newer version's catalogue, which falls back to it).
pub fn find(id: &str) -> Option<&'static Theme> {
    if id.is_empty() || id == SYSTEM_ID {
        return None;
    }
    THEMES.iter().find(|t| t.id == id)
}

thread_local! {
    /// The provider carrying the active palette. Empty for the stock look.
    static PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
    /// The active theme's id, as saved.
    static CURRENT: RefCell<String> = const { RefCell::new(String::new()) };
    /// Who wants telling when the theme changes — the reader and the
    /// composer, which bake colours into their documents. A watcher that
    /// returns `false` (its widget is gone) is dropped.
    static WATCHERS: RefCell<Vec<Box<dyn Fn() -> bool>>> = const { RefCell::new(Vec::new()) };
}

/// Install the theme provider and paint `id`'s palette.
///
/// Call once, early in app startup and *before* anything else listens for
/// the colour scheme: the palette has to be in place by the time the
/// scheme-dependent CSS and the reader read the theme's colours back on a
/// light/dark flip, and GTK runs `notify::dark` handlers in the order they
/// were connected.
pub fn install(id: &str) {
    let provider = gtk::CssProvider::new();
    if let Some(display) = gtk::gdk::Display::default() {
        // Above the static stylesheet (APPLICATION) and the scheme-dependent
        // provider (+1), so a theme's colours win wherever they are defined.
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 2,
        );
    }
    PROVIDER.with(|p| *p.borrow_mut() = Some(provider));
    CURRENT.with(|c| *c.borrow_mut() = id.to_string());
    repaint();
    // A light/dark flip swaps which of the theme's two palettes is on.
    adw::StyleManager::default().connect_dark_notify(|_| repaint());
}

/// Switch to `id` (or [`SYSTEM_ID`] for the stock look) and tell everything
/// that has colours baked into it.
pub fn set(id: &str) {
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
pub fn current() -> String {
    CURRENT.with(|c| c.borrow().clone())
}

/// The palette on screen for `dark`, or `None` under the stock look.
pub fn palette(dark: bool) -> Option<&'static Palette> {
    find(&current()).map(|t| t.variant(dark))
}

/// Run `f` whenever the theme changes; it is dropped once it returns
/// `false`, which is how a widget that has since been destroyed drops out.
/// A light/dark flip does *not* call these — listeners for that already
/// have libadwaita's own `notify::dark`.
pub fn connect_changed(f: impl Fn() -> bool + 'static) {
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

/// A palette as libadwaita's named colours, plus the one rule that needs
/// writing out.
///
/// Only the backgrounds and foregrounds are set: `accent_color`,
/// `destructive_color` and the other "on a normal background" variants are
/// derived by libadwaita from the ones below, which keeps text legible on
/// palettes whose accent is very light or very dark.
///
/// The divider between the message list and the reading pane is the
/// exception: a GtkPaned separator takes no named colour, so it is painted
/// here by class. (The sidebar's own divider needs nothing — it is an
/// Adwaita split view, and already follows `sidebar_border_color`.) The
/// border and shadow resets are load-bearing: libadwaita draws the
/// separator's hairline as a shadow, which would otherwise sit over the
/// palette colour and lighten it. It is painted a fifth darker than the
/// palette's border role, which every theme pitches brighter than the list
/// and the reader on either side of it. Because this string is empty under
/// the stock look, none of it reaches an unthemed window.
///
/// The sidebar's divider keeps libadwaita's own values rather than the
/// palette's `sidebar_border`: every theme's tint is lighter than the
/// sidebar behind it, which drew a bright line down the window beside the
/// rail. Black at the stock opacity reads as a shadow, the way the system
/// look does.
fn css(p: &Palette, dark: bool) -> String {
    // libadwaita's sidebar-border/-shade colours, light and dark.
    let (sidebar_border, sidebar_shade) = if dark {
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
@define-color sidebar_shade_color {sidebar_shade};\
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
@define-color shade_color {border};\
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

