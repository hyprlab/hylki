//! The `hylki-tuta` helper's operations, and its replies checked as
//! untrusted input: a missing key or a mistyped value is
//! `invalid_response`, never a panic or a guess.

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
    "invalid_recipient",
    "sender_unknown",
    "body_too_large",
    "confidential_send_unsupported",
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
};

/// The helper applies at most this many ids per mutation and refuses a
/// larger batch outright, so callers chunk first.
pub(super) const BATCH_LIMIT: usize = 50;

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

/// The per-id codes of a mutation reply, in the order asked: `None` where
/// it was applied. The helper applies a prefix, reports the first failure's
/// code and `not_attempted` for the rest; a reply that does not line up
/// with the request is corruption, not a partial success.
pub(super) fn outcomes(
    result: &Object,
    ids: &[String],
) -> Result<Vec<Option<String>>, HelperError> {
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
            match row.get("code") {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(code)) => Ok(Some(code.clone())),
                Some(_) => Err(invalid()),
            }
        })
        .collect()
}

/// Run a mutation over `ids` in batches, stopping at the first failure:
/// the ids applied, and the failure's code when there was one. Blocking.
pub(super) fn mutate(
    client: &HelperClient,
    operation: &str,
    ids: &[String],
    extra: &Object,
) -> (Vec<String>, Option<HelperError>) {
    let mut applied = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(BATCH_LIMIT) {
        let mut parameters = extra.clone();
        parameters.insert(
            "ids".into(),
            Value::Array(chunk.iter().map(|id| Value::from(id.as_str())).collect()),
        );
        let codes = match client
            .request(operation, parameters)
            .and_then(|result| outcomes(&result, chunk))
        {
            Ok(codes) => codes,
            Err(e) => return (applied, Some(e)),
        };
        for (id, code) in chunk.iter().zip(codes) {
            match code {
                None => applied.push(id.clone()),
                Some(code) => return (applied, Some(HelperError::new(&code))),
            }
        }
    }
    (applied, None)
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

/// Move `ids` into the folder `destination`. The helper names no new id:
/// Tuta may file a moved mail in another list, so the destination is
/// listed again rather than guessed. Blocking.
pub(super) fn move_ids(
    client: &HelperClient,
    ids: &[String],
    destination: &str,
) -> (Vec<String>, Option<HelperError>) {
    let mut extra = Object::new();
    extra.insert("destination".into(), destination.into());
    mutate(client, "move", ids, &extra)
}
