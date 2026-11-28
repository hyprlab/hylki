//! How an event repeats, in words, and the choices the editor's Repeat
//! menu offers.

use chrono::{Datelike, NaiveDate, Weekday};

use crate::calendar::recur::{Freq, Rule, Until};
use crate::i18n::{i18n, i18n_f, ni18n_f};

/// The Repeat menu, in order. `Custom` keeps a rule the presets can't say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Preset {
    Never,
    Daily,
    Weekdays,
    Weekly,
    Fortnightly,
    Monthly,
    Yearly,
    Custom,
}

pub(crate) const PRESETS: [Preset; 8] = [
    Preset::Never,
    Preset::Daily,
    Preset::Weekdays,
    Preset::Weekly,
    Preset::Fortnightly,
    Preset::Monthly,
    Preset::Yearly,
    Preset::Custom,
];

impl Preset {
    pub(crate) fn label(self) -> String {
        match self {
            Preset::Never => i18n("Never"),
            Preset::Daily => i18n("Every Day"),
            Preset::Weekdays => i18n("Every Weekday"),
            Preset::Weekly => i18n("Every Week"),
            Preset::Fortnightly => i18n("Every 2 Weeks"),
            Preset::Monthly => i18n("Every Month"),
            Preset::Yearly => i18n("Every Year"),
            Preset::Custom => i18n("Custom…"),
        }
    }

    /// The rule for this choice, starting on `day`. Custom and Never have
    /// none of their own.
    pub(crate) fn rule(self, day: NaiveDate) -> Option<Rule> {
        let mut r = match self {
            Preset::Never | Preset::Custom => return None,
            Preset::Daily => Rule::new(Freq::Daily),
            Preset::Weekdays => {
                let mut r = Rule::new(Freq::Weekly);
                r.by_day = [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri]
                    .into_iter()
                    .map(|d| (0, d))
                    .collect();
                r
            }
            Preset::Weekly => Rule::new(Freq::Weekly),
            Preset::Fortnightly => {
                let mut r = Rule::new(Freq::Weekly);
                r.interval = 2;
                r
            }
            Preset::Monthly => Rule::new(Freq::Monthly),
            Preset::Yearly => Rule::new(Freq::Yearly),
        };
        if self == Preset::Fortnightly || self == Preset::Weekly {
            r.by_day = vec![(0, day.weekday())];
        }
        Some(r)
    }

    /// Which choice a rule is, ignoring how it ends.
    pub(crate) fn of(rule: Option<&Rule>, day: NaiveDate) -> Preset {
        let Some(rule) = rule else {
            return Preset::Never;
        };
        let mut bare = rule.clone();
        bare.count = None;
        bare.until = None;
        for p in PRESETS {
            if let Some(r) = p.rule(day) {
                // A weekly rule that names no day repeats on the start's.
                let mut plain = r.clone();
                if matches!(p, Preset::Weekly | Preset::Fortnightly) {
                    plain.by_day.clear();
                }
                if r == bare || plain == bare {
                    return p;
                }
            }
        }
        Preset::Custom
    }
}

fn weekday_name(d: Weekday) -> String {
    // Any date on that weekday, written by GLib in the user's language.
    let monday = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap_or_default();
    let day = monday + chrono::Duration::days(i64::from(d.num_days_from_monday()));
    super::ctx::fmt(day, "%A")
}

fn ordinal(n: i32) -> String {
    match n {
        1 => i18n("first"),
        2 => i18n("second"),
        3 => i18n("third"),
        4 => i18n("fourth"),
        5 => i18n("fifth"),
        -1 => i18n("last"),
        -2 => i18n("second to last"),
        _ => n.to_string(),
    }
}

/// "A, B and C".
fn join(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => i18n_f(
            "{list} and {last}",
            &[("list", &rest.join(", ")), ("last", last)],
        ),
    }
}

