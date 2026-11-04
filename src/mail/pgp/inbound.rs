//! Recognising signed and encrypted mail and unwrapping it for the reader.

use super::keys::key_display;
use super::status::{parse_status, signature_verdict};
use super::{available, run, Gpg};
use crate::i18n::{i18n, i18n_f};
use crate::models::{PgpSignature, PgpStatus};

/// The OpenPGP structure a raw message carries, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Shape {
    /// PGP/MIME `multipart/encrypted`: the armoured ciphertext (part two).
    Encrypted { armored: Vec<u8> },
    /// PGP/MIME `multipart/signed`: the signed entity's exact bytes and the
    /// detached signature.
    Signed { data: Vec<u8>, signature: Vec<u8> },
    /// An armoured message inside a text body.
    InlineEncrypted { armored: Vec<u8> },
    /// A clear-signed block inside a text body.
    InlineSigned { text: Vec<u8> },
}

/// Whether `raw` is encrypted mail. It says so without touching GnuPG, so
/// no passphrase prompt comes of asking.
pub(crate) fn is_encrypted(raw: &[u8]) -> bool {
    detect(raw).is_some_and(|shape| shape.is_encrypted())
}

impl Shape {
    pub(super) fn is_encrypted(&self) -> bool {
        matches!(
            self,
            Shape::Encrypted { .. } | Shape::InlineEncrypted { .. }
        )
    }
}

/// Find the OpenPGP structure in a raw message.
pub(crate) fn detect(raw: &[u8]) -> Option<Shape> {
    use mail_parser::{MessageParser, MimeHeaders, PartType};
    let parsed = MessageParser::default().parse(raw)?;
    let root = parsed.root_part();
    let ctype = parsed.content_type();
    let (ct_type, ct_sub, protocol, boundary) = match ctype {
        Some(ct) => (
            ct.ctype().to_ascii_lowercase(),
            ct.subtype()
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default(),
            ct.attribute("protocol")
                .map(|p| p.to_ascii_lowercase())
                .unwrap_or_default(),
            ct.attribute("boundary").map(str::to_string),
        ),
        None => (String::new(), String::new(), String::new(), None),
    };
    if ct_type == "multipart" {
        let PartType::Multipart(children) = &root.body else {
            return None;
        };
        if ct_sub == "encrypted" && protocol == "application/pgp-encrypted" {
            // Part one is the version stub; part two carries the ciphertext.
            // Take the first part that looks like an armoured message rather
            // than trusting positions.
            for id in children {
                let part = parsed.part(*id)?;
                let text = part_text(part);
                if text.contains("-----BEGIN PGP MESSAGE-----") {
                    return Some(Shape::Encrypted {
                        armored: text.into_bytes(),
                    });
                }
            }
            return None;
        }
        if ct_sub == "signed" && protocol == "application/pgp-signature" {
            let (first, second) = (children.first()?, children.get(1)?);
            let first = parsed.part(*first)?;
            let sig = parsed.part(*second)?;
            let boundary = boundary?;
            let data = signed_bytes(raw, first.offset_header, &boundary)?;
            let signature = part_text(sig).into_bytes();
            if !signature.contains_str("-----BEGIN PGP SIGNATURE-----") {
                return None;
            }
            return Some(Shape::Signed { data, signature });
        }
    }
    // Inline forms: the first text body.
    let text = parsed.body_text(0)?;
    if text.contains("-----BEGIN PGP MESSAGE-----") && text.contains("-----END PGP MESSAGE-----") {
        return Some(Shape::InlineEncrypted {
            armored: armored_block(&text, "MESSAGE").into_bytes(),
        });
    }
    if text.contains("-----BEGIN PGP SIGNED MESSAGE-----")
        && text.contains("-----END PGP SIGNATURE-----")
    {
        return Some(Shape::InlineSigned {
            text: text.into_owned().into_bytes(),
        });
    }
    None
}

trait ContainsStr {
    fn contains_str(&self, needle: &str) -> bool;
}

impl ContainsStr for Vec<u8> {
    fn contains_str(&self, needle: &str) -> bool {
        self.windows(needle.len()).any(|w| w == needle.as_bytes())
    }
}

/// A part's decoded text (an armoured block is text whatever it is labelled).
fn part_text(part: &mail_parser::MessagePart) -> String {
    use mail_parser::PartType;
    match &part.body {
        PartType::Text(t) | PartType::Html(t) => t.to_string(),
        PartType::Binary(b) | PartType::InlineBinary(b) => String::from_utf8_lossy(b).into_owned(),
        _ => String::new(),
    }
}

