use super::*;

/// An imported card's UID comes from its text, so importing it again
/// finds the same contact: the derivation must never change.
#[test]
fn content_uids_stay_the_same() {
    let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada\r\nEND:VCARD\r\n\r\n";
    assert_eq!(content_uid(card), "hylki-9f293ea101dd5869a7b8b61d");
    assert_eq!(content_uid(&card.replace("\r\n", "\n")), content_uid(card));
}

#[test]
fn reading_the_book_leaves_the_file_untouched() {
    let dir = std::env::temp_dir().join(format!("hylki-local-contacts-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("local_contacts.db");
    let _ = std::fs::remove_file(&path);
    Store::open_at(&path)
        .unwrap()
        .upsert_one(&card("Ada", "ada@example.org"))
        .unwrap();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let before = std::fs::read(&path).unwrap();
    // Opening for writing again must not rewrite the schema either.
    drop(Store::open_at(&path).unwrap());
    let read = Store::open_read_at(&path).unwrap().unwrap();
    assert_eq!(
        read.emails().unwrap(),
        vec![("Ada".to_string(), "ada@example.org".to_string())]
    );
    drop(read);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let _ = std::fs::remove_dir_all(&dir);
}

fn card(name: &str, email: &str) -> String {
    format!("BEGIN:VCARD\r\nVERSION:3.0\r\nFN:{name}\r\nEMAIL:{email}\r\nEND:VCARD\r\n")
}

#[test]
fn split_finds_several_cards() {
    let text = format!("{}{}", card("Ann", "a@x.org"), card("Bob", "b@x.org"));
    let (blocks, truncated) = split_vcards(&text);
    assert_eq!(blocks.len(), 2);
    assert_eq!(truncated, 0);
    assert!(blocks[1].contains("FN:Bob"));
}

#[test]
fn split_accepts_lf_and_lowercase_markers() {
    let (blocks, _) = split_vcards("begin:vcard\nFN:Ann\nEMAIL:a@x.org\nend:vcard\n");
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].contains("\r\n"));
}

#[test]
fn split_keeps_folded_lines_inside_the_card() {
    let text = "BEGIN:VCARD\r\nFN:Ann\r\nNOTE:one\r\n two\r\nEND:VCARD\r\n";
    let (blocks, _) = split_vcards(text);
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].contains("NOTE:one\r\n two"));
}

#[test]
fn split_ignores_text_between_cards() {
    let text = format!(
        "junk\r\n{}more junk\r\n{}",
        card("Ann", "a@x.org"),
        card("Bob", "b@x.org")
    );
    assert_eq!(split_vcards(&text).0.len(), 2);
}

#[test]
fn split_counts_a_truncated_card_and_keeps_the_next() {
    let text = format!("BEGIN:VCARD\r\nFN:Cut\r\n{}", card("Bob", "b@x.org"));
    let (blocks, truncated) = split_vcards(&text);
    assert_eq!(truncated, 1);
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].contains("FN:Bob"));
}

#[test]
fn split_counts_a_truncated_card_at_the_end() {
    let text = format!("{}BEGIN:VCARD\r\nFN:Cut\r\n", card("Ann", "a@x.org"));
    let (blocks, truncated) = split_vcards(&text);
    assert_eq!((blocks.len(), truncated), (1, 1));
}

#[test]
fn decode_drops_a_bom_and_reads_utf8() {
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice("Zoë".as_bytes());
    assert_eq!(decode_text(&bytes).unwrap(), "Zoë");
}

#[test]
fn decode_falls_back_to_latin1() {
    assert_eq!(decode_text(&[b'Z', b'o', 0xEB]).unwrap(), "Zoë");
}

#[test]
fn decode_refuses_utf16_and_binary() {
    assert!(decode_text(&[0xFF, 0xFE, b'a', 0]).is_err());
    assert!(decode_text(&[b'a', 0, b'b']).is_err());
}

#[test]
fn insert_uid_goes_before_the_end_marker() {
    let out = insert_uid(&card("Ann", "a@x.org"), "u1");
    assert!(out.contains("UID:u1\r\nEND:VCARD"));
}

#[test]
fn import_adds_then_updates_and_is_idempotent() {
    let mut store = Store::in_memory();
    let text = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:u1\r\nFN:Ann\r\nEMAIL:a@x.org\r\nEND:VCARD\r\n\
                BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Bob\r\nEMAIL:b@x.org\r\nEND:VCARD\r\n";
    let first = import_text_into(&mut store, text).unwrap();
    assert_eq!((first.added, first.updated), (2, 0));
    let second = import_text_into(&mut store, text).unwrap();
    assert_eq!((second.added, second.updated), (0, 2));
    assert_eq!(store.vcards().unwrap().len(), 2);
}

