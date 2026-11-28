//! Reading and writing calendars in Evolution Data Server.
//!
//! A calendar is opened once and kept open for as long as the app runs.
//! EDS keeps a calendar in step with its server only while some client
//! holds it open, and it closes it when the connection that opened it goes,
//! so the calendars are opened on the process's one shared session bus
//! connection. The views learn of a change (theirs or another app's) from
//! the calendar's `Revision` property, which EDS bumps on every write.

use std::collections::HashMap;
use std::sync::Mutex;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use super::edit::Change;
use super::eds::{call, CALENDAR_IFACE, FACTORY_IFACE, FACTORY_PATH};
use super::model::Book;
use super::store::{CalInfo, CalKind, Loaded};
use super::time::{vtimezone, Zones};
use super::tree::Node;
use crate::contacts::directory::eds_source::{
    bus, create_source, new_source_uid, not_available, registry, remove_source, write_source,
};

/// The calendars open on the shared connection: source UID → (bus name,
/// object path).
fn opened() -> &'static Mutex<HashMap<String, (String, String)>> {
    static OPENED: std::sync::LazyLock<Mutex<HashMap<String, (String, String)>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &OPENED
}

fn conn() -> Result<gio::DBusConnection, String> {
    gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).map_err(|e| e.to_string())
}

/// The calendar `cal`, opened if it wasn't.
fn open(conn: &gio::DBusConnection, cal: &CalInfo) -> Result<(String, String), String> {
    if let Some(o) = opened()
        .lock()
        .ok()
        .and_then(|o| o.get(&cal.uid).cloned())
    {
        return Ok(o);
    }
    let dest = crate::contacts::calendar_factory_dest().ok_or_else(not_available)?;
    let method = if cal.tasks {
        "OpenTaskList"
    } else {
        "OpenCalendar"
    };
    let reply = call(
        conn,
        (&dest, FACTORY_PATH, FACTORY_IFACE),
        method,
        Some((cal.uid.as_str(),).to_variant()),
        30_000,
    )?;
    let (path, bus_name): (String, String) = reply
        .get()
        .ok_or_else(|| format!("unexpected reply to {method}: {}", reply.type_()))?;
    // A server's calendar may go to its server for this.
    call(conn, (&bus_name, &path, CALENDAR_IFACE), "Open", None, 60_000)?;
    if let Ok(mut o) = opened().lock() {
        o.insert(cal.uid.clone(), (bus_name.clone(), path.clone()));
    }
    Ok((bus_name, path))
}

/// Call `method` on the open calendar, opening it again once if EDS has
/// dropped it (its factory restarted, say).
fn on_calendar(
    conn: &gio::DBusConnection,
    cal: &CalInfo,
    method: &str,
    args: Option<glib::Variant>,
    timeout_ms: i32,
) -> Result<glib::Variant, String> {
    let (b, p) = open(conn, cal)?;
    match call(conn, (&b, &p, CALENDAR_IFACE), method, args.clone(), timeout_ms) {
        Err(e) if gone(&e) => {
            if let Ok(mut o) = opened().lock() {
                o.remove(&cal.uid);
            }
            let (b, p) = open(conn, cal)?;
            call(conn, (&b, &p, CALENDAR_IFACE), method, args, timeout_ms)
        }
        r => r,
    }
}

/// Whether an error says the calendar object is no longer there.
fn gone(e: &str) -> bool {
    let e = e.to_lowercase();
    e.contains("unknownmethod")
        || e.contains("no such interface")
        || e.contains("unknown object")
        || e.contains("serviceunknown")
        || e.contains("does not exist")
}

