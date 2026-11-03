//! Tests of reading, parsing and editing contacts.

#[test]
fn registry_reply_keeps_sources_and_skips_unrelated_objects() {
    use gtk::glib::{variant::ObjectPath, variant::ToVariant};
    use std::collections::HashMap;
    let path = ObjectPath::try_from("/org/gnome/evolution/dataserver/Source/test").unwrap();
    let data = "[Data Source]\nDisplayName=Test\n[Address Book]\nBackendName=local\n";
    let objects = HashMap::from([
        (
            path.clone(),
            HashMap::from([(
                "org.gnome.evolution.dataserver.Source",
                HashMap::from([("UID", "test".to_variant()), ("Data", data.to_variant())]),
            )]),
        ),
        (ObjectPath::try_from("/unrelated").unwrap(), HashMap::new()),
    ]);
    assert_eq!(
        super::sources_from_reply(&(objects,).to_variant()),
        Some(vec![(path.to_string(), "test".into(), data.into()),])
    );
    assert!(super::sources_from_reply(&"wrong reply type".to_variant()).is_none());
}

use super::{
    details_from_local_vcards, is_local_book, merge_target, prefer_book, vcard_display_name,
    vcard_photo, vcard_photo_with, Book,
};

pub(super) const ONE_PIXEL_PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

// iCloud's CardDAV photos: a quoted https URL parameter precedes the real
// file value — the split must not stop at the colon inside the quotes.
#[test]
fn quoted_params_do_not_swallow_the_value() {
    let line = "PHOTO;X-EVOLUTION-WEBDAV-IMG-URL=\"https://gateway.icloud.com/x/y\";\
                VALUE=uri:file:///tmp/photo.jpg";
    let (prop, value) = super::split_vcard_line(line).unwrap();
    assert!(prop.ends_with("VALUE=uri"));
    assert_eq!(value, "file:///tmp/photo.jpg");
}

#[test]
fn fn_preserves_case() {
    let v = "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Arnwine;Aaron;;;\r\nFN:Aaron Arnwine\r\nEND:VCARD\r\n";
    assert_eq!(vcard_display_name(v).as_deref(), Some("Aaron Arnwine"));
}

#[test]
fn fn_with_parameters() {
    let v = "FN;PID=1.1:Jane O'Brien\r\n";
    assert_eq!(vcard_display_name(v).as_deref(), Some("Jane O'Brien"));
}

#[test]
fn fn_line_folded() {
    // A folded FN value continues on the next line after a leading space.
    let v = "FN:Reallylongfirst\r\n Lastname\r\n";
    assert_eq!(
        vcard_display_name(v).as_deref(),
        Some("ReallylongfirstLastname")
    );
}

#[test]
fn fn_escaped_comma() {
    let v = "FN:Smith\\, John\r\n";
    assert_eq!(vcard_display_name(v).as_deref(), Some("Smith, John"));
}

#[test]
fn empty_or_missing_fn() {
    assert_eq!(vcard_display_name("FN:\r\nN:x;;;;\r\n"), None);
    assert_eq!(vcard_display_name("N:Doe;John;;;\r\n"), None);
}

#[test]
fn photo_decodes_nextcloud_data_uri() {
    let vcard = format!("item1.PHOTO:data:image/image/png\\;base64\\,{ONE_PIXEL_PNG}\r\n");
    let photo = vcard_photo(&vcard).unwrap();
    assert_eq!(&photo[..8], b"\x89PNG\r\n\x1a\n");
}

#[test]
fn photo_decodes_folded_encoding_b() {
    let split = ONE_PIXEL_PNG.len() / 2;
    let vcard = format!(
        "PHOTO;ENCODING=b;TYPE=PNG:{}\r\n {}\r\n",
        &ONE_PIXEL_PNG[..split],
        &ONE_PIXEL_PNG[split..]
    );
    assert!(vcard_photo(&vcard).is_some());
}

