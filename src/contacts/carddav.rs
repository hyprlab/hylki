//! CardDAV accounts set up in Hylki (#408), kept by Evolution Data Server.
//!
//! An account is an EDS collection source with the `webdav` backend, the
//! kind Evolution's Collection Account makes. EDS asks the server which
//! address books the account has, adds a book for each and keeps them in
//! step, so they read like any other EDS book and show in Evolution and
//! GNOME Contacts too. Hylki carries no CardDAV client of its own.
//!
//! The password goes in the keyring in the shape EDS keeps its own in, by
//! the account's UID. EDS then signs in by itself, on any desktop and while
//! Hylki is not running, and nobody has to answer a prompt.
//!
//! EDS keeps a book in step only while some app has it open. On GNOME,
//! GNOME Contacts does that; elsewhere nothing would, so Hylki opens the
//! books that sync with a server for as long as it runs ([`sync_books`]).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::i18n::{i18n, i18n_f};

use crate::contacts::directory::eds_source::{
    bus, create_source, invoke_authenticate, new_source_uid, not_available, registry,
    remove_source, write_source, SOURCE_IFACE,
};

#[derive(Debug, Clone)]
pub(crate) struct Account {
    /// The EDS source UID; empty for one not made yet.
    pub(crate) uid: String,
    pub(crate) name: String,
    /// The server as given: an address EDS starts looking for the address
    /// books from (`https://cloud.example.org`, or a full CardDAV URL).
    pub(crate) url: String,
    pub(crate) user: String,
    pub(crate) enabled: bool,
    /// The address books EDS found on the server, by name.
    pub(crate) books: Vec<String>,
    /// Whether the account's calendars are kept too (CalDAV, #715). EDS
    /// finds them on the same server, or on the service's calendar server.
    pub(crate) calendars: bool,
    /// The registry's object for the source, for writing and removing it.
    path: String,
    /// The source as EDS keeps it, so saving keeps the keys Hylki does not
    /// show (an Evolution-made account's calendars, say).
    data: String,
}

impl Account {
    pub(crate) fn empty() -> Self {
        Account {
            uid: String::new(),
            name: String::new(),
            url: String::new(),
            user: String::new(),
            enabled: true,
            books: Vec::new(),
            calendars: true,
            path: String::new(),
            data: String::new(),
        }
    }

    /// The name shown in lists: the given one, or the user at the server.
    pub(crate) fn title(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_string();
        }
        let host = parse_url(&self.url)
            .map(|u| u.host)
            .unwrap_or_else(|_| self.url.clone());
        if self.user.trim().is_empty() {
            host
        } else {
            format!("{}@{host}", self.user.trim())
        }
    }

    fn from_source(uid: &str, path: &str, data: &str, books: Vec<String>) -> Option<Self> {
        let kf = crate::desktop::platform::keyfile(data)?;
        let get = |group: &str, key: &str| {
            kf.string(group, key)
                .map(|s| s.to_string())
                .unwrap_or_default()
        };
        // A WebDAV collection, but not one GNOME Online Accounts keeps:
        // that is changed in Online Accounts.
        if get("Collection", "BackendName") != "webdav" || kf.has_group("GNOME Online Accounts") {
            return None;
        }
        Some(Account {
            uid: uid.to_string(),
            name: get("Data Source", "DisplayName"),
            url: get("Collection", "ContactsUrl"),
            user: get("Authentication", "User"),
            enabled: kf.boolean("Data Source", "Enabled").unwrap_or(true),
            books,
            calendars: kf.boolean("Collection", "CalendarEnabled").unwrap_or(false),
            path: path.to_string(),
            data: data.to_string(),
        })
    }

    /// The source key file for this account, over what EDS had.
    fn to_source(&self, url: &ServerUrl) -> String {
        let kf = crate::desktop::platform::keyfile(&self.data).unwrap_or_default();
        kf.set_string("Data Source", "DisplayName", &self.title());
        kf.set_boolean("Data Source", "Enabled", self.enabled);
        if self.data.is_empty() {
            kf.set_string("Data Source", "Parent", "");
            // Mail is another account's.
            kf.set_boolean("Collection", "MailEnabled", false);
            kf.set_boolean("Authentication", "RememberPassword", true);
        }
        kf.set_boolean("Collection", "CalendarEnabled", self.calendars);
        let calendar_url = kf
            .string("Collection", "CalendarUrl")
            .map(|s| s.to_string())
            .unwrap_or_default();
        if self.calendars && calendar_url.is_empty() {
            kf.set_string("Collection", "CalendarUrl", &calendar_server(&url.url));
        } else if !self.calendars && self.data.is_empty() {
            kf.set_string("Collection", "CalendarUrl", "");
        }
        kf.set_string("Collection", "BackendName", "webdav");
        kf.set_boolean("Collection", "ContactsEnabled", true);
        kf.set_string("Collection", "Identity", self.user.trim());
        kf.set_string("Collection", "ContactsUrl", &url.url);
        kf.set_string("Authentication", "Host", &url.host);
        kf.set_integer("Authentication", "Port", i32::from(url.port));
        kf.set_string("Authentication", "User", self.user.trim());
        kf.set_string("Authentication", "Method", "plain/password");
        kf.set_string("Security", "Method", if url.tls { "tls" } else { "none" });
        kf.to_data().to_string()
    }
}

