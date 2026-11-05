//! The app's look outside the mail: light or dark, the colors that follow
//! the light/dark flip, and reduced motion. The icon theme is in
//! [`crate::desktop::icon_theme`].

use adw::prelude::*;

use crate::config;

/// Force (or release) the app-wide color scheme per the appearance
/// preference — the whole chrome, not just message content, which has its own
/// setting.
pub(super) fn apply_app_theme(theme: config::AppTheme) {
    let scheme = if WIZARD_HOLDS_LIGHT.load(std::sync::atomic::Ordering::Relaxed) {
        adw::ColorScheme::ForceLight
    } else {
        match theme {
            config::AppTheme::System => adw::ColorScheme::Default,
            config::AppTheme::Light => adw::ColorScheme::ForceLight,
            config::AppTheme::Dark => adw::ColorScheme::ForceDark,
        }
    };
    adw::StyleManager::default().set_color_scheme(scheme);
}

/// Set while the welcome wizard is open. Its blue ground is designed for the
/// light scheme only, and libadwaita has no per-window scheme, so the whole
/// app is held light until the wizard closes.
pub(super) static WIZARD_HOLDS_LIGHT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Styles that branch on the color scheme, which static CSS cannot do: a
/// dedicated provider (above the static stylesheet's priority) carries the
/// scheme-dependent values and reloads whenever the scheme flips.
///
/// - The message list's selection pill: the alpha that reads right on dark is
///   heavy on a light ground, so light mode runs 25% lighter.
/// - The remote-content banner's shield: amber on dark, a deeper orange on
///   light where amber washes out.
/// - The Refresh button while offline (#442): orange, a step deeper on light.
/// - The composer's ground: the reader's deeper page shade, read from the
///   live theme (#148) rather than the stock GNOME values, so a custom theme
///   carries through to the composer like it does to the reader.
///
/// `window` is only where the theme's named colors are read from.
pub(super) fn install_scheme_css(window: &impl IsA<gtk::Widget>) {
    let provider = gtk::CssProvider::new();
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
    }
    let window = window.clone().upcast::<gtk::Widget>();
    let apply = move |provider: &gtk::CssProvider, dark: bool| {
        // The selection is the accent itself at full saturation, with the
        // accent's own foreground on it, while the list holds the keyboard. A
        // click into the reader hands focus straight back to the list, so
        // that is nearly always; when something else really has it (the
        // composer, a search entry, or focus lost in a rebuild) the row
        // turns grey, as a sidebar's does, so it is plain that the list's
        // keys will not reach it (#274). An inactive window keeps the
        // accent: focus is not lost, the window is just behind another.
        // The conversation chip goes back to its grey pill there, and a tag
        // keeps the text color picked for its fill: the white chip and the
        // window's text color left the count white on white.
        let shield = if dark { "#ffca28" } else { "#ff7800" };
        // GNOME's orange: its lighter step on dark, its deeper one on light.
        let offline = if dark { "#ffa348" } else { "#e66100" };
        // The compose surface sits on the reader's deeper page ground — the
        // same shade the threaded cards float on, as the theme defines it.
        let (_, page, _) = crate::ui::message_view::theme_grounds_for(&window, dark);
        provider.load_from_string(&format!(
            ".message-list > row:selected .message-row, \
             .message-list > row:selected:hover .message-row, \
             .message-list > row:selected:active .message-row {{ \
               background-color: @accent_bg_color; color: @accent_fg_color; }}\
             .message-list:not(:focus-within):not(:backdrop) > row:selected .message-row, \
             .message-list:not(:focus-within):not(:backdrop) > row:selected:hover .message-row, \
             .message-list:not(:focus-within):not(:backdrop) > row:selected:active .message-row {{ \
               background-color: alpha(@window_fg_color, 0.14); color: @window_fg_color; }}\
             .message-list:not(:focus-within):not(:backdrop) > row:selected .message-row label:not(.tag-chip) {{ \
               color: @window_fg_color; }}\
             .message-list:not(:focus-within):not(:backdrop) > row:selected .message-row .unread-dot {{ \
               background: @accent_bg_color; }}\
             .message-list:not(:focus-within):not(:backdrop) > row:selected .message-row .thread-chip {{ \
               background: alpha(@window_fg_color, 0.1); }}\
             .message-list:not(:focus-within):not(:backdrop) > row:selected .message-row .thread-chip image {{ \
               color: @window_fg_color; }}\
             .remote-alert image {{ color: {shield}; }}\
             .refresh-offline {{ color: {offline}; }}\
             .inline-compose-surface, .compose-pane {{ background-color: {page}; }}\
             .reader-split > separator {{ background-color: {page}; }}"
        ));
    };
    let style = adw::StyleManager::default();
    apply(&provider, style.is_dark());
    // A theme change moves the same colors without any scheme flip, so the
    // provider has to be reloadable on demand as well (AppMsg::Pref(PrefOutput::SetTheme)).
    SCHEME_REFRESH.with(|slot| {
        let provider = provider.clone();
        let apply = apply.clone();
        *slot.borrow_mut() = Some(Box::new(move || {
            apply(&provider, adw::StyleManager::default().is_dark());
        }));
    });
    style.connect_dark_notify(move |sm| {
        // The theme's named colors are re-resolved after this signal, not
        // before it: read them now and the lookup answers for the scheme
        // just left (a light-grey composer in dark mode). Apply once the
        // main loop comes round, as the reader does through its own message.
        let dark = sm.is_dark();
        let provider = provider.clone();
        let apply = apply.clone();
        gtk::glib::idle_add_local_once(move || apply(&provider, dark));
    });
}

/// Re-read the theme's colors into the scheme-dependent CSS, for a change
/// that moves them without flipping the color scheme.
pub(super) fn refresh_scheme_css() {
    SCHEME_REFRESH.with(|slot| {
        if let Some(refresh) = slot.borrow().as_ref() {
            refresh();
        }
    });
}

thread_local! {
    /// Reloads the scheme-dependent provider with the colors the live theme
    /// answers with now (see [`install_scheme_css`]).
    static SCHEME_REFRESH: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
}

/// GNOME Shell turns animations off in every app while a remote desktop
/// session is on, to spare the connection; that is not the person asking
/// for less motion. Where the app can read GNOME's own Reduce Animation
/// switch (through dconf, which the Flatpak has no access to), Hylki's
/// animations follow that switch instead, now and whenever it changes.
pub(super) fn follow_motion_setting() {
    use gtk::gio;
    if gio::SettingsBackend::default().type_().name() != "DConfSettingsBackend" {
        return;
    }
    let readable = gio::SettingsSchemaSource::default()
        .and_then(|s| s.lookup("org.gnome.desktop.interface", true))
        .is_some_and(|s| s.has_key("enable-animations"));
    let Some(gtk_settings) = gtk::Settings::default().filter(|_| readable) else {
        return;
    };
    // The binding holds the settings object for as long as GTK's lives.
    gio::Settings::new("org.gnome.desktop.interface")
        .bind("enable-animations", &gtk_settings, "gtk-enable-animations")
        .flags(gio::SettingsBindFlags::GET)
        .build();
}

/// Whether things on screen should move (see [`follow_motion_setting`]).
pub(super) fn motion_wanted() -> bool {
    gtk::Settings::default().is_some_and(|s| s.is_gtk_enable_animations())
}
