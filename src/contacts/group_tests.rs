//! Tests of contact groups: reading their members, editing them, and the
//! live EDS round trips (ignored unless EDS runs).

use super::tests::ONE_PIXEL_PNG;

/// An Evolution list as EDS 3.60 stores it: folded inside a parameter,
/// commas escaped in quoted values too.
const EDS_LIST: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:l1\r\nFN:Book Club\r\nN:;;;;\r\n\
    X-EVOLUTION-FILE-AS:Book Club\r\nX-EVOLUTION-LIST:TRUE\r\nX-EVOLUTION-LIST-SHOW-ADDRESSES:FALSE\r\n\
    EMAIL;X-EVOLUTION-DEST-HTML-MAIL=FALSE;X-EVOLUTION-DEST-EMAIL=\"alice@exampl\r\n e.org\";\
    X-EVOLUTION-DEST-NAME=\"Alice Able\";X-EVOLUTION-DEST-EMAIL-NUM=0;X-E\r\n VOLUTION-DEST-CONTACT-UID=p1:\
    Alice Able <alice@example.org>\r\n\
    EMAIL;X-EVOLUTION-DEST-EMAIL=\"bob@example.org\";X-EVOLUTION-DEST-NAME=\"Bob\\,\r\n  Jr.\":\
    \"Bob\\, Jr.\" <bob@example.org>\r\nREV:2026-10-17T12:15:22Z\r\nEND:VCARD\r\n";

fn people(c: &super::ContactDetails) -> Vec<(&str, &str)> {
    c.members
        .iter()
        .map(|m| (m.name.as_str(), m.email.as_str()))
        .collect()
}

#[test]
fn an_evolution_list_is_a_group_of_its_members() {
    let c = super::parse_vcard_fields(EDS_LIST).unwrap();
    assert_eq!(c.group, Some(super::GroupFormat::Evolution));
    assert_eq!(c.name, "Book Club");
    assert_eq!(
        people(&c),
        vec![
            ("Alice Able", "alice@example.org"),
            ("Bob, Jr.", "bob@example.org")
        ]
    );
    // Its members are not its own addresses.
    assert!(c.emails.is_empty());
    assert!(c.hide_addresses);
    assert!(c.group_editable());
    assert!(super::is_group_vcard(EDS_LIST));
    let shown = EDS_LIST.replace("SHOW-ADDRESSES:FALSE", "SHOW-ADDRESSES:TRUE");
    assert!(!super::parse_vcard_fields(&shown).unwrap().hide_addresses);
}

#[test]
fn a_member_whose_value_does_not_parse_comes_from_its_parameters() {
    let v = "BEGIN:VCARD\r\nFN:L\r\nX-EVOLUTION-LIST:TRUE\r\n\
             EMAIL;X-EVOLUTION-DEST-NAME=\"Ann\";X-EVOLUTION-DEST-EMAIL=\"ann@x.org\":Ann\r\nEND:VCARD\r\n";
    assert_eq!(
        people(&super::parse_vcard_fields(v).unwrap()),
        vec![("Ann", "ann@x.org")]
    );
}

#[test]
fn a_group_needs_a_name() {
    let v = "BEGIN:VCARD\r\nX-EVOLUTION-LIST:TRUE\r\nEMAIL:ann@x.org\r\nEND:VCARD\r\n";
    assert!(super::parse_vcard_fields(v).is_none());
}