/// Where a service keeps its calendars, from where it keeps its address
/// books: a server of their own for the big services, the same server
/// (found through its `.well-known/caldav`) for the rest.
pub(crate) fn calendar_server(contacts: &str) -> String {
    const SERVERS: [(&str, &str); 3] = [
        ("contacts.icloud.com", "https://caldav.icloud.com"),
        ("carddav.fastmail.com", "https://caldav.fastmail.com"),
        ("carddav.address.yahoo.com", "https://caldav.calendar.yahoo.com"),
    ];
    SERVERS
        .iter()
        .find(|(c, _)| contacts.contains(c))
        .map_or_else(|| contacts.to_string(), |(_, cal)| cal.to_string())
}

/// A server address taken apart the way EDS keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServerUrl {
    /// The whole address, with its scheme.
    url: String,
    pub(crate) host: String,
    port: u16,
    tls: bool,
}

/// Read what was typed as the server. A bare name is taken as HTTPS, which
/// every hosted CardDAV service speaks.
pub(crate) fn parse_url(text: &str) -> Result<ServerUrl, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(i18n("Enter the server’s address."));
    }
    let url = if text.contains("://") {
        text.to_string()
    } else {
        format!("https://{text}")
    };
    let uri = gtk::glib::Uri::parse(&url, gtk::glib::UriFlags::NONE)
        .map_err(|_| i18n_f("“{text}” is not a server address.", &[("text", text)]))?;
    let tls = match uri.scheme().to_ascii_lowercase().as_str() {
        "https" => true,
        "http" => false,
        _ => return Err(i18n("The address must start with https:// or http://.")),
    };
    let host = uri.host().map(|h| h.to_string()).unwrap_or_default();
    if host.is_empty() {
        return Err(i18n_f(
            "“{text}” is not a server address.",
            &[("text", text)],
        ));
    }
    let port = u16::try_from(uri.port())
        .ok()
        .filter(|p| *p != 0)
        .unwrap_or(if tls { 443 } else { 80 });
    Ok(ServerUrl {
        url,
        host,
        port,
        tls,
    })
}

