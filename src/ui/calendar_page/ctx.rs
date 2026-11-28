//! What every calendar view reads (the calendars' colors, the selection,
//! the week's shape) and what each sends back when it is clicked, dragged
//! or right-clicked.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use chrono::{Datelike, Duration, NaiveDate, Weekday};
use gtk::{gdk, glib};

use super::paint::Rgba;
use crate::calendar::model::{Occ, OccKey, Status};

/// Where a popover points: a widget and a box on it.
#[derive(Debug, Clone)]
pub(crate) struct Anchor {
    pub(crate) widget: gtk::Widget,
    pub(crate) rect: gdk::Rectangle,
}

impl Anchor {
    pub(crate) fn new(widget: &impl glib::object::IsA<gtk::Widget>, x: f64, y: f64, w: f64, h: f64) -> Anchor {
        use gtk::prelude::*;
        Anchor {
            widget: widget.clone().upcast(),
            rect: gdk::Rectangle::new(x as i32, y as i32, (w as i32).max(1), (h as i32).max(1)),
        }
    }
}

/// What a view asks the page to do.
#[derive(Debug, Clone)]
pub(crate) enum Act {
    /// Select an occurrence (none: clear), showing its details at `anchor`.
    Select(Option<OccKey>, Option<Anchor>),
    /// Open an occurrence in the editor.
    Open(OccKey),
    /// A new event over a span, from a drag or a double click.
    Create {
        start: i64,
        end: i64,
        all_day: bool,
        anchor: Anchor,
    },
    /// An occurrence dragged to a new start and end.
    Move { key: OccKey, start: i64, end: i64 },
    /// Show a day, in another view.
    Show(NaiveDate, Option<crate::calendar::prefs::View>),
    /// List one day's events (the month view's "N more").
    DayList(NaiveDate, Anchor),
    /// The right-click menu, on an occurrence or on a moment.
    Menu {
        key: Option<OccKey>,
        at: Option<i64>,
        all_day: bool,
        anchor: Anchor,
    },
}

pub(crate) struct Ctx {
    /// Each calendar's color, by UID.
    pub(crate) colors: HashMap<String, Rgba>,
    /// The calendars that take changes.
    pub(crate) writable: HashSet<String>,
    pub(crate) selected: Option<OccKey>,
    pub(crate) week_start: Weekday,
    pub(crate) week_numbers: bool,
    /// The working day, in hours.
    pub(crate) work: (u32, u32),
    /// The user's own addresses, for their answer to a meeting.
    pub(crate) me: Vec<String>,
    pub(crate) act: Rc<dyn Fn(Act)>,
}

pub(crate) type Shared = Rc<RefCell<Ctx>>;

impl Ctx {
    /// The color an occurrence is drawn in: its own (RFC 7986), else its
    /// calendar's.
    pub(crate) fn color(&self, occ: &Occ) -> Rgba {
        if let Some(c) = &occ.comp.color {
            if let Ok(rgba) = gdk::RGBA::parse(c) {
                return Rgba::from(rgba);
            }
        }
        self.colors
            .get(&occ.key.calendar)
            .copied()
            .unwrap_or(Rgba::new(0.21, 0.52, 0.89, 1.0))
    }

    pub(crate) fn is_selected(&self, occ: &Occ) -> bool {
        self.selected.as_ref() == Some(&occ.key)
    }

    pub(crate) fn can_change(&self, occ: &Occ) -> bool {
        self.writable.contains(&occ.key.calendar)
    }

    /// How the user stands on a meeting: drawn hatched while unanswered or
    /// tentative, faded once declined.
    pub(crate) fn standing(&self, occ: &Occ) -> Standing {
        let c = &occ.comp;
        if c.status == Status::Cancelled {
            return Standing::Cancelled;
        }
        match c.my_status(&self.me) {
            Some("DECLINED") => Standing::Declined,
            Some("TENTATIVE") => Standing::Tentative,
            Some("NEEDS-ACTION") if c.invited(&self.me) => Standing::Unanswered,
            _ if c.status == Status::Tentative => Standing::Tentative,
            _ => Standing::Going,
        }
    }