/// Read every object of `cal`.
pub(crate) fn load(cal: &CalInfo) -> Result<Loaded, String> {
    let conn = conn()?;
    let reply = on_calendar(&conn, cal, "GetObjectList", Some(("#t",).to_variant()), 60_000)?;
    let (objects,): (Vec<String>,) = reply
        .get()
        .ok_or_else(|| format!("unexpected reply to GetObjectList: {}", reply.type_()))?;
    let mut zones = Zones::default();
    let book = Book::read(&objects, &mut zones);
    // Zones the system doesn't know (Exchange's Windows names that the
    // table misses) come from the calendar's own store.
    for tzid in book.tzids() {
        if zones.knows(&tzid) {
            continue;
        }
        if let Ok(r) = on_calendar(&conn, cal, "GetTimezone", Some((tzid.as_str(),).to_variant()), 10_000) {
            if let Some((text,)) = r.get::<(String,)>() {
                if let Some(node) = Node::parse(&text) {
                    zones.learn(&node);
                }
            }
        }
    }
    let (b, p) = open(&conn, cal)?;
    let writable = property(&conn, &b, &p, "Writable").and_then(|v| v.get::<bool>());
    Ok(Loaded {
        book,
        zones,
        writable,
        watch: Some((b, p)),
    })
}

fn property(conn: &gio::DBusConnection, b: &str, p: &str, name: &str) -> Option<glib::Variant> {
    let r = call(
        conn,
        (b, p, "org.freedesktop.DBus.Properties"),
        "Get",
        Some((CALENDAR_IFACE, name).to_variant()),
        5_000,
    )
    .ok()?;
    // Get answers (v): the value is the variant inside it.
    (r.n_children() == 1)
        .then(|| r.child_value(0))
        .and_then(|v| v.as_variant())
}

/// Ask a server's calendar to fetch what changed. EDS answers at once and
/// fetches in the background; the change arrives as a new revision.
pub(crate) fn refresh(cal: &CalInfo) {
    if let Ok(conn) = conn() {
        if let Err(e) = on_calendar(&conn, cal, "Refresh", None, 30_000) {
            tracing::debug!(calendar = %cal.uid, "calendar did not refresh: {e}");
        }
    }
}

/// Hand the calendar the `VTIMEZONE` of every zone `node` names that the
/// system knows, so the event reads the same on a server with no zone
/// database of its own.
fn zones_for(conn: &gio::DBusConnection, cal: &CalInfo, node: &Node) {
    let mut done: Vec<String> = Vec::new();
    for line in node.props() {
        if let Some(tz) = line.param("TZID") {
            if done.iter().any(|d| d == tz) {
                continue;
            }
            done.push(tz.to_string());
            if let Some(vtz) = vtimezone(tz) {
                let _ = on_calendar(
                    conn,
                    cal,
                    "AddTimezone",
                    Some((vtz.write(),).to_variant()),
                    10_000,
                );
            }
        }
    }
}

/// Hand over the zones an imported document defines.
pub(crate) fn add_zones(cal: &CalInfo, zones: &[Node]) {
    let Ok(conn) = conn() else {
        return;
    };
    for z in zones {
        let _ = on_calendar(&conn, cal, "AddTimezone", Some((z.write(),).to_variant()), 10_000);
    }
}

/// Make each change, in order.
pub(crate) fn apply(cal: &CalInfo, changes: &[Change]) -> Result<(), String> {
    let conn = conn()?;
    for change in changes {
        match change {
            Change::Create(node) => {
                zones_for(&conn, cal, node);
                let ics = node.write();
                let created = on_calendar(
                    &conn,
                    cal,
                    "CreateObjects",
                    Some((vec![ics.clone()], 0u32).to_variant()),
                    60_000,
                );
                if let Err(e) = created {
                    // Importing an event that is already there replaces it.
                    if e.to_lowercase().contains("exist") {
                        modify(&conn, cal, node)?;
                    } else {
                        return Err(e);
                    }
                }
            }
            Change::Replace(node) => {
                zones_for(&conn, cal, node);
                modify(&conn, cal, node)?;
            }
            Change::Remove { uid } => {
                remove(&conn, cal, uid, "", "all")?;
            }
            Change::RemoveOne { uid, rid } => {
                remove(&conn, cal, uid, &rid.value(), "this")?;
            }
        }
    }
    Ok(())
}

