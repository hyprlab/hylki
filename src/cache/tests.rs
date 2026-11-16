//! Tests of the cache's queries.

use rusqlite::{params, Connection};

use super::test_util::*;
use super::*;
use crate::models::{FolderKind, Message};

/// A smart folder's scan (#417) keeps to the folders asked for, lets the
/// prefilter and the matcher narrow it, newest first, up to the limit.
#[test]
fn smart_scan_reads_only_what_it_is_asked_for() {
    let c = Cache::in_memory().unwrap();
    for (path, uid, ts, starred, subject) in [
        ("INBOX", 1u32, 100i64, 1, "invoice one"),
        ("INBOX", 2, 300, 0, "invoice two"),
        ("Sent", 3, 200, 1, "invoice sent"),
        ("Trash", 4, 400, 1, "invoice gone"),
    ] {
        c.conn.execute(
            "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, recipients, cc, message_id, references_) \
             VALUES (1, ?1, ?2, '', '', ?3, '', ?4, 1, ?5, 0, '', '', '', '')",
            params![path, uid, subject, ts, starred],
        ).unwrap();
    }
    let uids = |v: Vec<(String, Message)>| v.into_iter().map(|(_, m)| m.uid).collect::<Vec<_>>();
    let none = crate::config::SmartPrefilter::default();
    let trash = vec!["Trash".to_string()];
    assert_eq!(
        uids(c.smart_scan(1, None, &trash, none, 10, |_| true)),
        [2, 3, 1]
    );
    let sent = vec!["Sent".to_string()];
    assert_eq!(
        uids(c.smart_scan(1, Some(&sent), &[], none, 10, |_| true)),
        [3]
    );
    let starred = crate::config::SmartPrefilter {
        starred: Some(true),
        ..none
    };
    assert_eq!(
        uids(c.smart_scan(1, None, &trash, starred, 10, |_| true)),
        [3, 1]
    );
    assert_eq!(
        uids(c.smart_scan(1, None, &trash, none, 10, |m| m.subject.ends_with("one"))),
        [1]
    );
    assert_eq!(uids(c.smart_scan(1, None, &[], none, 2, |_| true)), [4, 2]);
    assert!(c
        .smart_scan(1, Some(&[]), &[], none, 10, |_| true)
        .is_empty());
    assert!(c.smart_scan(2, None, &[], none, 10, |_| true).is_empty());
}

/// An account that suggests only its own (#410) gets the addresses in
/// its own mail, not another account's.
#[test]
fn account_addresses_are_one_accounts() {
    let c = Cache::in_memory().unwrap();
    for (account, uid, from, to, cc) in [
        (
            1u32,
            1u32,
            "Boss@Work.example",
            "me@work.example, colleague@work.example",
            "",
        ),
        (1, 2, "me@work.example", "client@elsewhere.example", "x"),
        (2, 1, "friend@home.example", "me@home.example", ""),
    ] {
        c.conn.execute(
            "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, recipients, cc, message_id, references_) \
             VALUES (?1, 'INBOX', ?2, '', ?3, '', '', 0, 0, 0, 0, ?4, ?5, '', '')",
            params![account, uid, from, to, cc],
        ).unwrap();
    }
    let mut work: Vec<String> = c.account_addresses(1).into_iter().collect();
    work.sort();
    assert_eq!(
        work,
        [
            "boss@work.example",
            "client@elsewhere.example",
            "colleague@work.example",
            "me@work.example"
        ]
    );
    assert!(c.account_addresses(3).is_empty());
}

/// Removing downloaded mail keeps the index, and an age limit only takes
/// what is older than it (#394).
#[test]
fn downloaded_mail_goes_and_the_index_stays() {
    let c = Cache::in_memory().unwrap();
    for (uid, ts) in [(1u32, 100i64), (2, 500)] {
        c.conn.execute(
            "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, recipients, cc, message_id, references_) \
             VALUES (1, 'INBOX', ?1, '', '', '', '', ?2, 0, 0, 1, '', '', '', '')",
            params![uid, ts],
        ).unwrap();
        c.conn.execute("INSERT INTO bodies (account_id, folder_path, uid, body) VALUES (1, 'INBOX', ?1, 'hello')", [uid]).unwrap();
        c.conn.execute("INSERT INTO attachments (account_id, folder_path, uid, idx, name, data) VALUES (1, 'INBOX', ?1, 0, 'a', x'0102')", [uid]).unwrap();
    }
    assert_eq!(c.downloaded_size(), (10, 4));
    assert_eq!(c.remove_downloaded(Some(300)).unwrap(), 1);
    assert_eq!(c.downloaded_size(), (5, 2));
    assert_eq!(c.remove_downloaded(None).unwrap(), 1);
    assert_eq!(c.downloaded_size(), (0, 0));
    let left: i64 = c
        .conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 2);
}