/// Every CardDAV account EDS knows that is not GNOME Online Accounts', with
/// the address books found for each, Evolution's own accounts included.
pub(crate) fn list() -> Result<Vec<Account>, String> {
    let sources = crate::contacts::registry_sources().ok_or_else(not_available)?;
    let mut books: HashMap<String, Vec<String>> = HashMap::new();
    for (_, _, data) in &sources {
        let Some(kf) = crate::desktop::platform::keyfile(data) else {
            continue;
        };
        if !kf.has_group("Address Book") {
            continue;
        }
        let parent = kf
            .string("Data Source", "Parent")
            .map(|s| s.to_string())
            .unwrap_or_default();
        let name = kf
            .string("Data Source", "DisplayName")
            .map(|s| s.to_string())
            .unwrap_or_default();
        if !parent.is_empty() {
            books.entry(parent).or_default().push(name);
        }
    }
    let mut out: Vec<Account> = sources
        .iter()
        .filter_map(|(path, uid, data)| {
            let mut found = books.get(uid).cloned().unwrap_or_default();
            found.sort_by_key(|b| b.to_lowercase());
            Account::from_source(uid, path, data, found)
        })
        .collect();
    out.sort_by_key(|a| a.title().to_lowercase());
    Ok(out)
}

/// Make or update an account; a new password replaces the kept one, none
/// keeps it. Answers the account's UID.
pub(crate) fn save(account: &Account, password: Option<&str>) -> Result<String, String> {
    let url = parse_url(&account.url)?;
    let conn = bus()?;
    let dest = registry()?;
    let mut account = account.clone();
    let password = password.filter(|p| !p.is_empty());
    if account.uid.is_empty() {
        account.uid = new_source_uid();
        // In the keyring before the source exists, so EDS finds it the
        // moment it first looks.
        if let Some(pw) = password {
            crate::config::store_eds_password(&account.uid, &account.title(), pw)
                .map_err(|e| e.to_string())?;
        }
        create_source(&conn, &dest, &account.uid, account.to_source(&url))?;
    } else {
        write_source(&conn, &dest, &account.path, account.to_source(&url))?;
        if let Some(pw) = password {
            crate::config::store_eds_password(&account.uid, &account.title(), pw)
                .map_err(|e| e.to_string())?;
            // A source that gave up on the old password tries the new one
            // now rather than at its next look.
            let _ = invoke_authenticate(&conn, &dest, &account.path, &account.user, pw);
        }
    }
    Ok(account.uid)
}

/// Switch an account on or off without touching the rest of it.
pub(crate) fn set_enabled(account: &Account, enabled: bool) -> Result<(), String> {
    let mut a = account.clone();
    a.enabled = enabled;
    save(&a, None).map(|_| ())
}

/// Remove an account and its address books from EDS. Nothing on the server
/// changes.
pub(crate) fn remove(account: &Account) -> Result<(), String> {
    let conn = bus()?;
    let dest = registry()?;
    remove_source(&conn, &dest, &account.path)?;
    crate::config::delete_eds_password(&account.uid);
    Ok(())
}

/// Whether the keyring holds the account's password.
pub(crate) fn has_password(account: &Account) -> bool {
    crate::config::has_eds_password(&account.uid)
}

/// How an account is doing, for its row in Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Status {
    /// Signed in; the address books are those in [`Account::books`].
    Connected,
    /// EDS is still signing in, or looking for the address books.
    Connecting,
    NeedsPassword,
    Rejected,
    Failed(String),
}

impl Status {
    /// What went wrong, in words, or `None` when nothing did.
    pub(crate) fn problem(&self) -> Option<String> {
        match self {
            Status::Connected | Status::Connecting => None,
            Status::NeedsPassword => Some(i18n("Enter the password to sign in.")),
            Status::Rejected => Some(i18n("The server did not accept the user name or password.")),
            Status::Failed(e) => Some(e.clone()),
        }
    }
}

