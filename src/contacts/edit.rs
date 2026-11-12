//! Editing contacts and groups: patching the stored vCard so everything
//! the editor does not know survives, and the EDS calls that write it.

use std::path::PathBuf;

use super::categories::{category_of, delete_category_group, save_category_group};
use super::details::{parse_vcard_fields, ContactDetails, GroupFormat, Labeled, Member, Postal};
use super::eds_write::{
    book_call, book_get_contact, book_get_uids, factory_dest, open_book, sexpr_escape,
};
use super::photos::local_changed;
use super::read::is_local_book;
use super::vcard::{escape_vcard_value, insert_after_version, property_name};
use crate::i18n::i18n;

// ---------------------------------------------------------------------------
// In-app editing (vCard patching + EDS writes)
// ---------------------------------------------------------------------------

/// The fields the in-app contact editor can change.
#[derive(Debug, Clone, Default)]
pub(crate) struct ContactEdit {
    pub(crate) name: String,
    pub(crate) nickname: String,
    pub(crate) org: String,
    pub(crate) title: String,
    pub(crate) note: String,
    pub(crate) emails: Vec<Labeled>,
    pub(crate) phones: Vec<Labeled>,
    pub(crate) urls: Vec<String>,
    pub(crate) addresses: Vec<Postal>,
    /// A BDAY value ([`birthday_value`]), empty for none.
    pub(crate) birthday: String,
    pub(crate) photo: PhotoEdit,
}

