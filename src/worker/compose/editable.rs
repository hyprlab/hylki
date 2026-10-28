//! Taking a queued or saved message apart again for editing, and putting
//! back the Bcc a sent copy or a retry needs.

use crate::encoding::escape_html;
use crate::worker::compose::recipients::mailbox;
use crate::worker::render::attachments::attachment_parts;
use crate::worker::render::body::inline_cid_images;
use crate::worker::{format_recipient, parse_recipients};

/// Render an address header back into the form the composer's fields use —
/// `Ada Lovelace <ada@example.com>, bob@example.com` — keeping display names so
/// editing a queued message doesn't reduce everyone to a bare address.
pub(in crate::worker) fn addr_list(header: Option<&mail_parser::Address>) -> String {
    header
        .map(|a| {
            a.iter()
                .filter_map(|addr| {
                    let email = addr.address()?.trim();
                    if email.is_empty() {
                        return None;
                    }
                    Some(format_recipient(addr.name().unwrap_or_default(), email))
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// A queued message taken apart for editing: the fields a composer needs, plus
/// its attachments as bytes.
pub(crate) struct EditableMessage {
    /// The From as written: "Name <address>", or the bare address.
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) cc: String,
    pub(crate) bcc: String,
    pub(crate) subject: String,
    /// Threading headers, stored the way an outgoing message carries them:
    /// without angle brackets, References space-separated.
    pub(crate) in_reply_to: String,
    pub(crate) references: String,
    /// The best body to edit: the HTML alternative when there is one, otherwise
    /// the plain text escaped into HTML (the composer edits HTML). Pictures
    /// pasted into the text come back as the `data:` URIs they were written as.
    pub(crate) body_html: String,
    /// The files, without the pictures already back in the text.
    pub(crate) attachments: Vec<crate::models::Attachment>,
    /// It asks for a read receipt (#520).
    pub(crate) receipt: bool,
}

/// Whoever the envelope names that none of the `headers` (address lists)
/// do: the Bcc recipients of bytes lettre stripped them from.
fn unnamed_rcpts(envelope_rcpts: &[String], headers: &[&str]) -> Vec<String> {
    let named: Vec<String> = headers
        .iter()
        .flat_map(|h| parse_recipients(h))
        .map(|(_, addr)| addr.to_lowercase())
        .collect();
    envelope_rcpts
        .iter()
        .filter(|addr| !named.contains(&addr.to_lowercase()))
        .cloned()
        .collect()
}

/// The bytes a sent copy is filed as (#476): the sent bytes with a `Bcc`
/// header naming `bcc`. lettre strips Bcc from what goes over SMTP, which is
/// right for the recipients and wrong for the sender's own copy, the only
/// place left that says who else was sent it. Header order is free, so it
/// goes first.
pub(in crate::worker) fn with_bcc(raw: &[u8], bcc: &str) -> Vec<u8> {
    let mailboxes: lettre::message::Mailboxes = parse_recipients(bcc)
        .into_iter()
        .filter_map(|(name, addr)| mailbox(&name, &addr).ok())
        .collect();
    if mailboxes.iter().next().is_none() {
        return raw.to_vec();
    }
    let mut headers = lettre::message::header::Headers::new();
    headers.set(lettre::message::header::Bcc::from(mailboxes));
    let mut out = headers.to_string().into_bytes();
    out.extend_from_slice(raw);
    out
}

/// [`with_bcc`] for a queued message, whose Bcc recipients survive only in
/// the stored envelope.
pub(in crate::worker) fn queued_with_bcc(raw: &[u8], envelope_rcpts: &[String]) -> Vec<u8> {
    let parsed = mail_parser::MessageParser::default().parse_headers(raw);
    let to = parsed
        .as_ref()
        .map(|p| addr_list(p.to()))
        .unwrap_or_default();
    let cc = parsed
        .as_ref()
        .map(|p| addr_list(p.cc()))
        .unwrap_or_default();
    let bcc = parsed
        .as_ref()
        .map(|p| addr_list(p.bcc()))
        .unwrap_or_default();
    with_bcc(
        raw,
        &unnamed_rcpts(envelope_rcpts, &[&to, &cc, &bcc]).join(", "),
    )
}

/// The Message-IDs of an `In-Reply-To` or `References` header, without
/// their angle brackets, space-separated.
fn msgid_list(hv: &mail_parser::HeaderValue) -> String {
    use mail_parser::HeaderValue;
    let strip = |t: &str| {
        t.trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .trim()
            .to_string()
    };
    match hv {
        HeaderValue::Text(t) => strip(t),
        HeaderValue::TextList(v) => v.iter().map(|t| strip(t)).collect::<Vec<_>>().join(" "),
        _ => String::new(),
    }
}

/// The body as the composer edits it: the HTML part with its `cid:`
/// pictures inlined (their Content-IDs noted in `embedded`), else the text
/// part escaped, a `<div>` per line.
fn editable_body(parsed: Option<&mail_parser::Message>, embedded: &mut Vec<String>) -> String {
    let html = parsed
        .and_then(|p| {
            let html = p.body_html(0)?;
            let mut budget = 16 * 1024 * 1024;
            Some(inline_cid_images(&html, &p.parts, &mut budget, embedded))
        })
        .unwrap_or_default();
    if html.trim().is_empty() {
        let text = parsed
            .and_then(|p| p.body_text(0).map(|b| b.to_string()))
            .unwrap_or_default();
        // The composer edits HTML, so plain text has to be escaped and its line
        // breaks preserved — otherwise the whole body collapses into one line.
        text.lines()
            .map(|line| format!("<div>{}</div>", escape_html(line)))
            .collect::<Vec<_>>()
            .join("")
    } else {
        html
    }
}

/// Take a queued message or a saved draft apart so it can be edited again.
///
/// The stored bytes are the source of truth, but they are not quite the whole
/// message: lettre strips `Bcc` from what goes on the wire, so a queued
/// message's Bcc recipients survive only in the stored envelope. Anything in
/// the envelope that isn't in To or Cc is therefore a Bcc recipient, and is
/// restored as one. A draft keeps its `Bcc` header and has no envelope.
pub(crate) fn editable_from_raw(raw: &[u8], envelope_rcpts: &[String]) -> EditableMessage {
    use mail_parser::{MessageParser, MimeHeaders};

    let parsed = MessageParser::default().parse(raw);
    let from = parsed
        .as_ref()
        .and_then(|p| p.from())
        .and_then(|a| a.first())
        .and_then(|a| {
            Some(format_recipient(
                a.name().unwrap_or_default(),
                a.address()?.trim(),
            ))
        })
        .unwrap_or_default();
    let to = parsed
        .as_ref()
        .map(|p| addr_list(p.to()))
        .unwrap_or_default();
    let cc = parsed
        .as_ref()
        .map(|p| addr_list(p.cc()))
        .unwrap_or_default();
    let subject = parsed
        .as_ref()
        .and_then(|p| p.subject())
        .unwrap_or_default()
        .to_string();
    let in_reply_to = parsed
        .as_ref()
        .map(|p| msgid_list(p.in_reply_to()))
        .unwrap_or_default();
    let references = parsed
        .as_ref()
        .map(|p| msgid_list(p.references()))
        .unwrap_or_default();

    let header_bcc = parsed
        .as_ref()
        .map(|p| addr_list(p.bcc()))
        .unwrap_or_default();
    let bcc = std::iter::once(header_bcc.clone())
        .filter(|b| !b.is_empty())
        .chain(unnamed_rcpts(envelope_rcpts, &[&to, &cc, &header_bcc]))
        .collect::<Vec<_>>()
        .join(", ");

    // Pictures pasted into the text went out as `cid:` parts (#113). They
    // come back into the text, and off the list of files, or the composer
    // would show a broken image and attach the picture a second time.
    let mut embedded: Vec<String> = Vec::new();
    let body_html = editable_body(parsed.as_ref(), &mut embedded);

    let attachments = parsed
        .as_ref()
        .map(|p| {
            attachment_parts(p, raw)
                .into_iter()
                .filter(|(id, _)| {
                    !p.parts
                        .get(*id)
                        .and_then(|part| part.content_id())
                        .is_some_and(|cid| embedded.iter().any(|e| e == cid))
                })
                .map(|(_, a)| a)
                .collect()
        })
        .unwrap_or_default();

    EditableMessage {
        from,
        to,
        cc,
        bcc,
        subject,
        in_reply_to,
        references,
        body_html,
        attachments,
        receipt: parsed
            .as_ref()
            .and_then(crate::mail::receipt::detect)
            .is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::AccountConfig;
    use crate::worker::compose::build::{build_draft, build_email};
    use crate::worker::test_support::{sample_account, sample_outgoing};
    use crate::worker::{summary_from_raw, OutgoingMessage};

    #[test]
    fn a_queued_message_can_be_taken_apart_for_editing() {
        // Round trip: compose → build → (failed send) → edit. What comes back has
        // to be what went in, including the Bcc that lettre strips from the bytes.
        let account = AccountConfig {
            name: "Me".into(),
            email: "me@example.com".into(),
            ..sample_account()
        };
        let msg = OutgoingMessage {
            to: "Ada Lovelace <ada@example.com>".into(),
            cc: "carol@example.com".into(),
            bcc: "hidden@example.com".into(),
            subject: "Quarterly numbers".into(),
            body: "First line\nSecond line".into(),
            ..sample_outgoing()
        };
        let email = build_email(&account, &msg).expect("builds");
        let rcpts: Vec<String> = email
            .envelope()
            .to()
            .iter()
            .map(|a| a.to_string())
            .collect();
        let raw = email.formatted();

        // lettre drops Bcc from the wire format, so the envelope is the only
        // record of it — which is exactly why it is stored alongside.
        assert!(!String::from_utf8_lossy(&raw).contains("hidden@example.com"));

        let editable = editable_from_raw(&raw, &rcpts);
        assert_eq!(editable.to, "Ada Lovelace <ada@example.com>");
        assert_eq!(editable.cc, "carol@example.com");
        assert_eq!(editable.bcc, "hidden@example.com");
        assert_eq!(editable.subject, "Quarterly numbers");
        // A plain-text body is escaped into HTML for the editor, keeping its lines.
        assert!(
            editable.body_html.contains("First line"),
            "{}",
            editable.body_html
        );
        assert!(
            editable.body_html.contains("Second line"),
            "{}",
            editable.body_html
        );
        assert!(editable.attachments.is_empty());
    }

    /// The copy filed in Sent names the Bcc recipients the wire copy leaves
    /// out (#476), whether it was sent at once or waited in the Outbox.
    #[test]
    fn a_sent_copy_keeps_its_bcc() {
        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            cc: "carol@example.com".into(),
            bcc: "Hidden Person <hidden@example.com>, other@example.com".into(),
            subject: "Quarterly numbers".into(),
            body: "Hello".into(),
            ..sample_outgoing()
        };
        let email = build_email(&sample_account(), &msg).expect("builds");
        let rcpts: Vec<String> = email
            .envelope()
            .to()
            .iter()
            .map(|a| a.to_string())
            .collect();
        let raw = email.formatted();
        assert!(!String::from_utf8_lossy(&raw).contains("hidden@example.com"));

        let bcc_of = |bytes: &[u8]| summary_from_raw(1, 1, 1, bytes).bcc;
        let copy = with_bcc(&raw, &msg.bcc);
        assert_eq!(bcc_of(&copy), "hidden@example.com, other@example.com");
        // Nothing else about the message changes: the bytes that went out
        // follow the one header put in front of them.
        assert!(copy.ends_with(&raw));
        let parsed = mail_parser::MessageParser::default()
            .parse(&copy)
            .expect("parses");
        assert_eq!(
            parsed.bcc().and_then(|a| a.first()).and_then(|a| a.name()),
            Some("Hidden Person")
        );
        assert_eq!(parsed.subject(), Some("Quarterly numbers"));

        let queued = queued_with_bcc(&raw, &rcpts);
        assert_eq!(bcc_of(&queued), "hidden@example.com, other@example.com");

        // No Bcc, no change.
        assert_eq!(with_bcc(&raw, ""), raw);
        let to_only: Vec<String> = vec!["ada@example.com".into(), "carol@example.com".into()];
        assert_eq!(queued_with_bcc(&raw, &to_only), raw);
    }

    #[test]
    fn editing_a_queued_message_keeps_its_attachments_and_html() {
        let dir = std::env::temp_dir().join(format!("hylki-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("report.pdf");
        std::fs::write(&file, [0x25, 0x50, 0x44, 0x46, 0x00, 0xff]).expect("write");

        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            subject: "With a file".into(),
            body: "plain fallback".into(),
            html: "<p>rich <b>body</b></p>".into(),
            attachments: vec![file.to_string_lossy().to_string()],
            ..sample_outgoing()
        };
        let email = build_email(&sample_account(), &msg).expect("builds");
        let rcpts: Vec<String> = email
            .envelope()
            .to()
            .iter()
            .map(|a| a.to_string())
            .collect();
        let editable = editable_from_raw(&email.formatted(), &rcpts);

        // The HTML alternative is what the composer edits, not the plain fallback.
        assert!(
            editable.body_html.contains("rich <b>body</b>"),
            "{}",
            editable.body_html
        );
        assert_eq!(editable.attachments.len(), 1);
        assert_eq!(editable.attachments[0].name, "report.pdf");
        assert_eq!(
            editable.attachments[0].data,
            [0x25, 0x50, 0x44, 0x46, 0x00, 0xff]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// #349, #350: a saved draft comes back as it was written: the alias it
    /// was from, its Bcc, its place in the thread, a picture in the text and
    /// a file beside it.
    #[test]
    fn a_saved_draft_comes_back_whole() {
        let dir = std::env::temp_dir().join(format!("hylki-draft-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("notes.txt");
        std::fs::write(&file, b"notes").expect("write");

        let msg = OutgoingMessage {
            from_alias: Some("Work <work@example.com>".into()),
            to: "ada@example.com".into(),
            bcc: "hidden@example.com".into(),
            subject: "Re: Plans".into(),
            html: "<p>see <img src=\"data:image/png;base64,iVBORw0KGgo=\"></p>".into(),
            attachments: vec![file.to_string_lossy().to_string()],
            in_reply_to: "Orig@Example.com".into(),
            references: "first@example.com Orig@Example.com".into(),
            ..sample_outgoing()
        };
        let raw = build_draft(&sample_account(), &msg)
            .expect("builds")
            .formatted();
        let editable = editable_from_raw(&raw, &[]);

        assert_eq!(editable.from, "Work <work@example.com>");
        assert_eq!(editable.bcc, "hidden@example.com");
        assert_eq!(editable.in_reply_to, "Orig@Example.com");
        assert_eq!(editable.references, "first@example.com Orig@Example.com");
        assert!(
            editable.body_html.contains("data:image/png;base64,"),
            "{}",
            editable.body_html
        );
        assert!(
            !editable.body_html.contains("cid:"),
            "{}",
            editable.body_html
        );
        assert_eq!(
            editable.attachments.len(),
            1,
            "the picture is in the text, not a file"
        );
        assert_eq!(editable.attachments[0].name, "notes.txt");
        std::fs::remove_dir_all(&dir).ok();
    }
}
