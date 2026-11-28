//! Events and tasks as the calendar views use them, read from the
//! components a calendar holds, and a series turned into the occurrences
//! that fall in a span of time.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{Duration, NaiveDate, NaiveDateTime};

use super::recur::{self, Rule};
use super::time::{day_start, local_day, Stamp, Zones};
use super::tree::Node;
use crate::models::InvitePerson;

/// The most occurrences of one series read for a span: a rule repeating
/// every minute should not stall a month view.
const SERIES_LIMIT: usize = 2_000;

/// An event's or task's standing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Status {
    #[default]
    None,
    Tentative,
    Confirmed,
    Cancelled,
    NeedsAction,
    InProcess,
    Completed,
}

impl Status {
    fn read(text: &str) -> Status {
        match text.trim().to_ascii_uppercase().as_str() {
            "TENTATIVE" => Status::Tentative,
            "CONFIRMED" => Status::Confirmed,
            "CANCELLED" => Status::Cancelled,
            "NEEDS-ACTION" => Status::NeedsAction,
            "IN-PROCESS" => Status::InProcess,
            "COMPLETED" => Status::Completed,
            _ => Status::None,
        }
    }

    pub(crate) fn ical(self) -> Option<&'static str> {
        Some(match self {
            Status::None => return None,
            Status::Tentative => "TENTATIVE",
            Status::Confirmed => "CONFIRMED",
            Status::Cancelled => "CANCELLED",
            Status::NeedsAction => "NEEDS-ACTION",
            Status::InProcess => "IN-PROCESS",
            Status::Completed => "COMPLETED",
        })
    }
}

/// One `VEVENT` or `VTODO`: a single event, the master of a series, or one
/// changed occurrence of a series (it has a `RECURRENCE-ID`).
#[derive(Debug, Clone)]
pub(crate) struct Comp {
    pub(crate) uid: String,
    pub(crate) task: bool,
    /// Set on a changed occurrence: the start it replaces.
    pub(crate) rid: Option<Stamp>,
    pub(crate) summary: String,
    pub(crate) location: String,
    pub(crate) description: String,
    pub(crate) url: String,
    pub(crate) start: Option<Stamp>,
    pub(crate) end: Option<Stamp>,
    /// `DURATION` in seconds, when there is no end.
    pub(crate) duration: Option<i64>,
    pub(crate) due: Option<Stamp>,
    pub(crate) rrule: Option<Rule>,
    pub(crate) exdates: Vec<Stamp>,
    pub(crate) rdates: Vec<Stamp>,
    pub(crate) status: Status,
    /// `TRANSP:TRANSPARENT`: shown as free time.
    pub(crate) free: bool,
    /// `CLASS:PRIVATE` or `CONFIDENTIAL`.
    pub(crate) private: bool,
    pub(crate) organizer: Option<InvitePerson>,
    pub(crate) attendees: Vec<InvitePerson>,
    /// Alarms relative to the start, in seconds before it (negative after).
    pub(crate) alarms: Vec<i64>,
    pub(crate) categories: Vec<String>,
    /// RFC 7986 `COLOR`, a CSS name or `#rrggbb`.
    pub(crate) color: Option<String>,
    pub(crate) sequence: i64,
    /// The component as it arrived, for edits that keep what isn't shown.
    pub(crate) node: Node,
}

