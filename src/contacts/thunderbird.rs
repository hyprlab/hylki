//! Contacts straight from Thunderbird's address books (#681), with no
//! export first. Since Thunderbird 78 each book is a SQLite file in the
//! profile: `abook.sqlite` is the Personal Address Book, `history.sqlite`
//! Collected Addresses, and `prefs.js` names the others. A card written by
//! Thunderbird 102 or later keeps its whole vCard under `_vCard`; an older
//! one only has fields of its own, which are matched as Thunderbird's CSV
//! export names them. Mailing lists become groups.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::categories::{card_categories, with_categories};
use super::csv_import::{contacts_from, detect, CsvContacts};
use super::local::{insert_uid, vcard_cards};
use crate::i18n::i18n;
use crate::mail::merge::Table;

/// One of Thunderbird's address books, as the import window lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TbBook {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    /// The profile's name, to tell two profiles' books apart.
    pub(crate) profile: String,
    pub(crate) contacts: usize,
    pub(crate) lists: usize,
    /// Collected Addresses: everyone ever written to, so not picked first.
    pub(crate) collected: bool,
}

/// Where Thunderbird keeps its profiles, under the home folder: its own
/// folder, the Flatpak's (Flathub's ID is lower case now, and was not at
/// first) and the Snap's.
const ROOTS: [&str; 4] = [
    ".thunderbird",
    ".var/app/org.mozilla.thunderbird/.thunderbird",
    ".var/app/org.mozilla.Thunderbird/.thunderbird",
    "snap/thunderbird/common/.thunderbird",
];

/// The folders on this computer that hold Thunderbird profiles.
pub(crate) fn profile_roots() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    ROOTS
        .iter()
        .map(|r| home.join(r))
        .filter(|r| r.join("profiles.ini").is_file())
        .collect()
}

/// The books in `dir`: a folder of profiles (with its `profiles.ini`), one
/// profile, or a folder holding `.thunderbird`, as a folder chooser may
/// leave any of them.
pub(crate) fn books_in(dir: &Path) -> Vec<TbBook> {
    if dir.join("profiles.ini").is_file() {
        return profiles_in(dir)
            .into_iter()
            .flat_map(|(name, path)| profile_books(&path, &name))
            .collect();
    }
    if dir.join("prefs.js").is_file() || dir.join("abook.sqlite").is_file() {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        return profile_books(dir, &name);
    }
    let inner = dir.join(".thunderbird");
    if inner.join("profiles.ini").is_file() {
        return books_in(&inner);
    }
    Vec::new()
}

/// The profiles `profiles.ini` in `root` lists, with their folders, the
/// one Thunderbird opens first leading.
fn profiles_in(root: &Path) -> Vec<(String, PathBuf)> {
    let Ok(text) = std::fs::read_to_string(root.join("profiles.ini")) else {
        return Vec::new();
    };
    let (profiles, default) = parse_profiles_ini(&text);
    let mut found: Vec<(String, PathBuf, bool)> = profiles
        .into_iter()
        .map(|p| {
            let first = default.as_deref() == Some(p.path.as_str()) || p.default;
            let path = if p.relative {
                root.join(&p.path)
            } else {
                PathBuf::from(&p.path)
            };
            (p.name, path, first)
        })
        .filter(|(_, path, _)| path.is_dir())
        .collect();
    found.sort_by_key(|(_, _, first)| !first);
    found.into_iter().map(|(n, p, _)| (n, p)).collect()
}

#[derive(Debug, Default, PartialEq)]
struct IniProfile {
    name: String,
    path: String,
    relative: bool,
    default: bool,
}

/// The `[ProfileN]` sections of `profiles.ini`, and the profile an
/// `[Install…]` section names as the one it opens.
fn parse_profiles_ini(text: &str) -> (Vec<IniProfile>, Option<String>) {
    let mut profiles = Vec::new();
    let mut install = None;
    let mut section = String::new();
    let mut current: Option<IniProfile> = None;
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            profiles.extend(current.take().filter(|p| !p.path.is_empty()));
            section = name.to_string();
            if section.starts_with("Profile") {
                current = Some(IniProfile {
                    relative: true,
                    ..IniProfile::default()
                });
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if let Some(p) = current.as_mut() {
            match key {
                "Name" => p.name = value.to_string(),
                "Path" => p.path = value.to_string(),
                "IsRelative" => p.relative = value == "1",
                "Default" => p.default = value == "1",
                _ => {}
            }
        } else if section.starts_with("Install") && key == "Default" && install.is_none() {
            install = Some(value.to_string());
        }
    }
    profiles.extend(current.filter(|p| !p.path.is_empty()));
    (profiles, install)
}

/// The books of the profile in `dir`.
fn profile_books(dir: &Path, profile: &str) -> Vec<TbBook> {
    let prefs = std::fs::read_to_string(dir.join("prefs.js")).unwrap_or_default();
    let mut books: Vec<TbBook> = book_files(&prefs)
        .into_iter()
        .filter_map(|(file, name)| {
            // An empty book has nothing to offer.
            let mut book = book_at(&dir.join(&file)).ok().filter(|b| b.contacts > 0)?;
            book.profile = profile.to_string();
            if let Some(name) = name {
                book.name = name;
            }
            Some(book)
        })
        .collect();
    books.sort_by_key(|b| b.collected);
    books
}

