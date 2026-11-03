//! Sending from an IMAP account: the Outbox flush, the Sent copy, marking
//! what a message answered, and the Drafts folder.

use async_imap::types::Fetch;
use futures::TryStreamExt;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};
use crate::worker::compose::editable::queued_with_bcc;
use crate::worker::imap::keywords::FORWARDED_KEYWORD;
use crate::worker::imap::mutate::{store_flag, store_keyword};
use crate::worker::imap::retry::load_messages_retry;
use crate::worker::imap::wire::{
    append_msg, create_box, expunge_all, sel, store_uids, subscribe_box,
};
use crate::worker::imap::ImapSession;
use crate::worker::outbox::{
    current_sent_copy_path, due_outbox_items, emit_outbox, note_answered, outbox_envelope,
    raw_in_reply_to,
};
use crate::worker::smtp::{send_raw_smtp, smtp_refused};
use crate::worker::{answered_copies, reply_copy_path, WorkerEvent};

/// Mark what a sent message answered (#454): `\Answered` on the original
/// of a reply, `$Forwarded` on that of a forward. A mailbox that takes no
/// keywords has nowhere to keep `$Forwarded`, and the arrow then lasts until
/// the folder syncs. A failure is only logged: the message went, and that
/// is what the user asked about.
pub(super) async fn mark_answered(
    session: &mut Option<ImapSession>,
    cache: Option<&Cache>,
    account_id: u32,
    in_reply_to: &str,
    forwards: &str,
    emit: &impl Fn(WorkerEvent),
) {
    for (path, uid, forwarded) in answered_copies(cache, account_id, in_reply_to, forwards) {
        if let Some(sess) = session.as_mut() {
            let stored = if forwarded {
                store_keyword(sess, &path, uid, FORWARDED_KEYWORD, true)
                    .await
                    .map(|_| ())
            } else {
                store_flag(sess, &path, uid, "\\Answered", true).await
            };
            if let Err(e) = stored {
                tracing::warn!("[account {account_id}] could not mark {path}/{uid} answered: {e}");
            }
        }
        note_answered(cache, account_id, path, uid, forwarded, emit);
    }
}

/// File a sent copy: beside the original when `reply` names a folder, and
/// in `sent` when there is none or that folder will not take it (one the
/// user may only read). The folder that took it, or why none did.
pub(super) async fn file_sent_copy(
    session: &mut ImapSession,
    reply: Option<&str>,
    sent: Option<&str>,
    raw: &[u8],
) -> Result<Option<String>, async_imap::error::Error> {
    if let Some(path) = reply {
        match append_to_sent(session, path, raw).await {
            Ok(()) => return Ok(Some(path.to_string())),
            Err(e) => tracing::warn!("could not file the reply's copy in {path}, using Sent: {e}"),
        }
    }
    let Some(path) = sent else { return Ok(None) };
    append_to_sent(session, path, raw)
        .await
        .map(|()| Some(path.to_string()))
}

