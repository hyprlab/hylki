use std::cell::RefCell;
use std::rc::Rc;

use super::maildir::tests::Scratch;
use super::*;

const RAW: &[u8] = b"From: Ann <ann@example.org>\r\n\
To: me@example.com\r\n\
Subject: Kept\r\n\
Date: Tue, 1 Jul 2025 10:52:37 +0200\r\n\
Message-ID: <kept-1@example.org>\r\n\r\nThe body\r\n";

type Events = Rc<RefCell<Vec<WorkerEvent>>>;

/// A worker over a fresh tree and an in-memory cache, and what it emits.
fn worker(dir: &Scratch) -> (LocalWorker<impl Fn(WorkerEvent)>, Events) {
    let events: Events = Rc::default();
    let sink = events.clone();
    let store = Maildir::open(dir.path().join("local")).expect("tree");
    let cache = Cache::in_memory().expect("cache");
    let w = LocalWorker::new(
        1,
        crate::config::local_account(),
        store,
        Some(cache),
        move |e| sink.borrow_mut().push(e),
    );
    (w, events)
}

fn run(w: &mut LocalWorker<impl Fn(WorkerEvent)>, req: MailRequest) {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(w.handle(req));
}

fn import(w: &mut LocalWorker<impl Fn(WorkerEvent)>, path: &str, seen: bool) {
    run(
        w,
        MailRequest::ImportRaw {
            token: 7,
            path: path.to_string(),
            raw: RAW.to_vec(),
            seen,
            flagged: false,
        },
    );
}

