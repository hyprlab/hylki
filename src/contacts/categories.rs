//! Contact groups as Nextcloud keeps them (#652). Such a group has no card
//! of its own: it is a name in the `CATEGORIES` of each member's card, so
//! the Nextcloud web app, DAVx⁵ and Thunderbird show the same groups. Hylki
//! lists one stand-in entry per name, and writes a change to the group as
//! changes to its members' cards.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::details::{ContactDetails, GroupFormat, Member};
use super::edit::{kept_lines_by, BookCards, GroupEdit};
use super::eds_write::{book_call, book_get_contact, book_get_uids, sexpr_escape};
use super::vcard::{
    escape_vcard_value, property_name, split_unescaped, split_vcard_line, unescape_vcard_text,
    unfold_vcard,
};
use crate::i18n::i18n;

/// What a category group's UID starts with: the group has none of its own,
/// and Hylki needs one to find it by.
const UID_PREFIX: &str = "hylki-category:";

/// The UID Hylki gives the group named `name`.
pub(crate) fn group_uid(name: &str) -> String {
    format!("{UID_PREFIX}{name}")
}

/// The name of the category group a UID stands for, if it stands for one.
pub(crate) fn category_of(uid: &str) -> Option<&str> {
    uid.strip_prefix(UID_PREFIX)
}

/// The stand-in card of a category group. Nothing writes it to a book: it
/// only carries the group's name to [`save_category_group`] as the group
/// was when its form opened.
fn stand_in(name: &str) -> String {
    format!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{}\r\nFN:{}\r\nEND:VCARD\r\n",
        group_uid(name),
        escape_vcard_value(name)
    )
}

/// The group name a stand-in card carries.
fn stand_in_name(vcard: &str) -> Option<String> {
    unfold_vcard(vcard).lines().find_map(|line| {
        let (prop, value) = split_vcard_line(line)?;
        property_name(prop)
            .eq_ignore_ascii_case("UID")
            .then(|| category_of(value.trim()).map(str::to_string))
            .flatten()
    })
}

