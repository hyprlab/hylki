//! Changes made while the server could not be reached (#442).
//!
//! Marking mail read, starring it, tagging, moving and deleting it all show at
//! once in the list, and used to fail a moment later with "Could not move
//! message: …" when there was no connection, leaving the list and the server
//! disagreeing until the next sync put the message back. Such a change is now
//! kept instead: applied to the cache straight away, so the lists show it and
//! go on showing it across a restart, and written to the cache's
//! `pending_actions` table, from which the worker sends it once a connection
//! is back. The Outbox does the same for sending.
//!
//! Only a change that could not reach the server waits here. A server that
//! was reached and said no still reports its error, as before; so does one
//! that refuses a queued change when it is finally sent.
//!
//! The queue is folded as it grows ([`collapse`]): marking a message read and
//! then unread again leaves nothing to send, two moves of the same message
//! are one, and nothing is sent about a message a queued deletion erases.
//! What is left goes out in the order it was done, consecutive moves to the
//! same folder in one command ([`batches`]).

use crate::cache::Cache;
use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::models::{PendingAction, PendingChange};

use super::{MailRequest, WorkerEvent};

/// The changes `req` makes, one per message, as they would be queued; empty
/// for a request that is no such change. Nothing is looked up yet: `was`
/// and the Message-ID are filled in by [`park`].
pub(super) fn changes_of(req: &MailRequest) -> Vec<PendingAction> {
    let one = |path: &str, uid: u32, change: PendingChange, message_id: &str| PendingAction {
        id: 0,
        path: path.to_string(),
        uid,
        change,
        was: None,
        message_id: message_id.to_string(),
        attempts: 0,
    };
    match req {
        MailRequest::SetSeen { path, uid, seen } => vec![one(path, *uid, PendingChange::Seen(*seen), "")],
        MailRequest::SetFlagged { path, uid, flagged } => {
            vec![one(path, *uid, PendingChange::Flagged(*flagged), "")]
        }
        MailRequest::SetKeyword { path, uid, message_id, keyword, add } => vec![one(
            path,
            *uid,
            PendingChange::Keyword { keyword: keyword.clone(), add: *add },
            message_id,
        )],
        MailRequest::MoveMessage { path, uid, dest } => {
            vec![one(path, *uid, PendingChange::Move { dest: dest.clone() }, "")]
        }
        MailRequest::MoveMessages { path, uids, dest } => uids
            .iter()
            .map(|uid| one(path, *uid, PendingChange::Move { dest: dest.clone() }, ""))
            .collect(),
        MailRequest::PurgeMessages { path, uids } => {
            uids.iter().map(|uid| one(path, *uid, PendingChange::Purge, "")).collect()
        }
        _ => Vec::new(),
    }
}

/// Keep `changes` for later: apply them to the cache, so every list shows
/// them now and after a restart, and queue them, folded into what was
/// already waiting. Returns false when there was nothing to keep or no cache
/// to keep it in; the caller then reports the failure as before.
pub(super) fn park(cache: Option<&Cache>, account_id: u32, changes: Vec<PendingAction>) -> bool {
    let Some(c) = cache else { return false };
    if changes.is_empty() {
        return false;
    }
    for mut action in changes {
        let known = c.marks_of(account_id, &action.path, action.uid);
        if let Some((unread, starred, keywords, message_id)) = &known {
            action.was = match &action.change {
                PendingChange::Seen(_) => Some(!unread),
                PendingChange::Flagged(_) => Some(*starred),
                PendingChange::Keyword { keyword, .. } => {
                    Some(keywords.iter().any(|k| k.eq_ignore_ascii_case(keyword)))
                }
                PendingChange::Move { .. } | PendingChange::Purge => None,
            };
            if action.message_id.is_empty() {
                action.message_id = message_id.clone();
            }
        }
        // A move or deletion needs nothing here: the folder's list leaves
        // out a message with one waiting (see `Cache::load_messages`).
        match &action.change {
            PendingChange::Seen(seen) => c.set_unread(account_id, &action.path, action.uid, !seen),
            PendingChange::Flagged(on) => c.set_starred(account_id, &action.path, action.uid, *on),
            PendingChange::Keyword { keyword, add } => {
                c.set_keyword(account_id, &action.path, action.uid, keyword, *add);
            }
            PendingChange::Move { .. } | PendingChange::Purge => {}
        }
        if c.queue_pending(account_id, &action).is_none() {
            return false;
        }
    }
    let queued = c.pending_actions(account_id);
    let folded = collapse(queued.clone());
    if folded != queued {
        c.replace_pending(account_id, &folded);
    }
    tracing::debug!(account_id, waiting = folded.len(), "change kept until the server can be reached");
    true
}

