//! Reading the address books: the EDS SQLite files, opened read-only,
//! which books are wanted, and the Hylki book beside them.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use super::details::{
    parse_vcard_fields, person_address, resolve_book_members, resolve_members, ContactDetails,
    GroupFormat,
};
use super::eds_write::{factory_dest, open_book, BOOK_IFACE};
use super::photos::find_dbs;
use super::registry::{book_uid_active, book_uid_name, registry_books, registry_books_and_formats};
use super::vcard::{is_group_vcard, vcard_display_name, vcard_photo_with};
use super::Contact;
use crate::i18n::i18n;

/// A writable address book the user can add contacts to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Book {
    /// EDS source UID (what `OpenAddressBook` expects).
    pub(crate) uid: String,
    pub(crate) name: String,
    /// How a new group is written to this book; `None` where the server
    /// keeps no groups (Google stores them as odd people, Microsoft 365
    /// refuses them).
    pub(crate) groups: Option<GroupFormat>,
}

// Inside a Flatpak sandbox `dirs::{data,cache,config}_dir()` are redirected into
// ~/.var/app/<app-id>/, but EDS's books live in the *host's* real XDG dirs (made
// visible by the `--filesystem=xdg-{data,cache,config}/evolution:ro` grants). So
// under Flatpak resolve them from the real home instead of the redirected XDG env.
fn host_xdg(sandbox: fn() -> Option<PathBuf>, subdir: &str) -> Option<PathBuf> {
    if crate::desktop::platform::is_flatpak() {
        dirs::home_dir().map(|h| h.join(subdir))
    } else {
        sandbox()
    }
}

pub(super) fn data_dir() -> Option<PathBuf> {
    host_xdg(dirs::data_dir, ".local/share").map(|d| d.join("evolution/addressbook"))
}

pub(super) fn cache_dir() -> Option<PathBuf> {
    host_xdg(dirs::cache_dir, ".cache").map(|d| d.join("evolution/addressbook"))
}

/// How EDS lays out one kind of address book on disk: a book kept on this
/// computer is a `contacts.db` under the data directory, a book mirrored
/// from a server (CardDAV, Google…) a `cache.db` under the cache directory,
/// each with its own table and column names.
pub(super) struct EdsLayout {
    /// Where the books' directories are.
    root: fn() -> Option<PathBuf>,
    /// The database file in each book's directory.
    file: &'static str,
    /// The table of cards, with its vCard and UID columns.
    pub(super) cards: &'static str,
    pub(super) vcard: &'static str,
    pub(super) uid: &'static str,
    /// The table of the cards' addresses: `value`, by `uid`.
    pub(super) emails: &'static str,
    /// A book mirrored from a server.
    pub(super) cached: bool,
    /// What a book is called when the registry names none.
    pub(super) unnamed: &'static str,
}

pub(super) const LOCAL_BOOKS: EdsLayout = EdsLayout {
    root: data_dir,
    file: "contacts.db",
    cards: "folder_id",
    vcard: "vcard",
    uid: "uid",
    emails: "folder_id_email_list",
    cached: false,
    unnamed: "On This Computer",
};

pub(super) const CACHED_BOOKS: EdsLayout = EdsLayout {
    root: cache_dir,
    file: "cache.db",
    cards: "ECacheObjects",
    vcard: "ECacheOBJ",
    uid: "ECacheUID",
    emails: "attrlist_email_list",
    cached: true,
    unnamed: "CardDAV Address Book",
};

/// Both layouts, in the order the books are read.
pub(super) const EDS_LAYOUTS: [&EdsLayout; 2] = [&LOCAL_BOOKS, &CACHED_BOOKS];

impl EdsLayout {
    /// Every book database of this layout.
    pub(super) fn dbs(&self) -> Vec<PathBuf> {
        (self.root)()
            .map(|dir| find_dbs(&dir, self.file))
            .unwrap_or_default()
    }

