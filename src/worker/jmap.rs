//! The JMAP path (issue #245).
//!
//! JMAP (RFC 8620/8621) is JSON over HTTPS: one `POST` carries a batch of
//! method calls, and a message's raw RFC 5322 bytes come from a blob
//! download URL, so they feed the exact same parsing pipeline as IMAP and
//! Graph. Compared with Graph this fits Hylki's model with less bending:
//! mailboxes carry their role (inbox, sent, drafts…), a message keeps its id
//! when it moves, keywords are the same `$seen` / `$flagged` / user keywords
//! IMAP has (so tags need no translation), and Message-ID, In-Reply-To and
//! References come with every listing, so threading uses the real headers.
//!
//! Message uids are the same stable string-hash the POP3 and Graph paths
//! use (JMAP ids are opaque strings); the uid → id map is rebuilt from every
//! folder listing. Sending goes through `EmailSubmission`, which the server
//! files in Sent itself, so a JMAP account needs no SMTP settings. New mail
//! arrives over the server's EventSource push channel when the account's
//! push preference allows, with a poll as the fallback.
//!
//! Proven against a local Stalwart container; Fastmail speaks the same
//! standard but has not been tried by hand.

use super::*;

/// Summaries listed per folder (matches the Graph path's indexing appetite).
const JMAP_INDEX_CAP: usize = 300;

const CAP_CORE: &str = "urn:ietf:params:jmap:core";
const CAP_MAIL: &str = "urn:ietf:params:jmap:mail";
const CAP_SUBMISSION: &str = "urn:ietf:params:jmap:submission";

/// The summary properties one listing asks `Email/get` for.
const EMAIL_PROPS: &[&str] = &[
    "id", "blobId", "threadId", "mailboxIds", "keywords", "hasAttachment", "receivedAt",
    "subject", "from", "replyTo", "to", "cc", "messageId", "inReplyTo", "references", "preview",
];

/// Keywords the server holds that are flags, not tags: what IMAP's system
/// flags are to its keywords.
const SYSTEM_KEYWORDS: &[&str] = &["$seen", "$flagged", "$draft", "$answered", "$forwarded", "$recent", "$deleted"];

/// What the session resource told us, plus the credentials to keep using.
#[derive(Clone, Debug)]
pub(super) struct JmapSession {
    api_url: String,
    download_url: String,
    upload_url: String,
    event_source_url: Option<String>,
    /// The JMAP account id (the primary mail account of the sign-in).
    account: String,
    /// The `Authorization` header value.
    auth: String,
}

/// One mailbox, flattened out of the tree.
struct JmapFolder {
    mailbox_id: String,
    folder: Folder,
}

/// Per-account state the JMAP loop threads through its handlers.
struct JmapState {
    /// The session, once the server has been reached.
    session: Option<JmapSession>,
    /// Hylki folder path → (folder id, mailbox id), from the last listing.
    folders: HashMap<String, (u32, String)>,
    /// Message uid (hashed JMAP id) → (JMAP id, blob id).
    uids: HashMap<u32, (String, String)>,
    /// The Drafts folder's (folder id, path).
    drafts: Option<(u32, String)>,
    /// The Inbox's (folder id, path), for the new-mail poll.
    inbox: Option<(u32, String)>,
    /// The Sent folder's (folder id, path).
    sent: Option<(u32, String)>,
    /// The account's manual Special Folders assignments (#82).
    roles: BTreeMap<String, String>,
    /// The account's hidden folders (#239).
    hidden: Vec<String>,
}

use std::collections::{BTreeMap, HashMap, HashSet};

impl JmapState {
    fn chip_count(&self, folder_id: u32, messages: &[Message]) -> u32 {
        let kind = match &self.drafts {
            Some((id, _)) if *id == folder_id => FolderKind::Drafts,
            _ => FolderKind::Custom,
        };
        chip_count_of(kind, messages)
    }

    fn adopt_folders(&mut self, list: &[JmapFolder]) {
        self.folders = list
            .iter()
            .map(|f| (f.folder.path.clone(), (f.folder.id, f.mailbox_id.clone())))
            .collect();
        let find = |kind: FolderKind| {
            list.iter().find(|f| f.folder.kind == kind).map(|f| (f.folder.id, f.folder.path.clone()))
        };
        self.drafts = find(FolderKind::Drafts);
        self.inbox = find(FolderKind::Inbox);
        self.sent = find(FolderKind::Sent);
    }

    /// The path of a mailbox id, from the last listing.
    fn path_of(&self, mailbox_id: &str) -> Option<(u32, String)> {
        self.folders
            .iter()
            .find(|(_, (_, id))| id == mailbox_id)
            .map(|(p, (fid, _))| (*fid, p.clone()))
    }
}

// ---------------------------------------------------------------------------
// Wire
// ---------------------------------------------------------------------------

/// The server's base URL from the account's incoming-server field: a bare
/// host gets `https://` (and the port when it is not 443); a URL with its
/// scheme is used as typed, so a server on plain HTTP on a private network
/// can be reached.
pub(super) fn jmap_base_url(account: &AccountConfig) -> String {
    let host = account.imap_host.trim().trim_end_matches('/');
    if host.starts_with("http://") || host.starts_with("https://") {
        return host.to_string();
    }
    match account.imap_port {
        0 | 443 => format!("https://{host}"),
        port => format!("https://{host}:{port}"),
    }
}

/// `scheme://host[:port]` of a URL (the whole string when it has no path).
fn origin_of(url: &str) -> &str {
    match url.find("://") {
        Some(i) => match url[i + 3..].find('/') {
            Some(j) => &url[..i + 3 + j],
            None => url,
        },
        None => url,
    }
}

fn jmap_auth(account: &AccountConfig) -> String {
    use base64::Engine;
    let user = if account.username.is_empty() { &account.email } else { &account.username };
    let b64 = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{}", account.password));
    format!("Basic {b64}")
}

/// Read a ureq error into something a user can act on.
fn jmap_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(401, _) => {
            "the server refused the sign-in (401): check the username and password".to_string()
        }
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            // JMAP problem details (RFC 7807) carry a `detail`; surface that.
            let msg = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| {
                    v["detail"].as_str().or(v["title"].as_str()).map(str::to_string)
                })
                .unwrap_or_else(|| body.chars().take(200).collect());
            format!("the server returned {code}: {msg}")
        }
        other => other.to_string(),
    }
}

/// The JMAP conversation for the console: method names in, verdict out (the
/// credentials travel in a header and are never logged).
fn jmap_wire(what: &str) {
    tracing::debug!(target: "hylki::jmap", "> {what}");
}

fn jmap_wired<T>(what: &str, r: &Result<T, String>) {
    match r {
        Ok(_) => tracing::debug!(target: "hylki::jmap", "< OK ({what})"),
        Err(e) => tracing::warn!(target: "hylki::jmap", "< {e} ({what})"),
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .build()
}

/// Fetch the session resource: where the API lives and which account is
/// the mailbox.
pub(super) fn jmap_connect(account: &AccountConfig) -> Result<JmapSession, String> {
    let base = jmap_base_url(account);
    let auth = jmap_auth(account);
    // The well-known path answers with a redirect to the session resource
    // (a 307 on Stalwart). Followed by hand, credentials included: ureq
    // drops the Authorization header when it follows one itself, and the
    // session then comes back for nobody.
    let no_follow = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .redirects(0)
        .build();
    let mut url = format!("{base}/.well-known/jmap");
    let mut hops = 0;
    let v: serde_json::Value = loop {
        jmap_wire("GET session");
        let resp = no_follow
            .get(&url)
            .set("Authorization", &auth)
            .set("Accept", "application/json")
            .call()
            .map_err(jmap_err);
        jmap_wired("GET session", &resp);
        let resp = resp?;
        if (300..400).contains(&resp.status()) {
            let Some(next) = resp.header("Location").map(str::to_string) else {
                return Err(format!("the server redirected ({}) without saying where", resp.status()));
            };
            hops += 1;
            if hops > 5 {
                return Err("the server keeps redirecting the session request".into());
            }
            url = if next.starts_with("http://") || next.starts_with("https://") {
                next
            } else {
                format!("{base}/{}", next.trim_start_matches('/'))
            };
            continue;
        }
        break resp.into_json().map_err(|e| e.to_string())?;
    };
    // A server names itself in the session: absolute URLs on the host it
    // was set up with. An account whose server is given as a URL, scheme
    // and all, is pointed at that origin on purpose (a server on a private
    // network, or behind a tunnel, reached other than by its public name),
    // so the typed origin stands in for the one the server advertises for
    // itself. Only that one: a URL on another host (Fastmail serves blobs
    // from a separate one) is left alone.
    let typed_origin = account.imap_host.trim().starts_with("http").then(|| origin_of(&base).to_string());
    let advertised = v["apiUrl"].as_str().map(|u| origin_of(u).to_string()).unwrap_or_default();
    let absolute = |u: &str| -> String {
        if !(u.starts_with("http://") || u.starts_with("https://")) {
            return format!("{base}/{}", u.trim_start_matches('/'));
        }
        match &typed_origin {
            Some(mine) if !advertised.is_empty() && origin_of(u) == advertised => {
                format!("{mine}{}", &u[advertised.len()..])
            }
            _ => u.to_string(),
        }
    };
    let api_url = v["apiUrl"].as_str().map(absolute).ok_or("the session resource has no apiUrl")?;
    let download_url =
        v["downloadUrl"].as_str().map(absolute).ok_or("the session resource has no downloadUrl")?;
    let upload_url =
        v["uploadUrl"].as_str().map(absolute).ok_or("the session resource has no uploadUrl")?;
    let event_source_url = v["eventSourceUrl"].as_str().map(absolute);
    // The mail account: the primary one for mail, else the first personal
    // account that has the mail capability.
    let account_id = v["primaryAccounts"][CAP_MAIL]
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            v["accounts"].as_object().and_then(|accts| {
                accts
                    .iter()
                    .find(|(_, a)| a["accountCapabilities"][CAP_MAIL].is_object())
                    .map(|(id, _)| id.clone())
            })
        })
        .ok_or("the sign-in has no mail account on this server")?;
    if !v["capabilities"][CAP_MAIL].is_object() {
        return Err("the server does not offer JMAP for mail".into());
    }
    Ok(JmapSession { api_url, download_url, upload_url, event_source_url, account: account_id, auth })
}