/// What a request answers with once it is done, said as soon as it is
/// queued instead: to the app a queued change is a change made.
pub(super) fn settle_queued(req: &MailRequest, emit: &impl Fn(WorkerEvent)) {
    match req {
        MailRequest::SetSeen { path, uid, .. } => {
            emit(WorkerEvent::SeenSettled { path: path.clone(), uid: *uid });
        }
        MailRequest::SetKeyword { path, uid, keyword, .. } => emit(WorkerEvent::TagSettled {
            path: path.clone(),
            uid: *uid,
            keyword: keyword.clone(),
        }),
        MailRequest::MoveMessages { .. } | MailRequest::PurgeMessages { .. } => emit(WorkerEvent::BulkComplete),
        _ => {}
    }
}

/// Drop the account's queue if it was made for another server: the
/// account's protocol, host, port or user changed since (see
/// [`Cache::claim_pending`]). Run as a worker starts, before anything is
/// sent.
///
/// Only the protocol and the host count. A port or user name changed while
/// getting an account to connect again leaves the queue alone: the
/// Message-ID check before every move or deletion
/// ([`same_message`]) already keeps a change off a message it was not made
/// to. When a queue is dropped the user is told, once.
pub(super) fn claim(
    cache: Option<&Cache>,
    account_id: u32,
    account: &crate::config::AccountConfig,
    emit: &impl Fn(WorkerEvent),
) {
    let Some(c) = cache else { return };
    let server = format!("{:?}|{}", account.protocol, account.imap_host.trim().to_lowercase());
    let dropped = c.claim_pending(account_id, &server);
    if dropped > 0 {
        tracing::info!(account_id, dropped, "queued changes dropped: the account now points at another server");
        let n = dropped.min(u32::MAX as usize) as u32;
        emit(WorkerEvent::Notice(ni18n_f(
            "{n} change waiting for {account} was discarded because the server changed",
            "{n} changes waiting for {account} were discarded because the server changed",
            n,
            &[("n", &n.to_string()), ("account", &account.display_label())],
        )));
    }
}

/// When the queue is next sent ([`replay_pending`](super::replay_pending)
/// and its Graph and JMAP sides). Not on every pass of the worker's loop: a
/// server that answers "not now" (a folder in use, throttling) would have
/// every change spend its [`MAX_ATTEMPTS`] in seconds. A run is due with a
/// new connection and when the network or the connection comes back; one
/// that left changes waiting is tried again [`RETRY_AFTER`] later.
#[derive(Debug, Clone, Default)]
pub(super) struct ReplayClock {
    due: bool,
    after: Option<std::time::Instant>,
}

/// How long a run that left changes waiting holds off the next one.
pub(super) const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(60);

impl ReplayClock {
    /// A clock for a worker that starts with changes waiting (or not).
    pub(super) fn new(due: bool) -> Self {
        ReplayClock { due, after: None }
    }

    /// A new connection, a reconnect, or the network back: run at once.
    pub(super) fn wake(&mut self) {
        self.due = true;
        self.after = None;
    }

    /// A change joined the queue: run soon, unless a run that failed is
    /// being waited out.
    pub(super) fn changed(&mut self) {
        if self.after.is_none() {
            self.due = true;
        }
    }

    /// Whether to send the queue now.
    pub(super) fn should_run(&self, now: std::time::Instant) -> bool {
        self.due || self.after.is_some_and(|t| now >= t)
    }

    /// A run ended at `now`, with `waiting` changes left.
    pub(super) fn ran(&mut self, now: std::time::Instant, waiting: usize) {
        self.due = false;
        self.after = (waiting > 0).then(|| now + RETRY_AFTER);
    }
}

/// The folder list a Graph or JMAP replay goes by could not be had (the
/// server answered nothing, or not yet): without it no folder can be told
/// gone from not known yet, so nothing is sent or dropped. Each change
/// counts one attempt, and the run stops.
pub(super) fn folders_unknown(cache: &Cache, queue: &[PendingAction], emit: &impl Fn(WorkerEvent)) {
    tracing::info!(waiting = queue.len(), "queued changes wait: the folder list could not be loaded");
    for step in batches(queue) {
        attempt_failed(cache, &step.change, &step.ids, &i18n("Could not reach the server"), emit);
    }
}

/// Tell the app how many changes are waiting, for the status bar, and
/// answer the same count.
pub(super) fn report(cache: Option<&Cache>, account_id: u32, emit: &impl Fn(WorkerEvent)) -> usize {
    let count = cache.map_or(0, |c| c.pending_count(account_id));
    emit(WorkerEvent::PendingChanges { count });
    count
}

/// Queue `req` if it is a change that can wait ([`park`]) and answer it as
/// done ([`settle_queued`]). Answers how many changes now wait, or `None`
/// when `req` was not queued.
pub(super) fn queue_request(
    cache: Option<&Cache>,
    account_id: u32,
    req: &MailRequest,
    emit: &impl Fn(WorkerEvent),
) -> Option<usize> {
    if !park(cache, account_id, changes_of(req)) {
        return None;
    }
    settle_queued(req, emit);
    Some(report(cache, account_id, emit))
}