/// The categories on a card, in order, each once. A card may have several
/// `CATEGORIES` lines, each with several names split by commas.
pub(crate) fn card_categories(vcard: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in unfold_vcard(vcard).lines() {
        let Some((prop, value)) = split_vcard_line(line) else {
            continue;
        };
        if !property_name(prop).eq_ignore_ascii_case("CATEGORIES") {
            continue;
        }
        for part in split_unescaped(value.trim(), ',') {
            let name = unescape_vcard_text(part).trim().to_string();
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// A card with its categories set to `names`, and no `CATEGORIES` line
/// when there are none.
pub(super) fn with_categories(vcard: &str, names: &[String]) -> String {
    let mut out = kept_lines_by(vcard, |prop| prop == "CATEGORIES");
    if !names.is_empty() {
        let values: Vec<String> = names.iter().map(|n| escape_vcard_value(n)).collect();
        out.push(format!("CATEGORIES:{}", values.join(",")));
    }
    out.push("END:VCARD".to_string());
    out.join("\r\n") + "\r\n"
}

/// The category groups of one book, made from its people's cards: one per
/// name, holding everyone whose card has it. `bcc` names those whose mail
/// goes in Bcc, which a category cannot say.
pub(super) fn category_groups(
    people: &[ContactDetails],
    book_uid: &str,
    book_name: &str,
    bcc: &HashSet<String>,
) -> Vec<ContactDetails> {
    let mut groups: BTreeMap<String, Vec<Member>> = BTreeMap::new();
    for p in people
        .iter()
        .filter(|p| !p.is_group() && !p.eds_uid.is_empty())
    {
        for name in card_categories(&p.raw_vcard) {
            groups.entry(name).or_default().push(Member {
                name: p.name.clone(),
                email: p.primary_email().unwrap_or_default().to_string(),
                uid: p.eds_uid.clone(),
                ..Member::default()
            });
        }
    }
    groups
        .into_iter()
        .map(|(name, members)| ContactDetails {
            raw_vcard: stand_in(&name),
            eds_uid: group_uid(&name),
            hide_addresses: bcc.contains(&name),
            name,
            group: Some(GroupFormat::Categories),
            members,
            book_uid: book_uid.to_string(),
            book_name: book_name.to_string(),
            ..ContactDetails::default()
        })
        .collect()
}

/// Add the category groups of the books that keep them to `out`, which
/// holds every card read, each with its book.
pub(super) fn add_category_groups(
    out: &mut Vec<ContactDetails>,
    formats: &HashMap<String, Option<GroupFormat>>,
) {
    let books: HashSet<&str> = formats
        .iter()
        .filter(|(_, f)| **f == Some(GroupFormat::Categories))
        .map(|(uid, _)| uid.as_str())
        .collect();
    if books.is_empty() {
        return;
    }
    let bcc = crate::contacts::contact_sources::bcc_groups();
    let mut by_book: BTreeMap<(String, String), Vec<ContactDetails>> = BTreeMap::new();
    for c in out.iter().filter(|c| books.contains(c.book_uid.as_str())) {
        by_book
            .entry((c.book_uid.clone(), c.book_name.clone()))
            .or_default()
            .push(c.clone());
    }
    let none = HashSet::new();
    for ((uid, name), people) in by_book {
        let bcc = bcc.get(&uid).unwrap_or(&none);
        out.extend(category_groups(&people, &uid, &name, bcc));
    }
}

/// Write a category group to its book: each member's card gets the name,
/// and any other card that has it loses it; a rename takes the old name
/// (from `original`, the group's stand-in) off every card. A member from
/// another book gets a card here first, without that book's categories.
/// Blocking: call from a background thread.
pub(super) fn save_category_group(
    book_uid: &str,
    original: Option<&str>,
    mut edit: GroupEdit,
) -> Result<(), String> {
    let name = edit.name.trim().to_string();
    if name.is_empty() {
        return Err(i18n("A group needs a name."));
    }
    let old = original.and_then(stand_in_name);
    let mut cards = BookCards::open(book_uid)?;
    let cardless = |m: &Member| m.uid.is_empty() && (!m.email.is_empty() || !m.card.is_empty());
    for m in edit.members.iter_mut().filter(|m| cardless(m)) {
        let copy = if m.card.is_empty() {
            String::new()
        } else {
            with_categories(&m.card, &[])
        };
        m.uid = cards.card_for(&m.name, &m.email, &copy)?;
    }
    let wanted: HashSet<String> = edit
        .members
        .iter()
        .filter(|m| !m.uid.is_empty())
        .map(|m| m.uid.clone())
        .collect();
    if original.is_none() && wanted.is_empty() {
        return Err(i18n(
            "A group in this address book is kept on its people's cards, so it needs at least one person.",
        ));
    }
    let BookCards::Eds { conn, bus, path } = &cards else {
        return Err(i18n("Hylki cannot change this kind of group"));
    };
    // The cards that may change: the members, and whoever has the name or
    // the old one now.
    let mut touched: Vec<String> = wanted.iter().cloned().collect();
    for n in old.iter().chain(std::iter::once(&name)) {
        let query = format!("(is \"category_list\" \"{}\")", sexpr_escape(n));
        touched.extend(book_get_uids(conn, bus, path, &query)?);
    }
    touched.sort();
    touched.dedup();
    let mut changed = Vec::new();
    for uid in &touched {
        let vcard = book_get_contact(conn, bus, path, uid)?;
        let before = card_categories(&vcard);
        let after = categories_after(&before, old.as_deref(), &name, wanted.contains(uid));
        if after != before {
            changed.push(with_categories(&vcard, &after));
        }
    }
    if !changed.is_empty() {
        book_call(conn, bus, path, "ModifyContacts", &(changed, 0u32))?;
    }
    crate::contacts::contact_sources::set_bcc_group(
        book_uid,
        old.as_deref(),
        &name,
        edit.hide_addresses && !wanted.is_empty(),
    )
}

/// A card's categories after a save of the group `name`, once `old`: the
/// old name gone, and the new one there only for a member.
fn categories_after(before: &[String], old: Option<&str>, name: &str, member: bool) -> Vec<String> {
    let mut after: Vec<String> = before
        .iter()
        .filter(|c| {
            (Some(c.as_str()) != old || c.as_str() == name) && (member || c.as_str() != name)
        })
        .cloned()
        .collect();
    if member && !after.iter().any(|c| c == name) {
        after.push(name.to_string());
    }
    after
}

/// Delete the category group `name` from a book: the name comes off every
/// card that has it, and the cards stay.
pub(super) fn delete_category_group(book_uid: &str, name: &str) -> Result<(), String> {
    let edit = GroupEdit {
        name: name.to_string(),
        ..GroupEdit::default()
    };
    save_category_group(book_uid, Some(&stand_in(name)), edit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(uid: &str, name: &str, email: &str, categories: &str) -> ContactDetails {
        let mut card = format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{name}\r\n");
        if !email.is_empty() {
            card.push_str(&format!("EMAIL:{email}\r\n"));
        }
        if !categories.is_empty() {
            card.push_str(&format!("CATEGORIES:{categories}\r\n"));
        }
        card.push_str("END:VCARD\r\n");
        let mut c = super::super::parse_vcard_fields(&card).unwrap();
        c.raw_vcard = card;
        c
    }

    #[test]
    fn reads_categories() {
        let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nCATEGORIES:Family,Book Club\\, North\r\n\
                    categories:Family,Work\r\nEND:VCARD\r\n";
        assert_eq!(
            card_categories(card),
            vec!["Family", "Book Club, North", "Work"]
        );
        assert!(card_categories("BEGIN:VCARD\r\nCATEGORIES:\r\nEND:VCARD\r\n").is_empty());
    }

    #[test]
    fn writes_categories_in_one_line() {
        let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:a\r\nCATEGORIES:Old\r\nFN:Ann\r\n\
                    CATEGORIES:Other\r\nEND:VCARD\r\n";
        let names = vec!["Family".to_string(), "Book Club, North".to_string()];
        let out = with_categories(card, &names);
        assert_eq!(
            out,
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:a\r\nFN:Ann\r\n\
             CATEGORIES:Family,Book Club\\, North\r\nEND:VCARD\r\n"
        );
        assert_eq!(card_categories(&out), names);
        assert!(!with_categories(card, &[]).contains("CATEGORIES"));
    }

    #[test]
    fn groups_from_cards() {
        let people = vec![
            person("1", "Ann", "ann@x.org", "Family,Work"),
            person("2", "Bob", "", "Family"),
            person("3", "Cy", "cy@x.org", ""),
        ];
        let bcc = HashSet::from(["Work".to_string()]);
        let groups = category_groups(&people, "nc", "Nextcloud", &bcc);
        assert_eq!(groups.len(), 2);
        let family = &groups[0];
        assert_eq!(family.name, "Family");
        assert_eq!(family.group, Some(GroupFormat::Categories));
        assert_eq!(family.eds_uid, "hylki-category:Family");
        assert_eq!(family.book_uid, "nc");
        assert!(family.group_editable() && !family.hide_addresses);
        let uids: Vec<&str> = family.members.iter().map(|m| m.uid.as_str()).collect();
        assert_eq!(uids, vec!["1", "2"]);
        // Bob has only a phone or nothing: a member all the same, not
        // someone to send to.
        assert_eq!(family.reachable().count(), 1);
        assert!(groups[1].hide_addresses);
        assert_eq!(stand_in_name(&family.raw_vcard).as_deref(), Some("Family"));
    }

    #[test]
    fn stand_in_keeps_odd_names() {
        let name = "Friends, old; and new";
        let card = stand_in(name);
        assert_eq!(stand_in_name(&card).as_deref(), Some(name));
        assert_eq!(category_of(&group_uid(name)), Some(name));
        assert_eq!(category_of("4f1c-real-uid"), None);
    }

    #[test]
    fn categories_after_a_save() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let before = s(&["Family", "Work"]);
        // Joins, leaves, stays.
        assert_eq!(
            categories_after(&s(&["Work"]), None, "Family", true),
            s(&["Work", "Family"])
        );
        assert_eq!(
            categories_after(&before, None, "Family", false),
            s(&["Work"])
        );
        assert_eq!(categories_after(&before, None, "Family", true), before);
        // Renamed: the old name goes, the new one comes for members only.
        assert_eq!(
            categories_after(&before, Some("Family"), "Kin", true),
            s(&["Work", "Kin"])
        );
        assert_eq!(
            categories_after(&before, Some("Family"), "Kin", false),
            s(&["Work"])
        );
        // Deleted: an empty group of the same name.
        assert_eq!(
            categories_after(&before, Some("Family"), "Family", false),
            s(&["Work"])
        );
    }

    /// CATEGORY_LIVE=<book uid>: in a sealed rig whose CardDAV book is under
    /// /remote.php/ and holds Ann (Family, Work), Bob (Family, no address)
    /// and Cy (none), rename Family to Kin with Ann, Cy and a new Dan, then
    /// delete Kin. Checks each step through EDS and Hylki's own reads.
    #[test]
    #[ignore]
    fn live() {
        let Ok(book) = std::env::var("CATEGORY_LIVE") else {
            return;
        };
        let dest = super::super::factory_dest().unwrap();
        let conn = zbus::blocking::Connection::session().unwrap();
        let (bus, path) = super::super::open_book(&conn, &dest, &book).unwrap();
        book_call(&conn, &bus, &path, "Refresh", &()).unwrap();
        let all = || book_get_uids(&conn, &bus, &path, "").unwrap();
        for _ in 0..40 {
            if all().len() >= 3 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let cats = |uid: &str| card_categories(&book_get_contact(&conn, &bus, &path, uid).unwrap());
        let books = super::super::writable_books();
        println!("books: {books:?}");
        let ours = books.iter().find(|b| b.uid == book).expect("the book");
        assert_eq!(ours.groups, Some(GroupFormat::Categories));
        let details = super::super::read_contact_details();
        let family = details
            .iter()
            .find(|c| c.is_group() && c.book_uid == book && c.name == "Family")
            .expect("the Family group")
            .clone();
        let names: Vec<&str> = family.members.iter().map(|m| m.name.as_str()).collect();
        println!("Family: {names:?}");
        assert_eq!(names, vec!["Ann", "Bob"]);
        let uid_of = |n: &str| {
            details
                .iter()
                .find(|c| !c.is_group() && c.book_uid == book && c.name == n)
                .unwrap()
                .eds_uid
                .clone()
        };
        let (ann, bob, cy) = (uid_of("Ann"), uid_of("Bob"), uid_of("Cy"));
        // The composer's suggestions see it too.
        let (active, formats) = super::super::registry_books_and_formats();
        let groups = super::super::read_groups_in(&active, &formats);
        assert!(groups
            .iter()
            .any(|g| g.name == "Family" && g.members.len() == 2));

        let member = |uid: &str| Member {
            uid: uid.to_string(),
            ..Member::default()
        };
        let edit = GroupEdit {
            name: "Kin".into(),
            members: vec![
                member(&ann),
                member(&cy),
                Member {
                    name: "Dan".into(),
                    email: "dan@example.org".into(),
                    ..Member::default()
                },
            ],
            hide_addresses: true,
            kept: Vec::new(),
        };
        super::super::save_group(
            &book,
            GroupFormat::Categories,
            Some(&family.raw_vcard),
            edit,
        )
        .unwrap();
        assert_eq!(cats(&ann), vec!["Work", "Kin"]);
        assert!(cats(&bob).is_empty());
        assert_eq!(cats(&cy), vec!["Kin"]);
        let dan = book_get_uids(&conn, &bus, &path, "(is \"email\" \"dan@example.org\")")
            .unwrap()
            .pop()
            .expect("Dan's card");
        assert_eq!(cats(&dan), vec!["Kin"]);
        let bcc = crate::contacts::contact_sources::bcc_groups();
        assert!(bcc.get(&book).is_some_and(|n| n.contains("Kin")));

        super::super::delete_contacts(&book, &[group_uid("Kin")]).unwrap();
        assert_eq!(cats(&ann), vec!["Work"]);
        assert!(cats(&cy).is_empty() && cats(&dan).is_empty());
        assert!(!crate::contacts::contact_sources::bcc_groups().contains_key(&book));
        // A new group needs someone in it.
        let empty = GroupEdit {
            name: "Nobody".into(),
            ..GroupEdit::default()
        };
        assert!(super::super::save_group(&book, GroupFormat::Categories, None, empty).is_err());
        println!("live categories: ok");
    }
}
