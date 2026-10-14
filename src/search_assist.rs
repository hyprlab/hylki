//! What the search field suggests while it is typed into, and the chips a
//! chosen suggestion becomes. A chip is one condition of the search ("From:
//! Anna", "Since 1 Oct"); the chips and whatever is still typed make up the
//! search text the list and the servers read (see `mail_search`), so typing
//! that text by hand stays the same search.
//!
//! The words the field understands follow the interface language: the
//! keywords (`from:`, or `von:` in German) and the words for dates ("today",
//! "last week", the months) are translated, each translation may give
//! several spellings separated by commas, and the English ones always work
//! too. They are read through `Vocab`, which tests fill with their own
//! words.

use chrono::{Datelike, NaiveDate};

use crate::i18n::{i18n, i18n_f, i18n_noop};
use crate::mail_search::{parse_date, Field};

/// One condition of a search, shown as a chip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// A person, by address, in one of the address fields.
    Person { field: Field, name: String, email: String },
    /// Text in one field, or in any (`Field::Any`).
    Text { field: Field, text: String },
    /// Mail dated on or after `after` and before `before` (either open).
    Date { after: Option<NaiveDate>, before: Option<NaiveDate> },
    /// Mail with an attachment.
    HasAttachment,
}

/// The fields a person chip can be switched between.
pub const PERSON_FIELDS: [Field; 4] = [Field::From, Field::To, Field::Cc, Field::ReplyTo];

/// The fields a text chip can be switched between.
pub const TEXT_FIELDS: [Field; 4] = [Field::Any, Field::Subject, Field::Body, Field::Attachment];

/// A value in the search syntax: as it is when it is one plain word, quoted
/// otherwise.
fn quoted(value: &str) -> String {
    let plain = !value.is_empty()
        && !value.contains(|c: char| c.is_whitespace() || matches!(c, '"' | '(' | ')' | ':'))
        && !matches!(value, "AND" | "OR" | "NOT")
        && !value.starts_with('-');
    if plain {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('"', "\\\""))
    }
}

fn key(field: Field) -> &'static str {
    match field {
        Field::From => "from",
        Field::To => "to",
        Field::Cc => "cc",
        Field::ReplyTo => "replyto",
        Field::Subject => "subject",
        Field::Body => "body",
        Field::Attachment => "attachment",
        Field::Any => "",
    }
}

impl Token {
    /// The token in the search syntax, with the English keywords, which
    /// read the same in every language.
    pub fn to_query(&self) -> String {
        match self {
            Token::Person { field, email, .. } => format!("{}:{}", key(*field), quoted(email)),
            Token::Text { field: Field::Any, text } => quoted(text),
            Token::Text { field, text } => format!("{}:{}", key(*field), quoted(text)),
            Token::Date { after, before } => {
                let mut parts = Vec::new();
                if let Some(d) = after {
                    parts.push(format!("after:{}", d.format("%Y-%m-%d")));
                }
                if let Some(d) = before {
                    parts.push(format!("before:{}", d.format("%Y-%m-%d")));
                }
                parts.join(" ")
            }
            Token::HasAttachment => "has:attachment".to_string(),
        }
    }

    /// The chip's text.
    pub fn label(&self) -> String {
        match self {
            Token::Person { field, name, email } => {
                let who = if name.trim().is_empty() { email.as_str() } else { name.as_str() };
                field_label(*field, who)
            }
            Token::Text { field, text } => field_label(*field, text),
            Token::Date { after, before } => date_label(*after, *before),
            Token::HasAttachment => i18n("Has an attachment"),
        }
    }

    /// The same condition on another field, for a chip's field menu.
    pub fn with_field(&self, to: Field) -> Token {
        match self {
            Token::Person { name, email, .. } => Token::Person { field: to, name: name.clone(), email: email.clone() },
            Token::Text { text, .. } => Token::Text { field: to, text: text.clone() },
            other => other.clone(),
        }
    }

    /// The fields this chip can be switched to, or none.
    pub fn fields(&self) -> &'static [Field] {
        match self {
            Token::Person { .. } => &PERSON_FIELDS,
            Token::Text { .. } => &TEXT_FIELDS,
            _ => &[],
        }
    }
}

