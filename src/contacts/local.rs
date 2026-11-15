//! Hylki's own address book, kept in an application SQLite file.
//!
//! It sits next to the Evolution Data Server (EDS) books: people who do not
//! run EDS can still keep contacts, and everyone can import `.vcf` files.
//! vCards are stored verbatim, so properties Hylki does not edit (PHOTO, ADR,
//! X-...) survive a round trip, exactly as with an EDS book. The
//! `contact_email` table is derived data, rebuilt on every write. A contact
//! group is stored like a contact, as an Evolution list vCard; its members
//! are not indexed there, since they are not the group's own addresses.
//!
//! This module is the only code that touches `local_contacts.db`;
//! `contacts.rs` routes to it by [`LOCAL_BOOK_UID`].

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};

use super::details::{GroupFormat, Member};
use super::edit::{new_group_vcard, patched_group_vcard, GroupEdit};
use crate::i18n::{i18n, i18n_f};

/// The reserved `book_uid` of the local book.
pub(crate) const LOCAL_BOOK_UID: &str = "hylki-local";

/// Largest `.vcf` file read in one go.
const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// Largest single vCard accepted (a contact with an embedded photo can be big).
const MAX_VCARD_BYTES: usize = 5 * 1024 * 1024;
/// The stored cards that may be groups, by their text: an Evolution list,
/// or a vCard 4 or Apple group. LIKE ignores ASCII case.
const GROUP_WHERE: &str = "vcard LIKE '%X-EVOLUTION-LIST:TRUE%' OR vcard LIKE '%KIND:group%'";
/// Schema version stored in `PRAGMA user_version`.
const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS contact (
    uid     TEXT PRIMARY KEY,
    vcard   TEXT NOT NULL,
    name    TEXT NOT NULL,
    updated INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS contact_email (
    uid         TEXT NOT NULL REFERENCES contact(uid) ON DELETE CASCADE,
    email_lower TEXT NOT NULL,
    email       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS contact_email_idx ON contact_email(email_lower);
";

/// What an import did. Counts are per contact, except `failed_files`, which
/// names the files that could not be read at all (with the reason).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ImportOutcome {
    pub(crate) added: usize,
    pub(crate) updated: usize,
    /// Blocks with neither a name nor an email, or a group with no name.
    pub(crate) skipped: usize,
    /// Truncated or oversized vCards.
    pub(crate) errors: usize,
    pub(crate) failed_files: Vec<String>,
}

impl ImportOutcome {
    /// Add another outcome's counts to this one.
    fn merge(&mut self, other: ImportOutcome) {
        self.added += other.added;
        self.updated += other.updated;
        self.skipped += other.skipped;
        self.errors += other.errors;
        self.failed_files.extend(other.failed_files);
    }
}

/// How one vCard fared on its way into the store.
#[derive(Debug, PartialEq, Eq)]
enum Upsert {
    Added,
    Updated,
    Invalid,
    TooBig,
    /// A UID already met earlier in the same import: two different people
    /// numbered alike by their exporter, or a file holding a card twice.
    /// The first one is kept rather than overwritten.
    Duplicate,
}

/// An open `local_contacts.db`.
struct Store {
    conn: Connection,
}

/// Where the book lives, in the application data dir.
fn db_path() -> Option<PathBuf> {
    crate::config::data_base().map(|d| d.join("hylki").join("local_contacts.db"))
}

impl Store {
    /// Open (creating if needed) the database for writing.
    fn open() -> Result<Store, String> {
        let path = db_path().ok_or("no XDG data directory for the local address book")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        Store::open_at(&path)
    }

    /// Open the database for reading only, or `None` while there is no book
    /// yet. A read never writes the file: the photo watcher fingerprints it,
    /// and a write on every read made it look changed on every look.
    fn open_read() -> Result<Option<Store>, String> {
        let Some(path) = db_path().filter(|p| p.is_file()) else {
            return Ok(None);
        };
        Store::open_read_at(&path)
    }

    fn open_read_at(path: &Path) -> Result<Option<Store>, String> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI;
        let conn = Connection::open_with_flags(path, flags)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        match schema_version(&conn).map_err(|e| format!("{}: {e}", path.display()))? {
            0 => Ok(None),
            _ => Ok(Some(Store { conn })),
        }
    }

    /// Open the database at `path` for writing. A new file is made owner-only
    /// before SQLite writes anything into it.
    fn open_at(path: &Path) -> Result<Store, String> {
        {
            use std::os::unix::fs::OpenOptionsExt;
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path);
        }
        let conn = Connection::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        restrict(path);
        Store::init(conn).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Enable foreign keys, and create the schema when the file has none yet.
    fn init(conn: Connection) -> Result<Store, String> {
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(|e| e.to_string())?;
        if schema_version(&conn)? < SCHEMA_VERSION {
            conn.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
            conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
                .map_err(|e| e.to_string())?;
        }
        Ok(Store { conn })
    }

    /// An in-memory store, for tests.
    #[cfg(test)]
    fn in_memory() -> Store {
        Store::init(Connection::open_in_memory().unwrap()).unwrap()
    }

    /// Insert or update every vCard in one transaction and count the results.
    fn upsert_all(&mut self, vcards: &[String]) -> Result<ImportOutcome, String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        let mut out = ImportOutcome::default();
        let mut seen = std::collections::HashSet::new();
        for vcard in vcards {
            match upsert_tx(&tx, vcard, true, Some(&mut seen))? {
                Upsert::Added => out.added += 1,
                Upsert::Updated => out.updated += 1,
                Upsert::Invalid | Upsert::Duplicate => out.skipped += 1,
                Upsert::TooBig => out.errors += 1,
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(out)
    }

    /// Insert or update one vCard, failing when it is not a usable contact.
    fn upsert_one(&mut self, vcard: &str) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        match upsert_tx(&tx, vcard, false, None)? {
            Upsert::Added | Upsert::Updated | Upsert::Duplicate => {}
            Upsert::Invalid => {
                return Err(i18n(
                    "A contact needs a name, an email address or a phone number",
                ))
            }
            Upsert::TooBig => return Err(i18n("The contact is too large")),
        }
        tx.commit().map_err(|e| e.to_string())
    }

    /// Add `groups`' members to the book's groups of the same names (any
    /// case), making the groups that are not there yet. Only Evolution
    /// lists, the kind the book writes, are added to.
    fn merge_groups(&mut self, groups: &[(String, Vec<Member>)]) -> Result<(), String> {
        if groups.is_empty() {
            return Ok(());
        }
        let existing: Vec<_> = self
            .group_vcards()?
            .into_iter()
            .filter_map(|v| {
                let mut g = crate::contacts::parse_vcard_fields(&v)?;
                // The parse leaves the text out; the patch keeps its UID.
                g.raw_vcard = v;
                Some(g)
            })
            .filter(|g| g.group == Some(GroupFormat::Evolution))
            .collect();
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        for (name, members) in groups {
            let card = match existing
                .iter()
                .find(|g| g.name.trim().eq_ignore_ascii_case(name.trim()))
            {
                Some(g) => {
                    let mut all = g.members.clone();
                    let before = all.len();
                    for m in members {
                        if !all.iter().any(|a| a.same(m)) {
                            all.push(m.clone());
                        }
                    }
                    if all.len() == before {
                        continue;
                    }
                    let edit = GroupEdit {
                        name: g.name.clone(),
                        members: all,
                        hide_addresses: g.hide_addresses,
                        kept: g.hidden_members.clone(),
                    };
                    patched_group_vcard(&g.raw_vcard, &edit)
                }
                None => new_group_vcard(&GroupEdit {
                    name: name.clone(),
                    members: members.clone(),
                    ..GroupEdit::default()
                }),
            };
            upsert_tx(&tx, &card, true, None)?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    /// Remove a contact (its email rows go with it).
    fn delete(&self, uid: &str) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM contact WHERE uid = ?1", [uid])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Every stored vCard, sorted by display name.
    fn vcards(&self) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT vcard FROM contact ORDER BY name COLLATE NOCASE, uid")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    /// One stored vCard by its UID.
    fn vcard(&self, uid: &str) -> Result<Option<String>, String> {
        self.conn
            .query_row("SELECT vcard FROM contact WHERE uid = ?1", [uid], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|e| e.to_string())
    }

    /// The vCards of the book's contact groups. Matched on the text; the
    /// caller parses each to be sure.
    fn group_vcards(&self) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT vcard FROM contact WHERE {GROUP_WHERE} ORDER BY name COLLATE NOCASE, uid"
            ))
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    /// How many people the book holds: its contacts less its groups.
    fn people(&self) -> usize {
        let sql = format!("SELECT count(*) FROM contact WHERE NOT ({GROUP_WHERE})");
        self.conn
            .query_row(&sql, [], |r| r.get::<_, i64>(0))
            .map_or(0, |n| n as usize)
    }

    /// (display name, email) for every address in the book.
    fn emails(&self) -> Result<Vec<(String, String)>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.name, e.email FROM contact_email e \
                 JOIN contact c ON c.uid = e.uid ORDER BY c.name COLLATE NOCASE, e.email",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    /// (lower-cased email, uid) of the contacts whose vCard carries a PHOTO.
    fn photo_entries(&self) -> Result<Vec<(String, String)>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT e.email_lower, c.uid FROM contact_email e \
                 JOIN contact c ON c.uid = e.uid \
                 WHERE c.vcard LIKE '%' || char(10) || 'PHOTO%'",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }
}

