//! Building the RFC 5322 bytes of an outgoing message with lettre: headers,
//! threading, the text/HTML alternatives, attachments, inline images and the
//! calendar part.

use lettre::Message as LettreMessage;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::worker::compose::pgp_mime::{pgp_wrap, protect_headers, PgpEntity, PROTECTED_SUBJECT};
use crate::worker::compose::recipients::mailbox;
use crate::worker::render::attachments::guess_mime;
use crate::worker::smtp::SmtpError;
use crate::worker::{extract_body, parse_recipients, OutgoingMessage};

/// The Message-ID a built draft carries, bare, to find it again among the
/// Drafts folder's messages once it is saved (#340).
pub(in crate::worker) fn draft_message_id(email: &LettreMessage) -> String {
    email
        .headers()
        .get_raw("Message-ID")
        .unwrap_or_default()
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_string()
}

/// Spell a message's threading ids (In-Reply-To, References) the way they
/// arrived before the message goes anywhere: the cache stores them lowercased
/// for matching, and a server that looks a parent up by its exact id — Proton
/// Bridge — would otherwise find nothing and file the copy without them.
pub(in crate::worker) fn restore_msgid_case(cache: Option<&Cache>, msg: &mut OutgoingMessage) {
    let Some(c) = cache else { return };
    msg.in_reply_to = c.exact_msgids(&msg.in_reply_to);
    msg.references = c.exact_msgids(&msg.references);
}

/// Build the RFC 822 email (headers + MIME body) from a composed message, for
/// sending: the SMTP envelope is derived from To/Cc/Bcc, so a message with no
/// recipient is refused here as well as in the composer.
pub(in crate::worker) fn build_email(
    account: &AccountConfig,
    msg: &OutgoingMessage,
) -> Result<LettreMessage, SmtpError> {
    build_message(account, msg, false)
}

/// The body the reader will show for `msg` once its copy is read back from
/// Sent: the bytes a send builds, through the same extraction, so the card a
/// sent reply gets at once is the card its copy draws later. Attachments
/// and OpenPGP are left out; neither changes the body shown.
pub(crate) fn sent_body_preview(account: &AccountConfig, msg: &OutgoingMessage) -> Option<String> {
    let mut msg = msg.clone();
    msg.attachments.clear();
    msg.sign = false;
    msg.encrypt = false;
    let email = build_email(account, &msg).ok()?;
    Some(extract_body(&email.formatted()))
}

/// Build the same bytes for the Drafts folder. A draft is often saved before
/// any address is typed (the composer saves on the way out whenever the body
/// was edited), and lettre refuses to build a message whose envelope has no
/// destination — "missing destination address, invalid envelope". The
/// envelope is only ever used by SMTP, and a draft is appended as raw bytes,
/// so a recipient-less draft gets an explicit envelope naming the sender,
/// which never reaches the formatted headers.
pub(in crate::worker) fn build_draft(
    account: &AccountConfig,
    msg: &OutgoingMessage,
) -> Result<LettreMessage, SmtpError> {
    build_message(account, msg, true)
}

