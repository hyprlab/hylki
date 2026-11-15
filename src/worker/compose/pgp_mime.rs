//! OpenPGP/MIME (#133): protected headers, signing and encrypting an
//! outgoing message through the user's GnuPG.

use lettre::message::Mailbox;

use crate::config::AccountConfig;
use crate::i18n::i18n_f;
use crate::worker::compose::recipients::mailbox;
use crate::worker::smtp::SmtpError;
use crate::worker::{parse_recipients, OutgoingMessage};

/// A message body before OpenPGP wrapping: one MIME entity either way.
pub(super) enum PgpEntity {
    Multi(lettre::message::MultiPart),
    Single(lettre::message::SinglePart),
}

impl PgpEntity {
    /// The entity's exact bytes, headers included: what a signature covers
    /// and what encryption hides. lettre formats a part the same way every
    /// time (its boundary is fixed at creation), so the signed bytes are
    /// the bytes that go on the wire.
    fn formatted(&self) -> Vec<u8> {
        match self {
            PgpEntity::Multi(m) => m.formatted(),
            PgpEntity::Single(s) => s.formatted(),
        }
    }
}

/// The outer Subject of an encrypted message, as Thunderbird sends it.
pub(super) const PROTECTED_SUBJECT: &str = "...";

/// Protected headers (#493, RFC 9788): the encrypted entity carries the real
/// Subject and who the message is from and to, flagged so the reader takes
/// them from there, and the Subject outside is [`PROTECTED_SUBJECT`]. Bcc
/// stays out, as every recipient can decrypt what is inside.
pub(super) fn protect_headers(
    entity: PgpEntity,
    msg: &OutgoingMessage,
    from: &Mailbox,
) -> Result<PgpEntity, SmtpError> {
    use lettre::message::header::{Cc, ContentType, From, ReplyTo, Subject, To};
    use lettre::message::{Mailboxes, MultiPart};
    let mut multi = match entity {
        PgpEntity::Multi(m) => m,
        PgpEntity::Single(s) => MultiPart::mixed().singlepart(s),
    };
    let mailboxes = |field: &str| -> Result<Option<Mailboxes>, SmtpError> {
        let list = parse_recipients(field)
            .into_iter()
            .map(|(name, addr)| mailbox(&name, &addr))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((!list.is_empty()).then(|| list.into_iter().collect()))
    };
    let flagged = multi
        .headers()
        .get_raw("Content-Type")
        .and_then(|ct| ContentType::parse(&format!("{ct}; protected-headers=\"v1\"")).ok())
        .ok_or("could not mark the encrypted part as carrying the subject")?;
    let headers = multi.headers_mut();
    headers.set(flagged);
    headers.set(From::from(Mailboxes::from(from.clone())));
    if let Some(to) = mailboxes(&msg.to)? {
        headers.set(To::from(to));
    }
    if let Some(cc) = mailboxes(&msg.cc)? {
        headers.set(Cc::from(cc));
    }
    if let Some(reply_to) = mailboxes(&msg.reply_to)? {
        headers.set(ReplyTo::from(reply_to));
    }
    headers.set(Subject::from(msg.subject.clone()));
    Ok(PgpEntity::Multi(multi))
}

/// The user's key for an outgoing message: the account's chosen key, else
/// the key whose address matches the From, else the account's own address.
fn signing_key(account: &AccountConfig, from_addr: &str) -> Option<crate::mail::pgp::KeyInfo> {
    let gpg = crate::mail::pgp::Gpg::system();
    account
        .pgp_key
        .as_deref()
        .and_then(|f| crate::mail::pgp::secret_key_by_fingerprint(&gpg, f))
        .or_else(|| crate::mail::pgp::secret_key_for(&gpg, from_addr))
        .or_else(|| crate::mail::pgp::secret_key_for(&gpg, &account.email))
}

