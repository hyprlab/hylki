//! Plays a sound through GStreamer's `playbin`, audio only (#644).
//!
//! `gtk::MediaFile` is built for video: even for a sound it gives GStreamer a
//! GL sink, which on Wayland wraps GDK's own EGL display. When that player is
//! freed, GStreamer calls `eglTerminate` on the display it believes it owns,
//! GDK's GL context will not make current again, and the next frame aborts
//! the app in libepoxy. Only the OpenGL renderer draws through that display,
//! so only it froze. A `playbin` told to play audio alone never builds a
//! video branch and never touches GL.
//!
//! libgstreamer is opened at run time, not linked: it is the copy GTK's own
//! media support loads, so the build needs no GStreamer headers.

use std::ffi::{c_char, c_int, c_void};
use std::sync::OnceLock;

use gtk::glib;
use gtk::glib::translate::{from_glib_full, from_glib_none, ToGlibPtr};
use gtk::prelude::*;
use libloading::Library;

/// `GST_STATE_NULL` and `GST_STATE_PLAYING`.
const STATE_NULL: c_int = 1;
const STATE_PLAYING: c_int = 4;
/// `GST_STATE_CHANGE_FAILURE`.
const CHANGE_FAILURE: c_int = 0;

/// The few libgstreamer calls the player makes. Properties and signals go
/// through GObject, which the bindings already speak.
struct Gst {
    _lib: Library,
    element_factory_make: unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_void,
    element_set_state: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    element_get_bus: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    bus_add_signal_watch: unsafe extern "C" fn(*mut c_void),
    bus_remove_signal_watch: unsafe extern "C" fn(*mut c_void),
    message_parse_error:
        unsafe extern "C" fn(*mut c_void, *mut *mut glib::ffi::GError, *mut *mut c_char),
}

// SAFETY: plain function pointers into a library that stays loaded.
unsafe impl Send for Gst {}
unsafe impl Sync for Gst {}

fn gst() -> Option<&'static Gst> {
    static GST: OnceLock<Option<Gst>> = OnceLock::new();
    GST.get_or_init(|| {
        // SAFETY: the signatures are libgstreamer's own; gst_init_check may
        // be called any number of times, and GTK may already have.
        let loaded = unsafe { load() };
        if let Err(e) = &loaded {
            tracing::warn!("new-mail sound: GStreamer is not available: {e}");
        }
        loaded.ok()
    })
    .as_ref()
}

unsafe fn load() -> Result<Gst, String> {
    let lib = Library::new("libgstreamer-1.0.so.0").map_err(|e| e.to_string())?;
    let init_check: unsafe extern "C" fn(
        *mut c_int,
        *mut *mut *mut c_char,
        *mut *mut glib::ffi::GError,
    ) -> glib::ffi::gboolean = *lib.get(b"gst_init_check\0").map_err(|e| e.to_string())?;
    let mut error = std::ptr::null_mut();
    if init_check(std::ptr::null_mut(), std::ptr::null_mut(), &mut error) == 0 {
        let e: glib::Error = from_glib_full(error);
        return Err(e.to_string());
    }
    macro_rules! sym {
        ($name:literal) => {
            *lib.get(concat!($name, "\0").as_bytes()).map_err(|e| e.to_string())?
        };
    }
    Ok(Gst {
        element_factory_make: sym!("gst_element_factory_make"),
        element_set_state: sym!("gst_element_set_state"),
        element_get_bus: sym!("gst_element_get_bus"),
        bus_add_signal_watch: sym!("gst_bus_add_signal_watch"),
        bus_remove_signal_watch: sym!("gst_bus_remove_signal_watch"),
        message_parse_error: sym!("gst_message_parse_error"),
        _lib: lib,
    })
}

/// How a sound stopped.
pub(crate) enum End {
    Finished,
    Failed(String),
}

/// One sound playing. Dropping it stops the sound and closes its audio
/// stream.
pub(crate) struct Player {
    gst: &'static Gst,
    playbin: glib::Object,
    bus: glib::Object,
}

