use super::*;

/// A folder of its own for one test.
fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hylki-tb-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A book as Thunderbird writes one: `cards` are (card, property, value).
fn write_book(path: &Path, cards: &[(&str, &str, &str)], lists: &[(&str, &str, &[&str])]) {
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch(
        "CREATE TABLE properties (card TEXT, name TEXT, value TEXT);
         CREATE TABLE lists (uid TEXT PRIMARY KEY, name TEXT, nickName TEXT, description TEXT);
         CREATE TABLE list_cards (list TEXT, card TEXT, PRIMARY KEY(list, card));",
    )
    .unwrap();
    for (card, name, value) in cards {
        db.execute(
            "INSERT INTO properties VALUES (?1, ?2, ?3)",
            [card, name, value],
        )
        .unwrap();
    }
    for (uid, name, members) in lists {
        db.execute("INSERT INTO lists VALUES (?1, ?2, '', '')", [uid, name])
            .unwrap();
        for m in *members {
            db.execute("INSERT INTO list_cards VALUES (?1, ?2)", [uid, m])
                .unwrap();
        }
    }
}

const ANNA: &str = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Anna Berg\r\nN:Berg;Anna;;;\r\n\
EMAIL;PREF=1:anna@example.com\r\nTEL;TYPE=cell;VALUE=TEXT:+46 70 123\r\nEND:VCARD\r\n";

#[test]
fn reads_vcards_and_older_cards_with_their_uids_and_lists() {
    let dir = test_dir("read");
    let path = dir.join("abook.sqlite");
    write_book(
        &path,
        &[
            ("uid-anna", "_vCard", ANNA),
            ("uid-anna", "PrimaryEmail", "anna@example.com"),
            ("uid-old", "FirstName", "Olle"),
            ("uid-old", "LastName", "Ek"),
            ("uid-old", "PrimaryEmail", "olle@example.com"),
            ("uid-old", "WorkPhone", "555"),
            ("uid-none", "PopularityIndex", "3"),
        ],
        &[("list-1", "Choir", &["uid-anna", "uid-old"])],
    );
    let found = read_book(&path).unwrap();
    assert_eq!(found.cards.len(), 2);
    assert_eq!(found.skipped, 1);
    let anna = &found.cards[0];
    assert!(anna.contains("FN:Anna Berg"));
    assert!(anna.contains("UID:uid-anna"));
    assert!(anna.contains("CATEGORIES:Choir"));
    let parsed = crate::contacts::details::parse_vcard_fields(anna).unwrap();
    assert_eq!(parsed.name, "Anna Berg");
    assert_eq!(parsed.primary_email(), Some("anna@example.com"));
    let old = &found.cards[1];
    assert!(old.contains("Olle Ek"), "{old}");
    assert!(old.contains("olle@example.com"));
    assert!(old.contains("555"));
    assert!(old.contains("UID:uid-old"));
    assert_eq!(found.groups, vec![("Choir".to_string(), vec![0, 1])]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_card_keeps_its_own_uid_and_categories() {
    let card = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:mine\r\nFN:B\r\nCATEGORIES:Work\r\nEND:VCARD\r\n";
    let found = contacts_of(
        vec![(
            "row".to_string(),
            HashMap::from([("_vCard".to_string(), card.to_string())]),
        )],
        vec![("Friends".to_string(), vec!["row".to_string()])],
    );
    let c = &found.cards[0];
    assert!(c.contains("UID:mine") && !c.contains("UID:row"));
    assert_eq!(card_categories(c), vec!["Work", "Friends"]);
}

#[test]
fn finds_the_books_of_every_profile_default_first() {
    let root = test_dir("profiles");
    std::fs::write(
        root.join("profiles.ini"),
        "[Profile1]\nName=old\nIsRelative=1\nPath=a.old\n\n\
         [Profile0]\nName=default-release\nIsRelative=1\nPath=b.default-release\n\n\
         [InstallABC]\nDefault=b.default-release\n",
    )
    .unwrap();
    let (old, main) = (root.join("a.old"), root.join("b.default-release"));
    for p in [&old, &main] {
        std::fs::create_dir_all(p).unwrap();
        write_book(
            &p.join("abook.sqlite"),
            &[("u", "PrimaryEmail", "x@y.z")],
            &[],
        );
    }
    write_book(
        &main.join("history.sqlite"),
        &[("h", "PrimaryEmail", "h@y.z")],
        &[],
    );
    write_book(
        &main.join("abook-1.sqlite"),
        &[("w", "PrimaryEmail", "w@y.z")],
        &[],
    );
    write_book(&main.join("abook-2.sqlite"), &[], &[]);
    std::fs::write(
        main.join("prefs.js"),
        "user_pref(\"ldap_2.servers.Work.description\", \"Work \\\"A\\\"\");\n\
         user_pref(\"ldap_2.servers.Work.dirType\", 101);\n\
         user_pref(\"ldap_2.servers.Work.filename\", \"abook-1.sqlite\");\n\
         user_pref(\"ldap_2.servers.Empty.filename\", \"abook-2.sqlite\");\n\
         user_pref(\"ldap_2.servers.Dir.dirType\", 0);\n\
         user_pref(\"ldap_2.servers.Dir.filename\", \"ldap.sqlite\");\n",
    )
    .unwrap();
    write_book(&main.join("ldap.sqlite"), &[], &[]);

    let books = books_in(&root);
    let seen: Vec<(&str, &str, usize, bool)> = books
        .iter()
        .map(|b| (b.profile.as_str(), b.name.as_str(), b.contacts, b.collected))
        .collect();
    assert_eq!(
        seen,
        vec![
            ("default-release", "Personal Address Book", 1, false),
            ("default-release", "Work \"A\"", 1, false),
            ("default-release", "Collected Addresses", 1, true),
            ("old", "Personal Address Book", 1, false),
        ]
    );
    // A profile folder picked on its own, and the folder holding the root.
    assert_eq!(books_in(&main).len(), 3);
    assert!(books_in(&root.join("missing")).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn tells_a_book_from_other_files() {
    let dir = test_dir("kind");
    let book = dir.join("abook.sqlite");
    write_book(&book, &[], &[]);
    assert!(is_sqlite(&book));
    let text = dir.join("a.vcf");
    std::fs::write(&text, ANNA).unwrap();
    assert!(!is_sqlite(&text));
    let other = dir.join("other.sqlite");
    rusqlite::Connection::open(&other)
        .unwrap()
        .execute_batch("CREATE TABLE t (x);")
        .unwrap();
    assert!(book_at(&other).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reads_user_pref_lines() {
    assert_eq!(
        user_pref("user_pref(\"a.b\", \"c\");"),
        Some(("a.b".to_string(), serde_json::json!("c")))
    );
    assert_eq!(
        user_pref("user_pref(\"a.n\", 102);"),
        Some(("a.n".to_string(), serde_json::json!(102)))
    );
    assert_eq!(user_pref("// comment"), None);
}
