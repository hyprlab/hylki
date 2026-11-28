//! Dates and times as iCalendar writes them, and the zones they name.
//!
//! A recurring event repeats on its own wall clock: a weekly 9:00 meeting in
//! New York stays at 9:00 there across the change to daylight saving time,
//! which moves it by an hour in UTC. So a series is expanded in the wall
//! time of its `DTSTART` zone, and only each occurrence is turned into an
//! instant. This module holds both halves: the wall-clock value as written
//! ([`Stamp`]) and the zone lookups that turn it into Unix seconds and back
//! ([`Zones`]).

use std::collections::HashMap;

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use gtk::glib;

use super::tree::Node;
use crate::mail::invite::ics::Line;
use crate::mail::invite::tz::{Observance, TimeZoneSpec};
use crate::mail::invite::Stamp as InviteStamp;

/// A `DTSTART`, `DTEND`, `DUE`, `RECURRENCE-ID` or `EXDATE` value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Stamp {
    /// `VALUE=DATE`: a whole day.
    Date(NaiveDate),
    /// No zone: the same clock reading wherever the reader is.
    Floating(NaiveDateTime),
    /// `…Z`.
    Utc(NaiveDateTime),
    /// `TZID=…`: a wall time in a named zone.
    Zoned(NaiveDateTime, String),
}

impl Stamp {
    /// Read a property line's value.
    pub(crate) fn read(line: &Line) -> Option<Stamp> {
        let raw = line.raw.trim();
        // EXDATE and RDATE may carry several values; the first is this one.
        let raw = raw.split(',').next().unwrap_or(raw);
        let date_param = line
            .param("VALUE")
            .is_some_and(|v| v.eq_ignore_ascii_case("DATE"));
        Stamp::parse(raw, line.param("TZID"), date_param)
    }

    /// Every value of a property that may list several (`EXDATE`, `RDATE`).
    pub(crate) fn read_list(line: &Line) -> Vec<Stamp> {
        let date_param = line
            .param("VALUE")
            .is_some_and(|v| v.eq_ignore_ascii_case("DATE"));
        // RDATE;VALUE=PERIOD lists start/end pairs; the start is the date.
        line.raw
            .split(',')
            .filter_map(|v| {
                let v = v.split('/').next().unwrap_or(v).trim();
                Stamp::parse(v, line.param("TZID"), date_param)
            })
            .collect()
    }

    /// `20261102`, `20261102T090000`, `20261102T080000Z`.
    pub(crate) fn parse(raw: &str, tzid: Option<&str>, date_param: bool) -> Option<Stamp> {
        let raw = raw.trim();
        let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() < 8 {
            return None;
        }
        let num = |a: usize, b: usize| digits.get(a..b)?.parse::<u32>().ok();
        let date = NaiveDate::from_ymd_opt(num(0, 4)? as i32, num(4, 6)?, num(6, 8)?)?;
        if date_param || digits.len() < 14 {
            return Some(Stamp::Date(date));
        }
        let time = NaiveTime::from_hms_opt(num(8, 10)?, num(10, 12)?, num(12, 14)?.min(59))?;
        let wall = date.and_time(time);
        if raw.ends_with(['Z', 'z']) {
            return Some(Stamp::Utc(wall));
        }
        match tzid.map(str::trim).filter(|t| !t.is_empty()) {
            Some(tz) => Some(Stamp::Zoned(wall, tz.to_string())),
            None => Some(Stamp::Floating(wall)),
        }
    }

    pub(crate) fn is_date(&self) -> bool {
        matches!(self, Stamp::Date(_))
    }

    /// The wall-clock reading: midnight for a date.
    pub(crate) fn wall(&self) -> NaiveDateTime {
        match self {
            Stamp::Date(d) => d.and_time(NaiveTime::MIN),
            Stamp::Floating(w) | Stamp::Utc(w) | Stamp::Zoned(w, _) => *w,
        }
    }

    pub(crate) fn date(&self) -> NaiveDate {
        self.wall().date()
    }

