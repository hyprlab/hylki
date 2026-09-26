//! Ending a launch the desktop started for an action that shows no window.
//!
//! GNOME Shell runs a notification button as a launch of the app: it opens a
//! startup sequence, which is the busy pointer, and hands its token over with
//! the action. The sequence ends when a window of ours spends that token, or
//! after 15 seconds. Mark as Read, Archive, Delete and Spam deliberately show
//! nothing, so without a word from us the pointer spins for the full 15.
//!
//! On X11, GDK has the "remove" message for this. On Wayland, GDK only ends a
//! launch by spending the token on a window (`notify_startup_complete` is a
//! no-op once xdg-activation is bound). mutter still reads
//! `gtk_shell1.set_startup_id` as "this launch is complete", and GTK keeps its
//! own binding of that interface private, so this module makes a second one
//! on GTK's connection.

use std::cell::RefCell;

use gtk::glib::translate::ToGlibPtr;
use gtk::prelude::*;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};

#[allow(dead_code, non_upper_case_globals, non_camel_case_types, clippy::all)]
mod protocol {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("resources/protocols/gtk-shell.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("resources/protocols/gtk-shell.xml");
}

use protocol::gtk_shell1::GtkShell1;

extern "C" {
    // Public GDK Wayland API; gtk4-sys links the library, and the gdk4-wayland
    // crate would add nothing but these three declarations.
    fn gdk_wayland_display_get_wl_display(
        display: *mut gtk::gdk::ffi::GdkDisplay,
    ) -> *mut std::ffi::c_void;
    fn gdk_wayland_display_get_startup_notification_id(
        display: *mut gtk::gdk::ffi::GdkDisplay,
    ) -> *const std::ffi::c_char;
    fn gdk_wayland_display_set_startup_notification_id(
        display: *mut gtk::gdk::ffi::GdkDisplay,
        startup_id: *const std::ffi::c_char,
    );
}

/// End the launch that brought in the action being run right now, if it
/// came with one. Call it from the action's own handler: GTK holds the token
/// only from the start of that emission until the next one.
pub fn complete() {
    let Some(display) = gtk::gdk::Display::default() else { return };
    let kind = display.type_().name();
    let raw: *mut gtk::gdk::ffi::GdkDisplay = display.to_glib_none().0;
    if kind.starts_with("GdkX11") {
        // NULL: the id GTK stored for this emission, which it then forgets.
        unsafe { gtk::gdk::ffi::gdk_display_notify_startup_complete(raw, std::ptr::null()) };
        return;
    }
    if !kind.starts_with("GdkWayland") {
        return;
    }
    let token = unsafe {
        let id = gdk_wayland_display_get_startup_notification_id(raw);
        if id.is_null() {
            return;
        }
        std::ffi::CStr::from_ptr(id).to_string_lossy().into_owned()
    };
    // Forget it, too: a completed token spent on the next window we present
    // (a tray click, say) would only keep that window from the focus.
    unsafe { gdk_wayland_display_set_startup_notification_id(raw, std::ptr::null()) };
    SHELL.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(Shell::bind(raw));
        }
        if let Some(Some(shell)) = slot.as_mut() {
            shell.complete(&token);
        }
    });
}

thread_local! {
    // Bound once, on first use; None when the compositor has no gtk_shell1
    // (anything but mutter), so the attempt is not repeated.
    static SHELL: RefCell<Option<Option<Shell>>> = const { RefCell::new(None) };
}

struct Shell {
    conn: Connection,
    queue: EventQueue<State>,
    shell: GtkShell1,
}

impl Shell {
    fn bind(display: *mut gtk::gdk::ffi::GdkDisplay) -> Option<Self> {
        let wl_display = unsafe { gdk_wayland_display_get_wl_display(display) };
        if wl_display.is_null() {
            return None;
        }
        // The display stays GTK's: a foreign backend never closes it.
        let backend =
            unsafe { wayland_backend::client::Backend::from_foreign_display(wl_display.cast()) };
        let conn = Connection::from_backend(backend);
        let (globals, queue) = registry_queue_init::<State>(&conn)
            .inspect_err(|e| tracing::warn!("startup: wayland registry: {e}"))
            .ok()?;
        let shell = globals
            .bind::<GtkShell1, _, _>(&queue.handle(), 1..=1, ())
            .inspect_err(|e| tracing::debug!("startup: no gtk_shell1: {e}"))
            .ok()?;
        Some(Self { conn, queue, shell })
    }

    fn complete(&mut self, token: &str) {
        // Nothing here listens, but the queue would keep every event.
        let _ = self.queue.dispatch_pending(&mut State);
        self.shell.set_startup_id(Some(token.to_owned()));
        if let Err(e) = self.conn.flush() {
            tracing::warn!("startup: could not end the startup sequence: {e}");
        }
    }
}

struct State;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<GtkShell1, ()> for State {
    fn event(
        _: &mut Self,
        _: &GtkShell1,
        _: protocol::gtk_shell1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
