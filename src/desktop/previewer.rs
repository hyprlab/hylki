//! Previewing files in the GNOME file previewer, Sushi, the window Files
//! opens on Space (#625). It is asked over D-Bus, the way Files asks it.
//!
//! The files are staged where opened attachments are (see
//! [`crate::ui::attachment_io`]), a place both sides of a Flatpak sandbox
//! read at the same path, because the previewer is a program of the host.
//! The arrow keys in its window come back as `SelectionEvent`s, and step
//! through the files it was shown with.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use crate::desktop::window_handle::{self, ExportedHandle};
use crate::models::Attachment;

const NAME: &str = "org.gnome.NautilusPreviewer";
const PATH: &str = "/org/gnome/NautilusPreviewer";
const IFACE: &str = "org.gnome.NautilusPreviewer2";

/// What is being previewed, from the last [`show`].
struct Session {
    files: Vec<Attachment>,
    staged: Vec<Option<PathBuf>>,
    at: usize,
    handle: ExportedHandle,
    /// Told when the arrow keys move to another file, with its index.
    on_step: Option<Rc<dyn Fn(usize)>>,
}

thread_local! {
    static AVAILABLE: Cell<Option<bool>> = const { Cell::new(None) };
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    static SUBSCRIBED: Cell<bool> = const { Cell::new(false) };
    /// Sushi 51 added an activation token to `ShowFile`; older ones refuse
    /// the call with it, and are then called without it.
    static WITH_TOKEN: Cell<bool> = const { Cell::new(true) };
}

fn bus() -> Option<gio::DBusConnection> {
    gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()
}

/// Whether the previewer is installed, or running. Asked once: a session
/// bus lists what it can start, and a Flatpak's lists only the names its
/// permissions let it talk to.
pub(crate) fn available() -> bool {
    if let Some(known) = AVAILABLE.get() {
        return known;
    }
    let found = bus().is_some_and(|conn| {
        ["ListActivatableNames", "ListNames"].iter().any(|method| {
            conn.call_sync(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                method,
                None,
                Some(glib::VariantTy::new("(as)").unwrap()),
                gio::DBusCallFlags::NONE,
                1000,
                gio::Cancellable::NONE,
            )
            .ok()
            .and_then(|v| v.child_value(0).get::<Vec<String>>())
            .is_some_and(|names| names.iter().any(|n| n == NAME))
        })
    });
    if !found {
        tracing::debug!("the GNOME file previewer is not installed");
    }
    AVAILABLE.set(Some(found));
    found
}

/// Preview `files[at]`, over `parent`. The arrow keys in the previewer move
/// through `files`, and `on_step` hears where they land. Shown again, the
/// file closes the previewer, as Space does in Files.
pub(crate) fn show(
    files: Vec<Attachment>,
    at: usize,
    parent: Option<&gtk::Window>,
    on_step: Option<Rc<dyn Fn(usize)>>,
) {
    if at >= files.len() {
        return;
    }
    let token = parent.and_then(activation_token).unwrap_or_default();
    let start = move |handle: ExportedHandle| {
        // A file shown before keeps the copy staged for it, so the
        // previewer knows it for the same file and closes on it.
        let staged = SESSION.with_borrow(|old| {
            files
                .iter()
                .map(|f| {
                    let old = old.as_ref()?;
                    let i = old
                        .files
                        .iter()
                        .position(|o| o.name == f.name && o.data == f.data)?;
                    old.staged[i].clone()
                })
                .collect::<Vec<_>>()
        });
        let session = Session {
            files,
            staged,
            at,
            handle,
            on_step,
        };
        // The one before is let go first, and its exported handle with it.
        SESSION.with_borrow_mut(|s| *s = Some(session));
        show_current(true, &token);
    };
    match parent {
        Some(window) => window_handle::export(window, start),
        None => start(ExportedHandle::none()),
    }
}

/// An xdg-activation token, so the compositor lets the previewer take the
/// focus it was asked for.
fn activation_token(window: &gtk::Window) -> Option<String> {
    let context = WidgetExt::display(window).app_launch_context();
    context
        .startup_notify_id(None::<&gio::AppInfo>, &[])
        .map(|t| t.to_string())
}

/// Stage the session's current file if it is not yet, and show it.
fn show_current(close_if_shown: bool, token: &str) {
    let Some((path, handle)) = SESSION.with_borrow_mut(|s| {
        let s = s.as_mut()?;
        let at = s.at;
        if s.staged[at].is_none() {
            let file = &s.files[at];
            s.staged[at] = crate::ui::attachment_io::stage(&file.name, &file.data);
        }
        Some((s.staged[at].clone()?, s.handle.as_str().to_string()))
    }) else {
        return;
    };
    let Some(conn) = bus() else { return };
    subscribe(&conn);
    let uri = gio::File::for_path(&path).uri().to_string();
    call_show_file(conn, uri, handle, close_if_shown, token.to_string());
}

