//! Writing events: a new one from the editor, an edit over what the server
//! had, and the three ways a change to one occurrence of a series can go
//! (that one, it and the ones after, or all of them).
//!
//! Each change is worked out here, on the component itself, rather than
//! left to the calendar backend: EDS's backends differ in what they do with
//! "this and future", and Hylki's own file calendar has no backend at all.
//! What reaches the store is always a whole component to create, replace
//! or remove.

use chrono::{Duration, NaiveDate, NaiveDateTime};

use super::model::{Comp, Occ, Status};
use super::recur::{self, Rule, Until};
use super::time::{local_tzid, utc_now_value, Stamp, Zones};
use super::tree::Node;
use crate::mail::invite::ics::escape;
use crate::models::InvitePerson;

/// When an event happens, as the editor leaves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Span {
    /// Wall times in `tzid` (local when `None`).
    Timed {
        start: NaiveDateTime,
        end: NaiveDateTime,
        tzid: Option<String>,
    },
    /// Whole days, the last one included.
    AllDay { first: NaiveDate, last: NaiveDate },
}

/// An event or task as the editor leaves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Draft {
    pub(crate) task: bool,
    pub(crate) summary: String,
    pub(crate) location: String,
    pub(crate) description: String,
    pub(crate) url: String,
    pub(crate) span: Span,
    /// A task without a due date has no span; `span` is then ignored.
    pub(crate) undated: bool,
    pub(crate) repeat: Option<Rule>,
    /// Seconds before the start.
    pub(crate) alarms: Vec<i64>,
    pub(crate) free: bool,
    pub(crate) private: bool,
    pub(crate) status: Status,
    pub(crate) organizer: Option<InvitePerson>,
    pub(crate) attendees: Vec<InvitePerson>,
    pub(crate) categories: Vec<String>,
    pub(crate) completed: bool,
}

impl Draft {
    /// A new event over `span`.
    pub(crate) fn new(span: Span) -> Draft {
        Draft {
            task: false,
            summary: String::new(),
            location: String::new(),
            description: String::new(),
            url: String::new(),
            span,
            undated: false,
            repeat: None,
            alarms: Vec::new(),
            free: false,
            private: false,
            status: Status::None,
            organizer: None,
            attendees: Vec::new(),
            categories: Vec::new(),
            completed: false,
        }
    }

    /// An occurrence as the editor opens it: the series' rule, and this
    /// occurrence's own times.
    pub(crate) fn from_occ(occ: &Occ, master: Option<&Comp>, zones: &mut Zones) -> Draft {
        let c = &occ.comp;
        let span = if occ.all_day {
            Span::AllDay {
                first: occ.first_day,
                last: occ.last_day,
            }
        } else {
            let zone = c.anchor().and_then(|s| s.tzid()).map(str::to_string);
            let like = c.anchor().cloned().unwrap_or(Stamp::Floating(NaiveDateTime::MIN));
            // A UTC event is edited in local time, which is what it shows in.
            let (start, end) = match &like {
                Stamp::Zoned(..) => (zones.wall_of(occ.start, &like), zones.wall_of(occ.end, &like)),
                _ => (
                    super::time::local_wall(occ.start),
                    super::time::local_wall(occ.end),
                ),
            };
            Span::Timed {
                start,
                end,
                tzid: zone,
            }
        };
        Draft {
            task: c.task,
            summary: c.summary.clone(),
            location: c.location.clone(),
            description: c.description.clone(),
            url: c.url.clone(),
            span,
            undated: c.anchor().is_none(),
            repeat: master.and_then(|m| m.rrule.clone()).or_else(|| c.rrule.clone()),
            alarms: c.alarms.clone(),
            free: c.free,
            private: c.private,
            status: c.status,
            organizer: c.organizer.clone(),
            attendees: c.attendees.clone(),
            categories: c.categories.clone(),
            completed: c.status == Status::Completed,
        }
    }
}

/// A new UID.
pub(crate) fn new_uid() -> String {
    super::ical::new_uid()
}