    /// The book databases [`db_book_wanted`] keeps.
    pub(super) fn wanted_dbs<'a>(
        &self,
        active: &'a Option<HashMap<String, String>>,
        saved_only: bool,
    ) -> impl Iterator<Item = PathBuf> + 'a {
        self.dbs()
            .into_iter()
            .filter(move |db| db_book_wanted(db, active, saved_only))
    }

    /// Every book of this layout as (database, source UID). The built-in
    /// local book's folder is `system`, its source UID
    /// `system-address-book`.
    pub(super) fn books(&self) -> Vec<(PathBuf, String)> {
        let Some(dir) = (self.root)() else {
            return Vec::new();
        };
        std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let db = entry.path().join(self.file);
                if !db.is_file() {
                    return None;
                }
                let folder = entry.file_name().to_string_lossy().to_string();
                let uid = if !self.cached && folder == "system" {
                    "system-address-book".to_string()
                } else {
                    folder
                };
                Some((db, uid))
            })
            .collect()
    }

    /// Each card with one of its addresses, as (vCard, address).
    fn cards_and_addresses(&self) -> String {
        format!(
            "SELECT c.{vcard}, e.value FROM {cards} c JOIN {emails} e ON c.{uid} = e.uid",
            vcard = self.vcard,
            cards = self.cards,
            emails = self.emails,
            uid = self.uid,
        )
    }
}

fn config_dir() -> Option<PathBuf> {
    host_xdg(dirs::config_dir, ".config").map(|d| d.join("evolution"))
}

/// Read every address email from the local + cached (CardDAV) EDS books.
pub(crate) fn read_contacts() -> Vec<Contact> {
    read_contacts_from(false).0
}

/// Like [`read_contacts`], but only the contacts someone saved: the books
/// that fill themselves from mail ("Collected Addresses", "Recently
/// contacted", "Other contacts") are left out, so a filter on "sender is in
/// Contacts" does not trust an address just because you once mailed it.
/// `None` when any book could not be read: a filter then cannot tell a
/// stranger from a contact whose book failed, and must match neither way.
fn read_saved_contacts() -> Option<Vec<Contact>> {
    let (contacts, complete) = read_contacts_from(true);
    complete.then_some(contacts)
}

/// The saved contacts' addresses, lower-cased, for the filter rules that ask
/// whether a sender is in Contacts (PR #384). The filter pass runs on the main
/// thread and a read goes to D-Bus and every book, so after the first read
/// the set is refreshed in the background once it is a minute old, and the
/// pass uses the last one. `None` inside means unknown: unreadable or empty.
#[derive(Clone, Default)]
pub(crate) struct SavedAddresses(std::sync::Arc<std::sync::Mutex<SavedAddressesState>>);

#[derive(Default)]
struct SavedAddressesState {
    read_at: Option<std::time::Instant>,
    set: Option<std::sync::Arc<HashSet<String>>>,
    refreshing: bool,
}

impl SavedAddresses {
    const FRESH_FOR: std::time::Duration = std::time::Duration::from_secs(60);

    /// The addresses for this filter pass. Only the very first call reads
    /// on the caller's thread: mail filtered before any read would never be
    /// looked at again.
    pub(crate) fn get(&self) -> Option<std::sync::Arc<HashSet<String>>> {
        let (never, stale) = {
            let st = self.0.lock().unwrap_or_else(|e| e.into_inner());
            (
                st.read_at.is_none(),
                st.read_at.is_some_and(|t| t.elapsed() > Self::FRESH_FOR),
            )
        };
        if never {
            let set = read_saved_addresses();
            let mut st = self.0.lock().unwrap_or_else(|e| e.into_inner());
            st.read_at = Some(std::time::Instant::now());
            st.set = set;
        } else if stale {
            self.refresh();
        }
        self.0.lock().unwrap_or_else(|e| e.into_inner()).set.clone()
    }

    /// Read the addresses again in the background (after a contact changed,
    /// or a rule that needs them was added).
    pub(crate) fn refresh(&self) {
        {
            let mut st = self.0.lock().unwrap_or_else(|e| e.into_inner());
            if st.refreshing {
                return;
            }
            st.refreshing = true;
        }
        let shared = self.0.clone();
        std::thread::spawn(move || {
            let set = read_saved_addresses();
            let mut st = shared.lock().unwrap_or_else(|e| e.into_inner());
            st.read_at = Some(std::time::Instant::now());
            st.set = set;
            st.refreshing = false;
        });
    }
}

fn read_saved_addresses() -> Option<std::sync::Arc<HashSet<String>>> {
    let set: HashSet<String> = read_saved_contacts()?
        .into_iter()
        .map(|c| c.email.trim().to_lowercase())
        .collect();
    (!set.is_empty()).then(|| std::sync::Arc::new(set))
}

/// Whether a book fills itself from mail rather than being kept by hand.
/// Judged by the source UID and display name, the only trace EDS leaves.
pub(super) fn is_auto_collected(uid: &str, name: &str) -> bool {
    const MARKERS: [&str; 3] = ["collected", "recently contacted", "other contacts"];
    let (uid, name) = (uid.to_lowercase(), name.to_lowercase());
    MARKERS.iter().any(|m| uid.contains(m) || name.contains(m))
}

