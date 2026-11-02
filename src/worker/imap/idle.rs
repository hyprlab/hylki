//! IMAP IDLE on the main connection: wait for new mail in the open folder
//! while still answering requests, and notice mail that landed between
//! commands.

use std::time::Duration;

use async_native_tls::TlsStream;
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::models::FolderKind;
use crate::worker::imap::folders::cached_folder_kind;
use crate::worker::imap::prefetch::{queue_attachment_prefetch, queue_body_prefetch};
use crate::worker::imap::retry::load_messages_retry;
use crate::worker::imap::stream::ImapIo;
use crate::worker::imap::sweep::{emit_body_hits, selected_chip_count};
use crate::worker::imap::wire::sel;
use crate::worker::imap::{ImapSession, IDLE_DONE_TIMEOUT, IDLE_START_TIMEOUT};
use crate::worker::{MailRequest, WorkerEvent};

/// Result of an IDLE wait: a request to handle, a folder that was refreshed
/// (new mail arrived), or the channel closing.
pub(super) enum IdleOutcome {
    Request(MailRequest),
    Refreshed,
    Quiet,
    Closed,
}

/// Block for the next request (used as the IDLE fallback).
async fn recv_one(rx: &mut mpsc::UnboundedReceiver<MailRequest>) -> IdleOutcome {
    match rx.recv().await {
        Some(req) => IdleOutcome::Request(req),
        None => IdleOutcome::Closed,
    }
}

/// End an IDLE and recover the session, giving the DONE handshake
/// [`IDLE_DONE_TIMEOUT`] to complete. `None` on timeout or error — the future
/// (and with it the wedged connection) is simply dropped, and the caller's
/// next request reconnects (#91: an unbounded await here swallowed the very
/// request that interrupted the IDLE).
pub(super) async fn idle_done(
    mut handle: async_imap::extensions::idle::Handle<ImapIo<TlsStream<TcpStream>>>,
) -> Option<ImapSession> {
    handle.as_mut().set_idle(false);
    match tokio::time::timeout(IDLE_DONE_TIMEOUT, handle.done()).await {
        Ok(res) => res.ok(),
        Err(_) => {
            tracing::info!("IDLE DONE went unanswered; dropping the connection");
            None
        }
    }
}