/// Removing an account moves the ones after it up a place; their cached
/// rows move with them, and the removed account's go.
#[test]
fn cached_rows_follow_their_accounts() {
    let c = Cache::in_memory().unwrap();
    let put = |id: u32, path: &str| {
        c.conn.execute("INSERT INTO folders (account_id, path, name, kind, unread, ord) VALUES (?1, ?2, ?2, 0, 0, 0)", params![id, path]).unwrap();
        c.conn
            .execute(
                "INSERT INTO bodies (account_id, folder_path, uid, body) VALUES (?1, ?2, 1, ?2)",
                params![id, path],
            )
            .unwrap();
    };
    let paths = |id: u32| -> Vec<String> {
        let mut stmt = c
            .conn
            .prepare("SELECT path FROM folders WHERE account_id = ?1 ORDER BY path")
            .unwrap();
        stmt.query_map([id], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let body = |id: u32| -> Option<String> {
        c.conn
            .query_row("SELECT body FROM bodies WHERE account_id = ?1", [id], |r| {
                r.get(0)
            })
            .ok()
    };
    put(1, "a-inbox");
    put(2, "b-inbox");
    put(3, "c-inbox");
    let all = |v: &[(u32, &str)]| {
        v.iter()
            .map(|(i, e)| (*i, e.to_string()))
            .collect::<Vec<_>>()
    };
    // A cache from before owners were kept is taken as it is.
    c.claim_accounts(&all(&[(1, "a@x"), (2, "b@x"), (3, "C@x")]))
        .unwrap();
    assert_eq!(paths(1), ["a-inbox"]);
    // The first account goes.
    c.claim_accounts(&all(&[(1, "b@x"), (2, "c@x")])).unwrap();
    assert_eq!(paths(1), ["b-inbox"]);
    assert_eq!(paths(2), ["c-inbox"]);
    assert!(paths(3).is_empty());
    assert_eq!(body(1).as_deref(), Some("b-inbox"));
    // Nothing changed: nothing moves.
    c.claim_accounts(&all(&[(1, "b@x"), (2, "c@x")])).unwrap();
    assert_eq!(paths(2), ["c-inbox"]);
    // The last account goes, and another address takes the first place.
    c.claim_accounts(&all(&[(1, "d@x")])).unwrap();
    assert!(paths(1).is_empty() && paths(2).is_empty());
    assert_eq!(body(2), None);
}

/// Rows of a folder the account does not have go, in every table of what can
/// be had again, and only those.
#[test]
fn rows_of_folders_an_account_does_not_have_go() {
    let c = Cache::in_memory().unwrap();
    let folder = |account: u32, path: &str| {
        c.conn
            .execute(
                "INSERT INTO folders (account_id, path, name, kind, unread, ord) \
                 VALUES (?1, ?2, ?2, 0, 0, 0)",
                params![account, path],
            )
            .unwrap();
    };
    let mail = |account: u32, path: &str, uid: u32| {
        let m = Message {
            id: uid,
            uid,
            subject: format!("{account} {path} {uid}"),
            message_id: format!("{account}-{path}-{uid}@example.org"),
            ..Message::blank()
        };
        c.upsert_messages(account, path, &[m]);
        c.save_body(account, path, uid, "<p>words</p>");
    };
    let count = |table: &str, account: u32, path: &str| -> i64 {
        c.conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE account_id = ?1 AND folder_path = ?2"),
                params![account, path],
                |r| r.get(0),
            )
            .unwrap()
    };
    folder(1, "INBOX");
    mail(1, "INBOX", 1);
    mail(1, "[Gmail]/All Mail", 2);
    mail(1, "Hidden", 3);
    // No folder list yet: nothing known about these.
    mail(2, "INBOX", 4);
    // Not on a server, so not for tidying.
    folder(3, "INBOX");
    mail(3, "Saved", 5);
    c.conn
        .execute(
            "INSERT INTO local_copies (account_id, folder_path, uid, message_id) \
             VALUES (1, '[Gmail]/All Mail', 2, 'x')",
            [],
        )
        .unwrap();

    let gone = c
        .drop_rows_of_missing_folders(&[(1, vec!["Hidden".into()]), (2, Vec::new())])
        .unwrap();

    assert_eq!(gone, 1);
    assert_eq!(count("messages", 1, "[Gmail]/All Mail"), 0);
    assert_eq!(count("bodies", 1, "[Gmail]/All Mail"), 0);
    assert_eq!(count("local_copies", 1, "[Gmail]/All Mail"), 1);
    for (account, path) in [(1, "INBOX"), (1, "Hidden"), (2, "INBOX"), (3, "Saved")] {
        assert_eq!(count("messages", account, path), 1, "{account} {path}");
        assert_eq!(count("bodies", account, path), 1, "{account} {path}");
    }
    // A hidden folder with no list of hidden ones, and the search index.
    assert_eq!(
        c.drop_rows_of_missing_folders(&[(1, Vec::new())]).unwrap(),
        1
    );
    assert_eq!(c.find_body_text("words", 10).len(), 3);
    assert_eq!(
        c.drop_rows_of_missing_folders(&[(1, Vec::new())]).unwrap(),
        0
    );
}

/// How long the conversation lookups take on a real cache (#259):
/// `HYLKI_CACHE_TIMING=<copy of cache.db> cargo test --release --bin hylki
/// cache::tests::thread_lookup_timing -- --ignored --nocapture`. Takes
/// each account's newest 256 Message-IDs as one page of badges, and the
/// newest reply's ids as one opened conversation. An encrypted copy also
/// needs its key in `HYLKI_CACHE_TIMING_KEY`, from
/// `secret-tool lookup service co.hyprlab.Hylki username cache-key`.
#[test]
#[ignore]
fn thread_lookup_timing() {
    let Ok(path) = std::env::var("HYLKI_CACHE_TIMING") else {
        return;
    };
    let protection = match std::env::var("HYLKI_CACHE_TIMING_KEY") {
        Ok(hex) => super::at_rest::Protection::Encrypted(
            super::at_rest::Key::from_hex(&hex).expect("a 64-digit hex key"),
        ),
        Err(_) => super::at_rest::Protection::Plain,
    };
    let c = Cache {
        conn: super::at_rest::connect(
            std::path::Path::new(&path),
            rusqlite::OpenFlags::default(),
            &protection,
        )
        .unwrap(),
        protection,
    };
    let accounts: Vec<u32> = c
        .conn
        .prepare("SELECT DISTINCT account_id FROM messages")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .flatten()
        .collect();
    for account in accounts {
        let ids: Vec<String> = c
            .conn
            .prepare("SELECT message_id FROM messages WHERE account_id = ?1 AND message_id <> '' ORDER BY ts DESC LIMIT 256")
            .unwrap()
            .query_map([account], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        let groups: Vec<(String, Vec<String>)> =
            ids.iter().map(|i| (i.clone(), vec![i.clone()])).collect();
        let at = std::time::Instant::now();
        let found = c.thread_summaries(account, &groups).len();
        let page = at.elapsed();
        let open_ids: Vec<String> = ids.iter().take(3).cloned().collect();
        let at = std::time::Instant::now();
        let members = c.messages_by_thread_ids(account, &open_ids).len();
        println!(
            "account {account}: page of {} ids {page:.1?} ({found} groups), open {:.1?} ({members} members)",
            ids.len(),
            at.elapsed()
        );
    }
}

#[test]
fn background_reader_sees_wal_updates_without_writing() {
    let path = std::env::temp_dir().join(format!(
        "hylki-cache-reader-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    let cache = Cache {
        conn: Connection::open(&path).unwrap(),
        protection: super::at_rest::Protection::Plain,
    };
    cache.conn.execute_batch(SCHEMA).unwrap();
    cache
        .conn
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    cache.save_body(1, "INBOX", 42, "first");
    let reader = cache.read_connection().unwrap();
    assert_eq!(reader.load_body(1, "INBOX", 42).as_deref(), Some("first"));
    cache.save_body(1, "INBOX", 42, "updated");
    assert_eq!(reader.load_body(1, "INBOX", 42).as_deref(), Some("updated"));
    let error = reader.conn.execute("DELETE FROM bodies", []).unwrap_err();
    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::ReadOnly)
    );
    drop(reader);
    drop(cache);
    std::fs::remove_file(path).unwrap();
}

/// The cache holds message bodies, attachment bytes and the address book, so
/// it should be no more readable than `accounts.toml` is.
///
/// Ignored by default: it sets `XDG_DATA_HOME`, which is process-global and
/// would leak into tests running beside it. Run it on its own:
///
/// ```text
/// cargo test -- --ignored --test-threads=1 the_cache_is_not_world_readable
/// ```
#[test]
#[ignore = "mutates process-global environment"]
#[cfg(unix)]
fn the_cache_is_not_world_readable() {
    use std::os::unix::fs::PermissionsExt;

    let base = std::env::temp_dir().join(format!("hylki-cache-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    std::env::set_var("XDG_DATA_HOME", &base);

    let dir = base.join("hylki");
    // Start from the permissions the old code left behind, to prove an
    // existing cache is tightened rather than only a freshly created one.
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(dir.join("cache.db"), b"").unwrap();
    std::fs::set_permissions(dir.join("cache.db"), std::fs::Permissions::from_mode(0o644)).unwrap();

    super::at_rest::unlock(super::at_rest::Protection::Encrypted(
        super::at_rest::Key::generate().unwrap(),
    ));
    let cache = Cache::open().expect("cache opens");
    drop(cache);

    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700, "cache directory");
    assert_eq!(mode(&dir.join("cache.db")), 0o600, "cache.db");
    for side in ["cache.db-wal", "cache.db-shm"] {
        let p = dir.join(side);
        if p.exists() {
            assert_eq!(mode(&p), 0o600, "{side}");
        }
    }

    std::env::remove_var("XDG_DATA_HOME");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn folder_kinds_keep_their_stored_numbers() {
    // #360: Templates came after Custom; a cache written before it
    // still reads every folder as it was.
    for kind in [
        FolderKind::Inbox,
        FolderKind::Starred,
        FolderKind::Sent,
        FolderKind::Drafts,
        FolderKind::Templates,
        FolderKind::Archive,
        FolderKind::Junk,
        FolderKind::Trash,
        FolderKind::Custom,
    ] {
        assert_eq!(kind_from_i64(kind_to_i64(kind)), kind);
    }
    assert_eq!(kind_to_i64(FolderKind::Custom), 7);
    assert_eq!(kind_to_i64(FolderKind::Templates), 8);
}

#[test]
fn message_ids_go_back_on_the_wire_in_the_case_they_arrived() {
    let c = Cache::in_memory().unwrap();
    note_msgid_case("vireo-12tPxIJKt7PhQVbIiO@gmail.com");
    note_msgid_case("plain@example.com"); // all lowercase: nothing to note
                                          // Known before it is on disk (noted, not yet carried by a batch).
    assert_eq!(
        c.exact_msgids("vireo-12tpxijkt7phqvbiio@gmail.com plain@example.com"),
        "vireo-12tPxIJKt7PhQVbIiO@gmail.com plain@example.com"
    );
    // The next batch of messages writes it down; then it is on disk.
    c.upsert_messages(1, "INBOX", &[]);
    assert!(
        msgid_case_pending().lock().unwrap().is_empty(),
        "drained by the batch"
    );
    assert_eq!(
        c.exact_msgids("vireo-12tpxijkt7phqvbiio@gmail.com"),
        "vireo-12tPxIJKt7PhQVbIiO@gmail.com"
    );
    // An id never seen with capitals is left as stored.
    assert_eq!(c.exact_msgids("unknown@x"), "unknown@x");
    assert_eq!(c.exact_msgids(""), "");
}

#[test]
fn outbox_round_trips_a_queued_message() {
    let c = Cache::in_memory().unwrap();
    let rcpts = vec!["ada@example.com".to_string(), "bcc@example.com".to_string()];
    let id = c
        .queue_outbox(
            1,
            "me@example.com",
            &rcpts,
            "Ada Lovelace <ada@example.com>",
            "Notes",
            "the body preview",
            b"From: me\r\n\r\nbody",
            Some("Sent"),
            "connection refused",
            None,
        )
        .expect("queued");

    let items = c.outbox_items(1);
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item.id, id);
    // The envelope has to survive verbatim — Bcc exists only here, so losing a
    // recipient silently drops someone from the message.
    assert_eq!(item.rcpts, rcpts);
    assert_eq!(item.from_addr, "me@example.com");
    assert_eq!(item.subject, "Notes");
    assert_eq!(item.preview, "the body preview");
    assert_eq!(item.raw, b"From: me\r\n\r\nbody");
    assert_eq!(item.sent_path.as_deref(), Some("Sent"));
    assert_eq!(item.attempts, 1);
    assert_eq!(item.last_error, "connection refused");
    assert!(item.queued_at > 0);

    // Another account's queue is its own.
    assert!(c.outbox_items(2).is_empty());

    c.record_outbox_failure(id, "no route to host");
    let item = c.outbox_items(1).remove(0);
    assert_eq!(item.attempts, 2);
    assert_eq!(item.last_error, "no route to host");

    c.delete_outbox(id);
    assert!(c.outbox_items(1).is_empty());
}

/// Send Later (#145): the scheduled time survives the round trip, and a
/// message queued to go now has none.
#[test]
fn outbox_keeps_a_scheduled_time() {
    let c = Cache::in_memory().unwrap();
    let rcpts = vec!["ada@example.com".to_string()];
    let later = c
        .queue_outbox(
            1,
            "me@example.com",
            &rcpts,
            "ada",
            "Later",
            "",
            b"raw",
            None,
            "",
            Some(1_900_000_000),
        )
        .expect("queued");
    let now = c
        .queue_outbox(
            1,
            "me@example.com",
            &rcpts,
            "ada",
            "Now",
            "",
            b"raw",
            None,
            "",
            None,
        )
        .expect("queued");
    let items = c.outbox_items(1);
    let by = |id: u32| items.iter().find(|i| i.id == id).expect("listed");
    assert_eq!(by(later).send_at, Some(1_900_000_000));
    assert_eq!(by(now).send_at, None);
    assert!(
        by(later).as_message().preview.starts_with("Scheduled for "),
        "{}",
        by(later).as_message().preview
    );
}

#[test]
fn outbox_keeps_the_order_messages_were_queued_in() {
    let c = Cache::in_memory().unwrap();
    for (subject, at) in [("second", 200), ("first", 100), ("third", 300)] {
        c.conn
            .execute(
                "INSERT INTO outbox(account_id, from_addr, rcpts, recipients, subject, \
                 preview, raw, sent_path, queued_at, attempts, last_error) \
                 VALUES(1, 'me@example.com', 'a@b.com', '', ?1, '', x'00', NULL, ?2, 1, '')",
                params![subject, at],
            )
            .unwrap();
    }
    let subjects: Vec<String> = c.outbox_items(1).into_iter().map(|i| i.subject).collect();
    assert_eq!(subjects, ["first", "second", "third"]);
}

/// Graph, JMAP and POP3 store a folder whole. The preview went missing
/// on the way, so the list served from the cache had none and every sync
/// blanked the previews until the server's answer came in (#330).
#[test]
fn a_folder_saved_whole_keeps_its_previews() {
    let c = Cache::in_memory().unwrap();
    let m = Message {
        id: 9,
        account_id: 1,
        folder_id: 2,
        uid: 9,
        from_name: "Ada".into(),
        from_addr: "ada@example.com".into(),
        reply_to: String::new(),
        to: String::new(),
        cc: String::new(),
        bcc: String::new(),
        subject: "Hello".into(),
        preview: "The first line".into(),
        body: String::new(),
        date: String::new(),
        timestamp: 1,
        unread: false,
        starred: false,
        answered: false,
        forwarded: false,
        keywords: Vec::new(),
        has_attachment: false,
        message_id: String::new(),
        references: String::new(),
        importance: Default::default(),
        due: 0,
        labels: Vec::new(),
    };
    c.save_messages(1, "Inbox", std::slice::from_ref(&m));
    assert_eq!(c.load_messages(1, "Inbox", 2), [m]);
}

/// Replied and forwarded come back from the cache, by every query that
/// reads a row, and a reply sent from here marks its original (#454).
#[test]
fn replied_and_forwarded_round_trip_through_the_cache() {
    let c = Cache::in_memory().unwrap();
    let m = |uid: u32, answered: bool| Message {
        id: uid,
        account_id: 1,
        folder_id: 1,
        uid,
        from_name: "Ada".into(),
        from_addr: "ada@example.com".into(),
        reply_to: String::new(),
        to: String::new(),
        cc: String::new(),
        bcc: String::new(),
        subject: "Hello".into(),
        preview: String::new(),
        body: String::new(),
        date: String::new(),
        timestamp: uid as i64,
        unread: false,
        starred: false,
        answered,
        forwarded: false,
        keywords: vec!["Work".into()],
        has_attachment: false,
        message_id: format!("m{uid}@x"),
        references: String::new(),
        importance: Default::default(),
        due: 0,
        labels: Vec::new(),
    };
    c.upsert_messages(1, "INBOX", &[m(1, true), m(2, false)]);
    let flags = |c: &Cache| -> Vec<(bool, bool)> {
        c.load_messages(1, "INBOX", 1)
            .iter()
            .map(|m| (m.answered, m.forwarded))
            .collect()
    };
    assert_eq!(flags(&c), [(false, false), (true, false)]);
    c.set_answered(1, "INBOX", 2, true);
    c.set_answered(1, "INBOX", 2, false);
    assert_eq!(flags(&c), [(true, true), (true, false)]);
    assert_eq!(c.answer_state(1, "INBOX", 2), (true, true));
    assert_eq!(c.answer_state(1, "INBOX", 9), (false, false), "no such row");
    let (_, tagged) = c
        .messages_with_keyword(1, "Work", "Work")
        .into_iter()
        .find(|(_, m)| m.uid == 2)
        .unwrap();
    assert!(tagged.answered && tagged.forwarded);
    // The server's word replaces it at the next sync.
    c.upsert_messages(1, "INBOX", &[m(2, false)]);
    assert_eq!(flags(&c), [(false, false), (true, false)]);
}

/// A sent copy's Bcc comes back from the cache, by every query that
/// reads a row (#476).
#[test]
fn a_sent_copys_bcc_round_trips_through_the_cache() {
    let c = Cache::in_memory().unwrap();
    let m = Message {
        id: 3,
        account_id: 1,
        folder_id: 4,
        uid: 3,
        from_name: "Me".into(),
        from_addr: "me@example.com".into(),
        reply_to: String::new(),
        to: "ada@example.com".into(),
        cc: String::new(),
        bcc: "hidden@example.com, other@example.com".into(),
        subject: "Hello".into(),
        preview: String::new(),
        body: String::new(),
        date: String::new(),
        timestamp: 1,
        unread: false,
        starred: false,
        answered: false,
        forwarded: false,
        keywords: vec!["Work".into()],
        has_attachment: false,
        message_id: "s1@x".into(),
        references: String::new(),
        importance: Default::default(),
        due: 0,
        labels: Vec::new(),
    };
    c.save_messages(1, "Sent", std::slice::from_ref(&m));
    assert_eq!(c.load_messages(1, "Sent", 4), std::slice::from_ref(&m));
    let (_, tagged) = c.messages_with_keyword(1, "Work", "Work").pop().unwrap();
    assert_eq!(tagged.bcc, m.bcc);
    c.upsert_messages(
        1,
        "Sent",
        &[Message {
            bcc: "hidden@example.com".into(),
            ..m
        }],
    );
    assert_eq!(c.load_messages(1, "Sent", 4)[0].bcc, "hidden@example.com");
}

/// A queued forward remembers what it forwards (#454).
#[test]
fn a_queued_forward_keeps_its_original() {
    let c = Cache::in_memory().unwrap();
    let id = c
        .queue_outbox(
            1,
            "me@x",
            &["ada@x".into()],
            "Ada",
            "Fwd: Hello",
            "",
            b"raw",
            None,
            "",
            None,
        )
        .unwrap();
    assert_eq!(c.outbox_items(1)[0].forwards, "");
    c.set_outbox_forwards(id, "m1@x");
    assert_eq!(c.outbox_items(1)[0].forwards, "m1@x");
}

/// Gmail labels may hold spaces, so they cannot share the keywords'
/// space-joined column (#374).
#[test]
fn gmail_labels_round_trip_through_the_cache() {
    let c = Cache::in_memory().unwrap();
    let mut m = Message {
        id: 4,
        account_id: 1,
        folder_id: 1,
        uid: 4,
        from_name: "Ada".into(),
        from_addr: "ada@example.com".into(),
        reply_to: String::new(),
        to: String::new(),
        cc: String::new(),
        bcc: String::new(),
        subject: "Hello".into(),
        preview: String::new(),
        body: String::new(),
        date: String::new(),
        timestamp: 1,
        unread: false,
        starred: false,
        answered: false,
        forwarded: false,
        keywords: vec!["$label1".into()],
        has_attachment: false,
        message_id: String::new(),
        references: String::new(),
        importance: Default::default(),
        due: 0,
        labels: vec![
            "My Label".into(),
            "Work/Projects".into(),
            "&AOk-t&AOk-".into(),
        ],
    };
    c.upsert_messages(1, "INBOX", std::slice::from_ref(&m));
    assert_eq!(c.load_messages(1, "INBOX", 1), [m.clone()]);
    let (_, by_keyword) = c
        .messages_with_keyword(1, "$label1", "Urgent")
        .pop()
        .unwrap();
    assert_eq!(by_keyword.labels, m.labels);
    // A label named like a tag finds the message for that tag (#374),
    // whatever its case, the non-ASCII one included.
    assert_eq!(c.messages_with_keyword(1, "work-kw", "my label").len(), 1);
    assert_eq!(c.messages_with_keyword(1, "x", "été").len(), 1);
    assert_eq!(
        c.messages_with_keyword(1, "x", "Work").len(),
        0,
        "a nested label is its own name"
    );
    assert_eq!(c.messages_with_keyword(1, "nothing", "Nothing").len(), 0);

    // A label taken off in Gmail comes off here at the next sync.
    m.labels.clear();
    c.upsert_messages(1, "INBOX", std::slice::from_ref(&m));
    assert!(c.load_messages(1, "INBOX", 1)[0].labels.is_empty());

    // The one-time fill writes labels alone, and walks down the folder.
    assert_eq!(c.label_fill_state(1, "INBOX"), (u32::MAX, false));
    assert_eq!(c.uids_below(1, "INBOX", u32::MAX, 10), [4]);
    assert!(c.uids_below(1, "INBOX", 4, 10).is_empty());
    let found = std::collections::HashMap::from([(4, vec!["Filled In".to_string()]), (99, vec![])]);
    assert_eq!(c.set_gmail_labels(1, "INBOX", &found), 1);
    assert_eq!(
        c.set_gmail_labels(1, "INBOX", &found),
        0,
        "nothing new the second time"
    );
    let back = c.load_messages(1, "INBOX", 1).remove(0);
    assert_eq!(back.labels, ["Filled In"]);
    assert_eq!(back.keywords, ["$label1"]);
    c.set_label_fill_state(1, "INBOX", 4, true);
    assert_eq!(c.label_fill_state(1, "INBOX"), (4, true));
}

#[test]
fn redecode_encoded_subjects_fixes_raw_encoded_words_in_place() {
    let c = Cache::in_memory().unwrap();
    add_folder(&c, "INBOX", FolderKind::Inbox);
    // A subject an older build stored raw after aborting on the over-long word.
    let raw = "=?utf-8?Q?92=2Dyear=2Dold=20artist=20Sheila=20Hicks?=";
    add_msg(&c, "INBOX", 1, "Popova", raw, 100);
    add_msg(&c, "INBOX", 2, "Plain", "Already fine", 200);

    Cache::redecode_encoded_subjects(&c.conn);

    let subj: String = c
        .conn
        .query_row("SELECT subject FROM messages WHERE uid = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(subj, "92-year-old artist Sheila Hicks");
    let plain: String = c
        .conn
        .query_row("SELECT subject FROM messages WHERE uid = 2", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(plain, "Already fine");
}

/// v21: a suspicious verdict goes with its body, to be checked again;
/// a passing one and its body stay.
#[test]
fn recheck_suspicious_senders_drops_only_those_verdicts_and_bodies() {
    use crate::models::{SenderCheck, SenderTrust};
    let c = Cache::in_memory().unwrap();
    let check = |trust| SenderCheck {
        trust,
        ..Default::default()
    };
    c.save_body(1, "INBOX", 1, "<p>flagged</p>");
    c.save_sender_check(1, "INBOX", 1, &check(SenderTrust::Suspicious));
    c.save_body(1, "INBOX", 2, "<p>fine</p>");
    c.save_sender_check(1, "INBOX", 2, &check(SenderTrust::Pass));

    Cache::recheck_suspicious_senders(&c.conn);

    assert!(c.load_body(1, "INBOX", 1).is_none());
    assert!(c.load_sender_check(1, "INBOX", 1).is_none());
    assert!(c.load_body(1, "INBOX", 2).is_some());
    assert_eq!(
        c.load_sender_check(1, "INBOX", 2).map(|k| k.trust),
        Some(SenderTrust::Pass)
    );
}

/// v24: plain bodies that may hold a flowed soft break go, to render again
/// with their lines joined; HTML and other plain bodies stay.
#[test]
fn drop_soft_broken_plain_bodies_keeps_html_and_fixed_text() {
    use crate::worker::wrap_plain;
    let c = Cache::in_memory().unwrap();
    c.save_body(1, "INBOX", 1, &wrap_plain("soft \r\nbreak"));
    c.save_body(1, "INBOX", 2, &wrap_plain("hard\r\nbreak"));
    c.save_body(1, "INBOX", 3, "<p>html \n</p>");
    c.save_body(
        1,
        "INBOX",
        4,
        "<div class=\"hylki-plain\">soft \nbreak</div>",
    );

    Cache::drop_soft_broken_plain_bodies(&c.conn);

    assert!(c.load_body(1, "INBOX", 1).is_none());
    assert!(c.load_body(1, "INBOX", 2).is_some());
    assert!(c.load_body(1, "INBOX", 3).is_some());
    assert!(c.load_body(1, "INBOX", 4).is_none());
}

/// v25: plain bodies with an `@` go, to render again with their addresses
/// linked; HTML and other plain bodies stay.
#[test]
fn drop_plain_bodies_with_addresses_keeps_html_and_other_text() {
    use crate::worker::wrap_plain;
    let c = Cache::in_memory().unwrap();
    c.save_body(1, "INBOX", 1, &wrap_plain("mail ann@example.com"));
    c.save_body(1, "INBOX", 2, &wrap_plain("no address"));
    c.save_body(1, "INBOX", 3, "<p>ann@example.com</p>");
    c.save_body(
        1,
        "INBOX",
        4,
        "<div class=\"hylki-plain\">@ann@example.org</div>",
    );

    Cache::drop_plain_bodies_with_addresses(&c.conn);

    assert!(c.load_body(1, "INBOX", 1).is_none());
    assert!(c.load_body(1, "INBOX", 2).is_some());
    assert!(c.load_body(1, "INBOX", 3).is_some());
    assert!(c.load_body(1, "INBOX", 4).is_none());
}

#[test]
fn clean_quoted_names_strips_the_escapes_in_place() {
    let c = Cache::in_memory().unwrap();
    add_folder(&c, "INBOX", FolderKind::Inbox);
    add_msg(
        &c,
        "INBOX",
        1,
        r#"\"Sender Name\""#,
        r#"Re: \"Plans\""#,
        100,
    );
    add_msg(&c, "INBOX", 2, "Plain Name", "Already fine", 200);

    Cache::clean_quoted_names(&c.conn);

    let row = |uid: u32| -> (String, String) {
        c.conn
            .query_row(
                "SELECT from_name, subject FROM messages WHERE uid = ?1",
                [uid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    };
    assert_eq!(
        row(1),
        ("Sender Name".to_string(), r#"Re: "Plans""#.to_string())
    );
    assert_eq!(
        row(2),
        ("Plain Name".to_string(), "Already fine".to_string())
    );
}

/// A chip number the server gave since the last full listing is what the
/// next launch starts from: a connect keeps the cached numbers (#370), and
/// a Drafts chip of 1 came back at every start after the draft was gone.
#[test]
fn a_folders_fresh_chip_number_outlasts_the_session() {
    let c = Cache::in_memory().unwrap();
    let folder = |path: &str, kind: FolderKind, unread: u32| crate::models::Folder {
        id: 0,
        account_id: 1,
        name: path.to_string(),
        path: path.to_string(),
        kind,
        unread,
        childless: false,
    };
    c.save_folders(
        1,
        &[
            folder("INBOX", FolderKind::Inbox, 3),
            folder("Drafts", FolderKind::Drafts, 1),
        ],
    );
    c.set_folder_unread(1, "Drafts", 0);
    // A folder the cache does not hold, or another account's, changes nothing.
    c.set_folder_unread(1, "Gone", 5);
    c.set_folder_unread(2, "INBOX", 9);
    let counts: Vec<(String, u32)> = c
        .load_folders(1)
        .into_iter()
        .map(|f| (f.path, f.unread))
        .collect();
    assert_eq!(
        counts,
        vec![("INBOX".to_string(), 3), ("Drafts".to_string(), 0)]
    );
}