fn call_show_file(
    conn: gio::DBusConnection,
    uri: String,
    handle: String,
    close_if_shown: bool,
    token: String,
) {
    let with_token = WITH_TOKEN.get();
    let params = if with_token {
        (
            uri.as_str(),
            handle.as_str(),
            close_if_shown,
            token.as_str(),
        )
            .to_variant()
    } else {
        (uri.as_str(), handle.as_str(), close_if_shown).to_variant()
    };
    let retry = conn.clone();
    conn.call(
        Some(NAME),
        PATH,
        IFACE,
        "ShowFile",
        Some(&params),
        None,
        gio::DBusCallFlags::NONE,
        10_000,
        gio::Cancellable::NONE,
        move |res| match res {
            Ok(_) => {}
            Err(e) if with_token && e.matches(gio::DBusError::InvalidArgs) => {
                WITH_TOKEN.set(false);
                call_show_file(retry, uri, handle, close_if_shown, token);
            }
            Err(e) => {
                tracing::warn!("the file previewer did not show the file: {e}");
                SESSION.with_borrow_mut(|s| *s = None);
            }
        },
    );
}

/// Listen for the previewer's arrow keys and for its window going, once.
fn subscribe(conn: &gio::DBusConnection) {
    if SUBSCRIBED.replace(true) {
        return;
    }
    conn.signal_subscribe(
        Some(NAME),
        Some(IFACE),
        // Names on the wire, never translated.
        Some("SelectionEvent"),
        Some(PATH),
        None,
        gio::DBusSignalFlags::NONE,
        |_, _, _, _, _, params| {
            if let Some(direction) = params.child_value(0).get::<u32>() {
                step(direction);
            }
        },
    );
    conn.signal_subscribe(
        Some(NAME),
        Some("org.freedesktop.DBus.Properties"),
        Some("PropertiesChanged"),
        Some(PATH),
        Some(IFACE),
        gio::DBusSignalFlags::NONE,
        |_, _, _, _, _, params| {
            let changed = params.child_value(1);
            let dict = glib::VariantDict::new(Some(&changed));
            let closed = dict
                .lookup::<bool>("Visible")
                .ok()
                .flatten()
                .is_some_and(|v| !v);
            // Files, or another program, showed a file of its own over its
            // own window: the arrow keys are its business now.
            let taken = dict
                .lookup::<String>("ParentHandle")
                .ok()
                .flatten()
                .is_some_and(|h| {
                    SESSION.with_borrow(|s| s.as_ref().is_some_and(|s| s.handle.as_str() != h))
                });
            if closed || taken {
                SESSION.with_borrow_mut(|s| *s = None);
            }
        },
    );
}

/// The previewer's arrow keys, a `GtkDirectionType`: back for Up, Left and
/// Shift+Tab, on for the rest. Past either end nothing happens.
fn step(direction: u32) {
    let moved = SESSION.with_borrow_mut(|s| {
        let s = s.as_mut()?;
        s.at = stepped(s.at, s.files.len(), direction)?;
        Some((s.at, s.on_step.clone()))
    });
    let Some((at, on_step)) = moved else { return };
    show_current(false, "");
    // Told outside the borrow: whoever hears it may show again.
    if let Some(on_step) = on_step {
        on_step(at);
    }
}

/// Where `direction` goes from `at` among `len` files, if anywhere.
fn stepped(at: usize, len: usize, direction: u32) -> Option<usize> {
    if matches!(direction, 1 | 2 | 4) {
        at.checked_sub(1)
    } else {
        Some(at + 1).filter(|&n| n < len)
    }
}

#[cfg(test)]
mod tests {
    use super::stepped;

    #[test]
    fn the_arrow_keys_step_back_and_on_and_stop_at_the_ends() {
        // GTK_DIR_TAB_FORWARD, TAB_BACKWARD, UP, DOWN, LEFT, RIGHT.
        assert_eq!(stepped(1, 3, 0), Some(2));
        assert_eq!(stepped(1, 3, 1), Some(0));
        assert_eq!(stepped(1, 3, 2), Some(0));
        assert_eq!(stepped(1, 3, 3), Some(2));
        assert_eq!(stepped(1, 3, 4), Some(0));
        assert_eq!(stepped(1, 3, 5), Some(2));
        assert_eq!(stepped(0, 3, 4), None);
        assert_eq!(stepped(2, 3, 5), None);
    }
}
