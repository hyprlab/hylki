//! The quick filters (#392 and before) and Gmail's check filters: which
//! messages and conversations they let through.

use crate::i18n::i18n;
use crate::models::Message;
use gtk::glib;
use relm4::prelude::*;

/// One of the message list's quick filters (#97, #441): with it on, the
/// list shows only the mail that has the property. Together they narrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuickFilter {
    Unread,
    Starred,
    Attachment,
    ToMe,
    Contacts,
    Important,
    Tagged,
    Today,
}

impl QuickFilter {
    pub(crate) const ALL: [QuickFilter; 8] = [
        QuickFilter::Unread,
        QuickFilter::Starred,
        QuickFilter::Attachment,
        QuickFilter::ToMe,
        QuickFilter::Contacts,
        QuickFilter::Important,
        QuickFilter::Tagged,
        QuickFilter::Today,
    ];

    pub(crate) fn label(self) -> String {
        use crate::i18n::i18n_noop;
        i18n(match self {
            QuickFilter::Unread => i18n_noop("Unread"),
            QuickFilter::Starred => i18n_noop("Starred"),
            QuickFilter::Attachment => i18n_noop("Has Attachments"),
            QuickFilter::ToMe => i18n_noop("Addressed to Me"),
            QuickFilter::Contacts => i18n_noop("From Contacts"),
            QuickFilter::Important => i18n_noop("High Importance"),
            QuickFilter::Tagged => i18n_noop("Tagged"),
            QuickFilter::Today => i18n_noop("Today"),
        })
    }

    /// The icon the row, the reader or the sidebar already uses for it.
    pub(crate) fn icon(self) -> &'static str {
        match self {
            QuickFilter::Unread => "mail-unread-symbolic",
            QuickFilter::Starred => "starred-symbolic",
            QuickFilter::Attachment => "mail-attachment-symbolic",
            QuickFilter::ToMe => "avatar-default-symbolic",
            QuickFilter::Contacts => "x-office-address-book-symbolic",
            QuickFilter::Important => "emblem-important-symbolic",
            QuickFilter::Tagged => "tag-symbolic",
            QuickFilter::Today => "x-office-calendar-symbolic",
        }
    }

    fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// Which end of the quick filters' date range a day is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DateBound {
    From,
    To,
}

/// The quick filters that are on, and the days the list is narrowed to:
/// mail from the start of `from` to the end of `to`, both on this
/// computer's clock, either end left open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct QuickFilters {
    bits: u16,
    from: Option<chrono::NaiveDate>,
    to: Option<chrono::NaiveDate>,
}

impl QuickFilters {
    pub(crate) fn has(self, f: QuickFilter) -> bool {
        self.bits & f.bit() != 0
    }

    pub(crate) fn with(self, f: QuickFilter, on: bool) -> Self {
        Self {
            bits: if on {
                self.bits | f.bit()
            } else {
                self.bits & !f.bit()
            },
            ..self
        }
    }

    pub(crate) fn date(self, end: DateBound) -> Option<chrono::NaiveDate> {
        match end {
            DateBound::From => self.from,
            DateBound::To => self.to,
        }
    }

    pub(crate) fn with_date(self, end: DateBound, day: Option<chrono::NaiveDate>) -> Self {
        match end {
            DateBound::From => Self { from: day, ..self },
            DateBound::To => Self { to: day, ..self },
        }
    }

    pub(crate) fn has_dates(self) -> bool {
        self.from.is_some() || self.to.is_some()
    }

    /// The range as Unix seconds, the end excluded. Days given the wrong
    /// way round are read in order rather than matching nothing.
    pub(super) fn span(self) -> (i64, i64) {
        let (from, to) = match (self.from, self.to) {
            (Some(a), Some(b)) if a > b => (Some(b), Some(a)),
            other => other,
        };
        (
            from.map_or(i64::MIN, start_of_day),
            to.and_then(|d| d.succ_opt()).map_or(i64::MAX, start_of_day),
        )
    }

