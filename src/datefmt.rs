//! Dates and times in the format the system asks for (#32).
//!
//! Hylki's UI is English, but a date is not English: someone whose desktop is set
//! to German expects 23.08.2026 and 14:03, not 08/23/2026 and 2:03 PM. Chrono
//! formats dates by pattern only — it has no notion of a locale — so the patterns
//! it was given were hard-coded American, whatever the machine was set to.
//!
//! GLib does know: `g_date_time_format` reads `LC_TIME`, so `%x` is the locale's
//! own date, `%b` its month names, and `%p` empty wherever the clock is 24-hour.
//! Those three are all it takes to follow the system while keeping Hylki's own
//! layout — the list still shows "23. Aug, 14:03" where it showed "Aug 23, 2:03
//! PM", rather than surrendering the arrangement to `%c`.
//!
//! The locale is read once. It cannot change under a running process without the
//! environment changing, which does not happen while the app is up.

use crate::config::{ClockStyle, DateStyle};
use gtk::glib;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

/// The user's choice, when they have made one: dates and the clock can each be
/// pinned regardless of the locale (#32). Held as plain atomics so formatting
/// stays a free function that any thread can call.
static DATE_STYLE: AtomicU8 = AtomicU8::new(0);
static CLOCK_STYLE: AtomicU8 = AtomicU8::new(0);

/// Apply the user's preference. Called at startup and whenever it changes.
pub fn set_style(date: DateStyle, clock: ClockStyle) {
    DATE_STYLE.store(
        match date {
            DateStyle::System => 0,
            DateStyle::MonthFirst => 1,
            DateStyle::DayFirst => 2,
            DateStyle::YearFirst => 3,
        },
        Ordering::Relaxed,
    );
    CLOCK_STYLE.store(
        match clock {
            ClockStyle::System => 0,
            ClockStyle::Twelve => 1,
            ClockStyle::TwentyFour => 2,
        },
        Ordering::Relaxed,
    );
}

/// A local `glib::DateTime`, or `None` for a timestamp outside its range.
fn local(ts: i64) -> Option<glib::DateTime> {
    glib::DateTime::from_unix_local(ts).ok()
}

fn fmt(dt: &glib::DateTime, pattern: &str) -> String {
    let text = dt.format(pattern).map(|s| s.to_string()).unwrap_or_default();
    // Some locales pad a field ("%b" is " 8月" in Japanese), which doubles up
    // against the space this pattern already puts there.
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether the day is written before the month (23.08. rather than 08/23).
fn day_first() -> bool {
    match DATE_STYLE.load(Ordering::Relaxed) {
        1 | 3 => return false,
        2 => return true,
        _ => {}
    }
    system_day_first()
}

fn system_day_first() -> bool {
    static DAY_FIRST: OnceLock<bool> = OnceLock::new();
    *DAY_FIRST.get_or_init(|| {
        // A date whose day and month can't be confused for one another: whichever
        // number the locale prints first is the field it puts first.
        day_first_from_probe(&order_probe())
    })
}

fn day_first_from_probe(probe: &str) -> bool {
    match (probe.find("25"), probe.find("12")) {
        (Some(day), Some(month)) => day < month,
        // No recognisable date: keep the arrangement Hylki has always used.
        _ => false,
    }
}

/// Whether the year is written first, as Japanese and Chinese locales do.
fn year_first() -> bool {
    match DATE_STYLE.load(Ordering::Relaxed) {
        1 | 2 => return false,
        3 => return true,
        _ => {}
    }
    system_year_first()
}

fn system_year_first() -> bool {
    static YEAR_FIRST: OnceLock<bool> = OnceLock::new();
    *YEAR_FIRST.get_or_init(|| year_first_from_probe(&order_probe()))
}

fn year_first_from_probe(probe: &str) -> bool {
    match (probe.find("2026"), probe.find("25"), probe.find("12")) {
        (Some(year), Some(day), Some(month)) => year < day && year < month,
        _ => false,
    }
}

/// What a day-first locale writes after the day: German has "23. Aug", British
/// and French "23 Aug". The locale's own date says which — 23.08.2026 against
/// 23/08/2026.
fn day_suffix() -> &'static str {
    static SUFFIX: OnceLock<bool> = OnceLock::new();
    if *SUFFIX.get_or_init(|| order_probe().contains("25.")) {
        "."
    } else {
        ""
    }
}

