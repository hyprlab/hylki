//! Calendars and task lists in Evolution Data Server: which ones can take a
//! new item, and writing one there.

use std::collections::HashMap;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use super::{ical, AccountNames, Item, ItemKind, Source};
use crate::desktop::platform::{keyfile, keyfile_value};

const FACTORY_PATH: &str = "/org/gnome/evolution/dataserver/CalendarFactory";
const FACTORY_IFACE: &str = "org.gnome.evolution.dataserver.CalendarFactory";
const CALENDAR_IFACE: &str = "org.gnome.evolution.dataserver.Calendar";

/// Backends that only ever read: birthdays from Contacts, the weather, and
/// subscribed (webcal) calendars.
const READ_ONLY_BACKENDS: [&str; 3] = ["contacts", "weather", "webcal"];

/// The calendars, or the task lists, a new item can go to, the local ones
/// first, each under its account's name here. `None` when EDS is not there
/// to ask. Blocking: call it off the UI thread.
pub(crate) fn sources(kind: ItemKind, mut names: AccountNames) -> Option<Vec<Source>> {
    crate::contacts::calendar_factory_dest()?;
    let registry = crate::contacts::registry_sources()?;
    let data: HashMap<String, String> = registry
        .into_iter()
        .map(|(_, uid, data)| (uid, data))
        .collect();
    names.given = crate::contacts::contact_sources::names();
    Some(sources_from(kind, &data, &names))
}

/// [`sources`], from the registry's sources by UID.
fn sources_from(
    kind: ItemKind,
    registry: &HashMap<String, String>,
    names: &AccountNames,
) -> Vec<Source> {
    let section = section(kind);
    // An address two accounts sign in with (iCloud's and Microsoft's, say)
    // can't say which mail account's nickname is meant.
    let mut identities: HashMap<String, usize> = HashMap::new();
    for data in registry.values() {
        if let Some(i) = keyfile_value(data, "Collection", "Identity") {
            *identities.entry(i.to_lowercase()).or_default() += 1;
        }
    }
    let mut found: Vec<(Option<String>, Source)> = Vec::new();
    for (uid, data) in registry {
        let Some(kf) = keyfile(data) else {
            continue;
        };
        if !kf.has_group(section) || kf.boolean("Data Source", "Enabled").ok() == Some(false) {
            continue;
        }
        let backend = kf.string(section, "BackendName").unwrap_or_default();
        if READ_ONLY_BACKENDS.contains(&backend.as_str()) || upgraded_reminders(data) {
            continue;
        }
        let account = match collection(data, registry) {
            Some(Collection { enabled: false, .. }) => continue,
            Some(c) => Some(c.name(names, |i| identities.get(i).copied().unwrap_or(0) > 1)),
            None => None,
        };
        let own = keyfile_value(data, "Data Source", "DisplayName").unwrap_or_else(|| uid.clone());
        let name = match &account {
            Some(a) if *a != own => format!("{a} · {own}"),
            _ => own,
        };
        found.push((
            account,
            Source {
                uid: uid.clone(),
                name,
            },
        ));
    }
    found.sort_by_key(|(account, s)| {
        (
            account.is_some(),
            account.as_ref().map(|a| a.to_lowercase()),
            s.name.to_lowercase(),
        )
    });
    found.into_iter().map(|(_, s)| s).collect()
}

/// iCloud's Reminders as CalDAV still sees it: once reminders are upgraded
/// (iOS 13 and on) Apple serves them over no CalDAV at all, and leaves the
/// old list behind with a warning sign on its name and a note that says so.
/// EDS takes a task written there, but it never reaches Reminders.
fn upgraded_reminders(data: &str) -> bool {
    let host = keyfile_value(data, "Authentication", "Host").unwrap_or_default();
    let name = keyfile_value(data, "Data Source", "DisplayName").unwrap_or_default();
    host.ends_with("icloud.com") && name.contains('\u{26A0}')
}

/// The keyfile group that marks a source as a calendar or a task list.
fn section(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Event => "Calendar",
        ItemKind::Task => "Task List",
    }
}

/// The account a source belongs to.
struct Collection {
    uid: String,
    /// The address it signs in with.
    identity: Option<String>,
    /// Its name in EDS.
    display: Option<String>,
    /// Who serves it ("iCloud", "Google"), where that is known.
    provider: Option<String>,
    /// Whether calendars are switched on for it (GOA's Calendar switch).
    enabled: bool,
}