#[test]
fn a_uid_met_twice_in_one_import_keeps_the_first_card() {
    let mut store = Store::in_memory();
    let text = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:1\r\nFN:Ann\r\nEMAIL:a@x.org\r\nEND:VCARD\r\n\
                BEGIN:VCARD\r\nVERSION:3.0\r\nUID:1\r\nFN:Bob\r\nEMAIL:b@x.org\r\nEND:VCARD\r\n";
    let out = import_text_into(&mut store, text).unwrap();
    assert_eq!((out.added, out.updated, out.skipped), (1, 0, 1));
    assert_eq!(
        store.emails().unwrap(),
        vec![("Ann".to_string(), "a@x.org".to_string())]
    );
}

#[test]
fn quoted_printable_names_are_decoded() {
    let block = "BEGIN:VCARD\r\nVERSION:2.1\r\n\
                 N;CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:M=C3=BCller;J=C3=BCrgen;;;\r\n\
                 FN;CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:J=C3=BCrgen =\r\nM=C3=BCller\r\n\
                 NOTE;ENCODING=QUOTED-PRINTABLE;CHARSET=ISO-8859-1:caf=E9=0D=0Aau lait\r\n\
                 EMAIL;INTERNET:j@x.org\r\nEND:VCARD\r\n";
    let out = decode_quoted_printable(block);
    assert!(out.contains("\r\nN:Müller;Jürgen;;;\r\n"), "{out}");
    assert!(out.contains("\r\nFN:Jürgen Müller\r\n"), "{out}");
    assert!(out.contains("\r\nNOTE:café\\nau lait\r\n"), "{out}");
    assert!(out.contains("\r\nEMAIL;INTERNET:j@x.org\r\n"), "{out}");
    let details = crate::contacts::parse_vcard_fields(&out).unwrap();
    assert_eq!(details.name, "Jürgen Müller");
    // A 3.0 card goes through untouched.
    let plain = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ann=\r\nEND:VCARD\r\n";
    assert_eq!(decode_quoted_printable(plain), plain);
}

#[test]
fn import_skips_cards_without_name_or_email() {
    let mut store = Store::in_memory();
    let text = "BEGIN:VCARD\r\nVERSION:3.0\r\nNOTE:nobody\r\nEND:VCARD\r\n\
                BEGIN:VCARD\r\nVERSION:3.0\r\nX-EVOLUTION-LIST:TRUE\r\nEMAIL:t@x.org\r\nEND:VCARD\r\n";
    let out = import_text_into(&mut store, text).unwrap();
    assert_eq!((out.added, out.skipped), (0, 2));
}

#[test]
fn an_imported_list_is_a_group_whose_members_are_not_contacts() {
    let mut store = Store::in_memory();
    let text = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Team\r\nX-EVOLUTION-LIST:TRUE\r\nEMAIL:Tom <t@x.org>\r\nEND:VCARD\r\n\
                BEGIN:VCARD\r\nVERSION:4.0\r\nKIND:group\r\nFN:Kin\r\nMEMBER:mailto:k@x.org\r\nEND:VCARD\r\n";
    let out = import_text_into(&mut store, text).unwrap();
    assert_eq!(out.added, 2);
    store.upsert_one(&card("Ann", "a@x.org")).unwrap();
    assert_eq!(
        store.emails().unwrap(),
        vec![("Ann".to_string(), "a@x.org".to_string())]
    );
    assert_eq!(store.group_vcards().unwrap().len(), 2);
    assert_eq!(store.people(), 1);
}

#[test]
fn import_counts_oversized_and_truncated_cards_as_errors() {
    let mut store = Store::in_memory();
    let big = format!(
        "BEGIN:VCARD\r\nFN:Big\r\nNOTE:{}\r\nEND:VCARD\r\n",
        "x".repeat(MAX_VCARD_BYTES + 1)
    );
    let text = format!("{big}BEGIN:VCARD\r\nFN:Cut\r\n");
    let out = import_text_into(&mut store, &text).unwrap();
    assert_eq!((out.added, out.errors), (0, 2));
}