/// Ask EDS how the account's sign-in went. Blocking: it asks the keyring too.
pub(crate) fn status(account: &Account) -> Status {
    let (Ok(conn), Ok(dest)) = (bus(), registry()) else {
        return Status::Failed(not_available());
    };
    let last = conn
        .call_method(
            Some(dest.as_str()),
            account.path.as_str(),
            Some(SOURCE_IFACE),
            "GetLastCredentialsRequiredArguments",
            &(),
        )
        .ok()
        .and_then(|r| {
            r.body()
                .deserialize::<(String, String, String, String, String)>()
                .ok()
        });
    let connection =
        zbus::blocking::Proxy::new(&conn, dest.as_str(), account.path.as_str(), SOURCE_IFACE)
            .ok()
            .and_then(|p| p.get_property::<String>("ConnectionStatus").ok())
            .unwrap_or_default();
    let (reason, message) = last.map(|(r, _, _, _, m)| (r, m)).unwrap_or_default();
    match reason.as_str() {
        "rejected" => Status::Rejected,
        // Asked as the source starts, before EDS has read the keyring: a
        // password kept there is on its way.
        "required" if crate::config::has_eds_password(&account.uid) => Status::Connecting,
        "required" => Status::NeedsPassword,
        "ssl_failed" if message.trim().is_empty() => {
            Status::Failed(i18n("The server’s certificate could not be verified."))
        }
        "ssl_failed" => Status::Failed(i18n_f(
            "The server’s certificate could not be verified: {error}",
            &[("error", message.trim())],
        )),
        "error" if message.trim().is_empty() => {
            Status::Failed(i18n("Could not connect to the server."))
        }
        "error" => Status::Failed(message.trim().to_string()),
        _ if connection == "connecting" || account.books.is_empty() => Status::Connecting,
        _ => Status::Connected,
    }
}

// ---------------------------------------------------------------------------
// Keeping the books in step
// ---------------------------------------------------------------------------

/// The address books Hylki holds open, by source UID: when each was last
/// asked to sync.
fn opened() -> &'static Mutex<HashMap<String, Instant>> {
    static OPENED: std::sync::OnceLock<Mutex<HashMap<String, Instant>>> =
        std::sync::OnceLock::new();
    OPENED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A book synced this recently is not asked again unless forced.
const SYNC_AFTER: Duration = Duration::from_secs(5 * 60);

/// The enabled address books that sync with a server: those of CardDAV,
/// GNOME Online Accounts and Microsoft 365, not the local ones, which have
/// nothing to fetch, nor LDAP directories, which are asked as one types.
fn server_books() -> Vec<String> {
    let Some(sources) = crate::contacts::registry_sources() else {
        return Vec::new();
    };
    let by_uid: HashMap<String, String> = sources
        .iter()
        .map(|(_, uid, data)| (uid.clone(), data.clone()))
        .collect();
    let off = crate::contacts::contact_sources::off();
    let disabled: HashSet<String> = sources
        .iter()
        .filter(|(_, _, data)| {
            crate::desktop::platform::keyfile(data)
                .is_some_and(|kf| kf.boolean("Data Source", "Enabled").ok() == Some(false))
        })
        .map(|(_, uid, _)| uid.clone())
        .collect();
    sources
        .iter()
        .filter_map(|(_, uid, data)| {
            let kf = crate::desktop::platform::keyfile(data)?;
            let backend = kf.string("Address Book", "BackendName").ok()?.to_string();
            let parent = kf
                .string("Data Source", "Parent")
                .map(|s| s.to_string())
                .unwrap_or_default();
            let wanted = !matches!(backend.as_str(), "local" | "ldap" | "")
                && !disabled.contains(uid)
                && !disabled.contains(&parent)
                && crate::contacts::contact_sources::book_used(uid, &by_uid, &off);
            wanted.then(|| uid.clone())
        })
        .collect()
}