fn build_message(
    account: &AccountConfig,
    msg: &OutgoingMessage,
    draft: bool,
) -> Result<LettreMessage, SmtpError> {
    // A send-as alias replaces the From header (and, if the alias has its own
    // SMTP, the transport — see `send_raw_smtp`); the Sent copy and everything
    // else about the send stays the account's (#34).
    let (from, from_addr) = match msg.from_alias.as_deref() {
        Some(alias) => match parse_recipients(alias).into_iter().next() {
            Some((name, addr)) => (mailbox(&name, &addr)?, addr),
            None => (
                mailbox(&account.name, &account.email)?,
                account.email.clone(),
            ),
        },
        None => (
            mailbox(&account.name, &account.email)?,
            account.email.clone(),
        ),
    };
    let sender_address = from.email.clone();
    let from_mailbox = from.clone();
    let mut builder = LettreMessage::builder().from(from);
    // A mail merge field (#447) is no address, but a draft or template
    // written for a merge keeps it: as the display name of an address that
    // reaches no one, which the composer turns back into the field.
    let recipient = |name: &str, addr: &str| match mailbox(name, addr) {
        Err(_) if draft && crate::mail::merge::is_field(&format!("{name} {addr}")) => {
            let written = crate::worker::format_recipient(name, addr);
            mailbox(&written, &crate::mail::merge::draft_field_address(&written))
        }
        built => built,
    };
    let mut recipients = 0;
    for (name, addr) in parse_recipients(&msg.to) {
        builder = builder.to(recipient(&name, &addr)?);
        recipients += 1;
    }
    for (name, addr) in parse_recipients(&msg.cc) {
        builder = builder.cc(recipient(&name, &addr)?);
        recipients += 1;
    }
    for (name, addr) in parse_recipients(&msg.bcc) {
        builder = builder.bcc(recipient(&name, &addr)?);
        recipients += 1;
    }
    // A draft is the user's own copy and has to come back with its Bcc
    // recipients (#350); a sent message goes without them, and only its
    // Sent copy gets them back, from [`with_bcc`] (#476).
    if draft {
        builder = builder.keep_bcc();
    }
    if draft && recipients == 0 {
        let envelope =
            lettre::address::Envelope::new(Some(sender_address.clone()), vec![sender_address])?;
        builder = builder.envelope(envelope);
    }
    // Our own Message-ID, in the account's own domain. Without one the SMTP
    // server assigns it on the way out — so the copy filed in Sent has no id at
    // all, and nothing that replies to it can ever be linked back. Setting it
    // here means the copy we keep and the copy that arrives share the same id.
    // Encrypted, the real subject travels inside unless the account says
    // otherwise (#493); see `protect_headers`.
    let protect = msg.encrypt && !account.plain_subject;
    let subject = if protect {
        PROTECTED_SUBJECT.to_string()
    } else {
        msg.subject.clone()
    };
    let mut builder = builder
        .subject(subject)
        .message_id(Some(new_message_id(&from_addr)));
    // Threading headers, re-wrapped in the angle brackets the wire format wants
    // (they are stored stripped). References carries the whole chain so a client
    // can place the reply even if it never saw the immediate parent.
    if !msg.in_reply_to.trim().is_empty() {
        builder = builder.header(lettre::message::header::InReplyTo::from(bracketed(
            &msg.in_reply_to,
        )));
    }
    // A read receipt asked for (#520) comes back to the address it left
    // from. A receipt never asks for one itself. A draft keeps the header
    // too, so the composer finds the request again when it is reopened.
    if msg.request_receipt && msg.receipt_report.is_none() {
        builder = builder.raw_header(super::report::request_header(&from_addr));
    }
    for header in super::importance::headers(msg.importance) {
        builder = builder.raw_header(header);
    }
    // Reply-To (#58): answers go where the sender asked, not to From.
    for (name, addr) in parse_recipients(&msg.reply_to) {
        if let Ok(mbox) = mailbox(&name, &addr) {
            builder = builder.reply_to(mbox);
        }
    }
    // Graph accounts thread by a synthetic "graph-conv:<id>" token (there is no
    // cheap way to read the real References header over the API) — that token is
    // internal glue and must never reach a wire header.
    let wire_references = msg
        .references
        .split_whitespace()
        .filter(|t| !t.starts_with("graph-conv:"))
        .collect::<Vec<_>>()
        .join(" ");
    if !wire_references.trim().is_empty() {
        builder = builder.header(lettre::message::header::References::from(bracketed(
            &wire_references,
        )));
    }
    // Autocrypt (Level 1): the sender's key on everything the account
    // sends, so the people it writes to can encrypt back. Not on a draft,
    // which goes to no one.
    if !draft {
        if let Some(header) = super::pgp_mime::autocrypt_header(account, &from_addr) {
            builder = builder.raw_header(header);
        }
    }

    use lettre::message::{header::ContentType, Attachment, MultiPart, SinglePart};
    // An answer to an invitation (#223): plain text for the person,
    // text/calendar for their calendar, as alternatives of one another —
    // the shape Evolution, Outlook and Google Calendar all send.
    if let Some(cal) = &msg.calendar {
        let ctype = format!("text/calendar; charset=utf-8; method={}", cal.method);
        let part = SinglePart::builder()
            .header(ContentType::parse(&ctype).unwrap_or(ContentType::TEXT_PLAIN))
            .body(cal.ics.clone());
        let alt = MultiPart::alternative()
            .singlepart(SinglePart::plain(msg.body.clone()))
            .singlepart(part);
        return Ok(builder.multipart(alt)?);
    }
    // A read receipt (#520): its words and its report, nothing else.
    if let Some(fields) = &msg.receipt_report {
        return Ok(builder.multipart(super::report::report_multipart(&msg.body, fields)?)?);
    }
    let has_html = !msg.html.trim().is_empty();
    // The message's readable half: alternative(plain, html) — wrapped in
    // related() carrying an inline part per image the composer embedded as a
    // `data:` URI (#113). Inline parts go out `Content-Disposition: inline`
    // with a Content-ID the HTML references by `cid:`, so they render in
    // place and stay out of the recipient's attachment list — the same
    // distinction the reader's own `extract_attachments` draws.
    let content = has_html.then(|| {
        let (html, images) = extract_data_images(&msg.html);
        let alt = MultiPart::alternative_plain_html(msg.body.clone(), html);
        if images.is_empty() {
            return alt;
        }
        let mut related = MultiPart::related().multipart(alt);
        for img in images {
            let ct = ContentType::parse(&img.mime).unwrap_or_else(|_| {
                ContentType::parse("application/octet-stream").expect("static content type")
            });
            let part = match img.name {
                Some(name) => Attachment::new_inline_with_name(img.cid, name),
                None => Attachment::new_inline(img.cid),
            };
            related = related.singlepart(part.body(file_body(img.data), ct));
        }
        related
    });
    // OpenPGP (#133): the body as one MIME entity, signed and/or encrypted
    // into the PGP/MIME shape the reader recognises.
    if msg.sign || msg.encrypt {
        let entity = if msg.attachments.is_empty() {
            match content {
                Some(content) => PgpEntity::Multi(content),
                None => PgpEntity::Single(SinglePart::plain(msg.body.clone())),
            }
        } else {
            PgpEntity::Multi(attachments_multipart(content, msg)?)
        };
        let entity = if protect {
            protect_headers(entity, msg, &from_mailbox)?
        } else {
            entity
        };
        let wrapped = pgp_wrap(account, msg, &from_addr, entity)?;
        return Ok(builder.multipart(wrapped)?);
    }
    let email = if msg.attachments.is_empty() {
        match content {
            Some(content) => builder.multipart(content)?,
            // A bare body() writes no MIME-Version and no charset, so a
            // client that doesn't guess UTF-8 garbles every accent (#412).
            None => builder.singlepart(SinglePart::plain(msg.body.clone()))?,
        }
    } else {
        builder.multipart(attachments_multipart(content, msg)?)?
    };
    Ok(email)
}

