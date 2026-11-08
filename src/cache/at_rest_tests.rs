//! The cache at rest: it is encrypted on disk, an unencrypted one is
//! encrypted in place without losing a row, and a key that is missing,
//! wrong or out of reach never opens it unencrypted instead.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use super::at_rest::{
    connect, convert, file_state, prepare, remove_cache, FileState, Key, KeyStore, Problem,
    Protection,
};
use super::test_util::{add_att, add_msg};
use super::Cache;

/// A directory of its own for one test, gone when it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "hylki-at-rest-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn db(&self) -> PathBuf {
        self.0.join("cache.db")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A keyring in memory: what it holds, and whether it answers.
#[derive(Default)]
struct FakeKeyring {
    key: RefCell<Option<String>>,
    locked: bool,
    /// Takes the key and then forgets it, as a broken provider might.
    forgetful: bool,
}

impl FakeKeyring {
    fn holding(key: &Key) -> FakeKeyring {
        FakeKeyring {
            key: RefCell::new(Some(key.hex().to_string())),
            ..Default::default()
        }
    }

    fn locked() -> FakeKeyring {
        FakeKeyring {
            locked: true,
            ..Default::default()
        }
    }
}

impl KeyStore for FakeKeyring {
    fn read(&self) -> Result<Option<String>, String> {
        if self.locked {
            return Err("locked".into());
        }
        Ok(self.key.borrow().clone())
    }

    fn store(&self, key: &Key) -> Result<(), String> {
        if self.locked {
            return Err("locked".into());
        }
        if !self.forgetful {
            *self.key.borrow_mut() = Some(key.hex().to_string());
        }
        Ok(())
    }
}

fn encrypted(key: &Key) -> Protection {
    Protection::Encrypted(key.clone())
}

/// A cache as an earlier, unencrypted Hylki left it: the whole schema, a
/// message with its body (and so its search index entry) and an attachment.
fn plaintext_fixture(db: &Path) {
    let cache = Cache::open_at(db, Protection::Plain).unwrap();
    add_msg(&cache, "INBOX", 7, "Ann", "Quarterly numbers", 100);
    cache.save_body(1, "INBOX", 7, "<p>The budget is attached.</p>");
    add_att(&cache, "INBOX", 7, 0, "budget.xlsx", b"PK\x03\x04 sheet");
}

fn head(db: &Path) -> Vec<u8> {
    std::fs::read(db).unwrap().into_iter().take(16).collect()
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_new_cache_is_encrypted_on_disk() {
    let dir = Scratch::new("new");
    let keyring = FakeKeyring::default();
    let protection = prepare(&dir.db(), true, &keyring).unwrap();
    let key = Key::from_hex(keyring.key.borrow().as_deref().unwrap()).unwrap();
    assert_eq!(protection, encrypted(&key));

    let cache = Cache::open_at(&dir.db(), protection).unwrap();
    add_msg(&cache, "INBOX", 1, "Ann", "A secret subject", 100);
    cache.save_body(1, "INBOX", 1, "<p>a secret body</p>");
    drop(cache);

    assert_ne!(head(&dir.db()), b"SQLite format 3\0");
    assert_eq!(file_state(&dir.db()).unwrap(), FileState::Encrypted);
    let raw = std::fs::read(dir.db()).unwrap();
    assert!(!raw.windows(6).any(|w| w == b"secret"), "plaintext on disk");
    assert_eq!(mode(&dir.db()), 0o600);
}

#[test]
fn every_connection_reports_the_expected_cipher() {
    let dir = Scratch::new("cipher");
    let key = Key::generate().unwrap();
    drop(Cache::open_at(&dir.db(), encrypted(&key)).unwrap());
    let conn = connect(
        &dir.db(),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
        &encrypted(&key),
    )
    .unwrap();
    let pragma = |name: &str| -> String {
        conn.query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(pragma("cipher_memory_security"), "1");
    assert_eq!(pragma("cipher_page_size"), "4096");
    assert_eq!(pragma("cipher_hmac_algorithm"), "HMAC_SHA512");
    assert!(pragma("cipher_version").starts_with("4."));
}

/// A file keyed with other cipher settings is refused, not read with them.
#[test]
fn other_cipher_settings_are_refused() {
    let dir = Scratch::new("settings");
    let key = Key::generate().unwrap();
    {
        let conn = Connection::open(dir.db()).unwrap();
        conn.pragma_update(None, "key", format!("x'{}'", key.hex()))
            .unwrap();
        conn.execute_batch("PRAGMA cipher_page_size = 8192; CREATE TABLE t (x);")
            .unwrap();
    }
    let error = connect(
        &dir.db(),
        OpenFlags::SQLITE_OPEN_READ_WRITE,
        &encrypted(&key),
    )
    .unwrap_err();
    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::NotADatabase)
    );
    // Opened with those settings on purpose, the check still says no.
    let conn = Connection::open(dir.db()).unwrap();
    conn.execute_batch("PRAGMA cipher_memory_security = ON;")
        .unwrap();
    conn.pragma_update(None, "key", format!("x'{}'", key.hex()))
        .unwrap();
    conn.execute_batch("PRAGMA cipher_page_size = 8192;")
        .unwrap();
    conn.query_row("SELECT count(*) FROM t", [], |_| Ok(()))
        .unwrap();
    let detail = super::at_rest::check_cipher(&conn).unwrap_err().to_string();
    assert!(detail.contains("cipher_page_size"), "{detail}");
}

