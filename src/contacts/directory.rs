//! LDAP directories (#307), kept by Evolution Data Server.
//!
//! A directory is an EDS address book with the `ldap` backend, the same kind
//! Evolution makes, so one set up in either app shows in both and Hylki
//! carries no LDAP client of its own. The composer asks the directories as a
//! recipient is typed; nothing is copied down.
//!
//! EDS keeps a directory's password in the keyring only when a desktop
//! client that can prompt for it put it there, as Evolution does; EDS then
//! signs in with it on its own. Hylki keeps its own copy under `ldap:<uid>`
//! for the directories it set up, and hands it over whenever EDS says the
//! directory wants one.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::contacts::Suggestion;
use crate::i18n::{i18n, i18n_f};

pub(crate) mod eds_source;

pub(crate) use eds_source::bus;
use eds_source::{
    create_source, invoke_authenticate, new_source_uid, not_available, registry, remove_source,
    write_source, SOURCE_IFACE,
};
#[cfg(test)]
use eds_source::{MANAGER_IFACE, MANAGER_PATH};

/// Fewer characters than this match too much of a large directory to be
/// worth the round trip.
pub(crate) const MIN_QUERY: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Security {
    StartTls,
    Ldaps,
    None,
}

impl Security {
    fn key(self) -> &'static str {
        match self {
            Security::StartTls => "starttls",
            Security::Ldaps => "ldaps",
            Security::None => "none",
        }
    }

    fn from_key(s: &str) -> Self {
        match s {
            "ldaps" => Security::Ldaps,
            "none" => Security::None,
            _ => Security::StartTls,
        }
    }

    pub(crate) fn default_port(self) -> u16 {
        if self == Security::Ldaps {
            636
        } else {
            389
        }
    }
}

/// What was typed as the server, taken apart: `ldaps://host:636/dc=x` is a
/// common way to write one (curl and ldapsearch take it), but EDS wants the
/// bare host, and resolving the whole URL as a name fails as "Not
/// connected" (#307). Answers the host, and the encryption and port the
/// text named, if any.
pub(crate) fn parse_server(text: &str) -> (String, Option<Security>, Option<u16>) {
    let text = text.trim();
    let (security, rest) = match text.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("ldaps") => {
            (Some(Security::Ldaps), rest)
        }
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("ldap") => (None, rest),
        _ => (None, text),
    };
    let authority = rest.split(['/', '?']).next().unwrap_or("");
    // A bracketed IPv6 address keeps its colons.
    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        match inner.split_once(']') {
            Some((h, tail)) => (h, tail.strip_prefix(':')),
            None => (inner, None),
        }
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => (h, Some(p)),
            _ => (authority, None),
        }
    };
    let port = port.and_then(|p| p.parse::<u16>().ok()).filter(|p| *p != 0);
    (host.to_string(), security, port)
}

#[derive(Debug, Clone)]
pub(crate) struct Directory {
    /// The EDS source UID; empty for one not made yet.
    pub(crate) uid: String,
    pub(crate) name: String,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) security: Security,
    pub(crate) base_dn: String,
    /// Search the whole tree under the base, or only its first level.
    pub(crate) subtree: bool,
    /// Who to sign in as; empty searches anonymously.
    pub(crate) bind_dn: String,
    pub(crate) enabled: bool,
    /// The registry's object for the source, for writing and removing it.
    path: String,
    /// The source as EDS keeps it, so saving keeps the keys Hylki does not
    /// show (Evolution's own, such as the search filter and offline copy).
    data: String,
}

impl Directory {
    pub(crate) fn empty() -> Self {
        Directory {
            uid: String::new(),
            name: String::new(),
            host: String::new(),
            port: Security::StartTls.default_port(),
            security: Security::StartTls,
            base_dn: String::new(),
            subtree: true,
            bind_dn: String::new(),
            enabled: true,
            path: String::new(),
            data: String::new(),
        }
    }

    /// The name shown in lists: the given one, or the server.
    pub(crate) fn title(&self) -> String {
        if self.name.trim().is_empty() {
            self.host.clone()
        } else {
            self.name.clone()
        }
    }

    fn keyring_key(&self) -> String {
        format!("ldap:{}", self.uid)
    }

