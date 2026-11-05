//! Experimental native Tuta adapter. One JSON request/response on private stdio.
//! No IMAP, SMTP, listener, keyring, database, or message logging.
mod diag;
mod transport;

use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use crypto_primitives::{
    aes::{Aes256Key, InitializationVector},
    key::GenericAesKey,
    randomizer_facade::RandomizerFacade,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{self, Read},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::{Arc, Mutex},
    time::Duration,
};
use transport::Submit;
use tutabridge_tuta::mail::MailExtensions;
use tutasdk::{
    bindings::{
        file_client::{FileClient, FileClientError},
        rest_client::RestClient,
    },
    blobs::blob_facade::FileData,
    crypto::sha256,
    entities::{
        entity_facade::list_element_id,
        generated::{
            sys::{CloseSessionServicePost, PublicKeyGetIn},
            tutanota::{
                AttachmentKeyData, DraftAttachment, DraftCreateData, DraftData, DraftRecipient,
                EncryptedMailAddress, Mail, MailSet, MailSetEntry, MoveMailData,
                NewDraftAttachment, SendDraftData, SendDraftParameters, SendDraftReturn,
                TutanotaFile,
            },
        },
        Entity,
    },
    login::{CredentialType, Credentials, LoginError},
    net::native_rest_client::NativeRestClient,
    rest_error::HttpError,
    services::{
        generated::{
            sys::{CloseSessionService, PublicKeyService},
            tutanota::{DraftService, MoveMailService, SendDraftService},
        },
        ExtraServiceParams,
    },
    tutanota_constants::{ArchiveDataType, PublicKeyIdentifierType, SecondFactorType},
    util::BASE64_EXT,
    ApiCallError, CustomId, GeneratedId, IdTupleCustom, IdTupleGenerated, ListLoadDirection,
    LoggedInSdk, Sdk,
};

const VERSION: u32 = 1;
// Attachments ride the same stdin pipe as the request (base64), so the cap
// covers a large mail, not just a JSON envelope. 32 MiB bounds memory while
// leaving headroom over the blob service's own per-file limits.
const MAX_REQUEST_BYTES: u64 = 32 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

/// What a request that overran `REQUEST_TIMEOUT` reports. Before a send's
/// submit left the process nothing can have been delivered, so that is a
/// plain `timeout`; after it, what the submit got back decides.
fn timeout_outcome(is_send: bool, submit: Submit) -> Result<Value> {
    if !is_send || submit == Submit::NotSent {
        return Err("timeout");
    }
    submit_outcome(submit).map(|()| sent_reply(None, None))
}
const TUTA_URL: &str = "https://app.tuta.com";
// The server rejects larger mail-mutation batches with Bad Request; both the
// SDK and the bridge extensions chunk at this size.
const MAIL_BATCH_LIMIT: usize = 50;
// Mirrors TS UNCOMPRESSED_MAX_SIZE: the official client refuses to create or
// update a draft past this body size.
const MAX_BODY_BYTES: usize = 1024 * 1024;
// Folder types, per the role table below.
const SENT_FOLDER_TYPE: i64 = 2;
const TRASH_FOLDER_TYPE: i64 = 3;
// A new conversation, not a reply or forward: server-side conversation
// linking for replies needs the parent's conversation id, which Hylki only
// knows as an SMTP Message-ID, so every send starts conversation type NEW.
// Local threading still works through the In-Reply-To/References headers the
// caller filed with its own copy.
const CONVERSATION_NEW: i64 = 0;
const METHOD_NONE: i64 = 0;
// The notification language on the send call. Only the server's notification
// mails (external password delivery, which this helper refuses to compose)
// read it; an ordinary send delivers as a normal mail.
const SEND_LANGUAGE: &str = "en";

type Result<T> = std::result::Result<T, &'static str>;

// The pinned SDK prints progress to stdout (see `fetch_server_model`), which
// would corrupt the single-line reply the transport parses. Park fd 1 on
// /dev/null while SDK code runs; dropping the guard restores it.
struct QuietStdout {
    saved: OwnedFd,
}

fn quiet_stdout() -> io::Result<QuietStdout> {
    // SAFETY: dup hands back an owned fd on success; -1 is checked.
    let saved = unsafe {
        let fd = libc::dup(libc::STDOUT_FILENO);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        OwnedFd::from_raw_fd(fd)
    };
    let null = std::fs::File::open("/dev/null")?;
    // SAFETY: both fds are valid; stdout now points at /dev/null.
    if unsafe { libc::dup2(null.as_raw_fd(), libc::STDOUT_FILENO) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(QuietStdout { saved })
}

impl Drop for QuietStdout {
    fn drop(&mut self) {
        // SAFETY: the saved fd stays valid for the guard's lifetime.
        unsafe {
            libc::dup2(self.saved.as_raw_fd(), libc::STDOUT_FILENO);
        }
    }
}

// Deliberately not Debug: this structure contains session and wrapping keys.
#[derive(Deserialize, Serialize)]
struct Session {
    login: String,
    user_id: String,
    access_token: String,
    encrypted_passphrase_key: Vec<u8>,
}
impl Session {
    fn credentials(self) -> Credentials {
        Credentials {
            login: self.login,
            user_id: GeneratedId(self.user_id),
            access_token: self.access_token,
            encrypted_passphrase_key: self.encrypted_passphrase_key,
            credential_type: CredentialType::Internal,
        }
    }
    fn from_credentials(c: Credentials) -> Self {
        Self {
            login: c.login,
            user_id: c.user_id.to_string(),
            access_token: c.access_token,
            encrypted_passphrase_key: c.encrypted_passphrase_key,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Login {
        email: String,
        password: String,
        totp: Option<String>,
    },
    Sync {
        session: Session,
        folder: Option<String>,
        limit: usize,
        offset: usize,
    },
    Message {
        session: Session,
        id: String,
    },
    MarkRead {
        session: Session,
        ids: Vec<String>,
        is_read: bool,
    },
    Move {
        session: Session,
        ids: Vec<String>,
        destination: String,
    },
    Send {
        session: Session,
        sender: String,
        sender_name: String,
        to: Vec<SendRecipient>,
        cc: Vec<SendRecipient>,
        bcc: Vec<SendRecipient>,
        reply_tos: Vec<SendRecipient>,
        subject: String,
        body_html: String,
        delivery: Delivery,
        ordinary_confirmed: bool,
        attachments: Vec<SendAttachment>,
    },
    Revoke {
        session: Session,
    },
}

/// Which delivery the caller chose. There is no default: ordinary delivery
/// additionally requires `ordinary_confirmed`, so it can never be selected by
/// omitting a field.
#[derive(Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
enum Delivery {
    Secure,
    Ordinary,
}

#[derive(Deserialize)]
struct SendRecipient {
    name: String,
    address: String,
}

#[derive(Deserialize)]
struct SendAttachment {
    name: String,
    mime_type: String,
    cid: Option<String>,
    /// Standard-base64 file bytes.
    data: String,
}
#[derive(Deserialize)]
struct Request {
    version: u32,
    #[serde(flatten)]
    operation: Operation,
}

#[derive(Default)]
struct MemoryFiles(Mutex<HashMap<String, Vec<u8>>>);
#[async_trait::async_trait]
impl FileClient for MemoryFiles {
    async fn persist_content(
        &self,
        name: String,
        content: Vec<u8>,
    ) -> std::result::Result<(), FileClientError> {
        self.0
            .lock()
            .map_err(|_| FileClientError::Unknown)?
            .insert(name, content);
        Ok(())
    }
    async fn read_content(&self, name: String) -> std::result::Result<Vec<u8>, FileClientError> {
        self.0
            .lock()
            .map_err(|_| FileClientError::Unknown)?
            .get(&name)
            .cloned()
            .ok_or(FileClientError::NotFound)
    }
}

fn tuple(value: &str) -> Result<IdTupleGenerated> {
    let (list, element) = value.split_once('/').ok_or("invalid_id")?;
    if [list, element].iter().any(|p| {
        p.is_empty()
            || !p
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    }) {
        return Err("invalid_id");
    }
    Ok(IdTupleGenerated::new(
        GeneratedId(list.into()),
        GeneratedId(element.into()),
    ))
}
fn id(value: &IdTupleGenerated) -> String {
    format!("{}/{}", value.list_id, value.element_id)
}

fn role(kind: i64) -> (&'static str, &'static str) {
    match kind {
        1 => ("inbox", "Inbox"),
        2 => ("sent", "Sent"),
        3 => ("trash", "Trash"),
        4 => ("archive", "Archive"),
        5 => ("junk", "Spam"),
        6 => ("drafts", "Drafts"),
        _ => ("other", "Folder"),
    }
}
// `conversation` is the conversation entry's list id: Tuta files every mail
// of one conversation in one such list, and header listings carry no
// Message-ID or References, so it is the only threading key a client has
// before a body is read. `reply_type` is Tuta's own replied/forwarded mark
// (0 none, 1 replied, 2 forwarded, 3 both). All three fields are additive:
// a client that predates them ignores them.
fn header(mail: &Mail) -> Result<Value> {
    if !mail._errors.is_empty() {
        return Err("decryption_failed");
    }
    let mail_id = id(mail._id.as_ref().ok_or("incomplete_response")?);
    Ok(
        json!({"id": mail_id, "subject": mail.subject, "date_ms": mail.receivedDate.as_millis(),
        "sender": {"name": mail.sender.name, "address": mail.sender.address},
        "recipient": mail.firstRecipient.as_ref().map(|r| json!({"name": r.name, "address": r.address})),
        "is_unread": mail.unread, "confidential": mail.confidential,
        "conversation": mail.conversationEntry.list_id.to_string(),
        "has_attachments": !mail.attachments.is_empty(),
        "reply_type": mail.replyType}),
    )
}
fn resume_error(error: LoginError) -> &'static str {
    match error {
        // The server no longer honors this token: revoked, expired, or logged
        // out elsewhere. The user signs in again; retrying changes nothing.
        LoginError::InvalidAccessToken { .. } => "session_revoked",
        // Our stored material is corrupt, not the server's state.
        LoginError::InvalidPassphrase { .. } | LoginError::InvalidKey { .. } => "session_invalid",
        LoginError::ApiCall { source } => match source {
            ApiCallError::RestClient { .. } => "transport_failed",
            ApiCallError::ServerResponseError { source } => server_error(source),
            ApiCallError::InternalSdkError { .. } => "resume_failed",
        },
    }
}

fn server_error(error: HttpError) -> &'static str {
    match error {
        // No automatic reauthentication: a refused client version is not an
        // invalid password. The fix is an app update, not a retry.
        HttpError::InvalidSoftwareVersionError => "client_outdated",
        HttpError::SessionExpiredError
        | HttpError::NotAuthenticatedError
        | HttpError::NotAuthorizedError
        | HttpError::AccessExpiredError
        | HttpError::AccessDeactivatedError
        | HttpError::AccessBlockedError => "session_revoked",
        HttpError::RequestTimeoutError => "timeout",
        HttpError::ConnectionError
        | HttpError::InternalServerError
        | HttpError::BadGatewayError
        | HttpError::ServiceUnavailableError { .. }
        | HttpError::TooManyRequestsError { .. }
        | HttpError::LockedError => "transport_failed",
        _ => "resume_failed",
    }
}

async fn resume(sdk: &Sdk, session: Session) -> Result<Arc<LoggedInSdk>> {
    sdk.login(session.credentials()).await.map_err(|error| {
        diag::failed("session resume", &error);
        resume_error(error)
    })
}

/// Whether an address is shaped like one. Deliberately loose -- the server is
/// authoritative and rejects what is truly undeliverable -- so a strict check
/// here can only wrongly refuse mail. Anything without exactly one @, an
/// empty side, or whitespace/control characters is not an address at all.
fn valid_address(address: &str) -> bool {
    let mut parts = address.split('@');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(local), Some(domain), None) => {
            !local.is_empty()
                && !domain.is_empty()
                && !address.chars().any(|c| c.is_whitespace() || c.is_control())
        }
        _ => false,
    }
}