/// An undo of a move that never reached the server (#200) is forgetting it:
/// the messages are still where they were, the cache's rows with them. When
/// `req` is such an undo, shows them there again and answers how many
/// changes still wait; `None` when it cancelled nothing. The caller still
/// runs the server's side of the undo, which finds nothing to move back
/// unless the move went out just before it was cancelled.
pub(super) fn undo_queued_move(
    cache: Option<&Cache>,
    account_id: u32,
    req: &MailRequest,
    emit: &impl Fn(WorkerEvent),
) -> Option<usize> {
    let MailRequest::UndoMove { path, dest, dest_folder_id, message_ids } = req else { return None };
    if cancel_moves(cache, account_id, dest, path, message_ids) == 0 {
        return None;
    }
    if let Some(c) = cache {
        let messages = c.load_messages(account_id, dest, *dest_folder_id);
        emit(WorkerEvent::Messages { folder_id: *dest_folder_id, messages });
    }
    emit(WorkerEvent::Restored { folder_id: *dest_folder_id, message_ids: message_ids.clone() });
    Some(report(cache, account_id, emit))
}

/// How many times a change may fail without an answer from the server
/// (the connection dropping under it, a server error, throttling) before
/// it is reported and given up on, so one change cannot hold the queue
/// for good.
pub(super) const MAX_ATTEMPTS: u32 = 5;

/// Count a failed attempt at `ids`; the ones that have failed
/// [`MAX_ATTEMPTS`] times are reported with `e` and dropped. Only called
/// from a run that reached the server (a session, or the network monitor
/// reporting a network), and runs are spaced by [`ReplayClock`], so five
/// attempts are five tries minutes apart, not five in a second.
pub(super) fn attempt_failed(
    cache: &Cache,
    change: &PendingChange,
    ids: &[i64],
    e: &str,
    emit: &impl Fn(WorkerEvent),
) {
    let spent = cache.fail_pending(ids, MAX_ATTEMPTS);
    if !spent.is_empty() {
        tracing::warn!(?spent, "queued change given up after {MAX_ATTEMPTS} attempts: {e}");
        emit(WorkerEvent::error(refused_text(change, spent.len(), e)));
        cache.drop_pending(&spent);
    }
}

/// How a failed request to an HTTP mail server (Microsoft Graph, JMAP) is
/// taken by the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Failure {
    /// No answer, or one that says "not now": the change waits.
    Keep,
    /// The server read the change and said no: it is reported and dropped.
    Refused,
}

/// Read a Graph or JMAP error, as their helpers word it, into a [`Failure`].
/// A status (`… returned 404: …`) decides: throttling (429), a timeout
/// (408), a sign-in that needs renewing (401) and server errors (5xx) keep
/// the change, any other 4xx refuses it. A JMAP method error (`Email/set:
/// serverFail …`) is read by its type ([`jmap_error_keeps`]); a folder the
/// server does not know is a refusal; anything else, a resolver or
/// connection failure, keeps it.
pub(super) fn classify_http(e: &str) -> Failure {
    let status = e.split("returned ").nth(1).and_then(|rest| rest.get(..3)).and_then(|s| s.parse::<u16>().ok());
    let status = status.or_else(|| e.contains("(401)").then_some(401));
    match status {
        Some(401 | 408 | 429) => Failure::Keep,
        Some(code) if code >= 500 => Failure::Keep,
        Some(_) => Failure::Refused,
        None if e.starts_with("Email/") || e.starts_with("Mailbox/") => {
            let kind = e.split_once(": ").map_or("", |(_, rest)| rest.split([' ', '(']).next().unwrap_or(""));
            if jmap_error_keeps(kind) { Failure::Keep } else { Failure::Refused }
        }
        None if e.starts_with("unknown folder") => Failure::Refused,
        None => Failure::Keep,
    }
}

/// Whether a JMAP error type (a method error, or the type of a message an
/// `Email/set` did not update) means "not now" rather than "no" (RFC 8620
/// §3.6.2, §5.3): the server failing or busy, or the request rate limited.
pub(super) fn jmap_error_keeps(kind: &str) -> bool {
    matches!(kind, "serverUnavailable" | "serverFail" | "serverPartialFail" | "rateLimit" | "tooManyRequests")
}

