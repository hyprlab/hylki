//! Microsoft Graph path (issue #36)
//!
//! Microsoft 365 accounts imported from GNOME Online Accounts authenticate with
//! a GOA token scoped to the Graph API — it cannot log in to IMAP or SMTP at
//! all. So these accounts speak Graph (REST) end to end: folders, summaries,
//! raw MIME bodies (`/$value`, which feeds the exact same parsing pipeline as
//! IMAP), flags, moves, drafts, and `sendMail`. Message uids are the same
//! stable string-hash the POP3 path uses (Graph ids are strings); the
//! uid → Graph-id map is rebuilt from every folder listing. Threading uses a
//! synthetic `graph-conv:<conversationId>` reference token (stripped before any
//! wire header in `build_email`) because the real References header isn't
//! available from list queries.

use super::*;

pub(super) const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";
/// The newest summaries a folder load lists, for a quick first paint; the
/// background index fetches the rest of the folder (#429).
pub(super) const GRAPH_FIRST_PAGE: usize = 300;
/// Ids asked for per page of a folder's id-only listing; Graph may serve
/// fewer, and its next-page link carries on from there.
const GRAPH_ID_PAGE: usize = 1000;
/// Summaries one background step asks for (#429): one request's worth,
/// so a click is never queued behind more than that.
const GRAPH_BACKFILL_CHUNK: usize = 500;
/// A run of missing mail shorter than this is fetched message by message
/// rather than by its dates.
const GRAPH_RANGE_MIN: usize = 3;
/// How many times a throttled request waits and tries again.
const GRAPH_THROTTLE_TRIES: u32 = 4;
/// How long a folder's id listing is trusted before opening the folder
/// lists it again, catching mail deleted elsewhere (#429).
const GRAPH_RELIST_AFTER: Duration = Duration::from_secs(15 * 60);
/// The pause after a background step fails, before the next try.
const GRAPH_BACKFILL_BACKOFF: Duration = Duration::from_secs(60);

/// One Graph mail folder, flattened out of the tree.
pub(super) struct GraphFolder {
    pub(super) graph_id: String,
    pub(super) folder: Folder,
}

fn graph_auth(token: &str) -> String {
    format!("Bearer {token}")
}

/// Read a ureq error into something a user can act on (status + body snippet).
fn graph_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            // Graph errors are JSON with a nested message; surface just that.
            let msg = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                .unwrap_or_else(|| body.chars().take(200).collect());
            format!("Microsoft Graph returned {code}: {msg}")
        }
        other => other.to_string(),
    }
}

/// The Microsoft Graph conversation for the console: method and URL (the
/// token travels in a header and is never logged), then the verdict.
fn graph_wire(method: &str, url: &str) {
    tracing::debug!(target: "hylki::graph", "> {method} {}", url.strip_prefix(GRAPH_BASE).unwrap_or(url));
}

fn graph_wired<T>(method: &str, url: &str, r: &Result<T, String>) {
    let path = url.strip_prefix(GRAPH_BASE).unwrap_or(url);
    match r {
        Ok(_) => tracing::debug!(target: "hylki::graph", "< OK ({method} {path})"),
        Err(e) => tracing::warn!(target: "hylki::graph", "< {e} ({method} {path})"),
    }
}

/// How long Microsoft asks a throttled client to wait: `Retry-After`, in
/// seconds, on a 429 or a 503, within reason. `None` for any other answer.
fn graph_throttle_wait(e: &ureq::Error) -> Option<Duration> {
    match e {
        ureq::Error::Status(429 | 503, resp) => {
            let secs = resp
                .header("Retry-After")
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(5);
            Some(Duration::from_secs(secs.min(120)))
        }
        _ => None,
    }
}

/// A GET that respects throttling: a 429 or 503 waits as long as the
/// server asks and tries again, a few times, before it counts as failed.
/// The background index (#429) is what makes this matter: it is the first
/// thing here to send many requests in a row.
fn graph_get(token: &str, url: &str) -> Result<ureq::Response, String> {
    let mut tries = 0;
    loop {
        match ureq::get(url).set("Authorization", &graph_auth(token)).call() {
            Err(e) if tries < GRAPH_THROTTLE_TRIES => {
                let Some(wait) = graph_throttle_wait(&e) else { return Err(graph_err(e)) };
                tries += 1;
                tracing::info!(target: "hylki::graph", "throttled, waiting {} s", wait.as_secs());
                std::thread::sleep(wait);
            }
            r => return r.map_err(graph_err),
        }
    }
}

pub(super) fn graph_get_json(token: &str, url: &str) -> Result<serde_json::Value, String> {
    graph_wire("GET", url);
    let r = graph_get(token, url).and_then(|resp| resp.into_json().map_err(|e| e.to_string()));
    graph_wired("GET", url, &r);
    r
}

/// [`graph_get_json`], with a 404 as `None`: a message that has gone since
/// it was listed (moved, deleted) is no failure.
fn graph_get_json_opt(token: &str, url: &str) -> Result<Option<serde_json::Value>, String> {
    match graph_get_json(token, url) {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.starts_with("Microsoft Graph returned 404") => Ok(None),
        Err(e) => Err(e),
    }
}

fn graph_get_bytes(token: &str, url: &str) -> Result<Vec<u8>, String> {
    graph_wire("GET", url);
    let resp = graph_get(token, url);
    graph_wired("GET", url, &resp);
    let resp = resp?;
    let limit = raw_message_limit();
    let declared = resp.header("Content-Length").and_then(|v| v.trim().parse::<u64>().ok());
    read_capped(resp.into_reader(), declared, limit)
}


/// POST/PATCH a JSON body; an empty 2xx response comes back as `Null`.
fn graph_send_json(
    token: &str,
    method: &str,
    url: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    graph_wire(method, url);
    let resp = ureq::request(method, url)
        .set("Authorization", &graph_auth(token))
        .send_json(body.clone())
        .map_err(graph_err);
    graph_wired(method, url, &resp);
    let text = resp?.into_string().map_err(|e| e.to_string())?;
    if text.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
}

/// POST raw MIME (base64, `text/plain` content type — Graph's MIME format) to
/// `sendMail` or a create-message endpoint.
fn graph_post_mime(token: &str, url: &str, raw: &[u8]) -> Result<serde_json::Value, String> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
    graph_wire("POST(mime)", url);
    let resp = ureq::post(url)
        .set("Authorization", &graph_auth(token))
        .set("Content-Type", "text/plain")
        .send_string(&b64)
        .map_err(graph_err);
    graph_wired("POST(mime)", url, &resp);
    let text = resp?.into_string().map_err(|e| e.to_string())?;
    if text.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
}

pub(super) fn graph_delete_req(token: &str, url: &str) -> Result<(), String> {
    graph_wire("DELETE", url);
    let r = ureq::delete(url)
        .set("Authorization", &graph_auth(token))
        .call()
        .map_err(graph_err)
        .map(|_| ());
    graph_wired("DELETE", url, &r);
    r
}

/// Follow `@odata.nextLink` pagination, collecting `value` arrays up to `cap`.
pub(super) fn graph_paged(token: &str, first_url: &str, cap: usize) -> Result<Vec<serde_json::Value>, String> {
    graph_paged_more(token, first_url, cap).map(|(items, _)| items)
}

/// [`graph_paged`], saying too whether the server had more than `cap`.
fn graph_paged_more(
    token: &str,
    first_url: &str,
    cap: usize,
) -> Result<(Vec<serde_json::Value>, bool), String> {
    let mut out = Vec::new();
    let mut url = first_url.to_string();
    loop {
        let page = graph_get_json(token, &url)?;
        if let Some(items) = page["value"].as_array() {
            out.extend(items.iter().cloned());
        }
        let next = page["@odata.nextLink"].as_str();
        if out.len() >= cap {
            let more = out.len() > cap || next.is_some();
            out.truncate(cap);
            return Ok((out, more));
        }
        match next {
            Some(next) => url = next.to_string(),
            None => return Ok((out, false)),
        }
    }
}

