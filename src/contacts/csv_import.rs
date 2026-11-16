//! Contacts from a CSV file (#655): what Google Contacts, Outlook and
//! Thunderbird export, and any sheet whose first row names its columns in
//! English ("Name", "Email", "Phone"…), matched to contact fields first by
//! their headers and then in the import dialog. Each row becomes a vCard,
//! and the labels or categories a row names become contact groups.

mod fields;

pub(crate) use fields::{can_import, detect, targets, Target};

use super::details::{birthday_parts, birthday_value, Labeled, Postal};
use super::edit::{new_vcard, ContactEdit};
use super::vcard::escape_vcard_value;
use crate::mail::merge::Table;

/// A column that holds one thing about the person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Single {
    Full,
    Given,
    Middle,
    Family,
    Prefix,
    Suffix,
    Nickname,
    Org,
    Dept,
    Title,
    Note,
    Birthday,
    BirthYear,
    BirthMonth,
    BirthDay,
    Groups,
}

/// The kinds of value a person can have several of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Email,
    Phone,
    Url,
    Address,
}

/// A part of a postal address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Part {
    PoBox,
    Extended,
    Street,
    Locality,
    Region,
    Code,
    Country,
    /// The whole address in one cell, used when there are no parts.
    Formatted,
}

/// What a column holds. A value of a set is keyed by its slot: a label
/// column and the value it labels share one, as do the parts of one
/// address ([`fields::resolve`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Col {
    Single(Single),
    Value(Kind, String, &'static str),
    Label(Kind, String),
    Address(String, &'static str, Part),
}

/// A header made comparable: lower case, words split on anything that is
/// not a letter or digit, "e-mail" as "email".
fn norm(header: &str) -> String {
    let spaced: String = header
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();
    let words: Vec<&str> = spaced.split_whitespace().collect();
    words.join(" ").replace("e mail", "email")
}

/// The label a word in a header or a label cell stands for.
fn label_of(text: &str, phone: bool) -> &'static str {
    let t = text.to_lowercase();
    if phone && t.contains("fax") {
        "Fax"
    } else if phone && (t.contains("mobile") || t.contains("cell")) {
        "Mobile"
    } else if t.contains("home") || t.contains("personal") {
        "Home"
    } else if t.contains("work") || t.contains("business") || t.contains("company") {
        "Work"
    } else {
        ""
    }
}

/// What a single-value header names, if it names one.
fn single(h: &str, has_job_title: bool) -> Option<Single> {
    Some(match h {
        "name" | "full name" | "display name" | "contact name" => Single::Full,
        "first name" | "firstname" | "given name" | "forename" => Single::Given,
        "middle name" | "additional name" => Single::Middle,
        "last name" | "lastname" | "family name" | "surname" => Single::Family,
        "name prefix" | "prefix" | "honorific prefix" => Single::Prefix,
        // Outlook's "Title" is Mr or Dr; it has "Job Title" beside it.
        "title" if has_job_title => Single::Prefix,
        "name suffix" | "suffix" | "honorific suffix" => Single::Suffix,
        "nickname" | "nick name" => Single::Nickname,
        "company"
        | "organization"
        | "organisation"
        | "organization name"
        | "organization 1 name"
        | "org" => Single::Org,
        "department" | "organization department" | "organization 1 department" => Single::Dept,
        "job title" | "organization title" | "organization 1 title" | "position" | "title" => {
            Single::Title
        }
        "notes" | "note" | "comments" => Single::Note,
        "birthday" | "birth date" | "birthdate" | "date of birth" => Single::Birthday,
        "birth year" => Single::BirthYear,
        "birth month" => Single::BirthMonth,
        "birth day" => Single::BirthDay,
        "labels" | "group membership" | "categories" | "groups" | "group" => Single::Groups,
        _ => return None,
    })
}

/// A Google header of a numbered set: "email 1 value", "address 2 city".
fn numbered(h: &str) -> Option<Col> {
    let mut words = h.splitn(3, ' ');
    let (kind, n, rest) = (words.next()?, words.next()?, words.next()?);
    if !n.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let kind = match kind {
        "email" => Kind::Email,
        "phone" => Kind::Phone,
        "website" => Kind::Url,
        "address" => Kind::Address,
        _ => return None,
    };
    let slot = n.to_string();
    if matches!(rest, "label" | "type") {
        return Some(Col::Label(kind, slot));
    }
    if kind != Kind::Address {
        return (rest == "value").then_some(Col::Value(kind, slot, ""));
    }
    let part = match rest {
        "street" => Part::Street,
        "city" => Part::Locality,
        "po box" => Part::PoBox,
        "region" => Part::Region,
        "postal code" => Part::Code,
        "country" => Part::Country,
        "extended address" => Part::Extended,
        "formatted" => Part::Formatted,
        _ => return None,
    };
    Some(Col::Address(slot, "", part))
}