#[test]
fn photo_does_not_fetch_remote_uri() {
    assert!(vcard_photo("PHOTO;VALUE=uri:https://example.com/tracker.png\r\n").is_none());
}

/// A scratch directory tree for exercising [`super::confine_to_roots`]:
/// `<tmp>/vireo-confine-<pid>-<n>/{root,outside}` with one file in each,
/// removed on drop.
struct ConfineFixture {
    base: std::path::PathBuf,
}

impl ConfineFixture {
    fn new(n: u32) -> Self {
        let base = std::env::temp_dir().join(format!("hylki-confine-{}-{n}", std::process::id()));
        std::fs::create_dir_all(base.join("root")).unwrap();
        std::fs::create_dir_all(base.join("outside")).unwrap();
        std::fs::write(base.join("root/photo.png"), b"in").unwrap();
        std::fs::write(base.join("outside/secret"), b"out").unwrap();
        Self { base }
    }

    fn root(&self) -> std::path::PathBuf {
        self.base.join("root")
    }
}

impl Drop for ConfineFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

#[test]
fn photo_files_inside_a_root_are_admitted() {
    let fx = ConfineFixture::new(1);
    let roots = vec![fx.root()];
    let admitted = super::confine_to_roots(&fx.root().join("photo.png"), &roots);
    assert!(admitted.is_some());
}

#[test]
fn dotdot_traversal_cannot_leave_a_root() {
    let fx = ConfineFixture::new(2);
    let roots = vec![fx.root()];
    let escape = fx.root().join("../outside/secret");
    assert_eq!(super::confine_to_roots(&escape, &roots), None);
}

#[test]
fn a_symlink_planted_inside_a_root_cannot_reach_outside() {
    let fx = ConfineFixture::new(3);
    let roots = vec![fx.root()];
    let link = fx.root().join("link.png");
    std::os::unix::fs::symlink(fx.base.join("outside/secret"), &link).unwrap();
    assert_eq!(super::confine_to_roots(&link, &roots), None);
}

#[test]
fn a_lookalike_sibling_directory_is_not_a_root() {
    let fx = ConfineFixture::new(4);
    let sibling = fx.base.join("root-evil");
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::write(sibling.join("photo.png"), b"no").unwrap();
    let roots = vec![fx.root()];
    assert_eq!(
        super::confine_to_roots(&sibling.join("photo.png"), &roots),
        None
    );
}

#[test]
fn no_roots_admits_nothing() {
    let fx = ConfineFixture::new(5);
    assert_eq!(
        super::confine_to_roots(&fx.root().join("photo.png"), &[]),
        None
    );
    // A root that doesn't resolve is the same as no root at all.
    let ghost = vec![fx.base.join("missing")];
    assert_eq!(
        super::confine_to_roots(&fx.root().join("photo.png"), &ghost),
        None
    );
}

#[test]
fn a_host_bearing_file_uri_is_refused() {
    assert_eq!(
        super::read_eds_photo_file("file://evil-host/etc/passwd", 1024),
        None
    );
}

/// A birthday is written as mail dates are, so the words and the order
/// are the locale's; a vCard value that is no date passes through.
#[test]
fn birthdays_are_written_as_dates() {
    let full = super::pretty_birthday("1985-04-12");
    assert!(full.contains("1985") && full.contains("12"), "{full}");
    assert_eq!(super::pretty_birthday("19850412"), full);
    let leap = super::pretty_birthday("--02-29");
    assert!(leap.contains("29") && !leap.contains("2000"), "{leap}");
    assert_eq!(super::pretty_birthday("1985-02-30"), "1985-02-30");
    assert_eq!(super::pretty_birthday("sometime"), "sometime");
}

