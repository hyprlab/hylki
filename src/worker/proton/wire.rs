//! The `hylki-proton` helper's operations, and its replies checked as
//! untrusted input: a missing key or a mistyped value is
//! `invalid_response`, never a panic or a guess.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde_json::Value;

use crate::worker::helper::{HelperClient, HelperError, HelperSpec, Object, HELPER_TIMEOUT};

/// The helper's executable name.
pub(super) const HELPER_NAME: &str = "hylki-proton";

/// A reply is JSON whose bulk is a base64 message with its attachments;
/// its size is checked before it is parsed, so an unbounded one never costs
/// that.
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// The codes the helper may answer with (its README), plus the ones
/// [`crate::worker::helper`] makes itself. Anything else is
/// `request_failed`.
const ERROR_CODES: &[&str] = &[
    "helper_missing",
    "helper_failed",
    "response_too_large",
    "invalid_response",
    "unsupported_version",
    "invalid_request",
    "login_failed",
    "second_factor_required",
    "invalid_totp",
    "unsupported_second_factor",
    "second_factor_failed",
    "mailbox_password_required",
    "mailbox_password_failed",
    "human_verification_required",
    "human_verification_failed",
    "client_outdated",
    "session_invalid",
    "session_revoked",
    "revoke_failed",
    "transport_failed",
    "timeout",
    "request_failed",
    "invalid_id",
    "invalid_page",
    "invalid_batch",
    "folder_not_found",
    "invalid_destination",
    "decryption_failed",
    "attachment_failed",
    "invalid_recipient",
    "sender_unknown",
    "ordinary_delivery_requires_consent",
    "confidential_send_unsupported",
    "message_too_large",
    "send_failed",
    "send_uncertain",
];

pub(in crate::worker) const SPEC: HelperSpec = HelperSpec {
    name: HELPER_NAME,
    codes: ERROR_CODES,
    max_response_bytes: MAX_RESPONSE_BYTES,
    timeout: HELPER_TIMEOUT,
    rotates_sessions: true,
};

/// The helper applies at most this many ids per mutation and refuses a
/// larger batch outright, so callers chunk first.
pub(super) const BATCH_LIMIT: usize = 50;

fn invalid() -> HelperError {
    HelperError::new("invalid_response")
}

/// One folder (a system folder, a user folder or a user label) as a sync
/// lists it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct FolderRow {
    /// The label id the helper takes back.
    pub(super) id: String,
    /// `inbox`, `drafts`, `sent`, `starred`, `archive`, `junk`, `trash` or
    /// `other`.
    pub(super) role: String,
    pub(super) label: String,
    /// The parent's id, empty at the top.
    pub(super) parent: String,
    /// `system`, `folder` (a place mail lives) or `label` (a tag).
    pub(super) kind: String,
    /// The server's unread count.
    pub(super) unread: u32,
}

/// What changed since the event a sync was asked from.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Changes {
    /// The server lost track: everything is to be listed again.
    pub(super) refresh: bool,
    /// More events wait: ask again from the new event id.
    pub(super) more: bool,
    /// Ids gone from the mailbox.
    pub(super) deleted: Vec<String>,
    /// The current header of every message made or changed.
    pub(super) updated: Vec<Value>,
}

/// One page of a sync: the folder tree, a folder's headers, the event to
/// ask from next time, and with `since`, what changed.
#[derive(Debug, Clone)]
pub(super) struct SyncPage {
    pub(super) folders: Vec<FolderRow>,
    /// The id of the folder listed (the Inbox when none was asked for).
    pub(super) folder: String,
    pub(super) headers: Vec<Value>,
    /// The folder's complete id snapshot, newest first: only on a first
    /// page asked without `since`, and not past 10 000 messages. Checked
    /// before anything is pruned from it: a corrupt one would empty the
    /// folder's cache.
    pub(super) all_ids: Option<Vec<String>>,
    /// The last id of a full page, to ask for the page after it.
    pub(super) next_cursor: Option<String>,
    /// The event to pass as `since` next time.
    pub(super) event_id: String,
    pub(super) changes: Option<Changes>,
}

fn string_field(row: &Object, key: &str) -> String {
    row.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn id_list(value: Option<&Value>) -> Result<Vec<String>, HelperError> {
    match value {
        Some(Value::Array(ids)) => ids
            .iter()
            .map(|id| match id {
                Value::String(id) if !id.is_empty() => Ok(id.clone()),
                _ => Err(invalid()),
            })
            .collect(),
        _ => Err(invalid()),
    }
}

fn folder_row(row: &Value) -> Result<FolderRow, HelperError> {
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
        kind: string_field(row, "kind"),
        unread: row
            .get("unread")
            .and_then(Value::as_u64)
            .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX)),
    })
}

fn changes(value: &Value) -> Result<Changes, HelperError> {
    let Value::Object(changes) = value else {
        return Err(invalid());
    };
    let flag = |key: &str| match changes.get(key) {
        Some(Value::Bool(flag)) => Ok(*flag),
        _ => Err(invalid()),
    };
    let updated = match changes.get("updated") {
        Some(Value::Array(rows)) => rows.clone(),
        _ => return Err(invalid()),
    };
    Ok(Changes {
        refresh: flag("refresh")?,
        more: flag("more")?,
        deleted: id_list(changes.get("deleted"))?,
        updated,
    })
}

