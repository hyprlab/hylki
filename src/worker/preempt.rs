//! Reader-first ordering: the loads the reader waits on may overtake list
//! syncs, refresh ticks and the mutations that leave the mail they read
//! alone, and may interrupt an in-flight network read when nothing they
//! must wait for is queued.

use tokio::sync::mpsc;

use crate::worker::MailRequest;

/// A request that puts mail in front of the reader: what the user is
/// waiting on right now.
fn is_reader_load(req: &MailRequest) -> bool {
    matches!(
        req,
        MailRequest::LoadBody { .. }
            | MailRequest::LoadBodies { .. }
            | MailRequest::LoadSource { .. }
            | MailRequest::LoadAttachments { download: true, .. }
            | MailRequest::ExportRaw {
                for_reader: true,
                ..
            }
    )
}

/// A folder list fetch: work the reader's loads may go ahead of.
fn is_list_load(req: &MailRequest) -> bool {
    matches!(
        req,
        MailRequest::LoadMessages { .. } | MailRequest::SyncFolder { .. }
    )
}

/// Refresh ticks now only schedule idle work, so they are no longer a
/// barrier between a reader and a folder sync already in the backlog.
/// Drafts fetched ahead of time (and a message's bytes for a move to
/// another account) are reads too, which the draft clicked on overtakes.
fn can_yield_to_reader(req: &MailRequest) -> bool {
    is_list_load(req)
        || matches!(
            req,
            MailRequest::RefreshUnread
                | MailRequest::ExportRaw {
                    for_reader: false,
                    ..
                }
        )
}

/// The folder and UIDs a reader load reads, `None` for any other request.
fn reads(req: &MailRequest) -> Option<(&str, Vec<u32>)> {
    match req {
        MailRequest::LoadBody { path, uid, .. }
        | MailRequest::LoadSource { path, uid, .. }
        | MailRequest::LoadAttachments { path, uid, .. }
        | MailRequest::ExportRaw { path, uid, .. } => Some((path, vec![*uid])),
        MailRequest::LoadBodies { items, path } => {
            Some((path, items.iter().map(|(_, uid)| *uid).collect()))
        }
        _ => None,
    }
}

/// Whether `ahead`, a mutation queued before a reader load of `uids` in
/// `path`, leaves what that load reads as it was.
///
/// A message's content never changes under its UID (RFC 3501 §2.3.1.1), so
/// a flag or keyword change cannot alter what a body fetch returns, and a
/// move or erase alters it only by taking that very message out of `path`.
/// Anything else (a folder renamed or deleted, a draft saved, a reconnect)
/// stays a barrier.
fn leaves_alone(ahead: &MailRequest, path: &str, uids: &[u32]) -> bool {
    let untouched = |p: &str, moved: &[u32]| p != path || !moved.iter().any(|u| uids.contains(u));
    match ahead {
        MailRequest::SetSeen { .. }
        | MailRequest::SetFlagged { .. }
        | MailRequest::SetKeyword { .. }
        | MailRequest::MarkAllRead { .. }
        | MailRequest::Settle { .. } => true,
        MailRequest::MoveMessage { path: p, uid, .. }
        | MailRequest::MarkSpam { path: p, uid, .. }
        | MailRequest::MarkHam { path: p, uid, .. } => untouched(p, std::slice::from_ref(uid)),
        MailRequest::MoveMessages {
            path: p,
            uids: moved,
            ..
        }
        | MailRequest::MarkHamMany {
            path: p,
            uids: moved,
            ..
        }
        | MailRequest::PurgeMessages {
            path: p,
            uids: moved,
        } => untouched(p, moved),
        _ => false,
    }
}

/// Whether the reader load `reader` may go ahead of `ahead`.
fn may_overtake(reader: &MailRequest, ahead: &MailRequest) -> bool {
    can_yield_to_reader(ahead)
        || reads(reader).is_some_and(|(path, uids)| leaves_alone(ahead, path, &uids))
}

/// Take everything else already queued behind `first` and move the reader's
/// loads ahead of the list syncs they sit behind, keeping every other order.
///
/// The worker serves one request at a time, so a message opened right after
/// a view that loads its folder (a notification click into Inboxes, which
/// asks every account for its inbox, then opens the mail) waited for the
/// whole list fetch — and the IDLE hand-off before it — before its body was
/// even asked for. The body is what the user is looking at; the list can
/// follow. A reader load overtakes lists and deferred refresh ticks, and
/// flag changes and moves of other mail (#296: a run of archives or read
/// toggles on a slow server kept the message clicked on its spinner for a
/// minute). It never overtakes a move of the mail it reads, nor a folder
/// change or reconnect, so what it reads is what the requests before it
/// left.
pub(super) fn reorder_reader_loads(
    first: MailRequest,
    rx: &mut mpsc::UnboundedReceiver<MailRequest>,
    backlog: &mut std::collections::VecDeque<MailRequest>,
) -> MailRequest {
    let mut queue: Vec<MailRequest> = Vec::with_capacity(backlog.len() + 2);
    queue.push(first);
    queue.extend(backlog.drain(..));
    while let Ok(req) = rx.try_recv() {
        queue.push(req);
    }
    if queue.len() > 1 {
        let mut ordered: Vec<MailRequest> = Vec::with_capacity(queue.len());
        for req in queue {
            if is_reader_load(&req) {
                // Slot in ahead of list fetches and deferred refresh ticks.
                let mut at = ordered.len();
                while at > 0 && may_overtake(&req, &ordered[at - 1]) {
                    at -= 1;
                }
                ordered.insert(at, req);
            } else {
                ordered.push(req);
            }
        }
        queue = ordered;
    }
    let mut it = queue.into_iter();
    let first = it.next().expect("queue holds at least the request taken");
    backlog.extend(it);
    first
}