    fn from_source(uid: &str, path: &str, data: &str) -> Option<Self> {
        let kf = crate::desktop::platform::keyfile(data)?;
        let get = |group: &str, key: &str| {
            kf.string(group, key)
                .map(|s| s.to_string())
                .unwrap_or_default()
        };
        if get("Address Book", "BackendName") != "ldap" {
            return None;
        }
        // EDS keeps the encryption in the Security extension. Hylki used to
        // write it under a key EDS never reads, so a directory saved then
        // still holds what the user chose there (#307).
        let mut method = get("Security", "Method");
        if method.is_empty() {
            method = get("LDAP Backend", "SecurityMethod");
        }
        let security = Security::from_key(&method);
        Some(Directory {
            uid: uid.to_string(),
            name: get("Data Source", "DisplayName"),
            host: get("Authentication", "Host"),
            port: kf
                .integer("Authentication", "Port")
                .ok()
                .and_then(|p| u16::try_from(p).ok())
                .filter(|p| *p != 0)
                .unwrap_or(security.default_port()),
            security,
            base_dn: get("LDAP Backend", "RootDn"),
            subtree: get("LDAP Backend", "Scope") != "onelevel",
            bind_dn: if get("Authentication", "Method") == "none" {
                String::new()
            } else {
                get("Authentication", "User")
            },
            enabled: kf.boolean("Data Source", "Enabled").unwrap_or(true),
            path: path.to_string(),
            data: data.to_string(),
        })
    }

    /// Saved by Hylki before 1.44, with its encryption only under a key EDS
    /// never read, so EDS has been connecting without any (#307).
    fn wants_security_moved(&self) -> bool {
        crate::desktop::platform::keyfile(&self.data).is_some_and(|kf| {
            kf.has_key("LDAP Backend", "SecurityMethod")
                .unwrap_or(false)
                && !kf.has_key("Security", "Method").unwrap_or(false)
        })
    }

    /// The source key file for this directory, over what EDS had.
    fn to_source(&self) -> String {
        let kf = crate::desktop::platform::keyfile(&self.data).unwrap_or_default();
        kf.set_string("Data Source", "DisplayName", &self.title());
        kf.set_boolean("Data Source", "Enabled", self.enabled);
        if self.data.is_empty() {
            // Where Evolution files its LDAP books.
            kf.set_string("Data Source", "Parent", "ldap-stub");
            kf.set_boolean("Offline", "StaySynchronized", false);
            kf.set_integer("LDAP Backend", "Limit", 100);
            kf.set_boolean("LDAP Backend", "CanBrowse", false);
            kf.set_boolean("Authentication", "RememberPassword", true);
        }
        kf.set_string("Address Book", "BackendName", "ldap");
        kf.set_string("Authentication", "Host", &parse_server(&self.host).0);
        kf.set_integer("Authentication", "Port", i32::from(self.port));
        kf.set_string("Authentication", "User", self.bind_dn.trim());
        let method = kf
            .string("Authentication", "Method")
            .map(|s| s.to_string())
            .unwrap_or_default();
        let method = if self.bind_dn.trim().is_empty() {
            "none"
        } else if method == "ldap/simple-email" {
            // Evolution's sign-in by email address, which it resolves to
            // the entry itself: keep it.
            "ldap/simple-email"
        } else {
            "ldap/simple-binddn"
        };
        kf.set_string("Authentication", "Method", method);
        kf.set_string("LDAP Backend", "RootDn", self.base_dn.trim());
        kf.set_string(
            "LDAP Backend",
            "Scope",
            if self.subtree { "subtree" } else { "onelevel" },
        );
        kf.set_string("Security", "Method", self.security.key());
        let _ = kf.remove_key("LDAP Backend", "SecurityMethod");
        kf.to_data().to_string()
    }
}

/// Every LDAP directory EDS knows, Evolution's included, by name.
pub(crate) fn list() -> Result<Vec<Directory>, String> {
    let sources = crate::contacts::registry_sources().ok_or_else(not_available)?;
    let mut out: Vec<Directory> = sources
        .iter()
        .filter_map(|(path, uid, data)| Directory::from_source(uid, path, data))
        .collect();
    for dir in out.iter_mut().filter(|d| d.wants_security_moved()) {
        move_security(dir);
    }
    out.sort_by_key(|d| d.title().to_lowercase());
    Ok(out)
}

