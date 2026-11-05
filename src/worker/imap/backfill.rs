//! The background index (#429): the rest of each folder past the first
//! page, a chunk at a time, and the one-time References repair.

use async_imap::types::Fetch;
use futures::TryStreamExt;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::i18n_f;
use crate::models::Message;
use crate::worker::ids::merge_msgids;
use crate::worker::imap::connect::connect;
use crate::worker::imap::fetch_mode::{
    compact_parse_error, is_parse_error, readable_error, step_fetch_mode,
};
use crate::worker::imap::gmail::read_gmail_labels;
use crate::worker::imap::prefetch::queue_attachment_prefetch;
use crate::worker::imap::summary::{build_summary, references_of, summary_from_headers};
use crate::worker::imap::wire::{fetch_uids, search_uids, sel, uid_set};
use crate::worker::imap::{ImapSession, BACKFILL_CHUNK, REFS_FETCH_ITEM};
use crate::worker::WorkerEvent;

/// How many messages one References-repair step asks the server about. Each row
/// costs only its References header, so this can be larger than an envelope
/// chunk without making the connection unresponsive to real requests.
const REFS_REPAIR_CHUNK: usize = 500;

/// One step of the one-time References repair for the folder at the head of the
/// queue (#21, #42).
///
/// Messages indexed before Hylki asked for References hold only In-Reply-To,
/// which names the immediate parent — and for an incoming reply that parent is
/// usually your own message in Sent, so the link points outside the folder and
/// the reply starts a thread of its own. This asks the server for the one header
/// that fixes it, for the replies that can still be improved, newest first.
///
/// The watermark makes it resumable and run exactly once per folder; a repaired
/// row re-threads the next time its folder is read from cache.
pub(super) async fn run_one_refs_repair(
    queue: &mut std::collections::VecDeque<(u32, String)>,
    session: &mut Option<ImapSession>,
    account_id: u32,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    let Some((folder_id, path)) = queue.pop_front() else {
        return;
    };
    let Some(c) = cache else { return }; // nothing to repair without a cache
    let Some(mut s) = session.take() else {
        queue.push_front((folder_id, path));
        return;
    };

    let (below, _) = c.refs_repair_state(account_id, &path);
    let chunk = c.uids_needing_references(account_id, &path, below, REFS_REPAIR_CHUNK);
    if chunk.is_empty() {
        c.set_refs_repair_state(account_id, &path, 0, true);
        *session = Some(s); // folder done — don't requeue
        return;
    }

    let set = uid_set(&chunk.iter().map(|(uid, _)| *uid).collect::<Vec<_>>());
    let fetched: Result<Vec<Fetch>, _> = async {
        sel(&mut s, &path).await?;
        fetch_uids(&mut s, &set, format!("(UID{REFS_FETCH_ITEM})"))
            .await?
            .try_collect()
            .await
    }
    .await;
    let Ok(fetches) = fetched else {
        // Leave the watermark alone and try this folder again later.
        queue.push_back((folder_id, path));
        *session = Some(s);
        return;
    };

    let found: std::collections::HashMap<u32, String> = fetches
        .iter()
        .filter_map(|f| f.uid.map(|uid| (uid, references_of(f))))
        .collect();
    let mut repaired = 0usize;
    for (uid, existing) in &chunk {
        if let Some(refs) = found.get(uid) {
            let merged = merge_msgids(refs, existing);
            if &merged != existing {
                c.set_references(account_id, &path, *uid, &merged);
                repaired += 1;
            }
        }
    }
    // Walk on from the oldest uid this chunk covered.
    let lowest = chunk.iter().map(|(uid, _)| *uid).min().unwrap_or(0);
    c.set_refs_repair_state(account_id, &path, lowest, false);
    queue.push_back((folder_id, path));
    *session = Some(s);
    if repaired > 0 {
        // The folder's cached rows have changed underneath whatever is on
        // screen; re-emitting lets the list re-thread with what was found.
        emit(WorkerEvent::RefsRepaired { folder_id });
    }
}