/// EDS `.source` files are key files: a book's name comes from its
/// account collection, and only `[Data Source]`'s Enabled switches it off.
#[test]
fn eds_sources_are_read_as_key_files() {
    let book = "[Data Source]\nDisplayName=Contacts\nEnabled=true\nParent=acct\n\n\
                [Address Book]\nBackendName=carddav\n\n[Refresh]\nEnabled=false\n";
    let acct = "[Data Source]\nDisplayName=Google\n\n[Collection]\nBackendName=google\n\
                Identity=someone@gmail.com\n";
    let sources: std::collections::HashMap<String, String> =
        [("acct".to_string(), acct.to_string())].into();
    assert_eq!(
        super::book_display_name(book, &sources),
        "Google — someone@gmail.com · Contacts"
    );
    assert!(
        !super::data_source_disabled(book),
        "[Refresh] Enabled=false is not the switch"
    );
    // Nextcloud: several books under one account get distinct names…
    let nc = "[Data Source]\nDisplayName=alice@cloud.example\n\n[Collection]\nBackendName=webdav\n\
              Identity=alice\n";
    let sources: std::collections::HashMap<String, String> =
        [("acct".to_string(), nc.to_string())].into();
    let recent = book.replace("DisplayName=Contacts", "DisplayName=Recently contacted");
    assert_eq!(
        super::book_display_name(book, &sources),
        "CardDAV — alice · Contacts"
    );
    assert_eq!(
        super::book_display_name(&recent, &sources),
        "CardDAV — alice · Recently contacted"
    );
    // …while a standalone book (no parent) isn't named twice.
    let lone = "[Data Source]\nDisplayName=Work\n\n[Address Book]\nBackendName=carddav\n";
    assert_eq!(super::book_display_name(lone, &sources), "CardDAV — Work");
    assert!(super::data_source_disabled(
        &book.replace("Enabled=true", "Enabled=false")
    ));
    assert_eq!(
        crate::desktop::platform::keyfile_value(book, "Address Book", "Missing"),
        None
    );
}

fn card(name: &str, email: &str) -> String {
    format!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{name}\r\nFN:{name}\r\nEMAIL:{email}\r\nEND:VCARD\r\n"
    )
}

#[test]
fn books_that_fill_themselves_from_mail_are_spotted() {
    assert!(super::is_auto_collected("contact-collected", ""));
    assert!(super::is_auto_collected("x1", "Collected Addresses"));
    assert!(super::is_auto_collected(
        "x2",
        "CardDAV \u{2014} alice · Recently contacted"
    ));
    assert!(super::is_auto_collected(
        "x3",
        "Google \u{2014} me@gmail.com · Other contacts"
    ));
    assert!(!super::is_auto_collected(
        "system-address-book",
        "On This Computer"
    ));
    assert!(!super::is_auto_collected(
        "x4",
        "CardDAV \u{2014} alice · Contacts"
    ));
}

#[test]
fn the_chosen_book_comes_first() {
    let book = |uid: &str| Book {
        uid: uid.into(),
        name: uid.into(),
        groups: None,
    };
    let mut books = vec![book("a"), book("b"), book("hylki-local")];
    prefer_book(&mut books, "hylki-local");
    assert_eq!(
        books.iter().map(|b| b.uid.as_str()).collect::<Vec<_>>(),
        ["hylki-local", "a", "b"]
    );
    prefer_book(&mut books, "gone");
    prefer_book(&mut books, "");
    assert_eq!(books[0].uid, "hylki-local");
}

#[test]
fn only_the_reserved_uid_is_local() {
    assert!(is_local_book("hylki-local"));
    assert!(!is_local_book("system-address-book"));
}

#[test]
fn local_details_carry_the_book_and_the_raw_vcard() {
    let v = card("Ann", "ann@x.org");
    let out = details_from_local_vcards(vec![v.clone()], "Hylki");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].book_uid, "hylki-local");
    assert_eq!(out[0].book_name, "Hylki");
    assert_eq!(out[0].eds_uid, "Ann");
    assert_eq!(out[0].raw_vcard, v);
}