impl Comp {
    /// Read a `VEVENT` or `VTODO`.
    pub(crate) fn read(node: &Node) -> Option<Comp> {
        let task = match node.name.as_str() {
            "VEVENT" => false,
            "VTODO" => true,
            _ => return None,
        };
        let uid = node.text("UID");
        if uid.is_empty() {
            return None;
        }
        let stamp = |name: &str| node.prop(name).and_then(|l| Stamp::read(&l));
        let list = |name: &str| -> Vec<Stamp> {
            node.all(name)
                .iter()
                .flat_map(Stamp::read_list)
                .collect()
        };
        let alarms = node
            .children
            .iter()
            .filter(|c| c.name == "VALARM")
            .filter_map(|a| {
                let trigger = a.prop("TRIGGER")?;
                // An alarm set to a fixed moment, or counted from the end,
                // is kept in the event but not offered as a lead time.
                if trigger
                    .param("VALUE")
                    .is_some_and(|v| v.eq_ignore_ascii_case("DATE-TIME"))
                    || trigger
                        .param("RELATED")
                        .is_some_and(|v| v.eq_ignore_ascii_case("END"))
                {
                    return None;
                }
                crate::mail::invite::duration_seconds(trigger.raw.trim()).map(|s| -s)
            })
            .collect();
        let organizer = node
            .prop("ORGANIZER")
            .and_then(|l| crate::mail::invite::person(&l));
        let attendees = node
            .all("ATTENDEE")
            .iter()
            .filter_map(crate::mail::invite::person)
            .collect();
        let categories = node
            .all("CATEGORIES")
            .iter()
            .flat_map(|l| {
                l.raw
                    .split(',')
                    .map(|c| c.replace("\\,", ",").trim().to_string())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
            })
            .collect();
        let class = node.text("CLASS").to_ascii_uppercase();
        Some(Comp {
            uid,
            task,
            rid: stamp("RECURRENCE-ID"),
            summary: node.text("SUMMARY"),
            location: node.text("LOCATION"),
            description: node.text("DESCRIPTION"),
            url: node.text("URL"),
            start: stamp("DTSTART"),
            end: stamp("DTEND"),
            duration: node
                .prop("DURATION")
                .and_then(|l| crate::mail::invite::duration_seconds(l.raw.trim())),
            due: stamp("DUE"),
            rrule: node.prop("RRULE").and_then(|l| Rule::parse(&l.raw)),
            exdates: list("EXDATE"),
            rdates: list("RDATE"),
            status: Status::read(&node.text("STATUS")),
            free: node.text("TRANSP").eq_ignore_ascii_case("TRANSPARENT"),
            private: class == "PRIVATE" || class == "CONFIDENTIAL",
            organizer,
            attendees,
            alarms,
            categories,
            color: Some(node.text("COLOR")).filter(|c| !c.is_empty()),
            sequence: node.text("SEQUENCE").parse().unwrap_or(0),
            node: node.clone(),
        })
    }

    /// What the event is placed by: its start, or a task's due date.
    pub(crate) fn anchor(&self) -> Option<&Stamp> {
        self.start.as_ref().or(self.due.as_ref())
    }

    /// The event's length in seconds of wall time (whole days for an
    /// all-day event), from its end, its duration or the RFC's defaults.
    pub(crate) fn length(&self, zones: &mut Zones) -> i64 {
        let Some(start) = self.anchor() else {
            return 0;
        };
        if let (Some(end), false) = (&self.end, self.task) {
            if end.is_date() && start.is_date() {
                return (end.date() - start.date()).num_seconds().max(86_400);
            }
            return (zones.unix(end) - zones.unix(start)).max(0);
        }
        if let Some(d) = self.duration {
            return d.max(0);
        }
        // No end: an all-day event takes its day, a timed one is a moment.
        if start.is_date() {
            86_400
        } else {
            0
        }
    }

    /// Whether this is the master of a series.
    pub(crate) fn repeats(&self) -> bool {
        self.rid.is_none() && (self.rrule.is_some() || !self.rdates.is_empty())
    }

    /// Whether the organizer is someone else: a meeting the user was
    /// invited to.
    pub(crate) fn invited(&self, me: &[String]) -> bool {
        self.organizer
            .as_ref()
            .is_some_and(|o| !me.iter().any(|m| m.eq_ignore_ascii_case(&o.email)))
            && !self.attendees.is_empty()
    }

    /// The user's own answer to a meeting, from the attendee list.
    pub(crate) fn my_status<'a>(&'a self, me: &[String]) -> Option<&'a str> {
        self.attendees
            .iter()
            .find(|a| me.iter().any(|m| m.eq_ignore_ascii_case(&a.email)))
            .map(|a| a.status.as_str())
    }
}

/// Which occurrence of which event: what a view selects and an edit names.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct OccKey {
    pub(crate) calendar: String,
    pub(crate) uid: String,
    /// The occurrence's id within its series (its original start, as
    /// `RECURRENCE-ID` writes it), or empty for a single event.
    pub(crate) rid: String,
}