/// The text part (plain, or alternative plain+html) followed by the
/// attachments, as one mixed multipart.
fn attachments_multipart(
    content: Option<lettre::message::MultiPart>,
    msg: &OutgoingMessage,
) -> Result<lettre::message::MultiPart, SmtpError> {
    use lettre::message::{header::ContentType, Attachment, MultiPart, SinglePart};
    let mut multipart = match content {
        Some(content) => MultiPart::mixed().multipart(content),
        None => MultiPart::mixed().singlepart(SinglePart::plain(msg.body.clone())),
    };
    for path in &msg.attachments {
        // Name the file in the error: "No such file or directory" on its own
        // gives no clue which attachment went missing, and under Flatpak the
        // portal's /run/user/.../doc/ paths do expire.
        let bytes = std::fs::read(path)
            .map_err(|e| format!("could not read the attachment {path}: {e}"))?;
        let name = std::path::Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "attachment".to_string());
        let ct = ContentType::parse(&guess_mime(&name)).unwrap_or(ContentType::TEXT_PLAIN);
        multipart = multipart.singlepart(Attachment::new(name).body(file_body(bytes), ct));
    }
    Ok(multipart)
}

/// A file's bytes as base64, whatever they hold. lettre sends ASCII bytes as
/// 7bit without touching their line endings, so a text file with Unix ones
/// went out with bare LFs that strict servers refuse (#414); rewriting them
/// would change the file.
fn file_body(bytes: Vec<u8>) -> lettre::message::Body {
    use lettre::message::{header::ContentTransferEncoding, Body};
    Body::new_with_encoding(bytes, ContentTransferEncoding::Base64).expect("base64 takes any bytes")
}

