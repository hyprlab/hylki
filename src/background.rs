//! Running in the background, the way GNOME expects (issue #3).
//!
//! GNOME has no system tray — the sanctioned equivalent is the **Background
//! Apps** section of Quick Settings, which xdg-desktop-portal populates from
//! sandboxed apps that are running without a window. So keeping the process
//! alive after the last window closes is what puts Hylki there; nothing needs to
//! draw an icon.
//!
//! Two portal calls make it a good citizen rather than a mystery process:
//!
//! * `RequestBackground` asks the user's permission to keep running, and can
//!   register an autostart entry so Hylki is already watching for mail at login.
//! * `SetStatus` sets the line shown beside the app in that menu, so it says
//!   what it is doing rather than merely existing.
//!
//! Both are best-effort. On a desktop with no portal — or with the permission
//! refused — Hylki still runs; it just isn't listed, which is the desktop's
//! decision to make, not ours.

use zbus::zvariant::Value;
use crate::i18n::{i18n, ni18n_f};

const PORTAL_DEST: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE_BACKGROUND: &str = "org.freedesktop.portal.Background";

/// Ask the portal for permission to keep running with no window, and to start at
/// login when `autostart` is set.
///
/// The portal replies asynchronously over a `Request` object; the answer is not
/// waited for. Hylki behaves the same either way — the permission governs how
/// the desktop *presents* a background app, not whether the process may run —
/// and blocking a settings toggle on a dialog the user may leave sitting there
/// would be worse than proceeding.
pub fn request(autostart: bool) {
    let reason = i18n("Hylki checks for new mail and shows notifications while its window is closed.");
    if let Err(e) = call_request(&reason, autostart) {
        tracing::debug!("background portal request skipped: {e}");
    }
}

fn call_request(reason: &str, autostart: bool) -> Result<(), String> {
    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let mut options: std::collections::HashMap<&str, Value> = std::collections::HashMap::new();
    options.insert("reason", Value::from(reason));
    options.insert("autostart", Value::from(autostart));
    // Autostart runs this rather than the desktop file's Exec line, so logging in
    // leaves Hylki checking mail from the Background Apps menu instead of opening
    // a window at you. Not `dbus-activatable`: that would autostart over D-Bus,
    // which needs DBusActivatable in the desktop file, and takes no arguments.
    let commandline = vec!["hylki".to_string(), crate::HIDDEN_FLAG.to_string()];
    options.insert("commandline", Value::from(commandline));
    conn.call_method(
        Some(PORTAL_DEST),
        PORTAL_PATH,
        Some(IFACE_BACKGROUND),
        "RequestBackground",
        // An empty parent window: this can be triggered from a settings toggle
        // rather than a window the portal should parent its dialog to.
        &("", options),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Set the line GNOME shows beside Hylki in Background Apps.
///
/// Truncated to the portal's 96-character limit — an over-long status is
/// rejected outright, which would leave no message at all.
pub fn set_status(message: &str) {
    let message: String = message.chars().take(96).collect();
    let run = || -> Result<(), String> {
        let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
        let mut options: std::collections::HashMap<&str, Value> = std::collections::HashMap::new();
        options.insert("message", Value::from(message.as_str()));
        conn.call_method(
            Some(PORTAL_DEST),
            PORTAL_PATH,
            Some(IFACE_BACKGROUND),
            "SetStatus",
            &(options,),
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    };
    if let Err(e) = run() {
        // Version 1 of the portal has no SetStatus; the app is still listed, just
        // without a message.
        tracing::debug!("background status not set: {e}");
    }
}

/// The status line: what Hylki is doing for you while it has no window.
pub fn status_text(unread: u32) -> String {
    match unread {
        0 => i18n("Checking for new mail"),
        n => ni18n_f("{n} unread message", "{n} unread messages", n, &[("n", &n.to_string())]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_says_what_it_is_doing() {
        assert_eq!(status_text(0), "Checking for new mail");
        assert_eq!(status_text(1), "1 unread message");
        assert_eq!(status_text(7), "7 unread messages");
    }

    #[test]
    fn status_fits_the_portal_limit() {
        // The portal rejects anything longer than 96 characters, which would
        // leave the entry with no message at all.
        assert!(status_text(u32::MAX).chars().count() <= 96);
    }
}
