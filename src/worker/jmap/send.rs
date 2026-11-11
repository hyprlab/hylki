//! Sending through EmailSubmission: identities, the Sent copy the server
//! files, the answered mark, and the Outbox.

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};
use crate::worker::compose::build::build_email;
use crate::worker::compose::editable::{queued_with_bcc, with_bcc};
use crate::worker::jmap::index::JmapState;
use crate::worker::jmap::messages::jmap_get_messages;
use crate::worker::jmap::ops::{jmap_destroy_all, jmap_patch_message, IMPORT_REFUSED};
use crate::worker::jmap::wire::{
    args, call, jmap_call, jmap_upload, JmapSession, CAP_CORE, CAP_MAIL, CAP_SUBMISSION,
};
use crate::worker::outbox::{due_outbox_items, emit_outbox, note_answered, raw_in_reply_to};
use crate::worker::{answered_copies, blocking, reply_copy_path, OutgoingMessage, WorkerEvent};

/// How a refused EmailSubmission starts its error. Every `notCreated` type
/// there (invalidRecipients, forbiddenToSend and the rest) is final, so the
/// message goes back to the composer rather than to the Outbox (#368).
pub(super) const SUBMISSION_REFUSED: &str = "the server refused to send it";

/// The account's sending identities, as the server lists them.
pub(super) fn jmap_identities(s: &JmapSession) -> Result<Vec<serde_json::Value>, String> {
    let responses = jmap_call(
        s,
        &[CAP_CORE, CAP_SUBMISSION],
        vec![call(
            0,
            "Identity/get",
            serde_json::json!({ "accountId": s.account, "ids": null }),
        )],
    )?;
    Ok(args(&responses, 0)["list"]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

/// Tell the app which identities the server has (#346), so the composer
/// offers them as From addresses.
pub(super) fn emit_jmap_identities(s: &JmapSession, emit: &impl Fn(WorkerEvent)) {
    match jmap_identities(s) {
        Ok(list) => emit(WorkerEvent::Identities(
            list.iter()
                .filter_map(|i| {
                    let email = i["email"].as_str()?.trim().to_string();
                    let name = i["name"].as_str().unwrap_or_default().trim().to_string();
                    (!email.is_empty()).then_some((name, email))
                })
                .collect(),
        )),
        Err(e) => tracing::info!(target: "hylki::jmap", "Identity/get: {e}"),
    }
}

/// Which identity sends from `from` (#346): the one with that address,
/// else one for anyone at its domain (`*@domain`, as Fastmail lists a
/// domain's catch-all), else the first the server offers.
pub(super) fn pick_identity<'a>(list: &'a [serde_json::Value], from: &str) -> Option<&'a str> {
    let domain = from.rsplit_once('@').map(|(_, d)| d).unwrap_or_default();
    let email = |i: &serde_json::Value| i["email"].as_str().unwrap_or_default().trim().to_string();
    list.iter()
        .find(|i| email(i).eq_ignore_ascii_case(from))
        .or_else(|| {
            list.iter().find(|i| {
                email(i)
                    .strip_prefix("*@")
                    .is_some_and(|d| !domain.is_empty() && d.eq_ignore_ascii_case(domain))
            })
        })
        .or_else(|| list.first())
        .and_then(|i| i["id"].as_str())
}

/// The identity to submit as (see [`pick_identity`]).
pub(super) fn jmap_identity_for(s: &JmapSession, from: &str) -> Result<String, String> {
    let list = jmap_identities(s)?;
    pick_identity(&list, from)
        .map(str::to_string)
        .ok_or_else(|| "the server offers no sending identity for this account".to_string())
}

