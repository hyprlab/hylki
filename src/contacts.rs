//! GNOME Contacts integration via Evolution Data Server (EDS).
//!
//! Reading is done straight from the EDS address-book SQLite caches (covers
//! both the local address book and CardDAV books, which EDS mirrors locally).
//! Writing goes through the EDS D-Bus API so changes are tracked by EDS and
//! synced back to CardDAV servers.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};
use crate::i18n::i18n;

/// A name + email pair from the address book.
#[derive(Debug, Clone)]
pub struct Contact {
    pub name: String,
    pub email: String,
}

/// A recipient autocomplete suggestion (from Contacts or mail history).
#[derive(Debug, Clone)]
pub struct Suggestion {
    pub name: String,
    pub email: String,
    /// True when this came from the address book.
    pub from_contacts: bool,
    /// How often this address appears in mail history (0 for contacts-only).
    pub score: u32,
    /// One of the user's own account addresses: offered, since people do
    /// mail themselves, but ranked after everyone else so it does not sit
    /// on top of every list (it is on nearly every message received).
    pub own: bool,
}

impl Suggestion {
    /// "Name <email>" for inserting into a recipient field (email alone if no name).
    pub fn display(&self) -> String {
        if self.name.trim().is_empty() || self.name == self.email {
            self.email.clone()
        } else {
            format!("{} <{}>", self.name, self.email)
        }
    }

    /// Whether the suggestion matches a typed fragment (name or email).
    pub fn matches(&self, q: &str) -> bool {
        let q = q.to_lowercase();
        self.email.to_lowercase().contains(&q) || self.name.to_lowercase().contains(&q)
    }
}

/// Combined, de-duplicated recipient suggestions from Contacts and mail history
/// (addresses sent to / received from). History frequency becomes each
/// suggestion's `score` so frequently used addresses — contact or not — can
/// rank ahead. The user's `own` (name, email) account addresses are in the
/// list too, flagged and with no score, so they come last.
pub fn suggestions(own: &[(String, String)]) -> Vec<Suggestion> {
    let own_keys: HashSet<String> = own.iter().map(|(_, e)| e.to_lowercase()).collect();
    let mut map: std::collections::HashMap<String, Suggestion> = std::collections::HashMap::new();

    for c in read_contacts() {
        let key = c.email.to_lowercase();
        if own_keys.contains(&key) {
            continue;
        }
        map.entry(key).or_insert(Suggestion {
            name: c.name,
            email: c.email,
            from_contacts: true,
            score: 0,
            own: false,
        });
    }

    if let Ok(cache) = crate::cache::Cache::open() {
        for (name, email, count) in cache.address_history() {
            let key = email.to_lowercase();
            if own_keys.contains(&key) {
                continue;
            }
            map.entry(key)
                .and_modify(|s| s.score = s.score.max(count))
                .or_insert(Suggestion { name, email, from_contacts: false, score: count, own: false });
        }
    }

    for (name, email) in own {
        let key = email.to_lowercase();
        if key.is_empty() || !key.contains('@') {
            continue;
        }
        map.entry(key).or_insert(Suggestion {
            name: if name.trim().is_empty() { email.clone() } else { name.clone() },
            email: email.clone(),
            from_contacts: false,
            score: 0,
            own: true,
        });
    }

    let mut out: Vec<Suggestion> = map.into_values().collect();
    // Default order: most-used first, then name (the field filter re-ranks live).
    out.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

/// A writable address book the user can add contacts to.
#[derive(Debug, Clone)]
pub struct Book {
    /// EDS source UID (what `OpenAddressBook` expects).
    pub uid: String,
    pub name: String,
}

// Inside a Flatpak sandbox `dirs::{data,cache,config}_dir()` are redirected into
// ~/.var/app/<app-id>/, but EDS's books live in the *host's* real XDG dirs (made
// visible by the `--filesystem=xdg-{data,cache,config}/evolution:ro` grants). So
// under Flatpak resolve them from the real home instead of the redirected XDG env.
fn host_xdg(sandbox: fn() -> Option<PathBuf>, subdir: &str) -> Option<PathBuf> {
    if crate::platform::is_flatpak() {
        dirs::home_dir().map(|h| h.join(subdir))
    } else {
        sandbox()
    }
}

fn data_dir() -> Option<PathBuf> {
    host_xdg(dirs::data_dir, ".local/share").map(|d| d.join("evolution/addressbook"))
}

fn cache_dir() -> Option<PathBuf> {
    host_xdg(dirs::cache_dir, ".cache").map(|d| d.join("evolution/addressbook"))
}

fn config_dir() -> Option<PathBuf> {
    host_xdg(dirs::config_dir, ".config").map(|d| d.join("evolution"))
}

/// Read every address email from the local + cached (CardDAV) EDS books.
pub fn read_contacts() -> Vec<Contact> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // Local books: table `folder_id` + `folder_id_email_list`. Read the vCard
    // (not `full_name`, which EDS stores case-folded for search) so the display
    // name keeps its original capitalisation.
    let active = registry_books();
    if let Some(dir) = data_dir() {
        for db in find_dbs(&dir, "contacts.db") {
            if !db_book_active(&db, &active) {
                continue;
            }
            read_book_db(
                &db,
                "SELECT f.vcard, e.value FROM folder_id f \
                 JOIN folder_id_email_list e ON f.uid = e.uid",
                &mut out,
                &mut seen,
            );
        }
    }
    // Cached books (CardDAV etc.): table `ECacheObjects` + `attrlist_email_list`.
    if let Some(dir) = cache_dir() {
        for db in find_dbs(&dir, "cache.db") {
            if !db_book_active(&db, &active) {
                continue;
            }
            read_book_db(
                &db,
                "SELECT o.ECacheOBJ, e.value FROM ECacheObjects o \
                 JOIN attrlist_email_list e ON o.ECacheUID = e.uid",
                &mut out,
                &mut seen,
            );
        }
    }
    out
}

/// `book_uid_active` keyed off a book database's directory name.
fn db_book_active(db: &std::path::Path, active: &Option<HashMap<String, String>>) -> bool {
    let Some(folder) = db.parent().and_then(|p| p.file_name()) else {
        return true;
    };
    let folder = folder.to_string_lossy();
    let uid = if folder == "system" { "system-address-book" } else { folder.as_ref() };
    book_uid_active(uid, active)
}

/// Photo bytes and the EDS database state they came from. The inexpensive
/// fingerprint check lets a long-running Hylki notice CardDAV synchronizations
/// without subscribing to backend-specific EDS signals.
#[derive(Clone)]
struct PhotoLocation {
    db: PathBuf,
    uid: String,
    cached_book: bool,
}

/// (path, size, mtime-nanos) per EDS database file — the cheap change signal.
type DbFingerprint = Vec<(PathBuf, u64, u128)>;

struct ContactPhotoIndex {
    photos: HashMap<String, Vec<PhotoLocation>>,
    fingerprint: DbFingerprint,
    generation: u64,
    ready: bool,
}