/// Shared body of [`read_contacts`] and [`read_saved_contacts`], and
/// whether every book it wanted could be read.
fn read_contacts_from(saved_only: bool) -> (Vec<Contact>, bool) {
    read_contacts_in(&registry_books(), saved_only)
}

/// [`read_contacts_from`] in the books `active` names.
pub(super) fn read_contacts_in(
    active: &Option<HashMap<String, String>>,
    saved_only: bool,
) -> (Vec<Contact>, bool) {
    let mut complete = true;
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // Local books, then cached ones (CardDAV etc.). Read the vCard (not
    // `full_name`, which EDS stores case-folded for search) so the display
    // name keeps its original capitalisation.
    for layout in EDS_LAYOUTS {
        let sql = layout.cards_and_addresses();
        for db in layout.wanted_dbs(active, saved_only) {
            complete &= read_book_db(&db, &sql, &mut out, &mut seen);
        }
    }
    // The local Hylki book, last: an address EDS already has keeps EDS's name.
    complete &= add_local_contacts(&mut out, &mut seen);
    (out, complete)
}

/// Every contact group in the address books the suggestions read, with its
/// members' addresses: Evolution's lists, vCard 4 and Apple groups,
/// Nextcloud's categories in the books `formats` says keep them, and the
/// groups of the Hylki book, in the EDS books `active` names. Only the
/// groups' cards are read, and the cards with categories, not the books.
pub(super) fn read_groups_in(
    active: &Option<HashMap<String, String>>,
    formats: &HashMap<String, Option<GroupFormat>>,
) -> Vec<ContactDetails> {
    const WHERE: &str = "LIKE '%X-EVOLUTION-LIST%' OR {col} LIKE '%KIND:group%'";
    let mut out = Vec::new();
    let mut read = |db: &std::path::Path, table: &str, col: &str, uid_col: &str| {
        let Ok(conn) = open_book_db(db) else { return };
        let query = format!(
            "SELECT {col} FROM {table} WHERE {col} {}",
            WHERE.replace("{col}", col)
        );
        let Ok(mut stmt) = conn.prepare(&query) else {
            return;
        };
        let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
            return;
        };
        let lookup_sql = format!("SELECT {col} FROM {table} WHERE {uid_col} = ?1");
        let lookup = |uid: &str| -> Option<(String, String)> {
            let vcard: String = conn.query_row(&lookup_sql, [uid], |r| r.get(0)).ok()?;
            person_address(&vcard)
        };
        for vcard in rows.flatten() {
            let Some(mut c) = parse_vcard_fields(&vcard).filter(ContactDetails::is_group) else {
                continue;
            };
            resolve_members(&mut c, lookup);
            c.raw_vcard = vcard;
            out.push(c);
        }
    };
    for layout in EDS_LAYOUTS {
        for db in layout.wanted_dbs(active, false) {
            read(&db, layout.cards, layout.vcard, layout.uid);
        }
    }
    out.extend(read_category_groups(active, formats));
    if crate::contacts::local::exists() && crate::contacts::contact_sources::hylki_used() {
        match crate::contacts::local::group_vcards() {
            Ok(vcards) => {
                for vcard in vcards {
                    let Some(mut c) = parse_vcard_fields(&vcard).filter(ContactDetails::is_group)
                    else {
                        continue;
                    };
                    resolve_members(&mut c, |uid| {
                        crate::contacts::local::vcard_by_uid(uid)
                            .ok()
                            .flatten()
                            .as_deref()
                            .and_then(person_address)
                    });
                    c.book_uid = crate::contacts::local::LOCAL_BOOK_UID.to_string();
                    c.raw_vcard = vcard;
                    out.push(c);
                }
            }
            Err(e) => tracing::warn!("contacts: cannot read the local address book's groups: {e}"),
        }
    }
    out
}