/// What the editor does to a contact's photo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum PhotoEdit {
    #[default]
    Keep,
    Remove,
    /// A new photo: the image file's bytes, made small by the editor.
    Set(Vec<u8>),
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
pub(crate) fn patched_vcard(original: &str, e: &ContactEdit) -> String {
    const REPLACED: [&str; 11] = [
        "FN", "N", "NICKNAME", "ORG", "TITLE", "NOTE", "EMAIL", "TEL", "URL", "ADR", "BDAY",
    ];
    let photo = e.photo != PhotoEdit::Keep;
    let mut out = kept_lines_by(original, |prop| {
        REPLACED.contains(&prop)
            || (photo && matches!(prop, "PHOTO" | "X-IMAGETYPE" | "X-IMAGEHASH"))
    });

    // FN is required: with no name, the card goes by what else it has.
    let fallback = || {
        [&e.org, &e.nickname]
            .into_iter()
            .map(|s| s.trim().to_string())
            .chain(
                e.emails
                    .iter()
                    .chain(&e.phones)
                    .map(|l| l.value.trim().to_string()),
            )
            .find(|s| !s.is_empty())
            .unwrap_or_default()
    };
    let named = !e.name.trim().is_empty();
    let fn_value = if named {
        e.name.trim().to_string()
    } else {
        fallback()
    };
    out.push(format!("FN:{}", escape_vcard_value(&fn_value)));
    let name = if named { e.name.trim() } else { "" };
    // Structured name: last word as the family name — the same heuristic an
    // address book's "sort by surname" uses.
    let mut words: Vec<&str> = name.split_whitespace().collect();
    let family = if words.len() > 1 {
        words.pop().unwrap_or("")
    } else {
        ""
    };
    out.push(format!(
        "N:{};{};;;",
        escape_vcard_value(family),
        escape_vcard_value(&words.join(" "))
    ));
    for a in e.addresses.iter().filter(|a| !a.is_empty()) {
        out.push(format!("ADR{}:{}", type_param_for(&a.label), a.adr_value()));
    }
    if !e.birthday.trim().is_empty() {
        out.push(format!("BDAY:{}", e.birthday.trim()));
    }
    if let PhotoEdit::Set(bytes) = &e.photo {
        use base64::Engine as _;
        let kind = match crate::models::image_ext(bytes) {
            "png" => "PNG",
            "gif" => "GIF",
            _ => "JPEG",
        };
        out.push(format!(
            "PHOTO;ENCODING=b;TYPE={kind}:{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ));
    }
    if !e.nickname.trim().is_empty() {
        out.push(format!(
            "NICKNAME:{}",
            escape_vcard_value(e.nickname.trim())
        ));
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
pub(crate) fn new_vcard(e: &ContactEdit) -> String {
    patched_vcard(EMPTY_VCARD, e)
}

/// The lines of a stored vCard up to its END, less the properties in
/// `replaced` (folded continuations go with their property).
fn kept_lines(original: &str, replaced: &[&str]) -> Vec<String> {
    kept_lines_by(original, |prop| replaced.contains(&prop))
}

/// The lines of `original` before END:VCARD, less the properties (with
/// their folded continuations) `drop` names; `drop` is given the upper-case
/// property name.
pub(super) fn kept_lines_by(original: &str, drop: impl Fn(&str) -> bool) -> Vec<String> {
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
        let prop = property_name(line.split(':').next().unwrap_or("")).to_ascii_uppercase();
        skipping = drop(&prop);
        if !skipping {
            out.push(line.to_string());
        }
    }
    out
}

/// The fields the group editor can change.
#[derive(Debug, Clone, Default)]
pub(crate) struct GroupEdit {
    pub(crate) name: String,
    pub(crate) members: Vec<Member>,
    pub(crate) hide_addresses: bool,
    /// Member lines the editor did not show ([`ContactDetails::hidden_members`]),
    /// written back as they were.
    pub(crate) kept: Vec<String>,
}

/// Rewrite a stored Evolution list with the editor's values. A member who
/// was already there keeps the line it was read from; one added here gets
/// a line of the same shape as Evolution's.
pub(super) fn patched_group_vcard(original: &str, e: &GroupEdit) -> String {
    const REPLACED: [&str; 7] = [
        "FN",
        "N",
        "X-EVOLUTION-FILE-AS",
        "X-EVOLUTION-LIST",
        "X-EVOLUTION-LIST-SHOW-ADDRESSES",
        "EMAIL",
        "KIND",
    ];
    let mut out = kept_lines(original, &REPLACED);
    let name = escape_vcard_value(e.name.trim());
    out.push(format!("FN:{name}"));
    out.push("N:;;;;".to_string());
    out.push(format!("X-EVOLUTION-FILE-AS:{name}"));
    out.push("X-EVOLUTION-LIST:TRUE".to_string());
    out.push(format!(
        "X-EVOLUTION-LIST-SHOW-ADDRESSES:{}",
        if e.hide_addresses { "FALSE" } else { "TRUE" }
    ));
    for m in &e.members {
        if !m.line.is_empty() {
            out.push(m.line.clone());
            continue;
        }
        // A parameter value is quoted and cannot hold a quote.
        let param = |v: &str| {
            v.chars()
                .filter(|c| !matches!(c, '"' | '\r' | '\n'))
                .collect::<String>()
        };
        let mut line = String::from("EMAIL");
        if !m.name.trim().is_empty() {
            line.push_str(&format!(
                ";X-EVOLUTION-DEST-NAME=\"{}\"",
                param(m.name.trim())
            ));
        }
        line.push_str(&format!(";X-EVOLUTION-DEST-EMAIL=\"{}\"", param(&m.email)));
        line.push_str(&format!(
            ":{}",
            escape_vcard_value(&crate::worker::format_recipient(&m.name, &m.email))
        ));
        out.push(line);
    }
    out.push("END:VCARD".to_string());
    out.join("\r\n") + "\r\n"
}

/// A brand-new group's vCard from the editor's values.
pub(crate) fn new_group_vcard(e: &GroupEdit) -> String {
    patched_group_vcard(EMPTY_VCARD, e)
}

pub(super) const EMPTY_VCARD: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nEND:VCARD\r\n";

/// Rewrite an Apple group (or write a new one, from [`EMPTY_VCARD`]) with
/// the editor's values. Every member must carry the UID of their card in
/// the group's book, which [`save_group`] sees to.
pub(super) fn patched_apple_group(original: &str, e: &GroupEdit) -> String {
    const REPLACED: [&str; 5] = [
        "FN",
        "N",
        "X-ADDRESSBOOKSERVER-KIND",
        "X-ADDRESSBOOKSERVER-MEMBER",
        "X-EVOLUTION-LIST-SHOW-ADDRESSES",
    ];
    let mut out = kept_lines(original, &REPLACED);
    let name = escape_vcard_value(e.name.trim());
    out.push(format!("FN:{name}"));
    out.push(format!("N:{name};;;;"));
    out.push("X-ADDRESSBOOKSERVER-KIND:group".to_string());
    // Apple's groups have no Bcc of their own; Evolution's line, which
    // other apps pass over, keeps the choice.
    if e.hide_addresses {
        out.push("X-EVOLUTION-LIST-SHOW-ADDRESSES:FALSE".to_string());
    }
    for m in &e.members {
        if !m.line.is_empty() {
            out.push(m.line.clone());
        } else if !m.uid.is_empty() {
            out.push(format!("X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:{}", m.uid));
        }
    }
    out.extend(e.kept.iter().cloned());
    out.push("END:VCARD".to_string());
    out.join("\r\n") + "\r\n"
}

/// Write a group to `book_uid`: changed when `original` is its stored card,
/// else new. An Apple group holds cards of its own book, so a member with
/// no card there gets one first, with their name and address (blocking:
/// call from a background thread).
pub(crate) fn save_group(
    book_uid: &str,
    format: GroupFormat,
    original: Option<&str>,
    mut edit: GroupEdit,
) -> Result<(), String> {
    if format == GroupFormat::Categories {
        return save_category_group(book_uid, original, edit);
    }
    let create = original.is_none();
    let original = original.unwrap_or(EMPTY_VCARD);
    let vcard = match format {
        GroupFormat::Evolution => patched_group_vcard(original, &edit),
        GroupFormat::Apple => {
            let cardless = |m: &Member| {
                m.uid.is_empty() && m.line.is_empty() && (!m.email.is_empty() || !m.card.is_empty())
            };
            if edit.members.iter().any(cardless) {
                let mut cards = BookCards::open(book_uid)?;
                for m in edit.members.iter_mut().filter(|m| cardless(m)) {
                    m.uid = cards.card_for(&m.name, &m.email, &m.card)?;
                }
            }
            patched_apple_group(original, &edit)
        }
        GroupFormat::Kind | GroupFormat::Categories => {
            return Err(i18n("Hylki cannot change this kind of group"))
        }
    };
    if create {
        create_contact(book_uid, &vcard)
    } else {
        modify_contact(book_uid, &vcard)
    }
}

/// Move a group to another book: made in `to_book` as `format` from `edit`,
/// then deleted from `from_book`, and only once made.
pub(crate) fn move_group(
    from_book: &str,
    uid: &str,
    to_book: &str,
    format: GroupFormat,
    edit: GroupEdit,
) -> Result<(), String> {
    save_group(to_book, format, None, edit)?;
    delete_contact(from_book, uid)
}

/// The people's cards of one book, for giving each group member one.
pub(super) enum BookCards {
    Local,
    Eds {
        conn: zbus::blocking::Connection,
        bus: String,
        path: String,
    },
}

impl BookCards {
    pub(super) fn open(book_uid: &str) -> Result<Self, String> {
        if is_local_book(book_uid) {
            return Ok(Self::Local);
        }
        let dest = factory_dest().ok_or("Evolution Data Server is not available")?;
        let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
        let (bus, path) = open_book(&conn, &dest, book_uid)?;
        Ok(Self::Eds { conn, bus, path })
    }

    /// The UID of the person in the book with `email` (or, with none, of
    /// that name), after making them a card when there is none: `copy`, the
    /// card of another book, else one with their name and address.
    pub(super) fn card_for(
        &mut self,
        name: &str,
        email: &str,
        copy: &str,
    ) -> Result<String, String> {
        let card = if copy.is_empty() {
            new_vcard(&ContactEdit {
                name: if name.trim().is_empty() {
                    email.to_string()
                } else {
                    name.trim().to_string()
                },
                emails: vec![Labeled {
                    label: String::new(),
                    value: email.to_string(),
                }],
                ..ContactEdit::default()
            })
        } else {
            copy.to_string()
        };
        // Someone with no address is known by name alone.
        let (field, value) = if email.trim().is_empty() {
            ("full_name", name.trim())
        } else {
            ("email", email.trim())
        };
        match self {
            Self::Local => {
                let found = crate::contacts::local::list_vcards()?
                    .into_iter()
                    .find_map(|v| {
                        let c = parse_vcard_fields(&v).filter(|c| !c.is_group())?;
                        let same = if field == "email" {
                            c.emails.iter().any(|e| e.value.eq_ignore_ascii_case(value))
                        } else {
                            c.emails.is_empty() && c.name.trim().eq_ignore_ascii_case(value)
                        };
                        same.then_some(c.eds_uid)
                    });
                if let Some(uid) = found.filter(|u| !u.is_empty()) {
                    return Ok(uid);
                }
                let uid = format!(
                    "hylki-{}",
                    crate::rng::token(24).map_err(|e| e.to_string())?
                );
                let card = insert_after_version(&card, &format!("UID:{uid}"));
                write_local(crate::contacts::local::create(&card))?;
                Ok(uid)
            }
            Self::Eds { conn, bus, path } => {
                let query = format!("(is \"{field}\" \"{}\")", sexpr_escape(value));
                for uid in book_get_uids(conn, bus, path, &query)? {
                    let found = book_get_contact(conn, bus, path, &uid)?;
                    // By name, only someone else with no address either.
                    let fits = parse_vcard_fields(&found).is_some_and(|c| {
                        !c.is_group() && (field == "email" || c.emails.is_empty())
                    });
                    if fits {
                        return Ok(uid);
                    }
                }
                let reply = book_call(conn, bus, path, "CreateContacts", &(vec![card], 0u32))?;
                let (uids,): (Vec<String>,) =
                    reply.body().deserialize().map_err(|e| e.to_string())?;
                uids.into_iter()
                    .next()
                    .ok_or_else(|| "The address book made no contact".to_string())
            }
        }
    }
}

/// Tell the photo index and the UI about a finished local write.
fn write_local(result: Result<(), String>) -> Result<(), String> {
    if result.is_ok() {
        local_changed();
    }
    result
}

/// Import `.vcf` files into the local Hylki book (blocking: call from a
/// background thread).
pub(crate) fn import_vcf_files(
    paths: &[PathBuf],
) -> Result<crate::contacts::local::ImportOutcome, String> {
    let outcome = crate::contacts::local::import_files(paths)?;
    if outcome.added + outcome.updated > 0 {
        local_changed();
    }
    Ok(outcome)
}

/// Write a modified vCard back to its book through EDS.
pub(crate) fn modify_contact(book_uid: &str, vcard: &str) -> Result<(), String> {
    if is_local_book(book_uid) {
        return write_local(crate::contacts::local::modify(vcard));
    }
    let dest = factory_dest().ok_or("Evolution Data Server is not available")?;
    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let (bus, path) = open_book(&conn, &dest, book_uid)?;
    book_call(
        &conn,
        &bus,
        &path,
        "ModifyContacts",
        &(vec![vcard.to_string()], 0u32),
    )?;
    Ok(())
}

/// Create a contact from a full vCard through EDS.
pub(crate) fn create_contact(book_uid: &str, vcard: &str) -> Result<(), String> {
    if is_local_book(book_uid) {
        return write_local(crate::contacts::local::create(vcard));
    }
    let dest = factory_dest().ok_or("Evolution Data Server is not available")?;
    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let (bus, path) = open_book(&conn, &dest, book_uid)?;
    book_call(
        &conn,
        &bus,
        &path,
        "CreateContacts",
        &(vec![vcard.to_string()], 0u32),
    )?;
    Ok(())
}

/// Delete contacts (by their EDS UIDs) from one book, in one call.
pub(crate) fn delete_contacts(book_uid: &str, uids: &[String]) -> Result<(), String> {
    // A category group has no card: its name comes off its people's.
    let (groups, uids): (Vec<&String>, Vec<&String>) =
        uids.iter().partition(|u| category_of(u).is_some());
    for g in groups {
        delete_category_group(book_uid, category_of(g).unwrap_or_default())?;
    }
    if uids.is_empty() {
        return Ok(());
    }
    let uids: Vec<String> = uids.into_iter().cloned().collect();
    if is_local_book(book_uid) {
        for uid in &uids {
            crate::contacts::local::delete(uid)?;
        }
        return write_local(Ok(()));
    }
    let dest = factory_dest().ok_or("Evolution Data Server is not available")?;
    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let (bus, path) = open_book(&conn, &dest, book_uid)?;
    book_call(&conn, &bus, &path, "RemoveContacts", &(uids.to_vec(), 0u32))?;
    Ok(())
}

/// A contact's card as a new one for another book: without the UID, the
/// revision and what the old book's server kept in it, and with its photo
/// in the card itself. A cached photo is a file of the old book, which goes
/// with the contact.
pub(crate) fn moved_vcard(c: &ContactDetails) -> String {
    use base64::Engine as _;
    let mut out = kept_lines_by(&c.raw_vcard, |prop| {
        matches!(
            prop,
            "UID" | "REV" | "PHOTO" | "X-IMAGETYPE" | "X-IMAGEHASH"
        ) || prop.starts_with("X-EVOLUTION-WEBDAV")
            || prop.starts_with("X-EVOLUTION-GOOGLE")
            || prop.starts_with("X-EVOLUTION-M365")
    });
    if let Some(photo) = &c.photo_bytes() {
        let kind = crate::models::image_ext(photo).to_ascii_uppercase();
        let kind = if kind == "JPG" {
            "JPEG".to_string()
        } else {
            kind
        };
        out.push(format!(
            "PHOTO;ENCODING=b;TYPE={kind}:{}",
            base64::engine::general_purpose::STANDARD.encode(photo)
        ));
    }
    out.push("END:VCARD".to_string());
    out.join("\r\n") + "\r\n"
}

/// Move a contact: made in `to_book` from `vcard` ([`moved_vcard`]), then
/// deleted from `from_book`, and only once the new one is made.
pub(crate) fn move_contact(
    from_book: &str,
    uid: &str,
    vcard: &str,
    to_book: &str,
) -> Result<(), String> {
    create_contact(to_book, vcard)?;
    delete_contact(from_book, uid)
}

/// Delete a contact (by its EDS UID) from its book.
pub(super) fn delete_contact(book_uid: &str, uid: &str) -> Result<(), String> {
    if let Some(name) = category_of(uid) {
        return delete_category_group(book_uid, name);
    }
    if is_local_book(book_uid) {
        return write_local(crate::contacts::local::delete(uid));
    }
    let dest = factory_dest().ok_or("Evolution Data Server is not available")?;
    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let (bus, path) = open_book(&conn, &dest, book_uid)?;
    book_call(
        &conn,
        &bus,
        &path,
        "RemoveContacts",
        &(vec![uid.to_string()], 0u32),
    )?;
    Ok(())
}