/// Open every address book that syncs with a server, and ask those not
/// synced in the last few minutes (all of them, with `force`) to fetch what
/// changed. Blocking: run it off the main thread. Answers whether any book
/// was synced, so the caller can read the contacts again.
pub(crate) fn sync_books(force: bool) -> bool {
    let (Ok(conn), Some(factory)) = (bus(), crate::contacts::factory_dest()) else {
        return false;
    };
    let mut synced = false;
    for uid in server_books() {
        let due = opened().lock().ok().is_none_or(|o| {
            o.get(&uid)
                .is_none_or(|t| force || t.elapsed() >= SYNC_AFTER)
        });
        if !due {
            continue;
        }
        // The book stays open on Hylki's connection, and EDS keeps it in
        // step with the server while it is.
        let (bus_name, path) = match crate::contacts::open_book(&conn, &factory, &uid) {
            Ok(book) => book,
            Err(e) => {
                tracing::debug!(book = %uid, "address book did not open: {e}");
                continue;
            }
        };
        if let Ok(mut o) = opened().lock() {
            o.insert(uid.clone(), Instant::now());
        }
        match conn.call_method(
            Some(bus_name.as_str()),
            path.as_str(),
            Some(crate::contacts::BOOK_IFACE),
            "Refresh",
            &(),
        ) {
            Ok(_) => synced = true,
            Err(e) => tracing::debug!(book = %uid, "address book did not sync: {e}"),
        }
    }
    synced
}

/// How long EDS gets to find the password in the keyring and sign in.
const SIGN_IN_GRACE: Duration = Duration::from_secs(6);