/// "From: Anna", "Subject: report" and so on.
pub fn field_label(field: Field, value: &str) -> String {
    let template = match field {
        Field::From => i18n("From: {value}"),
        Field::To => i18n("To: {value}"),
        Field::Cc => i18n("Cc: {value}"),
        Field::ReplyTo => i18n("Reply-To: {value}"),
        Field::Subject => i18n("Subject: {value}"),
        Field::Body => i18n("Message: {value}"),
        Field::Attachment => i18n("Attachment: {value}"),
        Field::Any => i18n("Anywhere: {value}"),
    };
    template.replace("{value}", value)
}

/// What a field is called in a chip's menu.
pub fn field_name(field: Field) -> String {
    match field {
        Field::From => i18n("From"),
        Field::To => i18n("To"),
        Field::Cc => i18n("Cc"),
        Field::ReplyTo => i18n("Reply-To"),
        Field::Subject => i18n("Subject"),
        Field::Body => i18n("Message text"),
        Field::Attachment => i18n("Attachment"),
        Field::Any => i18n("Anywhere"),
    }
}

/// A date the way the chips show it: the day, with the year only when it is
/// not this one.
fn short_date(d: NaiveDate, today: NaiveDate) -> String {
    // Midday, so no clock change moves the day.
    let ts = crate::mail_search::local_midnight(d) + 12 * 3600;
    if d.year() == today.year() {
        crate::datefmt::day_month(ts)
    } else {
        crate::datefmt::day_month_year(ts)
    }
}

fn date_label(after: Option<NaiveDate>, before: Option<NaiveDate>) -> String {
    let today = chrono::Local::now().date_naive();
    match (after, before) {
        (Some(a), Some(b)) if a.succ_opt() == Some(b) => i18n_f("On {date}", &[("date", &short_date(a, today))]),
        (Some(a), Some(b)) => i18n_f(
            "From {start} to {end}",
            &[("start", &short_date(a, today)), ("end", &short_date(b.pred_opt().unwrap_or(b), today))],
        ),
        (Some(a), None) => i18n_f("Since {date}", &[("date", &short_date(a, today))]),
        (None, Some(b)) => i18n_f("Before {date}", &[("date", &short_date(b, today))]),
        (None, None) => i18n("Any date"),
    }
}

/// The search field's words in the interface language. Each entry is a
/// msgid of the form "search keyword|from": translators give the word only
/// ("von"), or several spellings separated by commas ("seit,nach"), and the
/// part before the bar is never shown.
pub struct Vocab {
    translate: Box<dyn Fn(&str) -> String>,
}

impl Vocab {
    /// The words of the language the interface is in.
    pub fn current() -> Vocab {
        Vocab { translate: Box::new(i18n) }
    }

    #[cfg(test)]
    pub fn with(translate: impl Fn(&str) -> String + 'static) -> Vocab {
        Vocab { translate: Box::new(translate) }
    }

    /// Every spelling of one word: the English one, then the translated ones,
    /// lowercased.
    fn words(&self, msgid: &str) -> Vec<String> {
        let english = msgid.rsplit('|').next().unwrap_or(msgid).to_lowercase();
        let translated = (self.translate)(msgid);
        let translated = translated.rsplit('|').next().unwrap_or(&translated).to_lowercase();
        let mut out = vec![english];
        for w in translated.split(',').map(str::trim).filter(|w| !w.is_empty()) {
            if !out.iter().any(|o| o == w) {
                out.push(w.to_string());
            }
        }
        out
    }

    /// The word shown for a keyword in this language: the first translated
    /// spelling.
    fn first(&self, msgid: &str) -> String {
        self.words(msgid).last().cloned().unwrap_or_default()
    }

    /// The field a typed keyword (without its colon) stands for, in any of
    /// its spellings, or `None` when it is not one.
    pub fn field_key(&self, typed: &str) -> Option<KeyWord> {
        let typed = typed.to_lowercase();
        for (msgid, word) in keyword_ids() {
            if self.words(msgid).contains(&typed) {
                return Some(word);
            }
        }
        None
    }
}

/// What a keyword in the search text is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyWord {
    Field(Field),
    Before,
    After,
}

