//! `import`: file one RFC 5322 message into a folder, end-to-end encrypted,
//! the way the official desktop client imports a mail file (tutanota
//! `src/app-kit/mimimi`, `importer.rs` and `attachment_importer.rs`):
//!
//! 0. the account's plan must be one the official client imports on
//!    (Legend or Unlimited), checked first as its import button does;
//! 1. `ImportMailService` GET creates an `ImportFileMailState` for the
//!    target folder;
//! 2. the message's attachments go to the blob store, each under its own
//!    file key;
//! 3. `ImportMailService` POST takes the encrypted `ImportMailData`;
//! 4. the state's imported-mail list is polled until it names the new mail,
//!    which gives the mail's id;
//! 5. the state is finalised, as the official client finalises an import it
//!    ran to the end (or stopped).
//!
//! The MIME conversion is upstream's own (see `mime`). Nothing here talks
//! IMAP: the server files the mail from the encrypted data alone.
use super::{aggregate_id, diag, id, resume, tuple, Result, Session};
use crate::mailbox_ops::{api_code, load_mailbox, IMPORTED_FOLDER_TYPE, LABEL_FOLDER_TYPE};
use base64::{engine::general_purpose::STANDARD, Engine};
use crypto_primitives::{
    aes::{Aes256Key, InitializationVector},
    key::GenericAesKey,
    randomizer_facade::RandomizerFacade,
    versioned::VersionedAesKey,
};
use mail_parser::MessageParser;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;
use tutasdk::{
    blobs::blob_facade::FileData,
    entities::{
        generated::{
            sys::StringWrapper,
            tutanota::{
                ImportFileMailState, ImportMailData, ImportMailGetIn, ImportMailPostIn,
                ImportedFileMail, MailSetEntry,
            },
        },
        json_size_estimator::estimate_json_size,
    },
    rest_error::{HttpError, ImportFailureReason, PreconditionFailedReason},
    services::{generated::tutanota::ImportMailService, ExtraServiceParams},
    tutanota_constants::{ArchiveDataType, PlanType},
    ApiCallError, GeneratedId, IdTupleGenerated, ListLoadDirection, LoggedInSdk, Sdk,
};

mod mime;

use mime::{ImportableMail, ImportableMailAttachment};

/// The largest message taken, decoded. Tuta caps a sent mail's attachments
/// at 25 MiB; a message past that is not one Tuta would carry either, and
/// the cap bounds what the helper holds: the request (`MAX_REQUEST_BYTES`
/// in `main.rs` leaves room for this much in base64), the decoded bytes and
/// the parsed parts.
pub(crate) const MAX_IMPORT_BYTES: usize = 25 * 1024 * 1024;
/// The most MIME parts a message may have. Upstream sets none, but every
/// non-body part becomes a blob upload and an attachment entry, so a
/// pathological message of tiny parts would cost thousands of each.
const MAX_IMPORT_PARTS: usize = 256;
/// Upstream's `MAX_REQUEST_SIZE`: the largest `ImportMailPostIn` it sends.
/// A mail whose data (headers and body, attachments being blobs) alone
/// estimates past it is one upstream sets aside as too big.
const MAX_IMPORT_DATA_BYTES: usize = 8 * 1024 * 1024;
/// TS `MailSetKind.SCHEDULED`: the official client never offers it as an
/// import target.
const SCHEDULED_FOLDER_TYPE: i64 = 10;
/// What upstream names the mail set an import files into; the server keeps
/// one of type IMPORTED beside the target folder.
const IMPORTED_MAIL_SET_NAME: &str = "@internal-mailset";
/// TS `ImportStatus`.
const IMPORT_STATUS_CANCELED: i64 = 2;
const IMPORT_STATUS_FINISHED: i64 = 3;
/// How often, and how far apart, the imported-mail list is read before the
/// mail is reported without its id. The whole run must stay inside the
/// helper's 90-second budget.
const RESOLVE_ATTEMPTS: u32 = 8;
const RESOLVE_DELAY: Duration = Duration::from_millis(750);

/// The plans that may import, TS `HighestTierPlans` (`PlanType.Legend` and
/// `PlanType.Unlimited`). The official client checks this before it starts
/// an import; the server answers any other plan's import with a bare 400.
const IMPORT_PLANS: [i64; 2] = [PlanType::Legend as i64, PlanType::Unlimited as i64];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Import {
    pub session: Session,
    /// The target folder's compound id.
    folder: String,
    /// The whole message, standard base64.
    mime_base64: String,
    /// Upstream imports every mail read; a caller moving an unread mail in
    /// from elsewhere can keep it unread.
    #[serde(default)]
    unread: bool,
}