fn normalise_mime(mime_type: &str) -> (&str, &str) {
    match mime_type.split_once('/') {
        Some((main, sub)) if !main.is_empty() && !sub.is_empty() => (main, sub),
        _ => ("application", "octet-stream"),
    }
}

/// Per-id outcomes for a mutation batch, in input order: every id up to the
/// first failure applied, the failed id carries its code, and the rest were
/// never attempted. A chunk is one service call, so a failed chunk reports
/// its head and the tail stays unknown -- all three mutations are idempotent,
/// and the next sync reconciles whatever the server did apply.
fn outcomes(ids: &[String], applied: usize, code: Option<&'static str>) -> Value {
    let rows: Vec<Value> = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            let outcome = if index < applied {
                None
            } else if index == applied {
                code
            } else {
                Some("not_attempted")
            };
            json!({"id": id, "code": outcome})
        })
        .collect();
    json!({"outcomes": rows})
}

/// What a send whose submit did not come back cleanly reports, decided by
/// what the submit request did on the wire (see `transport`), never by the
/// SDK's error alone: the SDK reports a 412 with a precondition reason it
/// does not know, a failed serialization before the request and an
/// unreadable reply after a 200 all as the same `InternalSdkError`.
///
/// Only a request that left the process and got no definite answer is
/// `send_uncertain`: the mail may have gone out, and retrying may deliver it
/// twice. A request that never left, or that the server refused with a 4xx,
/// sent nothing (`send_failed`, or the specific session/version code), so a
/// retry is safe. `Ok` is a submit the server accepted whose reply was lost
/// or unreadable afterwards: the mail went.
fn submit_outcome(submit: Submit) -> Result<()> {
    match submit {
        Submit::NotSent => Err("send_failed"),
        Submit::NoStatus => Err("send_uncertain"),
        Submit::Answered(200..=299) => Ok(()),
        Submit::Answered(401 | 440) => Err("session_revoked"),
        Submit::Answered(474) => Err("client_outdated"),
        // Every other 4xx (and 47x) is a refusal decided before sending, as
        // is 507: the server checks storage before it accepts the mail.
        Submit::Answered(400..=499 | 507) => Err("send_failed"),
        // 5xx from the server or a gateway, or a status nothing expects:
        // the request may have executed.
        Submit::Answered(_) => Err("send_uncertain"),
    }
}

/// The result of a sent mail. Without the server's reply (accepted, but the
/// reply was unreadable or the run overran after it) the ids are null: the
/// mail went, and nothing a caller does with a send depends on them.
fn sent_reply(reply: Option<&SendDraftReturn>, counts: Option<(usize, usize)>) -> Value {
    json!({
        "message_id": reply.map(|r| r.messageId.clone()),
        "sent_date_ms": reply.map(|r| r.sentDate.as_millis()),
        "sent_mail": reply.map(|r| id(&r.sentMail)),
        "internal_recipients": counts.map(|(internal, _)| internal),
        "external_recipients": counts.map(|(_, external)| external),
    })
}

// Which code to report when none of the offered challenges is TOTP: name
// the blocking factor honestly so the UI can say what the account actually
// needs. A mix, or a type this helper predates, stays generic.
fn unsupported_factor_code(challenge_types: &[i64]) -> &'static str {
    let mut kinds = challenge_types.to_vec();
    kinds.sort_unstable();
    kinds.dedup();
    match kinds.as_slice() {
        [only] if *only == SecondFactorType::U2f as i64 => "unsupported_u2f",
        [only] if *only == SecondFactorType::Webauthn as i64 => "unsupported_webauthn",
        _ => "unsupported_second_factor",
    }
}

/// Which code a failed sign-in reports. Only a server that answered can
/// have refused the address or the password; no connection, an outage or
/// a refused client version must not read as a wrong password.
fn initiate_error(error: LoginError) -> &'static str {
    match error {
        LoginError::ApiCall {
            source: ApiCallError::RestClient { .. },
        } => "transport_failed",
        LoginError::ApiCall {
            source: ApiCallError::ServerResponseError { source },
        } => match source {
            HttpError::InvalidSoftwareVersionError => "client_outdated",
            HttpError::RequestTimeoutError => "timeout",
            HttpError::ConnectionError
            | HttpError::InternalServerError
            | HttpError::BadGatewayError
            | HttpError::ServiceUnavailableError { .. }
            | HttpError::TooManyRequestsError { .. } => "transport_failed",
            _ => "login_failed",
        },
        _ => "login_failed",
    }
}