/// Fold a queue (oldest first) into the shortest one that leaves every
/// message the same:
///
/// - A mark set again on the same message is one change, to where it ended
///   up; if that is where it started (read, then unread again), none.
/// - A mark made after a move of the same message goes before the move: a
///   message keeps its flags when it is moved, so starring an archived
///   message offline stars it where it lands.
/// - A message moved again is moved once, to the last folder; moved to the
///   folder it is in, it is not moved at all.
/// - A deletion makes whatever came before it about that message moot, and
///   nothing comes after one.
///
/// The folded queue takes the ids of the original one in order, so sorting
/// by id gives the order to send it in.
pub(super) fn collapse(queue: Vec<PendingAction>) -> Vec<PendingAction> {
    let mut ids: Vec<i64> = queue.iter().map(|a| a.id).collect();
    ids.sort_unstable();
    let mut out: Vec<PendingAction> = Vec::with_capacity(queue.len());
    for action in queue {
        let same = |o: &PendingAction| o.path == action.path && o.uid == action.uid;
        let away = out.iter().position(|o| same(o) && o.change.takes_away());
        match (away, &action.change) {
            (Some(i), PendingChange::Move { dest }) => {
                if matches!(out[i].change, PendingChange::Move { .. }) {
                    out.remove(i);
                    if *dest != action.path {
                        out.push(action);
                    }
                }
                // Erased already: there is nothing left to move.
            }
            (Some(i), PendingChange::Purge) => {
                if matches!(out[i].change, PendingChange::Move { .. }) {
                    out.retain(|o| !same(o));
                    out.push(action);
                }
            }
            (Some(i), change) => {
                // A mark on a message a queued deletion erases is moot; on
                // one a queued move takes away, it goes in ahead of the move.
                if matches!(out[i].change, PendingChange::Move { .. }) {
                    let mut at = i;
                    let mut merged = action.clone();
                    if let Some(j) = out.iter().position(|o| same(o) && o.change.same_mark(change)) {
                        let first = out.remove(j);
                        if j < at {
                            at -= 1;
                        }
                        merged.was = first.was;
                    }
                    if merged.was != merged.change.mark() {
                        out.insert(at, merged);
                    }
                }
            }
            (None, PendingChange::Move { dest }) if *dest == action.path => {}
            (None, PendingChange::Move { .. }) => out.push(action),
            (None, PendingChange::Purge) => {
                out.retain(|o| !same(o));
                out.push(action);
            }
            (None, change) => match out.iter().position(|o| same(o) && o.change.same_mark(change)) {
                Some(j) => {
                    let first = out.remove(j);
                    let merged = PendingAction { was: first.was, ..action };
                    if merged.was != merged.change.mark() {
                        out.push(merged);
                    }
                }
                None => out.push(action),
            },
        }
    }
    for (a, id) in out.iter_mut().zip(ids) {
        a.id = id;
    }
    out
}

/// Whether the message the server now holds under a queued change's folder
/// and UID is the one the change was made to: `server` is its Message-ID
/// there (`None`: no message under that UID any more). A UID is only a
/// number within one folder, and a server that renumbered the folder, or an
/// account pointed at another server, could hand the change to a different
/// message, so the Message-ID has to agree. One the cache never knew can't
/// be checked: a mark or a move goes ahead on the UID, as it would have when
/// it was made, but a deletion does not, being the one change that cannot
/// be taken back.
pub(super) fn same_message(queued: &str, server: Option<&str>, change: &PendingChange) -> bool {
    let Some(server) = server else { return false };
    if queued.is_empty() {
        return !matches!(change, PendingChange::Purge);
    }
    server.eq_ignore_ascii_case(queued)
}

/// One step of sending the queue: a change to one message, or a move or
/// deletion of several messages in one folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Batch {
    /// The queue entries it carries out, index-aligned with `uids`.
    pub ids: Vec<i64>,
    pub path: String,
    pub uids: Vec<u32>,
    pub change: PendingChange,
    /// Index-aligned with `uids`; empty strings where unknown.
    pub message_ids: Vec<String>,
}

/// Group a folded queue into the steps that send it, in order: runs of moves
/// from one folder to another, and of deletions in one folder, are one step
/// each (one UID MOVE, one EXPUNGE); a mark is a step of its own.
pub(super) fn batches(queue: &[PendingAction]) -> Vec<Batch> {
    let mut out: Vec<Batch> = Vec::new();
    for a in queue {
        if let Some(last) = out.last_mut() {
            if a.change.takes_away() && last.change == a.change && last.path == a.path {
                last.ids.push(a.id);
                last.uids.push(a.uid);
                last.message_ids.push(a.message_id.clone());
                continue;
            }
        }
        out.push(Batch {
            ids: vec![a.id],
            path: a.path.clone(),
            uids: vec![a.uid],
            change: a.change.clone(),
            message_ids: vec![a.message_id.clone()],
        });
    }
    out
}

/// The error a queued change the server refused is reported with: the one it
/// would have had when it was made. `count` is how many messages it was for.
pub(super) fn refused_text(change: &PendingChange, count: usize, e: &str) -> String {
    match change {
        PendingChange::Seen(_) => i18n_f("Could not update message: {e}", &[("e", e)]),
        PendingChange::Flagged(_) => i18n_f("Could not change the star: {e}", &[("e", e)]),
        PendingChange::Keyword { .. } => i18n_f("Could not tag message: {e}", &[("e", e)]),
        PendingChange::Move { .. } if count == 1 => i18n_f("Could not move message: {e}", &[("e", e)]),
        PendingChange::Move { .. } => {
            i18n_f("Could not move {len} messages: {e}", &[("len", &count.to_string()), ("e", e)])
        }
        PendingChange::Purge => i18n_f("Could not delete permanently: {e}", &[("e", e)]),
    }
}

