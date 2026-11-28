//! The calendars' own dialogs: a new calendar on this computer, a
//! subscription to a published calendar, importing a calendar file, a
//! calendar's name and color, and removing one.

use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use super::{CalendarOutput, CalendarPage, Cmd};
use crate::calendar::store::{self, CalInfo, CalKind};
use crate::calendar::PALETTE;
use crate::i18n::{i18n, i18n_f};

/// A row of color swatches to pick from, the chosen one ringed.
fn swatches(chosen: &str) -> (gtk::Box, Rc<std::cell::RefCell<String>>) {
    let picked = Rc::new(std::cell::RefCell::new(chosen.to_string()));
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.set_halign(gtk::Align::Center);
    let mut first: Option<gtk::ToggleButton> = None;
    let mut colors: Vec<String> = PALETTE.iter().map(|c| c.to_string()).collect();
    if !colors.iter().any(|c| c.eq_ignore_ascii_case(chosen)) && chosen.starts_with('#') {
        colors.insert(0, chosen.to_string());
    }
    for color in colors {
        let b = gtk::ToggleButton::new();
        b.add_css_class("calendar-swatch");
        b.add_css_class("circular");
        let c = color.clone();
        let area = gtk::DrawingArea::new();
        area.set_content_width(20);
        area.set_content_height(20);
        let rgba = super::paint::Rgba::hex(&c);
        area.set_draw_func(move |_, cr, w, h| {
            super::paint::dot(cr, f64::from(w) / 2.0, f64::from(h) / 2.0, 10.0, rgba);
        });
        b.set_child(Some(&area));
        b.set_tooltip_text(Some(&color));
        match &first {
            Some(f) => b.set_group(Some(f)),
            None => first = Some(b.clone()),
        }
        b.set_active(color.eq_ignore_ascii_case(chosen));
        let p = picked.clone();
        b.connect_toggled(move |b| {
            if b.is_active() {
                *p.borrow_mut() = c.clone();
            }
        });
        row.append(&b);
    }
    (row, picked)
}

impl CalendarPage {
    fn alert(&self, heading: &str, body: Option<&str>) -> adw::AlertDialog {
        let d = adw::AlertDialog::new(Some(heading), body);
        d.add_response("cancel", &i18n("Cancel"));
        d.set_close_response("cancel");
        d
    }