fn keyword_ids() -> [(&'static str, KeyWord); 9] {
    [
        // Translators: a keyword of the search field, typed before a colon
        // ("from:anna"). Give the word only; several spellings may be given,
        // separated by commas. The English word always works too.
        (i18n_noop("search keyword|from"), KeyWord::Field(Field::From)),
        (i18n_noop("search keyword|to"), KeyWord::Field(Field::To)),
        (i18n_noop("search keyword|cc"), KeyWord::Field(Field::Cc)),
        (i18n_noop("search keyword|replyto"), KeyWord::Field(Field::ReplyTo)),
        (i18n_noop("search keyword|subject"), KeyWord::Field(Field::Subject)),
        (i18n_noop("search keyword|body"), KeyWord::Field(Field::Body)),
        (i18n_noop("search keyword|attachment"), KeyWord::Field(Field::Attachment)),
        (i18n_noop("search keyword|before"), KeyWord::Before),
        (i18n_noop("search keyword|after"), KeyWord::After),
    ]
}

/// The translatable words for dates, each with what it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DateWord {
    Today,
    Yesterday,
    ThisWeek,
    LastWeek,
    ThisMonth,
    LastMonth,
    ThisYear,
    LastYear,
    Month(u32),
}

fn date_ids() -> Vec<(&'static str, DateWord)> {
    vec![
        // Translators: a word the search field reads as a date. Give the
        // word only; several spellings may be given, separated by commas.
        (i18n_noop("search date|today"), DateWord::Today),
        (i18n_noop("search date|yesterday"), DateWord::Yesterday),
        (i18n_noop("search date|this week"), DateWord::ThisWeek),
        (i18n_noop("search date|last week"), DateWord::LastWeek),
        (i18n_noop("search date|this month"), DateWord::ThisMonth),
        (i18n_noop("search date|last month"), DateWord::LastMonth),
        (i18n_noop("search date|this year"), DateWord::ThisYear),
        (i18n_noop("search date|last year"), DateWord::LastYear),
        (i18n_noop("search month|january,jan"), DateWord::Month(1)),
        (i18n_noop("search month|february,feb"), DateWord::Month(2)),
        (i18n_noop("search month|march,mar"), DateWord::Month(3)),
        (i18n_noop("search month|april,apr"), DateWord::Month(4)),
        (i18n_noop("search month|may"), DateWord::Month(5)),
        (i18n_noop("search month|june,jun"), DateWord::Month(6)),
        (i18n_noop("search month|july,jul"), DateWord::Month(7)),
        (i18n_noop("search month|august,aug"), DateWord::Month(8)),
        (i18n_noop("search month|september,sep,sept"), DateWord::Month(9)),
        (i18n_noop("search month|october,oct"), DateWord::Month(10)),
        (i18n_noop("search month|november,nov"), DateWord::Month(11)),
        (i18n_noop("search month|december,dec"), DateWord::Month(12)),
    ]
}

fn month_start(year: i32, month: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, 1).expect("a month starts on its first")
}

fn next_month(d: NaiveDate) -> NaiveDate {
    if d.month() == 12 {
        month_start(d.year() + 1, 1)
    } else {
        month_start(d.year(), d.month() + 1)
    }
}

/// The days a date word covers, `[after, before)`, counted from `today`.
/// Weeks start on Monday.
fn date_range(word: DateWord, today: NaiveDate) -> (NaiveDate, NaiveDate) {
    let day = |n: i64| today + chrono::Duration::days(n);
    let monday = day(-(today.weekday().num_days_from_monday() as i64));
    let this_month = month_start(today.year(), today.month());
    match word {
        DateWord::Today => (today, day(1)),
        DateWord::Yesterday => (day(-1), today),
        DateWord::ThisWeek => (monday, day(1)),
        DateWord::LastWeek => (monday - chrono::Duration::days(7), monday),
        DateWord::ThisMonth => (this_month, day(1)),
        DateWord::LastMonth => {
            let start = (this_month - chrono::Duration::days(1)).with_day(1).expect("first of a month");
            (start, this_month)
        }
        DateWord::ThisYear => (month_start(today.year(), 1), day(1)),
        DateWord::LastYear => (month_start(today.year() - 1, 1), month_start(today.year(), 1)),
        DateWord::Month(m) => {
            // The last such month that has begun: September in October is
            // this year's, December in October last year's.
            let year = if m <= today.month() { today.year() } else { today.year() - 1 };
            let start = month_start(year, m);
            (start, next_month(start))
        }
    }
}

/// One suggestion under the search field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub token: Token,
    /// What the row says.
    pub title: String,
    /// The smaller line under it, if any (a person's address).
    pub detail: String,
}

/// Most people suggested at once.
const PEOPLE: usize = 4;