/// Sign and/or encrypt an outgoing entity into PGP/MIME (RFC 3156).
pub(super) fn pgp_wrap(
    account: &AccountConfig,
    msg: &OutgoingMessage,
    from_addr: &str,
    entity: PgpEntity,
) -> Result<PgpEntity, SmtpError> {
    use lettre::message::{header::ContentType, MultiPart, SinglePart};
    let gpg = crate::mail::pgp::Gpg::system();
    let key = signing_key(account, from_addr);
    let inner = entity.formatted();
    let no_key = || {
        i18n_f(
            "no OpenPGP key of your own for {addr}; generate one under Settings, OpenPGP",
            &[("addr", from_addr)],
        )
    };
    if msg.encrypt {
        let addresses = |fields: &[&String]| -> Vec<String> {
            let mut found: Vec<String> = fields
                .iter()
                .flat_map(|field| {
                    parse_recipients(field)
                        .into_iter()
                        .map(|(_, addr)| addr.to_ascii_lowercase())
                })
                .collect();
            found.sort();
            found.dedup();
            found
        };
        let to_cc = addresses(&[&msg.to, &msg.cc]);
        // Bcc keys are hidden in the packet; one also in To or Cc is
        // visible there anyway, and the sender's own copy is added below.
        let mut bcc = addresses(&[&msg.bcc]);
        let from_lower = from_addr.to_ascii_lowercase();
        bcc.retain(|a| !to_cc.contains(a) && *a != from_lower);
        // Each address goes to gpg as the fingerprint of the key checked
        // here, so the key the composer warned about is the key used, not
        // whichever one gpg finds first for the address.
        let key_for = |addr: &String| -> Result<String, SmtpError> {
            let found = crate::mail::pgp::public_key_for(&gpg, addr)
                .ok_or_else(|| i18n_f("no OpenPGP key for {addr}", &[("addr", addr)]))?;
            if !found.vouched_for(addr) {
                tracing::warn!(
                    "encrypting to key {} for {addr}, which the keyring does not vouch for",
                    found.fingerprint
                );
            }
            Ok(found.fingerprint)
        };
        let mut recipients = to_cc.iter().map(key_for).collect::<Result<Vec<_>, _>>()?;
        let hidden = bcc.iter().map(key_for).collect::<Result<Vec<_>, _>>()?;
        // The sender's own copy: to the account's key when it has one (by
        // fingerprint, so a chosen key with another address still reads its
        // Sent mail), else to whatever key the From address has.
        match &key {
            Some(k) => recipients.push(k.fingerprint.clone()),
            None => {
                let Some(own) = crate::mail::pgp::public_key_for(&gpg, from_addr) else {
                    return Err(no_key().into());
                };
                recipients.push(own.fingerprint);
            }
        }
        let signer = if msg.sign {
            Some(key.ok_or_else(no_key)?.fingerprint)
        } else {
            None
        };
        let ciphertext =
            crate::mail::pgp::encrypt(&gpg, &inner, &recipients, &hidden, signer.as_deref())?;
        let control = SinglePart::builder()
            .header(ContentType::parse("application/pgp-encrypted").expect("static content type"))
            .body(String::from("Version: 1\r\n"));
        let payload = SinglePart::builder()
            .header(
                ContentType::parse("application/octet-stream; name=\"encrypted.asc\"")
                    .expect("static content type"),
            )
            .body(armored(ciphertext));
        return Ok(PgpEntity::Multi(
            MultiPart::encrypted("application/pgp-encrypted".into())
                .singlepart(control)
                .singlepart(payload),
        ));
    }
    let key = key.ok_or_else(no_key)?;
    let (signature, micalg) = crate::mail::pgp::sign_detached(&gpg, &inner, &key.fingerprint)?;
    let sig_part = SinglePart::builder()
        .header(
            ContentType::parse("application/pgp-signature; name=\"signature.asc\"")
                .expect("static content type"),
        )
        .body(armored(signature));
    Ok(PgpEntity::Single(signed_multipart(
        &inner,
        &sig_part.formatted(),
        &micalg,
    )?))
}

/// The multipart/signed entity, written by hand rather than by lettre (#666).
///
/// lettre puts the outer delimiter straight after the signed part's last
/// line, so that part's final CRLF belongs to the delimiter and the
/// signature covers the part without it. Exchange rewrites a message's MIME
/// on the way out and leaves a blank line there, which hands the CRLF back
/// to the part and breaks the signature. Evolution writes that blank line
/// itself and signs the part with its CRLF; so does this, and a server that
/// tidies the layout the same way finds nothing to change.
fn signed_multipart(
    inner: &[u8],
    signature: &[u8],
    micalg: &str,
) -> Result<lettre::message::SinglePart, SmtpError> {
    use lettre::message::header::{ContentTransferEncoding, ContentType};
    use lettre::message::{Body, SinglePart};
    let boundary = crate::rng::from_alphabet(
        40,
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
    )
    .map_err(|e| e.to_string())?;
    let content_type = ContentType::parse(&format!(
        "multipart/signed; boundary=\"{boundary}\"; protocol=\"application/pgp-signature\"; micalg=\"{micalg}\""
    ))
    .map_err(|e| e.to_string())?;
    let mut body = Vec::with_capacity(inner.len() + signature.len() + 3 * boundary.len() + 16);
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(inner);
    body.extend_from_slice(format!("\r\n--{boundary}\r\n").as_bytes());
    body.extend_from_slice(signature);
    // lettre ends a single part's body with the CRLF this line needs.
    body.extend_from_slice(format!("--{boundary}--").as_bytes());
    // Every part lettre builds is 7-bit already; 8bit is for one that is
    // not, as a multipart may carry no other encoding.
    let encoding = if body.is_ascii() {
        ContentTransferEncoding::SevenBit
    } else {
        ContentTransferEncoding::EightBit
    };
    Ok(SinglePart::builder()
        .header(content_type)
        .body(Body::dangerous_pre_encoded(body, encoding)))
}

/// gpg's ASCII armour as text, so lettre gives it CRLF line endings: as raw
/// bytes its LF-only lines reached the wire and Stalwart refused the whole
/// message for bare newlines (#414).
fn armored(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}
