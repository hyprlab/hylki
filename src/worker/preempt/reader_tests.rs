//! The reader's loads, sends and Empty Trash counts against the queue.

use super::*;

use std::time::Duration;

use crate::worker::imap::backfill::{restore_interrupted_backfill, Backfill};
use crate::worker::preempt::{interrupt_read_for_reader, reorder_reader_loads};

fn body() -> MailRequest {
    MailRequest::LoadBody {
        message_id: 42,
        path: "INBOX".into(),
        uid: 42,
    }
}

fn move_of(uid: u32) -> MailRequest {
    MailRequest::MoveMessage {
        path: "INBOX".into(),
        uid,
        dest: "Archive".into(),
    }
}

fn seen(uid: u32) -> MailRequest {
    MailRequest::SetSeen {
        path: "INBOX".into(),
        uid,
        seen: false,
    }
}

/// #296: a run of read toggles and archives queued on a slow server
/// held the message clicked on behind every one of them. None of them
/// touches it, so it goes first and they keep their order.
#[test]
fn a_body_overtakes_flag_changes_and_moves_of_other_mail() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(seen(42)).unwrap();
    tx.send(move_of(7)).unwrap();
    tx.send(MailRequest::Settle {
        path: "INBOX".into(),
        uids: vec![7],
    })
    .unwrap();
    tx.send(MailRequest::MoveMessages {
        path: "Archive".into(),
        uids: vec![42],
        dest: "Trash".into(),
    })
    .unwrap();
    tx.send(body()).unwrap();
    let mut backlog = std::collections::VecDeque::new();
    let first = reorder_reader_loads(seen(9), &mut rx, &mut backlog);
    assert!(matches!(first, MailRequest::LoadBody { uid: 42, .. }));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::SetSeen { uid: 9, .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::SetSeen { uid: 42, .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::MoveMessage { uid: 7, .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::Settle { .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::MoveMessages { .. })
    ));
}

fn count() -> MailRequest {
    MailRequest::CountFolder {
        path: "Trash".into(),
    }
}

fn sync(path: &str) -> MailRequest {
    MailRequest::SyncFolder {
        folder_id: 1,
        path: path.into(),
    }
}

/// #676: the Empty Trash count sat behind every folder sync queued on a
/// slow server; it goes first, and the syncs keep their order.
#[test]
fn an_empty_count_overtakes_queued_syncs() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(sync("Archive")).unwrap();
    tx.send(MailRequest::RefreshUnread).unwrap();
    tx.send(count()).unwrap();
    let mut backlog = std::collections::VecDeque::new();
    let first = reorder_reader_loads(sync("INBOX"), &mut rx, &mut backlog);
    assert!(matches!(first, MailRequest::CountFolder { .. }));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::SyncFolder { path, .. }) if path == "INBOX"
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::SyncFolder { path, .. }) if path == "Archive"
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::RefreshUnread)
    ));
}

/// A move may be filling the folder counted, so the count waits for it.
#[test]
fn an_empty_count_waits_for_a_move() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(sync("Archive")).unwrap();
    tx.send(count()).unwrap();
    let mut backlog = std::collections::VecDeque::new();
    let first = reorder_reader_loads(move_of(7), &mut rx, &mut backlog);
    assert!(matches!(first, MailRequest::MoveMessage { .. }));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::CountFolder { .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::SyncFolder { .. })
    ));
}

/// It never cuts a read short: that would cost a reconnect.
#[tokio::test]
async fn an_empty_count_does_not_interrupt_a_read() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(count()).unwrap();
    let mut backlog = std::collections::VecDeque::new();
    let done = interrupt_read_for_reader(
        async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            7
        },
        &mut rx,
        &mut backlog,
    )
    .await;
    assert_eq!(done, Some(7));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::CountFolder { .. })
    ));
}

/// A move taking the message out of its folder is what the body has to
/// wait for, and so is anything behind which it would have to pass.
#[test]
fn a_body_waits_for_a_move_of_its_own_message() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(MailRequest::MoveMessages {
        path: "INBOX".into(),
        uids: vec![41, 42],
        dest: "Trash".into(),
    })
    .unwrap();
    tx.send(seen(7)).unwrap();
    tx.send(body()).unwrap();
    let mut backlog = std::collections::VecDeque::new();
    let first = reorder_reader_loads(MailRequest::Reconnect, &mut rx, &mut backlog);
    assert!(matches!(first, MailRequest::Reconnect));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::MoveMessages { .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::LoadBody { .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::SetSeen { uid: 7, .. })
    ));
}

