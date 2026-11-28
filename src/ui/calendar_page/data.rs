//! The page's reading and writing: the calendars listed and loaded off the
//! UI thread, a calendar read again when EDS says it changed, and each
//! change shown at once, written in the background, and undoable.

use std::time::{Duration as StdDuration, Instant};

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use relm4::prelude::*;

use super::{CalendarInput, CalendarOutput, CalendarPage, Cmd};
use crate::calendar::edit::Change;
use crate::calendar::model::Comp;
use crate::calendar::store::{self, CalInfo, Loaded};
use crate::i18n::{i18n, i18n_f};

/// What undoes the last change: the changes that put things back, in the
/// calendar they go to.
#[derive(Debug, Clone)]
pub(crate) struct Undo {
    pub(crate) calendar: String,
    pub(crate) changes: Vec<Change>,
}

/// How long the calendar list is trusted before a visit lists it again.
const LIST_AGAIN: StdDuration = StdDuration::from_secs(20);

impl CalendarPage {
    pub(super) fn on_shown(&mut self, sender: &ComponentSender<Self>) {
        let stale = self.listed_at.is_none_or(|t| t.elapsed() >= LIST_AGAIN);
        if stale {
            self.list(sender, false);
        }
        self.refresh();
        let views = self.parts.views.clone();
        gtk::glib::idle_add_local_once(move || {
            views.grab_focus();
        });
    }

    /// List the calendars again; with `reload`, read every one again too
    /// and ask the servers for what changed.
    pub(super) fn list(&mut self, sender: &ComponentSender<Self>, reload: bool) {
        self.listed_at = Some(Instant::now());
        if reload {
            self.loaded.clear();
        }
        self.parts.spinner.set_visible(true);
        let backend = self.backend;
        let names = self.names.clone();
        let s = sender.command_sender().clone();
        std::thread::spawn(move || {
            let r = store::calendars(backend, names);
            let _ = s.send(Cmd::Calendars(r));
        });
        if reload {
            let backend = self.backend;
            let cals = self.calendars.clone();
            std::thread::spawn(move || {
                for c in &cals {
                    store::refresh(backend, c);
                }
            });
        }
    }

    pub(super) fn on_calendars(&mut self, r: Result<Vec<CalInfo>, String>, sender: &ComponentSender<Self>) {
        let cals = match r {
            Ok(c) => c,
            Err(e) => {
                self.parts.spinner.set_visible(false);
                let _ = sender.output(CalendarOutput::Notice {
                    text: i18n_f("Could not read the calendars: {e}", &[("e", &e)]),
                    error: true,
                });
                return;
            }
        };
        let first_time = self.calendars.is_empty();
        // Gone calendars go from the views; new ones are read.
        let uids: Vec<String> = cals.iter().map(|c| c.uid.clone()).collect();
        self.loaded.retain(|u, _| uids.contains(u));
        self.calendars = cals.into_iter().filter(|c| !c.tasks).collect();
        {
            let mut ctx = self.ctx.borrow_mut();
            ctx.colors = self
                .calendars
                .iter()
                .map(|c| (c.uid.clone(), super::paint::Rgba::hex(&c.color)))
                .collect();
            ctx.writable = self
                .calendars
                .iter()
                .filter(|c| c.writable)
                .map(|c| c.uid.clone())
                .collect();
        }
        self.parts.side.set_calendars(&self.calendars, &self.hidden);
        let mut pending = 0;
        for c in self.calendars.clone() {
            if self.loaded.contains_key(&c.uid) {
                continue;
            }
            pending += 1;
            self.load(&c, sender);
        }
        if pending == 0 {
            self.parts.spinner.set_visible(false);
        }
        if first_time {
            // Ask the servers once a visit's first read is in.
            let backend = self.backend;
            let cals = self.calendars.clone();
            std::thread::spawn(move || {
                for c in &cals {
                    store::refresh(backend, c);
                }
            });
        }
        self.refresh();
    }

    fn load(&self, cal: &CalInfo, sender: &ComponentSender<Self>) {
        let backend = self.backend;
        let cal = cal.clone();
        let s = sender.command_sender().clone();
        std::thread::spawn(move || {
            let r = store::load(backend, &cal);
            let _ = s.send(Cmd::Loaded(cal.uid.clone(), r));
        });
    }

    pub(super) fn on_loaded(&mut self, uid: String, r: Result<Loaded, String>, sender: &ComponentSender<Self>) {
        self.reloading.remove(&uid);
        match r {
            Ok(l) => {
                if let Some((bus, path)) = &l.watch {
                    self.watch(&uid, bus, path, sender);
                }
                if let Some(w) = l.writable {
                    let mut ctx = self.ctx.borrow_mut();
                    if w {
                        if self.calendars.iter().any(|c| c.uid == uid && c.writable) {
                            ctx.writable.insert(uid.clone());
                        }
                    } else {
                        ctx.writable.remove(&uid);
                    }
                }
                self.errors.remove(&uid);
                self.loaded.insert(uid, l);
            }
            Err(e) => {
                tracing::warn!(calendar = %uid, "calendar did not load: {e}");
                self.errors.insert(uid, e);
            }
        }
        let waiting = self
            .calendars
            .iter()
            .any(|c| !self.loaded.contains_key(&c.uid) && !self.errors.contains_key(&c.uid));
        self.parts.spinner.set_visible(waiting);
        self.refresh();
    }