/// After a wait before `req` could start (connecting, which took 8 seconds
/// on a slow Gmail account), whether a reader load that arrived meanwhile
/// goes first. If so, both are put back at the front of `backlog`, the
/// reader's load ahead, and `None` comes back; else `req` does.
///
/// Started anyway, the list fetch was interrupted by the queued load at
/// once, and an interrupted read drops its connection, so the reader paid
/// for a second connect before its message came (#296).
pub(super) fn yield_to_queued_reader(
    req: MailRequest,
    rx: &mut mpsc::UnboundedReceiver<MailRequest>,
    backlog: &mut std::collections::VecDeque<MailRequest>,
) -> Option<MailRequest> {
    if !can_yield_to_reader(&req) {
        return Some(req);
    }
    while let Ok(r) = rx.try_recv() {
        backlog.push_back(r);
    }
    // Reader loads keep their order among themselves, so only the first
    // can come to the front.
    let Some(at) = backlog.iter().position(is_reader_load) else {
        return Some(req);
    };
    if !backlog
        .iter()
        .take(at)
        .all(|ahead| may_overtake(&backlog[at], ahead))
    {
        return Some(req);
    }
    let reader = backlog.remove(at).expect("found above");
    backlog.push_front(req);
    backlog.push_front(reader);
    None
}

/// Whether `req`, a read or star change, is replaced by a later one for the
/// same message already in `backlog`, with nothing between that moves or
/// erases the message. Only the last of them decides what the server ends
/// up with: toggled ten times in a minute on a slow server, each one had
/// cost a round trip with the reader waiting behind them (#296).
pub(super) fn superseded(
    req: &MailRequest,
    backlog: &std::collections::VecDeque<MailRequest>,
) -> bool {
    let same = |a: &MailRequest, b: &MailRequest| match (a, b) {
        (
            MailRequest::SetSeen { path, uid, .. },
            MailRequest::SetSeen {
                path: p, uid: u, ..
            },
        )
        | (
            MailRequest::SetFlagged { path, uid, .. },
            MailRequest::SetFlagged {
                path: p, uid: u, ..
            },
        ) => path == p && uid == u,
        _ => false,
    };
    let (MailRequest::SetSeen { path, uid, .. } | MailRequest::SetFlagged { path, uid, .. }) = req
    else {
        return false;
    };
    for later in backlog {
        if same(req, later) {
            return true;
        }
        if !leaves_alone(later, path, std::slice::from_ref(uid)) && !is_reader_load(later) {
            return false;
        }
    }
    false
}

/// Let a reader interrupt network reads, without overtaking a queued
/// mutation it has to wait for.
/// Callers must own the session inside `work`, so cancellation drops a partially
/// consumed IMAP response, and restore the interrupted job for a later retry.
/// The reader then reconnects rather than reusing that response stream.
pub(super) async fn interrupt_read_for_reader<F: std::future::Future>(
    work: F,
    rx: &mut mpsc::UnboundedReceiver<MailRequest>,
    backlog: &mut std::collections::VecDeque<MailRequest>,
) -> Option<F::Output> {
    tokio::pin!(work);
    let mut closed = false;
    loop {
        tokio::select! {
            result = &mut work => return Some(result),
            req = rx.recv(), if !closed => {
                let Some(req) = req else { closed = true; continue; };
                let interrupt = is_reader_load(&req) && backlog.iter().all(|r| {
                    is_reader_load(r) || may_overtake(&req, r)
                });
                backlog.push_back(req);
                if interrupt {
                    return None;
                }
            }
        }
    }
}

#[cfg(test)]
mod reader_preemption_tests {
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

        // Behind a move of its own message it waits, and so does the list.
        let mut backlog = std::collections::VecDeque::from([move_of(42), body()]);
        let kept = yield_to_queued_reader(list(), &mut rx, &mut backlog);
        assert!(matches!(kept, Some(MailRequest::LoadMessages { .. })));
        assert_eq!(backlog.len(), 2);

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
        assert!(matches!(first, MailRequest::SyncFolder { .. }));
        assert!(matches!(
            backlog.pop_front(),
            Some(MailRequest::RefreshUnread)
        ));
        assert!(matches!(
            backlog.pop_front(),
            Some(MailRequest::MoveMessage { .. })
        ));
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
}