static CONTACT_PHOTOS: OnceLock<Arc<Mutex<ContactPhotoIndex>>> = OnceLock::new();
static PHOTO_LOAD_STARTED: OnceLock<()> = OnceLock::new();
static PHOTO_WATCH_STARTED: OnceLock<()> = OnceLock::new();
type PhotoChangeCallback = Box<dyn Fn() + Send + Sync + 'static>;
static PHOTO_CHANGE_CALLBACKS: OnceLock<Mutex<Vec<PhotoChangeCallback>>> = OnceLock::new();
const PHOTO_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

fn new_photo_index() -> Arc<Mutex<ContactPhotoIndex>> {
    Arc::new(Mutex::new(ContactPhotoIndex {
        photos: HashMap::new(),
        fingerprint: Vec::new(),
        generation: 1,
        ready: false,
    }))
}

fn load_stable_photo_index() -> (HashMap<String, Vec<PhotoLocation>>, DbFingerprint) {
    let mut last = (HashMap::new(), Vec::new());
    for _ in 0..3 {
        let before = photo_db_fingerprint();
        let photos = load_contact_photo_index();
        let after = photo_db_fingerprint();
        last = (photos, after.clone());
        if before == after {
            break;
        }
    }
    last
}

fn notify_photo_change() {
    if let Some(callbacks) = PHOTO_CHANGE_CALLBACKS.get() {
        for callback in callbacks.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            callback();
        }
    }
}

fn start_photo_load() {
    PHOTO_LOAD_STARTED.get_or_init(|| {
        let index = CONTACT_PHOTOS.get_or_init(new_photo_index).clone();
        let _ = std::thread::Builder::new()
            .name("eds-photo-load".into())
            .spawn(move || {
                let (photos, fingerprint) = load_stable_photo_index();
                let mut current = index.lock().unwrap_or_else(|p| p.into_inner());
                current.photos = photos;
                current.fingerprint = fingerprint;
                current.generation = current.generation.wrapping_add(1).max(1);
                current.ready = true;
                drop(current);
                notify_photo_change();
            });
    });
}

fn start_photo_watcher() {
    PHOTO_WATCH_STARTED.get_or_init(|| {
        let index = CONTACT_PHOTOS.get_or_init(new_photo_index).clone();
        let _ = std::thread::Builder::new()
            .name("eds-photo-watch".into())
            .spawn(move || loop {
                std::thread::sleep(PHOTO_REFRESH_INTERVAL);
                let fingerprint = photo_db_fingerprint();
                let changed = {
                    let current = index.lock().unwrap_or_else(|p| p.into_inner());
                    fingerprint != current.fingerprint
                };
                if !changed {
                    continue;
                }
                // Scan outside the lock. UI lookups continue using the previous
                // immutable snapshot until the replacement is ready.
                let (photos, stable_fingerprint) = load_stable_photo_index();
                let mut current = index.lock().unwrap_or_else(|p| p.into_inner());
                current.photos = photos;
                current.fingerprint = stable_fingerprint;
                current.generation = current.generation.wrapping_add(1).max(1);
                drop(current);
                notify_photo_change();
            });
    });
}

pub fn watch_photo_changes<F: Fn() + Send + Sync + 'static>(callback: F) {
    PHOTO_CHANGE_CALLBACKS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(Box::new(callback));
    start_photo_load();
    start_photo_watcher();
}

/// Current index generation. Avatar textures and negative results use this to
/// invalidate themselves after an EDS/CardDAV database change.
pub fn photo_generation() -> u64 {
    start_photo_load();
    start_photo_watcher();
    CONTACT_PHOTOS
        .get_or_init(new_photo_index)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .generation
}

pub fn photos_ready() -> bool {
    start_photo_load();
    CONTACT_PHOTOS
        .get_or_init(new_photo_index)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .ready
}

/// Find contact-photo candidates by email in the local EDS caches. Multiple
/// books can contain the same address; the avatar loader tries each candidate.
/// CardDAV backends such as Nextcloud keep PHOTO in the cached vCard, so this
/// does not contact the address-book server or disclose the sender address to a third
/// party.
pub fn contact_photos(email: &str) -> Vec<Arc<[u8]>> {
    let key = email.trim().to_lowercase();
    if key.is_empty() {
        return Vec::new();
    }
    start_photo_load();
    start_photo_watcher();
    let locations = CONTACT_PHOTOS
        .get_or_init(new_photo_index)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .photos
        .get(&key)
        .cloned()
        .unwrap_or_default();
    locations
        .iter()
        .filter_map(read_photo_at)
        .map(Arc::from)
        .collect()
}

fn photo_db_fingerprint() -> DbFingerprint {
    let mut files = Vec::new();
    for db in photo_db_paths() {
        files.push(db.clone());
        for suffix in ["-journal", "-wal"] {
            files.push(PathBuf::from(format!("{}{suffix}", db.to_string_lossy())));
        }
    }
    files.sort();
    files
        .into_iter()
        .filter_map(|path| {
            let metadata = std::fs::metadata(&path).ok()?;
            let modified = metadata
                .modified()
                .unwrap_or(SystemTime::UNIX_EPOCH)
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            Some((path, metadata.len(), modified))
        })
        .collect()
}

fn photo_db_paths() -> Vec<PathBuf> {
    let mut dbs = Vec::new();
    if let Some(dir) = data_dir() {
        dbs.extend(find_dbs(&dir, "contacts.db"));
    }
    if let Some(dir) = cache_dir() {
        dbs.extend(find_dbs(&dir, "cache.db"));
    }
    dbs.sort();
    dbs
}

fn load_contact_photo_index() -> HashMap<String, Vec<PhotoLocation>> {
    let mut photos = HashMap::new();
    if let Some(dir) = data_dir() {
        for db in find_dbs(&dir, "contacts.db") {
            read_photo_locations(
                &db,
                "SELECT e.value, f.uid FROM folder_id f \
                 JOIN folder_id_email_list e ON f.uid = e.uid \
                 WHERE f.vcard LIKE '%PHOTO%'",
                false,
                &mut photos,
            );
        }
    }
    if let Some(dir) = cache_dir() {
        for db in find_dbs(&dir, "cache.db") {
            read_photo_locations(
                &db,
                "SELECT e.value, o.ECacheUID FROM ECacheObjects o \
                 JOIN attrlist_email_list e ON o.ECacheUID = e.uid \
                 WHERE o.ECacheOBJ LIKE '%PHOTO%'",
                true,
                &mut photos,
            );
        }
    }
    tracing::debug!(count = photos.len(), "indexed EDS contact photos");
    photos
}

fn read_photo_locations(
    path: &std::path::Path,
    query: &str,
    cached_book: bool,
    photos: &mut HashMap<String, Vec<PhotoLocation>>,
) {
    let conn = match open_book_db(path) {
        Ok(conn) => conn,
        Err(e) => {
            tracing::warn!("contacts: cannot open {} for photos: {e}", path.display());
            return;
        }
    };
    let mut stmt = match conn.prepare(query) {
        Ok(stmt) => stmt,
        Err(e) => {
            tracing::warn!("contacts: photo query failed on {}: {e}", path.display());
            return;
        }
    };
    let rows = match stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("contacts: photo rows failed on {}: {e}", path.display());
            return;
        }
    };
    for (email, uid) in rows.flatten() {
        let key = email.trim().to_lowercase();
        if !key.is_empty() {
            let location = PhotoLocation {
                db: path.to_path_buf(),
                uid,
                cached_book,
            };
            photos.entry(key).or_default().push(location);
        }
    }
}

