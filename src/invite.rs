//! Meeting invitations: reading the `text/calendar` part a message carries,
//! and writing the answer back (#223).
//!
//! **What arrives.** A meeting request is an ordinary email with one extra
//! MIME part: `text/calendar; method=REQUEST`, holding an iCalendar
//! (RFC 5545) document with a `VEVENT` in it. The readable half of the
//! message is whatever the sender's calendar wrote for clients that cannot
//! read the part — Zimbra's is a table of fields, Outlook's a sentence — so
//! a client that ignores the part shows a description of a meeting and no
//! meeting. This module reads the part instead: what, when, where, who, and
//! whether the sender is asking for an answer.
//!
//! **What is read.** The first `VEVENT` outside a `VTIMEZONE`: `UID`,
//! `SEQUENCE`, `SUMMARY`, `LOCATION`, `DTSTART`/`DTEND` (or `DURATION`),
//! `ORGANIZER`, every `ATTENDEE` with its `PARTSTAT`, `RRULE` and
//! `RECURRENCE-ID`. Times are the awkward part: iCalendar writes them as
//! UTC (`…Z`), as a wall time in a named zone (`TZID=Europe/Paris`), as a
//! bare date for an all-day event, or as a wall time in no zone at all.
//! A named zone goes through GLib, which knows the zone database; a name
//! GLib does not know (Exchange writes Windows zone names like "W. Europe
//! Standard Time") falls back to the `VTIMEZONE` the same document carries,
//! whose `STANDARD`/`DAYLIGHT` blocks give the offsets and the rule for
//! which one is in force.
//!
//! **The answer.** RFC 5546 says an answer is the same event sent back with
//! `METHOD:REPLY`, carrying only the organizer, the event's identity and
//! *one* attendee — the person answering — with their `PARTSTAT` set. That
//! is what [`reply_ics`] writes; the app mails it to the organizer as the
//! `text/calendar` half of a two-part message, which is what Evolution,
//! Outlook and Google Calendar all send and all understand.
//!
//! Nothing here touches the network or the clock beyond formatting: the
//! part is read once, when the message's body is fetched, and rides with
//! the sender check into the cache (see [`crate::verify`]).

use gtk::glib;

use crate::models::{Invite, InvitePerson};

/// The largest calendar part that is read. An invitation is a few KB; a
/// published calendar export can be megabytes, and it would be stored
/// beside every sender check. Anything larger is left to the attachment
/// list, which is where a file that size belongs.
const MAX_ICS: usize = 256 * 1024;

/// Read the invitation a raw message carries, if it carries one.
pub fn detect_raw(raw: &[u8]) -> Option<Invite> {
    let parsed = mail_parser::MessageParser::default().parse(raw)?;
    detect(&parsed)
}

/// Read the invitation a parsed message carries, if it carries one.
pub fn detect(parsed: &mail_parser::Message) -> Option<Invite> {
    use mail_parser::MimeHeaders;
    for part in parsed.parts.iter() {
        let calendar = part
            .content_type()
            .is_some_and(|c| c.ctype().eq_ignore_ascii_case("text")
                && c.subtype().is_some_and(|s| s.eq_ignore_ascii_case("calendar")))
            || part
                .attachment_name()
                .is_some_and(|n| n.to_ascii_lowercase().ends_with(".ics"));
        if !calendar || part.contents().len() > MAX_ICS {
            continue;
        }
        // `text_contents` decodes the part's charset; a part typed as
        // something else (an `.ics` sent as an octet-stream) has none to
        // decode, and UTF-8 is what iCalendar is written in.
        let text = match part.text_contents() {
            Some(t) => t.to_string(),
            None => String::from_utf8_lossy(part.contents()).to_string(),
        };
        if let Some(mut invite) = parse(&text) {
            // The method lives on the part's own Content-Type as often as
            // in the document (`method=REQUEST`), and some senders write it
            // in only one of the two.
            if invite.method.is_empty() {
                invite.method = part
                    .content_type()
                    .and_then(|c| c.attribute("method"))
                    .unwrap_or("PUBLISH")
                    .to_ascii_uppercase();
            }
            return Some(invite);
        }
    }
    None
}