#[test]
fn kind_and_apple_groups_find_their_members_in_their_book() {
    let person =
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:p1\r\nFN:Pat Doe\r\nEMAIL:pat@x.org\r\nEND:VCARD\r\n";
    let kind = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:g1\r\nKIND:group\r\nFN:Team\r\n\
                MEMBER:mailto:ann@x.org\r\nMEMBER:urn:uuid:p1\r\nMEMBER:urn:uuid:gone\r\nEND:VCARD\r\n";
    let apple =
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:g2\r\nX-ADDRESSBOOKSERVER-KIND:group\r\nN:Family\r\n\
                 FN:Family\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:p1\r\nEND:VCARD\r\n";
    let mut book: Vec<super::ContactDetails> = [person, kind, apple]
        .iter()
        .map(|v| super::parse_vcard_fields(v).unwrap())
        .collect();
    super::resolve_book_members(&mut book);
    assert_eq!(
        people(&book[1]),
        vec![("", "ann@x.org"), ("Pat Doe", "pat@x.org")]
    );
    assert_eq!(people(&book[2]), vec![("Pat Doe", "pat@x.org")]);
    // Neither has Evolution's Bcc switch; Hylki edits Apple's only.
    assert!(!book[1].hide_addresses && !book[1].group_editable());
    assert!(!book[2].hide_addresses && book[2].group_editable());
    assert_eq!(book[2].group, Some(super::GroupFormat::Apple));
    // The member with no card in the book is kept, out of sight.
    assert_eq!(
        book[1].hidden_members,
        vec!["MEMBER:urn:uuid:gone".to_string()]
    );
    assert!(super::is_group_vcard(apple));
    assert!(!super::is_group_vcard(person));
}

#[test]
fn a_moved_card_leaves_its_old_book_behind() {
    let c = super::ContactDetails {
        raw_vcard: "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:abc\r\nFN:Pat Doe\r\nEMAIL:pat@x.org\r\n\
                    PHOTO;VALUE=uri:file:///home/u/.cache/evolution/addressbook/b/photos/abc.jpg\r\n\
                    X-EVOLUTION-WEBDAV-ETAG:k4lwse3r\r\nREV:2026-10-18T09:00:00Z\r\nNOTE:Long note that\r\n \
                    goes on\r\nEND:VCARD\r\n"
            .into(),
        photo: Some(vec![0x89, b'P', b'N', b'G', 1, 2]),
        ..Default::default()
    };
    let out = super::moved_vcard(&c);
    assert!(
        !out.contains("UID:") && !out.contains("REV:") && !out.contains("WEBDAV"),
        "{out}"
    );
    assert!(!out.contains("file://"), "{out}");
    assert!(
        out.contains("PHOTO;ENCODING=b;TYPE=PNG:iVBORwEC\r\n"),
        "{out}"
    );
    assert!(out.contains("NOTE:Long note that\r\n goes on\r\n"), "{out}");
    assert!(out.ends_with("END:VCARD\r\n"));
    let back = super::parse_vcard_fields(&out).unwrap();
    assert_eq!(
        (back.name.as_str(), back.primary_email()),
        ("Pat Doe", Some("pat@x.org"))
    );
}

#[test]
fn an_apple_group_shows_people_with_no_address() {
    let person =
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:p1\r\nFN:Pat Doe\r\nTEL:+1 555 0100\r\nEND:VCARD\r\n";
    let mailed =
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:p2\r\nFN:Kim\r\nEMAIL:kim@x.org\r\nEND:VCARD\r\n";
    let apple = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:g\r\nX-ADDRESSBOOKSERVER-KIND:group\r\nFN:Mission\r\n\
                 X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:p1\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:p2\r\n\
                 X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:p1\r\nEND:VCARD\r\n";
    let mut book: Vec<super::ContactDetails> = [person, mailed, apple]
        .iter()
        .map(|v| super::parse_vcard_fields(v).unwrap())
        .collect();
    super::resolve_book_members(&mut book);
    let g = &book[2];
    assert_eq!(people(g), vec![("Pat Doe", ""), ("Kim", "kim@x.org")]);
    assert_eq!(
        g.hidden_members.len(),
        1,
        "the repeated card is kept, once shown"
    );
    // Mail goes to those with an address only.
    assert_eq!(g.members_field(), "Kim <kim@x.org>");
    assert_eq!(
        super::group_suggestions(vec![g.clone()])[0]
            .group
            .as_ref()
            .unwrap()
            .members
            .len(),
        1
    );
}