/// The messages of the last list of `folder_id` the worker showed.
fn last_list(events: &Events, folder_id: u32) -> Vec<Message> {
    events
        .borrow()
        .iter()
        .rev()
        .find_map(|e| match e {
            WorkerEvent::Messages {
                folder_id: f,
                messages,
            } if *f == folder_id => Some(messages.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// A new account shows its Trash; its Inbox, empty, stays out of sight
/// until mail is put in it, and the Trash keeps its id either way.
#[test]
fn an_empty_inbox_is_not_listed() {
    let dir = Scratch::new();
    let (mut w, events) = worker(&dir);
    let kinds = |w: &LocalWorker<_>| -> Vec<(String, FolderKind, u32)> {
        w.folders
            .iter()
            .map(|f| (f.path.clone(), f.kind, f.id))
            .collect()
    };
    assert_eq!(kinds(&w), [("Trash".to_string(), FolderKind::Trash, 2)]);
    // How the folders nest comes first, then the folders (#554).
    assert!(matches!(events.borrow()[0], WorkerEvent::Hierarchy(_)));
    assert!(matches!(events.borrow()[1], WorkerEvent::Folders(_)));
    import(&mut w, INBOX, false);
    assert_eq!(
        kinds(&w),
        [
            ("INBOX".to_string(), FolderKind::Inbox, 1),
            ("Trash".to_string(), FolderKind::Trash, 2)
        ]
    );
}

#[test]
fn mail_moved_in_is_stored_listed_and_answered() {
    let dir = Scratch::new();
    let (mut w, events) = worker(&dir);
    import(&mut w, "Archive/2025", true);
    assert!(events.borrow().iter().any(|e| matches!(
        e,
        WorkerEvent::RawImported {
            token: 7,
            result: Ok(())
        }
    )));
    // The folder it asked for was made, and listed.
    let archive = w.folder("Archive/2025").expect("listed").clone();
    run(
        &mut w,
        MailRequest::LoadMessages {
            folder_id: archive.id,
            path: archive.path.clone(),
        },
    );
    let list = last_list(&events, archive.id);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].subject, "Kept");
    assert_eq!(list[0].message_id, "kept-1@example.org");
    assert!(!list[0].unread);
}

#[test]
fn flags_and_tags_reach_the_file_and_the_cache() {
    let dir = Scratch::new();
    let (mut w, _events) = worker(&dir);
    import(&mut w, INBOX, false);
    run(
        &mut w,
        MailRequest::SetSeen {
            path: INBOX.into(),
            uid: 1,
            seen: true,
        },
    );
    run(
        &mut w,
        MailRequest::SetFlagged {
            path: INBOX.into(),
            uid: 1,
            flagged: true,
        },
    );
    run(
        &mut w,
        MailRequest::SetKeyword {
            path: INBOX.into(),
            uid: 1,
            message_id: "kept-1@example.org".into(),
            keyword: "$label1".into(),
            add: true,
        },
    );
    let e = w.store.entry(INBOX, 1).unwrap();
    assert_eq!(
        (e.flags.as_str(), e.keywords.clone()),
        ("FS", vec!["$label1".to_string()])
    );
    let m = w
        .cache
        .as_ref()
        .unwrap()
        .load_messages(1, INBOX, 1)
        .remove(0);
    assert!(!m.unread && m.starred);
    assert_eq!(m.keywords, ["$label1"]);
}

#[test]
fn a_move_takes_the_rendered_body_along_and_undo_brings_it_back() {
    let dir = Scratch::new();
    let (mut w, events) = worker(&dir);
    import(&mut w, INBOX, false);
    run(
        &mut w,
        MailRequest::LoadBody {
            message_id: 1,
            path: INBOX.into(),
            uid: 1,
        },
    );
    assert!(w.cache.as_ref().unwrap().load_body(1, INBOX, 1).is_some());
    run(
        &mut w,
        MailRequest::MoveMessage {
            path: INBOX.into(),
            uid: 1,
            dest: "Trash".into(),
        },
    );
    let c = w.cache.as_ref().unwrap();
    assert!(c.load_body(1, INBOX, 1).is_none());
    assert!(c.load_body(1, "Trash", 1).is_some());
    assert!(w.store.scan(INBOX).unwrap().is_empty());

    let inbox = w.folder_id(INBOX);
    run(
        &mut w,
        MailRequest::UndoMove {
            path: "Trash".into(),
            dest: INBOX.into(),
            dest_folder_id: inbox,
            message_ids: vec!["kept-1@example.org".into()],
        },
    );
    let list = last_list(&events, inbox);
    assert_eq!(list.len(), 1);
    // Numbers are not given again: it comes back as the Inbox's second.
    assert_eq!(list[0].uid, 2);
    assert!(w.cache.as_ref().unwrap().load_body(1, INBOX, 2).is_some());
}

#[test]
fn snoozed_mail_comes_back_unread() {
    let dir = Scratch::new();
    let (mut w, events) = worker(&dir);
    import(&mut w, "Snoozed", true);
    let inbox = w.folder_id(INBOX);
    run(
        &mut w,
        MailRequest::Unsnooze {
            path: "Snoozed".into(),
            dest: INBOX.into(),
            dest_folder_id: inbox,
            message_ids: vec!["kept-1@example.org".into()],
        },
    );
    assert!(events
        .borrow()
        .iter()
        .any(|e| matches!(e, WorkerEvent::Unsnoozed { done: true, .. })));
    assert!(last_list(&events, inbox)[0].unread);
}

#[test]
fn renaming_a_folder_keeps_its_mail_and_its_cache() {
    let dir = Scratch::new();
    let (mut w, _events) = worker(&dir);
    import(&mut w, "Work", true);
    run(
        &mut w,
        MailRequest::LoadBody {
            message_id: 1,
            path: "Work".into(),
            uid: 1,
        },
    );
    run(
        &mut w,
        MailRequest::RenameFolder {
            old_path: "Work".into(),
            new_path: "Jobs".into(),
        },
    );
    assert!(w.folder("Jobs").is_some() && w.folder("Work").is_none());
    assert!(w.cache.as_ref().unwrap().load_body(1, "Jobs", 1).is_some());
    assert_eq!(w.store.read("Jobs", 1).unwrap(), RAW);
}

#[test]
fn deleting_a_folder_puts_its_mail_in_the_trash() {
    let dir = Scratch::new();
    let (mut w, _events) = worker(&dir);
    import(&mut w, "Old", true);
    run(
        &mut w,
        MailRequest::DeleteFolder {
            path: "Old".into(),
            trash: Some("Trash".into()),
        },
    );
    assert!(w.folder("Old").is_none());
    assert_eq!(w.store.scan("Trash").unwrap().len(), 1);
}

#[test]
fn the_background_reads_every_folder_and_renders_its_mail() {
    let dir = Scratch::new();
    {
        let mut store = Maildir::open(dir.path().join("local")).unwrap();
        store.deliver("Archive", RAW, "S", &[]).unwrap();
    }
    let (mut w, _events) = worker(&dir);
    for _ in 0..10 {
        w.index_step();
    }
    let c = w.cache.as_ref().unwrap();
    assert_eq!(c.load_messages(1, "Archive", 0).len(), 1);
    assert!(c.load_body(1, "Archive", 1).is_some());
}

#[test]
fn mail_sent_from_here_goes_back_to_its_composer() {
    let dir = Scratch::new();
    let (mut w, events) = worker(&dir);
    run(
        &mut w,
        MailRequest::Send {
            message: Box::new(crate::worker::test_support::sample_outgoing()),
            sent_path: None,
            ticket: 3,
        },
    );
    assert!(events.borrow().iter().any(|e| matches!(
        e,
        WorkerEvent::SendDone {
            ticket: 3,
            sent: false
        }
    )));
}

/// A copy another account's worker drops into the tree (#552) is listed,
/// and its folder shown, when the local worker looks.
#[test]
fn copies_from_another_account_are_shown() {
    let dir = Scratch::new();
    let (mut w, events) = worker(&dir);
    super::maildir::deliver_into(&dir.path().join("local"), "iCloud/Inbox", RAW, "S").unwrap();
    w.show_copies();
    let folder = w.folder("iCloud/Inbox").expect("listed").clone();
    assert!(w.folder("iCloud").is_some());
    let list = last_list(&events, folder.id);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].subject, "Kept");
    assert!(!list[0].unread);
}

/// What other accounts have left to copy (#552) is reported when it
/// changes, so the spinner turns and the status line says so.
#[test]
fn what_is_left_to_copy_is_reported() {
    let dir = Scratch::new();
    let (mut w, events) = worker(&dir);
    let progress = |events: &Events| {
        events
            .borrow()
            .iter()
            .filter_map(|e| match e {
                WorkerEvent::CopyProgress { left } => Some(*left),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    super::copy::set_pending(9001, 5);
    w.report_copies();
    w.report_copies();
    let said = progress(&events);
    assert_eq!(said.len(), 1, "said once: {said:?}");
    assert!(said[0] >= 5);
    super::copy::set_pending(9001, 0);
    w.report_copies();
    assert_eq!(progress(&events).len(), 2);
}