/// The locale's written form of 25 December 2026, from which its field order and
/// separators are read.
fn order_probe() -> String {
    static PROBE: OnceLock<String> = OnceLock::new();
    PROBE
        .get_or_init(|| {
            glib::DateTime::from_utc(2026, 12, 25, 12, 0, 0.0)
                .ok()
                .map(|d| fmt(&d, "%x"))
                .unwrap_or_default()
        })
        .clone()
}

/// Whether the clock runs on a 12-hour dial.
fn ampm() -> bool {
    match CLOCK_STYLE.load(Ordering::Relaxed) {
        1 => return true,
        2 => return false,
        _ => {}
    }
    system_ampm()
}

/// The system's clock: the desktop's own setting first (GNOME's Time
/// Format, which the locale knows nothing about, #173), then what the
/// locale writes. Re-read now and then so a change in Settings shows up
/// without a restart.
fn system_ampm() -> bool {
    static CACHE: Mutex<Option<(std::time::Instant, bool)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, v)) = *cache {
        if at.elapsed() < std::time::Duration::from_secs(30) {
            return v;
        }
    }
    let v = desktop_ampm().unwrap_or_else(locale_ampm);
    *cache = Some((std::time::Instant::now(), v));
    v
}

/// The desktop's clock format (GNOME's Time Format): `None` where the
/// desktop has no such setting.
fn desktop_ampm() -> Option<bool> {
    match crate::desktop::setting("org.gnome.desktop.interface", "clock-format")?.as_str() {
        "12h" => Some(true),
        "24h" => Some(false),
        _ => None,
    }
}

fn locale_ampm() -> bool {
    static AMPM: OnceLock<bool> = OnceLock::new();
    *AMPM.get_or_init(|| {
        // Ask for one o'clock in the afternoon in the locale's own time format.
        // Not `%p`: nearly every locale defines AM/PM strings whether or not it
        // uses them, so a non-empty `%p` said "12-hour" for British and German
        // clocks alike. What the locale writes for 13:00 does not.
        let probe = glib::DateTime::from_utc(2026, 12, 25, 13, 0, 0.0)
            .ok()
            .map(|d| fmt(&d, "%X"))
            .unwrap_or_default();
        ampm_from_probe(&probe)
    })
}

/// A whole hour on the clock in use: "8:00" or "8:00 AM", for labels
/// that name a time of day (Send Later's presets).
pub fn clock_label(hour: u32) -> String {
    let Ok(dt) = glib::DateTime::from_local(2026, 1, 1, hour as i32, 0, 0.0) else {
        return format!("{hour}:00");
    };
    if ampm() {
        fmt(&dt, "%-I:%M %p")
    } else {
        fmt(&dt, "%H:%M")
    }
}

fn ampm_from_probe(probe: &str) -> bool {
    // A 24-hour clock writes the hour as 13; a 12-hour one writes 1 and a marker.
    !probe.contains("13") && !probe.is_empty()
}

/// The time of day, without seconds: "2:03 PM" or "14:03", as the locale has it.
pub fn time(ts: i64) -> String {
    let Some(dt) = local(ts) else {
        return String::new();
    };
    if ampm() {
        fmt(&dt, "%-I:%M %p")
    } else {
        fmt(&dt, "%H:%M")
    }
}