/// A background job to index the rest of a folder (everything past the fast first
/// page) so search covers the whole mailbox. `remaining` is the still-to-fetch
/// UIDs (newest first), computed lazily on the first drain.
#[derive(Clone)]
pub(in crate::worker) struct Backfill {
    pub(in crate::worker) folder_id: u32,
    pub(in crate::worker) path: String,
    pub(in crate::worker) remaining: Option<Vec<u32>>,
    /// Whether this folder feeds the attachments gallery (not Trash/Junk/Drafts);
    /// if so, its backfilled messages' attachments are prefetched too.
    pub(in crate::worker) gallery: bool,
    /// Steps that failed in a row while connected; at [`BACKFILL_TRIES`] the
    /// folder is left out of the index for the session.
    pub(in crate::worker) failures: u32,
}

/// How many failed steps in a row a folder's background index takes before
/// it is given up for the session. Nothing waits between the tries (the
/// idle ladder climbs again at once), so without a limit a folder the
/// server always fails on is asked about in a tight loop (#512).
const BACKFILL_TRIES: u32 = 3;

/// Whether the server turned the command down on a sound connection (NO or
/// BAD): the folder, not the link, is the problem, and a fresh login gets
/// the same answer. Dovecot lists a Sieve script's symlink as a folder and
/// answers every SELECT of it with NO [SERVERBUG] (#512).
fn refused(e: &async_imap::error::Error) -> bool {
    matches!(
        e,
        async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_)
    )
}

/// Whether a folder left out of the index has been shown in the status bar
/// this run. Once is enough (#512): the next one, on this account or another
/// on the same server, most likely has the same cause, and the log names
/// every one.
static LEFT_OUT_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Leave a folder out of the background index for the session: the list
/// stops promising more of it, the log says why, and the first one of the
/// run is shown in the status bar.
fn give_up_backfill(
    job: &Backfill,
    account_id: u32,
    e: &async_imap::error::Error,
    emit: &impl Fn(WorkerEvent),
) {
    tracing::warn!(
        "background index: leaving {} out on account {account_id}: {}",
        job.path,
        compact_parse_error(&e.to_string())
    );
    emit(WorkerEvent::BackfillDone {
        folder_id: job.folder_id,
    });
    if !LEFT_OUT_SHOWN.swap(true, std::sync::atomic::Ordering::Relaxed) {
        emit(WorkerEvent::error(i18n_f(
            "“{folder}” is left out of search: {reason}",
            &[("folder", &job.path), ("reason", &server_reason(e))],
        )));
    }
}

/// What the server said in a NO or BAD, without the response-code wrapping
/// async-imap puts round it or the time and timings Dovecot ends with.
fn server_reason(e: &async_imap::error::Error) -> String {
    let text = e.to_string();
    let said = text
        .split_once("info: Some(\"")
        .and_then(|(_, rest)| rest.rsplit_once("\")"))
        .map_or(text.as_str(), |(said, _)| said);
    let said = said
        .match_indices(" [")
        .find(|(i, _)| said[i + 2..].starts_with(|c: char| c.is_ascii_digit()))
        .map_or(said, |(i, _)| &said[..i]);
    readable_error(&compact_parse_error(said))
}

// select! may receive a reader before polling the job at all. In that case
// the original is still queued; otherwise restore the snapshot, including any
// chunk the cancelled future drained before the network response arrived.
pub(in crate::worker) fn restore_interrupted_backfill(
    queue: &mut std::collections::VecDeque<Backfill>,
    job: Backfill,
) {
    if !queue.iter().any(|queued| queued.path == job.path) {
        queue.push_front(job);
    }
}

