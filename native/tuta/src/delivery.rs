//! How a send is delivered: confidentially (end-to-end encrypted, Tuta
//! recipients only) or ordinarily, and the key material each mode hands
//! `SendDraftService`. Mirrors the official client's `MailFacade.sendDraft`.
//!
//! No public-key cryptography happens here. Recipient keys are loaded and the
//! bucket key is encrypted for them inside the SDK, through the audited
//! `AsymmetricCryptoFacade` path that Hylki's SDK patch exposes
//! (`sdk-patches/01-recipient-key-data.patch`); this module only wraps
//! symmetric keys with the SDK's own AES functions, as the ordinary path
//! already did.
//!
//! Key verification: the official client only refuses a key that differs from
//! one the user verified by hand. Hylki has no verified contacts, so the
//! current key from the PublicKeyService is used, as the official client does
//! for every unverified contact. Each recipient's key version and fingerprint
//! is reported back, so a caller can pin keys (trust on first use) later.

use crate::{aggregate_id, diag, Delivery, RecipientKind, Result, SEND_LANGUAGE};
use crypto_primitives::{
    aes::InitializationVector, key::GenericAesKey, randomizer_facade::RandomizerFacade,
};
use serde_json::{json, Value};
use std::sync::Mutex;
use tutasdk::{
    crypto::asymmetric_crypto_facade::{
        AsymmetricCryptoError, InternalRecipientKey, InternalRecipientKeyError,
    },
    crypto::public_key_provider::PublicKeyLoadingError,
    entities::generated::tutanota::{
        AttachmentKeyData, InternalRecipientKeyData, SendDraftData, SendDraftParameters,
    },
    tutanota_constants::{CryptoProtocolVersion, EncryptionAuthStatus},
    ApiCallError, GeneratedId, IdTupleGenerated, LoggedInSdk,
};

/// How a send goes out, decided from its recipients before anything is
/// written server-side.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    /// End-to-end encrypted for every recipient, all of them on Tuta.
    Confidential,
    /// Encrypted in transit only, for everyone. Carries the (internal,
    /// external) counts, or None when a lookup failed but an external
    /// recipient settled the mode anyway.
    Ordinary(Option<(usize, usize)>),
}

/// The official client's rule: a mail is confidential when the user asked
/// for it or when no recipient is external (`SendMailModel.isConfidential()`
/// is `confidential || !containsExternalRecipients()`). So an all-Tuta send
/// is always encrypted end to end and needs no consent, whatever `delivery`
/// says, and ordinary delivery reaches every recipient of a mixed send
/// alike.
///
/// Refusals, all definitive and before any server-side write:
/// - `secure` with an external recipient: that needs a password-protected
///   secure-external delivery, which the helper does not offer.
/// - ordinary delivery without `ordinary_confirmed`.
/// - no external recipient but a failed lookup: the mode cannot be decided,
///   so the lookup's own code.
pub(crate) fn plan(
    delivery: &Delivery,
    ordinary_confirmed: bool,
    kinds: &[RecipientKind],
) -> Result<Plan> {
    let count = |wanted: RecipientKind| kinds.iter().filter(|&&kind| kind == wanted).count();
    let (internal, external) = (
        count(RecipientKind::Internal),
        count(RecipientKind::External),
    );
    let unknown = kinds.iter().find_map(|kind| match kind {
        RecipientKind::Unknown(code) => Some(*code),
        _ => None,
    });
    match (external, unknown) {
        (0, Some(code)) => Err(code),
        (0, None) => Ok(Plan::Confidential),
        _ if *delivery == Delivery::Secure => Err("secure_external_unsupported"),
        _ if !ordinary_confirmed => Err("ordinary_delivery_requires_consent"),
        (_, Some(_)) => Ok(Plan::Ordinary(None)),
        (_, None) => Ok(Plan::Ordinary(Some((internal, external)))),
    }
}

/// Every address once, first spelling kept: the official client files one
/// key-data entry per recipient, and a To that repeats in Cc is still one.
pub(crate) fn unique_addresses(addresses: &[String]) -> Vec<String> {
    let mut unique: Vec<String> = Vec::with_capacity(addresses.len());
    for address in addresses {
        if !unique.iter().any(|seen| seen.eq_ignore_ascii_case(address)) {
            unique.push(address.clone());
        }
    }
    unique
}

