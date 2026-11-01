//! The iCalendar text of a new event or task.

use chrono::{Duration, NaiveDate};
use gtk::glib;

use super::{Item, ItemKind, When};
use crate::encoding::escape_text_value as escape;
use crate::mail::invite::fold;

/// A UID no other calendar item has.
pub(crate) fn new_uid() -> String {
    let token = crate::rng::token(32).unwrap_or_else(|_| crate::datefmt::now().to_string());
    format!("{token}@hylki")
}

/// The bare `VEVENT` or `VTODO`, as EDS takes a new item.
pub(super) fn component(item: &Item, uid: &str, now: i64) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&fold(&text));
        out.push_str("\r\n");
    };
    let name = match item.kind {
        ItemKind::Event => "VEVENT",
        ItemKind::Task => "VTODO",
    };
    line(format!("BEGIN:{name}"));
    line(format!("UID:{uid}"));
    line(format!("DTSTAMP:{}", stamp_utc(now)));
    line(format!("CREATED:{}", stamp_utc(now)));
    line(format!("SUMMARY:{}", escape(item.summary.trim())));
    // UTC rather than a TZID: the calendar then needs no VTIMEZONE to place
    // it, and shows it at the local time it was picked at.
    match item.when {
        When::Timed { start, end } => {
            line(format!("DTSTART:{}", stamp_utc(start)));
            line(format!("DTEND:{}", stamp_utc(end.max(start))));
        }
        When::AllDay { first, last } => {
            // DTEND is the day after the last, which is not part of it.
            let after = last.max(first) + Duration::days(1);
            line(format!("DTSTART;VALUE=DATE:{}", day(first)));
            line(format!("DTEND;VALUE=DATE:{}", day(after)));
        }
        When::Due(due) => {
            if let Some(due) = due {
                line(format!("DUE;VALUE=DATE:{}", day(due)));
            }
        }
    }
    if item.kind == ItemKind::Task {
        line("STATUS:NEEDS-ACTION".into());
    }
    let notes = item.notes.trim();
    if !notes.is_empty() {
        line(format!("DESCRIPTION:{}", escape(notes)));
    }
    for &minutes in &item.alarms {
        line("BEGIN:VALARM".into());
        line("ACTION:DISPLAY".into());
        line(format!("TRIGGER;RELATED=START:{}", trigger(minutes)));
        // DISPLAY needs a text: the event's title is what the alarm shows.
        line(format!("DESCRIPTION:{}", escape(item.summary.trim())));
        line("END:VALARM".into());
    }
    line(format!("END:{name}"));
    out
}

/// A whole `.ics` file holding the item, for the calendar application.
pub(crate) fn calendar_file(item: &Item, uid: &str, now: i64) -> String {
    let mut out = String::from(
        "BEGIN:VCALENDAR\r\nPRODID:-//Hyprlab//Hylki//EN\r\nVERSION:2.0\r\nMETHOD:PUBLISH\r\n",
    );
    out.push_str(&component(item, uid, now));
    out.push_str("END:VCALENDAR\r\n");
    out
}