/// One image the composer embedded, lifted out of the outgoing HTML: its
/// generated Content-ID, MIME type, and decoded bytes.
struct InlineImage {
    cid: String,
    mime: String,
    data: Vec<u8>,
    /// The file's own name, when the composer knew one — carried on the
    /// `<img>` as `alt`. Pasted image *bytes* never have one.
    name: Option<String>,
}

/// Lift every quoted `data:image/...;base64,...` URI out of outgoing HTML
/// into an [`InlineImage`], replacing the URI with a `cid:` reference (#113).
/// Anything that is not a well-formed base64 image data URI in an attribute
/// value is left exactly as written.
fn extract_data_images(html: &str) -> (String, Vec<InlineImage>) {
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut parts: Vec<InlineImage> = Vec::new();
    let mut i = 0usize;
    // Content-IDs are per-message; the timestamp keeps them globally unique
    // the way Message-IDs are, since some clients index by Content-ID alone.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    while let Some(off) = html[i..].find("data:image/") {
        let start = i + off;
        // Only splice an attribute value: the URI must sit right after a
        // quote and run to the matching one.
        let quote = match start.checked_sub(1).map(|p| bytes[p]) {
            Some(q @ (b'"' | b'\'')) => Some(q as char),
            _ => None,
        };
        let len = quote.and_then(|q| html[start..].find(q));
        let lifted = len.and_then(|len| {
            let uri = &html[start..start + len];
            let (meta, payload) = uri.split_once(',')?;
            let mime = meta.strip_prefix("data:")?.strip_suffix(";base64")?;
            let data = crate::encoding::base64_decode(payload)?;
            Some((len, mime.to_string(), data))
        });
        match lifted {
            Some((len, mime, data)) => {
                let cid = format!("{}.{}@hylki.inline", stamp, parts.len() + 1);
                let name = alt_of_enclosing_tag(html, start, start + len);
                out.push_str(&html[i..start]);
                out.push_str("cid:");
                out.push_str(&cid);
                parts.push(InlineImage {
                    cid,
                    mime,
                    data,
                    name,
                });
                i = start + len;
            }
            None => {
                out.push_str(&html[i..start + "data:image/".len()]);
                i = start + "data:image/".len();
            }
        }
    }
    out.push_str(&html[i..]);
    (out, parts)
}

/// The `alt` of the tag a lifted `data:` URI sat in — the filename the
/// composer put there for a picture that came from a file (see the editor's
/// insert script). The tag is delimited by the `<` before the URI and the `>`
/// after it; anything unterminated, or a value that is not a plain filename,
/// yields `None` and the part simply goes out unnamed as before.
fn alt_of_enclosing_tag(html: &str, uri_start: usize, uri_end: usize) -> Option<String> {
    let open = html[..uri_start].rfind('<')?;
    let close = uri_end + html[uri_end..].find('>')?;
    let tag = &html[open..close];
    let at = tag.find("alt=")?;
    let rest = &tag[at + "alt=".len()..];
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let value = &rest[quote.len_utf8()..];
    let value = &value[..value.find(quote)?];
    let unescaped = value
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    // Only ever a bare filename: `alt` is attacker-reachable through a quoted
    // reply, and it ends up in a Content-Disposition header.
    let name = std::path::Path::new(&unescaped)
        .file_name()?
        .to_str()?
        .to_string();
    let clean = !name.is_empty()
        && name.len() <= 200
        && !name
            .chars()
            .any(|c| c.is_control() || c == '"' || c == '\\');
    clean.then_some(name)
}

/// A Message-ID for outgoing mail: unique, and in the sender's own domain so it
/// looks like what it is rather than like this machine.
fn new_message_id(from: &str) -> String {
    let domain = from
        .rsplit('@')
        .next()
        .filter(|d| !d.is_empty())
        .unwrap_or("localhost");
    let unique = crate::rng::nonce(18).unwrap_or_else(|_| {
        // Entropy is only unavailable in extremis; the clock still separates
        // one message from the next.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos().to_string())
            .unwrap_or_default()
    });
    format!("<hylki-{unique}@{domain}>")
}

/// Wrap each stored (bare) message id back in angle brackets for a header value.
pub(in crate::worker) fn bracketed(ids: &str) -> String {
    ids.split_whitespace()
        .map(|id| {
            if id.starts_with('<') {
                id.to_string()
            } else {
                format!("<{id}>")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[path = "build_tests.rs"]
mod tests;