/// Read an iCalendar document into the one event it is about.
///
/// A document with no `VEVENT` (a free/busy reply, a bare `VTODO`) is not an
/// invitation and reads as nothing, so the reader shows the message as it
/// always did rather than an empty card.
pub fn parse(ics: &str) -> Option<Invite> {
    let lines = unfold(ics);
    let mut method = String::new();
    let mut zones: Vec<TimeZoneSpec> = Vec::new();
    let mut event: Option<EventProps> = None;
    // Which component each line belongs to, innermost last.
    let mut stack: Vec<String> = Vec::new();
    let mut current: Option<EventProps> = None;
    let mut zone: Option<TimeZoneSpec> = None;
    let mut observance: Option<Observance> = None;

    for line in &lines {
        let name = line.name.to_ascii_uppercase();
        match name.as_str() {
            "BEGIN" => {
                let component = line.value.to_ascii_uppercase();
                match component.as_str() {
                    "VEVENT" if event.is_none() && !in_timezone(&stack) => {
                        current = Some(EventProps::default());
                    }
                    "VTIMEZONE" => zone = Some(TimeZoneSpec::default()),
                    "STANDARD" | "DAYLIGHT" => observance = Some(Observance::default()),
                    _ => {}
                }
                stack.push(component);
                continue;
            }
            "END" => {
                let component = line.value.to_ascii_uppercase();
                match component.as_str() {
                    "VEVENT" => {
                        if let Some(props) = current.take() {
                            event = Some(props);
                        }
                    }
                    "VTIMEZONE" => {
                        if let Some(z) = zone.take() {
                            zones.push(z);
                        }
                    }
                    "STANDARD" | "DAYLIGHT" => {
                        if let (Some(o), Some(z)) = (observance.take(), zone.as_mut()) {
                            z.observances.push(o);
                        }
                    }
                    _ => {}
                }
                stack.pop();
                continue;
            }
            "METHOD" if stack.last().is_some_and(|c| c == "VCALENDAR") => {
                method = line.value.to_ascii_uppercase();
                continue;
            }
            _ => {}
        }
        // An alarm's own fields (its TRIGGER, its DESCRIPTION "Reminder")
        // are not the event's.
        if stack.last().is_some_and(|c| c == "VALARM") {
            continue;
        }
        if let Some(o) = observance.as_mut() {
            o.take(line);
            continue;
        }
        if let Some(z) = zone.as_mut() {
            if name == "TZID" {
                z.id = line.value.clone();
            }
            continue;
        }
        if let Some(props) = current.as_mut() {
            props.take(line);
        }
    }

    let props = event?;
    let start = props.start.as_ref().and_then(|s| s.resolve(&zones));
    let end = props
        .end
        .as_ref()
        .and_then(|e| e.resolve(&zones))
        .or_else(|| Some(start? + props.duration?))
        // An event with a start and no end runs to the end of the day when
        // it is all-day, and is a point in time otherwise — which is what
        // iCalendar means by a missing DTEND.
        .or(start);
    Some(Invite {
        method,
        uid: props.uid,
        sequence: props.sequence,
        summary: props.summary,
        location: props.location,
        start: start.unwrap_or_default(),
        end: end.unwrap_or_default(),
        all_day: props.start.as_ref().is_some_and(|s| s.date_only),
        recurrence_id: props.recurrence_id,
        repeats: props.rrule,
        organizer: props.organizer,
        attendees: props.attendees,
        cancelled: props.status.eq_ignore_ascii_case("CANCELLED"),
        ics: ics.to_string(),
    })
}

/// Whether the component being read sits inside a `VTIMEZONE`, whose
/// `DTSTART` and `RRULE` describe a clock change rather than a meeting.
fn in_timezone(stack: &[String]) -> bool {
    stack.iter().any(|c| c == "VTIMEZONE")
}

// ---------------------------------------------------------------------------
// The document
// ---------------------------------------------------------------------------

/// One unfolded content line: `NAME;PARAM=VALUE:value`.
#[derive(Debug, Default)]
struct Line {
    name: String,
    params: Vec<(String, String)>,
    /// The value with iCalendar's text escapes undone (`\n`, `\,`, `\;`).
    value: String,
    /// The value exactly as written, for the fields that are not text
    /// (a date, an address) and must not be un-escaped.
    raw: String,
}

