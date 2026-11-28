//! The calendar's settings and its remembered layout, in
//! `~/.config/hylki/calendar.toml`.

use chrono::Weekday;
use serde::{Deserialize, Serialize};

/// Which view the calendar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum View {
    Day,
    #[default]
    Week,
    Month,
    Year,
    Agenda,
}

/// The first day of the week.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum WeekStart {
    /// What the system's locale says.
    #[default]
    System,
    Sunday,
    Monday,
    Saturday,
}

impl WeekStart {
    pub(crate) fn weekday(self) -> Weekday {
        match self {
            WeekStart::Sunday => Weekday::Sun,
            WeekStart::Monday => Weekday::Mon,
            WeekStart::Saturday => Weekday::Sat,
            WeekStart::System => system_week_start(),
        }
    }
}

/// When Hylki shows an event's alarms itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Reminders {
    /// Only when the desktop's own alarm service isn't running.
    #[default]
    Automatic,
    Always,
    Never,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct CalendarPrefs {
    pub(crate) view: View,
    pub(crate) week_start: WeekStart,
    pub(crate) week_numbers: bool,
    /// The working day, in hours: shaded outside, and where a day or week
    /// view opens scrolled to.
    pub(crate) work_start: u32,
    pub(crate) work_end: u32,
    /// Whether Week shows the five working days only.
    pub(crate) work_week: bool,
    /// A new event's length, in minutes.
    pub(crate) default_minutes: u32,
    /// A new event's alarm, in minutes before; `None` for no alarm.
    pub(crate) default_alarm: Option<u32>,
    pub(crate) show_declined: bool,
    pub(crate) reminders: Reminders,
    /// Where a new event goes, by calendar UID.
    pub(crate) default_calendar: String,
    /// Calendars hidden from the views, by UID.
    pub(crate) hidden: Vec<String>,
    /// The height of an hour in the day and week views, in pixels.
    pub(crate) hour_height: f64,
}

impl Default for CalendarPrefs {
    fn default() -> Self {
        CalendarPrefs {
            view: View::Week,
            week_start: WeekStart::System,
            week_numbers: false,
            work_start: 8,
            work_end: 18,
            work_week: false,
            default_minutes: 60,
            default_alarm: Some(15),
            show_declined: true,
            reminders: Reminders::Automatic,
            default_calendar: String::new(),
            hidden: Vec::new(),
            hour_height: 48.0,
        }
    }
}

fn path() -> Option<std::path::PathBuf> {
    crate::config::config_file("calendar.toml")
}

impl CalendarPrefs {
    pub(crate) fn load() -> CalendarPrefs {
        let mut p: CalendarPrefs = crate::config::read_toml(path()).unwrap_or_default();
        p.work_start = p.work_start.min(23);
        p.work_end = p.work_end.clamp(p.work_start + 1, 24);
        p.default_minutes = p.default_minutes.clamp(5, 24 * 60);
        p.hour_height = p.hour_height.clamp(24.0, 160.0);
        p
    }

    pub(crate) fn save(&self) {
        crate::config::save_private_toml(path(), self, "calendar.toml", true);
    }
}

/// The locale's first day of the week, as GTK's own calendar reads it.
pub(crate) fn system_week_start() -> Weekday {
    static FIRST: std::sync::OnceLock<Weekday> = std::sync::OnceLock::new();
    *FIRST.get_or_init(|| {
        // glibc's _NL_TIME_FIRST_WEEKDAY: 1 is Sunday. Its week begins on
        // _NL_TIME_WEEK_1STDAY, which every glibc locale sets to a Sunday.
        const NL_TIME_FIRST_WEEKDAY: libc::nl_item = 0x20068;
        // SAFETY: nl_langinfo returns a pointer into the locale's static
        // data, valid until the locale changes; only its first byte is read.
        let first = unsafe {
            let p = libc::nl_langinfo(NL_TIME_FIRST_WEEKDAY);
            if p.is_null() {
                2
            } else {
                *p as u8
            }
        };
        match first {
            1 => Weekday::Sun,
            7 => Weekday::Sat,
            _ => Weekday::Mon,
        }
    })
}