/// One time an event happens, placed in local time.
#[derive(Debug, Clone)]
pub(crate) struct Occ {
    pub(crate) key: OccKey,
    pub(crate) comp: Arc<Comp>,
    /// Unix seconds. An all-day event starts at the local midnight of its
    /// first day and ends at the next midnight after its last.
    pub(crate) start: i64,
    pub(crate) end: i64,
    pub(crate) all_day: bool,
    /// Local days, the last one included.
    pub(crate) first_day: NaiveDate,
    pub(crate) last_day: NaiveDate,
    /// Part of a series (the master or a changed occurrence).
    pub(crate) in_series: bool,
    /// The occurrence's original start, for editing it alone.
    pub(crate) rid: Option<Stamp>,
}

impl Occ {
    /// Whether it spans more than one day or is all-day: drawn in the day
    /// row above the hours.
    pub(crate) fn in_day_row(&self) -> bool {
        self.all_day || self.first_day != self.last_day && self.end - self.start >= 86_400
    }
}

/// Every component of one calendar, grouped for expanding.
#[derive(Debug, Clone, Default)]
pub(crate) struct Book {
    /// By UID: the master (or single event) and its changed occurrences.
    pub(crate) series: HashMap<String, Series>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Series {
    pub(crate) master: Option<Arc<Comp>>,
    pub(crate) changed: Vec<Arc<Comp>>,
}

impl Book {
    /// Read a calendar's objects as EDS hands them over: each one a
    /// `VEVENT`, a `VTODO` or a `VCALENDAR` holding several (a series with
    /// changed occurrences comes as one). `VTIMEZONE`s found go to `zones`.
    pub(crate) fn read(objects: &[String], zones: &mut Zones) -> Book {
        let mut book = Book::default();
        for text in objects {
            for node in Node::parse_all(text) {
                book.take(node, zones);
            }
        }
        book
    }

    fn take(&mut self, node: Node, zones: &mut Zones) {
        match node.name.as_str() {
            "VCALENDAR" => {
                for child in node.children {
                    self.take(child, zones);
                }
            }
            "VTIMEZONE" => zones.learn(&node),
            _ => {
                if let Some(comp) = Comp::read(&node) {
                    self.add(comp);
                }
            }
        }
    }

    pub(crate) fn add(&mut self, comp: Comp) {
        let series = self.series.entry(comp.uid.clone()).or_default();
        if comp.rid.is_some() {
            series.changed.retain(|c| c.rid != comp.rid);
            series.changed.push(Arc::new(comp));
        } else {
            series.master = Some(Arc::new(comp));
        }
    }

    /// The component an occurrence came from.
    pub(crate) fn comp(&self, uid: &str, rid: &str) -> Option<Arc<Comp>> {
        let series = self.series.get(uid)?;
        if !rid.is_empty() {
            if let Some(c) = series
                .changed
                .iter()
                .find(|c| c.rid.as_ref().is_some_and(|r| r.value() == rid))
            {
                return Some(c.clone());
            }
        }
        series.master.clone()
    }

    /// Every TZID the events name, for asking the calendar about those
    /// the system doesn't know.
    pub(crate) fn tzids(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in self.series.values() {
            for c in s.master.iter().chain(s.changed.iter()) {
                for st in [&c.start, &c.end, &c.due, &c.rid].into_iter().flatten() {
                    if let Some(t) = st.tzid() {
                        if !out.iter().any(|o| o == t) {
                            out.push(t.to_string());
                        }
                    }
                }
            }
        }
        out
    }