/// How `rule` repeats from `start`, in a sentence.
pub(crate) fn describe(rule: &Rule, start: NaiveDate) -> String {
    let n = rule.interval.max(1);
    let ns = n.to_string();
    let every = match rule.freq {
        Freq::Daily => ni18n_f("Every day", "Every {n} days", n, &[("n", &ns)]),
        Freq::Weekly => ni18n_f("Every week", "Every {n} weeks", n, &[("n", &ns)]),
        Freq::Monthly => ni18n_f("Every month", "Every {n} months", n, &[("n", &ns)]),
        Freq::Yearly => ni18n_f("Every year", "Every {n} years", n, &[("n", &ns)]),
        Freq::Hourly => ni18n_f("Every hour", "Every {n} hours", n, &[("n", &ns)]),
        Freq::Minutely => ni18n_f("Every minute", "Every {n} minutes", n, &[("n", &ns)]),
        Freq::Secondly => ni18n_f("Every second", "Every {n} seconds", n, &[("n", &ns)]),
    };
    let weekdays = [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri];
    let mut text = if rule.freq == Freq::Weekly
        && n == 1
        && rule.by_day.len() == 5
        && weekdays.iter().all(|w| rule.by_day.contains(&(0, *w)))
    {
        i18n("Every weekday")
    } else {
        let days: Vec<String> = rule
            .by_day
            .iter()
            .map(|(o, d)| {
                if *o == 0 {
                    weekday_name(*d)
                } else {
                    i18n_f(
                        "the {nth} {day}",
                        &[("nth", &ordinal(*o)), ("day", &weekday_name(*d))],
                    )
                }
            })
            .collect();
        match rule.freq {
            Freq::Weekly if !days.is_empty() => {
                i18n_f("{every} on {days}", &[("every", &every), ("days", &join(&days))])
            }
            Freq::Monthly | Freq::Yearly if !days.is_empty() => {
                i18n_f("{every} on {days}", &[("every", &every), ("days", &join(&days))])
            }
            Freq::Monthly if !rule.by_month_day.is_empty() => {
                let list: Vec<String> = rule
                    .by_month_day
                    .iter()
                    .map(|d| {
                        if *d < 0 {
                            ordinal(*d)
                        } else {
                            d.to_string()
                        }
                    })
                    .collect();
                i18n_f(
                    "{every} on day {days}",
                    &[("every", &every), ("days", &join(&list))],
                )
            }
            Freq::Monthly => i18n_f(
                "{every} on day {days}",
                &[("every", &every), ("days", &start.day().to_string())],
            ),
            _ => every,
        }
    };
    if let Some(count) = rule.count {
        text = ni18n_f(
            "{rule}, once",
            "{rule}, {n} times",
            count,
            &[("rule", &text), ("n", &count.to_string())],
        );
    } else if let Some(until) = &rule.until {
        let day = match until {
            Until::Date(d) => *d,
            Until::Floating(w) => w.date(),
            Until::Utc(u) => crate::calendar::time::local_day(*u),
        };
        text = i18n_f(
            "{rule}, until {date}",
            &[("rule", &text), ("date", &super::ctx::long_day(day))],
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 25).unwrap()
    }

    #[test]
    fn presets_read_back_as_themselves() {
        for p in PRESETS {
            if let Some(r) = p.rule(day()) {
                assert_eq!(Preset::of(Some(&r), day()), p, "{p:?}");
            }
        }
        assert_eq!(Preset::of(None, day()), Preset::Never);
        let plain_weekly = Rule::parse("FREQ=WEEKLY").unwrap();
        assert_eq!(Preset::of(Some(&plain_weekly), day()), Preset::Weekly);
        let odd = Rule::parse("FREQ=MONTHLY;BYDAY=2TU").unwrap();
        assert_eq!(Preset::of(Some(&odd), day()), Preset::Custom);
        let ending = Rule::parse("FREQ=DAILY;COUNT=3").unwrap();
        assert_eq!(Preset::of(Some(&ending), day()), Preset::Daily);
    }

    #[test]
    fn rules_read_as_sentences() {
        let d = |s: &str| describe(&Rule::parse(s).unwrap(), day());
        assert_eq!(d("FREQ=DAILY"), "Every day");
        assert_eq!(d("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"), "Every weekday");
        assert_eq!(d("FREQ=DAILY;INTERVAL=3;COUNT=4"), "Every 3 days, 4 times");
        assert!(d("FREQ=MONTHLY").ends_with("on day 25"));
    }
}