/// The target and the converted message, checked before any I/O.
fn prepare(request: &Import) -> Result<(IdTupleGenerated, ImportableMail)> {
    let folder = tuple(&request.folder)?;
    // Refused before decoding, so an oversized request costs no copy.
    if request.mime_base64.len() > MAX_IMPORT_BYTES.div_ceil(3) * 4 {
        return Err("import_too_large");
    }
    let bytes = STANDARD
        .decode(request.mime_base64.as_bytes())
        .map_err(|_| "invalid_request")?;
    if bytes.len() > MAX_IMPORT_BYTES {
        return Err("import_too_large");
    }
    let mut mail = convert(&bytes)?;
    mail.unread = request.unread;
    Ok((folder, mail))
}

/// The message as upstream's importer reads it. Anything without a header
/// is not a message at all.
fn convert(bytes: &[u8]) -> Result<ImportableMail> {
    let message = MessageParser::default()
        .parse(bytes)
        .ok_or("import_invalid")?;
    if message.headers_raw().next().is_none() {
        return Err("import_invalid");
    }
    if message.parts.len() > MAX_IMPORT_PARTS {
        return Err("import_too_large");
    }
    Ok(ImportableMail::from_parsed_message(&message))
}

/// Whether mail can be imported into a mail set of this type: the folders
/// the official client lists, less the ones a mail cannot be filed in.
/// Its own import dialog also leaves out the Inbox, but only so a bulk
/// import cannot hold up incoming mail; one message cannot.
fn importable(folder_type: i64) -> bool {
    ![
        LABEL_FOLDER_TYPE,
        IMPORTED_FOLDER_TYPE,
        SCHEDULED_FOLDER_TYPE,
    ]
    .contains(&folder_type)
}

/// The code a failed import call reports, with a diagnostic line.
fn fail(stage: &'static str) -> impl FnOnce(ApiCallError) -> &'static str {
    move |error| {
        diag::failed(stage, &error);
        match &error {
            ApiCallError::ServerResponseError {
                source:
                    HttpError::PreconditionFailedError(Some(PreconditionFailedReason::ImportFailure(
                        reason,
                    ))),
            } => match reason {
                // Import is off for this account's plan, or for now.
                ImportFailureReason::ImportDisabled => "import_refused",
                ImportFailureReason::ImportTargetFolderDeleted => "folder_not_found",
            },
            _ => api_code(error, "import_failed", "folder_not_found"),
        }
    }
}

/// Whether the account's plan may import, read as the official client reads
/// it (`UserController.getPlanType`: the customer info's plan).
async fn plan_allows_import(sdk: &LoggedInSdk) -> Result<bool> {
    let info = sdk
        .customer_facade
        .fetch_customer_info()
        .await
        .map_err(fail("customer info"))?;
    if !IMPORT_PLANS.contains(&info.plan) {
        diag::note(&format!(
            "import needs plan 7 or 10, account has {}",
            info.plan
        ));
        return Ok(false);
    }
    Ok(true)
}

pub(crate) async fn import(sdk: &Sdk, request: Import) -> Result<Value> {
    let (folder, mut mail) = prepare(&request)?;
    let logged_in = resume(sdk, request.session).await?;
    let sdk = &*logged_in;
    if !plan_allows_import(sdk).await? {
        return Err("import_refused");
    }
    let mailbox = load_mailbox(sdk).await?;
    match mailbox
        .sets
        .iter()
        .find(|set| set._id.as_ref() == Some(&folder))
    {
        Some(set) if importable(set.folderType) => {}
        _ => return Err("folder_not_found"),
    }
    let randomizer = RandomizerFacade::from_core(rand_core::OsRng);
    let group_key = sdk
        .get_current_sym_group_key(&mailbox.group)
        .await
        .map_err(fail("mail group key"))?;
    // TS `MailUploadDataWithAttachment.create_from_importable_mail`.
    let session_key = GenericAesKey::Aes256(Aes256Key::generate(&randomizer));
    let owner_enc =
        group_key.encrypt_key(&session_key, InitializationVector::generate(&randomizer));
    let version = i64::try_from(owner_enc.version).map_err(|_| "import_failed")?;
    let attachments = mail.take_out_attachments();
    let data = mail.make_import_mail_data(owner_enc.object, version);
    if estimate_json_size(&data) > MAX_IMPORT_DATA_BYTES {
        return Err("import_too_large");
    }
    let state = create_state(sdk, &randomizer, &group_key, &mailbox.group, folder).await?;
    let staged = Staged {
        sdk,
        randomizer: &randomizer,
        group_key: &group_key,
        group: &mailbox.group,
        state: &state,
    };
    match staged.post(data, session_key, &attachments).await {
        Ok(()) => {
            let mail_id = staged.resolve(RESOLVE_ATTEMPTS).await;
            staged.finalise(IMPORT_STATUS_FINISHED, 1, 0).await;
            Ok(json!({"id": mail_id}))
        }
        Err(code) => {
            staged.finalise(IMPORT_STATUS_CANCELED, 0, 1).await;
            Err(code)
        }
    }
}

/// A fresh import state for one mail into `folder`. TS
/// `ImportEssential.create_new_server_import_state`.
async fn create_state(
    sdk: &LoggedInSdk,
    randomizer: &RandomizerFacade,
    group_key: &VersionedAesKey,
    group: &GeneratedId,
    folder: IdTupleGenerated,
) -> Result<IdTupleGenerated> {
    let session_key = GenericAesKey::Aes256(Aes256Key::generate(randomizer));
    let owner_enc = group_key.encrypt_key(&session_key, InitializationVector::generate(randomizer));
    let reply = sdk
        .get_service_executor()
        .get::<ImportMailService>(
            ImportMailGetIn {
                _format: 0,
                newImportedMailSetName: IMPORTED_MAIL_SET_NAME.to_string(),
                ownerEncSessionKey: owner_enc.object,
                ownerGroup: group.clone(),
                ownerKeyVersion: i64::try_from(owner_enc.version).map_err(|_| "import_failed")?,
                totalMails: 1,
                targetMailFolder: folder,
                _errors: Default::default(),
            },
            ExtraServiceParams {
                session_key: Some(session_key),
                ..Default::default()
            },
        )
        .await
        .map_err(fail("import state"))?;
    Ok(reply.importFileMailState)
}

/// One import under way: what each later step needs.
struct Staged<'a> {
    sdk: &'a LoggedInSdk,
    randomizer: &'a RandomizerFacade,
    group_key: &'a VersionedAesKey,
    group: &'a GeneratedId,
    state: &'a IdTupleGenerated,
}