    pub(crate) fn emit(&self, act: Act) {
        (self.act)(act);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Standing {
    Going,
    Tentative,
    Unanswered,
    Declined,
    Cancelled,
}

impl Standing {
    pub(crate) fn hatched(self) -> bool {
        matches!(self, Standing::Tentative | Standing::Unanswered)
    }
    pub(crate) fn faded(self) -> bool {
        matches!(self, Standing::Declined | Standing::Cancelled)
    }
    pub(crate) fn struck(self) -> bool {
        self == Standing::Cancelled
    }
}

/// The first day of the week holding `day`.
pub(crate) fn week_of(day: NaiveDate, start: Weekday) -> NaiveDate {
    let back = (7 + day.weekday().num_days_from_monday() - start.num_days_from_monday()) % 7;
    day - Duration::days(i64::from(back))
}

/// The first of `day`'s month.
pub(crate) fn month_of(day: NaiveDate) -> NaiveDate {
    day.with_day(1).unwrap_or(day)
}

/// `months` months on from `day`, the day clamped to the month's length.
pub(crate) fn add_months(day: NaiveDate, months: i32) -> NaiveDate {
    let total = day.year() * 12 + day.month0() as i32 + months;
    let (y, m) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    let last = crate::calendar::time::days_in_month(y, m);
    NaiveDate::from_ymd_opt(y, m, day.day().min(last)).unwrap_or(day)
}

/// The ISO week number.
pub(crate) fn week_number(day: NaiveDate) -> u32 {
    day.iso_week().week()
}

/// A date written with a GLib pattern, in the user's language.
pub(crate) fn fmt(day: NaiveDate, pattern: &str) -> String {
    glib::DateTime::from_local(day.year(), day.month() as i32, day.day() as i32, 12, 0, 0.0)
        .ok()
        .and_then(|d| d.format(pattern).ok())
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

/// "Mon".
pub(crate) fn weekday_short(day: NaiveDate) -> String {
    fmt(day, "%a")
}

/// "November 2026".
pub(crate) fn month_year(day: NaiveDate) -> String {
    fmt(day, "%B %Y")
}

/// "November".
pub(crate) fn month_name(day: NaiveDate) -> String {
    fmt(day, "%B")
}

/// "Friday, 27 November" in the order the locale writes it.
pub(crate) fn long_day(day: NaiveDate) -> String {
    let date = crate::datefmt::day_month(noon(day));
    format!("{}, {date}", fmt(day, "%A"))
}

/// Midday of `day`, local: an instant that is on that day in any zone
/// change.
pub(crate) fn noon(day: NaiveDate) -> i64 {
    crate::calendar::time::local_unix(day.and_hms_opt(12, 0, 0).unwrap_or_default())
}

/// An hour as the time ruler writes it: "9 AM" or "09:00".
pub(crate) fn hour_label(hour: u32) -> String {
    let label = crate::datefmt::clock_label(hour % 24);
    // "9:00 AM" reads as "9 AM" in a ruler; a 24-hour clock keeps "09:00".
    match label.split_once(' ') {
        Some((time, mark)) if !mark.is_empty() => {
            format!("{} {mark}", time.trim_end_matches(":00"))
        }
        _ => {
            if label.len() == 4 {
                format!("0{label}")
            } else {
                label
            }
        }
    }
}

/// The days of a span, first to last.
pub(crate) fn days(first: NaiveDate, count: i64) -> Vec<NaiveDate> {
    (0..count).map(|i| first + Duration::days(i)).collect()
}

/// The calendars a user has said to hide, as a set.
pub(crate) fn hidden_set(list: &[String]) -> HashSet<String> {
    list.iter().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn weeks_start_where_asked() {
        // Friday 27 November 2026.
        let fri = d(2026, 11, 27);
        assert_eq!(week_of(fri, Weekday::Mon), d(2026, 11, 23));
        assert_eq!(week_of(fri, Weekday::Sun), d(2026, 11, 22));
        assert_eq!(week_of(fri, Weekday::Sat), d(2026, 11, 21));
        assert_eq!(week_of(d(2026, 11, 22), Weekday::Sun), d(2026, 11, 22));
    }

    #[test]
    fn months_add_and_clamp() {
        assert_eq!(add_months(d(2026, 1, 31), 1), d(2026, 2, 28));
        assert_eq!(add_months(d(2026, 11, 15), 2), d(2027, 1, 15));
        assert_eq!(add_months(d(2026, 1, 15), -1), d(2025, 12, 15));
    }
}