impl Line {
    fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Split a document into content lines, joining the continuations.
///
/// RFC 5545 folds any line longer than 75 octets by breaking it and starting
/// the next with a single space or tab, which is *not* part of the value —
/// a long description or a URL is routinely split mid-word.
fn unfold(ics: &str) -> Vec<Line> {
    let mut joined: Vec<String> = Vec::new();
    for raw in ics.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        match line.strip_prefix([' ', '\t']) {
            Some(rest) => {
                if let Some(last) = joined.last_mut() {
                    last.push_str(rest);
                    continue;
                }
                joined.push(rest.to_string());
            }
            None => joined.push(line.to_string()),
        }
    }
    joined.iter().filter_map(|l| split_line(l)).collect()
}

/// Read one content line. The name and its parameters end at the first colon
/// that is not inside a quoted parameter value — `TZID="Europe/Paris"` and
/// `mailto:someone@example.org` both put colons where a naive split breaks.
fn split_line(line: &str) -> Option<Line> {
    if line.trim().is_empty() {
        return None;
    }
    let mut quoted = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let (head, value) = match colon {
        Some(i) => (&line[..i], &line[i + 1..]),
        None => (line, ""),
    };
    let mut parts = split_unquoted(head, ';');
    let name = parts.next()?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let params = parts
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((k.trim().to_string(), unquote(v.trim()).to_string()))
        })
        .collect();
    Some(Line {
        name,
        params,
        value: unescape(value),
        raw: value.to_string(),
    })
}

/// Split on a separator that is outside double quotes.
fn split_unquoted(text: &str, sep: char) -> impl Iterator<Item = &str> {
    let mut quoted = false;
    let mut bounds = vec![0usize];
    for (i, c) in text.char_indices() {
        match c {
            '"' => quoted = !quoted,
            c if c == sep && !quoted => bounds.push(i),
            _ => {}
        }
    }
    bounds.push(text.len());
    let mut out = Vec::new();
    for pair in bounds.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let piece = &text[a..b];
        out.push(piece.strip_prefix(sep).unwrap_or(piece));
    }
    out.into_iter()
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
}

/// Undo iCalendar's text escapes: `\n` (and `\N`) is a line break, and a
/// comma, semicolon or backslash in a value arrives with a backslash before
/// it.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') | Some('N') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Write a value back out with those escapes in place.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// Fold a content line at 75 octets, as RFC 5545 asks, breaking only
/// between characters so a multi-byte one is never split.
fn fold(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + line.len() / 70);
    let mut width = 0;
    for c in line.chars() {
        let n = c.len_utf8();
        if width + n > 73 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += n;
    }
    out
}

// ---------------------------------------------------------------------------
// The event
// ---------------------------------------------------------------------------

/// The fields of a `VEVENT`, as read; the dates still unresolved because a
/// `TZID` may name a zone the document defines further down.
#[derive(Debug, Default)]
struct EventProps {
    uid: String,
    sequence: i64,
    summary: String,
    location: String,
    status: String,
    recurrence_id: String,
    rrule: String,
    start: Option<Stamp>,
    end: Option<Stamp>,
    duration: Option<i64>,
    organizer: Option<InvitePerson>,
    attendees: Vec<InvitePerson>,
}

impl EventProps {
    fn take(&mut self, line: &Line) {
        match line.name.to_ascii_uppercase().as_str() {
            "UID" => self.uid = line.value.clone(),
            "SEQUENCE" => self.sequence = line.value.trim().parse().unwrap_or_default(),
            "SUMMARY" => self.summary = line.value.trim().to_string(),
            "LOCATION" => self.location = line.value.trim().to_string(),
            "STATUS" => self.status = line.value.trim().to_string(),
            "RECURRENCE-ID" => self.recurrence_id = line.raw.trim().to_string(),
            "RRULE" => self.rrule = line.raw.trim().to_ascii_uppercase(),
            "DTSTART" => self.start = Stamp::read(line),
            "DTEND" => self.end = Stamp::read(line),
            "DURATION" => self.duration = duration_seconds(line.raw.trim()),
            "ORGANIZER" => self.organizer = person(line),
            "ATTENDEE" => {
                if let Some(p) = person(line) {
                    self.attendees.push(p);
                }
            }
            _ => {}
        }
    }
}

