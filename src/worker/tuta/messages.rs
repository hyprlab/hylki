//! Messages as a Tuta sync lists them, turned into list rows.

use serde_json::Value;

use crate::models::Message;
use crate::worker::headers::label_from_timestamp;
use crate::worker::ids::{hash_uid, TUTA_CONV};

/// Tuta's replied/forwarded mark (`Mail.replyType`).
const REPLIED: i64 = 1;
const FORWARDED: i64 = 2;
const REPLIED_AND_FORWARDED: i64 = 3;

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// Map one header to a [`Message`], with its compound `listId/elementId` id
/// for the uid map. `None` for a header without an id or a date, which is
/// not a message the list could show.
///
/// A listing has no Message-ID, References or preview (Tuta keeps them in
/// the encrypted details, read with the body), so the conversation entry's
/// list id threads the message meanwhile, as a `tuta-conv:` token.
pub(super) fn tuta_message(
    v: &Value,
    account_id: u32,
    folder_id: u32,
) -> Option<(Message, String)> {
    let id = v["id"].as_str().filter(|id| !id.is_empty())?.to_string();
    let uid = hash_uid(&id);
    let ts = v["date_ms"].as_i64()? / 1000;
    let reply_type = v["reply_type"].as_i64().unwrap_or(0);
    let conversation = v["conversation"].as_str().unwrap_or_default();
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
        // Tuta has no star; the cache keeps Hylki's own (see the merge in
        // `index`).
        starred: false,
        answered: matches!(reply_type, REPLIED | REPLIED_AND_FORWARDED),
        forwarded: matches!(reply_type, FORWARDED | REPLIED_AND_FORWARDED),
        keywords: Vec::new(),
        has_attachment: v["has_attachments"].as_bool().unwrap_or(false),
        message_id: String::new(),
        references: if conversation.is_empty() {
            String::new()
        } else {
            format!("{TUTA_CONV}{conversation}")
        },
        importance: Default::default(),
        due: 0,
        labels: Vec::new(),
    };
    Some((msg, id))
}
