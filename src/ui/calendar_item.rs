//! The dialog that makes an event or a task from a message (#564): a title,
//! where it goes, when, and notes, all filled in from the message first.
//!
//! An `adw::MessageDialog`, as Add to Contacts is (see `ui::dialog`).

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Timelike};
use gtk::glib;

use crate::calendar::{Item, ItemKind, Source, When};
use crate::i18n::{i18n, ni18n_f};

/// What the dialog opens with.
pub(crate) struct Start {
    pub(crate) kind: ItemKind,
    pub(crate) summary: String,
    pub(crate) notes: String,
    /// Where an item went last time, chosen again if it is still offered.
    pub(crate) last_source: String,
    /// The alarms the last event had, chosen again.
    pub(crate) last_alarms: Vec<u32>,
}

/// Show the dialog over `parent`. `done` gets the item and the UID of the
/// calendar or task list picked, or `None` with no `sources` to pick from.
#[allow(deprecated)] // adw::MessageDialog, see ui::dialog
pub(crate) fn show(
    parent: &gtk::Window,
    start: Start,
    sources: Vec<Source>,
    done: impl Fn(Item, Option<String>) + 'static,
) {
    let kind = start.kind;
    let heading = match kind {
        ItemKind::Event => i18n("New Event"),
        ItemKind::Task => i18n("New Task"),
    };
    let dialog = adw::MessageDialog::new(Some(parent), Some(&heading), None);
    dialog.add_response("cancel", &i18n("Cancel"));
    // With nothing in EDS the item goes to the calendar application, which
    // asks again; the button says where it is going.
    let confirm = if sources.is_empty() {
        i18n("Open in Calendar")
    } else {
        i18n("Add")
    };
    dialog.add_response("add", &confirm);
    dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("add"));
    dialog.set_close_response("cancel");

    let form = gtk::ListBox::new();
    form.add_css_class("boxed-list");
    form.set_selection_mode(gtk::SelectionMode::None);

    let title = adw::EntryRow::new();
    title.set_title(&i18n("Title"));
    title.set_text(start.summary.trim());
    form.append(&title);
    {
        let dialog = dialog.clone();
        let check = move |row: &adw::EntryRow| {
            dialog.set_response_enabled("add", !row.text().trim().is_empty());
        };
        check(&title);
        title.connect_changed(check);
    }

    let source_row = (!sources.is_empty()).then(|| {
        let row = adw::ComboRow::new();
        row.set_title(&match kind {
            ItemKind::Event => i18n("Calendar"),
            ItemKind::Task => i18n("Task list"),
        });
        let labels: Vec<&str> = sources.iter().map(|s| s.name.as_str()).collect();
        row.set_model(Some(&gtk::StringList::new(&labels)));
        if let Some(i) = sources.iter().position(|s| s.uid == start.last_source) {
            row.set_selected(i as u32);
        }
        form.append(&row);
        row
    });

    let when: Box<dyn Fn() -> (When, Vec<u32>)> = match kind {
        ItemKind::Event => event_rows(&form, &start.last_alarms),
        ItemKind::Task => task_rows(&form),
    };

    let notes = gtk::TextView::new();
    notes.set_wrap_mode(gtk::WrapMode::WordChar);
    notes.set_top_margin(8);
    notes.set_bottom_margin(8);
    notes.set_left_margin(10);
    notes.set_right_margin(10);
    notes.buffer().set_text(start.notes.trim());
    notes.buffer().place_cursor(&notes.buffer().start_iter());
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_min_content_height(140);
    scroller.set_max_content_height(140);
    scroller.add_css_class("card");
    scroller.set_overflow(gtk::Overflow::Hidden);
    scroller.set_child(Some(&notes));
    let notes_label = gtk::Label::new(Some(&i18n("Notes")));
    notes_label.set_halign(gtk::Align::Start);
    notes_label.add_css_class("heading");

    let bx = gtk::Box::new(gtk::Orientation::Vertical, 12);
    bx.append(&form);
    bx.append(&notes_label);
    bx.append(&scroller);
    dialog.set_extra_child(Some(&bx));

    dialog.connect_response(None, move |_, resp| {
        if resp != "add" {
            return;
        }
        let buffer = notes.buffer();
        let (when, alarms) = when();
        let item = Item {
            kind,
            summary: title.text().trim().to_string(),
            notes: buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string(),
            when,
            alarms,
        };
        let source = source_row
            .as_ref()
            .and_then(|row| sources.get(row.selected() as usize))
            .map(|s| s.uid.clone());
        done(item, source);
    });
    dialog.present();
}