/// One `ORGANIZER` or `ATTENDEE` line: the address it points at and the
/// parameters that say who they are and where they stand.
fn person(line: &Line) -> Option<InvitePerson> {
    let email = line
        .raw
        .trim()
        .strip_prefix("mailto:")
        .or_else(|| line.raw.trim().strip_prefix("MAILTO:"))
        .unwrap_or(line.raw.trim())
        .trim()
        .to_string();
    let name = line.param("CN").unwrap_or_default().trim().to_string();
    if email.is_empty() && name.is_empty() {
        return None;
    }
    Some(InvitePerson {
        // A CN that is just the address again adds nothing.
        name: if name.eq_ignore_ascii_case(&email) { String::new() } else { name },
        email,
        status: line.param("PARTSTAT").unwrap_or("NEEDS-ACTION").to_ascii_uppercase(),
        // RFC 5545 defaults RSVP to FALSE; an organizer who wants an answer
        // says so.
        rsvp: line.param("RSVP").is_some_and(|v| v.eq_ignore_ascii_case("TRUE")),
        // A resource (a room, a projector) is on the attendee list but is
        // not a person, and listing it as one reads oddly.
        resource: line
            .param("CUTYPE")
            .is_some_and(|v| v.eq_ignore_ascii_case("RESOURCE") || v.eq_ignore_ascii_case("ROOM")),
    })
}

/// A date-time as written, before the zone it names has been found.
#[derive(Debug)]
struct Stamp {
    /// Calendar fields, as written on the wire.
    y: i32,
    mo: i32,
    d: i32,
    h: i32,
    mi: i32,
    s: i32,
    /// `…Z`: the value is UTC and names no zone.
    utc: bool,
    /// `VALUE=DATE`: an all-day event, with no time of day at all.
    date_only: bool,
    /// The `TZID` parameter, empty for a floating time.
    tzid: String,
}

impl Stamp {
    fn read(line: &Line) -> Option<Stamp> {
        let mut stamp = Stamp::parse(line.raw.trim())?;
        stamp.tzid = line.param("TZID").unwrap_or_default().trim().to_string();
        if line.param("VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE")) {
            stamp.date_only = true;
        }
        Some(stamp)
    }

    /// `20261102`, `20261102T090000` or `20261102T080000Z`.
    fn parse(text: &str) -> Option<Stamp> {
        let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() < 8 {
            return None;
        }
        let num = |a: usize, b: usize| digits.get(a..b)?.parse::<i32>().ok();
        let mut stamp = Stamp {
            y: num(0, 4)?,
            mo: num(4, 6)?,
            d: num(6, 8)?,
            h: 0,
            mi: 0,
            s: 0,
            utc: text.ends_with('Z') || text.ends_with('z'),
            date_only: digits.len() == 8,
            tzid: String::new(),
        };
        if digits.len() >= 14 {
            stamp.h = num(8, 10)?;
            stamp.mi = num(10, 12)?;
            stamp.s = num(12, 14)?;
        }
        Some(stamp)
    }

    /// The moment this stands for, in unix seconds.
    fn resolve(&self, zones: &[TimeZoneSpec]) -> Option<i64> {
        let tz = if self.utc {
            glib::TimeZone::utc()
        } else if self.tzid.is_empty() {
            // A floating time (and an all-day date) means "the same clock
            // reading wherever you are", which is the local one.
            glib::TimeZone::local()
        } else {
            self.zone(zones)?
        };
        glib::DateTime::new(&tz, self.y, self.mo, self.d, self.h, self.mi, self.s as f64)
            .ok()
            .map(|d| d.to_unix())
    }

    /// The zone this names: the system's database first, then the
    /// `VTIMEZONE` the document carries — which is the only hope for the
    /// Windows zone names Exchange writes ("W. Europe Standard Time").
    fn zone(&self, zones: &[TimeZoneSpec]) -> Option<glib::TimeZone> {
        if let Some(tz) = glib::TimeZone::from_identifier(Some(&self.tzid)) {
            return Some(tz);
        }
        let spec = zones.iter().find(|z| z.id.eq_ignore_ascii_case(&self.tzid))?;
        let offset = spec.offset_at(self)?;
        let (sign, mins) = if offset < 0 { ('-', -offset / 60) } else { ('+', offset / 60) };
        glib::TimeZone::from_identifier(Some(&format!("{sign}{:02}:{:02}", mins / 60, mins % 60)))
    }
}