/// Make or update a directory; a new password replaces the kept one, none
/// keeps it. Answers the directory's UID.
pub(crate) fn save(dir: &Directory, password: Option<&str>) -> Result<String, String> {
    let conn = bus()?;
    let dest = registry()?;
    let mut dir = dir.clone();
    if dir.uid.is_empty() {
        dir.uid = new_source_uid();
        create_source(&conn, &dest, &dir.uid, dir.to_source())?;
    } else {
        write_source(&conn, &dest, &dir.path, dir.to_source())?;
    }
    if let Some(pw) = password.filter(|p| !p.is_empty()) {
        crate::config::store_cloud_password(&dir.keyring_key(), pw).map_err(|e| e.to_string())?;
    }
    // The open book signed in with the old settings, and what failed was
    // about those.
    forget_book(&dir.uid);
    note_answer(&dir.uid);
    list_changed();
    Ok(dir.uid)
}

/// Write a directory's encryption where EDS reads it, once.
fn move_security(dir: &mut Directory) {
    let written =
        registry().and_then(|dest| write_source(&bus()?, &dest, &dir.path, dir.to_source()));
    match written {
        Ok(_) => {
            tracing::info!(directory = %dir.title(), security = ?dir.security, "LDAP directory's encryption moved where EDS reads it");
            dir.data = dir.to_source();
            forget_book(&dir.uid);
        }
        Err(e) => {
            tracing::warn!(directory = %dir.title(), "LDAP directory's encryption could not be moved: {e}")
        }
    }
}

/// Switch a directory on or off without touching the rest of it.
pub(crate) fn set_enabled(dir: &Directory, enabled: bool) -> Result<(), String> {
    let mut d = dir.clone();
    d.enabled = enabled;
    save(&d, None).map(|_| ())
}

pub(crate) fn remove(dir: &Directory) -> Result<(), String> {
    let conn = bus()?;
    let dest = registry()?;
    remove_source(&conn, &dest, &dir.path)?;
    crate::config::delete_cloud_password(&dir.keyring_key());
    forget_book(&dir.uid);
    note_answer(&dir.uid);
    list_changed();
    Ok(())
}

/// Whether Hylki holds a password for the directory.
pub(crate) fn has_password(dir: &Directory) -> bool {
    crate::config::load_cloud_password(&dir.keyring_key()).is_some()
}

// ---------------------------------------------------------------------------
// Searching
// ---------------------------------------------------------------------------

/// The books this process has open, by source UID: (bus name, object path).
fn books() -> &'static Mutex<HashMap<String, (String, String)>> {
    static BOOKS: OnceLock<Mutex<HashMap<String, (String, String)>>> = OnceLock::new();
    BOOKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn forget_book(uid: &str) {
    let book = books().lock().ok().and_then(|mut b| b.remove(uid));
    if let (Some((bus_name, path)), Ok(conn)) = (book, bus()) {
        let _ = conn.call_method(
            Some(bus_name.as_str()),
            path.as_str(),
            Some(crate::contacts::BOOK_IFACE),
            "Close",
            &(),
        );
    }
}

fn open(conn: &zbus::blocking::Connection, uid: &str) -> Result<(String, String), String> {
    if let Some(b) = books().lock().ok().and_then(|b| b.get(uid).cloned()) {
        return Ok(b);
    }
    let dest = crate::contacts::factory_dest()
        .ok_or_else(|| i18n("Evolution Data Server is not available"))?;
    let book = crate::contacts::open_book(conn, &dest, uid)?;
    if let Ok(mut b) = books().lock() {
        b.insert(uid.to_string(), book.clone());
    }
    Ok(book)
}

fn contact_list(
    conn: &zbus::blocking::Connection,
    book: &(String, String),
    query: &str,
) -> Result<Vec<String>, String> {
    let reply = conn
        .call_method(
            Some(book.0.as_str()),
            book.1.as_str(),
            Some(crate::contacts::BOOK_IFACE),
            "GetContactList",
            &(query,),
        )
        .map_err(|e| e.to_string())?;
    let (cards,): (Vec<String>,) = reply.body().deserialize().map_err(|e| e.to_string())?;
    Ok(cards)
}

/// Why EDS last asked for the directory's sign-in, and what went wrong.
#[derive(Debug, Default)]
struct LastCredentials {
    /// "required", "rejected", "ssl_failed", "error", or empty once the
    /// directory is connected.
    reason: String,
    /// What the server or the TLS library said, when it said anything.
    message: String,
}