/// The bucket key encrypted for every recipient, by the SDK. Unknown Tuta
/// addresses refuse the whole send (`recipient_not_found`) -- a confidential
/// mail is never downgraded, as in the official client.
pub(crate) async fn recipient_keys(
    sdk: &LoggedInSdk,
    sender_mail_group_id: &GeneratedId,
    bucket_key: &GenericAesKey,
    addresses: &[String],
) -> Result<Vec<InternalRecipientKey>> {
    sdk.encrypt_bucket_key_for_internal_recipients(sender_mail_group_id, bucket_key, addresses)
        .await
        .map_err(|error| match error {
            // Its Debug form lists addresses: count them instead.
            InternalRecipientKeyError::RecipientsNotFound(missing) => {
                diag::note(&format!("recipient keys: {} not found", missing.len()));
                "recipient_not_found"
            }
            InternalRecipientKeyError::ExternalSender => {
                diag::note("recipient keys: external sender");
                "send_failed"
            }
            InternalRecipientKeyError::Crypto(error) => {
                diag::failed("recipient keys", &error);
                match error {
                    AsymmetricCryptoError::ApiCall(ApiCallError::RestClient { .. })
                    | AsymmetricCryptoError::PublicKeyLoadingError(
                        PublicKeyLoadingError::KeyLoadingError(ApiCallError::RestClient { .. }),
                    ) => "transport_failed",
                    _ => "send_failed",
                }
            }
        })
}

/// What a recipient's key looked like, for the send reply: never key bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecipientReport {
    address: String,
    key_version: i64,
    protocol: &'static str,
    fingerprint: String,
}

impl RecipientReport {
    pub(crate) fn of(key: &InternalRecipientKey) -> Self {
        let protocol = if key.key_data.protocolVersion == CryptoProtocolVersion::TutaCrypt as i64 {
            "tuta_crypt"
        } else {
            "rsa"
        };
        Self {
            address: key.key_data.mailAddress.clone(),
            key_version: key.key_data.recipientKeyVersion,
            protocol,
            fingerprint: key
                .public_key_fingerprint
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        }
    }

    fn to_json(&self) -> Value {
        json!({"address": self.address, "key_version": self.key_version,
            "protocol": self.protocol, "fingerprint": self.fingerprint})
    }
}

/// The mode a submitted send went out in, as the reply reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Delivered {
    pub(crate) confidential: bool,
    pub(crate) recipients: Vec<RecipientReport>,
}

impl Delivered {
    pub(crate) fn to_json(&self) -> Value {
        json!({"confidential": self.confidential,
            "recipients": self.recipients.iter().map(RecipientReport::to_json).collect::<Vec<_>>()})
    }
}

// Recorded just before the submit, so a run that overruns its timeout after
// the submit still reports the mode the mail went out in.
static DELIVERED: Mutex<Option<Delivered>> = Mutex::new(None);

pub(crate) fn record(delivered: &Delivered) {
    *DELIVERED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(delivered.clone());
}