#[test]
fn a_wrong_key_fails_closed() {
    let dir = Scratch::new("wrong");
    let right = Key::generate().unwrap();
    plaintext_fixture(&dir.db());
    prepare(&dir.db(), true, &FakeKeyring::holding(&right)).unwrap();
    let before = std::fs::read(dir.db()).unwrap();

    let wrong = Key::generate().unwrap();
    let error = connect(
        &dir.db(),
        OpenFlags::SQLITE_OPEN_READ_WRITE,
        &encrypted(&wrong),
    )
    .unwrap_err();
    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::NotADatabase)
    );
    assert!(matches!(
        prepare(&dir.db(), true, &FakeKeyring::holding(&wrong)),
        Err(Problem::WrongKey(_))
    ));
    // Not even with encryption switched off: that needs the right key too.
    assert!(matches!(
        prepare(&dir.db(), false, &FakeKeyring::holding(&wrong)),
        Err(Problem::WrongKey(_))
    ));
    assert!(Cache::open_at(&dir.db(), Protection::Plain).is_err());
    assert_eq!(
        std::fs::read(dir.db()).unwrap(),
        before,
        "the cache was written"
    );
}

#[test]
fn a_missing_key_fails_closed() {
    let dir = Scratch::new("missing");
    let key = Key::generate().unwrap();
    drop(Cache::open_at(&dir.db(), encrypted(&key)).unwrap());
    let keyring = FakeKeyring::default();
    assert_eq!(prepare(&dir.db(), true, &keyring), Err(Problem::KeyMissing));
    assert_eq!(
        prepare(&dir.db(), false, &keyring),
        Err(Problem::KeyMissing)
    );
    // No new key was made for a cache the old one locks.
    assert!(keyring.key.borrow().is_none());
    let bad = FakeKeyring {
        key: RefCell::new(Some("not a key".into())),
        ..Default::default()
    };
    assert!(matches!(
        prepare(&dir.db(), true, &bad),
        Err(Problem::WrongKey(_))
    ));
}

