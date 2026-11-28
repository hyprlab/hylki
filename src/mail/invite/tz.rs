//! Time zones a document defines itself (`VTIMEZONE`), for when the name it
//! uses means nothing to the system: each observance's yearly rule and the
//! offset in force at a moment.

use super::ics::Line;
use super::Stamp;

/// A zone the document defines itself, for when its name means nothing to
/// the system.
#[derive(Debug, Default)]
pub(crate) struct TimeZoneSpec {
    pub(crate) id: String,
    pub(crate) observances: Vec<Observance>,
}

/// One `STANDARD` or `DAYLIGHT` block: the offset it puts the clock on, and
/// (through its yearly rule) the day it starts.
#[derive(Debug, Default)]
pub(crate) struct Observance {
    /// Seconds east of UTC once this is in force.
    pub(crate) offset_to: i32,
    /// Month it begins, from the rule (`BYMONTH`) or its own `DTSTART`.
    month: i32,
    /// Day of the month it begins, when the rule gives one outright.
    day: i32,
    /// `BYDAY=-1SU`: which weekday, and which one of the month
    /// (1 = first, -1 = last).
    weekday: i32,
    ordinal: i32,
    /// Wall time it changes at, in seconds past midnight.
    at: i32,
}

impl Observance {
    pub(crate) fn take(&mut self, line: &Line) {
        match line.name.to_ascii_uppercase().as_str() {
            "TZOFFSETTO" => self.offset_to = utc_offset(line.raw.trim()).unwrap_or_default(),
            "DTSTART" => {
                if let Some(s) = Stamp::parse(line.raw.trim()) {
                    self.month = s.mo;
                    self.day = s.d;
                    self.at = s.h * 3600 + s.mi * 60 + s.s;
                }
            }
            "RRULE" => {
                for part in line.raw.split(';') {
                    let Some((k, v)) = part.split_once('=') else {
                        continue;
                    };
                    match k.to_ascii_uppercase().as_str() {
                        "BYMONTH" => self.month = v.trim().parse().unwrap_or(self.month),
                        "BYMONTHDAY" => self.day = v.trim().parse().unwrap_or(self.day),
                        "BYDAY" => {
                            let v = v.trim().to_ascii_uppercase();
                            let (num, day) = v.split_at(v.len().saturating_sub(2));
                            self.ordinal = num.parse().unwrap_or(1);
                            self.weekday = match day {
                                "SU" => 7,
                                "MO" => 1,
                                "TU" => 2,
                                "WE" => 3,
                                "TH" => 4,
                                "FR" => 5,
                                "SA" => 6,
                                _ => 0,
                            };
                            // A BYDAY rule names a weekday, not a date.
                            self.day = 0;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// The day of the month this observance begins in a given year.
    fn day_in(&self, year: i32) -> Option<i32> {
        if self.day > 0 {
            return Some(self.day);
        }
        if self.weekday == 0 {
            return None;
        }
        let last = days_in_month(year, self.month);
        if self.ordinal < 0 {
            // Counting back: the last (or second-to-last) such weekday.
            let mut found = 0;
            let mut day = last;
            while day >= 1 {
                if weekday_of(year, self.month, day) == self.weekday {
                    found -= 1;
                    if found == self.ordinal {
                        return Some(day);
                    }
                }
                day -= 1;
            }
            return None;
        }
        let mut found = 0;
        for day in 1..=last {
            if weekday_of(year, self.month, day) == self.weekday {
                found += 1;
                if found == self.ordinal.max(1) {
                    return Some(day);
                }
            }
        }
        None
    }
}

impl TimeZoneSpec {
    /// Which of this zone's offsets is in force at a wall-clock reading.
    ///
    /// The comparison is done in the zone's own wall time, which is what the
    /// event is written in: the changeover sits at a wall time too, so no
    /// conversion is needed to place the reading either side of it. A zone
    /// with one observance (no daylight saving) answers with that one.
    pub(crate) fn offset_at(&self, when: &Stamp) -> Option<i32> {
        if self.observances.len() == 1 {
            return Some(self.observances[0].offset_to);
        }
        let key =
            |mo: i32, d: i32, at: i32| (mo as i64) * 100_000_000 + (d as i64) * 100_000 + at as i64;
        let reading = key(when.mo, when.d, when.h * 3600 + when.mi * 60 + when.s);
        // Sort the year's changeovers and take the last one already past.
        // Before the first, the one in force is the year's last — the
        // northern hemisphere changes in spring and autumn, the southern in
        // the other order, and wrapping round the year handles both.
        let mut changes: Vec<(i64, i32)> = self
            .observances
            .iter()
            .filter_map(|o| Some((key(o.month, o.day_in(when.y)?, o.at), o.offset_to)))
            .collect();
        if changes.is_empty() {
            return self.observances.first().map(|o| o.offset_to);
        }
        changes.sort_by_key(|(k, _)| *k);
        let in_force = changes
            .iter()
            .rev()
            .find(|(k, _)| *k <= reading)
            .or_else(|| changes.last())?;
        Some(in_force.1)
    }
}

/// `+0100`, `-043000`: seconds east of UTC.
fn utc_offset(text: &str) -> Option<i32> {
    let (sign, rest) = match text.chars().next()? {
        '-' => (-1, &text[1..]),
        '+' => (1, &text[1..]),
        _ => (1, text),
    };
    let digits: String = rest.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 4 {
        return None;
    }
    let h: i32 = digits.get(0..2)?.parse().ok()?;
    let m: i32 = digits.get(2..4)?.parse().ok()?;
    let s: i32 = digits.get(4..6).and_then(|v| v.parse().ok()).unwrap_or(0);
    Some(sign * (h * 3600 + m * 60 + s))
}

fn days_in_month(year: i32, month: i32) -> i32 {
    use chrono::Datelike;
    let (y, m) = if month >= 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    chrono::NaiveDate::from_ymd_opt(y, m as u32, 1)
        .and_then(|first| first.pred_opt())
        .map_or(30, |last| last.day() as i32)
}

/// ISO weekday, Monday 1 to Sunday 7.
fn weekday_of(year: i32, month: i32, day: i32) -> i32 {
    use chrono::Datelike;
    chrono::NaiveDate::from_ymd_opt(year, month as u32, day as u32)
        .map_or(0, |d| d.weekday().number_from_monday() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_sunday_of_a_month_is_found() {
        // 25 October 2026 is the last Sunday of that month.
        let o = Observance {
            month: 10,
            ordinal: -1,
            weekday: 7,
            ..Observance::default()
        };
        assert_eq!(o.day_in(2026), Some(25));
        // And the second Sunday of March 2026 is the 8th.
        let o = Observance {
            month: 3,
            ordinal: 2,
            weekday: 7,
            ..Observance::default()
        };
        assert_eq!(o.day_in(2026), Some(8));
    }
}