/// A new component for `draft`.
pub(crate) fn create(draft: &Draft) -> Node {
    let mut node = Node::new(if draft.task { "VTODO" } else { "VEVENT" });
    node.push(format!("UID:{}", new_uid()));
    node.push(format!("DTSTAMP:{}", utc_now_value()));
    node.push(format!("CREATED:{}", utc_now_value()));
    apply(&mut node, draft, None);
    node
}

/// Write `draft` over `node`, keeping whatever else it holds. `was` is the
/// component as read, to tell whether the times changed (which an attendee's
/// calendar learns from a higher `SEQUENCE`).
pub(crate) fn apply(node: &mut Node, draft: &Draft, was: Option<&Comp>) {
    node.set_text("SUMMARY", draft.summary.trim());
    node.set_text("LOCATION", draft.location.trim());
    node.set_text("DESCRIPTION", draft.description.trim());
    if draft.url.trim().is_empty() {
        node.remove("URL");
    } else {
        node.replace(format!("URL:{}", draft.url.trim()));
    }
    let (start, end) = stamps(&draft.span);
    if draft.task {
        node.remove("DTEND");
        node.remove("DURATION");
        node.remove("DTSTART");
        if draft.undated {
            node.remove("DUE");
        } else {
            node.replace(start.line("DUE"));
        }
        if draft.completed {
            node.replace("STATUS:COMPLETED".into());
            node.replace("PERCENT-COMPLETE:100".into());
            if node.prop("COMPLETED").is_none() {
                node.replace(format!("COMPLETED:{}", utc_now_value()));
            }
        } else {
            if was.is_some_and(|w| w.status == Status::Completed) {
                node.replace("STATUS:NEEDS-ACTION".into());
                node.remove("PERCENT-COMPLETE");
            }
            node.remove("COMPLETED");
        }
    } else {
        node.replace(start.line("DTSTART"));
        node.remove("DURATION");
        node.replace(end.line("DTEND"));
        match draft.status.ical() {
            Some(s) => node.replace(format!("STATUS:{s}")),
            None => node.remove("STATUS"),
        }
        if draft.free {
            node.replace("TRANSP:TRANSPARENT".into());
        } else {
            node.remove("TRANSP");
        }
    }
    // A changed occurrence carries no rule of its own.
    if node.prop("RECURRENCE-ID").is_none() {
        match &draft.repeat {
            Some(rule) => node.replace(format!("RRULE:{}", rule.to_ical())),
            None => {
                node.remove("RRULE");
                node.remove("EXDATE");
                node.remove("RDATE");
            }
        }
    }
    if draft.private {
        node.replace("CLASS:PRIVATE".into());
    } else if node.text("CLASS").eq_ignore_ascii_case("PRIVATE") {
        node.remove("CLASS");
    }
    if draft.categories.is_empty() {
        node.remove("CATEGORIES");
    } else {
        let cats: Vec<String> = draft.categories.iter().map(|c| escape(c)).collect();
        node.replace(format!("CATEGORIES:{}", cats.join(",")));
    }
    set_people(node, draft);
    set_alarms(node, &draft.alarms, &draft.summary);
    // By instant, not as written: an event the server wrote in UTC comes
    // back in local time without having moved.
    let moved = was.is_some_and(|w| {
        let mut zones = Zones::default();
        let mut at = |s: Option<&Stamp>| s.map(|s| zones.unix(s));
        at(w.anchor()) != at(Some(&start)) || (!w.task && at(w.end.as_ref()) != at(Some(&end)))
    });
    if moved {
        let seq = was.map_or(0, |w| w.sequence) + 1;
        node.replace(format!("SEQUENCE:{seq}"));
    }
    node.replace(format!("DTSTAMP:{}", utc_now_value()));
    node.replace(format!("LAST-MODIFIED:{}", utc_now_value()));
}

