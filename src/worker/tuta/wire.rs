//! The `hylki-tuta` helper's operations, and its replies checked as
//! untrusted input: a missing key or a mistyped value is
//! `invalid_response`, never a panic or a guess.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde_json::Value;

use crate::worker::helper::{HelperClient, HelperError, HelperSpec, Object, HELPER_TIMEOUT};

/// The helper's executable name.
pub(super) const HELPER_NAME: &str = "hylki-tuta";

/// A reply is JSON whose bulk is base64 bodies and attachments; its size is
/// checked before it is parsed, so an unbounded one never costs that.
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// The codes the helper may answer with (its `main.rs`), plus the ones
/// [`crate::worker::helper`] makes itself. Anything else is
/// `request_failed`.
const ERROR_CODES: &[&str] = &[
    "helper_missing",
    "helper_failed",
    "response_too_large",
    "invalid_response",
    "unsupported_version",
    "invalid_request",
    "invalid_id",
    "invalid_page",
    "invalid_batch",
    "invalid_thread",
    "invalid_recipient",
    "sender_unknown",
    "body_too_large",
    "recipient_not_found",
    "secure_external_unsupported",
    "ordinary_delivery_requires_consent",
    "send_failed",
    "send_uncertain",
    "login_failed",
    "resume_failed",
    "session_revoked",
    "session_invalid",
    "client_outdated",
    "revoke_failed",
    "second_factor_required",
    "invalid_totp",
    "unsupported_second_factor",
    "unsupported_u2f",
    "unsupported_webauthn",
    "second_factor_failed",
    "request_failed",
    "folder_not_found",
    "folder_failed",
    "folder_exists",
    "invalid_parent",
    "not_custom_folder",
    "limit_reached",
    "label_exists",
    "not_a_draft",
    "invalid_name",
    "invalid_color",
    "label_not_found",
    "label_failed",
    "draft_not_found",
    "draft_failed",
    "not_trash_or_spam",
    "delete_failed",
    "operation_still_active",
    "import_invalid",
    "import_too_large",
    "import_refused",
    "import_failed",
    "watch_failed",
    "incomplete_response",
    "unsupported_legacy_body",
    "decryption_failed",
    "attachment_failed",
    "transport_failed",
    "timeout",
    "unsupported_operation",
];

pub(in crate::worker) const SPEC: HelperSpec = HelperSpec {
    name: HELPER_NAME,
    codes: ERROR_CODES,
    max_response_bytes: MAX_RESPONSE_BYTES,
    timeout: HELPER_TIMEOUT,
    rotates_sessions: false,
};

/// The helper applies at most this many ids per mutation and refuses a
/// larger batch outright, so callers chunk first.
pub(super) const BATCH_LIMIT: usize = 50;

/// The largest message the helper imports (its `import::MAX_IMPORT_BYTES`);
/// a larger one is refused here rather than sent over the pipe for nothing.
pub(super) const MAX_IMPORT_BYTES: usize = 25 * 1024 * 1024;

fn invalid() -> HelperError {
    HelperError::new("invalid_response")
}

/// One folder as a sync lists it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct FolderRow {
    /// The compound `listId/elementId` the helper takes back.
    pub(super) id: String,
    /// `inbox`, `sent`, `trash`, `archive`, `junk`, `drafts` or `other`.
    pub(super) role: String,
    pub(super) label: String,
    /// The parent folder's id, empty at the top.
    pub(super) parent: String,
}

/// One of the account's labels, as a sync lists them beside the folders.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct LabelRow {
    /// The compound id a header's `labels` names it by.
    pub(super) id: String,
    pub(super) name: String,
    /// `#rrggbb`, or empty.
    pub(super) color: String,
}

/// One page of a sync: the folder tree, a folder's headers from `offset`,
/// and on the first page every id the folder holds, newest first.
#[derive(Debug, Clone)]
pub(super) struct SyncPage {
    pub(super) folders: Vec<FolderRow>,
    /// The id of the folder listed (the Inbox when none was asked for).
    pub(super) folder: String,
    pub(super) headers: Vec<Value>,
    /// The folder's complete id snapshot, only on offset zero. Checked
    /// before anything is pruned from it: a corrupt one would empty the
    /// folder's cache.
    pub(super) all_ids: Option<Vec<String>>,
    /// The account's labels; `None` from a helper that does not list them.
    pub(super) labels: Option<Vec<LabelRow>>,
}