fn read_photo_at(location: &PhotoLocation) -> Option<Vec<u8>> {
    let conn = open_book_db(&location.db).ok()?;
    let query = if location.cached_book {
        "SELECT ECacheOBJ FROM ECacheObjects WHERE ECacheUID = ?1"
    } else {
        "SELECT vcard FROM folder_id WHERE uid = ?1"
    };
    let vcard: Option<String> = conn
        .query_row(query, [&location.uid], |row| row.get(0))
        .ok()?;
    vcard.as_deref().and_then(vcard_photo)
}

fn find_dbs(root: &std::path::Path, file: &str) -> Vec<PathBuf> {
    let mut dbs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let db = entry.path().join(file);
            if db.is_file() {
                dbs.push(db);
            }
        }
    }
    dbs.sort();
    dbs
}

/// Extract the display name (`FN`) from a vCard, preserving its capitalisation.
/// Handles line folding (a CRLF/LF followed by a space or tab continues the
/// previous line) and the standard text escapes.
fn unfold_vcard(vcard: &str) -> String {
    vcard
        .replace("\r\n ", "")
        .replace("\r\n\t", "")
        .replace("\n ", "")
        .replace("\n\t", "")
}

/// Split a (unfolded) vCard content line into property (name + params) and
/// value at the first colon *outside* double quotes. A naive colon split
/// breaks on quoted parameter values — iCloud's photo lines carry
/// `X-EVOLUTION-WEBDAV-IMG-URL="https://…"` before the real value, and the
/// `https:` inside the quotes swallowed everything after it.
fn split_vcard_line(line: &str) -> Option<(&str, &str)> {
    let mut in_quotes = false;
    for (i, ch) in line.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => return Some((&line[..i], &line[i + 1..])),
            _ => {}
        }
    }
    None
}

fn vcard_display_name(vcard: &str) -> Option<String> {
    for line in unfold_vcard(vcard).lines() {
        let Some((prop, value)) = line.split_once(':') else {
            continue;
        };
        // The property name is everything before the first ';' (which begins any
        // parameters), e.g. `FN` or `FN;PID=1.1`.
        let name = prop.split(';').next().unwrap_or("").trim();
        if name.eq_ignore_ascii_case("FN") {
            let value = unescape_vcard_text(value);
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Decode an embedded vCard PHOTO. Handles both Nextcloud's escaped data URI
/// form and the conventional `PHOTO;ENCODING=b` form. Remote PHOTO URLs are
/// deliberately not fetched, both for privacy and to avoid tracking.
fn vcard_photo(vcard: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;

    const MAX_PHOTO_BYTES: usize = 2_000_000;
    for line in unfold_vcard(vcard).lines() {
        let Some((property, raw_value)) = split_vcard_line(line) else {
            continue;
        };
        let name = property
            .split(';')
            .next()
            .unwrap_or("")
            .rsplit('.')
            .next()
            .unwrap_or("")
            .trim();
        if !name.eq_ignore_ascii_case("PHOTO") {
            continue;
        }

        let value = raw_value.replace("\\;", ";").replace("\\,", ",");
        let property_upper = property.to_ascii_uppercase();
        let payload = if value.to_ascii_lowercase().starts_with("data:image/") {
            let Some((metadata, payload)) = value.split_once(',') else {
                continue;
            };
            if !metadata.to_ascii_lowercase().ends_with(";base64") {
                continue;
            }
            payload
        } else if property_upper.contains("ENCODING=B")
            || property_upper.contains("ENCODING=BASE64")
        {
            value.as_str()
        } else if value.to_ascii_lowercase().starts_with("file://") {
            if let Some(bytes) = read_eds_photo_file(&value, MAX_PHOTO_BYTES) {
                return Some(bytes);
            }
            continue;
        } else {
            // Never fetch http(s) or other remote PHOTO URIs implicitly.
            continue;
        };

        let encoded: String = payload.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        // Base64 expands data by roughly 4/3. Reject oversized input before
        // allocating the decoded image buffer.
        if encoded.len() > (MAX_PHOTO_BYTES * 4 / 3 + 4) {
            continue;
        }
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) {
            if !bytes.is_empty() && bytes.len() <= MAX_PHOTO_BYTES {
                return Some(bytes);
            }
        }
    }
    None
}

/// Read an EDS-materialized `file://` photo, but only from an address-book data
/// or cache directory. A CardDAV vCard must not be able to make Hylki read an
/// arbitrary local file.
fn read_eds_photo_file(uri: &str, max_bytes: usize) -> Option<Vec<u8>> {
    use std::io::Read;

    let (path, host) = gtk::glib::filename_from_uri(uri).ok()?;
    if host.is_some() {
        return None;
    }
    let roots: Vec<PathBuf> = [data_dir(), cache_dir()].into_iter().flatten().collect();
    let path = confine_to_roots(&path, &roots)?;

    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (!bytes.is_empty() && bytes.len() <= max_bytes).then_some(bytes)
}

/// Canonicalize `path` and admit it only when it lands inside one of `roots`
/// (also canonicalized) — `..` segments and symlinks are resolved before the
/// containment check, so neither can escape a root. No resolvable roots means
/// nothing is admitted.
fn confine_to_roots(path: &std::path::Path, roots: &[PathBuf]) -> Option<PathBuf> {
    let path = path.canonicalize().ok()?;
    roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| path.starts_with(&root))
        .then_some(path)
}

/// Unescape a vCard TEXT value: `\n`/`\N` → newline, `\,` → `,`, `\;` → `;`,
/// `\\` → `\`.
fn unescape_vcard_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn open_book_db(path: &std::path::Path) -> rusqlite::Result<rusqlite::Connection> {
    use rusqlite::OpenFlags;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    // Under Flatpak the book DB lives on a read-only host mount; a WAL database
    // can't be opened read-only there (it needs to touch the -shm/-wal files), so
    // open it `immutable=1` to read the committed snapshot without any locking.
    if crate::platform::is_flatpak() {
        rusqlite::Connection::open_with_flags(
            format!("file:{}?immutable=1", path.to_string_lossy()),
            flags,
        )
    } else {
        rusqlite::Connection::open_with_flags(path, flags)
    }
}

fn read_book_db(path: &std::path::Path, query: &str, out: &mut Vec<Contact>, seen: &mut HashSet<String>) {
    let conn = match open_book_db(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("contacts: cannot open {}: {e}", path.display());
            return;
        }
    };
    let mut stmt = match conn.prepare(query) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("contacts: query failed on {}: {e}", path.display());
            return;
        }
    };
    let rows = stmt.query_map([], |row| {
        let vcard: Option<String> = row.get(0).ok();
        let email: String = row.get(1)?;
        let name = vcard.as_deref().and_then(vcard_display_name).unwrap_or_default();
        Ok((name, email))
    });
    if let Ok(rows) = rows {
        for (name, email) in rows.flatten() {
            let email = email.trim().to_string();
            if email.is_empty() || !email.contains('@') {
                continue;
            }
            let key = email.to_lowercase();
            if seen.insert(key) {
                let name = if name.trim().is_empty() { email.clone() } else { name };
                out.push(Contact { name, email });
            }
        }
    }
}