/// All day, Starts, Ends and two alarms. Moving the start takes the end
/// along, so the event keeps its length.
fn event_rows(form: &gtk::ListBox, last_alarms: &[u32]) -> Box<dyn Fn() -> (When, Vec<u32>)> {
    let begin = next_hour();
    let start = Moment::new(begin);
    let end = Moment::new(begin + Duration::hours(1));
    let all_day = adw::SwitchRow::new();
    all_day.set_title(&i18n("All day"));
    form.append(&all_day);
    form.append(&start.row(&i18n("Starts")));
    form.append(&end.row(&i18n("Ends")));
    let alarms = [
        alarm_row(&i18n("Alarm"), last_alarms.first().copied()),
        alarm_row(&i18n("Second alarm"), last_alarms.get(1).copied()),
    ];
    for row in &alarms {
        form.append(row);
    }
    for m in [&start, &end] {
        all_day
            .bind_property("active", &m.time, "visible")
            .invert_boolean()
            .sync_create()
            .build();
    }
    {
        let (was, end_now) = (Rc::new(Cell::new(start.at())), end.clone());
        let start_now = start.clone();
        start.connect_changed(move || {
            let (Some(before), Some(after)) = (was.get(), start_now.at()) else {
                return;
            };
            was.set(Some(after));
            if let Some(e) = end_now.at() {
                end_now.set(e + (after - before));
            }
        });
    }
    Box::new(move || {
        let when = if all_day.is_active() {
            When::AllDay {
                first: start.date.get(),
                last: end.date.get().max(start.date.get()),
            }
        } else {
            let s = start.at().map_or(0, |t| t.timestamp());
            let e = end.at().map_or(s, |t| t.timestamp());
            When::Timed {
                start: s,
                end: e.max(s),
            }
        };
        let mut picked: Vec<u32> = alarms
            .iter()
            .filter_map(|row| ALARMS.get(row.selected() as usize).copied().flatten())
            .collect();
        // The same alarm twice would go off twice.
        picked.dedup();
        (when, picked)
    })
}

/// The alarms offered, in minutes before the start, the ones GNOME Calendar
/// offers and one at the start itself.
const ALARMS: [Option<u32>; 10] = [
    None,
    Some(0),
    Some(5),
    Some(10),
    Some(15),
    Some(30),
    Some(60),
    Some(24 * 60),
    Some(2 * 24 * 60),
    Some(7 * 24 * 60),
];

/// When one of the event's alarms goes off, `last` picked first.
fn alarm_row(title: &str, last: Option<u32>) -> adw::ComboRow {
    let labels: Vec<String> = ALARMS.iter().map(|a| alarm_label(*a)).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let row = adw::ComboRow::new();
    row.set_title(title);
    row.set_model(Some(&gtk::StringList::new(&labels)));
    if let Some(i) = ALARMS.iter().position(|a| *a == last) {
        row.set_selected(i as u32);
    }
    row
}

fn alarm_label(alarm: Option<u32>) -> String {
    const DAY: u32 = 24 * 60;
    let Some(m) = alarm else {
        return i18n("None");
    };
    let count = |n: u32| n.to_string();
    match m {
        0 => i18n("At the start"),
        m if m % (7 * DAY) == 0 => {
            let n = m / (7 * DAY);
            ni18n_f(
                "{n} week before",
                "{n} weeks before",
                n,
                &[("n", &count(n))],
            )
        }
        m if m % DAY == 0 => {
            let n = m / DAY;
            ni18n_f("{n} day before", "{n} days before", n, &[("n", &count(n))])
        }
        m if m % 60 == 0 => {
            let n = m / 60;
            ni18n_f(
                "{n} hour before",
                "{n} hours before",
                n,
                &[("n", &count(n))],
            )
        }
        n => ni18n_f(
            "{n} minute before",
            "{n} minutes before",
            n,
            &[("n", &count(n))],
        ),
    }
}

/// Due, with a switch: a task has no due day unless one is given.
fn task_rows(form: &gtk::ListBox) -> Box<dyn Fn() -> (When, Vec<u32>)> {
    let due = Moment::new(Local::now());
    due.time.set_visible(false);
    let on = gtk::Switch::new();
    on.set_valign(gtk::Align::Center);
    on.bind_property("active", &due.button, "sensitive")
        .sync_create()
        .build();
    let row = due.row(&i18n("Due"));
    row.add_suffix(&on);
    row.set_activatable_widget(Some(&on));
    form.append(&row);
    Box::new(move || {
        (
            When::Due(on.is_active().then(|| due.date.get())),
            Vec::new(),
        )
    })
}

/// The next whole hour from now.
fn next_hour() -> DateTime<Local> {
    let n = Local::now() + Duration::hours(1);
    n.with_minute(0)
        .and_then(|n| n.with_second(0))
        .and_then(|n| n.with_nanosecond(0))
        .unwrap_or(n)
}