/// Day and month, no year: "Aug 23" or "23. Aug".
pub fn day_month(ts: i64) -> String {
    let Some(dt) = local(ts) else {
        return String::new();
    };
    let text = if day_first() {
        fmt(&dt, &format!("%-d{} %b", day_suffix()))
    } else {
        fmt(&dt, "%b %-d")
    };
    text.trim().to_string()
}

/// Day, month and year: "Aug 23, 2026" or "23. Aug 2026".
///
/// Hylki's own arrangement, with the locale's field order and month names rather
/// than the locale's written date (`%x`, which is all digits). A date is read at
/// a glance in a list of mail, and "Aug 23" is quicker to place than "08/23" —
/// the point of #32 was that the *order* and the *clock* were American, not that
/// the month should stop being spelled.
pub fn day_month_year(ts: i64) -> String {
    let Some(dt) = local(ts) else {
        return String::new();
    };
    let text = if year_first() {
        fmt(&dt, "%Y %b %-d")
    } else if day_first() {
        fmt(&dt, &format!("%-d{} %b %Y", day_suffix()))
    } else {
        fmt(&dt, "%b %-d, %Y")
    };
    text.trim().to_string()
}

/// Date and time together, for the reader's header and printed pages.
pub fn date_time(ts: i64) -> String {
    if local(ts).is_none() {
        return String::new();
    }
    format!("{} at {}", day_month_year(ts), time(ts))
}

/// The day an event falls on, with its weekday: "Monday, 2 Nov 2026".
/// An appointment is placed by its weekday more than by its date, which is
/// why a calendar invitation names it and a message list does not.
pub fn weekday_date(ts: i64) -> String {
    let Some(dt) = local(ts) else {
        return String::new();
    };
    let day = day_month_year(ts);
    match fmt(&dt, "%A") {
        w if w.is_empty() => day,
        w => format!("{w}, {day}"),
    }
}

/// When an event runs: "Monday, 2 Nov 2026 · 09:00 – 10:00", or the two
/// dates when it spans days, or the day alone when it takes all of one.
///
/// `end` at or before `start` (an event written with no end) shows the
/// start alone rather than a range that goes backwards.
pub fn event_span(start: i64, end: i64, all_day: bool) -> String {
    if local(start).is_none() {
        return String::new();
    }
    if all_day {
        // An all-day event ends at midnight on the day *after* the last one
        // it covers, so a single day arrives as a 24-hour range.
        let last = end - 86_400;
        if last > start && day_key(last) != day_key(start) {
            return format!("{} – {}", weekday_date(start), weekday_date(last));
        }
        return weekday_date(start);
    }
    if end <= start {
        return format!("{} · {}", weekday_date(start), time(start));
    }
    if day_key(end) == day_key(start) {
        return format!("{} · {} – {}", weekday_date(start), time(start), time(end));
    }
    format!(
        "{} {} – {} {}",
        weekday_date(start),
        time(start),
        weekday_date(end),
        time(end)
    )
}

/// The year a timestamp falls in, locally.
pub fn year(ts: i64) -> i32 {
    local(ts).map(|dt| dt.year()).unwrap_or_default()
}

/// The local calendar day a timestamp falls on, as (year, day-of-year).
pub fn day_key(ts: i64) -> (i32, i32) {
    local(ts)
        .map(|dt| (dt.year(), dt.day_of_year()))
        .unwrap_or_default()
}