/// The address part a header's last words name: "street 2", "zipcode".
fn address_part(rest: &str) -> Option<Part> {
    Some(match rest {
        "street" | "street 2" | "street 3" | "address" | "address 2" | "street address" => {
            Part::Street
        }
        "city" | "town" => Part::Locality,
        "state" | "province" | "region" | "county" => Part::Region,
        "postal code" | "postcode" | "zip" | "zip code" | "zipcode" => Part::Code,
        "country" | "country region" => Part::Country,
        "po box" => Part::PoBox,
        _ => return None,
    })
}

/// What a header names. `None` for a column nothing here reads.
fn classify(h: &str, has_job_title: bool) -> Option<Col> {
    if let Some(s) = single(h, has_job_title) {
        return Some(Col::Single(s));
    }
    if let Some(col) = numbered(h) {
        return Some(col);
    }
    let words: Vec<&str> = h.split(' ').collect();
    let first = words.first().copied().unwrap_or("");
    // Addresses: "Home Street", "Business Postal Code", "Work ZipCode",
    // or a part alone for one address with no label.
    if let Some(label) = match first {
        "home" => Some("Home"),
        "business" | "work" => Some("Work"),
        "other" => Some(""),
        _ => None,
    } {
        if let Some(part) = address_part(&words[1..].join(" ")) {
            return Some(Col::Address(first.to_string(), label, part));
        }
    }
    if let Some(part) = address_part(h) {
        return Some(Col::Address(String::new(), "", part));
    }
    // Email: "Email", "E-mail 2 Address", "Primary Email", "Work Email".
    if words.contains(&"email") {
        if h.contains("type") || h.contains("display") {
            return None;
        }
        return Some(Col::Value(Kind::Email, h.to_string(), label_of(h, false)));
    }
    if words.iter().any(|w| {
        matches!(
            *w,
            "phone" | "telephone" | "tel" | "mobile" | "cell" | "fax" | "pager"
        )
    }) {
        // The assistant's number is not the person's.
        if first == "assistant" {
            return None;
        }
        return Some(Col::Value(Kind::Phone, h.to_string(), label_of(h, true)));
    }
    if matches!(h, "url" | "website" | "web site" | "homepage" | "home page")
        || h.starts_with("web page")
        || h.ends_with("web page")
    {
        return Some(Col::Value(Kind::Url, h.to_string(), ""));
    }
    None
}

/// What each column of `table` holds. A sheet whose headers name no
/// address takes the column of email addresses for one.
fn columns(table: &Table) -> Vec<Option<Col>> {
    let headers: Vec<String> = table.columns.iter().map(|c| norm(c)).collect();
    let has_job_title = headers.iter().any(|h| h == "job title");
    let mut cols: Vec<Option<Col>> = headers.iter().map(|h| classify(h, has_job_title)).collect();
    if !cols
        .iter()
        .any(|c| matches!(c, Some(Col::Value(Kind::Email, ..))))
    {
        for c in table.address_columns() {
            if cols[c].is_none() {
                cols[c] = Some(Col::Value(Kind::Email, c.to_string(), ""));
            }
        }
    }
    cols
}

/// The values in a cell: Google puts several in one, split by ":::".
fn values(cell: &str) -> impl Iterator<Item = &str> {
    cell.split(":::").map(str::trim).filter(|v| !v.is_empty())
}

/// The group names in a cell: Google's labels split by ":::", less the
/// ones it keeps for itself ("* myContacts", "* starred"); Outlook's
/// categories split by ";"; else by ",".
fn group_names(cell: &str) -> Vec<String> {
    let sep = if cell.contains(":::") {
        ":::"
    } else if cell.contains(';') {
        ";"
    } else {
        ","
    };
    cell.split(sep)
        .map(str::trim)
        .filter(|g| !g.is_empty() && !g.starts_with('*'))
        .map(str::to_string)
        .collect()
}