fn string_field(row: &Object, key: &str) -> String {
    row.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Read a sync reply.
pub(super) fn sync_page(result: &Object) -> Result<SyncPage, HelperError> {
    let Some(Value::Array(rows)) = result.get("folders") else {
        return Err(invalid());
    };
    let folders = rows
        .iter()
        .map(|row| {
            let Value::Object(row) = row else {
                return Err(invalid());
            };
            let id = string_field(row, "id");
            if id.is_empty() {
                return Err(invalid());
            }
            Ok(FolderRow {
                id,
                role: string_field(row, "role"),
                label: string_field(row, "label"),
                parent: string_field(row, "parent"),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let labels = match result.get("labels") {
        None => None,
        Some(Value::Array(rows)) => Some(
            rows.iter()
                .map(|row| {
                    let Value::Object(row) = row else {
                        return Err(invalid());
                    };
                    let (id, name) = (string_field(row, "id"), string_field(row, "name"));
                    if id.is_empty() || name.is_empty() {
                        return Err(invalid());
                    }
                    Ok(LabelRow {
                        id,
                        name,
                        color: string_field(row, "color"),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Some(_) => return Err(invalid()),
    };
    let folder = match result.get("folder") {
        Some(Value::String(id)) if !id.is_empty() => id.clone(),
        _ => return Err(invalid()),
    };
    let headers = match result.get("messages") {
        Some(Value::Array(headers)) => headers.clone(),
        _ => return Err(invalid()),
    };
    let all_ids = match result.get("all_ids") {
        None | Some(Value::Null) => None,
        Some(Value::Array(ids)) => Some(
            ids.iter()
                .map(|id| match id {
                    Value::String(id) if !id.is_empty() => Ok(id.clone()),
                    _ => Err(invalid()),
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Some(_) => return Err(invalid()),
    };
    Ok(SyncPage {
        folders,
        folder,
        headers,
        all_ids,
        labels,
    })
}

/// A page of `folder` (the Inbox when `None`): `limit` headers from
/// `offset`. Blocking.
pub(super) fn sync(
    client: &HelperClient,
    folder: Option<&str>,
    limit: usize,
    offset: usize,
) -> Result<SyncPage, HelperError> {
    let mut parameters = Object::new();
    parameters.insert("folder".into(), folder.map_or(Value::Null, Value::from));
    parameters.insert("limit".into(), limit.into());
    parameters.insert("offset".into(), offset.into());
    sync_page(&client.request("sync", parameters)?)
}

/// One message, decrypted, as the helper gives it: its header, HTML body,
/// recipients, original headers and attachments. Blocking.
pub(super) fn message(client: &HelperClient, id: &str) -> Result<Object, HelperError> {
    let mut parameters = Object::new();
    parameters.insert("id".into(), id.into());
    client.request("message", parameters)
}

/// One id's part of a mutation reply.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Outcome {
    /// Why it was not applied; `None` where it was.
    pub(super) code: Option<String>,
    /// The id a move left it under, where the helper says. A Tuta mail
    /// keeps its id when it moves, so this is the same id; it is still
    /// read rather than assumed.
    pub(super) new_id: Option<String>,
}

/// The outcome of each id of a mutation reply, in the order asked. The
/// helper applies a prefix, reports the first failure's code and
/// `not_attempted` for the rest; a reply that does not line up with the
/// request is corruption, not a partial success.
pub(super) fn outcomes(result: &Object, ids: &[String]) -> Result<Vec<Outcome>, HelperError> {
    let Some(Value::Array(rows)) = result.get("outcomes") else {
        return Err(invalid());
    };
    if rows.len() != ids.len() {
        return Err(invalid());
    }
    rows.iter()
        .zip(ids)
        .map(|(row, id)| {
            if row.get("id").and_then(Value::as_str) != Some(id.as_str()) {
                return Err(invalid());
            }
            let code = match row.get("code") {
                None | Some(Value::Null) => None,
                Some(Value::String(code)) => Some(code.clone()),
                Some(_) => return Err(invalid()),
            };
            let new_id = match row.get("new_id") {
                None | Some(Value::Null) => None,
                Some(Value::String(new)) if !new.is_empty() => Some(new.clone()),
                Some(_) => return Err(invalid()),
            };
            Ok(Outcome { code, new_id })
        })
        .collect()
}

/// A code a reply row gave, as an error: one off the list reads as
/// `request_failed`, as a reply's own error code does.
fn row_error(code: &str) -> HelperError {
    if ERROR_CODES.contains(&code) {
        HelperError::new(code)
    } else {
        HelperError::new("request_failed")
    }
}

/// Run a mutation over `ids` in batches, stopping at the first failure:
/// each id applied with the id the helper says it now has, and the
/// failure when there was one. Blocking.
pub(super) fn mutate_rows(
    client: &HelperClient,
    operation: &str,
    ids: &[String],
    extra: &Object,
) -> (Vec<(String, Option<String>)>, Option<HelperError>) {
    let mut applied = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(BATCH_LIMIT) {
        let mut parameters = extra.clone();
        parameters.insert(
            "ids".into(),
            Value::Array(chunk.iter().map(|id| Value::from(id.as_str())).collect()),
        );
        let rows = match client
            .request(operation, parameters)
            .and_then(|result| outcomes(&result, chunk))
        {
            Ok(rows) => rows,
            Err(e) => return (applied, Some(e)),
        };
        for (id, row) in chunk.iter().zip(rows) {
            match row.code {
                None => applied.push((id.clone(), row.new_id)),
                Some(code) => return (applied, Some(row_error(&code))),
            }
        }
    }
    (applied, None)
}

/// [`mutate_rows`], the ids applied alone.
pub(super) fn mutate(
    client: &HelperClient,
    operation: &str,
    ids: &[String],
    extra: &Object,
) -> (Vec<String>, Option<HelperError>) {
    let (applied, error) = mutate_rows(client, operation, ids, extra);
    (applied.into_iter().map(|(id, _)| id).collect(), error)
}

/// Mark `ids` read or unread. Blocking.
pub(super) fn mark_read(
    client: &HelperClient,
    ids: &[String],
    read: bool,
) -> (Vec<String>, Option<HelperError>) {
    let mut extra = Object::new();
    extra.insert("is_read".into(), read.into());
    mutate(client, "mark_read", ids, &extra)
}

/// Move `ids` into the folder `destination`: each id moved, with the id
/// the helper says it has there. Blocking.
pub(super) fn move_ids(
    client: &HelperClient,
    ids: &[String],
    destination: &str,
) -> (Vec<(String, Option<String>)>, Option<HelperError>) {
    let mut extra = Object::new();
    extra.insert("destination".into(), destination.into());
    mutate_rows(client, "move", ids, &extra)
}

/// Erase `ids`, which are in `folder` (the Trash or Spam: the server
/// erases nothing anywhere else). Blocking.
pub(super) fn delete_ids(
    client: &HelperClient,
    ids: &[String],
    folder: &str,
) -> (Vec<String>, Option<HelperError>) {
    let mut extra = Object::new();
    extra.insert("folder".into(), folder.into());
    mutate(client, "delete", ids, &extra)
}

/// Erase everything in `folder` (the Trash or Spam): how many messages it
/// held. Blocking.
pub(super) fn empty_folder(client: &HelperClient, folder: &str) -> Result<u64, HelperError> {
    let mut parameters = Object::new();
    parameters.insert("id".into(), folder.into());
    client
        .request("empty_folder", parameters)?
        .get("deleted")
        .and_then(Value::as_u64)
        .ok_or_else(invalid)
}

/// The non-empty `id` a reply names.
fn reply_id(result: &Object) -> Result<String, HelperError> {
    match result.get("id") {
        Some(Value::String(id)) if !id.is_empty() => Ok(id.clone()),
        _ => Err(invalid()),
    }
}

/// Make a folder named `name` under `parent` (at the top for `None`): its
/// id. Blocking.
pub(super) fn create_folder(
    client: &HelperClient,
    name: &str,
    parent: Option<&str>,
) -> Result<String, HelperError> {
    let mut parameters = Object::new();
    parameters.insert("name".into(), name.into());
    if let Some(parent) = parent {
        parameters.insert("parent".into(), parent.into());
    }
    reply_id(&client.request("create_folder", parameters)?)
}

/// Rename the folder `id`, move it under `parent` ("" for the top), or
/// both; `None` leaves that one as it is. Blocking.
pub(super) fn rename_folder(
    client: &HelperClient,
    id: &str,
    name: Option<&str>,
    parent: Option<&str>,
) -> Result<(), HelperError> {
    let mut parameters = Object::new();
    parameters.insert("id".into(), id.into());
    if let Some(name) = name {
        parameters.insert("name".into(), name.into());
    }
    if let Some(parent) = parent {
        parameters.insert("parent".into(), parent.into());
    }
    client.request("rename_folder", parameters).map(|_| ())
}

/// Delete the folder `id`. As in Tuta's own app, one in the Trash or Spam
/// goes for good, an empty one too, and one holding mail goes to the Trash
/// with it: whether it went to the Trash. Blocking.
pub(super) fn delete_folder(client: &HelperClient, id: &str) -> Result<bool, HelperError> {
    let mut parameters = Object::new();
    parameters.insert("id".into(), id.into());
    match client
        .request("delete_folder", parameters)?
        .get("outcome")
        .and_then(Value::as_str)
    {
        Some("deleted") => Ok(false),
        Some("trashed") => Ok(true),
        _ => Err(invalid()),
    }
}

/// Make a label named `name` in `color` (`#rrggbb`): its id. Blocking.
pub(super) fn create_label(
    client: &HelperClient,
    name: &str,
    color: &str,
) -> Result<String, HelperError> {
    let mut parameters = Object::new();
    parameters.insert("name".into(), name.into());
    parameters.insert("color".into(), color.into());
    reply_id(&client.request("create_label", parameters)?)
}

/// Put the labels `add` on `ids` and take `remove` off them. Blocking.
pub(super) fn apply_labels(
    client: &HelperClient,
    ids: &[String],
    add: &[String],
    remove: &[String],
) -> (Vec<String>, Option<HelperError>) {
    let list = |labels: &[String]| Value::from(labels.to_vec());
    let mut extra = Object::new();
    extra.insert("add".into(), list(add));
    extra.insert("remove".into(), list(remove));
    mutate(client, "apply_labels", ids, &extra)
}

/// Store a draft made of `parameters` (a send's, less its delivery), over
/// the draft `draft_id` when there is one: the draft's id. Blocking.
pub(super) fn save_draft(
    client: &HelperClient,
    mut parameters: Object,
    draft_id: Option<&str>,
) -> Result<String, HelperError> {
    if let Some(id) = draft_id {
        parameters.insert("draft_id".into(), id.into());
    }
    reply_id(&client.request("save_draft", parameters)?)
}

/// Delete the draft `id` for good. Blocking.
pub(super) fn delete_draft(client: &HelperClient, id: &str) -> Result<(), HelperError> {
    let mut parameters = Object::new();
    parameters.insert("id".into(), id.into());
    client.request("delete_draft", parameters).map(|_| ())
}

/// File the whole message `raw` in `folder`, as Tuta's own app imports a
/// mail file, unread or not: the new mail's id, which the helper leaves out
/// when the server took the mail but had not listed it yet. Blocking.
pub(super) fn import(
    client: &HelperClient,
    folder: &str,
    raw: &[u8],
    unread: bool,
) -> Result<Option<String>, HelperError> {
    if raw.len() > MAX_IMPORT_BYTES {
        return Err(HelperError::new("import_too_large"));
    }
    let mut parameters = Object::new();
    parameters.insert("folder".into(), folder.into());
    parameters.insert("mime_base64".into(), STANDARD.encode(raw).into());
    parameters.insert("unread".into(), unread.into());
    match client.request("import", parameters)?.get("id") {
        Some(Value::String(id)) if !id.is_empty() => Ok(Some(id.clone())),
        Some(Value::Null) => Ok(None),
        _ => Err(invalid()),
    }
}
