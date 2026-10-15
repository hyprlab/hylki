//! Choosing a time to put something off until: Snooze's menu of presets, and
//! the date-and-time dialog it shares with Send Later (#145).

use adw::prelude::*;

use crate::i18n::i18n;
use crate::ui::context_menu::MenuEntry;

/// The icon Snooze wears everywhere: the toolbar, the menus, the sidebar.
pub const SNOOZE_ICON: &str = "alarm-symbolic";

/// Snooze's menu: the presets from [`crate::later::snooze_presets`], then
/// "Pick a date and time…". `on_pick` is called with the chosen time, or
/// with `None` for the dialog, which the caller opens over its own window.
pub fn snooze_sections(on_pick: impl Fn(Option<i64>) + Clone + 'static) -> Vec<Vec<MenuEntry>> {
    let presets = crate::later::snooze_presets(crate::datefmt::now())
        .into_iter()
        .map(|p| {
            let pick = on_pick.clone();
            MenuEntry::new(p.label, move || pick(Some(p.at)))
        })
        .collect();
    let custom = MenuEntry::new(i18n("Pick a date and time…"), move || on_pick(None));
    vec![presets, vec![custom]]
}

/// A calendar and an hour/minute pair, starting from `current` if there is
/// one, else the next full hour. `on_pick` gets the time chosen; a time
/// already past is refused with a word of explanation rather than taken to
/// mean "now".
pub fn pick_time(
    parent: &impl IsA<gtk::Widget>,
    title: &str,
    confirm: &str,
    current: Option<i64>,
    on_pick: impl Fn(i64) + 'static,
) {
    use chrono::{Datelike, Local, TimeZone, Timelike};
    let start = match current {
        Some(t) => Local.timestamp_opt(t, 0).single().unwrap_or_else(Local::now),
        None => {
            let n = Local::now() + chrono::Duration::hours(1);
            n.with_minute(0).and_then(|n| n.with_second(0)).unwrap_or(n)
        }
    };
    let dialog = adw::AlertDialog::new(Some(title), None);
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
        let ndt = chrono::NaiveDate::from_ymd_opt(d.year(), d.month() as u32, d.day_of_month() as u32)?
            .and_hms_opt(hour.value_as_int() as u32, minute.value_as_int() as u32, 0)?;
        Local.from_local_datetime(&ndt).earliest().map(|t| t.timestamp())
    };
    let parent: gtk::Widget = parent.clone().upcast();
    let anchor = parent.clone();
    dialog.connect_response(None, move |_, resp| {
        if resp != "ok" {
            return;
        }
        match chosen() {
            Some(t) if t > crate::datefmt::now() => on_pick(t),
            _ => {
                // An explicit time in the past is a slip, not a request to
                // act at once.
                let d = adw::AlertDialog::new(
                    Some(i18n("That time has passed").as_str()),
                    Some(i18n("Choose a time later than now.").as_str()),
                );
                d.add_response("ok", &i18n("OK"));
                d.present(Some(&anchor));
            }
        }
    });
    dialog.present(Some(&parent));
}