/// The category groups of the cached books that keep them, from the cards
/// that have categories.
fn read_category_groups(
    active: &Option<HashMap<String, String>>,
    formats: &HashMap<String, Option<GroupFormat>>,
) -> Vec<ContactDetails> {
    let mut cards = Vec::new();
    let layout = &CACHED_BOOKS;
    for (db, uid) in layout.books() {
        if formats.get(&uid) != Some(&Some(GroupFormat::Categories))
            || !book_uid_active(&uid, active)
        {
            continue;
        }
        let Ok(conn) = open_book_db(&db) else {
            continue;
        };
        let query = format!(
            "SELECT {col} FROM {table} WHERE {col} LIKE '%CATEGORIES%'",
            col = layout.vcard,
            table = layout.cards
        );
        let Ok(mut stmt) = conn.prepare(&query) else {
            continue;
        };
        let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
            continue;
        };
        for vcard in rows.flatten() {
            let Some(mut c) = parse_vcard_fields(&vcard).filter(|c| !c.is_group()) else {
                continue;
            };
            c.book_uid = uid.clone();
            c.raw_vcard = vcard;
            cards.push(c);
        }
    }
    super::categories::add_category_groups(&mut cards, formats);
    cards.retain(ContactDetails::is_group);
    cards
}

/// Append the local book's addresses, skipping the ones already in `seen`.
/// False when the book is there but could not be read.
fn add_local_contacts(out: &mut Vec<Contact>, seen: &mut HashSet<String>) -> bool {
    if !crate::contacts::local::exists() || !crate::contacts::contact_sources::hylki_used() {
        return true;
    }
    let rows = match crate::contacts::local::emails() {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("contacts: cannot read the local address book: {e}");
            return false;
        }
    };
    for (name, email) in rows {
        let email = email.trim().to_string();
        if email.is_empty() || !email.contains('@') || !seen.insert(email.to_lowercase()) {
            continue;
        }
        let name = if name.trim().is_empty() {
            email.clone()
        } else {
            name
        };
        out.push(Contact { name, email });
    }
    true
}

/// Whether `book_uid` is the local Hylki book rather than an EDS one.
pub(crate) fn is_local_book(book_uid: &str) -> bool {
    book_uid == crate::contacts::local::LOCAL_BOOK_UID
}

/// The local Hylki book as a [`Book`].
pub(crate) fn local_book() -> Book {
    Book {
        uid: crate::contacts::local::LOCAL_BOOK_UID.to_string(),
        name: crate::contacts::contact_sources::names()
            .remove(crate::contacts::local::LOCAL_BOOK_UID)
            .unwrap_or_else(|| i18n("Hylki Address Book")),
        groups: Some(GroupFormat::Evolution),
    }
}

/// Parse stored local vCards into the Contacts view's entries. A local
/// vCard may come from any `.vcf` file, so its PHOTO is read only when
/// embedded, never from a `file://` path.
pub(super) fn details_from_local_vcards(
    vcards: Vec<String>,
    book_name: &str,
) -> Vec<ContactDetails> {
    vcards
        .into_iter()
        .filter_map(|vcard| {
            let mut c = parse_vcard_fields(&vcard)?;
            c.book_uid = crate::contacts::local::LOCAL_BOOK_UID.to_string();
            c.book_name = book_name.to_string();
            c.photo = vcard_photo_with(&vcard, false);
            c.raw_vcard = vcard;
            Some(c)
        })
        .collect()
}

/// The local book's contacts, parsed (empty when it was never written).
pub(super) fn local_details() -> Vec<ContactDetails> {
    if !crate::contacts::local::exists() || !crate::contacts::contact_sources::hylki_used() {
        return Vec::new();
    }
    match crate::contacts::local::list_vcards() {
        Ok(vcards) => {
            let mut book = details_from_local_vcards(vcards, &local_book().name);
            resolve_book_members(&mut book);
            book
        }
        Err(e) => {
            tracing::warn!("contacts: cannot read the local address book: {e}");
            Vec::new()
        }
    }
}

/// `book_uid_active` keyed off a book database's directory name, minus the
/// auto-collected books when `saved_only` is set.
fn db_book_wanted(
    db: &std::path::Path,
    active: &Option<HashMap<String, String>>,
    saved_only: bool,
) -> bool {
    let Some(folder) = db.parent().and_then(|p| p.file_name()) else {
        return true;
    };
    let folder = folder.to_string_lossy();
    let uid = if folder == "system" {
        "system-address-book"
    } else {
        folder.as_ref()
    };
    if !book_uid_active(uid, active) {
        return false;
    }
    let name = active
        .as_ref()
        .and_then(|m| m.get(uid))
        .map_or("", String::as_str);
    !(saved_only && is_auto_collected(uid, name))
}

