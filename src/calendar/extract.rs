//! Dates and times read from text (#637): the day, the time and the end of
//! an event in what someone selected in a message, for Create Event and
//! Create Task to start from.
//!
//! Rules, not a model. Month and weekday names come in English and in the
//! system's language (from GLib, as `datefmt` takes them); "today" and
//! "tomorrow" are translatable. What is not recognised is left to the
//! dialog's own defaults, which the user changes there anyway.

use std::ops::Range;

use chrono::{Datelike, Duration, NaiveDate, NaiveTime, Timelike, Weekday};

#[cfg(test)]
mod tests;

/// What was found. Each part is there only when the text gave it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Found {
    /// The first day.
    pub(crate) date: Option<NaiveDate>,
    /// The last day of a range of days ("20–22 November").
    pub(crate) last: Option<NaiveDate>,
    pub(crate) start: Option<NaiveTime>,
    pub(crate) end: Option<NaiveTime>,
    /// Where in the text the date and the times were written.
    pub(crate) spans: Vec<Range<usize>>,
}

/// The words dates are written with.
#[derive(Debug, Clone)]
pub(crate) struct Words {
    months: Vec<(String, u32)>,
    weekdays: Vec<(String, Weekday)>,
    today: Vec<String>,
    tomorrow: Vec<String>,
    /// Whether 03/04 is the 3rd of April rather than March the 4th.
    day_first: bool,
}

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

const WEEKDAYS: [(&str, Weekday); 7] = [
    ("monday", Weekday::Mon),
    ("tuesday", Weekday::Tue),
    ("wednesday", Weekday::Wed),
    ("thursday", Weekday::Thu),
    ("friday", Weekday::Fri),
    ("saturday", Weekday::Sat),
    ("sunday", Weekday::Sun),
];

/// Words that only say a time follows ("at 3pm", "um 15 Uhr"), dropped from
/// a title with the time.
const LEADS: [&str; 10] = [
    "at", "@", "from", "between", "um", "ab", "von", "de", "dalle", "om",
];

/// Words that join the two ends of a range.
const RANGE: [&str; 11] = [
    "-", "to", "till", "until", "through", "and", "bis", "à", "a", "al", "au",
];

impl Words {
    /// English only, `day_first` deciding 03/04.
    pub(crate) fn english(day_first: bool) -> Self {
        let mut months: Vec<(String, u32)> = MONTHS
            .iter()
            .zip(1..)
            .flat_map(|(name, n)| [(name.to_string(), n), (name[..3].to_string(), n)])
            .collect();
        months.push(("sept".into(), 9));
        let mut weekdays: Vec<(String, Weekday)> = WEEKDAYS
            .iter()
            .flat_map(|(name, d)| [(name.to_string(), *d), (name[..3].to_string(), *d)])
            .collect();
        for (short, d) in [
            ("tues", Weekday::Tue),
            ("thur", Weekday::Thu),
            ("thurs", Weekday::Thu),
        ] {
            weekdays.push((short.into(), d));
        }
        Words {
            months,
            weekdays,
            today: vec!["today".into(), "tonight".into()],
            tomorrow: vec!["tomorrow".into()],
            day_first,
        }
    }

    /// English, and the system's own names and words, with its date order.
    pub(crate) fn system() -> Self {
        use crate::i18n::i18n;
        use gtk::glib;
        let mut w = Self::english(crate::datefmt::day_first());
        let name = |d: &glib::DateTime, pattern: &str| {
            d.format(pattern)
                .map(|s| s.trim().trim_end_matches('.').to_lowercase())
                .unwrap_or_default()
        };
        // %OB and %Ob are the names standing alone; %B and %b the ones in a
        // date, which differ where the month is declined (Russian "ноября").
        for month in 1..=12u32 {
            if let Ok(d) = glib::DateTime::from_utc(2026, month as i32, 15, 12, 0, 0.0) {
                for pattern in ["%B", "%OB", "%b", "%Ob"] {
                    w.months.push((name(&d, pattern), month));
                }
            }
        }
        // 2026-11-16 was a Monday.
        for (i, (_, day)) in WEEKDAYS.iter().enumerate() {
            if let Ok(d) = glib::DateTime::from_utc(2026, 11, 16 + i as i32, 12, 0, 0.0) {
                for pattern in ["%A", "%a"] {
                    w.weekdays.push((name(&d, pattern), *day));
                }
            }
        }
        let list =
            |s: String| -> Vec<String> { s.split_whitespace().map(str::to_lowercase).collect() };
        // Translators: words in a message that mean today, for Create Event
        // from Selection; lower case, separated by spaces. English already works.
        w.today.extend(list(i18n("today tonight")));
        // Translators: words in a message that mean tomorrow, as above.
        w.tomorrow.extend(list(i18n("tomorrow")));
        w.months.retain(|(n, _)| !n.is_empty());
        w.weekdays.retain(|(n, _)| !n.is_empty());
        w
    }