#[test]
fn create_modify_delete_keep_the_email_index_in_sync() {
    let mut store = Store::in_memory();
    store
        .upsert_one(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:u1\r\nFN:Ann\r\nEMAIL:A@X.org\r\nEND:VCARD\r\n",
        )
        .unwrap();
    assert_eq!(
        store.emails().unwrap(),
        vec![("Ann".to_string(), "A@X.org".to_string())]
    );

    store
        .upsert_one(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:u1\r\nFN:Ann B\r\nEMAIL:new@x.org\r\nEND:VCARD\r\n",
        )
        .unwrap();
    assert_eq!(
        store.emails().unwrap(),
        vec![("Ann B".to_string(), "new@x.org".to_string())]
    );

    store.delete("u1").unwrap();
    assert!(store.emails().unwrap().is_empty());
    assert!(store.vcards().unwrap().is_empty());
}

#[test]
fn a_group_made_in_the_editor_is_stored_with_a_uid() {
    let mut store = Store::in_memory();
    let edit = crate::contacts::GroupEdit {
        name: "Team".into(),
        members: vec![crate::contacts::Member {
            name: "Ann".into(),
            email: "a@x.org".into(),
            ..Default::default()
        }],
        hide_addresses: false,
        kept: Vec::new(),
    };
    store
        .upsert_one(&crate::contacts::new_group_vcard(&edit))
        .unwrap();
    let stored = store.group_vcards().unwrap();
    assert!(stored[0].contains("\r\nUID:hylki-"));
    // Ann is in the group, not in the book.
    assert!(store.emails().unwrap().is_empty());
    assert_eq!(store.people(), 0);
}

#[test]
fn create_rejects_a_card_with_nothing_in_it() {
    let mut store = Store::in_memory();
    assert!(store
        .upsert_one("BEGIN:VCARD\r\nVERSION:3.0\r\nEND:VCARD\r\n")
        .is_err());
}

#[test]
fn created_cards_get_a_stored_uid() {
    let mut store = Store::in_memory();
    store.upsert_one(&card("Ann", "a@x.org")).unwrap();
    let stored = store.vcards().unwrap().remove(0);
    assert!(stored.contains("\r\nUID:hylki-"));
}

#[test]
fn photo_entries_list_only_contacts_with_a_photo() {
    let mut store = Store::in_memory();
    store.upsert_one("BEGIN:VCARD\r\nUID:p1\r\nFN:Pic\r\nEMAIL:Pic@x.org\r\nPHOTO;ENCODING=b:AAAA\r\nEND:VCARD\r\n").unwrap();
    store.upsert_one(&card("Plain", "plain@x.org")).unwrap();
    assert_eq!(
        store.photo_entries().unwrap(),
        vec![("pic@x.org".to_string(), "p1".to_string())]
    );
}

#[test]
fn a_newer_schema_is_refused() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA user_version = 99;").unwrap();
    assert!(Store::init(conn).is_err());
}

#[test]
fn a_csv_import_fills_groups_by_name_and_is_idempotent() {
    let mut store = Store::in_memory();
    let list = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:g1\r\nFN:Friends\r\nX-EVOLUTION-LIST:TRUE\r\n\
                EMAIL:Tom <t@x.org>\r\nEND:VCARD\r\n";
    import_text_into(&mut store, list).unwrap();
    let csv = "Name,Email,Labels\nAnn,a@x.org,* myContacts ::: friends ::: Choir\nBob,,Choir\n";
    let first = import_csv_into(&mut store, csv.as_bytes()).unwrap();
    assert_eq!((first.added, first.updated), (2, 0));
    let second = import_csv_into(&mut store, csv.as_bytes()).unwrap();
    assert_eq!((second.added, second.updated), (0, 2));
    let groups: Vec<_> = store
        .group_vcards()
        .unwrap()
        .iter()
        .filter_map(|v| crate::contacts::parse_vcard_fields(v))
        .map(|g| {
            let emails: Vec<String> = g.members.iter().map(|m| m.email.clone()).collect();
            (g.name, emails)
        })
        .collect();
    assert_eq!(
        groups,
        [
            ("Choir".to_string(), vec!["a@x.org".to_string()]),
            (
                "Friends".to_string(),
                vec!["t@x.org".to_string(), "a@x.org".to_string()]
            ),
        ]
    );
}

#[test]
fn csv_files_are_told_from_vcards() {
    assert!(is_csv(Path::new("a.csv"), b"BEGIN:VCARD"));
    assert!(!is_csv(Path::new("a.vcf"), b"Name,Email"));
    assert!(is_csv(Path::new("a.txt"), b"Name,Email\nAnn,a@x.org"));
    assert!(!is_csv(
        Path::new("export"),
        b"begin:vcard\r\nFN:A\r\nend:vcard"
    ));
}
