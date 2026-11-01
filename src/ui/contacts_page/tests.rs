//! Tests of the contact list.

use crate::contacts::ContactDetails;

use super::menus::matches_query;
use super::rows::same_contacts;
use super::types::ContactFilter;

fn contact(vcard: &str) -> ContactDetails {
    ContactDetails {
        name: "Ann".into(),
        book_uid: "book".into(),
        eds_uid: "ann".into(),
        raw_vcard: vcard.into(),
        ..ContactDetails::default()
    }
}

/// A sync that only stamps the cards anew leaves the list as it is; any
/// real change, a new photo among them, has it listed again (#477).
#[test]
fn a_reread_that_only_moves_rev_is_the_same_list() {
    let card = |rev: &str, tel: &str| {
        format!("BEGIN:VCARD\r\nFN:Ann\r\nREV:{rev}\r\nTEL:{tel}\r\nEND:VCARD")
    };
    let shown = vec![contact(&card("2026-10-18T12:23:02Z", "1"))];
    assert!(same_contacts(
        &shown,
        &[contact(&card("2026-10-18T12:25:15Z", "1"))]
    ));
    assert!(!same_contacts(
        &shown,
        &[contact(&card("2026-10-18T12:25:15Z", "2"))]
    ));
    let mut photo = contact(&card("2026-10-18T12:23:02Z", "1"));
    photo.photo_key = 7;
    assert!(!same_contacts(&shown, &[photo]));
    assert!(!same_contacts(&shown, &[]));
}

/// "group" finds every group whatever its name, and a person only by
/// their own details; the Groups filter keeps the groups alone.
#[test]
fn group_finds_the_groups() {
    let person = contact("");
    let mut club = contact("");
    club.name = "Book Club".into();
    club.group = Some(crate::contacts::GroupFormat::Evolution);
    for q in ["group", "groups", " group "] {
        assert!(matches_query(&club, q), "{q}");
        assert!(!matches_query(&person, q), "{q}");
    }
    assert!(!matches_query(&club, "grou"));
    let none = std::collections::HashSet::new();
    assert!(ContactFilter::Groups.passes(&club, &none));
    assert!(!ContactFilter::Groups.passes(&person, &none));
}