    /// Every occurrence that overlaps `[from, to)` (Unix seconds).
    pub(crate) fn occurrences(
        &self,
        calendar: &str,
        zones: &mut Zones,
        from: i64,
        to: i64,
    ) -> Vec<Occ> {
        let mut out = Vec::new();
        for series in self.series.values() {
            expand_series(calendar, series, zones, from, to, &mut out);
        }
        out
    }
}

/// The occurrences of one series in `[from, to)`.
fn expand_series(
    calendar: &str,
    series: &Series,
    zones: &mut Zones,
    from: i64,
    to: i64,
    out: &mut Vec<Occ>,
) {
    // The changed occurrences stand in for the ones they replace, wherever
    // they were moved to.
    let mut replaced: Vec<i64> = Vec::new();
    for c in &series.changed {
        if let Some(rid) = &c.rid {
            replaced.push(zones.unix(rid));
        }
        if c.status == Status::Cancelled {
            continue;
        }
        if let Some(occ) = place(calendar, c, c.anchor().cloned(), zones, true) {
            if overlaps(&occ, from, to) {
                out.push(occ);
            }
        }
    }
    let Some(master) = &series.master else {
        return;
    };
    let Some(start) = master.anchor().cloned() else {
        return;
    };
    if !master.repeats() {
        if let Some(occ) = place(calendar, master, Some(start), zones, false) {
            if overlaps(&occ, from, to) {
                out.push(occ);
            }
        }
        return;
    }
    let length = master.length(zones);
    // Wide enough to catch an occurrence that began before the span and
    // runs into it.
    let from_wall = zones.wall_of(from - length - 86_400, &start);
    let to_wall = zones.wall_of(to + 86_400, &start);
    let tzid = start.tzid().map(str::to_string);
    let mut starts: Vec<NaiveDateTime> = {
        let cell = std::cell::RefCell::new(&mut *zones);
        let to_utc = |w: NaiveDateTime| -> i64 {
            match &tzid {
                Some(t) => cell.borrow_mut().wall_to_unix(w, t),
                None if start_is_utc(&start) => w.and_utc().timestamp(),
                None => super::time::local_unix(w),
            }
        };
        match &master.rrule {
            Some(rule) => {
                recur::expand(rule, start.wall(), from_wall, to_wall, &to_utc, SERIES_LIMIT)
            }
            None => vec![start.wall()],
        }
    };
    for r in &master.rdates {
        let w = r.wall();
        if w >= from_wall && w < to_wall && !starts.contains(&w) {
            starts.push(w);
        }
    }
    let excluded: Vec<NaiveDateTime> = master.exdates.iter().map(Stamp::wall).collect();
    let excluded_days: Vec<NaiveDate> = master
        .exdates
        .iter()
        .filter(|e| e.is_date())
        .map(Stamp::date)
        .collect();
    for wall in starts {
        if excluded.contains(&wall) || excluded_days.contains(&wall.date()) {
            continue;
        }
        let rid = start.with_wall(wall);
        if replaced.contains(&zones.unix(&rid)) {
            continue;
        }
        if let Some(mut occ) = place(calendar, master, Some(rid.clone()), zones, true) {
            if overlaps(&occ, from, to) {
                occ.key.rid = rid.value();
                occ.rid = Some(rid);
                out.push(occ);
            }
        }
    }
}

fn start_is_utc(s: &Stamp) -> bool {
    matches!(s, Stamp::Utc(_))
}

fn overlaps(occ: &Occ, from: i64, to: i64) -> bool {
    if occ.start == occ.end {
        return occ.start >= from && occ.start < to;
    }
    occ.end > from && occ.start < to
}

/// An occurrence of `comp` starting at `at` (its own start when `None`).
fn place(
    calendar: &str,
    comp: &Arc<Comp>,
    at: Option<Stamp>,
    zones: &mut Zones,
    in_series: bool,
) -> Option<Occ> {
    let at = at.or_else(|| comp.anchor().cloned())?;
    let length = comp.length(zones);
    let key = OccKey {
        calendar: calendar.to_string(),
        uid: comp.uid.clone(),
        rid: comp.rid.as_ref().map(Stamp::value).unwrap_or_default(),
    };
    if at.is_date() {
        let first = at.date();
        let days = (length / 86_400).max(1);
        let last = first + Duration::days(days - 1);
        return Some(Occ {
            key,
            comp: comp.clone(),
            start: day_start(first),
            end: day_start(last + Duration::days(1)),
            all_day: true,
            first_day: first,
            last_day: last,
            in_series,
            rid: comp.rid.clone(),
        });
    }
    let start = zones.unix(&at);
    let end = start + length;
    // An event ending at midnight doesn't take the next day.
    let last = local_day((end - 1).max(start));
    Some(Occ {
        key,
        comp: comp.clone(),
        start,
        end,
        all_day: false,
        first_day: local_day(start),
        last_day: last,
        in_series,
        rid: comp.rid.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    fn book(text: &str) -> (Book, Zones) {
        let mut zones = Zones::default();
        let b = Book::read(&[text.to_string()], &mut zones);
        (b, zones)
    }

    fn day(y: i32, m: u32, d: u32) -> i64 {
        day_start(NaiveDate::from_ymd_opt(y, m, d).unwrap())
    }

    #[test]
    fn an_event_reads_its_fields() {
        let (b, _) = book(
            "BEGIN:VEVENT\nUID:1\nSUMMARY:Lunch\\, with Sam\nLOCATION:Café\n\
             DTSTART;TZID=Europe/Paris:20261102T120000\nDTEND;TZID=Europe/Paris:20261102T130000\n\
             ORGANIZER;CN=Sam:mailto:sam@x.org\nATTENDEE;PARTSTAT=ACCEPTED:mailto:me@x.org\n\
             CATEGORIES:Work,Food\nTRANSP:TRANSPARENT\nCLASS:PRIVATE\nSTATUS:TENTATIVE\n\
             BEGIN:VALARM\nTRIGGER:-PT15M\nACTION:DISPLAY\nEND:VALARM\nEND:VEVENT\n",
        );
        let c = b.comp("1", "").unwrap();
        assert_eq!(c.summary, "Lunch, with Sam");
        assert_eq!(c.location, "Café");
        assert_eq!(c.alarms, vec![900]);
        assert_eq!(c.categories, vec!["Work", "Food"]);
        assert!(c.free && c.private);
        assert_eq!(c.status, Status::Tentative);
        assert!(c.invited(&["me@x.org".into()]));
        assert_eq!(c.my_status(&["ME@x.org".into()]), Some("ACCEPTED"));
    }

    #[test]
    fn an_all_day_event_takes_its_days() {
        let (b, mut z) = book(
            "BEGIN:VEVENT\nUID:2\nDTSTART;VALUE=DATE:20261102\nDTEND;VALUE=DATE:20261105\nEND:VEVENT\n",
        );
        let occ = b.occurrences("c", &mut z, day(2026, 11, 1), day(2026, 11, 30));
        assert_eq!(occ.len(), 1);
        assert!(occ[0].all_day && occ[0].in_day_row());
        assert_eq!(occ[0].first_day.day(), 2);
        assert_eq!(occ[0].last_day.day(), 4);
    }

    #[test]
    fn a_series_skips_exdates_and_takes_its_changed_occurrences() {
        let (b, mut z) = book(
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:3\nSUMMARY:Standup\n\
             DTSTART:20261102T090000\nDTEND:20261102T091500\nRRULE:FREQ=DAILY;COUNT=5\n\
             EXDATE:20261103T090000\nEND:VEVENT\n\
             BEGIN:VEVENT\nUID:3\nRECURRENCE-ID:20261104T090000\nSUMMARY:Standup (moved)\n\
             DTSTART:20261104T100000\nDTEND:20261104T101500\nEND:VEVENT\nEND:VCALENDAR\n",
        );
        let mut occ = b.occurrences("c", &mut z, day(2026, 11, 1), day(2026, 12, 1));
        occ.sort_by_key(|o| o.start);
        let days: Vec<u32> = occ.iter().map(|o| o.first_day.day()).collect();
        assert_eq!(days, vec![2, 4, 5, 6]);
        let moved = &occ[1];
        assert_eq!(moved.comp.summary, "Standup (moved)");
        assert_eq!(moved.key.rid, "20261104T090000");
        assert_eq!(occ[2].key.rid, "20261105T090000");
        assert!(occ.iter().all(|o| o.in_series));
    }

    #[test]
    fn an_endless_series_answers_a_late_span() {
        let (b, mut z) = book(
            "BEGIN:VEVENT\nUID:4\nDTSTART;VALUE=DATE:19900315\nRRULE:FREQ=YEARLY\nSUMMARY:Birthday\nEND:VEVENT\n",
        );
        let occ = b.occurrences("c", &mut z, day(2026, 3, 1), day(2026, 4, 1));
        assert_eq!(occ.len(), 1);
        assert_eq!(occ[0].first_day, NaiveDate::from_ymd_opt(2026, 3, 15).unwrap());
        assert_eq!(occ[0].key.rid, "20260315");
    }

    #[test]
    fn a_timed_event_ending_at_midnight_keeps_to_its_day() {
        let (b, mut z) = book(
            "BEGIN:VEVENT\nUID:5\nDTSTART:20261102T220000\nDTEND:20261103T000000\nEND:VEVENT\n",
        );
        let occ = b.occurrences("c", &mut z, day(2026, 11, 1), day(2026, 11, 30));
        assert_eq!(occ[0].first_day, occ[0].last_day);
        assert!(!occ[0].in_day_row());
    }
}