/// The start and end values a span writes. A timed event goes in its zone,
/// or the system's, so a series keeps its wall time across daylight saving
/// time; with no zone name to be had, it goes in UTC.
fn stamps(span: &Span) -> (Stamp, Stamp) {
    match span {
        Span::AllDay { first, last } => (
            Stamp::Date(*first),
            Stamp::Date((*last).max(*first) + Duration::days(1)),
        ),
        Span::Timed { start, end, tzid } => {
            let end = (*end).max(*start);
            match tzid.clone().or_else(local_tzid) {
                Some(tz) => (Stamp::Zoned(*start, tz.clone()), Stamp::Zoned(end, tz)),
                None => {
                    let s = super::time::local_unix(*start);
                    let e = super::time::local_unix(end);
                    let utc = |u: i64| {
                        chrono::DateTime::from_timestamp(u, 0)
                            .map(|d| d.naive_utc())
                            .unwrap_or_default()
                    };
                    (Stamp::Utc(utc(s)), Stamp::Utc(utc(e)))
                }
            }
        }
    }
}

fn set_people(node: &mut Node, draft: &Draft) {
    node.remove("ATTENDEE");
    match &draft.organizer {
        Some(o) if !draft.attendees.is_empty() => node.replace(person_line("ORGANIZER", o, false)),
        _ if draft.attendees.is_empty() => node.remove("ORGANIZER"),
        _ => {}
    }
    for a in &draft.attendees {
        node.push(person_line("ATTENDEE", a, true));
    }
}

fn person_line(name: &str, p: &InvitePerson, attendee: bool) -> String {
    let mut line = name.to_string();
    if !p.name.trim().is_empty() {
        line.push_str(&format!(";CN=\"{}\"", p.name.trim().replace('"', "")));
    }
    if attendee {
        if p.resource {
            line.push_str(";CUTYPE=RESOURCE");
        }
        line.push_str(";ROLE=REQ-PARTICIPANT");
        let status = if p.status.is_empty() {
            "NEEDS-ACTION"
        } else {
            p.status.as_str()
        };
        line.push_str(&format!(";PARTSTAT={status}"));
        if p.rsvp {
            line.push_str(";RSVP=TRUE");
        }
    }
    line.push_str(&format!(":mailto:{}", p.email.trim()));
    line
}

/// Replace the alarms that count back from the start; keep any other kind
/// (a fixed moment, one counted from the end) as the server wrote it.
fn set_alarms(node: &mut Node, alarms: &[i64], summary: &str) {
    node.children.retain(|c| {
        if c.name != "VALARM" {
            return true;
        }
        let Some(t) = c.prop("TRIGGER") else {
            return false;
        };
        t.param("VALUE")
            .is_some_and(|v| v.eq_ignore_ascii_case("DATE-TIME"))
            || t.param("RELATED")
                .is_some_and(|v| v.eq_ignore_ascii_case("END"))
    });
    let mut seen = Vec::new();
    for &before in alarms {
        if seen.contains(&before) {
            continue;
        }
        seen.push(before);
        let mut a = Node::new("VALARM");
        a.push("ACTION:DISPLAY".into());
        a.push(format!("TRIGGER;RELATED=START:{}", duration(-before)));
        a.push(format!(
            "DESCRIPTION:{}",
            escape(if summary.trim().is_empty() {
                "Reminder"
            } else {
                summary.trim()
            })
        ));
        node.children.push(a);
    }
}

/// `-PT15M`, `PT0S`, `-P1D`: seconds as an iCalendar duration.
pub(crate) fn duration(secs: i64) -> String {
    let sign = if secs < 0 { "-" } else { "" };
    let s = secs.abs();
    if s == 0 {
        return "PT0S".into();
    }
    if s % 604_800 == 0 {
        return format!("{sign}P{}W", s / 604_800);
    }
    if s % 86_400 == 0 {
        return format!("{sign}P{}D", s / 86_400);
    }
    let (d, rest) = (s / 86_400, s % 86_400);
    let mut out = format!("{sign}P");
    if d > 0 {
        out.push_str(&format!("{d}D"));
    }
    out.push('T');
    let (h, m, sec) = (rest / 3600, (rest % 3600) / 60, rest % 60);
    if h > 0 {
        out.push_str(&format!("{h}H"));
    }
    if m > 0 {
        out.push_str(&format!("{m}M"));
    }
    if sec > 0 {
        out.push_str(&format!("{sec}S"));
    }
    out
}

