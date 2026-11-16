//! The EDS source registry: which books are live, what they and their
//! accounts are called, and how each keeps its groups.

use std::collections::HashMap;

use super::details::GroupFormat;
use super::eds_write::{dest_from_service_files, remembered};
use super::read::source_display_name;
use crate::i18n::i18n;

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
pub(super) fn registry_books() -> Option<HashMap<String, String>> {
    Some(books_from(&source_map()?))
}

/// How each book keeps a new group, by book UID.
pub(super) type BookFormats = HashMap<String, Option<GroupFormat>>;

/// [`registry_books`], and how each book keeps a new group
/// ([`formats_from`]), from one call to the registry.
pub(super) fn registry_books_and_formats() -> (Option<HashMap<String, String>>, BookFormats) {
    match source_map() {
        Some(sources) => (Some(books_from(&sources)), formats_from(&sources)),
        None => (None, HashMap::new()),
    }
}

/// The registry's sources by UID.
fn source_map() -> Option<HashMap<String, String>> {
    Some(
        registry_sources()?
            .into_iter()
            .map(|(_, uid, data)| (uid, data))
            .collect(),
    )
}

fn books_from(sources: &HashMap<String, String>) -> HashMap<String, String> {
    let mut books = HashMap::new();
    let off = crate::contacts::contact_sources::off();
    let names = crate::contacts::contact_sources::names();
    for (uid, data) in sources {
        // An address book, and not switched off (GOA sets the book source's
        // [Data Source] Enabled=false when Contacts is toggled off for the
        // account). Only that section's Enabled counts — other sections
        // (Refresh etc.) have Enabled keys of their own. Nor switched off
        // for Hylki in Settings → Contacts, it or its account.
        if data.contains("[Address Book]")
            && !data_source_disabled(data)
            && crate::contacts::contact_sources::book_used(uid, sources, &off)
        {
            let name = book_display_name(data, sources);
            let account = names.get(&crate::contacts::contact_sources::group_of(uid, sources));
            books.insert(
                uid.clone(),
                with_given_name(&with_account_name(&name, account), names.get(uid)),
            );
        }
    }
    books
}

/// Every source the EDS registry holds: (object path, UID, key file).
/// `None` when the registry can't be reached.
pub(crate) fn registry_sources() -> Option<Vec<(String, String, String)>> {
    use gtk::gio;

    let dest = sources_dest()?;
    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;
    // zbus 4's blocking call has no deadline. A deadlocked source registry
    // used to strand every composer lookup forever. GIO bounds the actual
    // request, so the worker can fall back to cached books and mail history
    // instead of leaving another blocked thread behind on every open.
    let reply = match conn.call_sync(
        Some(dest.as_str()),
        "/org/gnome/evolution/dataserver/SourceManager",
        "org.freedesktop.DBus.ObjectManager",
        "GetManagedObjects",
        None,
        None,
        gio::DBusCallFlags::NONE,
        3000,
        gio::Cancellable::NONE,
    ) {
        Ok(reply) => reply,
        Err(e) => {
            tracing::warn!("EDS source registry unavailable: {e}");
            return None;
        }
    };
    sources_from_reply(&reply)
}

pub(super) fn sources_from_reply(
    reply: &gtk::glib::Variant,
) -> Option<Vec<(String, String, String)>> {
    type Props = HashMap<String, gtk::glib::Variant>;
    type Objects = HashMap<gtk::glib::variant::ObjectPath, HashMap<String, Props>>;
    let (objects,): (Objects,) = reply.get()?;
    let mut sources = Vec::new();
    for (path, ifaces) in &objects {
        let Some(props) = ifaces.get("org.gnome.evolution.dataserver.Source") else {
            continue;
        };
        let uid = props.get("UID").and_then(|v| v.str()).map(str::to_string);
        let data = props.get("Data").and_then(|v| v.str()).map(str::to_string);
        if let (Some(uid), Some(data)) = (uid, data) {
            sources.push((path.to_string(), uid, data));
        }
    }
    Some(sources)
}

/// A book's friendly name: which account it belongs to and over what — walked
/// up the source's parent chain to the account collection, whose backend
/// (google / microsoft365 / webdav) and identity say it best.
/// A book's name with the one given to its account in Hylki, if any, in
/// place of the account's own ("Work · Family" for "CardDAV — a@b.org ·
/// Family"); a book that is its account alone keeps its address as its own
/// name ("Work · a@b.org").
pub(super) fn with_account_name(name: &str, account: Option<&String>) -> String {
    match (account, name.split_once(" · "), name.split_once(" — ")) {
        (None, _, _) => name.to_string(),
        (Some(account), Some((_, own)), _) => format!("{account} · {own}"),
        (Some(account), None, Some((_, own))) => format!("{account} · {own}"),
        (Some(account), None, None) => account.clone(),
    }
}

/// A book's name with the one given to it in Hylki, if any, in place of the
/// book's own: after its account's ("CardDAV — a@b.org · Family"), or alone
/// for a book that is no account's.
pub(super) fn with_given_name(name: &str, given: Option<&String>) -> String {
    match (given, name.split_once(" · ")) {
        (None, _) => name.to_string(),
        (Some(given), Some((account, _))) => format!("{account} · {given}"),
        (Some(given), None) if name.contains(" — ") => format!("{name} · {given}"),
        (Some(given), None) => given.clone(),
    }
}