    fn month(&self, word: &str) -> Option<u32> {
        self.months.iter().find(|(n, _)| n == word).map(|(_, m)| *m)
    }

    fn weekday(&self, word: &str) -> Option<Weekday> {
        self.weekdays
            .iter()
            .find(|(n, _)| n == word)
            .map(|(_, d)| *d)
    }
}

/// The date and the times written in `text`. A day written without a year
/// is the next one on or after `reference`, and "tomorrow" is the day after
/// it: the message's own date, which is what its words meant.
pub(crate) fn find(text: &str, words: &Words, reference: NaiveDate) -> Found {
    let toks = tokens(text);
    let p = Parser {
        t: &toks,
        w: words,
        reference,
    };
    let mut found = Found::default();
    let mut i = 0;
    while i < toks.len() && (found.date.is_none() || found.start.is_none()) {
        if found.date.is_none() {
            if let Some((first, last, next)) = p.date(i) {
                found.date = Some(first);
                found.last = last;
                found.spans.push(toks[i].at.start..toks[next - 1].at.end);
                i = next;
                continue;
            }
        }
        if found.start.is_none() {
            if let Some((start, end, next)) = p.time(i) {
                found.start = Some(start);
                found.end = end;
                found.spans.push(toks[i].at.start..toks[next - 1].at.end);
                i = next;
                continue;
            }
        }
        i += 1;
    }
    found
}

/// A title from a short selection ("Lunch with Anna on Friday at 1pm"):
/// the text without the date and the times found in it, or `None` where
/// that leaves nothing, or the selection is more than a line.
pub(crate) fn title(text: &str, spans: &[Range<usize>]) -> Option<String> {
    let short = text.trim();
    if short.contains('\n') || short.chars().count() > 80 {
        return None;
    }
    let toks = tokens(text);
    let mut keep = vec![true; toks.len()];
    for (i, t) in toks.iter().enumerate() {
        if spans
            .iter()
            .any(|s| s.start <= t.at.start && t.at.end <= s.end)
        {
            keep[i] = false;
            // What only led up to it goes with it: "on", "at", "next".
            let mut j = i;
            while j > 0 && is_lead(&toks[j - 1]) {
                j -= 1;
                keep[j] = false;
            }
        }
    }
    let mut out = String::new();
    let mut previous = None;
    for (i, t) in toks.iter().enumerate().filter(|(i, _)| keep[*i]) {
        if !out.is_empty() && (t.gap || previous != Some(i - 1)) {
            out.push(' ');
        }
        out.push_str(&text[t.at.clone()]);
        previous = Some(i);
    }
    let trimmed = out
        .trim_matches(|c: char| c.is_whitespace() || ",;:.!-–—".contains(c))
        .to_string();
    (trimmed.chars().filter(|c| c.is_alphabetic()).count() >= 2).then_some(trimmed)
}