/// The address book files `prefs.js` names, with the name each was given;
/// the Personal Address Book and Collected Addresses are there unnamed when
/// nothing renamed them. A directory server (type 0) keeps no book here.
fn book_files(prefs: &str) -> Vec<(String, Option<String>)> {
    let mut servers: HashMap<String, HashMap<String, serde_json::Value>> = HashMap::new();
    for line in prefs.lines() {
        let Some((key, value)) = user_pref(line) else {
            continue;
        };
        let Some(rest) = key.strip_prefix("ldap_2.servers.") else {
            continue;
        };
        let Some((server, field)) = rest.split_once('.') else {
            continue;
        };
        servers
            .entry(server.to_string())
            .or_default()
            .insert(field.to_string(), value);
    }
    let text = |s: &HashMap<String, serde_json::Value>, f: &str| {
        s.get(f).and_then(|v| v.as_str()).map(str::to_string)
    };
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for (key, file) in [("pab", "abook.sqlite"), ("history", "history.sqlite")] {
        let name = servers.get(key).and_then(|s| text(s, "description"));
        out.push((file.to_string(), name));
    }
    let mut others: Vec<(String, Option<String>)> = servers
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "pab" | "history"))
        .filter(|(_, s)| s.get("dirType").and_then(|v| v.as_i64()) != Some(0))
        .filter_map(|(_, s)| {
            let file = text(s, "filename")?;
            let plain = !file.contains(['/', '\\']) && file.ends_with(".sqlite");
            plain.then(|| (file, text(s, "description")))
        })
        .collect();
    others.sort_by(|a, b| a.1.cmp(&b.1));
    for o in others {
        if !out.iter().any(|(f, _)| *f == o.0) {
            out.push(o);
        }
    }
    out
}

/// The key and value of a `user_pref("key", value);` line of `prefs.js`.
fn user_pref(line: &str) -> Option<(String, serde_json::Value)> {
    let inner = line
        .trim()
        .strip_prefix("user_pref(")?
        .strip_suffix(';')?
        .trim_end()
        .strip_suffix(')')?;
    let (key, value): (String, serde_json::Value) =
        serde_json::from_str(&format!("[{inner}]")).ok()?;
    Some((key, value))
}

/// Whether the file at `path` is a SQLite database, by its first bytes.
pub(crate) fn is_sqlite(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 16];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok_and(|()| &head == b"SQLite format 3\0")
}

/// The book in the file at `path`, named by its file until `prefs.js`
/// says otherwise.
pub(crate) fn book_at(path: &Path) -> Result<TbBook, String> {
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let collected = file == "history.sqlite";
    let name = match file.as_str() {
        "abook.sqlite" => i18n("Personal Address Book"),
        "history.sqlite" => i18n("Collected Addresses"),
        _ => file.trim_end_matches(".sqlite").to_string(),
    };
    let (contacts, lists) = with_copy(path, |db| {
        let count = |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0));
        let contacts = count("SELECT COUNT(DISTINCT card) FROM properties")?;
        let lists = count("SELECT COUNT(*) FROM lists").unwrap_or(0);
        Ok((contacts as usize, lists as usize))
    })?;
    let profile = path
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(TbBook {
        path: path.to_path_buf(),
        name,
        profile,
        contacts,
        lists,
        collected,
    })
}