#[test]
fn an_edited_apple_group_names_its_members_by_card() {
    let apple = "BEGIN:VCARD\r\nVERSION:3.0\r\nPRODID:-//Apple Inc.//iOS 18//EN\r\nUID:g2\r\n\
                 X-ADDRESSBOOKSERVER-KIND:group\r\nN:Family\r\nFN:Family\r\n\
                 X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:p1\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:p2\r\n\
                 X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:nomail\r\nREV:2026-10-18T09:00:00Z\r\nEND:VCARD\r\n";
    let mut g = super::parse_vcard_fields(apple).unwrap();
    super::resolve_members(&mut g, |uid| match uid {
        "p1" => Some(("Pat".into(), "pat@x.org".into())),
        "p2" => Some(("Kim".into(), "kim@x.org".into())),
        _ => None,
    });
    assert_eq!(g.members[0].uid, "p1");
    // Kim leaves, Lee (whose card save_group made) joins, Bcc goes on.
    let mut members = vec![g.members[0].clone()];
    members.push(super::Member {
        name: "Lee".into(),
        email: "lee@x.org".into(),
        uid: "p3".into(),
        ..Default::default()
    });
    let edit = super::GroupEdit {
        name: "Family; Close".into(),
        members,
        hide_addresses: true,
        kept: g.hidden_members.clone(),
    };
    let out = super::patched_apple_group(apple, &edit);
    assert!(
        out.contains("\r\nUID:g2\r\n") && out.contains("PRODID:-//Apple Inc.//iOS 18//EN"),
        "{out}"
    );
    assert!(
        out.contains("X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:p1\r\n"),
        "{out}"
    );
    assert!(
        out.contains("X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:p3\r\n"),
        "{out}"
    );
    assert!(
        out.contains("X-ADDRESSBOOKSERVER-MEMBER:urn:uuid:nomail\r\n"),
        "{out}"
    );
    assert!(!out.contains("urn:uuid:p2"), "{out}");
    assert_eq!(out.matches("X-ADDRESSBOOKSERVER-KIND:group").count(), 1);
    assert!(!out.contains("X-EVOLUTION-LIST:"), "{out}");
    let back = super::parse_vcard_fields(&out).unwrap();
    assert_eq!(back.name, "Family; Close");
    assert_eq!(back.group, Some(super::GroupFormat::Apple));
    assert!(back.hide_addresses);
    let refs: Vec<&str> = back
        .member_refs
        .iter()
        .map(|(uid, _)| uid.as_str())
        .collect();
    assert_eq!(refs, vec!["p1", "p3", "nomail"]);

    // A new one, with Bcc off, says nothing of it.
    let edit = super::GroupEdit {
        name: "Team".into(),
        hide_addresses: false,
        ..edit
    };
    let new = super::patched_apple_group(super::EMPTY_VCARD, &edit);
    assert!(
        new.starts_with("BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Team\r\nN:Team;;;;\r\n"),
        "{new}"
    );
    assert!(
        !new.contains("SHOW-ADDRESSES") && !new.contains("UID:"),
        "{new}"
    );
}

#[test]
fn books_keep_groups_as_their_servers_do() {
    use super::{group_format_for, GroupFormat};
    assert_eq!(
        group_format_for(Some("local")),
        Some(GroupFormat::Evolution)
    );
    assert_eq!(group_format_for(None), Some(GroupFormat::Evolution));
    assert_eq!(group_format_for(Some("webdav")), Some(GroupFormat::Apple));
    assert_eq!(group_format_for(Some("carddav")), Some(GroupFormat::Apple));
    assert_eq!(group_format_for(Some("google")), None);
    assert_eq!(group_format_for(Some("microsoft365")), None);
}