/// Address books the user can add contacts to (local + cached/CardDAV).
pub fn writable_books() -> Vec<Book> {
    let mut books = Vec::new();
    let active = registry_books();
    if let Some(dir) = data_dir() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if !entry.path().join("contacts.db").is_file() {
                continue;
            }
            let folder = entry.file_name().to_string_lossy().to_string();
            // The built-in local book's source UID is "system-address-book".
            let uid = if folder == "system" {
                "system-address-book".to_string()
            } else {
                folder
            };
            if !book_uid_active(&uid, &active) {
                continue;
            }
            let name = book_uid_name(&uid, &active, "On This Computer");
            books.push(Book { uid, name });
        }
    }
    if let Some(dir) = cache_dir() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if !entry.path().join("cache.db").is_file() {
                continue;
            }
            let uid = entry.file_name().to_string_lossy().to_string();
            if !book_uid_active(&uid, &active) {
                continue;
            }
            let name = book_uid_name(&uid, &active, "CardDAV Address Book");
            books.push(Book { uid, name });
        }
    }
    books
}

/// Best-effort display name from the EDS source file for a book UID.
fn source_display_name(uid: &str) -> Option<String> {
    let path = config_dir()?.join(format!("sources/{uid}.source"));
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("DisplayName="))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// Writing (EDS D-Bus)
// ---------------------------------------------------------------------------

/// Discover the versioned AddressBook factory bus name (e.g. `…AddressBook10`).
fn factory_dest() -> Option<String> {
    let dir = std::path::Path::new("/usr/share/dbus-1/services");
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.contains("AddressBook") && name.ends_with(".service") {
            if let Ok(text) = std::fs::read_to_string(entry.path()) {
                if let Some(n) = text.lines().find_map(|l| l.strip_prefix("Name=")) {
                    let n = n.trim();
                    if n.contains("AddressBook") {
                        return Some(n.to_string());
                    }
                }
            }
        }
    }
    None
}

const FACTORY_PATH: &str = "/org/gnome/evolution/dataserver/AddressBookFactory";
const FACTORY_IFACE: &str = "org.gnome.evolution.dataserver.AddressBookFactory";
const BOOK_IFACE: &str = "org.gnome.evolution.dataserver.AddressBook";

/// Open a book by source UID; returns (bus_name, object_path) for the book.
fn open_book(
    conn: &zbus::blocking::Connection,
    dest: &str,
    uid: &str,
) -> Result<(String, String), String> {
    let reply = conn
        .call_method(Some(dest), FACTORY_PATH, Some(FACTORY_IFACE), "OpenAddressBook", &(uid,))
        .map_err(|e| e.to_string())?;
    // Returns (object_path, bus_name).
    let (object_path, bus_name): (String, String) =
        reply.body().deserialize().map_err(|e| e.to_string())?;
    conn.call_method(Some(bus_name.as_str()), object_path.as_str(), Some(BOOK_IFACE), "Open", &())
        .map_err(|e| e.to_string())?;
    Ok((bus_name, object_path))
}

/// What happened when adding an email to Contacts.
#[derive(Debug)]
pub enum AddOutcome {
    /// A new contact was created.
    Created,
    /// The email was merged into an existing contact (carries its name).
    Merged(String),
    /// The email was already in the address book (carries the contact's name).
    AlreadyPresent(String),
}

/// Add `email` (with `name`) to Contacts, detecting duplicates and merging:
/// - if the email already exists anywhere, do nothing;
/// - else if a contact with the same name exists in the book, append the email;
/// - else create a new contact. Blocking — call from a background thread.
pub fn add_or_merge(book_uid: &str, name: &str, email: &str) -> Result<AddOutcome, String> {
    // 1. Already present (in any book)? Don't create a duplicate.
    let email_l = email.trim().to_lowercase();
    if let Some(existing) = read_contacts()
        .into_iter()
        .find(|c| c.email.to_lowercase() == email_l)
    {
        return Ok(AddOutcome::AlreadyPresent(existing.name));
    }

    let dest = factory_dest().ok_or("Evolution Data Server is not available")?;
    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let (bus, path) = open_book(&conn, &dest, book_uid)?;

    // 2. A contact with the same name in this book → merge the email into it.
    if !name.trim().is_empty() {
        let query = format!("(is \"full_name\" \"{}\")", sexpr_escape(name.trim()));
        let uids = book_get_uids(&conn, &bus, &path, &query)?;
        if let Some(uid) = uids.first() {
            let vcard = book_get_contact(&conn, &bus, &path, uid)?;
            let merged = add_email_to_vcard(&vcard, email.trim());
            book_call(&conn, &bus, &path, "ModifyContacts", &(vec![merged], 0u32))?;
            return Ok(AddOutcome::Merged(name.trim().to_string()));
        }
    }

    // 3. Create a new contact.
    book_call(
        &conn,
        &bus,
        &path,
        "CreateContacts",
        &(vec![vcard_for(name, email)], 0u32),
    )?;
    Ok(AddOutcome::Created)
}

fn book_call<B>(
    conn: &zbus::blocking::Connection,
    bus: &str,
    path: &str,
    method: &str,
    body: &B,
) -> Result<zbus::Message, String>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    conn.call_method(Some(bus), path, Some(BOOK_IFACE), method, body)
        .map_err(|e| e.to_string())
}

fn book_get_uids(
    conn: &zbus::blocking::Connection,
    bus: &str,
    path: &str,
    query: &str,
) -> Result<Vec<String>, String> {
    let reply = book_call(conn, bus, path, "GetContactListUids", &(query,))?;
    let (uids,): (Vec<String>,) = reply.body().deserialize().map_err(|e| e.to_string())?;
    Ok(uids)
}

fn book_get_contact(
    conn: &zbus::blocking::Connection,
    bus: &str,
    path: &str,
    uid: &str,
) -> Result<String, String> {
    let reply = book_call(conn, bus, path, "GetContact", &(uid,))?;
    let (vcard,): (String,) = reply.body().deserialize().map_err(|e| e.to_string())?;
    Ok(vcard)
}

/// Insert an EMAIL line into an existing vCard, just before END:VCARD.
fn add_email_to_vcard(vcard: &str, email: &str) -> String {
    let line = format!("EMAIL;TYPE=INTERNET:{email}\r\n");
    match vcard.rfind("END:VCARD") {
        Some(pos) => {
            let mut s = String::with_capacity(vcard.len() + line.len());
            s.push_str(&vcard[..pos]);
            if !s.ends_with('\n') {
                s.push_str("\r\n");
            }
            s.push_str(&line);
            s.push_str(&vcard[pos..]);
            s
        }
        None => vcard.to_string(),
    }
}

/// Escape a value for an EDS S-expression string literal.
fn sexpr_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn vcard_for(name: &str, email: &str) -> String {
    let name = if name.trim().is_empty() { email } else { name.trim() };
    // Minimal vCard 3.0; EDS assigns the UID.
    format!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:{name}\r\nN:;{name};;;\r\n\
         EMAIL;TYPE=INTERNET:{email}\r\nEND:VCARD\r\n"
    )
}

