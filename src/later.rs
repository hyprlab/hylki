//! The times a message can be put off until: Send Later's presets (#145) and
//! Snooze's, which offer the same ones plus "Later today".
//!
//! Everything is worked out in local time from a clock reading passed in,
//! rather than read here, so the demo's clock (`HYLKI_DEMO_NOW`) is the one
//! the presets count from, and the tests can pin both the clock and the time
//! zone.

use chrono::{Datelike, Duration, Local, TimeZone, Timelike};

/// The hour "morning" means in a preset's name.
pub const MORNING: u32 = 8;
/// The hour "afternoon" means.
pub const AFTERNOON: u32 = 13;
/// "Later today" is not offered once three hours on would be past this hour:
/// a message snoozed at ten at night comes back tomorrow morning instead.
const LATE: u32 = 21;

/// `days` after the day of `now`, at `hour`:00, local time.
pub fn day_at(now: i64, days: i64, hour: u32) -> i64 {
    day_at_in(&Local, now, days, hour)
}

/// The coming Monday at `hour`:00 local time (a Monday today means next
/// week's).
pub fn next_monday(now: i64, hour: u32) -> i64 {
    next_monday_in(&Local, now, hour)
}

fn day_at_in<Tz: TimeZone>(tz: &Tz, now: i64, days: i64, hour: u32) -> i64 {
    let Some(today) = tz.timestamp_opt(now, 0).single() else { return now };
    let day = (today + Duration::days(days)).date_naive();
    let Some(at) = day.and_hms_opt(hour, 0, 0) else { return now };
    // A clock change can skip the hour (spring) or pass it twice (autumn):
    // the first of two, or the hour as UTC would have it when it is skipped.
    tz.from_local_datetime(&at)
        .earliest()
        .map(|t| t.timestamp())
        .unwrap_or_else(|| at.and_utc().timestamp())
}

fn next_monday_in<Tz: TimeZone>(tz: &Tz, now: i64, hour: u32) -> i64 {
    let weekday = tz
        .timestamp_opt(now, 0)
        .single()
        .map(|t| t.weekday().num_days_from_monday() as i64)
        .unwrap_or(0);
    let ahead = (7 - weekday) % 7;
    day_at_in(tz, now, if ahead == 0 { 7 } else { ahead }, hour)
}

/// "Later today": three hours from `now`, on the hour. `None` when that would
/// be late in the evening, or tomorrow already.
fn later_today_in<Tz: TimeZone>(tz: &Tz, now: i64) -> Option<i64> {
    let here = tz.timestamp_opt(now, 0).single()?;
    let on = here.clone() + Duration::hours(3);
    // Up to the next full hour: 14:20 becomes 18:00, 14:00 stays 17:00.
    let hour = if on.minute() == 0 && on.second() == 0 { on.hour() } else { on.hour() + 1 };
    if on.date_naive() != here.date_naive() || hour > LATE {
        return None;
    }
    Some(day_at_in(tz, now, 0, hour))
}

/// One of the presets a menu offers: its label, already translated, and the
/// time it stands for.
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub label: String,
    pub at: i64,
}

/// The times Snooze offers from `now`, soonest first: later today (while the
/// day has some left), tomorrow morning and afternoon, and Monday morning,
/// the same presets Send Later has (#145). "Pick a date and time…" is the
/// menu's own last entry.
pub fn snooze_presets(now: i64) -> Vec<Preset> {
    snooze_times(&Local, now)
        .into_iter()
        .map(|(kind, at)| Preset { label: preset_label(kind, at), at })
        .collect()
}

/// Which preset a time is, for its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PresetKind {
    LaterToday,
    TomorrowMorning,
    TomorrowAfternoon,
    MondayMorning,
}

