//! Server-side drafts: create or update one (`DraftService` POST/PUT), and
//! discard one. Mirrors TS `MailFacade.createDraft`/`updateDraft` and the
//! official client's discard (into Trash, then deleted from there).
use super::{
    diag, draft_payload, draft_recipients, encrypted_name, id, normalise_mime, resume,
    thread::{Action, Thread},
    trash_and_sent, tuple, valid_address, Result, SendAttachment, SendRecipient, Session,
    MAX_BODY_BYTES,
};
use crate::mailbox_ops::api_code;
use base64::{engine::general_purpose::STANDARD, Engine};
use crypto_primitives::{
    aes::{Aes256Key, InitializationVector},
    key::GenericAesKey,
    randomizer_facade::RandomizerFacade,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tutabridge_tuta::mail::MailExtensions;
use tutasdk::{
    blobs::blob_facade::FileData,
    entities::generated::tutanota::{
        DraftAttachment, DraftCreateData, DraftUpdateData, EncryptedMailAddress, Mail,
        MoveMailData, NewDraftAttachment,
    },
    rest_error::HttpError,
    services::{
        generated::tutanota::{DraftService, MoveMailService},
        ExtraServiceParams,
    },
    tutanota_constants::ArchiveDataType,
    ApiCallError, GeneratedId, IdTupleGenerated, LoggedInSdk, Sdk,
};

/// TS `MailState.DRAFT`.
const MAIL_STATE_DRAFT: i64 = 0;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SaveDraft {
    pub session: Session,
    /// The draft to update; absent creates a new one.
    draft_id: Option<String>,
    sender: String,
    sender_name: String,
    to: Vec<SendRecipient>,
    cc: Vec<SendRecipient>,
    bcc: Vec<SendRecipient>,
    reply_tos: Vec<SendRecipient>,
    subject: String,
    body_html: String,
    /// The draft's own end-to-end flag, as the official composer stores it.
    confidential: bool,
    /// The draft's complete attachment set: an update replaces the stored
    /// files with these.
    attachments: Vec<SendAttachment>,
    /// The parent's Message-ID, for a reply or forward (see `thread`). A
    /// draft's conversation is fixed when it is created: on an update these
    /// are checked and otherwise unused, as `DraftUpdateData` has no place
    /// for them.
    in_reply_to: Option<String>,
    /// Whitespace-separated Message-IDs, oldest first.
    references: Option<String>,
    action: Option<Action>,
}

/// Request checks made before any I/O. A draft may have no recipient yet,
/// but every address it does carry must be one, and every Message-ID too.
fn validate(request: &SaveDraft) -> Result<(Option<IdTupleGenerated>, Thread)> {
    let addresses = request
        .to
        .iter()
        .chain(&request.cc)
        .chain(&request.bcc)
        .chain(&request.reply_tos);
    if addresses
        .map(|r| r.address.as_str())
        .any(|a| !valid_address(a))
    {
        return Err("invalid_recipient");
    }
    if request.body_html.len() > MAX_BODY_BYTES {
        return Err("body_too_large");
    }
    let thread = Thread::of(
        request.in_reply_to.as_deref(),
        request.references.as_deref(),
        request.action,
    )?;
    let draft = request.draft_id.as_deref().map(tuple).transpose()?;
    Ok((draft, thread))
}

/// The sender's mail group: one of the account's own addresses, as in send.
async fn sender_group(sdk: &LoggedInSdk, sender: &str) -> Result<GeneratedId> {
    sdk.mail_facade()
        .get_group_id_for_mail_address(sender)
        .await
        .map_err(|error| {
            diag::failed("sender lookup", &error);
            api_code(error, "draft_failed", "sender_unknown")
        })
}

/// Upload the attachments and describe them for `DraftData.addedAttachments`,
/// each file key wrapped with the mail group key, as TS
/// `_createAddedAttachments` does for new files.
pub(crate) async fn stage_attachments(
    sdk: &LoggedInSdk,
    mail_group: &GeneratedId,
    group_key: &GenericAesKey,
    group_key_version: u64,
    attachments: &[SendAttachment],
) -> Result<Vec<DraftAttachment>> {
    if attachments.is_empty() {
        // No upload at all: the SDK would still fetch a write token.
        return Ok(Vec::new());
    }
    let randomizer = RandomizerFacade::from_core(rand_core::OsRng);
    let mut staged = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        let bytes = STANDARD
            .decode(attachment.data.as_bytes())
            .map_err(|_| "attachment_failed")?;
        staged.push((
            bytes,
            GenericAesKey::Aes256(Aes256Key::generate(&randomizer)),
        ));
    }
    let file_data: Vec<FileData> = staged
        .iter()
        .map(|(bytes, key)| FileData {
            session_key: key.clone(),
            data: bytes,
        })
        .collect();
    let tokens = sdk
        .blob_facade()
        .encrypt_and_upload_multiple(ArchiveDataType::Attachments, mail_group, file_data.iter())
        .await
        .map_err(diag::fail("blob upload", "attachment_failed"))?;
    if tokens.len() != staged.len() {
        return Err("attachment_failed");
    }
    let version = i64::try_from(group_key_version).map_err(|_| "draft_failed")?;
    let mut added = Vec::with_capacity(staged.len());
    for ((attachment, (_, key)), tokens) in attachments.iter().zip(&staged).zip(tokens) {
        let encrypt = |text: &[u8]| {
            key.encrypt_data(text, InitializationVector::generate(&randomizer))
                .map_err(|_| "attachment_failed")
        };
        let (main, sub) = normalise_mime(&attachment.mime_type);
        added.push(DraftAttachment {
            _id: None,
            ownerEncFileSessionKey: group_key
                .encrypt_key(key, InitializationVector::generate(&randomizer)),
            ownerKeyVersion: version,
            newFile: Some(NewDraftAttachment {
                _id: None,
                encFileName: encrypt(attachment.name.as_bytes())?,
                encMimeType: encrypt(format!("{main}/{sub}").as_bytes())?,
                encCid: attachment
                    .cid
                    .as_ref()
                    .map(|cid| encrypt(cid.as_bytes()))
                    .transpose()?,
                referenceTokens: tokens,
            }),
            existingFile: None,
        });
    }
    Ok(added)
}