/// Nextcloud's books, from GNOME Online Accounts or added in Hylki, are
/// under /remote.php/ and keep groups as categories (#652).
#[test]
fn nextcloud_books_keep_categories() {
    let book = |path: &str| {
        format!(
            "[Data Source]\nDisplayName=Contacts\nParent=abc\n\n[WebDAV Backend]\n\
             DisplayName=Contacts\nResourcePath={path}\n\n[Address Book]\nBackendName=carddav\n"
        )
    };
    assert!(super::keeps_categories(&book(
        "/remote.php/dav/addressbooks/users/admin/contacts/"
    )));
    assert!(super::keeps_categories(&book(
        "/nextcloud/remote.php/carddav/addressbooks/ann/contacts/"
    )));
    assert!(!super::keeps_categories(&book(
        "/1234567/carddavhome/card/"
    )));
    assert!(!super::keeps_categories(
        "[Data Source]\nDisplayName=Local\n\n[Address Book]\nBackendName=local\n"
    ));
}

#[test]
fn an_edited_list_keeps_the_lines_of_the_members_it_keeps() {
    let c = super::parse_vcard_fields(EDS_LIST).unwrap();
    let mut members = vec![c.members[0].clone()];
    members.push(super::Member {
        name: "Carol \"CJ\", Ng".into(),
        email: "carol@x.org".into(),
        ..Default::default()
    });
    let edit = super::GroupEdit {
        name: "Readers, All".into(),
        members,
        hide_addresses: false,
        kept: Vec::new(),
    };
    let out = super::patched_group_vcard(EDS_LIST, &edit);
    assert!(out.contains("X-EVOLUTION-DEST-CONTACT-UID=p1"), "{out}");
    assert!(
        out.contains("\r\nUID:l1\r\n") && out.contains("\r\nREV:"),
        "{out}"
    );
    assert!(!out.contains("bob@example.org"), "{out}");
    assert_eq!(out.matches("X-EVOLUTION-LIST:TRUE").count(), 1);
    let back = super::parse_vcard_fields(&out).unwrap();
    assert_eq!(back.name, "Readers, All");
    assert!(!back.hide_addresses);
    assert_eq!(
        people(&back),
        vec![
            ("Alice Able", "alice@example.org"),
            ("Carol \"CJ\", Ng", "carol@x.org")
        ]
    );
}

#[test]
fn a_new_group_reads_back_as_written() {
    let edit = super::GroupEdit {
        name: "Book Club".into(),
        members: vec![super::Member {
            name: "Ann".into(),
            email: "ann@x.org".into(),
            ..Default::default()
        }],
        hide_addresses: true,
        kept: Vec::new(),
    };
    let back = super::parse_vcard_fields(&super::new_group_vcard(&edit)).unwrap();
    assert_eq!(back.group, Some(super::GroupFormat::Evolution));
    assert!(back.hide_addresses);
    assert_eq!(people(&back), vec![("Ann", "ann@x.org")]);
    assert_eq!(back.members_field(), "Ann <ann@x.org>");
}