/// The schema version the file was written with, refusing a newer one.
fn schema_version(conn: &Connection) -> Result<i64, String> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if version > SCHEMA_VERSION {
        return Err(format!(
            "the local address book is from a newer Hylki (schema {version})"
        ));
    }
    Ok(version)
}

/// Restrict the database file to its owner (the book holds personal data).
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

/// Write one vCard into the open transaction, keeping `contact_email` in sync.
/// A vCard without a UID gets one: derived from its content when `stable_uid`
/// (imports, so importing the same file twice updates instead of duplicating),
/// random otherwise (a contact made in the editor).
fn upsert_tx(
    tx: &Transaction,
    vcard: &str,
    stable_uid: bool,
    seen: Option<&mut std::collections::HashSet<String>>,
) -> Result<Upsert, String> {
    if vcard.len() > MAX_VCARD_BYTES {
        return Ok(Upsert::TooBig);
    }
    let Some(details) = crate::contacts::parse_vcard_fields(vcard) else {
        return Ok(Upsert::Invalid);
    };
    let (uid, vcard) = if details.eds_uid.is_empty() {
        let uid = if stable_uid {
            content_uid(vcard)
        } else {
            random_uid()?
        };
        let with_uid = insert_uid(vcard, &uid);
        (uid, with_uid)
    } else {
        (details.eds_uid.clone(), vcard.to_string())
    };
    if let Some(seen) = seen {
        if !seen.insert(uid.clone()) {
            return Ok(Upsert::Duplicate);
        }
    }
    let existed = tx
        .query_row("SELECT 1 FROM contact WHERE uid = ?1", [&uid], |_| Ok(()))
        .optional()
        .map_err(|e| e.to_string())?
        .is_some();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    tx.execute(
        "INSERT INTO contact (uid, vcard, name, updated) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(uid) DO UPDATE SET vcard = ?2, name = ?3, updated = ?4",
        params![uid, vcard, details.name, now],
    )
    .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM contact_email WHERE uid = ?1", [&uid])
        .map_err(|e| e.to_string())?;
    for email in &details.emails {
        tx.execute(
            "INSERT INTO contact_email (uid, email_lower, email) VALUES (?1, ?2, ?3)",
            params![uid, email.value.to_lowercase(), email.value],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(if existed {
        Upsert::Updated
    } else {
        Upsert::Added
    })
}

/// A random UID for a contact created in the editor.
fn random_uid() -> Result<String, String> {
    Ok(format!(
        "hylki-{}",
        crate::rng::token(24).map_err(|e| e.to_string())?
    ))
}

/// A UID derived from the vCard text, so the same card always maps to the same contact.
fn content_uid(vcard: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(vcard.replace("\r\n", "\n").trim().as_bytes());
    let hex: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
    format!("hylki-{hex}")
}

/// Insert a `UID:` line just before `END:VCARD` (returns the text unchanged
/// when there is no end marker).
fn insert_uid(vcard: &str, uid: &str) -> String {
    let Some(pos) = vcard.to_ascii_uppercase().rfind("END:VCARD") else {
        return vcard.to_string();
    };
    let mut out = String::with_capacity(vcard.len() + uid.len() + 8);
    out.push_str(&vcard[..pos]);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str("\r\n");
    }
    out.push_str(&format!("UID:{uid}\r\n"));
    out.push_str(&vcard[pos..]);
    out
}

/// Split the text of a `.vcf` file into its `BEGIN:VCARD` ... `END:VCARD`
/// blocks, each kept verbatim (with CRLF line ends). Returns the blocks and
/// how many were cut short (a `BEGIN` with no `END`). Text between blocks is
/// ignored. Continuation lines (folding) stay inside their block.
fn split_vcards(text: &str) -> (Vec<String>, usize) {
    let mut blocks = Vec::new();
    let mut truncated = 0;
    let mut current: Option<String> = None;
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        let marker = line.trim_end();
        if marker.eq_ignore_ascii_case("BEGIN:VCARD") {
            if current.is_some() {
                truncated += 1;
            }
            current = Some(String::new());
        }
        let Some(block) = current.as_mut() else {
            continue;
        };
        block.push_str(line);
        block.push_str("\r\n");
        if marker.eq_ignore_ascii_case("END:VCARD") {
            blocks.extend(current.take());
        }
    }
    if current.is_some() {
        truncated += 1;
    }
    (blocks, truncated)
}