pub(super) fn book_display_name(book_data: &str, sources: &HashMap<String, String>) -> String {
    let own = crate::desktop::platform::keyfile_value(book_data, "Data Source", "DisplayName");
    let (backend, identity, top_display) = book_account(book_data, sources);
    let account = identity.or(top_display.or(own.clone()));
    let label = match (backend.as_deref(), account.as_deref()) {
        (Some("local"), _) | (None, None) => i18n("On This Computer"),
        (Some("google"), Some(a)) => format!("Google — {a}"),
        (Some("google"), None) => "Google".to_string(),
        (Some("microsoft365"), Some(a)) => format!("Microsoft 365 — {a}"),
        (Some("microsoft365"), None) => "Microsoft 365".to_string(),
        (Some("ldap"), Some(a)) => format!("LDAP — {a}"),
        (Some("ldap"), None) => "LDAP".to_string(),
        (_, Some(a)) => format!("CardDAV — {a}"),
        (_, None) => i18n("CardDAV Address Book"),
    };
    // One account can hold several books (Nextcloud: Contacts, Recently
    // contacted, System address book), so name the book too — unless it is
    // the account itself.
    match own {
        Some(own) if !own.is_empty() && Some(&own) != account.as_ref() => {
            format!("{label} · {own}")
        }
        _ => label,
    }
}

/// The backend that serves a book, walked up its parent chain to the
/// account collection (google / microsoft365 / webdav / local…), with the
/// account's identity and display name when there is one.
fn book_account(
    book_data: &str,
    sources: &HashMap<String, String>,
) -> (Option<String>, Option<String>, Option<String>) {
    let mut backend =
        crate::desktop::platform::keyfile_value(book_data, "Address Book", "BackendName");
    let mut identity: Option<String> = None;
    let mut top_display = None;
    let mut current = book_data.to_string();
    for _ in 0..4 {
        let Some(parent) =
            crate::desktop::platform::keyfile_value(&current, "Data Source", "Parent")
        else {
            break;
        };
        let Some(parent_data) = sources.get(&parent) else {
            break;
        };
        if let Some(b) =
            crate::desktop::platform::keyfile_value(parent_data, "Collection", "BackendName")
        {
            backend = Some(b);
        }
        if identity.is_none() {
            identity =
                crate::desktop::platform::keyfile_value(parent_data, "Collection", "Identity");
        }
        if let Some(d) =
            crate::desktop::platform::keyfile_value(parent_data, "Data Source", "DisplayName")
        {
            top_display = Some(d);
        }
        current = parent_data.clone();
    }
    (backend, identity, top_display)
}

/// How each address book in the registry keeps a new group: Evolution's
/// lists on this computer, categories on Nextcloud, Apple's groups on
/// another CardDAV server (what iCloud, Fastmail and the Apple and DAVx⁵
/// clients read), none on Google, which keeps a group as a person with odd
/// addresses, nor on Microsoft 365 or a directory, which take none.
fn formats_from(sources: &HashMap<String, String>) -> BookFormats {
    sources
        .iter()
        .filter(|(_, data)| data.contains("[Address Book]"))
        .map(|(uid, data)| {
            let format = if keeps_categories(data) {
                Some(GroupFormat::Categories)
            } else {
                group_format_for(book_account(data, sources).0.as_deref())
            };
            (uid.clone(), format)
        })
        .collect()
}

/// Whether a book is Nextcloud's (or ownCloud's, which it came from), whose
/// books are all under `/remote.php/`: its Contacts app knows a group only
/// as a category on each member's card (#652).
pub(super) fn keeps_categories(book_data: &str) -> bool {
    crate::desktop::platform::keyfile_value(book_data, "WebDAV Backend", "ResourcePath")
        .is_some_and(|path| path.to_ascii_lowercase().contains("/remote.php/"))
}

pub(super) fn group_format_for(backend: Option<&str>) -> Option<GroupFormat> {
    match backend {
        None | Some("local") => Some(GroupFormat::Evolution),
        Some("google" | "microsoft365" | "ldap") => None,
        Some(_) => Some(GroupFormat::Apple),
    }
}

/// Whether a source keyfile's `[Data Source]` group says `Enabled=false`.
pub(super) fn data_source_disabled(data: &str) -> bool {
    crate::desktop::platform::keyfile(data).and_then(|kf| kf.boolean("Data Source", "Enabled").ok())
        == Some(false)
}

/// Discover the versioned Sources registry bus name (e.g. `…Sources5`).
pub(crate) fn sources_dest() -> Option<String> {
    remembered(
        "org.gnome.evolution.dataserver.Sources",
        sources_dest_from_files,
    )
}

/// Fallback: read the name from the host's D-Bus service files (works outside
/// Flatpak only).
fn sources_dest_from_files() -> Option<String> {
    dest_from_service_files("evolution.dataserver.Sources", |_| true)
}

/// Whether the registry lists `uid` as live (fail-open when unreachable,
/// but for a book switched off for Hylki by its own UID).
pub(super) fn book_uid_active(uid: &str, books: &Option<HashMap<String, String>>) -> bool {
    books.as_ref().map_or_else(
        || !crate::contacts::contact_sources::off().contains(uid),
        |map| map.contains_key(uid),
    )
}

/// The registry's friendly name for a book, with a fallback for when the
/// registry is unreachable.
pub(super) fn book_uid_name(
    uid: &str,
    books: &Option<HashMap<String, String>>,
    fallback: &str,
) -> String {
    books
        .as_ref()
        .and_then(|map| map.get(uid).cloned())
        .unwrap_or_else(|| source_display_name(uid).unwrap_or_else(|| fallback.to_string()))
}
