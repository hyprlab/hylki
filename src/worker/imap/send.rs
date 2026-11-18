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
use crate::worker::local::sent::keep_sent_here;
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
    file_pending_sent_copies(cache, account_id, account, session, use_envelope, emit).await;
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
                // Who it left as decides whether the server kept a copy (#640).
                let from = envelope.from().map(|f| f.to_string());
                let dest = current_sent_copy_path(
                    account,
                    account_id,
                    Some(cache),
                    item.sent_path.as_deref(),
                    from.as_deref(),
                );
                let reply = reply_copy_path(
                    account,
                    account_id,
                    Some(cache),
                    &raw_in_reply_to(&item.raw),
                    from.as_deref(),
                );
                let copy = queued_with_bcc(&item.raw, &item.rcpts);
                let filed = match session.as_mut() {
                    Some(sess) => {
                        file_sent_copy(sess, reply.as_deref(), dest.as_deref(), &copy).await
                    }
                    // A POP3 account: no folders on its server.
                    None => Ok(None),
                };
                match filed {
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
                    Ok(None) => keep_sent_here(account, from.as_deref(), &copy, emit),
                    Err(e) => {
                        tracing::warn!("outbox: sent, but saving the copy failed: {e}");
                        cache.queue_sent_copy(
                            account_id,
                            &copy,
                            reply.as_deref(),
                            dest.as_deref(),
                            &e.to_string(),
                        );
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

/// File the copies of sent messages that the server refused earlier (#678),
/// oldest first. One that fails again stays for the next connection; the
/// user was told when the send happened, so a retry is quiet.
async fn file_pending_sent_copies(
    cache: &Cache,
    account_id: u32,
    account: &AccountConfig,
    session: &mut Option<ImapSession>,
    use_envelope: &mut bool,
    emit: &impl Fn(WorkerEvent),
) {
    for copy in cache.pending_sent_copies(account_id) {
        let Some(sess) = session.as_mut() else { return };
        match file_sent_copy(
            sess,
            copy.reply_path.as_deref(),
            copy.sent_path.as_deref(),
            &copy.raw,
        )
        .await
        {
            Ok(path) => {
                cache.delete_sent_copy(copy.id);
                if let Some(path) = path {
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
            }
            Err(e) => {
                tracing::warn!(
                    "[account {account_id}] the sent copy kept since {} attempts still could not be saved: {e}",
                    copy.attempts
                );
                cache.record_sent_copy_failure(copy.id, &e.to_string());
                return;
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A sent copy the server refused is kept, and filed by the next
    /// connection once the folder takes it (#678). Run against a test
    /// server only: `IMAP_LIVE=host,port,user,password`.
    #[test]
    #[ignore]
    fn live_a_refused_sent_copy_is_filed_later() {
        use crate::worker::imap::connect::connect;
        use crate::worker::imap::wire::{box_status, create_box, delete_box};
        use crate::worker::test_support::sample_account;

        let Ok(spec) = std::env::var("IMAP_LIVE") else {
            return;
        };
        let p: Vec<&str> = spec.splitn(4, ',').collect();
        let account = AccountConfig {
            imap_host: p[0].into(),
            imap_port: p[1].parse().expect("port"),
            username: p[2].into(),
            password: p[3].into(),
            security: Some(crate::config::ServerSecurity {
                imap_starttls: false,
                imap_accept_invalid_certs: true,
                ..Default::default()
            }),
            ..sample_account()
        };
        let folder = format!("SentCopy-{}", std::process::id());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let cache = Cache::in_memory().unwrap();
            let mut session = Some(connect(&account).await.expect("connect"));
            let raw =
                b"From: me@example.test\r\nTo: ada@example.test\r\nSubject: Kept\r\n\r\nText\r\n";
            let id = cache
                .queue_sent_copy(1, raw, None, Some(&folder), "first try")
                .expect("kept");
            let mut use_envelope = true;
            let emit = |_: WorkerEvent| {};

            // The folder does not exist yet: the copy stays.
            file_pending_sent_copies(&cache, 1, &account, &mut session, &mut use_envelope, &emit)
                .await;
            let kept = cache.pending_sent_copies(1);
            assert_eq!(kept.len(), 1);
            assert_eq!((kept[0].id, kept[0].attempts), (id, 2));

            create_box(session.as_mut().unwrap(), &folder)
                .await
                .expect("create");
            file_pending_sent_copies(&cache, 1, &account, &mut session, &mut use_envelope, &emit)
                .await;
            assert!(cache.pending_sent_copies(1).is_empty());
            let sess = session.as_mut().expect("still connected");
            let status = box_status(sess, &folder, "(MESSAGES)")
                .await
                .expect("status");
            assert_eq!(status.exists, 1);
            delete_box(sess, &folder).await.expect("delete");
        });
    }
}
