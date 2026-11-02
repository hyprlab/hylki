//! Events and tasks made from a message (#564): written to a calendar or a
//! task list in Evolution Data Server, or, where there is none, handed to
//! the calendar application as an `.ics` file.

mod eds;
mod ical;

pub(crate) use eds::{create, sources};
pub(crate) use ical::{calendar_file, new_uid};

use std::collections::HashMap;

use chrono::NaiveDate;

/// What is being made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ItemKind {
    Event,
    Task,
}

/// When an event happens, or when a task is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum When {
    /// From one moment to another, in Unix seconds.
    Timed { start: i64, end: i64 },
    /// Whole days, the last one included.
    AllDay { first: NaiveDate, last: NaiveDate },
    /// A task's due day, if it has one.
    Due(Option<NaiveDate>),
}

/// An event or a task as the dialog leaves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Item {
    pub(crate) kind: ItemKind,
    pub(crate) summary: String,
    pub(crate) notes: String,
    pub(crate) when: When,
    /// An event's alarms, in minutes before it starts: 0 is at the start.
    pub(crate) alarms: Vec<u32>,
}

/// What accounts are called here, for naming their calendars.
#[derive(Debug, Clone, Default)]
pub(crate) struct AccountNames {
    /// The names given to EDS accounts in Settings → Contacts, by the
    /// account's source UID.
    pub(crate) given: HashMap<String, String>,
    /// The mail accounts' nicknames, by lowercase address.
    pub(crate) nicknames: HashMap<String, String>,
}

/// A calendar or a task list in EDS that can take a new item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Source {
    pub(crate) uid: String,
    pub(crate) name: String,
}
