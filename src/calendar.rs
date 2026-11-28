//! Calendars (#715): events and tasks read from Evolution Data Server (or
//! Hylki's own calendar files where there is none), expanded into the
//! occurrences the calendar views draw, and written back.
//!
//! Also the events and tasks made from a message (#564), written to a
//! calendar or a task list, or handed to the calendar application as an
//! `.ics` file where there is nowhere to write them.

mod demo;
mod eds;
mod eds_cal;
pub(crate) mod edit;
mod extract;
mod files;
mod ical;
pub(crate) mod model;
pub(crate) mod prefs;
pub(crate) mod recur;
pub(crate) mod store;
pub(crate) mod time;
pub(crate) mod tree;

pub(crate) use eds::{create, sources};
pub(crate) use extract::{find, title, Found, Words};
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

/// The colors a calendar without one of its own is given, GNOME's palette.
pub(crate) const PALETTE: [&str; 9] = [
    "#3584e4", "#2ec27e", "#e5a50a", "#e66100", "#c01c28", "#9141ac", "#986a44", "#26a269",
    "#1c71d8",
];

/// A color for a calendar that names none, the same one every time.
pub(crate) fn palette_color(uid: &str) -> String {
    let sum = uid.bytes().fold(0usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize));
    PALETTE[sum % PALETTE.len()].to_string()
}
