//! Where the calendars live, behind one face: Evolution Data Server where
//! the desktop has it (GNOME's calendars, Online Accounts, CalDAV accounts
//! set up here), Hylki's own calendar files where it doesn't, and the demo's
//! made-up week.
//!
//! Every call blocks (D-Bus, disk): run it off the UI thread.

use super::edit::Change;
use super::model::Book;
use super::time::Zones;
use super::tree::Node;
use super::AccountNames;

/// What a calendar is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CalKind {
    /// On this computer (EDS's own, or Hylki's file).
    Local,
    /// Kept in step with a server: CalDAV, Google, Microsoft 365.
    Remote,
    /// The birthdays and anniversaries EDS reads from the address books.
    Birthdays,
    /// An `.ics` address the calendar reads from (webcal).
    Subscription,
    /// Weather, and other read-only feeds.
    Other,
}

/// One calendar or task list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CalInfo {
    pub(crate) uid: String,
    /// Its own name.
    pub(crate) name: String,
    /// The account it belongs to, named as elsewhere here.
    pub(crate) account: Option<String>,
    /// `#rrggbb`.
    pub(crate) color: String,
    pub(crate) writable: bool,
    /// A task list rather than a calendar.
    pub(crate) tasks: bool,
    pub(crate) kind: CalKind,
    /// The source above it in EDS (its account), for grouping.
    pub(crate) parent: String,
}

impl CalInfo {
    /// "Account · Calendar", or the calendar alone.
    pub(crate) fn full_name(&self) -> String {
        match &self.account {
            Some(a) if *a != self.name => format!("{a} · {}", self.name),
            _ => self.name.clone(),
        }
    }
}

/// Where the calendars are kept for this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Backend {
    Eds,
    Files,
    Demo,
}

/// The backend this run uses: the demo's when the app runs as a demo, EDS
/// when its calendar factory answers, Hylki's files otherwise.
pub(crate) fn backend() -> Backend {
    if std::env::var_os("HYLKI_DEMO").is_some() {
        return Backend::Demo;
    }
    if crate::contacts::calendar_factory_dest().is_some() && crate::contacts::sources_dest().is_some() {
        Backend::Eds
    } else {
        Backend::Files
    }
}

/// Every calendar and task list, local ones first.
pub(crate) fn calendars(backend: Backend, names: AccountNames) -> Result<Vec<CalInfo>, String> {
    match backend {
        Backend::Eds => super::eds::calendars(names).ok_or_else(crate::contacts::directory::eds_source::not_available),
        Backend::Files => Ok(super::files::calendars()),
        Backend::Demo => Ok(super::demo::calendars()),
    }
}

/// What a calendar holds, read.
#[derive(Debug, Clone, Default)]
pub(crate) struct Loaded {
    pub(crate) book: Book,
    pub(crate) zones: Zones,
    /// What the calendar says of itself once open, where it says.
    pub(crate) writable: Option<bool>,
    /// The bus name and object path to watch for changes (EDS only).
    pub(crate) watch: Option<(String, String)>,
}

/// Read every event of `cal`.
pub(crate) fn load(backend: Backend, cal: &CalInfo) -> Result<Loaded, String> {
    match backend {
        Backend::Eds => super::eds_cal::load(cal),
        Backend::Files => super::files::load(cal),
        Backend::Demo => Ok(super::demo::load(cal)),
    }
}

/// Ask `cal` to fetch what changed on its server.
pub(crate) fn refresh(backend: Backend, cal: &CalInfo) {
    if backend == Backend::Eds && cal.kind != CalKind::Local {
        super::eds_cal::refresh(cal);
    }
}

/// Make `changes` to `cal`, in order. A zone an event names goes with it,
/// for a server that knows no zones of its own.
pub(crate) fn apply(backend: Backend, cal: &CalInfo, changes: &[Change]) -> Result<(), String> {
    match backend {
        Backend::Eds => super::eds_cal::apply(cal, changes),
        Backend::Files => super::files::apply(cal, changes),
        Backend::Demo => super::demo::apply(cal, changes),
    }
}

/// Every component of the series `uid` in `cal`, written out whole: what
/// moving an event to another calendar copies.
pub(crate) fn whole(book: &Book, uid: &str) -> Vec<Node> {
    book.series
        .get(uid)
        .map(|s| {
            s.master
                .iter()
                .chain(s.changed.iter())
                .map(|c| c.node.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// A new calendar on this computer.
pub(crate) fn new_calendar(backend: Backend, name: &str, color: &str) -> Result<String, String> {
    match backend {
        Backend::Eds => super::eds_cal::new_local(name, color),
        Backend::Files => super::files::new_calendar(name, color),
        Backend::Demo => Ok(super::demo::new_calendar(name, color)),
    }
}

/// A calendar read from an `.ics` address, kept up to date.
pub(crate) fn subscribe(backend: Backend, url: &str, name: &str, color: &str) -> Result<String, String> {
    match backend {
        Backend::Eds => super::eds_cal::subscribe(url, name, color),
        _ => Err(crate::i18n::i18n(
            "Subscriptions need Evolution Data Server, which is not available.",
        )),
    }
}

/// Rename a calendar and give it a color. A server's calendar keeps its
/// name there; only the color changes, here and in every app that reads EDS.
pub(crate) fn set_look(backend: Backend, cal: &CalInfo, name: &str, color: &str) -> Result<(), String> {
    match backend {
        Backend::Eds => super::eds_cal::set_look(cal, name, color),
        Backend::Files => super::files::set_look(cal, name, color),
        Backend::Demo => {
            super::demo::set_look(cal, name, color);
            Ok(())
        }
    }
}

/// Remove a calendar made on this computer, or a subscription.
pub(crate) fn delete_calendar(backend: Backend, cal: &CalInfo) -> Result<(), String> {
    match backend {
        Backend::Eds => super::eds_cal::delete(cal),
        Backend::Files => super::files::delete(cal),
        Backend::Demo => {
            super::demo::delete(cal);
            Ok(())
        }
    }
}

/// Add every event in an `.ics` document to `cal`: answers how many.
/// Events already there (by UID) are replaced, so importing twice is safe.
pub(crate) fn import(backend: Backend, cal: &CalInfo, text: &str) -> Result<usize, String> {
    let mut changes = Vec::new();
    let mut zones = Vec::new();
    for node in Node::parse_all(text) {
        let parts: Vec<Node> = if node.name == "VCALENDAR" {
            node.children
        } else {
            vec![node]
        };
        for part in parts {
            match part.name.as_str() {
                "VEVENT" | "VTODO" => changes.push(Change::Create(part)),
                "VTIMEZONE" => zones.push(part),
                _ => {}
            }
        }
    }
    if changes.is_empty() {
        return Err(crate::i18n::i18n("The file holds no events."));
    }
    let count = changes
        .iter()
        .filter(|c| matches!(c, Change::Create(n) if n.prop("RECURRENCE-ID").is_none()))
        .count();
    if backend == Backend::Eds {
        super::eds_cal::add_zones(cal, &zones);
    }
    apply(backend, cal, &changes)?;
    Ok(count)
}
