//! Hylki's own calendars, for a desktop without Evolution Data Server: one
//! `.ics` file per calendar in `~/.local/share/hylki/calendars/`, with the
//! names and colors beside them in `calendars.toml`.
//!
//! A plain iCalendar file is what any other calendar app can import, so the
//! events are never locked in here.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::edit::Change;
use super::model::Book;
use super::store::{CalInfo, CalKind, Loaded};
use super::time::Zones;
use super::tree::Node;
use crate::i18n::i18n;

/// One write at a time: two quick edits must not read the same old file.
static LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    calendar: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    uid: String,
    name: String,
    color: String,
}

fn dir() -> Option<PathBuf> {
    Some(crate::config::data_base()?.join("hylki").join("calendars"))
}

fn index_path() -> Option<PathBuf> {
    Some(dir()?.join("calendars.toml"))
}

fn file(uid: &str) -> Option<PathBuf> {
    // A UID is made here, but a hand-edited index must not reach outside.
    let safe: String = uid
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    Some(dir()?.join(format!("{safe}.ics")))
}

fn read_index() -> Index {
    crate::config::read_toml(index_path()).unwrap_or_default()
}

fn write_index(index: &Index) {
    crate::config::save_private_toml(index_path(), index, "calendars.toml", true);
}

fn info(e: &Entry) -> CalInfo {
    CalInfo {
        uid: e.uid.clone(),
        name: e.name.clone(),
        account: None,
        color: e.color.clone(),
        writable: true,
        tasks: false,
        kind: CalKind::Local,
        parent: String::new(),
    }
}

/// The calendars, with a first one made the first time there are none.
pub(crate) fn calendars() -> Vec<CalInfo> {
    let _g = LOCK.lock();
    let mut index = read_index();
    if index.calendar.is_empty() {
        index.calendar.push(Entry {
            uid: super::edit::new_uid(),
            name: i18n("Personal"),
            color: "#3584e4".into(),
        });
        write_index(&index);
    }
    index.calendar.iter().map(info).collect()
}

fn read_nodes(uid: &str) -> Vec<Node> {
    let Some(text) = file(uid).and_then(|p| std::fs::read_to_string(p).ok()) else {
        return Vec::new();
    };
    Node::parse_all(&text)
        .into_iter()
        .flat_map(|n| {
            if n.name == "VCALENDAR" {
                n.children
            } else {
                vec![n]
            }
        })
        .collect()
}

fn write_nodes(uid: &str, nodes: &[Node]) -> Result<(), String> {
    let path = file(uid).ok_or_else(|| i18n("There is no data folder to keep calendars in."))?;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let mut cal = Node::new("VCALENDAR");
    cal.push("VERSION:2.0".into());
    cal.push(format!("PRODID:-//Hyprlab//{}//EN", crate::APP_NAME));
    cal.children = nodes.to_vec();
    crate::config::replace_file(&path, |tmp| crate::config::write_private(tmp, &cal.write()))
        .map_err(|e| e.to_string())
}

pub(crate) fn load(cal: &CalInfo) -> Result<Loaded, String> {
    let nodes = read_nodes(&cal.uid);
    let mut zones = Zones::default();
    let texts: Vec<String> = nodes.iter().map(Node::write).collect();
    let book = Book::read(&texts, &mut zones);
    Ok(Loaded {
        book,
        zones,
        writable: Some(true),
        watch: None,
    })
}

pub(crate) fn apply(cal: &CalInfo, changes: &[Change]) -> Result<(), String> {
    let _g = LOCK.lock();
    let mut nodes = read_nodes(&cal.uid);
    apply_to(&mut nodes, changes);
    write_nodes(&cal.uid, &nodes)
}

/// The changes made to a calendar kept as a list of components: what a
/// server does with them, done here.
pub(crate) fn apply_to(nodes: &mut Vec<Node>, changes: &[Change]) {
    let id = |n: &Node| (n.text("UID"), n.prop("RECURRENCE-ID").map(|l| l.raw.trim().to_string()));
    for change in changes {
        match change {
            Change::Create(node) | Change::Replace(node) => {
                let key = id(node);
                match nodes.iter().position(|n| id(n) == key) {
                    Some(i) => nodes[i] = node.clone(),
                    None => nodes.push(node.clone()),
                }
            }
            Change::Remove { uid } => nodes.retain(|n| n.text("UID") != *uid),
            Change::RemoveOne { uid, rid } => {
                let value = rid.value();
                nodes.retain(|n| {
                    !(n.text("UID") == *uid
                        && n
                            .prop("RECURRENCE-ID")
                            .is_some_and(|l| l.raw.trim() == value))
                });
                if let Some(master) = nodes
                    .iter_mut()
                    .find(|n| n.text("UID") == *uid && n.prop("RECURRENCE-ID").is_none())
                {
                    master.push(rid.line("EXDATE"));
                }
            }
        }
    }
}

pub(crate) fn new_calendar(name: &str, color: &str) -> Result<String, String> {
    let _g = LOCK.lock();
    let mut index = read_index();
    let uid = super::edit::new_uid();
    index.calendar.push(Entry {
        uid: uid.clone(),
        name: name.trim().to_string(),
        color: color.to_string(),
    });
    write_index(&index);
    Ok(uid)
}

pub(crate) fn set_look(cal: &CalInfo, name: &str, color: &str) -> Result<(), String> {
    let _g = LOCK.lock();
    let mut index = read_index();
    if let Some(e) = index.calendar.iter_mut().find(|e| e.uid == cal.uid) {
        if !name.trim().is_empty() {
            e.name = name.trim().to_string();
        }
        e.color = color.to_string();
    }
    write_index(&index);
    Ok(())
}

pub(crate) fn delete(cal: &CalInfo) -> Result<(), String> {
    let _g = LOCK.lock();
    let mut index = read_index();
    index.calendar.retain(|e| e.uid != cal.uid);
    write_index(&index);
    if let Some(p) = file(&cal.uid) {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::time::Stamp;

    #[test]
    fn changes_land_on_the_component_list() {
        let ev = |uid: &str, extra: &str| {
            Node::parse(&format!(
                "BEGIN:VEVENT\nUID:{uid}\nDTSTART:20261102T090000Z\n{extra}END:VEVENT\n"
            ))
            .unwrap()
        };
        let mut nodes = Vec::new();
        apply_to(
            &mut nodes,
            &[
                Change::Create(ev("a", "RRULE:FREQ=DAILY\n")),
                Change::Create(ev("b", "")),
                Change::Replace(ev("a", "RECURRENCE-ID:20261103T090000Z\nSUMMARY:moved\n")),
            ],
        );
        assert_eq!(nodes.len(), 3);
        apply_to(
            &mut nodes,
            &[Change::RemoveOne {
                uid: "a".into(),
                rid: Stamp::parse("20261103T090000Z", None, false).unwrap(),
            }],
        );
        assert_eq!(nodes.len(), 2);
        assert!(nodes[0].text("EXDATE").contains("20261103T090000Z"));
        apply_to(&mut nodes, &[Change::Remove { uid: "b".into() }]);
        assert_eq!(nodes.len(), 1);
    }
}