#[test]
fn only_the_last_read_change_of_a_message_is_sent() {
    use crate::worker::preempt::superseded;
    let queued = std::collections::VecDeque::from([seen(9), move_of(7), seen(42)]);
    assert!(superseded(&seen(42), &queued));
    assert!(!superseded(
        &seen(9),
        &std::collections::VecDeque::from([seen(42)])
    ));
    // A move of the message in between: the earlier mark still goes,
    // before the move takes it away.
    let queued = std::collections::VecDeque::from([move_of(42), seen(42)]);
    assert!(!superseded(&seen(42), &queued));
    // A star change does not replace a read change.
    let star = MailRequest::SetFlagged {
        path: "INBOX".into(),
        uid: 42,
        flagged: true,
    };
    assert!(!superseded(
        &seen(42),
        &std::collections::VecDeque::from([star])
    ));
}

/// #296: a click while the list fetch was still connecting.
#[test]
fn a_reader_queued_while_connecting_goes_first_on_the_new_session() {
    use crate::worker::preempt::yield_to_queued_reader;
    let list = || MailRequest::LoadMessages {
        folder_id: 1,
        path: "INBOX".into(),
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(seen(7)).unwrap();
    tx.send(body()).unwrap();
    let mut backlog = std::collections::VecDeque::new();
    assert!(yield_to_queued_reader(list(), &mut rx, &mut backlog).is_none());
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::LoadBody { .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::LoadMessages { .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::SetSeen { .. })
    ));

    // Behind a move of its own message it waits. The move goes ahead
    // of the list (#676), and the reader stays behind the move.
    let mut backlog = std::collections::VecDeque::from([move_of(42), body()]);
    assert!(yield_to_queued_reader(list(), &mut rx, &mut backlog).is_none());
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::MoveMessage { .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::LoadMessages { .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::LoadBody { .. })
    ));

    // A mutation is never put off for a reader.
    let mut backlog = std::collections::VecDeque::from([body()]);
    let kept = yield_to_queued_reader(seen(7), &mut rx, &mut backlog);
    assert!(matches!(kept, Some(MailRequest::SetSeen { .. })));
}

#[tokio::test(start_paused = true)]
async fn a_reader_interrupts_a_read_with_only_other_mail_changing() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut backlog = std::collections::VecDeque::from([seen(42), move_of(7)]);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.send(body()).unwrap();
    });
    let result = interrupt_read_for_reader(
        tokio::time::sleep(Duration::from_secs(10)),
        &mut rx,
        &mut backlog,
    )
    .await;
    assert!(result.is_none());
}

#[test]
fn a_refresh_tick_does_not_leave_a_folder_sync_ahead_of_the_reader() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(MailRequest::RefreshUnread).unwrap();
    tx.send(MailRequest::LoadMessages {
        folder_id: 1,
        path: "INBOX".into(),
    })
    .unwrap();
    tx.send(body()).unwrap();
    let mut backlog = std::collections::VecDeque::new();
    let first = reorder_reader_loads(
        MailRequest::SyncFolder {
            folder_id: 1,
            path: "INBOX".into(),
        },
        &mut rx,
        &mut backlog,
    );
    assert!(matches!(first, MailRequest::LoadBody { .. }));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::SyncFolder { .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::RefreshUnread)
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::LoadMessages { .. })
    ));
}

/// A view's flag and tag checks read flags only: the message clicked
/// goes ahead of them and of the drafts queued before them (#680).
#[test]
fn a_body_overtakes_flag_checks_and_drafts_fetched_ahead() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(MailRequest::RefreshFlags {
        paths: vec!["INBOX".into()],
    })
    .unwrap();
    tx.send(MailRequest::RefreshKeywords {
        keywords: vec!["$label1".into()],
    })
    .unwrap();
    tx.send(body()).unwrap();
    let mut backlog = std::collections::VecDeque::new();
    let draft = MailRequest::ExportRaw {
        token: 1,
        path: "Drafts".into(),
        uid: 1,
        for_reader: false,
    };
    let first = reorder_reader_loads(draft, &mut rx, &mut backlog);
    assert!(matches!(first, MailRequest::LoadBody { .. }));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::ExportRaw { .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::RefreshFlags { .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::RefreshKeywords { .. })
    ));
}