/// Decode the bytes of a `.vcf` file: UTF-8 (a BOM is dropped), else Latin-1,
/// which older exporters use. UTF-16 and binary data are refused.
fn decode_text(bytes: &[u8]) -> Result<String, String> {
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return Err(i18n("UTF-16 files are not supported"));
    }
    if bytes.contains(&0) {
        return Err(i18n("The file is not a text file"));
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => Ok(s.to_string()),
        Err(_) => Ok(bytes.iter().map(|&b| b as char).collect()),
    }
}

/// Rewrite the quoted-printable properties of a vCard 2.1 block as plain
/// text, as 3.0 and 4.0 write them: phones export names such as
/// `FN;CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:J=C3=BCrgen`, with `=` at a
/// line's end continuing the value on the next line. Other lines are kept
/// as they are.
fn decode_quoted_printable(block: &str) -> String {
    let mut out = String::with_capacity(block.len());
    let mut lines = block.split("\r\n").peekable();
    while let Some(line) = lines.next() {
        let Some((prop, value)) = line.split_once(':') else {
            push_line(&mut out, line);
            continue;
        };
        let mut params = prop.split(';');
        let name = params.next().unwrap_or("");
        let params: Vec<&str> = params.collect();
        let qp = params.iter().any(|p| {
            p.eq_ignore_ascii_case("ENCODING=QUOTED-PRINTABLE")
                || p.eq_ignore_ascii_case("QUOTED-PRINTABLE")
        });
        if !qp {
            push_line(&mut out, line);
            continue;
        }
        let mut raw = value.to_string();
        while raw.ends_with('=') {
            raw.pop();
            match lines.next() {
                Some(next) => raw.push_str(next),
                None => break,
            }
        }
        let charset = params
            .iter()
            .find_map(|p| {
                p.split_once('=')
                    .filter(|(k, _)| k.eq_ignore_ascii_case("CHARSET"))
                    .map(|(_, v)| v)
            })
            .unwrap_or("UTF-8");
        let bytes = qp_bytes(&raw);
        let text = if charset.eq_ignore_ascii_case("UTF-8") {
            String::from_utf8(bytes)
                .unwrap_or_else(|e| e.into_bytes().iter().map(|&b| b as char).collect())
        } else {
            // ISO-8859-1 and its Windows cousin, which older phones use.
            bytes.iter().map(|&b| b as char).collect()
        };
        let kept: Vec<&str> = params
            .iter()
            .copied()
            .filter(|p| {
                !p.eq_ignore_ascii_case("ENCODING=QUOTED-PRINTABLE")
                    && !p.eq_ignore_ascii_case("QUOTED-PRINTABLE")
                    && !p.to_ascii_uppercase().starts_with("CHARSET=")
            })
            .collect();
        let mut rebuilt = name.to_string();
        for p in kept {
            rebuilt.push(';');
            rebuilt.push_str(p);
        }
        rebuilt.push(':');
        rebuilt.push_str(&text.replace("\r\n", "\\n").replace('\n', "\\n"));
        push_line(&mut out, &rebuilt);
    }
    out
}

