//! Reader-first ordering: the loads the reader waits on may overtake list
//! syncs, refresh ticks and the mutations that leave the mail they read
//! alone, and may interrupt an in-flight network read when nothing they
//! must wait for is queued. A message being sent overtakes and interrupts
//! the same reads, though no mutation. The count the Empty Trash question
//! asks for overtakes queued list syncs and refresh ticks, and interrupts
//! nothing. So does a move, a delete or an erase, with the `Settle` behind
//! it.

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

/// A message sent now. The user has closed its composer and is waiting for
/// it to go: it waited behind two minutes of drafts fetched ahead on a slow
/// server (#678). Send Later only files it in the Outbox, which can wait.
fn is_send_now(req: &MailRequest) -> bool {
    matches!(req, MailRequest::Send { message, .. }
        if message.send_at.is_none_or(|t| t <= crate::datefmt::now()))
}

/// What goes ahead of the work done for the user's sake rather than at
/// their asking: the reader's loads and a message being sent.
fn goes_first(req: &MailRequest) -> bool {
    is_reader_load(req) || is_send_now(req)
}

/// The count the Empty Trash / Empty Junk question waits on (#676): one
/// EXAMINE, which sat behind every folder sync queued on a slow server. It
/// never cuts a read short, which costs a reconnect, since the question
/// shows the cached count meanwhile. Nor does it pass a move, which may be
/// filling the very folder it counts.
fn is_empty_count(req: &MailRequest) -> bool {
    matches!(req, MailRequest::CountFolder { .. })
}

/// Mail the user moved, deleted, marked as spam or erased, and the
/// `Settle` sent right behind it. On a slow server each folder fetch took
/// 20 to 60 seconds, and a delete queued behind a few of them reached Trash
/// minutes later (#676). A folder fetched after the move reads what the move
/// left, which is what the list shows already. Never cuts a read short: on
/// a provider that limits logins, the reconnect could be refused.
fn takes_mail_out(req: &MailRequest) -> bool {
    matches!(
        req,
        MailRequest::MoveMessage { .. }
            | MailRequest::MoveMessages { .. }
            | MailRequest::MarkSpam { .. }
            | MailRequest::MarkHam { .. }
            | MailRequest::MarkHamMany { .. }
            | MailRequest::PurgeMessages { .. }
            | MailRequest::UndoMove { .. }
            | MailRequest::EmptyFolder { .. }
            | MailRequest::Settle { .. }
    )
}

/// What moves up the queue past the list syncs and refresh ticks before it.
fn jumps_queue(req: &MailRequest) -> bool {
    goes_first(req) || is_empty_count(req) || takes_mail_out(req)
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
/// So are the star, read and tag checks of a view: they change flags in the
/// index, never what a body fetch returns, and one queued behind a run of
/// drafts fetched ahead kept a message on its spinner for 100 s (#680).
fn can_yield_to_reader(req: &MailRequest) -> bool {
    is_list_load(req)
        || matches!(
            req,
            MailRequest::RefreshUnread
                | MailRequest::RefreshFlags { .. }
                | MailRequest::RefreshKeywords { .. }
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

/// Whether `req`, which [`jumps_queue`], may go ahead of `ahead`. A move
/// passes only list fetches and refresh ticks: every other request, a
/// draft's bytes fetched for a move to another account among them, keeps
/// its place before it.
fn overtakes(req: &MailRequest, ahead: &MailRequest) -> bool {
    if takes_mail_out(req) {
        is_list_load(ahead)
            || matches!(
                ahead,
                MailRequest::RefreshUnread
                    | MailRequest::RefreshFlags { .. }
                    | MailRequest::RefreshKeywords { .. }
            )
    } else {
        may_overtake(req, ahead)
    }
}

/// Take everything else already queued behind `first` and move the reader's
/// loads, sends, moves and Empty Trash counts ahead of the list syncs they
/// sit behind, keeping every other order.
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
            if jumps_queue(&req) {
                // Slot in ahead of list fetches and deferred refresh ticks.
                let mut at = ordered.len();
                while at > 0 && overtakes(&req, &ordered[at - 1]) {
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
    // Reader loads, sends, moves and counts keep their order among
    // themselves, so only the first can come to the front.
    let Some(at) = backlog.iter().position(jumps_queue) else {
        return Some(req);
    };
    if !backlog
        .iter()
        .take(at)
        .chain(std::iter::once(&req))
        .all(|ahead| overtakes(&backlog[at], ahead))
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
                let interrupt = goes_first(&req) && backlog.iter().all(|r| {
                    goes_first(r) || may_overtake(&req, r)
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
mod move_tests;

#[cfg(test)]
mod reader_tests;