/// A draft clicked goes ahead of the drafts being fetched ahead of
/// time, which keep their order behind it.
#[test]
fn a_draft_opened_overtakes_the_drafts_fetched_ahead() {
    let raw = |token, for_reader| MailRequest::ExportRaw {
        token,
        path: "Drafts".into(),
        uid: token as u32,
        for_reader,
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(raw(2, false)).unwrap();
    tx.send(raw(3, false)).unwrap();
    tx.send(raw(9, true)).unwrap();
    let mut backlog = std::collections::VecDeque::new();
    let first = reorder_reader_loads(raw(1, false), &mut rx, &mut backlog);
    let token = |r: &MailRequest| match r {
        MailRequest::ExportRaw { token, .. } => *token,
        _ => 0,
    };
    assert_eq!(token(&first), 9);
    let rest: Vec<u64> = backlog.iter().map(token).collect();
    assert_eq!(rest, [1, 2, 3]);
}

fn send(ticket: u64, send_at: Option<i64>) -> MailRequest {
    let message = crate::worker::OutgoingMessage {
        send_at,
        ..crate::worker::test_support::sample_outgoing()
    };
    MailRequest::Send {
        message: Box::new(message),
        sent_path: None,
        ticket,
    }
}

fn draft_ahead(uid: u32) -> MailRequest {
    MailRequest::ExportRaw {
        token: uid.into(),
        path: "Drafts".into(),
        uid,
        for_reader: false,
    }
}

/// #678: Send pressed while seven large drafts were being fetched ahead
/// went out two minutes later. It goes ahead of them, and of the list
/// and flag checks, but never ahead of a draft being saved.
#[test]
fn a_send_overtakes_drafts_fetched_ahead_but_not_a_save() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(draft_ahead(2)).unwrap();
    tx.send(MailRequest::RefreshFlags {
        paths: vec!["INBOX".into()],
    })
    .unwrap();
    tx.send(send(7, None)).unwrap();
    let mut backlog = std::collections::VecDeque::new();
    let first = reorder_reader_loads(draft_ahead(1), &mut rx, &mut backlog);
    assert!(matches!(first, MailRequest::Send { ticket: 7, .. }));
    assert_eq!(backlog.len(), 3);

    // A mutation stays a barrier, and what is ahead of it stays there.
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(seen(4)).unwrap();
    tx.send(send(8, None)).unwrap();
    tx.send(draft_ahead(2)).unwrap();
    let mut backlog = std::collections::VecDeque::new();
    let first = reorder_reader_loads(draft_ahead(1), &mut rx, &mut backlog);
    assert!(matches!(first, MailRequest::ExportRaw { uid: 1, .. }));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::SetSeen { .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::Send { ticket: 8, .. })
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(send(9, None)).unwrap();
    let mut backlog = std::collections::VecDeque::new();
    let first = reorder_reader_loads(seen(4), &mut rx, &mut backlog);
    assert!(matches!(first, MailRequest::SetSeen { .. }));
}

/// Send Later only files the message in the Outbox: it keeps its place.
#[test]
fn a_send_later_keeps_its_place() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let later = crate::datefmt::now() + 3600;
    tx.send(send(7, Some(later))).unwrap();
    let mut backlog = std::collections::VecDeque::new();
    let first = reorder_reader_loads(draft_ahead(1), &mut rx, &mut backlog);
    assert!(matches!(first, MailRequest::ExportRaw { .. }));
}

#[tokio::test(start_paused = true)]
async fn a_send_interrupts_a_draft_being_fetched_ahead() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut backlog = std::collections::VecDeque::from([draft_ahead(3)]);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.send(send(7, None)).unwrap();
    });
    let result = interrupt_read_for_reader(
        tokio::time::sleep(Duration::from_secs(10)),
        &mut rx,
        &mut backlog,
    )
    .await;
    assert!(result.is_none());
    assert!(matches!(
        backlog.pop_back(),
        Some(MailRequest::Send { ticket: 7, .. })
    ));
}