/// One API request: the method calls, in order, with their responses back
/// in the same order. A method-level error comes back as an `Err` naming
/// the call, since nothing after it would make sense.
fn jmap_call(
    s: &JmapSession,
    using: &[&str],
    calls: Vec<serde_json::Value>,
) -> Result<Vec<serde_json::Value>, String> {
    let names: Vec<String> = calls
        .iter()
        .filter_map(|c| c[0].as_str().map(str::to_string))
        .collect();
    let what = names.join(" + ");
    jmap_wire(&what);
    let body = serde_json::json!({ "using": using, "methodCalls": calls });
    let r: Result<serde_json::Value, String> = agent()
        .post(&s.api_url)
        .set("Authorization", &s.auth)
        .set("Accept", "application/json")
        .send_json(body)
        .map_err(jmap_err)
        .and_then(|resp| resp.into_json().map_err(|e| e.to_string()));
    jmap_wired(&what, &r);
    let v = r?;
    let responses = v["methodResponses"]
        .as_array()
        .cloned()
        .ok_or_else(|| "the server's reply has no methodResponses".to_string())?;
    for resp in &responses {
        if resp[0].as_str() == Some("error") {
            let kind = resp[1]["type"].as_str().unwrap_or("unknown error");
            let desc = resp[1]["description"].as_str().unwrap_or("");
            let tag = resp[2].as_str().unwrap_or("");
            let call = names.get(tag.parse::<usize>().unwrap_or(usize::MAX)).cloned().unwrap_or_default();
            return Err(if desc.is_empty() {
                format!("{call}: {kind}")
            } else {
                format!("{call}: {kind} ({desc})")
            });
        }
    }
    Ok(responses)
}

/// A method call tagged by its position, so an error can be traced back.
fn call(idx: usize, name: &str, args: serde_json::Value) -> serde_json::Value {
    serde_json::json!([name, args, idx.to_string()])
}

/// The arguments of the response at `idx`.
fn args(responses: &[serde_json::Value], idx: usize) -> serde_json::Value {
    responses.get(idx).map(|r| r[1].clone()).unwrap_or(serde_json::Value::Null)
}