async fn login(sdk: &Sdk, email: String, password: String, totp: Option<String>) -> Result<Value> {
    let initiated = sdk
        .initiate_session(&email, &password, "Hylki")
        .await
        .map_err(|error| {
            diag::failed("sign-in", &error);
            initiate_error(error)
        })?;
    if !initiated.challenges.is_empty() {
        let offers_totp = initiated
            .challenges
            .iter()
            .any(|c| c.r#type == SecondFactorType::Totp as i64);
        if !offers_totp {
            let types: Vec<i64> = initiated.challenges.iter().map(|c| c.r#type).collect();
            return Err(unsupported_factor_code(&types));
        }
        let code = totp.ok_or("second_factor_required")?;
        if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err("invalid_totp");
        }
        sdk.authenticate_with_second_factor_totp(
            &initiated.credentials.access_token,
            code.parse().map_err(|_| "invalid_totp")?,
        )
        .await
        .map_err(diag::fail("second factor", "second_factor_failed"))?;
        for _ in 0..30 {
            if !sdk
                .is_second_factor_pending(&initiated.credentials.access_token)
                .await
                .map_err(diag::fail("second factor poll", "second_factor_failed"))?
            {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    sdk.login(initiated.credentials.clone())
        .await
        .map_err(diag::fail("session login", "login_failed"))?;
    Ok(json!({"session": Session::from_credentials(initiated.credentials)}))
}

// First bytes of an access token: the session list id. Mirrors the SDK's own
// session id parsing, which lives behind a private function.
const GENERATED_ID_BYTES: usize = 9;

fn session_id_from_token(token: &str) -> Option<IdTupleCustom> {
    let bytes = URL_SAFE_NO_PAD.decode(token).ok()?;
    if bytes.len() < GENERATED_ID_BYTES {
        return None;
    }
    let (list_id_bytes, element_id_bytes) = bytes.split_at(GENERATED_ID_BYTES);
    Some(IdTupleCustom {
        list_id: GeneratedId(BASE64_EXT.encode(list_id_bytes)),
        element_id: CustomId(URL_SAFE_NO_PAD.encode(sha256(element_id_bytes))),
    })
}

async fn revoke(sdk: &Sdk, session: Session) -> Result<Value> {
    let token = session.access_token.clone();
    let session_id = session_id_from_token(&token).ok_or("session_invalid")?;
    let logged_in = match sdk.login(session.credentials()).await {
        Ok(logged_in) => logged_in,
        // Idempotent: a dead session needs no closing.
        Err(
            LoginError::InvalidAccessToken { .. }
            | LoginError::InvalidPassphrase { .. }
            | LoginError::InvalidKey { .. },
        ) => return Ok(json!({})),
        Err(error) => return Err(resume_error(error)),
    };
    let closed = CloseSessionServicePost {
        _format: 0,
        accessToken: token,
        sessionId: session_id,
    };
    match logged_in
        .get_service_executor()
        .post::<CloseSessionService>(closed, ExtraServiceParams::default())
        .await
    {
        Ok(()) => Ok(json!({})),
        // Raced with an expiry or a second client: gone is gone.
        Err(ApiCallError::ServerResponseError {
            source:
                HttpError::NotFoundError
                | HttpError::NotAuthenticatedError
                | HttpError::SessionExpiredError,
        }) => Ok(json!({})),
        Err(_) => Err("revoke_failed"),
    }
}

async fn sync(
    sdk: Arc<LoggedInSdk>,
    selected: Option<String>,
    limit: usize,
    offset: usize,
) -> Result<Value> {
    if limit == 0 || limit > 1000 || offset > 1_000_000 {
        return Err("invalid_page");
    }
    let facade = sdk.mail_facade();
    let client = facade.get_crypto_entity_client();
    let mailbox = facade
        .load_user_mailbox()
        .await
        .map_err(|_| "request_failed")?;
    let folders: Vec<MailSet> = client
        .load_all(&mailbox.mailSets.mailSets, ListLoadDirection::ASC)
        .await
        .map_err(|_| "request_failed")?;
    let mut folder_rows = Vec::new();
    let mut target: Option<MailSet> = None;
    for folder in folders {
        if !folder._errors.is_empty() {
            return Err("decryption_failed");
        }
        let folder_id = id(folder._id.as_ref().ok_or("incomplete_response")?);
        let (folder_role, fallback) = role(folder.folderType);
        folder_rows.push(json!({"id": folder_id, "role": folder_role,
            "label": if folder.name.is_empty() { fallback } else { &folder.name },
            "parent": folder.parentFolder.as_ref().map(id).unwrap_or_default()}));
        if selected.as_ref() == Some(&folder_id) || (selected.is_none() && folder.folderType == 1) {
            target = Some(folder);
        }
    }
    let target = target.ok_or("folder_not_found")?;
    // Full entry enumeration is authoritative only on success; callers must not prune on errors.
    let entries: Vec<MailSetEntry> = client
        .load_all(&target.entries, ListLoadDirection::DESC)
        .await
        .map_err(|_| "request_failed")?;
    let all_ids: Vec<String> = entries.iter().map(|e| id(&e.mail)).collect();
    let mut messages = Vec::new();
    for entry in entries.iter().skip(offset).take(limit) {
        let mail: Mail = client
            .load(&entry.mail)
            .await
            .map_err(|_| "request_failed")?;
        messages.push(header(&mail)?);
    }
    Ok(json!({"folders": folder_rows, "messages": messages,
        "folder": id(target._id.as_ref().ok_or("incomplete_response")?),
        "exists": all_ids.len(), "all_ids": if offset == 0 { Some(all_ids) } else { None }, "offset": offset}))
}
async fn message(sdk: Arc<LoggedInSdk>, mail_id: String) -> Result<Value> {
    let mail: Mail = sdk
        .mail_facade()
        .get_crypto_entity_client()
        .load(&tuple(&mail_id)?)
        .await
        .map_err(|_| "request_failed")?;
    let extensions = MailExtensions::new(sdk);
    let details = if mail.mailDetailsDraft.is_some() {
        extensions.load_mail_details_draft(&mail).await
    } else if mail.mailDetails.is_some() {
        extensions.load_mail_details_blob(&mail).await
    } else {
        return Err("unsupported_legacy_body");
    }
    .map_err(|_| "decryption_failed")?;
    if !details.body._errors.is_empty() {
        return Err("decryption_failed");
    }
    let attachments = extensions
        .load_attachments(&mail)
        .await
        .map_err(|_| "attachment_failed")?;
    if attachments.iter().any(|(file, _)| !file._errors.is_empty()) {
        return Err("attachment_failed");
    }
    let rows: Vec<Value> = attachments.iter().map(|(file, bytes)| json!({
        "name": file.name, "mime_type": file.mimeType, "cid": file.cid, "data": STANDARD.encode(bytes)
    })).collect();
    let address = |a: &tutasdk::entities::generated::tutanota::MailAddress| json!({"name": a.name, "address": a.address});
    Ok(json!({"header": header(&mail)?,
        "html": details.body.compressedText.or(details.body.text).ok_or("incomplete_response")?,
        "headers": details.headers.and_then(|h| h.compressedHeaders.or(h.headers)).unwrap_or_default(),
        "to": details.recipients.toRecipients.iter().map(address).collect::<Vec<_>>(),
        "cc": details.recipients.ccRecipients.iter().map(address).collect::<Vec<_>>(),
        "reply_to": details.replyTos.iter().map(|a| json!({"name":a.name,"address":a.address})).collect::<Vec<_>>(),
        "attachments": rows}))
}
/// Tuta's own mail domains, from `TUTA_MAIL_ADDRESS_DOMAINS` in the official
/// client (`platform-kit/app-env/TutanotaConstants.ts`). An address on one of
/// these is internal without a lookup, as `RecipientsModel` treats it.
const TUTA_MAIL_ADDRESS_DOMAINS: [&str; 6] = [
    "tuta.com",
    "tutamail.com",
    "tuta.io",
    "tutanota.com",
    "tutanota.de",
    "keemail.me",
];

fn is_tuta_address(address: &str) -> bool {
    address.rsplit_once('@').is_some_and(|(_, domain)| {
        TUTA_MAIL_ADDRESS_DOMAINS
            .iter()
            .any(|tuta| domain.trim().eq_ignore_ascii_case(tuta))
    })
}

/// What a recipient is to Tuta: another Tuta mailbox (internal), a mailbox
/// elsewhere (external), or unknown because the key lookup itself failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecipientKind {
    Internal,
    External,
    Unknown(&'static str),
}

/// Classify every recipient, as the official client's `RecipientsModel`
/// does: Tuta domains are internal outright, otherwise the public-key
/// service decides (a key means internal, 404 means external).
async fn classify_recipients(sdk: &LoggedInSdk, addresses: &[String]) -> Vec<RecipientKind> {
    let mut kinds = Vec::with_capacity(addresses.len());
    for address in addresses {
        if is_tuta_address(address) {
            kinds.push(RecipientKind::Internal);
            continue;
        }
        let lookup = sdk
            .get_service_executor()
            .get::<PublicKeyService>(
                PublicKeyGetIn {
                    _format: 0,
                    identifier: address.clone(),
                    version: None,
                    identifierType: PublicKeyIdentifierType::MailAddress as i64,
                },
                ExtraServiceParams::default(),
            )
            .await;
        kinds.push(match lookup {
            Ok(_) => RecipientKind::Internal,
            Err(ApiCallError::ServerResponseError {
                source: HttpError::NotFoundError,
            }) => RecipientKind::External,
            Err(error) => {
                diag::failed("recipient resolution", &error);
                RecipientKind::Unknown(match error {
                    ApiCallError::RestClient { .. } => "transport_failed",
                    _ => "send_failed",
                })
            }
        });
    }
    kinds
}

/// Whether ordinary delivery may go ahead, decided before anything is
/// written server-side. Returns the (internal, external) counts, or None when
/// some lookup failed but an external recipient already settles the mode.
///
/// The official client never sends ordinary mail when no recipient is
/// external: `SendMailModel.isConfidential()` is `confidential ||
/// !containsExternalRecipients()`, so an all-Tuta send is always end-to-end
/// encrypted -- which this helper cannot do (docs/tuta-confidential-send.md).
/// Such a send is refused definitively instead of being submitted as
/// ordinary mail the server does not deliver. With no external recipient and
/// a failed lookup the mode cannot be decided, so that refuses too, with the
/// lookup's own definitive code.
fn ordinary_delivery_plan(kinds: &[RecipientKind]) -> Result<Option<(usize, usize)>> {
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
        (0, None) => Err("confidential_send_unsupported"),
        (_, Some(_)) => Ok(None),
        (_, None) => Ok(Some((internal, external))),
    }
}

/// The account's Trash and Sent folder ids.
async fn trash_and_sent(sdk: &LoggedInSdk) -> Option<(IdTupleGenerated, IdTupleGenerated)> {
    let mailbox = sdk.mail_facade().load_user_mailbox().await.ok()?;
    let folders: Vec<MailSet> = sdk
        .mail_facade()
        .get_crypto_entity_client()
        .load_all(&mailbox.mailSets.mailSets, ListLoadDirection::ASC)
        .await
        .ok()?;
    let find = |kind: i64| {
        folders
            .iter()
            .find(|folder| folder.folderType == kind)
            .and_then(|folder| folder._id.clone())
    };
    Some((find(TRASH_FOLDER_TYPE)?, find(SENT_FOLDER_TYPE)?))
}

/// Discard a staged draft after a send that definitely did not go out, the
/// way the official client discards one: into Trash, then deleted for good
/// from there. The server refuses a permanent delete anywhere but Trash and
/// Spam (Bad Request from Drafts), so deleting it in place never worked.
/// Best-effort: the outbox row stays the recovery point, so a leftover draft
/// is clutter, not data loss. Never called after an uncertain submit -- a
/// mail the server may still be sending is left alone -- and the move skips
/// anything already in Sent all the same.
async fn cleanup_draft(sdk: Arc<LoggedInSdk>, draft: &IdTupleGenerated) {
    let Some((trash, sent)) = trash_and_sent(&sdk).await else {
        diag::note("draft cleanup failed: no trash or sent folder");
        return;
    };
    if let Err(error) = sdk
        .get_service_executor()
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
    {
        diag::failed("draft cleanup (move to trash)", &error);
        return;
    }
    if let Err(error) = MailExtensions::new(sdk)
        .delete_mails(vec![draft.clone()], trash)
        .await
    {
        diag::failed("draft cleanup (delete from trash)", &error);
    }
}

/// A name for an encrypted name field: the given one, else the address.
///
/// The pinned SDK writes an encrypted field holding its default value -- an
/// empty string -- as a bare `""` instead of ciphertext
/// (`encrypt_and_map_inner`), where the official client encrypts every
/// non-null value (`CryptoMapper.encryptValue`). The draft is stored either
/// way, but with an empty sender name, recipient name and bodyText the live
/// server answered `SendDraftService` with 500; with the address in the
/// names and the body in bodyText it answers 201. TutaBridge's live-tested
/// send makes the same two fallbacks.
fn encrypted_name(name: &str, address: &str) -> String {
    if name.is_empty() { address } else { name }.to_string()
}

fn draft_recipients(recipients: &[SendRecipient]) -> Vec<DraftRecipient> {
    recipients
        .iter()
        .map(|r| DraftRecipient {
            _id: None,
            name: encrypted_name(&r.name, &r.address),
            mailAddress: r.address.clone(),
            _errors: Default::default(),
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn draft_payload(
    subject: String,
    body_html: String,
    sender: String,
    sender_name: String,
    to: Vec<DraftRecipient>,
    cc: Vec<DraftRecipient>,
    bcc: Vec<DraftRecipient>,
    reply_tos: Vec<EncryptedMailAddress>,
    added_attachments: Vec<DraftAttachment>,
) -> DraftData {
    DraftData {
        _id: None,
        subject,
        // The official client leaves bodyText empty, which it encrypts; the
        // pinned SDK would send a bare "" instead (see `encrypted_name`), so
        // it carries the body too, as TutaBridge's live-tested send does.
        bodyText: body_html.clone(),
        senderName: encrypted_name(&sender_name, &sender),
        senderMailAddress: sender,
        // Ordinary delivery only (see `send`): never flipped silently.
        confidential: false,
        method: METHOD_NONE,
        compressedBodyText: Some(body_html),
        toRecipients: to,
        ccRecipients: cc,
        bccRecipients: bcc,
        addedAttachments: added_attachments,
        removedAttachments: Vec::new(),
        replyTos: reply_tos,
        _errors: Default::default(),
    }
}

/// A fresh aggregate id: four random bytes, base64url, as the official
/// client's worker and the SDK's own encrypting mapper mint them. The SDK
/// fills missing ones in only while encrypting, and `SendDraftData` is not
/// encrypted: without an id on every aggregate its serializer refuses the
/// request (`Invalid value: ... _id`) before anything is sent.
fn aggregate_id(randomizer: &RandomizerFacade) -> Option<CustomId> {
    Some(CustomId(
        URL_SAFE_NO_PAD.encode(randomizer.generate_random_array::<4>()),
    ))
}

fn ordinary_send_payload(
    draft: IdTupleGenerated,
    mail_session_key: Vec<u8>,
    attachment_keys: Vec<AttachmentKeyData>,
    randomizer: &RandomizerFacade,
) -> SendDraftData {
    // Both copies of the attachment keys are aggregates in their own right.
    let keyed = |keys: &[AttachmentKeyData]| -> Vec<AttachmentKeyData> {
        keys.iter()
            .map(|key| AttachmentKeyData {
                _id: aggregate_id(randomizer),
                ..key.clone()
            })
            .collect()
    };
    SendDraftData {
        _format: 0,
        language: SEND_LANGUAGE.to_string(),
        // Ordinary delivery hands the server the clear session keys, exactly
        // as the official client does for a non-confidential draft. The
        // bucket-encrypted variants stay empty: there is no bucket.
        mailSessionKey: Some(mail_session_key.clone()),
        bucketEncMailSessionKey: None,
        senderNameUnencrypted: None,
        // Plaintext stays off in both copies: the official client reads the
        // account's sendPlaintextOnly property, but Hylki has no such
        // setting and always composes HTML, so the flag matches the content
        // rather than a server-side switch the user cannot see here.
        plaintext: false,
        calendarMethod: false,
        sessionEncEncryptionAuthStatus: None,
        sendAt: None,
        allowUndo: false,
        internalRecipientKeyData: Vec::new(),
        secureExternalRecipientKeyData: Vec::new(),
        attachmentKeyData: keyed(&attachment_keys),
        mail: draft.clone(),
        symEncInternalRecipientKeyData: Vec::new(),
        parameters: Some(SendDraftParameters {
            _id: aggregate_id(randomizer),
            language: SEND_LANGUAGE.to_string(),
            mailSessionKey: Some(mail_session_key),
            bucketEncMailSessionKey: None,
            senderNameUnencrypted: None,
            plaintext: false,
            calendarMethod: false,
            sessionEncEncryptionAuthStatus: None,
            mail: draft,
            internalRecipientKeyData: Vec::new(),
            secureExternalRecipientKeyData: Vec::new(),
            symEncInternalRecipientKeyData: Vec::new(),
            attachmentKeyData: keyed(&attachment_keys),
        }),
    }
}

struct StagedAttachment {
    name: String,
    mime_type: String,
    cid: Option<String>,
    bytes: Vec<u8>,
    session_key: GenericAesKey,
}

#[allow(clippy::too_many_arguments)]
async fn send(
    sdk: Arc<LoggedInSdk>,
    sender: String,
    sender_name: String,
    to: Vec<SendRecipient>,
    cc: Vec<SendRecipient>,
    bcc: Vec<SendRecipient>,
    reply_tos: Vec<SendRecipient>,
    subject: String,
    body_html: String,
    delivery: Delivery,
    ordinary_confirmed: bool,
    attachments: Vec<SendAttachment>,
) -> Result<Value> {
    // Secure delivery is refused, not downgraded. Composing it needs the
    // recipient-key path the pinned SDK keeps private: LoggedInSdk exposes
    // neither AsymmetricCryptoFacade nor PublicKeyProvider, and the TutaCrypt
    // sender-identity flow has no public entry point, so only legacy RSA
    // recipients could be served -- and serving some Tuta recipients while
    // failing others is a trap, not a feature. Follow-up: expose the facades
    // (an SDK patch adding getters) or re-pin to an SDK with a send API, then
    // implement recipient-key lookup + bucket encryption + key verification
    // before enabling this arm.
    if delivery == Delivery::Secure {
        return Err("confidential_send_unsupported");
    }
    if !ordinary_confirmed {
        return Err("ordinary_delivery_requires_consent");
    }
    let addresses: Vec<String> = to
        .iter()
        .chain(cc.iter())
        .chain(bcc.iter())
        .map(|r| r.address.clone())
        .collect();
    if addresses.is_empty()
        || addresses.iter().any(|a| !valid_address(a))
        || reply_tos.iter().any(|r| !valid_address(&r.address))
    {
        return Err("invalid_recipient");
    }
    if body_html.as_bytes().len() > MAX_BODY_BYTES {
        return Err("body_too_large");
    }
    let randomizer = RandomizerFacade::from_core(rand_core::OsRng);
    let mut staged = Vec::with_capacity(attachments.len());
    for attachment in &attachments {
        let bytes = STANDARD
            .decode(attachment.data.as_bytes())
            .map_err(|_| "attachment_failed")?;
        staged.push(StagedAttachment {
            name: attachment.name.clone(),
            mime_type: attachment.mime_type.clone(),
            cid: attachment.cid.clone(),
            bytes,
            session_key: GenericAesKey::Aes256(Aes256Key::generate(&randomizer)),
        });
    }
    // The sender must be one of the account's own addresses (primary or
    // alias); anything else is refused rather than sent from someone's name.
    let mail_group_id = match sdk
        .mail_facade()
        .get_group_id_for_mail_address(&sender)
        .await
    {
        Ok(group) => group,
        Err(ApiCallError::ServerResponseError {
            source: HttpError::NotFoundError,
        }) => return Err("sender_unknown"),
        Err(error) => {
            diag::failed("sender lookup", &error);
            return Err(match error {
                ApiCallError::RestClient { .. } => "transport_failed",
                _ => "send_failed",
            });
        }
    };
    let mail_group_key = sdk
        .get_current_sym_group_key(&mail_group_id)
        .await
        .map_err(diag::fail("sender group key", "send_failed"))?;
    // Decided before any server-side write (blob upload, draft, submit): a
    // send the helper cannot deliver as ordinary mail is refused here.
    let counts = ordinary_delivery_plan(&classify_recipients(&sdk, &addresses).await)?;
    let file_data: Vec<FileData> = staged
        .iter()
        .map(|s| FileData {
            session_key: s.session_key.clone(),
            data: &s.bytes,
        })
        .collect();
    // No attachments, no upload: the SDK would otherwise still fetch a blob
    // write token and post an upload of nothing (seen live).
    let reference_tokens = if file_data.is_empty() {
        Vec::new()
    } else {
        sdk.blob_facade()
            .encrypt_and_upload_multiple(
                ArchiveDataType::Attachments,
                &mail_group_id,
                file_data.iter(),
            )
            .await
            .map_err(diag::fail("blob upload", "attachment_failed"))?
    };
    if reference_tokens.len() != staged.len() {
        return Err("attachment_failed");
    }
    let mut added_attachments = Vec::with_capacity(staged.len());
    for (staged_attachment, tokens) in staged.iter().zip(reference_tokens) {
        let owner_enc = mail_group_key.encrypt_key(
            &staged_attachment.session_key,
            InitializationVector::generate(&randomizer),
        );
        let encrypt_meta = |text: &[u8]| {
            staged_attachment
                .session_key
                .encrypt_data(text, InitializationVector::generate(&randomizer))
        };
        let (main, sub) = normalise_mime(&staged_attachment.mime_type);
        let enc_cid = staged_attachment
            .cid
            .as_ref()
            .map(|cid| encrypt_meta(cid.as_bytes()))
            .transpose()
            .map_err(|_| "attachment_failed")?;
        added_attachments.push(DraftAttachment {
            _id: None,
            ownerEncFileSessionKey: owner_enc.object,
            ownerKeyVersion: i64::try_from(owner_enc.version).map_err(|_| "send_failed")?,
            newFile: Some(NewDraftAttachment {
                _id: None,
                encFileName: encrypt_meta(staged_attachment.name.as_bytes())
                    .map_err(|_| "attachment_failed")?,
                encMimeType: encrypt_meta(format!("{main}/{sub}").as_bytes())
                    .map_err(|_| "attachment_failed")?,
                encCid: enc_cid,
                referenceTokens: tokens,
            }),
            existingFile: None,
        });
    }
    let mail_session_key = GenericAesKey::Aes256(Aes256Key::generate(&randomizer));
    let owner_enc = mail_group_key.encrypt_key(
        &mail_session_key,
        InitializationVector::generate(&randomizer),
    );
    let created = sdk
        .get_service_executor()
        .post::<DraftService>(
            DraftCreateData {
                _format: 0,
                previousMessageId: None,
                conversationType: CONVERSATION_NEW,
                ownerEncSessionKey: owner_enc.object,
                ownerKeyVersion: i64::try_from(owner_enc.version).map_err(|_| "send_failed")?,
                draftData: draft_payload(
                    subject,
                    body_html,
                    sender,
                    sender_name,
                    draft_recipients(&to),
                    draft_recipients(&cc),
                    draft_recipients(&bcc),
                    reply_tos
                        .iter()
                        .map(|r| EncryptedMailAddress {
                            _id: None,
                            name: encrypted_name(&r.name, &r.address),
                            address: r.address.clone(),
                            _errors: Default::default(),
                        })
                        .collect(),
                    added_attachments,
                ),
                _errors: Default::default(),
            },
            ExtraServiceParams {
                session_key: Some(mail_session_key.clone()),
                ..Default::default()
            },
        )
        .await
        .map_err(diag::fail(
            "draft create (DraftService post)",
            "send_failed",
        ))?;
    let draft = created.draft;
    // From here on every failure attempts to remove the staged draft, so a
    // failed send leaves no orphan behind. The outbox row stays the recovery
    // point either way.
    let mail: Mail = match sdk
        .mail_facade()
        .get_crypto_entity_client()
        .load(&draft)
        .await
    {
        Ok(mail) => mail,
        Err(error) => {
            diag::failed("draft load", &error);
            cleanup_draft(sdk.clone(), &draft).await;
            return Err("send_failed");
        }
    };
    let attachment_keys = match file_session_keys(&sdk, &mail).await {
        Ok(keys) => keys,
        Err(()) => {
            diag::note("attachment keys failed");
            cleanup_draft(sdk.clone(), &draft).await;
            return Err("send_failed");
        }
    };
    let reply = match sdk
        .get_service_executor()
        .post::<SendDraftService>(
            ordinary_send_payload(
                draft.clone(),
                mail_session_key.as_bytes().to_vec(),
                attachment_keys,
                &randomizer,
            ),
            ExtraServiceParams::default(),
        )
        .await
    {
        Ok(reply) => reply,
        Err(error) => {
            let submit = transport::submit();
            diag::failed("send draft (SendDraftService post)", &error);
            diag::note(&format!("submit on the wire: {submit:?}"));
            return match submit_outcome(submit) {
                Ok(()) => Ok(sent_reply(None, counts)),
                // The server may still be sending it: leave the draft alone.
                Err(code @ "send_uncertain") => Err(code),
                Err(code) => {
                    cleanup_draft(sdk.clone(), &draft).await;
                    Err(code)
                }
            };
        }
    };
    Ok(sent_reply(Some(&reply), counts))
}

/// Each attachment file's session key, resolved from the server like the
/// official client does -- never by assuming the server lists the files in
/// upload order. A mismatched key would corrupt the attachment.
async fn file_session_keys(
    sdk: &LoggedInSdk,
    mail: &Mail,
) -> std::result::Result<Vec<AttachmentKeyData>, ()> {
    if mail.attachments.is_empty() {
        return Ok(Vec::new());
    }
    let type_ref = TutanotaFile::type_ref();
    let client = sdk.get_entity_client();
    let model = client
        .resolve_server_type_ref(&type_ref)
        .map_err(|error| diag::failed("attachment keys", &error))?;
    let mut lists: Vec<(GeneratedId, Vec<GeneratedId>)> = Vec::new();
    for file_id in &mail.attachments {
        match lists.iter_mut().find(|(list, _)| *list == file_id.list_id) {
            Some((_, elements)) if elements.contains(&file_id.element_id) => return Err(()),
            Some((_, elements)) => elements.push(file_id.element_id.clone()),
            None => lists.push((file_id.list_id.clone(), vec![file_id.element_id.clone()])),
        }
    }
    let crypto = sdk
        .mail_facade()
        .get_crypto_entity_client()
        .get_crypto_facade()
        .clone();
    let mut keys = HashMap::new();
    for (list, elements) in &lists {
        let loaded = client
            .load_multiple(&type_ref, list, elements)
            .await
            .map_err(|error| diag::failed("attachment keys", &error))?;
        for parsed in loaded {
            let file_id = list_element_id(&model, &parsed).ok_or(())?;
            if file_id.list_id != *list || keys.contains_key(&id(&file_id)) {
                return Err(());
            }
            let resolved = crypto
                .resolve_session_key(&parsed, &model)
                .await
                .map_err(|error| diag::failed("attachment keys", &error))?
                .ok_or(())?;
            keys.insert(id(&file_id), resolved.session_key);
        }
    }
    mail.attachments
        .iter()
        .map(|file_id| {
            keys.remove(&id(file_id)).map(|key| AttachmentKeyData {
                _id: None,
                bucketEncFileSessionKey: None,
                fileSessionKey: Some(key.as_bytes().to_vec()),
                file: file_id.clone(),
            })
        })
        .collect::<Option<Vec<_>>>()
        .ok_or(())
}

async fn dispatch(request: Request) -> Result<Value> {
    if request.version != VERSION {
        return Err("unsupported_version");
    }
    let native =
        NativeRestClient::try_new().map_err(diag::fail("transport setup", "transport_failed"))?;
    let transport: Arc<dyn RestClient> = Arc::new(transport::Traced(Arc::new(native)));
    let sdk = Sdk::new(TUTA_URL.into(), transport, Arc::new(MemoryFiles::default()));
    match request.operation {
        Operation::Login {
            email,
            password,
            totp,
        } => login(&sdk, email, password, totp).await,
        Operation::Sync {
            session,
            folder,
            limit,
            offset,
        } => sync(resume(&sdk, session).await?, folder, limit, offset).await,
        Operation::Message { session, id } => message(resume(&sdk, session).await?, id).await,
        Operation::MarkRead {
            session,
            ids,
            is_read,
        } => {
            if ids.len() > MAIL_BATCH_LIMIT {
                return Err("invalid_batch");
            }
            let parsed = ids.iter().map(|s| tuple(s)).collect::<Result<Vec<_>>>()?;
            let unread = !is_read;
            let logged_in = resume(&sdk, session).await?;
            let mut applied = 0;
            for chunk in parsed.chunks(MAIL_BATCH_LIMIT) {
                if logged_in
                    .mail_facade()
                    .set_unread_status_for_mails(chunk.to_vec(), unread)
                    .await
                    .is_err()
                {
                    return Ok(outcomes(&ids, applied, Some("request_failed")));
                }
                applied += chunk.len();
            }
            Ok(outcomes(&ids, applied, None))
        }
        Operation::Move {
            session,
            ids,
            destination,
        } => {
            if ids.len() > MAIL_BATCH_LIMIT {
                return Err("invalid_batch");
            }
            let parsed = ids.iter().map(|s| tuple(s)).collect::<Result<Vec<_>>>()?;
            let destination = tuple(&destination)?;
            let logged_in = resume(&sdk, session).await?;
            let mut applied = 0;
            for chunk in parsed.chunks(MAIL_BATCH_LIMIT) {
                if MailExtensions::new(logged_in.clone())
                    .move_mails(chunk.to_vec(), destination.clone())
                    .await
                    .is_err()
                {
                    return Ok(outcomes(&ids, applied, Some("request_failed")));
                }
                applied += chunk.len();
            }
            Ok(outcomes(&ids, applied, None))
        }
        Operation::Send {
            session,
            sender,
            sender_name,
            to,
            cc,
            bcc,
            reply_tos,
            subject,
            body_html,
            delivery,
            ordinary_confirmed,
            attachments,
        } => {
            send(
                resume(&sdk, session).await?,
                sender,
                sender_name,
                to,
                cc,
                bcc,
                reply_tos,
                subject,
                body_html,
                delivery,
                ordinary_confirmed,
                attachments,
            )
            .await
        }
        Operation::Revoke { session } => revoke(&sdk, session).await,
    }
}
#[tokio::main]
async fn main() {
    // If stdout cannot be parked the request still runs; a chatty SDK only
    // risks the reply, it must not fail the operation itself.
    let quiet = quiet_stdout().ok();
    let mut input = Vec::new();
    let result = match io::stdin()
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut input)
    {
        Ok(_) if input.len() <= MAX_REQUEST_BYTES as usize => {
            match serde_json::from_slice::<Request>(&input) {
                Ok(request) => {
                    // A send that overruns the operation timeout after its
                    // submit went out may have delivered: what the submit got
                    // back decides, never a plain timeout the caller would
                    // read as "nothing happened". Before the submit, nothing
                    // was sent.
                    let is_send = matches!(request.operation, Operation::Send { .. });
                    match tokio::time::timeout(REQUEST_TIMEOUT, dispatch(request)).await {
                        Ok(result) => result,
                        Err(_) => timeout_outcome(is_send, transport::submit()),
                    }
                }
                Err(_) => Err("invalid_request"),
            }
        }
        _ => Err("invalid_request"),
    };
    let response = match result {
        Ok(result) => json!({"version": VERSION, "result": result}),
        Err(code) => json!({"version": VERSION, "error": {"code": code}}),
    };
    drop(quiet);
    println!("{response}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tutasdk::bindings::rest_client::RestClientError;

    fn resume_of(error: LoginError) -> &'static str {
        resume_error(error)
    }

    fn server(code: HttpError) -> LoginError {
        LoginError::ApiCall {
            source: ApiCallError::ServerResponseError { source: code },
        }
    }

    #[test]
    fn resume_distinguishes_revoked_outage_and_refused_version() {
        let token = || LoginError::InvalidAccessToken {
            error_message: String::new(),
        };
        assert_eq!(resume_of(token()), "session_revoked");
        assert_eq!(
            resume_of(server(HttpError::SessionExpiredError)),
            "session_revoked"
        );
        assert_eq!(
            resume_of(server(HttpError::NotAuthenticatedError)),
            "session_revoked"
        );
        assert_eq!(
            resume_of(server(HttpError::InvalidSoftwareVersionError)),
            "client_outdated"
        );
        assert_eq!(
            resume_of(LoginError::ApiCall {
                source: ApiCallError::RestClient {
                    source: RestClientError::NetworkError,
                },
            }),
            "transport_failed"
        );
        assert_eq!(
            resume_of(server(HttpError::ServiceUnavailableError {
                suspension_time_sec: None,
            })),
            "transport_failed"
        );
        assert_eq!(resume_of(server(HttpError::RequestTimeoutError)), "timeout");
        assert_eq!(
            resume_of(server(HttpError::BadRequestError)),
            "resume_failed"
        );
    }

    #[test]
    fn a_sign_in_without_a_server_is_not_a_wrong_password() {
        assert_eq!(
            initiate_error(LoginError::ApiCall {
                source: ApiCallError::RestClient {
                    source: RestClientError::NetworkError,
                },
            }),
            "transport_failed"
        );
        assert_eq!(
            initiate_error(server(HttpError::InvalidSoftwareVersionError)),
            "client_outdated"
        );
        assert_eq!(
            initiate_error(server(HttpError::NotAuthenticatedError)),
            "login_failed"
        );
    }

    #[test]
    fn corrupt_local_material_is_not_a_revoked_session() {
        assert_eq!(
            resume_of(LoginError::InvalidPassphrase {
                error_message: String::new(),
            }),
            "session_invalid"
        );
        assert_eq!(
            resume_of(LoginError::InvalidKey {
                error_message: String::new(),
            }),
            "session_invalid"
        );
    }

    #[test]
    fn blocking_second_factor_is_named_honestly() {
        assert_eq!(unsupported_factor_code(&[0]), "unsupported_u2f");
        assert_eq!(unsupported_factor_code(&[0, 0]), "unsupported_u2f");
        assert_eq!(unsupported_factor_code(&[2]), "unsupported_webauthn");
        assert_eq!(
            unsupported_factor_code(&[0, 2]),
            "unsupported_second_factor"
        );
        assert_eq!(unsupported_factor_code(&[7]), "unsupported_second_factor");
        assert_eq!(unsupported_factor_code(&[]), "unsupported_second_factor");
    }

    #[test]
    fn session_id_comes_from_the_token_prefix() {
        assert_eq!(session_id_from_token("!!!not-base64!!!"), None);
        assert_eq!(session_id_from_token("dG9v"), None);
        let raw: Vec<u8> = (0..16).collect();
        let token = URL_SAFE_NO_PAD.encode(&raw);
        let parsed = session_id_from_token(&token).unwrap();
        assert_eq!(
            BASE64_EXT.decode(parsed.list_id.to_string()).unwrap(),
            raw[..9]
        );
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(parsed.element_id.to_string())
                .unwrap(),
            sha256(&raw[9..])
        );
    }

    #[test]
    fn ids_preserve_both_components() {
        assert_eq!(id(&tuple("list_1/mail-2").unwrap()), "list_1/mail-2");
        for value in ["mail", "/mail", "list/", "a/b/c", "a/../b", "a/é"] {
            assert!(tuple(value).is_err());
        }
    }
    #[test]
    fn unknown_folder_types_stay_browsable() {
        assert_eq!(role(1), ("inbox", "Inbox"));
        assert_eq!(role(99), ("other", "Folder"));
    }
    #[test]
    fn request_rejects_unrecognized_operations() {
        assert!(serde_json::from_str::<Request>(r#"{"version":1,"op":"star"}"#).is_err());
    }
    #[test]
    fn send_parses_its_delivery_choice() {
        let request: Request = serde_json::from_str(
            r#"{"version":1,"op":"send","session":{"login":"a@tuta.com","user_id":"u",
            "access_token":"t","encrypted_passphrase_key":[]},
            "sender":"a@tuta.com","sender_name":"A","to":[],"cc":[],"bcc":[],
            "reply_tos":[],"subject":"s","body_html":"b","delivery":"ordinary",
            "ordinary_confirmed":true,"attachments":[]}"#,
        )
        .unwrap();
        assert!(matches!(
            request.operation,
            Operation::Send {
                delivery: Delivery::Ordinary,
                ordinary_confirmed: true,
                ..
            }
        ));
        assert!(serde_json::from_str::<Request>(
            r#"{"version":1,"op":"send","session":{"login":"a@tuta.com","user_id":"u",
            "access_token":"t","encrypted_passphrase_key":[]},
            "sender":"a@tuta.com","sender_name":"A","to":[],"cc":[],"bcc":[],
            "reply_tos":[],"subject":"s","body_html":"b","delivery":"whenever",
            "ordinary_confirmed":true,"attachments":[]}"#
        )
        .is_err());
    }
    #[test]
    fn address_validation_rejects_only_the_shapeless() {
        for address in ["ada@tuta.com", "a+b@x.io", "Ada@Tuta.Com"] {
            assert!(valid_address(address), "{address}");
        }
        for address in [
            "",
            "ada",
            "@tuta.com",
            "ada@",
            "a@b@c",
            "a d@x.io",
            "a@x.io\n",
            "\t@x.io",
        ] {
            assert!(!valid_address(address), "{address:?}");
        }
    }
    #[test]
    fn mime_fallback_matches_the_read_path() {
        assert_eq!(normalise_mime("text/html"), ("text", "html"));
        assert_eq!(normalise_mime("noslash"), ("application", "octet-stream"));
        assert_eq!(normalise_mime("/html"), ("application", "octet-stream"));
        assert_eq!(normalise_mime("text/"), ("application", "octet-stream"));
    }
    #[test]
    fn batch_outcomes_report_the_successful_prefix() {
        let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let full = outcomes(&ids, 3, None);
        assert_eq!(
            full["outcomes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["code"].is_null())
                .collect::<Vec<_>>(),
            vec![true, true, true]
        );
        let partial = outcomes(&ids, 1, Some("request_failed"));
        let codes: Vec<Value> = partial["outcomes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["code"].clone())
            .collect();
        assert!(codes[0].is_null());
        assert_eq!(codes[1], json!("request_failed"));
        assert_eq!(codes[2], json!("not_attempted"));
        let empty = outcomes(&[], 0, None);
        assert!(empty["outcomes"].as_array().unwrap().is_empty());
    }
    #[test]
    fn a_submit_that_never_left_sent_nothing() {
        // A failed serialization or encryption, or a request the HTTP client
        // could not even build: the server never saw it.
        assert_eq!(submit_outcome(Submit::NotSent), Err("send_failed"));
    }
    #[test]
    fn only_a_submit_lost_in_flight_or_failing_server_side_is_uncertain() {
        assert_eq!(submit_outcome(Submit::NoStatus), Err("send_uncertain"));
        for status in [500, 502, 503, 504, 0, 302] {
            assert_eq!(
                submit_outcome(Submit::Answered(status)),
                Err("send_uncertain"),
                "{status}"
            );
        }
    }
    #[test]
    fn a_refusal_is_definitive_whatever_the_sdk_made_of_it() {
        // 412 is what the SDK turns into an InternalSdkError when it does not
        // know the precondition reason; the status alone decides here.
        for status in [
            400, 403, 404, 405, 408, 412, 413, 423, 429, 470, 471, 472, 473, 475, 507,
        ] {
            assert_eq!(
                submit_outcome(Submit::Answered(status)),
                Err("send_failed"),
                "{status}"
            );
        }
        assert_eq!(
            submit_outcome(Submit::Answered(401)),
            Err("session_revoked")
        );
        assert_eq!(
            submit_outcome(Submit::Answered(440)),
            Err("session_revoked")
        );
        assert_eq!(
            submit_outcome(Submit::Answered(474)),
            Err("client_outdated")
        );
    }
    #[test]
    fn an_accepted_submit_with_an_unreadable_reply_was_sent() {
        assert_eq!(submit_outcome(Submit::Answered(200)), Ok(()));
        assert_eq!(submit_outcome(Submit::Answered(201)), Ok(()));
        let reply = sent_reply(None, Some((0, 1)));
        assert!(reply["sent_mail"].is_null());
        assert_eq!(reply["external_recipients"], json!(1));
    }
    #[test]
    fn tuta_domains_are_internal_without_a_lookup() {
        for address in [
            "ada@tuta.io",
            "Ada@TUTA.COM",
            "a@tutanota.de",
            "a@keemail.me",
        ] {
            assert!(is_tuta_address(address), "{address}");
        }
        for address in [
            "ada@example.com",
            "ada@nottuta.io",
            "tuta.io",
            "ada@tuta.io.evil",
        ] {
            assert!(!is_tuta_address(address), "{address}");
        }
    }
    #[test]
    fn an_all_internal_send_is_refused_before_anything_is_written() {
        use RecipientKind::*;
        // A send to one's own Tuta address: the official client would make it
        // end-to-end encrypted, which this helper cannot -- refuse, never
        // submit it as ordinary mail.
        assert_eq!(
            ordinary_delivery_plan(&[Internal]),
            Err("confidential_send_unsupported")
        );
        assert_eq!(
            ordinary_delivery_plan(&[Internal, Internal]),
            Err("confidential_send_unsupported")
        );
        assert_eq!(
            ordinary_delivery_plan(&[]),
            Err("confidential_send_unsupported")
        );
    }
    #[test]
    fn an_external_recipient_keeps_ordinary_delivery() {
        use RecipientKind::*;
        assert_eq!(ordinary_delivery_plan(&[External]), Ok(Some((0, 1))));
        assert_eq!(
            ordinary_delivery_plan(&[Internal, External]),
            Ok(Some((1, 1)))
        );
        // A failed lookup leaves the counts unknown, not the send.
        assert_eq!(
            ordinary_delivery_plan(&[External, Unknown("transport_failed")]),
            Ok(None)
        );
    }
    #[test]
    fn an_undecidable_mode_refuses_definitively() {
        use RecipientKind::*;
        assert_eq!(
            ordinary_delivery_plan(&[Internal, Unknown("transport_failed")]),
            Err("transport_failed")
        );
        assert_eq!(
            ordinary_delivery_plan(&[Unknown("send_failed")]),
            Err("send_failed")
        );
    }
    #[test]
    fn only_a_timeout_after_submit_can_be_uncertain() {
        assert_eq!(timeout_outcome(true, Submit::NotSent), Err("timeout"));
        assert_eq!(timeout_outcome(false, Submit::NoStatus), Err("timeout"));
        assert_eq!(timeout_outcome(false, Submit::NotSent), Err("timeout"));
        assert_eq!(
            timeout_outcome(true, Submit::NoStatus),
            Err("send_uncertain")
        );
        assert_eq!(
            timeout_outcome(true, Submit::Answered(400)),
            Err("send_failed")
        );
        assert!(timeout_outcome(true, Submit::Answered(200)).is_ok());
    }
    #[test]
    fn draft_payload_never_marks_ordinary_mail_confidential() {
        let payload = draft_payload(
            "subject".to_string(),
            "<p>body</p>".to_string(),
            "ada@tuta.com".to_string(),
            "Ada".to_string(),
            vec![DraftRecipient {
                _id: None,
                name: "Bo".to_string(),
                mailAddress: "bo@example.com".to_string(),
                _errors: Default::default(),
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert!(!payload.confidential);
        assert_eq!(payload.compressedBodyText.as_deref(), Some("<p>body</p>"));
        assert_eq!(payload.method, METHOD_NONE);
        assert_eq!(payload.toRecipients.len(), 1);
        assert_eq!(payload.senderName, "Ada");
    }
    #[test]
    fn no_encrypted_draft_text_is_left_empty() {
        // The pinned SDK sends an empty encrypted string as a bare "", which
        // the send service fails on: names fall back to the address and the
        // body rides in bodyText as well.
        let unnamed = [SendRecipient {
            name: String::new(),
            address: "bo@example.com".to_string(),
        }];
        let payload = draft_payload(
            "subject".to_string(),
            "<p>body</p>".to_string(),
            "ada@tuta.com".to_string(),
            String::new(),
            draft_recipients(&unnamed),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(payload.senderName, "ada@tuta.com");
        assert_eq!(payload.toRecipients[0].name, "bo@example.com");
        assert_eq!(payload.bodyText, "<p>body</p>");
        assert_eq!(encrypted_name("Bo", "bo@example.com"), "Bo");
    }
    #[test]
    fn ordinary_send_payload_carries_clear_keys_and_no_bucket() {
        let randomizer = RandomizerFacade::from_core(rand_core::OsRng);
        let draft = IdTupleGenerated::new(GeneratedId("list".into()), GeneratedId("mail".into()));
        let payload = ordinary_send_payload(draft, vec![7; 32], Vec::new(), &randomizer);
        assert_eq!(payload.mailSessionKey, Some(vec![7; 32]));
        assert!(payload.bucketEncMailSessionKey.is_none());
        assert!(payload.internalRecipientKeyData.is_empty());
        assert!(payload.secureExternalRecipientKeyData.is_empty());
        assert!(payload.symEncInternalRecipientKeyData.is_empty());
        assert!(!payload.plaintext);
        let parameters = payload.parameters.unwrap();
        assert_eq!(parameters.mailSessionKey, Some(vec![7; 32]));
        assert_eq!(parameters.language, SEND_LANGUAGE);
    }
    #[test]
    fn every_send_payload_aggregate_carries_an_id() {
        // SendDraftData is not encrypted, so the SDK never fills a missing
        // aggregate id in, and its serializer refuses the submit before it
        // is sent ("Invalid value: ... TypeId(1788) _id", seen live).
        let randomizer = RandomizerFacade::from_core(rand_core::OsRng);
        let draft = IdTupleGenerated::new(GeneratedId("list".into()), GeneratedId("mail".into()));
        let key = |element: &str| AttachmentKeyData {
            _id: None,
            bucketEncFileSessionKey: None,
            fileSessionKey: Some(vec![1; 32]),
            file: IdTupleGenerated::new(GeneratedId("files".into()), GeneratedId(element.into())),
        };
        let payload =
            ordinary_send_payload(draft, vec![7; 32], vec![key("a"), key("b")], &randomizer);
        let parameters = payload.parameters.as_ref().unwrap();
        let ids: Vec<&CustomId> = std::iter::once(&parameters._id)
            .chain(payload.attachmentKeyData.iter().map(|k| &k._id))
            .chain(parameters.attachmentKeyData.iter().map(|k| &k._id))
            .map(|id| id.as_ref().expect("aggregate without an id"))
            .collect();
        assert_eq!(ids.len(), 5);
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
    fn attachment_metadata_round_trips_through_the_sdk_cipher() {
        // Offline proof that what the send path encrypts, the SDK's own
        // decrypt path reads back: same functions the official client's
        // format depends on.
        let randomizer = RandomizerFacade::from_core(rand_core::OsRng);
        let key = GenericAesKey::Aes256(Aes256Key::generate(&randomizer));
        for text in ["résumé.pdf", "text/html", "part1"] {
            let encrypted = key
                .encrypt_data(text.as_bytes(), InitializationVector::generate(&randomizer))
                .unwrap();
            assert_eq!(key.decrypt_data(&encrypted).unwrap(), text.as_bytes());
        }
        let wrapped = key.encrypt_key(&key, InitializationVector::generate(&randomizer));
        assert!(!wrapped.is_empty());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn stdout_is_parked_on_dev_null_while_held() {
        use std::os::unix::fs::MetadataExt;
        let target = |path: &str| {
            let meta = std::fs::metadata(path).unwrap();
            (meta.dev(), meta.ino())
        };
        let (null, before) = (target("/dev/null"), target("/proc/self/fd/1"));
        {
            let _quiet = quiet_stdout().unwrap();
            assert_eq!(target("/proc/self/fd/1"), null);
        }
        assert_eq!(target("/proc/self/fd/1"), before);
    }
    #[tokio::test]
    async fn incompatible_version_never_connects() {
        let request = serde_json::from_str(
            r#"{"version":2,"op":"login","email":"a@b","password":"fixture"}"#,
        )
        .unwrap();
        assert_eq!(dispatch(request).await.unwrap_err(), "unsupported_version");
    }
}
