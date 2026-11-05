//! Sending from a Tuta account. The helper does not take MIME: Tuta builds
//! the mail itself from its parts (sender, recipients, subject, HTML body
//! and attachments), so the message `build_email` produces is read back
//! into those parts. Tuta files the Sent copy itself.
//!
//! Delivery is ordinary mail (encrypted in transit, not end to end): the
//! helper cannot yet encrypt for Tuta recipients, so a message only to
//! Tuta addresses is refused rather than sent unencrypted, and a message
//! that also goes elsewhere goes as ordinary mail to everyone, as Tuta's
//! own app sends it.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use mail_parser::{MessageParser, MimeHeaders, PartType};
use serde_json::{json, Value};

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};
use crate::models::{FolderKind, OutboxItem};
use crate::worker::compose::build::build_email;
use crate::worker::helper::{HelperClient, HelperError, Object};
use crate::worker::outbox::{due_outbox_items, emit_outbox, uncertain_mark, uncertain_text};
use crate::worker::tuta::index::{run, tuta_load_folder, TutaState};
use crate::worker::tuta::ops::emit_folder;
use crate::worker::{offline_text, OutgoingMessage, WorkerEvent};

/// How a send ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SendOutcome {
    Sent,
    /// Nothing went out and trying again could go through: the Outbox
    /// keeps it. The text says why.
    Failed(String),
    /// Nothing went out, and the same message would be refused again: it
    /// goes back to the composer. The text says why.
    Refused(String),
    /// It may have gone out. Never sent again on its own.
    Uncertain,
}

/// What a helper failure on a send means for the message.
pub(super) fn send_outcome(error: &HelperError) -> SendOutcome {
    match error.code.as_str() {
        // The helper reached the submit and lost track of it. A helper that
        // died, overran, or answered something unreadable cannot say where
        // it got to either: any of those may have followed the submit.
        "send_uncertain"
        | "helper_failed"
        | "timeout"
        | "invalid_response"
        | "response_too_large"
        | "unsupported_version"
        | "request_failed" => SendOutcome::Uncertain,
        // Decided before anything was written to the server.
        "confidential_send_unsupported"
        | "invalid_recipient"
        | "sender_unknown"
        | "body_too_large"
        | "invalid_request"
        | "ordinary_delivery_requires_consent" => SendOutcome::Refused(error.message()),
        // The helper is missing, the sign-in needs renewing, the server was
        // out of reach, or it turned the draft down before the submit: none
        // sent anything, and a later try may well go.
        _ => SendOutcome::Failed(error.message()),
    }
}

