//! Writing through Evolution Data Server over D-Bus: finding the address
//! book factory, opening a book, and adding an address.

use super::details::parse_vcard_fields;
use super::photos::local_changed;
use super::read::{is_local_book, read_contacts};
use super::vcard::is_group_vcard;

// ---------------------------------------------------------------------------
// Writing (EDS D-Bus)
// ---------------------------------------------------------------------------

/// Find a versioned EDS bus name (`<prefix><digits>`, e.g. `…AddressBook10`)
/// by asking the session bus itself.
///
/// Inside Flatpak, `/usr/share/dbus-1/services` is the *runtime's* directory
/// and holds none of the host's EDS service files, so the file scan below
/// finds nothing there. The bus, however, lists the host's EDS names (the
/// `--talk-name=org.gnome.evolution.dataserver.*` grant makes them visible),
/// so query it first and keep the file scan as a fallback.
fn bus_dest(prefix: &str) -> Option<String> {
    let conn = zbus::blocking::Connection::session().ok()?;
    let proxy = zbus::blocking::fdo::DBusProxy::new(&conn).ok()?;
    let mut names: Vec<String> = proxy
        .list_names()
        .unwrap_or_default()
        .iter()
        .map(|n| n.as_str().to_string())
        .collect();
    names.extend(
        proxy
            .list_activatable_names()
            .unwrap_or_default()
            .iter()
            .map(|n| n.as_str().to_string()),
    );
    // Highest interface version wins if several are present.
    names
        .into_iter()
        .filter_map(|n| {
            let ver: u32 = n.strip_prefix(prefix)?.parse().ok()?;
            Some((ver, n))
        })
        .max_by_key(|(ver, _)| *ver)
        .map(|(_, n)| n)
}

/// The EDS name for `prefix`, from the bus or else the service files, looked
/// up once per run: callers ask on every contact write and directory search,
/// some of them on the UI thread, and the name does not change under a
/// running session. Only a name found is kept, so EDS starting late is
/// still picked up.
pub(super) fn remembered(
    prefix: &'static str,
    from_files: fn() -> Option<String>,
) -> Option<String> {
    static FOUND: std::sync::Mutex<Vec<(&'static str, String)>> = std::sync::Mutex::new(Vec::new());
    if let Some((_, name)) = FOUND.lock().ok()?.iter().find(|(p, _)| *p == prefix) {
        return Some(name.clone());
    }
    let name = bus_dest(prefix).or_else(from_files)?;
    tracing::debug!("EDS: {prefix} is {name}");
    if let Ok(mut found) = FOUND.lock() {
        found.push((prefix, name.clone()));
    }
    Some(name)
}

/// Discover the versioned AddressBook factory bus name (e.g. `…AddressBook10`).
pub(crate) fn factory_dest() -> Option<String> {
    remembered(
        "org.gnome.evolution.dataserver.AddressBook",
        factory_dest_from_files,
    )
}

/// Discover the versioned Calendar factory bus name (e.g. `…Calendar8`),
/// which serves task lists too: where events and tasks made from mail are
/// written (#564).
pub(crate) fn calendar_factory_dest() -> Option<String> {
    remembered("org.gnome.evolution.dataserver.Calendar", || {
        dest_from_service_files("evolution.dataserver.Calendar", |_| true)
    })
}

/// Fallback: read the name from the host's D-Bus service files (works outside
/// Flatpak only).
fn factory_dest_from_files() -> Option<String> {
    dest_from_service_files("AddressBook", |name| name.contains("AddressBook"))
}

/// The bus name in the first of the host's D-Bus service files whose file
/// name holds `file_needle` and whose name `accept` takes.
pub(super) fn dest_from_service_files(
    file_needle: &str,
    accept: impl Fn(&str) -> bool,
) -> Option<String> {
    let dir = std::path::Path::new("/usr/share/dbus-1/services");
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.contains(file_needle) && name.ends_with(".service") {
            if let Ok(text) = std::fs::read_to_string(entry.path()) {
                if let Some(n) =
                    crate::desktop::platform::keyfile_value(&text, "D-BUS Service", "Name")
                {
                    if accept(&n) {
                        return Some(n);
                    }
                }
            }
        }
    }
    None
}