/// The armoured block between the BEGIN and END lines, inclusive.
pub(super) fn armored_block(text: &str, kind: &str) -> String {
    let begin = format!("-----BEGIN PGP {kind}-----");
    let end = format!("-----END PGP {kind}-----");
    let (Some(b), Some(e)) = (text.find(&begin), text.find(&end)) else {
        return text.to_string();
    };
    text[b..e + end.len()].to_string()
}

/// The exact bytes the signature covers (RFC 3156 §5): the signed part from
/// its first header up to, but not including, the CRLF that precedes the
/// boundary delimiter. Found on the raw bytes, not through the parser, so
/// nothing is re-encoded on the way.
pub(super) fn signed_bytes(raw: &[u8], start: usize, boundary: &str) -> Option<Vec<u8>> {
    if start >= raw.len() {
        return None;
    }
    let crlf = format!("\r\n--{boundary}");
    let lf = format!("\n--{boundary}");
    let rest = &raw[start..];
    let end = find(rest, crlf.as_bytes()).or_else(|| find(rest, lf.as_bytes()))?;
    Some(rest[..end].to_vec())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The outcome of handling a message: what to render, and the verdict.
#[derive(Debug, Clone)]
pub(crate) struct Unwrapped {
    /// The MIME entity to render and take attachments from: the decrypted
    /// message, or the signed part. `None` when there is nothing readable
    /// (undecryptable), in which case the reader shows the verdict instead.
    pub(crate) inner: Option<Vec<u8>>,
    pub(crate) status: PgpStatus,
}

/// Decrypt and/or verify a message, whatever form it takes. `None` when the
/// message carries no OpenPGP structure at all.
pub(crate) fn unwrap_message(raw: &[u8], gpg: &Gpg) -> Option<Unwrapped> {
    let shape = detect(raw)?;
    let mut unwrapped = unwrap_shape(shape, raw, gpg);
    unwrapped.status.sender_addr = from_address(raw);
    unwrapped.status.autocrypt = autocrypt_key(raw).map(|k| crate::encoding::base64_encode(&k));
    Some(unwrapped)
}

/// The From address of a raw message, lowercased ("" when it has none).
fn from_address(raw: &[u8]) -> String {
    use mail_parser::MessageParser;
    MessageParser::default()
        .parse(raw)
        .and_then(|m| {
            m.from()
                .and_then(|f| f.first())
                .and_then(|a| a.address().map(|s| s.to_ascii_lowercase()))
        })
        .unwrap_or_default()
}

fn unwrap_shape(shape: Shape, _raw: &[u8], gpg: &Gpg) -> Unwrapped {
    if !available() {
        let status = PgpStatus {
            encrypted: shape.is_encrypted(),
            decrypted: false,
            signature: PgpSignature::None,
            notes: vec![i18n(
                "GnuPG (gpg) is not installed, so this message cannot be read.",
            )],
            ..Default::default()
        };
        // A signed message is still readable without gpg.
        let inner = match &shape {
            Shape::Signed { data, .. } => Some(data.clone()),
            Shape::InlineSigned { text } => Some(text_entity(text)),
            _ => None,
        };
        return Unwrapped { inner, status };
    }
    match shape {
        Shape::Encrypted { armored } | Shape::InlineEncrypted { armored } => {
            let out = run(gpg, &["--decrypt"], &armored, None);
            let outcome = parse_status(&out.status_lines);
            let decrypted = outcome.decryption_ok && !out.stdout.is_empty();
            let mut notes = Vec::new();
            let signature = signature_verdict(&outcome, &mut notes);
            if decrypted {
                notes.insert(0, i18n("Decrypted with your key."));
            } else if let Some(k) = outcome.no_seckey.first() {
                notes.insert(
                    0,
                    i18n_f(
                        "Encrypted to key {id}, which is not in your keyring.",
                        &[("id", &key_display(k))],
                    ),
                );
            } else if outcome.nodata {
                notes.insert(
                    0,
                    i18n("The encrypted block is damaged or not OpenPGP data."),
                );
            } else if let Some(d) = out.detail.clone() {
                notes.insert(0, d);
            }
            let inner = decrypted.then(|| {
                // PGP/MIME wraps a full MIME entity; inline PGP is bare text.
                if looks_like_mime(&out.stdout) {
                    out.stdout.clone()
                } else {
                    text_entity(&out.stdout)
                }
            });
            let subject = inner.as_deref().and_then(protected_subject);
            Unwrapped {
                inner,
                status: PgpStatus {
                    encrypted: true,
                    decrypted,
                    signature,
                    notes,
                    subject,
                    ..Default::default()
                },
            }
        }
        Shape::Signed { data, signature } => {
            let out = run(gpg, &["--verify"], &data, Some(&signature));
            let outcome = parse_status(&out.status_lines);
            let mut notes = Vec::new();
            let signature = signature_verdict(&outcome, &mut notes);
            if matches!(signature, PgpSignature::None) {
                if let Some(d) = out.detail.clone() {
                    notes.push(d);
                }
            }
            Unwrapped {
                inner: Some(data),
                status: PgpStatus {
                    encrypted: false,
                    decrypted: false,
                    signature,
                    notes,
                    ..Default::default()
                },
            }
        }
        Shape::InlineSigned { text } => {
            // `--decrypt` on a clear-signed block verifies it and prints the
            // signed text, dearmoured.
            let out = run(gpg, &["--decrypt"], &text, None);
            let outcome = parse_status(&out.status_lines);
            let mut notes = Vec::new();
            let signature = signature_verdict(&outcome, &mut notes);
            let inner = if out.stdout.is_empty() {
                text_entity(&text)
            } else {
                text_entity(&out.stdout)
            };
            Unwrapped {
                inner: Some(inner),
                status: PgpStatus {
                    encrypted: false,
                    decrypted: false,
                    signature,
                    notes,
                    ..Default::default()
                },
            }
        }
    }
}

/// The Subject on a decrypted entity (#493). Thunderbird, Enigmail,
/// Mailvelope and RFC 9788 put the real one there and leave a placeholder
/// outside, so this is what the reader shows. A plain entity has no Subject
/// of its own, so its presence is the signal; the `protected-headers` or
/// `hp` flag is not required, as not every sender sets it.
pub(super) fn protected_subject(entity: &[u8]) -> Option<String> {
    use mail_parser::MessageParser;
    let parsed = MessageParser::default().parse(entity)?;
    let subject = parsed.subject()?.trim();
    (!subject.is_empty()).then(|| subject.to_string())
}

/// The legacy display part of a message with protected headers: a first
/// text part that repeats the Subject for clients that do not read it from
/// the headers inside. This one does, so the part is hidden, never shown in
/// the body or listed as a file. `None` unless the entity is flagged as
/// carrying protected headers, so ordinary mail is never touched.
pub(crate) fn legacy_display_part(parsed: &mail_parser::Message) -> Option<usize> {
    use mail_parser::MimeHeaders;
    let root = parsed.root_part();
    let ct = root.content_type()?;
    if !(ct.has_attribute("protected-headers") || ct.has_attribute("hp")) {
        return None;
    }
    let first = *root.sub_parts()?.first()?;
    let ct = parsed.part(first)?.content_type()?;
    let legacy = ct.ctype().eq_ignore_ascii_case("text")
        && (ct
            .subtype()
            .is_some_and(|s| s.eq_ignore_ascii_case("rfc822-headers"))
            || ct.has_attribute("protected-headers")
            || ct
                .attribute("hp-legacy-display")
                .is_some_and(|v| v.trim() == "1"));
    legacy.then_some(first)
}

/// Whether decrypted bytes are a MIME entity (headers first) rather than
/// bare text.
pub(super) fn looks_like_mime(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
    let first = head.lines().next().unwrap_or("");
    let lower = first.to_ascii_lowercase();
    lower.starts_with("content-type:")
        || lower.starts_with("content-transfer-encoding:")
        || lower.starts_with("content-disposition:")
        || lower.starts_with("mime-version:")
        || protected_header_block(&String::from_utf8_lossy(
            &bytes[..bytes.len().min(16 * 1024)],
        ))
}

/// Whether text opens with a header block that has a Content-Type in it:
/// an entity with protected headers may put its Subject or From first.
pub(super) fn protected_header_block(head: &str) -> bool {
    let mut content_type = false;
    for line in head.lines() {
        if line.trim().is_empty() {
            return content_type;
        }
        if line.starts_with([' ', '\t']) {
            continue;
        }
        let Some((name, _)) = line.split_once(':') else {
            return false;
        };
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return false;
        }
        content_type |= name.eq_ignore_ascii_case("content-type");
    }
    false
}

/// Bare text as a `text/plain` entity the renderer understands.
pub(super) fn text_entity(text: &[u8]) -> Vec<u8> {
    let mut out =
        b"Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n"
            .to_vec();
    out.extend_from_slice(text);
    out
}

/// The sender's key carried in an Autocrypt header (Level 1: `addr=…;
/// keydata=<base64>`), decoded, if the message has one.
pub(super) fn autocrypt_key(raw: &[u8]) -> Option<Vec<u8>> {
    use mail_parser::MessageParser;
    let parsed = MessageParser::default().parse(raw)?;
    let header = parsed.header_raw("Autocrypt")?;
    let mut keydata = None;
    for attr in header.split(';') {
        let attr = attr.trim();
        if let Some(v) = attr.strip_prefix("keydata=") {
            keydata = Some(v.split_whitespace().collect::<String>());
        }
    }
    let keydata = keydata?;
    crate::encoding::base64_decode(&keydata).filter(|b| !b.is_empty())
}
