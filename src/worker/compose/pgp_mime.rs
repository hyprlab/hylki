//! OpenPGP/MIME (#133): protected headers, signing and encrypting an
//! outgoing message through the user's GnuPG, with Autocrypt's headers
//! (Level 1) around it.

use lettre::message::header::{HeaderName, HeaderValue};
use lettre::message::Mailbox;

use crate::config::AccountConfig;
use crate::i18n::i18n_f;
use crate::mail::pgp::autocrypt::{header_value, PreferEncrypt};
use crate::mail::pgp::discovery::{export_key, export_minimal};
use crate::mail::pgp::{Gpg, Recipient};
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
/// stays out, as every recipient can decrypt what is inside, except in an
/// encrypted draft (`with_bcc`), which only the user can.
pub(super) fn protect_headers(
    entity: PgpEntity,
    msg: &OutgoingMessage,
    from: &Mailbox,
    with_bcc: bool,
) -> Result<PgpEntity, SmtpError> {
    use lettre::message::header::{Bcc, Cc, ContentType, From, ReplyTo, Subject, To};
    use lettre::message::Mailboxes;
    let mut multi = into_multi(entity);
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
    if with_bcc {
        if let Some(bcc) = mailboxes(&msg.bcc)? {
            headers.set(Bcc::from(bcc));
        }
    }
    if let Some(reply_to) = mailboxes(&msg.reply_to)? {
        headers.set(ReplyTo::from(reply_to));
    }
    headers.set(Subject::from(msg.subject.clone()));
    Ok(PgpEntity::Multi(multi))
}

/// An entity that can carry headers of its own: a lone part goes into a
/// `multipart/mixed` of one.
fn into_multi(entity: PgpEntity) -> lettre::message::MultiPart {
    match entity {
        PgpEntity::Multi(m) => m,
        PgpEntity::Single(s) => lettre::message::MultiPart::mixed().singlepart(s),
    }
}

/// Headers whose values are already folded: written as given. Each value
/// is built by `header_value` from an address checked to hold nothing a
/// header could be broken out of, and base64.
fn folded_header(name: &'static str, values: &[String]) -> Option<HeaderValue> {
    let joined = values.join(&format!("\r\n{name}: "));
    (!values.is_empty()).then(|| {
        HeaderValue::dangerous_new_pre_encoded(
            HeaderName::new_from_ascii_str(name),
            joined.clone(),
            joined,
        )
    })
}

/// The `Autocrypt:` header for mail from `from_addr`: the account's key,
/// cut down to what Level 1 sends, and its preference. `None` when
/// Autocrypt is off for the account or it has no key.
pub(super) fn autocrypt_header(account: &AccountConfig, from_addr: &str) -> Option<HeaderValue> {
    if !account.autocrypt || !crate::mail::pgp::available() {
        return None;
    }
    let gpg = Gpg::system();
    let key = signing_key(account, from_addr)?;
    let keydata = export_minimal(&gpg, &key.fingerprint, from_addr)?;
    let value = header_value(
        &from_addr.trim().to_ascii_lowercase(),
        Some(PreferEncrypt::from_mutual(account.autocrypt_mutual)),
        &keydata,
    );
    folded_header("Autocrypt", &[value])
}

