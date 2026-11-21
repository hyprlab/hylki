//! Calls to xdg-desktop-portal that the window waits on.
//!
//! They go through GIO with a timeout, not zbus: a zbus call waits for its
//! answer for as long as it takes, and a portal that has stopped answering
//! then holds the main thread with it. The window never appears, and the
//! app's D-Bus name goes unanswered, so every later launch times out
//! handing off to it (#691).

use gtk::gio;
use gtk::glib;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";

/// A portal answers in milliseconds; this only bounds one that does not.
const TIMEOUT_MS: i32 = 2000;

/// How long a portal that timed out is left alone. Settings are read every
/// time a date is drawn, and each of those reads would otherwise wait out
/// the timeout again.
const REST: Duration = Duration::from_secs(300);

static TIMED_OUT_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// Call `method` on the portal's `iface` and return its reply, or `None`
/// when there is no portal, the call fails, or the portal has stopped
/// answering.
pub(crate) fn call(iface: &str, method: &str, args: &glib::Variant) -> Option<glib::Variant> {
    if resting() {
        return None;
    }
    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;
    match conn.call_sync(
        Some(DEST),
        PATH,
        iface,
        method,
        Some(args),
        None,
        gio::DBusCallFlags::NONE,
        TIMEOUT_MS,
        gio::Cancellable::NONE,
    ) {
        Ok(reply) => Some(reply),
        Err(e) if e.matches(gio::IOErrorEnum::TimedOut) => {
            tracing::warn!(
                "the desktop portal did not answer {iface}.{method} within {TIMEOUT_MS} ms; \
                 not asking it again for {} minutes",
                REST.as_secs() / 60
            );
            *TIMED_OUT_AT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
            None
        }
        Err(e) => {
            tracing::debug!("portal call {iface}.{method} failed: {e}");
            None
        }
    }
}

fn resting() -> bool {
    let mut at = TIMED_OUT_AT.lock().unwrap_or_else(|e| e.into_inner());
    match *at {
        Some(t) if t.elapsed() < REST => true,
        Some(_) => {
            *at = None;
            false
        }
        None => false,
    }
}

/// The value inside any number of variant boxes: the Settings portal's
/// `Read` wraps its answer twice, `ReadOne` once.
pub(crate) fn unboxed(mut v: glib::Variant) -> glib::Variant {
    while let Some(inner) = v.as_variant() {
        v = inner;
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::glib::prelude::ToVariant;

    #[test]
    fn boxes_are_taken_off_the_value() {
        let twice = "24h".to_variant().to_variant().to_variant();
        assert_eq!(unboxed(twice).str(), Some("24h"));
        assert_eq!(unboxed(true.to_variant()).get::<bool>(), Some(true));
    }
}