    /// The same kind of value at another wall-clock reading.
    pub(crate) fn with_wall(&self, wall: NaiveDateTime) -> Stamp {
        match self {
            Stamp::Date(_) => Stamp::Date(wall.date()),
            Stamp::Floating(_) => Stamp::Floating(wall),
            Stamp::Utc(_) => Stamp::Utc(wall),
            Stamp::Zoned(_, tz) => Stamp::Zoned(wall, tz.clone()),
        }
    }

    /// The value alone: `20261102T090000`, with `Z` for UTC. What EDS takes
    /// as an occurrence's id.
    pub(crate) fn value(&self) -> String {
        match self {
            Stamp::Date(d) => d.format("%Y%m%d").to_string(),
            Stamp::Floating(w) | Stamp::Zoned(w, _) => w.format("%Y%m%dT%H%M%S").to_string(),
            Stamp::Utc(w) => w.format("%Y%m%dT%H%M%SZ").to_string(),
        }
    }

    /// The whole content line: `DTSTART;TZID=Europe/Paris:20261102T090000`.
    pub(crate) fn line(&self, name: &str) -> String {
        match self {
            Stamp::Date(_) => format!("{name};VALUE=DATE:{}", self.value()),
            Stamp::Zoned(_, tz) => format!("{name};TZID={}:{}", quote_param(tz), self.value()),
            _ => format!("{name}:{}", self.value()),
        }
    }

    /// The zone named, if any.
    pub(crate) fn tzid(&self) -> Option<&str> {
        match self {
            Stamp::Zoned(_, tz) => Some(tz),
            _ => None,
        }
    }
}

/// A parameter value that holds a `:`, `;` or `,` goes in quotes.
fn quote_param(v: &str) -> String {
    if v.contains([':', ';', ',']) {
        format!("\"{}\"", v.replace('"', ""))
    } else {
        v.to_string()
    }
}

/// How a zone is known.
#[derive(Clone)]
enum Zone {
    Glib(glib::TimeZone),
    /// Only from a `VTIMEZONE`, for names the system lacks.
    Spec(std::sync::Arc<TimeZoneSpec>),
}

/// The zones a set of events name, looked up once each.
#[derive(Default, Clone)]
pub(crate) struct Zones {
    known: HashMap<String, Option<Zone>>,
    /// `VTIMEZONE`s handed over by a calendar or a document, by TZID.
    specs: HashMap<String, std::sync::Arc<TimeZoneSpec>>,
}