#[test]
fn a_locked_keyring_fails_closed() {
    let dir = Scratch::new("locked");
    // Nothing yet: no unencrypted cache is started instead.
    assert!(matches!(
        prepare(&dir.db(), true, &FakeKeyring::locked()),
        Err(Problem::KeyringUnavailable(_))
    ));
    assert!(!dir.db().exists());
    // An unencrypted cache waiting to be encrypted stays as it was.
    plaintext_fixture(&dir.db());
    let before = std::fs::read(dir.db()).unwrap();
    assert!(matches!(
        prepare(&dir.db(), true, &FakeKeyring::locked()),
        Err(Problem::KeyringUnavailable(_))
    ));
    assert_eq!(std::fs::read(dir.db()).unwrap(), before);
    // An encrypted one cannot be opened.
    let key = Key::generate().unwrap();
    prepare(&dir.db(), true, &FakeKeyring::holding(&key)).unwrap();
    assert!(matches!(
        prepare(&dir.db(), true, &FakeKeyring::locked()),
        Err(Problem::KeyringUnavailable(_))
    ));
}

#[test]
fn a_key_the_keyring_did_not_keep_is_not_used() {
    let dir = Scratch::new("forgetful");
    plaintext_fixture(&dir.db());
    let keyring = FakeKeyring {
        forgetful: true,
        ..Default::default()
    };
    assert!(matches!(
        prepare(&dir.db(), true, &keyring),
        Err(Problem::KeyringUnavailable(_))
    ));
    assert_eq!(file_state(&dir.db()).unwrap(), FileState::Plain);
}