/// A unix time as iCalendar UTC: `20261102T080000Z`.
fn stamp_utc(ts: i64) -> String {
    glib::DateTime::from_unix_utc(ts)
        .ok()
        .and_then(|d| d.format("%Y%m%dT%H%M%SZ").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Minutes before the start as an iCalendar DURATION: `-PT15M`, `-P1D`,
/// `PT0S` at the start.
fn trigger(minutes: u32) -> String {
    const DAY: u32 = 24 * 60;
    match minutes {
        0 => "PT0S".into(),
        m if m % (7 * DAY) == 0 => format!("-P{}W", m / (7 * DAY)),
        m if m % DAY == 0 => format!("-P{}D", m / DAY),
        m if m % 60 == 0 => format!("-PT{}H", m / 60),
        m => format!("-PT{m}M"),
    }
}

/// A day as an iCalendar DATE: `20261102`.
fn day(date: NaiveDate) -> String {
    date.format("%Y%m%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// 2026-11-02 08:00 UTC.
    const MONDAY_8: i64 = 1_793_606_400;

    #[test]
    fn an_event_is_a_vevent_in_utc() {
        let item = Item {
            kind: ItemKind::Event,
            summary: "Lunch, then; the review".into(),
            notes: "From Ada\nBring the slides".into(),
            when: When::Timed {
                start: MONDAY_8,
                end: MONDAY_8 + 3600,
            },
            alarms: Vec::new(),
        };
        let ics = component(&item, "u1@hylki", MONDAY_8 - 60);
        assert!(ics.starts_with("BEGIN:VEVENT\r\nUID:u1@hylki\r\n"), "{ics}");
        assert!(ics.contains("DTSTAMP:20261102T075900Z\r\n"));
        assert!(ics.contains("SUMMARY:Lunch\\, then\\; the review\r\n"));
        assert!(ics.contains("DTSTART:20261102T080000Z\r\nDTEND:20261102T090000Z\r\n"));
        assert!(ics.contains("DESCRIPTION:From Ada\\nBring the slides\r\n"));
        assert!(ics.ends_with("END:VEVENT\r\n"));
        assert!(!ics.contains("STATUS:"));
    }

    #[test]
    fn an_all_day_event_ends_the_day_after_its_last() {
        let item = Item {
            kind: ItemKind::Event,
            summary: "Trip".into(),
            notes: String::new(),
            when: When::AllDay {
                first: date(2026, 12, 30),
                last: date(2026, 12, 31),
            },
            alarms: Vec::new(),
        };
        let ics = component(&item, "u2@hylki", MONDAY_8);
        assert!(ics.contains("DTSTART;VALUE=DATE:20261230\r\n"));
        assert!(ics.contains("DTEND;VALUE=DATE:20270101\r\n"));
        assert!(!ics.contains("DESCRIPTION"));
    }

    #[test]
    fn a_task_is_a_vtodo_with_or_without_a_due_day() {
        let mut item = Item {
            kind: ItemKind::Task,
            summary: "Pay the invoice".into(),
            notes: String::new(),
            when: When::Due(Some(date(2026, 11, 5))),
            alarms: Vec::new(),
        };
        let ics = component(&item, "u3@hylki", MONDAY_8);
        assert!(ics.starts_with("BEGIN:VTODO\r\n"));
        assert!(ics.contains("DUE;VALUE=DATE:20261105\r\n"));
        assert!(ics.contains("STATUS:NEEDS-ACTION\r\n"));
        assert!(ics.ends_with("END:VTODO\r\n"));
        item.when = When::Due(None);
        assert!(!component(&item, "u3@hylki", MONDAY_8).contains("DUE"));
    }

    #[test]
    fn long_notes_are_folded_and_the_file_wraps_the_item() {
        let item = Item {
            kind: ItemKind::Event,
            summary: "Notes".into(),
            notes: "word ".repeat(40),
            when: When::Timed {
                start: MONDAY_8,
                end: MONDAY_8,
            },
            alarms: Vec::new(),
        };
        let file = calendar_file(&item, "u4@hylki", MONDAY_8);
        assert!(file.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(file.contains("METHOD:PUBLISH\r\nBEGIN:VEVENT\r\n"));
        assert!(file.ends_with("END:VEVENT\r\nEND:VCALENDAR\r\n"));
        assert!(file.split("\r\n").all(|l| l.len() <= 75), "{file}");
        assert!(file.contains("\r\n "), "a continuation line: {file}");
    }

    #[test]
    fn an_alarm_goes_off_before_the_start() {
        let mut item = Item {
            kind: ItemKind::Event,
            summary: "Dentist".into(),
            notes: String::new(),
            when: When::Timed {
                start: MONDAY_8,
                end: MONDAY_8 + 1800,
            },
            alarms: vec![15],
        };
        let ics = component(&item, "u5@hylki", MONDAY_8);
        assert!(
            ics.contains(
                "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;RELATED=START:-PT15M\r\n\
                 DESCRIPTION:Dentist\r\nEND:VALARM\r\nEND:VEVENT\r\n"
            ),
            "{ics}"
        );
        for (minutes, trigger) in [(0, "PT0S"), (60, "-PT1H"), (1440, "-P1D"), (10080, "-P1W")] {
            item.alarms = vec![minutes];
            let ics = component(&item, "u5@hylki", MONDAY_8);
            assert!(ics.contains(&format!("START:{trigger}\r\n")), "{ics}");
        }
        item.alarms = vec![15, 24 * 60];
        let ics = component(&item, "u5@hylki", MONDAY_8);
        assert_eq!(ics.matches("BEGIN:VALARM").count(), 2, "{ics}");
        assert!(ics.contains("START:-PT15M\r\n") && ics.contains("START:-P1D\r\n"));
        item.alarms.clear();
        assert!(!component(&item, "u5@hylki", MONDAY_8).contains("VALARM"));
    }

    #[test]
    fn uids_differ() {
        assert_ne!(new_uid(), new_uid());
        assert!(new_uid().ends_with("@hylki"));
    }
}
