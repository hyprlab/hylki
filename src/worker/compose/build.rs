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
    let mut recipients = 0;
    for (name, addr) in parse_recipients(&msg.to) {
        builder = builder.to(mailbox(&name, &addr)?);
        recipients += 1;
    }
    for (name, addr) in parse_recipients(&msg.cc) {
        builder = builder.cc(mailbox(&name, &addr)?);
        recipients += 1;
    }
    for (name, addr) in parse_recipients(&msg.bcc) {
        builder = builder.bcc(mailbox(&name, &addr)?);
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
mod tests {
    use super::*;

    use crate::worker::render::attachments::extract_attachments;
    use crate::worker::test_support::{sample_account, sample_outgoing};
    use crate::worker::CalendarPart;

    /// `alt` arrives from outside on any quoted reply and lands in a header,
    /// so a quote, a backslash or a control character never reaches one, and
    /// a value that is no filename at all names nothing.
    #[test]
    fn an_alt_that_is_not_a_plain_filename_names_nothing() {
        let html = "<img src='data:image/png;base64,AAAA' alt='%s'>";
        for bad in ["a\"b", "a\\b", "a\nb", "", ".", ".."] {
            let doc = html.replace("%s", bad);
            let (_, imgs) = extract_data_images(&doc);
            assert_eq!(imgs.len(), 1, "{doc}");
            assert_eq!(imgs[0].name.as_deref(), None, "{doc}");
        }
        // A path is not rejected but reduced: only its last segment can name
        // the part, so a traversal cannot escape into the header.
        let (_, ok) = extract_data_images(
            "<img src='data:image/png;base64,AAAA' alt='../../etc/report.png'>",
        );
        assert_eq!(ok[0].name.as_deref(), Some("report.png"));
    }

    /// The lift leaves anything that is not a well-formed quoted base64 image
    /// data URI exactly as written.
    #[test]
    fn extract_data_images_leaves_odd_uris_alone() {
        let html = "<p>data:image/png is a scheme</p><img src=\"data:image/png;base65,zzz\">";
        let (out, imgs) = extract_data_images(html);
        assert_eq!(out, html);
        assert!(imgs.is_empty());

        // Single-quoted attributes lift too.
        let (out, imgs) = extract_data_images("<img src='data:image/jpeg;base64,AAAA'>");
        assert!(out.contains("src='cid:"), "{out}");
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].mime, "image/jpeg");
        assert_eq!(imgs[0].data, vec![0, 0, 0]);
    }

    /// A draft saved before any address is typed: the bytes build without a
    /// To header, and the send path still refuses the same message.
    #[test]
    fn recipient_less_draft_builds_but_send_does_not() {
        let account = sample_account();
        let mut msg = sample_outgoing();
        msg.body = "half-written".into();
        let raw = build_draft(&account, &msg)
            .expect("draft builds")
            .formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(!text.contains("\r\nTo:"), "no To header: {text}");
        assert!(text.contains("half-written"), "{text}");
        let err = build_email(&account, &msg)
            .expect_err("send needs a recipient")
            .to_string();
        assert!(err.contains("missing destination"), "{err}");
        // With a recipient the draft is built the ordinary way.
        msg.to = "Peer <peer@hylki.invalid>".into();
        let raw = build_draft(&account, &msg).expect("builds").formatted();
        assert!(String::from_utf8_lossy(&raw).contains("peer@hylki.invalid"));
    }

    /// OpenPGP sending (#133), end to end against a real gpg in a throwaway
    /// keyring: a signed message comes out as PGP/MIME multipart/signed whose
    /// signature the reader's own path verifies over lettre's exact bytes; an
    /// encrypted one as multipart/encrypted that decrypts back to the body.
    /// Skipped where gpg is missing.
    #[test]
    fn build_email_signs_and_encrypts_with_pgp_mime() {
        if !crate::mail::pgp::available() {
            eprintln!("gpg not installed; skipping");
            return;
        }
        let home = std::env::temp_dir().join(format!("hylki-gpg-send-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        // `Gpg::system()` (what build_email uses) honours this override.
        std::env::set_var("HYLKI_GNUPGHOME", &home);
        let gpg = crate::mail::pgp::Gpg::system();
        let mut account = sample_account();
        account.email = "sender@hylki.invalid".into();
        let fpr =
            crate::mail::pgp::generate_key(&gpg, "Sender", "sender@hylki.invalid", "never", "")
                .expect("own key");
        crate::mail::pgp::generate_key(&gpg, "Peer", "peer@hylki.invalid", "never", "")
            .expect("peer key");

        let mut msg = sample_outgoing();
        msg.to = "Peer <peer@hylki.invalid>".into();
        msg.body = "signed body text".into();
        msg.sign = true;
        let raw = build_email(&account, &msg).expect("builds").formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("multipart/signed"), "{text}");
        assert!(text.contains("micalg=\"pgp-sha"), "{text}");
        assert!(!has_bare_lf(&raw), "{text}");
        let u = crate::mail::pgp::unwrap_message(&raw, &gpg).expect("recognised");
        assert!(
            matches!(u.status.signature, crate::models::PgpSignature::Good { .. }),
            "{:?}",
            u.status
        );
        assert!(!u.status.encrypted);
        let inner = String::from_utf8_lossy(&u.inner.unwrap()).to_string();
        assert!(inner.contains("signed body text"), "{inner}");

        // With an attachment the signed entity is the whole mixed multipart.
        let path = home.join("note.txt");
        std::fs::write(&path, b"attached").unwrap();
        msg.attachments = vec![path.to_string_lossy().to_string()];
        let raw = build_email(&account, &msg).expect("builds").formatted();
        let u = crate::mail::pgp::unwrap_message(&raw, &gpg).expect("recognised");
        assert!(
            matches!(u.status.signature, crate::models::PgpSignature::Good { .. }),
            "{:?}",
            u.status
        );
        assert!(!extract_attachments(&u.inner.unwrap()).is_empty());

        // Encrypted and signed: readable by the peer and by the sender.
        msg.attachments.clear();
        msg.encrypt = true;
        msg.body = "secret body text".into();
        msg.subject = "Secret plans".into();
        msg.bcc = "Hidden <sender@hylki.invalid>".into();
        let raw = build_email(&account, &msg).expect("builds").formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("multipart/encrypted"), "{text}");
        assert!(
            !text.contains("secret body text"),
            "plaintext must not appear: {text}"
        );
        // Protected headers (#493): the subject is inside, "..." outside.
        assert!(
            !text.contains("Secret plans"),
            "the subject must not appear: {text}"
        );
        assert!(text.contains("Subject: ...\r\n"), "{text}");
        assert!(!has_bare_lf(&raw), "{text}");
        let u = crate::mail::pgp::unwrap_message(&raw, &gpg).expect("recognised");
        assert!(u.status.encrypted && u.status.decrypted, "{:?}", u.status);
        assert!(
            matches!(u.status.signature, crate::models::PgpSignature::Good { .. }),
            "{:?}",
            u.status
        );
        assert_eq!(u.status.subject.as_deref(), Some("Secret plans"));
        let inner = String::from_utf8_lossy(&u.inner.unwrap()).to_string();
        assert!(inner.contains("secret body text"), "{inner}");
        assert!(inner.contains("protected-headers=\"v1\""), "{inner}");
        assert!(
            inner.contains("peer@hylki.invalid"),
            "the To travels inside: {inner}"
        );
        assert!(
            !inner.contains("Hidden"),
            "Bcc must stay out of what every recipient reads: {inner}"
        );
        assert_eq!(
            extract_body(inner.as_bytes())
                .matches("secret body text")
                .count(),
            1
        );
        msg.bcc.clear();
        // An account that leaves the subject readable (#493) sends it outside.
        account.plain_subject = true;
        let raw = build_email(&account, &msg).expect("builds").formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("Subject: Secret plans\r\n"), "{text}");
        assert!(
            !text.contains("secret body text"),
            "plaintext must not appear: {text}"
        );
        let u = crate::mail::pgp::unwrap_message(&raw, &gpg).expect("recognised");
        assert!(
            u.status.decrypted && u.status.subject.is_none(),
            "{:?}",
            u.status
        );
        account.plain_subject = false;

        // A recipient without a key is refused by name; a From without a key too.
        msg.cc = "nobody@hylki.invalid".into();
        let err = build_email(&account, &msg)
            .expect_err("no key for nobody")
            .to_string();
        assert!(err.contains("nobody@hylki.invalid"), "{err}");
        msg.cc.clear();
        account.email = "keyless@hylki.invalid".into();
        let err = build_email(&account, &msg)
            .expect_err("no own key")
            .to_string();
        assert!(err.contains("keyless@hylki.invalid"), "{err}");
        // The account's chosen key overrides the address match.
        account.pgp_key = Some(fpr);
        build_email(&account, &msg).expect("chosen key signs");

        // A draft of the same message stays plain.
        let plain = OutgoingMessage {
            sign: false,
            encrypt: false,
            ..msg.clone()
        };
        let raw = build_email(&account, &plain).expect("builds").formatted();
        assert!(String::from_utf8_lossy(&raw).contains("secret body text"));

        std::env::remove_var("HYLKI_GNUPGHOME");
        let _ = std::process::Command::new("gpgconf")
            .env("GNUPGHOME", &home)
            .args(["--kill", "all"])
            .status();
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A reply with no In-Reply-To/References is a new conversation to every
    /// client that receives it — including Hylki's own threading.
    #[test]
    fn reply_to_header_reaches_the_wire() {
        let mut msg = sample_outgoing();
        msg.to = "someone@example.com".into();
        msg.reply_to = "list@example.org, Other <other@example.net>".into();
        let email = build_email(&sample_account(), &msg).expect("builds");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(raw.contains("Reply-To:"), "Reply-To must be present: {raw}");
        assert!(raw.contains("list@example.org"), "{raw}");
        assert!(raw.contains("other@example.net"), "{raw}");
    }

    #[test]
    fn a_reply_carries_its_threading_headers() {
        let mut msg = sample_outgoing();
        msg.to = "someone@example.com".into();
        msg.in_reply_to = "parent@example.com".into();
        msg.references = "root@example.com parent@example.com".into();
        let email = build_email(&sample_account(), &msg).expect("builds");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(
            raw.contains("In-Reply-To: <parent@example.com>"),
            "In-Reply-To must be present and bracketed: {raw}"
        );
        assert!(
            raw.contains("References: <root@example.com> <parent@example.com>"),
            "References must carry the whole chain, bracketed: {raw}"
        );
    }

    /// A message that starts a conversation must not carry empty headers.
    /// Without a Message-ID of our own the server assigns one on the way out, so
    /// the copy filed in Sent has none — and nothing that replies to it can be
    /// linked back to it. It must also be in the sender's domain, not this
    /// machine's hostname.
    #[test]
    fn outgoing_mail_carries_a_message_id_in_the_senders_domain() {
        let account = AccountConfig {
            email: "me@example.com".into(),
            ..sample_account()
        };
        let msg = OutgoingMessage {
            to: "you@example.org".into(),
            ..sample_outgoing()
        };
        let raw =
            String::from_utf8_lossy(&build_email(&account, &msg).expect("builds").formatted())
                .to_string();
        let line = raw
            .lines()
            .find(|l| l.starts_with("Message-ID:"))
            .unwrap_or_else(|| panic!("a Message-ID is set: {raw}"));
        assert!(
            line.contains("@example.com>"),
            "in the sender's domain: {line}"
        );
    }

    #[test]
    fn a_new_message_carries_no_threading_headers() {
        let msg = OutgoingMessage {
            to: "someone@example.com".into(),
            ..sample_outgoing()
        };
        let email = build_email(&sample_account(), &msg).expect("builds");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(!raw.contains("In-Reply-To"), "{raw}");
        assert!(!raw.contains("References"), "{raw}");
    }

    #[test]
    fn an_alias_replaces_from_but_not_the_transport() {
        let account = sample_account();
        let mut msg = sample_outgoing();
        msg.to = "someone@example.com".into();
        msg.from_alias = Some("Ann Work <ann@work.example>".into());
        let email = build_email(&account, &msg).expect("builds");
        let wire = String::from_utf8(email.formatted()).expect("utf8");
        let from_line = wire
            .lines()
            .find(|l| l.starts_with("From:"))
            .expect("From header");
        assert!(
            from_line.contains("ann@work.example"),
            "alias address on the wire: {from_line}"
        );
        assert!(
            !from_line.contains(&account.email),
            "account address replaced: {from_line}"
        );
        // The Message-ID follows the alias's domain, so replies thread back.
        let id_line = wire
            .lines()
            .find(|l| l.starts_with("Message-ID:"))
            .expect("id");
        assert!(
            id_line.contains("@work.example"),
            "id in alias domain: {id_line}"
        );
    }

    #[test]
    fn build_email_carries_attachments() {
        // Issue #15: "Send failed: Invalid input" when sending with attachments.
        // "Invalid input" is lettre's AddressError, raised only by parsing a
        // "Name <addr>" string — which 1.8.x did for From/To and 1.9.0 replaced
        // with mailboxes built from parts. This pins the attachment path itself,
        // including a name that would have needed quoting and a UTF-8 filename.
        let dir = std::env::temp_dir().join(format!("hylki-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("Meeting notes, final.pdf");
        // Binary, so the encoder has to base64 it — as it would a real PDF.
        std::fs::write(
            &file,
            [
                0x25, 0x50, 0x44, 0x46, 0x2d, 0x31, 0x2e, 0x34, 0x00, 0x80, 0xff,
            ],
        )
        .expect("write attachment");

        let account = AccountConfig {
            name: "Powers, Benny".into(),
            email: "benny@example.com".into(),
            ..sample_account()
        };
        let msg = OutgoingMessage {
            to: "Ada Lovelace <ada@example.com>".into(),
            subject: "Notes".into(),
            attachments: vec![file.to_string_lossy().to_string()],
            ..sample_outgoing()
        };

        let email = build_email(&account, &msg).expect("email builds with an attachment");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(raw.contains("multipart/mixed"), "{raw}");
        assert!(raw.contains("application/pdf"), "{raw}");
        // The filename has a comma and a space, so it has to be quoted or encoded.
        assert!(
            raw.contains("Meeting notes, final.pdf") || raw.contains("Meeting%20notes"),
            "{raw}"
        );
        // Base64 of the bytes written above.
        assert!(raw.contains("base64"), "{raw}");
        assert!(raw.contains("JVBERi0xLjQAgP8="), "{raw}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every line on the wire ends CRLF: JMAP servers refuse a message with a
    /// lone LF anywhere in it (#414).
    fn has_bare_lf(raw: &[u8]) -> bool {
        raw.iter()
            .enumerate()
            .any(|(i, &b)| b == b'\n' && (i == 0 || raw[i - 1] != b'\r'))
    }

    /// #412: plain text with nothing attached still says it is UTF-8 text,
    /// or a client that doesn't guess garbles every accent.
    #[test]
    fn plain_text_declares_its_charset() {
        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            body: "Plain text with öäü and 🙂\nsecond line".into(),
            ..sample_outgoing()
        };
        let raw = build_email(&sample_account(), &msg)
            .expect("email builds")
            .formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("MIME-Version: 1.0\r\n"), "{text}");
        assert!(
            text.contains("Content-Type: text/plain; charset=utf-8\r\n"),
            "{text}"
        );
        assert!(!has_bare_lf(&raw), "{text}");
        let parsed = mail_parser::MessageParser::default()
            .parse(&raw)
            .expect("parses");
        assert_eq!(
            parsed.body_text(0).as_deref().map(str::trim_end),
            Some("Plain text with öäü and 🙂\r\nsecond line")
        );
    }

    /// #414: a text file with Unix line endings goes out as base64, so its
    /// bytes arrive unchanged and no lone LF reaches the wire.
    #[test]
    fn a_unix_text_attachment_leaves_no_bare_newline() {
        let dir = std::env::temp_dir().join(format!("hylki-lf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.txt");
        std::fs::write(&file, b"one\ntwo\n").unwrap();
        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            attachments: vec![file.to_string_lossy().to_string()],
            ..sample_outgoing()
        };
        let raw = build_email(&sample_account(), &msg)
            .expect("email builds")
            .formatted();
        assert!(!has_bare_lf(&raw), "{}", String::from_utf8_lossy(&raw));
        let parsed = mail_parser::MessageParser::default()
            .parse(&raw)
            .expect("parses");
        assert_eq!(
            parsed.attachment(0).map(|a| a.contents()),
            Some(&b"one\ntwo\n"[..])
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A picture pasted into the composer rides the HTML as a data: URI; the
    /// wire message must carry it as an inline cid: part inside
    /// multipart/related — rendered in place, absent from the recipient's
    /// attachment list — with no data: URI left behind.
    #[test]
    fn pasted_images_go_out_as_inline_cid_parts() {
        let png = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x01, 0x02,
        ];
        let uri = format!(
            "data:image/png;base64,{}",
            crate::encoding::base64_encode(&png)
        );
        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            body: "see picture".into(),
            html: format!("<p>see</p><img src=\"{uri}\" style=\"max-width:100%\"><p>picture</p>"),
            ..sample_outgoing()
        };
        let email = build_email(&sample_account(), &msg).expect("email builds");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(raw.contains("multipart/related"), "{raw}");
        assert!(raw.contains("Content-ID"), "{raw}");
        assert!(raw.contains("cid:"), "{raw}");
        assert!(raw.contains("Content-Disposition: inline"), "{raw}");
        assert!(raw.contains("image/png"), "{raw}");
        assert!(!raw.contains("data:image"), "{raw}");
    }

    /// A picture dropped in from a file manager keeps the name it had; one
    /// pasted as raw bytes has none to keep.
    #[test]
    fn an_images_own_filename_reaches_the_inline_part() {
        let png = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00];
        let uri = format!(
            "data:image/png;base64,{}",
            crate::encoding::base64_encode(&png)
        );
        let raw_for = |img: String| {
            let msg = OutgoingMessage {
                to: "ada@example.com".into(),
                body: "see picture".into(),
                html: format!("<p>see</p>{img}"),
                ..sample_outgoing()
            };
            let email = build_email(&sample_account(), &msg).expect("email builds");
            String::from_utf8_lossy(&email.formatted()).to_string()
        };
        let named = raw_for(format!("<img src=\"{uri}\" alt=\"holiday snap.png\">"));
        assert!(named.contains("holiday snap.png"), "{named}");
        let anonymous = raw_for(format!("<img src=\"{uri}\">"));
        assert!(
            anonymous.contains("Content-Disposition: inline"),
            "{anonymous}"
        );
        assert!(!anonymous.contains("filename"), "{anonymous}");
    }

    /// HTML without an embedded image keeps the old, plain alternative shape.
    #[test]
    fn html_without_images_stays_a_plain_alternative() {
        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            body: "hi".into(),
            html: "<p>hi</p>".into(),
            ..sample_outgoing()
        };
        let email = build_email(&sample_account(), &msg).expect("email builds");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(raw.contains("multipart/alternative"), "{raw}");
        assert!(!raw.contains("multipart/related"), "{raw}");
    }

    #[test]
    fn build_email_reports_a_missing_attachment_by_name() {
        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            attachments: vec!["/nonexistent/report.pdf".into()],
            ..sample_outgoing()
        };
        let err = build_email(&sample_account(), &msg).expect_err("missing file fails");
        assert!(err.to_string().contains("report.pdf"), "{err}");
    }

    #[test]
    fn build_email_accepts_named_recipients() {
        let account = AccountConfig {
            name: "Jason Martin".into(),
            email: "me@example.com".into(),
            ..sample_account()
        };
        let msg = OutgoingMessage {
            to: "Alfonso Lizárraga <alfonso@example.com>, plain@example.com".into(),
            ..sample_outgoing()
        };
        let email = build_email(&account, &msg).expect("email builds");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(raw.contains("alfonso@example.com"), "{raw}");
        assert!(raw.contains("plain@example.com"), "{raw}");
    }

    /// An answer to an invitation goes out as plain text beside a
    /// `text/calendar` part that carries the iTIP method (#223) — a
    /// calendar that cannot find the method ignores the answer.
    #[test]
    fn an_invitation_answer_carries_its_calendar_part() {
        let account = sample_account();
        let msg = OutgoingMessage {
            to: "chair@example.org".into(),
            subject: "Accepted: Architecture sync".into(),
            body: "Jason has accepted this invitation.".into(),
            calendar: Some(CalendarPart {
                ics: "BEGIN:VCALENDAR\r\nMETHOD:REPLY\r\nEND:VCALENDAR\r\n".into(),
                method: "REPLY".into(),
            }),
            ..sample_outgoing()
        };
        let raw = build_message(&account, &msg, false)
            .expect("builds")
            .formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("multipart/alternative"), "{text}");
        assert!(
            text.contains("text/calendar; charset=utf-8; method=REPLY"),
            "the method rides on the part: {text}"
        );
        assert!(text.contains("METHOD:REPLY"), "{text}");
        assert!(
            text.contains("Jason has accepted this invitation."),
            "{text}"
        );
    }
}