/// Replace a component: a changed occurrence alone ("this"), else the
/// whole series, its changed occurrences kept.
fn modify(conn: &gio::DBusConnection, cal: &CalInfo, node: &Node) -> Result<(), String> {
    let mode = if node.prop("RECURRENCE-ID").is_some() {
        "this"
    } else {
        "all"
    };
    let r = on_calendar(
        conn,
        cal,
        "ModifyObjects",
        Some((vec![node.write()], mode, 0u32).to_variant()),
        60_000,
    );
    match r {
        // The occurrence a series lost, or an event another app removed
        // meanwhile: written afresh.
        Err(e) if e.to_lowercase().contains("not found") && mode == "all" => on_calendar(
            conn,
            cal,
            "CreateObjects",
            Some((vec![node.write()], 0u32).to_variant()),
            60_000,
        )
        .map(|_| ()),
        r => r.map(|_| ()),
    }
}

fn remove(conn: &gio::DBusConnection, cal: &CalInfo, uid: &str, rid: &str, mode: &str) -> Result<(), String> {
    let ids = vec![(uid.to_string(), rid.to_string())];
    match on_calendar(
        conn,
        cal,
        "RemoveObjects",
        Some((ids, mode, 0u32).to_variant()),
        60_000,
    ) {
        // Already gone is what was asked for.
        Err(e) if e.to_lowercase().contains("not found") => Ok(()),
        r => r.map(|_| ()),
    }
}

/// A new calendar under "On This Computer".
pub(crate) fn new_local(name: &str, color: &str) -> Result<String, String> {
    let kf = glib::KeyFile::new();
    kf.set_string("Data Source", "DisplayName", name.trim());
    kf.set_boolean("Data Source", "Enabled", true);
    kf.set_string("Data Source", "Parent", "local-stub");
    kf.set_string("Calendar", "BackendName", "local");
    kf.set_string("Calendar", "Color", color);
    kf.set_boolean("Calendar", "Selected", true);
    let uid = new_source_uid();
    let c = bus()?;
    create_source(&c, &registry()?, &uid, kf.to_data().to_string())?;
    Ok(uid)
}

/// An `.ics` address, read by EDS's webcal backend and fetched again every
/// hour. `webcal://` is the scheme calendar links are published with; the
/// file itself comes over HTTPS.
pub(crate) fn subscribe(url: &str, name: &str, color: &str) -> Result<String, String> {
    let url = url.trim();
    let url = url
        .strip_prefix("webcal://")
        .or_else(|| url.strip_prefix("webcals://"))
        .map(|rest| format!("https://{rest}"))
        .unwrap_or_else(|| url.to_string());
    let parsed = glib::Uri::parse(&url, glib::UriFlags::NONE)
        .map_err(|_| crate::i18n::i18n("That is not a web address."))?;
    let scheme = parsed.scheme().to_lowercase();
    if scheme != "https" && scheme != "http" {
        return Err(crate::i18n::i18n("That is not a web address."));
    }
    let host = parsed.host().map(|h| h.to_string()).unwrap_or_default();
    if host.is_empty() {
        return Err(crate::i18n::i18n("That is not a web address."));
    }
    let tls = scheme == "https";
    let port = match parsed.port() {
        p if p > 0 => p,
        _ if tls => 443,
        _ => 80,
    };
    let kf = glib::KeyFile::new();
    let title = if name.trim().is_empty() {
        host.clone()
    } else {
        name.trim().to_string()
    };
    kf.set_string("Data Source", "DisplayName", &title);
    kf.set_boolean("Data Source", "Enabled", true);
    kf.set_string("Data Source", "Parent", "webcal-stub");
    kf.set_string("Calendar", "BackendName", "webcal");
    kf.set_string("Calendar", "Color", color);
    kf.set_boolean("Calendar", "Selected", true);
    kf.set_string("Authentication", "Host", &host);
    kf.set_integer("Authentication", "Port", port);
    kf.set_string("Security", "Method", if tls { "tls" } else { "none" });
    kf.set_string("WebDAV Backend", "ResourcePath", parsed.path().as_str());
    if let Some(q) = parsed.query() {
        kf.set_string("WebDAV Backend", "ResourceQuery", q.as_str());
    }
    kf.set_boolean("Refresh", "Enabled", true);
    kf.set_integer("Refresh", "IntervalMinutes", 60);
    kf.set_boolean("Offline", "StaySynchronized", true);
    let uid = new_source_uid();
    let c = bus()?;
    create_source(&c, &registry()?, &uid, kf.to_data().to_string())?;
    Ok(uid)
}