fn last_credentials(conn: &zbus::blocking::Connection, dir: &Directory) -> LastCredentials {
    let Ok(dest) = registry() else {
        return LastCredentials::default();
    };
    conn.call_method(
        Some(dest.as_str()),
        dir.path.as_str(),
        Some(SOURCE_IFACE),
        "GetLastCredentialsRequiredArguments",
        &(),
    )
    .ok()
    .and_then(|r| {
        r.body()
            .deserialize::<(String, String, String, String, String)>()
            .ok()
    })
    .map(
        |(reason, _cert_pem, _cert_errors, _error_name, message)| LastCredentials {
            reason,
            message,
        },
    )
    .unwrap_or_default()
}

/// The error worth showing for a directory that failed to connect: what
/// EDS recorded, which says why, over the bare "Not connected" the book
/// answers with (#307).
fn connect_error(dir: &Directory, last: &LastCredentials, fallback: String) -> String {
    let detail = last.message.trim();
    let server = format!("{}:{}", dir.host.trim(), dir.port);
    match last.reason.as_str() {
        // Still waiting for a password: neither Hylki nor the keyring EDS
        // reads has one for it.
        "required" if !dir.bind_dn.trim().is_empty() => needs_password(),
        "required" => i18n("The directory needs a sign-in. Add one in Settings → Contacts."),
        "rejected" => i18n("The directory did not accept the password."),
        "ssl_failed" if detail.is_empty() => i18n("The directory’s certificate could not be verified."),
        "ssl_failed" => i18n_f("The directory’s certificate could not be verified: {error}", &[("error", detail)]),
        // EDS's own words for a connection that never came up.
        _ if detail == "Repository offline" => match dir.security {
            Security::Ldaps => i18n_f(
                "Could not connect to {server} over TLS. Check the server, the port and that it takes LDAPS.",
                &[("server", &server)],
            ),
            _ => i18n_f("Could not connect to {server}. Check the server and the port.", &[("server", &server)]),
        },
        _ if detail == "TLS not available" => i18n_f(
            "Could not start StartTLS with {server}: the server does not offer it, or its certificate is not trusted.",
            &[("server", &server)],
        ),
        _ if !detail.is_empty() => detail.to_string(),
        _ => fallback,
    }
}

fn authenticate(
    conn: &zbus::blocking::Connection,
    dir: &Directory,
    password: &str,
) -> Result<(), String> {
    let dest = registry()?;
    invoke_authenticate(conn, &dest, &dir.path, &dir.bind_dn, password)
}

/// Ask one directory, signing in first when EDS says it wants that.
fn query_directory(dir: &Directory, query: &str) -> Result<Vec<String>, String> {
    let conn = bus()?;
    let book = match open(&conn, &dir.uid) {
        Ok(b) => b,
        Err(e) => {
            forget_book(&dir.uid);
            // EDS takes a server it cannot resolve or route to as offline.
            if e.contains("Offline unavailable") {
                let server = format!("{}:{}", dir.host.trim(), dir.port);
                return Err(i18n_f(
                    "Could not reach {server}. Check the server’s name and the network.",
                    &[("server", &server)],
                ));
            }
            return Err(e);
        }
    };
    let first = match contact_list(&conn, &book, query) {
        Ok(cards) => return Ok(cards),
        Err(e) => e,
    };
    // A book whose factory went away since: open it again, once.
    if first.contains("UnknownObject")
        || first.contains("ServiceUnknown")
        || first.contains("NoReply")
    {
        forget_book(&dir.uid);
        let book = open(&conn, &dir.uid)?;
        return contact_list(&conn, &book, query);
    }
    let last = last_credentials(&conn, dir);
    tracing::debug!(directory = %dir.title(), reason = %last.reason, message = %last.message, "LDAP directory not answering: {first}");
    let signs_in = !dir.bind_dn.trim().is_empty();
    let own = if signs_in {
        crate::config::load_cloud_password(&dir.keyring_key())
    } else {
        None
    };
    match (last.reason.as_str(), own) {
        // Wants the password, or failed to connect last time: sign in
        // again, which also makes the backend try the server again.
        ("required" | "rejected" | "ssl_failed" | "error", Some(pw)) => {
            authenticate(&conn, dir, &pw)?
        }
        // No password of Hylki's, as for a directory set up in Evolution:
        // "required" is what the backend records as it opens, and EDS then
        // signs in by itself with the password Evolution stored (#307).
        // Anything else is an answer already.
        ("required", None) => {}
        ("rejected" | "ssl_failed" | "error", None) => {
            return Err(connect_error(dir, &last, first))
        }
        _ => {}
    }
    wait_for_answer(&conn, dir, &book, query, last)
}