/// What to suggest for `typed`, the text after the chips. `people` are
/// known names and addresses, most used first. Empty text suggests a few
/// common conditions to start from.
pub fn suggest(typed: &str, people: &[(String, String)], vocab: &Vocab, today: NaiveDate) -> Vec<Suggestion> {
    let typed = typed.trim();
    let mut out = Vec::new();
    if typed.is_empty() {
        for word in [DateWord::Today, DateWord::ThisWeek, DateWord::LastMonth] {
            out.push(word_suggestion(word, today));
        }
        out.push(Suggestion {
            token: Token::HasAttachment,
            title: Token::HasAttachment.label(),
            detail: String::new(),
        });
        return out;
    }
    // A keyword already typed ("from:an") narrows what is offered.
    let (key, value) = match typed.split_once(':') {
        Some((k, v)) => match vocab.field_key(k) {
            Some(word) => (Some(word), v.trim()),
            None => (None, typed),
        },
        None => (None, typed),
    };
    suggest_for(key, value, people, vocab, today)
}

/// What to suggest for `value` in the field `key` stands for (a keyword
/// badge in the search field), or in any field when there is none.
pub fn suggest_for(
    key: Option<KeyWord>,
    value: &str,
    people: &[(String, String)],
    vocab: &Vocab,
    today: NaiveDate,
) -> Vec<Suggestion> {
    let value = value.trim();
    let mut out = Vec::new();
    if value.is_empty() {
        // Only a keyword so far: what it can be followed by.
        match key {
            Some(KeyWord::Field(f)) if PERSON_FIELDS.contains(&f) => {
                for (name, email) in people.iter().take(PEOPLE) {
                    let token = Token::Person { field: f, name: name.clone(), email: email.clone() };
                    let detail = if name.trim().is_empty() || name == email { String::new() } else { email.clone() };
                    out.push(Suggestion { title: token.label(), token, detail });
                }
            }
            Some(k @ (KeyWord::Before | KeyWord::After)) => {
                for word in [DateWord::Today, DateWord::LastWeek, DateWord::LastMonth, DateWord::LastYear] {
                    let (start, _) = date_range(word, today);
                    let token = match k {
                        KeyWord::Before => Token::Date { after: None, before: Some(start) },
                        _ => Token::Date { after: Some(start), before: None },
                    };
                    out.push(Suggestion { title: token.label(), detail: word_title(word, start), token });
                }
            }
            _ => {}
        }
        return out;
    }
    let lower = value.to_lowercase();
    let wants_person = matches!(key, None | Some(KeyWord::Field(Field::From | Field::To | Field::Cc | Field::ReplyTo)));
    let person_field = match key {
        Some(KeyWord::Field(f)) => f,
        _ => Field::From,
    };
    if wants_person {
        let mut matched: Vec<&(String, String)> = people
            .iter()
            .filter(|(name, email)| name.to_lowercase().contains(&lower) || email.to_lowercase().contains(&lower))
            .collect();
        // Names and addresses that start with the text come first; the list
        // is already most used first within each group.
        matched.sort_by_key(|(name, email)| {
            let starts = name.to_lowercase().split_whitespace().any(|w| w.starts_with(&lower))
                || email.to_lowercase().starts_with(&lower);
            !starts
        });
        for (name, email) in matched.into_iter().take(PEOPLE) {
            let token = Token::Person { field: person_field, name: name.clone(), email: email.clone() };
            let detail = if name.trim().is_empty() || name == email { String::new() } else { email.clone() };
            out.push(Suggestion { title: token.label(), token, detail });
        }
    }
    if matches!(key, None | Some(KeyWord::Before) | Some(KeyWord::After)) {
        out.extend(date_suggestions(value, key, vocab, today));
    }
    let text_fields: &[Field] = match key {
        None => &[Field::Subject, Field::Body, Field::Attachment],
        Some(KeyWord::Field(f)) if !PERSON_FIELDS.contains(&f) => std::slice::from_ref(match f {
            Field::Subject => &Field::Subject,
            Field::Body => &Field::Body,
            _ => &Field::Attachment,
        }),
        Some(KeyWord::Field(f)) => match f {
            // A person field with no one known by that text: the text itself.
            Field::From => &[Field::From],
            Field::To => &[Field::To],
            Field::Cc => &[Field::Cc],
            _ => &[Field::ReplyTo],
        },
        _ => &[],
    };
    for field in text_fields {
        let token = Token::Text { field: *field, text: value.to_string() };
        out.push(Suggestion { title: token.label(), token, detail: String::new() });
    }
    if key.is_none() && vocab_matches_attachment(&lower, vocab) {
        out.push(Suggestion {
            token: Token::HasAttachment,
            title: Token::HasAttachment.label(),
            detail: String::new(),
        });
    }
    out
}