/// Run `read` on a copy of the book at `path`. Thunderbird may have it
/// open, with recent changes still in its `-wal` file, so both are copied
/// to a folder of our own and read there, where nothing else writes.
fn with_copy<T>(
    path: &Path,
    read: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>,
) -> Result<T, String> {
    let dir = std::env::temp_dir().join(format!(
        "hylki-thunderbird-{}",
        crate::rng::token(12).map_err(|e| e.to_string())?
    ));
    crate::config::private_dir(&dir).map_err(|e| e.to_string())?;
    let result = (|| {
        let copy = dir.join("book.sqlite");
        std::fs::copy(path, &copy).map_err(|e| e.to_string())?;
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        if Path::new(&wal).is_file() {
            std::fs::copy(&wal, dir.join("book.sqlite-wal")).map_err(|e| e.to_string())?;
        }
        let db = rusqlite::Connection::open(&copy).map_err(|e| e.to_string())?;
        read(&db).map_err(|_| i18n("This is not a Thunderbird address book"))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// The fields of a card from before Thunderbird 102, and the column of
/// Thunderbird's CSV export each one is.
const FIELDS: [(&str, &str); 32] = [
    ("FirstName", "First Name"),
    ("LastName", "Last Name"),
    ("DisplayName", "Display Name"),
    ("NickName", "Nickname"),
    ("PrimaryEmail", "Primary Email"),
    ("SecondEmail", "Secondary Email"),
    ("WorkPhone", "Work Phone"),
    ("HomePhone", "Home Phone"),
    ("FaxNumber", "Fax Number"),
    ("PagerNumber", "Pager Number"),
    ("CellularNumber", "Mobile Number"),
    ("HomeAddress", "Home Address"),
    ("HomeAddress2", "Home Address 2"),
    ("HomeCity", "Home City"),
    ("HomeState", "Home State"),
    ("HomeZipCode", "Home ZipCode"),
    ("HomeCountry", "Home Country"),
    ("WorkAddress", "Work Address"),
    ("WorkAddress2", "Work Address 2"),
    ("WorkCity", "Work City"),
    ("WorkState", "Work State"),
    ("WorkZipCode", "Work ZipCode"),
    ("WorkCountry", "Work Country"),
    ("JobTitle", "Job Title"),
    ("Department", "Department"),
    ("Company", "Organization"),
    ("WebPage1", "Web Page 1"),
    ("WebPage2", "Web Page 2"),
    ("BirthYear", "Birth Year"),
    ("BirthMonth", "Birth Month"),
    ("BirthDay", "Birth Day"),
    ("Notes", "Notes"),
];

/// The contacts of the book at `path`, each with Thunderbird's UID so a
/// second import updates them, and its mailing lists as groups.
pub(crate) fn read_book(path: &Path) -> Result<CsvContacts, String> {
    let (cards, lists) = with_copy(path, |db| {
        let mut props: Vec<(String, HashMap<String, String>)> = Vec::new();
        let mut at: HashMap<String, usize> = HashMap::new();
        let mut stmt = db.prepare("SELECT card, name, value FROM properties ORDER BY rowid")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            ))
        })?;
        for row in rows {
            let (card, name, value) = row?;
            let i = *at.entry(card.clone()).or_insert_with(|| {
                props.push((card, HashMap::new()));
                props.len() - 1
            });
            props[i].1.insert(name, value);
        }
        // An older book may have no lists table; it then has no lists.
        let mut lists: Vec<(String, Vec<String>)> = Vec::new();
        if let Ok(mut stmt) = db.prepare(
            "SELECT l.name, c.card FROM lists l JOIN list_cards c ON c.list = l.uid \
             ORDER BY l.name, c.rowid",
        ) {
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (list, card) = row?;
                match lists.iter_mut().find(|(l, _)| *l == list) {
                    Some((_, cards)) => cards.push(card),
                    None => lists.push((list, vec![card])),
                }
            }
        }
        Ok((props, lists))
    })?;
    Ok(contacts_of(cards, lists))
}

/// Make the contacts of a book's cards and lists.
fn contacts_of(
    cards: Vec<(String, HashMap<String, String>)>,
    lists: Vec<(String, Vec<String>)>,
) -> CsvContacts {
    let columns: Vec<String> = FIELDS.iter().map(|(_, h)| h.to_string()).collect();
    let mut out = CsvContacts {
        cards: Vec::new(),
        groups: Vec::new(),
        skipped: 0,
    };
    let mut index: HashMap<String, usize> = HashMap::new();
    for (uid, props) in cards {
        let card = match props.get("_vCard").filter(|v| !v.trim().is_empty()) {
            Some(v) => vcard_cards(v.as_bytes())
                .ok()
                .and_then(|(found, _)| found.into_iter().next()),
            None => {
                let row: Vec<String> = FIELDS
                    .iter()
                    .map(|(p, _)| props.get(*p).cloned().unwrap_or_default())
                    .collect();
                let table = Table {
                    columns: columns.clone(),
                    rows: vec![row],
                };
                contacts_from(&table, &detect(&table))
                    .cards
                    .into_iter()
                    .next()
            }
        };
        let Some(card) = card else {
            out.skipped += 1;
            continue;
        };
        let has_uid = card
            .lines()
            .any(|l| l.len() >= 4 && l[..4].eq_ignore_ascii_case("UID:"));
        let card = if has_uid || uid.is_empty() {
            card
        } else {
            insert_uid(&card, &uid)
        };
        index.insert(uid, out.cards.len());
        out.cards.push(card);
    }
    let mut named: HashMap<usize, Vec<String>> = HashMap::new();
    for (list, members) in lists {
        let at: Vec<usize> = members
            .iter()
            .filter_map(|m| index.get(m).copied())
            .collect();
        if list.trim().is_empty() || at.is_empty() {
            continue;
        }
        for &i in &at {
            named.entry(i).or_default().push(list.clone());
        }
        out.groups.push((list, at));
    }
    // As a CSV row's labels are: a book that keeps no groups of its own
    // still shows them on each contact.
    for (i, lists) in named {
        let mut names = card_categories(&out.cards[i]);
        for l in lists {
            if !names.iter().any(|n| n.eq_ignore_ascii_case(&l)) {
                names.push(l);
            }
        }
        out.cards[i] = with_categories(&out.cards[i], &names);
    }
    out
}

#[cfg(test)]
#[path = "thunderbird_tests.rs"]
mod tests;