/// List the account's mail folders (tree flattened, well-known roles mapped),
/// sorted and id-numbered exactly like the IMAP path's folder list.
pub(super) fn graph_list_folders(
    token: &str,
    account_id: u32,
    assignments: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<GraphFolder>, String> {
    graph_list_folders_at(token, GRAPH_BASE, account_id, assignments)
}

/// Deeper than any folder tree Exchange allows (300 levels): a tree that
/// goes further is answered wrongly, and is refused rather than walked.
const GRAPH_FOLDER_DEPTH_LIMIT: usize = 300;

/// More folders than an Exchange mailbox holds; a listing past it is
/// refused rather than cut short.
const GRAPH_FOLDER_COUNT_LIMIT: usize = 100_000;

/// [`graph_list_folders`] against `base` (a test's own server). Every page
/// of every level is read; a level that cannot be read fails the whole
/// listing, so a partial tree is never taken for the whole one, which would
/// make the folders it misses disappear from the sidebar.
fn graph_list_folders_at(
    token: &str,
    base: &str,
    account_id: u32,
    assignments: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<GraphFolder>, String> {
    // Well-known folders name the roles; everything else is Custom. A role
    // folder some account type lacks (e.g. archive) just 404s — skip it.
    let mut roles: std::collections::HashMap<String, FolderKind> = Default::default();
    for (wk, kind) in [
        ("inbox", FolderKind::Inbox),
        ("sentitems", FolderKind::Sent),
        ("drafts", FolderKind::Drafts),
        ("deleteditems", FolderKind::Trash),
        ("junkemail", FolderKind::Junk),
        ("archive", FolderKind::Archive),
    ] {
        match graph_get_json(token, &format!("{base}/me/mailFolders/{wk}?$select=id")) {
            Ok(v) => {
                if let Some(id) = v["id"].as_str() {
                    roles.insert(id.to_string(), kind);
                }
            }
            // A role only names a folder the listing finds anyway.
            Err(e) => tracing::debug!("graph: no {wk} folder: {e}"),
        }
    }

    const SELECT: &str =
        "$select=id,displayName,childFolderCount,unreadItemCount,totalItemCount";
    let roots = graph_paged(token, &format!("{base}/me/mailFolders?$top=100&{SELECT}"), usize::MAX)?;

    // Flatten the tree depth-first; paths join with '/' like the sidebar's
    // hierarchy expects. A folder met twice (a server answering a child
    // listing with an ancestor) is walked once, so a loop ends.
    let mut out: Vec<GraphFolder> = Vec::new();
    let mut seen: std::collections::HashSet<String> = Default::default();
    // Graph folder id → (every message, unread): the chip picks one once the
    // kinds are final (a manual Drafts assignment counts every draft).
    let mut counts: std::collections::HashMap<String, (u32, u32)> = Default::default();
    let mut queue: Vec<(serde_json::Value, String, usize)> =
        roots.into_iter().map(|v| (v, String::new(), 0)).collect();
    while let Some((v, prefix, depth)) = queue.pop() {
        let Some(gid) = v["id"].as_str() else { continue };
        if !seen.insert(gid.to_string()) {
            tracing::warn!("graph: folder {gid} listed twice; walked once");
            continue;
        }
        if depth > GRAPH_FOLDER_DEPTH_LIMIT || out.len() >= GRAPH_FOLDER_COUNT_LIMIT {
            return Err(i18n("The server's folder list is deeper or larger than any mailbox can be"));
        }
        let name = v["displayName"].as_str().unwrap_or("?").to_string();
        let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        let kind = match roles.get(gid) {
            Some(kind) => *kind,
            // No well-known folder is for templates (#360): a top-level
            // one goes by name.
            None if prefix.is_empty() && crate::models::is_templates_name(&name) => FolderKind::Templates,
            None => FolderKind::Custom,
        };
        if v["childFolderCount"].as_i64().unwrap_or(0) > 0 {
            let children = graph_paged(
                token,
                &format!("{base}/me/mailFolders/{gid}/childFolders?$top=100&{SELECT}"),
                usize::MAX,
            )?;
            queue.extend(children.into_iter().map(|c| (c, path.clone(), depth + 1)));
        }
        let count = |field: &str| v[field].as_i64().unwrap_or(0).max(0) as u32;
        counts.insert(gid.to_string(), (count("totalItemCount"), count("unreadItemCount")));
        out.push(GraphFolder {
            graph_id: gid.to_string(),
            folder: Folder {
                id: 0, // assigned by order below
                account_id,
                name,
                path,
                kind,
                unread: 0, // filled in below, once the kinds are final
            },
        });
    }

    out.sort_by(|a, b| {
        folder_order(a.folder.kind)
            .cmp(&folder_order(b.folder.kind))
            .then_with(|| a.folder.path.to_lowercase().cmp(&b.folder.path.to_lowercase()))
    });
    for (i, f) in out.iter_mut().enumerate() {
        f.folder.id = i as u32 + 1;
    }
    // Manual Special Folders assignments (#82) over the well-known roles,
    // before the chips are counted — as the IMAP listing does.
    let mut folders: Vec<Folder> = out.iter().map(|f| f.folder.clone()).collect();
    crate::models::assign_folder_roles(assignments, &mut folders);
    for (gf, f) in out.iter_mut().zip(folders) {
        gf.folder.kind = f.kind;
        let (total, unseen) = counts.get(&gf.graph_id).copied().unwrap_or_default();
        gf.folder.unread = if chip_counts_all(f.kind) { total } else { unseen };
    }
    Ok(out)
}

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
pub(super) fn graph_message(v: &serde_json::Value, account_id: u32, folder_id: u32) -> Option<(Message, String)> {
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
    let from_addr = v["from"]["emailAddress"]["address"].as_str().unwrap_or("").to_string();
    let reply_to = graph_addrs(&v["replyTo"]);
    let reply_to =
        if reply_to.eq_ignore_ascii_case(&from_addr) { String::new() } else { reply_to };
    let msg = Message {
        id: uid,
        account_id,
        folder_id,
        uid,
        from_name: v["from"]["emailAddress"]["name"].as_str().unwrap_or("").to_string(),
        from_addr,
        reply_to,
        to: graph_addrs(&v["toRecipients"]),
        cc: graph_addrs(&v["ccRecipients"]),
        subject: v["subject"].as_str().unwrap_or("").to_string(),
        preview,
        body: String::new(),
        date: if ts > 0 { label_from_timestamp(ts) } else { String::new() },
        timestamp: ts,
        unread: !v["isRead"].as_bool().unwrap_or(true),
        starred: v["flag"]["flagStatus"].as_str() == Some("flagged"),
        keywords: v["categories"]
            .as_array()
            .map(|cs| cs.iter().filter_map(|c| c.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
        has_attachment: v["hasAttachments"].as_bool().unwrap_or(false),
        message_id: normalize_msgid(v["internetMessageId"].as_str().unwrap_or("").as_bytes()),
        references: v["conversationId"]
            .as_str()
            .map(|c| format!("graph-conv:{}", c.to_ascii_lowercase()))
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
                                from,replyTo,toRecipients,ccRecipients,receivedDateTime,isRead,\
                                flag,hasAttachments,categories,importance";

/// When a still-open follow-up flag falls due, in Unix seconds, or 0 (#334).
/// Graph gives a wall-clock time and the zone it is in, which is UTC unless
/// the request asked for another.
fn graph_due(flag: &serde_json::Value) -> i64 {
    if flag["flagStatus"].as_str() != Some("flagged") {
        return 0;
    }
    let Some(at) = flag["dueDateTime"]["dateTime"].as_str() else { return 0 };
    // Seven decimal places of seconds, which chrono will not take whole.
    let at = at.split('.').next().unwrap_or(at);
    chrono::NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M:%S")
        .map(|t| t.and_utc().timestamp())
        .unwrap_or(0)
}

/// List a folder's newest summaries (newest first), up to
/// [`GRAPH_FIRST_PAGE`], and whether the folder holds more than that.
fn graph_list_messages(
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
    Ok((items.iter().filter_map(|v| graph_message(v, account_id, folder_id)).collect(), more))
}

/// The first page of a folder's id-only listing (#429): every message's id
/// and arrival time, newest first, a thousand to a page.
fn graph_ids_url(base: &str, folder_graph_id: &str) -> String {
    format!(
        "{base}/me/mailFolders/{folder_graph_id}/messages\
         ?$top={GRAPH_ID_PAGE}&$orderby=receivedDateTime%20desc&$select=id,receivedDateTime"
    )
}

/// One page of an id-only listing: (Graph id, `receivedDateTime`) pairs,
/// and the link to the next page, if there is one.
fn graph_id_page(token: &str, url: &str) -> Result<(Vec<(String, String)>, Option<String>), String> {
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
fn graph_list_ids(token: &str, base: &str, folder_graph_id: &str) -> Result<Vec<(String, String)>, String> {
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

/// A stretch of mail the cache lacks (#429), consecutive in the folder's
/// newest-first order, with the arrival times that bound it.
#[derive(Clone, Debug, PartialEq)]
struct GraphRun {
    ids: Vec<String>,
    newest: String,
    oldest: String,
}

/// What a folder's complete id listing (newest first) asks of the cache:
/// the rows to drop, and the runs of mail to fetch, newest first, none
/// longer than [`GRAPH_BACKFILL_CHUNK`]. `since` is when the listing began.
fn graph_index_plan(
    server: &[(String, String)],
    cached: &std::collections::HashMap<u32, i64>,
    since: i64,
) -> (Vec<u32>, Vec<GraphRun>) {
    let uids: std::collections::HashSet<u32> = server.iter().map(|(id, _)| hash_uid(id)).collect();
    let gone = listing_gone(cached, &uids, since);
    let mut runs: Vec<GraphRun> = Vec::new();
    let mut open = false;
    for (id, at) in server {
        if cached.contains_key(&hash_uid(id)) {
            open = false;
            continue;
        }
        match runs.last_mut() {
            Some(run) if open && run.ids.len() < GRAPH_BACKFILL_CHUNK => {
                run.ids.push(id.clone());
                run.oldest = at.clone();
            }
            _ => runs.push(GraphRun { ids: vec![id.clone()], newest: at.clone(), oldest: at.clone() }),
        }
        open = true;
    }
    (gone, runs)
}

/// The summaries of one run of missing mail. A long run is one listing of
/// the folder between its dates (which may bring a cached neighbour sharing
/// a date along, harmlessly); a short one, or one without dates, is asked
/// for message by message, and a message gone since the listing is skipped.
fn graph_fetch_run(
    token: &str,
    base: &str,
    folder_graph_id: &str,
    run: &GraphRun,
    account_id: u32,
    folder_id: u32,
) -> Result<Vec<(Message, String)>, String> {
    let dated = run.ids.len() >= GRAPH_RANGE_MIN && !run.newest.is_empty() && !run.oldest.is_empty();
    let items = if dated {
        let url = format!(
            "{base}/me/mailFolders/{folder_graph_id}/messages\
             ?$filter=receivedDateTime%20ge%20{}%20and%20receivedDateTime%20le%20{}\
             &$orderby=receivedDateTime%20desc&$top={GRAPH_BACKFILL_CHUNK}&{GRAPH_MSG_SELECT}",
            crate::percent::encode(&run.oldest),
            crate::percent::encode(&run.newest),
        );
        // Bounded, should many messages share the run's end dates.
        let mut items = graph_paged(token, &url, run.ids.len() + GRAPH_BACKFILL_CHUNK)?;
        // The range is the listing's dates, which Graph prints to the whole
        // second; a message stored with a fraction past `le`, one without a
        // date, or one past the bound above is not in it. Each one asked for
        // and not returned is fetched by its id, so the folder is never
        // called indexed with mail missing.
        let got: std::collections::HashSet<&str> = items.iter().filter_map(|v| v["id"].as_str()).collect();
        let missing: Vec<String> = run.ids.iter().filter(|id| !got.contains(id.as_str())).cloned().collect();
        if !missing.is_empty() {
            tracing::info!("graph: a dated fetch left out {} of {} messages; fetching them by id", missing.len(), run.ids.len());
        }
        for id in missing {
            let url = format!("{base}/me/messages/{id}?{GRAPH_MSG_SELECT}");
            items.extend(graph_get_json_opt(token, &url)?);
        }
        items
    } else {
        let mut items = Vec::new();
        for id in &run.ids {
            let url = format!("{base}/me/messages/{id}?{GRAPH_MSG_SELECT}");
            items.extend(graph_get_json_opt(token, &url)?);
        }
        items
    };
    Ok(items.iter().filter_map(|v| graph_message(v, account_id, folder_id)).collect())
}

/// The messages in a folder with a Message-ID, wherever they are in it: a
/// filter, not a listing, so mail older than the first page is found too.
/// `exact` is the id as it was spelt, without its angle brackets.
fn graph_find_by_msgid(
    token: &str,
    base: &str,
    folder_graph_id: &str,
    exact: &str,
) -> Result<Vec<(Message, String)>, String> {
    let literal = format!("'<{}>'", exact.replace('\'', "''"));
    let url = format!(
        "{base}/me/mailFolders/{folder_graph_id}/messages\
         ?$filter=internetMessageId%20eq%20{}&$top=10&{GRAPH_MSG_SELECT}",
        crate::percent::encode(&literal)
    );
    let items = graph_paged(token, &url, 10)?;
    Ok(items.iter().filter_map(|v| graph_message(v, 0, 0)).collect())
}

/// The background index of one folder (#429): its id-only listing a page a
/// step, then its runs of missing mail a run a step.
struct GraphBackfill {
    path: String,
    phase: GraphBackfillPhase,
    /// Whether the list has already been told the folder's index is done:
    /// a pass that re-checks a finished folder, or one that kept failing.
    told: bool,
    /// Steps that failed in a row.
    failures: u32,
}

enum GraphBackfillPhase {
    /// Paging through the ids: the next page's link (`None` for the first)
    /// and what came so far, from a listing begun at `since`.
    Listing { next: Option<String>, ids: Vec<(String, String)>, since: i64 },
    Fetching { runs: std::collections::VecDeque<GraphRun> },
}

impl GraphBackfill {
    fn new(path: &str, told: bool) -> GraphBackfill {
        GraphBackfill {
            path: path.to_string(),
            phase: GraphBackfillPhase::Listing { next: None, ids: Vec::new(), since: crate::datefmt::now() },
            told,
            failures: 0,
        }
    }
}

/// Per-account state the Graph loop threads through its handlers.
pub(super) struct GraphState {
    /// Hylki folder path → (folder id, Graph folder id), from the last listing.
    folders: std::collections::HashMap<String, (u32, String)>,
    /// Message uid (hashed Graph id) → Graph message id.
    uids: std::collections::HashMap<u32, String>,
    /// The Drafts folder's (folder id, path), for draft reloads after a send.
    drafts: Option<(u32, String)>,
    /// The Inbox's (folder id, path), for the new-mail poll.
    inbox: Option<(u32, String)>,
    /// The account's manual Special Folders assignments (#82), applied to
    /// every listing.
    roles: std::collections::BTreeMap<String, String>,
    /// The account's hidden folders (#239), left out of every listing.
    hidden: Vec<String>,
    /// Where Graph is: [`GRAPH_BASE`], or a test's own server.
    base: String,
    /// The folders whose background index (#429) is still to run, the one
    /// at the front in progress.
    backfill: std::collections::VecDeque<GraphBackfill>,
    /// When each folder's background index last finished, by path.
    listed: std::collections::HashMap<String, std::time::Instant>,
    /// When a message lookup last listed each folder's ids, by path.
    looked_up: std::collections::HashMap<String, std::time::Instant>,
    /// No background step before this, after one failed.
    backfill_wait: Option<std::time::Instant>,
}

impl GraphState {
    fn new(account: &AccountConfig) -> GraphState {
        GraphState {
            folders: Default::default(),
            uids: Default::default(),
            drafts: None,
            inbox: None,
            roles: account.folder_roles.clone(),
            hidden: account.hidden_folders.clone(),
            base: GRAPH_BASE.to_string(),
            backfill: Default::default(),
            listed: Default::default(),
            looked_up: Default::default(),
            backfill_wait: None,
        }
    }

    /// Queue a folder's background index unless it is queued already.
    /// `front` puts it at the head of the queue (back there, if it was
    /// queued): the folder on screen goes before the rest of the mailbox.
    fn queue_backfill(&mut self, path: &str, front: bool, told: bool) {
        if let Some(i) = self.backfill.iter().position(|j| j.path == path) {
            if front && i > 0 {
                if let Some(job) = self.backfill.remove(i) {
                    self.backfill.push_front(job);
                }
            }
            return;
        }
        let job = GraphBackfill::new(path, told);
        if front {
            self.backfill.push_front(job);
        } else {
            self.backfill.push_back(job);
        }
    }

    /// Queue every listed folder's background index, the Inbox and the
    /// other special folders first, as the folder list orders them.
    fn queue_all_backfills(&mut self) {
        let mut folders: Vec<(u32, String)> =
            self.folders.iter().map(|(path, (id, _))| (*id, path.clone())).collect();
        folders.sort();
        for (_, path) in folders {
            if !self.listed.contains_key(&path)
                && !crate::models::folder_is_hidden(&path, Some("/"), &self.hidden)
            {
                self.queue_backfill(&path, false, false);
            }
        }
    }

    /// After a folder's newest mail has been listed: whether its index is
    /// complete now. A folder that fits in the first page is; so is one
    /// whose background index has run, though opening it again after a
    /// while queues another pass for mail deleted elsewhere. Otherwise its
    /// background index is queued, first in line when `user` opened it.
    fn index_after_load(&mut self, path: &str, complete: bool, user: bool) -> bool {
        if complete {
            self.backfill.retain(|j| j.path != path);
            self.listed.insert(path.to_string(), std::time::Instant::now());
            return true;
        }
        match self.listed.get(path) {
            Some(at) => {
                if user && at.elapsed() >= GRAPH_RELIST_AFTER {
                    self.queue_backfill(path, true, true);
                }
                true
            }
            None => {
                self.queue_backfill(path, user, false);
                false
            }
        }
    }

    /// The chip number for a folder just loaded: every draft in Drafts,
    /// unread mail elsewhere.
    fn chip_count(&self, folder_id: u32, messages: &[Message]) -> u32 {
        let kind = match &self.drafts {
            Some((id, _)) if *id == folder_id => FolderKind::Drafts,
            _ => FolderKind::Custom,
        };
        chip_count_of(kind, messages)
    }

    fn adopt_folders(&mut self, list: &[GraphFolder]) {
        self.folders = list
            .iter()
            .map(|f| (f.folder.path.clone(), (f.folder.id, f.graph_id.clone())))
            .collect();
        self.drafts = list
            .iter()
            .find(|f| f.folder.kind == FolderKind::Drafts)
            .map(|f| (f.folder.id, f.folder.path.clone()));
        self.inbox = list
            .iter()
            .find(|f| f.folder.kind == FolderKind::Inbox)
            .map(|f| (f.folder.id, f.folder.path.clone()));
        // Every folder is indexed in the background, as an IMAP account's
        // is, so search reaches the whole mailbox (#429).
        self.queue_all_backfills();
    }
}

pub(super) async fn run_graph(
    account_id: u32,
    account: AccountConfig,
    mut rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent),
) {
    // A label whose name the message leaves out is named from the
    // organization's list (#411) on its way to the reader, whether the
    // check was just made or comes from the cache, which keeps the header's.
    let emit = |mut ev: WorkerEvent| {
        if let WorkerEvent::SenderChecked { check, .. } = &mut ev {
            if let Some(label) = check.label.as_mut() {
                crate::graph_labels::fill(&account, label);
            }
        }
        emit(ev)
    };
    let cache = Cache::open().map_err(|e| tracing::warn!("cache unavailable: {e}")).ok();

    emit(WorkerEvent::Account(Account {
        id: account_id,
        name: account.name.clone(),
        email: account.email.clone(),
        label: account.display_label(),
        accent: accent_for(account_id).into(),
    }));

    // Cached folders immediately, then the live list.
    let cached_folders = cache.as_ref().map(|c| c.load_folders(account_id)).unwrap_or_default();
    if !cached_folders.is_empty() {
        emit(WorkerEvent::Folders(cached_folders));
    }

    // When the Junk / Trash auto-empty (#140) last ran for this worker.
    let mut last_auto_empty: Option<std::time::Instant> = None;
    let mut state = GraphState::new(&account);

    // Fetch a token and the folder list. A GOA token failure here is the one
    // users actually hit (signed out in GNOME Settings), so say that.
    if let Some(token) = graph_token(&account, &emit).await {
        refresh_graph_folders(&token, account_id, cache.as_ref(), &mut state, &emit).await;
    }

    // Graph has no push channel (nothing like IMAP IDLE is available to this
    // token), so new mail arrives on a poll. The auto-fetch preference sets the
    // cadence when it's on; otherwise a quiet couple of minutes.
    let poll_secs = match crate::config::load_privacy().fetch_interval_secs {
        0 => 120,
        s => s.max(60),
    };
    let mut poll = tokio::time::interval(Duration::from_secs(poll_secs));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    poll.tick().await; // consume the interval's immediate first tick

    loop {
        // The background index (#429) only runs while nothing else is
        // asked: a request, then the poll, come first, and each step is a
        // single request so neither waits long behind one.
        let indexing = !state.backfill.is_empty();
        let wait = state.backfill_wait;
        let req = tokio::select! {
            biased;
            req = rx.recv() => match req {
                Some(req) => req,
                None => break,
            },
            _ = poll.tick() => {
                graph_poll_inbox(account_id, &account, cache.as_ref(), &mut state, &emit).await;
                continue;
            }
            _ = backfill_pause(wait), if indexing => {
                graph_backfill_step(&account, account_id, cache.as_ref(), &mut state, &emit).await;
                continue;
            }
        };
        // A folder the user opened, rather than a background refresh.
        let user = matches!(req, MailRequest::LoadMessages { .. });
        match req {
            MailRequest::Locate { message_id } => {
                // No folders to search here beyond what the cache already
                // held: answer "not found" so the app can report the miss.
                emit(WorkerEvent::Located { message_id: message_id.clone(), hit: None });
            }
            // Graph reports no mailbox size or limit to the user it signs in.
            MailRequest::Quota => emit(WorkerEvent::Quota(None)),
            MailRequest::FindKeywords => {
                // Categories are defined once per mailbox, with a name and a
                // color: the master list is the whole answer. Counts come
                // from the cache, the only place Hylki has them.
                let mut found: Vec<KeywordFinding> = Vec::new();
                if let Some(token) = graph_token(&account, &emit).await {
                    let t = token.clone();
                    let cats = blocking(move || {
                        graph_get_json(&t, &format!("{GRAPH_BASE}/me/outlook/masterCategories"))
                    }).await;
                    match cats {
                        Ok(v) => {
                            for c in v["value"].as_array().into_iter().flatten() {
                                let Some(name) = c["displayName"].as_str() else { continue };
                                let count = cache
                                    .as_ref()
                                    .map(|c| c.count_with_keyword(account_id, name))
                                    .unwrap_or(0);
                                found.push(KeywordFinding {
                                    keyword: name.to_string(),
                                    name: Some(name.to_string()),
                                    color: graph_preset_color(c["color"].as_str().unwrap_or("")),
                                    count,
                                    folders: Vec::new(),
                                });
                            }
                        }
                        Err(e) => tracing::warn!("[account {account_id}] find tags: categories failed: {e}"),
                    }
                }
                emit(WorkerEvent::KeywordsFound(found));
            }
            MailRequest::RefreshKeywords { .. } => {
                // Categories arrive with each folder's listing, so the
                // listing is the sync: every folder is re-read, which
                // rewrites its cached keywords (#166).
                let mut paths = Vec::new();
                if let Some(token) = graph_token(&account, &emit).await {
                    let folders: Vec<(String, u32)> =
                        state.folders.iter().map(|(p, (id, _))| (p.clone(), *id)).collect();
                    for (path, folder_id) in folders {
                        if graph_load_folder(&token, account_id, folder_id, &path, cache.as_ref(), &mut state)
                            .await
                            .is_ok()
                        {
                            paths.push(path);
                        }
                    }
                }
                emit(WorkerEvent::KeywordsSynced { paths });
            }

            // Cache-only, exactly like the IMAP path: assemble the conversation
            // from every folder's cached summaries.
            // Graph has no equivalent of BODYSTRUCTURE in the shape this path
            // uses: attachments come out of the raw MIME it downloads, so
            // describing one costs the same as fetching it. Listing them
            // through /messages/{id}/attachments would work, but it is a
            // different call on a backend there is no account here to test
            // against — so a Microsoft account's gallery still shows what has
            // been downloaded rather than reaching back through the archive.
            MailRequest::ScanAttachments { folder_path } => {
                emit(WorkerEvent::AttachmentsScanned { folder_path, added: 0, remaining: 0 });
            }
            MailRequest::LoadRelated { message_id, ids } => {
                let messages = cache
                    .as_ref()
                    .map(|c| related_from_cache(c, account_id, &ids))
                    .unwrap_or_default();
                emit(WorkerEvent::Related { message_id, messages });
            }
            MailRequest::LoadThreadSummaries { groups } => {
                let summaries = cache
                    .as_ref()
                    .map(|c| thread_summaries_with_members(c, account_id, &groups))
                    .unwrap_or_default();
                emit(WorkerEvent::ThreadSummaries { summaries });
            }

            MailRequest::LoadMessages { folder_id, path }
            | MailRequest::SyncFolder { folder_id, path } => {
                if let Some(c) = cache.as_ref() {
                    let cached = c.load_messages(account_id, &path, folder_id);
                    if !cached.is_empty() {
                        emit(WorkerEvent::Messages { folder_id, messages: cached });
                    }
                }
                emit(WorkerEvent::Status(i18n("Syncing…")));
                let Some(token) = graph_token(&account, &emit).await else {
                    // No token, so no mail is coming: end the folder's index
                    // (#218) rather than leave the list spinning.
                    emit(WorkerEvent::BackfillDone { folder_id });
                    emit(WorkerEvent::Status(String::new()));
                    continue;
                };
                match graph_sync_folder(
                    &token, account_id, folder_id, &path, cache.as_ref(), &mut state,
                )
                .await
                {
                    Ok((messages, complete)) => {
                        let unread = state.chip_count(folder_id, &messages);
                        emit_graph_body_hits(&token, &account, folder_id, &path, &messages, &state, &emit)
                            .await;
                        emit(WorkerEvent::Messages { folder_id, messages });
                        emit(WorkerEvent::FolderUnread { folder_id, unread });
                        // The index is complete when the first page held the
                        // whole folder, or once the background index has run
                        // (#429). Said at once when it is, so the list's
                        // "Loading more…" tail never waits on nothing (#218).
                        if state.index_after_load(&path, complete, user) {
                            emit(WorkerEvent::BackfillDone { folder_id });
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::net_error(i18n_f("Could not fetch mail: {e}", &[("e", &(e).to_string())])));
                        emit(WorkerEvent::BackfillDone { folder_id });
                    }
                }
                emit(WorkerEvent::Status(String::new()));
            }

            MailRequest::LoadBody { message_id, path, uid } => {
                if let Some(body) = cache.as_ref().and_then(|c| c.load_body(account_id, &path, uid))
                {
                    let check = cache.as_ref().and_then(|c| c.load_sender_check(account_id, &path, uid));
                    emit(WorkerEvent::Body { message_id, path: path.clone(), body });
                    if let Some(check) = check {
                        emit(WorkerEvent::SenderChecked { path: path.to_string(), message_id, check });
                    }
                    continue;
                }
                match graph_fetch_raw(&account, &mut state, &path, uid, &emit).await {
                    Ok(raw) => {
                        let (body, check, _) = render_raw(&raw);
                        if let Some(c) = cache.as_ref() {
                            if body_cacheable(&check) {
                                c.save_body(account_id, &path, uid, &body);
                            }
                            c.save_sender_check(account_id, &path, uid, &check);
                        }
                        emit(WorkerEvent::Body { message_id, path: path.clone(), body });
                        emit(WorkerEvent::SenderChecked { path: path.to_string(), message_id, check });
                    }
                    Err(e) => emit(WorkerEvent::net_error(i18n_f("Could not load message: {e}", &[("e", &(e).to_string())]))),
                }
            }

            MailRequest::LoadBodies { items, path } => {
                for (message_id, uid) in items {
                    if let Some(body) =
                        cache.as_ref().and_then(|c| c.load_body(account_id, &path, uid))
                    {
                        emit(WorkerEvent::Body { message_id, path: path.clone(), body });
                        if let Some(check) = cache.as_ref().and_then(|c| c.load_sender_check(account_id, &path, uid)) {
                            emit(WorkerEvent::SenderChecked { path: path.to_string(), message_id, check });
                        }
                        continue;
                    }
                    match graph_fetch_raw(&account, &mut state, &path, uid, &emit).await {
                        Ok(raw) => {
                            let (body, check, _) = render_raw(&raw);
                            if let Some(c) = cache.as_ref() {
                                if body_cacheable(&check) {
                                    c.save_body(account_id, &path, uid, &body);
                                }
                                c.save_sender_check(account_id, &path, uid, &check);
                            }
                            emit(WorkerEvent::Body { message_id, path: path.clone(), body });
                            emit(WorkerEvent::SenderChecked { path: path.to_string(), message_id, check });
                        }
                        Err(e) => emit(WorkerEvent::net_error(i18n_f("Could not load message: {e}", &[("e", &(e).to_string())]))),
                    }
                }
            }

            MailRequest::LoadSource { message_id: _, path, uid } => {
                match graph_fetch_raw(&account, &mut state, &path, uid, &emit).await {
                    Ok(raw) => emit(WorkerEvent::Source {
                        text: String::from_utf8_lossy(&raw).into_owned(),
                    }),
                    Err(e) => emit(WorkerEvent::net_error(i18n_f("Could not load source: {e}", &[("e", &(e).to_string())]))),
                }
            }

            MailRequest::LoadAttachments { message_id, path, uid, download } => {
                if let Some(c) = cache.as_ref() {
                    let items = c.load_attachments(account_id, &path, uid);
                    if !items.is_empty() {
                        emit(WorkerEvent::Attachments { path: path.to_string(), message_id, items });
                        continue;
                    }
                }
                if !download {
                    emit(WorkerEvent::AttachmentsPending { path: path.to_string(), message_id });
                    continue;
                }
                match graph_fetch_raw(&account, &mut state, &path, uid, &emit).await {
                    Ok(raw) => {
                        let items = extract_attachments(&raw);
                        if let Some(c) = cache.as_ref() {
                            c.save_attachments(account_id, &path, uid, &items);
                        }
                        emit(WorkerEvent::Attachments { path: path.to_string(), message_id, items });
                    }
                    Err(e) => emit(WorkerEvent::net_error(i18n_f("Could not load attachments: {e}", &[("e", &(e).to_string())]))),
                }
            }

            MailRequest::SetSeen { path, uid, seen } => {
                if let Some(c) = cache.as_ref() {
                    c.set_unread(account_id, &path, uid, !seen);
                }
                graph_patch_message(
                    &account,
                    &mut state,
                    &path,
                    uid,
                    serde_json::json!({ "isRead": seen }),
                    &emit,
                )
                .await;
                emit(WorkerEvent::SeenSettled { path, uid });
            }

            MailRequest::SetFlagged { path, uid, flagged } => {
                if let Some(c) = cache.as_ref() {
                    c.set_starred(account_id, &path, uid, flagged);
                }
                let status = if flagged { "flagged" } else { "notFlagged" };
                graph_patch_message(
                    &account,
                    &mut state,
                    &path,
                    uid,
                    serde_json::json!({ "flag": { "flagStatus": status } }),
                    &emit,
                )
                .await;
            }

            MailRequest::SetKeyword { path, uid, keyword, add, .. } => {
                // Categories travel as the whole list, so the cached row is
                // the base: patched first, then sent as it now stands.
                let categories = match cache.as_ref() {
                    Some(c) => {
                        c.set_keyword(account_id, &path, uid, &keyword, add);
                        c.keywords_of(account_id, &path, uid)
                    }
                    None if add => vec![keyword.clone()],
                    None => Vec::new(),
                };
                graph_patch_message(
                    &account,
                    &mut state,
                    &path,
                    uid,
                    serde_json::json!({ "categories": categories }),
                    &emit,
                )
                .await;
                emit(WorkerEvent::TagSettled { path, uid, keyword });
            }

            MailRequest::MarkAllRead { folder_id, path } => {
                // The server side is one PATCH per message; run it over the
                // cached unread rows (the listing window), then settle the cache.
                let unread_uids: Vec<u32> = cache
                    .as_ref()
                    .map(|c| {
                        c.load_messages(account_id, &path, folder_id)
                            .into_iter()
                            .filter(|m| m.unread)
                            .map(|m| m.uid)
                            .collect()
                    })
                    .unwrap_or_default();
                for uid in unread_uids {
                    graph_patch_message(
                        &account,
                        &mut state,
                        &path,
                        uid,
                        serde_json::json!({ "isRead": true }),
                        &emit,
                    )
                    .await;
                }
                if let Some(c) = cache.as_ref() {
                    c.mark_folder_read(account_id, &path);
                }
                emit(WorkerEvent::FolderUnread { folder_id, unread: 0 });
            }

            MailRequest::MoveMessage { path, uid, dest } => {
                if let Err(e) =
                    graph_move_uids(&account, account_id, &mut state, &path, &[uid], &dest, cache.as_ref())
                        .await
                {
                    emit(WorkerEvent::error(i18n_f("Could not move message: {e}", &[("e", &(e).to_string())])));
                }
            }

            MailRequest::MarkSpam { path, uid, dest } => {
                if let Err(e) =
                    graph_move_uids(&account, account_id, &mut state, &path, &[uid], &dest, cache.as_ref())
                        .await
                {
                    emit(WorkerEvent::error(i18n_f("Could not mark as spam: {e}", &[("e", &(e).to_string())])));
                }
            }

            // Microsoft 365 learns from the move itself: back to the Inbox.
            MailRequest::MarkHam { path, uid, dest } => {
                if let Err(e) =
                    graph_move_uids(&account, account_id, &mut state, &path, &[uid], &dest, cache.as_ref())
                        .await
                {
                    emit(WorkerEvent::error(i18n_f("Could not mark as not spam: {e}", &[("e", &(e).to_string())])));
                }
            }
            MailRequest::MarkHamMany { path, uids, dest } => {
                if let Err(e) =
                    graph_move_uids(&account, account_id, &mut state, &path, &uids, &dest, cache.as_ref())
                        .await
                {
                    emit(WorkerEvent::error(i18n_f("Could not mark {len} messages as not spam: {e}", &[("len", &(uids.len()).to_string()), ("e", &(e).to_string())])));
                }
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::MoveMessages { path, uids, dest } => {
                if let Err(e) =
                    graph_move_uids(&account, account_id, &mut state, &path, &uids, &dest, cache.as_ref())
                        .await
                {
                    emit(WorkerEvent::error(i18n_f("Could not move messages: {e}", &[("e", &(e).to_string())])));
                }
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::PurgeMessages { path, uids } => {
                graph_purge_uids(&account, account_id, &mut state, &path, uids, cache.as_ref(), &emit).await;
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::EmptyFolder { folder_id, path } => {
                // No bulk endpoint for an arbitrary folder: list it, then
                // delete each message the way a purge does.
                let Some(token) = graph_token(&account, &emit).await else { continue };
                // Every message in it, not just the newest page or what the
                // cache has indexed so far (#429).
                let listed = match state.folders.get(&path).cloned() {
                    Some((_, gid)) => {
                        let (t, base) = (token.clone(), state.base.clone());
                        blocking(move || graph_list_ids(&t, &base, &gid)).await
                    }
                    None => Err(format!("unknown folder {path}")),
                };
                match listed {
                    Ok(ids) => {
                        let uids: Vec<u32> = ids
                            .into_iter()
                            .map(|(id, _)| {
                                let uid = hash_uid(&id);
                                state.uids.insert(uid, id);
                                uid
                            })
                            .collect();
                        let n = uids.len();
                        graph_purge_uids(&account, account_id, &mut state, &path, uids, cache.as_ref(), &emit).await;
                        tracing::info!("emptied {path}: {n} message(s) erased");
                        emit(WorkerEvent::Messages { folder_id, messages: Vec::new() });
                        emit(WorkerEvent::FolderUnread { folder_id, unread: 0 });
                    }
                    Err(e) => emit(WorkerEvent::error(i18n_f("Could not empty the folder: {e}", &[("e", &e.to_string())]))),
                }
            }

            MailRequest::UndoMove { path, dest, dest_folder_id, message_ids } => {
                match graph_undo_move(&account, account_id, &mut state, &path, &dest, &message_ids, cache.as_ref())
                    .await
                {
                    Ok(0) => {
                        tracing::info!("undo: the messages are no longer where that move put them");
                    }
                    Ok(_) => {
                        // Reload the restored folder so the messages reappear.
                        if let Some(token) = graph_token(&account, &emit).await {
                            if let Ok(messages) = graph_load_folder(
                                &token,
                                account_id,
                                dest_folder_id,
                                &dest,
                                cache.as_ref(),
                                &mut state,
                            )
                            .await
                            {
                                let unread = state.chip_count(dest_folder_id, &messages);
                                emit(WorkerEvent::Messages {
                                    folder_id: dest_folder_id,
                                    messages,
                                });
                                emit(WorkerEvent::FolderUnread {
                                    folder_id: dest_folder_id,
                                    unread,
                                });
                            }
                        }
                    }
                    Err(e) => emit(WorkerEvent::error(i18n_f("Undo failed: {e}", &[("e", &(e).to_string())]))),
                }
                // The app spins its busy indicator until an undo answers.
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::CreateFolder { path } => {
                let Some(token) = graph_token(&account, &emit).await else { continue };
                // "A/B" nests under A (resolved from the last listing);
                // otherwise a top-level folder.
                let (url, name) = match path.rsplit_once('/') {
                    Some((parent, leaf)) => match state.folders.get(parent) {
                        Some((_, pgid)) => (
                            format!("{GRAPH_BASE}/me/mailFolders/{pgid}/childFolders"),
                            leaf.to_string(),
                        ),
                        None => (format!("{GRAPH_BASE}/me/mailFolders"), path.clone()),
                    },
                    None => (format!("{GRAPH_BASE}/me/mailFolders"), path.clone()),
                };
                let t = token.clone();
                let body = serde_json::json!({ "displayName": name });
                let r = blocking(move || graph_send_json(&t, "POST", &url, &body)).await;
                match r {
                    Ok(_) => {
                        refresh_graph_folders(&token, account_id, cache.as_ref(), &mut state, &emit)
                            .await;
                    }
                    Err(e) => emit(WorkerEvent::error(i18n_f("Could not create folder: {e}", &[("e", &(e).to_string())]))),
                }
            }

            MailRequest::RenameFolder { old_path, new_path } => {
                let Some(token) = graph_token(&account, &emit).await else { continue };
                let Some((_, gid)) = state.folders.get(&old_path).cloned() else {
                    emit(WorkerEvent::error(i18n("Could not rename folder: unknown folder")));
                    continue;
                };
                // Graph renames by displayName; moving between parents would be
                // a different call — the sidebar only renames leaves here.
                let leaf = new_path.rsplit('/').next().unwrap_or(&new_path).to_string();
                let t = token.clone();
                let url = format!("{GRAPH_BASE}/me/mailFolders/{gid}");
                let body = serde_json::json!({ "displayName": leaf });
                let r =
                    blocking(move || graph_send_json(&t, "PATCH", &url, &body)).await;
                match r {
                    Ok(_) => {
                        refresh_graph_folders(&token, account_id, cache.as_ref(), &mut state, &emit)
                            .await;
                    }
                    Err(e) => emit(WorkerEvent::error(i18n_f("Could not rename folder: {e}", &[("e", &(e).to_string())]))),
                }
            }

            MailRequest::SetHiddenFolders { paths } => {
                state.hidden = paths;
                let Some(token) = graph_token(&account, &emit).await else { continue };
                refresh_graph_folders(&token, account_id, cache.as_ref(), &mut state, &emit).await;
            }

            MailRequest::DeleteFolder { path, trash: _ } => {
                // Graph's folder delete moves the folder (contents included) to
                // Deleted Items itself; no separate content move needed.
                let Some(token) = graph_token(&account, &emit).await else { continue };
                let Some((_, gid)) = state.folders.get(&path).cloned() else {
                    emit(WorkerEvent::error(i18n("Could not delete folder: unknown folder")));
                    continue;
                };
                let t = token.clone();
                let url = format!("{GRAPH_BASE}/me/mailFolders/{gid}");
                let r = blocking(move || graph_delete_req(&t, &url)).await;
                match r {
                    Ok(()) => {
                        refresh_graph_folders(&token, account_id, cache.as_ref(), &mut state, &emit)
                            .await;
                    }
                    Err(e) => emit(WorkerEvent::error(i18n_f("Could not delete folder: {e}", &[("e", &(e).to_string())]))),
                }
            }

            MailRequest::SaveDraft { message, folder_id, path, autosave, template } => {
                if autosave.is_none() {
                    emit(WorkerEvent::Status(saving_text(template)));
                }
                // A template (#360) is made where every new message is, in
                // Drafts, and moved on: so its folder is first made if the
                // account has none, and found by its Graph id.
                let mut folder_id = folder_id;
                let mut template_gid = None;
                if template {
                    if !state.folders.contains_key(&path) {
                        if let Some(token) = graph_token(&account, &emit).await {
                            let (t, url) = (token.clone(), format!("{GRAPH_BASE}/me/mailFolders"));
                            let body = serde_json::json!({ "displayName": path });
                            match blocking(move || graph_send_json(&t, "POST", &url, &body)).await {
                                Ok(_) => refresh_graph_folders(&token, account_id, cache.as_ref(), &mut state, &emit).await,
                                Err(e) => tracing::warn!("graph: could not create {path}: {e}"),
                            }
                        }
                    }
                    if let Some((id, gid)) = state.folders.get(&path) {
                        folder_id = *id;
                        template_gid = Some(gid.clone());
                    }
                }
                let mut message = OutgoingMessage { sign: false, encrypt: false, ..*message };
                restore_msgid_case(cache.as_ref(), &mut message);
                let mut why = String::new();
                let saved = match build_draft(&account, &message) {
                    Ok(email) => {
                        let message_id = draft_message_id(&email);
                        let raw = email.formatted();
                        match graph_token(&account, &emit).await {
                            Some(token) => {
                                let t = token.clone();
                                let url = format!("{GRAPH_BASE}/me/messages");
                                let r = blocking(move || {
                                    graph_post_mime(&t, &url, &raw)
                                }).await;
                                let r = match (r, &template_gid) {
                                    (Ok(created), Some(dest)) => match created["id"].as_str() {
                                        Some(gid) => {
                                            let (t, url) = (token.clone(), format!("{GRAPH_BASE}/me/messages/{gid}/move"));
                                            let body = serde_json::json!({ "destinationId": dest });
                                            blocking(move || graph_send_json(&t, "POST", &url, &body)).await
                                        }
                                        None => Err("the new message has no id".to_string()),
                                    },
                                    (Ok(_), None) if template => Err(i18n("unknown folder")),
                                    (r, _) => r,
                                };
                                match r {
                                    Ok(_) => {
                                        // Replace the previous version of this draft.
                                        if let Some(o) = &message.draft_origin {
                                            graph_drop_draft_origin(&account, &mut state, account_id, o, cache.as_ref(), &emit).await;
                                        }
                                        if let Ok(messages) = graph_load_folder(
                                            &token,
                                            account_id,
                                            folder_id,
                                            &path,
                                            cache.as_ref(),
                                            &mut state,
                                        )
                                        .await
                                        {
                                            emit(WorkerEvent::Messages { folder_id, messages });
                                        }
                                        Some(message_id)
                                    }
                                    Err(e) => {
                                        why = e.to_string();
                                        None
                                    }
                                }
                            }
                            None => {
                                why = i18n("Could not reach the server");
                                None
                            }
                        }
                    }
                    Err(e) => {
                        why = e.to_string();
                        None
                    }
                };
                emit(WorkerEvent::Status(String::new()));
                match saved {
                    Some(id) => emit(WorkerEvent::DraftSaved { autosave, message_id: Some(id) }),
                    None => draft_not_saved(autosave, template, why, message, &emit),
                }
            }

            // `sent_path` is unused: Graph's sendMail files the Sent copy itself.
            // Send Later (#145): into the Outbox until its time.
            MailRequest::Send { mut message, sent_path: _ }
                if message.send_at.is_some_and(|t| t > crate::datefmt::now()) =>
            {
                let at = message.send_at.unwrap_or_default();
                restore_msgid_case(cache.as_ref(), &mut message);
                schedule_send(cache.as_ref(), account_id, &account, &message, None, at, &emit);
                if let Some(o) = &message.draft_origin {
                    graph_drop_draft_origin(&account, &mut state, account_id, o, cache.as_ref(), &emit).await;
                }
            }

            MailRequest::Send { mut message, sent_path: _ } => {
                emit(WorkerEvent::Status(i18n("Sending…")));
                restore_msgid_case(cache.as_ref(), &mut message);
                match graph_send_message(&account, &message, &emit).await {
                    Ok(()) => {
                        emit(WorkerEvent::Status(String::new()));
                        // If sending an edited draft, remove the obsolete draft.
                        if let Some(o) = message.draft_origin.clone() {
                            if o.account_id == account_id {
                                graph_drop_draft_origin(&account, &mut state, account_id, &o, cache.as_ref(), &emit).await;
                                if let Some(token) = graph_token(&account, &emit).await {
                                    if let Ok(messages) = graph_load_folder(
                                        &token,
                                        account_id,
                                        o.folder_id,
                                        &o.path,
                                        cache.as_ref(),
                                        &mut state,
                                    )
                                    .await
                                    {
                                        emit(WorkerEvent::Messages {
                                            folder_id: o.folder_id,
                                            messages,
                                        });
                                    }
                                }
                            }
                        }
                        drop_superseded_outbox(cache.as_ref(), account_id, &message, &emit);
                        // The server files the copy in Sent Items itself; list
                        // that folder now so the copy is in the cache and can
                        // join the conversation it answers (#199).
                        let sent = cache
                            .as_ref()
                            .and_then(|c| c.load_folders(account_id).into_iter().find(|f| f.kind == FolderKind::Sent));
                        if let (Some(c), Some(sent)) = (cache.as_ref(), sent) {
                            if let Some(token) = graph_token(&account, &emit).await {
                                if let Ok(messages) = graph_load_folder(
                                    &token, account_id, sent.id, &sent.path, Some(c), &mut state,
                                )
                                .await
                                {
                                    emit(WorkerEvent::Messages { folder_id: sent.id, messages });
                                }
                            }
                        }
                        emit(WorkerEvent::Sent);
                    }
                    Err(e) => {
                        emit(WorkerEvent::Status(String::new()));
                        send_failed(cache.as_ref(), account_id, &account, &message, None, &e, false, &emit);
                    }
                }
            }

            MailRequest::LoadOutbox => emit_outbox(cache.as_ref(), account_id, &emit),

            MailRequest::DeleteOutbox { id } => delete_queued(cache.as_ref(), account_id, id, &emit),

            MailRequest::FlushOutbox { id } => {
                graph_flush_outbox(cache.as_ref(), account_id, &account, id, &emit).await;
            }

            MailRequest::RefreshUnread => {
                // Quiet token fetch: this rides the auto-fetch tick, and a
                // signed-out account already errors on interactive actions.
                let quiet = |_: WorkerEvent| {};
                if let Some(token) = graph_token(&account, &quiet).await {
                    graph_refresh_unread(&token, account_id, cache.as_ref(), &mut state, &emit)
                        .await;
                    auto_empty_graph(&token, account_id, &account, cache.as_ref(), &mut state, &mut last_auto_empty, &emit)
                        .await;
                }
            }

            MailRequest::Reconnect => {
                if let Some(token) = graph_token(&account, &emit).await {
                    refresh_graph_folders(&token, account_id, cache.as_ref(), &mut state, &emit)
                        .await;
                }
            }

            MailRequest::Settle { path, uids } => emit(WorkerEvent::MovesSettled { path, uids }),

            MailRequest::ExportRaw { token, path, uid, .. } => {
                let raw = graph_fetch_raw(&account, &mut state, &path, uid, &emit).await;
                emit(WorkerEvent::RawExported { token, raw });
            }

            // Graph files a message posted to a folder as a draft; it is never
            // offered as a destination.
            MailRequest::ImportRaw { token, .. } => emit(WorkerEvent::RawImported {
                token,
                result: Err(i18n("A Microsoft account can't receive mail from another account")),
            }),

            // Microsoft 365 deletes an attachment in place: the message
            // keeps its id, and only what the cache holds of it changes.
            MailRequest::DeleteAttachment { message_id, path, uid, name, size } => {
                emit(WorkerEvent::Status(i18n("Removing the attachment…")));
                let result = graph_delete_attachment(&account, &mut state, &path, uid, &name, size, &emit).await;
                emit(WorkerEvent::Status(String::new()));
                match result {
                    Ok(()) => {
                        if let Some(c) = cache.as_ref() {
                            c.delete_body(account_id, &path, uid);
                            match graph_fetch_raw(&account, &mut state, &path, uid, &emit).await {
                                Ok(raw) => cache_rewritten(c, account_id, &path, uid, &raw),
                                // Fetched again at the next open.
                                Err(_) => {
                                    c.save_attachments(account_id, &path, uid, &[]);
                                    c.save_attachment_meta(account_id, &path, uid, &[]);
                                }
                            }
                        }
                        emit(WorkerEvent::AttachmentDeleted { message_id, path, uid, new_uid: Some(uid), name, size });
                    }
                    Err(e) => strip_refused(
                        &emit,
                        i18n_f("Could not remove the attachment: {e}", &[("e", &e)]),
                        message_id,
                        name,
                        size,
                    ),
                }
            }
        }
    }
}

/// One poll tick: refresh the Inbox and emit it. The app diffs the arriving
/// list against its cache, so this drives both the visible refresh and the
/// desktop notification for genuinely new mail. Token failures stay quiet here
/// — a signed-out account already errors on every interactive action.
async fn graph_poll_inbox(
    account_id: u32,
    account: &AccountConfig,
    cache: Option<&Cache>,
    state: &mut GraphState,
    emit: &impl Fn(WorkerEvent),
) {
    let quiet = |_: WorkerEvent| {};
    let Some(token) = graph_token(account, &quiet).await else { return };
    if state.inbox.is_none() {
        // The startup folder listing may have failed (offline launch).
        refresh_graph_folders(&token, account_id, cache, state, emit).await;
    }
    let Some((folder_id, path)) = state.inbox.clone() else { return };
    if let Ok(messages) =
        graph_load_folder(&token, account_id, folder_id, &path, cache, state).await
    {
        let unread = messages.iter().filter(|m| m.unread).count() as u32;
        emit_graph_body_hits(&token, account, folder_id, &path, &messages, state, emit).await;
        emit(WorkerEvent::Messages { folder_id, messages });
        emit(WorkerEvent::FolderUnread { folder_id, unread });
    }
    // The poll only re-syncs the inbox; mail filed by server-side rules lands
    // in other folders without passing through it. Re-list for every folder's
    // unreadItemCount so their chips keep pace too.
    graph_refresh_unread(&token, account_id, cache, state, emit).await;
}

/// Re-list the folders for their server-side unread counts and push them, but
/// stay silent on failure — this runs on the background poll, where a transient
/// network error is not worth a banner (unlike [`refresh_graph_folders`]).
///
/// Emits the merged folder list first (so a renamed/new folder gets fresh ids),
/// then one [`WorkerEvent::FolderUnread`] per folder: the per-folder event is
/// the path allowed to assert a genuine zero, which the app's SetFolders merge
/// deliberately ignores.
pub(super) async fn graph_refresh_unread(
    token: &str,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut GraphState,
    emit: &impl Fn(WorkerEvent),
) {
    let t = token.to_string();
    let roles = state.roles.clone();
    let Ok(list) = blocking(move || graph_list_folders(&t, account_id, &roles)).await
    else {
        return;
    };
    if list.is_empty() {
        return;
    }
    state.adopt_folders(&list);
    let folders: Vec<Folder> = list.into_iter().map(|f| f.folder).collect();
    if let Some(c) = cache {
        c.save_folders(account_id, &folders);
    }
    let counts: Vec<(u32, u32)> = folders.iter().map(|f| (f.id, f.unread)).collect();
    emit(WorkerEvent::Folders(folders));
    for (folder_id, unread) in counts {
        emit(WorkerEvent::FolderUnread { folder_id, unread });
    }
}

/// Delete messages for good, one Graph request each; the cache and the uid
/// map forget every one that went.
async fn graph_purge_uids(
    account: &AccountConfig,
    account_id: u32,
    state: &mut GraphState,
    path: &str,
    uids: Vec<u32>,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    for uid in uids {
        let deleted = match graph_resolve(account, state, path, uid, emit).await {
            Some((token, gid)) => {
                let url = format!("{GRAPH_BASE}/me/messages/{gid}");
                blocking(move || graph_delete_req(&token, &url)).await
                    .is_ok()
            }
            None => false,
        };
        if deleted {
            state.uids.remove(&uid);
            if let Some(c) = cache {
                c.delete_message(account_id, path, uid);
            }
        }
    }
}

/// A fresh GOA token, or a user-actionable error.
pub(super) async fn graph_token(account: &AccountConfig, emit: &impl Fn(WorkerEvent)) -> Option<String> {
    match fetch_oauth_token(account).await {
        Some(t) => Some(t),
        None => {
            // Signed in through Hylki's own Microsoft app (#329) rather than
            // GNOME Online Accounts: it is Hylki's sign-in to renew.
            emit(WorkerEvent::net_error(if account.goa_id.is_some() {
                format!(
                    "GNOME Online Accounts could not provide a sign-in token for {}. Open \
                     Settings → Online Accounts and sign in again.",
                    account.email
                )
            } else {
                format!(
                    "Microsoft did not accept the saved sign-in for {}. Open Hylki's Settings → \
                     Mail Accounts and sign in to it again.",
                    account.email
                )
            }));
            None
        }
    }
}

/// Re-list the folders, emit them, remember the path → Graph-id map.
async fn refresh_graph_folders(
    token: &str,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut GraphState,
    emit: &impl Fn(WorkerEvent),
) {
    let t = token.to_string();
    let roles = state.roles.clone();
    let r = blocking(move || graph_list_folders(&t, account_id, &roles)).await;
    match r {
        Ok(mut list) => {
            // Hidden folders (#239) leave here, before the ids settle.
            list.retain(|f| !crate::models::folder_is_hidden(&f.folder.path, Some("/"), &state.hidden));
            state.adopt_folders(&list);
            let folders: Vec<Folder> = list.into_iter().map(|f| f.folder).collect();
            if let Some(c) = cache {
                c.save_folders(account_id, &folders);
            }
            emit(WorkerEvent::Folders(folders));
        }
        Err(e) => emit(WorkerEvent::net_error(i18n_f("Could not list folders: {e}", &[("e", &(e).to_string())]))),
    }
}

/// List a folder's newest summaries, refresh the uid map and the cache, and
/// answer with the cached folder as it now stands.
async fn graph_load_folder(
    token: &str,
    account_id: u32,
    folder_id: u32,
    path: &str,
    cache: Option<&Cache>,
    state: &mut GraphState,
) -> Result<Vec<Message>, String> {
    graph_sync_folder(token, account_id, folder_id, path, cache, state).await.map(|(m, _)| m)
}

/// [`graph_load_folder`], saying too whether the listing was the whole
/// folder (it fitted in [`GRAPH_FIRST_PAGE`]).
async fn graph_sync_folder(
    token: &str,
    account_id: u32,
    folder_id: u32,
    path: &str,
    cache: Option<&Cache>,
    state: &mut GraphState,
) -> Result<(Vec<Message>, bool), String> {
    // An unknown path usually means the folder list hasn't been fetched yet
    // (or the folder is new) — refresh it once before giving up.
    if !state.folders.contains_key(path) {
        let t = token.to_string();
        let roles = state.roles.clone();
        if let Ok(list) =
            blocking(move || graph_list_folders(&t, account_id, &roles)).await
        {
            state.adopt_folders(&list);
        }
    }
    let (_, gid) = state
        .folders
        .get(path)
        .cloned()
        .ok_or_else(|| format!("unknown folder {path}"))?;

    let (t, base) = (token.to_string(), state.base.clone());
    let (listed, more) = blocking(move || {
        graph_list_messages(&t, &base, &gid, account_id, folder_id)
    }).await?;

    let mut messages = Vec::with_capacity(listed.len());
    for (m, gid) in listed {
        state.uids.insert(m.uid, gid);
        messages.push(m);
    }
    let Some(c) = cache else { return Ok((messages, !more)) };
    // Merged over the cached folder rather than replacing it, which kept
    // only the newest page (#429): the listing speaks for what it covers,
    // the background index for the rest.
    let cached = c.cached_uid_times(account_id, path);
    for uid in window_gone(&cached, &messages, !more) {
        c.delete_message(account_id, path, uid);
        state.uids.remove(&uid);
    }
    c.upsert_messages(account_id, path, &messages);
    Ok((c.load_messages(account_id, path, folder_id), !more))
}

/// Pause the background index after a failed step, until `wait`.
async fn backfill_pause(wait: Option<std::time::Instant>) {
    if let Some(at) = wait {
        tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await;
    }
}

/// One step of the background index (#429): one request for the folder at
/// the front of the queue, a page of its ids or a run of its missing mail.
/// The cache gains what came, and the list hears of it as an append; the
/// folder's index is done once its last run is in.
///
/// The id listing, rather than a delta query, is what tells the cache what
/// the server no longer has: it is the listing every folder load already
/// makes, asked for ids alone, and keeps no sync state to store, expire or
/// recover from.
async fn graph_backfill_step(
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut GraphState,
    emit: &impl Fn(WorkerEvent),
) {
    let quiet = |_: WorkerEvent| {};
    let ok = match graph_token(account, &quiet).await {
        Some(token) => graph_backfill_with(&token, account_id, cache, state, emit).await,
        None => false,
    };
    if ok {
        state.backfill_wait = None;
        return;
    }
    state.backfill_wait = Some(std::time::Instant::now() + GRAPH_BACKFILL_BACKOFF);
    note_backfill_failure(state, emit);
}

/// A step of the folder at the front of the index queue failed. Offline, or
/// refused: after a few tries the list stops promising more (#218), and the
/// folder goes to the back of the queue, so one folder the server keeps
/// refusing does not hold up every other folder's index behind it.
fn note_backfill_failure(state: &mut GraphState, emit: &impl Fn(WorkerEvent)) {
    let Some(job) = state.backfill.front_mut() else { return };
    job.failures += 1;
    if job.failures < 3 {
        return;
    }
    if !job.told {
        job.told = true;
        if let Some((folder_id, _)) = state.folders.get(&job.path) {
            emit(WorkerEvent::BackfillDone { folder_id: *folder_id });
        }
    }
    job.failures = 0;
    if let Some(job) = state.backfill.pop_front() {
        state.backfill.push_back(job);
    }
}

/// [`graph_backfill_step`] with its token; false when the request failed,
/// the job then still at the front of the queue to try again.
async fn graph_backfill_with(
    token: &str,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut GraphState,
    emit: &impl Fn(WorkerEvent),
) -> bool {
    let Some(mut job) = state.backfill.pop_front() else { return true };
    // A folder gone or hidden since it was queued has nothing left to index.
    let Some((folder_id, gid)) = state.folders.get(&job.path).cloned() else { return true };
    let base = state.base.clone();
    match &mut job.phase {
        GraphBackfillPhase::Listing { next, ids, since } => {
            let url = next.clone().unwrap_or_else(|| graph_ids_url(&base, &gid));
            let t = token.to_string();
            let (page, more) = match blocking(move || graph_id_page(&t, &url)).await {
                Ok(page) => page,
                Err(e) => {
                    tracing::warn!("[account {account_id}] index of {}: {e}", job.path);
                    state.backfill.push_front(job);
                    return false;
                }
            };
            ids.extend(page);
            if more.is_some() {
                *next = more;
                job.failures = 0;
                state.backfill.push_front(job);
                return true;
            }
            for (id, _) in ids.iter() {
                state.uids.insert(hash_uid(id), id.clone());
            }
            let cached = cache.map(|c| c.cached_uid_times(account_id, &job.path)).unwrap_or_default();
            let (gone, runs) = graph_index_plan(ids, &cached, *since);
            tracing::info!(
                "[account {account_id}] index of {}: {} on the server, {} to fetch in {} run(s), {} gone",
                job.path,
                ids.len(),
                runs.iter().map(|r| r.ids.len()).sum::<usize>(),
                runs.len(),
                gone.len()
            );
            if let (Some(c), false) = (cache, gone.is_empty()) {
                for uid in &gone {
                    c.delete_message(account_id, &job.path, *uid);
                }
                // The list still shows what went; this is the folder as it is.
                emit(WorkerEvent::Messages { folder_id, messages: c.load_messages(account_id, &job.path, folder_id) });
            }
            job.failures = 0;
            job.phase = GraphBackfillPhase::Fetching { runs: runs.into() };
        }
        GraphBackfillPhase::Fetching { runs } => {
            let Some(run) = runs.front().cloned() else { return true };
            let t = token.to_string();
            let fetched = blocking(move || graph_fetch_run(&t, &base, &gid, &run, account_id, folder_id)).await;
            let fetched = match fetched {
                Ok(fetched) => fetched,
                Err(e) => {
                    tracing::warn!("[account {account_id}] index of {}: {e}", job.path);
                    state.backfill.push_front(job);
                    return false;
                }
            };
            runs.pop_front();
            job.failures = 0;
            let mut messages = Vec::with_capacity(fetched.len());
            for (m, id) in fetched {
                state.uids.insert(m.uid, id);
                messages.push(m);
            }
            if !messages.is_empty() {
                if let Some(c) = cache {
                    c.upsert_messages(account_id, &job.path, &messages);
                }
                emit(WorkerEvent::MessagesAppend { folder_id, messages });
            }
        }
    }
    if matches!(&job.phase, GraphBackfillPhase::Fetching { runs } if runs.is_empty()) {
        state.listed.insert(job.path.clone(), std::time::Instant::now());
        if !job.told {
            emit(WorkerEvent::BackfillDone { folder_id });
        }
    } else {
        state.backfill.push_front(job);
    }
    true
}

/// Delete the draft a message was opened from, on the server and in the
/// cache, once it has been sent, scheduled or saved as a new version.
async fn graph_drop_draft_origin(
    account: &AccountConfig,
    state: &mut GraphState,
    account_id: u32,
    origin: &crate::models::DraftOrigin,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    if origin.account_id != account_id {
        return;
    }
    if let Some((tok, gid)) = graph_resolve(account, state, &origin.path, origin.uid, emit).await {
        let url = format!("{GRAPH_BASE}/me/messages/{gid}");
        let _ = blocking(move || graph_delete_req(&tok, &url)).await;
    }
    if let Some(c) = cache {
        c.delete_message(account_id, &origin.path, origin.uid);
    }
}

/// Graph ids of messages a search on the server found (see
/// `server_search.rs`), by account address and uid: such a message may be in no
/// listing this worker has made, and opening it needs its id.
static SEARCH_IDS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<(String, u32), String>>> =
    std::sync::OnceLock::new();

/// Most search ids kept; past it the oldest searches' ids are forgotten,
/// and those messages are found by listing their folder as before.
const SEARCH_IDS_CAP: usize = 20_000;

pub(super) fn remember_search_id(account: &str, uid: u32, gid: &str) {
    let map = SEARCH_IDS.get_or_init(Default::default);
    if let Ok(mut m) = map.lock() {
        if m.len() >= SEARCH_IDS_CAP {
            m.clear();
        }
        m.insert((account.to_lowercase(), uid), gid.to_string());
    }
}

fn search_id(account: &str, uid: u32) -> Option<String> {
    SEARCH_IDS.get()?.lock().ok()?.get(&(account.to_lowercase(), uid)).cloned()
}

/// Resolve a message uid to (token, Graph id), re-listing the folder once if
/// the uid isn't in the map (fresh start from cache, or a moved message).
async fn graph_resolve(
    account: &AccountConfig,
    state: &mut GraphState,
    path: &str,
    uid: u32,
    emit: &impl Fn(WorkerEvent),
) -> Option<(String, String)> {
    let token = graph_token(account, emit).await?;
    if let Some(gid) = state.uids.get(&uid) {
        return Some((token, gid.clone()));
    }
    if let Some(gid) = search_id(&account.email, uid) {
        state.uids.insert(uid, gid.clone());
        return Some((token, gid));
    }
    // Not indexed yet: list the folder (fills the uid map) and try again. The
    // account/folder ids only label the discarded summaries, so zeros are fine.
    let _ = graph_load_folder(&token, 0, 0, path, None, state).await;
    if let Some(gid) = state.uids.get(&uid) {
        return Some((token, gid.clone()));
    }
    // Older than the first page (#429), from an earlier session's index:
    // the folder's id listing names every message in it, once a while.
    let recent = |at: Option<&std::time::Instant>| at.is_some_and(|at| at.elapsed() < GRAPH_RELIST_AFTER);
    if !recent(state.listed.get(path)) && !recent(state.looked_up.get(path)) {
        if let Some((_, folder_gid)) = state.folders.get(path).cloned() {
            let (t, base) = (token.clone(), state.base.clone());
            if let Ok(ids) = blocking(move || graph_list_ids(&t, &base, &folder_gid)).await {
                for (id, _) in ids {
                    state.uids.insert(hash_uid(&id), id);
                }
                state.looked_up.insert(path.to_string(), std::time::Instant::now());
            }
        }
    }
    state.uids.get(&uid).map(|gid| (token, gid.clone()))
}

/// Delete the attachment called `name` (nearest `size` among namesakes)
/// from a message (#289).
async fn graph_delete_attachment(
    account: &AccountConfig,
    state: &mut GraphState,
    path: &str,
    uid: u32,
    name: &str,
    size: u64,
    emit: &impl Fn(WorkerEvent),
) -> Result<(), String> {
    let (token, gid) = graph_resolve(account, state, path, uid, emit)
        .await
        .ok_or_else(|| "message not found".to_string())?;
    let name = name.to_string();
    blocking(move || {
        let url = format!("{GRAPH_BASE}/me/messages/{gid}/attachments?$select=id,name,size");
        let listed = graph_get_json(&token, &url)?;
        let aid = listed["value"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|a| a["name"].as_str() == Some(name.as_str()))
            .min_by_key(|a| a["size"].as_u64().unwrap_or(0).abs_diff(size))
            .and_then(|a| a["id"].as_str().map(str::to_string))
            .ok_or_else(|| i18n("The attachment is no longer in the message on the server"))?;
        graph_delete_req(&token, &format!("{GRAPH_BASE}/me/messages/{gid}/attachments/{aid}"))
    }).await
}

/// Fetch a message's raw RFC 822 bytes.
async fn graph_fetch_raw(
    account: &AccountConfig,
    state: &mut GraphState,
    path: &str,
    uid: u32,
    emit: &impl Fn(WorkerEvent),
) -> Result<Vec<u8>, String> {
    let (token, gid) = graph_resolve(account, state, path, uid, emit)
        .await
        .ok_or_else(|| "message not found".to_string())?;
    let url = format!("{GRAPH_BASE}/me/messages/{gid}/$value");
    blocking(move || graph_get_bytes(&token, &url)).await
}

/// PATCH one message (read state, flag). Errors are logged, not surfaced — the
/// optimistic UI state already changed and a stale flag is not worth a dialog.
async fn graph_patch_message(
    account: &AccountConfig,
    state: &mut GraphState,
    path: &str,
    uid: u32,
    body: serde_json::Value,
    emit: &impl Fn(WorkerEvent),
) {
    let Some((token, gid)) = graph_resolve(account, state, path, uid, emit).await else {
        return;
    };
    let url = format!("{GRAPH_BASE}/me/messages/{gid}");
    let r = blocking(move || graph_send_json(&token, "PATCH", &url, &body)).await;
    if let Err(e) = r {
        tracing::warn!("graph: could not update message flags: {e}");
    }
}

/// Move messages to another folder. The Graph id changes in transit; the moved
/// entries leave the uid map and the destination re-indexes on its next load.
async fn graph_move_uids(
    account: &AccountConfig,
    account_id: u32,
    state: &mut GraphState,
    path: &str,
    uids: &[u32],
    dest: &str,
    cache: Option<&Cache>,
) -> Result<(), String> {
    let quiet = |_e: WorkerEvent| {};
    let token = graph_token(account, &quiet)
        .await
        .ok_or_else(|| "no sign-in token from GNOME Online Accounts".to_string())?;
    let dest_gid = match state.folders.get(dest) {
        Some((_, gid)) => gid.clone(),
        None => return Err(format!("unknown folder {dest}")),
    };
    for &uid in uids {
        let Some(gid) = state.uids.get(&uid).cloned() else { continue };
        let t = token.clone();
        let url = format!("{GRAPH_BASE}/me/messages/{gid}/move");
        let body = serde_json::json!({ "destinationId": dest_gid });
        blocking(move || graph_send_json(&t, "POST", &url, &body)).await?;
        state.uids.remove(&uid);
        if let Some(c) = cache {
            c.delete_message(account_id, path, uid);
        }
    }
    Ok(())
}

/// Undo a move: the messages are in `path` (where the move put them) with new
/// Graph ids — find them by Internet Message-ID and move them back to `dest`.
async fn graph_undo_move(
    account: &AccountConfig,
    account_id: u32,
    state: &mut GraphState,
    path: &str,
    dest: &str,
    message_ids: &[String],
    cache: Option<&Cache>,
) -> Result<usize, String> {
    let quiet = |_e: WorkerEvent| {};
    let token = graph_token(account, &quiet)
        .await
        .ok_or_else(|| "no sign-in token from GNOME Online Accounts".to_string())?;
    let (folder_id, gid) = state
        .folders
        .get(path)
        .cloned()
        .ok_or_else(|| format!("unknown folder {path}"))?;
    let wanted: std::collections::HashSet<&str> =
        message_ids.iter().map(|s| s.as_str()).collect();

    let (t, base, folder_gid) = (token.clone(), state.base.clone(), gid.clone());
    let (listed, _) = blocking(move || {
        graph_list_messages(&t, &base, &folder_gid, account_id, folder_id)
    }).await?;

    let mut uids = Vec::new();
    let mut found: std::collections::HashSet<String> = Default::default();
    for (m, mgid) in listed {
        if wanted.contains(m.message_id.as_str()) {
            state.uids.insert(m.uid, mgid);
            uids.push(m.uid);
            found.insert(m.message_id);
        }
    }
    // A message keeps its arrival date when it moves, so an old one lands
    // below the newest page (#429): those are asked for by Message-ID.
    for mid in wanted.iter().filter(|mid| !found.contains(**mid)) {
        let exact = cache.map(|c| c.exact_msgids(mid)).unwrap_or_else(|| mid.to_string());
        let (t, base, folder_gid) = (token.clone(), state.base.clone(), gid.clone());
        match blocking(move || graph_find_by_msgid(&t, &base, &folder_gid, &exact)).await {
            Ok(hits) => {
                for (m, mgid) in hits {
                    if !uids.contains(&m.uid) {
                        state.uids.insert(m.uid, mgid);
                        uids.push(m.uid);
                    }
                }
            }
            Err(e) => tracing::warn!("undo: could not look up <{mid}> in {path}: {e}"),
        }
    }
    if uids.is_empty() {
        return Ok(0);
    }
    let n = uids.len();
    graph_move_uids(account, account_id, state, path, &uids, dest, cache).await?;
    Ok(n)
}

/// Send over Graph: `sendMail` takes the same raw MIME `build_email` produces
/// and files the Sent copy itself.
async fn graph_send_message(
    account: &AccountConfig,
    message: &OutgoingMessage,
    emit: &impl Fn(WorkerEvent),
) -> Result<(), String> {
    let email = build_email(account, message).map_err(|e| e.to_string())?;
    let raw = email.formatted();
    let token = graph_token(account, emit)
        .await
        .ok_or_else(|| "no sign-in token from GNOME Online Accounts".to_string())?;
    let url = format!("{GRAPH_BASE}/me/sendMail");
    blocking(move || graph_post_mime(&token, &url, &raw)).await?;
    Ok(())
}

/// The Outbox retry loop for Graph accounts: same queue and bookkeeping as
/// [`flush_outbox`], with `sendMail` as the transport (Sent copy automatic).
async fn graph_flush_outbox(
    cache: Option<&Cache>,
    account_id: u32,
    account: &AccountConfig,
    id: Option<u32>,
    emit: &impl Fn(WorkerEvent),
) {
    let Some(cache) = cache else { return };
    let now = crate::datefmt::now();
    let items: Vec<crate::models::OutboxItem> = cache
        .outbox_items(account_id)
        .into_iter()
        .filter(|item| match id {
            Some(wanted) => wanted == item.id,
            None => item.send_at.is_none_or(|t| t <= now),
        })
        .collect();
    if items.is_empty() {
        return;
    }
    emit(WorkerEvent::Status(i18n("Sending…")));
    let Some(token) = graph_token(account, emit).await else {
        emit(WorkerEvent::Status(String::new()));
        return;
    };
    let mut sent_any = false;
    for item in items {
        let t = token.clone();
        let raw = item.raw.clone();
        let url = format!("{GRAPH_BASE}/me/sendMail");
        let r = blocking(move || graph_post_mime(&t, &url, &raw)).await;
        match r {
            Ok(_) => {
                sent_any = true;
                cache.delete_outbox(item.id);
            }
            Err(e) => {
                cache.record_outbox_failure(item.id, &e);
                emit(WorkerEvent::error(i18n_f("Still could not send “{subject}”: {e}", &[("subject", &item.subject.to_string()), ("e", &e.to_string())])));
                break;
            }
        }
    }
    emit(WorkerEvent::Status(String::new()));
    if sent_any {
        emit(WorkerEvent::Sent);
    }
    emit_outbox(Some(cache), account_id, emit);
}

/// A Microsoft 365 category color (`preset0`…`preset24`) as the nearest
/// `#rrggbb`; `None` for "none" or anything unknown.
fn graph_preset_color(preset: &str) -> Option<String> {
    let hex = match preset.to_ascii_lowercase().as_str() {
        "preset0" => "#c01c28",  // red
        "preset1" => "#e66100",  // orange
        "preset2" => "#865e3c",  // brown
        "preset3" => "#f5c211",  // yellow
        "preset4" => "#2ec27e",  // green
        "preset5" => "#0e9aa7",  // teal
        "preset6" => "#6b8e23",  // olive
        "preset7" => "#1c71d8",  // blue
        "preset8" => "#813d9c",  // purple
        "preset9" => "#b5127a",  // cranberry
        "preset10" => "#5d7c99", // steel
        "preset11" => "#3f5567", // dark steel
        "preset12" => "#77767b", // gray
        "preset13" => "#5e5c64", // dark gray
        "preset14" => "#241f31", // black
        "preset15" => "#a51d2d", // dark red
        "preset16" => "#c64600", // dark orange
        "preset17" => "#63452c", // dark brown
        "preset18" => "#e5a50a", // dark yellow
        "preset19" => "#26a269", // dark green
        "preset20" => "#0b7c85", // dark teal
        "preset21" => "#55701c", // dark olive
        "preset22" => "#1a5fb4", // dark blue
        "preset23" => "#613583", // dark purple
        "preset24" => "#8f0e5d", // dark cranberry
        _ => return None,
    };
    Some(hex.to_string())
}

/// Auto-empty (#140) for a Microsoft 365 account, by the well-known folder
/// names and `receivedDateTime`.
async fn auto_empty_graph(
    token: &str,
    account_id: u32,
    account: &AccountConfig,
    cache: Option<&Cache>,
    state: &mut GraphState,
    last: &mut Option<std::time::Instant>,
    emit: &impl Fn(WorkerEvent),
) {
    if !auto_empty_due(account, last) {
        return;
    }
    let folders = cache.map(|c| c.load_folders(account_id)).unwrap_or_default();
    let mut purged_any = false;
    for (kind, role, days) in auto_empty_roles(account) {
        let well_known = match kind {
            FolderKind::Junk => "junkemail",
            _ => "deleteditems",
        };
        let path = role_folder_path(account, &folders, kind, role);
        let url = format!(
            "{GRAPH_BASE}/me/mailFolders/{well_known}/messages\
             ?$filter=receivedDateTime%20le%20{}&$select=id&$top=100",
            graph_before_date(days)
        );
        let t = token.to_string();
        let items = blocking(move || graph_paged(&t, &url, GRAPH_FIRST_PAGE)).await;
        let ids: Vec<String> = match items {
            Ok(items) => items.iter().filter_map(|v| v["id"].as_str().map(str::to_string)).collect(),
            Err(e) => {
                tracing::warn!("auto-empty: could not list {well_known} on account {account_id}: {e}");
                continue;
            }
        };
        if ids.is_empty() {
            continue;
        }
        let mut deleted = 0usize;
        for gid in ids {
            let t = token.to_string();
            let url = format!("{GRAPH_BASE}/me/messages/{gid}");
            let ok = blocking(move || graph_delete_req(&t, &url)).await
                .is_ok();
            if ok {
                deleted += 1;
                let uid = hash_uid(&gid);
                state.uids.remove(&uid);
                if let (Some(c), Some(path)) = (cache, path.as_deref()) {
                    c.delete_message(account_id, path, uid);
                }
            }
        }
        tracing::info!(
            "auto-empty: deleted {deleted} message(s) older than {days} days from {well_known} on account {account_id}"
        );
        purged_any |= deleted > 0;
    }
    if purged_any {
        graph_refresh_unread(token, account_id, cache, state, emit).await;
    }
}

/// What one body search found: the Graph ids of the matching messages, and
/// the time before which matches may be missing, when Graph's result limit
/// could not be got past.
pub(super) struct BodySearch {
    pub(super) ids: std::collections::HashSet<String>,
    pub(super) unchecked_before: Option<i64>,
}

/// Every message in a folder whose body holds `needle`. Graph returns at most
/// 1,000 results for one `$search`, newest sent first, and cannot page past
/// them. When a search fills that limit, it is asked again for the messages
/// sent on or before the day of the oldest result, and so on back, which
/// reaches every match unless more than 1,000 were sent on one day; then
/// what lies on and before that day is reported as unchecked.
pub(super) fn graph_body_search(token: &str, base: &str, folder_gid: &str, needle: &str) -> Result<BodySearch, String> {
    let mut ids = std::collections::HashSet::new();
    let mut window: Option<chrono::NaiveDate> = None;
    // Each round moves back by at least a day; a mailbox's history is
    // not longer than this many rounds of 1,000 results.
    for _ in 0..1000 {
        // KQL, as the rule search has always sent it: the term in quotes;
        // a quote inside it would end the term.
        let mut kql = format!("body:{}", needle.replace('"', " "));
        if let Some(day) = window {
            kql = format!("{kql} AND sent<={}", day.format("%Y-%m-%d"));
        }
        let url = format!(
            "{base}/me/mailFolders/{folder_gid}/messages?$search={}&$select=id,sentDateTime&$top=250",
            crate::percent::encode(&format!("\"{kql}\""))
        );
        let items = graph_paged(token, &url, super::server_search::GRAPH_SEARCH_LIMIT)?;
        ids.extend(items.iter().filter_map(|v| v["id"].as_str().map(str::to_string)));
        if items.len() < super::server_search::GRAPH_SEARCH_LIMIT {
            return Ok(BodySearch { ids, unchecked_before: None });
        }
        let oldest = items
            .iter()
            .filter_map(|v| v["sentDateTime"].as_str())
            .filter_map(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&chrono::Utc).date_naive())
            .min();
        match oldest {
            Some(day) if window != Some(day) => window = Some(day),
            // A full day of results, or no dates to go by: the rest cannot
            // be reached. Everything up to the end of that day is in doubt.
            _ => {
                let day = oldest.or(window).unwrap_or_default();
                let end = day.succ_opt().unwrap_or(day).and_hms_opt(0, 0, 0).expect("midnight").and_utc().timestamp();
                return Ok(BodySearch { ids, unchecked_before: Some(end) });
            }
        }
    }
    Ok(BodySearch { ids, unchecked_before: Some(i64::MAX) })
}

/// Microsoft Graph's side of [`emit_body_hits`]: one `$search="body:…"`
/// listing per alternative over the inbox, intersected with the listed
/// messages (Graph ids hash to the uids the app knows).
async fn emit_graph_body_hits(
    token: &str,
    account: &AccountConfig,
    folder_id: u32,
    path: &str,
    messages: &[Message],
    state: &GraphState,
    emit: &impl Fn(WorkerEvent),
) {
    if messages.is_empty() || state.inbox.as_ref().map(|(_, p)| p.as_str()) != Some(path) {
        return;
    }
    let needles = crate::config::filter_body_needles(&account.email);
    if needles.is_empty() {
        return;
    }
    let Some((_, gid)) = state.folders.get(path).cloned() else { return };
    let listed: std::collections::HashSet<u32> = messages.iter().map(|m| m.uid).collect();
    let mut hits: std::collections::HashMap<u32, Vec<String>> = Default::default();
    let mut unchecked = 0usize;
    for needle in needles {
        let (t, base, g, n) = (token.to_string(), state.base.clone(), gid.clone(), needle.clone());
        match blocking(move || graph_body_search(&t, &base, &g, &n)).await {
            Ok(found) => {
                for uid in found.ids.iter().map(|id| hash_uid(id)).filter(|u| listed.contains(u)) {
                    hits.entry(uid).or_default().push(needle.clone());
                }
                if let Some(before) = found.unchecked_before {
                    let n = messages
                        .iter()
                        .filter(|m| m.timestamp < before && !hits.get(&m.uid).is_some_and(|h| h.contains(&needle)))
                        .count();
                    tracing::warn!("filter: Graph body search for {needle:?} could not reach {n} older messages");
                    unchecked += n;
                }
            }
            Err(e) => tracing::warn!("filter: Graph body search for {needle:?} failed: {e}"),
        }
    }
    if unchecked > 0 {
        emit(WorkerEvent::Notice(i18n_f(
            "Message-body filter rules could not be checked against {n} older Inbox messages: Microsoft 365 returns at most 1,000 search results per day searched",
            &[("n", &unchecked.to_string())],
        )));
    }
    tracing::info!("filter: Graph body search over {} messages hit {}", listed.len(), hits.len());
    emit(WorkerEvent::BodyHits { folder_id, hits });
}

#[cfg(test)]
mod tests {
    use super::graph_due;

    use super::*;

    /// A Graph message summary as `/messages` lists it.
    fn graph_msg(id: &str, at: &str) -> serde_json::Value {
        serde_json::json!({
            "@odata.etag": "W/\"CQAAABYAAAD\"",
            "id": id,
            "receivedDateTime": at,
            "subject": format!("Subject {id}"),
            "bodyPreview": "Hello there",
            "isRead": true,
            "hasAttachments": false,
            "internetMessageId": format!("<{id}@example.com>"),
            "conversationId": format!("AAQk-{id}"),
            "importance": "normal",
            "categories": [],
            "flag": { "flagStatus": "notFlagged" },
            "from": { "emailAddress": { "name": "Ada", "address": "ada@example.com" } },
            "toRecipients": [{ "emailAddress": { "name": "Me", "address": "me@example.com" } }],
            "ccRecipients": [],
            "replyTo": [],
        })
    }

    /// The same message as an id-only listing names it.
    fn graph_id(id: &str, at: &str) -> serde_json::Value {
        serde_json::json!({ "@odata.etag": "W/\"CQAAABYAAAD\"", "id": id, "receivedDateTime": at })
    }

    fn day(d: u32) -> String {
        format!("2026-09-{d:02}T09:00:00Z")
    }

    /// A cached row for `id`, received at `ts`.
    fn row(id: &str, ts: i64, folder_id: u32) -> Message {
        let v = graph_msg(id, "2026-09-01T09:00:00Z");
        let (mut m, _) = graph_message(&v, 1, folder_id).expect("summary");
        m.timestamp = ts;
        m
    }

    fn ts(at: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(at).unwrap().timestamp()
    }

    /// One answer of the loopback server: the first unused one whose `key`
    /// is part of the request's path and query.
    struct Answer {
        key: String,
        status: u16,
        headers: &'static str,
        body: String,
    }

    fn answer(key: &str, status: u16, body: serde_json::Value) -> Answer {
        Answer { key: key.to_string(), status, headers: "", body: body.to_string() }
    }

    /// An HTTP server on loopback for `answers.len()` requests, as in the
    /// JMAP tests; hands back the request targets it was sent. Each body is
    /// formatted with `{origin}` replaced, for the next-page links.
    fn serve(answers: Vec<Answer>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let origin = format!("http://{}", listener.local_addr().expect("addr"));
        let o = origin.clone();
        let handle = std::thread::spawn(move || {
            let mut answers: Vec<Option<Answer>> = answers.into_iter().map(Some).collect();
            let mut seen = Vec::new();
            for _ in 0..answers.len() {
                let Ok((stream, _)) = listener.accept() else { break };
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                reader.read_line(&mut line).expect("request line");
                let target = line.split_whitespace().nth(1).unwrap_or("").to_string();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                }
                let hit = answers
                    .iter_mut()
                    .find(|a| a.as_ref().is_some_and(|a| target.contains(&a.key)))
                    .and_then(Option::take);
                let (status, headers, body) = match hit {
                    Some(a) => (a.status, a.headers, a.body.replace("{origin}", &o)),
                    None => (404, "", String::new()),
                };
                let mut out = stream;
                let _ = write!(
                    out,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                seen.push(target);
            }
            seen
        });
        (origin, handle)
    }

    fn state_for(origin: &str, path: &str, folder_id: u32, gid: &str) -> GraphState {
        let mut state = GraphState::new(&super::super::sample_account());
        state.base = origin.to_string();
        state.folders.insert(path.to_string(), (folder_id, gid.to_string()));
        state
    }

    /// #429: missing mail comes in runs, newest first, none longer than a
    /// chunk; a cached row the server has not got is gone, unless it is
    /// newer than the listing.
    #[test]
    fn the_index_plan_fetches_runs_and_drops_what_went() {
        let server: Vec<(String, String)> =
            (1..=8).map(|i| (format!("M{i}"), day(9 - i))).collect();
        let since = ts("2026-09-10T00:00:00Z");
        let cached: std::collections::HashMap<u32, i64> = [
            (hash_uid("M1"), ts(&day(8))),
            (hash_uid("M2"), ts(&day(7))),
            (hash_uid("M5"), ts(&day(4))),
            (hash_uid("GONE"), ts("2026-01-01T00:00:00Z")),
            (hash_uid("NEW"), since + 60),
        ]
        .into_iter()
        .collect();
        let (gone, runs) = graph_index_plan(&server, &cached, since);
        assert_eq!(gone, vec![hash_uid("GONE")]);
        assert_eq!(
            runs,
            vec![
                GraphRun { ids: vec!["M3".into(), "M4".into()], newest: day(6), oldest: day(5) },
                GraphRun { ids: vec!["M6".into(), "M7".into(), "M8".into()], newest: day(3), oldest: day(1) },
            ]
        );

        // A long run is cut into chunks.
        let many: Vec<(String, String)> =
            (0..GRAPH_BACKFILL_CHUNK + 5).map(|i| (format!("X{i}"), day(1))).collect();
        let (_, runs) = graph_index_plan(&many, &Default::default(), since);
        assert_eq!(runs.iter().map(|r| r.ids.len()).collect::<Vec<_>>(), vec![GRAPH_BACKFILL_CHUNK, 5]);
    }

    /// #429: a listing of the newest page speaks only for the mail it
    /// covers; a complete one for the whole folder.
    #[test]
    fn a_window_listing_keeps_older_rows() {
        let listed = vec![row("A", 300, 1), row("B", 200, 1)];
        let cached: std::collections::HashMap<u32, i64> =
            [(hash_uid("A"), 300), (hash_uid("B"), 200), (hash_uid("DEL"), 250), (hash_uid("OLD"), 100)]
                .into_iter()
                .collect();
        assert_eq!(window_gone(&cached, &listed, false), vec![hash_uid("DEL")]);
        let mut all = vec![hash_uid("DEL"), hash_uid("OLD")];
        all.sort_unstable();
        assert_eq!(window_gone(&cached, &listed, true), all);
        assert!(window_gone(&cached, &[], false).is_empty());
    }

    /// #429: a throttled request waits as asked and goes again.
    fn folder(id: &str, name: &str, children: usize) -> serde_json::Value {
        serde_json::json!({ "id": id, "displayName": name, "childFolderCount": children,
                            "unreadItemCount": 0, "totalItemCount": 0 })
    }

    /// The answers for the six role lookups the listing starts with.
    fn role_answers() -> Vec<Answer> {
        ["inbox", "sentitems", "drafts", "deleteditems", "junkemail", "archive"]
            .iter()
            .map(|wk| {
                if *wk == "inbox" {
                    answer(&format!("mailFolders/{wk}?"), 200, serde_json::json!({ "id": "R0" }))
                } else {
                    answer(&format!("mailFolders/{wk}?"), 404, serde_json::json!({}))
                }
            })
            .collect()
    }

    /// More top-level folders than one page and than the old 200 cap, a
    /// chain deeper than the old four levels, and one folder with more
    /// children than the old cap: every one of them is listed.
    #[test]
    fn a_large_deep_folder_tree_is_listed_whole() {
        let mut answers = role_answers();
        // 201 roots over three pages: R0 (the inbox) .. R200.
        let roots: Vec<serde_json::Value> = (0..=200)
            .map(|i| folder(&format!("R{i}"), &format!("Root {i:03}"), usize::from(i == 1 || i == 2)))
            .collect();
        answers.push(answer("/me/mailFolders?$top=100", 200, serde_json::json!({
            "value": roots[..100], "@odata.nextLink": "{origin}/me/mailFolders?page=2" })));
        answers.push(answer("/me/mailFolders?page=2", 200, serde_json::json!({
            "value": roots[100..200], "@odata.nextLink": "{origin}/me/mailFolders?page=3" })));
        answers.push(answer("/me/mailFolders?page=3", 200, serde_json::json!({ "value": roots[200..] })));
        // R1 holds a chain six deep.
        for d in 0..6 {
            let parent = if d == 0 { "R1".to_string() } else { format!("D{}", d - 1) };
            let children = usize::from(d < 5);
            answers.push(answer(&format!("mailFolders/{parent}/childFolders"), 200,
                serde_json::json!({ "value": [folder(&format!("D{d}"), &format!("Level {}", d + 1), children)] })));
        }
        // R2 holds 201 children over two pages.
        let kids: Vec<serde_json::Value> = (0..201).map(|i| folder(&format!("K{i}"), &format!("Kid {i:03}"), 0)).collect();
        answers.push(answer("mailFolders/R2/childFolders", 200, serde_json::json!({
            "value": kids[..200], "@odata.nextLink": "{origin}/me/mailFolders/R2/kids?page=2" })));
        answers.push(answer("mailFolders/R2/kids?page=2", 200, serde_json::json!({ "value": kids[200..] })));
        let (origin, server) = serve(answers);

        let folders = graph_list_folders_at("t", &origin, 1, &Default::default()).expect("listed");
        drop(server);
        assert_eq!(folders.len(), 201 + 6 + 201);
        let paths: std::collections::HashSet<&str> = folders.iter().map(|f| f.folder.path.as_str()).collect();
        assert!(paths.contains("Root 001/Level 1/Level 2/Level 3/Level 4/Level 5/Level 6"));
        assert!(paths.contains("Root 002/Kid 200"));
        assert!(paths.contains("Root 200"));
        assert_eq!(folders.iter().find(|f| f.graph_id == "R0").unwrap().folder.kind, FolderKind::Inbox);
    }

    /// A child listing that fails fails the listing: the sidebar keeps the
    /// folders it had rather than losing the ones that could not be read.
    #[test]
    fn a_folder_level_that_cannot_be_read_fails_the_listing() {
        let mut answers = role_answers();
        answers.push(answer("/me/mailFolders?$top=100", 200,
            serde_json::json!({ "value": [folder("R0", "Inbox", 0), folder("R1", "Projects", 2)] })));
        answers.push(answer("mailFolders/R1/childFolders", 500, serde_json::json!({ "error": { "code": "x" } })));
        let (origin, _server) = serve(answers);
        assert!(graph_list_folders_at("t", &origin, 1, &Default::default()).is_err());
    }

    /// A server that names an ancestor among a folder's children does not
    /// send the listing round in circles.
    #[test]
    fn a_folder_loop_is_walked_once() {
        let mut answers = role_answers();
        answers.push(answer("/me/mailFolders?$top=100", 200, serde_json::json!({ "value": [folder("R1", "A", 1)] })));
        answers.push(answer("mailFolders/R1/childFolders", 200, serde_json::json!({ "value": [folder("C1", "B", 1)] })));
        answers.push(answer("mailFolders/C1/childFolders", 200, serde_json::json!({ "value": [folder("R1", "A", 1)] })));
        let (origin, _server) = serve(answers);
        let folders = graph_list_folders_at("t", &origin, 1, &Default::default()).expect("listed");
        assert_eq!(folders.iter().map(|f| f.folder.path.as_str()).collect::<Vec<_>>(), vec!["A", "A/B"]);
    }

    fn sent(id: usize, day: u32) -> serde_json::Value {
        serde_json::json!({ "id": format!("M{id}"), "sentDateTime": format!("2026-10-{day:02}T12:00:00Z") })
    }

    /// Under, at and over the old cap of 250, every match is found; past
    /// Graph's own limit of 1,000 the search moves back a day at a time.
    #[test]
    fn a_body_search_finds_matches_past_the_old_cap() {
        for n in [249usize, 250, 251] {
            let page: Vec<_> = (0..n).map(|i| sent(i, 14)).collect();
            let mut answers = vec![answer("/messages?$search=", 200, serde_json::json!({
                "value": page[..n.min(250)], "@odata.nextLink": "{origin}/next?p=2" }))];
            answers.push(answer("/next?p=2", 200, serde_json::json!({ "value": page[n.min(250)..] })));
            let (origin, _server) = serve(answers);
            let found = graph_body_search("t", &origin, "F", "invoice").unwrap();
            assert_eq!(found.ids.len(), n, "{n} matches");
            assert_eq!(found.unchecked_before, None);
        }
    }

    #[test]
    fn a_body_search_past_graphs_limit_goes_back_by_date() {
        // First search: 1,000 results, the oldest sent on the 10th.
        let first: Vec<_> = (0..1000).map(|i| sent(i, if i < 990 { 14 } else { 10 })).collect();
        // Second, for mail sent on or before the 10th: the rest.
        let second: Vec<_> = (990..1300).map(|i| sent(i, if i < 1000 { 10 } else { 3 })).collect();
        let answers = vec![
            answer("sent%3C%3D2026-10-10", 200, serde_json::json!({ "value": second })),
            answer("/messages?$search=", 200, serde_json::json!({ "value": first })),
        ];
        let (origin, server) = serve(answers);
        let found = graph_body_search("t", &origin, "F", "invoice").unwrap();
        assert_eq!(found.ids.len(), 1300);
        assert_eq!(found.unchecked_before, None);
        let seen = server.join().unwrap();
        assert!(seen[1].contains("sent%3C%3D2026-10-10"), "{seen:?}");
    }

    #[test]
    fn a_full_day_of_results_is_reported_not_skipped() {
        let day: Vec<_> = (0..1000).map(|i| sent(i, 10)).collect();
        let answers = vec![
            answer("sent%3C%3D2026-10-10", 200, serde_json::json!({ "value": day.clone() })),
            answer("/messages?$search=", 200, serde_json::json!({ "value": day })),
        ];
        let (origin, _server) = serve(answers);
        let found = graph_body_search("t", &origin, "F", "invoice").unwrap();
        assert_eq!(found.ids.len(), 1000);
        let end_of_day = chrono::NaiveDate::from_ymd_opt(2026, 10, 11).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        assert_eq!(found.unchecked_before, Some(end_of_day));
    }

    /// A raw message is read whole or refused, never cut at the limit and
    /// passed on as if complete.
    #[test]
    fn a_raw_message_is_read_whole_or_refused() {
        let limit = 1000u64;
        for (len, ok) in [(999usize, true), (1000, true), (1001, false)] {
            let data = vec![b'x'; len];
            let undeclared = read_capped(&data[..], None, limit);
            assert_eq!(undeclared.is_ok(), ok, "{len} bytes, no length given");
            if ok {
                assert_eq!(undeclared.unwrap().len(), len);
            }
            assert_eq!(read_capped(&data[..], Some(len as u64), limit).is_ok(), ok, "{len} bytes, length given");
        }
        // Too large by the header alone: nothing is read.
        assert!(read_capped(&b""[..], Some(5000), limit).is_err());
        // A body shorter than it said is a broken download.
        assert!(read_capped(&b"abc"[..], Some(10), limit).is_err());
    }

    #[test]
    fn a_throttled_request_is_retried() {
        let (origin, server) = serve(vec![
            Answer {
                key: "/messages".into(),
                status: 429,
                headers: "Retry-After: 0\r\n",
                body: serde_json::json!({ "error": { "code": "ApplicationThrottled", "message": "slow down" } })
                    .to_string(),
            },
            answer("/messages", 200, serde_json::json!({ "value": [graph_id("M1", &day(1))] })),
        ]);
        let (ids, next) = graph_id_page("t", &graph_ids_url(&origin, "F")).expect("retried");
        assert_eq!(ids, vec![("M1".to_string(), day(1))]);
        assert_eq!(next, None);
        assert_eq!(server.join().expect("server").len(), 2);
    }

    /// #429: the whole background pass over a folder, against recorded
    /// answers: the ids in two pages, a short run fetched message by
    /// message (one gone meanwhile), a long one by its dates; the cache
    /// keeps what it had, loses what the server lost, and the list hears
    /// of each step and of the end.
    #[tokio::test]
    async fn the_background_index_fills_and_prunes_a_folder() {
        let page1: Vec<serde_json::Value> = (1..=4).map(|i| graph_id(&format!("M{i}"), &day(9 - i))).collect();
        let page2: Vec<serde_json::Value> = (5..=8).map(|i| graph_id(&format!("M{i}"), &day(9 - i))).collect();
        let ranged: Vec<serde_json::Value> = (5..=8).map(|i| graph_msg(&format!("M{i}"), &day(9 - i))).collect();
        let (origin, server) = serve(vec![
            answer(
                "$top=1000&$orderby=receivedDateTime%20desc&$select=id,receivedDateTime",
                200,
                serde_json::json!({
                    "value": page1,
                    "@odata.nextLink": "{origin}/me/mailFolders/F-archive/messages?$select=id,receivedDateTime&$skip=4",
                }),
            ),
            answer("$skip=4", 200, serde_json::json!({ "value": page2 })),
            answer("/me/messages/M3?", 200, graph_msg("M3", &day(6))),
            answer(
                "/me/messages/M4?",
                404,
                serde_json::json!({ "error": { "code": "ErrorItemNotFound", "message": "Not found." } }),
            ),
            answer("$filter=receivedDateTime%20ge%20", 200, serde_json::json!({ "value": ranged })),
        ]);
        let cache = Cache::in_memory().expect("cache");
        let now = crate::datefmt::now();
        cache.upsert_messages(
            1,
            "Archive",
            &[
                row("M1", ts(&day(8)), 4),
                row("M2", ts(&day(7)), 4),
                row("M5", ts(&day(4)), 4),
                row("GONE", ts("2026-01-01T00:00:00Z"), 4),
                row("NEW", now + 60, 4),
            ],
        );
        let mut state = state_for(&origin, "Archive", 4, "F-archive");
        state.queue_backfill("Archive", true, false);
        let events = std::cell::RefCell::new(Vec::new());
        let emit = |e: WorkerEvent| events.borrow_mut().push(e);
        for _ in 0..4 {
            assert!(graph_backfill_with("t", 1, Some(&cache), &mut state, &emit).await);
        }
        assert!(state.backfill.is_empty(), "the folder's index is done");

        let mut have: Vec<u32> = cache.cached_uids(1, "Archive").into_iter().collect();
        have.sort_unstable();
        let mut want: Vec<u32> = ["M1", "M2", "M3", "M5", "M6", "M7", "M8", "NEW"].iter().map(|id| hash_uid(id)).collect();
        want.sort_unstable();
        assert_eq!(have, want);
        assert_eq!(state.uids.get(&hash_uid("M7")).map(String::as_str), Some("M7"));

        let events = events.into_inner();
        let kinds: Vec<&str> = events
            .iter()
            .map(|e| match e {
                WorkerEvent::Messages { folder_id: 4, .. } => "messages",
                WorkerEvent::MessagesAppend { folder_id: 4, .. } => "append",
                WorkerEvent::BackfillDone { folder_id: 4 } => "done",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["messages", "append", "append", "done"]);

        let seen = server.join().expect("server");
        assert_eq!(seen.len(), 5);
        assert!(
            seen[4].contains("ge%202026-09-01T09%3A00%3A00Z%20and%20receivedDateTime%20le%202026-09-03T09%3A00%3A00Z"),
            "{}",
            seen[4]
        );
    }

    /// A folder the server keeps refusing moves to the back of the queue
    /// after three tries, so the others are indexed meanwhile.
    #[test]
    fn a_failing_folder_does_not_hold_up_the_others() {
        let mut state = state_for("http://unused", "Archive", 4, "F-archive");
        state.folders.insert("Sent".into(), (2, "F-sent".into()));
        state.queue_backfill("Archive", false, false);
        state.queue_backfill("Sent", false, false);
        let events = std::cell::RefCell::new(Vec::new());
        let emit = |e: WorkerEvent| events.borrow_mut().push(e);
        for _ in 0..2 {
            note_backfill_failure(&mut state, &emit);
            assert_eq!(state.backfill.front().map(|j| j.path.as_str()), Some("Archive"));
        }
        note_backfill_failure(&mut state, &emit);
        assert_eq!(state.backfill.front().map(|j| j.path.as_str()), Some("Sent"), "the next folder's turn");
        assert_eq!(state.backfill.back().map(|j| j.path.as_str()), Some("Archive"), "still queued");
        assert!(events.borrow().iter().any(|e| matches!(e, WorkerEvent::BackfillDone { folder_id: 4 })));
    }

    /// A date-range fetch that leaves out a message it was asked for (the
    /// newest of a run whose time Exchange keeps to a fraction of a second,
    /// while the listing prints whole seconds, falls outside `le`) must not
    /// let the folder's index be marked done with that message missing.
    #[tokio::test]
    async fn a_range_that_misses_a_message_does_not_complete_the_index() {
        let listed: Vec<serde_json::Value> = (1..=5).map(|i| graph_id(&format!("M{i}"), &day(9 - i))).collect();
        let ranged: Vec<serde_json::Value> = (2..=5).map(|i| graph_msg(&format!("M{i}"), &day(9 - i))).collect();
        let (origin, _server) = serve(vec![
            answer("$select=id,receivedDateTime", 200, serde_json::json!({ "value": listed })),
            answer("$filter=receivedDateTime%20ge%20", 200, serde_json::json!({ "value": ranged })),
            answer("/me/messages/M1?", 200, graph_msg("M1", &day(8))),
        ]);
        let cache = Cache::in_memory().expect("cache");
        let mut state = state_for(&origin, "Archive", 4, "F-archive");
        state.queue_backfill("Archive", true, false);
        let events = std::cell::RefCell::new(Vec::new());
        let emit = |e: WorkerEvent| events.borrow_mut().push(e);
        for _ in 0..2 {
            assert!(graph_backfill_with("t", 1, Some(&cache), &mut state, &emit).await);
        }
        assert!(state.backfill.is_empty());
        assert!(events.borrow().iter().any(|e| matches!(e, WorkerEvent::BackfillDone { folder_id: 4 })));
        let have = cache.cached_uids(1, "Archive");
        for i in 1..=5 {
            assert!(have.contains(&hash_uid(&format!("M{i}"))), "M{i} is indexed before the folder is called done");
        }
    }

    /// #429: re-listing the newest page leaves the older cached mail be,
    /// and says the folder holds more than the page.
    #[tokio::test]
    async fn a_folder_load_merges_over_the_cache() {
        let page = |from: usize| -> Vec<serde_json::Value> {
            (from..from + 100)
                .map(|i| graph_msg(&format!("N{i}"), &format!("2026-10-01T{:02}:{:02}:00Z", 23 - i / 60, 59 - i % 60)))
                .collect()
        };
        let (origin, server) = serve(vec![
            answer(
                "$top=100&$orderby",
                200,
                serde_json::json!({ "value": page(0), "@odata.nextLink": "{origin}/me/mailFolders/F-inbox/messages?$skip=100" }),
            ),
            answer(
                "$skip=100",
                200,
                serde_json::json!({ "value": page(100), "@odata.nextLink": "{origin}/me/mailFolders/F-inbox/messages?$skip=200" }),
            ),
            answer(
                "$skip=200",
                200,
                serde_json::json!({ "value": page(200), "@odata.nextLink": "{origin}/me/mailFolders/F-inbox/messages?$skip=300" }),
            ),
        ]);
        let cache = Cache::in_memory().expect("cache");
        // An old message from the background index, and a new one deleted
        // on the server since it was cached.
        cache.upsert_messages(
            1,
            "Inbox",
            &[row("OLD", ts("2025-01-01T00:00:00Z"), 1), row("DELETED", ts("2026-10-01T23:59:30Z"), 1)],
        );
        let mut state = state_for(&origin, "Inbox", 1, "F-inbox");
        let (messages, complete) =
            graph_sync_folder("t", 1, 1, "Inbox", Some(&cache), &mut state).await.expect("listed");
        assert!(!complete, "the server has more than the first page");
        assert_eq!(messages.len(), GRAPH_FIRST_PAGE + 1);
        assert!(messages.iter().any(|m| m.uid == hash_uid("OLD")));
        assert!(!messages.iter().any(|m| m.uid == hash_uid("DELETED")));
        assert_eq!(server.join().expect("server").len(), 3);

        // Not complete and never indexed: queued, first, for the folder on
        // screen; the list keeps its "Loading more…".
        assert!(!state.index_after_load("Inbox", complete, true));
        assert_eq!(state.backfill.front().map(|j| j.path.as_str()), Some("Inbox"));
        // A folder that fitted in one page is complete as listed.
        assert!(state.index_after_load("Inbox", true, true));
        assert!(state.backfill.is_empty());
    }

    /// #429: an undone move finds an old message by its Message-ID.
    #[test]
    fn a_message_is_found_by_its_message_id() {
        let (origin, server) = serve(vec![answer(
            "$filter=internetMessageId",
            200,
            serde_json::json!({ "value": [graph_msg("M9", &day(1))] }),
        )]);
        let hits = graph_find_by_msgid("t", &origin, "F-trash", "M9@Example.com").expect("found");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1, "M9");
        let seen = server.join().expect("server");
        assert!(seen[0].contains("$filter=internetMessageId%20eq%20%27%3CM9%40Example.com%3E%27"), "{}", seen[0]);
    }

    /// #334: a flagged message's due date, in UTC; none once the flag is
    /// complete or cleared, or without a date.
    #[test]
    fn a_due_date_is_read_only_from_an_open_flag() {
        let flag = |status: &str| {
            serde_json::json!({
                "flagStatus": status,
                "dueDateTime": { "dateTime": "2026-10-05T04:00:00.0000000", "timeZone": "UTC" },
            })
        };
        assert_eq!(graph_due(&flag("flagged")), 1_791_172_800);
        assert_eq!(graph_due(&flag("complete")), 0);
        assert_eq!(graph_due(&flag("notFlagged")), 0);
        assert_eq!(graph_due(&serde_json::json!({ "flagStatus": "flagged" })), 0);
    }
}
