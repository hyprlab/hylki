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
//! the sender check into the cache (see [`crate::mail::verify`]).

use gtk::glib;

use crate::models::{Invite, InvitePerson};

mod ics;
mod reply;
mod tz;

pub(crate) use ics::fold;
use ics::{unfold, Line};
pub(crate) use reply::reply_ics;
use tz::{Observance, TimeZoneSpec};

/// The largest calendar part that is read. An invitation is a few KB; a
/// published calendar export can be megabytes, and it would be stored
/// beside every sender check. Anything larger is left to the attachment
/// list, which is where a file that size belongs.
const MAX_ICS: usize = 256 * 1024;

/// Read the invitation a raw message carries, if it carries one.
pub(crate) fn detect_raw(raw: &[u8]) -> Option<Invite> {
    let parsed = mail_parser::MessageParser::default().parse(raw)?;
    detect(&parsed)
}

/// Read the invitation a parsed message carries, if it carries one.
pub(crate) fn detect(parsed: &mail_parser::Message) -> Option<Invite> {
    use mail_parser::MimeHeaders;
    for part in parsed.parts.iter() {
        let calendar = part.content_type().is_some_and(|c| {
            c.ctype().eq_ignore_ascii_case("text")
                && c.subtype()
                    .is_some_and(|s| s.eq_ignore_ascii_case("calendar"))
        }) || part
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
fn parse(ics: &str) -> Option<Invite> {
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
        name: if name.eq_ignore_ascii_case(&email) {
            String::new()
        } else {
            name
        },
        email,
        status: line
            .param("PARTSTAT")
            .unwrap_or("NEEDS-ACTION")
            .to_ascii_uppercase(),
        // RFC 5545 defaults RSVP to FALSE; an organizer who wants an answer
        // says so.
        rsvp: line
            .param("RSVP")
            .is_some_and(|v| v.eq_ignore_ascii_case("TRUE")),
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
        if line
            .param("VALUE")
            .is_some_and(|v| v.eq_ignore_ascii_case("DATE"))
        {
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
        let spec = zones
            .iter()
            .find(|z| z.id.eq_ignore_ascii_case(&self.tzid))?;
        let offset = spec.offset_at(self)?;
        let (sign, mins) = if offset < 0 {
            ('-', -offset / 60)
        } else {
            ('+', offset / 60)
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) const ZIMBRA: &str = "BEGIN:VCALENDAR\r\n\
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
        assert_eq!(
            inv.organizer.as_ref().map(|o| o.email.as_str()),
            Some("chloe@example.org")
        );
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
        assert_eq!(
            parse(&tabbed).unwrap().summary,
            "A rather long meeting name"
        );
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
    fn a_length_of_time_reads_in_every_unit() {
        assert_eq!(duration_seconds("PT1H"), Some(3600));
        assert_eq!(duration_seconds("P1DT2H30M"), Some(95_400));
        assert_eq!(duration_seconds("-PT15M"), Some(-900));
        assert_eq!(duration_seconds("P2W"), Some(1_209_600));
        assert_eq!(duration_seconds("nonsense"), None);
    }
}