// ---------------------------------------------------------------------------
// Full contact details (the in-app Contacts view)
// ---------------------------------------------------------------------------

/// One labelled value on a contact (a "Home" email, a "Mobile" phone…). The
/// label may be empty when the vCard carries no usable TYPE.
#[derive(Debug, Clone)]
pub struct Labeled {
    pub label: String,
    pub value: String,
}

/// A fully parsed address-book entry, everything the in-app Contacts view
/// shows. Read straight from the EDS vCards, so it matches GNOME Contacts.
#[derive(Debug, Clone, Default)]
pub struct ContactDetails {
    pub name: String,
    pub nickname: String,
    pub org: String,
    pub title: String,
    pub birthday: String,
    pub note: String,
    pub emails: Vec<Labeled>,
    pub phones: Vec<Labeled>,
    pub addresses: Vec<Labeled>,
    pub urls: Vec<String>,
    pub photo: Option<Vec<u8>>,
    /// EDS source UID of the book this contact came from — where edits go.
    pub book_uid: String,
    /// That book's display name ("On This Computer", the CardDAV account…).
    pub book_name: String,
    /// The contact's own EDS UID (for deletion and deep links).
    pub eds_uid: String,
    /// The stored vCard, verbatim; edits patch this so unknown properties
    /// (PHOTO, UID, X-…) survive the round trip.
    pub raw_vcard: String,
}

impl ContactDetails {
    /// The address used for avatars and as the compose default.
    pub fn primary_email(&self) -> Option<&str> {
        self.emails.first().map(|e| e.value.as_str())
    }
}

/// Every *live, enabled* address-book source, straight from the EDS source
/// registry over D-Bus: uid → a friendly name for the account it belongs to
/// ("Google — a@gmail.com", "CardDAV — j@mac.com", "On This Computer").
///
/// The cache/data directories of removed accounts linger on disk (EDS never
/// deletes them), so without this a book removed in GOA or GNOME Contacts
/// keeps showing its contacts forever. The registry — not the sources
/// directory — is the authority: GOA-derived sources are registry-only and
/// never written to disk. `None` when the registry can't be reached (then
/// keep everything rather than hide it all).
fn registry_books() -> Option<HashMap<String, String>> {
    let dest = sources_dest()?;
    let conn = zbus::blocking::Connection::session().ok()?;
    let reply = conn
        .call_method(
            Some(dest.as_str()),
            "/org/gnome/evolution/dataserver/SourceManager",
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .ok()?;
    type Props = HashMap<String, zbus::zvariant::OwnedValue>;
    type Objects = HashMap<zbus::zvariant::OwnedObjectPath, HashMap<String, Props>>;
    let (objects,): (Objects,) = reply.body().deserialize().ok()?;
    let mut sources: HashMap<String, String> = HashMap::new();
    for ifaces in objects.values() {
        let Some(props) = ifaces.get("org.gnome.evolution.dataserver.Source") else {
            continue;
        };
        let uid =
            props.get("UID").and_then(|v| v.downcast_ref::<&str>().ok()).map(str::to_string);
        let data =
            props.get("Data").and_then(|v| v.downcast_ref::<&str>().ok()).map(str::to_string);
        if let (Some(uid), Some(data)) = (uid, data) {
            sources.insert(uid, data);
        }
    }
    let mut books = HashMap::new();
    for (uid, data) in &sources {
        // An address book, and not switched off (GOA sets the book source's
        // [Data Source] Enabled=false when Contacts is toggled off for the
        // account). Only that section's Enabled counts — other sections
        // (Refresh etc.) have Enabled keys of their own.
        if data.contains("[Address Book]") && !data_source_disabled(data) {
            books.insert(uid.clone(), book_display_name(data, &sources));
        }
    }
    Some(books)
}

/// A `Key=value` lookup inside one `[Section]` of a source keyfile.
fn keyfile_get(data: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for line in data.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line.eq_ignore_ascii_case(section);
            continue;
        }
        if in_section {
            if let Some(v) = line.strip_prefix(key).and_then(|r| r.strip_prefix('=')) {
                let v = v.trim();
                return (!v.is_empty()).then(|| v.to_string());
            }
        }
    }
    None
}

/// A book's friendly name: which account it belongs to and over what — walked
/// up the source's parent chain to the account collection, whose backend
/// (google / microsoft365 / webdav) and identity say it best.
fn book_display_name(book_data: &str, sources: &HashMap<String, String>) -> String {
    let mut backend = keyfile_get(book_data, "[Address Book]", "BackendName");
    let mut identity: Option<String> = None;
    let mut top_display = keyfile_get(book_data, "[Data Source]", "DisplayName");
    let mut current = book_data.to_string();
    for _ in 0..4 {
        let Some(parent) = keyfile_get(&current, "[Data Source]", "Parent") else { break };
        let Some(parent_data) = sources.get(&parent) else { break };
        if let Some(b) = keyfile_get(parent_data, "[Collection]", "BackendName") {
            backend = Some(b);
        }
        if identity.is_none() {
            identity = keyfile_get(parent_data, "[Collection]", "Identity");
        }
        if let Some(d) = keyfile_get(parent_data, "[Data Source]", "DisplayName") {
            top_display = Some(d);
        }
        current = parent_data.clone();
    }
    let account = identity.or(top_display);
    match (backend.as_deref(), account) {
        (Some("local"), _) | (None, None) => i18n("On This Computer"),
        (Some("google"), Some(a)) => format!("Google — {a}"),
        (Some("google"), None) => "Google".to_string(),
        (Some("microsoft365"), Some(a)) => format!("Microsoft 365 — {a}"),
        (Some("microsoft365"), None) => "Microsoft 365".to_string(),
        (_, Some(a)) => format!("CardDAV — {a}"),
        (_, None) => i18n("CardDAV Address Book"),
    }
}

/// Whether a source keyfile's `[Data Source]` section says `Enabled=false`.
fn data_source_disabled(data: &str) -> bool {
    let mut in_data_source = false;
    for line in data.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_data_source = line.eq_ignore_ascii_case("[Data Source]");
            continue;
        }
        if in_data_source {
            if let Some(v) = line.strip_prefix("Enabled=") {
                return v.trim().eq_ignore_ascii_case("false");
            }
        }
    }
    false
}

/// Discover the versioned Sources registry bus name (e.g. `…Sources5`).
fn sources_dest() -> Option<String> {
    let dir = std::path::Path::new("/usr/share/dbus-1/services");
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.contains("evolution.dataserver.Sources") && name.ends_with(".service") {
            if let Ok(text) = std::fs::read_to_string(entry.path()) {
                if let Some(n) = text.lines().find_map(|l| l.strip_prefix("Name=")) {
                    return Some(n.trim().to_string());
                }
            }
        }
    }
    None
}

/// Whether the registry lists `uid` as live (fail-open when unreachable).
fn book_uid_active(uid: &str, books: &Option<HashMap<String, String>>) -> bool {
    books.as_ref().map_or(true, |map| map.contains_key(uid))
}

