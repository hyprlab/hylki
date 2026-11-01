//! Reader-first ordering: the loads the reader waits on may overtake list
//! syncs and refresh ticks, never a mutation, and may interrupt an in-flight
//! network read when nothing but reads is queued.

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

/// Take everything else already queued behind `first` and move the reader's
/// loads ahead of the list syncs they sit behind, keeping every other order.
///
/// The worker serves one request at a time, so a message opened right after
/// a view that loads its folder (a notification click into Inboxes, which
/// asks every account for its inbox, then opens the mail) waited for the
/// whole list fetch — and the IDLE hand-off before it — before its body was
/// even asked for. The body is what the user is looking at; the list can
/// follow. A reader load overtakes lists and deferred refresh ticks, never a
/// move, flag change or reconnect, so what it reads is what the
/// requests before it left.
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
                while at > 0 && can_yield_to_reader(&ordered[at - 1]) {
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

/// Let a reader interrupt network reads, without overtaking queued mutations.
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
                    is_reader_load(r) || can_yield_to_reader(r)
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
        tx.send(MailRequest::SetSeen {
            path: "INBOX".into(),
            uid: 42,
            seen: true,
        })
        .unwrap();
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
            Some(MailRequest::SetSeen { .. })
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
        let mut backlog = std::collections::VecDeque::from([MailRequest::SetSeen {
            path: "INBOX".into(),
            uid: 42,
            seen: true,
        }]);
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
            Some(MailRequest::SetSeen { .. })
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
        tx.send(MailRequest::SetSeen {
            path: "INBOX".into(),
            uid: 42,
            seen: true,
        })
        .unwrap();
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
            Some(MailRequest::SetSeen { .. })
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
