//! A contact in full, as the Contacts page shows it, read from its vCard,
//! and the members of contact groups.

use std::collections::{HashMap, HashSet};

use super::read::{local_details, open_book_db, EDS_LAYOUTS};
use super::registry::{book_uid_active, book_uid_name, registry_books_and_formats};
use super::vcard::{
    escape_vcard_value, property_name, split_unescaped, split_vcard_line, unescape_vcard_text,
    unfold_vcard, vcard_photo, vcard_photo_with,
};

// ---------------------------------------------------------------------------
// Full contact details (the in-app Contacts view)
// ---------------------------------------------------------------------------

/// A postal address in the parts of a vCard ADR.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Postal {
    pub(crate) label: String,
    pub(crate) pobox: String,
    pub(crate) extended: String,
    pub(crate) street: String,
    pub(crate) locality: String,
    pub(crate) region: String,
    pub(crate) code: String,
    pub(crate) country: String,
}

impl Postal {
    fn from_adr(label: String, raw: &str) -> Postal {
        let mut parts = split_unescaped(raw, ';')
            .into_iter()
            .map(|p| unescape_vcard_text(p.trim()));
        let mut next = || parts.next().unwrap_or_default();
        Postal {
            label,
            pobox: next(),
            extended: next(),
            street: next(),
            locality: next(),
            region: next(),
            code: next(),
            country: next(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        [
            &self.pobox,
            &self.extended,
            &self.street,
            &self.locality,
            &self.region,
            &self.code,
            &self.country,
        ]
        .iter()
        .all(|p| p.trim().is_empty())
    }

    pub(super) fn adr_value(&self) -> String {
        [
            &self.pobox,
            &self.extended,
            &self.street,
            &self.locality,
            &self.region,
            &self.code,
            &self.country,
        ]
        .iter()
        .map(|p| escape_vcard_value(p.trim()))
        .collect::<Vec<_>>()
        .join(";")
    }
}

/// A birthday as (year, month, day) from a stored BDAY, the year `None`
/// when it has none ("--04-12").
pub(crate) fn birthday_parts(raw: &str) -> Option<(Option<i32>, u32, u32)> {
    let head = raw.split('T').next().unwrap_or(raw).trim();
    let digits: String = head.chars().filter(|c| c.is_ascii_digit()).collect();
    let (year, month, day) = if head.starts_with("--") && digits.len() == 4 {
        (None, &digits[0..2], &digits[2..4])
    } else if digits.len() == 8 {
        (digits[0..4].parse().ok(), &digits[4..6], &digits[6..8])
    } else {
        return None;
    };
    let (month, day) = (month.parse().ok()?, day.parse().ok()?);
    ((1..=12).contains(&month) && (1..=31).contains(&day)).then_some((year, month, day))
}

/// A BDAY value from its parts, with no year as "--MM-DD".
pub(crate) fn birthday_value(year: Option<i32>, month: u32, day: u32) -> String {
    match year {
        Some(y) => format!("{y:04}-{month:02}-{day:02}"),
        None => format!("--{month:02}-{day:02}"),
    }
}

/// One labelled value on a contact (a "Home" email, a "Mobile" phone…). The
/// label may be empty when the vCard carries no usable TYPE.
#[derive(Debug, Clone)]
pub(crate) struct Labeled {
    pub(crate) label: String,
    pub(crate) value: String,
}

/// A fully parsed address-book entry, everything the in-app Contacts view
/// shows. Read straight from the EDS vCards, so it matches GNOME Contacts.
#[derive(Debug, Clone, Default)]
pub(crate) struct ContactDetails {
    pub(crate) name: String,
    pub(crate) nickname: String,
    pub(crate) org: String,
    pub(crate) title: String,
    pub(crate) birthday: String,
    pub(crate) note: String,
    pub(crate) emails: Vec<Labeled>,
    pub(crate) phones: Vec<Labeled>,
    pub(crate) addresses: Vec<Labeled>,
    /// The same addresses in their parts, for the editor.
    pub(crate) postal: Vec<Postal>,
    /// BDAY as stored ("1985-04-12", "--04-12"…), for the editor;
    /// `birthday` is it in words.
    pub(crate) birthday_raw: String,
    pub(crate) urls: Vec<String>,
    pub(crate) photo: Option<Vec<u8>>,
    /// [`photo_key`] of `photo`, set by [`read_contact_details`]; 0 when
    /// there is none or it was not worked out.
    pub(crate) photo_key: u64,
    /// EDS source UID of the book this contact came from — where edits go.
    pub(crate) book_uid: String,
    /// That book's display name ("On This Computer", the CardDAV account…).
    pub(crate) book_name: String,
    /// The contact's own EDS UID (for deletion and deep links).
    pub(crate) eds_uid: String,
    /// The stored vCard, verbatim; edits patch this so unknown properties
    /// (PHOTO, UID, X-…) survive the round trip.
    pub(crate) raw_vcard: String,
    /// Set for a contact group, with how its vCard keeps the members. A
    /// group has no `emails` of its own: its addresses are the members'.
    pub(crate) group: Option<GroupFormat>,
    pub(crate) members: Vec<Member>,
    /// Members named by the UID of a contact in the same book
    /// (`urn:uuid:`), with the line naming them, until [`resolve_members`]
    /// looks them up.
    pub(crate) member_refs: Vec<(String, String)>,
    /// The lines of members [`resolve_members`] found no address for (a
    /// person with only a phone, a card not synced yet). They are not
    /// shown, and an edit keeps them.
    pub(crate) hidden_members: Vec<String>,
    /// Mail to the group puts its members in Bcc, so they do not see each
    /// other's addresses: Evolution's "Hide addresses when sending mail to
    /// this list".
    pub(crate) hide_addresses: bool,
}

impl ContactDetails {
    /// Whether the card has a photo, held here or let go by the Contacts
    /// list once it made the list's thumbnail.
    pub(crate) fn has_photo(&self) -> bool {
        self.photo.is_some() || self.photo_key != 0
    }

    /// The photo's bytes: those held, or read again from the card. The
    /// Contacts list keeps only thumbnails, since iCloud's megapixel photos
    /// came to some 40 MB held for the whole session (#504).
    pub(crate) fn photo_bytes(&self) -> Option<Vec<u8>> {
        if self.photo.is_some() || self.photo_key == 0 {
            return self.photo.clone();
        }
        vcard_photo_with(
            &self.raw_vcard,
            self.book_uid != crate::contacts::local::LOCAL_BOOK_UID,
        )
    }

    /// The address used for avatars and as the compose default.
    pub(crate) fn primary_email(&self) -> Option<&str> {
        self.emails.first().map(|e| e.value.as_str())
    }

    pub(crate) fn is_group(&self) -> bool {
        self.group.is_some()
    }

    /// Whether the group can be changed here: Evolution's lists, Apple's
    /// groups and categories, the kinds Hylki writes. A vCard 4 group is
    /// only read.
    pub(crate) fn group_editable(&self) -> bool {
        matches!(
            self.group,
            Some(GroupFormat::Evolution | GroupFormat::Apple | GroupFormat::Categories)
        )
    }

    /// How a group member found by card is known in a set of addresses.
    pub(crate) fn grouped_key(&self) -> String {
        format!("card:{}:{}", self.book_uid, self.eds_uid)
    }

    /// The members with an address to send to: an Apple group may hold
    /// people known only by a phone number.
    pub(crate) fn reachable(&self) -> impl Iterator<Item = &Member> {
        self.members.iter().filter(|m| !m.email.is_empty())
    }

    /// The members' addresses as one recipient field.
    pub(crate) fn members_field(&self) -> String {
        self.reachable()
            .map(|m| crate::worker::format_recipient(&m.name, &m.email))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// How a group's vCard lists its members.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroupFormat {
    /// `X-EVOLUTION-LIST:TRUE`, one `EMAIL` per member: what Evolution
    /// makes, and what Hylki writes, so an exported group imports there.
    Evolution,
    /// `X-ADDRESSBOOKSERVER-KIND:group`, one `X-ADDRESSBOOKSERVER-MEMBER`
    /// per member naming a contact of the same book by UID: Apple's, what
    /// iCloud and most CardDAV clients keep.
    Apple,
    /// `KIND:group` (vCard 4), with `MEMBER` lines.
    Kind,
    /// No card of its own: the group's name in the `CATEGORIES` of each
    /// member's card, the only groups Nextcloud knows (#652).
    Categories,
}

impl GroupFormat {
    /// Whether the group holds cards of its own book, and so can hold
    /// someone with no address, rather than addresses.
    pub(crate) fn holds_cards(self) -> bool {
        matches!(self, Self::Apple | Self::Categories)
    }
}

/// One person in a contact group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Member {
    pub(crate) name: String,
    pub(crate) email: String,
    /// The vCard line the member was read from, unfolded. An edit writes it
    /// back as it was, so what Evolution keeps about the member (the
    /// contact it was picked from, HTML mail) survives. Empty for a member
    /// added here.
    pub(crate) line: String,
    /// The UID of the member's own card in the group's book, for an Apple
    /// group, which names its members by it. Empty when not known yet.
    pub(crate) uid: String,
    /// The card of a contact of another book, copied into an Apple group's
    /// book when they join it ([`moved_vcard`]); empty for someone added by
    /// address, who gets a card with their name and address.
    pub(crate) card: String,
    /// The contact, by book and UID, a member was picked as in the group's
    /// form; the save finds their card by it.
    pub(crate) source: Option<(String, String)>,
}

impl Member {
    /// Whether `other` is the same person: the same card, or the same
    /// address.
    pub(crate) fn same(&self, other: &Member) -> bool {
        (!self.uid.is_empty() && self.uid == other.uid)
            || (!self.email.is_empty() && self.email.eq_ignore_ascii_case(&other.email))
            || (self.source.is_some() && self.source == other.source)
    }

    /// The key [`ContactDetails::grouped_key`] gives the member's card.
    pub(crate) fn card_key(&self, book_uid: &str) -> Option<String> {
        (!self.uid.is_empty()).then(|| format!("card:{book_uid}:{}", self.uid))
    }
}

/// Every contact from the local + cached (CardDAV) EDS books, fully parsed
/// and sorted by display name, contact groups among them.
pub(crate) fn read_contact_details() -> Vec<ContactDetails> {
    let mut out: Vec<ContactDetails> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let (active, formats) = registry_books_and_formats();
    // Walk the book directories directly (like `writable_books`) so each
    // contact remembers which book it belongs to.
    for layout in EDS_LAYOUTS {
        let sql = format!("SELECT {} FROM {}", layout.vcard, layout.cards);
        for (db, uid) in layout.books() {
            if !book_uid_active(&uid, &active) {
                continue;
            }
            let name = book_uid_name(&uid, &active, layout.unnamed);
            read_detail_db(&db, &sql, &uid, &name, &mut out, &mut seen);
        }
    }
    super::categories::add_category_groups(&mut out, &formats);
    out.extend(local_details());
    out.sort_by_key(|c| c.name.to_lowercase());
    // Hashed here, off the main thread: the page finds the photos it has
    // decoded already by these (#477).
    for c in &mut out {
        c.photo_key = c.photo.as_deref().map_or(0, photo_key);
    }
    out
}

/// A photo's identity, for the decoded avatars kept between reads. Never 0,
/// which stands for no photo.
pub(crate) fn photo_key(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish().max(1)
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
    let Ok(rows) = rows else { return };
    let mut book: Vec<ContactDetails> = Vec::new();
    for vcard in rows.flatten() {
        let Some(mut c) = parse_vcard_details(&vcard) else {
            continue;
        };
        c.book_uid = book_uid.to_string();
        c.book_name = book_name.to_string();
        c.raw_vcard = vcard;
        book.push(c);
    }
    resolve_book_members(&mut book);
    for c in book {
        if c.is_group() {
            out.push(c);
            continue;
        }
        // Every card is listed, the same person in two books twice, each
        // with its book, as an address book app lists them. Only a card
        // read twice (a book in two places on disk) is listed once.
        if c.eds_uid.is_empty() || seen.insert(format!("{}\u{0}{}", c.book_uid, c.eds_uid)) {
            out.push(c);
        }
    }
}

/// Resolve the groups' members named by UID against the people of the same
/// book, all of which are in `book`.
pub(super) fn resolve_book_members(book: &mut [ContactDetails]) {
    if !book.iter().any(|c| !c.member_refs.is_empty()) {
        return;
    }
    let people: HashMap<String, (String, String)> = book
        .iter()
        .filter(|c| !c.is_group() && !c.eds_uid.is_empty())
        .map(|c| {
            (
                c.eds_uid.clone(),
                (
                    c.name.clone(),
                    c.primary_email().unwrap_or_default().to_string(),
                ),
            )
        })
        .collect();
    for c in book.iter_mut().filter(|c| !c.member_refs.is_empty()) {
        resolve_members(c, |uid| people.get(uid).cloned());
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

/// "1985-04-12" / "19850412" → "Apr 12, 1985"; "--04-12" → "Apr 12", in the
/// locale's words and the user's date order, as mail dates are written;
/// anything else passes through unchanged.
pub(super) fn pretty_birthday(raw: &str) -> String {
    let head = raw.split('T').next().unwrap_or(raw);
    let digits: String = head.chars().filter(|c| c.is_ascii_digit()).collect();
    let (year, month, day) = if head.starts_with("--") && digits.len() == 4 {
        (None, &digits[0..2], &digits[2..4])
    } else if digits.len() == 8 {
        (Some(&digits[0..4]), &digits[4..6], &digits[6..8])
    } else {
        return raw.to_string();
    };
    let (Ok(m), Ok(d)) = (month.parse::<i32>(), day.parse::<i32>()) else {
        return raw.to_string();
    };
    // A birthday with no year is placed in a leap year, so 29 February is a
    // date too. Noon keeps the day the same in every time zone.
    let y = year.and_then(|y| y.parse::<i32>().ok()).unwrap_or(2000);
    let Ok(date) = gtk::glib::DateTime::from_local(y, m, d, 12, 0, 0.0) else {
        return raw.to_string();
    };
    match year {
        Some(_) => crate::datefmt::day_month_year(date.to_unix()),
        None => crate::datefmt::day_month(date.to_unix()),
    }
}

/// Parse the fields the Contacts view shows out of one vCard, its photo
/// included. Returns `None` for contact lists and entries with neither a name
/// nor an address.
pub(super) fn parse_vcard_details(vcard: &str) -> Option<ContactDetails> {
    let mut c = parse_vcard_fields(vcard)?;
    c.photo = vcard_photo(vcard);
    Some(c)
}

/// [`parse_vcard_details`] without the photo, which costs a base64 decode or
/// a file read: for imports and lookups that never show it.
pub(crate) fn parse_vcard_fields(vcard: &str) -> Option<ContactDetails> {
    let mut c = ContactDetails::default();
    // A list's EMAIL lines are its members, and the line saying so may come
    // after them, so they are kept aside until the card is read.
    let mut email_lines: Vec<(String, Vec<String>, String)> = Vec::new();
    let mut show_addresses = None;
    for line in unfold_vcard(vcard).lines() {
        let Some((prop, raw_value)) = split_vcard_line(line) else {
            continue;
        };
        let name = property_name(prop).to_ascii_uppercase();
        let params: Vec<&str> = prop.split(';').skip(1).collect();
        let value = unescape_vcard_text(raw_value.trim());
        if value.is_empty() {
            continue;
        }
        match name.as_str() {
            "X-EVOLUTION-LIST" if value.eq_ignore_ascii_case("true") => {
                c.group = Some(GroupFormat::Evolution)
            }
            "X-EVOLUTION-LIST-SHOW-ADDRESSES" => {
                show_addresses = Some(value.eq_ignore_ascii_case("true"))
            }
            "X-ADDRESSBOOKSERVER-KIND" if value.eq_ignore_ascii_case("group") => {
                c.group.get_or_insert(GroupFormat::Apple);
            }
            "KIND" if value.eq_ignore_ascii_case("group") => {
                c.group.get_or_insert(GroupFormat::Kind);
            }
            "MEMBER" | "X-ADDRESSBOOKSERVER-MEMBER" => read_member(&mut c, &value, line),
            "UID" if c.eds_uid.is_empty() => c.eds_uid = value,
            "FN" => c.name = value,
            "NICKNAME" if c.nickname.is_empty() => c.nickname = value,
            "ORG" => {
                // ORG is org;department;… — the org alone reads best.
                c.org = value.split(';').next().unwrap_or("").trim().to_string();
            }
            "TITLE" if c.title.is_empty() => c.title = value,
            "BDAY" => {
                c.birthday = pretty_birthday(&value);
                c.birthday_raw = value;
            }
            "NOTE" if c.note.is_empty() => c.note = value,
            "EMAIL" => {
                email_lines.push((
                    line.to_string(),
                    params.iter().map(|p| p.to_string()).collect(),
                    value.clone(),
                ));
                if value.contains('@') && !c.emails.iter().any(|e| e.value == value) {
                    c.emails.push(Labeled {
                        label: vcard_type_label(&params),
                        value,
                    });
                }
            }
            "TEL" => {
                if !c.phones.iter().any(|p| p.value == value) {
                    c.phones.push(Labeled {
                        label: vcard_type_label(&params),
                        value,
                    });
                }
            }
            "URL" => {
                if !c.urls.contains(&value) {
                    c.urls.push(value);
                }
            }
            "ADR" => read_address(&mut c, raw_value, &params),
            _ => {}
        }
    }
    if c.group.is_some() {
        return finish_group(c, show_addresses, email_lines);
    }
    // A card with no name goes by what else it has, as an address book
    // lists it; one with nothing to go by is skipped.
    if c.name.trim().is_empty() {
        c.name = fallback_name(&c)?;
    }
    Some(c)
}

/// A group member line (`MEMBER`, `X-ADDRESSBOOKSERVER-MEMBER`): an
/// address, or a reference to a card by UID to be looked up later.
fn read_member(c: &mut ContactDetails, value: &str, line: &str) {
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("mailto:") {
        let email = value[7..].trim().to_string();
        if email.contains('@')
            && !c
                .members
                .iter()
                .any(|m| m.email.eq_ignore_ascii_case(&email))
        {
            c.members.push(Member {
                email,
                line: line.to_string(),
                ..Member::default()
            });
        }
    } else {
        let uid = if lower.starts_with("urn:uuid:") {
            &value[9..]
        } else {
            value
        };
        c.member_refs
            .push((uid.trim().to_string(), line.to_string()));
    }
}

/// An `ADR` line, as one line of text and in its parts, unless the card
/// has it already.
fn read_address(c: &mut ContactDetails, raw_value: &str, params: &[&str]) {
    // pobox;extended;street;locality;region;postal-code;country.
    let formatted = raw_value
        .split(';')
        .map(|p| unescape_vcard_text(p.trim()))
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    if !formatted.is_empty() && !c.addresses.iter().any(|a| a.value == formatted) {
        c.addresses.push(Labeled {
            label: vcard_type_label(params),
            value: formatted,
        });
        c.postal
            .push(Postal::from_adr(vcard_type_label(params), raw_value.trim()));
    }
}

/// The rest of a group's card once every line is read: its members from
/// Evolution's `EMAIL` lines, and whether mail to it goes in Bcc. A group
/// with no name is no group.
fn finish_group(
    mut c: ContactDetails,
    show_addresses: Option<bool>,
    email_lines: Vec<(String, Vec<String>, String)>,
) -> Option<ContactDetails> {
    // A group is known by its name: it has no address to fall back on.
    if c.name.trim().is_empty() {
        return None;
    }
    if c.group == Some(GroupFormat::Evolution) {
        // Evolution's own default: a list with no say hides its addresses.
        c.hide_addresses = show_addresses != Some(true);
        for (line, params, value) in email_lines {
            let Some((name, email)) = evolution_member(&params, &value) else {
                continue;
            };
            if !c
                .members
                .iter()
                .any(|m| m.email.eq_ignore_ascii_case(&email))
            {
                c.members.push(Member {
                    name,
                    email,
                    line,
                    ..Member::default()
                });
            }
        }
    } else {
        // Other groups have no say of their own; Hylki writes
        // Evolution's line into an Apple group to keep Send as Bcc.
        c.hide_addresses = show_addresses == Some(false);
    }
    c.emails.clear();
    Some(c)
}

/// What a card with no name is listed as: its address, nickname, company
/// or phone number.
fn fallback_name(c: &ContactDetails) -> Option<String> {
    c.primary_email()
        .map(str::to_string)
        .or_else(|| (!c.nickname.is_empty()).then(|| c.nickname.clone()))
        .or_else(|| (!c.org.is_empty()).then(|| c.org.clone()))
        .or_else(|| c.phones.first().map(|p| p.value.clone()))
}

/// The (name, address) of one member of an Evolution list. The value is a
/// recipient ("Ann Lee <ann@x.org>"); Evolution also keeps the parts as
/// parameters, which are the fallback when the value does not parse.
fn evolution_member(params: &[String], value: &str) -> Option<(String, String)> {
    if let Some((name, email)) = crate::worker::parse_recipients(value).into_iter().next() {
        if email.contains('@') {
            return Some((name, email));
        }
    }
    let param = |key: &str| {
        params.iter().find_map(|p| {
            let (k, v) = p.split_once('=')?;
            k.trim()
                .eq_ignore_ascii_case(key)
                .then(|| unescape_vcard_text(v.trim().trim_matches('"')))
        })
    };
    let email = param("X-EVOLUTION-DEST-EMAIL").filter(|e| e.contains('@'))?;
    Some((param("X-EVOLUTION-DEST-NAME").unwrap_or_default(), email))
}

/// Fill in the group members named by contact UID, from `lookup` (UID →
/// that contact's name and first address, empty when they have none, in the
/// group's own book). A member whose card is not there is not shown, but is
/// kept.
pub(super) fn resolve_members(
    c: &mut ContactDetails,
    lookup: impl Fn(&str) -> Option<(String, String)>,
) {
    for (uid, line) in std::mem::take(&mut c.member_refs) {
        let found = lookup(&uid).filter(|(_, email)| {
            email.is_empty()
                || !c
                    .members
                    .iter()
                    .any(|m| m.email.eq_ignore_ascii_case(email))
        });
        match found {
            Some((name, email)) if !c.members.iter().any(|m| m.uid == uid) => {
                c.members.push(Member {
                    name,
                    email,
                    line,
                    uid,
                    ..Member::default()
                });
            }
            _ => c.hidden_members.push(line),
        }
    }
}

/// The (name, first address or "") of a person's vCard, for
/// [`resolve_members`].
pub(super) fn person_address(vcard: &str) -> Option<(String, String)> {
    let c = parse_vcard_fields(vcard).filter(|c| !c.is_group())?;
    let email = c.primary_email().unwrap_or_default().to_string();
    Some((c.name, email))
}