/// Append a line and its CRLF, unless it is the empty tail after the last.
fn push_line(out: &mut String, line: &str) {
    if line.is_empty() {
        return;
    }
    out.push_str(line);
    out.push_str("\r\n");
}

/// The bytes a quoted-printable value stands for: `=XX` is a byte, anything
/// else is itself.
fn qp_bytes(raw: &str) -> Vec<u8> {
    let b = raw.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'='
            && i + 2 < b.len()
            && b[i + 1].is_ascii_hexdigit()
            && b[i + 2].is_ascii_hexdigit()
        {
            let hex = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
            out.push(hex(b[i + 1]) << 4 | hex(b[i + 2]));
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Import the vCards found in `text` into the store, in one transaction.
fn import_text_into(store: &mut Store, text: &str) -> Result<ImportOutcome, String> {
    let (blocks, truncated) = split_vcards(text);
    let blocks: Vec<String> = blocks.iter().map(|b| decode_quoted_printable(b)).collect();
    let mut out = store.upsert_all(&blocks)?;
    out.errors += truncated;
    Ok(out)
}

/// Import the contacts of a CSV file (#655), and add each to the groups
/// its row names: an existing group of the same name, or a new one.
fn import_csv_into(store: &mut Store, bytes: &[u8]) -> Result<ImportOutcome, String> {
    let found = super::csv_import::csv_contacts(bytes)?;
    let mut out = store.upsert_all(&found.cards)?;
    out.skipped += found.skipped;
    store.merge_groups(&found.groups)?;
    Ok(out)
}

/// Import several `.vcf` or CSV files. A file that cannot be read or
/// stored is listed in `failed_files`; the others are still imported.
pub(crate) fn import_files(paths: &[PathBuf]) -> Result<ImportOutcome, String> {
    let mut store = Store::open()?;
    let mut total = ImportOutcome::default();
    for path in paths {
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let imported = read_file(path).and_then(|bytes| {
            if is_csv(path, &bytes) {
                import_csv_into(&mut store, &bytes)
            } else {
                decode_text(&bytes).and_then(|text| import_text_into(&mut store, &text))
            }
        });
        match imported {
            Ok(outcome) => total.merge(outcome),
            Err(e) => total.failed_files.push(format!("{label}: {e}")),
        }
    }
    Ok(total)
}

/// Whether a file is a CSV rather than vCards: by its name, else by
/// having no vCard in it.
fn is_csv(path: &Path, bytes: &[u8]) -> bool {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "csv" | "tsv" => true,
        "vcf" | "vcard" => false,
        _ => !bytes
            .windows(11)
            .any(|w| w.eq_ignore_ascii_case(b"BEGIN:VCARD")),
    }
}

/// Read one file to import, refusing anything over [`MAX_FILE_BYTES`].
fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_FILE_BYTES {
        return Err(i18n_f(
            "The file is larger than {size} MB",
            &[("size", &(MAX_FILE_BYTES / 1024 / 1024).to_string())],
        ));
    }
    std::fs::read(path).map_err(|e| e.to_string())
}