/// The registry's friendly name for a book, with a fallback for when the
/// registry is unreachable.
fn book_uid_name(
    uid: &str,
    books: &Option<HashMap<String, String>>,
    fallback: &str,
) -> String {
    books
        .as_ref()
        .and_then(|map| map.get(uid).cloned())
        .unwrap_or_else(|| source_display_name(uid).unwrap_or_else(|| fallback.to_string()))
}

/// Every contact from the local + cached (CardDAV) EDS books, fully parsed
/// and sorted by display name. Contact *lists* (EDS distribution lists) are
/// skipped — they aren't people.
pub fn read_contact_details() -> Vec<ContactDetails> {
    let mut out: Vec<ContactDetails> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let active = registry_books();
    // Walk the book directories directly (like `writable_books`) so each
    // contact remembers which book it belongs to.
    if let Some(dir) = data_dir() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let db = entry.path().join("contacts.db");
            if !db.is_file() {
                continue;
            }
            let folder = entry.file_name().to_string_lossy().to_string();
            let uid = if folder == "system" { "system-address-book".to_string() } else { folder };
            if !book_uid_active(&uid, &active) {
                continue;
            }
            let name = book_uid_name(&uid, &active, "On This Computer");
            read_detail_db(&db, "SELECT vcard FROM folder_id", &uid, &name, &mut out, &mut seen);
        }
    }
    if let Some(dir) = cache_dir() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let db = entry.path().join("cache.db");
            if !db.is_file() {
                continue;
            }
            let uid = entry.file_name().to_string_lossy().to_string();
            if !book_uid_active(&uid, &active) {
                continue;
            }
            let name = book_uid_name(&uid, &active, "CardDAV Address Book");
            read_detail_db(
                &db,
                "SELECT ECacheOBJ FROM ECacheObjects",
                &uid,
                &name,
                &mut out,
                &mut seen,
            );
        }
    }
    out.sort_by_key(|c| c.name.to_lowercase());
    out
}

fn read_detail_db(
    path: &std::path::Path,
    query: &str,
    book_uid: &str,
    book_name: &str,
    out: &mut Vec<ContactDetails>,
    seen: &mut HashSet<String>,
) {
    let conn = match open_book_db(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("contacts: cannot open {}: {e}", path.display());
            return;
        }
    };
    let mut stmt = match conn.prepare(query) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("contacts: query failed on {}: {e}", path.display());
            return;
        }
    };
    let rows = stmt.query_map([], |row| row.get::<_, String>(0));
    if let Ok(rows) = rows {
        for vcard in rows.flatten() {
            let Some(mut c) = parse_vcard_details(&vcard) else { continue };
            c.book_uid = book_uid.to_string();
            c.book_name = book_name.to_string();
            c.raw_vcard = vcard;
            // The same person can live in several books; the first email is
            // as stable an identity as vCards offer.
            let key = c
                .primary_email()
                .map(|e| e.to_lowercase())
                .unwrap_or_else(|| format!("name:{}", c.name.to_lowercase()));
            if seen.insert(key) {
                out.push(c);
            }
        }
    }
}

/// The human label for a property's TYPE parameters ("" when none applies).
fn vcard_type_label(params: &[&str]) -> String {
    let joined = params.join(";").to_uppercase();
    for (needle, label) in [
        ("CELL", "Mobile"),
        ("HOME", "Home"),
        ("WORK", "Work"),
        ("FAX", "Fax"),
    ] {
        if joined.contains(needle) {
            return label.to_string();
        }
    }
    String::new()
}

/// "1985-04-12" / "19850412" → "April 12, 1985"; "--04-12" → "April 12";
/// anything else passes through unchanged.
fn pretty_birthday(raw: &str) -> String {
    const MONTHS: [&str; 12] = [
        "January", "February", "March", "April", "May", "June", "July", "August", "September",
        "October", "November", "December",
    ];
    let head = raw.split('T').next().unwrap_or(raw);
    let digits: String = head.chars().filter(|c| c.is_ascii_digit()).collect();
    let (year, month, day) = if head.starts_with("--") && digits.len() == 4 {
        (None, &digits[0..2], &digits[2..4])
    } else if digits.len() == 8 {
        (Some(&digits[0..4]), &digits[4..6], &digits[6..8])
    } else {
        return raw.to_string();
    };
    let (Ok(m), Ok(d)) = (month.parse::<usize>(), day.parse::<u32>()) else {
        return raw.to_string();
    };
    let Some(month_name) = (1..=12).contains(&m).then(|| MONTHS[m - 1]) else {
        return raw.to_string();
    };
    match year {
        Some(y) => format!("{month_name} {d}, {y}"),
        None => format!("{month_name} {d}"),
    }
}

/// Parse the fields the Contacts view shows out of one vCard. Returns `None`
/// for contact lists and entries with neither a name nor an address.
fn parse_vcard_details(vcard: &str) -> Option<ContactDetails> {
    let mut c = ContactDetails::default();
    for line in unfold_vcard(vcard).lines() {
        let Some((prop, raw_value)) = split_vcard_line(line) else { continue };
        let mut parts = prop.split(';');
        // Group prefixes (`item1.EMAIL`) hide the property name; strip them.
        let name = parts
            .next()
            .unwrap_or("")
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        let params: Vec<&str> = parts.collect();
        let value = unescape_vcard_text(raw_value.trim());
        if value.is_empty() {
            continue;
        }
        match name.as_str() {
            "X-EVOLUTION-LIST" if value.eq_ignore_ascii_case("true") => return None,
            "UID" if c.eds_uid.is_empty() => c.eds_uid = value,
            "FN" => c.name = value,
            "NICKNAME" if c.nickname.is_empty() => c.nickname = value,
            "ORG" => {
                // ORG is org;department;… — the org alone reads best.
                c.org = value.split(';').next().unwrap_or("").trim().to_string();
            }
            "TITLE" if c.title.is_empty() => c.title = value,
            "BDAY" => c.birthday = pretty_birthday(&value),
            "NOTE" if c.note.is_empty() => c.note = value,
            "EMAIL" => {
                if value.contains('@') && !c.emails.iter().any(|e| e.value == value) {
                    c.emails.push(Labeled { label: vcard_type_label(&params), value });
                }
            }
            "TEL" => {
                if !c.phones.iter().any(|p| p.value == value) {
                    c.phones.push(Labeled { label: vcard_type_label(&params), value });
                }
            }
            "URL" => {
                if !c.urls.contains(&value) {
                    c.urls.push(value);
                }
            }
            "ADR" => {
                // pobox;extended;street;locality;region;postal-code;country.
                let formatted = raw_value
                    .split(';')
                    .map(|p| unescape_vcard_text(p.trim()))
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
                    .join(", ");
                if !formatted.is_empty() && !c.addresses.iter().any(|a| a.value == formatted) {
                    c.addresses.push(Labeled { label: vcard_type_label(&params), value: formatted });
                }
            }
            _ => {}
        }
    }
    if c.name.trim().is_empty() {
        c.name = c
            .primary_email()
            .map(str::to_string)
            .or_else(|| (!c.nickname.is_empty()).then(|| c.nickname.clone()))?;
    }
    c.photo = vcard_photo(vcard);
    Some(c)
}

