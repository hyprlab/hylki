//! Messages as a Proton sync lists them, turned into list rows.

use serde_json::Value;

use crate::models::Message;
use crate::worker::headers::label_from_timestamp;
use crate::worker::ids::{hash_uid, normalize_msgid};

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// Map one header to a [`Message`], with its Proton id for the uid map.
/// `None` for a header without an id or a date, which is not a message
/// the list could show.
///
/// Proton keeps the original Message-ID in the listing, so a row threads
/// by it at once; its References come with the body (see
/// `ops::note_threading`), which joins a reply to its thread.
pub(super) fn proton_message(
    v: &Value,
    account_id: u32,
    folder_id: u32,
) -> Option<(Message, String)> {
    let id = v["id"].as_str().filter(|id| !id.is_empty())?.to_string();
    let uid = hash_uid(&id);
    let ts = v["date_ms"].as_i64()? / 1000;
    let message_id = normalize_msgid(v["message_id"].as_str().unwrap_or_default().as_bytes());
    let msg = Message {
        id: uid,
        account_id,
        folder_id,
        uid,
        from_name: text(&v["sender"]["name"]),
        from_addr: text(&v["sender"]["address"]),
        reply_to: String::new(),
        // Only the first recipient is listed; the body has them all.
        to: text(&v["recipient"]["address"]),
        cc: String::new(),
        bcc: String::new(),
        subject: text(&v["subject"]),
        preview: String::new(),
        body: String::new(),
        date: if ts > 0 {
            label_from_timestamp(ts)
        } else {
            String::new()
        },
        timestamp: ts,
        // Missing reads as unread: a badge that should not be there heals
        // on the next sync, while a missing one hides mail that arrived.
        unread: v["is_unread"].as_bool().unwrap_or(true),
        starred: v["is_starred"].as_bool().unwrap_or(false),
        // The listing has no replied or forwarded mark.
        answered: false,
        forwarded: false,
        keywords: Vec::new(),
        has_attachment: v["has_attachments"].as_bool().unwrap_or(false),
        message_id,
        references: String::new(),
        importance: Default::default(),
        due: 0,
        labels: Vec::new(),
    };
    Some((msg, id))
}

/// The label ids a header names, for which folders it belongs in.
pub(super) fn label_ids(v: &Value) -> Vec<&str> {
    v["label_ids"]
        .as_array()
        .map(|ids| ids.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}
