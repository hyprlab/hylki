//! Recurrence rules (RFC 5545 3.3.10): reading an `RRULE`, writing one back,
//! and listing the starts of a series inside a window of time.
//!
//! The rule works on wall-clock times in the event's own zone, never on
//! UTC: a weekly 9:00 meeting stays at 9:00 across a daylight saving change,
//! so the zone is applied to each start afterwards, by the caller. Only a
//! UTC `UNTIL` needs a moment to compare against, which is what `to_utc` is
//! for.
//!
//! The expansion walks the rule one period at a time (a year for YEARLY, a
//! week for WEEKLY and so on). Each period's candidate days are every day it
//! holds, kept or dropped by the BYxxx parts, then crossed with its times of
//! day, then cut by BYSETPOS. That one shape covers the RFC's table of
//! "expand" and "limit" cases, because expanding a field and limiting it come
//! to the same set once the period's days are all on the table. The fields a
//! rule leaves out come from `DTSTART`: a WEEKLY rule with no BYDAY repeats
//! on DTSTART's weekday, a MONTHLY one on its day of the month.
//!
//! A calendar asks for a few weeks at a time of series that may have begun
//! decades ago. Without a COUNT the walk jumps straight to the period the
//! window starts in; with one it has to count from the first start, and a
//! cap on the work done keeps a rule that never matches (the 30th of
//! February) from looping for ever.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};