const FACTORY_PATH: &str = "/org/gnome/evolution/dataserver/AddressBookFactory";
const FACTORY_IFACE: &str = "org.gnome.evolution.dataserver.AddressBookFactory";
pub(crate) const BOOK_IFACE: &str = "org.gnome.evolution.dataserver.AddressBook";

/// Open a book by source UID; returns (bus_name, object_path) for the book.
pub(crate) fn open_book(
    conn: &zbus::blocking::Connection,
    dest: &str,
    uid: &str,
) -> Result<(String, String), String> {
    let reply = conn
        .call_method(
            Some(dest),
            FACTORY_PATH,
            Some(FACTORY_IFACE),
            "OpenAddressBook",
            &(uid,),
        )
        .map_err(|e| e.to_string())?;
    // Returns (object_path, bus_name).
    let (object_path, bus_name): (String, String) =
        reply.body().deserialize().map_err(|e| e.to_string())?;
    conn.call_method(
        Some(bus_name.as_str()),
        object_path.as_str(),
        Some(BOOK_IFACE),
        "Open",
        &(),
    )
    .map_err(|e| e.to_string())?;
    Ok((bus_name, object_path))
}

/// What happened when adding an email to Contacts.
#[derive(Debug)]
pub(crate) enum AddOutcome {
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
pub(crate) fn add_or_merge(book_uid: &str, name: &str, email: &str) -> Result<AddOutcome, String> {
    // 1. Already present (in any book)? Don't create a duplicate.
    let email_l = email.trim().to_lowercase();
    if let Some(existing) = read_contacts()
        .into_iter()
        .find(|c| c.email.to_lowercase() == email_l)
    {
        return Ok(AddOutcome::AlreadyPresent(existing.name));
    }

    if is_local_book(book_uid) {
        return add_or_merge_local(name, email);
    }

    let dest = factory_dest().ok_or("Evolution Data Server is not available")?;
    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let (bus, path) = open_book(&conn, &dest, book_uid)?;

    // 2. A contact with the same name in this book → merge the email into it.
    if !name.trim().is_empty() {
        let query = format!("(is \"full_name\" \"{}\")", sexpr_escape(name.trim()));
        let uids = book_get_uids(&conn, &bus, &path, &query)?;
        // A list of the same name is not that person.
        for uid in &uids {
            let vcard = book_get_contact(&conn, &bus, &path, uid)?;
            if is_group_vcard(&vcard) {
                continue;
            }
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

/// [`add_or_merge`] for the local book: merge into the contact with the same
/// name, else create one.
fn add_or_merge_local(name: &str, email: &str) -> Result<AddOutcome, String> {
    let vcards = if crate::contacts::local::exists() {
        crate::contacts::local::list_vcards()?
    } else {
        Vec::new()
    };
    let result = match merge_target(&vcards, name) {
        Some(vcard) => {
            crate::contacts::local::modify(&add_email_to_vcard(&vcard, email.trim()))?;
            AddOutcome::Merged(name.trim().to_string())
        }
        None => {
            crate::contacts::local::create(&vcard_for(name, email))?;
            AddOutcome::Created
        }
    };
    local_changed();
    Ok(result)
}

/// The stored vCard whose display name equals `name` (case-insensitive).
pub(super) fn merge_target(vcards: &[String], name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    vcards
        .iter()
        .find(|v| {
            parse_vcard_fields(v)
                .is_some_and(|d| !d.is_group() && d.name.trim().eq_ignore_ascii_case(name))
        })
        .cloned()
}

pub(super) fn book_call<B>(
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

pub(super) fn book_get_uids(
    conn: &zbus::blocking::Connection,
    bus: &str,
    path: &str,
    query: &str,
) -> Result<Vec<String>, String> {
    let reply = book_call(conn, bus, path, "GetContactListUids", &(query,))?;
    let (uids,): (Vec<String>,) = reply.body().deserialize().map_err(|e| e.to_string())?;
    Ok(uids)
}

pub(super) fn book_get_contact(
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
pub(super) fn add_email_to_vcard(vcard: &str, email: &str) -> String {
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
pub(super) fn sexpr_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(super) fn vcard_for(name: &str, email: &str) -> String {
    let name = if name.trim().is_empty() {
        email
    } else {
        name.trim()
    };
    // Minimal vCard 3.0; EDS assigns the UID.
    format!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:{name}\r\nN:;{name};;;\r\n\
         EMAIL;TYPE=INTERNET:{email}\r\nEND:VCARD\r\n"
    )
}
