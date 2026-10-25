//! Picking a time later than now, for Send Later (#145) and Snooze (#527):
//! the preset times both menus offer, and the calendar dialog behind
//! "Pick a date and time…".

use adw::prelude::*;

use crate::i18n::{i18n, i18n_f};
use crate::ui::dialog::alert;

/// `days` from today at `hour`:00, local time.
pub(crate) fn preset_time(days: i64, hour: u32) -> i64 {
    use chrono::{Duration, Local, TimeZone};
    let day = (Local::now() + Duration::days(days)).date_naive();
    let ndt = day.and_hms_opt(hour, 0, 0).unwrap_or_default();
    Local
        .from_local_datetime(&ndt)
        .single()
        .map(|t| t.timestamp())
        .unwrap_or_else(crate::datefmt::now)
}

/// How many days ahead the coming `weekday` is (0 = Monday): a week when
/// it is today.
fn days_until(today: u32, weekday: u32) -> i64 {
    let ahead = (7 + weekday as i64 - today as i64) % 7;
    if ahead == 0 {
        7
    } else {
        ahead
    }
}

/// The coming Monday at `hour`:00 local time (a Monday today means next week's).
pub(crate) fn next_monday(hour: u32) -> i64 {
    use chrono::Datelike;
    let today = chrono::Local::now().weekday().num_days_from_monday();
    preset_time(days_until(today, 0), hour)
}

/// One preset of a "later" menu: its label and the time it stands for.
pub(crate) struct LaterPreset {
    pub(crate) label: String,
    pub(crate) at: i64,
}

/// The times a snooze is offered (#527), soonest first: this evening while
/// there is time left in the day, tomorrow morning and afternoon, Saturday
/// from Monday to Thursday, and Monday unless that is tomorrow.
pub(crate) fn snooze_presets() -> Vec<LaterPreset> {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    let today = now.weekday().num_days_from_monday();
    let mut presets = Vec::new();
    if now.hour() < 17 {
        presets.push(LaterPreset {
            label: i18n_f(
                "Later today ({time})",
                &[("time", &crate::datefmt::clock_label(18))],
            ),
            at: preset_time(0, 18),
        });
    }
    presets.push(LaterPreset {
        label: i18n_f(
            "Tomorrow morning ({time})",
            &[("time", &crate::datefmt::clock_label(8))],
        ),
        at: preset_time(1, 8),
    });
    presets.push(LaterPreset {
        label: i18n_f(
            "Tomorrow afternoon ({time})",
            &[("time", &crate::datefmt::clock_label(13))],
        ),
        at: preset_time(1, 13),
    });
    // Friday's tomorrow is Saturday already.
    if today < 4 {
        presets.push(LaterPreset {
            label: i18n_f(
                "Saturday morning ({time})",
                &[("time", &crate::datefmt::clock_label(8))],
            ),
            at: preset_time(days_until(today, 5), 8),
        });
    }
    // Sunday's tomorrow morning is Monday's.
    if today != 6 {
        presets.push(LaterPreset {
            label: i18n_f(
                "Monday morning ({time})",
                &[("time", &crate::datefmt::clock_label(8))],
            ),
            at: next_monday(8),
        });
    }
    presets
}

/// What a choice in the Snooze menu asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnoozeChoice {
    /// Until this time.
    Until(i64),
    /// A time from the calendar.
    Pick,
    /// Back now, out of the Snoozed folder.
    Wake,
}

/// The Snooze menu's sections (#527): the presets, the calendar, and, for
/// mail already snoozed, Unsnooze.
pub(crate) fn snooze_menu(
    snoozed: bool,
    choose: impl Fn(SnoozeChoice) + Clone + 'static,
) -> Vec<Vec<crate::ui::context_menu::MenuEntry>> {
    use crate::ui::context_menu::MenuEntry;
    let entry = |label: String, choice: SnoozeChoice| {
        let choose = choose.clone();
        MenuEntry::new(label, move || choose(choice))
    };
    let presets = snooze_presets()
        .into_iter()
        .map(|p| entry(p.label, SnoozeChoice::Until(p.at)))
        .collect();
    let mut sections = vec![
        presets,
        vec![entry(i18n("Pick a date and time…"), SnoozeChoice::Pick)],
    ];
    if snoozed {
        sections.push(vec![
            entry(i18n("Unsnooze"), SnoozeChoice::Wake).icon("mail-inbox-symbolic")
        ]);
    }
    sections
}

/// A calendar and an hour/minute pair, starting from `current` if there is
/// one, else the next full hour; `pick` gets the time chosen. A time already
/// past is refused rather than taken to mean "now" by surprise.
#[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
pub(crate) fn pick_later_time(
    parent: Option<&gtk::Window>,
    current: Option<i64>,
    title: &str,
    confirm: &str,
    pick: impl Fn(i64) + 'static,
) {
    use chrono::{Datelike, Local, TimeZone, Timelike};
    let start = match current {
        Some(t) => Local
            .timestamp_opt(t, 0)
            .single()
            .unwrap_or_else(Local::now),
        None => {
            let n = Local::now() + chrono::Duration::hours(1);
            n.with_minute(0).and_then(|n| n.with_second(0)).unwrap_or(n)
        }
    };
    let dialog = adw::MessageDialog::new(parent, Some(title), None);
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("ok", confirm);
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let calendar = gtk::Calendar::new();
    calendar.set_year(start.year());
    calendar.set_month(start.month0() as i32);
    calendar.set_day(start.day() as i32);
    bx.append(&calendar);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.set_halign(gtk::Align::Center);
    let hour = gtk::SpinButton::with_range(0.0, 23.0, 1.0);
    hour.set_value(start.hour() as f64);
    hour.set_orientation(gtk::Orientation::Vertical);
    hour.set_wrap(true);
    let minute = gtk::SpinButton::with_range(0.0, 55.0, 5.0);
    minute.set_value((start.minute() / 5 * 5) as f64);
    minute.set_orientation(gtk::Orientation::Vertical);
    minute.set_wrap(true);
    row.append(&hour);
    row.append(&gtk::Label::new(Some(":")));
    row.append(&minute);
    bx.append(&row);
    dialog.set_extra_child(Some(&bx));
    let chosen = move || -> Option<i64> {
        let d = calendar.date();
        let ndt =
            chrono::NaiveDate::from_ymd_opt(d.year(), d.month() as u32, d.day_of_month() as u32)?
                .and_hms_opt(hour.value_as_int() as u32, minute.value_as_int() as u32, 0)?;
        Local
            .from_local_datetime(&ndt)
            .single()
            .map(|t| t.timestamp())
    };
    dialog.connect_response(None, move |dlg, resp| {
        if resp != "ok" {
            return;
        }
        match chosen() {
            Some(t) if t > crate::datefmt::now() => pick(t),
            _ => alert(
                dlg.transient_for().as_ref(),
                i18n("That time has passed").as_str(),
                i18n("Choose a time later than now.").as_str(),
            ),
        }
    });
    dialog.present();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_coming_weekday_is_never_today() {
        // Monday to Monday is a week; Friday to Saturday is one day.
        assert_eq!(days_until(0, 0), 7);
        assert_eq!(days_until(4, 5), 1);
        assert_eq!(days_until(5, 0), 2);
        assert_eq!(days_until(6, 5), 6);
    }

    #[test]
    fn every_snooze_preset_is_later_than_now() {
        let now = crate::datefmt::now();
        let presets = snooze_presets();
        assert!(presets.len() >= 3);
        assert!(presets.iter().all(|p| p.at > now));
        assert!(presets.windows(2).all(|w| w[0].at <= w[1].at));
    }
}