    pub(crate) fn is_empty(self) -> bool {
        self.bits == 0 && !self.has_dates()
    }
}

/// Local midnight of `day`, Unix seconds; an hour on where a clock change
/// skips midnight.
pub(super) fn start_of_day(day: chrono::NaiveDate) -> i64 {
    use chrono::TimeZone;
    let midnight = day.and_time(chrono::NaiveTime::MIN);
    [midnight, midnight + chrono::Duration::hours(1)]
        .iter()
        .find_map(|t| chrono::Local.from_local_datetime(t).earliest())
        .map_or_else(|| midnight.and_utc().timestamp(), |t| t.timestamp())
}

/// What the quick filters need to judge a message, gathered once a pass.
pub(super) struct QuickFilterContext {
    pub(super) filters: QuickFilters,
    /// Every address you send from, lower case ("Addressed to Me").
    pub(super) own: std::collections::HashSet<String>,
    /// Your contacts' addresses; none read yet lets nothing through.
    pub(super) contacts: Option<std::sync::Arc<std::collections::HashSet<String>>>,
    pub(super) tags: Vec<crate::config::Tag>,
    /// Local midnight, Unix seconds ("Today").
    pub(super) today: i64,
    /// The date range as Unix seconds, the end excluded.
    pub(super) span: (i64, i64),
}

impl QuickFilterContext {
    pub(super) fn passes(&self, m: &Message) -> bool {
        (self.span.0..self.span.1).contains(&m.timestamp) && self.passes_checks(m)
    }

    fn passes_checks(&self, m: &Message) -> bool {
        QuickFilter::ALL
            .iter()
            .filter(|f| self.filters.has(**f))
            .all(|f| match f {
                QuickFilter::Unread => m.unread,
                QuickFilter::Starred => m.starred,
                QuickFilter::Attachment => m.has_attachment,
                QuickFilter::ToMe => [m.to.as_str(), m.cc.as_str()]
                    .iter()
                    .flat_map(|list| list.split(','))
                    .any(|part| self.own.contains(&bare_address(part))),
                QuickFilter::Contacts => self
                    .contacts
                    .as_ref()
                    .is_some_and(|c| c.contains(&m.from_addr.trim().to_lowercase())),
                QuickFilter::Important => m.importance == crate::models::Importance::High,
                QuickFilter::Tagged => !m.pills(&self.tags).is_empty(),
                QuickFilter::Today => m.timestamp >= self.today,
            })
    }
}

/// `Name <addr>` or a bare `addr`, as the address alone in lower case.
pub(super) fn bare_address(part: &str) -> String {
    let part = part.trim();
    let addr = match part.rsplit_once('<') {
        Some((_, rest)) => rest.trim_end_matches('>'),
        None => part,
    };
    addr.trim().to_lowercase()
}

/// The start of today in local time, Unix seconds.
pub(super) fn start_of_today() -> i64 {
    let Ok(now) = glib::DateTime::now_local() else {
        return 0;
    };
    glib::DateTime::new(
        &now.timezone(),
        now.year(),
        now.month(),
        now.day_of_month(),
        0,
        0,
        0.0,
    )
    .map(|d| d.to_unix())
    .unwrap_or(0)
}

/// Which conversations Gmail's * r, * u, * s and * t select (#392).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckFilter {
    Read,
    Unread,
    Starred,
    Unstarred,
}

impl CheckFilter {
    /// Whether a conversation, all its members given, is one to select. A
    /// conversation counts as unread, or starred, when any member is, as
    /// its row shows it.
    pub(super) fn matches(self, members: &[Message]) -> bool {
        let unread = members.iter().any(|m| m.unread);
        let starred = members.iter().any(|m| m.starred);
        match self {
            CheckFilter::Read => !unread,
            CheckFilter::Unread => unread,
            CheckFilter::Starred => starred,
            CheckFilter::Unstarred => !starred,
        }
    }
}