/// Send raw RFC 5322 bytes: upload, import into the Sent folder, submit.
/// The server files nothing else, so the copy is the imported message; a
/// refused submission takes it back out.
pub(super) fn jmap_submit(
    s: &JmapSession,
    raw: &[u8],
    from: &str,
    rcpts: &[String],
    sent_mailbox: &str,
) -> Result<String, String> {
    let blob = jmap_upload(s, raw, "message/rfc822")?;
    let identity = jmap_identity_for(s, from)?;
    let responses = jmap_call(
        s,
        &[CAP_CORE, CAP_MAIL],
        vec![call(
            0,
            "Email/import",
            serde_json::json!({
                "accountId": s.account,
                "emails": { "m": {
                    "blobId": blob,
                    "mailboxIds": { sent_mailbox: true },
                    "keywords": { "$seen": true },
                }},
            }),
        )],
    )?;
    let imported = args(&responses, 0);
    let email_id = imported["created"]["m"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| {
            let why = &imported["notCreated"]["m"];
            format!(
                "{IMPORT_REFUSED}: {}",
                why["description"]
                    .as_str()
                    .or(why["type"].as_str())
                    .unwrap_or("unknown reason")
            )
        })?;
    let envelope = serde_json::json!({
        "mailFrom": { "email": from },
        "rcptTo": rcpts.iter().map(|r| serde_json::json!({ "email": r })).collect::<Vec<_>>(),
    });
    let submitted = jmap_call(
        s,
        &[CAP_CORE, CAP_MAIL, CAP_SUBMISSION],
        vec![call(
            0,
            "EmailSubmission/set",
            serde_json::json!({
                "accountId": s.account,
                "create": { "s": { "emailId": email_id, "identityId": identity, "envelope": envelope } },
            }),
        )],
    );
    let ok = match &submitted {
        Ok(r) => {
            let r = args(r, 0);
            if r["created"]["s"].is_object() {
                Ok(())
            } else {
                let why = &r["notCreated"]["s"];
                Err(format!(
                    "{SUBMISSION_REFUSED}: {}",
                    why["description"]
                        .as_str()
                        .or(why["type"].as_str())
                        .unwrap_or("unknown reason")
                ))
            }
        }
        Err(e) => Err(e.clone()),
    };
    if let Err(e) = ok {
        let _ = jmap_destroy_all(s, &[email_id]);
        return Err(e);
    }
    Ok(email_id)
}

/// The Sent folder a copy of outgoing mail is filed in (#199): the
/// account's choice, else the server's Sent mailbox.
fn jmap_sent_target(
    account: &AccountConfig,
    state: &JmapState,
    queued: Option<&str>,
) -> Option<(u32, String, String)> {
    let path = account
        .sent_copy_path
        .clone()
        .filter(|p| !p.is_empty() && state.folders.contains_key(p))
        .or_else(|| {
            queued
                .map(str::to_string)
                .filter(|p| state.folders.contains_key(p))
        })
        .or_else(|| state.sent.as_ref().map(|(_, p)| p.clone()))?;
    let (folder_id, mailbox_id) = state.folders.get(&path).cloned()?;
    Some((folder_id, path, mailbox_id))
}

/// Send over JMAP: the same raw MIME `build_email` produces, submitted by
/// the server, with the Sent copy indexed at once (#199).
pub(super) async fn jmap_send_message(
    s: &JmapSession,
    account_id: u32,
    account: &AccountConfig,
    message: &OutgoingMessage,
    sent_path: Option<&str>,
    cache: Option<&Cache>,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
) -> Result<(), String> {
    let email = build_email(account, message).map_err(|e| e.to_string())?;
    let envelope = email.envelope().clone();
    let from = envelope
        .from()
        .map(|f| f.to_string())
        .unwrap_or_else(|| account.email.clone());
    let rcpts: Vec<String> = envelope.to().iter().map(|a| a.to_string()).collect();
    // The copy keeps its Bcc (#476); the server removes it on delivery
    // (RFC 8621, 7.5).
    let raw = with_bcc(&email.formatted(), &message.bcc);
    let sent = jmap_sent_target(account, state, sent_path)
        .ok_or_else(|| "the account has no Sent folder to file the copy in".to_string())?;
    let reply = jmap_reply_target(account, account_id, state, cache, &message.in_reply_to);
    let sess = s.clone();
    let ((folder_id, path), email_id) =
        blocking(move || jmap_submit_filed(&sess, &raw, &from, &rcpts, reply, sent)).await?;
    jmap_index_sent_copy(
        s, account_id, folder_id, &path, &email_id, cache, state, emit,
    )
    .await;
    Ok(())
}

/// The mailbox beside a reply's original (#468), when the account files
/// replies there: see [`reply_copy_path`].
fn jmap_reply_target(
    account: &AccountConfig,
    account_id: u32,
    state: &JmapState,
    cache: Option<&Cache>,
    in_reply_to: &str,
) -> Option<(u32, String, String)> {
    // JMAP submits through its own server, never an alias's SMTP.
    let path = reply_copy_path(account, account_id, cache, in_reply_to, None)?;
    let (folder_id, mailbox_id) = state.folders.get(&path).cloned()?;
    Some((folder_id, path, mailbox_id))
}