/// How long EDS gets to find the password it keeps for a directory and sign
/// in with it, before the directory is taken as one nobody has a password
/// for.
const EDS_SIGN_IN: Duration = Duration::from_secs(4);

/// Ask again until the directory answers, says why it will not, or runs out
/// of time. Signing in and connecting happen after the calls that start them
/// return, and a TLS handshake with a distant server takes a while. EDS keeps
/// the last reason it recorded, so only a new one ends the wait early; a
/// "required" that stays is a password nobody gave.
fn wait_for_answer(
    conn: &zbus::blocking::Connection,
    dir: &Directory,
    book: &(String, String),
    query: &str,
    before: LastCredentials,
) -> Result<Vec<String>, String> {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(12);
    loop {
        std::thread::sleep(Duration::from_millis(250));
        match contact_list(conn, book, query) {
            Ok(cards) => return Ok(cards),
            Err(e) => {
                let now = last_credentials(conn, dir);
                let failed = matches!(now.reason.as_str(), "rejected" | "ssl_failed" | "error")
                    && (now.reason.as_str(), now.message.as_str())
                        != (before.reason.as_str(), before.message.as_str());
                let unanswered = now.reason == "required" && started.elapsed() >= EDS_SIGN_IN;
                if failed || unanswered || Instant::now() >= deadline {
                    return Err(connect_error(dir, &now, e));
                }
            }
        }
    }
}

fn needs_password() -> String {
    i18n("The directory needs a password. Add it in Settings → Contacts.")
}

/// An EDS query string: a quoted, escaped literal.
fn sexp_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// How long the directories that are switched on are taken as known: the
/// composer asks at every pause in the typing, and a directory switched on
/// in Evolution should still show up without a restart.
const LIST_FRESH: Duration = Duration::from_secs(5);

/// How long a directory that failed is left alone before it is asked again:
/// a server that is down, or a sign-in nobody can give, would otherwise hold
/// up every pause in the typing.
const RETRY_AFTER: Duration = Duration::from_secs(30);

static LIST_STALE: AtomicBool = AtomicBool::new(false);

/// Hylki changed a directory: read the list again at the next lookup.
pub(crate) fn list_changed() {
    LIST_STALE.store(true, Ordering::Relaxed);
}

/// The directories that are switched on, read again once [`LIST_FRESH`] has
/// passed or Hylki changed one.
fn enabled() -> Vec<Directory> {
    static CACHE: Mutex<Option<(Instant, Vec<Directory>)>> = Mutex::new(None);
    let stale = LIST_STALE.swap(false, Ordering::Relaxed);
    if let Some((at, dirs)) = CACHE.lock().ok().and_then(|c| c.clone()) {
        if !stale && at.elapsed() < LIST_FRESH {
            return dirs;
        }
    }
    let off = crate::contacts::contact_sources::off();
    let dirs: Vec<Directory> = match list() {
        Ok(d) => d
            .into_iter()
            .filter(|d| d.enabled && !off.contains(&d.uid))
            .collect(),
        Err(e) => {
            tracing::debug!("LDAP directories unavailable: {e}");
            Vec::new()
        }
    };
    if let Ok(mut c) = CACHE.lock() {
        *c = Some((Instant::now(), dirs.clone()));
    }
    dirs
}

/// The directories that are switched on, as (UID, name), for the Contacts
/// view's address book filter. Blocking, but only a registry call at most
/// every [`LIST_FRESH`].
pub(crate) fn enabled_names() -> Vec<(String, String)> {
    enabled()
        .into_iter()
        .map(|d| (d.uid.clone(), d.title()))
        .collect()
}

/// A directory's last failure, kept until it answers again.
struct Failure {
    message: String,
    at: Instant,
}