/// Fill a URL template's `{name}` slots.
fn fill_template(template: &str, slots: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (k, v) in slots {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// The raw bytes of a blob, capped well above any sane message (64 MB).
fn jmap_download(s: &JmapSession, blob_id: &str) -> Result<Vec<u8>, String> {
    let url = fill_template(
        &s.download_url,
        &[("accountId", &s.account), ("blobId", blob_id), ("type", "message%2Frfc822"), ("name", "message.eml")],
    );
    jmap_wire("GET blob");
    let resp = agent().get(&url).set("Authorization", &s.auth).call().map_err(jmap_err);
    jmap_wired("GET blob", &resp);
    let resp = resp?;
    let mut out = Vec::new();
    use std::io::Read;
    resp.into_reader()
        .take(64 * 1024 * 1024)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// Upload raw bytes; the blob id to import or attach.
fn jmap_upload(s: &JmapSession, raw: &[u8], content_type: &str) -> Result<String, String> {
    let url = fill_template(&s.upload_url, &[("accountId", &s.account)]);
    jmap_wire("POST upload");
    let r: Result<serde_json::Value, String> = agent()
        .post(&url)
        .set("Authorization", &s.auth)
        .set("Content-Type", content_type)
        .send_bytes(raw)
        .map_err(jmap_err)
        .and_then(|resp| resp.into_json().map_err(|e| e.to_string()));
    jmap_wired("POST upload", &r);
    r?["blobId"].as_str().map(str::to_string).ok_or_else(|| "the upload reply has no blobId".into())
}

// ---------------------------------------------------------------------------
// Folders and summaries
// ---------------------------------------------------------------------------

fn role_kind(role: &str) -> FolderKind {
    match role {
        "inbox" => FolderKind::Inbox,
        "sent" => FolderKind::Sent,
        "drafts" => FolderKind::Drafts,
        "trash" => FolderKind::Trash,
        "junk" => FolderKind::Junk,
        "archive" => FolderKind::Archive,
        _ => FolderKind::Custom,
    }
}

/// List the account's mailboxes (tree flattened into `/` paths, roles
/// mapped), sorted and id-numbered exactly like the IMAP path's folder list.
fn jmap_list_folders(
    s: &JmapSession,
    account_id: u32,
    assignments: &BTreeMap<String, String>,
) -> Result<Vec<JmapFolder>, String> {
    let responses = jmap_call(
        s,
        &[CAP_CORE, CAP_MAIL],
        vec![call(0, "Mailbox/get", serde_json::json!({ "accountId": s.account, "ids": null }))],
    )?;
    let list = args(&responses, 0)["list"].as_array().cloned().unwrap_or_default();
    let by_id: HashMap<String, &serde_json::Value> = list
        .iter()
        .filter_map(|m| m["id"].as_str().map(|id| (id.to_string(), m)))
        .collect();
    // A mailbox's path is its ancestors' names and its own, joined with '/',
    // which is what the sidebar's hierarchy expects. A name with a slash in
    // it would read as nesting; the character is swapped for a look-alike.
    let name_of = |m: &serde_json::Value| -> String {
        m["name"].as_str().unwrap_or("?").replace('/', "∕")
    };
    let mut out: Vec<JmapFolder> = Vec::new();
    let mut counts: HashMap<String, (u32, u32)> = HashMap::new();
    for m in &list {
        let Some(id) = m["id"].as_str() else { continue };
        let mut names = vec![name_of(m)];
        let mut parent = m["parentId"].as_str();
        let mut depth = 0;
        while let Some(pid) = parent {
            let Some(p) = by_id.get(pid) else { break };
            names.push(name_of(p));
            parent = p["parentId"].as_str();
            depth += 1;
            if depth > 10 {
                break;
            }
        }
        names.reverse();
        let path = names.join("/");
        let name = names.last().cloned().unwrap_or_default();
        let kind = m["role"].as_str().map(role_kind).unwrap_or(FolderKind::Custom);
        let count = |field: &str| m[field].as_i64().unwrap_or(0).max(0) as u32;
        counts.insert(id.to_string(), (count("totalEmails"), count("unreadEmails")));
        out.push(JmapFolder {
            mailbox_id: id.to_string(),
            folder: Folder { id: 0, account_id, name, path, kind, unread: 0 },
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
    // Manual Special Folders assignments (#82) over the server's roles,
    // before the chips are counted, as the IMAP listing does.
    let mut folders: Vec<Folder> = out.iter().map(|f| f.folder.clone()).collect();
    crate::models::assign_folder_roles(assignments, &mut folders);
    for (jf, f) in out.iter_mut().zip(folders) {
        jf.folder.kind = f.kind;
        let (total, unseen) = counts.get(&jf.mailbox_id).copied().unwrap_or_default();
        jf.folder.unread = if chip_counts_all(f.kind) { total } else { unseen };
    }
    Ok(out)
}

/// The comma-separated addresses of a JMAP address list.
fn jmap_addrs(v: &serde_json::Value) -> String {
    v.as_array()
        .map(|list| {
            list.iter().filter_map(|r| r["email"].as_str()).collect::<Vec<_>>().join(", ")
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

/// Map one `Email/get` summary to a [`Message`]. Returns the JMAP id and
/// blob id too, so the caller can index them.
fn jmap_message(v: &serde_json::Value, account_id: u32, folder_id: u32) -> Option<(Message, String, String)> {
    let id = v["id"].as_str()?.to_string();
    let blob = v["blobId"].as_str().unwrap_or("").to_string();
    let uid = hash_uid(&id);
    let ts = v["receivedAt"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp())
        .unwrap_or(0);
    let preview: String = v["preview"]
        .as_str()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect();
    let preview = pgp_preview(&preview).unwrap_or(preview);
    let from = v["from"].as_array().and_then(|a| a.first()).cloned().unwrap_or_default();
    let from_addr = from["email"].as_str().unwrap_or("").to_string();
    let reply_to = jmap_addrs(&v["replyTo"]);
    let reply_to = if reply_to.eq_ignore_ascii_case(&from_addr) { String::new() } else { reply_to };
    let keywords = v["keywords"].as_object();
    let has = |k: &str| keywords.and_then(|m| m.get(k)).and_then(|b| b.as_bool()).unwrap_or(false);
    let user_keywords: Vec<String> = keywords
        .map(|m| {
            m.iter()
                .filter(|(k, b)| b.as_bool().unwrap_or(false) && !SYSTEM_KEYWORDS.contains(&k.to_ascii_lowercase().as_str()))
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
        to: jmap_addrs(&v["to"]),
        cc: jmap_addrs(&v["cc"]),
        subject: v["subject"].as_str().unwrap_or("").to_string(),
        preview,
        body: String::new(),
        date: if ts > 0 { label_from_timestamp(ts) } else { String::new() },
        timestamp: ts,
        unread: !has("$seen"),
        starred: has("$flagged"),
        keywords: user_keywords,
        has_attachment: v["hasAttachment"].as_bool().unwrap_or(false),
        message_id: jmap_msgids(&v["messageId"]).into_iter().next().unwrap_or_default(),
        references: refs.join(" "),
    };
    Some((msg, id, blob))
}

/// The ids matching a filter, newest first, up to `limit`.
fn jmap_query_ids(s: &JmapSession, filter: serde_json::Value, limit: usize) -> Result<Vec<String>, String> {
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
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default())
}

/// Summaries for a set of ids.
fn jmap_get_messages(
    s: &JmapSession,
    ids: &[String],
    account_id: u32,
    folder_id: u32,
) -> Result<Vec<(Message, String, String)>, String> {
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
        .map(|list| list.iter().filter_map(|v| jmap_message(v, account_id, folder_id)).collect())
        .unwrap_or_default())
}

/// A folder's newest summaries (newest first): the query and the get in one
/// request, the second fed by the first.
fn jmap_list_messages(
    s: &JmapSession,
    mailbox_id: &str,
    account_id: u32,
    folder_id: u32,
) -> Result<Vec<(Message, String, String)>, String> {
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
                    "limit": JMAP_INDEX_CAP,
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
    Ok(args(&responses, 1)["list"]
        .as_array()
        .map(|list| list.iter().filter_map(|v| jmap_message(v, account_id, folder_id)).collect())
        .unwrap_or_default())
}

/// `Email/set`: patch some messages and/or destroy others. Anything the
/// server refused is logged, not surfaced: the UI already moved on.
fn jmap_set_emails(
    s: &JmapSession,
    update: serde_json::Value,
    destroy: &[String],
) -> Result<serde_json::Value, String> {
    let mut a = serde_json::json!({ "accountId": s.account });
    if update.as_object().is_some_and(|m| !m.is_empty()) {
        a["update"] = update;
    }
    if !destroy.is_empty() {
        a["destroy"] = serde_json::json!(destroy);
    }
    let responses = jmap_call(s, &[CAP_CORE, CAP_MAIL], vec![call(0, "Email/set", a)])?;
    let r = args(&responses, 0);
    for key in ["notUpdated", "notDestroyed"] {
        if let Some(m) = r[key].as_object() {
            for (id, why) in m {
                tracing::warn!(
                    "jmap: Email/set {key} {id}: {}",
                    why["description"].as_str().or(why["type"].as_str()).unwrap_or("?")
                );
            }
        }
    }
    Ok(r)
}

/// Patch every id the same way, in batches the server accepts.
fn jmap_patch_all(s: &JmapSession, ids: &[String], patch: &serde_json::Value) -> Result<(), String> {
    for chunk in ids.chunks(200) {
        let update: serde_json::Map<String, serde_json::Value> =
            chunk.iter().map(|id| (id.clone(), patch.clone())).collect();
        jmap_set_emails(s, serde_json::Value::Object(update), &[])?;
    }
    Ok(())
}

fn jmap_destroy_all(s: &JmapSession, ids: &[String]) -> Result<(), String> {
    for chunk in ids.chunks(200) {
        jmap_set_emails(s, serde_json::json!({}), chunk)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Async glue
// ---------------------------------------------------------------------------

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.unwrap_or_else(|_| Err("task failed".into()))
}

/// The session, connecting on first use (and after a reconnect dropped it).
/// A failure is reported through `emit` and leaves the state untouched, so
/// the next request tries again.
async fn jmap_session(
    account: &AccountConfig,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
) -> Option<JmapSession> {
    if let Some(s) = &state.session {
        return Some(s.clone());
    }
    let a = account.clone();
    match blocking(move || jmap_connect(&a)).await {
        Ok(s) => {
            state.session = Some(s.clone());
            Some(s)
        }
        Err(e) => {
            emit(WorkerEvent::Error {
                text: i18n_f("Could not reach the JMAP server: {e}", &[("e", &e)]),
                connectivity: true,
            });
            None
        }
    }
}

/// Re-list the folders, emit them, remember the path → mailbox-id map.
async fn refresh_jmap_folders(
    s: &JmapSession,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
) {
    let sess = s.clone();
    let roles = state.roles.clone();
    match blocking(move || jmap_list_folders(&sess, account_id, &roles)).await {
        Ok(mut list) => {
            list.retain(|f| !crate::models::folder_is_hidden(&f.folder.path, Some("/"), &state.hidden));
            state.adopt_folders(&list);
            let folders: Vec<Folder> = list.into_iter().map(|f| f.folder).collect();
            if let Some(c) = cache {
                c.save_folders(account_id, &folders);
            }
            emit(WorkerEvent::Folders(folders));
        }
        Err(e) => emit(WorkerEvent::Error {
            text: i18n_f("Could not list folders: {e}", &[("e", &e)]),
            connectivity: true,
        }),
    }
}

/// Re-list the folders for their unread counts and push them, quietly: this
/// rides the background poll and the push channel.
async fn jmap_refresh_unread(
    s: &JmapSession,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
) {
    let sess = s.clone();
    let roles = state.roles.clone();
    let Ok(mut list) = blocking(move || jmap_list_folders(&sess, account_id, &roles)).await else {
        return;
    };
    list.retain(|f| !crate::models::folder_is_hidden(&f.folder.path, Some("/"), &state.hidden));
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

/// List a folder's summaries, refresh the uid map and the cache.
async fn jmap_load_folder(
    s: &JmapSession,
    account_id: u32,
    folder_id: u32,
    path: &str,
    cache: Option<&Cache>,
    state: &mut JmapState,
) -> Result<Vec<Message>, String> {
    if !state.folders.contains_key(path) {
        let sess = s.clone();
        let roles = state.roles.clone();
        if let Ok(list) = blocking(move || jmap_list_folders(&sess, account_id, &roles)).await {
            state.adopt_folders(&list);
        }
    }
    let (_, mailbox_id) =
        state.folders.get(path).cloned().ok_or_else(|| format!("unknown folder {path}"))?;
    let sess = s.clone();
    let listed = blocking(move || jmap_list_messages(&sess, &mailbox_id, account_id, folder_id)).await?;
    let mut messages = Vec::with_capacity(listed.len());
    for (m, id, blob) in listed {
        state.uids.insert(m.uid, (id, blob));
        messages.push(m);
    }
    if let Some(c) = cache {
        c.save_messages(account_id, path, &messages);
    }
    Ok(messages)
}

/// Resolve a message uid to its JMAP (id, blob id), re-listing the folder
/// once if the uid isn't in the map (fresh start from cache).
async fn jmap_resolve(
    s: &JmapSession,
    state: &mut JmapState,
    path: &str,
    uid: u32,
) -> Option<(String, String)> {
    if let Some(ids) = state.uids.get(&uid) {
        return Some(ids.clone());
    }
    let _ = jmap_load_folder(s, 0, 0, path, None, state).await;
    state.uids.get(&uid).cloned()
}

/// Fetch a message's raw RFC 5322 bytes.
async fn jmap_fetch_raw(
    s: &JmapSession,
    state: &mut JmapState,
    path: &str,
    uid: u32,
) -> Result<Vec<u8>, String> {
    let (_, blob) = jmap_resolve(s, state, path, uid).await.ok_or_else(|| "message not found".to_string())?;
    if blob.is_empty() {
        return Err("message has no blob".into());
    }
    let sess = s.clone();
    blocking(move || jmap_download(&sess, &blob)).await
}

/// Patch one message (read state, flag, keyword). Errors are logged, not
/// surfaced: the optimistic UI state already changed.
async fn jmap_patch_message(
    s: &JmapSession,
    state: &mut JmapState,
    path: &str,
    uid: u32,
    patch: serde_json::Value,
) {
    let Some((id, _)) = jmap_resolve(s, state, path, uid).await else { return };
    let sess = s.clone();
    let update = serde_json::json!({ id: patch });
    if let Err(e) = blocking(move || jmap_set_emails(&sess, update, &[])).await {
        tracing::warn!("jmap: could not update message: {e}");
    }
}

/// Move messages to another folder (a message keeps its id across a move,
/// so the uid map stays right). `extra` patches ride along: the `$junk` /
/// `$notjunk` keywords a spam verdict sets, which is what the server's
/// classifier learns from.
async fn jmap_move_uids(
    s: &JmapSession,
    account_id: u32,
    state: &mut JmapState,
    path: &str,
    uids: &[u32],
    dest: &str,
    extra: serde_json::Value,
    cache: Option<&Cache>,
) -> Result<(), String> {
    let dest_id = match state.folders.get(dest) {
        Some((_, id)) => id.clone(),
        None => return Err(format!("unknown folder {dest}")),
    };
    let mut update = serde_json::Map::new();
    let mut moved = Vec::new();
    for &uid in uids {
        let Some((id, _)) = jmap_resolve(s, state, path, uid).await else { continue };
        let mut patch = serde_json::json!({ "mailboxIds": { dest_id.clone(): true } });
        if let Some(m) = extra.as_object() {
            for (k, v) in m {
                patch[k] = v.clone();
            }
        }
        update.insert(id, patch);
        moved.push(uid);
    }
    if update.is_empty() {
        return Ok(());
    }
    let sess = s.clone();
    blocking(move || jmap_set_emails(&sess, serde_json::Value::Object(update), &[])).await?;
    if let Some(c) = cache {
        for uid in moved {
            c.delete_message(account_id, path, uid);
        }
    }
    Ok(())
}

/// Undo a move: the messages are in `path` (where the move put them); find
/// them by Message-ID and move them back to `dest`.
async fn jmap_undo_move(
    s: &JmapSession,
    account_id: u32,
    state: &mut JmapState,
    path: &str,
    dest: &str,
    message_ids: &[String],
    cache: Option<&Cache>,
) -> Result<usize, String> {
    let (folder_id, mailbox_id) =
        state.folders.get(path).cloned().ok_or_else(|| format!("unknown folder {path}"))?;
    let wanted: HashSet<&str> = message_ids.iter().map(|s| s.as_str()).collect();
    let sess = s.clone();
    let listed = blocking(move || jmap_list_messages(&sess, &mailbox_id, account_id, folder_id)).await?;
    let mut uids = Vec::new();
    for (m, id, blob) in listed {
        if wanted.contains(m.message_id.as_str()) {
            state.uids.insert(m.uid, (id, blob));
            uids.push(m.uid);
        }
    }
    if uids.is_empty() {
        return Ok(0);
    }
    let n = uids.len();
    jmap_move_uids(s, account_id, state, path, &uids, dest, serde_json::json!({}), cache).await?;
    Ok(n)
}

/// Delete messages for good; the cache and the uid map forget every one.
async fn jmap_purge_uids(
    s: &JmapSession,
    account_id: u32,
    state: &mut JmapState,
    path: &str,
    uids: Vec<u32>,
    cache: Option<&Cache>,
) {
    let mut ids = Vec::new();
    let mut gone = Vec::new();
    for uid in uids {
        if let Some((id, _)) = jmap_resolve(s, state, path, uid).await {
            ids.push(id);
            gone.push(uid);
        }
    }
    if ids.is_empty() {
        return;
    }
    let sess = s.clone();
    if blocking(move || jmap_destroy_all(&sess, &ids)).await.is_ok() {
        for uid in gone {
            state.uids.remove(&uid);
            if let Some(c) = cache {
                c.delete_message(account_id, path, uid);
            }
        }
    }
}

/// Everything in a mailbox, in pages, for a bulk operation.
fn jmap_all_ids_in(s: &JmapSession, mailbox_id: &str, extra_filter: serde_json::Value) -> Result<Vec<String>, String> {
    let mut filter = serde_json::json!({ "inMailbox": mailbox_id });
    if let Some(m) = extra_filter.as_object() {
        for (k, v) in m {
            filter[k] = v.clone();
        }
    }
    let mut out = Vec::new();
    loop {
        let responses = jmap_call(
            s,
            &[CAP_CORE, CAP_MAIL],
            vec![call(
                0,
                "Email/query",
                serde_json::json!({
                    "accountId": s.account,
                    "filter": filter,
                    "position": out.len(),
                    "limit": 500,
                }),
            )],
        )?;
        let page: Vec<String> = args(&responses, 0)["ids"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let n = page.len();
        out.extend(page);
        if n < 500 || out.len() >= 50_000 {
            return Ok(out);
        }
    }
}

/// Body-search hits (#191) for the inbox listing: one `Email/query` with a
/// `body` filter per needle.
async fn emit_jmap_body_hits(
    s: &JmapSession,
    account: &AccountConfig,
    folder_id: u32,
    path: &str,
    messages: &[Message],
    state: &JmapState,
    emit: &impl Fn(WorkerEvent),
) {
    if messages.is_empty() || state.inbox.as_ref().map(|(_, p)| p.as_str()) != Some(path) {
        return;
    }
    let needles = crate::config::filter_body_needles(&account.email);
    if needles.is_empty() {
        return;
    }
    let Some((_, mailbox_id)) = state.folders.get(path).cloned() else { return };
    let listed: HashSet<u32> = messages.iter().map(|m| m.uid).collect();
    let mut hits: HashMap<u32, Vec<String>> = HashMap::new();
    for needle in needles {
        let sess = s.clone();
        let mid = mailbox_id.clone();
        let n = needle.clone();
        let found = blocking(move || {
            jmap_query_ids(&sess, serde_json::json!({ "inMailbox": mid, "body": n }), 250)
        })
        .await;
        match found {
            Ok(ids) => {
                for uid in ids.iter().map(|id| hash_uid(id)).filter(|u| listed.contains(u)) {
                    hits.entry(uid).or_default().push(needle.clone());
                }
            }
            Err(e) => tracing::warn!("filter: JMAP body search for {needle:?} failed: {e}"),
        }
    }
    tracing::info!("filter: JMAP body search over {} messages hit {}", listed.len(), hits.len());
    emit(WorkerEvent::BodyHits { folder_id, hits });
}

/// Auto-empty (#140): destroy whatever in Junk / Trash is older than the
/// account's chosen age.
async fn auto_empty_jmap(
    s: &JmapSession,
    account_id: u32,
    account: &AccountConfig,
    cache: Option<&Cache>,
    state: &mut JmapState,
    last: &mut Option<std::time::Instant>,
    emit: &impl Fn(WorkerEvent),
) {
    if !auto_empty_due(account, last) {
        return;
    }
    let folders = cache.map(|c| c.load_folders(account_id)).unwrap_or_default();
    let mut purged_any = false;
    for (kind, role, days) in auto_empty_roles(account) {
        let Some(path) = role_folder_path(account, &folders, kind, role) else { continue };
        let Some((_, mailbox_id)) = state.folders.get(&path).cloned() else { continue };
        let before = graph_before_date(days);
        let sess = s.clone();
        let ids = match blocking(move || {
            let ids = jmap_all_ids_in(&sess, &mailbox_id, serde_json::json!({ "before": before }))?;
            jmap_destroy_all(&sess, &ids)?;
            Ok(ids)
        })
        .await
        {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!("auto-empty: could not empty {path} on account {account_id}: {e}");
                continue;
            }
        };
        for id in &ids {
            let uid = hash_uid(id);
            state.uids.remove(&uid);
            if let Some(c) = cache {
                c.delete_message(account_id, &path, uid);
            }
        }
        tracing::info!(
            "auto-empty: deleted {} message(s) older than {days} days from {path} on account {account_id}",
            ids.len()
        );
        purged_any |= !ids.is_empty();
    }
    if purged_any {
        jmap_refresh_unread(s, account_id, cache, state, emit).await;
    }
}

// ---------------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------------

/// The identity to submit as: the one whose address is the From, else the
/// first the server offers.
fn jmap_identity_for(s: &JmapSession, from: &str) -> Result<String, String> {
    let responses = jmap_call(
        s,
        &[CAP_CORE, CAP_SUBMISSION],
        vec![call(0, "Identity/get", serde_json::json!({ "accountId": s.account, "ids": null }))],
    )?;
    let list = args(&responses, 0)["list"].as_array().cloned().unwrap_or_default();
    let by_addr = list
        .iter()
        .find(|i| i["email"].as_str().is_some_and(|e| e.eq_ignore_ascii_case(from)));
    by_addr
        .or_else(|| list.first())
        .and_then(|i| i["id"].as_str().map(str::to_string))
        .ok_or_else(|| "the server offers no sending identity for this account".to_string())
}

/// Send raw RFC 5322 bytes: upload, import into the Sent folder, submit.
/// The server files nothing else, so the copy is the imported message; a
/// refused submission takes it back out.
fn jmap_submit(
    s: &JmapSession,
    raw: &[u8],
    from: &str,
    rcpts: &[String],
    sent_mailbox: &str,
) -> Result<String, String> {
    let blob = jmap_upload(s, raw, "message/rfc822")?;
    let identity = jmap_identity_for(s, from)?;
    let responses = jmap_call(
        s,
        &[CAP_CORE, CAP_MAIL],
        vec![call(
            0,
            "Email/import",
            serde_json::json!({
                "accountId": s.account,
                "emails": { "m": {
                    "blobId": blob,
                    "mailboxIds": { sent_mailbox: true },
                    "keywords": { "$seen": true },
                }},
            }),
        )],
    )?;
    let imported = args(&responses, 0);
    let email_id = imported["created"]["m"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| {
            let why = &imported["notCreated"]["m"];
            format!(
                "the server would not take the message: {}",
                why["description"].as_str().or(why["type"].as_str()).unwrap_or("unknown reason")
            )
        })?;
    let envelope = serde_json::json!({
        "mailFrom": { "email": from },
        "rcptTo": rcpts.iter().map(|r| serde_json::json!({ "email": r })).collect::<Vec<_>>(),
    });
    let submitted = jmap_call(
        s,
        &[CAP_CORE, CAP_MAIL, CAP_SUBMISSION],
        vec![call(
            0,
            "EmailSubmission/set",
            serde_json::json!({
                "accountId": s.account,
                "create": { "s": { "emailId": email_id, "identityId": identity, "envelope": envelope } },
            }),
        )],
    );
    let ok = match &submitted {
        Ok(r) => {
            let r = args(r, 0);
            if r["created"]["s"].is_object() {
                Ok(())
            } else {
                let why = &r["notCreated"]["s"];
                Err(format!(
                    "the server refused to send it: {}",
                    why["description"].as_str().or(why["type"].as_str()).unwrap_or("unknown reason")
                ))
            }
        }
        Err(e) => Err(e.clone()),
    };
    if let Err(e) = ok {
        let _ = jmap_destroy_all(s, &[email_id]);
        return Err(e);
    }
    Ok(email_id)
}

/// The Sent folder a copy of outgoing mail is filed in (#199): the
/// account's choice, else the server's Sent mailbox.
fn jmap_sent_target(account: &AccountConfig, state: &JmapState, queued: Option<&str>) -> Option<(u32, String, String)> {
    let path = account
        .sent_copy_path
        .clone()
        .filter(|p| !p.is_empty() && state.folders.contains_key(p))
        .or_else(|| queued.map(str::to_string).filter(|p| state.folders.contains_key(p)))
        .or_else(|| state.sent.as_ref().map(|(_, p)| p.clone()))?;
    let (folder_id, mailbox_id) = state.folders.get(&path).cloned()?;
    Some((folder_id, path, mailbox_id))
}

/// Send over JMAP: the same raw MIME `build_email` produces, submitted by
/// the server, with the Sent copy indexed at once (#199).
async fn jmap_send_message(
    s: &JmapSession,
    account_id: u32,
    account: &AccountConfig,
    message: &OutgoingMessage,
    sent_path: Option<&str>,
    cache: Option<&Cache>,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
) -> Result<(), String> {
    let email = build_email(account, message).map_err(|e| e.to_string())?;
    let envelope = email.envelope().clone();
    let from = envelope.from().map(|f| f.to_string()).unwrap_or_else(|| account.email.clone());
    let rcpts: Vec<String> = envelope.to().iter().map(|a| a.to_string()).collect();
    let raw = email.formatted();
    let (folder_id, path, mailbox_id) = jmap_sent_target(account, state, sent_path)
        .ok_or_else(|| "the account has no Sent folder to file the copy in".to_string())?;
    let sess = s.clone();
    let email_id = blocking(move || jmap_submit(&sess, &raw, &from, &rcpts, &mailbox_id)).await?;
    jmap_index_sent_copy(s, account_id, folder_id, &path, &email_id, cache, state, emit).await;
    Ok(())
}

/// Put the sent copy in the cache and on screen before the app is told the
/// send is done (#199), so it can join the conversation it answers.
async fn jmap_index_sent_copy(
    s: &JmapSession,
    account_id: u32,
    folder_id: u32,
    path: &str,
    email_id: &str,
    cache: Option<&Cache>,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
) {
    let sess = s.clone();
    let ids = vec![email_id.to_string()];
    if let Ok(got) = blocking(move || jmap_get_messages(&sess, &ids, account_id, folder_id)).await {
        let mut messages = Vec::new();
        for (m, id, blob) in got {
            state.uids.insert(m.uid, (id, blob));
            messages.push(m);
        }
        if !messages.is_empty() {
            if let Some(c) = cache {
                c.upsert_messages(account_id, path, &messages);
            }
            emit(WorkerEvent::Messages { folder_id, messages });
        }
    }
}

/// The Outbox retry loop for JMAP accounts: same queue and bookkeeping as
/// [`flush_outbox`], with `EmailSubmission` as the transport.
async fn jmap_flush_outbox(
    s: &JmapSession,
    cache: Option<&Cache>,
    account_id: u32,
    account: &AccountConfig,
    id: Option<u32>,
    state: &mut JmapState,
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
    let mut sent_any = false;
    for item in items {
        let Some((folder_id, path, mailbox_id)) = jmap_sent_target(account, state, item.sent_path.as_deref()) else {
            cache.record_outbox_failure(item.id, "no Sent folder to file the copy in");
            continue;
        };
        let sess = s.clone();
        let raw = item.raw.clone();
        let from = item.from_addr.clone();
        let rcpts = item.rcpts.clone();
        match blocking(move || jmap_submit(&sess, &raw, &from, &rcpts, &mailbox_id)).await {
            Ok(email_id) => {
                sent_any = true;
                cache.delete_outbox(item.id);
                jmap_index_sent_copy(s, account_id, folder_id, &path, &email_id, Some(cache), state, emit).await;
            }
            Err(e) => {
                cache.record_outbox_failure(item.id, &e);
                emit(WorkerEvent::Error {
                    text: i18n_f(
                        "Still could not send “{subject}”: {e}",
                        &[("subject", &item.subject), ("e", &e)],
                    ),
                    connectivity: false,
                });
            }
        }
    }
    emit(WorkerEvent::Status(String::new()));
    if sent_any {
        emit(WorkerEvent::Sent);
    }
    emit_outbox(Some(cache), account_id, emit);
}

/// Save a draft: upload the bytes, import them into Drafts.
fn jmap_import_draft(s: &JmapSession, raw: &[u8], drafts_mailbox: &str) -> Result<String, String> {
    let blob = jmap_upload(s, raw, "message/rfc822")?;
    let responses = jmap_call(
        s,
        &[CAP_CORE, CAP_MAIL],
        vec![call(
            0,
            "Email/import",
            serde_json::json!({
                "accountId": s.account,
                "emails": { "d": {
                    "blobId": blob,
                    "mailboxIds": { drafts_mailbox: true },
                    "keywords": { "$draft": true, "$seen": true },
                }},
            }),
        )],
    )?;
    let r = args(&responses, 0);
    r["created"]["d"]["id"].as_str().map(str::to_string).ok_or_else(|| {
        let why = &r["notCreated"]["d"];
        why["description"].as_str().or(why["type"].as_str()).unwrap_or("the server would not take the draft").to_string()
    })
}

/// Drop the draft a message was opened from, server-side and in the cache.
async fn jmap_drop_draft_origin(
    s: &JmapSession,
    account_id: u32,
    origin: &crate::models::DraftOrigin,
    cache: Option<&Cache>,
    state: &mut JmapState,
) {
    if origin.account_id != account_id {
        return;
    }
    if let Some((id, _)) = jmap_resolve(s, state, &origin.path, origin.uid).await {
        let sess = s.clone();
        let _ = blocking(move || jmap_destroy_all(&sess, &[id])).await;
        state.uids.remove(&origin.uid);
    }
    if let Some(c) = cache {
        c.delete_message(account_id, &origin.path, origin.uid);
    }
}

// ---------------------------------------------------------------------------
// Push
// ---------------------------------------------------------------------------

/// Keep the server's EventSource open on a thread of its own and tick the
/// channel whenever mail or mailboxes change. The stream is re-opened after
/// a pause when it drops; the thread ends once the worker has gone.
fn spawn_jmap_push(s: JmapSession, tx: mpsc::UnboundedSender<()>) {
    let Some(template) = s.event_source_url.clone() else { return };
    let url = fill_template(&template, &[("types", "Email,Mailbox"), ("closeafter", "no"), ("ping", "30")]);
    std::thread::Builder::new()
        .name("jmap-push".into())
        .spawn(move || {
            use std::io::BufRead;
            loop {
                if tx.is_closed() {
                    return;
                }
                jmap_wire("GET events");
                let resp = ureq::AgentBuilder::new()
                    .timeout_connect(Duration::from_secs(20))
                    // The server pings every 30 s; well past that means the
                    // connection is dead.
                    .timeout_read(Duration::from_secs(120))
                    .build()
                    .get(&url)
                    .set("Authorization", &s.auth)
                    .set("Accept", "text/event-stream")
                    .call()
                    .map_err(jmap_err);
                jmap_wired("GET events", &resp);
                if let Ok(resp) = resp {
                    let reader = std::io::BufReader::new(resp.into_reader());
                    let mut event = String::new();
                    for line in reader.lines() {
                        let Ok(line) = line else { break };
                        if let Some(name) = line.strip_prefix("event:") {
                            event = name.trim().to_string();
                        } else if line.is_empty() {
                            if event == "state" && tx.send(()).is_err() {
                                return;
                            }
                            event.clear();
                        }
                    }
                }
                if tx.is_closed() {
                    return;
                }
                std::thread::sleep(Duration::from_secs(30));
            }
        })
        .ok();
}

/// One poll tick or push signal: refresh the Inbox and the folder counts.
async fn jmap_poll_inbox(
    s: &JmapSession,
    account_id: u32,
    account: &AccountConfig,
    cache: Option<&Cache>,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
) {
    if state.inbox.is_none() {
        refresh_jmap_folders(s, account_id, cache, state, emit).await;
    }
    let Some((folder_id, path)) = state.inbox.clone() else { return };
    if let Ok(messages) = jmap_load_folder(s, account_id, folder_id, &path, cache, state).await {
        let unread = messages.iter().filter(|m| m.unread).count() as u32;
        emit_jmap_body_hits(s, account, folder_id, &path, &messages, state, emit).await;
        emit(WorkerEvent::Messages { folder_id, messages });
        emit(WorkerEvent::FolderUnread { folder_id, unread });
    }
    jmap_refresh_unread(s, account_id, cache, state, emit).await;
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

/// Test that the server accepts the credentials and offers mail.
pub(super) async fn test_jmap(account: &AccountConfig) -> Result<(), String> {
    let a = account.clone();
    blocking(move || jmap_connect(&a)).await.map(|_| ())
}

/// A body from the cache, or none.
fn serve_cached(
    cache: Option<&Cache>,
    account_id: u32,
    path: &str,
    uid: u32,
    message_id: u32,
    emit: &impl Fn(WorkerEvent),
) -> bool {
    let Some(c) = cache else { return false };
    let Some(body) = c.load_body(account_id, path, uid) else { return false };
    emit(WorkerEvent::Body { message_id, path: path.to_string(), body });
    if let Some(check) = c.load_sender_check(account_id, path, uid) {
        emit(WorkerEvent::SenderChecked { message_id, check });
    }
    true
}

/// Fetch, render and cache one body.
async fn jmap_deliver_body(
    s: &JmapSession,
    account_id: u32,
    message_id: u32,
    path: &str,
    uid: u32,
    cache: Option<&Cache>,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
) {
    match jmap_fetch_raw(s, state, path, uid).await {
        Ok(raw) => {
            let (body, check, _) = render_raw(&raw);
            if let Some(c) = cache {
                if body_cacheable(&check) {
                    c.save_body(account_id, path, uid, &body);
                }
                c.save_sender_check(account_id, path, uid, &check);
            }
            emit(WorkerEvent::Body { message_id, path: path.to_string(), body });
            emit(WorkerEvent::SenderChecked { message_id, check });
        }
        Err(e) => emit(WorkerEvent::Error {
            text: i18n_f("Could not load message: {e}", &[("e", &e)]),
            connectivity: true,
        }),
    }
}

pub(super) async fn run_jmap(
    account_id: u32,
    mut account: AccountConfig,
    mut rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent),
) {
    // The password lives in the keyring, like an IMAP account's.
    if account.password.is_empty() {
        if let Some(pw) = crate::config::load_password(&account.email) {
            account.password = pw;
        }
    } else {
        let _ = crate::config::store_password(&account.email, &account.password);
        crate::config::strip_passwords_on_disk();
    }

    let cache = Cache::open().map_err(|e| tracing::warn!("cache unavailable: {e}")).ok();

    emit(WorkerEvent::Account(Account {
        id: account_id,
        name: account.name.clone(),
        email: account.email.clone(),
        label: account.display_label(),
        accent: accent_for(account_id).into(),
    }));

    let cached_folders = cache.as_ref().map(|c| c.load_folders(account_id)).unwrap_or_default();
    if !cached_folders.is_empty() {
        emit(WorkerEvent::Folders(cached_folders));
    }

    let mut last_auto_empty: Option<std::time::Instant> = None;
    let mut state = JmapState {
        session: None,
        folders: HashMap::new(),
        uids: HashMap::new(),
        drafts: None,
        inbox: None,
        sent: None,
        roles: account.folder_roles.clone(),
        hidden: account.hidden_folders.clone(),
    };

    let (push_tx, mut push_rx) = mpsc::unbounded_channel::<()>();
    let push_enabled = account.push.unwrap_or_else(crate::config::load_push);
    let mut push_started = false;

    if let Some(s) = jmap_session(&account, &mut state, &emit).await {
        refresh_jmap_folders(&s, account_id, cache.as_ref(), &mut state, &emit).await;
        if push_enabled {
            spawn_jmap_push(s.clone(), push_tx.clone());
            push_started = true;
        }
    }

    // The poll is the fallback for a server without push (or with it off):
    // the auto-fetch cadence when set, otherwise a quiet couple of minutes.
    let poll_secs = match crate::config::load_fetch_interval() {
        0 => 120,
        s => s.max(60),
    };
    let mut poll = tokio::time::interval(Duration::from_secs(poll_secs));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    poll.tick().await;

    loop {
        let req = tokio::select! {
            req = rx.recv() => match req {
                Some(req) => req,
                None => break,
            },
            _ = poll.tick() => {
                if let Some(s) = jmap_session(&account, &mut state, &|_| {}).await {
                    jmap_poll_inbox(&s, account_id, &account, cache.as_ref(), &mut state, &emit).await;
                    if push_enabled && !push_started {
                        spawn_jmap_push(s.clone(), push_tx.clone());
                        push_started = true;
                    }
                }
                continue;
            }
            Some(()) = push_rx.recv() => {
                // A burst of changes is one refresh.
                while push_rx.try_recv().is_ok() {}
                if let Some(s) = jmap_session(&account, &mut state, &|_| {}).await {
                    jmap_poll_inbox(&s, account_id, &account, cache.as_ref(), &mut state, &emit).await;
                }
                continue;
            }
        };
        match req {
            MailRequest::Locate { message_id } => {
                // The cache first, as on IMAP: every folder listing went
                // through it. Then a header filter on the server, which finds
                // the message wherever it is filed on a server that indexes
                // Message-ID (Stalwart's full-text index does not).
                let mut hit = cache
                    .as_ref()
                    .map(|c| c.locate_by_message_id(&message_id))
                    .unwrap_or_default()
                    .into_iter()
                    .find(|(a, _, _)| *a == account_id)
                    .map(|(_, path, uid)| (path, uid));
                if hit.is_none() {
                    if let Some(s) = jmap_session(&account, &mut state, &emit).await {
                    let sess = s.clone();
                    let mid = message_id.clone();
                    let found = blocking(move || {
                        let ids = jmap_query_ids(&sess, serde_json::json!({ "header": ["Message-ID", mid] }), 5)?;
                        jmap_get_messages(&sess, &ids, 0, 0).map(|got| {
                            got.into_iter().map(|(m, id, blob)| (m, id, blob)).collect::<Vec<_>>()
                        })
                    })
                    .await;
                    if let Ok(got) = found {
                        // The listing carries the mailbox ids in the raw
                        // object, which the summary drops; ask again for
                        // just those.
                        let ids: Vec<String> = got.iter().map(|(_, id, _)| id.clone()).collect();
                        let sess = s.clone();
                        let boxes = blocking(move || {
                            let r = jmap_call(
                                &sess,
                                &[CAP_CORE, CAP_MAIL],
                                vec![call(0, "Email/get", serde_json::json!({
                                    "accountId": sess.account, "ids": ids, "properties": ["id", "blobId", "mailboxIds"]
                                }))],
                            )?;
                            Ok(args(&r, 0)["list"].as_array().cloned().unwrap_or_default())
                        })
                        .await
                        .unwrap_or_default();
                        'outer: for e in boxes {
                            let Some(id) = e["id"].as_str() else { continue };
                            let blob = e["blobId"].as_str().unwrap_or("").to_string();
                            for (mbox, _) in e["mailboxIds"].as_object().into_iter().flatten() {
                                if let Some((_, path)) = state.path_of(mbox) {
                                    let uid = hash_uid(id);
                                    state.uids.insert(uid, (id.to_string(), blob.clone()));
                                    hit = Some((path, uid));
                                    break 'outer;
                                }
                            }
                        }
                    }
                    }
                }
                emit(WorkerEvent::Located { message_id, hit });
            }
            MailRequest::FindKeywords => {
                // JMAP has no keyword listing; the cached rows are the
                // survey, as they are for the count on every path.
                let mut seen: BTreeMap<String, usize> = BTreeMap::new();
                if let Some(c) = cache.as_ref() {
                    for f in c.load_folders(account_id) {
                        for m in c.load_messages(account_id, &f.path, f.id) {
                            for k in m.keywords {
                                *seen.entry(k).or_default() += 1;
                            }
                        }
                    }
                }
                let found = seen
                    .into_iter()
                    .map(|(keyword, count)| KeywordFinding { keyword, name: None, color: None, count, folders: Vec::new() })
                    .collect();
                emit(WorkerEvent::KeywordsFound(found));
            }
            MailRequest::RefreshKeywords { .. } => {
                let mut paths = Vec::new();
                if let Some(s) = jmap_session(&account, &mut state, &emit).await {
                    let folders: Vec<(String, u32)> =
                        state.folders.iter().map(|(p, (id, _))| (p.clone(), *id)).collect();
                    for (path, folder_id) in folders {
                        if jmap_load_folder(&s, account_id, folder_id, &path, cache.as_ref(), &mut state).await.is_ok() {
                            paths.push(path);
                        }
                    }
                }
                emit(WorkerEvent::KeywordsSynced { paths });
            }

            // Cache-only, exactly like the other paths.
            MailRequest::ScanAttachments { folder_path } => {
                emit(WorkerEvent::AttachmentsScanned { folder_path, added: 0, remaining: 0 });
            }
            MailRequest::LoadRelated { message_id, ids } => {
                let messages = cache.as_ref().map(|c| related_from_cache(c, account_id, &ids)).unwrap_or_default();
                emit(WorkerEvent::Related { message_id, messages });
            }
            MailRequest::LoadThreadCounts { groups } => {
                let counts = cache.as_ref().map(|c| c.thread_counts(account_id, &groups)).unwrap_or_default();
                emit(WorkerEvent::ThreadCounts { counts });
            }

            MailRequest::LoadMessages { folder_id, path } | MailRequest::SyncFolder { folder_id, path } => {
                if let Some(c) = cache.as_ref() {
                    let cached = c.load_messages(account_id, &path, folder_id);
                    if !cached.is_empty() {
                        emit(WorkerEvent::Messages { folder_id, messages: cached });
                    }
                }
                emit(WorkerEvent::Status(i18n("Syncing…")));
                let Some(s) = jmap_session(&account, &mut state, &emit).await else {
                    emit(WorkerEvent::BackfillDone { folder_id });
                    emit(WorkerEvent::Status(String::new()));
                    continue;
                };
                match jmap_load_folder(&s, account_id, folder_id, &path, cache.as_ref(), &mut state).await {
                    Ok(messages) => {
                        let unread = state.chip_count(folder_id, &messages);
                        emit_jmap_body_hits(&s, &account, folder_id, &path, &messages, &state, &emit).await;
                        emit(WorkerEvent::Messages { folder_id, messages });
                        emit(WorkerEvent::FolderUnread { folder_id, unread });
                        emit(WorkerEvent::BackfillDone { folder_id });
                    }
                    Err(e) => {
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not fetch mail: {e}", &[("e", &e)]),
                            connectivity: true,
                        });
                        emit(WorkerEvent::BackfillDone { folder_id });
                    }
                }
                emit(WorkerEvent::Status(String::new()));
            }

            MailRequest::LoadBody { message_id, path, uid } => {
                if serve_cached(cache.as_ref(), account_id, &path, uid, message_id, &emit) {
                    continue;
                }
                let Some(s) = jmap_session(&account, &mut state, &emit).await else { continue };
                jmap_deliver_body(&s, account_id, message_id, &path, uid, cache.as_ref(), &mut state, &emit).await;
            }

            MailRequest::LoadBodies { items, path } => {
                for (message_id, uid) in items {
                    if serve_cached(cache.as_ref(), account_id, &path, uid, message_id, &emit) {
                        continue;
                    }
                    let Some(s) = jmap_session(&account, &mut state, &emit).await else { break };
                    jmap_deliver_body(&s, account_id, message_id, &path, uid, cache.as_ref(), &mut state, &emit).await;
                }
            }

            MailRequest::LoadSource { message_id: _, path, uid } => {
                let Some(s) = jmap_session(&account, &mut state, &emit).await else { continue };
                match jmap_fetch_raw(&s, &mut state, &path, uid).await {
                    Ok(raw) => emit(WorkerEvent::Source { text: String::from_utf8_lossy(&raw).into_owned() }),
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Could not load source: {e}", &[("e", &e)]),
                        connectivity: true,
                    }),
                }
            }

            MailRequest::LoadAttachments { message_id, path, uid, download } => {
                if let Some(c) = cache.as_ref() {
                    let items = c.load_attachments(account_id, &path, uid);
                    if !items.is_empty() {
                        emit(WorkerEvent::Attachments { message_id, items });
                        continue;
                    }
                }
                if !download {
                    emit(WorkerEvent::AttachmentsPending { message_id });
                    continue;
                }
                let Some(s) = jmap_session(&account, &mut state, &emit).await else { continue };
                match jmap_fetch_raw(&s, &mut state, &path, uid).await {
                    Ok(raw) => {
                        let items = extract_attachments(&raw);
                        if let Some(c) = cache.as_ref() {
                            c.save_attachments(account_id, &path, uid, &items);
                        }
                        emit(WorkerEvent::Attachments { message_id, items });
                    }
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Could not load attachments: {e}", &[("e", &e)]),
                        connectivity: true,
                    }),
                }
            }

            MailRequest::SetSeen { path, uid, seen } => {
                if let Some(c) = cache.as_ref() {
                    c.set_unread(account_id, &path, uid, !seen);
                }
                if let Some(s) = jmap_session(&account, &mut state, &emit).await {
                    let patch = serde_json::json!({ "keywords/$seen": if seen { serde_json::Value::Bool(true) } else { serde_json::Value::Null } });
                    jmap_patch_message(&s, &mut state, &path, uid, patch).await;
                }
                emit(WorkerEvent::SeenSettled { path, uid });
            }

            MailRequest::SetFlagged { path, uid, flagged } => {
                if let Some(c) = cache.as_ref() {
                    c.set_starred(account_id, &path, uid, flagged);
                }
                if let Some(s) = jmap_session(&account, &mut state, &emit).await {
                    let patch = serde_json::json!({ "keywords/$flagged": if flagged { serde_json::Value::Bool(true) } else { serde_json::Value::Null } });
                    jmap_patch_message(&s, &mut state, &path, uid, patch).await;
                }
            }

            MailRequest::SetKeyword { path, uid, keyword, add, .. } => {
                if let Some(c) = cache.as_ref() {
                    c.set_keyword(account_id, &path, uid, &keyword, add);
                }
                if let Some(s) = jmap_session(&account, &mut state, &emit).await {
                    let patch = serde_json::json!({ format!("keywords/{keyword}"): if add { serde_json::Value::Bool(true) } else { serde_json::Value::Null } });
                    jmap_patch_message(&s, &mut state, &path, uid, patch).await;
                }
            }

            MailRequest::MarkAllRead { folder_id, path } => {
                if let Some(s) = jmap_session(&account, &mut state, &emit).await {
                    if let Some((_, mailbox_id)) = state.folders.get(&path).cloned() {
                        let sess = s.clone();
                        let r = blocking(move || {
                            let ids = jmap_all_ids_in(&sess, &mailbox_id, serde_json::json!({ "notKeyword": "$seen" }))?;
                            jmap_patch_all(&sess, &ids, &serde_json::json!({ "keywords/$seen": true }))
                        })
                        .await;
                        if let Err(e) = r {
                            tracing::warn!("jmap: mark all read in {path} failed: {e}");
                        }
                    }
                }
                if let Some(c) = cache.as_ref() {
                    c.mark_folder_read(account_id, &path);
                }
                emit(WorkerEvent::FolderUnread { folder_id, unread: 0 });
            }

            MailRequest::MoveMessage { path, uid, dest } => {
                let Some(s) = jmap_session(&account, &mut state, &emit).await else { continue };
                if let Err(e) = jmap_move_uids(&s, account_id, &mut state, &path, &[uid], &dest, serde_json::json!({}), cache.as_ref()).await {
                    emit(WorkerEvent::Error {
                        text: i18n_f("Could not move message: {e}", &[("e", &e)]),
                        connectivity: false,
                    });
                }
            }

            MailRequest::MarkSpam { path, uid, dest } => {
                let Some(s) = jmap_session(&account, &mut state, &emit).await else { continue };
                let verdict = serde_json::json!({ "keywords/$junk": true, "keywords/$notjunk": null });
                if let Err(e) = jmap_move_uids(&s, account_id, &mut state, &path, &[uid], &dest, verdict, cache.as_ref()).await {
                    emit(WorkerEvent::Error {
                        text: i18n_f("Could not mark as spam: {e}", &[("e", &e)]),
                        connectivity: false,
                    });
                }
            }

            MailRequest::MarkHam { path, uid, dest } => {
                let Some(s) = jmap_session(&account, &mut state, &emit).await else { continue };
                let verdict = serde_json::json!({ "keywords/$junk": null, "keywords/$notjunk": true });
                if let Err(e) = jmap_move_uids(&s, account_id, &mut state, &path, &[uid], &dest, verdict, cache.as_ref()).await {
                    emit(WorkerEvent::Error {
                        text: i18n_f("Could not mark as not spam: {e}", &[("e", &e)]),
                        connectivity: false,
                    });
                }
            }
            MailRequest::MarkHamMany { path, uids, dest } => {
                if let Some(s) = jmap_session(&account, &mut state, &emit).await {
                    let verdict = serde_json::json!({ "keywords/$junk": null, "keywords/$notjunk": true });
                    if let Err(e) = jmap_move_uids(&s, account_id, &mut state, &path, &uids, &dest, verdict, cache.as_ref()).await {
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not mark {len} messages as not spam: {e}", &[("len", &uids.len().to_string()), ("e", &e)]),
                            connectivity: false,
                        });
                    }
                }
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::MoveMessages { path, uids, dest } => {
                if let Some(s) = jmap_session(&account, &mut state, &emit).await {
                    if let Err(e) = jmap_move_uids(&s, account_id, &mut state, &path, &uids, &dest, serde_json::json!({}), cache.as_ref()).await {
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not move messages: {e}", &[("e", &e)]),
                            connectivity: false,
                        });
                    }
                }
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::PurgeMessages { path, uids } => {
                if let Some(s) = jmap_session(&account, &mut state, &emit).await {
                    jmap_purge_uids(&s, account_id, &mut state, &path, uids, cache.as_ref()).await;
                }
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::EmptyFolder { folder_id, path } => {
                let Some(s) = jmap_session(&account, &mut state, &emit).await else { continue };
                let Some((_, mailbox_id)) = state.folders.get(&path).cloned() else { continue };
                let sess = s.clone();
                let r = blocking(move || {
                    let ids = jmap_all_ids_in(&sess, &mailbox_id, serde_json::json!({}))?;
                    jmap_destroy_all(&sess, &ids)?;
                    Ok(ids)
                })
                .await;
                match r {
                    Ok(ids) => {
                        for id in &ids {
                            let uid = hash_uid(id);
                            state.uids.remove(&uid);
                            if let Some(c) = cache.as_ref() {
                                c.delete_message(account_id, &path, uid);
                            }
                        }
                        tracing::info!("emptied {path}: {} message(s) erased", ids.len());
                        emit(WorkerEvent::Messages { folder_id, messages: Vec::new() });
                        emit(WorkerEvent::FolderUnread { folder_id, unread: 0 });
                    }
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Could not empty the folder: {e}", &[("e", &e)]),
                        connectivity: false,
                    }),
                }
            }

            MailRequest::UndoMove { path, dest, dest_folder_id, message_ids } => {
                let Some(s) = jmap_session(&account, &mut state, &emit).await else { continue };
                match jmap_undo_move(&s, account_id, &mut state, &path, &dest, &message_ids, cache.as_ref()).await {
                    Ok(0) => tracing::info!("undo: the messages are no longer where that move put them"),
                    Ok(_) => {
                        if let Ok(messages) = jmap_load_folder(&s, account_id, dest_folder_id, &dest, cache.as_ref(), &mut state).await {
                            let unread = state.chip_count(dest_folder_id, &messages);
                            emit(WorkerEvent::Messages { folder_id: dest_folder_id, messages });
                            emit(WorkerEvent::FolderUnread { folder_id: dest_folder_id, unread });
                        }
                    }
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Undo failed: {e}", &[("e", &e)]),
                        connectivity: false,
                    }),
                }
            }

            MailRequest::CreateFolder { path } => {
                let Some(s) = jmap_session(&account, &mut state, &emit).await else { continue };
                // "A/B" nests under A (resolved from the last listing);
                // otherwise a top-level mailbox.
                let (parent, name) = match path.rsplit_once('/') {
                    Some((parent, leaf)) => match state.folders.get(parent) {
                        Some((_, pid)) => (Some(pid.clone()), leaf.to_string()),
                        None => (None, path.clone()),
                    },
                    None => (None, path.clone()),
                };
                let sess = s.clone();
                let r = blocking(move || {
                    let r = jmap_call(
                        &sess,
                        &[CAP_CORE, CAP_MAIL],
                        vec![call(0, "Mailbox/set", serde_json::json!({
                            "accountId": sess.account,
                            "create": { "c": { "name": name, "parentId": parent } },
                        }))],
                    )?;
                    let r = args(&r, 0);
                    if r["created"]["c"].is_object() {
                        Ok(())
                    } else {
                        Err(r["notCreated"]["c"]["description"].as_str().unwrap_or("the server refused").to_string())
                    }
                })
                .await;
                match r {
                    Ok(()) => refresh_jmap_folders(&s, account_id, cache.as_ref(), &mut state, &emit).await,
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Could not create folder: {e}", &[("e", &e)]),
                        connectivity: false,
                    }),
                }
            }

            MailRequest::RenameFolder { old_path, new_path } => {
                let Some(s) = jmap_session(&account, &mut state, &emit).await else { continue };
                let Some((_, id)) = state.folders.get(&old_path).cloned() else {
                    emit(WorkerEvent::Error { text: i18n("Could not rename folder: unknown folder"), connectivity: false });
                    continue;
                };
                let leaf = new_path.rsplit('/').next().unwrap_or(&new_path).to_string();
                let sess = s.clone();
                let r = blocking(move || {
                    let r = jmap_call(
                        &sess,
                        &[CAP_CORE, CAP_MAIL],
                        vec![call(0, "Mailbox/set", serde_json::json!({
                            "accountId": sess.account,
                            "update": { id.clone(): { "name": leaf } },
                        }))],
                    )?;
                    let r = args(&r, 0);
                    if r["updated"].as_object().is_some_and(|m| m.contains_key(&id)) {
                        Ok(())
                    } else {
                        Err(r["notUpdated"][&id]["description"].as_str().unwrap_or("the server refused").to_string())
                    }
                })
                .await;
                match r {
                    Ok(()) => refresh_jmap_folders(&s, account_id, cache.as_ref(), &mut state, &emit).await,
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Could not rename folder: {e}", &[("e", &e)]),
                        connectivity: false,
                    }),
                }
            }

            MailRequest::SetHiddenFolders { paths } => {
                state.hidden = paths;
                let Some(s) = jmap_session(&account, &mut state, &emit).await else { continue };
                refresh_jmap_folders(&s, account_id, cache.as_ref(), &mut state, &emit).await;
            }

            MailRequest::DeleteFolder { path, trash } => {
                let Some(s) = jmap_session(&account, &mut state, &emit).await else { continue };
                let Some((_, id)) = state.folders.get(&path).cloned() else {
                    emit(WorkerEvent::Error { text: i18n("Could not delete folder: unknown folder"), connectivity: false });
                    continue;
                };
                // The contents go to Trash first when the app names one, as
                // on IMAP; then the mailbox goes, with whatever is left.
                let trash_id = trash.as_ref().and_then(|t| state.folders.get(t)).map(|(_, id)| id.clone());
                let sess = s.clone();
                let r = blocking(move || {
                    if let Some(trash_id) = trash_id {
                        let ids = jmap_all_ids_in(&sess, &id, serde_json::json!({}))?;
                        jmap_patch_all(&sess, &ids, &serde_json::json!({ "mailboxIds": { trash_id: true } }))?;
                    }
                    let r = jmap_call(
                        &sess,
                        &[CAP_CORE, CAP_MAIL],
                        vec![call(0, "Mailbox/set", serde_json::json!({
                            "accountId": sess.account,
                            "destroy": [id.clone()],
                            "onDestroyRemoveEmails": true,
                        }))],
                    )?;
                    let r = args(&r, 0);
                    if r["destroyed"].as_array().is_some_and(|a| a.iter().any(|v| v.as_str() == Some(&id))) {
                        Ok(())
                    } else {
                        Err(r["notDestroyed"][&id]["description"].as_str().unwrap_or("the server refused").to_string())
                    }
                })
                .await;
                match r {
                    Ok(()) => refresh_jmap_folders(&s, account_id, cache.as_ref(), &mut state, &emit).await,
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Could not delete folder: {e}", &[("e", &e)]),
                        connectivity: false,
                    }),
                }
            }

            MailRequest::SaveDraft { message, folder_id, path } => {
                emit(WorkerEvent::Status(i18n("Saving draft…")));
                let mut message = OutgoingMessage { sign: false, encrypt: false, ..*message };
                restore_msgid_case(cache.as_ref(), &mut message);
                let mut saved = false;
                match build_draft(&account, &message) {
                    Ok(email) => {
                        let raw = email.formatted();
                        if let Some(s) = jmap_session(&account, &mut state, &emit).await {
                            let mailbox_id = state.folders.get(&path).map(|(_, id)| id.clone());
                            match mailbox_id {
                                Some(mailbox_id) => {
                                    let sess = s.clone();
                                    match blocking(move || jmap_import_draft(&sess, &raw, &mailbox_id)).await {
                                        Ok(_) => {
                                            if let Some(o) = message.draft_origin.clone() {
                                                jmap_drop_draft_origin(&s, account_id, &o, cache.as_ref(), &mut state).await;
                                            }
                                            if let Ok(messages) = jmap_load_folder(&s, account_id, folder_id, &path, cache.as_ref(), &mut state).await {
                                                emit(WorkerEvent::Messages { folder_id, messages });
                                            }
                                            saved = true;
                                        }
                                        Err(e) => emit(WorkerEvent::Error {
                                            text: i18n_f("Could not save draft: {e}", &[("e", &e)]),
                                            connectivity: false,
                                        }),
                                    }
                                }
                                None => emit(WorkerEvent::Error {
                                    text: i18n("Could not save draft: unknown folder"),
                                    connectivity: false,
                                }),
                            }
                        }
                    }
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Could not save draft: {e}", &[("e", &e.to_string())]),
                        connectivity: false,
                    }),
                }
                emit(WorkerEvent::Status(String::new()));
                if saved {
                    emit(WorkerEvent::DraftSaved);
                }
            }

            // Send Later (#145): into the Outbox until its time.
            MailRequest::Send { mut message, sent_path }
                if message.send_at.is_some_and(|t| t > crate::datefmt::now()) =>
            {
                let at = message.send_at.unwrap_or_default();
                restore_msgid_case(cache.as_ref(), &mut message);
                schedule_send(cache.as_ref(), account_id, &account, &message, sent_path.as_deref(), at, &emit);
                if let Some(o) = message.draft_origin.clone() {
                    if let Some(s) = jmap_session(&account, &mut state, &emit).await {
                        jmap_drop_draft_origin(&s, account_id, &o, cache.as_ref(), &mut state).await;
                        if o.account_id == account_id {
                            if let Ok(messages) = jmap_load_folder(&s, account_id, o.folder_id, &o.path, cache.as_ref(), &mut state).await {
                                emit(WorkerEvent::Messages { folder_id: o.folder_id, messages });
                            }
                        }
                    }
                }
            }

            MailRequest::Send { mut message, sent_path } => {
                emit(WorkerEvent::Status(i18n("Sending…")));
                restore_msgid_case(cache.as_ref(), &mut message);
                let sent = match jmap_session(&account, &mut state, &emit).await {
                    Some(s) => {
                        jmap_send_message(&s, account_id, &account, &message, sent_path.as_deref(), cache.as_ref(), &mut state, &emit)
                            .await
                            .map(|()| s)
                    }
                    None => Err("could not reach the server".to_string()),
                };
                emit(WorkerEvent::Status(String::new()));
                match sent {
                    Ok(s) => {
                        if let Some(o) = message.draft_origin.clone() {
                            if o.account_id == account_id {
                                jmap_drop_draft_origin(&s, account_id, &o, cache.as_ref(), &mut state).await;
                                if let Ok(messages) = jmap_load_folder(&s, account_id, o.folder_id, &o.path, cache.as_ref(), &mut state).await {
                                    emit(WorkerEvent::Messages { folder_id: o.folder_id, messages });
                                }
                            }
                        }
                        if let (Some(queued), Some(c)) = (message.outbox_origin, cache.as_ref()) {
                            c.delete_outbox(queued);
                            emit_outbox(cache.as_ref(), account_id, &emit);
                        }
                        emit(WorkerEvent::Sent);
                    }
                    Err(e) => {
                        let queued = queue_failed_send(cache.as_ref(), account_id, &account, &message, sent_path.as_deref(), &e);
                        if let (true, Some(old), Some(c)) = (queued, message.outbox_origin, cache.as_ref()) {
                            c.delete_outbox(old);
                        }
                        emit(WorkerEvent::Error {
                            text: if queued {
                                i18n_f("Send failed: {e}. The message is in the Outbox and will be sent when the connection is back.", &[("e", &e)])
                            } else {
                                i18n_f("Send failed: {e}", &[("e", &e)])
                            },
                            connectivity: false,
                        });
                        emit_outbox(cache.as_ref(), account_id, &emit);
                    }
                }
            }

            MailRequest::LoadOutbox => emit_outbox(cache.as_ref(), account_id, &emit),

            MailRequest::DeleteOutbox { id } => {
                if let Some(c) = cache.as_ref() {
                    c.delete_outbox(id);
                }
                emit_outbox(cache.as_ref(), account_id, &emit);
            }

            MailRequest::FlushOutbox { id } => {
                if let Some(s) = jmap_session(&account, &mut state, &emit).await {
                    jmap_flush_outbox(&s, cache.as_ref(), account_id, &account, id, &mut state, &emit).await;
                }
            }

            MailRequest::RefreshUnread => {
                if let Some(s) = jmap_session(&account, &mut state, &|_| {}).await {
                    jmap_refresh_unread(&s, account_id, cache.as_ref(), &mut state, &emit).await;
                    auto_empty_jmap(&s, account_id, &account, cache.as_ref(), &mut state, &mut last_auto_empty, &emit).await;
                }
            }

            MailRequest::Reconnect => {
                // A fresh session resource: the server may have moved, or
                // the credentials changed.
                state.session = None;
                if let Some(s) = jmap_session(&account, &mut state, &emit).await {
                    refresh_jmap_folders(&s, account_id, cache.as_ref(), &mut state, &emit).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(host: &str, port: u16) -> AccountConfig {
        AccountConfig { imap_host: host.into(), imap_port: port, ..sample_account() }
    }

    #[test]
    fn base_url_from_host_or_url() {
        assert_eq!(jmap_base_url(&account("mail.example.org", 443)), "https://mail.example.org");
        assert_eq!(jmap_base_url(&account("mail.example.org", 0)), "https://mail.example.org");
        assert_eq!(jmap_base_url(&account("mail.example.org", 8443)), "https://mail.example.org:8443");
        assert_eq!(jmap_base_url(&account("http://127.0.0.1:8080/", 443)), "http://127.0.0.1:8080");
        assert_eq!(jmap_base_url(&account(" https://jmap.example.org ", 993)), "https://jmap.example.org");
    }

    #[test]
    fn origin_is_scheme_host_port() {
        assert_eq!(origin_of("https://mail.example.test/jmap/"), "https://mail.example.test");
        assert_eq!(origin_of("http://127.0.0.1:8080"), "http://127.0.0.1:8080");
        assert_eq!(origin_of("/jmap/"), "/jmap/");
    }

    #[test]
    fn template_slots_are_filled() {
        let t = "https://h/download/{accountId}/{blobId}/{name}?type={type}";
        assert_eq!(
            fill_template(t, &[("accountId", "a"), ("blobId", "b"), ("name", "m.eml"), ("type", "x")]),
            "https://h/download/a/b/m.eml?type=x"
        );
    }

    #[test]
    fn summary_maps_keywords_and_threading() {
        let v = serde_json::json!({
            "id": "M1", "blobId": "B1", "threadId": "T1",
            "receivedAt": "2026-09-21T10:00:00Z",
            "subject": "Hello", "preview": "  first   words ",
            "from": [{ "name": "Ada", "email": "ada@example.org" }],
            "replyTo": [{ "email": "ada@example.org" }],
            "to": [{ "email": "me@example.org" }, { "email": "you@example.org" }],
            "keywords": { "$seen": true, "$flagged": true, "Work": true, "$Junk": false },
            "hasAttachment": true,
            "messageId": ["<A@x>"], "inReplyTo": ["<B@x>"], "references": ["<C@x>", "<B@x>"],
        });
        let (m, id, blob) = jmap_message(&v, 7, 3).unwrap();
        assert_eq!((id.as_str(), blob.as_str()), ("M1", "B1"));
        assert_eq!(m.uid, hash_uid("M1"));
        assert!(!m.unread && m.starred && m.has_attachment);
        assert_eq!(m.keywords, vec!["Work"]);
        assert_eq!(m.reply_to, "", "a Reply-To equal to From is dropped");
        assert_eq!(m.to, "me@example.org, you@example.org");
        assert_eq!(m.message_id, "a@x");
        assert_eq!(m.references, "b@x c@x");
        assert_eq!(m.preview, "first words");
        assert_eq!(m.timestamp, 1_789_984_800);
    }

    /// The whole wire path against a real server: `JMAP_LIVE=url,user,pw`
    /// (a Stalwart container, say) and `cargo test jmap::tests::live -- --ignored --nocapture`.
    /// Lists, downloads, flags, moves, drafts and sends a message to the
    /// account itself, and leaves the mailbox as it found it, bar the Sent copy.
    #[test]
    #[ignore]
    fn live() {
        let Ok(spec) = std::env::var("JMAP_LIVE") else { return };
        let mut parts = spec.splitn(3, ',');
        let acc = AccountConfig {
            imap_host: parts.next().unwrap_or_default().into(),
            imap_port: 443,
            username: parts.next().unwrap_or_default().into(),
            password: parts.next().unwrap_or_default().into(),
            ..sample_account()
        };
        let s = jmap_connect(&acc).expect("session");
        println!("api {} download {} upload {} events {:?}", s.api_url, s.download_url, s.upload_url, s.event_source_url);
        let folders = jmap_list_folders(&s, 1, &BTreeMap::new()).expect("folders");
        for f in &folders {
            println!("folder {:?} {} kind={:?} unread={}", f.folder.path, f.mailbox_id, f.folder.kind, f.folder.unread);
        }
        let inbox = folders.iter().find(|f| f.folder.kind == FolderKind::Inbox).expect("inbox");
        let trash = folders.iter().find(|f| f.folder.kind == FolderKind::Trash).expect("trash");
        let drafts = folders.iter().find(|f| f.folder.kind == FolderKind::Drafts).expect("drafts");
        let sent = folders.iter().find(|f| f.folder.kind == FolderKind::Sent).expect("sent");
        let listed = jmap_list_messages(&s, &inbox.mailbox_id, 1, inbox.folder.id).expect("list");
        for (m, id, _) in &listed {
            println!("message uid={} id={id} unread={} {:?} <{}> refs={:?}", m.uid, m.unread, m.subject, m.message_id, m.references);
        }
        let (first, id, blob) = listed.first().cloned().expect("a message in the inbox");
        let raw = jmap_download(&s, &blob).expect("raw");
        assert!(raw.starts_with(b"From:") || raw.windows(6).any(|w| w == b"\r\nFrom:" || w == b"\nFrom: "), "raw RFC 5322");
        println!("raw {} bytes", raw.len());
        // Flags: seen on, then back off.
        jmap_set_emails(&s, serde_json::json!({ id.clone(): { "keywords/$seen": true } }), &[]).expect("seen");
        let again = jmap_get_messages(&s, &[id.clone()], 1, inbox.folder.id).expect("get");
        assert!(!again[0].0.unread, "marked seen");
        jmap_set_emails(&s, serde_json::json!({ id.clone(): { "keywords/$seen": null } }), &[]).expect("unseen");
        // Move to Trash and back.
        jmap_set_emails(&s, serde_json::json!({ id.clone(): { "mailboxIds": { trash.mailbox_id.clone(): true } } }), &[]).expect("move");
        assert!(jmap_list_messages(&s, &trash.mailbox_id, 1, 0).expect("trash").iter().any(|(_, i, _)| i == &id));
        jmap_set_emails(&s, serde_json::json!({ id.clone(): { "mailboxIds": { inbox.mailbox_id.clone(): true } } }), &[]).expect("move back");
        // Locate by Message-ID header.
        let found = jmap_query_ids(&s, serde_json::json!({ "header": ["Message-ID", first.message_id.clone()] }), 5).expect("locate");
        println!("header filter for {:?} found {found:?} (Stalwart: none; the cache answers first)", first.message_id);
        // Draft in, draft out.
        let draft = b"From: me <me@example.test>\r\nSubject: draft\r\n\r\nbody\r\n";
        let did = jmap_import_draft(&s, draft, &drafts.mailbox_id).expect("draft");
        assert!(jmap_list_messages(&s, &drafts.mailbox_id, 1, 0).expect("drafts").iter().any(|(_, i, _)| i == &did));
        jmap_destroy_all(&s, &[did]).expect("drop draft");
        // Send to ourselves: the Sent copy is the submitted message.
        let me = acc.username.clone();
        let msg = format!("From: {me}\r\nTo: {me}\r\nSubject: live test\r\nMessage-ID: <live-{}@hylki.test>\r\n\r\nsent over JMAP\r\n", crate::datefmt::now());
        let sid = jmap_submit(&s, msg.as_bytes(), &me, &[me.clone()], &sent.mailbox_id).expect("submit");
        assert!(jmap_list_messages(&s, &sent.mailbox_id, 1, 0).expect("sent").iter().any(|(_, i, _)| i == &sid));
        println!("submitted {sid}");
    }

    #[test]
    fn errors_name_the_call() {
        // A method-level error is an Err naming the call it answered.
        let responses = serde_json::json!([["error", { "type": "unknownMethod" }, "1"]]);
        let names = ["Mailbox/get".to_string(), "Email/query".to_string()];
        let resp = responses.as_array().unwrap();
        let tag = resp[0][2].as_str().unwrap();
        assert_eq!(names[tag.parse::<usize>().unwrap()], "Email/query");
    }
}
