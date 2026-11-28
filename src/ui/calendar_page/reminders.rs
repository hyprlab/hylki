//! Event reminders: an event's alarms shown as desktop notifications.
//!
//! On GNOME, and wherever Evolution Data Server's alarm service runs, the
//! desktop shows them already, from the same calendars; showing them here
//! too would show each twice. So by default the page shows them only where
//! that service isn't running (Settings → Calendar → Show reminders).

use std::time::{Duration, Instant};

use relm4::prelude::*;

use super::{CalendarInput, CalendarPage};
use crate::calendar::prefs::Reminders;
use crate::calendar::store::Backend;
use crate::i18n::{i18n_f, ni18n_f};

/// How often the alarms due are looked for.
pub(super) const TICK: u32 = 30;
/// The longest lead time looked ahead for: a week, the longest offered.
const AHEAD: i64 = 8 * 86_400;

/// Whether EDS's alarm service is running: by its process, or its name on
/// the session bus (the only way to see it from inside a Flatpak).
fn alarm_service() -> bool {
    static SEEN: std::sync::Mutex<Option<(Instant, bool)>> = std::sync::Mutex::new(None);
    let mut g = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, seen)) = *g {
        if at.elapsed() < Duration::from_secs(300) {
            return seen;
        }
    }
    let by_process = std::fs::read_dir("/proc").into_iter().flatten().flatten().any(|e| {
        std::fs::read_to_string(e.path().join("comm"))
            .is_ok_and(|c| c.trim_end().starts_with("evolution-alar"))
    });
    let seen = by_process || {
        use gtk::gio;
        gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
            .ok()
            .and_then(|c| {
                c.call_sync(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                    "NameHasOwner",
                    Some(&gtk::glib::Variant::from(("org.gnome.Evolution-alarm-notify",))),
                    None,
                    gio::DBusCallFlags::NONE,
                    2000,
                    gio::Cancellable::NONE,
                )
                .ok()
            })
            .and_then(|r| r.get::<(bool,)>())
            .is_some_and(|(b,)| b)
    };
    *g = Some((Instant::now(), seen));
    seen
}

impl CalendarPage {
    fn reminders_on(&self) -> bool {
        if !self.enabled {
            return false;
        }
        match self.prefs.reminders {
            Reminders::Never => false,
            Reminders::Always => true,
            Reminders::Automatic => self.backend != Backend::Demo && !(self.backend == Backend::Eds && alarm_service()),
        }
    }

    /// Show the alarms that went off since the last look.
    pub(super) fn on_tick(&mut self, sender: &ComponentSender<Self>) {
        if !self.reminders_on() {
            self.reminded_to = None;
            return;
        }
        // Nothing read yet: the calendars are read in the background, the
        // page unseen.
        if self.listed_at.is_none() {
            self.list(sender, false);
            return;
        }
        let now = crate::datefmt::now();
        // A first look reaches back one tick; a laptop that slept doesn't
        // fire everything it slept through.
        let since = match self.reminded_to {
            Some(t) if now - t < 15 * 60 => t,
            _ => now - i64::from(TICK),
        };
        self.reminded_to = Some(now);
        let occs = self.occurrences(now - 3600, now + AHEAD);
        for o in occs {
            for &before in &o.comp.alarms {
                let at = o.start - before;
                if at <= since || at > now {
                    continue;
                }
                let title = if o.comp.summary.is_empty() {
                    crate::i18n::i18n("Untitled")
                } else {
                    o.comp.summary.clone()
                };
                let mut body = if o.all_day {
                    super::inspect::when_text(&o)
                } else if o.start > now {
                    let mins = ((o.start - now + 59) / 60) as u32;
                    let lead = if mins >= 60 * 24 {
                        super::inspect::when_text(&o)
                    } else if mins >= 60 {
                        ni18n_f("In {n} hour", "In {n} hours", mins / 60, &[("n", &(mins / 60).to_string())])
                    } else {
                        ni18n_f("In {n} minute", "In {n} minutes", mins, &[("n", &mins.to_string())])
                    };
                    i18n_f("{when}, at {time}", &[("when", &lead), ("time", &crate::datefmt::time(o.start))])
                } else {
                    i18n_f("Now, until {time}", &[("time", &crate::datefmt::time(o.end))])
                };
                if !o.comp.location.is_empty() {
                    body = format!("{body}\n{}", o.comp.location);
                }
                let id = format!("{}-{}-{}", o.key.uid, o.key.rid, before);
                crate::desktop::notify::event_reminder(&id, &title, &body);
            }
        }
    }
}

/// Look for alarms due every [`TICK`] seconds, the first a few seconds
/// after start.
pub(super) fn start(sender: &ComponentSender<CalendarPage>) {
    let s = sender.input_sender().clone();
    gtk::glib::timeout_add_seconds_local_once(8, move || {
        let _ = s.send(CalendarInput::Tick);
        gtk::glib::timeout_add_seconds_local(TICK, move || {
            let _ = s.send(CalendarInput::Tick);
            gtk::glib::ControlFlow::Continue
        });
    });
}