impl Collection {
    /// What the account is called here: the name given to it in Settings →
    /// Contacts, else the nickname of the mail account with its address,
    /// else the address. An address more than one account signs in with
    /// (`shared`, given lowercase) can't say which mail account is meant,
    /// so the provider's name stands in for it.
    fn name(&self, names: &AccountNames, shared: impl Fn(&str) -> bool) -> String {
        let identity = self.identity.as_ref().map(|i| i.to_lowercase());
        let shared = identity.as_deref().is_some_and(shared);
        names
            .given
            .get(&self.uid)
            .or_else(|| {
                if shared {
                    self.provider.as_ref()
                } else {
                    identity.as_ref().and_then(|i| names.nicknames.get(i))
                }
            })
            .or(self.identity.as_ref())
            .or(self.display.as_ref())
            .cloned()
            .unwrap_or_else(|| self.uid.clone())
    }
}

/// The account collection above a source, walked up its parents.
fn collection(data: &str, registry: &HashMap<String, String>) -> Option<Collection> {
    let mut current = data;
    for _ in 0..4 {
        let parent = keyfile_value(current, "Data Source", "Parent")?;
        let parent_data = registry.get(&parent)?;
        if let Some(kf) = keyfile(parent_data).filter(|kf| kf.has_group("Collection")) {
            let on = |group: &str, key: &str| kf.boolean(group, key).ok() != Some(false);
            return Some(Collection {
                uid: parent,
                identity: keyfile_value(parent_data, "Collection", "Identity"),
                display: keyfile_value(parent_data, "Data Source", "DisplayName"),
                provider: provider(parent_data),
                enabled: on("Data Source", "Enabled") && on("Collection", "CalendarEnabled"),
            });
        }
        current = parent_data;
    }
    None
}

/// The provider of an account collection, by its backend, or for a
/// WebDAV one by its server: GNOME connects to iCloud over WebDAV.
fn provider(data: &str) -> Option<String> {
    let url = |group: &str| keyfile_value(data, group, "CalendarUrl").unwrap_or_default();
    match keyfile_value(data, "Collection", "BackendName").as_deref() {
        Some("google") => Some("Google".into()),
        Some("microsoft365") => Some("Microsoft 365".into()),
        _ if [url("Collection"), url("GNOME Online Accounts")]
            .iter()
            .any(|u| u.contains("icloud.com")) =>
        {
            Some("iCloud".into())
        }
        _ => None,
    }
}

/// Write `item` to the calendar or task list `source_uid`. Blocking: call it
/// off the UI thread. The error is EDS's own, for the status bar.
pub(crate) fn create(source_uid: &str, item: &Item) -> Result<(), String> {
    let dest = crate::contacts::calendar_factory_dest()
        .ok_or_else(|| "Evolution Data Server is not available".to_string())?;
    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .map_err(|e| e.to_string())?;
    let open = match item.kind {
        ItemKind::Event => "OpenCalendar",
        ItemKind::Task => "OpenTaskList",
    };
    let reply = call(
        &conn,
        (&dest, FACTORY_PATH, FACTORY_IFACE),
        open,
        Some((source_uid,).to_variant()),
        30_000,
    )?;
    let (path, bus): (String, String) = reply
        .get()
        .ok_or_else(|| format!("unexpected reply to {open}: {}", reply.type_()))?;
    let calendar = (bus.as_str(), path.as_str(), CALENDAR_IFACE);
    // A remote calendar may go to its server for either of these.
    call(&conn, calendar, "Open", None, 60_000)?;
    let ics = ical::component(item, &ical::new_uid(), crate::datefmt::now());
    let created = call(
        &conn,
        calendar,
        "CreateObjects",
        Some((vec![ics], 0u32).to_variant()),
        60_000,
    );
    let _ = call(&conn, calendar, "Close", None, 5_000);
    created.map(|_| ())
}

/// One method call, bounded by `timeout_ms`: GIO's, as the source registry
/// is asked, so a stuck backend can't hold the thread forever.
fn call(
    conn: &gio::DBusConnection,
    (dest, path, iface): (&str, &str, &str),
    method: &str,
    args: Option<glib::Variant>,
    timeout_ms: i32,
) -> Result<glib::Variant, String> {
    conn.call_sync(
        Some(dest),
        path,
        iface,
        method,
        args.as_ref(),
        None,
        gio::DBusCallFlags::NONE,
        timeout_ms,
        gio::Cancellable::NONE,
    )
    .map_err(|e| remote_message(&e.to_string()))
}