fn is_lead(t: &Tok) -> bool {
    match &t.kind {
        Kind::Word(w) => {
            LEADS.contains(&w.as_str())
                || [
                    "on", "this", "next", "am", "le", "el", "the", "by", "for", "op",
                ]
                .contains(&w.as_str())
        }
        Kind::Mark(c) => *c == '@' || *c == ',',
        Kind::Num(..) => false,
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    /// Lower case.
    Word(String),
    /// The value and how many digits wrote it.
    Num(u32, usize),
    /// Any other character; every dash is '-'.
    Mark(char),
}

#[derive(Debug, Clone)]
struct Tok {
    kind: Kind,
    at: Range<usize>,
    /// Whether white space comes before it.
    gap: bool,
}

fn tokens(text: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    let mut gap = false;
    while let Some((at, c)) = chars.next() {
        if c.is_whitespace() {
            gap = true;
            continue;
        }
        let mut end = at + c.len_utf8();
        let kind = if c.is_alphabetic() {
            while let Some(&(i, n)) = chars.peek() {
                if !n.is_alphabetic() {
                    break;
                }
                end = i + n.len_utf8();
                chars.next();
            }
            Kind::Word(text[at..end].to_lowercase())
        } else if c.is_ascii_digit() {
            while let Some(&(i, n)) = chars.peek() {
                if !n.is_ascii_digit() {
                    break;
                }
                end = i + 1;
                chars.next();
            }
            let digits = &text[at..end];
            Kind::Num(digits.parse().unwrap_or(u32::MAX), digits.len())
        } else if matches!(c, '–' | '—' | '‐' | '‑') {
            Kind::Mark('-')
        } else {
            Kind::Mark(c)
        };
        out.push(Tok {
            kind,
            at: at..end,
            gap,
        });
        gap = false;
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Half {
    Am,
    Pm,
    /// Written on a 24-hour clock: "15h", "15 Uhr", noon.
    Full,
    None,
}

/// A time as written, before a range or "at" says what a bare hour means.
#[derive(Debug, Clone, Copy)]
struct Clock {
    h: u32,
    m: u32,
    half: Half,
    /// Minutes after a colon: a time whatever else is written.
    colon: bool,
}

impl Clock {
    /// Whether it is a time on its own, rather than any number.
    fn plain(&self) -> bool {
        self.half != Half::None || self.colon
    }

    fn resolve(&self) -> Option<NaiveTime> {
        let h = match self.half {
            Half::Am if (1..=12).contains(&self.h) => self.h % 12,
            Half::Pm if (1..=12).contains(&self.h) => self.h % 12 + 12,
            Half::Am | Half::Pm => return None,
            _ => self.h,
        };
        NaiveTime::from_hms_opt(h, self.m, 0)
    }

    /// A bare hour as people say one: "at 3" is in the afternoon.
    fn guessed(mut self) -> Self {
        if self.half == Half::None && (1..=6).contains(&self.h) {
            self.half = Half::Pm;
        }
        self
    }
}

struct Parser<'a> {
    t: &'a [Tok],
    w: &'a Words,
    reference: NaiveDate,
}

impl Parser<'_> {
    fn word(&self, i: usize) -> Option<&str> {
        match &self.t.get(i)?.kind {
            Kind::Word(w) => Some(w),
            Kind::Mark('@') => Some("@"),
            Kind::Mark('-') => Some("-"),
            _ => None,
        }
    }

    fn num(&self, i: usize) -> Option<(u32, usize)> {
        match self.t.get(i)?.kind {
            Kind::Num(n, d) => Some((n, d)),
            _ => None,
        }
    }

    fn mark(&self, i: usize) -> Option<char> {
        match self.t.get(i)?.kind {
            Kind::Mark(c) => Some(c),
            _ => None,
        }
    }

    /// Whether token `i` follows the one before it with no space.
    fn tight(&self, i: usize) -> bool {
        self.t.get(i).is_some_and(|t| !t.gap)
    }

    /// A day number's "st", "th", "er" or German full stop, if it has one.
    fn ordinal(&self, i: usize) -> usize {
        let suffix = self.tight(i)
            && (self.mark(i) == Some('.')
                || self
                    .word(i)
                    .is_some_and(|w| ["st", "nd", "rd", "th", "er", "e"].contains(&w)));
        i + usize::from(suffix)
    }

    fn day(&self, i: usize) -> Option<u32> {
        self.num(i)
            .filter(|&(n, d)| d <= 2 && (1..=31).contains(&n))
            .map(|(n, _)| n)
    }

    fn year(&self, i: usize) -> Option<i32> {
        match self.num(i)? {
            (y, 4) if (1900..=2200).contains(&y) => Some(y as i32),
            _ => None,
        }
    }

    /// A day, or a range of days, starting at token `i`; the index after it.
    fn date(&self, i: usize) -> Option<(NaiveDate, Option<NaiveDate>, usize)> {
        let word = self.word(i);
        if word.is_some_and(|w| self.w.today.iter().any(|t| t == w)) {
            return Some((self.reference, None, i + 1));
        }
        if word.is_some_and(|w| self.w.tomorrow.iter().any(|t| t == w)) {
            return Some((self.reference + Duration::days(1), None, i + 1));
        }
        let next = word == Some("next");
        let j = i + usize::from(next || word == Some("this"));
        if let Some(day) = self.word(j).and_then(|w| self.w.weekday(w)) {
            // "Thu." and "Thursday, 20 November".
            let mut k = j + 1;
            if self.mark(k) == Some('.') && self.tight(k) {
                k += 1;
            }
            if self.mark(k) == Some(',') {
                k += 1;
            }
            if let Some(found) = self.written(k).or_else(|| self.numeric(k)) {
                return Some(found);
            }
            return Some((upcoming(self.reference, day, next), None, j + 1));
        }
        self.written(i).or_else(|| self.numeric(i))
    }

    /// "November 20th, 2026", "Nov 20-22", "20 November", "20. November",
    /// "1er novembre", "the 20th of November".
    fn written(&self, i: usize) -> Option<(NaiveDate, Option<NaiveDate>, usize)> {
        let i = i + usize::from(self.word(i) == Some("the"));
        let (month, first, last, mut k) =
            if let Some(month) = self.word(i).and_then(|w| self.w.month(w)) {
                let mut k = i + 1;
                if self.mark(k) == Some('.') && self.tight(k) {
                    k += 1;
                }
                let first = self.day(k)?;
                k = self.ordinal(k + 1);
                let mut last = None;
                if self.word(k).is_some_and(|w| RANGE.contains(&w)) {
                    if let Some(d) = self.day(k + 1) {
                        last = Some(d);
                        k = self.ordinal(k + 2);
                    }
                }
                (month, first, last, k)
            } else {
                let first = self.day(i)?;
                let mut k = self.ordinal(i + 1);
                let mut last = None;
                if self.word(k).is_some_and(|w| RANGE.contains(&w)) {
                    if let Some(d) = self.day(k + 1) {
                        last = Some(d);
                        k = self.ordinal(k + 2);
                    }
                }
                if self.word(k).is_some_and(|w| ["of", "de"].contains(&w)) {
                    k += 1;
                }
                let month = self.word(k).and_then(|w| self.w.month(w))?;
                k += 1;
                if self.mark(k) == Some('.') && self.tight(k) {
                    k += 1;
                }
                (month, first, last, k)
            };
        let comma = usize::from(self.mark(k) == Some(','));
        let year = self.year(k + comma);
        if year.is_some() {
            k += comma + 1;
        }
        let first_date = self.on(year, month, first)?;
        let last_date = last
            .and_then(|d| NaiveDate::from_ymd_opt(first_date.year(), month, d))
            .filter(|d| *d > first_date);
        Some((first_date, last_date, k))
    }

    /// "2026-11-20", "20/11/2026", "11/20", "20.11.", with the order the
    /// system writes dates in where the numbers do not decide it.
    fn numeric(&self, i: usize) -> Option<(NaiveDate, Option<NaiveDate>, usize)> {
        let (a, a_digits) = self.num(i)?;
        let sep = self.mark(i + 1).filter(|c| matches!(c, '/' | '.' | '-'))?;
        if !self.tight(i + 1) || !self.tight(i + 2) {
            return None;
        }
        let (b, b_digits) = self.num(i + 2)?;
        let third = (self.mark(i + 3) == Some(sep) && self.tight(i + 3) && self.tight(i + 4))
            .then(|| self.num(i + 4))
            .flatten();
        if a_digits == 4 {
            let (d, _) = third?;
            let date = NaiveDate::from_ymd_opt(a as i32, b, d)?;
            return Some((date, None, i + 5));
        }
        if a_digits > 2 || b_digits > 2 {
            return None;
        }
        let (year, mut k) = match third {
            Some((y, 4)) => (Some(y as i32), i + 5),
            Some((y, 2)) => (Some(2000 + y as i32), i + 5),
            Some(_) => return None,
            // 3-4 is more often a range of hours than a date.
            None if sep == '-' => return None,
            None => (None, i + 3),
        };
        // "3.30pm" and "15.30 Uhr" are times.
        if year.is_none() && self.word(k).is_some_and(|w| is_half(w).is_some()) {
            return None;
        }
        if year.is_none() && sep == '.' && self.mark(k) == Some('.') && self.tight(k) {
            k += 1;
        }
        let day_first = if a > 12 {
            true
        } else if b > 12 {
            false
        } else {
            // A dotted date is a European one.
            sep == '.' || self.w.day_first
        };
        let (day, month) = if day_first { (a, b) } else { (b, a) };
        Some((self.on(year, month, day)?, None, k))
    }

    /// The day `day` of `month` in `year`, or the next on or after the
    /// reference day when no year is given.
    fn on(&self, year: Option<i32>, month: u32, day: u32) -> Option<NaiveDate> {
        if let Some(y) = year {
            return NaiveDate::from_ymd_opt(y, month, day);
        }
        let this = self.reference.year();
        (this..=this + 4)
            .filter_map(|y| NaiveDate::from_ymd_opt(y, month, day))
            .find(|d| *d >= self.reference)
    }

    /// A time or a range of times starting at token `i`.
    fn time(&self, i: usize) -> Option<(NaiveTime, Option<NaiveTime>, usize)> {
        let lead = self.word(i).filter(|w| LEADS.contains(w));
        let j = i + usize::from(lead.is_some());
        let (first, k) = self.clock(j)?;
        if self.word(k).is_some_and(|w| RANGE.contains(&w)) {
            if let Some((second, e)) = self.clock(k + 1) {
                if first.plain() || second.plain() || lead.is_some() {
                    let (a, b) = pair(first, second);
                    if let (Some(a), Some(b)) = (a.resolve(), b.resolve()) {
                        return Some((a, Some(b), e));
                    }
                }
            }
        }
        let single = if first.plain() {
            first
        } else if lead.is_some_and(|w| w == "at" || w == "@") && (1..=12).contains(&first.h) {
            first.guessed()
        } else {
            return None;
        };
        Some((single.resolve()?, None, k))
    }

    /// One time as written, and the index after it.
    fn clock(&self, i: usize) -> Option<(Clock, usize)> {
        if self
            .word(i)
            .is_some_and(|w| ["noon", "midday"].contains(&w))
        {
            let noon = Clock {
                h: 12,
                m: 0,
                half: Half::Full,
                colon: false,
            };
            return Some((noon, i + 1));
        }
        let (h, digits) = self.num(i)?;
        if digits > 2 || h > 23 {
            return None;
        }
        let mut c = Clock {
            h,
            m: 0,
            half: Half::None,
            colon: false,
        };
        let mut k = i + 1;
        let minutes = |at: usize| {
            self.num(at)
                .filter(|&(m, d)| d == 2 && m < 60 && self.tight(at))
                .map(|(m, _)| m)
        };
        let sep = self.mark(k).filter(|c| matches!(c, ':' | '.'));
        if let (Some(sep), Some(m)) = (sep.filter(|_| self.tight(k)), minutes(k + 1)) {
            c.m = m;
            c.colon = sep == ':';
            k += 2;
        }
        // "15h", "15h30"
        if c.m == 0 && self.word(k) == Some("h") && self.tight(k) {
            c.half = Half::Full;
            k += 1;
            if let Some(m) = minutes(k) {
                c.m = m;
                k += 1;
            }
            return Some((c, k));
        }
        if let Some(w) = self.word(k) {
            if let Some(half) = is_half(w) {
                c.half = half;
                k += 1;
                // "p.m."
                if self.mark(k) == Some('.') && self.tight(k) {
                    k += 1;
                }
                return Some((c, k));
            }
            if (w == "a" || w == "p")
                && self.mark(k + 1) == Some('.')
                && self.word(k + 2) == Some("m")
            {
                c.half = if w == "a" { Half::Am } else { Half::Pm };
                k += 3;
                if self.mark(k) == Some('.') && self.tight(k) {
                    k += 1;
                }
                return Some((c, k));
            }
        }
        // A dotted time with nothing after it is as likely a date or a
        // number: 15.30 is a time only as "15.30 Uhr" or "3.30pm".
        if sep == Some('.') && !c.colon && c.m != 0 {
            return None;
        }
        Some((c, k))
    }
}

/// What a word after a number says of it: "pm", "Uhr".
fn is_half(w: &str) -> Option<Half> {
    match w {
        "am" => Some(Half::Am),
        "pm" => Some(Half::Pm),
        "uhr" | "h" => Some(Half::Full),
        _ => None,
    }
}

/// Both ends of "3-4pm" or "11am-1": a bare end takes the other's half of
/// the day, unless that puts it the wrong side of the other.
fn pair(mut a: Clock, mut b: Clock) -> (Clock, Clock) {
    let minutes = |c: &Clock| c.resolve().map(|t| t.num_seconds_from_midnight());
    match (a.half, b.half) {
        (Half::None, Half::Am | Half::Pm) => {
            a.half = b.half;
            if minutes(&a) > minutes(&b) && b.half == Half::Pm {
                a.half = Half::Am;
            }
        }
        (Half::Am | Half::Pm, Half::None) => {
            b.half = a.half;
            if minutes(&b) < minutes(&a) && a.half == Half::Am {
                b.half = Half::Pm;
            }
        }
        (Half::None, Half::None) if !a.colon && !b.colon => {
            a = a.guessed();
            b = b.guessed();
        }
        _ => {}
    }
    (a, b)
}

/// The next `day` from `from`: `from` itself when it is that day, unless
/// `next` says a later one.
fn upcoming(from: NaiveDate, day: Weekday, next: bool) -> NaiveDate {
    let ahead = (7 + day.num_days_from_monday() - from.weekday().num_days_from_monday()) % 7;
    let ahead = if ahead == 0 && next { 7 } else { ahead };
    from + Duration::days(i64::from(ahead))
}
