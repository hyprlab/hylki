//! Whether the X server can map this process's shared memory (#547).
//!
//! Without a GPU (Xvnc, Xvfb, a remote display), GTK's GL renderer runs on
//! Mesa's software driver, which hands every frame to the X server through
//! MIT-SHM. Mesa only asks whether the extension is there and the client is
//! local; when the server then cannot attach the segment (it runs as another
//! user or in another IPC namespace, as an Xvnc started from inetd or a
//! systemd socket can), Mesa logs "Failed to attach to x11 shm" and draws
//! nothing, and the window stays black. Cairo's renderer checks properly and
//! falls back to plain image requests, so it is the one to use there.
//!
//! libxcb is opened at run time, not linked: a Wayland session never needs it.

use std::ffi::{c_char, c_int, c_void};

use libloading::{Library, Symbol};

/// `xcb_query_extension_reply_t`; only `present` is read, the rest is layout.
#[repr(C)]
#[allow(dead_code)]
struct ExtensionReply {
    response_type: u8,
    pad0: u8,
    sequence: u16,
    length: u32,
    present: u8,
    major_opcode: u8,
    first_event: u8,
    first_error: u8,
}

/// `xcb_void_cookie_t`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Cookie {
    sequence: u32,
}

/// The head of `xcb_generic_error_t`.
#[repr(C)]
#[allow(dead_code)]
struct Error {
    response_type: u8,
    error_code: u8,
}

/// X11's BadRequest: the server says MIT-SHM is not for this client.
const BAD_REQUEST: u8 = 1;

type Conn = *mut c_void;

/// Whether GTK will draw on X11, where `DISPLAY` alone does not settle it:
/// GTK prefers Wayland, and finds its socket without `WAYLAND_DISPLAY` too.
pub(crate) fn gtk_uses_x11() -> bool {
    if std::env::var_os("DISPLAY").is_none() {
        return false;
    }
    if let Ok(backend) = std::env::var("GDK_BACKEND") {
        return backend.split(',').next().is_some_and(|b| b.trim() == "x11");
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return false;
    }
    !std::env::var_os("XDG_RUNTIME_DIR")
        .is_some_and(|dir| std::path::Path::new(&dir).join("wayland-0").exists())
}

/// True when the server offers MIT-SHM to this client but cannot attach a
/// segment it makes, the case Mesa does not survive. Anything that cannot be
/// tested (no libxcb, no display, no segment) answers false: GTK's choice
/// stands.
pub(crate) fn attach_fails() -> bool {
    // SAFETY: the signatures are libxcb's and libxcb-shm's own, every
    // pointer they return is checked before use, and errors are freed with
    // libc's free, as xcb documents.
    unsafe { probe() }.unwrap_or(false)
}

unsafe fn probe() -> Option<bool> {
    let xcb = Library::new("libxcb.so.1").ok()?;
    let shm = Library::new("libxcb-shm.so.0").ok()?;
    let connect: Symbol<unsafe extern "C" fn(*const c_char, *mut c_int) -> Conn> =
        xcb.get(b"xcb_connect\0").ok()?;
    let has_error: Symbol<unsafe extern "C" fn(Conn) -> c_int> =
        xcb.get(b"xcb_connection_has_error\0").ok()?;
    let disconnect: Symbol<unsafe extern "C" fn(Conn)> = xcb.get(b"xcb_disconnect\0").ok()?;
    let extension_data: Symbol<unsafe extern "C" fn(Conn, *mut c_void) -> *const ExtensionReply> =
        xcb.get(b"xcb_get_extension_data\0").ok()?;
    let generate_id: Symbol<unsafe extern "C" fn(Conn) -> u32> =
        xcb.get(b"xcb_generate_id\0").ok()?;
    let request_check: Symbol<unsafe extern "C" fn(Conn, Cookie) -> *mut Error> =
        xcb.get(b"xcb_request_check\0").ok()?;
    let shm_id: Symbol<*mut c_void> = shm.get(b"xcb_shm_id\0").ok()?;
    let attach: Symbol<unsafe extern "C" fn(Conn, u32, u32, u8) -> Cookie> =
        shm.get(b"xcb_shm_attach_checked\0").ok()?;
    let detach: Symbol<unsafe extern "C" fn(Conn, u32) -> Cookie> =
        shm.get(b"xcb_shm_detach_checked\0").ok()?;

    let conn = connect(std::ptr::null(), std::ptr::null_mut());
    if conn.is_null() {
        return None;
    }
    if has_error(conn) != 0 {
        disconnect(conn);
        return None;
    }
    // Asks xcb whether the error a request drew, if any, has this code.
    let error_code = |cookie: Cookie| -> Option<u8> {
        let e = request_check(conn, cookie);
        (!e.is_null()).then(|| {
            let code = (*e).error_code;
            libc::free(e.cast());
            code
        })
    };
    let result = (|| {
        let ext = extension_data(conn, *shm_id);
        if ext.is_null() || (*ext).present == 0 {
            return Some(false);
        }
        // Mesa's own test: detaching segment 0 draws BadRequest from a server
        // that will not share memory with this client, which Mesa then never
        // asks it to.
        if error_code(detach(conn, 0)) == Some(BAD_REQUEST) {
            return Some(false);
        }
        let id = libc::shmget(libc::IPC_PRIVATE, 4096, libc::IPC_CREAT | 0o600);
        if id < 0 {
            return None;
        }
        let seg = generate_id(conn);
        let failed = error_code(attach(conn, seg, id as u32, 0)).is_some();
        if !failed {
            error_code(detach(conn, seg));
        }
        libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
        Some(failed)
    })();
    disconnect(conn);
    result
}
