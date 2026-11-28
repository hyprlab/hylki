//! Settings → Calendar (#715): the shape of the week and the working day,
//! what a new event starts with, and declined meetings and reminders.
//! Saved to `calendar.toml`, which the calendar reads.

use std::rc::Rc;

use adw::prelude::*;

use crate::calendar::prefs::{CalendarPrefs, Reminders, WeekStart};
use crate::i18n::{i18n, ni18n_f};
use crate::ui::calendar_item::{alarm_label, ALARMS};

const LENGTHS: [u32; 6] = [15, 30, 45, 60, 90, 120];

fn length_label(m: u32) -> String {
    if m.is_multiple_of(60) {
        let h = m / 60;
        ni18n_f("{n} hour", "{n} hours", h, &[("n", &h.to_string())])
    } else if m > 60 {
        crate::i18n::i18n_f("{n} hours", &[("n", &format!("{:.1}", f64::from(m) / 60.0))])
    } else {
        ni18n_f("{n} minute", "{n} minutes", m, &[("n", &m.to_string())])
    }
}

fn combo(title: &str, labels: &[String], selected: usize) -> adw::ComboRow {
    let row = adw::ComboRow::new();
    row.set_title(title);
    let l: Vec<&str> = labels.iter().map(String::as_str).collect();
    row.set_model(Some(&gtk::StringList::new(&l)));
    row.set_selected(selected as u32);
    row
}

