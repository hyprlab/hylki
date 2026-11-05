//! Sending from a Proton account. The helper takes the message's parts in
//! the shape Tuta's does (see [`crate::worker::helper::send`]) plus its
//! Message-ID and its place in a conversation, and builds, encrypts and
//! submits the mail itself: a Proton recipient always gets it end-to-end
//! encrypted, an outside one with a known key gets PGP, and any other
//! outside one gets it over TLS. Proton files the Sent copy itself.
//!
//! The request asks for ordinary delivery, confirmed: that only means a
//! recipient without a key is not refused. Nobody gets less encryption
//! than Proton's own apps would give them.
//!
//! A reply or a forward names the message it answers: Proton's draft API
//! takes no headers of its own, so the helper finds that message and links
//! the draft to it, and the server then writes In-Reply-To and References
//! and marks the original answered or forwarded.

use mail_parser::MessageParser;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};
use crate::models::FolderKind;
use crate::worker::compose::build::build_email;
use crate::worker::helper::send::{
    exact_forwards, send_outcome, send_parameters, sendable, SendOutcome,
};
use crate::worker::helper::{HelperClient, HelperError, Object};
use crate::worker::outbox::{due_outbox_items, emit_outbox, uncertain_mark, uncertain_text};
use crate::worker::proton::index::{emit_folder, proton_load_folder, run, ProtonState};
use crate::worker::{offline_text, OutgoingMessage, WorkerEvent};

/// The helper's `send` parameters: the shared parts and threading (see
/// [`send_parameters`]), and the Message-ID the message was built with, so
/// the copy in Sent and every recipient's carry the one Hylki threads by.
/// `forwards` is [`exact_forwards`] of the message's `forwards`.
pub(super) fn proton_send_parameters(
    raw: &[u8],
    envelope: &[String],
    forwards: &str,
) -> Result<Object, HelperError> {
    let mut parameters = send_parameters(raw, envelope, forwards)?;
    let parsed = MessageParser::default().parse_headers(raw);
    let message_id = parsed
        .as_ref()
        .and_then(|m| m.message_id())
        .filter(|id| !id.is_empty());
    if let Some(id) = message_id {
        parameters.insert("message_id".into(), id.into());
    }
    Ok(parameters)
}

/// Send built bytes. Any reply with a result means the mail went.
pub(super) async fn proton_send_raw(
    client: HelperClient,
    raw: Vec<u8>,
    envelope: Vec<String>,
    forwards: String,
) -> SendOutcome {
    let sent = run(move || {
        let parameters = proton_send_parameters(&raw, &envelope, &forwards)?;
        client.request("send", parameters)
    })
    .await;
    match sent {
        Ok(_) => SendOutcome::Sent,
        Err(e) => {
            if e.code != "send_uncertain" {
                tracing::warn!("proton: send failed: {}", e.code);
            }
            send_outcome(&e)
        }
    }
}

/// Send a composed message. `forwards` is [`exact_forwards`] of its
/// `forwards`.
pub(super) async fn proton_send_message(
    account: &AccountConfig,
    state: &ProtonState,
    message: &OutgoingMessage,
    forwards: String,
) -> SendOutcome {
    // Proton builds and encrypts the mail itself from its parts, which
    // would leave Hylki's own OpenPGP signature or encryption behind:
    // refused, not sent without it.
    if message.sign || message.encrypt {
        return SendOutcome::Refused(i18n(
            "Hylki's OpenPGP signing and encryption aren't available for Proton accounts: Proton encrypts the message itself. Turn them off to send.",
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
    proton_send_raw(client, email.formatted(), envelope, forwards).await
}

/// List the Sent folder, where Proton filed the copy, so it joins the
/// conversation it answers (#199).
pub(super) async fn proton_list_sent(
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut ProtonState,
    emit: &impl Fn(WorkerEvent),
) {
    let Some(path) = state.path_of(FolderKind::Sent) else {
        return;
    };
    let Some((folder_id, _)) = state.folders.get(&path).cloned() else {
        return;
    };
    if let Ok(messages) =
        proton_load_folder(account, account_id, folder_id, &path, cache, state, emit).await
    {
        emit_folder(folder_id, messages, state, emit);
    }
}

/// The Outbox for a Proton account: each due message goes as it was
/// queued. One that may have gone out is marked and kept, never retried.
pub(super) async fn proton_flush_outbox(
    cache: Option<&Cache>,
    account_id: u32,
    account: &AccountConfig,
    id: Option<u32>,
    state: &mut ProtonState,
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
        match proton_send_raw(
            client.clone(),
            item.raw.clone(),
            item.rcpts.clone(),
            exact_forwards(cache, &item.forwards),
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
        proton_list_sent(account, account_id, cache, state, emit).await;
        emit(WorkerEvent::Sent);
    }
    emit_outbox(cache, account_id, emit);
}