    pub(super) fn look_dialog(&self, cal: CalInfo, sender: &ComponentSender<Self>) {
        let named = matches!(cal.kind, CalKind::Local | CalKind::Subscription);
        let d = self.alert(&if named { i18n("Name and Color") } else { i18n("Color") }, None);
        d.add_response("save", &i18n("Save"));
        d.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("save"));
        let bx = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let name = gtk::Entry::new();
        name.set_text(&cal.name);
        name.set_activates_default(true);
        name.set_visible(named);
        bx.append(&name);
        let (row, picked) = swatches(&cal.color);
        bx.append(&row);
        d.set_extra_child(Some(&bx));
        let s = sender.command_sender().clone();
        let backend = self.backend;
        d.connect_response(Some("save"), move |_, _| {
            let (cal, n, c) = (cal.clone(), name.text().to_string(), picked.borrow().clone());
            let s = s.clone();
            std::thread::spawn(move || {
                let r = store::set_look(backend, &cal, &n, &c);
                if let Err(e) = r {
                    tracing::warn!("calendar look not saved: {e}");
                }
                let _ = s.send(Cmd::Reload);
            });
        });
        d.present(Some(&self.parts.views));
    }

    pub(super) fn new_calendar_dialog(&self, sender: &ComponentSender<Self>) {
        let d = self.alert(&i18n("New Calendar"), Some(&i18n("Kept on this computer.")));
        d.add_response("add", &i18n("Add"));
        d.set_response_appearance("add", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("add"));
        d.set_response_enabled("add", false);
        let bx = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let name = gtk::Entry::new();
        name.set_placeholder_text(Some(&i18n("Name")));
        name.set_activates_default(true);
        let dd = d.clone();
        name.connect_changed(move |e| dd.set_response_enabled("add", !e.text().trim().is_empty()));
        bx.append(&name);
        let used: Vec<String> = self.calendars.iter().map(|c| c.color.to_lowercase()).collect();
        let fresh = PALETTE
            .iter()
            .find(|c| !used.contains(&c.to_string()))
            .unwrap_or(&PALETTE[0]);
        let (row, picked) = swatches(fresh);
        bx.append(&row);
        d.set_extra_child(Some(&bx));
        let s = sender.command_sender().clone();
        let out = sender.output_sender().clone();
        let backend = self.backend;
        d.connect_response(Some("add"), move |_, _| {
            let (n, c) = (name.text().trim().to_string(), picked.borrow().clone());
            let (s, out) = (s.clone(), out.clone());
            std::thread::spawn(move || {
                match store::new_calendar(backend, &n, &c) {
                    Ok(_) => {}
                    Err(e) => {
                        let _ = out.send(CalendarOutput::Notice {
                            text: i18n_f("Could not add the calendar: {e}", &[("e", &e)]),
                            error: true,
                        });
                    }
                }
                // EDS takes a moment to list a source it has just made.
                std::thread::sleep(std::time::Duration::from_millis(400));
                let _ = s.send(Cmd::Reload);
            });
        });
        d.present(Some(&self.parts.views));
    }

    pub(super) fn subscribe_dialog(&self, sender: &ComponentSender<Self>) {
        let d = self.alert(
            &i18n("Subscribe to a Calendar"),
            Some(&i18n("The address of a published calendar, such as a webcal:// or .ics link. It is checked for changes every hour.")),
        );
        d.add_response("add", &i18n("Subscribe"));
        d.set_response_appearance("add", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("add"));
        d.set_response_enabled("add", false);
        let bx = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let url = gtk::Entry::new();
        url.set_placeholder_text(Some("webcal://example.org/calendar.ics"));
        url.set_input_purpose(gtk::InputPurpose::Url);
        url.set_activates_default(true);
        let dd = d.clone();
        url.connect_changed(move |e| dd.set_response_enabled("add", e.text().contains('.')));
        bx.append(&url);
        let name = gtk::Entry::new();
        name.set_placeholder_text(Some(&i18n("Name")));
        name.set_activates_default(true);
        bx.append(&name);
        let (row, picked) = swatches(PALETTE[5]);
        bx.append(&row);
        d.set_extra_child(Some(&bx));
        let s = sender.command_sender().clone();
        let out = sender.output_sender().clone();
        let backend = self.backend;
        d.connect_response(Some("add"), move |_, _| {
            let (u, n, c) = (url.text().trim().to_string(), name.text().trim().to_string(), picked.borrow().clone());
            let (s, out) = (s.clone(), out.clone());
            std::thread::spawn(move || {
                if let Err(e) = store::subscribe(backend, &u, &n, &c) {
                    let _ = out.send(CalendarOutput::Notice {
                        text: i18n_f("Could not subscribe: {e}", &[("e", &e)]),
                        error: true,
                    });
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(400));
                let _ = s.send(Cmd::Reload);
            });
        });
        d.present(Some(&self.parts.views));
    }

    pub(super) fn import_dialog(&self, sender: &ComponentSender<Self>) {
        let targets: Vec<CalInfo> = {
            let ctx = self.ctx.borrow();
            self.calendars
                .iter()
                .filter(|c| ctx.writable.contains(&c.uid))
                .cloned()
                .collect()
        };
        if targets.is_empty() {
            self.toast(&i18n("No calendar here takes new events."));
            return;
        }
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&i18n("Calendar files")));
        filter.add_mime_type("text/calendar");
        filter.add_suffix("ics");
        let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let chooser = gtk::FileDialog::new();
        chooser.set_title(&i18n("Import Events"));
        chooser.set_filters(Some(&filters));
        let window = self.parts.views.root().and_downcast::<gtk::Window>();
        let parent = self.parts.views.clone();
        let s = sender.command_sender().clone();
        let out = sender.output_sender().clone();
        let backend = self.backend;
        let default = self.default_calendar_uid();
        chooser.open(window.as_ref(), gtk::gio::Cancellable::NONE, move |r| {
            let Ok(file) = r else {
                return;
            };
            let Some(path) = file.path() else {
                return;
            };
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    let _ = out.send(CalendarOutput::Notice {
                        text: i18n_f("Could not read the file: {e}", &[("e", &e.to_string())]),
                        error: true,
                    });
                    return;
                }
            };
            // Which calendar the events go to.
            let d = adw::AlertDialog::new(Some(&i18n("Import Events")), None);
            d.add_response("cancel", &i18n("Cancel"));
            d.add_response("import", &i18n("Import"));
            d.set_response_appearance("import", adw::ResponseAppearance::Suggested);
            d.set_close_response("cancel");
            let names: Vec<String> = targets.iter().map(CalInfo::full_name).collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            let pick = gtk::DropDown::from_strings(&names);
            if let Some(i) = targets.iter().position(|c| Some(&c.uid) == default.as_ref()) {
                pick.set_selected(i as u32);
            }
            d.set_extra_child(Some(&pick));
            let (s, out, targets) = (s.clone(), out.clone(), targets.clone());
            d.connect_response(Some("import"), move |_, _| {
                let Some(cal) = targets.get(pick.selected() as usize).cloned() else {
                    return;
                };
                let (s, out, text) = (s.clone(), out.clone(), text.clone());
                std::thread::spawn(move || {
                    let (msg, error) = match store::import(backend, &cal, &text) {
                        Ok(n) => (
                            crate::i18n::ni18n_f(
                                "Imported {n} event into {calendar}",
                                "Imported {n} events into {calendar}",
                                n as u32,
                                &[("n", &n.to_string()), ("calendar", &cal.full_name())],
                            ),
                            false,
                        ),
                        Err(e) => (i18n_f("Could not import: {e}", &[("e", &e)]), true),
                    };
                    let _ = out.send(CalendarOutput::Notice { text: msg, error });
                    let _ = s.send(Cmd::Changed(cal.uid.clone()));
                });
            });
            d.present(Some(&parent));
        });
    }

    fn default_calendar_uid(&self) -> Option<String> {
        let ctx = self.ctx.borrow();
        self.calendars
            .iter()
            .filter(|c| ctx.writable.contains(&c.uid))
            .find(|c| c.uid == self.prefs.default_calendar)
            .or_else(|| self.calendars.iter().find(|c| ctx.writable.contains(&c.uid)))
            .map(|c| c.uid.clone())
    }

    pub(super) fn delete_calendar(&self, cal: CalInfo, sender: &ComponentSender<Self>) {
        let (heading, body) = if cal.kind == CalKind::Subscription {
            (
                i18n_f("Unsubscribe from {name}?", &[("name", &cal.name)]),
                i18n("Its events go from here. The published calendar is not changed."),
            )
        } else {
            (
                i18n_f("Delete {name}?", &[("name", &cal.name)]),
                i18n("The calendar and every event in it are deleted from this computer."),
            )
        };
        let d = self.alert(&heading, Some(&body));
        d.add_response("delete", &if cal.kind == CalKind::Subscription { i18n("Unsubscribe") } else { i18n("Delete") });
        d.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        let s = sender.command_sender().clone();
        let backend = self.backend;
        d.connect_response(Some("delete"), move |_, _| {
            let (cal, s) = (cal.clone(), s.clone());
            std::thread::spawn(move || {
                if let Err(e) = store::delete_calendar(backend, &cal) {
                    tracing::warn!("calendar not removed: {e}");
                }
                std::thread::sleep(std::time::Duration::from_millis(300));
                let _ = s.send(Cmd::Reload);
            });
        });
        d.present(Some(&self.parts.views));
    }
}