impl Player {
    /// Start playing `uri` (`file://` or `resource://`). `on_end` runs on the
    /// main loop when the sound ends or fails, from inside a bus signal: the
    /// player is best dropped from an idle callback after it.
    pub(crate) fn play(uri: &str, on_end: impl Fn(End) + 'static) -> Result<Player, String> {
        let gst = gst().ok_or("GStreamer is not available")?;
        // SAFETY: a new element comes back floating, and from_glib_none
        // sinks it; the bus comes back with a reference of its own.
        let (playbin, bus) = unsafe {
            let element = (gst.element_factory_make)(c"playbin".as_ptr(), std::ptr::null());
            if element.is_null() {
                return Err("GStreamer has no playbin element".into());
            }
            let playbin: glib::Object = from_glib_none(element as *mut glib::gobject_ffi::GObject);
            let bus = (gst.element_get_bus)(element);
            if bus.is_null() {
                return Err("playbin has no bus".into());
            }
            let bus: glib::Object = from_glib_full(bus as *mut glib::gobject_ffi::GObject);
            (playbin, bus)
        };
        playbin.set_property("uri", uri);
        let flags = playbin
            .find_property("flags")
            .and_then(|p| glib::FlagsClass::with_type(p.value_type()))
            .and_then(|class| {
                let audio = class.from_nick_string("audio|soft-volume").ok()?;
                class.to_value(audio)
            })
            .ok_or("playbin has no audio-only setting")?;
        playbin.set_property_from_value("flags", &flags);

        let player = Player { gst, playbin, bus };
        // SAFETY: the bus is a GstBus; the watch is removed in Drop.
        unsafe { (gst.bus_add_signal_watch)(player.bus_ptr()) };
        let on_end = std::rc::Rc::new(on_end);
        let end = on_end.clone();
        player.bus.connect_local("message::eos", false, move |_| {
            end(End::Finished);
            None
        });
        player.bus.connect_local("message::error", false, move |values| {
            // SAFETY: the signal's second argument is the GstMessage.
            let reason =
                values.get(1).map(|message| unsafe { parse_error(gst, message) }).unwrap_or_default();
            on_end(End::Failed(reason));
            None
        });
        // SAFETY: a GstElement and one of GstState's values.
        if unsafe { (gst.element_set_state)(player.element_ptr(), STATE_PLAYING) } == CHANGE_FAILURE {
            return Err("playbin would not start".into());
        }
        Ok(player)
    }

    fn element_ptr(&self) -> *mut c_void {
        self.playbin.as_ptr() as *mut c_void
    }

    fn bus_ptr(&self) -> *mut c_void {
        self.bus.as_ptr() as *mut c_void
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // SAFETY: the element and bus this player made, with the watch
        // added in play().
        unsafe {
            (self.gst.element_set_state)(self.element_ptr(), STATE_NULL);
            (self.gst.bus_remove_signal_watch)(self.bus_ptr());
        }
    }
}

/// The text of an error message on the bus.
unsafe fn parse_error(gst: &Gst, message: &glib::Value) -> String {
    let boxed = glib::gobject_ffi::g_value_get_boxed(message.to_glib_none().0);
    if boxed.is_null() {
        return String::new();
    }
    let mut error = std::ptr::null_mut();
    let mut debug: *mut c_char = std::ptr::null_mut();
    (gst.message_parse_error)(boxed, &mut error, &mut debug);
    glib::ffi::g_free(debug as *mut c_void);
    if error.is_null() {
        return String::new();
    }
    let e: glib::Error = from_glib_full(error);
    e.message().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plays `uri` on a main loop of the test's own and says how it ended,
    /// or None where the machine has no GStreamer.
    fn run(uri: &str) -> Option<Result<(), String>> {
        gst()?;
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let ended = std::rc::Rc::new(std::cell::RefCell::new(None));
            let tell = ended.clone();
            let player = Player::play(uri, move |end| {
                *tell.borrow_mut() = Some(match end {
                    End::Finished => Ok(()),
                    End::Failed(e) => Err(e),
                });
            });
            let player = match player {
                Ok(p) => p,
                Err(e) => return Some(Err(e)),
            };
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while ended.borrow().is_none() && std::time::Instant::now() < deadline {
                ctx.iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            drop(player);
            let result = ended.borrow_mut().take();
            Some(result.unwrap_or_else(|| Err("no end within 10 s".into())))
        })
        .expect("thread-default context")
    }

    #[test]
    fn a_file_that_is_not_a_sound_fails_out_loud() {
        let path =
            std::env::temp_dir().join(format!("hylki-not-a-sound-{}.ogg", std::process::id()));
        std::fs::write(&path, b"this is not audio").unwrap();
        let result = run(&gtk::gio::File::for_path(&path).uri());
        let _ = std::fs::remove_file(&path);
        if let Some(result) = result {
            // Fails in decoding, not in setting up the player.
            let e = result.expect_err("garbage played");
            assert!(!e.contains("playbin"), "{e}");
        }
    }

    #[test]
    #[ignore = "needs an audio output"]
    fn plays_a_built_in_sound() {
        let bytes = glib::Bytes::from_static(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/sounds.gresource"
        )));
        gtk::gio::resources_register(&gtk::gio::Resource::from_data(&bytes).unwrap());
        let result = run("resource:///co/hyprlab/Hylki/sounds/click.ogg").expect("GStreamer");
        assert_eq!(result, Ok(()));
    }
}