/// Read a sync reply.
pub(super) fn sync_page(result: &Object) -> Result<SyncPage, HelperError> {
    let folders = match result.get("folders") {
        Some(Value::Array(rows)) => rows.iter().map(folder_row).collect::<Result<_, _>>()?,
        _ => return Err(invalid()),
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
        ids => Some(id_list(ids)?),
    };
    let next_cursor = match result.get("next_cursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) if !id.is_empty() => Some(id.clone()),
        Some(_) => return Err(invalid()),
    };
    let event_id = match result.get("event_id") {
        Some(Value::String(id)) if !id.is_empty() => id.clone(),
        _ => return Err(invalid()),
    };
    let changes = match result.get("changes") {
        None | Some(Value::Null) => None,
        Some(value) => Some(changes(value)?),
    };
    Ok(SyncPage {
        folders,
        folder,
        headers,
        all_ids,
        next_cursor,
        event_id,
        changes,
    })
}

/// Where in a folder a page starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Start {
    /// The newest message; the page carries the id snapshot.
    Newest,
    /// `n` messages down, newest first.
    Offset(usize),
    /// After the message with this id, newest first: stable while mail
    /// arrives.
    After(String),
}

/// A page of `folder` (the Inbox when `None`): `limit` headers from
/// `from`, and with `since`, the changes since that event. Blocking.
pub(super) fn sync(
    client: &HelperClient,
    folder: Option<&str>,
    limit: usize,
    from: Start,
    since: Option<&str>,
) -> Result<SyncPage, HelperError> {
    let mut parameters = Object::new();
    if let Some(folder) = folder {
        parameters.insert("folder".into(), folder.into());
    }
    parameters.insert("limit".into(), limit.into());
    match from {
        Start::Newest => {}
        Start::Offset(offset) => {
            parameters.insert("offset".into(), offset.into());
        }
        Start::After(cursor) => {
            parameters.insert("cursor".into(), cursor.into());
        }
    }
    if let Some(since) = since {
        parameters.insert("since".into(), since.into());
    }
    sync_page(&client.request("sync", parameters)?)
}

/// One message, decrypted and put together as RFC 5322 by the helper: its
/// bytes and its header row. Blocking.
pub(super) fn message(client: &HelperClient, id: &str) -> Result<(Vec<u8>, Value), HelperError> {
    let mut parameters = Object::new();
    parameters.insert("id".into(), id.into());
    let mut reply = client.request("message", parameters)?;
    let raw = match reply.get("mime") {
        Some(Value::String(mime)) => STANDARD.decode(mime).map_err(|_| invalid())?,
        _ => return Err(invalid()),
    };
    if raw.is_empty() {
        return Err(invalid());
    }
    let header = reply.remove("header").unwrap_or(Value::Null);
    Ok((raw, header))
}

/// The per-id codes of a mutation reply, in the order asked: `None` where
/// it was applied. A reply that does not line up with the request is
/// corruption, not a partial success.
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

/// Run a mutation over `ids` in batches: the ids applied, and the first
/// failure when there was one. Unlike Tuta's, the helper reports every id
/// of a batch on its own, so an id the server refused does not stop the
/// rest; an outage (any other per-id code) or a failed batch does.
/// Blocking.
pub(super) fn mutate(
    client: &HelperClient,
    operation: &str,
    ids: &[String],
    extra: &Object,
) -> (Vec<String>, Option<HelperError>) {
    let mut applied = Vec::with_capacity(ids.len());
    let mut failed: Option<HelperError> = None;
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
        let mut outage = false;
        for (id, code) in chunk.iter().zip(codes) {
            match code {
                None => applied.push(id.clone()),
                Some(code) => {
                    outage |= code != "request_failed";
                    failed.get_or_insert_with(|| HelperError::new(&code));
                }
            }
        }
        if outage {
            break;
        }
    }
    (applied, failed)
}

fn flag(key: &str, value: bool) -> Object {
    let mut extra = Object::new();
    extra.insert(key.into(), value.into());
    extra
}

/// Mark `ids` read or unread. Blocking.
pub(super) fn mark_read(
    client: &HelperClient,
    ids: &[String],
    read: bool,
) -> (Vec<String>, Option<HelperError>) {
    mutate(client, "mark_read", ids, &flag("is_read", read))
}

/// Star or unstar `ids` (Proton's Starred label). Blocking.
pub(super) fn star(
    client: &HelperClient,
    ids: &[String],
    starred: bool,
) -> (Vec<String>, Option<HelperError>) {
    mutate(client, "star", ids, &flag("is_starred", starred))
}

/// Move `ids` into the folder `destination`. Proton keeps a message's id
/// across a move. Blocking.
pub(super) fn move_ids(
    client: &HelperClient,
    ids: &[String],
    destination: &str,
) -> (Vec<String>, Option<HelperError>) {
    let mut extra = Object::new();
    extra.insert("destination".into(), destination.into());
    mutate(client, "move", ids, &extra)
}