/// A day on a button that opens a calendar, and a time on one that opens
/// an hour and a minute: side by side, they leave the row its title.
#[derive(Clone)]
struct Moment {
    date: Rc<Cell<NaiveDate>>,
    button: gtk::MenuButton,
    calendar: gtk::Calendar,
    hour: gtk::SpinButton,
    minute: gtk::SpinButton,
    /// The time's button, hidden for a whole day.
    time: gtk::MenuButton,
}

impl Moment {
    fn new(at: DateTime<Local>) -> Self {
        let calendar = gtk::Calendar::new();
        let popover = gtk::Popover::new();
        popover.set_child(Some(&calendar));
        let button = gtk::MenuButton::new();
        button.set_popover(Some(&popover));
        button.set_valign(gtk::Align::Center);
        let spin = |max: f64, step: f64| {
            let s = gtk::SpinButton::with_range(0.0, max, step);
            s.set_wrap(true);
            s.set_numeric(true);
            s.set_orientation(gtk::Orientation::Vertical);
            s.connect_output(|s| {
                s.set_text(&format!("{:02}", s.value_as_int()));
                glib::Propagation::Stop
            });
            s
        };
        let (hour, minute) = (spin(23.0, 1.0), spin(59.0, 5.0));
        let spins = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        spins.append(&hour);
        spins.append(&gtk::Label::new(Some(":")));
        spins.append(&minute);
        let time_popover = gtk::Popover::new();
        time_popover.set_child(Some(&spins));
        let time = gtk::MenuButton::new();
        time.set_popover(Some(&time_popover));
        time.set_valign(gtk::Align::Center);
        // Tabular figures, so 09:00 is as wide as 10:00 and the buttons of
        // Starts and Ends line up.
        button.add_css_class("numeric");
        time.add_css_class("numeric");
        let m = Moment {
            date: Rc::new(Cell::new(at.date_naive())),
            button,
            calendar,
            hour,
            minute,
            time,
        };
        m.set(at);
        for spin in [&m.hour, &m.minute] {
            let me = m.clone();
            spin.connect_value_changed(move |_| me.show_time());
        }
        let me = m.clone();
        m.calendar.connect_day_selected(move |c| {
            let d = c.date();
            if let Some(day) =
                NaiveDate::from_ymd_opt(d.year(), d.month() as u32, d.day_of_month() as u32)
            {
                me.show_date(day);
            }
            popover.popdown();
        });
        m
    }

    fn row(&self, title: &str) -> adw::ActionRow {
        // The title is a prefix rather than the row's own: that one wraps
        // anywhere, so in a dialog this narrow the buttons squeezed it to a
        // letter per line. A label that does not wrap keeps its word and
        // widens the dialog instead.
        let label = gtk::Label::new(Some(title));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        let row = adw::ActionRow::new();
        row.add_prefix(&label);
        row.update_property(&[gtk::accessible::Property::Label(title)]);
        let suffix = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        suffix.append(&self.button);
        suffix.append(&self.time);
        row.add_suffix(&suffix);
        row
    }

    /// The moment shown, in local time; `None` for a time the clocks skip.
    fn at(&self) -> Option<DateTime<Local>> {
        let t = self.date.get().and_hms_opt(
            self.hour.value_as_int() as u32,
            self.minute.value_as_int() as u32,
            0,
        )?;
        Local.from_local_datetime(&t).earliest()
    }

    fn set(&self, at: DateTime<Local>) {
        self.hour.set_value(at.hour() as f64);
        self.minute.set_value(at.minute() as f64);
        self.show_date(at.date_naive());
        self.show_time();
        if let Ok(d) =
            glib::DateTime::from_local(at.year(), at.month() as i32, at.day() as i32, 12, 0, 0.0)
        {
            self.calendar.select_day(&d);
        }
    }

    fn show_date(&self, day: NaiveDate) {
        let noon = day
            .and_hms_opt(12, 0, 0)
            .and_then(|t| Local.from_local_datetime(&t).earliest());
        self.date.set(day);
        self.button.set_label(&noon.map_or_else(String::new, |t| {
            crate::datefmt::day_month_year(t.timestamp())
        }));
    }

    /// The time on its button, on the clock the app uses.
    fn show_time(&self) {
        if let Some(t) = self.at() {
            self.time.set_label(&crate::datefmt::time(t.timestamp()));
        }
    }

    /// `f` runs whenever the day, the hour or the minute changes.
    fn connect_changed(&self, f: impl Fn() + 'static) {
        let f = Rc::new(f);
        let g = f.clone();
        self.calendar.connect_day_selected(move |_| g());
        let g = f.clone();
        self.hour.connect_value_changed(move |_| g());
        self.minute.connect_value_changed(move |_| f());
    }
}