fn snooze_times<Tz: TimeZone>(tz: &Tz, now: i64) -> Vec<(PresetKind, i64)> {
    let mut out = Vec::new();
    if let Some(at) = later_today_in(tz, now) {
        out.push((PresetKind::LaterToday, at));
    }
    out.push((PresetKind::TomorrowMorning, day_at_in(tz, now, 1, MORNING)));
    out.push((PresetKind::TomorrowAfternoon, day_at_in(tz, now, 1, AFTERNOON)));
    // On a Sunday, Monday morning is tomorrow morning: offered once.
    let monday = next_monday_in(tz, now, MORNING);
    if !out.iter().any(|(_, at)| *at == monday) {
        out.push((PresetKind::MondayMorning, monday));
    }
    out
}

fn preset_label(kind: PresetKind, at: i64) -> String {
    use crate::i18n::i18n_f;
    let hour = Local.timestamp_opt(at, 0).single().map(|t| t.hour()).unwrap_or(MORNING);
    let time = crate::datefmt::clock_label(hour);
    match kind {
        PresetKind::LaterToday => i18n_f("Later today ({time})", &[("time", &time)]),
        PresetKind::TomorrowMorning => i18n_f("Tomorrow morning ({time})", &[("time", &time)]),
        PresetKind::TomorrowAfternoon => i18n_f("Tomorrow afternoon ({time})", &[("time", &time)]),
        PresetKind::MondayMorning => i18n_f("Monday morning ({time})", &[("time", &time)]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, NaiveDate};

    /// Central European summer time, with no clock changes to get in the way.
    fn cest() -> FixedOffset {
        FixedOffset::east_opt(2 * 3600).unwrap()
    }

    fn at(d: u32, h: u32, m: u32) -> i64 {
        let naive = NaiveDate::from_ymd_opt(2026, 10, d).unwrap().and_hms_opt(h, m, 0).unwrap();
        cest().from_local_datetime(&naive).single().unwrap().timestamp()
    }

    #[test]
    fn days_and_mondays_land_on_the_hour() {
        // Thursday 15 October 2026, 14:20.
        let now = at(15, 14, 20);
        assert_eq!(day_at_in(&cest(), now, 1, MORNING), at(16, 8, 0));
        assert_eq!(day_at_in(&cest(), now, 0, 18), at(15, 18, 0));
        assert_eq!(next_monday_in(&cest(), now, MORNING), at(19, 8, 0));
        // A Monday's "Monday" is next week's.
        assert_eq!(next_monday_in(&cest(), at(19, 7, 0), MORNING), at(26, 8, 0));
    }

    #[test]
    fn later_today_is_three_hours_on_the_hour_until_the_evening() {
        assert_eq!(later_today_in(&cest(), at(15, 14, 20)), Some(at(15, 18, 0)));
        assert_eq!(later_today_in(&cest(), at(15, 14, 0)), Some(at(15, 17, 0)));
        assert_eq!(later_today_in(&cest(), at(15, 18, 0)), Some(at(15, 21, 0)));
        assert_eq!(later_today_in(&cest(), at(15, 18, 1)), None);
        assert_eq!(later_today_in(&cest(), at(15, 23, 30)), None);
    }

    #[test]
    fn presets_come_soonest_first_without_repeats() {
        let kinds = |now| snooze_times(&cest(), now).into_iter().map(|(k, _)| k).collect::<Vec<_>>();
        use PresetKind::*;
        // A Thursday afternoon: all four.
        assert_eq!(kinds(at(15, 14, 20)), [LaterToday, TomorrowMorning, TomorrowAfternoon, MondayMorning]);
        // A Thursday night: no "later today".
        assert_eq!(kinds(at(15, 22, 0)), [TomorrowMorning, TomorrowAfternoon, MondayMorning]);
        // A Sunday: Monday morning is tomorrow morning, offered once.
        assert_eq!(kinds(at(18, 10, 0)), [LaterToday, TomorrowMorning, TomorrowAfternoon]);
        let times = snooze_times(&cest(), at(15, 14, 20));
        assert!(times.windows(2).all(|w| w[0].1 < w[1].1));
    }
}