fn addresses_of(address: Option<&mail_parser::Address<'_>>) -> Vec<(String, String)> {
    address
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    let email = a.address.as_deref().filter(|a| !a.is_empty())?;
                    Some((
                        a.name.as_deref().unwrap_or_default().to_string(),
                        email.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn address_rows(addresses: &[(String, String)]) -> Value {
    addresses
        .iter()
        .map(|(name, address)| json!({"name": name, "address": address}))
        .collect()
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// The HTML Tuta stores: the HTML part, else the text part escaped.
fn send_body(message: &mail_parser::Message<'_>) -> String {
    let html = message
        .html_part(0)
        .filter(|part| matches!(part.body, PartType::Html(_)));
    match html.or_else(|| message.text_part(0)).map(|part| &part.body) {
        Some(PartType::Html(html)) => html.to_string(),
        Some(PartType::Text(text)) => format!("<pre>{}</pre>", escape_html(text)),
        _ => String::new(),
    }
}

fn send_attachments(message: &mail_parser::Message<'_>) -> Result<Vec<Value>, HelperError> {
    message
        .attachments()
        .map(|part| {
            let data: &[u8] = match &part.body {
                PartType::Binary(data) | PartType::InlineBinary(data) => data,
                PartType::Text(text) | PartType::Html(text) => text.as_bytes(),
                // A forwarded message travels as its own bytes.
                PartType::Message(nested) => nested.raw_message(),
                PartType::Multipart(_) => return Err(HelperError::new("attachment_failed")),
            };
            let mime_type = part.content_type().map_or_else(
                || "text/plain".to_string(),
                |kind| match &kind.c_subtype {
                    Some(sub) => format!("{}/{}", kind.c_type, sub).to_ascii_lowercase(),
                    None => kind.c_type.to_ascii_lowercase(),
                },
            );
            let cid = part
                .content_id()
                .map(|cid| cid.trim().trim_matches(['<', '>']))
                .filter(|cid| !cid.is_empty());
            Ok(json!({
                "name": part.attachment_name().filter(|n| !n.is_empty()).unwrap_or("attachment"),
                "mime_type": mime_type,
                "cid": cid,
                "data": STANDARD.encode(data),
            }))
        })
        .collect()
}

/// The helper's `send` parameters for a built message and its envelope.
/// To and Cc keep their names from the headers; Bcc is only in the
/// envelope, so it is the envelope less To and Cc. Threading headers are
/// not passed: Tuta links conversations by its own ids.
pub(super) fn send_parameters(raw: &[u8], envelope: &[String]) -> Result<Object, HelperError> {
    let message = MessageParser::default()
        .parse(raw)
        .ok_or_else(|| HelperError::new("invalid_request"))?;
    let (sender_name, sender) = addresses_of(message.from())
        .into_iter()
        .next()
        .ok_or_else(|| HelperError::new("sender_unknown"))?;
    let to = addresses_of(message.to());
    let cc = addresses_of(message.cc());
    let named: Vec<String> = to
        .iter()
        .chain(&cc)
        .map(|(_, a)| a.to_lowercase())
        .collect();
    let bcc: Vec<(String, String)> = envelope
        .iter()
        .filter(|address| !named.contains(&address.to_lowercase()))
        .map(|address| (String::new(), address.clone()))
        .collect();
    let reply_tos = addresses_of(message.reply_to());
    let mut parameters = Object::new();
    for (key, value) in [
        ("sender", Value::from(sender)),
        ("sender_name", sender_name.into()),
        ("to", address_rows(&to)),
        ("cc", address_rows(&cc)),
        ("bcc", address_rows(&bcc)),
        ("reply_tos", address_rows(&reply_tos)),
        ("subject", message.subject().unwrap_or_default().into()),
        ("body_html", send_body(&message).into()),
        // The only delivery the helper can make, chosen here and nowhere
        // else; the helper refuses ordinary delivery unless it is confirmed.
        ("delivery", "ordinary".into()),
        ("ordinary_confirmed", true.into()),
        ("attachments", Value::Array(send_attachments(&message)?)),
    ] {
        parameters.insert(key.into(), value);
    }
    Ok(parameters)
}

/// Send built bytes. Any reply with a result means the mail went: its
/// fields are the server's own ids, which nothing here needs.
pub(super) async fn tuta_send_raw(
    client: HelperClient,
    raw: Vec<u8>,
    envelope: Vec<String>,
) -> SendOutcome {
    let sent = run(move || {
        let parameters = send_parameters(&raw, &envelope)?;
        client.request("send", parameters)
    })
    .await;
    match sent {
        Ok(_) => SendOutcome::Sent,
        Err(e) => {
            if e.code != "send_uncertain" {
                tracing::warn!("tuta: send failed: {}", e.code);
            }
            send_outcome(&e)
        }
    }
}

/// Send a composed message.
pub(super) async fn tuta_send_message(
    account: &AccountConfig,
    state: &TutaState,
    message: &OutgoingMessage,
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
    tuta_send_raw(client, email.formatted(), envelope).await
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

/// The items a flush may send: never one whose last send may have gone
/// out. Asked for by id (Send Now), such an item is said to be held back.
fn sendable(
    items: Vec<OutboxItem>,
    id: Option<u32>,
    emit: &impl Fn(WorkerEvent),
) -> Vec<OutboxItem> {
    let (held, sendable): (Vec<_>, Vec<_>) =
        items.into_iter().partition(OutboxItem::is_send_uncertain);
    if id.is_some() && !held.is_empty() {
        emit(WorkerEvent::error(i18n(
            "This message may already have been sent, so it is not sent again from the Outbox. Check Sent; to send it anyway, open it and send it from the composer.",
        )));
    }
    sendable
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
        match tuta_send_raw(client.clone(), item.raw.clone(), item.rcpts.clone()).await {
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
