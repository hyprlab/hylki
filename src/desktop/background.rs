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

use crate::i18n::{i18n, ni18n_f};
use super::portal;
use gtk::glib::prelude::ToVariant;

const IFACE_BACKGROUND: &str = "org.freedesktop.portal.Background";

/// Ask the portal for permission to keep running with no window, and to start at
/// login when `autostart` is set.
///
/// The portal replies asynchronously over a `Request` object; the answer is not
/// waited for. Hylki behaves the same either way — the permission governs how
/// the desktop *presents* a background app, not whether the process may run —
/// and blocking a settings toggle on a dialog the user may leave sitting there
/// would be worse than proceeding.
pub(crate) fn request(autostart: bool) {
    let reason =
        i18n("Hylki checks for new mail and shows notifications while its window is closed.");
    if let Err(e) = call_request(&reason, autostart) {
        tracing::debug!("background portal request skipped: {e}");
    }
}

fn call_request(reason: &str, autostart: bool) -> Result<(), String> {
    let args = request_args(reason, autostart);
    portal::call(IFACE_BACKGROUND, "RequestBackground", &args)
        .map(|_| ())
        .ok_or_else(|| "no answer from the portal".to_string())
}

/// `RequestBackground`'s `(sa{sv})` arguments. Built with `tuple_from_iter`:
/// a Rust tuple's `to_variant()` boxes a `Variant` member as `v`, and the
/// portal rejects a `(sv)` call outright.
fn request_args(reason: &str, autostart: bool) -> gtk::glib::Variant {
    let options = gtk::glib::VariantDict::new(None);
    options.insert("reason", reason);
    options.insert("autostart", autostart);
    // Autostart runs this rather than the desktop file's Exec line, so logging in
    // leaves Hylki checking mail from the Background Apps menu instead of opening
    // a window at you. Not `dbus-activatable`: that would autostart over D-Bus,
    // which needs DBusActivatable in the desktop file, and takes no arguments.
    let commandline = vec!["hylki".to_string(), crate::HIDDEN_FLAG.to_string()];
    options.insert("commandline", commandline);
    // An empty parent window: this can be triggered from a settings toggle
    // rather than a window the portal should parent its dialog to.
    gtk::glib::Variant::tuple_from_iter(["".to_variant(), options.end()])
}

/// Set the line GNOME shows beside Hylki in Background Apps.
///
/// Truncated to the portal's 96-character limit — an over-long status is
/// rejected outright, which would leave no message at all.
pub(crate) fn set_status(message: &str) {
    // Version 1 of the portal has no SetStatus; the app is still listed, just
    // without a message.
    if portal::call(IFACE_BACKGROUND, "SetStatus", &status_args(message)).is_none() {
        tracing::debug!("background status not set");
    }
}

/// `SetStatus`'s `(a{sv})` arguments (see [`request_args`]), the message
/// truncated to the portal's limit.
fn status_args(message: &str) -> gtk::glib::Variant {
    let message: String = message.chars().take(96).collect();
    let options = gtk::glib::VariantDict::new(None);
    options.insert("message", message.as_str());
    gtk::glib::Variant::tuple_from_iter([options.end()])
}

/// The status line: what Hylki is doing for you while it has no window.
pub(crate) fn status_text(unread: u32) -> String {
    match unread {
        0 => i18n("Checking for new mail"),
        n => ni18n_f(
            "{n} unread message",
            "{n} unread messages",
            n,
            &[("n", &n.to_string())],
        ),
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

    #[test]
    fn portal_calls_have_the_signatures_it_expects() {
        // xdg-desktop-portal rejects a call whose arguments do not match the
        // method's signature, so the request and status never arrived.
        let request = request_args("reason", true);
        assert_eq!(request.type_().as_str(), "(sa{sv})");
        let options = gtk::glib::VariantDict::new(Some(&request.child_value(1)));
        assert_eq!(options.lookup::<bool>("autostart").unwrap(), Some(true));
        assert_eq!(
            options.lookup::<Vec<String>>("commandline").unwrap(),
            Some(vec!["hylki".to_string(), crate::HIDDEN_FLAG.to_string()])
        );
        assert_eq!(status_args("Checking").type_().as_str(), "(a{sv})");
    }
}