fn failures() -> &'static Mutex<HashMap<String, Failure>> {
    static FAILURES: OnceLock<Mutex<HashMap<String, Failure>>> = OnceLock::new();
    FAILURES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Why the directory last failed to answer, until it answers again: for
/// Settings to show beside it.
pub(crate) fn last_error(uid: &str) -> Option<String> {
    failures()
        .lock()
        .ok()
        .and_then(|f| f.get(uid).map(|x| x.message.clone()))
}

fn note_failure(uid: &str, message: &str) {
    if let Ok(mut f) = failures().lock() {
        f.insert(
            uid.to_string(),
            Failure {
                message: message.to_string(),
                at: Instant::now(),
            },
        );
    }
}

fn note_answer(uid: &str) {
    if let Ok(mut f) = failures().lock() {
        f.remove(uid);
    }
}

/// Failed within [`RETRY_AFTER`]: not asked again yet. Answers the failure.
fn resting(uid: &str) -> Option<String> {
    let f = failures().lock().ok()?;
    f.get(uid)
        .filter(|x| x.at.elapsed() < RETRY_AFTER)
        .map(|x| x.message.clone())
}

/// One lookup at a time per directory: a later one waits for the sign-in or
/// the failure the earlier one is waiting on, rather than starting another.
fn directory_lock(uid: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    locks.entry(uid.to_string()).or_default().clone()
}

/// A directory that did not answer, and why, worded for the status bar.
#[derive(Debug, Clone)]
pub(crate) struct DirectoryFailure {
    pub(crate) uid: String,
    pub(crate) name: String,
    pub(crate) message: String,
}

impl DirectoryFailure {
    pub(crate) fn text(&self) -> String {
        i18n_f(
            "{name} did not answer: {error}",
            &[("name", &self.name), ("error", &self.message)],
        )
    }
}

/// What the directories that are switched on hold for `typed`: the vCards,
/// by directory, and the directories that failed. One that failed a moment
/// ago is not asked again yet but is still among the failures, so whoever
/// searches learns why nothing came of it, not only the first to ask (#307).
/// Blocking: call it off the main thread.
fn lookup(typed: &str) -> (Vec<(Directory, Vec<String>)>, Vec<DirectoryFailure>) {
    let typed = typed.trim();
    if typed.chars().count() < MIN_QUERY || typed.len() > 128 {
        return (Vec::new(), Vec::new());
    }
    let mut failed = Vec::new();
    let mut dirs = Vec::new();
    for dir in enabled() {
        match resting(&dir.uid) {
            Some(message) => {
                tracing::debug!(directory = %dir.title(), "LDAP directory not asked, it failed less than {}s ago: {message}", RETRY_AFTER.as_secs());
                failed.push(DirectoryFailure {
                    uid: dir.uid.clone(),
                    name: dir.title(),
                    message,
                });
            }
            None => dirs.push(dir),
        }
    }
    if dirs.is_empty() {
        return (Vec::new(), failed);
    }
    let q = sexp_str(typed);
    // EDS maps these onto the directory's attributes: cn and sn for the
    // name, mail for the address. Name and address are matched anywhere,
    // as Evolution's own search does and as Contacts and mail history are
    // here, so a surname finds "Name Surname" and name.surname@ when the
    // server keeps no sn of its own (#307).
    let query = format!(
        "(or (contains \"full_name\" {q}) (contains \"email\" {q}) (beginswith \"full_name\" {q}) (beginswith \"nickname\" {q}) (beginswith \"file_as\" {q}))"
    );
    // Each directory on its own thread: one that is slow to sign in does
    // not hold up the others.
    let answers: Vec<(Directory, Result<Vec<String>, String>, Duration)> =
        std::thread::scope(|scope| {
            let query = &query;
            let asked: Vec<_> = dirs
                .into_iter()
                .map(|dir| {
                    scope.spawn(move || {
                        let lock = directory_lock(&dir.uid);
                        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
                        let started = Instant::now();
                        // The lookup this one waited behind may have just failed.
                        if let Some(message) = resting(&dir.uid) {
                            return (dir, Err(message), started.elapsed());
                        }
                        let answer = query_directory(&dir, query);
                        if let Err(e) = &answer {
                            note_failure(&dir.uid, e);
                        }
                        (dir, answer, started.elapsed())
                    })
                })
                .collect();
            asked.into_iter().filter_map(|h| h.join().ok()).collect()
        });
    let mut found = Vec::new();
    for (dir, answer, took) in answers {
        match answer {
            Ok(cards) => {
                tracing::debug!(
                    directory = %dir.title(),
                    typed,
                    entries = cards.len(),
                    with_address = cards.iter().filter(|c| !suggestions_from_vcard(c).is_empty()).count(),
                    ms = took.as_millis() as u64,
                    "LDAP directory answered"
                );
                note_answer(&dir.uid);
                found.push((dir, cards));
            }
            Err(e) => {
                tracing::warn!(directory = %dir.title(), typed, ms = took.as_millis() as u64, "LDAP directory lookup failed: {e}");
                failed.push(DirectoryFailure {
                    uid: dir.uid.clone(),
                    name: dir.title(),
                    message: e,
                });
            }
        }
    }
    (found, failed)
}

