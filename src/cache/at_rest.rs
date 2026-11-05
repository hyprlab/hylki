//! The cache encrypted at rest: SQLCipher, with a random key kept in the
//! keyring. The cache holds every cached body and attachment, mail a bridge
//! decrypted from end-to-end encryption included, so a copied disk, a
//! backup or another program reading the home directory must not get them.
//!
//! The key is read once, before the window is built ([`prepare`]), and every
//! connection opened after that is keyed with it. Nothing falls back to an
//! unencrypted file: a key that is missing, wrong or out of reach leaves the
//! cache locked, and the caller asks the user what to do.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rusqlite::{Connection, OpenFlags};

/// What every encrypted connection must report after it is keyed. These are
/// SQLCipher 4's defaults; they are checked rather than trusted, so a library
/// that changed them, or a file made with others, is refused instead of read
/// with weaker settings.
const CIPHER_SETTINGS: [(&str, &str); 6] = [
    ("cipher", "aes-256-cbc"),
    ("cipher_page_size", "4096"),
    ("cipher_hmac_algorithm", "HMAC_SHA512"),
    ("cipher_kdf_algorithm", "PBKDF2_HMAC_SHA512"),
    ("cipher_plaintext_header_size", "0"),
    ("cipher_memory_security", "1"),
];

/// The first 16 bytes of every unencrypted SQLite database.
const PLAIN_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// The key to the cache: 32 random bytes, as 64 hex digits. It is handed to
/// SQLCipher as a raw key, so opening a connection costs no key derivation.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Key(std::sync::Arc<str>);

impl Key {
    /// A new key from the system's random source.
    pub(crate) fn generate() -> Result<Key, String> {
        let mut bytes = [0u8; 32];
        crate::rng::fill(&mut bytes).map_err(|e| format!("no random bytes for a key: {e}"))?;
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        bytes.fill(0);
        Ok(Key(hex.into()))
    }

    /// A key as the keyring keeps it; `None` for anything Hylki did not write.
    pub(crate) fn from_hex(text: &str) -> Option<Key> {
        let text = text.trim();
        (text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| Key(text.to_ascii_lowercase().into()))
    }

    pub(crate) fn hex(&self) -> &str {
        &self.0
    }

    /// SQLCipher's raw-key form. Only hex digits go in, so it cannot carry
    /// anything else into the statement it is put in.
    fn sql_literal(&self) -> String {
        format!("x'{}'", self.0)
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key(..)")
    }
}

/// How the cache file is stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Protection {
    Encrypted(Key),
    /// Settings → Privacy → Encrypt the mail cache, switched off.
    Plain,
}

/// Set once the cache is unlocked; until then no connection opens.
static UNLOCKED: OnceLock<Protection> = OnceLock::new();

/// Let every later [`super::Cache::open`] use `protection`. Only the first
/// call counts: the key cannot change while connections are open.
pub(crate) fn unlock(protection: Protection) {
    let _ = UNLOCKED.set(protection);
}

/// How to open the cache, or an error while it is locked (no key read yet,
/// or the demo, which never touches the real cache).
pub(super) fn unlocked() -> rusqlite::Result<Protection> {
    UNLOCKED.get().cloned().ok_or_else(|| {
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
            Some("the mail cache is locked".to_string()),
        )
    })
}

/// Open the database at `path` and key it. An encrypted connection is
/// checked before it is handed out: the key must open the file, and the
/// cipher must be the one [`CIPHER_SETTINGS`] names.
pub(super) fn connect(
    path: &Path,
    flags: OpenFlags,
    protection: &Protection,
) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(path, flags)?;
    if let Protection::Encrypted(key) = protection {
        // Process-wide, and before the key: SQLCipher then wipes every
        // allocation it frees, key material and decrypted pages included.
        conn.execute_batch("PRAGMA cipher_memory_security = ON;")?;
        conn.pragma_update(None, "key", key.sql_literal())?;
        check_cipher(&conn)?;
    }
    Ok(conn)
}