fn reply_addresses(reply_tos: &[SendRecipient]) -> Vec<EncryptedMailAddress> {
    reply_tos
        .iter()
        .map(|r| EncryptedMailAddress {
            _id: None,
            name: encrypted_name(&r.name, &r.address),
            address: r.address.clone(),
            _errors: Default::default(),
        })
        .collect()
}

/// Load a stored draft, refusing anything that is not one: an id that
/// names a received or sent mail must never be overwritten or discarded.
async fn load_draft(sdk: &LoggedInSdk, draft: &IdTupleGenerated) -> Result<Mail> {
    let mail: Mail = sdk
        .mail_facade()
        .get_crypto_entity_client()
        .load(draft)
        .await
        .map_err(|error| {
            diag::failed("draft load", &error);
            api_code(error, "draft_failed", "draft_not_found")
        })?;
    if !mail._errors.is_empty() {
        return Err("decryption_failed");
    }
    if mail.state != MAIL_STATE_DRAFT || mail.mailDetailsDraft.is_none() {
        return Err("not_a_draft");
    }
    Ok(mail)
}

/// The draft's group key at the version it was written with and the draft's
/// session key under it, as TS `updateDraft` derives them.
async fn draft_keys(sdk: &LoggedInSdk, mail: &Mail) -> Result<(GenericAesKey, u64, GenericAesKey)> {
    let group = mail._ownerGroup.as_ref().ok_or("incomplete_response")?;
    let encrypted = mail
        ._ownerEncSessionKey
        .as_ref()
        .ok_or("incomplete_response")?;
    let version = mail._ownerKeyVersion.unwrap_or(0).unsigned_abs();
    let group_key = sdk
        .mail_facade()
        .get_crypto_entity_client()
        .get_crypto_facade()
        .get_key_loader_facade()
        .load_sym_group_key(group, version, None)
        .await
        .map_err(|error| {
            diag::failed("draft group key", &error);
            "draft_failed"
        })?;
    let session_key = group_key.decrypt_aes_key(encrypted).map_err(|error| {
        diag::failed("draft session key", &error);
        "decryption_failed"
    })?;
    Ok((group_key, version, session_key))
}