/// Which occurrences of a series a change covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    This,
    ThisAndFuture,
    All,
}

/// What a store is asked to do.
#[derive(Debug, Clone)]
pub(crate) enum Change {
    /// Add a component.
    Create(Node),
    /// Replace the component with this UID (and `RECURRENCE-ID`, if it
    /// has one) by this one.
    Replace(Node),
    /// Remove a whole series, or a single event.
    Remove { uid: String },
    /// Remove one occurrence of a series: an `EXDATE` on the master, and
    /// the changed occurrence if there is one.
    RemoveOne { uid: String, rid: Stamp },
}

/// The occurrence `occ` changed to `draft`, over `scope`.
pub(crate) fn edit(
    occ: &Occ,
    master: Option<&Comp>,
    draft: &Draft,
    scope: Scope,
    zones: &mut Zones,
) -> Vec<Change> {
    let series = master.filter(|m| m.repeats());
    match (series, scope) {
        (None, _) => {
            let mut node = occ.comp.node.clone();
            apply(&mut node, draft, Some(&occ.comp));
            vec![Change::Replace(node)]
        }
        (Some(m), Scope::All) => {
            // The whole series moves by as much as this occurrence did,
            // from its own first start.
            let mut node = m.node.clone();
            let shifted = shift_span(draft, occ, m, zones);
            apply(&mut node, &shifted, Some(m));
            // Changed occurrences keep their own times but take the new
            // wording.
            let mut out = vec![Change::Replace(node)];
            if occ.comp.rid.is_some() {
                let mut one = occ.comp.node.clone();
                let mut d = draft.clone();
                d.repeat = None;
                apply(&mut one, &d, Some(&occ.comp));
                out.push(Change::Replace(one));
            }
            out
        }
        (Some(m), Scope::This) => {
            let Some(rid) = occ.rid.clone().or_else(|| occ.comp.rid.clone()) else {
                return Vec::new();
            };
            let mut node = if occ.comp.rid.is_some() {
                occ.comp.node.clone()
            } else {
                detached(m, &rid)
            };
            let mut d = draft.clone();
            d.repeat = None;
            apply(&mut node, &d, Some(&occ.comp));
            vec![Change::Replace(node)]
        }
        (Some(m), Scope::ThisAndFuture) => {
            let Some(rid) = occ.rid.clone().or_else(|| occ.comp.rid.clone()) else {
                return Vec::new();
            };
            let Some(first) = m.anchor() else {
                return Vec::new();
            };
            // The first occurrence: the same as changing them all.
            if rid.wall() <= first.wall() {
                return edit(occ, master, draft, Scope::All, zones);
            }
            let mut out = Vec::new();
            let mut ended = m.node.clone();
            end_before(&mut ended, m, &rid, zones);
            out.push(Change::Replace(ended));
            let mut fresh = Node::new(&m.node.name);
            fresh.push(format!("UID:{}", new_uid()));
            fresh.push(format!("DTSTAMP:{}", utc_now_value()));
            // The lines of the old series that the editor doesn't write.
            for line in &m.node.lines {
                let name = line
                    .split([':', ';'])
                    .next()
                    .unwrap_or_default()
                    .to_ascii_uppercase();
                if !matches!(
                    name.as_str(),
                    "UID" | "DTSTAMP" | "RRULE" | "EXDATE" | "RDATE" | "SEQUENCE" | "CREATED"
                ) {
                    fresh.push(line.clone());
                }
            }
            fresh.children = m.node.children.clone();
            let mut d = draft.clone();
            if let Some(rule) = d.repeat.as_mut() {
                // A count goes on with what the old series had left.
                if let Some(count) = rule.count {
                    let before = recur::all(
                        rule,
                        first.wall(),
                        &|w| w.and_utc().timestamp(),
                        count as usize,
                    )
                    .map(|v| v.iter().filter(|w| **w < rid.wall()).count() as u32)
                    .unwrap_or(0);
                    rule.count = Some(count.saturating_sub(before).max(1));
                }
            }
            apply(&mut fresh, &d, None);
            out.push(Change::Create(fresh));
            out
        }
    }
}