impl std::fmt::Debug for Zones {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Zones")
            .field("known", &self.known.keys().collect::<Vec<_>>())
            .field("specs", &self.specs.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Zones {
    /// Learn the zone a `VTIMEZONE` component defines.
    pub(crate) fn learn(&mut self, node: &Node) {
        if node.name != "VTIMEZONE" {
            return;
        }
        let mut spec = TimeZoneSpec {
            id: node.text("TZID"),
            ..TimeZoneSpec::default()
        };
        for child in &node.children {
            if child.name == "STANDARD" || child.name == "DAYLIGHT" {
                let mut o = Observance::default();
                for line in child.props() {
                    o.take(&line);
                }
                spec.observances.push(o);
            }
        }
        if spec.id.is_empty() || spec.observances.is_empty() {
            return;
        }
        let id = spec.id.clone();
        self.known.remove(&id);
        self.specs.insert(id, std::sync::Arc::new(spec));
    }

    /// Whether the system knows `tzid`, or a `VTIMEZONE` for it was learned.
    pub(crate) fn knows(&mut self, tzid: &str) -> bool {
        self.zone(tzid).is_some()
    }

    fn zone(&mut self, tzid: &str) -> Option<Zone> {
        if let Some(z) = self.known.get(tzid) {
            return z.clone();
        }
        let found = system_zone(tzid)
            .map(Zone::Glib)
            .or_else(|| self.specs.get(tzid).cloned().map(Zone::Spec));
        self.known.insert(tzid.to_string(), found.clone());
        found
    }

    /// The instant `stamp` stands for. A date is its local midnight; a zone
    /// nobody knows is read as local time, which is right far more often
    /// than UTC would be.
    pub(crate) fn unix(&mut self, stamp: &Stamp) -> i64 {
        match stamp {
            Stamp::Utc(w) => w.and_utc().timestamp(),
            Stamp::Date(_) | Stamp::Floating(_) => local_unix(stamp.wall()),
            Stamp::Zoned(w, tz) => self.wall_to_unix(*w, tz),
        }
    }

    /// The instant a wall time in zone `tzid` stands for.
    pub(crate) fn wall_to_unix(&mut self, wall: NaiveDateTime, tzid: &str) -> i64 {
        match self.zone(tzid) {
            Some(Zone::Glib(tz)) => glib_unix(&tz, wall),
            Some(Zone::Spec(spec)) => {
                let offset = spec.offset_at(&invite_stamp(wall)).unwrap_or(0);
                wall.and_utc().timestamp() - i64::from(offset)
            }
            None => local_unix(wall),
        }
    }

    /// The wall time an instant reads as in the zone of `like` (UTC for a
    /// UTC value, local for a floating one or a date).
    pub(crate) fn wall_of(&mut self, unix: i64, like: &Stamp) -> NaiveDateTime {
        match like {
            Stamp::Utc(_) => chrono::DateTime::from_timestamp(unix, 0)
                .map(|d| d.naive_utc())
                .unwrap_or_default(),
            Stamp::Date(_) | Stamp::Floating(_) => local_wall(unix),
            Stamp::Zoned(_, tz) => match self.zone(tz) {
                Some(Zone::Glib(z)) => glib_wall(&z, unix),
                Some(Zone::Spec(spec)) => {
                    // Whichever of the zone's offsets reads back as itself.
                    for o in &spec.observances {
                        let wall = utc_wall(unix + i64::from(o.offset_to));
                        if spec.offset_at(&invite_stamp(wall)) == Some(o.offset_to) {
                            return wall;
                        }
                    }
                    utc_wall(unix)
                }
                None => local_wall(unix),
            },
        }
    }
}

fn invite_stamp(w: NaiveDateTime) -> InviteStamp {
    InviteStamp::wall(
        w.year(),
        w.month() as i32,
        w.day() as i32,
        w.hour() as i32,
        w.minute() as i32,
        w.second() as i32,
    )
}

/// A zone the system's database has. Outlook and Exchange write Windows
/// names; the common ones map to the IANA zone they mean.
fn system_zone(tzid: &str) -> Option<glib::TimeZone> {
    let id = tzid.trim().trim_start_matches('/');
    // Some clients write `/mozilla.org/20050126_1/Europe/Paris` or
    // `/freeassociation.sourceforge.net/Europe/Paris`.
    let tail = id
        .split('/')
        .collect::<Vec<_>>()
        .windows(2)
        .rev()
        .find(|w| w[0].chars().next().is_some_and(|c| c.is_ascii_uppercase()))
        .map(|w| format!("{}/{}", w[0], w[1]));
    for name in [Some(id.to_string()), windows_zone(id).map(str::to_string), tail]
        .into_iter()
        .flatten()
    {
        if name.is_empty() {
            continue;
        }
        // GLib answers UTC for a name it does not know; only a name that
        // reads back as itself (or the plain UTC names) is a real find.
        if let Some(tz) = glib::TimeZone::from_identifier(Some(&name)) {
            let ident = tz.identifier().to_string();
            if ident == name || name.eq_ignore_ascii_case("UTC") || name == "Etc/UTC" {
                return Some(tz);
            }
        }
    }
    None
}

/// The commonest Windows zone names and the IANA zone each means, from
/// CLDR's windowsZones table.
fn windows_zone(name: &str) -> Option<&'static str> {
    const MAP: &[(&str, &str)] = &[
        ("UTC", "Etc/UTC"),
        ("GMT Standard Time", "Europe/London"),
        ("Greenwich Standard Time", "Atlantic/Reykjavik"),
        ("W. Europe Standard Time", "Europe/Berlin"),
        ("Central Europe Standard Time", "Europe/Budapest"),
        ("Romance Standard Time", "Europe/Paris"),
        ("Central European Standard Time", "Europe/Warsaw"),
        ("E. Europe Standard Time", "Europe/Chisinau"),
        ("FLE Standard Time", "Europe/Kiev"),
        ("GTB Standard Time", "Europe/Bucharest"),
        ("Russian Standard Time", "Europe/Moscow"),
        ("Turkey Standard Time", "Europe/Istanbul"),
        ("Israel Standard Time", "Asia/Jerusalem"),
        ("South Africa Standard Time", "Africa/Johannesburg"),
        ("Arabian Standard Time", "Asia/Dubai"),
        ("India Standard Time", "Asia/Kolkata"),
        ("China Standard Time", "Asia/Shanghai"),
        ("Singapore Standard Time", "Asia/Singapore"),
        ("Tokyo Standard Time", "Asia/Tokyo"),
        ("Korea Standard Time", "Asia/Seoul"),
        ("AUS Eastern Standard Time", "Australia/Sydney"),
        ("E. Australia Standard Time", "Australia/Brisbane"),
        ("Cen. Australia Standard Time", "Australia/Adelaide"),
        ("W. Australia Standard Time", "Australia/Perth"),
        ("New Zealand Standard Time", "Pacific/Auckland"),
        ("Eastern Standard Time", "America/New_York"),
        ("Central Standard Time", "America/Chicago"),
        ("Mountain Standard Time", "America/Denver"),
        ("US Mountain Standard Time", "America/Phoenix"),
        ("Pacific Standard Time", "America/Los_Angeles"),
        ("Alaskan Standard Time", "America/Anchorage"),
        ("Hawaiian Standard Time", "Pacific/Honolulu"),
        ("Atlantic Standard Time", "America/Halifax"),
        ("Newfoundland Standard Time", "America/St_Johns"),
        ("Canada Central Standard Time", "America/Regina"),
        ("SA Pacific Standard Time", "America/Bogota"),
        ("E. South America Standard Time", "America/Sao_Paulo"),
        ("Argentina Standard Time", "America/Buenos_Aires"),
        ("Central Standard Time (Mexico)", "America/Mexico_City"),
    ];
    MAP.iter().find(|(w, _)| *w == name).map(|(_, i)| *i)
}

fn glib_unix(tz: &glib::TimeZone, w: NaiveDateTime) -> i64 {
    glib::DateTime::new(
        tz,
        w.year(),
        w.month() as i32,
        w.day() as i32,
        w.hour() as i32,
        w.minute() as i32,
        f64::from(w.second()),
    )
    .map(|d| d.to_unix())
    .unwrap_or_else(|_| w.and_utc().timestamp())
}

fn glib_wall(tz: &glib::TimeZone, unix: i64) -> NaiveDateTime {
    glib::DateTime::from_unix_utc(unix)
        .and_then(|d| d.to_timezone(tz))
        .ok()
        .and_then(|d| {
            NaiveDate::from_ymd_opt(d.year(), d.month() as u32, d.day_of_month() as u32)?
                .and_hms_opt(d.hour() as u32, d.minute() as u32, d.second() as u32)
        })
        .unwrap_or_else(|| utc_wall(unix))
}

fn utc_wall(unix: i64) -> NaiveDateTime {
    chrono::DateTime::from_timestamp(unix, 0)
        .map(|d| d.naive_utc())
        .unwrap_or_default()
}

/// The instant a local wall time stands for.
pub(crate) fn local_unix(w: NaiveDateTime) -> i64 {
    glib_unix(&glib::TimeZone::local(), w)
}

/// The local wall time of an instant.
pub(crate) fn local_wall(unix: i64) -> NaiveDateTime {
    glib_wall(&glib::TimeZone::local(), unix)
}

/// Local midnight at the start of `day`.
pub(crate) fn day_start(day: NaiveDate) -> i64 {
    local_unix(day.and_time(NaiveTime::MIN))
}

/// The local day an instant falls on.
pub(crate) fn local_day(unix: i64) -> NaiveDate {
    local_wall(unix).date()
}

/// The system's own zone name (`Europe/Paris`), for writing a recurring
/// event that should keep its wall time. `None` when it can't be told,
/// and a new event is then written in UTC.
pub(crate) fn local_tzid() -> Option<String> {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim_start_matches(':');
        if !tz.starts_with('/') && system_zone(tz).is_some() {
            return Some(tz.to_string());
        }
    }
    let ident = glib::TimeZone::local().identifier().to_string();
    if ident.contains('/') && !ident.starts_with('/') && system_zone(&ident).is_some() {
        return Some(ident);
    }
    for file in ["/etc/localtime", "/run/host/etc/localtime"] {
        if let Ok(target) = std::fs::read_link(file) {
            let target = target.to_string_lossy().to_string();
            if let Some((_, name)) = target.split_once("zoneinfo/") {
                if system_zone(name).is_some() {
                    return Some(name.to_string());
                }
            }
        }
    }
    std::fs::read_to_string("/etc/timezone")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| system_zone(s).is_some())
}

