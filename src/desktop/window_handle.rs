//! A window's handle for another program to put its own window over ours,
//! in the `x11:XID` or `wayland:HANDLE` form portals and the GNOME file
//! previewer take (#625).
//!
//! On Wayland the handle is an xdg-foreign export, which lives until it is
//! dropped, so [`ExportedHandle`] drops it when it goes. The GDK backend
//! functions are looked up at run time, not linked: a GTK built without one
//! of the backends must still run Hylki.

use std::cell::RefCell;
use std::ffi::{c_char, c_ulong, c_void, CStr, CString};

use gtk::glib;
use gtk::glib::translate::ToGlibPtr;
use gtk::prelude::*;
use libloading::os::unix::Library;

type GdkToplevel = gtk::gdk::ffi::GdkToplevel;
type GdkSurface = gtk::gdk::ffi::GdkSurface;
type Exported = unsafe extern "C" fn(*mut GdkToplevel, *const c_char, *mut c_void);
type Destroy = unsafe extern "C" fn(*mut c_void);
type ExportFn =
    unsafe extern "C" fn(*mut GdkToplevel, Option<Exported>, *mut c_void, Option<Destroy>) -> i32;
type DropFn = unsafe extern "C" fn(*mut GdkToplevel, *const c_char);
type UnexportFn = unsafe extern "C" fn(*mut GdkToplevel);
type XidFn = unsafe extern "C" fn(*mut GdkSurface) -> c_ulong;

/// A handle for `window`, kept valid while this lives.
pub(crate) struct ExportedHandle {
    handle: String,
    /// The Wayland toplevel the handle was exported from, to drop it.
    wayland: Option<gtk::gdk::Toplevel>,
}

impl ExportedHandle {
    /// The handle as other programs take it; empty when there is none.
    pub(crate) fn as_str(&self) -> &str {
        &self.handle
    }

    /// No handle: the other window opens on its own.
    pub(crate) fn none() -> Self {
        Self {
            handle: String::new(),
            wayland: None,
        }
    }
}

impl Drop for ExportedHandle {
    fn drop(&mut self) {
        let Some(toplevel) = self.wayland.take() else {
            return;
        };
        let Some(raw) = self.handle.strip_prefix("wayland:") else {
            return;
        };
        let ptr: *mut GdkToplevel = toplevel.to_glib_none().0;
        // GTK 4.12 lets a toplevel hold several exports and drops each by
        // name; before it there was one, unexported whole.
        if let Some(drop_fn) = symbol::<DropFn>(c"gdk_wayland_toplevel_drop_exported_handle") {
            if let Ok(c) = CString::new(raw) {
                unsafe { drop_fn(ptr, c.as_ptr()) };
            }
        } else if let Some(unexport) = symbol::<UnexportFn>(c"gdk_wayland_toplevel_unexport_handle")
        {
            unsafe { unexport(ptr) };
        }
    }
}

/// A function of the running GTK, if it has it.
fn symbol<T: Copy>(name: &CStr) -> Option<T> {
    let this = Library::this();
    let sym = unsafe { this.get::<T>(name.to_bytes_with_nul()) }.ok()?;
    Some(*sym)
}

/// Whether `obj` is of the GDK backend type `name` (registered only once
/// that backend is in use).
fn is_a(obj: &impl IsA<glib::Object>, name: &str) -> bool {
    glib::Type::from_name(name).is_some_and(|t| obj.type_().is_a(t))
}

/// Export `window`'s handle and hand it to `done`: at once on X11, after
/// the compositor answers on Wayland. Anything else, or a failure, gives an
/// empty handle, so the other window opens on its own rather than not at all.
pub(crate) fn export(window: &gtk::Window, done: impl FnOnce(ExportedHandle) + 'static) {
    let Some(surface) = window.surface() else {
        done(ExportedHandle::none());
        return;
    };
    if is_a(&surface, "GdkX11Surface") {
        let handle = symbol::<XidFn>(c"gdk_x11_surface_get_xid")
            .map(|xid| unsafe { xid(surface.to_glib_none().0) })
            .filter(|&xid| xid != 0)
            .map(|xid| format!("x11:{xid:x}"))
            .unwrap_or_default();
        done(ExportedHandle {
            handle,
            wayland: None,
        });
        return;
    }
    let toplevel = surface.downcast::<gtk::gdk::Toplevel>().ok();
    let (Some(toplevel), Some(export)) = (
        toplevel.filter(|t| is_a(t, "GdkWaylandToplevel")),
        symbol::<ExportFn>(c"gdk_wayland_toplevel_export_handle"),
    ) else {
        done(ExportedHandle::none());
        return;
    };

    type Slot = RefCell<Option<Box<dyn FnOnce(Option<String>)>>>;
    unsafe extern "C" fn exported(_: *mut GdkToplevel, handle: *const c_char, data: *mut c_void) {
        let slot = unsafe { &*(data as *const Slot) };
        let handle = (!handle.is_null()).then(|| {
            unsafe { CStr::from_ptr(handle) }
                .to_string_lossy()
                .into_owned()
        });
        if let Some(f) = slot.borrow_mut().take() {
            f(handle);
        }
    }
    unsafe extern "C" fn destroy(data: *mut c_void) {
        drop(unsafe { Box::from_raw(data as *mut Slot) });
    }

    let kept = toplevel.clone();
    let slot: Box<Slot> = Box::new(RefCell::new(Some(Box::new(
        move |handle: Option<String>| {
            done(match handle {
                Some(h) if !h.is_empty() => ExportedHandle {
                    handle: format!("wayland:{h}"),
                    wayland: Some(kept),
                },
                _ => ExportedHandle::none(),
            })
        },
    ))));
    let data = Box::into_raw(slot) as *mut c_void;
    let started = unsafe {
        export(
            toplevel.to_glib_none().0,
            Some(exported),
            data,
            Some(destroy),
        )
    };
    if started == 0 {
        // Refused (no xdg-foreign in the compositor): GTK kept nothing.
        let slot = unsafe { Box::from_raw(data as *mut Slot) };
        let f = slot.borrow_mut().take();
        if let Some(f) = f {
            f(None);
        }
    }
}