/// The page. `changed` gets the settings after every change, saved.
pub(crate) fn build(changed: impl Fn(CalendarPrefs) + 'static) -> adw::PreferencesPage {
    let changed: Rc<dyn Fn(CalendarPrefs)> = Rc::new(changed);
    let page = adw::PreferencesPage::new();
    let prefs = CalendarPrefs::load();
    // Each row changes one field of what is on disk now: the page keeps
    // its own layout (the view, hidden calendars) in the same file.
    let update = {
        let changed = changed.clone();
        move |f: &dyn Fn(&mut CalendarPrefs)| {
            let mut p = CalendarPrefs::load();
            f(&mut p);
            p.save();
            changed(p);
        }
    };
    let update = Rc::new(update);

    let week = adw::PreferencesGroup::new();
    week.set_title(&i18n("Week"));
    let starts = [WeekStart::System, WeekStart::Sunday, WeekStart::Monday, WeekStart::Saturday];
    let start_row = combo(
        &i18n("First day of the week"),
        &[i18n("As the system has it"), i18n("Sunday"), i18n("Monday"), i18n("Saturday")],
        starts.iter().position(|s| *s == prefs.week_start).unwrap_or(0),
    );
    let u = update.clone();
    start_row.connect_selected_notify(move |r| {
        let s = starts[r.selected() as usize % 4];
        u(&|p| p.week_start = s);
    });
    week.add(&start_row);
    let days_row = combo(
        &i18n("Week view shows"),
        &[i18n("All seven days"), i18n("Monday to Friday")],
        usize::from(prefs.work_week),
    );
    let u = update.clone();
    days_row.connect_selected_notify(move |r| {
        let on = r.selected() == 1;
        u(&|p| p.work_week = on);
    });
    week.add(&days_row);
    let numbers = adw::SwitchRow::new();
    numbers.set_title(&i18n("Show week numbers"));
    numbers.set_active(prefs.week_numbers);
    let u = update.clone();
    numbers.connect_active_notify(move |r| {
        let on = r.is_active();
        u(&|p| p.week_numbers = on);
    });
    week.add(&numbers);
    page.add(&week);

    let day = adw::PreferencesGroup::new();
    day.set_title(&i18n("Working Hours"));
    day.set_description(Some(&i18n("Shaded outside them. Day and Week open scrolled to their start.")));
    let hours: Vec<String> = (0..=24).map(crate::ui::calendar_page::hour_name).collect();
    let from = combo(&i18n("From"), &hours[..24], prefs.work_start as usize);
    let to = combo(&i18n("To"), &hours[1..], prefs.work_end.saturating_sub(1) as usize);
    let u = update.clone();
    from.connect_selected_notify(move |r| {
        let h = r.selected();
        u(&|p| {
            p.work_start = h;
            p.work_end = p.work_end.max(h + 1);
        });
    });
    let u = update.clone();
    to.connect_selected_notify(move |r| {
        let h = r.selected() + 1;
        u(&|p| {
            p.work_end = h;
            p.work_start = p.work_start.min(h - 1);
        });
    });
    day.add(&from);
    day.add(&to);
    page.add(&day);

    let new = adw::PreferencesGroup::new();
    new.set_title(&i18n("New Events"));
    let lengths: Vec<String> = LENGTHS.iter().map(|m| length_label(*m)).collect();
    let length = combo(
        &i18n("Length"),
        &lengths,
        LENGTHS.iter().position(|m| *m == prefs.default_minutes).unwrap_or(3),
    );
    let u = update.clone();
    length.connect_selected_notify(move |r| {
        let m = LENGTHS[r.selected() as usize % LENGTHS.len()];
        u(&|p| p.default_minutes = m);
    });
    new.add(&length);
    let alerts: Vec<String> = ALARMS.iter().map(|a| alarm_label(*a)).collect();
    let alert = combo(
        &i18n("Alert"),
        &alerts,
        ALARMS.iter().position(|a| *a == prefs.default_alarm).unwrap_or(0),
    );
    let u = update.clone();
    alert.connect_selected_notify(move |r| {
        let a = ALARMS.get(r.selected() as usize).copied().flatten();
        u(&|p| p.default_alarm = a);
    });
    new.add(&alert);
    // The calendars are read from EDS, which can take a moment.
    let cal_row = adw::ComboRow::new();
    cal_row.set_title(&i18n("Calendar"));
    cal_row.set_sensitive(false);
    new.add(&cal_row);
    {
        let (row, u, chosen) = (cal_row.clone(), update.clone(), prefs.default_calendar.clone());
        gtk::glib::spawn_future_local(async move {
            let list = gtk::gio::spawn_blocking(|| {
                let backend = crate::calendar::store::backend();
                crate::calendar::store::calendars(backend, Default::default()).unwrap_or_default()
            })
            .await
            .unwrap_or_default();
            let list: Vec<_> = list.into_iter().filter(|c| c.writable && !c.tasks).collect();
            if list.is_empty() {
                return;
            }
            let names: Vec<String> = list.iter().map(|c| c.full_name()).collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            row.set_model(Some(&gtk::StringList::new(&names)));
            row.set_factory(Some(&crate::ui::contacts_page::book_label_factory(true)));
            row.set_list_factory(Some(&crate::ui::contacts_page::book_label_factory(false)));
            if let Some(i) = list.iter().position(|c| c.uid == chosen) {
                row.set_selected(i as u32);
            }
            row.set_sensitive(true);
            let uids: Vec<String> = list.into_iter().map(|c| c.uid).collect();
            row.connect_selected_notify(move |r| {
                if let Some(uid) = uids.get(r.selected() as usize).cloned() {
                    u(&|p| p.default_calendar = uid.clone());
                }
            });
        });
    }
    page.add(&new);

    let other = adw::PreferencesGroup::new();
    other.set_title(&i18n("Meetings and Reminders"));
    let declined = adw::SwitchRow::new();
    declined.set_title(&i18n("Show declined meetings"));
    declined.set_active(prefs.show_declined);
    let u = update.clone();
    declined.connect_active_notify(move |r| {
        let on = r.is_active();
        u(&|p| p.show_declined = on);
    });
    other.add(&declined);
    let kinds = [Reminders::Automatic, Reminders::Always, Reminders::Never];
    let reminders = combo(
        &i18n("Show reminders"),
        &[i18n("Automatic"), i18n("Always"), i18n("Never")],
        kinds.iter().position(|k| *k == prefs.reminders).unwrap_or(0),
    );
    reminders.set_subtitle(&i18n("Automatic leaves them to the desktop where it shows them."));
    let u = update.clone();
    reminders.connect_selected_notify(move |r| {
        let k = kinds[r.selected() as usize % 3];
        u(&|p| p.reminders = k);
    });
    other.add(&reminders);
    page.add(&other);
    page
}