/// EDS's message without the D-Bus error name GIO puts in front of it.
fn remote_message(text: &str) -> String {
    match text.strip_prefix("GDBus.Error:") {
        Some(rest) => rest.split_once(": ").map_or(rest, |(_, m)| m).to_string(),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> HashMap<String, String> {
        [
            (
                "system-calendar",
                "[Data Source]\nDisplayName=Personal\nEnabled=true\nParent=local-stub\n\
                 [Calendar]\nBackendName=local\n",
            ),
            (
                "local-stub",
                "[Data Source]\nDisplayName=On This Computer\nEnabled=true\n",
            ),
            (
                "birthdays",
                "[Data Source]\nDisplayName=Birthdays & Anniversaries\nParent=contacts-stub\n\
                 [Calendar]\nBackendName=contacts\n",
            ),
            (
                "icloud",
                "[Data Source]\nDisplayName=iCloud\nEnabled=true\n\
                 [Collection]\nBackendName=webdav\nIdentity=ada@icloud.com\nCalendarUrl=https://caldav.icloud.com/\n",
            ),
            (
                "work",
                "[Data Source]\nDisplayName=Work\nEnabled=true\nParent=icloud\n\
                 [Calendar]\nBackendName=caldav\n",
            ),
            (
                "home",
                "[Data Source]\nDisplayName=Home\nEnabled=true\nParent=icloud\n\
                 [Calendar]\nBackendName=caldav\n",
            ),
            (
                "reminders",
                "[Data Source]\nDisplayName=Reminders\nEnabled=true\nParent=icloud\n\
                 [Task List]\nBackendName=caldav\n",
            ),
            (
                "upgraded",
                "[Data Source]\nDisplayName=Reminders \u{26A0}\u{FE0F}\nEnabled=true\nParent=icloud\n\
                 [Authentication]\nHost=p23-caldav.icloud.com\n\
                 [Task List]\nBackendName=caldav\n",
            ),
            (
                "off",
                "[Data Source]\nDisplayName=Old\nEnabled=false\nParent=icloud\n\
                 [Calendar]\nBackendName=caldav\n",
            ),
            (
                "google",
                "[Data Source]\nDisplayName=Google\nEnabled=true\n\
                 [Collection]\nBackendName=google\nIdentity=ada@gmail.com\nCalendarEnabled=false\n",
            ),
            (
                "gcal",
                "[Data Source]\nDisplayName=ada@gmail.com\nEnabled=true\nParent=google\n\
                 [Calendar]\nBackendName=caldav\n",
            ),
            (
                "holidays",
                "[Data Source]\nDisplayName=Holidays\nEnabled=true\n\
                 [Calendar]\nBackendName=webcal\n",
            ),
        ]
        .into_iter()
        .map(|(uid, data)| (uid.to_string(), data.to_string()))
        .collect()
    }

    fn names_with(kind: ItemKind, names: &AccountNames) -> Vec<String> {
        sources_from(kind, &registry(), names)
            .into_iter()
            .map(|s| s.name)
            .collect()
    }

    fn names(kind: ItemKind) -> Vec<String> {
        names_with(kind, &AccountNames::default())
    }

    #[test]
    fn writable_calendars_come_local_first_with_their_account() {
        // Not the birthdays, a subscription, a disabled calendar, nor an
        // account with its calendars switched off.
        assert_eq!(
            names(ItemKind::Event),
            ["Personal", "ada@icloud.com · Home", "ada@icloud.com · Work"]
        );
    }

    #[test]
    fn task_lists_are_their_own_sources() {
        // Not the list iCloud left behind when its reminders were upgraded.
        assert_eq!(names(ItemKind::Task), ["ada@icloud.com · Reminders"]);
    }

    #[test]
    fn accounts_go_by_the_names_given_here() {
        let mut names = AccountNames::default();
        names
            .nicknames
            .insert("ada@icloud.com".into(), "Personal mail".into());
        assert_eq!(
            names_with(ItemKind::Task, &names),
            ["Personal mail · Reminders"]
        );
        // The name given in Settings → Contacts comes first.
        names.given.insert("icloud".into(), "iCloud".into());
        assert_eq!(names_with(ItemKind::Task, &names), ["iCloud · Reminders"]);
    }

    #[test]
    fn a_shared_address_keeps_its_nickname_to_itself() {
        let mut registry = registry();
        registry.insert(
            "m365".into(),
            "[Data Source]\nDisplayName=Microsoft 365\nEnabled=true\n\
             [Collection]\nBackendName=microsoft365\nIdentity=Ada@iCloud.com\n"
                .into(),
        );
        registry.insert(
            "m365-tasks".into(),
            "[Data Source]\nDisplayName=Tasks\nEnabled=true\nParent=m365\n\
             [Task List]\nBackendName=microsoft365\n"
                .into(),
        );
        let mut names = AccountNames::default();
        names
            .nicknames
            .insert("ada@icloud.com".into(), "Mail".into());
        let listed: Vec<String> = sources_from(ItemKind::Task, &registry, &names)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(listed, ["iCloud · Reminders", "Microsoft 365 · Tasks"]);
    }

    #[test]
    fn the_dbus_error_name_is_dropped() {
        assert_eq!(
            remote_message(
                "GDBus.Error:org.gnome.evolution.dataserver.Calendar.PermissionDenied: \
                 Cannot create calendar object: Permission denied"
            ),
            "Cannot create calendar object: Permission denied"
        );
        assert_eq!(remote_message("Timeout was reached"), "Timeout was reached");
    }

    /// Writes an event and a task to a real EDS and reads them back. Only
    /// in a sealed rig whose bus can start nothing but EDS, with its own
    /// HOME and XDG dirs (`HYLKI_CAL_RIG=1`): never on a real session.
    #[test]
    #[ignore]
    fn live_rig_write() {
        if std::env::var("HYLKI_CAL_RIG").is_err() {
            return;
        }
        let read_back = |kind: ItemKind, uid: &str| -> Vec<String> {
            let dest = crate::contacts::calendar_factory_dest().unwrap();
            let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).unwrap();
            let open = match kind {
                ItemKind::Event => "OpenCalendar",
                ItemKind::Task => "OpenTaskList",
            };
            let r = call(
                &conn,
                (&dest, FACTORY_PATH, FACTORY_IFACE),
                open,
                Some((uid,).to_variant()),
                30_000,
            )
            .unwrap();
            let (path, bus): (String, String) = r.get().unwrap();
            let cal = (bus.as_str(), path.as_str(), CALENDAR_IFACE);
            call(&conn, cal, "Open", None, 30_000).unwrap();
            let r = call(
                &conn,
                cal,
                "GetObjectList",
                Some(("#t",).to_variant()),
                30_000,
            )
            .unwrap();
            let (objects,): (Vec<String>,) = r.get().unwrap();
            objects
        };
        let calendars = sources(ItemKind::Event, AccountNames::default()).expect("EDS answers");
        eprintln!("calendars: {calendars:?}");
        let personal = calendars.first().expect("a calendar");
        let event = Item {
            kind: ItemKind::Event,
            summary: "Rig lunch, with notes".into(),
            notes: "From: Ada\nDate: today\n\nBring the slides".into(),
            when: super::super::When::Timed {
                start: 1_793_606_400,
                end: 1_793_610_000,
            },
            alarms: vec![15, 24 * 60],
        };
        create(&personal.uid, &event).expect("event written");
        let events = read_back(ItemKind::Event, &personal.uid);
        eprintln!("{events:#?}");
        assert!(events
            .iter()
            .any(|e| e.contains("SUMMARY:Rig lunch\\, with notes")
                && e.contains("20261102T080000Z")
                && e.contains("Bring the slides")
                && e.contains("BEGIN:VALARM")
                && e.contains("-PT15M")
                && e.contains("-P1D")));

        let lists = sources(ItemKind::Task, AccountNames::default()).expect("EDS answers");
        eprintln!("task lists: {lists:?}");
        let list = lists.first().expect("a task list");
        let task = Item {
            kind: ItemKind::Task,
            summary: "Pay the invoice".into(),
            notes: String::new(),
            when: super::super::When::Due(chrono::NaiveDate::from_ymd_opt(2026, 11, 5)),
            alarms: Vec::new(),
        };
        create(&list.uid, &task).expect("task written");
        let tasks = read_back(ItemKind::Task, &list.uid);
        eprintln!("{tasks:#?}");
        assert!(tasks.iter().any(
            |t| t.contains("SUMMARY:Pay the invoice") && t.contains("DUE;VALUE=DATE:20261105")
        ));
    }
}
