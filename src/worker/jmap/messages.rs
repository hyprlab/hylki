//! Emails as JMAP lists them: the properties asked for, turned into list
//! rows, and the id listings the background index works from.

use crate::models::Message;
use crate::worker::headers::label_from_timestamp;
use crate::worker::ids::{hash_uid, normalize_msgid};
use crate::worker::jmap::wire::{args, call, jmap_call, JmapSession, CAP_CORE, CAP_MAIL};
use crate::worker::preview::pgp_preview;

/// The newest summaries a folder load lists, for a quick first paint; the
/// background index fetches the rest of the folder (#429).
const JMAP_FIRST_PAGE: usize = 300;

/// Ids asked for per page of a folder's id listing (the server may give
/// fewer; the next page starts where this one ended).
const JMAP_ID_PAGE: usize = 1000;

/// The summary properties one listing asks `Email/get` for.
const EMAIL_PROPS: &[&str] = &[
    "id",
    "blobId",
    "threadId",
    "mailboxIds",
    "keywords",
    "hasAttachment",
    "receivedAt",
    "subject",
    "from",
    "replyTo",
    "to",
    "cc",
    "bcc",
    "messageId",
    "inReplyTo",
    "references",
    "preview",
    "header:X-Priority:asText",
    "header:Importance:asText",
    "header:Priority:asText",
];

/// Keywords the server holds that are flags, not tags: what IMAP's system
/// flags are to its keywords.
const SYSTEM_KEYWORDS: &[&str] = &[
    "$seen",
    "$flagged",
    "$draft",
    "$answered",
    "$forwarded",
    "$recent",
    "$deleted",
];

