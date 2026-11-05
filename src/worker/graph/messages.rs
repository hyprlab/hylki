//! Messages as Graph lists them: the summary fields asked for, turned into
//! list rows, and the id-only listings the background index works from.

use crate::models::Message;
use crate::worker::graph::http::{graph_get_json, graph_paged, graph_paged_more};
use crate::worker::headers::label_from_timestamp;
use crate::worker::ids::{hash_uid, normalize_msgid, GRAPH_CONV};
use crate::worker::preview::pgp_preview;

/// The newest summaries a folder load lists, for a quick first paint; the
/// background index fetches the rest of the folder (#429).
pub(super) const GRAPH_FIRST_PAGE: usize = 300;

/// Ids asked for per page of a folder's id-only listing; Graph may serve
/// fewer, and its next-page link carries on from there.
const GRAPH_ID_PAGE: usize = 1000;

/// The comma-separated addresses of a Graph recipient array.
fn graph_addrs(v: &serde_json::Value) -> String {
    v.as_array()
        .map(|list| {
            list.iter()
                .filter_map(|r| r["emailAddress"]["address"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// Map one Graph message summary to a [`Message`]. Returns the Graph id too so
/// the caller can index it.
pub(super) fn graph_message(
    v: &serde_json::Value,
    account_id: u32,
    folder_id: u32,
) -> Option<(Message, String)> {
    let gid = v["id"].as_str()?.to_string();
    let uid = hash_uid(&gid);
    let ts = v["receivedDateTime"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp())
        .unwrap_or(0);
    let preview: String = v["bodyPreview"]
        .as_str()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect();
    let preview = pgp_preview(&preview).unwrap_or(preview);
    let from_addr = v["from"]["emailAddress"]["address"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let reply_to = graph_addrs(&v["replyTo"]);
    let reply_to = if reply_to.eq_ignore_ascii_case(&from_addr) {
        String::new()
    } else {
        reply_to
    };
    let last_verb = v["singleValueExtendedProperties"]
        .as_array()
        .and_then(|props| {
            props.iter().find(|p| {
                p["id"]
                    .as_str()
                    .is_some_and(|id| id.eq_ignore_ascii_case(LAST_VERB))
            })
        })
        .and_then(|p| p["value"].as_str()?.parse::<u32>().ok());
    let msg = Message {
        id: uid,
        account_id,
        folder_id,
        uid,
        from_name: v["from"]["emailAddress"]["name"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        from_addr,
        reply_to,
        to: graph_addrs(&v["toRecipients"]),
        cc: graph_addrs(&v["ccRecipients"]),
        bcc: graph_addrs(&v["bccRecipients"]),
        subject: v["subject"].as_str().unwrap_or("").to_string(),
        preview,
        body: String::new(),
        date: if ts > 0 {
            label_from_timestamp(ts)
        } else {
            String::new()
        },
        timestamp: ts,
        unread: !v["isRead"].as_bool().unwrap_or(true),
        starred: v["flag"]["flagStatus"].as_str() == Some("flagged"),
        answered: matches!(last_verb, Some(102 | 103)),
        forwarded: last_verb == Some(104),
        keywords: v["categories"]
            .as_array()
            .map(|cs| {
                cs.iter()
                    .filter_map(|c| c.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        has_attachment: v["hasAttachments"].as_bool().unwrap_or(false),
        message_id: normalize_msgid(v["internetMessageId"].as_str().unwrap_or("").as_bytes()),
        references: v["conversationId"]
            .as_str()
            .map(|c| format!("{GRAPH_CONV}{}", c.to_ascii_lowercase()))
            .unwrap_or_default(),
        importance: match v["importance"].as_str() {
            Some("high") => crate::models::Importance::High,
            Some("low") => crate::models::Importance::Low,
            _ => crate::models::Importance::Normal,
        },
        due: graph_due(&v["flag"]),
        labels: Vec::new(),
    };
    Some((msg, gid))
}

pub(super) const GRAPH_MSG_SELECT: &str = "$select=id,internetMessageId,conversationId,subject,bodyPreview,\
                                from,replyTo,toRecipients,ccRecipients,bccRecipients,receivedDateTime,isRead,\
                                flag,hasAttachments,categories,importance\
                                &$expand=singleValueExtendedProperties($filter=id%20eq%20'Integer%200x1081')";

/// Exchange keeps no replied or forwarded flag (#454), only the last thing
/// done to a message, as the MAPI property PidTagLastVerbExecuted: 102 reply,
/// 103 reply all, 104 forward. Outlook draws its arrow from PidTagIconIndex
/// (261 replied, 262 forwarded), so both are written.
pub(super) const LAST_VERB: &str = "Integer 0x1081";

pub(super) const ICON_INDEX: &str = "Integer 0x1080";

/// When a still-open follow-up flag falls due, in Unix seconds, or 0 (#334).
/// Graph gives a wall-clock time and the zone it is in, which is UTC unless
/// the request asked for another.
pub(super) fn graph_due(flag: &serde_json::Value) -> i64 {
    if flag["flagStatus"].as_str() != Some("flagged") {
        return 0;
    }
    let Some(at) = flag["dueDateTime"]["dateTime"].as_str() else {
        return 0;
    };
    // Seven decimal places of seconds, which chrono will not take whole.
    let at = at.split('.').next().unwrap_or(at);
    chrono::NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M:%S")
        .map(|t| t.and_utc().timestamp())
        .unwrap_or(0)
}

/// List a folder's newest summaries (newest first), up to
/// [`GRAPH_FIRST_PAGE`], and whether the folder holds more than that.
pub(super) fn graph_list_messages(
    token: &str,
    base: &str,
    folder_graph_id: &str,
    account_id: u32,
    folder_id: u32,
) -> Result<(Vec<(Message, String)>, bool), String> {
    let url = format!(
        "{base}/me/mailFolders/{folder_graph_id}/messages\
         ?$top=100&$orderby=receivedDateTime%20desc&{GRAPH_MSG_SELECT}"
    );
    let (items, more) = graph_paged_more(token, &url, GRAPH_FIRST_PAGE)?;
    Ok((
        items
            .iter()
            .filter_map(|v| graph_message(v, account_id, folder_id))
            .collect(),
        more,
    ))
}

/// The first page of a folder's id-only listing (#429): every message's id
/// and arrival time, newest first, a thousand to a page.
pub(super) fn graph_ids_url(base: &str, folder_graph_id: &str) -> String {
    format!(
        "{base}/me/mailFolders/{folder_graph_id}/messages\
         ?$top={GRAPH_ID_PAGE}&$orderby=receivedDateTime%20desc&$select=id,receivedDateTime"
    )
}

/// One page of an id-only listing: (Graph id, `receivedDateTime`) pairs,
/// and the link to the next page, if there is one.
type IdPage = (Vec<(String, String)>, Option<String>);

/// Fetch one [`IdPage`].
pub(super) fn graph_id_page(token: &str, url: &str) -> Result<IdPage, String> {
    let page = graph_get_json(token, url)?;
    let ids = page["value"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            let id = v["id"].as_str()?.to_string();
            Some((id, v["receivedDateTime"].as_str().unwrap_or("").to_string()))
        })
        .collect();
    Ok((ids, page["@odata.nextLink"].as_str().map(str::to_string)))
}

/// Every message id in a folder, page by page.
pub(super) fn graph_list_ids(
    token: &str,
    base: &str,
    folder_graph_id: &str,
) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut url = graph_ids_url(base, folder_graph_id);
    loop {
        let (ids, next) = graph_id_page(token, &url)?;
        out.extend(ids);
        match next {
            Some(next) => url = next,
            None => return Ok(out),
        }
    }
}

/// The messages in a folder with a Message-ID, wherever they are in it: a
/// filter, not a listing, so mail older than the first page is found too.
/// `exact` is the id as it was spelt, without its angle brackets.
pub(super) fn graph_find_by_msgid(
    token: &str,
    base: &str,
    folder_graph_id: &str,
    exact: &str,
) -> Result<Vec<(Message, String)>, String> {
    let literal = format!("'<{}>'", exact.replace('\'', "''"));
    let url = format!(
        "{base}/me/mailFolders/{folder_graph_id}/messages\
         ?$filter=internetMessageId%20eq%20{}&$top=10&{GRAPH_MSG_SELECT}",
        crate::encoding::percent::encode(&literal)
    );
    let items = graph_paged(token, &url, 10)?;
    Ok(items
        .iter()
        .filter_map(|v| graph_message(v, 0, 0))
        .collect())
}