/// Whether the text is the start of the word for "attachment", so that
/// "Has an attachment" is offered.
fn vocab_matches_attachment(lower: &str, vocab: &Vocab) -> bool {
    lower.chars().count() >= 3 && vocab.words("search keyword|attachment").iter().any(|w| w.starts_with(lower))
}

/// A date word as a suggestion: its name, and the days it covers under it.
fn word_suggestion(word: DateWord, today: NaiveDate) -> Suggestion {
    let (after, before) = date_range(word, today);
    let token = Token::Date { after: Some(after), before: Some(before) };
    Suggestion { title: word_title(word, after), detail: token.label(), token }
}

/// What a date word is called in the suggestions.
fn word_title(word: DateWord, start: NaiveDate) -> String {
    match word {
        DateWord::Today => i18n("Today"),
        DateWord::Yesterday => i18n("Yesterday"),
        DateWord::ThisWeek => i18n("This week"),
        DateWord::LastWeek => i18n("Last week"),
        DateWord::ThisMonth => i18n("This month"),
        DateWord::LastMonth => i18n("Last month"),
        DateWord::ThisYear => i18n("This year"),
        DateWord::LastYear => i18n("Last year"),
        // The month by its name in the interface's language, and its year.
        DateWord::Month(_) => gtk::glib::DateTime::from_local(start.year(), start.month() as i32, 1, 12, 0, 0.0)
            .ok()
            .and_then(|d| d.format("%B %Y").ok())
            .map(|s| s.to_string())
            .unwrap_or_else(|| start.format("%Y-%m").to_string()),
    }
}

/// Dates the text can be read as: a word ("yesterday", "September", or the
/// start of one), a year, or a date. A date offers "on", "since" and
/// "before" it; `key` picks one when `before:`/`after:` was typed.
fn date_suggestions(value: &str, key: Option<KeyWord>, vocab: &Vocab, today: NaiveDate) -> Vec<Suggestion> {
    let lower = value.to_lowercase();
    let mut out = Vec::new();
    let mut ranges: Vec<(NaiveDate, NaiveDate)> = Vec::new();
    let mut words: Vec<DateWord> = Vec::new();
    for (msgid, word) in date_ids() {
        // A word counts once enough of it is typed to tell it from others.
        let hit = vocab
            .words(msgid)
            .iter()
            .flat_map(|w| w.split(',').map(str::to_string).collect::<Vec<_>>())
            .any(|w| w == lower || (lower.chars().count() >= 3 && w.starts_with(&lower)));
        if hit {
            let r = date_range(word, today);
            if key.is_none() {
                if !words.iter().any(|w| date_range(*w, today) == r) {
                    words.push(word);
                }
            } else if !ranges.contains(&r) {
                ranges.push(r);
            }
        }
    }
    out.extend(words.into_iter().map(|w| word_suggestion(w, today)));
    if let Ok(year) = lower.parse::<i32>() {
        if (1990..=today.year()).contains(&year) {
            ranges.push((month_start(year, 1), month_start(year + 1, 1)));
        }
    }
    for r in ranges {
        let (after, before) = match key {
            Some(KeyWord::Before) => (None, Some(r.0)),
            Some(KeyWord::After) => (Some(r.0), None),
            _ => (Some(r.0), Some(r.1)),
        };
        let token = Token::Date { after, before };
        out.push(Suggestion { title: token.label(), token, detail: String::new() });
    }
    if let Some(d) = parse_date(value).or_else(|| parse_short_date(value, today)) {
        let next = d.succ_opt().unwrap_or(d);
        let options: Vec<(Option<NaiveDate>, Option<NaiveDate>)> = match key {
            Some(KeyWord::Before) => vec![(None, Some(d))],
            Some(KeyWord::After) => vec![(Some(d), None)],
            _ => vec![(Some(d), Some(next)), (Some(d), None), (None, Some(d))],
        };
        for (after, before) in options {
            let token = Token::Date { after, before };
            out.push(Suggestion { title: token.label(), token, detail: String::new() });
        }
    }
    out
}

/// "1.10." or "1.10" as this year's day (the year it was most recently).
fn parse_short_date(s: &str, today: NaiveDate) -> Option<NaiveDate> {
    let s = s.trim().trim_end_matches('.');
    let (d, m) = s.split_once('.')?;
    let (d, m): (u32, u32) = (d.parse().ok()?, m.parse().ok()?);
    let this = NaiveDate::from_ymd_opt(today.year(), m, d)?;
    if this > today {
        NaiveDate::from_ymd_opt(today.year() - 1, m, d)
    } else {
        Some(this)
    }
}