/// The comma-separated addresses of a JMAP address list.
fn jmap_addrs(v: &serde_json::Value) -> String {
    v.as_array()
        .map(|list| {
            list.iter()
                .filter_map(|r| r["email"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// The normalized Message-IDs of a JMAP id list, space-separated.
fn jmap_msgids(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .map(|list| {
            list.iter()
                .filter_map(|r| r.as_str())
                .map(|s| normalize_msgid(s.as_bytes()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// A summary as the list shows it, with its JMAP id and blob id.
type ListedEmail = (Message, String, String);

/// Map one `Email/get` summary to a [`Message`]. Returns the JMAP id and
/// blob id too, so the caller can index them.
pub(super) fn jmap_message(
    v: &serde_json::Value,
    account_id: u32,
    folder_id: u32,
) -> Option<ListedEmail> {
    let id = v["id"].as_str()?.to_string();
    let blob = v["blobId"].as_str().unwrap_or("").to_string();
    let uid = hash_uid(&id);
    let ts = v["receivedAt"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp())
        .unwrap_or(0);
    // The server builds this from the text part, entities and all (#683).
    let preview: String = crate::mail::preview_text::one_line(v["preview"].as_str().unwrap_or(""))
        .chars()
        .take(200)
        .collect();
    let preview = pgp_preview(&preview).unwrap_or(preview);
    let from = v["from"]
        .as_array()
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or_default();
    let from_addr = from["email"].as_str().unwrap_or("").to_string();
    let reply_to = jmap_addrs(&v["replyTo"]);
    let reply_to = if reply_to.eq_ignore_ascii_case(&from_addr) {
        String::new()
    } else {
        reply_to
    };
    let reply_to_name =
        crate::worker::headers::reply_to_display_name(&reply_to, v["replyTo"][0]["name"].as_str());
    let keywords = v["keywords"].as_object();
    let has = |k: &str| {
        keywords
            .and_then(|m| m.get(k))
            .and_then(|b| b.as_bool())
            .unwrap_or(false)
    };
    let user_keywords: Vec<String> = keywords
        .map(|m| {
            m.iter()
                .filter(|(k, b)| {
                    b.as_bool().unwrap_or(false)
                        && !SYSTEM_KEYWORDS.contains(&k.to_ascii_lowercase().as_str())
                })
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default();
    let mut refs = jmap_msgids(&v["inReplyTo"]);
    for r in jmap_msgids(&v["references"]) {
        if !refs.contains(&r) {
            refs.push(r);
        }
    }
    let msg = Message {
        id: uid,
        account_id,
        folder_id,
        uid,
        from_name: from["name"].as_str().unwrap_or("").to_string(),
        from_addr,
        reply_to,
        reply_to_name,
        to: jmap_addrs(&v["to"]),
        cc: jmap_addrs(&v["cc"]),
        bcc: jmap_addrs(&v["bcc"]),
        subject: v["subject"].as_str().unwrap_or("").to_string(),
        preview,
        body: String::new(),
        date: if ts > 0 {
            label_from_timestamp(ts)
        } else {
            String::new()
        },
        timestamp: ts,
        unread: !has("$seen"),
        starred: has("$flagged"),
        answered: has("$answered"),
        forwarded: has("$forwarded"),
        keywords: user_keywords,
        has_attachment: v["hasAttachment"].as_bool().unwrap_or(false),
        message_id: jmap_msgids(&v["messageId"])
            .into_iter()
            .next()
            .unwrap_or_default(),
        references: refs.join(" "),
        importance: crate::models::Importance::from_headers(|name| {
            v[format!("header:{name}:asText")].as_str()
        }),
        due: 0,
        labels: Vec::new(),
    };
    Some((msg, id, blob))
}

/// The ids matching a filter, newest first, up to `limit`.
pub(super) fn jmap_query_ids(
    s: &JmapSession,
    filter: serde_json::Value,
    limit: usize,
) -> Result<Vec<String>, String> {
    let responses = jmap_call(
        s,
        &[CAP_CORE, CAP_MAIL],
        vec![call(
            0,
            "Email/query",
            serde_json::json!({
                "accountId": s.account,
                "filter": filter,
                "sort": [{ "property": "receivedAt", "isAscending": false }],
                "limit": limit,
            }),
        )],
    )?;
    Ok(args(&responses, 0)["ids"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

/// Summaries for a set of ids.
pub(super) fn jmap_get_messages(
    s: &JmapSession,
    ids: &[String],
    account_id: u32,
    folder_id: u32,
) -> Result<Vec<ListedEmail>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let responses = jmap_call(
        s,
        &[CAP_CORE, CAP_MAIL],
        vec![call(
            0,
            "Email/get",
            serde_json::json!({ "accountId": s.account, "ids": ids, "properties": EMAIL_PROPS }),
        )],
    )?;
    Ok(args(&responses, 0)["list"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|v| jmap_message(v, account_id, folder_id))
                .collect()
        })
        .unwrap_or_default())
}

/// A folder's newest summaries (newest first): the query and the get in one
/// request, the second fed by the first.
pub(super) fn jmap_list_messages(
    s: &JmapSession,
    mailbox_id: &str,
    account_id: u32,
    folder_id: u32,
) -> Result<Vec<ListedEmail>, String> {
    jmap_list_newest(s, mailbox_id, account_id, folder_id).map(|(list, _)| list)
}

/// [`jmap_list_messages`], saying too whether the folder holds more than
/// [`JMAP_FIRST_PAGE`].
pub(super) fn jmap_list_newest(
    s: &JmapSession,
    mailbox_id: &str,
    account_id: u32,
    folder_id: u32,
) -> Result<(Vec<ListedEmail>, bool), String> {
    let responses = jmap_call(
        s,
        &[CAP_CORE, CAP_MAIL],
        vec![
            call(
                0,
                "Email/query",
                serde_json::json!({
                    "accountId": s.account,
                    "filter": { "inMailbox": mailbox_id },
                    "sort": [{ "property": "receivedAt", "isAscending": false }],
                    "limit": JMAP_FIRST_PAGE,
                    "calculateTotal": true,
                }),
            ),
            call(
                1,
                "Email/get",
                serde_json::json!({
                    "accountId": s.account,
                    "#ids": { "resultOf": "0", "name": "Email/query", "path": "/ids" },
                    "properties": EMAIL_PROPS,
                }),
            ),
        ],
    )?;
    let query = args(&responses, 0);
    let got = query["ids"].as_array().map_or(0, |a| a.len());
    // A server that keeps no total says there is more by filling the page.
    let more = match query["total"].as_u64() {
        Some(total) => total as usize > got,
        None => got >= JMAP_FIRST_PAGE,
    };
    let list = args(&responses, 1)["list"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|v| jmap_message(v, account_id, folder_id))
                .collect()
        })
        .unwrap_or_default();
    Ok((list, more))
}

/// One page of a folder's id listing (#429), newest first from `position`:
/// the ids, the server's query state, and whether this was the last page.
pub(super) fn jmap_id_page(
    s: &JmapSession,
    mailbox_id: &str,
    position: usize,
) -> Result<(Vec<String>, Option<String>, bool), String> {
    let responses = jmap_call(
        s,
        &[CAP_CORE, CAP_MAIL],
        vec![call(
            0,
            "Email/query",
            serde_json::json!({
                "accountId": s.account,
                "filter": { "inMailbox": mailbox_id },
                "sort": [{ "property": "receivedAt", "isAscending": false }],
                "position": position,
                "limit": JMAP_ID_PAGE,
                "calculateTotal": true,
            }),
        )],
    )?;
    let r = args(&responses, 0);
    let ids: Vec<String> = r["ids"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let last = ids.is_empty()
        || r["total"]
            .as_u64()
            .is_some_and(|t| (position + ids.len()) as u64 >= t);
    Ok((ids, r["queryState"].as_str().map(str::to_string), last))
}
