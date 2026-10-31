use std::cell::RefCell;
use std::rc::Rc as SharedRc;

use super::super::maildir::tests::Scratch;
use super::super::maildir::Maildir;
use super::*;
use crate::cache::Cache;

const MBOX: &str = "From a@x Tue Jul  1 10:52:37 2025\n\
From: a@x\n\
Subject: one\n\
Message-ID: <one@x>\n\
Status: RO\n\
\n\
Hello\n\
>From the archive\n\
\n\
From b@x Tue Jul  1 10:53:00 2025\n\
From: b@x\n\
Subject: two\n\
Message-ID: <two@x>\n\
X-Mozilla-Status: 0005\n\
\n\
Body two\n\
\n\
From c@x Tue Jul  1 10:54:00 2025\n\
From: c@x\n\
Subject: gone\n\
X-Mozilla-Status: 0009\n\
\n\
Deleted\n";

#[test]
fn an_mbox_splits_on_its_from_lines_and_reads_its_flags() {
    let dir = Scratch::new();
    let file = dir.path().join("Lists.mbox");
    std::fs::write(&file, MBOX).unwrap();
    let items = plan_mbox(&file).unwrap();
    assert_eq!(items.len(), 3);
    assert!(items.iter().all(|i| i.dest == "Lists"));
    let (raw, flags, _) = read_item(&items[0].source).unwrap().unwrap();
    let text = String::from_utf8(raw).unwrap();
    assert!(text.starts_with("From: a@x\n"));
    assert!(text.ends_with("Hello\nFrom the archive\n"), "{text:?}");
    assert_eq!(flags, "S");
    let (_, flags, _) = read_item(&items[1].source).unwrap().unwrap();
    assert_eq!(flags, "FS");
    // Thunderbird's expunged flag: left out.
    assert!(read_item(&items[2].source).unwrap().is_none());
}

#[test]
fn thunderbird_folders_nest_by_their_sbd_directories() {
    let dir = Scratch::new();
    assert_eq!(mbox_folder(&dir.path().join("Inbox")), INBOX);
    assert_eq!(
        mbox_folder(&dir.path().join("Work.sbd/Projects.sbd/Old")),
        "Work/Projects/Old"
    );
    assert_eq!(mbox_folder(&dir.path().join("Lists.mbox")), "Lists");
}

/// A tree the way Evolution and Dovecot keep it, a folder nested on disk,
/// and a message waiting to be expunged.
fn source_tree(root: &Path) {
    let msg = |id: &str| format!("From: a@x\r\nSubject: s\r\nMessage-ID: <{id}>\r\n\r\nb\r\n");
    for d in [
        "cur",
        "new",
        "tmp",
        ".Work_2E2025/cur",
        ".Trash/cur",
        "Nested/cur",
        "Nested/Deeper/cur",
    ] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::write(root.join("cur/1.a.h:2,S"), msg("in@x")).unwrap();
    std::fs::write(root.join("new/2.b.h"), msg("new@x")).unwrap();
    std::fs::write(root.join(".Work_2E2025/cur/3.c.h:2,Fa"), msg("work@x")).unwrap();
    std::fs::write(root.join(".Work_2E2025/dovecot-keywords"), "0 Urgent\n").unwrap();
    std::fs::write(root.join(".Trash/cur/4.d.h:2,ST"), msg("gone@x")).unwrap();
    std::fs::write(root.join("Nested/Deeper/cur/5.e.h:2,"), msg("deep@x")).unwrap();
}

#[test]
fn a_maildir_tree_is_planned_folder_by_folder() {
    let dir = Scratch::new();
    let root = dir.path().join("evolution");
    source_tree(&root);
    let items = plan_maildir(&root).unwrap();
    let mut got: Vec<(String, String, Vec<String>)> = items
        .iter()
        .map(|i| match &i.source {
            Source::File {
                flags, keywords, ..
            } => (i.dest.clone(), flags.clone(), keywords.clone()),
            Source::Mbox { .. } => unreachable!(),
        })
        .collect();
    got.sort();
    assert_eq!(
        got,
        [
            ("INBOX".into(), String::new(), vec![]),
            ("INBOX".into(), "S".into(), vec![]),
            ("Nested/Deeper".into(), String::new(), vec![]),
            ("Work.2025".into(), "F".into(), vec!["Urgent".to_string()]),
        ]
    );
}

#[test]
fn an_import_stores_everything_once() {
    let dir = Scratch::new();
    let src = dir.path().join("evolution");
    source_tree(&src);
    let events = SharedRc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    let store = Maildir::open(dir.path().join("local")).unwrap();
    let mut w = LocalWorker::new(
        1,
        crate::config::local_account(),
        store,
        Some(Cache::in_memory().unwrap()),
        move |e| sink.borrow_mut().push(e),
    );
    for _ in 0..2 {
        w.on_import(vec![src.clone()], false);
        while !w.import.is_empty() {
            w.import_step();
        }
    }
    assert_eq!(w.store.scan(INBOX).unwrap().len(), 2);
    let work = w.store.scan("Work.2025").unwrap();
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].keywords, ["Urgent"]);
    assert!(work[0].has('F'));
    assert_eq!(w.store.scan("Nested/Deeper").unwrap().len(), 1);
    assert!(w.folder("Work.2025").is_some());
    // Its own tree is not imported into itself.
    let own = w.store.root().to_path_buf();
    w.on_import(vec![own], false);
    assert!(w.import.is_empty());
}