/// An occurrence of `master` written out as a component of its own.
fn detached(master: &Comp, rid: &Stamp) -> Node {
    let mut node = master.node.clone();
    for name in ["RRULE", "RDATE", "EXDATE", "RECURRENCE-ID"] {
        node.remove(name);
    }
    node.push(rid.line("RECURRENCE-ID"));
    node
}

/// The draft of an occurrence, made to start where the whole series would
/// have to start for that occurrence to land where it was put.
fn shift_span(draft: &Draft, occ: &Occ, master: &Comp, zones: &mut Zones) -> Draft {
    let mut d = draft.clone();
    let Some(first) = master.anchor() else {
        return d;
    };
    let orig = occ.rid.clone().unwrap_or_else(|| first.clone());
    match &draft.span {
        Span::AllDay { first: f, last } => {
            let delta = *f - orig.date();
            let len = *last - *f;
            let start = first.date() + delta;
            d.span = Span::AllDay {
                first: start,
                last: start + len,
            };
        }
        Span::Timed { start, end, tzid } => {
            // A date or a zoned time is its own wall time; UTC reads as
            // the local time the editor showed.
            let was = if orig.is_date() || matches!(orig, Stamp::Zoned(..)) {
                orig.wall()
            } else {
                super::time::local_wall(zones.unix(&orig))
            };
            let base = if matches!(first, Stamp::Zoned(..)) || first.is_date() {
                first.wall()
            } else {
                super::time::local_wall(zones.unix(first))
            };
            let delta = *start - was;
            let len = *end - *start;
            d.span = Span::Timed {
                start: base + delta,
                end: base + delta + len,
                tzid: tzid.clone(),
            };
        }
    }
    d
}

/// End a series just before the occurrence `rid`.
fn end_before(node: &mut Node, master: &Comp, rid: &Stamp, zones: &mut Zones) {
    let Some(mut rule) = master.rrule.clone() else {
        return;
    };
    rule.count = None;
    rule.until = Some(match rid {
        Stamp::Date(d) => Until::Date(d.pred_opt().unwrap_or(*d)),
        Stamp::Floating(w) => Until::Floating(*w - Duration::seconds(1)),
        _ => Until::Utc(zones.unix(rid) - 1),
    });
    node.replace(format!("RRULE:{}", rule.to_ical()));
    node.replace(format!("DTSTAMP:{}", utc_now_value()));
}

/// Remove `occ` over `scope`.
pub(crate) fn remove(occ: &Occ, master: Option<&Comp>, scope: Scope, zones: &mut Zones) -> Vec<Change> {
    let series = master.filter(|m| m.repeats());
    let rid = occ.rid.clone().or_else(|| occ.comp.rid.clone());
    match (series, scope, rid) {
        (Some(_), Scope::This, Some(rid)) => vec![Change::RemoveOne {
            uid: occ.comp.uid.clone(),
            rid,
        }],
        (Some(m), Scope::ThisAndFuture, Some(rid))
            if m.anchor().is_some_and(|f| rid.wall() > f.wall()) =>
        {
            let mut node = m.node.clone();
            end_before(&mut node, m, &rid, zones);
            vec![Change::Replace(node)]
        }
        _ => vec![Change::Remove {
            uid: occ.comp.uid.clone(),
        }],
    }
}

/// Move `occ` to start at `start` and end at `end` (local Unix seconds), or
/// to whole days from `first` for a day-row drag.
pub(crate) fn moved_draft(occ: &Occ, master: Option<&Comp>, start: i64, end: i64, zones: &mut Zones) -> Draft {
    let mut d = Draft::from_occ(occ, master, zones);
    d.span = if occ.all_day {
        let first = super::time::local_day(start);
        let last = super::time::local_day((end - 1).max(start));
        Span::AllDay { first, last }
    } else {
        let tzid = match &d.span {
            Span::Timed { tzid, .. } => tzid.clone(),
            Span::AllDay { .. } => None,
        };
        let (s, e) = match tzid.as_deref() {
            Some(t) => {
                let like = Stamp::Zoned(NaiveDateTime::MIN, t.to_string());
                (zones.wall_of(start, &like), zones.wall_of(end, &like))
            }
            None => (super::time::local_wall(start), super::time::local_wall(end)),
        };
        Span::Timed {
            start: s,
            end: e,
            tzid,
        }
    };
    d
}