/// A field keyword at the start of the text, and the text after it: what
/// the search field turns into a badge as it is typed ("from:", "von:").
/// Quoted text and unknown keys are left as they are.
pub fn leading_keyword(typed: &str, vocab: &Vocab) -> Option<(KeyWord, String)> {
    let trimmed = typed.trim_start();
    let (key, rest) = trimmed.split_once(':')?;
    if key.is_empty() || key.contains(|c: char| c.is_whitespace() || c == '"' || c == '(') {
        return None;
    }
    let word = vocab.field_key(key.trim_start_matches('-'))?;
    if key.starts_with('-') {
        return None;
    }
    Some((word, rest.trim_start().to_string()))
}

/// What a keyword badge and its value add to the search: nothing until a
/// value is typed, and for a date only once the value reads as one.
pub fn pending_query(key: KeyWord, value: &str, today: NaiveDate) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }
    match key {
        KeyWord::Field(field) => Token::Text { field, text: value.to_string() }.to_query(),
        KeyWord::Before | KeyWord::After => match parse_date(value).or_else(|| parse_short_date(value, today)) {
            Some(d) => {
                let (after, before) = if key == KeyWord::After { (Some(d), None) } else { (None, Some(d)) };
                Token::Date { after, before }.to_query()
            }
            None => String::new(),
        },
    }
}

/// The badge's text: the field as the interface language calls it.
pub fn keyword_badge(key: KeyWord) -> String {
    match key {
        KeyWord::Field(f) => format!("{}:", field_name(f)),
        KeyWord::Before => i18n("Before:"),
        KeyWord::After => i18n("Since:"),
    }
}

/// The whole search text: the chips, then what is still typed.
pub fn compose(tokens: &[Token], typed: &str) -> String {
    let mut parts: Vec<String> = tokens.iter().map(Token::to_query).filter(|q| !q.is_empty()).collect();
    if !typed.trim().is_empty() {
        parts.push(typed.trim().to_string());
    }
    parts.join(" ")
}