/// A birthday as a BDAY value: ISO dates, Outlook's month/day/year (or
/// day/month when the first is over 12), and day.month.year. A year of
/// fewer than four digits is left out rather than guessed.
fn birthday(cell: &str) -> Option<String> {
    // Outlook may add a time of day.
    let cell = cell.split_whitespace().next()?;
    if let Some((year, month, day)) = birthday_parts(cell) {
        return Some(birthday_value(year, month, day));
    }
    let sep = ['/', '.', '-'].into_iter().find(|&s| cell.contains(s))?;
    let parts: Vec<&str> = cell.split(sep).map(str::trim).collect();
    let [a, b, c] = parts[..] else {
        return None;
    };
    let num = |s: &str| s.parse::<u32>().ok();
    let (year, month, day) = if a.len() == 4 {
        (a, num(b)?, num(c)?)
    } else if sep == '.' || num(a)? > 12 {
        (c, num(b)?, num(a)?)
    } else {
        (c, num(a)?, num(b)?)
    };
    dated(year, month, day)
}

/// A BDAY value from a year (empty or short for none), month and day.
fn dated(year: &str, month: u32, day: u32) -> Option<String> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year = (year.len() == 4)
        .then(|| year.parse::<i32>().ok())
        .flatten()
        .filter(|&y| y > 0);
    Some(birthday_value(year, month, day))
}

/// One value of a set, as a row fills it in.
#[derive(Default)]
struct Slot {
    kind: Option<Kind>,
    key: String,
    label: String,
    values: Vec<String>,
    postal: Postal,
    formatted: String,
}

/// The contacts of a CSV file, ready to store.
#[derive(Debug, Default)]
pub(crate) struct CsvContacts {
    pub(crate) cards: Vec<String>,
    /// Each group the rows name, with its members as indexes into `cards`.
    pub(crate) groups: Vec<(String, Vec<usize>)>,
    /// Rows that name nobody: no name, address, phone or company.
    pub(crate) skipped: usize,
}

/// The contacts of `table`, its columns matched as `targets` says.
pub(crate) fn contacts_from(table: &Table, targets: &[Target]) -> CsvContacts {
    let cols = fields::resolve(targets);
    let mut out = CsvContacts::default();
    for row in &table.rows {
        let Some((card, groups)) = row_card(&cols, row) else {
            out.skipped += 1;
            continue;
        };
        let at = out.cards.len();
        out.cards.push(card);
        for g in groups {
            match out
                .groups
                .iter_mut()
                .find(|(name, _)| name.eq_ignore_ascii_case(&g))
            {
                Some((_, members)) => members.push(at),
                None => out.groups.push((g, vec![at])),
            }
        }
    }
    out
}

/// The contacts of a CSV file, its columns matched by their headers.
#[cfg(test)]
fn csv_contacts(bytes: &[u8]) -> Result<CsvContacts, String> {
    let table = crate::mail::merge::parse_csv(bytes)?;
    let targets = detect(&table);
    if !can_import(&targets) {
        return Err("nothing names anyone".into());
    }
    Ok(contacts_from(&table, &targets))
}