impl Staged<'_> {
    /// Upload the attachments, then post the encrypted mail. A post that did
    /// not come back cleanly may still have been applied (lost in flight,
    /// a 5xx after the commit), so the state's list decides then: a mail it
    /// names was imported, and reporting a failure would invite a second
    /// copy.
    async fn post(
        &self,
        mut data: ImportMailData,
        session_key: GenericAesKey,
        attachments: &[ImportableMailAttachment],
    ) -> Result<()> {
        data.importedAttachments = self.upload(attachments).await?;
        let value = self
            .sdk
            .serialize_instance_to_json(data, session_key)
            .map_err(fail("import serialize"))?;
        let post = ImportMailPostIn {
            _format: 0,
            encImports: vec![StringWrapper {
                _id: aggregate_id(self.randomizer),
                value,
            }],
            importFileMailState: Some(self.state.clone()),
            imapFolderSyncState: None,
        };
        let server = self.upload_server().await?;
        let posted = self
            .sdk
            .get_service_executor()
            .post::<ImportMailService>(
                post,
                ExtraServiceParams {
                    base_url: Some(server),
                    ..Default::default()
                },
            )
            .await;
        match posted {
            Ok(_) => Ok(()),
            Err(error) => {
                let code = fail("import post")(error);
                if code != "import_refused" && self.imported_mail().await.is_some() {
                    return Ok(());
                }
                Err(code)
            }
        }
    }

    /// Each attachment as a blob under a fresh file key, described for
    /// `ImportMailData.importedAttachments`. TS
    /// `PerChunkAttachmentImporter.upload_attachments_for_chunk`.
    async fn upload(
        &self,
        attachments: &[ImportableMailAttachment],
    ) -> Result<Vec<tutasdk::entities::generated::tutanota::ImportAttachment>> {
        if attachments.is_empty() {
            // No upload at all: the SDK would still fetch a write token.
            return Ok(Vec::new());
        }
        let files: Vec<FileData> = attachments
            .iter()
            .map(|attachment| FileData {
                session_key: GenericAesKey::Aes256(Aes256Key::generate(self.randomizer)),
                data: &attachment.content,
            })
            .collect();
        let tokens = self
            .sdk
            .blob_facade()
            .encrypt_and_upload_multiple(ArchiveDataType::Attachments, self.group, files.iter())
            .await
            .map_err(fail("import blob upload"))?;
        // One token list per file, in upload order; anything else would pair
        // a file with another's blobs.
        if tokens.len() != attachments.len() {
            return Err("import_failed");
        }
        attachments
            .iter()
            .zip(&files)
            .zip(tokens)
            .map(|((attachment, file), tokens)| {
                attachment
                    .make_import_attachment_data(
                        self.group_key,
                        self.randomizer,
                        &file.session_key,
                        tokens,
                    )
                    .ok_or("import_failed")
            })
            .collect()
    }

    /// The blob server the post goes to: upstream spreads import load over
    /// the cluster by posting to the last server of a write token.
    async fn upload_server(&self) -> Result<String> {
        self.sdk
            .request_blob_facade_write_token(ArchiveDataType::Attachments)
            .await
            .map_err(fail("import write token"))?
            .servers
            .last()
            .map(|server| server.url.clone())
            .ok_or("import_failed")
    }

    async fn load_state(&self) -> Option<ImportFileMailState> {
        let client = self.sdk.mail_facade().get_crypto_entity_client();
        client
            .load::<ImportFileMailState, _>(self.state)
            .await
            .map_err(|error| diag::failed("import state load", &error))
            .ok()
    }

    /// The first mail the state's list names, if any yet.
    async fn imported_mail(&self) -> Option<IdTupleGenerated> {
        let list = self.load_state().await?.importedMails;
        let client = self.sdk.mail_facade().get_crypto_entity_client();
        let imported: Vec<ImportedFileMail> = client
            .load_all(&list, ListLoadDirection::ASC)
            .await
            .map_err(|error| diag::failed("imported mail list", &error))
            .ok()?;
        let entry: MailSetEntry = client
            .load(&imported.first()?.mailSetEntry)
            .await
            .map_err(|error| diag::failed("imported mail entry", &error))
            .ok()?;
        Some(entry.mail)
    }

    /// The imported mail's id. `None` when the list did not name it in time:
    /// the mail is in, and the caller relists the folder all the same.
    async fn resolve(&self, attempts: u32) -> Option<String> {
        for attempt in 0..attempts {
            if attempt > 0 {
                tokio::time::sleep(RESOLVE_DELAY).await;
            }
            if let Some(mail) = self.imported_mail().await {
                return Some(id(&mail));
            }
        }
        diag::note("imported mail not listed in time");
        None
    }

    /// Close the state as upstream closes one: `Finished` with its counts
    /// after the last mail, `Canceled` when the run stopped. Best-effort:
    /// the mail's own outcome is already decided, and a state left
    /// `Running` only shows as an import the official client cannot resume.
    async fn finalise(&self, status: i64, successful: i64, failed: i64) {
        let Some(mut state) = self.load_state().await else {
            return;
        };
        state.status = status;
        state.successfulMails += successful;
        state.failedMails += failed;
        if let Err(error) = self
            .sdk
            .mail_facade()
            .get_crypto_entity_client()
            .update_instance(state)
            .await
        {
            diag::failed("import state update", &error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = "From: Ada <ada@example.org>\r\n\
        To: Bob <bob@example.org>\r\n\
        Subject: token-123\r\n\
        Message-ID: <m1@example.org>\r\n\
        Date: Thu, 7 Nov 2024 15:54:04 +0100\r\n\
        MIME-Version: 1.0\r\n\
        Content-Type: multipart/mixed; boundary=b\r\n\
        \r\n\
        --b\r\n\
        Content-Type: text/plain; charset=utf-8\r\n\
        \r\n\
        Hello <you>\r\n\
        --b\r\n\
        Content-Type: application/octet-stream; name=\"a.bin\"\r\n\
        Content-Disposition: attachment; filename=\"a.bin\"\r\n\
        Content-Transfer-Encoding: base64\r\n\
        \r\n\
        AAECAw==\r\n\
        --b--\r\n";

    fn request(fields: Value) -> std::result::Result<Import, serde_json::Error> {
        let mut base = json!({
            "session": {"login": "l", "user_id": "u", "access_token": "t",
                "encrypted_passphrase_key": []},
            "folder": "list/element",
            "mime_base64": STANDARD.encode(RAW),
        });
        for (key, value) in fields.as_object().unwrap() {
            base[key] = value.clone();
        }
        serde_json::from_value(base)
    }

    fn prepared(fields: Value) -> Result<(IdTupleGenerated, ImportableMail)> {
        prepare(&request(fields).unwrap())
    }

    #[test]
    fn a_message_converts_with_its_attachment() {
        let (folder, mut mail) = prepared(json!({})).unwrap();
        assert_eq!(id(&folder), "list/element");
        assert_eq!(mail.subject, "token-123");
        assert_eq!(mail.html_body_text, "Hello &lt;you&gt;");
        assert_eq!(mail.message_id.as_deref(), Some("m1@example.org"));
        assert!(!mail.unread, "imported read, as upstream does");
        let attachments = mail.take_out_attachments();
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].content, [0, 1, 2, 3]);
        assert_eq!(attachments[0].meta_data.filename, "a.bin");
        let data = mail.make_import_mail_data(vec![1], 0);
        assert_eq!(data.subject, "token-123");
        assert_eq!(data.sender.address, "ada@example.org");
        assert_eq!(data.recipients.toRecipients[0].address, "bob@example.org");
        assert_eq!(data.state, 2);
        assert!(data.importedAttachments.is_empty());
        assert!(data.compressedHeaders.contains("Subject: token-123"));
    }

    #[test]
    fn unread_is_the_callers_choice() {
        let (_, mail) = prepared(json!({"unread": true})).unwrap();
        assert!(mail.unread);
    }

    #[test]
    fn requests_are_checked_before_any_io() {
        assert_eq!(
            prepared(json!({"folder": "nope"})).err(),
            Some("invalid_id")
        );
        assert_eq!(
            prepared(json!({"folder": "a/b/c"})).err(),
            Some("invalid_id")
        );
        assert_eq!(
            prepared(json!({"mime_base64": "not base64!"})).err(),
            Some("invalid_request")
        );
        assert_eq!(
            prepared(json!({"mime_base64": ""})).err(),
            Some("import_invalid")
        );
        assert!(request(json!({"extra": 1})).is_err(), "unknown fields");
        let mut without = json!({});
        without["mime_base64"] = Value::Null;
        assert!(request(without).is_err(), "the message is required");
    }

    #[test]
    fn a_body_without_headers_is_not_a_message() {
        assert_eq!(convert(b"\r\n\r\njust text").err(), Some("import_invalid"));
    }

    #[test]
    fn size_and_part_caps_hold() {
        // Over the cap in base64 is refused before decoding.
        let over = "A".repeat(MAX_IMPORT_BYTES.div_ceil(3) * 4 + 4);
        assert_eq!(
            prepared(json!({"mime_base64": over})).err(),
            Some("import_too_large")
        );
        let mut many =
            String::from("Subject: x\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n");
        for _ in 0..MAX_IMPORT_PARTS {
            many.push_str("--b\r\nContent-Type: text/plain\r\n\r\nx\r\n");
        }
        many.push_str("--b--\r\n");
        assert_eq!(convert(many.as_bytes()).err(), Some("import_too_large"));
    }

    #[test]
    fn only_mail_folders_are_targets() {
        for kind in [0, 1, 2, 3, 4, 5, 6] {
            assert!(importable(kind), "{kind}");
        }
        for kind in [
            LABEL_FOLDER_TYPE,
            IMPORTED_FOLDER_TYPE,
            SCHEDULED_FOLDER_TYPE,
        ] {
            assert!(!importable(kind), "{kind}");
        }
    }

    #[test]
    fn plan_and_folder_refusals_have_their_own_codes() {
        let precondition = |reason| ApiCallError::ServerResponseError {
            source: HttpError::PreconditionFailedError(Some(
                PreconditionFailedReason::ImportFailure(reason),
            )),
        };
        assert_eq!(
            fail("t")(precondition(ImportFailureReason::ImportDisabled)),
            "import_refused"
        );
        assert_eq!(
            fail("t")(precondition(ImportFailureReason::ImportTargetFolderDeleted)),
            "folder_not_found"
        );
        let plain = |source| ApiCallError::ServerResponseError { source };
        assert_eq!(
            fail("t")(plain(HttpError::NotFoundError)),
            "folder_not_found"
        );
        assert_eq!(
            fail("t")(plain(HttpError::BadRequestError)),
            "import_failed"
        );
        assert_eq!(
            fail("t")(plain(HttpError::NotAuthenticatedError)),
            "session_revoked"
        );
    }
}