/// Read through the key, then compare the cipher with [`CIPHER_SETTINGS`].
pub(super) fn check_cipher(conn: &Connection) -> rusqlite::Result<()> {
    // The first read is what tells a wrong key: SQLCipher answers
    // `NotADatabase` when the first page does not decrypt.
    conn.query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))?;
    for (pragma, want) in CIPHER_SETTINGS {
        let got: String = conn
            .query_row(&format!("PRAGMA {pragma}"), [], |r| r.get(0))
            .unwrap_or_default();
        if !got.eq_ignore_ascii_case(want) {
            return Err(cipher_error(format!(
                "{pragma} is {got:?}, expected {want:?}"
            )));
        }
    }
    let version: String = conn
        .query_row("PRAGMA cipher_version", [], |r| r.get(0))
        .unwrap_or_default();
    if !version.starts_with("4.") {
        return Err(cipher_error(format!("SQLCipher {version:?}, expected 4")));
    }
    Ok(())
}

fn cipher_error(detail: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISUSE),
        Some(format!("unexpected cipher settings: {detail}")),
    )
}

/// What is on disk at the cache's path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileState {
    Missing,
    Plain,
    Encrypted,
}

/// Tell an unencrypted cache from an encrypted one by its first bytes.
pub(crate) fn file_state(db: &Path) -> std::io::Result<FileState> {
    let mut head = Vec::with_capacity(PLAIN_MAGIC.len());
    match std::fs::File::open(db) {
        Ok(f) => {
            f.take(PLAIN_MAGIC.len() as u64).read_to_end(&mut head)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FileState::Missing),
        Err(e) => return Err(e),
    }
    if head.as_slice() == PLAIN_MAGIC {
        return Ok(FileState::Plain);
    }
    if !head.is_empty() {
        return Ok(FileState::Encrypted);
    }
    // An empty file can still have its whole content in a write-ahead log a
    // crash left behind; that log decides.
    if !std::fs::metadata(sidecar(db, "-wal")).is_ok_and(|m| m.len() > 0) {
        return Ok(FileState::Missing);
    }
    let plain = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .and_then(|c| c.query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(())));
    Ok(if plain.is_ok() {
        FileState::Plain
    } else {
        FileState::Encrypted
    })
}

/// The SQLite files beside `db` that hold its pages too.
const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];

pub(super) fn sidecar(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Where a converted copy is built before it replaces the cache.
fn staging_path(db: &Path) -> PathBuf {
    sidecar(db, ".rewrite")
}

/// Where the keyring keeps the cache's key.
pub(crate) trait KeyStore {
    /// The key, `Ok(None)` when there is none, `Err` when the keyring did
    /// not answer or is locked.
    fn read(&self) -> Result<Option<String>, String>;
    fn store(&self, key: &Key) -> Result<(), String>;
}

/// Why the cache stays locked.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Problem {
    /// The keyring did not answer, is locked, or refused to keep a key.
    KeyringUnavailable(String),
    /// The cache is encrypted and the keyring holds no key for it.
    KeyMissing,
    /// The keyring's key does not open the cache.
    WrongKey(String),
    /// Encrypting an unencrypted cache, or the reverse, failed; the cache is
    /// as it was.
    Conversion(String),
    /// The file could not be read at all.
    Storage(String),
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::KeyringUnavailable(e) => write!(f, "the keyring is unavailable: {e}"),
            Problem::KeyMissing => write!(f, "the keyring holds no key for the encrypted cache"),
            Problem::WrongKey(e) => write!(f, "the keyring's key does not open the cache: {e}"),
            Problem::Conversion(e) => write!(f, "the cache could not be converted: {e}"),
            Problem::Storage(e) => write!(f, "the cache could not be read: {e}"),
        }
    }
}