#[test]
fn same_email_in_two_books_is_kept_per_book() {
    // The EDS reader de-duplicates among EDS books by first email; the
    // local list is built on its own, so a shared address is not dropped.
    let a = details_from_local_vcards(vec![card("Ann", "same@x.org")], "Hylki");
    let b = details_from_local_vcards(vec![card("Ann", "same@x.org")], "Hylki");
    assert_eq!(a[0].primary_email(), b[0].primary_email());
    assert_eq!(a.len() + b.len(), 2);
}

#[test]
fn local_vcards_never_read_file_photos() {
    let v = "BEGIN:VCARD\r\nFN:Ann\r\nPHOTO;VALUE=uri:file:///etc/hostname\r\nEND:VCARD\r\n";
    assert!(vcard_photo_with(v, false).is_none());
}

#[test]
fn merge_target_matches_the_name_ignoring_case() {
    let vcards = vec![card("Ann Lee", "ann@x.org"), card("Bob", "bob@x.org")];
    assert!(merge_target(&vcards, " ann lee ")
        .unwrap()
        .contains("ann@x.org"));
    assert!(merge_target(&vcards, "Carl").is_none());
    assert!(merge_target(&vcards, "  ").is_none());
}

#[test]
fn an_edit_writes_addresses_birthdays_and_photos() {
    let original = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:u1\r\nFN:Pat Doe\r\n\
                    ADR;TYPE=HOME:PO 7;Flat 2;1 Main St\\, Rear;Springfield;IL;62701;USA\r\n\
                    BDAY:1985-04-12\r\nPHOTO;ENCODING=b;TYPE=PNG:iVBORw0KGgo=\r\nEND:VCARD\r\n";
    let c = super::parse_vcard_fields(original).unwrap();
    assert_eq!(c.postal[0].street, "1 Main St, Rear");
    assert_eq!(
        (c.postal[0].pobox.as_str(), c.postal[0].label.as_str()),
        ("PO 7", "Home")
    );
    assert_eq!(
        super::birthday_parts(&c.birthday_raw),
        Some((Some(1985), 4, 12))
    );
    let mut postal = c.postal.clone();
    postal[0].locality = "Shelbyville".into();
    postal.push(super::Postal {
        label: "Work".into(),
        street: "2 Side Rd".into(),
        ..Default::default()
    });
    let edit = super::ContactEdit {
        name: "Pat Doe".into(),
        addresses: postal,
        birthday: super::birthday_value(None, 4, 12),
        ..Default::default()
    };
    let out = super::patched_vcard(original, &edit);
    assert!(
        out.contains("ADR;TYPE=HOME:PO 7;Flat 2;1 Main St\\, Rear;Shelbyville;IL;62701;USA\r\n"),
        "{out}"
    );
    assert!(out.contains("ADR;TYPE=WORK:;;2 Side Rd;;;;\r\n"), "{out}");
    assert!(
        out.contains("BDAY:--04-12\r\n") && !out.contains("1985"),
        "{out}"
    );
    assert!(
        out.contains("PHOTO;ENCODING=b;TYPE=PNG:iVBORw0KGgo="),
        "a kept photo stays: {out}"
    );
    let gone = super::patched_vcard(
        original,
        &super::ContactEdit {
            photo: super::PhotoEdit::Remove,
            ..edit.clone()
        },
    );
    assert!(!gone.contains("PHOTO"), "{gone}");
    let set = super::patched_vcard(
        original,
        &super::ContactEdit {
            photo: super::PhotoEdit::Set(vec![0xFF, 0xD8, 0xFF, 1]),
            ..edit
        },
    );
    assert!(
        set.contains("PHOTO;ENCODING=b;TYPE=JPEG:/9j/AQ==\r\n") && !set.contains("iVBOR"),
        "{set}"
    );
}

