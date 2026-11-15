use super::*;

/// IMPORT_LIVE=<csv>: import the file twice into the first writable EDS
/// book and into the Hylki book, and check the second import only
/// updates. For the sealed EDS rig (temp/eds-rig), never a real session.
#[test]
#[ignore]
fn live_import() {
    let Ok(csv) = std::env::var("IMPORT_LIVE") else {
        return;
    };
    let path = PathBuf::from(&csv);
    let table = parse_csv(&std::fs::read(&path).unwrap()).unwrap();
    let fields = detect(&table);
    let file = ImportFile::Csv { path, fields };
    let mut books: Vec<Book> = crate::contacts::writable_books()
        .into_iter()
        .filter(|b| !is_local_book(&b.uid))
        .take(1)
        .collect();
    println!("EDS books: {books:?}");
    assert!(!books.is_empty(), "the rig has no EDS book");
    books.push(crate::contacts::local_book());
    for book in books {
        let req = ImportRequest {
            book: book.clone(),
            files: vec![file.clone()],
        };
        let first = run(&req).unwrap();
        let second = run(&req).unwrap();
        println!("{}: first {first:?}\n  second {second:?}", book.name);
        assert!(first.added > 0 && first.updated == 0, "{first:?}");
        assert_eq!((second.added, second.updated), (0, first.added));
        // Read back what the book holds.
        std::thread::sleep(std::time::Duration::from_millis(800));
        let held: Vec<_> = crate::contacts::read_contact_details()
            .into_iter()
            .filter(|c| c.book_uid == book.uid)
            .collect();
        for c in &held {
            let members: Vec<_> = c.members.iter().map(|m| m.email.as_str()).collect();
            println!(
                "  {} | emails {:?} | phones {} | group {:?} {members:?}",
                c.name,
                c.emails
                    .iter()
                    .map(|e| (&e.label, &e.value))
                    .collect::<Vec<_>>(),
                c.phones.len(),
                c.group
            );
        }
        let people = held.iter().filter(|c| !c.is_group()).count();
        assert_eq!(people, first.added, "a second import made duplicates");
        assert!(held.iter().any(|c| c.is_group()), "no group was made");
    }
    // IMPORT_LIVE_MORE=<csv>: a second file naming the same groups adds
    // to them rather than making new ones.
    let Ok(more) = std::env::var("IMPORT_LIVE_MORE") else {
        return;
    };
    let path = PathBuf::from(&more);
    let table = parse_csv(&std::fs::read(&path).unwrap()).unwrap();
    let book = crate::contacts::writable_books()
        .into_iter()
        .find(|b| !is_local_book(&b.uid))
        .unwrap();
    let req = ImportRequest {
        book: book.clone(),
        files: vec![ImportFile::Csv {
            fields: detect(&table),
            path,
        }],
    };
    println!("more: {:?}", run(&req).unwrap());
    std::thread::sleep(std::time::Duration::from_millis(800));
    let groups: Vec<_> = crate::contacts::read_contact_details()
        .into_iter()
        .filter(|c| c.book_uid == book.uid && c.is_group())
        .map(|g| (g.name, g.members.len()))
        .collect();
    println!("groups: {groups:?}");
    let friends: Vec<_> = groups.iter().filter(|(n, _)| n == "CSV Friends").collect();
    assert_eq!(friends.len(), 1, "{groups:?}");
    assert_eq!(friends[0].1, 3);
}