/// An `ATTENDEE` line with its `PARTSTAT` set to `status` and its RSVP
/// request answered.
pub(crate) fn set_partstat(line: &str, status: &str) -> String {
    let Some(p) = crate::mail::invite::ics::split_line(line) else {
        return line.to_string();
    };
    let mut out = p.name.clone();
    for (k, v) in &p.params {
        if k.eq_ignore_ascii_case("PARTSTAT") || k.eq_ignore_ascii_case("RSVP") {
            continue;
        }
        if v.contains([':', ';', ',']) {
            out.push_str(&format!(";{k}=\"{v}\""));
        } else {
            out.push_str(&format!(";{k}={v}"));
        }
    }
    out.push_str(&format!(";PARTSTAT={status}:{}", p.raw));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::model::Book;
    use crate::calendar::time::day_start;

    fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn at(y: i32, m: u32, d: u32, h: u32) -> NaiveDateTime {
        ymd(y, m, d).and_hms_opt(h, 0, 0).unwrap()
    }

    fn timed(s: NaiveDateTime, e: NaiveDateTime) -> Span {
        Span::Timed {
            start: s,
            end: e,
            tzid: Some("Europe/Paris".into()),
        }
    }

    #[test]
    fn a_new_event_writes_what_the_editor_set() {
        let mut d = Draft::new(timed(at(2026, 11, 2, 9), at(2026, 11, 2, 10)));
        d.summary = "Review, final".into();
        d.alarms = vec![900, 900, 0];
        d.repeat = Rule::parse("FREQ=WEEKLY;BYDAY=MO");
        let node = create(&d);
        let text = node.write();
        assert!(text.contains("SUMMARY:Review\\, final"));
        assert!(text.contains("DTSTART;TZID=Europe/Paris:20261102T090000"));
        assert!(text.contains("RRULE:FREQ=WEEKLY;BYDAY=MO"));
        assert_eq!(node.children.len(), 2, "{text}");
        assert!(text.contains("TRIGGER;RELATED=START:-PT15M"));
        assert!(text.contains("TRIGGER;RELATED=START:PT0S"));
    }

    #[test]
    fn an_edit_keeps_what_it_doesnt_know() {
        let mut zones = Zones::default();
        let book = Book::read(
            &["BEGIN:VEVENT\nUID:x\nSUMMARY:Old\nX-APPLE-TRAVEL-ADVISORY-BEHAVIOR:AUTOMATIC\n\
               DTSTART:20261102T090000Z\nDTEND:20261102T100000Z\nSEQUENCE:2\nEND:VEVENT\n"
                .into()],
            &mut zones,
        );
        let occ = book
            .occurrences("c", &mut zones, day_start(ymd(2026, 11, 1)), day_start(ymd(2026, 11, 3)))
            .remove(0);
        let mut d = Draft::from_occ(&occ, None, &mut zones);
        d.summary = "New".into();
        let changes = edit(&occ, None, &d, Scope::All, &mut zones);
        let Change::Replace(node) = &changes[0] else {
            panic!()
        };
        let text = node.write();
        assert!(text.contains("SUMMARY:New"));
        assert!(text.contains("X-APPLE-TRAVEL-ADVISORY-BEHAVIOR:AUTOMATIC"));
        assert_eq!(node.text("UID"), "x");
        // Same times: the sequence stays.
        assert_eq!(node.text("SEQUENCE"), "2");
    }

    fn series() -> (Book, Zones) {
        let mut zones = Zones::default();
        let book = Book::read(
            &["BEGIN:VEVENT\nUID:s\nSUMMARY:Standup\nDTSTART;TZID=Europe/Paris:20261102T090000\n\
               DTEND;TZID=Europe/Paris:20261102T091500\nRRULE:FREQ=DAILY;COUNT=10\nEND:VEVENT\n"
                .into()],
            &mut zones,
        );
        (book, zones)
    }

    fn third(book: &Book, zones: &mut Zones) -> Occ {
        let mut occ = book.occurrences(
            "c",
            zones,
            day_start(ymd(2026, 11, 1)),
            day_start(ymd(2026, 12, 1)),
        );
        occ.sort_by_key(|o| o.start);
        occ.remove(2)
    }

    #[test]
    fn moving_one_occurrence_detaches_it() {
        let (book, mut zones) = series();
        let occ = third(&book, &mut zones);
        let master = book.comp("s", "").unwrap();
        let mut d = Draft::from_occ(&occ, Some(&master), &mut zones);
        d.span = timed(at(2026, 11, 4, 11), at(2026, 11, 4, 12));
        let changes = edit(&occ, Some(&master), &d, Scope::This, &mut zones);
        let Change::Replace(node) = &changes[0] else {
            panic!()
        };
        let text = node.write();
        assert!(text.contains("RECURRENCE-ID;TZID=Europe/Paris:20261104T090000"), "{text}");
        assert!(text.contains("DTSTART;TZID=Europe/Paris:20261104T110000"));
        assert!(!text.contains("RRULE"));
    }

    #[test]
    fn changing_this_and_future_splits_the_series() {
        let (book, mut zones) = series();
        let occ = third(&book, &mut zones);
        let master = book.comp("s", "").unwrap();
        let mut d = Draft::from_occ(&occ, Some(&master), &mut zones);
        d.summary = "Standup (new room)".into();
        let changes = edit(&occ, Some(&master), &d, Scope::ThisAndFuture, &mut zones);
        assert_eq!(changes.len(), 2);
        let (Change::Replace(old), Change::Create(new)) = (&changes[0], &changes[1]) else {
            panic!()
        };
        assert!(old.text("RRULE").contains("UNTIL=20261104T075959Z"), "{}", old.text("RRULE"));
        assert!(new.text("RRULE").contains("COUNT=8"), "{}", new.text("RRULE"));
        assert_ne!(new.text("UID"), "s");
        assert!(new.write().contains("DTSTART;TZID=Europe/Paris:20261104T090000"));
    }

    #[test]
    fn moving_the_whole_series_by_one_occurrence() {
        let (book, mut zones) = series();
        let occ = third(&book, &mut zones);
        let master = book.comp("s", "").unwrap();
        let mut d = Draft::from_occ(&occ, Some(&master), &mut zones);
        d.span = timed(at(2026, 11, 4, 10), at(2026, 11, 4, 10) + Duration::minutes(15));
        let changes = edit(&occ, Some(&master), &d, Scope::All, &mut zones);
        let Change::Replace(node) = &changes[0] else {
            panic!()
        };
        assert!(node
            .write()
            .contains("DTSTART;TZID=Europe/Paris:20261102T100000"));
    }

    #[test]
    fn removing_one_or_the_rest() {
        let (book, mut zones) = series();
        let occ = third(&book, &mut zones);
        let master = book.comp("s", "").unwrap();
        let one = remove(&occ, Some(&master), Scope::This, &mut zones);
        assert!(matches!(&one[0], Change::RemoveOne { rid, .. } if rid.value() == "20261104T090000"));
        let rest = remove(&occ, Some(&master), Scope::ThisAndFuture, &mut zones);
        assert!(matches!(&rest[0], Change::Replace(n) if n.text("RRULE").contains("UNTIL")));
        let all = remove(&occ, Some(&master), Scope::All, &mut zones);
        assert!(matches!(&all[0], Change::Remove { uid } if uid == "s"));
    }

    #[test]
    fn durations_write_compactly() {
        assert_eq!(duration(-900), "-PT15M");
        assert_eq!(duration(0), "PT0S");
        assert_eq!(duration(-86_400), "-P1D");
        assert_eq!(duration(-604_800), "-P1W");
        assert_eq!(duration(-5400), "-PT1H30M");
        assert_eq!(duration(-(86_400 + 3600)), "-P1DT1H");
    }
}