/// `PT1H30M`, `P1D`, `-PT15M`: a length of time, in seconds.
fn duration_seconds(text: &str) -> Option<i64> {
    let (sign, rest) = match text.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, text.strip_prefix('+').unwrap_or(text)),
    };
    let rest = rest.strip_prefix('P')?;
    let mut total: i64 = 0;
    let mut number = String::new();
    for c in rest.chars() {
        match c {
            'T' => continue,
            c if c.is_ascii_digit() => number.push(c),
            unit => {
                let n: i64 = number.parse().ok()?;
                number.clear();
                total += n * match unit {
                    'W' => 604_800,
                    'D' => 86_400,
                    'H' => 3_600,
                    'M' => 60,
                    'S' => 1,
                    _ => return None,
                };
            }
        }
    }
    Some(sign * total)
}

// ---------------------------------------------------------------------------
// VTIMEZONE
// ---------------------------------------------------------------------------

/// A zone the document defines itself, for when its name means nothing to
/// the system.
#[derive(Debug, Default)]
struct TimeZoneSpec {
    id: String,
    observances: Vec<Observance>,
}

/// One `STANDARD` or `DAYLIGHT` block: the offset it puts the clock on, and
/// (through its yearly rule) the day it starts.
#[derive(Debug, Default)]
struct Observance {
    /// Seconds east of UTC once this is in force.
    offset_to: i32,
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
    fn take(&mut self, line: &Line) {
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
                    let Some((k, v)) = part.split_once('=') else { continue };
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
    fn offset_at(&self, when: &Stamp) -> Option<i32> {
        if self.observances.len() == 1 {
            return Some(self.observances[0].offset_to);
        }
        let key = |mo: i32, d: i32, at: i32| (mo as i64) * 100_000_000 + (d as i64) * 100_000 + at as i64;
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
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 30,
    }
}

/// Day of the week (1 = Monday … 7 = Sunday), by Zeller's congruence.
fn weekday_of(year: i32, month: i32, day: i32) -> i32 {
    let (m, y) = if month < 3 { (month + 12, year - 1) } else { (month, year) };
    let k = y % 100;
    let j = y / 100;
    let h = (day + (13 * (m + 1)) / 5 + k + k / 4 + j / 4 + 5 * j) % 7;
    // Zeller counts Saturday as 0.
    ((h + 5) % 7) + 1
}

// ---------------------------------------------------------------------------
// The answer
// ---------------------------------------------------------------------------

/// The iCalendar document that answers an invitation (RFC 5546 §3.2.3): the
/// same event, `METHOD:REPLY`, with the organizer and exactly one attendee —
/// the person answering, with their new `PARTSTAT`.
///
/// `SEQUENCE` and `RECURRENCE-ID` are carried over unchanged: they are how
/// the organizer's calendar knows which version of which occurrence is being
/// answered. Without them a reply to an updated meeting is applied to the
/// original, or to every occurrence of a series.
pub fn reply_ics(invite: &Invite, name: &str, email: &str, status: &str) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&fold(&text));
        out.push_str("\r\n");
    };
    line("BEGIN:VCALENDAR".into());
    line("PRODID:-//Hyprlab//Hylki//EN".into());
    line("VERSION:2.0".into());
    line("METHOD:REPLY".into());
    line("BEGIN:VEVENT".into());
    line(format!("UID:{}", escape(&invite.uid)));
    line(format!("SEQUENCE:{}", invite.sequence));
    line(format!("DTSTAMP:{}", stamp_utc(crate::datefmt::now())));
    if !invite.recurrence_id.is_empty() {
        line(format!("RECURRENCE-ID:{}", invite.recurrence_id));
    }
    if !invite.summary.is_empty() {
        line(format!("SUMMARY:{}", escape(&invite.summary)));
    }
    // The organizer's calendar matches the reply on UID, but a reply with
    // no ORGANIZER is rejected outright by Exchange.
    if let Some(o) = &invite.organizer {
        line(format!("ORGANIZER{}:mailto:{}", cn(&o.name), o.email));
    }
    line(format!(
        "ATTENDEE{}PARTSTAT={status}:mailto:{email}",
        match cn(name).as_str() {
            "" => ";".to_string(),
            c => format!("{c};"),
        }
    ));
    line("END:VEVENT".into());
    line("END:VCALENDAR".into());
    out
}