/// A `VTIMEZONE` for a zone the system knows, so a server that keeps no
/// zone database of its own can still read the event. Built from the
/// changes the zone makes this year: each becomes a yearly rule on its
/// weekday ("the last Sunday of October"), which is how every zone that
/// has daylight saving time sets it.
pub(crate) fn vtimezone(tzid: &str) -> Option<Node> {
    let tz = system_zone(tzid)?;
    let year = chrono::Local::now().year();
    let jan1 = NaiveDate::from_ymd_opt(year, 1, 1)?.and_time(NaiveTime::MIN);
    let offset_at = |unix: i64| -> i32 {
        glib::DateTime::from_unix_utc(unix)
            .and_then(|d| d.to_timezone(&tz))
            .map(|d| (d.utc_offset().as_seconds()) as i32)
            .unwrap_or(0)
    };
    let start = jan1.and_utc().timestamp();
    // Each change: the instant it happens, the offset before and after.
    let mut changes: Vec<(i64, i32, i32)> = Vec::new();
    let mut prev = offset_at(start);
    let mut t = start;
    let end = start + 366 * 86_400;
    while t < end {
        let next = t + 86_400;
        let now = offset_at(next);
        if now != prev {
            // Narrow it down to the second.
            let (mut lo, mut hi) = (t, next);
            while hi - lo > 1 {
                let mid = (lo + hi) / 2;
                if offset_at(mid) == prev {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            changes.push((hi, prev, now));
            prev = now;
        }
        t = next;
    }
    let mut node = Node::new("VTIMEZONE");
    node.push(format!("TZID:{tzid}"));
    let fmt_offset = |o: i32| {
        let sign = if o < 0 { '-' } else { '+' };
        let o = o.abs();
        format!("{sign}{:02}{:02}", o / 3600, (o % 3600) / 60)
    };
    if changes.is_empty() {
        let mut std = Node::new("STANDARD");
        std.push("DTSTART:19700101T000000".into());
        std.push(format!("TZOFFSETFROM:{}", fmt_offset(prev)));
        std.push(format!("TZOFFSETTO:{}", fmt_offset(prev)));
        node.children.push(std);
        return Some(node);
    }
    for (at, from, to) in changes {
        // The rule is written in the wall time before the change.
        let wall = utc_wall(at + i64::from(from));
        let dst = to > from;
        let mut o = Node::new(if dst { "DAYLIGHT" } else { "STANDARD" });
        let day = wall.day();
        let last = days_in_month(wall.year(), wall.month());
        let ordinal = if day + 7 > last {
            -1
        } else {
            ((day - 1) / 7 + 1) as i32
        };
        let weekday = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"]
            [wall.weekday().num_days_from_monday() as usize];
        o.push(format!(
            "DTSTART:1970{:02}{:02}T{:02}{:02}{:02}",
            wall.month(),
            day,
            wall.hour(),
            wall.minute(),
            wall.second()
        ));
        o.push(format!(
            "RRULE:FREQ=YEARLY;BYMONTH={};BYDAY={ordinal}{weekday}",
            wall.month()
        ));
        o.push(format!("TZOFFSETFROM:{}", fmt_offset(from)));
        o.push(format!("TZOFFSETTO:{}", fmt_offset(to)));
        node.children.push(o);
    }
    Some(node)
}

pub(crate) fn days_in_month(year: i32, month: u32) -> u32 {
    let (y, m) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1)
        .and_then(|d| d.pred_opt())
        .map_or(30, |d| d.day())
}

/// The current time as an iCalendar UTC value, for `DTSTAMP` and
/// `LAST-MODIFIED`.
pub(crate) fn utc_now_value() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::invite::ics::split_line;

    fn stamp(line: &str) -> Stamp {
        Stamp::read(&split_line(line).unwrap()).unwrap()
    }

    #[test]
    fn the_four_shapes_read_and_write_back() {
        for line in [
            "DTSTART;VALUE=DATE:20261102",
            "DTSTART:20261102T090000",
            "DTSTART:20261102T090000Z",
            "DTSTART;TZID=Europe/Paris:20261102T090000",
        ] {
            assert_eq!(stamp(line).line("DTSTART"), line);
        }
        assert!(stamp("DUE:20261102").is_date());
        assert_eq!(
            stamp("DTSTART;TZID=\"GMT+1: Paris\":20261102T090000").line("DTSTART"),
            "DTSTART;TZID=\"GMT+1: Paris\":20261102T090000"
        );
    }

    #[test]
    fn a_named_zone_keeps_its_wall_time_across_daylight_saving() {
        let mut zones = Zones::default();
        // 9:00 in New York is 13:00 UTC in summer and 14:00 in winter.
        let summer = stamp("DTSTART;TZID=America/New_York:20260701T090000");
        let winter = stamp("DTSTART;TZID=America/New_York:20261201T090000");
        assert_eq!(zones.unix(&summer) % 86_400, 13 * 3600);
        assert_eq!(zones.unix(&winter) % 86_400, 14 * 3600);
        let u = zones.unix(&winter);
        let back = zones.wall_of(u, &winter);
        assert_eq!(back, winter.wall());
    }

    #[test]
    fn windows_names_and_prefixed_names_resolve() {
        let mut zones = Zones::default();
        let a = stamp("DTSTART;TZID=W. Europe Standard Time:20260115T100000");
        let b = stamp("DTSTART;TZID=/mozilla.org/20050126_1/Europe/Berlin:20260115T100000");
        assert_eq!(zones.unix(&a), zones.unix(&b));
        assert_eq!(zones.unix(&a) % 86_400, 9 * 3600);
    }

    #[test]
    fn a_learned_vtimezone_stands_in_for_an_unknown_name() {
        let tz = Node::parse(
            "BEGIN:VTIMEZONE\nTZID:Custom Zone\nBEGIN:STANDARD\nDTSTART:19701025T030000\n\
             RRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\nTZOFFSETFROM:+0200\nTZOFFSETTO:+0100\n\
             END:STANDARD\nBEGIN:DAYLIGHT\nDTSTART:19700329T020000\n\
             RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\nTZOFFSETFROM:+0100\nTZOFFSETTO:+0200\n\
             END:DAYLIGHT\nEND:VTIMEZONE\n",
        )
        .unwrap();
        let mut zones = Zones::default();
        zones.learn(&tz);
        let s = stamp("DTSTART;TZID=Custom Zone:20260701T120000");
        assert_eq!(zones.unix(&s) % 86_400, 10 * 3600);
        let u = zones.unix(&s);
        assert_eq!(zones.wall_of(u, &s), s.wall());
    }

    #[test]
    fn a_generated_vtimezone_reads_back_as_the_same_zone() {
        let node = vtimezone("Europe/Paris").unwrap();
        assert_eq!(node.children.len(), 2);
        let mut zones = Zones::default();
        let mut renamed = node.clone();
        renamed.replace("TZID:Not A System Zone".into());
        zones.learn(&renamed);
        for day in ["20260115T120000", "20260715T120000"] {
            let a = stamp(&format!("DTSTART;TZID=Europe/Paris:{day}"));
            let b = stamp(&format!("DTSTART;TZID=Not A System Zone:{day}"));
            assert_eq!(zones.unix(&a), zones.unix(&b), "{day}");
        }
    }
}