/// Enter IMAP IDLE on `path` for up to `timeout_secs` and wait for new mail or
/// an incoming request. Returns `Refreshed` (after re-syncing) only when the
/// server actually reports new data; `Quiet` on timeout (so the caller can do a
/// prefetch pass and re-IDLE); `Request` to be handled normally; `Closed` when
/// the channel ends. Any IDLE error falls back to a plain receive.
pub(super) async fn idle_wait(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    account_id: u32,
    folder_id: u32,
    path: &str,
    rx: &mut mpsc::UnboundedReceiver<MailRequest>,
    cache: Option<&Cache>,
    use_envelope: &mut bool,
    body_prefetch: &mut std::collections::VecDeque<(String, u32)>,
    att_prefetch: &mut std::collections::VecDeque<(String, u32)>,
    body_emitted: &std::collections::HashSet<(String, u32)>,
    emit: &impl Fn(WorkerEvent),
    timeout_secs: u64,
) -> IdleOutcome {
    let Some(mut sess) = session.take() else {
        return recv_one(rx).await;
    };
    // Stale or wedged connection — drop it so the next request reconnects.
    // The timeout matters as much as the error: a dead-but-open connection
    // (silently dropped by a NAT during a long IDLE) answers nothing at all.
    let missed = match tokio::time::timeout(IDLE_START_TIMEOUT, sel(&mut sess, path)).await {
        Ok(Ok(mailbox)) => {
            // The sweep passes over the folder IDLE works, so a Snoozed
            // folder being watched says here how much it still holds
            // (#542): mail moved out of it never wakes the IDLE.
            if cached_folder_kind(cache, account_id, path) == Some(FolderKind::Snoozed) {
                emit(WorkerEvent::SnoozedCount {
                    path: path.to_string(),
                    count: mailbox.exists,
                });
            }
            let missed = missed_new_mail(account_id, path, mailbox.uid_next);
            // Taken as seen now: the sync below records it again, and one
            // that fails must not send every later wait round this branch.
            if missed {
                note_synced_uid_next(account_id, path, mailbox.uid_next);
            }
            missed
        }
        _ => return recv_one(rx).await,
    };

    enum Wake {
        Idle(async_imap::error::Result<async_imap::extensions::idle::IdleResponse>),
        Request(Option<MailRequest>),
        /// Mail arrived while the connection was busy; the SELECT just
        /// made reported it, and an IDLE started now never would.
        Missed,
    }
    let wake = if missed {
        tracing::info!("idle: new mail in {path} arrived between commands, syncing");
        *session = Some(sess);
        Wake::Missed
    } else {
        tracing::debug!(target: "hylki::imap", "> IDLE");
        let mut handle = sess.idle();
        match tokio::time::timeout(IDLE_START_TIMEOUT, handle.init()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                *session = idle_done(handle).await;
                return recv_one(rx).await;
            }
            // No answer: the handle (and its connection) is dropped, not reused.
            Err(_) => return recv_one(rx).await,
        }
        let wake = {
            handle.as_mut().set_idle(true);
            let (idle_fut, stop) = handle.wait_with_timeout(Duration::from_secs(timeout_secs));
            tokio::select! {
                r = idle_fut => Wake::Idle(r),
                req = rx.recv() => { drop(stop); Wake::Request(req) }
            }
        };
        *session = idle_done(handle).await;
        wake
    };

    match wake {
        Wake::Request(Some(req)) => IdleOutcome::Request(req),
        Wake::Request(None) => IdleOutcome::Closed,
        // Only re-sync on actual new data; a plain timeout is Quiet.
        Wake::Missed | Wake::Idle(Ok(async_imap::extensions::idle::IdleResponse::NewData(_))) => {
            if session.is_some() {
                if let Ok(messages) = load_messages_retry(
                    account_id,
                    session,
                    account,
                    folder_id,
                    path,
                    use_envelope,
                    cache,
                )
                .await
                {
                    if let Some(c) = cache {
                        c.upsert_messages(account_id, path, &messages);
                    }
                    // Pre-download the new mail's body (and any attachments) so
                    // opening it is instant.
                    queue_body_prefetch(body_prefetch, path, &messages, body_emitted);
                    queue_attachment_prefetch(att_prefetch, path, &messages, cache, account_id);
                    emit_body_hits(
                        session, account, account_id, folder_id, path, &messages, cache, emit,
                    )
                    .await;
                    emit(WorkerEvent::Messages {
                        folder_id,
                        messages,
                    });
                    // Refresh the true unread count too. IDLE only re-synced the
                    // message list; without this the sidebar chip never moves when
                    // new mail lands in a background (unfocused) inbox — it would
                    // take an explicit reload / "All Inboxes" refresh to appear.
                    if let Some(sess) = session.as_mut() {
                        let kind = cached_folder_kind(cache, account_id, path);
                        if let Some(unread) = selected_chip_count(sess, kind).await {
                            emit(WorkerEvent::FolderUnread { folder_id, unread });
                        }
                    }
                }
            }
            IdleOutcome::Refreshed
        }
        Wake::Idle(_) => IdleOutcome::Quiet,
    }
}

/// The UIDNEXT each folder had when its list was last loaded, by (account,
/// path). Mail that lands while the connection is busy with something else
/// is only told to the SELECT that comes before the next IDLE, never to the
/// IDLE itself, so that SELECT has to be compared against this (#336).
static SYNCED_UID_NEXT: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<(u32, String), u32>>,
> = std::sync::LazyLock::new(Default::default);

pub(super) fn note_synced_uid_next(account_id: u32, path: &str, uid_next: Option<u32>) {
    // A poisoned lock is skipped, as the worker's other session maps are.
    if let (Some(next), Ok(mut synced)) = (uid_next, SYNCED_UID_NEXT.lock()) {
        synced.insert((account_id, path.to_string()), next);
    }
}

/// Whether a folder's UIDNEXT has moved past what its last loaded list saw:
/// mail arrived that no sync has picked up.
fn missed_new_mail(account_id: u32, path: &str, uid_next: Option<u32>) -> bool {
    let seen = SYNCED_UID_NEXT
        .lock()
        .ok()
        .and_then(|synced| synced.get(&(account_id, path.to_string())).copied());
    matches!((seen, uid_next), (Some(seen), Some(now)) if now > seen)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #336: mail that lands while the connection is busy shows only as a
    /// moved UIDNEXT on the next SELECT.
    #[test]
    fn a_moved_uid_next_is_missed_mail() {
        let path = "INBOX-336-test";
        assert!(!missed_new_mail(99, path, Some(10)), "nothing loaded yet");
        note_synced_uid_next(99, path, Some(10));
        assert!(!missed_new_mail(99, path, Some(10)));
        assert!(missed_new_mail(99, path, Some(12)));
        assert!(!missed_new_mail(99, path, None));
        assert!(!missed_new_mail(98, path, Some(12)), "another account");
    }
}