pub(super) fn open_book_db(path: &std::path::Path) -> rusqlite::Result<rusqlite::Connection> {
    use rusqlite::OpenFlags;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    // Under Flatpak the book DB lives on a read-only host mount; a WAL database
    // can't be opened read-only there (it needs to touch the -shm/-wal files), so
    // open it `immutable=1` to read the committed snapshot without any locking.
    if crate::desktop::platform::is_flatpak() {
        rusqlite::Connection::open_with_flags(
            format!("file:{}?immutable=1", path.to_string_lossy()),
            flags,
        )
    } else {
        rusqlite::Connection::open_with_flags(path, flags)
    }
}

fn read_book_db(
    path: &std::path::Path,
    query: &str,
    out: &mut Vec<Contact>,
    seen: &mut HashSet<String>,
) -> bool {
    let conn = match open_book_db(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("contacts: cannot open {}: {e}", path.display());
            return false;
        }
    };
    let mut stmt = match conn.prepare(query) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("contacts: query failed on {}: {e}", path.display());
            return false;
        }
    };
    let rows = stmt.query_map([], |row| {
        let vcard: Option<String> = row.get(0).ok();
        let email: String = row.get(1)?;
        // EDS indexes a list's members as its addresses, lower-cased and
        // whole ("ann lee <ann@x.org>"): the list is offered as itself.
        let group = vcard.as_deref().is_some_and(is_group_vcard);
        let name = vcard
            .as_deref()
            .and_then(vcard_display_name)
            .unwrap_or_default();
        Ok((name, email, group))
    });
    let Ok(rows) = rows else { return false };
    for (name, email, _) in rows.flatten().filter(|(_, _, group)| !group) {
        let email = email.trim().to_string();
        if email.is_empty() || !email.contains('@') {
            continue;
        }
        let key = email.to_lowercase();
        if seen.insert(key) {
            let name = if name.trim().is_empty() {
                email.clone()
            } else {
                name
            };
            out.push(Contact { name, email });
        }
    }
    true
}

/// Address books the user can add contacts to (local + cached/CardDAV).
pub(crate) fn writable_books() -> Vec<Book> {
    let mut books = Vec::new();
    let (active, formats) = registry_books_and_formats();
    for layout in EDS_LAYOUTS {
        // A book whose groups the registry does not describe keeps them as
        // its kind of book usually does.
        let usual = if layout.cached {
            GroupFormat::Apple
        } else {
            GroupFormat::Evolution
        };
        for (_, uid) in layout.books() {
            if !book_uid_active(&uid, &active) {
                continue;
            }
            let name = book_uid_name(&uid, &active, layout.unnamed);
            let groups = formats.get(&uid).copied().unwrap_or(Some(usual));
            books.push(Book { uid, name, groups });
        }
    }
    if let (Some(dest), Ok(conn)) = (factory_dest(), zbus::blocking::Connection::session()) {
        books.retain(|b| !book_read_only(&conn, &dest, &b.uid));
    }
    // Last: unless chosen in Settings, EDS books keep priority as the
    // default destination.
    if crate::contacts::contact_sources::hylki_used() {
        books.push(local_book());
    }
    prefer_book(&mut books, &crate::config::load_contact_book());
    books
}

/// Move the book chosen for new contacts to the front, where every caller
/// takes its default from. An empty or vanished choice changes nothing.
pub(super) fn prefer_book(books: &mut [Book], uid: &str) {
    if let Some(i) = books.iter().position(|b| !uid.is_empty() && b.uid == uid) {
        books[..=i].rotate_right(1);
    }
}

/// Whether EDS says the book is read-only (Nextcloud's "Recently contacted"
/// and system address books are). `Open` fills in `Writable` — from the
/// server's privileges on first connect, cached after that. Any error counts
/// as writable, so a save still reports EDS's real failure.
fn book_read_only(conn: &zbus::blocking::Connection, dest: &str, uid: &str) -> bool {
    let Ok((bus, path)) = open_book(conn, dest, uid) else {
        return false;
    };
    conn.call_method(
        Some(bus.as_str()),
        path.as_str(),
        Some("org.freedesktop.DBus.Properties"),
        "Get",
        &(BOOK_IFACE, "Writable"),
    )
    .ok()
    .and_then(|r| r.body().deserialize::<(zbus::zvariant::OwnedValue,)>().ok())
    .and_then(|(v,)| v.downcast_ref::<bool>().ok())
        == Some(false)
}

/// Best-effort display name from the EDS source file for a book UID.
pub(super) fn source_display_name(uid: &str) -> Option<String> {
    let path = config_dir()?.join(format!("sources/{uid}.source"));
    crate::desktop::platform::keyfile_value(
        &std::fs::read_to_string(path).ok()?,
        "Data Source",
        "DisplayName",
    )
}