/// Determine which UIDs still need indexing: everything on the server not already
/// cached. Also reconciles deletions (cached UIDs the server no longer has).
async fn backfill_worklist(
    session: &mut ImapSession,
    account_id: u32,
    path: &str,
    cache: Option<&Cache>,
) -> Result<Vec<u32>, async_imap::error::Error> {
    sel(session, path).await?;
    let server: std::collections::HashSet<u32> = search_uids(session, "ALL").await?;
    let cached = cache
        .map(|c| c.cached_uids(account_id, path))
        .unwrap_or_default();
    if let Some(c) = cache {
        for uid in cached.iter() {
            if !server.contains(uid) {
                c.delete_message(account_id, path, *uid);
            }
        }
    }
    let mut remaining: Vec<u32> = server.difference(&cached).copied().collect();
    remaining.sort_unstable_by(|a, b| b.cmp(a)); // newest first
    Ok(remaining)
}

/// Fetch message summaries for a specific set of UIDs (used by the backfill).
pub(super) async fn fetch_summaries_by_uid(
    account_id: u32,
    session: &mut ImapSession,
    folder_id: u32,
    path: &str,
    uids: &[u32],
    use_envelope: bool,
) -> Result<Vec<Message>, async_imap::error::Error> {
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    sel(session, path).await?;
    let set = uid_set(uids);
    let items: String = if use_envelope {
        format!("(UID ENVELOPE FLAGS BODYSTRUCTURE INTERNALDATE{REFS_FETCH_ITEM})")
    } else {
        "(UID FLAGS BODY.PEEK[HEADER] INTERNALDATE)".to_string()
    };
    let fetches: Vec<Fetch> = fetch_uids(session, set, items).await?.try_collect().await?;
    let mut messages: Vec<Message> = fetches
        .iter()
        .map(|f| {
            if use_envelope {
                build_summary(account_id, f, folder_id)
            } else {
                summary_from_headers(account_id, f, folder_id)
            }
        })
        .collect();
    read_gmail_labels(account_id, session, path, uids, &mut messages).await?;
    Ok(messages)
}