/// The most periods one expansion walks through.
const MAX_PERIODS: u64 = 500_000;
/// The most candidate days one expansion looks at: a YEARLY period alone
/// holds 366 of them.
const MAX_DAYS: u64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Freq {
    Secondly,
    Minutely,
    Hourly,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Freq {
    fn parse(text: &str) -> Option<Freq> {
        Some(match text {
            "SECONDLY" => Freq::Secondly,
            "MINUTELY" => Freq::Minutely,
            "HOURLY" => Freq::Hourly,
            "DAILY" => Freq::Daily,
            "WEEKLY" => Freq::Weekly,
            "MONTHLY" => Freq::Monthly,
            "YEARLY" => Freq::Yearly,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Freq::Secondly => "SECONDLY",
            Freq::Minutely => "MINUTELY",
            Freq::Hourly => "HOURLY",
            Freq::Daily => "DAILY",
            Freq::Weekly => "WEEKLY",
            Freq::Monthly => "MONTHLY",
            Freq::Yearly => "YEARLY",
        }
    }

    /// Whether a period of this frequency is a whole day or longer, so
    /// its times of day come from BYHOUR, BYMINUTE and BYSECOND.
    fn daily_or_slower(self) -> bool {
        matches!(
            self,
            Freq::Daily | Freq::Weekly | Freq::Monthly | Freq::Yearly
        )
    }
}

/// When a series ends, in the form the rule wrote it: RFC 5545 has UNTIL
/// match DTSTART's own kind of value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Until {
    /// The last day, for an all-day series.
    Date(NaiveDate),
    /// A wall-clock time, for a floating series.
    Floating(NaiveDateTime),
    /// A moment, in unix seconds, for a series in a zone or in UTC.
    Utc(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rule {
    pub(crate) freq: Freq,
    pub(crate) interval: u32,
    pub(crate) count: Option<u32>,
    pub(crate) until: Option<Until>,
    pub(crate) by_second: Vec<u32>,
    pub(crate) by_minute: Vec<u32>,
    pub(crate) by_hour: Vec<u32>,
    /// A weekday, with the ordinal of it within the month or the year:
    /// 0 for every one, 1 for the first, -1 for the last.
    pub(crate) by_day: Vec<(i32, Weekday)>,
    pub(crate) by_month_day: Vec<i32>,
    pub(crate) by_year_day: Vec<i32>,
    pub(crate) by_week_no: Vec<i32>,
    pub(crate) by_month: Vec<u32>,
    pub(crate) by_set_pos: Vec<i32>,
    pub(crate) wkst: Weekday,
}

impl Rule {
    pub(crate) fn new(freq: Freq) -> Rule {
        Rule {
            freq,
            interval: 1,
            count: None,
            until: None,
            by_second: Vec::new(),
            by_minute: Vec::new(),
            by_hour: Vec::new(),
            by_day: Vec::new(),
            by_month_day: Vec::new(),
            by_year_day: Vec::new(),
            by_week_no: Vec::new(),
            by_month: Vec::new(),
            by_set_pos: Vec::new(),
            wkst: Weekday::Mon,
        }
    }

    /// Read `FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE`, with or without the
    /// `RRULE:` in front. A part that is not understood is left out rather
    /// than failing the rule, the way calendars treat an `X-` part; a rule
    /// without a FREQ is no rule.
    pub(crate) fn parse(text: &str) -> Option<Rule> {
        let text = text.trim();
        let text = match text.get(..6) {
            Some(head) if head.eq_ignore_ascii_case("RRULE:") => &text[6..],
            _ => text,
        };
        let mut freq = None;
        let mut rule = Rule::new(Freq::Daily);
        for part in text.split(';') {
            let Some((key, value)) = part.split_once('=') else {
                continue;
            };
            let key = key.trim().to_ascii_uppercase();
            let value = value.trim().to_ascii_uppercase();
            match key.as_str() {
                "FREQ" => freq = Freq::parse(&value),
                "INTERVAL" => rule.interval = value.parse::<u32>().unwrap_or(1).max(1),
                "COUNT" => rule.count = value.parse().ok(),
                "UNTIL" => rule.until = parse_until(&value),
                "BYSECOND" => rule.by_second = numbers(&value, |n: u32| n <= 60),
                "BYMINUTE" => rule.by_minute = numbers(&value, |n: u32| n <= 59),
                "BYHOUR" => rule.by_hour = numbers(&value, |n: u32| n <= 23),
                "BYDAY" => rule.by_day = value.split(',').filter_map(parse_by_day).collect(),
                "BYMONTHDAY" => {
                    rule.by_month_day = numbers(&value, |n: i32| n != 0 && n.abs() <= 31)
                }
                "BYYEARDAY" => {
                    rule.by_year_day = numbers(&value, |n: i32| n != 0 && n.abs() <= 366)
                }
                "BYWEEKNO" => rule.by_week_no = numbers(&value, |n: i32| n != 0 && n.abs() <= 53),
                "BYMONTH" => rule.by_month = numbers(&value, |n: u32| (1..=12).contains(&n)),
                "BYSETPOS" => {
                    rule.by_set_pos = numbers(&value, |n: i32| n != 0 && n.abs() <= 366)
                }
                "WKST" => rule.wkst = parse_weekday(&value).unwrap_or(Weekday::Mon),
                _ => {}
            }
        }
        rule.freq = freq?;
        Some(rule)
    }

    /// The rule as an `RRULE` value, its parts in one fixed order so two
    /// equal rules write the same text.
    pub(crate) fn to_ical(&self) -> String {
        let mut parts = vec![format!("FREQ={}", self.freq.name())];
        if self.interval > 1 {
            parts.push(format!("INTERVAL={}", self.interval));
        }
        if let Some(count) = self.count {
            parts.push(format!("COUNT={count}"));
        }
        if let Some(until) = self.until {
            parts.push(format!("UNTIL={}", until_text(until)));
        }
        let list = |name: &str, values: Vec<String>, parts: &mut Vec<String>| {
            if !values.is_empty() {
                parts.push(format!("{name}={}", values.join(",")));
            }
        };
        let text = |v: &[u32]| v.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let signed = |v: &[i32]| v.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        list("BYSECOND", text(&self.by_second), &mut parts);
        list("BYMINUTE", text(&self.by_minute), &mut parts);
        list("BYHOUR", text(&self.by_hour), &mut parts);
        list(
            "BYDAY",
            self.by_day
                .iter()
                .map(|&(n, wd)| {
                    if n == 0 {
                        weekday_name(wd).to_string()
                    } else {
                        format!("{n}{}", weekday_name(wd))
                    }
                })
                .collect(),
            &mut parts,
        );
        list("BYMONTHDAY", signed(&self.by_month_day), &mut parts);
        list("BYYEARDAY", signed(&self.by_year_day), &mut parts);
        list("BYWEEKNO", signed(&self.by_week_no), &mut parts);
        list("BYMONTH", text(&self.by_month), &mut parts);
        list("BYSETPOS", signed(&self.by_set_pos), &mut parts);
        if self.wkst != Weekday::Mon {
            parts.push(format!("WKST={}", weekday_name(self.wkst)));
        }
        parts.join(";")
    }
}

/// The starts of a series as wall-clock times in its own zone, in order:
/// every start `s` with `from <= s < to`, at most `limit` of them.
///
/// `dtstart` is always the first start, whether or not it fits the rule,
/// as RFC 5545 has it, and COUNT counts from there, not from `from`.
/// `to_utc` turns a wall-clock time into unix seconds, to compare against a
/// UTC UNTIL.
pub(crate) fn expand(
    rule: &Rule,
    dtstart: NaiveDateTime,
    from: NaiveDateTime,
    to: NaiveDateTime,
    to_utc: &dyn Fn(NaiveDateTime) -> i64,
    limit: usize,
) -> Vec<NaiveDateTime> {
    let mut out = Vec::new();
    if limit == 0 || from >= to {
        return out;
    }
    let walk = Walk::new(rule, dtstart);
    let past = |t: NaiveDateTime| match rule.until {
        None => false,
        Some(Until::Date(d)) => t.date() > d,
        Some(Until::Floating(u)) => t > u,
        Some(Until::Utc(u)) => to_utc(t) > u,
    };

    // The first start counts even if it fits neither the rule nor UNTIL.
    let mut taken: u64 = 1;
    if dtstart >= from && dtstart < to {
        out.push(dtstart);
        if out.len() >= limit {
            return out;
        }
    }
    let count_done = |taken: u64| rule.count.is_some_and(|c| taken >= u64::from(c));
    if count_done(taken) {
        return out;
    }

    // With no COUNT nothing before the window matters, so the walk starts
    // one period short of the one the window opens in.
    let mut k = if rule.count.is_none() && from > dtstart {
        (walk.units_to(from) / i64::from(rule.interval.max(1)) - 1).max(0)
    } else {
        0
    };
    let mut periods: u64 = 0;
    let mut days: u64 = 0;
    loop {
        periods += 1;
        if periods > MAX_PERIODS || days > MAX_DAYS {
            break;
        }
        let Some(start) = walk.period_start(k) else {
            break;
        };
        if start >= to || past(start) {
            break;
        }
        let Some(candidates) = walk.candidates(k, &mut days) else {
            break;
        };
        for c in candidates {
            if c <= dtstart {
                continue;
            }
            if past(c) || c >= to {
                return out;
            }
            taken += 1;
            if c >= from {
                out.push(c);
                if out.len() >= limit {
                    return out;
                }
            }
            if count_done(taken) {
                return out;
            }
        }
        k += 1;
    }
    out
}

/// Every start of a series that ends (by COUNT or UNTIL), at most `limit`
/// of them; `None` for a series that goes on for ever.
pub(crate) fn all(
    rule: &Rule,
    dtstart: NaiveDateTime,
    to_utc: &dyn Fn(NaiveDateTime) -> i64,
    limit: usize,
) -> Option<Vec<NaiveDateTime>> {
    if rule.count.is_none() && rule.until.is_none() {
        return None;
    }
    Some(expand(
        rule,
        dtstart,
        dtstart,
        NaiveDateTime::MAX,
        to_utc,
        limit,
    ))
}

// ---------------------------------------------------------------------------
// The walk
// ---------------------------------------------------------------------------

/// A rule with the parts it leaves out filled in from DTSTART, ready to
/// list the starts in one period after another.
struct Walk<'a> {
    rule: &'a Rule,
    dtstart: NaiveDateTime,
    by_month: Vec<u32>,
    by_month_day: Vec<i32>,
    by_day: Vec<(i32, Weekday)>,
    /// For a day or longer, the times of day; for a shorter period, what
    /// fills in below the period's own unit.
    hours: Vec<u32>,
    minutes: Vec<u32>,
    seconds: Vec<u32>,
    /// Whether an ordinal BYDAY counts within the month (else the year).
    ordinal_in_month: bool,
}

impl<'a> Walk<'a> {
    fn new(rule: &'a Rule, dtstart: NaiveDateTime) -> Walk<'a> {
        let mut by_month = rule.by_month.clone();
        let mut by_month_day = rule.by_month_day.clone();
        let mut by_day = rule.by_day.clone();
        let no_day_rule = rule.by_week_no.is_empty()
            && rule.by_year_day.is_empty()
            && rule.by_month_day.is_empty()
            && rule.by_day.is_empty();
        if no_day_rule {
            match rule.freq {
                Freq::Yearly => {
                    if by_month.is_empty() {
                        by_month.push(dtstart.month());
                    }
                    by_month_day.push(dtstart.day() as i32);
                }
                Freq::Monthly => by_month_day.push(dtstart.day() as i32),
                Freq::Weekly => by_day.push((0, dtstart.weekday())),
                _ => {}
            }
        }
        let fill = |given: &[u32], own: u32, needed: bool| -> Vec<u32> {
            let mut v = if given.is_empty() && needed {
                vec![own]
            } else {
                given.to_vec()
            };
            v.sort_unstable();
            v.dedup();
            v
        };
        let freq = rule.freq;
        let hours = fill(&rule.by_hour, dtstart.hour(), freq.daily_or_slower());
        let minutes = fill(
            &rule.by_minute,
            dtstart.minute(),
            freq.daily_or_slower() || freq == Freq::Hourly,
        );
        let seconds = fill(
            &rule.by_second,
            dtstart.second().min(59),
            freq != Freq::Secondly,
        );
        Walk {
            rule,
            dtstart,
            by_month,
            by_month_day,
            by_day,
            hours,
            minutes,
            seconds,
            ordinal_in_month: freq == Freq::Monthly
                || (freq == Freq::Yearly && !rule.by_month.is_empty()),
        }
    }

    fn step(&self, k: i64) -> Option<i64> {
        k.checked_mul(i64::from(self.rule.interval.max(1)))
    }

    /// The first moment of period `k`, counted in steps of the interval.
    fn period_start(&self, k: i64) -> Option<NaiveDateTime> {
        let n = self.step(k)?;
        let d0 = self.dtstart.date();
        let midnight = |d: NaiveDate| d.and_time(NaiveTime::MIN);
        match self.rule.freq {
            Freq::Yearly => {
                let year = i64::from(d0.year()).checked_add(n)?;
                NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, 1, 1).map(midnight)
            }
            Freq::Monthly => {
                let (y, m) = self.month_of(n)?;
                NaiveDate::from_ymd_opt(y, m, 1).map(midnight)
            }
            Freq::Weekly => week_start(d0, self.rule.wkst)
                .checked_add_signed(Duration::try_days(n.checked_mul(7)?)?)
                .map(midnight),
            Freq::Daily => d0
                .checked_add_signed(Duration::try_days(n)?)
                .map(midnight),
            Freq::Hourly => self
                .truncated(3600)
                .checked_add_signed(Duration::try_hours(n)?),
            Freq::Minutely => self
                .truncated(60)
                .checked_add_signed(Duration::try_minutes(n)?),
            Freq::Secondly => self
                .dtstart
                .with_nanosecond(0)?
                .checked_add_signed(Duration::try_seconds(n)?),
        }
    }

    /// DTSTART cut down to the hour or the minute.
    fn truncated(&self, unit: u32) -> NaiveDateTime {
        let t = self.dtstart.time();
        let secs = t.num_seconds_from_midnight() / unit * unit;
        self.dtstart.date().and_time(
            NaiveTime::from_num_seconds_from_midnight_opt(secs, 0).unwrap_or(NaiveTime::MIN),
        )
    }

    /// The year and month `n` months after DTSTART's.
    fn month_of(&self, n: i64) -> Option<(i32, u32)> {
        let d0 = self.dtstart.date();
        let index = (i64::from(d0.year()) * 12 + i64::from(d0.month0())).checked_add(n)?;
        Some((
            i32::try_from(index.div_euclid(12)).ok()?,
            index.rem_euclid(12) as u32 + 1,
        ))
    }

    /// How many of the rule's units (years, months, weeks...) lie between
    /// DTSTART's period and the one `t` falls in.
    fn units_to(&self, t: NaiveDateTime) -> i64 {
        let d0 = self.dtstart.date();
        match self.rule.freq {
            Freq::Yearly => i64::from(t.year() - d0.year()),
            Freq::Monthly => {
                i64::from(t.year() - d0.year()) * 12 + i64::from(t.month0())
                    - i64::from(d0.month0())
            }
            Freq::Weekly => {
                (week_start(t.date(), self.rule.wkst) - week_start(d0, self.rule.wkst)).num_days()
                    / 7
            }
            Freq::Daily => (t.date() - d0).num_days(),
            Freq::Hourly => (t - self.truncated(3600)).num_hours(),
            Freq::Minutely => (t - self.truncated(60)).num_minutes(),
            Freq::Secondly => (t - self.dtstart).num_seconds(),
        }
    }

    /// The starts in period `k`, in order, BYSETPOS applied. `None` past
    /// the end of the calendar.
    fn candidates(&self, k: i64, examined: &mut u64) -> Option<Vec<NaiveDateTime>> {
        let n = self.step(k)?;
        let start = self.period_start(k)?;
        let mut out = Vec::new();
        if self.rule.freq.daily_or_slower() {
            let days = self.period_days(n, start.date())?;
            *examined += days.len() as u64;
            for day in days.into_iter().filter(|d| self.day_fits(*d)) {
                for &h in &self.hours {
                    for &m in &self.minutes {
                        for &s in &self.seconds {
                            if let Some(t) = NaiveTime::from_hms_opt(h, m, s) {
                                out.push(day.and_time(t));
                            }
                        }
                    }
                }
            }
        } else {
            *examined += 1;
            let fits = |given: &[u32], value: u32| given.is_empty() || given.contains(&value);
            if self.day_fits(start.date()) && fits(&self.rule.by_hour, start.hour()) {
                let minutes = if self.rule.freq == Freq::Hourly {
                    self.minutes.clone()
                } else {
                    vec![start.minute()]
                };
                let seconds = if self.rule.freq == Freq::Secondly {
                    vec![start.second()]
                } else {
                    self.seconds.clone()
                };
                for m in minutes {
                    if !fits(&self.rule.by_minute, m) {
                        continue;
                    }
                    for &s in &seconds {
                        if !fits(&self.rule.by_second, s) {
                            continue;
                        }
                        if let Some(t) = NaiveTime::from_hms_opt(start.hour(), m, s) {
                            out.push(start.date().and_time(t));
                        }
                    }
                }
            }
        }
        if !self.rule.by_set_pos.is_empty() {
            out = set_positions(&out, &self.rule.by_set_pos);
        }
        Some(out)
    }

    /// Every day of a day-or-longer period.
    fn period_days(&self, n: i64, first: NaiveDate) -> Option<Vec<NaiveDate>> {
        Some(match self.rule.freq {
            Freq::Yearly => {
                // Only the months BYMONTH keeps need looking at.
                let months: Vec<u32> = if self.by_month.is_empty() {
                    (1..=12).collect()
                } else {
                    let mut m = self.by_month.clone();
                    m.sort_unstable();
                    m.dedup();
                    m
                };
                months
                    .into_iter()
                    .flat_map(|m| month_days(first.year(), m))
                    .collect()
            }
            Freq::Monthly => {
                let (y, m) = self.month_of(n)?;
                month_days(y, m)
            }
            Freq::Weekly => (0..7)
                .filter_map(|i| first.checked_add_signed(Duration::days(i)))
                .collect(),
            _ => vec![first],
        })
    }

    /// Whether a day passes the rule's BYMONTH, BYWEEKNO, BYYEARDAY,
    /// BYMONTHDAY and BYDAY.
    fn day_fits(&self, d: NaiveDate) -> bool {
        if !self.by_month.is_empty() && !self.by_month.contains(&d.month()) {
            return false;
        }
        if !self.rule.by_week_no.is_empty() {
            let (week, weeks) = week_number(d, self.rule.wkst);
            if !self
                .rule
                .by_week_no
                .iter()
                .any(|&n| n == week || n == week - weeks - 1)
            {
                return false;
            }
        }
        if !self.rule.by_year_day.is_empty() {
            let day = d.ordinal() as i32;
            let len = year_length(d.year());
            if !self
                .rule
                .by_year_day
                .iter()
                .any(|&n| n == day || n == day - len - 1)
            {
                return false;
            }
        }
        if !self.by_month_day.is_empty() {
            let day = d.day() as i32;
            let len = month_length(d.year(), d.month());
            if !self
                .by_month_day
                .iter()
                .any(|&n| n == day || n == day - len - 1)
            {
                return false;
            }
        }
        if !self.by_day.is_empty() && !self.by_day.iter().any(|&(n, wd)| self.weekday_fits(d, n, wd))
        {
            return false;
        }
        true
    }

    /// Whether `d` is the `n`th `wd` of its month or year (any `wd` for 0).
    /// Outside MONTHLY and YEARLY an ordinal means nothing and is ignored.
    fn weekday_fits(&self, d: NaiveDate, n: i32, wd: Weekday) -> bool {
        if d.weekday() != wd {
            return false;
        }
        if n == 0 || !matches!(self.rule.freq, Freq::Monthly | Freq::Yearly) {
            return true;
        }
        let (day, len) = if self.ordinal_in_month {
            (d.day() as i32, month_length(d.year(), d.month()))
        } else {
            (d.ordinal() as i32, year_length(d.year()))
        };
        if n > 0 {
            (day - 1) / 7 + 1 == n
        } else {
            (len - day) / 7 + 1 == -n
        }
    }
}

/// The starts at BYSETPOS's positions in one period's set, in order.
fn set_positions(set: &[NaiveDateTime], positions: &[i32]) -> Vec<NaiveDateTime> {
    let len = set.len() as i32;
    let mut out: Vec<NaiveDateTime> = positions
        .iter()
        .filter_map(|&p| {
            let i = if p > 0 { p - 1 } else { len + p };
            (0..len).contains(&i).then(|| set[i as usize])
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

// ---------------------------------------------------------------------------
// The calendar
// ---------------------------------------------------------------------------

fn month_days(year: i32, month: u32) -> Vec<NaiveDate> {
    (1..=month_length(year, month) as u32)
        .filter_map(|d| NaiveDate::from_ymd_opt(year, month, d))
        .collect()
}

fn month_length(year: i32, month: u32) -> i32 {
    let (y, m) = if month >= 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1)
        .and_then(|d| d.pred_opt())
        .map_or(30, |d| d.day() as i32)
}

fn year_length(year: i32) -> i32 {
    if NaiveDate::from_ymd_opt(year, 2, 29).is_some() {
        366
    } else {
        365
    }
}

/// The first day of the week `d` is in, weeks starting on `wkst`.
fn week_start(d: NaiveDate, wkst: Weekday) -> NaiveDate {
    let back = (d.weekday().num_days_from_monday() + 7 - wkst.num_days_from_monday()) % 7;
    d - Duration::days(i64::from(back))
}

/// The first day of week 1 of `year`: the week (starting on `wkst`) that
/// holds at least four days of the year, which is the one holding 4 January.
fn week_one(year: i32, wkst: Weekday) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(year, 1, 4).map(|d| week_start(d, wkst))
}

/// The week number of `d` and how many weeks its week-year has. A day in
/// the last days of December can be in week 1 of the next year, and one in
/// early January in the last week of the year before.
fn week_number(d: NaiveDate, wkst: Weekday) -> (i32, i32) {
    let weeks = |y: i32| match (week_one(y, wkst), week_one(y + 1, wkst)) {
        (Some(a), Some(b)) => ((b - a).num_days() / 7) as i32,
        _ => 52,
    };
    let year = d.year();
    let Some(this) = week_one(year, wkst) else {
        return (1, 52);
    };
    let next = week_one(year + 1, wkst);
    let (base, week_year) = if d < this {
        (week_one(year - 1, wkst).unwrap_or(this), year - 1)
    } else if next.is_some_and(|n| d >= n) {
        (next.unwrap_or(this), year + 1)
    } else {
        (this, year)
    };
    (((d - base).num_days() / 7) as i32 + 1, weeks(week_year))
}

// ---------------------------------------------------------------------------
// Reading and writing the parts
// ---------------------------------------------------------------------------

fn numbers<T: std::str::FromStr + Copy>(text: &str, ok: impl Fn(T) -> bool) -> Vec<T> {
    text.split(',')
        .filter_map(|v| v.trim().trim_start_matches('+').parse::<T>().ok())
        .filter(|&n| ok(n))
        .collect()
}

fn parse_weekday(text: &str) -> Option<Weekday> {
    Some(match text {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

fn weekday_name(wd: Weekday) -> &'static str {
    match wd {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    }
}

/// `MO`, `1FR`, `-2MO`, `+3TU`.
fn parse_by_day(text: &str) -> Option<(i32, Weekday)> {
    let text = text.trim();
    let split = text.len().checked_sub(2)?;
    let (num, day) = text.split_at(split);
    let wd = parse_weekday(day)?;
    let n = match num.trim_start_matches('+') {
        "" => 0,
        n => n.parse::<i32>().ok().filter(|n| n.abs() <= 53)?,
    };
    Some((n, wd))
}

/// `20261231`, `20261231T235959` or `20261231T235959Z`.
fn parse_until(text: &str) -> Option<Until> {
    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    let num = |a: usize, b: usize| digits.get(a..b)?.parse::<u32>().ok();
    let date = NaiveDate::from_ymd_opt(num(0, 4)? as i32, num(4, 6)?, num(6, 8)?)?;
    if digits.len() < 14 {
        return Some(Until::Date(date));
    }
    let t = date.and_hms_opt(num(8, 10)?, num(10, 12)?, num(12, 14)?.min(59))?;
    if text.ends_with('Z') {
        Some(Until::Utc(t.and_utc().timestamp()))
    } else {
        Some(Until::Floating(t))
    }
}

fn until_text(until: Until) -> String {
    match until {
        Until::Date(d) => d.format("%Y%m%d").to_string(),
        Until::Floating(t) => t.format("%Y%m%dT%H%M%S").to_string(),
        Until::Utc(u) => chrono::DateTime::from_timestamp(u, 0)
            .map(|t| t.naive_utc().format("%Y%m%dT%H%M%SZ").to_string())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests;