#[test]
fn a_group_is_suggested_once_and_only_with_someone_in_it() {
    let g = |name: &str, emails: &[&str]| super::ContactDetails {
        name: name.into(),
        group: Some(super::GroupFormat::Evolution),
        members: emails
            .iter()
            .map(|e| super::Member {
                email: e.to_string(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    let out = super::group_suggestions(vec![
        g("Team", &["a@x.org", "b@x.org"]),
        g("team", &["B@x.org", "a@x.org"]),
        g("Empty", &[]),
        g("Team", &["c@x.org"]),
    ]);
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|s| s.email.is_empty() && s.group.is_some()));
    // Only the name finds a group, not its members' addresses.
    assert!(out[0].matches("tea") && !out[0].matches("a@x"));
}

/// GROUP_LIVE=<book uid>: make an Apple group there through EDS with
/// one new member, change it, and remove what it made (GROUP_KEEP=1
/// leaves it). For a sealed rig or a book the tester owns.
#[test]
#[ignore]
fn live_group() {
    let Ok(book) = std::env::var("GROUP_LIVE") else {
        return;
    };
    // "apple": the first book that keeps Apple's groups.
    let book = if book == "apple" {
        let books = super::writable_books();
        println!("books: {books:?}");
        books
            .into_iter()
            .find(|b| b.groups == Some(super::GroupFormat::Apple))
            .expect("a CardDAV book")
            .uid
    } else {
        book
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let name = format!("Hylki group test {stamp} (delete me)");
    let member = |n: &str, e: &str| super::Member {
        name: n.into(),
        email: e.into(),
        ..Default::default()
    };
    let edit = super::GroupEdit {
        name: name.clone(),
        members: vec![member(
            "Hylki Test Member (delete me)",
            &format!("hylki-test-{stamp}@example.com"),
        )],
        hide_addresses: true,
        kept: Vec::new(),
    };
    super::save_group(&book, super::GroupFormat::Apple, None, edit).unwrap();
    let dest = super::factory_dest().unwrap();
    let conn = zbus::blocking::Connection::session().unwrap();
    let (bus, path) = super::open_book(&conn, &dest, &book).unwrap();
    let find = |q: String| super::book_get_uids(&conn, &bus, &path, &q).unwrap();
    let group_uid = find(format!("(is \"full_name\" \"{name}\")"))
        .pop()
        .expect("the group");
    let card = super::book_get_contact(&conn, &bus, &path, &group_uid).unwrap();
    println!("=== made:\n{card}");
    let stub = find(format!("(is \"email\" \"hylki-test-{stamp}@example.com\")"))
        .pop()
        .expect("the member's card");
    println!(
        "=== member's card:\n{}",
        super::book_get_contact(&conn, &bus, &path, &stub).unwrap()
    );
    let mut g = super::parse_vcard_fields(&card).unwrap();
    assert_eq!(g.group, Some(super::GroupFormat::Apple));
    assert!(g.hide_addresses);
    assert_eq!(
        g.member_refs
            .iter()
            .map(|(u, _)| u.as_str())
            .collect::<Vec<_>>(),
        vec![stub.as_str()]
    );
    super::resolve_members(&mut g, |uid| {
        super::person_address(&super::book_get_contact(&conn, &bus, &path, uid).ok()?)
    });
    // Bcc off and a second member, who already has a card by then.
    let mut members = g.members.clone();
    members.push(member("Again", &format!("hylki-test-{stamp}@example.com")));
    members.dedup_by(|a, b| a.email == b.email);
    let edit = super::GroupEdit {
        name: name.clone(),
        members,
        hide_addresses: false,
        kept: g.hidden_members.clone(),
    };
    super::save_group(&book, super::GroupFormat::Apple, Some(&card), edit).unwrap();
    let changed = super::book_get_contact(&conn, &bus, &path, &group_uid).unwrap();
    println!("=== changed:\n{changed}");
    let back = super::parse_vcard_fields(&changed).unwrap();
    assert!(!back.hide_addresses);
    assert_eq!(back.member_refs.len(), 1);
    if std::env::var_os("GROUP_KEEP").is_some() {
        return;
    }
    super::delete_contact(&book, &group_uid).unwrap();
    super::delete_contact(&book, &stub).unwrap();
    println!("removed {group_uid} and {stub}");
}

/// MOVE_LIVE=1: move a contact with a photo between the first two books
/// that keep Apple's groups (a sealed rig's CardDAV books), and remove it.
#[test]
#[ignore]
fn live_move() {
    if std::env::var_os("MOVE_LIVE").is_none() {
        return;
    }
    let books: Vec<String> = super::writable_books()
        .into_iter()
        .filter(|b| b.groups == Some(super::GroupFormat::Apple))
        .map(|b| b.uid)
        .collect();
    let (from, to) = (&books[0], &books[1]);
    let card = format!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Hylki Move Test\r\nN:Test;Hylki Move;;;\r\nEMAIL:move-test@example.com\r\n\
         PHOTO;ENCODING=b;TYPE=PNG:{ONE_PIXEL_PNG}\r\nEND:VCARD\r\n"
    );
    super::create_contact(from, &card).unwrap();
    let dest = super::factory_dest().unwrap();
    let conn = zbus::blocking::Connection::session().unwrap();
    let find = |book: &str| {
        let (bus, path) = super::open_book(&conn, &dest, book).unwrap();
        let uids = super::book_get_uids(
            &conn,
            &bus,
            &path,
            "(is \"email\" \"move-test@example.com\")",
        )
        .unwrap();
        uids.into_iter()
            .map(|u| {
                (
                    u.clone(),
                    super::book_get_contact(&conn, &bus, &path, &u).unwrap(),
                )
            })
            .collect::<Vec<_>>()
    };
    let (uid, stored) = find(from).pop().expect("made in the first book");
    let mut c = super::parse_vcard_details(&stored).unwrap();
    c.raw_vcard = stored;
    println!("photo bytes: {:?}", c.photo.as_ref().map(Vec::len));
    let moved = super::moved_vcard(&c);
    super::move_contact(from, &uid, &moved, to).unwrap();
    assert!(find(from).is_empty(), "gone from the first book");
    let (new_uid, there) = find(to).pop().expect("in the second book");
    println!("=== in the second book:\n{there}");
    assert!(super::parse_vcard_details(&there).unwrap().photo.is_some());
    super::delete_contact(to, &new_uid).unwrap();
}

/// MOVE_LIVE=1: a contact with only a phone number, in one book, joins
/// a new Apple group in another: copied there whole, then named in it.
#[test]
#[ignore]
fn live_copy_into_group() {
    if std::env::var_os("MOVE_LIVE").is_none() {
        return;
    }
    let books: Vec<String> = super::writable_books()
        .into_iter()
        .filter(|b| b.groups == Some(super::GroupFormat::Apple))
        .map(|b| b.uid)
        .collect();
    let (from, to) = (&books[0], &books[1]);
    let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Hylki Phone Only\r\nN:Only;Hylki Phone;;;\r\n\
                TEL;TYPE=CELL:+1 555 0199\r\nNOTE:copied whole\r\nEND:VCARD\r\n";
    let dest = super::factory_dest().unwrap();
    let conn = zbus::blocking::Connection::session().unwrap();
    let find = |book: &str, q: &str| {
        let (bus, path) = super::open_book(&conn, &dest, book).unwrap();
        super::book_get_uids(&conn, &bus, &path, q)
            .unwrap()
            .into_iter()
            .map(|u| {
                (
                    u.clone(),
                    super::book_get_contact(&conn, &bus, &path, &u).unwrap(),
                )
            })
            .collect::<Vec<_>>()
    };
    let by_name = "(is \"full_name\" \"Hylki Phone Only\")";
    // What an earlier run left.
    for book in [from, to] {
        for q in [by_name, "(beginswith \"full_name\" \"Hylki copy test\")"] {
            for (uid, _) in find(book, q) {
                super::delete_contact(book, &uid).unwrap();
            }
        }
    }
    super::create_contact(from, card).unwrap();
    let (src_uid, stored) = find(from, by_name).pop().expect("made");
    let mut c = super::parse_vcard_details(&stored).unwrap();
    c.raw_vcard = stored;
    let member = super::Member {
        name: c.name.clone(),
        card: super::moved_vcard(&c),
        ..Default::default()
    };
    let edit = super::GroupEdit {
        name: "Hylki copy test".into(),
        members: vec![member],
        ..Default::default()
    };
    super::save_group(to, super::GroupFormat::Apple, None, edit.clone()).unwrap();
    let (copy_uid, copy) = find(to, by_name).pop().expect("copied to the group's book");
    println!("=== the copy:\n{copy}");
    assert!(copy.contains("+1 555 0199") && copy.contains("copied whole"));
    let (group_uid, group) = find(to, "(is \"full_name\" \"Hylki copy test\")")
        .pop()
        .expect("the group");
    let refs = super::parse_vcard_fields(&group).unwrap().member_refs;
    assert_eq!(
        refs.iter().map(|(u, _)| u.as_str()).collect::<Vec<_>>(),
        vec![copy_uid.as_str()],
        "{group}"
    );
    // Joining another group finds the copy, by name, and makes none.
    super::save_group(
        to,
        super::GroupFormat::Apple,
        None,
        super::GroupEdit {
            name: "Hylki copy test 2".into(),
            ..edit
        },
    )
    .unwrap();
    assert_eq!(find(to, by_name).len(), 1);
    let (group2, _) = find(to, "(is \"full_name\" \"Hylki copy test 2\")")
        .pop()
        .unwrap();
    for (book, uid) in [
        (to, &group_uid),
        (to, &group2),
        (to, &copy_uid),
        (from, &src_uid),
    ] {
        super::delete_contact(book, uid).unwrap();
    }
}

/// MOVE_LIVE=1: an Apple group, with a member who has only a phone
/// number, moves to another book: made there with the member copied,
/// gone from the first.
#[test]
#[ignore]
fn live_move_group() {
    if std::env::var_os("MOVE_LIVE").is_none() {
        return;
    }
    let books: Vec<String> = super::writable_books()
        .into_iter()
        .filter(|b| b.groups == Some(super::GroupFormat::Apple))
        .map(|b| b.uid)
        .collect();
    let (from, to) = (&books[0], &books[1]);
    let dest = super::factory_dest().unwrap();
    let conn = zbus::blocking::Connection::session().unwrap();
    let find = |book: &str, q: &str| {
        let (bus, path) = super::open_book(&conn, &dest, book).unwrap();
        super::book_get_uids(&conn, &bus, &path, q)
            .unwrap()
            .into_iter()
            .map(|u| {
                (
                    u.clone(),
                    super::book_get_contact(&conn, &bus, &path, &u).unwrap(),
                )
            })
            .collect::<Vec<_>>()
    };
    let person_q = "(is \"full_name\" \"Hylki Mover\")";
    let group_q = "(is \"full_name\" \"Hylki moving group\")";
    for book in [from, to] {
        for q in [person_q, group_q] {
            for (uid, _) in find(book, q) {
                super::delete_contact(book, &uid).unwrap();
            }
        }
    }
    super::create_contact(
        from,
        "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Hylki Mover\r\nTEL:+1 555 0142\r\nEND:VCARD\r\n",
    )
    .unwrap();
    let (person_uid, stored) = find(from, person_q).pop().unwrap();
    let mut person = super::parse_vcard_details(&stored).unwrap();
    person.raw_vcard = stored;
    let member = super::Member {
        name: person.name.clone(),
        uid: person_uid.clone(),
        ..Default::default()
    };
    let edit = super::GroupEdit {
        name: "Hylki moving group".into(),
        members: vec![member],
        ..Default::default()
    };
    super::save_group(from, super::GroupFormat::Apple, None, edit).unwrap();
    let (group_uid, _) = find(from, group_q).pop().unwrap();
    // As the form sends it: the member's card copied from the old book.
    let moved = super::Member {
        name: person.name.clone(),
        card: super::moved_vcard(&person),
        ..Default::default()
    };
    let edit = super::GroupEdit {
        name: "Hylki moving group".into(),
        members: vec![moved],
        ..Default::default()
    };
    super::move_group(from, &group_uid, to, super::GroupFormat::Apple, edit).unwrap();
    assert!(find(from, group_q).is_empty(), "gone from the first book");
    let (new_group, group) = find(to, group_q).pop().expect("in the second book");
    let (copy_uid, copy) = find(to, person_q).pop().expect("the member copied");
    assert!(copy.contains("555 0142"));
    let refs = super::parse_vcard_fields(&group).unwrap().member_refs;
    assert_eq!(
        refs.iter().map(|(u, _)| u.as_str()).collect::<Vec<_>>(),
        vec![copy_uid.as_str()]
    );
    for (book, uid) in [(to, &new_group), (to, &copy_uid), (from, &person_uid)] {
        super::delete_contact(book, uid).unwrap();
    }
}