/// The keyword as the interface language spells it, for the field's hint.
pub fn localized_keyword(field: Field) -> String {
    let vocab = Vocab::current();
    let msgid = keyword_ids().into_iter().find(|(_, w)| *w == KeyWord::Field(field)).map(|(m, _)| m).unwrap_or("");
    vocab.first(msgid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn english() -> Vocab {
        Vocab::with(|s| s.to_string())
    }

    fn german() -> Vocab {
        Vocab::with(|s| {
            match s {
                "search keyword|from" => "von",
                "search keyword|to" => "an",
                "search keyword|subject" => "betreff",
                "search keyword|attachment" => "anhang",
                "search keyword|before" => "vor,bis",
                "search keyword|after" => "seit,nach",
                "search date|yesterday" => "gestern",
                "search date|last week" => "letzte woche",
                "search month|september,sep,sept" => "september,sep,sept",
                "search month|march,mar" => "märz,mär",
                other => other,
            }
            .to_string()
        })
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn people() -> Vec<(String, String)> {
        vec![
            ("Anna Müller".into(), "anna@example.org".into()),
            ("Hannah Berg".into(), "hb@example.org".into()),
            ("".into(), "news@shop.example".into()),
        ]
    }

    #[test]
    fn tokens_read_back_as_the_search_they_stand_for() {
        let tokens = vec![
            Token::Person { field: Field::From, name: "Anna Müller".into(), email: "anna@example.org".into() },
            Token::Text { field: Field::Subject, text: "quarterly report".into() },
            Token::Date { after: Some(day(2026, 10, 1)), before: None },
            Token::HasAttachment,
        ];
        let text = compose(&tokens, " invoice ");
        assert_eq!(text, "from:anna@example.org subject:\"quarterly report\" after:2026-10-01 has:attachment invoice");
        // It is the same search the parser reads.
        let q = crate::mail_search::Query::parse(&text).unwrap();
        assert_eq!(q.terms().len(), 3);
    }

    #[test]
    fn odd_values_are_quoted() {
        assert_eq!(Token::Text { field: Field::Any, text: "OR".into() }.to_query(), "\"OR\"");
        assert_eq!(Token::Text { field: Field::Any, text: "-x".into() }.to_query(), "\"-x\"");
        assert_eq!(Token::Text { field: Field::Body, text: "a\"b".into() }.to_query(), "body:\"a\\\"b\"");
        assert!(crate::mail_search::Query::parse(
            &Token::Text { field: Field::Body, text: "a\"b (c)".into() }.to_query()
        )
        .is_ok());
    }

    #[test]
    fn a_name_suggests_the_person_and_the_text() {
        let s = suggest("ann", &people(), &english(), day(2026, 10, 14));
        let kinds: Vec<&Token> = s.iter().map(|s| &s.token).collect();
        assert!(matches!(kinds[0], Token::Person { field: Field::From, email, .. } if email == "anna@example.org"));
        assert!(
            matches!(kinds[1], Token::Person { email, .. } if email == "hb@example.org"),
            "contains, after starts-with"
        );
        assert!(kinds.iter().any(|t| **t == Token::Text { field: Field::Subject, text: "ann".into() }));
        assert!(kinds.iter().any(|t| **t == Token::Text { field: Field::Body, text: "ann".into() }));
        assert_eq!(s[0].detail, "anna@example.org");
    }

    #[test]
    fn a_typed_keyword_narrows_the_suggestions_in_any_language() {
        for (typed, vocab) in [("to:hann", english()), ("an:hann", german())] {
            let s = suggest(typed, &people(), &vocab, day(2026, 10, 14));
            assert!(
                matches!(&s[0].token, Token::Person { field: Field::To, email, .. } if email == "hb@example.org"),
                "{typed}"
            );
            assert!(!s.iter().any(|x| matches!(x.token, Token::Text { field: Field::Subject, .. })), "{typed}");
        }
        let s = suggest("betreff:bericht", &people(), &german(), day(2026, 10, 14));
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].token, Token::Text { field: Field::Subject, text: "bericht".into() });
    }

    #[test]
    fn date_words_become_ranges_in_any_language() {
        let today = day(2026, 10, 14); // a Wednesday
        let first = |typed: &str, vocab: &Vocab| suggest(typed, &[], vocab, today)[0].token.clone();
        assert_eq!(
            first("yesterday", &english()),
            Token::Date { after: Some(day(2026, 10, 13)), before: Some(day(2026, 10, 14)) }
        );
        assert_eq!(
            first("gestern", &german()),
            Token::Date { after: Some(day(2026, 10, 13)), before: Some(day(2026, 10, 14)) }
        );
        assert_eq!(
            first("letzte woche", &german()),
            Token::Date { after: Some(day(2026, 10, 5)), before: Some(day(2026, 10, 12)) }
        );
        assert_eq!(
            first("sept", &german()),
            Token::Date { after: Some(day(2026, 9, 1)), before: Some(day(2026, 10, 1)) }
        );
        // December has not begun this year: last year's.
        assert_eq!(
            first("dec", &english()),
            Token::Date { after: Some(day(2025, 12, 1)), before: Some(day(2026, 1, 1)) }
        );
        assert_eq!(
            first("märz", &german()),
            Token::Date { after: Some(day(2026, 3, 1)), before: Some(day(2026, 4, 1)) }
        );
        assert_eq!(
            first("2025", &english()),
            Token::Date { after: Some(day(2025, 1, 1)), before: Some(day(2026, 1, 1)) }
        );
    }

    #[test]
    fn a_date_offers_on_since_and_before() {
        let today = day(2026, 10, 14);
        let s = suggest("1.10.", &[], &german(), today);
        let dates: Vec<&Token> = s.iter().map(|s| &s.token).filter(|t| matches!(t, Token::Date { .. })).collect();
        assert_eq!(
            dates,
            vec![
                &Token::Date { after: Some(day(2026, 10, 1)), before: Some(day(2026, 10, 2)) },
                &Token::Date { after: Some(day(2026, 10, 1)), before: None },
                &Token::Date { after: None, before: Some(day(2026, 10, 1)) },
            ]
        );
        // A day still to come this year is last year's.
        let s = suggest("24.12.", &[], &german(), today);
        assert!(s
            .iter()
            .any(|x| x.token == Token::Date { after: Some(day(2025, 12, 24)), before: Some(day(2025, 12, 25)) }));
        // With a keyword typed, only that side.
        let s = suggest("seit:1.10.2026", &[], &german(), today);
        assert_eq!(s[0].token, Token::Date { after: Some(day(2026, 10, 1)), before: None });
    }

    /// A keyword typed on its own offers what can follow it: the people
    /// most mailed for an address field, starting points for a date.
    #[test]
    fn a_keyword_alone_offers_what_follows_it() {
        let s = suggest("von:", &people(), &german(), day(2026, 10, 14));
        assert_eq!(s.len(), 3);
        assert!(s.iter().all(|x| matches!(x.token, Token::Person { field: Field::From, .. })));
        let s = suggest("seit:", &[], &german(), day(2026, 10, 14));
        assert!(!s.is_empty());
        assert!(s.iter().all(|x| matches!(x.token, Token::Date { after: Some(_), before: None })));
        assert!(suggest("betreff:", &people(), &german(), day(2026, 10, 14)).is_empty());
    }

    #[test]
    fn a_leading_keyword_becomes_a_badge() {
        let v = german();
        assert_eq!(leading_keyword("from:", &v), Some((KeyWord::Field(Field::From), String::new())));
        assert_eq!(leading_keyword("von: jo", &v), Some((KeyWord::Field(Field::From), "jo".into())));
        assert_eq!(leading_keyword("Seit:1.10.", &v), Some((KeyWord::After, "1.10.".into())));
        assert_eq!(leading_keyword("re: hello", &v), None, "not a keyword");
        assert_eq!(leading_keyword("\"from:x\"", &v), None, "quoted");
        assert_eq!(leading_keyword("-from:x", &v), None, "a negation stays syntax");
        assert_eq!(leading_keyword("invoice from:x", &v), None, "only at the start");
    }

    #[test]
    fn a_badge_adds_its_value_once_there_is_one() {
        let today = day(2026, 10, 14);
        assert_eq!(pending_query(KeyWord::Field(Field::From), "", today), "");
        assert_eq!(pending_query(KeyWord::Field(Field::From), "jo", today), "from:jo");
        assert_eq!(pending_query(KeyWord::Field(Field::Subject), "q3 plan", today), "subject:\"q3 plan\"");
        assert_eq!(pending_query(KeyWord::After, "1.10.", today), "after:2026-10-01");
        assert_eq!(pending_query(KeyWord::Before, "next tuesday", today), "", "not a date yet");
    }

    #[test]
    fn suggestions_for_a_badge_keep_to_its_field() {
        let s = suggest_for(Some(KeyWord::Field(Field::To)), "hann", &people(), &english(), day(2026, 10, 14));
        assert!(matches!(&s[0].token, Token::Person { field: Field::To, email, .. } if email == "hb@example.org"));
        assert!(s.iter().all(|x| !matches!(x.token, Token::Text { field: Field::Subject, .. })));
    }

    #[test]
    fn an_empty_field_offers_somewhere_to_start() {
        let s = suggest("", &people(), &english(), day(2026, 10, 14));
        assert!(s.iter().any(|x| x.token == Token::HasAttachment));
        assert!(s.iter().any(|x| matches!(x.token, Token::Date { .. })));
        assert!(!s.iter().any(|x| matches!(x.token, Token::Person { .. })));
    }

    #[test]
    fn the_start_of_attachment_offers_the_filter() {
        assert!(suggest("anh", &[], &german(), day(2026, 10, 14)).iter().any(|x| x.token == Token::HasAttachment));
        assert!(suggest("attach", &[], &english(), day(2026, 10, 14)).iter().any(|x| x.token == Token::HasAttachment));
    }

    #[test]
    fn a_chip_switches_field_but_keeps_its_value() {
        let t = Token::Person { field: Field::From, name: "Anna".into(), email: "a@x".into() };
        assert_eq!(t.with_field(Field::Cc).to_query(), "cc:a@x");
        assert_eq!(t.fields(), &PERSON_FIELDS);
        let t = Token::Text { field: Field::Any, text: "plan".into() };
        assert_eq!(t.with_field(Field::Subject).to_query(), "subject:plan");
        assert!(Token::HasAttachment.fields().is_empty());
    }

    #[test]
    fn localized_keywords_are_read_by_the_parser() {
        let v = german();
        assert_eq!(v.field_key("von"), Some(KeyWord::Field(Field::From)));
        assert_eq!(v.field_key("FROM"), Some(KeyWord::Field(Field::From)));
        assert_eq!(v.field_key("seit"), Some(KeyWord::After));
        assert_eq!(v.field_key("nach"), Some(KeyWord::After));
        assert_eq!(v.field_key("re"), None);
    }
}