/// `;CN="Ada Lovelace"`, or nothing for someone with no name on file.
fn cn(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return String::new();
    }
    format!(";CN=\"{}\"", name.replace(['"', '\\'], ""))
}

/// A unix time as iCalendar UTC: `20261102T080000Z`.
fn stamp_utc(ts: i64) -> String {
    glib::DateTime::from_unix_utc(ts)
        .ok()
        .and_then(|d| d.format("%Y%m%dT%H%M%SZ").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZIMBRA: &str = "BEGIN:VCALENDAR\r\n\
        PRODID:Zimbra-Calendar-Provider\r\n\
        VERSION:2.0\r\n\
        METHOD:REQUEST\r\n\
        BEGIN:VTIMEZONE\r\n\
        TZID:Europe/Paris\r\n\
        BEGIN:STANDARD\r\n\
        DTSTART:16010101T030000\r\n\
        TZOFFSETTO:+0100\r\n\
        RRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\n\
        END:STANDARD\r\n\
        BEGIN:DAYLIGHT\r\n\
        DTSTART:16010101T020000\r\n\
        TZOFFSETTO:+0200\r\n\
        RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\n\
        END:DAYLIGHT\r\n\
        END:VTIMEZONE\r\n\
        BEGIN:VEVENT\r\n\
        UID:26a4ff0c-1b6f\r\n\
        SUMMARY:point espaces verts\r\n\
        LOCATION:Salle B\\, 2e étage\r\n\
        DTSTART;TZID=\"Europe/Paris\":20261102T090000\r\n\
        DTEND;TZID=\"Europe/Paris\":20261102T100000\r\n\
        ORGANIZER;CN=Chloé Mercier:mailto:chloe@example.org\r\n\
        ATTENDEE;CN=Emmanuel P;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:me@example.com\r\n\
        ATTENDEE;CUTYPE=RESOURCE;CN=Salle B:mailto:salleb@example.org\r\n\
        BEGIN:VALARM\r\n\
        ACTION:DISPLAY\r\n\
        DESCRIPTION:Reminder\r\n\
        TRIGGER;RELATED=START:-PT15M\r\n\
        END:VALARM\r\n\
        END:VEVENT\r\n\
        END:VCALENDAR\r\n";

    #[test]
    fn a_zimbra_request_reads_whole() {
        let inv = parse(ZIMBRA).expect("an invitation");
        assert_eq!(inv.method, "REQUEST");
        assert_eq!(inv.uid, "26a4ff0c-1b6f");
        assert_eq!(inv.summary, "point espaces verts");
        // The escaped comma comes back as a comma.
        assert_eq!(inv.location, "Salle B, 2e étage");
        // 09:00 Paris in November is 08:00 UTC.
        assert_eq!(inv.start, 1_793_606_400);
        assert_eq!(inv.end, inv.start + 3600);
        assert!(!inv.all_day);
        assert_eq!(inv.organizer.as_ref().map(|o| o.email.as_str()), Some("chloe@example.org"));
        assert_eq!(inv.attendees.len(), 2);
        assert!(inv.attendees[0].rsvp);
        assert_eq!(inv.attendees[0].status, "NEEDS-ACTION");
        // The room is on the list but is not a person.
        assert!(inv.attendees[1].resource);
        assert_eq!(inv.guests().count(), 1, "one person invited, one room");
        assert!(!inv.cancelled);
    }

    #[test]
    fn an_alarms_own_fields_are_not_the_events() {
        let inv = parse(ZIMBRA).expect("an invitation");
        // The VALARM carries a DESCRIPTION and a TRIGGER; neither belongs to
        // the meeting, and its DTSTART-less TRIGGER must not become a time.
        assert_eq!(inv.summary, "point espaces verts");
        assert_eq!(inv.end - inv.start, 3600);
    }

    #[test]
    fn a_windows_zone_falls_back_to_the_documents_own_rules() {
        // Exchange names its zones in Windows' terms, which GLib has never
        // heard of — the VTIMEZONE beside it is the only definition there is.
        let ics = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\n\
            BEGIN:VTIMEZONE\r\nTZID:W. Europe Standard Time\r\n\
            BEGIN:STANDARD\r\nDTSTART:16010101T030000\r\nTZOFFSETTO:+0100\r\n\
            RRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\n\
            BEGIN:DAYLIGHT\r\nDTSTART:16010101T020000\r\nTZOFFSETTO:+0200\r\n\
            RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\nEND:DAYLIGHT\r\n\
            END:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:x\r\nSUMMARY:Sync\r\n\
            DTSTART;TZID=\"W. Europe Standard Time\":20261102T090000\r\n\
            DTEND;TZID=\"W. Europe Standard Time\":20261102T100000\r\n\
            END:VEVENT\r\nEND:VCALENDAR\r\n";
        let inv = parse(ics).expect("an invitation");
        assert_eq!(inv.start, 1_793_606_400);
        // In July the same zone is an hour further east.
        let summer = ics.replace("20261102", "20260702");
        let inv = parse(&summer).expect("an invitation");
        let dt = glib::DateTime::from_unix_utc(inv.start).unwrap();
        assert_eq!(dt.format("%H:%M").unwrap().as_str(), "07:00");
    }

    #[test]
    fn utc_and_all_day_and_duration_all_read() {
        let ics = "BEGIN:VCALENDAR\r\nMETHOD:PUBLISH\r\nBEGIN:VEVENT\r\nUID:u\r\n\
            DTSTART:20261102T080000Z\r\nDURATION:PT1H30M\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let inv = parse(ics).expect("an event");
        assert_eq!(inv.start, 1_793_606_400);
        assert_eq!(inv.end - inv.start, 5400);
        assert!(!inv.all_day);

        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:u\r\n\
            DTSTART;VALUE=DATE:20261102\r\nDTEND;VALUE=DATE:20261103\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let inv = parse(ics).expect("an event");
        assert!(inv.all_day);
        assert!(inv.end > inv.start);
    }

    #[test]
    fn folded_lines_are_joined_before_they_are_read() {
        // The space that starts a continuation is the fold marker, not part
        // of the value: a name broken mid-word comes back whole, and one
        // broken after a space keeps only the space it was written with.
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:u\r\nSUMMARY:A rather lo\r\n ng meeting name\r\nDTSTART:20261102T080000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        assert_eq!(parse(ics).unwrap().summary, "A rather long meeting name");
        let tabbed = ics.replace("\r\n ng", "\r\n\tng");
        assert_eq!(parse(&tabbed).unwrap().summary, "A rather long meeting name");
    }

    #[test]
    fn a_cancellation_says_so() {
        let ics = "BEGIN:VCALENDAR\r\nMETHOD:CANCEL\r\nBEGIN:VEVENT\r\nUID:u\r\n\
            STATUS:CANCELLED\r\nSUMMARY:Standup\r\nDTSTART:20261102T080000Z\r\n\
            END:VEVENT\r\nEND:VCALENDAR\r\n";
        let inv = parse(ics).expect("an event");
        assert_eq!(inv.method, "CANCEL");
        assert!(inv.cancelled);
    }

    #[test]
    fn a_document_with_no_event_is_not_an_invitation() {
        let ics = "BEGIN:VCALENDAR\r\nMETHOD:REPLY\r\nBEGIN:VFREEBUSY\r\nUID:u\r\n\
            END:VFREEBUSY\r\nEND:VCALENDAR\r\n";
        assert!(parse(ics).is_none());
    }

    #[test]
    fn the_calendar_part_is_found_in_the_message() {
        let raw = format!(
            "From: Chloé <chloe@example.org>\r\nTo: me@example.com\r\n\
             Subject: Invitation\r\nMIME-Version: 1.0\r\n\
             Content-Type: multipart/alternative; boundary=\"b\"\r\n\r\n\
             --b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nNouvelle demande\r\n\
             --b\r\nContent-Type: text/calendar; charset=utf-8; method=REQUEST; name=meeting.ics\r\n\r\n\
             {ZIMBRA}\r\n--b--\r\n"
        );
        let inv = detect_raw(raw.as_bytes()).expect("an invitation");
        assert_eq!(inv.summary, "point espaces verts");
        assert_eq!(inv.method, "REQUEST");
    }

    #[test]
    fn a_message_with_no_calendar_part_reads_as_nothing() {
        let raw = b"From: a@b.c\r\nSubject: Hi\r\n\r\nNo meeting here.\r\n";
        assert!(detect_raw(raw).is_none());
    }

    #[test]
    fn the_reply_carries_the_events_identity_and_one_attendee() {
        let inv = parse(ZIMBRA).expect("an invitation");
        let ics = reply_ics(&inv, "Emmanuel P", "me@example.com", "ACCEPTED");
        assert!(ics.contains("METHOD:REPLY"));
        assert!(ics.contains("UID:26a4ff0c-1b6f"));
        assert!(ics.contains("SEQUENCE:0"));
        assert!(ics.contains("ORGANIZER;CN=\"Chloé Mercier\":mailto:chloe@example.org"));
        assert!(ics.contains("ATTENDEE;CN=\"Emmanuel P\";PARTSTAT=ACCEPTED:mailto:me@example.com"));
        // Exactly one attendee: the room and the other guests are not
        // answering.
        assert_eq!(ics.matches("ATTENDEE").count(), 1);
        assert!(ics.ends_with("END:VCALENDAR\r\n"));
    }

    #[test]
    fn a_reply_to_one_occurrence_says_which() {
        let ics = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:u\r\nSEQUENCE:3\r\n\
            RECURRENCE-ID;TZID=Europe/Paris:20261102T090000\r\nSUMMARY:Standup\r\n\
            DTSTART:20261102T080000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let inv = parse(ics).expect("an invitation");
        let reply = reply_ics(&inv, "", "me@example.com", "DECLINED");
        assert!(reply.contains("SEQUENCE:3"));
        assert!(reply.contains("RECURRENCE-ID:20261102T090000"));
        assert!(reply.contains("ATTENDEE;PARTSTAT=DECLINED:mailto:me@example.com"));
    }

    #[test]
    fn long_lines_are_folded_on_the_way_out() {
        let ics = format!(
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:{}\r\nDTSTART:20261102T080000Z\r\n\
             END:VEVENT\r\nEND:VCALENDAR\r\n",
            "x".repeat(200)
        );
        let inv = parse(&ics).expect("an event");
        let reply = reply_ics(&inv, "", "me@example.com", "TENTATIVE");
        assert!(reply.lines().all(|l| l.len() <= 75));
        // And folds back to the same value.
        assert_eq!(parse(&reply).unwrap().uid.len(), 200);
    }

    #[test]
    fn a_length_of_time_reads_in_every_unit() {
        assert_eq!(duration_seconds("PT1H"), Some(3600));
        assert_eq!(duration_seconds("P1DT2H30M"), Some(95_400));
        assert_eq!(duration_seconds("-PT15M"), Some(-900));
        assert_eq!(duration_seconds("P2W"), Some(1_209_600));
        assert_eq!(duration_seconds("nonsense"), None);
    }

    #[test]
    fn the_last_sunday_of_a_month_is_found() {
        // 25 October 2026 is the last Sunday of that month.
        let o = Observance { month: 10, ordinal: -1, weekday: 7, ..Observance::default() };
        assert_eq!(o.day_in(2026), Some(25));
        // And the second Sunday of March 2026 is the 8th.
        let o = Observance { month: 3, ordinal: 2, weekday: 7, ..Observance::default() };
        assert_eq!(o.day_in(2026), Some(8));
    }
}