#[test]
fn overtaking_refresh_ticks_still_preserves_mutation_order() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(MailRequest::RefreshUnread).unwrap();
    tx.send(move_of(42)).unwrap();
    tx.send(MailRequest::LoadMessages {
        folder_id: 1,
        path: "INBOX".into(),
    })
    .unwrap();
    tx.send(MailRequest::RefreshUnread).unwrap();
    tx.send(body()).unwrap();
    let mut backlog = std::collections::VecDeque::new();
    let first = reorder_reader_loads(
        MailRequest::SyncFolder {
            folder_id: 1,
            path: "INBOX".into(),
        },
        &mut rx,
        &mut backlog,
    );
    // The move goes ahead of the syncs and ticks (#676); the reader of
    // the moved message stays behind it.
    assert!(matches!(first, MailRequest::MoveMessage { .. }));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::LoadBody { .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::SyncFolder { .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::RefreshUnread)
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::LoadMessages { .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::RefreshUnread)
    ));
}

#[tokio::test(start_paused = true)]
async fn a_reader_cancels_an_inflight_folder_read() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut backlog = std::collections::VecDeque::new();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.send(body()).unwrap();
    });
    let start = tokio::time::Instant::now();
    let result = interrupt_read_for_reader(
        tokio::time::sleep(Duration::from_secs(10)),
        &mut rx,
        &mut backlog,
    )
    .await;
    assert!(result.is_none());
    assert_eq!(start.elapsed(), Duration::from_millis(20));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::LoadBody { uid: 42, .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn a_reader_does_not_overtake_a_queued_mutation() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut backlog = std::collections::VecDeque::from([move_of(42)]);
    tx.send(body()).unwrap();
    let result = interrupt_read_for_reader(
        async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            7
        },
        &mut rx,
        &mut backlog,
    )
    .await;
    assert_eq!(result, Some(7));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::MoveMessage { .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::LoadBody { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn an_arriving_mutation_remains_a_barrier_to_a_later_reader() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut backlog = std::collections::VecDeque::new();
    tx.send(move_of(42)).unwrap();
    tx.send(body()).unwrap();
    let result = interrupt_read_for_reader(
        async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            7
        },
        &mut rx,
        &mut backlog,
    )
    .await;
    assert_eq!(result, Some(7));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::MoveMessage { .. })
    ));
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::LoadBody { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn cancelling_an_owned_read_drops_the_connection() {
    use tokio::io::AsyncReadExt;
    let (client, mut server) = tokio::io::duplex(16);
    let mut session = Some(client);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut backlog = std::collections::VecDeque::new();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.send(body()).unwrap();
    });
    let result = interrupt_read_for_reader(
        async {
            let mut owned = session.take().unwrap();
            let result = owned.read(&mut [0]).await;
            session = Some(owned);
            result
        },
        &mut rx,
        &mut backlog,
    )
    .await;
    assert!(result.is_none());
    assert!(session.is_none());
    // EOF proves the reader cannot accidentally reuse an interrupted stream.
    assert_eq!(server.read(&mut [0]).await.unwrap(), 0);
    assert!(matches!(
        backlog.pop_front(),
        Some(MailRequest::LoadBody { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn cancelled_backfill_restores_drained_uids_without_duplicate_jobs() {
    let job = Backfill {
        folder_id: 1,
        path: "INBOX".into(),
        remaining: Some(vec![3, 2, 1]),
        gallery: false,
        failures: 0,
    };
    let mut queue = std::collections::VecDeque::from([job.clone()]);
    // Cancellation before the future's first poll leaves the job queued.
    restore_interrupted_backfill(&mut queue, job.clone());
    assert_eq!(queue.len(), 1);
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.send(body()).unwrap();
    });
    let result = interrupt_read_for_reader(
        async {
            let mut owned = queue.pop_front().unwrap();
            owned.remaining.as_mut().unwrap().drain(..2);
            std::future::pending::<()>().await;
        },
        &mut rx,
        &mut Default::default(),
    )
    .await;
    assert!(result.is_none());
    assert!(queue.is_empty());
    restore_interrupted_backfill(&mut queue, job);
    assert_eq!(queue.len(), 1);
    assert_eq!(
        queue.front().unwrap().remaining.as_deref(),
        Some([3, 2, 1].as_slice())
    );
}

#[tokio::test(start_paused = true)]
async fn a_closed_queue_does_not_spin_or_cancel_the_read() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    drop(tx);
    let result = interrupt_read_for_reader(
        async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            7
        },
        &mut rx,
        &mut Default::default(),
    )
    .await;
    assert_eq!(result, Some(7));
}
