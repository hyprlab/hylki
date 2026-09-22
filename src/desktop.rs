//! What the app has to ask the desktop itself, over GTK's head.
//!
//! Its settings (GNOME's `org.gnome.desktop.interface` and kin), which the
//! locale and GTK know nothing about: the clock format (#173), the
//! monospace font (#181). Read through the settings portal, which works
//! inside the Flatpak sandbox and on the host alike, or — outside the
//! sandbox, when no portal answers — straight from GSettings where the
//! schema is installed.
//!
//! And its window manager, when the window has to come to the front and
//! `present` is not enough to get it there: see [`present_window`].

/// The string value of `key` in `namespace`, if the desktop has one.
pub fn setting(namespace: &str, key: &str) -> Option<String> {
    fn from_value(v: zbus::zvariant::Value<'_>) -> Option<String> {
        match v {
            zbus::zvariant::Value::Value(inner) => from_value(*inner),
            zbus::zvariant::Value::Str(s) => Some(s.as_str().to_string()),
            _ => None,
        }
    }
    let portal = (|| -> Option<String> {
        let conn = zbus::blocking::Connection::session().ok()?;
        let reply = conn
            .call_method(
                Some("org.freedesktop.portal.Desktop"),
                "/org/freedesktop/portal/desktop",
                Some("org.freedesktop.portal.Settings"),
                "ReadOne",
                &(namespace, key),
            )
            .ok()?;
        let body = reply.body();
        let v: zbus::zvariant::OwnedValue = body.deserialize().ok()?;
        from_value(v.into())
    })();
    if portal.is_some() || std::env::var_os("FLATPAK_ID").is_some() {
        return portal;
    }
    use gtk::gio::prelude::SettingsExt;
    let source = gtk::gio::SettingsSchemaSource::default()?;
    source.lookup(namespace, true)?;
    let settings = gtk::gio::Settings::new(namespace);
    Some(settings.string(key).as_str().to_string())
}

/// The desktop's monospace font as a Pango description, "Monospace 10"
/// where the desktop names none.
pub fn monospace_font() -> String {
    setting("org.gnome.desktop.interface", "monospace-font-name")
        .filter(|f| !f.trim().is_empty())
        .unwrap_or_else(|| "Monospace 10".to_string())
}

/// Bring `window` to the front, for a click that happened somewhere else:
/// the tray icon, a notification, a second launch.
///
/// `gtk_window_present` on its own does not do it on X11. A window manager
/// there refuses to raise a window when the request carries a timestamp
/// older than the last thing the user did: that is how an app that takes
/// its time starting is kept from stealing the focus from whatever the
/// person went on to do. GTK fills the timestamp in from the last input
/// event *this* process saw, and a tray click is the panel's event, not
/// ours, so the timestamp is however long ago the window was last touched.
/// It loses the comparison, and muffin (Cinnamon), mutter, xfwm4 and their
/// kin leave the window behind the one in front, flagged as wanting
/// attention (#247).
///
/// The way to say "the user asked for this, now" is a timestamp of zero:
/// the window manager then reads the current time off the server and
/// honours the request. GTK never sends that itself, so the request goes
/// out by hand, after `present` has mapped the window. Wayland has no such
/// message — a compositor only raises a window for an activation token the
/// launcher passed us, which `present` already spends (#187) — so this is
/// X11's alone, Xwayland included.
pub fn present_window(window: &impl gtk::prelude::IsA<gtk::Window>) {
    use gtk::prelude::*;
    let window = window.as_ref();
    window.set_visible(true);
    window.present();
    let Some(surface) = window.surface() else { return };
    if !surface.display().type_().name().starts_with("GdkX11") {
        return;
    }
    if let Ok(toplevel) = surface.downcast::<gtk::gdk::Toplevel>() {
        toplevel.focus(gtk::gdk::CURRENT_TIME);
    }
}