/// Undo of a move that is still waiting (#200): it never reached the server,
/// so undoing it is forgetting it. Takes the queued moves of the messages
/// named by `message_ids` from `from` to `to` out of the queue, and answers
/// how many there were; the messages then show in `from` again.
pub(super) fn cancel_moves(
    cache: Option<&Cache>,
    account_id: u32,
    from: &str,
    to: &str,
    message_ids: &[String],
) -> usize {
    let Some(c) = cache else { return 0 };
    let ids: Vec<i64> = c
        .pending_actions(account_id)
        .into_iter()
        .filter(|a| {
            a.path == from
                && a.change == PendingChange::Move { dest: to.to_string() }
                && !a.message_id.is_empty()
                && message_ids.iter().any(|m| m.eq_ignore_ascii_case(&a.message_id))
        })
        .map(|a| a.id)
        .collect();
    c.drop_pending(&ids);
    ids.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(id: i64, uid: u32, change: PendingChange, was: Option<bool>) -> PendingAction {
        PendingAction { id, path: "INBOX".into(), uid, change, was, message_id: String::new(), attempts: 0 }
    }

    fn mv(id: i64, uid: u32, dest: &str) -> PendingAction {
        act(id, uid, PendingChange::Move { dest: dest.into() }, None)
    }

    /// What a folded queue does, in order, without its ids.
    fn shape(queue: &[PendingAction]) -> Vec<(u32, PendingChange)> {
        queue.iter().map(|a| (a.uid, a.change.clone())).collect()
    }

    fn ids(queue: &[PendingAction]) -> Vec<i64> {
        queue.iter().map(|a| a.id).collect()
    }

    #[test]
    fn read_then_unread_again_leaves_nothing_to_send() {
        let q = vec![
            act(1, 7, PendingChange::Seen(true), Some(false)),
            act(2, 7, PendingChange::Seen(false), Some(true)),
        ];
        assert!(collapse(q).is_empty());
    }

    #[test]
    fn a_mark_set_twice_is_sent_once_where_it_ended_up() {
        // Starred, unstarred, starred: the star is what is left to send.
        let q = vec![
            act(1, 7, PendingChange::Flagged(true), Some(false)),
            act(2, 7, PendingChange::Flagged(false), Some(true)),
            act(3, 7, PendingChange::Flagged(true), Some(false)),
        ];
        let out = collapse(q);
        assert_eq!(shape(&out), [(7, PendingChange::Flagged(true))]);
        assert_eq!(out[0].was, Some(false));
    }

    #[test]
    fn a_message_the_cache_did_not_know_keeps_its_last_mark() {
        // Without the state it started in, "back where it was" can't be
        // told from a real change: the last mark is sent.
        let q = vec![act(1, 7, PendingChange::Seen(true), None), act(2, 7, PendingChange::Seen(false), None)];
        assert_eq!(shape(&collapse(q)), [(7, PendingChange::Seen(false))]);
    }

    #[test]
    fn marks_on_different_messages_and_tags_stay_apart() {
        let kw = |keyword: &str, add| PendingChange::Keyword { keyword: keyword.into(), add };
        let q = vec![
            act(1, 7, PendingChange::Seen(true), Some(false)),
            act(2, 8, PendingChange::Seen(false), Some(true)),
            act(3, 7, kw("$work", true), Some(false)),
            act(4, 7, kw("$home", true), Some(false)),
            act(5, 7, kw("$WORK", false), Some(true)),
        ];
        let out = collapse(q);
        // $work on and off again cancels, whatever its case.
        assert_eq!(
            shape(&out),
            [(7, PendingChange::Seen(true)), (8, PendingChange::Seen(false)), (7, kw("$home", true))]
        );
        assert_eq!(ids(&out), [1, 2, 3], "the folded queue keeps the first ids, in order");
    }

    #[test]
    fn a_message_moved_again_is_moved_once_to_the_last_folder() {
        assert_eq!(shape(&collapse(vec![mv(1, 7, "Archive"), mv(2, 7, "Trash")])), shape(&[mv(0, 7, "Trash")]));
        // Moved back to where it is: nothing to do at all.
        assert!(collapse(vec![mv(1, 7, "Archive"), mv(2, 7, "INBOX")]).is_empty());
    }

    #[test]
    fn a_mark_made_after_a_move_goes_ahead_of_it() {
        // Read, archived, then starred and unread again in the archive
        // view: the star and the unread mark travel with the message.
        let q = vec![
            act(1, 7, PendingChange::Seen(true), Some(false)),
            mv(2, 7, "Archive"),
            act(3, 7, PendingChange::Flagged(true), Some(false)),
            act(4, 8, PendingChange::Seen(true), Some(false)),
            act(5, 7, PendingChange::Seen(false), Some(true)),
        ];
        let out = collapse(q);
        assert_eq!(
            shape(&out),
            [
                (7, PendingChange::Flagged(true)),
                (7, PendingChange::Move { dest: "Archive".into() }),
                (8, PendingChange::Seen(true)),
            ],
            "read and unread again cancel; the star goes before the move"
        );
        assert!(ids(&out).windows(2).all(|w| w[0] < w[1]), "ids follow the order to send in");
    }

    #[test]
    fn a_deletion_makes_what_came_before_it_moot() {
        let q = vec![
            act(1, 7, PendingChange::Seen(true), Some(false)),
            act(2, 8, PendingChange::Seen(true), Some(false)),
            act(3, 7, PendingChange::Flagged(true), Some(false)),
            act(4, 7, PendingChange::Purge, None),
            act(5, 7, PendingChange::Purge, None),
            act(6, 7, PendingChange::Flagged(false), Some(true)),
        ];
        assert_eq!(shape(&collapse(q)), [(8, PendingChange::Seen(true)), (7, PendingChange::Purge)]);
    }

    #[test]
    fn the_queue_goes_out_in_order_with_moves_batched() {
        let q = vec![
            act(1, 5, PendingChange::Seen(true), Some(false)),
            mv(2, 7, "Archive"),
            mv(3, 8, "Archive"),
            mv(4, 9, "Trash"),
            act(5, 6, PendingChange::Purge, None),
            act(6, 10, PendingChange::Purge, None),
            mv(7, 11, "Archive"),
        ];
        let steps = batches(&collapse(q));
        let shape: Vec<(Vec<i64>, Vec<u32>)> = steps.iter().map(|b| (b.ids.clone(), b.uids.clone())).collect();
        assert_eq!(
            shape,
            [
                (vec![1], vec![5]),
                (vec![2, 3], vec![7, 8]),
                (vec![4], vec![9]),
                (vec![5, 6], vec![6, 10]),
                // Another run to Archive after something else: its own step,
                // so nothing is sent ahead of what was done before it.
                (vec![7], vec![11]),
            ]
        );
    }

    #[test]
    fn a_change_goes_to_the_message_it_was_made_to() {
        let mv = PendingChange::Move { dest: "Archive".into() };
        // The same message, whatever the case of its id.
        assert!(same_message("a@example.com", Some("A@Example.com"), &mv));
        // Another message under the same UID (the folder was renumbered, or
        // the account points at another server): never.
        assert!(!same_message("a@example.com", Some("b@example.com"), &mv));
        assert!(!same_message("a@example.com", Some("b@example.com"), &PendingChange::Purge));
        assert!(!same_message("a@example.com", Some(""), &PendingChange::Seen(true)));
        // Gone from the folder.
        assert!(!same_message("a@example.com", None, &mv));
        // Unknown to the cache: a mark or a move goes by the UID, a
        // deletion does not.
        assert!(same_message("", Some("b@example.com"), &PendingChange::Flagged(true)));
        assert!(same_message("", Some(""), &mv));
        assert!(!same_message("", Some("b@example.com"), &PendingChange::Purge));
    }

    #[test]
    fn http_failures_keep_or_refuse_the_change() {
        use Failure::*;
        assert_eq!(classify_http("Microsoft Graph returned 429: Too many requests"), Keep);
        assert_eq!(classify_http("Microsoft Graph returned 503: Service unavailable"), Keep);
        assert_eq!(classify_http("the server returned 500: oops"), Keep);
        assert_eq!(classify_http("the server refused the sign-in (401): check the username"), Keep);
        assert_eq!(classify_http("https://graph.microsoft.com/v1.0/me: Dns Failed: resolve"), Keep);
        assert_eq!(classify_http("no sign-in token from GNOME Online Accounts"), Keep);
        assert_eq!(classify_http("Microsoft Graph returned 404: The specified object was not found"), Refused);
        assert_eq!(classify_http("the server returned 403: forbidden"), Refused);
        assert_eq!(classify_http("Email/set: invalidArguments (bad patch)"), Refused);
        assert_eq!(classify_http("Email/set: forbidden"), Refused);
        assert_eq!(classify_http("unknown folder Archive"), Refused);
        // A JMAP server failing or busy says "not now", not "no".
        assert_eq!(classify_http("Email/set: serverUnavailable"), Keep);
        assert_eq!(classify_http("Email/get: serverFail (database locked)"), Keep);
        assert_eq!(classify_http("Email/set: serverPartialFail"), Keep);
        assert_eq!(classify_http("Mailbox/get: rateLimit (slow down)"), Keep);
        assert!(jmap_error_keeps("rateLimit"));
        assert!(!jmap_error_keeps("notFound"));
        assert!(!jmap_error_keeps("forbidden"));
    }

    #[test]
    fn a_failed_run_waits_a_while_before_the_next() {
        let t0 = std::time::Instant::now();
        let mut clock = ReplayClock::new(true);
        assert!(clock.should_run(t0), "due at start");
        // A folder in use: changes left waiting. The loop comes round many
        // times a second, and none of those passes may run it again.
        clock.ran(t0, 3);
        assert!(!clock.should_run(t0));
        assert!(!clock.should_run(t0 + std::time::Duration::from_secs(30)));
        // Another change joining the queue does not cut the pause short.
        clock.changed();
        assert!(!clock.should_run(t0 + std::time::Duration::from_secs(59)));
        assert!(clock.should_run(t0 + RETRY_AFTER), "once the pause is over");
        // A new connection runs it at once.
        clock.ran(t0, 3);
        clock.wake();
        assert!(clock.should_run(t0));
        // A run that sent everything leaves nothing to wait out.
        clock.ran(t0, 0);
        assert!(!clock.should_run(t0 + RETRY_AFTER));
        clock.changed();
        assert!(clock.should_run(t0), "a new change goes at once");
    }

    #[test]
    fn no_folder_list_keeps_the_queue() {
        let c = Cache::in_memory().unwrap();
        let req = MailRequest::MoveMessages { path: "INBOX".into(), uids: vec![7, 8], dest: "Archive".into() };
        assert!(park(Some(&c), 1, changes_of(&req)));
        let req = MailRequest::SetSeen { path: "INBOX".into(), uid: 9, seen: true };
        assert!(park(Some(&c), 1, changes_of(&req)));
        let queue = c.pending_actions(1);
        folders_unknown(&c, &queue, &|_| {});
        let after = c.pending_actions(1);
        assert_eq!(after.len(), 3, "nothing sent, nothing dropped");
        assert!(after.iter().all(|a| a.attempts == 1), "each change counts one attempt");
    }

    #[test]
    fn only_a_new_host_or_protocol_drops_the_queue_and_says_so() {
        let c = Cache::in_memory().unwrap();
        let notices = std::cell::RefCell::new(Vec::new());
        let emit = |ev: WorkerEvent| {
            if let WorkerEvent::Notice(text) = ev {
                notices.borrow_mut().push(text);
            }
        };
        let mut account = super::super::sample_account();
        claim(Some(&c), 1, &account, &emit);
        let req = MailRequest::SetSeen { path: "INBOX".into(), uid: 7, seen: true };
        assert!(park(Some(&c), 1, changes_of(&req)));
        // A port or user changed while getting the account to connect: the
        // queue stays, the Message-ID check guarding what it reaches.
        account.imap_port = 1993;
        account.username = "someone-else".into();
        claim(Some(&c), 1, &account, &emit);
        assert_eq!(c.pending_count(1), 1);
        assert!(notices.borrow().is_empty());
        // Another server: dropped, and the user is told.
        account.imap_host = "imap.elsewhere.example".into();
        claim(Some(&c), 1, &account, &emit);
        assert_eq!(c.pending_count(1), 0);
        assert_eq!(notices.borrow().len(), 1);
        assert!(notices.borrow()[0].contains("discarded"), "{}", notices.borrow()[0]);
    }

    #[test]
    fn a_change_that_keeps_failing_is_given_up() {
        let c = Cache::in_memory().unwrap();
        let req = MailRequest::SetSeen { path: "INBOX".into(), uid: 7, seen: true };
        assert!(park(Some(&c), 1, changes_of(&req)));
        let id = c.pending_actions(1)[0].id;
        let errors = std::cell::RefCell::new(Vec::new());
        let emit = |ev: WorkerEvent| {
            if let WorkerEvent::Error { text, .. } = ev {
                errors.borrow_mut().push(text);
            }
        };
        for _ in 1..MAX_ATTEMPTS {
            attempt_failed(&c, &PendingChange::Seen(true), &[id], "connection reset", &emit);
        }
        assert_eq!(c.pending_count(1), 1, "still waiting after {} failures", MAX_ATTEMPTS - 1);
        assert_eq!(c.pending_actions(1)[0].attempts, MAX_ATTEMPTS - 1);
        assert!(errors.borrow().is_empty());
        attempt_failed(&c, &PendingChange::Seen(true), &[id], "connection reset", &emit);
        assert_eq!(c.pending_count(1), 0, "given up");
        assert_eq!(errors.borrow().len(), 1, "and said so, once");
    }

    #[test]
    fn a_queue_for_another_server_or_of_an_unknown_kind_is_dropped() {
        let c = Cache::in_memory().unwrap();
        assert_eq!(c.claim_pending(1, "imap|mail.example.com|ada"), 0);
        let req = MailRequest::SetSeen { path: "INBOX".into(), uid: 7, seen: true };
        assert!(park(Some(&c), 1, changes_of(&req)));
        assert_eq!(c.claim_pending(1, "imap|mail.example.com|ada"), 0, "the same server keeps its queue");
        assert_eq!(c.claim_pending(1, "imap|other.example.com|ada"), 1, "pointed elsewhere, it goes");
        assert_eq!(c.pending_count(1), 0);

        // A kind a newer build wrote: dropped as it is read, not counted
        // as waiting for ever.
        assert!(park(Some(&c), 1, changes_of(&req)));
        c.conn_for_tests()
            .execute("UPDATE pending_actions SET kind = 'snooze' WHERE account_id = 1", [])
            .unwrap();
        assert!(c.pending_actions(1).is_empty());
        assert_eq!(c.pending_count(1), 0);
    }

    #[test]
    fn the_queue_survives_in_the_cache_and_folds_as_it_grows() {
        let c = Cache::in_memory().unwrap();
        let req = MailRequest::SetSeen { path: "INBOX".into(), uid: 7, seen: true };
        assert!(park(Some(&c), 1, changes_of(&req)));
        let req = MailRequest::MoveMessages { path: "INBOX".into(), uids: vec![8, 9], dest: "Archive".into() };
        assert!(park(Some(&c), 1, changes_of(&req)));
        assert_eq!(c.pending_count(1), 3);
        assert_eq!(c.pending_count(2), 0, "another account's queue is its own");

        let queue = c.pending_actions(1);
        assert_eq!(queue[0].change, PendingChange::Seen(true));
        assert_eq!(queue[1].change, PendingChange::Move { dest: "Archive".into() });
        assert!(queue[0].id < queue[1].id && queue[1].id < queue[2].id);

        // Unread again. The cache does not hold message 7, so whether it
        // started out read is unknown: the read mark folds into the unread
        // one, which is sent, after the moves.
        let req = MailRequest::SetSeen { path: "INBOX".into(), uid: 7, seen: false };
        assert!(park(Some(&c), 1, changes_of(&req)));
        let queue = c.pending_actions(1);
        assert_eq!(queue.len(), 3);
        assert_eq!(queue[2].change, PendingChange::Seen(false));

        c.drop_pending(&[queue[0].id, queue[1].id]);
        assert_eq!(c.pending_count(1), 1);
        // No cache, nowhere to keep it: the caller reports the failure.
        assert!(!park(None, 1, changes_of(&req)));
        // Not a change that can wait.
        assert!(changes_of(&MailRequest::RefreshUnread).is_empty());
    }

    #[test]
    fn a_queued_move_hides_the_message_until_it_is_done_or_undone() {
        let c = Cache::in_memory().unwrap();
        let raw = b"Message-ID: <a@example.com>\r\nFrom: ada@example.com\r\nSubject: Notes\r\n\r\nbody";
        let mut m = super::super::summary_from_raw(1, 0, 7, raw);
        m.unread = true;
        assert_eq!(m.message_id, "a@example.com");
        c.upsert_messages(1, "INBOX", &[m]);

        // Read offline: the cache shows it read, and remembers it was not.
        let req = MailRequest::SetSeen { path: "INBOX".into(), uid: 7, seen: true };
        assert!(park(Some(&c), 1, changes_of(&req)));
        assert!(!c.load_messages(1, "INBOX", 0)[0].unread);
        assert_eq!(c.pending_actions(1)[0].was, Some(false));
        // ...and unread again: nothing left to send, nothing changed.
        let req = MailRequest::SetSeen { path: "INBOX".into(), uid: 7, seen: false };
        assert!(park(Some(&c), 1, changes_of(&req)));
        assert_eq!(c.pending_count(1), 0);
        assert!(c.load_messages(1, "INBOX", 0)[0].unread);

        let ids = vec!["a@example.com".to_string()];
        assert_eq!(c.messages_by_thread_ids(1, &ids).len(), 1);
        let req = MailRequest::MoveMessage { path: "INBOX".into(), uid: 7, dest: "Archive".into() };
        assert!(park(Some(&c), 1, changes_of(&req)));
        assert!(c.load_messages(1, "INBOX", 0).is_empty(), "moved, as far as the list knows");
        assert!(c.messages_by_thread_ids(1, &ids).is_empty(), "and as far as its conversation knows");
        assert!(c.thread_summaries(1, &[("t".into(), ids.clone())]).is_empty());
        assert_eq!(c.pending_actions(1)[0].message_id, "a@example.com");

        assert_eq!(cancel_moves(Some(&c), 1, "INBOX", "Trash", &ids), 0);
        assert_eq!(cancel_moves(Some(&c), 1, "INBOX", "Archive", &["A@example.com".into()]), 1);
        assert_eq!(c.load_messages(1, "INBOX", 0).len(), 1, "undone before it was sent");
        assert_eq!(c.messages_by_thread_ids(1, &ids).len(), 1);
        assert_eq!(c.pending_count(1), 0);
    }
}