#[test]
fn a_card_with_no_name_or_address_is_still_a_contact() {
    let phone = "BEGIN:VCARD\r\nVERSION:3.0\r\nTEL:+1 555 0100\r\nEND:VCARD\r\n";
    assert_eq!(
        super::parse_vcard_fields(phone).unwrap().name,
        "+1 555 0100"
    );
    let org = "BEGIN:VCARD\r\nVERSION:3.0\r\nORG:Acme;\r\nTEL:+1 555 0100\r\nEND:VCARD\r\n";
    assert_eq!(super::parse_vcard_fields(org).unwrap().name, "Acme");
    assert!(super::parse_vcard_fields("BEGIN:VCARD\r\nVERSION:3.0\r\nEND:VCARD\r\n").is_none());
    // Saved with no name, it goes by its company.
    let edit = super::ContactEdit {
        org: "Acme".into(),
        phones: vec![super::Labeled {
            label: "Work".into(),
            value: "+1 555 0100".into(),
        }],
        ..Default::default()
    };
    let out = super::new_vcard(&edit);
    assert!(
        out.contains("\r\nFN:Acme\r\n") && out.contains("TEL;TYPE=WORK:+1 555 0100"),
        "{out}"
    );
}

#[test]
fn an_account_named_in_hylki_names_its_books_account() {
    let work = "Work".to_string();
    assert_eq!(
        super::with_account_name("CardDAV — a@b.org · card", Some(&work)),
        "Work · card"
    );
    assert_eq!(
        super::with_account_name("CardDAV — a@b.org", Some(&work)),
        "Work · a@b.org"
    );
    assert_eq!(
        super::with_account_name("On This Computer · Personal", Some(&work)),
        "Work · Personal"
    );
    assert_eq!(
        super::with_account_name("CardDAV — a@b.org · card", None),
        "CardDAV — a@b.org · card"
    );
    // And a book named too.
    let family = "Family".to_string();
    assert_eq!(
        super::with_given_name(
            &super::with_account_name("CardDAV — a@b.org · card", Some(&work)),
            Some(&family)
        ),
        "Work · Family"
    );
}

#[test]
fn a_book_named_in_hylki_keeps_its_account() {
    let given = "Family".to_string();
    assert_eq!(
        super::with_given_name("CardDAV — a@b.org · card", Some(&given)),
        "CardDAV — a@b.org · Family"
    );
    assert_eq!(
        super::with_given_name("CardDAV — Work", Some(&given)),
        "CardDAV — Work · Family"
    );
    assert_eq!(
        super::with_given_name("Hylki Address Book", Some(&given)),
        "Family"
    );
    assert_eq!(
        super::with_given_name("CardDAV — a@b.org · card", None),
        "CardDAV — a@b.org · card"
    );
}

#[test]
fn a_line_goes_after_the_version() {
    assert_eq!(
        super::insert_after_version(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:A\r\nEND:VCARD\r\n",
            "UID:x"
        ),
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:x\r\nFN:A\r\nEND:VCARD\r\n"
    );
}

/// What a directory finds joins what is known, once: a known address is
/// marked as in the directory, a new one follows, and the order is the
/// completion's (#307).
#[test]
fn directory_matches_join_the_known_ones_once() {
    use super::{merge_found, Suggestion};
    let person = |name: &str, email: &str, score: u32| Suggestion {
        name: name.into(),
        email: email.into(),
        score,
        ..Default::default()
    };
    let known = vec![person("Ada Lovelace", "ada@example.org", 2)];
    let directory = vec![
        person("Ada L.", "ADA@example.org", 0),
        person("Adam Smith", "adam@example.org", 0),
        person("Adam Smith", "adam@example.org", 0),
    ];
    let found = merge_found(known, &directory, "ada");
    let emails: Vec<&str> = found.iter().map(|s| s.email.as_str()).collect();
    assert_eq!(emails, ["ada@example.org", "adam@example.org"]);
    assert!(found[0].from_directory);
    assert_eq!(found[0].name, "Ada Lovelace");
}