/// The people whose name or address holds `typed`, or whose surname starts
/// with it, from every directory that is switched on, for the composer; and the
/// directories that failed. Blocking: call it off the main thread.
pub(crate) fn search(typed: &str) -> (Vec<Suggestion>, Vec<DirectoryFailure>) {
    let (found, failed) = lookup(typed);
    let suggestions = found
        .iter()
        .flat_map(|(_, cards)| cards.iter().flat_map(|c| suggestions_from_vcard(c)))
        .collect();
    (suggestions, failed)
}

/// The same people as whole entries, for the Contacts view, at most `limit`
/// from each directory, each naming the directory it came from as its book.
pub(crate) fn search_contacts(
    typed: &str,
    limit: usize,
) -> (Vec<crate::contacts::ContactDetails>, Vec<DirectoryFailure>) {
    let (found, failed) = lookup(typed);
    let mut out = Vec::new();
    for (dir, cards) in found {
        let mut entries = Vec::new();
        for card in cards {
            let Some(mut c) = crate::contacts::parse_vcard_fields(&card).filter(|c| !c.is_group())
            else {
                continue;
            };
            // A directory's photo is carried in the card; nothing on disk is
            // read for it.
            c.photo = crate::contacts::vcard_photo_with(&card, false);
            c.book_uid = dir.uid.clone();
            c.book_name = dir.title();
            // Not an entry Hylki can change or delete.
            c.eds_uid = String::new();
            c.raw_vcard = card;
            entries.push(c);
        }
        entries.sort_by_key(|c| c.name.to_lowercase());
        entries.truncate(limit);
        out.extend(entries);
    }
    out.sort_by_key(|c| c.name.to_lowercase());
    (out, failed)
}

/// Check a saved directory answers, signing in as it would for a search.
pub(crate) fn check(dir: &Directory) -> Result<(), String> {
    let r = query_directory(dir, "(is \"email\" \"hylki-check@invalid\")").map(|_| ());
    match &r {
        Ok(()) => {
            note_answer(&dir.uid);
            tracing::info!(directory = %dir.title(), host = %dir.host, port = dir.port, security = ?dir.security, "LDAP directory connected");
        }
        Err(e) => {
            note_failure(&dir.uid, e);
            tracing::warn!(directory = %dir.title(), host = %dir.host, port = dir.port, security = ?dir.security, "LDAP directory check failed: {e}");
        }
    }
    r
}

/// A suggestion per address on a directory entry.
fn suggestions_from_vcard(card: &str) -> Vec<Suggestion> {
    let unfolded = crate::contacts::unfold_vcard(card);
    let mut name = String::new();
    let mut emails = Vec::new();
    for line in unfolded.lines() {
        let Some((prop, value)) = crate::contacts::split_vcard_line(line) else {
            continue;
        };
        let prop = prop.split(';').next().unwrap_or("").trim();
        let value = crate::contacts::unescape_vcard_text(value)
            .trim()
            .to_string();
        if prop.eq_ignore_ascii_case("FN") && name.is_empty() {
            name = value;
        } else if prop.eq_ignore_ascii_case("EMAIL")
            && value.contains('@')
            && !value.contains(['\r', '\n'])
        {
            emails.push(value);
        }
    }
    emails
        .into_iter()
        .map(|email| Suggestion {
            name: name.clone(),
            email,
            from_contacts: true,
            from_directory: true,
            ..Default::default()
        })
        .collect()
}

#[cfg(test)]
mod tests;