pub(crate) async fn save_draft(sdk: &Sdk, request: SaveDraft) -> Result<Value> {
    let (existing, thread) = validate(&request)?;
    let sdk = resume(sdk, request.session).await?;
    let group = sender_group(&sdk, &request.sender).await?;
    let SaveDraft {
        sender,
        sender_name,
        to,
        cc,
        bcc,
        reply_tos,
        subject,
        body_html,
        confidential,
        attachments,
        ..
    } = request;
    let payload = |added: Vec<DraftAttachment>| {
        let mut data = draft_payload(
            subject.clone(),
            body_html.clone(),
            sender.clone(),
            sender_name.clone(),
            draft_recipients(&to),
            draft_recipients(&cc),
            draft_recipients(&bcc),
            reply_addresses(&reply_tos),
            added,
        );
        data.confidential = confidential;
        data
    };
    if let Some(draft) = existing {
        let mail = load_draft(&sdk, &draft).await?;
        if mail._ownerGroup.as_ref() != Some(&group) {
            // The sender moved to another mailbox: the draft stays where it is.
            return Err("sender_unknown");
        }
        let (group_key, version, session_key) = draft_keys(&sdk, &mail).await?;
        let added = stage_attachments(&sdk, &group, &group_key, version, &attachments).await?;
        let mut data = payload(added);
        // The attachment set is replaced: every stored file goes, the given
        // ones come (TS `_getRemovedAttachments` with only new files).
        data.removedAttachments = mail.attachments.clone();
        data.method = mail.method;
        sdk.get_service_executor()
            .put::<DraftService>(
                DraftUpdateData {
                    _format: 0,
                    draftData: data,
                    draft: draft.clone(),
                    _errors: Default::default(),
                },
                ExtraServiceParams {
                    session_key: Some(session_key),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| {
                diag::failed("draft update (DraftService put)", &error);
                api_code(error, "draft_failed", "draft_not_found")
            })?;
        return Ok(json!({"id": id(&draft)}));
    }
    let group_key = sdk
        .get_current_sym_group_key(&group)
        .await
        .map_err(diag::fail("draft group key", "draft_failed"))?;
    let added = stage_attachments(
        &sdk,
        &group,
        &group_key.object,
        group_key.version,
        &attachments,
    )
    .await?;
    let randomizer = RandomizerFacade::from_core(rand_core::OsRng);
    let session_key = GenericAesKey::Aes256(Aes256Key::generate(&randomizer));
    let owner_enc =
        group_key.encrypt_key(&session_key, InitializationVector::generate(&randomizer));
    let created = sdk
        .get_service_executor()
        .post::<DraftService>(
            DraftCreateData {
                _format: 0,
                previousMessageId: thread.parent,
                conversationType: thread.conversation_type,
                ownerEncSessionKey: owner_enc.object,
                ownerKeyVersion: i64::try_from(owner_enc.version).map_err(|_| "draft_failed")?,
                draftData: payload(added),
                _errors: Default::default(),
            },
            ExtraServiceParams {
                session_key: Some(session_key),
                ..Default::default()
            },
        )
        .await
        .map_err(|error| {
            diag::failed("draft create (DraftService post)", &error);
            api_code(error, "draft_failed", "draft_failed")
        })?;
    Ok(json!({"id": id(&created.draft)}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteDraft {
    pub session: Session,
    id: String,
}

/// Discard a draft the way the official client does: into Trash (never
/// out of Sent), then deleted for good from there. A draft that is already
/// gone counts as discarded.
pub(crate) async fn delete_draft(sdk: &Sdk, request: DeleteDraft) -> Result<Value> {
    let draft = tuple(&request.id)?;
    let sdk = resume(sdk, request.session).await?;
    match load_draft(&sdk, &draft).await {
        Ok(_) => {}
        Err("draft_not_found") => return Ok(json!({})),
        Err(code) => return Err(code),
    }
    let (trash, sent) = trash_and_sent(&sdk).await.ok_or("draft_failed")?;
    sdk.get_service_executor()
        .post::<MoveMailService>(
            MoveMailData {
                _format: 0,
                moveReason: None,
                targetFolder: trash.clone(),
                mails: vec![draft.clone()],
                excludeMailSet: Some(sent),
            },
            ExtraServiceParams::default(),
        )
        .await
        .map_err(|error| {
            diag::failed("draft discard (move to trash)", &error);
            api_code(error, "draft_failed", "draft_failed")
        })?;
    match MailExtensions::new(sdk)
        .delete_mails(vec![draft], trash)
        .await
    {
        Ok(())
        | Err(ApiCallError::ServerResponseError {
            source: HttpError::NotFoundError,
        }) => Ok(json!({})),
        Err(error) => {
            diag::failed("draft discard (delete from trash)", &error);
            Err(api_code(error, "draft_failed", "draft_failed"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(extra: Value) -> serde_json::Result<SaveDraft> {
        let mut base = json!({
            "session": {"login": "", "user_id": "", "access_token": "", "encrypted_passphrase_key": []},
            "sender": "me@example.org", "sender_name": "", "to": [], "cc": [], "bcc": [],
            "reply_tos": [], "subject": "", "body_html": "", "confidential": false,
            "attachments": [],
        });
        base.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(base)
    }

    #[test]
    fn a_draft_needs_no_recipient_but_valid_addresses() {
        assert_eq!(
            validate(&request(json!({})).unwrap()).map(|(d, _)| d),
            Ok(None)
        );
        let bad = request(json!({"cc": [{"name": "", "address": "nope"}]})).unwrap();
        assert_eq!(validate(&bad).err(), Some("invalid_recipient"));
        let bad_reply = request(json!({"reply_tos": [{"name": "", "address": "a b@c"}]})).unwrap();
        assert_eq!(validate(&bad_reply).err(), Some("invalid_recipient"));
    }

    #[test]
    fn a_draft_id_and_body_are_checked() {
        let update = request(json!({"draft_id": "list/elem"})).unwrap();
        assert_eq!(
            validate(&update).unwrap().0.map(|d| id(&d)),
            Some("list/elem".into())
        );
        let bad = request(json!({"draft_id": "elem"})).unwrap();
        assert_eq!(validate(&bad).err(), Some("invalid_id"));
        let big = request(json!({"body_html": "x".repeat(MAX_BODY_BYTES + 1)})).unwrap();
        assert_eq!(validate(&big).err(), Some("body_too_large"));
    }

    #[test]
    fn a_reply_draft_names_its_parent() {
        let (_, new) = validate(&request(json!({})).unwrap()).unwrap();
        assert_eq!(new.parent, None);
        let reply = request(json!({"in_reply_to": "<p@x>", "references": "<r@x> <p@x>",
            "action": "reply_all"}))
        .unwrap();
        let (_, thread) = validate(&reply).unwrap();
        assert_eq!(thread.parent.as_deref(), Some("p@x"));
        assert_ne!(thread.conversation_type, new.conversation_type);
        let bad = request(json!({"in_reply_to": "a b@x"})).unwrap();
        assert_eq!(validate(&bad).err(), Some("invalid_thread"));
        assert!(request(json!({"action": "redirect"})).is_err());
    }

    #[test]
    fn unknown_and_missing_fields_are_refused() {
        assert!(request(json!({"delivery": "ordinary"})).is_err());
        let mut missing = json!({
            "session": {"login": "", "user_id": "", "access_token": "", "encrypted_passphrase_key": []},
            "sender": "me@example.org",
        });
        assert!(serde_json::from_value::<SaveDraft>(missing.take()).is_err());
    }
}
