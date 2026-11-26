//! A move, delete or erase goes ahead of the folder fetches queued before it
//! (#676), and keeps its place among everything else.

use std::collections::VecDeque;

use tokio::sync::mpsc;

use super::{reorder_reader_loads, yield_to_queued_reader};
use crate::worker::MailRequest;

fn sync(path: &str) -> MailRequest {
    MailRequest::SyncFolder {
        folder_id: 1,
        path: path.into(),
    }
}

fn load(path: &str) -> MailRequest {
    MailRequest::LoadMessages {
        folder_id: 1,
        path: path.into(),
    }
}

fn delete(uids: &[u32]) -> MailRequest {
    MailRequest::MoveMessages {
        path: "INBOX".into(),
        uids: uids.to_vec(),
        dest: "Trash".into(),
    }
}

fn settle(uids: &[u32]) -> MailRequest {
    MailRequest::Settle {
        path: "INBOX".into(),
        uids: uids.to_vec(),
    }
}

fn body(uid: u32) -> MailRequest {
    MailRequest::LoadBody {
        message_id: 1,
        path: "INBOX".into(),
        uid,
    }
}

fn seen(uid: u32) -> MailRequest {
    MailRequest::SetSeen {
        path: "INBOX".into(),
        uid,
        seen: true,
    }
}

fn names(first: &MailRequest, backlog: &VecDeque<MailRequest>) -> Vec<String> {
    std::iter::once(first)
        .chain(backlog.iter())
        .map(|r| match r {
            MailRequest::SyncFolder { path, .. } => format!("sync {path}"),
            MailRequest::LoadMessages { path, .. } => format!("load {path}"),
            MailRequest::MoveMessages { uids, .. } => format!("move {uids:?}"),
            MailRequest::Settle { uids, .. } => format!("settle {uids:?}"),
            MailRequest::LoadBody { uid, .. } => format!("body {uid}"),
            MailRequest::SetSeen { uid, .. } => format!("seen {uid}"),
            MailRequest::ExportRaw { uid, .. } => format!("export {uid}"),
            MailRequest::RefreshUnread => "refresh".into(),
            MailRequest::CountFolder { .. } => "count".into(),
            other => format!("{other:?}"),
        })
        .collect()
}

/// Yahoo took 20 to 60 seconds per folder fetch, and a delete queued behind
/// a few of them reached Trash minutes later.
#[test]
fn a_delete_overtakes_queued_folder_fetches() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(load("*LinkedIn")).unwrap();
    tx.send(MailRequest::RefreshUnread).unwrap();
    tx.send(delete(&[5])).unwrap();
    tx.send(settle(&[5])).unwrap();
    let mut backlog = VecDeque::new();
    let first = reorder_reader_loads(sync("INBOX"), &mut rx, &mut backlog);
    assert_eq!(
        names(&first, &backlog),
        [
            "move [5]",
            "settle [5]",
            "sync INBOX",
            "load *LinkedIn",
            "refresh"
        ]
    );
}

/// Mutations keep their order: a read change made before the delete still
/// reaches the server first.
#[test]
fn a_delete_never_passes_another_change() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(sync("Archive")).unwrap();
    tx.send(delete(&[5])).unwrap();
    tx.send(settle(&[5])).unwrap();
    let mut backlog = VecDeque::new();
    let first = reorder_reader_loads(seen(9), &mut rx, &mut backlog);
    assert_eq!(
        names(&first, &backlog),
        ["seen 9", "move [5]", "settle [5]", "sync Archive"]
    );
}

/// Two deletes keep their order, each followed by its own settle.
#[test]
fn deletes_keep_their_order_among_themselves() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(delete(&[1])).unwrap();
    tx.send(settle(&[1])).unwrap();
    tx.send(sync("Archive")).unwrap();
    tx.send(delete(&[2])).unwrap();
    tx.send(settle(&[2])).unwrap();
    let mut backlog = VecDeque::new();
    let first = reorder_reader_loads(sync("INBOX"), &mut rx, &mut backlog);
    assert_eq!(
        names(&first, &backlog),
        [
            "move [1]",
            "settle [1]",
            "move [2]",
            "settle [2]",
            "sync INBOX",
            "sync Archive"
        ]
    );
}

/// The message being read still comes before a delete queued after it.
#[test]
fn a_delete_waits_behind_the_reader() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(body(3)).unwrap();
    tx.send(delete(&[5])).unwrap();
    tx.send(settle(&[5])).unwrap();
    let mut backlog = VecDeque::new();
    let first = reorder_reader_loads(sync("INBOX"), &mut rx, &mut backlog);
    assert_eq!(
        names(&first, &backlog),
        ["body 3", "move [5]", "settle [5]", "sync INBOX"]
    );
}

/// A message's bytes fetched for a move to another account keep their place.
#[test]
fn a_delete_does_not_pass_bytes_fetched_for_another_move() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(delete(&[5])).unwrap();
    let mut backlog = VecDeque::new();
    let export = MailRequest::ExportRaw {
        token: 1,
        path: "INBOX".into(),
        uid: 5,
        for_reader: false,
    };
    let first = reorder_reader_loads(export, &mut rx, &mut backlog);
    assert_eq!(names(&first, &backlog), ["export 5", "move [5]"]);
}

/// The Empty Trash count still waits for a delete that fills Trash.
#[test]
fn the_empty_count_waits_for_a_delete_that_overtook_a_sync() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(delete(&[5])).unwrap();
    tx.send(settle(&[5])).unwrap();
    tx.send(MailRequest::CountFolder {
        path: "Trash".into(),
    })
    .unwrap();
    let mut backlog = VecDeque::new();
    let first = reorder_reader_loads(sync("INBOX"), &mut rx, &mut backlog);
    assert_eq!(
        names(&first, &backlog),
        ["move [5]", "settle [5]", "count", "sync INBOX"]
    );
}

/// After a reconnect, a folder fetch about to start lets a delete queued
/// meanwhile go first.
#[test]
fn a_folder_fetch_yields_to_a_delete_after_connecting() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(delete(&[5])).unwrap();
    tx.send(settle(&[5])).unwrap();
    let mut backlog = VecDeque::new();
    assert!(yield_to_queued_reader(sync("INBOX"), &mut rx, &mut backlog).is_none());
    let first = backlog.pop_front().unwrap();
    assert_eq!(
        names(&first, &backlog),
        ["move [5]", "sync INBOX", "settle [5]"]
    );
}