#[test]
fn a_plaintext_cache_is_encrypted_in_place() {
    let dir = Scratch::new("migrate");
    plaintext_fixture(&dir.db());
    assert_eq!(head(&dir.db()), b"SQLite format 3\0");
    let version: i64 = Connection::open(dir.db())
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();

    let keyring = FakeKeyring::default();
    let protection = prepare(&dir.db(), true, &keyring).unwrap();
    assert_eq!(file_state(&dir.db()).unwrap(), FileState::Encrypted);
    assert_eq!(mode(&dir.db()), 0o600);
    for leftover in [
        "cache.db-wal",
        "cache.db-shm",
        "cache.db-journal",
        "cache.db.rewrite",
    ] {
        assert!(!dir.0.join(leftover).exists(), "{leftover} left behind");
    }

    let cache = Cache::open_at(&dir.db(), protection).unwrap();
    let after: i64 = cache
        .conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, version);
    assert_eq!(cache.count_messages(1, "INBOX"), 1);
    assert_eq!(
        cache.load_body(1, "INBOX", 7).as_deref(),
        Some("<p>The budget is attached.</p>")
    );
    let data: Vec<u8> = cache
        .conn
        .query_row("SELECT data FROM attachments WHERE uid = 7", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(data, b"PK\x03\x04 sheet");
    // The search index came across with its shadow tables.
    let hits: i64 = cache
        .conn
        .query_row(
            "SELECT count(*) FROM body_text WHERE body_text MATCH 'budget'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hits, 1);
    drop(cache);
    // A second start finds it encrypted and leaves it be.
    let again = prepare(&dir.db(), true, &keyring).unwrap();
    assert!(matches!(again, Protection::Encrypted(_)));
}

/// A crash can leave rows only in the write-ahead log; they come across.
#[test]
fn rows_still_in_the_log_are_encrypted_too() {
    let source = Scratch::new("wal-source");
    let dir = Scratch::new("wal");
    let cache = Cache::open_at(&source.db(), Protection::Plain).unwrap();
    cache
        .conn
        .execute_batch("PRAGMA wal_autocheckpoint = 0;")
        .unwrap();
    add_msg(&cache, "INBOX", 9, "Bo", "In the log", 200);
    // Copied while the connection is open: what a crash leaves behind.
    for name in ["cache.db", "cache.db-wal", "cache.db-shm"] {
        std::fs::copy(source.0.join(name), dir.0.join(name)).unwrap();
    }
    drop(cache);
    assert!(std::fs::metadata(dir.0.join("cache.db-wal")).unwrap().len() > 0);

    let protection = prepare(&dir.db(), true, &FakeKeyring::default()).unwrap();
    assert!(!dir.0.join("cache.db-wal").exists());
    let cache = Cache::open_at(&dir.db(), protection).unwrap();
    assert_eq!(cache.count_messages(1, "INBOX"), 1);
}

#[test]
fn a_failed_migration_leaves_the_original_untouched() {
    let dir = Scratch::new("fail");
    plaintext_fixture(&dir.db());
    let before = std::fs::read(dir.db()).unwrap();
    // A directory where the copy would go: the export cannot start.
    std::fs::create_dir(dir.0.join("cache.db.rewrite")).unwrap();
    let key = Key::generate().unwrap();
    let error = convert(&dir.db(), &Protection::Plain, &encrypted(&key)).unwrap_err();
    assert!(error.contains("cache.db.rewrite"), "{error}");
    assert_eq!(std::fs::read(dir.db()).unwrap(), before);
    assert_eq!(file_state(&dir.db()).unwrap(), FileState::Plain);
    std::fs::remove_dir(dir.0.join("cache.db.rewrite")).unwrap();

    // A source that does not open with the key it is said to have.
    let wrong = Key::generate().unwrap();
    assert!(convert(&dir.db(), &encrypted(&wrong), &Protection::Plain).is_err());
    assert_eq!(std::fs::read(dir.db()).unwrap(), before);
    assert!(!dir.0.join("cache.db.rewrite").exists());
}

#[test]
fn switching_encryption_off_decrypts_in_place() {
    let dir = Scratch::new("off");
    plaintext_fixture(&dir.db());
    let key = Key::generate().unwrap();
    let keyring = FakeKeyring::holding(&key);
    prepare(&dir.db(), true, &keyring).unwrap();
    assert_eq!(prepare(&dir.db(), false, &keyring), Ok(Protection::Plain));
    assert_eq!(head(&dir.db()), b"SQLite format 3\0");
    let cache = Cache::open_at(&dir.db(), Protection::Plain).unwrap();
    assert_eq!(cache.count_messages(1, "INBOX"), 1);
}

#[test]
fn a_reset_removes_every_file() {
    let dir = Scratch::new("reset");
    let key = Key::generate().unwrap();
    let cache = Cache::open_at(&dir.db(), encrypted(&key)).unwrap();
    add_msg(&cache, "INBOX", 1, "Ann", "S", 100);
    std::fs::write(dir.0.join("cache.db.rewrite"), b"half").unwrap();
    drop(cache);
    remove_cache(&dir.db()).unwrap();
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
    // And the next start begins a new one with the key it has.
    let protection = prepare(&dir.db(), true, &FakeKeyring::holding(&key)).unwrap();
    assert_eq!(protection, encrypted(&key));
}

#[test]
fn a_key_is_64_hex_digits_and_never_printed() {
    let key = Key::generate().unwrap();
    assert_eq!(key.hex().len(), 64);
    assert_ne!(Key::generate().unwrap(), key);
    assert_eq!(Key::from_hex(&key.hex().to_uppercase()), Some(key.clone()));
    assert_eq!(Key::from_hex("x'00'"), None);
    assert!(!format!("{key:?}").contains(key.hex()));
}

/// The readers the assistant server and the workers' lanes open are keyed
/// too, and still cannot write.
#[test]
fn a_reader_of_an_encrypted_cache_is_keyed_and_read_only() {
    let dir = Scratch::new("reader");
    let key = Key::generate().unwrap();
    let cache = Cache::open_at(&dir.db(), encrypted(&key)).unwrap();
    cache.save_body(1, "INBOX", 42, "first");
    let reader = cache.read_connection().unwrap();
    assert_eq!(reader.load_body(1, "INBOX", 42).as_deref(), Some("first"));
    cache.save_body(1, "INBOX", 42, "updated");
    assert_eq!(reader.load_body(1, "INBOX", 42).as_deref(), Some("updated"));
    let error = reader.conn.execute("DELETE FROM bodies", []).unwrap_err();
    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::ReadOnly)
    );
}