/// [`jmap_submit`] with the copy filed in `reply` when there is one, and in
/// `sent` when there is not or that mailbox will not take it (one the user
/// may only read). The folder that took it, and the copy's id.
fn jmap_submit_filed(
    s: &JmapSession,
    raw: &[u8],
    from: &str,
    rcpts: &[String],
    reply: Option<(u32, String, String)>,
    sent: (u32, String, String),
) -> Result<((u32, String), String), String> {
    if let Some((folder_id, path, mailbox_id)) = reply {
        match jmap_submit(s, raw, from, rcpts, &mailbox_id) {
            Err(e) if e.starts_with(IMPORT_REFUSED) => {
                tracing::warn!("could not file the reply's copy in {path}, using Sent: {e}");
            }
            r => return r.map(|id| ((folder_id, path), id)),
        }
    }
    let (folder_id, path, mailbox_id) = sent;
    jmap_submit(s, raw, from, rcpts, &mailbox_id).map(|id| ((folder_id, path), id))
}

/// Put the sent copy in the cache and on screen before the app is told the
/// send is done (#199), so it can join the conversation it answers.
async fn jmap_index_sent_copy(
    s: &JmapSession,
    account_id: u32,
    folder_id: u32,
    path: &str,
    email_id: &str,
    cache: Option<&Cache>,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
) {
    let sess = s.clone();
    let ids = vec![email_id.to_string()];
    if let Ok(got) = blocking(move || jmap_get_messages(&sess, &ids, account_id, folder_id)).await {
        let mut messages = Vec::new();
        for (m, id, blob) in got {
            state.uids.insert(m.uid, (id, blob));
            messages.push(m);
        }
        if !messages.is_empty() {
            if let Some(c) = cache {
                c.upsert_messages(account_id, path, &messages);
            }
            emit(WorkerEvent::Messages {
                folder_id,
                messages,
            });
        }
    }
}

/// [`super::mark_answered`] for JMAP: `$answered` or `$forwarded` (#454).
pub(super) async fn jmap_mark_answered(
    s: &JmapSession,
    state: &mut JmapState,
    cache: Option<&Cache>,
    account_id: u32,
    in_reply_to: &str,
    forwards: &str,
    emit: &impl Fn(WorkerEvent),
) {
    for (path, uid, forwarded) in answered_copies(cache, account_id, in_reply_to, forwards) {
        let keyword = if forwarded {
            "keywords/$forwarded"
        } else {
            "keywords/$answered"
        };
        jmap_patch_message(s, state, &path, uid, serde_json::json!({ keyword: true })).await;
        note_answered(cache, account_id, path, uid, forwarded, emit);
    }
}

/// The Outbox retry loop for JMAP accounts: same queue and bookkeeping as
/// [`flush_outbox`], with `EmailSubmission` as the transport.
pub(super) async fn jmap_flush_outbox(
    s: &JmapSession,
    cache: Option<&Cache>,
    account_id: u32,
    account: &AccountConfig,
    id: Option<u32>,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
) {
    let Some(cache) = cache else { return };
    let items = due_outbox_items(cache, account_id, id);
    if items.is_empty() {
        return;
    }
    emit(WorkerEvent::Status(i18n("Sending…")));
    let mut sent_any = false;
    for item in items {
        let Some(sent) = jmap_sent_target(account, state, item.sent_path.as_deref()) else {
            cache.record_outbox_failure(item.id, "no Sent folder to file the copy in");
            continue;
        };
        let reply = jmap_reply_target(
            account,
            account_id,
            state,
            Some(cache),
            &raw_in_reply_to(&item.raw),
        );
        let sess = s.clone();
        let raw = queued_with_bcc(&item.raw, &item.rcpts);
        let from = item.from_addr.clone();
        let rcpts = item.rcpts.clone();
        match blocking(move || jmap_submit_filed(&sess, &raw, &from, &rcpts, reply, sent)).await {
            Ok(((folder_id, path), email_id)) => {
                sent_any = true;
                cache.delete_outbox(item.id);
                jmap_mark_answered(
                    s,
                    state,
                    Some(cache),
                    account_id,
                    &raw_in_reply_to(&item.raw),
                    &item.forwards,
                    emit,
                )
                .await;
                jmap_index_sent_copy(
                    s,
                    account_id,
                    folder_id,
                    &path,
                    &email_id,
                    Some(cache),
                    state,
                    emit,
                )
                .await;
            }
            Err(e) => {
                cache.record_outbox_failure(item.id, &e);
                emit(WorkerEvent::error(i18n_f(
                    "Still could not send “{subject}”: {e}",
                    &[("subject", &item.subject), ("e", &e)],
                )));
            }
        }
    }
    emit(WorkerEvent::Status(String::new()));
    if sent_any {
        emit(WorkerEvent::Sent);
    }
    emit_outbox(Some(cache), account_id, emit);
}