/// The vCards of the local book's contact groups.
pub(crate) fn group_vcards() -> Result<Vec<String>, String> {
    Store::open_read()?.map_or(Ok(Vec::new()), |s| s.group_vcards())
}

/// Every stored vCard, sorted by display name.
pub(crate) fn list_vcards() -> Result<Vec<String>, String> {
    Store::open_read()?.map_or(Ok(Vec::new()), |s| s.vcards())
}

/// One stored vCard by UID.
pub(crate) fn vcard_by_uid(uid: &str) -> Result<Option<String>, String> {
    Store::open_read()?.map_or(Ok(None), |s| s.vcard(uid))
}

/// (display name, email) of every address in the local book.
pub(crate) fn emails() -> Result<Vec<(String, String)>, String> {
    Store::open_read()?.map_or(Ok(Vec::new()), |s| s.emails())
}

/// (lower-cased email, uid) of the local contacts that have a photo.
pub(crate) fn photo_entries() -> Result<Vec<(String, String)>, String> {
    Store::open_read()?.map_or(Ok(Vec::new()), |s| s.photo_entries())
}

/// Add a contact from a full vCard (a missing UID is generated).
pub(crate) fn create(vcard: &str) -> Result<(), String> {
    Store::open()?.upsert_one(vcard)
}

/// Replace a stored contact with its edited vCard.
pub(crate) fn modify(vcard: &str) -> Result<(), String> {
    Store::open()?.upsert_one(vcard)
}

/// Delete a contact by UID.
pub(crate) fn delete(uid: &str) -> Result<(), String> {
    Store::open()?.delete(uid)
}

/// How many contacts the book holds, its groups left out.
pub(crate) fn count() -> usize {
    Store::open_read().ok().flatten().map_or(0, |s| s.people())
}

/// Write every contact into one `.vcf` file at `path`, returning how many.
pub(crate) fn export_to(path: &Path) -> Result<usize, String> {
    let vcards = list_vcards()?;
    let mut text = String::new();
    for v in &vcards {
        text.push_str(v.trim_end_matches(['\r', '\n']));
        text.push_str("\r\n");
    }
    crate::config::write_private(path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(vcards.len())
}

/// Delete every contact in the book, returning how many there were.
pub(crate) fn delete_all() -> Result<usize, String> {
    if !exists() {
        return Ok(0);
    }
    let store = Store::open()?;
    store
        .conn
        .execute("DELETE FROM contact", [])
        .map_err(|e| e.to_string())
}

/// Whether the database file exists yet (nothing to read before the first write).
pub(crate) fn exists() -> bool {
    db_path().is_some_and(|p| p.is_file())
}

/// The file the fingerprint watcher should look at, with its journal files.
pub(crate) fn db_paths() -> Vec<PathBuf> {
    let Some(db) = db_path() else {
        return Vec::new();
    };
    ["", "-journal", "-wal"]
        .iter()
        .map(|suffix| PathBuf::from(format!("{}{suffix}", db.to_string_lossy())))
        .collect()
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