/// Get the cache at `db` ready to open: read or make its key, encrypt an
/// unencrypted cache (or the reverse, when `encrypt` is off), and check the
/// key opens it. On success every later connection uses the answer; on
/// failure the files are as they were.
pub(crate) fn prepare(
    db: &Path,
    encrypt: bool,
    keys: &dyn KeyStore,
) -> Result<Protection, Problem> {
    remove_with_sidecars(&staging_path(db)).map_err(|e| Problem::Storage(e.to_string()))?;
    let state = file_state(db).map_err(|e| Problem::Storage(e.to_string()))?;
    match (state, encrypt) {
        (FileState::Missing, false) | (FileState::Plain, false) => Ok(Protection::Plain),
        (FileState::Missing, true) => {
            // Sidecars a crash left beside no database would be replayed
            // into the new one.
            for suffix in SIDECARS {
                remove_file(&sidecar(db, suffix)).map_err(|e| Problem::Storage(e.to_string()))?;
            }
            Ok(Protection::Encrypted(key_or_new(keys)?))
        }
        (FileState::Plain, true) => {
            let to = Protection::Encrypted(key_or_new(keys)?);
            convert(db, &Protection::Plain, &to).map_err(Problem::Conversion)?;
            tracing::info!("cache: {} is encrypted now", db.display());
            Ok(to)
        }
        (FileState::Encrypted, _) => {
            let key = existing_key(keys)?;
            let from = Protection::Encrypted(key);
            if let Err(e) = connect(db, OpenFlags::SQLITE_OPEN_READ_WRITE, &from) {
                return Err(match e.sqlite_error_code() {
                    Some(rusqlite::ErrorCode::NotADatabase) => Problem::WrongKey(e.to_string()),
                    _ => Problem::Storage(e.to_string()),
                });
            }
            if encrypt {
                return Ok(from);
            }
            convert(db, &from, &Protection::Plain).map_err(Problem::Conversion)?;
            tracing::info!("cache: {} is no longer encrypted", db.display());
            Ok(Protection::Plain)
        }
    }
}

/// The keyring's key; an encrypted cache without one cannot be opened.
fn existing_key(keys: &dyn KeyStore) -> Result<Key, Problem> {
    match keys.read().map_err(Problem::KeyringUnavailable)? {
        Some(text) => Key::from_hex(&text)
            .ok_or_else(|| Problem::WrongKey("the keyring's entry is not a cache key".into())),
        None => Err(Problem::KeyMissing),
    }
}

/// The keyring's key, or a new one stored there and read back first. A key
/// the keyring did not really keep would lock the cache for good at the next
/// start, so nothing is encrypted with it until it is seen there.
fn key_or_new(keys: &dyn KeyStore) -> Result<Key, Problem> {
    if let Some(text) = keys.read().map_err(Problem::KeyringUnavailable)? {
        return Key::from_hex(&text)
            .ok_or_else(|| Problem::WrongKey("the keyring's entry is not a cache key".into()));
    }
    let key = Key::generate().map_err(Problem::KeyringUnavailable)?;
    keys.store(&key).map_err(Problem::KeyringUnavailable)?;
    match keys.read().map_err(Problem::KeyringUnavailable)? {
        Some(text) if Key::from_hex(&text).as_ref() == Some(&key) => Ok(key),
        _ => Err(Problem::KeyringUnavailable(
            "the keyring did not keep the new cache key".into(),
        )),
    }
}

/// Rewrite the cache at `db` from `from` to `to`: export into a staging
/// file, check the copy, then rename it over the original. Until the rename
/// the original is never written, so any failure leaves it as it was.
pub(crate) fn convert(db: &Path, from: &Protection, to: &Protection) -> Result<(), String> {
    let staging = staging_path(db);
    let result = export_and_check(db, &staging, from, to).and_then(|()| swap_in(db, &staging));
    if result.is_err() {
        let _ = remove_with_sidecars(&staging);
    }
    result
}