/// Sample contacts for demo mode (HYLKI_DEMO): the people from the demo
/// mailbox, fleshed out so the contacts view has something to show off.
/// No book/EDS identity — demo entries are display-only.
pub fn demo_contacts() -> Vec<ContactDetails> {
    let l = |label: &str, value: &str| Labeled { label: label.into(), value: value.into() };
    let contact = |name: &str, book: &str| ContactDetails {
        name: name.into(),
        book_name: book.into(),
        ..ContactDetails::default()
    };
    vec![
        ContactDetails {
            nickname: "Soph".into(),
            org: "Studio.dev".into(),
            title: "Design Lead".into(),
            birthday: "March 4, 1991".into(),
            note: "Prefers Figma links over attachments. Out on Fridays.".into(),
            emails: vec![l("Work", "sophie@studio.dev"), l("Home", "sophie.t@example.com")],
            phones: vec![l("Mobile", "+1 (415) 555-0114")],
            addresses: vec![l("Work", "2261 Market St, San Francisco, CA 94114")],
            urls: vec!["sophie.design".into()],
            ..contact("Sophie Turner", "CardDAV — jason@hylki.hyprlab.co")
        },
        ContactDetails {
            org: "Studio.dev".into(),
            title: "Engineering Manager".into(),
            emails: vec![l("Work", "marcus@studio.dev")],
            phones: vec![l("Mobile", "+1 (415) 555-0187"), l("Work", "+1 (415) 555-0100")],
            ..contact("Marcus Chen", "CardDAV — jason@hylki.hyprlab.co")
        },
        ContactDetails {
            org: "Studio.dev".into(),
            title: "Product Designer".into(),
            birthday: "November 19".into(),
            emails: vec![l("Work", "priya@studio.dev")],
            phones: vec![l("Mobile", "+1 (628) 555-0163")],
            ..contact("Priya Sharma", "Google — jason.m@gmail.com")
        },
        ContactDetails {
            title: "Illustrator".into(),
            note: "Freelance — invoices go to the studio address.".into(),
            emails: vec![l("", "emma@example.com")],
            urls: vec!["emmawright.art".into()],
            ..contact("Emma Wright", "Google — jason.m@gmail.com")
        },
        ContactDetails {
            org: "GNOME Foundation".into(),
            emails: vec![l("Work", "lena@gnome.org")],
            addresses: vec![l("Work", "21 Orinda Way, Orinda, CA 94563")],
            ..contact("Lena Fischer", "On This Computer")
        },
        ContactDetails {
            org: "Ferrous Type".into(),
            title: "Founder".into(),
            emails: vec![l("Work", "diego@ferroustype.com"), l("Home", "d.alvarez@example.com")],
            phones: vec![l("Mobile", "+34 612 555 021")],
            urls: vec!["ferroustype.com".into()],
            ..contact("Diego Álvarez", "CardDAV — jason@hylki.hyprlab.co")
        },
        ContactDetails {
            emails: vec![l("Home", "tom.okafor@example.com")],
            phones: vec![l("Mobile", "+44 7700 900 214")],
            birthday: "July 30, 1988".into(),
            ..contact("Tom Okafor", "On This Computer")
        },
        ContactDetails {
            org: "Kim & Partners".into(),
            title: "Attorney".into(),
            emails: vec![l("Work", "grace@kimpartners.example")],
            phones: vec![l("Work", "+1 (212) 555-0170")],
            addresses: vec![l("Work", "425 Lexington Ave, New York, NY 10017")],
            ..contact("Grace Kim", "CardDAV — jason@hylki.hyprlab.co")
        },
    ]
}

// ---------------------------------------------------------------------------
// In-app editing (vCard patching + EDS writes)
// ---------------------------------------------------------------------------

/// The fields the in-app contact editor can change.
#[derive(Debug, Clone, Default)]
pub struct ContactEdit {
    pub name: String,
    pub nickname: String,
    pub org: String,
    pub title: String,
    pub note: String,
    pub emails: Vec<Labeled>,
    pub phones: Vec<Labeled>,
    pub urls: Vec<String>,
}

/// Escape a value for a vCard text property.
fn escape_vcard_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ',' => out.push_str("\\,"),
            ';' => out.push_str("\\;"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            other => out.push(other),
        }
    }
    out
}

/// The TYPE parameter matching an editor label ("" for none).
fn type_param_for(label: &str) -> &'static str {
    match label {
        "Home" => ";TYPE=HOME",
        "Work" => ";TYPE=WORK",
        "Mobile" => ";TYPE=CELL",
        "Fax" => ";TYPE=FAX",
        _ => "",
    }
}

/// Rewrite a stored vCard with the editor's values. Only the editable
/// properties are replaced — everything else (UID, PHOTO, ADR, REV, X-…)
/// passes through untouched, so nothing GNOME Contacts manages is lost.
pub fn patched_vcard(original: &str, e: &ContactEdit) -> String {
    const REPLACED: [&str; 9] =
        ["FN", "N", "NICKNAME", "ORG", "TITLE", "NOTE", "EMAIL", "TEL", "URL"];
    let mut out: Vec<String> = Vec::new();
    let mut skipping = false;
    for raw in original.lines() {
        let line = raw.trim_end_matches('\r');
        // Folded continuation lines belong to the previous property.
        if line.starts_with(' ') || line.starts_with('\t') {
            if !skipping {
                out.push(line.to_string());
            }
            continue;
        }
        if line.eq_ignore_ascii_case("END:VCARD") {
            break;
        }
        let prop = line
            .split([':', ';'])
            .next()
            .unwrap_or("")
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        skipping = REPLACED.contains(&prop.as_str());
        if !skipping {
            out.push(line.to_string());
        }
    }

    let name = e.name.trim();
    out.push(format!("FN:{}", escape_vcard_value(name)));
    // Structured name: last word as the family name — the same heuristic an
    // address book's "sort by surname" uses.
    let mut words: Vec<&str> = name.split_whitespace().collect();
    let family = if words.len() > 1 { words.pop().unwrap_or("") } else { "" };
    out.push(format!(
        "N:{};{};;;",
        escape_vcard_value(family),
        escape_vcard_value(&words.join(" "))
    ));
    if !e.nickname.trim().is_empty() {
        out.push(format!("NICKNAME:{}", escape_vcard_value(e.nickname.trim())));
    }
    if !e.org.trim().is_empty() {
        out.push(format!("ORG:{};", escape_vcard_value(e.org.trim())));
    }
    if !e.title.trim().is_empty() {
        out.push(format!("TITLE:{}", escape_vcard_value(e.title.trim())));
    }
    if !e.note.trim().is_empty() {
        out.push(format!("NOTE:{}", escape_vcard_value(e.note.trim())));
    }
    for email in &e.emails {
        out.push(format!(
            "EMAIL{}:{}",
            type_param_for(&email.label),
            escape_vcard_value(&email.value)
        ));
    }
    for phone in &e.phones {
        out.push(format!(
            "TEL{}:{}",
            type_param_for(&phone.label),
            escape_vcard_value(&phone.value)
        ));
    }
    for url in &e.urls {
        out.push(format!("URL:{}", escape_vcard_value(url)));
    }
    out.push("END:VCARD".to_string());
    out.join("\r\n") + "\r\n"
}

