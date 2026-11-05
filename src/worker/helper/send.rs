//! The send request both helpers take (Tuta's shape, which Proton's helper
//! follows): the parts of a message `build_email` produced, read back out
//! of its MIME, since neither helper takes MIME, and its place in a
//! conversation. Each provider builds and encrypts the mail itself from
//! them and files the Sent copy.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use mail_parser::{MessageParser, MimeHeaders, PartType};
use serde_json::{json, Value};

use crate::cache::Cache;
use crate::i18n::i18n;
use crate::models::OutboxItem;
use crate::worker::helper::{HelperError, Object};
use crate::worker::WorkerEvent;

/// How a send ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::worker) enum SendOutcome {
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
pub(in crate::worker) fn send_outcome(error: &HelperError) -> SendOutcome {
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
        "recipient_not_found"
        | "secure_external_unsupported"
        | "confidential_send_unsupported"
        | "invalid_recipient"
        | "sender_unknown"
        | "body_too_large"
        | "invalid_request"
        | "invalid_thread"
        | "message_too_large"
        | "ordinary_delivery_requires_consent" => SendOutcome::Refused(error.message()),
        // The helper is missing, the sign-in needs renewing, the server was
        // out of reach, or it turned the draft down before the submit: none
        // sent anything, and a later try may well go.
        _ => SendOutcome::Failed(error.message()),
    }
}

pub(in crate::worker) fn addresses_of(
    address: Option<&mail_parser::Address<'_>>,
) -> Vec<(String, String)> {
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

/// The parts of a built message a helper takes, with `bcc` from wherever
/// the caller found it. To and Cc keep their names from the headers.
/// Threading goes separately (see [`thread_parameters`]).
pub(in crate::worker) fn message_parts(
    message: &mail_parser::Message<'_>,
    bcc: impl FnOnce(&[(String, String)]) -> Vec<(String, String)>,
) -> Result<Object, HelperError> {
    let (sender_name, sender) = addresses_of(message.from())
        .into_iter()
        .next()
        .ok_or_else(|| HelperError::new("sender_unknown"))?;
    let to = addresses_of(message.to());
    let cc = addresses_of(message.cc());
    let named: Vec<(String, String)> = to.iter().chain(&cc).cloned().collect();
    let bcc = bcc(&named);
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
        ("body_html", send_body(message).into()),
        ("attachments", Value::Array(send_attachments(message)?)),
    ] {
        parameters.insert(key.into(), value);
    }
    Ok(parameters)
}

/// Where a built message sits in a conversation, as the helper's optional
/// `in_reply_to`, `references` and `action`: nothing for new mail. A
/// forward names the message it forwards as its parent, as Tuta's and
/// Proton's own apps do. A reply is a reply-all when it goes to more than
/// one address: the composer keeps no other mark of it. `recipients` are
/// every address the message goes to.
pub(in crate::worker) fn thread_parameters(
    message: &mail_parser::Message<'_>,
    recipients: &[String],
    forwards: &str,
) -> Object {
    let in_reply_to = sendable_msgids(message.in_reply_to());
    let references = sendable_references(&sendable_msgids(message.references()));
    let forwarded = forwards.split_whitespace().find(|id| sendable_msgid(id));
    let parent = forwarded.or_else(|| in_reply_to.first().map(String::as_str));
    let mut parameters = Object::new();
    if parent.is_none() && references.is_empty() {
        return parameters;
    }
    if let Some(parent) = parent {
        parameters.insert("in_reply_to".into(), parent.into());
    }
    if !references.is_empty() {
        parameters.insert("references".into(), references.into());
    }
    let distinct: std::collections::HashSet<String> =
        recipients.iter().map(|a| a.to_lowercase()).collect();
    let action = match (forwarded, distinct.len()) {
        (Some(_), _) => "forward",
        (None, n) if n > 1 => "reply_all",
        (None, _) => "reply",
    };
    parameters.insert("action".into(), action.into());
    parameters
}

/// Whether a helper takes `id` as a Message-ID: it refuses the whole
/// request over one it does not, and a reply must not fail over a malformed
/// id some other client wrote into the thread.
fn sendable_msgid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 998
        && id
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b'<' && b != b'>')
}

/// The well-formed Message-IDs of an In-Reply-To or References header,
/// without their brackets.
fn sendable_msgids(header: &mail_parser::HeaderValue) -> Vec<String> {
    let ids: Vec<&str> = match header {
        mail_parser::HeaderValue::Text(id) => vec![id.as_ref()],
        mail_parser::HeaderValue::TextList(ids) => ids.iter().map(|id| id.as_ref()).collect(),
        _ => Vec::new(),
    };
    ids.into_iter()
        .map(|id| id.trim().trim_start_matches('<').trim_end_matches('>'))
        .filter(|id| sendable_msgid(id))
        .map(str::to_string)
        .collect()
}

/// The References chain as a helper takes it: the newest 100 ids within
/// 16 KiB, oldest first.
fn sendable_references(ids: &[String]) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut size = 0;
    for id in ids.iter().rev().take(100) {
        size += id.len() + 1;
        if size > 16 * 1024 {
            break;
        }
        kept.push(id);
    }
    kept.reverse();
    kept.join(" ")
}

/// The forwarded message's Message-ID as it arrived: the cache keeps it
/// lowercased for matching, and a helper finds the parent by its exact id
/// (as `restore_msgid_case` does for the threading headers).
pub(in crate::worker) fn exact_forwards(cache: Option<&Cache>, forwards: &str) -> String {
    cache.map_or_else(|| forwards.to_string(), |c| c.exact_msgids(forwards))
}

pub(in crate::worker) fn parse(raw: &[u8]) -> Result<mail_parser::Message<'_>, HelperError> {
    MessageParser::default()
        .parse(raw)
        .ok_or_else(|| HelperError::new("invalid_request"))
}

/// A helper's `send` parameters for a built message and its envelope.
/// Bcc is only in the envelope, so it is the envelope less To and Cc.
/// `forwards` is [`exact_forwards`] of the message's `forwards`.
pub(in crate::worker) fn send_parameters(
    raw: &[u8],
    envelope: &[String],
    forwards: &str,
) -> Result<Object, HelperError> {
    let message = parse(raw)?;
    let mut parameters = message_parts(&message, |named| {
        envelope
            .iter()
            .filter(|address| !named.iter().any(|(_, a)| a.eq_ignore_ascii_case(address)))
            .map(|address| (String::new(), address.clone()))
            .collect()
    })?;
    // Ordinary delivery is what each provider's own app does when it is
    // not given a choice: Tuta's goes confidential when every recipient is
    // on Tuta and ordinary for everyone otherwise, and Proton's encrypts for
    // every recipient it has a key for. It is confirmed here as those apps
    // confirm it by sending; a helper refuses ordinary delivery unconfirmed.
    parameters.insert("delivery".into(), "ordinary".into());
    parameters.insert("ordinary_confirmed".into(), true.into());
    parameters.extend(thread_parameters(&message, envelope, forwards));
    Ok(parameters)
}

/// The items a flush may send: never one whose last send may have gone
/// out. Asked for by id (Send Now), such an item is said to be held back.
pub(in crate::worker) fn sendable(
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