/// Advance one backfill job by a chunk. Fetches the next `BACKFILL_CHUNK` UIDs,
/// upserts them into the cache, and emits them as an append to the UI's index.
/// Requeues the job (at the back) if more remain. Reconnects and, if needed,
/// disables ENVELOPE parsing (iCloud) on a parse failure.
pub(super) async fn run_one_backfill(
    queue: &mut std::collections::VecDeque<Backfill>,
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    prefetch: &mut std::collections::VecDeque<(String, u32)>,
    use_envelope: &mut bool,
    emit: &impl Fn(WorkerEvent),
) {
    let Some(mut job) = queue.pop_front() else {
        return;
    };
    let Some(mut s) = session.take() else {
        queue.push_front(job);
        return;
    };

    // Compute the worklist on first touch (with one reconnect on failure).
    let worklist = match job.remaining.take() {
        Some(rem) => rem,
        None => match backfill_worklist(&mut s, account_id, &job.path, cache).await {
            Ok(rem) => rem,
            Err(e) if refused(&e) => {
                give_up_backfill(&job, account_id, &e, emit);
                *session = Some(s);
                return;
            }
            Err(_) => match connect(account).await {
                Ok(fresh) => {
                    s = fresh;
                    match backfill_worklist(&mut s, account_id, &job.path, cache).await {
                        Ok(rem) => rem,
                        Err(e) => {
                            *session = Some(s);
                            job.failures += 1;
                            if refused(&e) || job.failures >= BACKFILL_TRIES {
                                give_up_backfill(&job, account_id, &e, emit);
                            } else {
                                queue.push_back(job);
                            }
                            return;
                        }
                    }
                }
                Err(_) => {
                    // Stay offline; retry this job on the next connect.
                    queue.push_back(job);
                    return;
                }
            },
        },
    };

    let rem = job.remaining.insert(worklist);
    if rem.is_empty() {
        emit(WorkerEvent::BackfillDone {
            folder_id: job.folder_id,
        });
        *session = Some(s); // done — don't requeue
        return;
    }
    let take = rem.len().min(BACKFILL_CHUNK);
    let chunk: Vec<u32> = rem.drain(..take).collect();

    match fetch_summaries_by_uid(
        account_id,
        &mut s,
        job.folder_id,
        &job.path,
        &chunk,
        *use_envelope,
    )
    .await
    {
        Ok(msgs) => {
            job.failures = 0;
            if let Some(c) = cache {
                c.upsert_messages(account_id, &job.path, &msgs);
            }
            // Gallery folders: queue this chunk's attachments for background
            // download so they appear in the attachments gallery.
            if job.gallery {
                queue_attachment_prefetch(prefetch, &job.path, &msgs, cache, account_id);
            }
            emit(WorkerEvent::MessagesAppend {
                folder_id: job.folder_id,
                messages: msgs,
            });
            *session = Some(s);
        }
        Err(e) => {
            // Put the chunk back and reconnect; a parse error means the server
            // answers this way of asking with something unreadable (iCloud's
            // ENVELOPE, Mailfence with the preview items, #226) — move down
            // the same ladder the first load uses. A step down the ladder is
            // a new way of asking, not a failed try.
            for uid in chunk.into_iter().rev() {
                rem.insert(0, uid);
            }
            if !(is_parse_error(&e) && step_fetch_mode(account_id, use_envelope)) {
                job.failures += 1;
            }
            let gave_up = job.failures >= BACKFILL_TRIES;
            if gave_up {
                give_up_backfill(&job, account_id, &e, emit);
            }
            // A refusal leaves the connection sound; anything else may have
            // left it mid-reply.
            if refused(&e) {
                *session = Some(s);
            } else if let Ok(fresh) = connect(account).await {
                *session = Some(fresh);
            }
            // else: session stays None; the main loop reconnects on next request.
            if gave_up {
                return;
            }
        }
    }

    if job.remaining.as_ref().is_some_and(|r| !r.is_empty()) {
        queue.push_back(job);
    } else {
        emit(WorkerEvent::BackfillDone {
            folder_id: job.folder_id,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dovecot's refusal of a Sieve script's symlink, as async-imap hands it
    /// over (#512).
    fn serverbug() -> async_imap::error::Error {
        async_imap::error::Error::No(
            "code: None, info: Some(\"[SERVERBUG] Internal error occurred. Refer to server log \
             for more information. [2026-10-23 12:42:56] (0.001 + 0.013 + 0.012 secs).\")"
                .into(),
        )
    }

    #[test]
    fn server_reason_keeps_the_servers_words() {
        assert_eq!(
            server_reason(&serverbug()),
            "[SERVERBUG] Internal error occurred. Refer to server log for more information."
        );
        assert_eq!(
            server_reason(&async_imap::error::Error::ConnectionLost),
            async_imap::error::Error::ConnectionLost.to_string()
        );
    }

    /// Only the first folder left out in a run reaches the status bar; the
    /// list is told about every one.
    #[test]
    fn a_left_out_folder_is_shown_once_a_run() {
        let job = |folder_id| Backfill {
            folder_id,
            path: "dovecot.sieve".into(),
            remaining: None,
            gallery: false,
            failures: 0,
        };
        let events = std::cell::RefCell::new(Vec::new());
        let emit = |ev| events.borrow_mut().push(ev);
        give_up_backfill(&job(1), 1, &serverbug(), &emit);
        give_up_backfill(&job(2), 2, &serverbug(), &emit);
        let events = events.into_inner();
        let shown: Vec<&String> = events
            .iter()
            .filter_map(|ev| match ev {
                WorkerEvent::Error { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(
            shown,
            [
                "“dovecot.sieve” is left out of search: [SERVERBUG] Internal error occurred. \
              Refer to server log for more information."
            ]
        );
        let done = events
            .iter()
            .filter(|ev| matches!(ev, WorkerEvent::BackfillDone { .. }))
            .count();
        assert_eq!(done, 2);
    }
}