/// Try the queue: one message when `id` is given, otherwise all of them, oldest
/// first. A message that goes out is copied to Sent and dropped from the queue;
/// one that fails again keeps its place with the new reason recorded. `loud`
/// reports failures to the UI — background sweeps stay quiet, since the user did
/// not ask for anything and already knows the message is waiting.
pub(in crate::worker) async fn flush_outbox(
    cache: Option<&Cache>,
    account_id: u32,
    account: &AccountConfig,
    id: Option<u32>,
    session: &mut Option<ImapSession>,
    use_envelope: &mut bool,
    emit: &impl Fn(WorkerEvent),
    loud: bool,
) {
    let Some(cache) = cache else { return };
    let items = due_outbox_items(cache, account_id, id);
    if items.is_empty() {
        return;
    }

    if loud {
        emit(WorkerEvent::Status(i18n("Sending…")));
    }
    let mut sent_any = false;
    let mut sent = 0usize;
    for item in items {
        let envelope = match outbox_envelope(&item) {
            Some(envelope) => envelope,
            None => {
                // Unsendable as stored: keep it, but say so rather than retrying
                // it forever in silence.
                cache.record_outbox_failure(
                    item.id,
                    "the stored recipients are not valid addresses",
                );
                continue;
            }
        };
        match send_raw_smtp(account, &envelope, &item.raw).await {
            Ok(()) => {
                sent_any = true;
                sent += 1;
                cache.delete_outbox(item.id);
                mark_answered(
                    session,
                    Some(cache),
                    account_id,
                    &raw_in_reply_to(&item.raw),
                    &item.forwards,
                    emit,
                )
                .await;
                let dest = current_sent_copy_path(
                    account,
                    account_id,
                    Some(cache),
                    item.sent_path.as_deref(),
                );
                let reply = reply_copy_path(
                    account,
                    account_id,
                    Some(cache),
                    &raw_in_reply_to(&item.raw),
                );
                if let Some(sess) = session.as_mut() {
                    let copy = queued_with_bcc(&item.raw, &item.rcpts);
                    match file_sent_copy(sess, reply.as_deref(), dest.as_deref(), &copy).await {
                        Ok(Some(path)) => {
                            index_sent_copy(
                                account_id,
                                session,
                                account,
                                &path,
                                use_envelope,
                                Some(cache),
                                emit,
                            )
                            .await;
                        }
                        Ok(None) => {}
                        Err(e) => tracing::warn!("outbox: sent, but saving the copy failed: {e}"),
                    }
                }
            }
            Err(e) => {
                cache.record_outbox_failure(item.id, &e.to_string());
                if loud {
                    emit(WorkerEvent::error(i18n_f(
                        "Still could not send “{subject}”: {e}",
                        &[
                            ("subject", &item.subject.to_string()),
                            ("e", &e.to_string()),
                        ],
                    )));
                }
                // A failure now will almost certainly repeat for the rest of the
                // queue (the connection is down), so stop rather than hammering.
                // A message the server refused says nothing about the rest:
                // a merge's message with a bad address must not hold up the
                // others behind it (#447).
                if !smtp_refused(&e) {
                    break;
                }
            }
        }
    }
    if loud {
        emit(WorkerEvent::Status(String::new()));
    }
    if sent_any {
        // A background flush is silent up to here, but the user was last told the
        // message had *failed* to send. They have to learn that it since went.
        if !loud {
            emit(WorkerEvent::Notice(match sent {
                1 => "A message waiting in the Outbox has been sent".to_string(),
                n => format!("{n} messages waiting in the Outbox have been sent"),
            }));
        }
        emit(WorkerEvent::Sent);
    }
    emit_outbox(Some(cache), account_id, emit);
}

pub(super) async fn append_to_sent(
    session: &mut ImapSession,
    path: &str,
    raw: &[u8],
) -> Result<(), async_imap::error::Error> {
    // Mark the saved copy as already read.
    append_msg(session, path, Some("(\\Seen)"), raw).await
}

/// The copy of a message just sent was APPENDed to `path`: list that folder
/// again so the copy is in the cache now. A conversation is assembled from
/// the cache (`related_from_cache`), so until the copy is indexed the reply
/// cannot join the thread it answers, and the next visit to the Sent folder
/// was the only thing that indexed it (#199). The folder's listing goes out
/// too, so a list showing that folder (copies filed into the Inbox) is
/// current as well.
pub(super) async fn index_sent_copy(
    account_id: u32,
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    path: &str,
    use_envelope: &mut bool,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    let Some(c) = cache else { return };
    let Some(folder_id) = c
        .load_folders(account_id)
        .iter()
        .find(|f| f.path == path)
        .map(|f| f.id)
    else {
        return;
    };
    match load_messages_retry(
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
        Ok(messages) => {
            c.upsert_messages(account_id, path, &messages);
            emit(WorkerEvent::Messages {
                folder_id,
                messages,
            });
        }
        Err(e) => tracing::warn!("could not list {path} after saving the sent copy: {e}"),
    }
}

/// APPEND a draft to the Drafts folder (flagged `\Draft \Seen`), creating the
/// mailbox first if the server doesn't have one yet.
pub(super) async fn append_draft(
    session: &mut ImapSession,
    path: &str,
    raw: &[u8],
    template: bool,
) -> Result<(), async_imap::error::Error> {
    // A template (#360) is no draft: another client would offer to finish
    // and send it rather than start a message from it.
    let flags = if template {
        "(\\Seen)"
    } else {
        "(\\Draft \\Seen)"
    };
    if append_msg(session, path, Some(flags), raw).await.is_ok() {
        return Ok(());
    }
    // Folder likely doesn't exist — create it and retry.
    let _ = create_box(session, path).await;
    let _ = subscribe_box(session, path).await;
    append_msg(session, path, Some(flags), raw).await
}

/// Delete a superseded draft (the previous version being replaced or sent):
/// flag it `\Deleted` and expunge it from the Drafts folder.
pub(super) async fn delete_draft(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
) -> Result<(), async_imap::error::Error> {
    sel(session, path).await?;
    let _: Result<Vec<Fetch>, _> = store_uids(session, uid.to_string(), "+FLAGS (\\Deleted)")
        .await?
        .try_collect()
        .await;
    let _: Vec<u32> = expunge_all(session).await?.try_collect().await?;
    Ok(())
}