/// Key gossip (Level 1): in mail encrypted to several people, each one's
/// key, so any of them can reply to all encrypted. Inside the encryption
/// only: who else got the mail, and their keys, are nobody else's business.
fn add_gossip(entity: PgpEntity, keys: &[(String, Vec<u8>)]) -> PgpEntity {
    let values: Vec<String> = keys
        .iter()
        .map(|(addr, key)| header_value(addr, None, key))
        .collect();
    let Some(header) = folded_header("Autocrypt-Gossip", &values) else {
        return entity;
    };
    let mut multi = into_multi(entity);
    multi.headers_mut().insert_raw(header);
    PgpEntity::Multi(multi)
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

/// Whether a draft from `from_addr` can be encrypted to the user: gpg is
/// there and so is a key of their own.
pub(super) fn can_seal(account: &AccountConfig, from_addr: &str) -> bool {
    crate::mail::pgp::available()
        && (signing_key(account, from_addr).is_some()
            || crate::mail::pgp::public_key_for(&Gpg::system(), from_addr).is_some())
}

/// The To and Cc addresses, and the Bcc ones that are neither of those
/// nor the sender: gpg is given these as hidden recipients, so the key ids
/// in the message do not say who else got it.
fn split_recipients(msg: &OutgoingMessage, from_addr: &str) -> (Vec<String>, Vec<String>) {
    let addrs = |field: &str| -> Vec<String> {
        parse_recipients(field)
            .into_iter()
            .map(|(_, a)| a.trim().to_ascii_lowercase())
            .filter(|a| !a.is_empty())
            .collect()
    };
    let mut visible = addrs(&msg.to);
    visible.extend(addrs(&msg.cc));
    visible.sort();
    visible.dedup();
    let from = from_addr.trim().to_ascii_lowercase();
    let mut hidden: Vec<String> = addrs(&msg.bcc)
        .into_iter()
        .filter(|a| !visible.contains(a) && *a != from)
        .collect();
    hidden.sort();
    hidden.dedup();
    (visible, hidden)
}

/// The key a recipient is encrypted to: the user's keyring first, by
/// fingerprint, then the key in Hylki's own keyring that the composer found
/// for them (Autocrypt, gossip, WKD). With it, the key as gossip carries it.
fn recipient_key(
    gpg: &Gpg,
    found: Option<&Gpg>,
    msg: &OutgoingMessage,
    addr: &str,
) -> Result<(Recipient, Option<Vec<u8>>), SmtpError> {
    if let Some(k) = crate::mail::pgp::public_key_for(gpg, addr) {
        if !k.vouched_for(addr) {
            tracing::warn!(
                "encrypting to key {} for {addr}, which the keyring does not vouch for",
                k.fingerprint
            );
        }
        let gossip = export_minimal(gpg, &k.fingerprint, addr);
        return Ok((Recipient::Id(k.fingerprint), gossip));
    }
    let fpr = msg
        .peer_keys
        .iter()
        .find(|(a, _)| a.eq_ignore_ascii_case(addr))
        .map(|(_, f)| f.as_str());
    if let (Some(fpr), Some(found)) = (fpr, found) {
        if let Some(key) = export_minimal(found, fpr, addr).or_else(|| export_key(found, fpr)) {
            return Ok((Recipient::Key(key.clone()), Some(key)));
        }
    }
    Err(i18n_f("no OpenPGP key for {addr}", &[("addr", addr)]).into())
}

/// Sign and/or encrypt an outgoing entity into PGP/MIME (RFC 3156). A
/// `draft` is encrypted to the user alone and never signed: it is theirs
/// to read, on a server that should not (Level 1, "Message Drafts").
pub(super) fn pgp_wrap(
    account: &AccountConfig,
    msg: &OutgoingMessage,
    from_addr: &str,
    entity: PgpEntity,
    draft: bool,
) -> Result<PgpEntity, SmtpError> {
    use lettre::message::{header::ContentType, MultiPart, SinglePart};
    let gpg = Gpg::system();
    let key = signing_key(account, from_addr);
    let no_key = || {
        i18n_f(
            "no OpenPGP key of your own for {addr}; generate one under Settings, OpenPGP",
            &[("addr", from_addr)],
        )
    };
    if msg.encrypt {
        // The sender's own copy: to the account's key when it has one (by
        // fingerprint, so a chosen key with another address still reads its
        // Sent mail), else to whatever key the From address has.
        let own = match &key {
            Some(k) => Recipient::Id(k.fingerprint.clone()),
            None => {
                let Some(own) = crate::mail::pgp::public_key_for(&gpg, from_addr) else {
                    return Err(no_key().into());
                };
                Recipient::Id(own.fingerprint)
            }
        };
        let (recipients, hidden, entity) = if draft {
            (vec![own], Vec::new(), entity)
        } else {
            let found = Gpg::discovered();
            let (visible, hidden_addrs) = split_recipients(msg, from_addr);
            let mut recipients = Vec::new();
            let mut gossip = Vec::new();
            for addr in &visible {
                let (r, g) = recipient_key(&gpg, found.as_ref(), msg, addr)?;
                recipients.push(r);
                if let Some(g) = g {
                    gossip.push((addr.clone(), g));
                }
            }
            let mut hidden = Vec::new();
            for addr in &hidden_addrs {
                hidden.push(recipient_key(&gpg, found.as_ref(), msg, addr)?.0);
            }
            recipients.push(own);
            let entity = if account.autocrypt && visible.len() > 1 {
                add_gossip(entity, &gossip)
            } else {
                entity
            };
            (recipients, hidden, entity)
        };
        let signer = if msg.sign && !draft {
            Some(key.ok_or_else(no_key)?.fingerprint)
        } else {
            None
        };
        let ciphertext = crate::mail::pgp::encrypt(
            &gpg,
            &entity.formatted(),
            &recipients,
            &hidden,
            signer.as_deref(),
        )?;
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
    let inner = entity.formatted();
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
