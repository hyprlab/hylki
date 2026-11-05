//! Sending from a Tuta account. The helper does not take MIME: Tuta builds
//! the mail itself from its parts (sender, recipients, subject, HTML body
//! and attachments), so the message `build_email` produces is read back
//! into those parts (see [`crate::worker::helper::send`]). Tuta files the
//! Sent copy itself.
//!
//! Delivery is chosen as Tuta's own app chooses it: a message only to Tuta
//! addresses goes end-to-end encrypted ("confidential"), each recipient's
//! key loaded by the helper through Tuta's SDK, and a message that also
//! goes outside Tuta goes as ordinary mail (encrypted in transit only) to
//! everyone. A Tuta address the server has no key for is refused, never
//! sent unencrypted instead.
//!
//! A reply or a forward names the message it answers: the helper creates
//! the draft in that message's conversation, as Tuta's own app does, and
//! marks the original replied or forwarded once it went out.

use serde_json::Value;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};
use crate::models::FolderKind;
use crate::worker::compose::build::build_email;
use crate::worker::helper::send::{
    addresses_of, exact_forwards, message_parts, parse, send_outcome, send_parameters, sendable,
    thread_parameters, SendOutcome,
};
use crate::worker::helper::{HelperClient, HelperError, Object};
use crate::worker::outbox::{due_outbox_items, emit_outbox, uncertain_mark, uncertain_text};
use crate::worker::tuta::index::{run, tuta_load_folder, TutaState};
use crate::worker::tuta::ops::emit_folder;
use crate::worker::{offline_text, OutgoingMessage, WorkerEvent};

/// The helper's `save_draft` parameters for a built draft, which keeps its
/// Bcc in a header, as a draft is the user's own copy. It is not marked
/// confidential: that only asks Tuta's own app to send it to outside
/// addresses behind a password, which Hylki does not offer, and mail to
/// Tuta addresses goes end-to-end encrypted whatever the mark.
///
/// `forwards` is [`exact_forwards`] of the message's `forwards`.
pub(super) fn draft_parameters(raw: &[u8], forwards: &str) -> Result<Object, HelperError> {
    let message = parse(raw)?;
    let mut parameters = message_parts(&message, |_| addresses_of(message.bcc()))?;
    parameters.insert("confidential".into(), false.into());
    let recipients: Vec<String> = [message.to(), message.cc(), message.bcc()]
        .into_iter()
        .flat_map(addresses_of)
        .map(|(_, address)| address)
        .collect();
    parameters.extend(thread_parameters(&message, &recipients, forwards));
    Ok(parameters)
}

/// Send built bytes. Any reply with a result means the mail went. Its
/// fields are the server's ids, which nothing here needs, and whether it
/// went confidentially, with each recipient's key version and fingerprint
/// as the helper loaded them: Tuta verifies no key unless the user has
/// pinned it, and Hylki pins none yet, so those only go to the debug log.
pub(super) async fn tuta_send_raw(
    client: HelperClient,
    raw: Vec<u8>,
    envelope: Vec<String>,
    forwards: String,
) -> SendOutcome {
    let sent = run(move || {
        let parameters = send_parameters(&raw, &envelope, &forwards)?;
        client.request("send", parameters)
    })
    .await;
    match sent {
        Ok(reply) => {
            let keys = reply
                .get("recipients")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            match reply.get("confidential").and_then(Value::as_bool) {
                Some(true) => tracing::debug!("tuta: sent end-to-end encrypted to {keys} key(s)"),
                _ => tracing::debug!("tuta: sent as ordinary mail"),
            }
            SendOutcome::Sent
        }
        Err(e) => {
            if e.code != "send_uncertain" {
                tracing::warn!("tuta: send failed: {}", e.code);
            }
            send_outcome(&e)
        }
    }
}

/// Send a composed message. `forwards` is [`exact_forwards`] of its
/// `forwards`.
pub(super) async fn tuta_send_message(
    account: &AccountConfig,
    state: &TutaState,
    message: &OutgoingMessage,
    forwards: String,
) -> SendOutcome {
    // Tuta rebuilds the mail from its parts, which would leave an OpenPGP
    // signature or encryption behind: refused, not sent without it.
    if message.sign || message.encrypt {
        return SendOutcome::Refused(i18n(
            "OpenPGP signing and encryption aren't available for Tuta accounts. Turn them off to send.",
        ));
    }
    let email = match build_email(account, message) {
        Ok(email) => email,
        Err(e) => return SendOutcome::Failed(e.to_string()),
    };
    if !crate::desktop::power::reaches_internet() {
        return SendOutcome::Failed(offline_text());
    }
    let client = match state.client(account) {
        Ok(client) => client,
        Err(e) => return SendOutcome::Failed(e.message()),
    };
    let envelope = email
        .envelope()
        .to()
        .iter()
        .map(|a| a.to_string())
        .collect();
    tuta_send_raw(client, email.formatted(), envelope, forwards).await
}

/// List the Sent folder, where Tuta filed the copy, so it joins the
/// conversation it answers (#199).
pub(super) async fn tuta_list_sent(
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut TutaState,
    emit: &impl Fn(WorkerEvent),
) {
    let Some(path) = state.path_of(FolderKind::Sent) else {
        return;
    };
    let Some((folder_id, _)) = state.folders.get(&path).cloned() else {
        return;
    };
    if let Ok(messages) =
        tuta_load_folder(account, account_id, folder_id, &path, cache, state, emit).await
    {
        emit_folder(folder_id, messages, state, emit);
    }
}

/// The Outbox for a Tuta account: each due message goes as it was queued.
/// One that may have gone out is marked and kept, never retried.
pub(super) async fn tuta_flush_outbox(
    cache: Option<&Cache>,
    account_id: u32,
    account: &AccountConfig,
    id: Option<u32>,
    state: &mut TutaState,
    emit: &impl Fn(WorkerEvent),
) {
    let Some(c) = cache else { return };
    let items = sendable(due_outbox_items(c, account_id, id), id, emit);
    if items.is_empty() {
        emit_outbox(cache, account_id, emit);
        return;
    }
    let client = match state.client(account) {
        Ok(client) => client,
        Err(e) => {
            emit(WorkerEvent::error(e.message()));
            return;
        }
    };
    emit(WorkerEvent::Status(i18n("Sending…")));
    let mut sent_any = false;
    for item in items {
        let forwards = exact_forwards(cache, &item.forwards);
        match tuta_send_raw(
            client.clone(),
            item.raw.clone(),
            item.rcpts.clone(),
            forwards,
        )
        .await
        {
            SendOutcome::Sent => {
                sent_any = true;
                c.delete_outbox(item.id);
            }
            SendOutcome::Uncertain => {
                let why = uncertain_text();
                c.record_outbox_failure(item.id, &uncertain_mark(&why));
                emit(WorkerEvent::error(i18n_f(
                    "“{subject}”: {e}",
                    &[("subject", &item.subject), ("e", &why)],
                )));
                break;
            }
            SendOutcome::Failed(e) | SendOutcome::Refused(e) => {
                c.record_outbox_failure(item.id, &e);
                emit(WorkerEvent::error(i18n_f(
                    "Still could not send “{subject}”: {e}",
                    &[("subject", &item.subject), ("e", &e)],
                )));
                break;
            }
        }
    }
    emit(WorkerEvent::Status(String::new()));
    if sent_any {
        tuta_list_sent(account, account_id, cache, state, emit).await;
        emit(WorkerEvent::Sent);
    }
    emit_outbox(cache, account_id, emit);
}