    /// Listen for the calendar's revision moving.
    #[allow(deprecated)] // signal_subscribe: its replacement needs gio 0.21
    fn watch(&mut self, uid: &str, bus: &str, path: &str, sender: &ComponentSender<Self>) {
        if self.watched.contains_key(uid) {
            return;
        }
        let Ok(conn) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
            return;
        };
        let s = sender.command_sender().clone();
        let id = uid.to_string();
        let sub = conn.signal_subscribe(
            Some(bus),
            Some("org.freedesktop.DBus.Properties"),
            Some("PropertiesChanged"),
            Some(path),
            None,
            gio::DBusSignalFlags::NONE,
            move |_, _, _, _, _, params| {
                let changed = params
                    .child_value(1)
                    .iter()
                    .any(|e| e.child_value(0).str() == Some("Revision"));
                if changed {
                    let _ = s.send(Cmd::Changed(id.clone()));
                }
            },
        );
        self.watched.insert(uid.to_string(), sub);
    }

    /// Read a changed calendar again, once a burst of changes is over:
    /// the reads that arrive meanwhile are dropped, the one under way
    /// takes them in.
    pub(super) fn on_changed(&mut self, uid: String, sender: &ComponentSender<Self>) {
        if !self.reloading.insert(uid.clone()) {
            return;
        }
        let Some(cal) = self.calendars.iter().find(|c| c.uid == uid).cloned() else {
            self.reloading.remove(&uid);
            return;
        };
        let backend = self.backend;
        let s = sender.command_sender().clone();
        glib::timeout_add_local_once(StdDuration::from_millis(400), move || {
            std::thread::spawn(move || {
                let r = store::load(backend, &cal);
                let _ = s.send(Cmd::Loaded(cal.uid.clone(), r));
            });
        });
    }

    /// Make `changes` to calendar `uid`: shown at once, written in the
    /// background. `undo` is what puts it back.
    pub(super) fn write(&mut self, uid: &str, changes: Vec<Change>, undo: Option<Undo>, sender: &ComponentSender<Self>) {
        if changes.is_empty() {
            return;
        }
        if let Some(l) = self.loaded.get_mut(uid) {
            apply_local(&mut l.book, &changes);
        }
        self.undo = undo;
        self.refresh();
        let Some(cal) = self.calendars.iter().find(|c| c.uid == uid).cloned() else {
            return;
        };
        let backend = self.backend;
        let s = sender.command_sender().clone();
        std::thread::spawn(move || {
            let r = store::apply(backend, &cal, &changes);
            let _ = s.send(Cmd::Written(cal.uid.clone(), r));
        });
    }

    pub(super) fn on_written(&mut self, uid: String, r: Result<(), String>, sender: &ComponentSender<Self>) {
        if let Err(e) = r {
            let name = self
                .calendars
                .iter()
                .find(|c| c.uid == uid)
                .map(CalInfo::full_name)
                .unwrap_or_default();
            let _ = sender.output(CalendarOutput::Notice {
                text: i18n_f(
                    "Could not save the change to {calendar}: {e}",
                    &[("calendar", &name), ("e", &e)],
                ),
                error: true,
            });
            self.undo = None;
        }
        // What the calendar holds now: the change as the server kept it,
        // or as it was before a change that failed.
        self.on_changed(uid, sender);
    }

    /// Put the last change back.
    pub(super) fn undo_last(&mut self, sender: &ComponentSender<Self>) {
        let Some(u) = self.undo.take() else {
            return;
        };
        self.write(&u.calendar, u.changes, None, sender);
    }

    /// Show a toast that says what was done, with Undo when it can be.
    pub(super) fn done_toast(&self, text: &str, sender: &ComponentSender<Self>) {
        let t = adw::Toast::new(text);
        t.set_timeout(6);
        if self.undo.is_some() {
            t.set_button_label(Some(&i18n("Undo")));
            let s = sender.input_sender().clone();
            t.connect_button_clicked(move |_| {
                let _ = s.send(CalendarInput::Undo);
            });
        }
        self.parts.toasts.add_toast(t);
    }
}

/// `changes` made to a calendar's loaded copy, as the store will make them.
pub(crate) fn apply_local(book: &mut crate::calendar::model::Book, changes: &[Change]) {
    for c in changes {
        match c {
            Change::Create(node) | Change::Replace(node) => {
                if let Some(comp) = Comp::read(node) {
                    book.add(comp);
                }
            }
            Change::Remove { uid } => {
                book.series.remove(uid);
            }
            Change::RemoveOne { uid, rid } => {
                if let Some(s) = book.series.get_mut(uid) {
                    s.changed.retain(|c| c.rid.as_ref() != Some(rid));
                    if let Some(m) = s.master.as_ref() {
                        let mut node = m.node.clone();
                        node.push(rid.line("EXDATE"));
                        if let Some(comp) = Comp::read(&node) {
                            s.master = Some(std::sync::Arc::new(comp));
                        }
                    }
                }
            }
        }
    }
}