/// A brand-new contact's vCard from the editor's values.
pub fn new_vcard(e: &ContactEdit) -> String {
    patched_vcard("BEGIN:VCARD\r\nVERSION:3.0\r\nEND:VCARD\r\n", e)
}

/// Write a modified vCard back to its book through EDS.
pub fn modify_contact(book_uid: &str, vcard: &str) -> Result<(), String> {
    let dest = factory_dest().ok_or("Evolution Data Server is not available")?;
    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let (bus, path) = open_book(&conn, &dest, book_uid)?;
    book_call(&conn, &bus, &path, "ModifyContacts", &(vec![vcard.to_string()], 0u32))?;
    Ok(())
}

/// Create a contact from a full vCard through EDS.
pub fn create_contact(book_uid: &str, vcard: &str) -> Result<(), String> {
    let dest = factory_dest().ok_or("Evolution Data Server is not available")?;
    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let (bus, path) = open_book(&conn, &dest, book_uid)?;
    book_call(&conn, &bus, &path, "CreateContacts", &(vec![vcard.to_string()], 0u32))?;
    Ok(())
}

/// Delete a contact (by its EDS UID) from its book.
pub fn delete_contact(book_uid: &str, uid: &str) -> Result<(), String> {
    let dest = factory_dest().ok_or("Evolution Data Server is not available")?;
    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let (bus, path) = open_book(&conn, &dest, book_uid)?;
    book_call(&conn, &bus, &path, "RemoveContacts", &(vec![uid.to_string()], 0u32))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{vcard_display_name, vcard_photo};

    const ONE_PIXEL_PNG: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

    // iCloud's CardDAV photos: a quoted https URL parameter precedes the real
    // file value — the split must not stop at the colon inside the quotes.
    #[test]
    fn quoted_params_do_not_swallow_the_value() {
        let line = "PHOTO;X-EVOLUTION-WEBDAV-IMG-URL=\"https://gateway.icloud.com/x/y\";\
                    VALUE=uri:file:///tmp/photo.jpg";
        let (prop, value) = super::split_vcard_line(line).unwrap();
        assert!(prop.ends_with("VALUE=uri"));
        assert_eq!(value, "file:///tmp/photo.jpg");
    }

    #[test]
    fn fn_preserves_case() {
        let v = "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Arnwine;Aaron;;;\r\nFN:Aaron Arnwine\r\nEND:VCARD\r\n";
        assert_eq!(vcard_display_name(v).as_deref(), Some("Aaron Arnwine"));
    }

    #[test]
    fn fn_with_parameters() {
        let v = "FN;PID=1.1:Jane O'Brien\r\n";
        assert_eq!(vcard_display_name(v).as_deref(), Some("Jane O'Brien"));
    }

    #[test]
    fn fn_line_folded() {
        // A folded FN value continues on the next line after a leading space.
        let v = "FN:Reallylongfirst\r\n Lastname\r\n";
        assert_eq!(vcard_display_name(v).as_deref(), Some("ReallylongfirstLastname"));
    }

    #[test]
    fn fn_escaped_comma() {
        let v = "FN:Smith\\, John\r\n";
        assert_eq!(vcard_display_name(v).as_deref(), Some("Smith, John"));
    }

    #[test]
    fn empty_or_missing_fn() {
        assert_eq!(vcard_display_name("FN:\r\nN:x;;;;\r\n"), None);
        assert_eq!(vcard_display_name("N:Doe;John;;;\r\n"), None);
    }

    #[test]
    fn photo_decodes_nextcloud_data_uri() {
        let vcard = format!("item1.PHOTO:data:image/image/png\\;base64\\,{ONE_PIXEL_PNG}\r\n");
        let photo = vcard_photo(&vcard).unwrap();
        assert_eq!(&photo[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn photo_decodes_folded_encoding_b() {
        let split = ONE_PIXEL_PNG.len() / 2;
        let vcard = format!(
            "PHOTO;ENCODING=b;TYPE=PNG:{}\r\n {}\r\n",
            &ONE_PIXEL_PNG[..split],
            &ONE_PIXEL_PNG[split..]
        );
        assert!(vcard_photo(&vcard).is_some());
    }

    #[test]
    fn photo_does_not_fetch_remote_uri() {
        assert!(vcard_photo("PHOTO;VALUE=uri:https://example.com/tracker.png\r\n").is_none());
    }

    /// A scratch directory tree for exercising [`super::confine_to_roots`]:
    /// `<tmp>/vireo-confine-<pid>-<n>/{root,outside}` with one file in each,
    /// removed on drop.
    struct ConfineFixture {
        base: std::path::PathBuf,
    }

    impl ConfineFixture {
        fn new(n: u32) -> Self {
            let base = std::env::temp_dir()
                .join(format!("hylki-confine-{}-{n}", std::process::id()));
            std::fs::create_dir_all(base.join("root")).unwrap();
            std::fs::create_dir_all(base.join("outside")).unwrap();
            std::fs::write(base.join("root/photo.png"), b"in").unwrap();
            std::fs::write(base.join("outside/secret"), b"out").unwrap();
            Self { base }
        }

        fn root(&self) -> std::path::PathBuf {
            self.base.join("root")
        }
    }

    impl Drop for ConfineFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn photo_files_inside_a_root_are_admitted() {
        let fx = ConfineFixture::new(1);
        let roots = vec![fx.root()];
        let admitted = super::confine_to_roots(&fx.root().join("photo.png"), &roots);
        assert!(admitted.is_some());
    }

    #[test]
    fn dotdot_traversal_cannot_leave_a_root() {
        let fx = ConfineFixture::new(2);
        let roots = vec![fx.root()];
        let escape = fx.root().join("../outside/secret");
        assert_eq!(super::confine_to_roots(&escape, &roots), None);
    }

    #[test]
    fn a_symlink_planted_inside_a_root_cannot_reach_outside() {
        let fx = ConfineFixture::new(3);
        let roots = vec![fx.root()];
        let link = fx.root().join("link.png");
        std::os::unix::fs::symlink(fx.base.join("outside/secret"), &link).unwrap();
        assert_eq!(super::confine_to_roots(&link, &roots), None);
    }

    #[test]
    fn a_lookalike_sibling_directory_is_not_a_root() {
        let fx = ConfineFixture::new(4);
        let sibling = fx.base.join("root-evil");
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("photo.png"), b"no").unwrap();
        let roots = vec![fx.root()];
        assert_eq!(super::confine_to_roots(&sibling.join("photo.png"), &roots), None);
    }

    #[test]
    fn no_roots_admits_nothing() {
        let fx = ConfineFixture::new(5);
        assert_eq!(super::confine_to_roots(&fx.root().join("photo.png"), &[]), None);
        // A root that doesn't resolve is the same as no root at all.
        let ghost = vec![fx.base.join("missing")];
        assert_eq!(super::confine_to_roots(&fx.root().join("photo.png"), &ghost), None);
    }

    #[test]
    fn a_host_bearing_file_uri_is_refused() {
        assert_eq!(
            super::read_eds_photo_file("file://evil-host/etc/passwd", 1024),
            None
        );
    }
}