pub(crate) fn recorded() -> Option<Delivered> {
    DELIVERED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// The key material a send hands the server, per mode.
pub(crate) struct MailKeys {
    mail_session_key: Option<Vec<u8>>,
    bucket_enc_mail_session_key: Option<Vec<u8>>,
    attachment_keys: Vec<AttachmentKeyData>,
    internal_recipient_key_data: Vec<InternalRecipientKeyData>,
    session_enc_encryption_auth_status: Option<Vec<u8>>,
}

/// Ordinary delivery hands the server the clear session keys, exactly as the
/// official client does for a non-confidential draft. There is no bucket.
pub(crate) fn ordinary_keys(
    mail_session_key: &GenericAesKey,
    files: &[(IdTupleGenerated, GenericAesKey)],
) -> MailKeys {
    MailKeys {
        mail_session_key: Some(mail_session_key.as_bytes().to_vec()),
        bucket_enc_mail_session_key: None,
        attachment_keys: files
            .iter()
            .map(|(file, key)| AttachmentKeyData {
                _id: None,
                bucketEncFileSessionKey: None,
                fileSessionKey: Some(key.as_bytes().to_vec()),
                file: file.clone(),
            })
            .collect(),
        internal_recipient_key_data: Vec::new(),
        session_enc_encryption_auth_status: None,
    }
}

/// Confidential delivery never shows the server a session key: the mail's
/// and every attachment's are wrapped with the bucket key, which only the
/// recipients can unwrap. When every recipient key is TutaCrypt the mail also
/// carries its own authentication status (`TutacryptSender`) encrypted with
/// the mail session key, as `MailFacade.isTutaCryptMail` decides.
pub(crate) fn confidential_keys(
    bucket_key: &GenericAesKey,
    mail_session_key: &GenericAesKey,
    files: &[(IdTupleGenerated, GenericAesKey)],
    recipients: Vec<InternalRecipientKeyData>,
    randomizer: &RandomizerFacade,
) -> Result<MailKeys> {
    let wrap = |key: &GenericAesKey| {
        bucket_key.encrypt_key(key, InitializationVector::generate(randomizer))
    };
    let is_tuta_crypt = !recipients.is_empty()
        && recipients
            .iter()
            .all(|r| r.protocolVersion == CryptoProtocolVersion::TutaCrypt as i64);
    let session_enc_encryption_auth_status = if is_tuta_crypt {
        let status = (EncryptionAuthStatus::TutacryptSender as i64).to_string();
        Some(
            mail_session_key
                .encrypt_data(
                    status.as_bytes(),
                    InitializationVector::generate(randomizer),
                )
                .map_err(diag::fail("auth status", "send_failed"))?,
        )
    } else {
        None
    };
    Ok(MailKeys {
        mail_session_key: None,
        bucket_enc_mail_session_key: Some(wrap(mail_session_key)),
        attachment_keys: files
            .iter()
            .map(|(file, key)| AttachmentKeyData {
                _id: None,
                bucketEncFileSessionKey: Some(wrap(key)),
                fileSessionKey: None,
                file: file.clone(),
            })
            .collect(),
        internal_recipient_key_data: recipients,
        session_enc_encryption_auth_status,
    })
}

/// The submit for a staged draft. Its two copies (top level and
/// `parameters`) carry the same keys, as in the official client; every
/// aggregate gets its own id, since `SendDraftData` is not encrypted and the
/// SDK only fills missing ids in while encrypting.
pub(crate) fn send_payload(
    draft: IdTupleGenerated,
    keys: MailKeys,
    randomizer: &RandomizerFacade,
) -> SendDraftData {
    let attachments = || -> Vec<AttachmentKeyData> {
        keys.attachment_keys
            .iter()
            .map(|key| AttachmentKeyData {
                _id: aggregate_id(randomizer),
                ..key.clone()
            })
            .collect()
    };
    let recipients = || -> Vec<InternalRecipientKeyData> {
        keys.internal_recipient_key_data
            .iter()
            .map(|key| InternalRecipientKeyData {
                _id: aggregate_id(randomizer),
                ..key.clone()
            })
            .collect()
    };
    SendDraftData {
        _format: 0,
        language: SEND_LANGUAGE.to_string(),
        mailSessionKey: keys.mail_session_key.clone(),
        bucketEncMailSessionKey: keys.bucket_enc_mail_session_key.clone(),
        // Only a secure-external recipient's notification mail reads it.
        senderNameUnencrypted: None,
        // Plaintext stays off in both copies: the official client reads the
        // account's sendPlaintextOnly property, but Hylki has no such
        // setting and always composes HTML, so the flag matches the content
        // rather than a server-side switch the user cannot see here.
        plaintext: false,
        calendarMethod: false,
        sessionEncEncryptionAuthStatus: keys.session_enc_encryption_auth_status.clone(),
        sendAt: None,
        allowUndo: false,
        internalRecipientKeyData: recipients(),
        secureExternalRecipientKeyData: Vec::new(),
        attachmentKeyData: attachments(),
        mail: draft.clone(),
        symEncInternalRecipientKeyData: Vec::new(),
        parameters: Some(SendDraftParameters {
            _id: aggregate_id(randomizer),
            language: SEND_LANGUAGE.to_string(),
            mailSessionKey: keys.mail_session_key.clone(),
            bucketEncMailSessionKey: keys.bucket_enc_mail_session_key.clone(),
            senderNameUnencrypted: None,
            plaintext: false,
            calendarMethod: false,
            sessionEncEncryptionAuthStatus: keys.session_enc_encryption_auth_status.clone(),
            mail: draft,
            internalRecipientKeyData: recipients(),
            secureExternalRecipientKeyData: Vec::new(),
            symEncInternalRecipientKeyData: Vec::new(),
            attachmentKeyData: attachments(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use crypto_primitives::aes::Aes256Key;
    use tutasdk::CustomId;
    use RecipientKind::*;

    fn random_key(randomizer: &RandomizerFacade) -> GenericAesKey {
        GenericAesKey::Aes256(Aes256Key::generate(randomizer))
    }

    fn draft() -> IdTupleGenerated {
        IdTupleGenerated::new(GeneratedId("list".into()), GeneratedId("mail".into()))
    }

    fn file(element: &str) -> IdTupleGenerated {
        IdTupleGenerated::new(GeneratedId("files".into()), GeneratedId(element.into()))
    }

    fn recipient(address: &str, protocol: CryptoProtocolVersion) -> InternalRecipientKeyData {
        InternalRecipientKeyData {
            _id: None,
            mailAddress: address.to_string(),
            pubEncBucketKey: vec![9; 64],
            recipientKeyVersion: 2,
            protocolVersion: protocol as i64,
            senderKeyVersion: Some(1),
        }
    }

    #[test]
    fn an_all_tuta_send_is_confidential_without_consent() {
        for delivery in [Delivery::Ordinary, Delivery::Secure] {
            for confirmed in [false, true] {
                assert_eq!(
                    plan(&delivery, confirmed, &[Internal]),
                    Ok(Plan::Confidential)
                );
                assert_eq!(
                    plan(&delivery, confirmed, &[Internal, Internal]),
                    Ok(Plan::Confidential)
                );
            }
        }
    }

    #[test]
    fn a_mixed_send_is_ordinary_for_everyone_only_with_consent() {
        let delivery = Delivery::Ordinary;
        assert_eq!(
            plan(&delivery, true, &[Internal, External]),
            Ok(Plan::Ordinary(Some((1, 1))))
        );
        assert_eq!(
            plan(&delivery, true, &[External]),
            Ok(Plan::Ordinary(Some((0, 1))))
        );
        assert_eq!(
            plan(&delivery, false, &[Internal, External]),
            Err("ordinary_delivery_requires_consent")
        );
        // A failed lookup leaves the counts unknown, not the send.
        assert_eq!(
            plan(&delivery, true, &[External, Unknown("transport_failed")]),
            Ok(Plan::Ordinary(None))
        );
    }

    #[test]
    fn secure_delivery_never_falls_back_to_ordinary() {
        for kinds in [
            &[External][..],
            &[Internal, External],
            &[External, Unknown("transport_failed")],
        ] {
            for confirmed in [false, true] {
                assert_eq!(
                    plan(&Delivery::Secure, confirmed, kinds),
                    Err("secure_external_unsupported")
                );
            }
        }
    }

    #[test]
    fn an_undecidable_mode_refuses_with_the_lookup_code() {
        assert_eq!(
            plan(
                &Delivery::Ordinary,
                true,
                &[Internal, Unknown("transport_failed")]
            ),
            Err("transport_failed")
        );
        assert_eq!(
            plan(&Delivery::Secure, false, &[Unknown("send_failed")]),
            Err("send_failed")
        );
    }

    #[test]
    fn repeated_recipients_get_one_key_entry() {
        let addresses = [
            "ada@tuta.com".to_string(),
            "bo@tuta.com".to_string(),
            "Ada@Tuta.com".to_string(),
        ];
        assert_eq!(
            unique_addresses(&addresses),
            ["ada@tuta.com", "bo@tuta.com"]
        );
    }

    #[test]
    fn confidential_keys_hide_every_session_key_behind_the_bucket() {
        let randomizer = RandomizerFacade::from_core(rand_core::OsRng);
        let bucket_key = random_key(&randomizer);
        let mail_session_key = random_key(&randomizer);
        let files = [
            (file("a"), random_key(&randomizer)),
            (file("b"), random_key(&randomizer)),
        ];
        let keys = confidential_keys(
            &bucket_key,
            &mail_session_key,
            &files,
            vec![recipient("ada@tuta.com", CryptoProtocolVersion::TutaCrypt)],
            &randomizer,
        )
        .unwrap();
        let payload = send_payload(draft(), keys, &randomizer);
        let parameters = payload.parameters.as_ref().unwrap();
        for (session, bucket_enc) in [
            (&payload.mailSessionKey, &payload.bucketEncMailSessionKey),
            (
                &parameters.mailSessionKey,
                &parameters.bucketEncMailSessionKey,
            ),
        ] {
            assert!(session.is_none());
            let unwrapped = bucket_key
                .decrypt_aes_key(bucket_enc.as_ref().unwrap())
                .unwrap();
            assert_eq!(unwrapped.as_bytes(), mail_session_key.as_bytes());
        }
        for attachment_keys in [&payload.attachmentKeyData, &parameters.attachmentKeyData] {
            assert_eq!(attachment_keys.len(), 2);
            for (data, (file, key)) in attachment_keys.iter().zip(&files) {
                assert_eq!(&data.file, file);
                assert!(data.fileSessionKey.is_none());
                let unwrapped = bucket_key
                    .decrypt_aes_key(data.bucketEncFileSessionKey.as_ref().unwrap())
                    .unwrap();
                assert_eq!(unwrapped.as_bytes(), key.as_bytes());
            }
        }
        for recipients in [
            &payload.internalRecipientKeyData,
            &parameters.internalRecipientKeyData,
        ] {
            assert_eq!(recipients.len(), 1);
            assert_eq!(recipients[0].mailAddress, "ada@tuta.com");
            assert_eq!(recipients[0].pubEncBucketKey, vec![9; 64]);
        }
        // All TutaCrypt: the mail carries "sent by me" under its own key.
        for status in [
            &payload.sessionEncEncryptionAuthStatus,
            &parameters.sessionEncEncryptionAuthStatus,
        ] {
            let decrypted = mail_session_key
                .decrypt_data(status.as_ref().unwrap())
                .unwrap();
            assert_eq!(decrypted, b"4");
        }
        assert!(payload.secureExternalRecipientKeyData.is_empty());
        assert!(payload.symEncInternalRecipientKeyData.is_empty());
        assert!(payload.senderNameUnencrypted.is_none());
    }

    #[test]
    fn a_legacy_rsa_recipient_drops_the_auth_status() {
        let randomizer = RandomizerFacade::from_core(rand_core::OsRng);
        let keys = confidential_keys(
            &random_key(&randomizer),
            &random_key(&randomizer),
            &[],
            vec![
                recipient("ada@tuta.com", CryptoProtocolVersion::TutaCrypt),
                recipient("old@tutanota.de", CryptoProtocolVersion::Rsa),
            ],
            &randomizer,
        )
        .unwrap();
        let payload = send_payload(draft(), keys, &randomizer);
        assert!(payload.sessionEncEncryptionAuthStatus.is_none());
        assert!(payload
            .parameters
            .unwrap()
            .sessionEncEncryptionAuthStatus
            .is_none());
    }

    #[test]
    fn ordinary_keys_are_clear_with_no_bucket() {
        let randomizer = RandomizerFacade::from_core(rand_core::OsRng);
        let mail_session_key = random_key(&randomizer);
        let file_key = random_key(&randomizer);
        let keys = ordinary_keys(&mail_session_key, &[(file("a"), file_key.clone())]);
        let payload = send_payload(draft(), keys, &randomizer);
        let parameters = payload.parameters.as_ref().unwrap();
        for (session, bucket_enc, status) in [
            (
                &payload.mailSessionKey,
                &payload.bucketEncMailSessionKey,
                &payload.sessionEncEncryptionAuthStatus,
            ),
            (
                &parameters.mailSessionKey,
                &parameters.bucketEncMailSessionKey,
                &parameters.sessionEncEncryptionAuthStatus,
            ),
        ] {
            assert_eq!(session.as_deref(), Some(mail_session_key.as_bytes()));
            assert!(bucket_enc.is_none());
            assert!(status.is_none());
        }
        assert_eq!(
            payload.attachmentKeyData[0].fileSessionKey.as_deref(),
            Some(file_key.as_bytes())
        );
        assert!(payload.attachmentKeyData[0]
            .bucketEncFileSessionKey
            .is_none());
        assert!(payload.internalRecipientKeyData.is_empty());
        assert!(!payload.plaintext);
        assert_eq!(parameters.language, SEND_LANGUAGE);
    }

    #[test]
    fn every_send_payload_aggregate_carries_an_id() {
        // SendDraftData is not encrypted, so the SDK never fills a missing
        // aggregate id in, and its serializer refuses the submit before it
        // is sent ("Invalid value: ... TypeId(1788) _id", seen live).
        let randomizer = RandomizerFacade::from_core(rand_core::OsRng);
        let files = [
            (file("a"), random_key(&randomizer)),
            (file("b"), random_key(&randomizer)),
        ];
        let keys = confidential_keys(
            &random_key(&randomizer),
            &random_key(&randomizer),
            &files,
            vec![
                recipient("ada@tuta.com", CryptoProtocolVersion::TutaCrypt),
                recipient("bo@tuta.com", CryptoProtocolVersion::TutaCrypt),
            ],
            &randomizer,
        )
        .unwrap();
        let payload = send_payload(draft(), keys, &randomizer);
        let parameters = payload.parameters.as_ref().unwrap();
        let ids: Vec<&CustomId> = std::iter::once(&parameters._id)
            .chain(payload.attachmentKeyData.iter().map(|k| &k._id))
            .chain(parameters.attachmentKeyData.iter().map(|k| &k._id))
            .chain(payload.internalRecipientKeyData.iter().map(|k| &k._id))
            .chain(parameters.internalRecipientKeyData.iter().map(|k| &k._id))
            .map(|id| id.as_ref().expect("aggregate without an id"))
            .collect();
        assert_eq!(ids.len(), 9);
        for id in &ids {
            // Four random bytes, base64url without padding, as the SDK mints.
            assert_eq!(URL_SAFE_NO_PAD.decode(id.as_str()).unwrap().len(), 4);
        }
        // The file references survive, in order, in both copies.
        let files = |keys: &[AttachmentKeyData]| -> Vec<String> {
            keys.iter().map(|k| k.file.element_id.to_string()).collect()
        };
        assert_eq!(files(&payload.attachmentKeyData), ["a", "b"]);
        assert_eq!(files(&parameters.attachmentKeyData), ["a", "b"]);
    }

    #[test]
    fn the_reply_reports_key_versions_and_fingerprints_never_keys() {
        let key = InternalRecipientKey {
            key_data: recipient("ada@tuta.com", CryptoProtocolVersion::TutaCrypt),
            public_key_fingerprint: vec![0xab; 32],
        };
        let delivered = Delivered {
            confidential: true,
            recipients: vec![RecipientReport::of(&key)],
        };
        let reply = delivered.to_json();
        assert_eq!(reply["confidential"], json!(true));
        let row = &reply["recipients"][0];
        assert_eq!(row["address"], json!("ada@tuta.com"));
        assert_eq!(row["key_version"], json!(2));
        assert_eq!(row["protocol"], json!("tuta_crypt"));
        assert_eq!(row["fingerprint"], json!("ab".repeat(32)));
        assert_eq!(row.as_object().unwrap().len(), 4);
        let rsa = InternalRecipientKey {
            key_data: recipient("old@tutanota.de", CryptoProtocolVersion::Rsa),
            public_key_fingerprint: vec![1; 32],
        };
        assert_eq!(RecipientReport::of(&rsa).protocol, "rsa");
    }
}