/// One row's vCard and the groups it names; `None` for a row that names
/// nobody.
fn row_card(cols: &[Option<Col>], row: &[String]) -> Option<(String, Vec<String>)> {
    let mut one = std::collections::HashMap::new();
    let mut groups = Vec::new();
    let mut slots: Vec<Slot> = Vec::new();
    let mut slot = |kind: Kind, key: &str| -> usize {
        match slots
            .iter()
            .position(|s| s.kind == Some(kind) && s.key == key)
        {
            Some(i) => i,
            None => {
                slots.push(Slot {
                    kind: Some(kind),
                    key: key.to_string(),
                    ..Slot::default()
                });
                slots.len() - 1
            }
        }
    };
    let mut filled: Vec<(usize, &Col, &str)> = Vec::new();
    for (col, cell) in cols.iter().zip(row) {
        let (Some(col), cell) = (col, cell.trim()) else {
            continue;
        };
        if cell.is_empty() {
            continue;
        }
        let at = match col {
            Col::Single(Single::Groups) => {
                for g in group_names(cell) {
                    if !groups.iter().any(|x: &String| x.eq_ignore_ascii_case(&g)) {
                        groups.push(g);
                    }
                }
                continue;
            }
            Col::Single(s) => {
                one.entry(*s).or_insert_with(|| cell.to_string());
                continue;
            }
            Col::Value(kind, key, _) | Col::Label(kind, key) => slot(*kind, key),
            Col::Address(key, _, _) => slot(Kind::Address, key),
        };
        filled.push((at, col, cell));
    }
    for (at, col, cell) in filled {
        let s = &mut slots[at];
        match col {
            Col::Value(_, _, label) => {
                if s.label.is_empty() {
                    s.label = label.to_string();
                }
                s.values.extend(values(cell).map(str::to_string));
            }
            Col::Label(kind, _) => {
                s.label = label_of(cell, *kind == Kind::Phone).to_string();
            }
            Col::Address(_, label, part) => {
                if s.label.is_empty() {
                    s.label = label.to_string();
                }
                let p = &mut s.postal;
                let target = match part {
                    Part::PoBox => &mut p.pobox,
                    Part::Extended => &mut p.extended,
                    Part::Street => &mut p.street,
                    Part::Locality => &mut p.locality,
                    Part::Region => &mut p.region,
                    Part::Code => &mut p.code,
                    Part::Country => &mut p.country,
                    Part::Formatted => &mut s.formatted,
                };
                // Outlook's Street 2 and 3 are more lines of the street.
                if !target.is_empty() {
                    target.push('\n');
                }
                target.push_str(cell);
            }
            Col::Single(_) => {}
        }
    }

    let get = |s: Single| one.get(&s).map(String::as_str).unwrap_or("");
    let mut edit = ContactEdit {
        nickname: get(Single::Nickname).to_string(),
        org: get(Single::Org).to_string(),
        title: get(Single::Title).to_string(),
        note: get(Single::Note).to_string(),
        ..ContactEdit::default()
    };
    let parts = [
        get(Single::Prefix),
        get(Single::Given),
        get(Single::Middle),
        get(Single::Family),
        get(Single::Suffix),
    ];
    edit.name = match get(Single::Full) {
        "" => parts
            .iter()
            .filter(|p| !p.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" "),
        full => full.to_string(),
    };
    edit.birthday = birthday(get(Single::Birthday))
        .or_else(|| {
            let n = |s| get(s).trim().parse::<u32>().ok();
            dated(
                get(Single::BirthYear).trim(),
                n(Single::BirthMonth)?,
                n(Single::BirthDay)?,
            )
        })
        .unwrap_or_default();
    for mut s in slots {
        let labeled = |v: &String| Labeled {
            label: s.label.clone(),
            value: v.clone(),
        };
        match s.kind {
            Some(Kind::Email) => push_new(&mut edit.emails, s.values.iter().map(labeled)),
            Some(Kind::Phone) => push_new(&mut edit.phones, s.values.iter().map(labeled)),
            Some(Kind::Url) => {
                for v in s.values {
                    if !edit.urls.contains(&v) {
                        edit.urls.push(v);
                    }
                }
            }
            Some(Kind::Address) => {
                if s.postal.is_empty() && !s.formatted.is_empty() {
                    s.postal.street = std::mem::take(&mut s.formatted);
                }
                if !s.postal.is_empty() {
                    s.postal.label = s.label;
                    edit.addresses.push(s.postal);
                }
            }
            None => {}
        }
    }
    let someone = [&edit.name, &edit.nickname, &edit.org]
        .iter()
        .any(|v| !v.is_empty())
        || !edit.emails.is_empty()
        || !edit.phones.is_empty();
    if !someone {
        return None;
    }

    let mut card = new_vcard(&edit);
    if parts.iter().any(|p| !p.is_empty()) {
        let [prefix, given, middle, family, suffix] = parts.map(escape_vcard_value);
        card = replace_line(
            &card,
            "N:",
            &format!("N:{family};{given};{middle};{prefix};{suffix}"),
        );
    }
    let dept = get(Single::Dept);
    if !dept.is_empty() {
        let org = escape_vcard_value(&edit.org);
        let line = format!("ORG:{org};{}", escape_vcard_value(dept));
        card = if edit.org.is_empty() {
            replace_line(&card, "END:VCARD", &format!("{line}\r\nEND:VCARD"))
        } else {
            replace_line(&card, "ORG:", &line)
        };
    }
    if !groups.is_empty() {
        card = super::categories::with_categories(&card, &groups);
    }
    Some((card, groups))
}

/// Add each of `new` that `list` does not have yet (the same address in
/// two columns is one).
fn push_new(list: &mut Vec<Labeled>, new: impl Iterator<Item = Labeled>) {
    for v in new {
        if !list.iter().any(|l| l.value.eq_ignore_ascii_case(&v.value)) {
            list.push(v);
        }
    }
}

/// `card` with its first line that starts with `start` replaced by `line`.
fn replace_line(card: &str, start: &str, line: &str) -> String {
    let mut done = false;
    card.split("\r\n")
        .map(|l| {
            if !done && l.starts_with(start) {
                done = true;
                line.to_string()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

#[cfg(test)]
#[path = "csv_import_tests.rs"]
mod tests;