/// The registry's object path and key file for a source.
fn source(uid: &str) -> Result<(String, String), String> {
    crate::contacts::registry_sources()
        .ok_or_else(not_available)?
        .into_iter()
        .find(|(_, u, _)| u == uid)
        .map(|(path, _, data)| (path, data))
        .ok_or_else(|| crate::i18n::i18n("The calendar is no longer there."))
}

/// Give a calendar a color, and a name where the name is kept here.
pub(crate) fn set_look(cal: &CalInfo, name: &str, color: &str) -> Result<(), String> {
    let (path, data) = source(&cal.uid)?;
    let kf = crate::desktop::platform::keyfile(&data).unwrap_or_default();
    let section = if cal.tasks { "Task List" } else { "Calendar" };
    kf.set_string(section, "Color", color);
    if matches!(cal.kind, CalKind::Local | CalKind::Subscription) && !name.trim().is_empty() {
        kf.set_string("Data Source", "DisplayName", name.trim());
    }
    let c = bus()?;
    write_source(&c, &registry()?, &path, kf.to_data().to_string())
}

/// Remove a calendar and what EDS keeps for it.
pub(crate) fn delete(cal: &CalInfo) -> Result<(), String> {
    let (path, _) = source(&cal.uid)?;
    if let Ok(mut o) = opened().lock() {
        o.remove(&cal.uid);
    }
    let c = bus()?;
    remove_source(&c, &registry()?, &path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::edit::{self, Draft, Scope, Span};
    use crate::calendar::recur::Rule;
    use crate::calendar::AccountNames;
    use chrono::NaiveDate;

    fn occurrences(cal: &CalInfo) -> Vec<crate::calendar::model::Occ> {
        let mut l = load(cal).expect("calendar loads");
        let day = |d: u32| crate::calendar::time::day_start(NaiveDate::from_ymd_opt(2026, 11, d).unwrap());
        let mut o = l.book.occurrences(&cal.uid, &mut l.zones, day(1), day(30));
        o.sort_by_key(|x| x.start);
        o
    }

    /// Every write path against a real EDS: a new calendar, a series, one
    /// occurrence moved, one removed, the rest split off, all removed, the
    /// calendar's color and name, a subscription. Only in the sealed rig
    /// (`HYLKI_CAL_RIG=1`), never on a real session.
    #[test]
    #[ignore]
    fn live_rig_round_trip() {
        if std::env::var("HYLKI_CAL_RIG").is_err() {
            return;
        }
        let uid = new_local("Rig calendar", "#e66100").expect("calendar made");
        let mut cal = None;
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            cal = crate::calendar::eds::calendars(AccountNames::default())
                .unwrap_or_default()
                .into_iter()
                .find(|c| c.uid == uid);
            if cal.is_some() {
                break;
            }
        }
        let cal = cal.expect("the new calendar is listed");
        assert_eq!(cal.color, "#e66100");
        assert_eq!(cal.kind, CalKind::Local);

        let at = |d: u32, h: u32| NaiveDate::from_ymd_opt(2026, 11, d).unwrap().and_hms_opt(h, 0, 0).unwrap();
        let mut d = Draft::new(Span::Timed {
            start: at(2, 9),
            end: at(2, 10),
            tzid: Some("Europe/Paris".into()),
        });
        d.summary = "Rig standup".into();
        d.repeat = Rule::parse("FREQ=DAILY;COUNT=5");
        d.alarms = vec![600];
        apply(&cal, &[Change::Create(edit::create(&d))]).expect("series written");
        let occ = occurrences(&cal);
        eprintln!("after create: {}", occ.len());
        assert_eq!(occ.len(), 5);

        // The third alone, an hour later.
        let mut l = load(&cal).unwrap();
        let third = occ[2].clone();
        let master = l.book.comp(&third.key.uid, "");
        let mut moved = Draft::from_occ(&third, master.as_deref(), &mut l.zones);
        moved.span = Span::Timed {
            start: at(4, 10),
            end: at(4, 11),
            tzid: Some("Europe/Paris".into()),
        };
        let changes = edit::edit(&third, master.as_deref(), &moved, Scope::This, &mut l.zones);
        apply(&cal, &changes).expect("one occurrence moved");
        let occ = occurrences(&cal);
        assert_eq!(occ.len(), 5);
        assert!(occ.iter().any(|o| o.comp.rid.is_some()), "a changed occurrence");

        // The second removed.
        let mut l = load(&cal).unwrap();
        let second = occ[1].clone();
        let master = l.book.comp(&second.key.uid, "");
        let changes = edit::remove(&second, master.as_deref(), Scope::This, &mut l.zones);
        apply(&cal, &changes).expect("one occurrence removed");
        let occ = occurrences(&cal);
        eprintln!("after remove one: {}", occ.len());
        assert_eq!(occ.len(), 4);

        // The fourth and on renamed: a new series.
        let mut l = load(&cal).unwrap();
        let fourth = occ.iter().find(|o| o.first_day.format("%d").to_string() == "05").unwrap().clone();
        let master = l.book.comp(&fourth.key.uid, "");
        let mut renamed = Draft::from_occ(&fourth, master.as_deref(), &mut l.zones);
        renamed.summary = "Rig standup, new room".into();
        let changes = edit::edit(&fourth, master.as_deref(), &renamed, Scope::ThisAndFuture, &mut l.zones);
        apply(&cal, &changes).expect("series split");
        let occ = occurrences(&cal);
        let names: Vec<String> = occ.iter().map(|o| o.comp.summary.clone()).collect();
        eprintln!("after split: {names:?}");
        assert_eq!(names.iter().filter(|n| n.contains("new room")).count(), 2);
        assert_eq!(occ.len(), 4);

        // Everything removed.
        let uids: std::collections::HashSet<String> = occ.iter().map(|o| o.key.uid.clone()).collect();
        let changes: Vec<Change> = uids.into_iter().map(|uid| Change::Remove { uid }).collect();
        apply(&cal, &changes).expect("removed");
        assert!(occurrences(&cal).is_empty());

        set_look(&cal, "Rig renamed", "#2ec27e").expect("look set");
        std::thread::sleep(std::time::Duration::from_millis(500));
        let again = crate::calendar::eds::calendars(AccountNames::default())
            .unwrap()
            .into_iter()
            .find(|c| c.uid == uid)
            .unwrap();
        assert_eq!((again.name.as_str(), again.color.as_str()), ("Rig renamed", "#2ec27e"));

        let sub = subscribe("webcal://example.org/holidays.ics", "Holidays", "#c01c28").expect("subscribed");
        std::thread::sleep(std::time::Duration::from_millis(500));
        let listed = crate::calendar::eds::calendars(AccountNames::default()).unwrap();
        let s = listed.iter().find(|c| c.uid == sub).expect("subscription listed");
        assert_eq!(s.kind, CalKind::Subscription);
        delete(s).expect("unsubscribed");
        delete(&again).expect("calendar removed");
    }
}