/// Now, as a timestamp.
pub fn now() -> i64 {
    glib::DateTime::now_local().map(|d| d.to_unix()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An event's line says the weekday, and only shows a range when there
    /// is one to show. The wording is the locale's; the shape is not.
    #[test]
    fn an_events_span_reads_as_one_line() {
        // Nine in the morning, local, whatever zone the test runs in: the
        // arithmetic below then stays inside the day it started in.
        let day = glib::DateTime::from_unix_local(1_793_600_000).expect("a date");
        let start = glib::DateTime::new(
            &glib::TimeZone::local(),
            day.year(),
            day.month(),
            day.day_of_month(),
            9,
            0,
            0.0,
        )
        .expect("nine o'clock")
        .to_unix();

        let hour = event_span(start, start + 3600, false);
        assert!(hour.contains('–'), "a range within one day: {hour}");
        assert!(hour.contains('·'), "date and times are separated: {hour}");
        assert_eq!(hour.matches('·').count(), 1, "{hour}");

        // No end at all: the start, and no range that goes backwards.
        let point = event_span(start, 0, false);
        assert!(!point.contains('–'), "{point}");

        // All day: the date alone, even though the event runs to midnight.
        let day = event_span(start, start + 86_400, true);
        assert!(!day.contains('–'), "{day}");
        assert!(!day.contains('·'), "no time of day on an all-day event: {day}");

        // Two days: both dates.
        let two = event_span(start, start + 2 * 86_400, true);
        assert!(two.contains('–'), "{two}");

        // Unreadable: nothing rather than a made-up date.
        assert_eq!(event_span(i64::MAX, i64::MAX, false), "");
    }

    #[test]
    fn the_locales_field_order_is_read_from_its_own_date() {
        // Whatever the separators, whichever number comes first wins.
        assert!(day_first_from_probe("25.12.2026"));
        assert!(day_first_from_probe("25/12/26"));
        assert!(!day_first_from_probe("12/25/2026"));
        assert!(!day_first_from_probe("2026-12-25"));
        // Nothing recognisable: keep Hylki's own arrangement rather than guess.
        assert!(!day_first_from_probe(""));
        assert!(!day_first_from_probe("Freitag"));
    }

    #[test]
    fn a_chosen_format_overrides_the_locale() {
        // The C locale this test runs under writes month-first on a 24-hour clock;
        // each choice has to hold regardless of that.
        let ts = 1787478020; // 2026-08-23, 09:40 UTC
        set_style(DateStyle::DayFirst, ClockStyle::Twelve);
        assert!(day_month_year(ts).starts_with("23"), "{}", day_month_year(ts));
        assert!(time(ts).contains(':') && time(ts).len() > 5, "{}", time(ts));

        set_style(DateStyle::YearFirst, ClockStyle::TwentyFour);
        assert!(day_month_year(ts).starts_with("2026"), "{}", day_month_year(ts));
        assert!(!time(ts).contains("AM") && !time(ts).contains("PM"), "{}", time(ts));

        set_style(DateStyle::MonthFirst, ClockStyle::System);
        assert!(day_month_year(ts).starts_with("Aug"), "{}", day_month_year(ts));

        // Back to following the locale, so the other tests see it as they found it.
        set_style(DateStyle::System, ClockStyle::System);
    }

    #[test]
    fn a_year_first_locale_is_recognised() {
        assert!(year_first_from_probe("2026年12月25日"));
        assert!(year_first_from_probe("2026-12-25"));
        assert!(!year_first_from_probe("25.12.2026"));
        assert!(!year_first_from_probe("12/25/2026"));
        assert!(!year_first_from_probe(""));
    }

    #[test]
    fn the_clock_is_read_from_what_the_locale_writes_for_one_in_the_afternoon() {
        // 24-hour locales, whatever their separators.
        assert!(!ampm_from_probe("13:00:00"));
        assert!(!ampm_from_probe("13.00.00 Uhr"));
        // 12-hour ones.
        assert!(ampm_from_probe("1:00:00 PM"));
        assert!(ampm_from_probe("午後1:00:00"));
        // Nothing to go on: 24-hour, which is what the rest of the world uses.
        assert!(!ampm_from_probe(""));
    }

    #[test]
    fn a_timestamp_out_of_range_formats_to_nothing() {
        assert_eq!(time(i64::MIN), "");
        assert_eq!(day_month(i64::MIN), "");
        assert_eq!(day_month_year(i64::MIN), "");
        assert_eq!(date_time(i64::MIN), "");
    }
}