fn export_and_check(
    db: &Path,
    staging: &Path,
    from: &Protection,
    to: &Protection,
) -> Result<(), String> {
    create_private(staging).map_err(|e| format!("could not create {}: {e}", staging.display()))?;
    let src = connect(db, OpenFlags::SQLITE_OPEN_READ_WRITE, from)
        .map_err(|e| format!("could not open {}: {e}", db.display()))?;
    let version: i64 = src
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let counts = table_counts(&src).map_err(|e| e.to_string())?;
    let key = match to {
        Protection::Encrypted(key) => key.sql_literal(),
        Protection::Plain => String::new(),
    };
    let staging_text = staging
        .to_str()
        .ok_or_else(|| format!("{} is not UTF-8", staging.display()))?;
    src.execute(
        "ATTACH DATABASE ?1 AS rewrite KEY ?2",
        rusqlite::params![staging_text, key],
    )
    .map_err(|e| format!("could not attach {}: {e}", staging.display()))?;
    src.query_row("SELECT sqlcipher_export('rewrite')", [], |_| Ok(()))
        .map_err(|e| format!("export failed: {e}"))?;
    // sqlcipher_export copies tables, not the header: the schema version
    // decides which upgrades run, so it goes across by hand.
    src.execute_batch(&format!(
        "PRAGMA rewrite.user_version = {version}; DETACH DATABASE rewrite;"
    ))
    .map_err(|e| e.to_string())?;
    // Closing folds the write-ahead log back into the original, which leaves
    // nothing beside it that the copy has not got.
    src.close().map_err(|(_, e)| e.to_string())?;

    let copy = connect(staging, OpenFlags::SQLITE_OPEN_READ_ONLY, to)
        .map_err(|e| format!("the copy does not open: {e}"))?;
    let check: String = copy
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if check != "ok" {
        return Err(format!("the copy fails its check: {check}"));
    }
    let copied: i64 = copy
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if copied != version {
        return Err(format!("the copy is version {copied}, not {version}"));
    }
    if table_counts(&copy).map_err(|e| e.to_string())? != counts {
        return Err("the copy's rows differ from the original's".into());
    }
    drop(copy);
    let want = match to {
        Protection::Encrypted(_) => FileState::Encrypted,
        Protection::Plain => FileState::Plain,
    };
    if file_state(staging).map_err(|e| e.to_string())? != want {
        return Err(format!("the copy is not {want:?}"));
    }
    Ok(())
}

/// Every table that holds rows, with how many.
fn table_counts(conn: &Connection) -> rusqlite::Result<Vec<(String, i64)>> {
    let names: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND rootpage > 0 ORDER BY name",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    names
        .into_iter()
        .map(|name| {
            let n = conn.query_row(
                &format!("SELECT count(*) FROM \"{}\"", name.replace('"', "\"\"")),
                [],
                |r| r.get(0),
            )?;
            Ok((name, n))
        })
        .collect()
}

/// Put the checked copy in the original's place. The original's sidecars
/// go first: a write-ahead log left beside the new file would be replayed
/// into it, and theirs hold the old, unencrypted pages.
fn swap_in(db: &Path, staging: &Path) -> Result<(), String> {
    if std::fs::metadata(sidecar(db, "-wal")).is_ok_and(|m| m.len() > 0) {
        return Err(format!("{} is still in use", db.display()));
    }
    for suffix in SIDECARS {
        remove_file(&sidecar(db, suffix)).map_err(|e| e.to_string())?;
    }
    std::fs::File::open(staging)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::rename(staging, db).map_err(|e| format!("could not replace {}: {e}", db.display()))?;
    if let Some(dir) = db.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// Delete the cache and everything beside it, for a fresh start.
pub(crate) fn remove_cache(db: &Path) -> std::io::Result<()> {
    remove_with_sidecars(db)?;
    remove_with_sidecars(&staging_path(db))
}

fn remove_with_sidecars(path: &Path) -> std::io::Result<()> {
    remove_file(path)?;
    for suffix in SIDECARS {
        remove_file(&sidecar(path, suffix))?;
    }
    Ok(())
}

fn remove_file(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// A new, empty file only its owner can read, for SQLite to fill.
fn create_private(path: &Path) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path).map(drop)
}