/// Wait for a saved account's address books to show up in EDS, up to
/// `limit`, then sync them. Answers the account as it stands then. With
/// `signing_in`, a new password is being tried, so the books found with the
/// old one prove nothing until EDS has had its time to sign in.
pub(crate) fn await_books(uid: &str, limit: Duration, signing_in: bool) -> Option<Account> {
    let started = Instant::now();
    loop {
        let account = list()
            .ok()
            .and_then(|l| l.into_iter().find(|a| a.uid == uid));
        let grace_over = started.elapsed() >= SIGN_IN_GRACE;
        // A source asks for its password as it starts, and EDS answers from
        // the keyring a moment later: only a request that stays counts.
        let settled = account.as_ref().is_some_and(|a| match status(a) {
            Status::Rejected | Status::Failed(_) => true,
            Status::NeedsPassword => grace_over,
            Status::Connected | Status::Connecting => {
                !a.books.is_empty() && (!signing_in || grace_over)
            }
        });
        if settled || started.elapsed() >= limit {
            if account.as_ref().is_some_and(|a| !a.books.is_empty()) {
                sync_books(true);
            }
            return account;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_url, Account};

    #[test]
    fn a_bare_server_name_is_taken_as_https() {
        let u = parse_url(" cloud.example.org ").unwrap();
        assert_eq!(
            (u.url.as_str(), u.host.as_str(), u.port, u.tls),
            ("https://cloud.example.org", "cloud.example.org", 443, true)
        );
        let u = parse_url("http://127.0.0.1:5232/dav/").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.tls), ("127.0.0.1", 5232, false));
        assert!(parse_url("").is_err());
        assert!(parse_url("ftp://x.org").is_err());
    }

    #[test]
    fn an_account_is_a_webdav_collection_not_kept_by_online_accounts() {
        let own =
            "[Data Source]\nDisplayName=Work\nEnabled=true\n\n[Collection]\nBackendName=webdav\n\
                   ContactsUrl=https://dav.example.org/\n\n[Authentication]\nUser=ann\n";
        let a = Account::from_source("u1", "/p", own, vec!["Team".into()]).unwrap();
        assert_eq!(
            (a.title().as_str(), a.url.as_str(), a.user.as_str()),
            ("Work", "https://dav.example.org/", "ann")
        );
        let goa = format!("{own}\n[GNOME Online Accounts]\nAccountId=account_1\n");
        assert!(Account::from_source("u2", "/p", &goa, Vec::new()).is_none());
        let google = own.replace("BackendName=webdav", "BackendName=google");
        assert!(Account::from_source("u3", "/p", &google, Vec::new()).is_none());
    }

    #[test]
    fn a_new_account_asks_for_address_books_and_calendars() {
        let mut a = Account::empty();
        a.url = "dav.example.org".into();
        a.user = "ann".into();
        let data = a.to_source(&parse_url(&a.url).unwrap());
        let kf = crate::desktop::platform::keyfile(&data).unwrap();
        assert_eq!(kf.string("Collection", "BackendName").unwrap(), "webdav");
        assert!(kf.boolean("Collection", "ContactsEnabled").unwrap());
        assert!(kf.boolean("Collection", "CalendarEnabled").unwrap());
        assert_eq!(
            kf.string("Collection", "CalendarUrl").unwrap(),
            "https://dav.example.org"
        );
        a.calendars = false;
        let data = a.to_source(&parse_url(&a.url).unwrap());
        let kf = crate::desktop::platform::keyfile(&data).unwrap();
        assert!(!kf.boolean("Collection", "CalendarEnabled").unwrap());
        assert_eq!(super::calendar_server("https://contacts.icloud.com"), "https://caldav.icloud.com");
        assert!(!kf.boolean("Collection", "MailEnabled").unwrap());
        assert_eq!(
            kf.string("Collection", "ContactsUrl").unwrap(),
            "https://dav.example.org"
        );
        assert_eq!(kf.string("Security", "Method").unwrap(), "tls");
        assert_eq!(kf.integer("Authentication", "Port").unwrap(), 443);
        assert_eq!(
            kf.string("Data Source", "DisplayName").unwrap(),
            "ann@dav.example.org"
        );
    }

    /// The key files written for EDS, pinned: a new account, and an
    /// existing one rewritten over what EDS had.
    #[test]
    fn the_source_key_files_as_written() {
        use crate::config::golden::assert_golden;
        let mut a = Account::empty();
        a.url = "dav.example.org".into();
        a.user = " ann ".into();
        let new = a.to_source(&parse_url(&a.url).unwrap());
        assert_golden("contacts/carddav/testdata/source-new.ini", &new);
        a.data = "[Data Source]\nDisplayName=Old\nEnabled=true\nParent=\n\n\
                  [Collection]\nBackendName=webdav\nCalendarEnabled=true\nX-Kept=1\n"
            .into();
        a.name = "Work".into();
        a.enabled = false;
        let again = a.to_source(&parse_url("http://127.0.0.1:5232/dav/").unwrap());
        assert_golden("contacts/carddav/testdata/source-rewritten.ini", &again);
    }

    /// Against a real server, in a session with EDS and a keyring:
    /// `CARDDAV_LIVE='https://dav.example.org/|user|password'`. Sets the
    /// account up, waits for its books, syncs and lists the contacts, then
    /// tries a wrong password and removes the account (`CARDDAV_KEEP=1`
    /// keeps it). Run it on a sealed session bus with a keyring of its own
    /// (see the EDS rig), never the desktop's.
    #[test]
    #[ignore]
    fn live() {
        let Ok(spec) = std::env::var("CARDDAV_LIVE") else {
            return;
        };
        let mut f = spec.splitn(3, '|').map(str::to_string);
        let (url, user, pw) = (f.next().unwrap(), f.next().unwrap(), f.next().unwrap());
        let mut a = Account::empty();
        a.url = url;
        a.user = user;
        let uid = super::save(&a, Some(&pw)).unwrap();
        let got = super::await_books(&uid, std::time::Duration::from_secs(30), true).unwrap();
        println!(
            "books {:?}, {:?}, password kept: {}",
            got.books,
            super::status(&got),
            super::has_password(&got)
        );
        println!("synced: {}", super::sync_books(true));
        std::thread::sleep(std::time::Duration::from_secs(2));
        for c in crate::contacts::read_contact_details() {
            println!("  {} ({})", c.name, c.book_name);
        }
        if std::env::var_os("CARDDAV_KEEP").is_some() {
            return;
        }
        super::save(&got, Some("not the password")).unwrap();
        let again = super::await_books(&uid, std::time::Duration::from_secs(30), true).unwrap();
        println!("wrong password: {:?}", super::status(&again));
        assert_eq!(super::status(&again), super::Status::Rejected);
        super::remove(&again).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert!(super::list().unwrap().iter().all(|x| x.uid != uid));
        assert!(!super::has_password(&again));
    }
}
