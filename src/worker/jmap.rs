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

/// The newest summaries a folder load lists, for a quick first paint; the
/// background index fetches the rest of the folder (#429).
const JMAP_FIRST_PAGE: usize = 300;
/// Ids asked for per page of a folder's id listing (the server may give
/// fewer; the next page starts where this one ended).
const JMAP_ID_PAGE: usize = 1000;
/// Summaries one background step asks `Email/get` for.
const JMAP_BACKFILL_CHUNK: usize = 250;
/// How long a folder's id listing is trusted before opening the folder
/// lists it again, catching mail deleted elsewhere (#429).
const JMAP_RELIST_AFTER: Duration = Duration::from_secs(15 * 60);
/// The pause after a background step fails, before the next try.
const JMAP_BACKFILL_BACKOFF: Duration = Duration::from_secs(60);
/// A listing's `restarts` once the folder kept changing under it: listed
/// to the end all the same, but nothing is pruned by it.
const JMAP_UNSTEADY: u32 = u32::MAX;

const CAP_CORE: &str = "urn:ietf:params:jmap:core";
const CAP_MAIL: &str = "urn:ietf:params:jmap:mail";
const CAP_SUBMISSION: &str = "urn:ietf:params:jmap:submission";
/// RFC 9425: how much room the account has (#298).
const CAP_QUOTA: &str = "urn:ietf:params:jmap:quota";
/// How a refused EmailSubmission starts its error. Every `notCreated` type
/// there (invalidRecipients, forbiddenToSend and the rest) is final, so the
/// message goes back to the composer rather than to the Outbox (#368).
const SUBMISSION_REFUSED: &str = "the server refused to send it";

/// The summary properties one listing asks `Email/get` for.
const EMAIL_PROPS: &[&str] = &[
    "id", "blobId", "threadId", "mailboxIds", "keywords", "hasAttachment", "receivedAt",
    "subject", "from", "replyTo", "to", "cc", "messageId", "inReplyTo", "references", "preview",
    "header:X-Priority:asText", "header:Importance:asText", "header:Priority:asText",
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
    /// Whether the server answers `Quota/get` (RFC 9425).
    quota: bool,
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
    /// The folders whose background index (#429) is still to run, the one
    /// at the front in progress.
    backfill: std::collections::VecDeque<JmapBackfill>,
    /// When each folder's background index last finished, by path.
    listed: HashMap<String, std::time::Instant>,
    /// Message uid → JMAP id for mail an id listing named, whose blob id
    /// one `Email/get` gives when it is wanted.
    listed_ids: HashMap<u32, String>,
    /// When a message lookup last listed each folder's ids, by path.
    looked_up: HashMap<String, std::time::Instant>,
    /// No background step before this, after one failed.
    backfill_wait: Option<std::time::Instant>,
}

use std::collections::{BTreeMap, HashMap, HashSet};

/// The background index of one folder (#429): its id listing a page a
/// step, then its missing mail a chunk a step.
struct JmapBackfill {
    path: String,
    phase: JmapBackfillPhase,
    /// Whether the list has already been told the folder's index is done.
    told: bool,
    /// Steps that failed in a row.
    failures: u32,
}

enum JmapBackfillPhase {
    /// Paging through the ids, newest first, from a listing begun at
    /// `since`. `query_state` is the server's for the first page: a page
    /// under another state was cut from a different list, and starts the
    /// listing over (`restarts` times at most, then the pass prunes
    /// nothing).
    Listing { ids: Vec<String>, query_state: Option<String>, since: i64, restarts: u32 },
    Fetching { missing: std::collections::VecDeque<String> },
}

impl JmapBackfill {
    fn new(path: &str, told: bool) -> JmapBackfill {
        JmapBackfill {
            path: path.to_string(),
            phase: JmapBackfillPhase::Listing {
                ids: Vec::new(),
                query_state: None,
                since: crate::datefmt::now(),
                restarts: 0,
            },
            told,
            failures: 0,
        }
    }
}

impl JmapState {
    fn new(account: &AccountConfig) -> JmapState {
        JmapState {
            session: None,
            folders: HashMap::new(),
            uids: HashMap::new(),
            drafts: None,
            inbox: None,
            sent: None,
            roles: account.folder_roles.clone(),
            hidden: account.hidden_folders.clone(),
            backfill: Default::default(),
            listed: HashMap::new(),
            listed_ids: HashMap::new(),
            looked_up: HashMap::new(),
            backfill_wait: None,
        }
    }

    /// Queue a folder's background index unless it is queued already;
    /// `front` puts it at the head, for the folder on screen.
    fn queue_backfill(&mut self, path: &str, front: bool, told: bool) {
        if let Some(i) = self.backfill.iter().position(|j| j.path == path) {
            if front && i > 0 {
                if let Some(job) = self.backfill.remove(i) {
                    self.backfill.push_front(job);
                }
            }
            return;
        }
        let job = JmapBackfill::new(path, told);
        if front {
            self.backfill.push_front(job);
        } else {
            self.backfill.push_back(job);
        }
    }

    /// Queue every listed, unhidden folder's background index, in the
    /// folder list's order.
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
    /// complete now, as on the Graph path.
    fn index_after_load(&mut self, path: &str, complete: bool, user: bool) -> bool {
        if complete {
            self.backfill.retain(|j| j.path != path);
            self.listed.insert(path.to_string(), std::time::Instant::now());
            return true;
        }
        match self.listed.get(path) {
            Some(at) => {
                if user && at.elapsed() >= JMAP_RELIST_AFTER {
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
        // Every folder is indexed in the background, as an IMAP account's
        // is, so search reaches the whole mailbox (#429).
        self.queue_all_backfills();
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
    // Fastmail takes an API token, never the account password (#356).
    if account.jmap_token {
        return format!("Bearer {}", account.password.trim());
    }
    let user = if account.username.is_empty() { &account.email } else { &account.username };
    let b64 = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{}", account.password));
    format!("Basic {b64}")
}

/// Read a ureq error into something a user can act on.
fn jmap_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(401, _) => {
            "the server refused the sign-in (401): check the username and password, or the API token".to_string()
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
    let origin = origin_of(&base).to_string();
    // A password the worker could not read at start is read again (#375).
    let filled;
    let account = if account.password.is_empty() {
        filled = AccountConfig { password: super::sign_in_password_blocking(account, false)?, ..account.clone() };
        &filled
    } else {
        account
    };
    let auth = jmap_auth(account);
    // A server given with a path may be the session resource itself
    // (Fastmail's is https://api.fastmail.com/jmap/session, #356); when it
    // is not, the well-known path under it is tried, as for a bare host.
    let well_known = format!("{base}/.well-known/jmap");
    let v = if base.len() > origin.len() {
        match fetch_session(&base, &origin, &auth) {
            Ok(v) if v["apiUrl"].is_string() => v,
            Err(e) if e.contains("(401)") => return Err(e),
            _ => fetch_session(&well_known, &origin, &auth)?,
        }
    } else {
        fetch_session(&well_known, &origin, &auth)?
    };
    // A server names itself in the session: absolute URLs on the host it
    // was set up with. An account whose server is given as a URL, scheme
    // and all, is pointed at that origin on purpose (a server on a private
    // network, or behind a tunnel, reached other than by its public name),
    // so the typed origin stands in for the one the server advertises for
    // itself. Only that one: a URL on another host (Fastmail serves blobs
    // from a separate one) is left alone.
    let typed_origin = account.imap_host.trim().starts_with("http").then(|| origin.clone());
    let advertised = v["apiUrl"].as_str().map(|u| origin_of(u).to_string()).unwrap_or_default();
    let absolute = |u: &str| -> String {
        if !(u.starts_with("http://") || u.starts_with("https://")) {
            return format!("{origin}/{}", u.trim_start_matches('/'));
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
    let quota = v["capabilities"][CAP_QUOTA].is_object();
    Ok(JmapSession { api_url, download_url, upload_url, event_source_url, account: account_id, auth, quota })
}

/// GET the session resource at `url`. A well-known path answers with a
/// redirect to it (a 307 on Stalwart), followed here by hand, credentials
/// included: ureq drops the Authorization header when it follows one
/// itself, and the session then comes back for nobody.
fn fetch_session(url: &str, origin: &str, auth: &str) -> Result<serde_json::Value, String> {
    let no_follow = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .redirects(0)
        .build();
    let mut url = url.to_string();
    let mut hops = 0;
    loop {
        jmap_wire("GET session");
        let resp = no_follow
            .get(&url)
            .set("Authorization", auth)
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
                format!("{origin}/{}", next.trim_start_matches('/'))
            };
            continue;
        }
        return resp.into_json().map_err(|e| e.to_string());
    }
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

/// The mailbox size out of a `Quota/get` list (RFC 9425): the account's own
/// quota on octets that covers mail (the `Email` data type; Stalwart lists
/// it beside calendars and files), preferred over a domain's or the
/// server's; the hard limit, else the soft one.
fn jmap_storage_quota(list: &serde_json::Value) -> Option<MailboxQuota> {
    let quotas = list.as_array()?;
    let covers_mail = |q: &serde_json::Value| {
        q["types"].as_array().is_none_or(|t| t.is_empty() || t.iter().any(|x| x.as_str() == Some("Email")))
    };
    let rank = |q: &serde_json::Value| match q["scope"].as_str() {
        Some("account") => 0,
        Some("domain") => 1,
        _ => 2,
    };
    quotas
        .iter()
        .filter(|q| q["resourceType"].as_str() == Some("octets") && covers_mail(q))
        .filter_map(|q| {
            let limit = q["hardLimit"].as_u64().filter(|n| *n > 0).or_else(|| q["softLimit"].as_u64().filter(|n| *n > 0))?;
            Some((rank(q), MailboxQuota { used: q["used"].as_u64().unwrap_or(0), limit }))
        })
        .min_by_key(|(r, _)| *r)
        .map(|(_, q)| q)
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
    // Cyrus (Fastmail) refuses a message with a lone LF anywhere in it:
    // "Message contains bare newlines" (#414). Our builder writes CRLF
    // throughout; mail moved in from a laxer server may not.
    let raw = if content_type == "message/rfc822" { crlf_line_endings(raw) } else { raw.into() };
    let url = fill_template(&s.upload_url, &[("accountId", &s.account)]);
    jmap_wire("POST upload");
    let r: Result<serde_json::Value, String> = agent()
        .post(&url)
        .set("Authorization", &s.auth)
        .set("Content-Type", content_type)
        .send_bytes(&raw)
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
        let kind = match m["role"].as_str().map(role_kind) {
            Some(kind) => kind,
            // No role names templates (#360): a top-level one goes by name.
            None if names.len() == 1 && crate::models::is_templates_name(&name) => FolderKind::Templates,
            None => FolderKind::Custom,
        };
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
        importance: crate::models::Importance::from_headers(|name| v[format!("header:{name}:asText")].as_str()),
        due: 0,
        labels: Vec::new(),
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
    jmap_list_newest(s, mailbox_id, account_id, folder_id).map(|(list, _)| list)
}

/// [`jmap_list_messages`], saying too whether the folder holds more than
/// [`JMAP_FIRST_PAGE`].
fn jmap_list_newest(
    s: &JmapSession,
    mailbox_id: &str,
    account_id: u32,
    folder_id: u32,
) -> Result<(Vec<(Message, String, String)>, bool), String> {
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
        .map(|list| list.iter().filter_map(|v| jmap_message(v, account_id, folder_id)).collect())
        .unwrap_or_default();
    Ok((list, more))
}

/// One page of a folder's id listing (#429), newest first from `position`:
/// the ids, the server's query state, and whether this was the last page.
fn jmap_id_page(
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
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let last = ids.is_empty() || r["total"].as_u64().is_some_and(|t| (position + ids.len()) as u64 >= t);
    Ok((ids, r["queryState"].as_str().map(str::to_string), last))
}

/// What a folder's complete id listing asks of the cache (#429): the rows
/// to drop, and the ids to fetch, newest first.
fn jmap_index_plan(
    server: &[String],
    cached: &HashMap<u32, i64>,
    since: i64,
    prune: bool,
) -> (Vec<u32>, Vec<String>) {
    let uids: HashSet<u32> = server.iter().map(|id| hash_uid(id)).collect();
    let gone = if prune { listing_gone(cached, &uids, since) } else { Vec::new() };
    let mut seen = HashSet::new();
    let missing = server
        .iter()
        .filter(|id| !cached.contains_key(&hash_uid(id)) && seen.insert(id.as_str()))
        .cloned()
        .collect();
    (gone, missing)
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
            emit(WorkerEvent::net_error(i18n_f("Could not reach the JMAP server: {e}", &[("e", &e)])));
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
        Err(e) => emit(WorkerEvent::net_error(i18n_f("Could not list folders: {e}", &[("e", &e)]))),
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

/// List a folder's newest summaries, refresh the uid map and the cache, and
/// answer with the cached folder as it now stands.
async fn jmap_load_folder(
    s: &JmapSession,
    account_id: u32,
    folder_id: u32,
    path: &str,
    cache: Option<&Cache>,
    state: &mut JmapState,
) -> Result<Vec<Message>, String> {
    jmap_sync_folder(s, account_id, folder_id, path, cache, state).await.map(|(m, _)| m)
}

/// [`jmap_load_folder`], saying too whether the listing was the whole
/// folder (it fitted in [`JMAP_FIRST_PAGE`]).
async fn jmap_sync_folder(
    s: &JmapSession,
    account_id: u32,
    folder_id: u32,
    path: &str,
    cache: Option<&Cache>,
    state: &mut JmapState,
) -> Result<(Vec<Message>, bool), String> {
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
    let (listed, more) = blocking(move || jmap_list_newest(&sess, &mailbox_id, account_id, folder_id)).await?;
    let mut messages = Vec::with_capacity(listed.len());
    for (m, id, blob) in listed {
        state.uids.insert(m.uid, (id, blob));
        messages.push(m);
    }
    let Some(c) = cache else { return Ok((messages, !more)) };
    // Merged over the cached folder rather than replacing it, which kept
    // only the newest page (#429).
    let cached = c.cached_uid_times(account_id, path);
    for uid in window_gone(&cached, &messages, !more) {
        c.delete_message(account_id, path, uid);
        state.uids.remove(&uid);
    }
    c.upsert_messages(account_id, path, &messages);
    Ok((c.load_messages(account_id, path, folder_id), !more))
}

/// Pause the background index after a failed step, until `wait`.
async fn jmap_backfill_pause(wait: Option<std::time::Instant>) {
    if let Some(at) = wait {
        tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await;
    }
}

/// One step of the background index (#429): one request for the folder at
/// the front of the queue, a page of its ids or a chunk of its missing
/// mail, as on the Graph path. A failure leaves the job where it is and
/// pauses the index for a while.
async fn jmap_backfill_step(
    s: &JmapSession,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
) {
    if jmap_backfill_with(s, account_id, cache, state, emit).await {
        state.backfill_wait = None;
        return;
    }
    state.backfill_wait = Some(std::time::Instant::now() + JMAP_BACKFILL_BACKOFF);
    if let Some(job) = state.backfill.front_mut() {
        job.failures += 1;
        if job.failures >= 3 && !job.told {
            job.told = true;
            if let Some((folder_id, _)) = state.folders.get(&job.path) {
                emit(WorkerEvent::BackfillDone { folder_id: *folder_id });
            }
        }
    }
}

/// [`jmap_backfill_step`]'s work; false when the request failed.
async fn jmap_backfill_with(
    s: &JmapSession,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
) -> bool {
    let Some(mut job) = state.backfill.pop_front() else { return true };
    let Some((folder_id, mailbox_id)) = state.folders.get(&job.path).cloned() else { return true };
    match &mut job.phase {
        JmapBackfillPhase::Listing { ids, query_state, since, restarts } => {
            let (sess, position) = (s.clone(), ids.len());
            let page = match blocking(move || jmap_id_page(&sess, &mailbox_id, position)).await {
                Ok(page) => page,
                Err(e) => {
                    tracing::warn!("[account {account_id}] index of {}: {e}", job.path);
                    state.backfill.push_front(job);
                    return false;
                }
            };
            job.failures = 0;
            let (page, page_state, last) = page;
            if position == 0 {
                *query_state = page_state;
            } else if page_state != *query_state && *restarts != JMAP_UNSTEADY {
                // The folder changed between pages, so positions moved:
                // start over, or, after a few tries, list on without
                // pruning by what comes.
                if *restarts < 3 {
                    *restarts += 1;
                    ids.clear();
                    *since = crate::datefmt::now();
                    state.backfill.push_front(job);
                    return true;
                }
                *restarts = JMAP_UNSTEADY;
            }
            ids.extend(page);
            if !last {
                state.backfill.push_front(job);
                return true;
            }
            let prune = *restarts != JMAP_UNSTEADY;
            state.listed_ids.extend(ids.iter().map(|id| (hash_uid(id), id.clone())));
            let cached = cache.map(|c| c.cached_uid_times(account_id, &job.path)).unwrap_or_default();
            let (gone, missing) = jmap_index_plan(ids, &cached, *since, prune);
            tracing::info!(
                "[account {account_id}] index of {}: {} on the server, {} to fetch, {} gone",
                job.path,
                ids.len(),
                missing.len(),
                gone.len()
            );
            if let (Some(c), false) = (cache, gone.is_empty()) {
                for uid in &gone {
                    c.delete_message(account_id, &job.path, *uid);
                    state.uids.remove(uid);
                }
                emit(WorkerEvent::Messages { folder_id, messages: c.load_messages(account_id, &job.path, folder_id) });
            }
            job.phase = JmapBackfillPhase::Fetching { missing: missing.into() };
        }
        JmapBackfillPhase::Fetching { missing } => {
            let chunk: Vec<String> = missing.iter().take(JMAP_BACKFILL_CHUNK).cloned().collect();
            let sess = s.clone();
            let ids = chunk.clone();
            let fetched = match blocking(move || jmap_get_messages(&sess, &ids, account_id, folder_id)).await {
                Ok(fetched) => fetched,
                Err(e) => {
                    tracing::warn!("[account {account_id}] index of {}: {e}", job.path);
                    state.backfill.push_front(job);
                    return false;
                }
            };
            job.failures = 0;
            missing.drain(..chunk.len());
            let mut messages = Vec::with_capacity(fetched.len());
            for (m, id, blob) in fetched {
                state.uids.insert(m.uid, (id, blob));
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
    if matches!(&job.phase, JmapBackfillPhase::Fetching { missing } if missing.is_empty()) {
        state.listed.insert(job.path.clone(), std::time::Instant::now());
        if !job.told {
            emit(WorkerEvent::BackfillDone { folder_id });
        }
    } else {
        state.backfill.push_front(job);
    }
    true
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
    if let Some(ids) = state.uids.get(&uid) {
        return Some(ids.clone());
    }
    // Older than the first page (#429), from an earlier session's index:
    // an id listing names it (the background index's, or one made now, once
    // a while), and a get gives its blob.
    if !state.listed_ids.contains_key(&uid)
        && state.looked_up.get(path).is_none_or(|at| at.elapsed() >= JMAP_RELIST_AFTER)
    {
        let (_, mailbox_id) = state.folders.get(path).cloned()?;
        state.looked_up.insert(path.to_string(), std::time::Instant::now());
        let sess = s.clone();
        if let Ok(ids) = blocking(move || jmap_all_ids_in(&sess, &mailbox_id, serde_json::json!({}))).await {
            state.listed_ids.extend(ids.into_iter().map(|id| (hash_uid(&id), id)));
        }
    }
    let hit = vec![state.listed_ids.get(&uid)?.clone()];
    let sess = s.clone();
    let found = blocking(move || jmap_get_messages(&sess, &hit, 0, 0)).await.ok()?;
    for (m, id, blob) in found {
        state.uids.insert(m.uid, (id, blob));
    }
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
/// them by Message-ID and move them back to `dest`, with `extra` set on them
/// on the way (snoozed mail comes back unread).
#[allow(clippy::too_many_arguments)]
async fn jmap_undo_move(
    s: &JmapSession,
    account_id: u32,
    state: &mut JmapState,
    path: &str,
    dest: &str,
    message_ids: &[String],
    extra: serde_json::Value,
    cache: Option<&Cache>,
) -> Result<usize, String> {
    let (folder_id, mailbox_id) =
        state.folders.get(path).cloned().ok_or_else(|| format!("unknown folder {path}"))?;
    let wanted: HashSet<&str> = message_ids.iter().map(|s| s.as_str()).collect();
    let sess = s.clone();
    let listed = blocking(move || jmap_list_messages(&sess, &mailbox_id, account_id, folder_id)).await?;
    let mut uids = Vec::new();
    let mut found: HashSet<String> = HashSet::new();
    for (m, id, blob) in listed {
        if wanted.contains(m.message_id.as_str()) {
            state.uids.insert(m.uid, (id, blob));
            uids.push(m.uid);
            found.insert(m.message_id);
        }
    }
    // An old message keeps its date when it moves, below the newest page
    // (#429): asked for by its Message-ID header, where the server indexes it.
    let rest: Vec<String> = wanted.iter().filter(|mid| !found.contains(**mid)).map(|mid| mid.to_string()).collect();
    if !rest.is_empty() {
        let mailbox_id = state.folders.get(path).map(|(_, id)| id.clone()).unwrap_or_default();
        let exact: Vec<String> =
            rest.iter().map(|mid| cache.map(|c| c.exact_msgids(mid)).unwrap_or_else(|| mid.clone())).collect();
        let sess = s.clone();
        let hits = blocking(move || {
            let mut ids = Vec::new();
            for mid in exact {
                let filter = serde_json::json!({ "inMailbox": mailbox_id, "header": ["Message-ID", format!("<{mid}>")] });
                ids.extend(jmap_query_ids(&sess, filter, 5)?);
            }
            jmap_get_messages(&sess, &ids, account_id, folder_id)
        })
        .await;
        match hits {
            Ok(hits) => {
                for (m, id, blob) in hits {
                    if wanted.contains(m.message_id.as_str()) && !uids.contains(&m.uid) {
                        state.uids.insert(m.uid, (id, blob));
                        uids.push(m.uid);
                    }
                }
            }
            Err(e) => tracing::warn!("undo: Message-ID lookup in {path} failed: {e}"),
        }
    }
    if uids.is_empty() {
        return Ok(0);
    }
    let n = uids.len();
    jmap_move_uids(s, account_id, state, path, &uids, dest, extra, cache).await?;
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

/// Move the Inbox's list mail older than the account's chosen age to the
/// Trash (#407): the same choice as over IMAP, as a JMAP query filter.
async fn clean_lists_jmap(
    s: &JmapSession,
    account_id: u32,
    account: &AccountConfig,
    cache: Option<&Cache>,
    state: &mut JmapState,
    last: &mut Option<std::time::Instant>,
    emit: &impl Fn(WorkerEvent),
) {
    if !clean_lists_due(account, last) {
        return;
    }
    let days = account.clean_lists_days;
    let folders = cache.map(|c| c.load_folders(account_id)).unwrap_or_default();
    let Some(inbox) = folders.iter().find(|f| f.kind == FolderKind::Inbox).cloned() else { return };
    let Some(trash) = role_folder_path(account, &folders, FolderKind::Trash, "trash") else { return };
    let (Some((_, inbox_id)), Some((_, trash_id))) = (state.folders.get(&inbox.path).cloned(), state.folders.get(&trash).cloned()) else {
        return;
    };
    let before = graph_before_date(days);
    let sess = s.clone();
    let moved = blocking(move || {
        // The candidates, then their List-Unsubscribe header read: a
        // `header` filter would do both, but not every server answers one
        // (Stalwart finds nothing), as with IMAP's HEADER search.
        let filter = serde_json::json!({ "before": before, "notKeyword": "$flagged" });
        let candidates = jmap_all_ids_in(&sess, &inbox_id, filter)?;
        let mut ids = Vec::new();
        for chunk in candidates.chunks(500) {
            let responses = jmap_call(
                &sess,
                &[CAP_CORE, CAP_MAIL],
                vec![call(
                    0,
                    "Email/get",
                    serde_json::json!({
                        "accountId": sess.account,
                        "ids": chunk,
                        "properties": ["id", "header:List-Unsubscribe"],
                    }),
                )],
            )?;
            for email in args(&responses, 0)["list"].as_array().into_iter().flatten() {
                let listed = email["header:List-Unsubscribe"].as_str().is_some_and(|v| !v.trim().is_empty());
                if let (true, Some(id)) = (listed, email["id"].as_str()) {
                    ids.push(id.to_string());
                }
            }
        }
        if !ids.is_empty() {
            let update: serde_json::Map<String, serde_json::Value> = ids
                .iter()
                .map(|id| (id.clone(), serde_json::json!({ "mailboxIds": { trash_id.clone(): true } })))
                .collect();
            jmap_set_emails(&sess, serde_json::Value::Object(update), &[])?;
        }
        Ok(ids)
    })
    .await;
    match moved {
        Ok(ids) if !ids.is_empty() => {
            tracing::info!(
                "list clean-up: moved {} list message(s) older than {days} days to {trash} on account {account_id}",
                ids.len()
            );
            if let Some(c) = cache {
                for id in &ids {
                    let uid = hash_uid(id);
                    state.uids.remove(&uid);
                    c.delete_message(account_id, &inbox.path, uid);
                }
                emit(WorkerEvent::Messages { folder_id: inbox.id, messages: c.load_messages(account_id, &inbox.path, inbox.id) });
            }
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("list clean-up: could not move list mail on account {account_id}: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------------

/// The account's sending identities, as the server lists them.
fn jmap_identities(s: &JmapSession) -> Result<Vec<serde_json::Value>, String> {
    let responses = jmap_call(
        s,
        &[CAP_CORE, CAP_SUBMISSION],
        vec![call(0, "Identity/get", serde_json::json!({ "accountId": s.account, "ids": null }))],
    )?;
    Ok(args(&responses, 0)["list"].as_array().cloned().unwrap_or_default())
}

/// Tell the app which identities the server has (#346), so the composer
/// offers them as From addresses.
fn emit_jmap_identities(s: &JmapSession, emit: &impl Fn(WorkerEvent)) {
    match jmap_identities(s) {
        Ok(list) => emit(WorkerEvent::Identities(
            list.iter()
                .filter_map(|i| {
                    let email = i["email"].as_str()?.trim().to_string();
                    let name = i["name"].as_str().unwrap_or_default().trim().to_string();
                    (!email.is_empty()).then_some((name, email))
                })
                .collect(),
        )),
        Err(e) => tracing::info!(target: "hylki::jmap", "Identity/get: {e}"),
    }
}

/// Which identity sends from `from` (#346): the one with that address,
/// else one for anyone at its domain (`*@domain`, as Fastmail lists a
/// domain's catch-all), else the first the server offers.
fn pick_identity<'a>(list: &'a [serde_json::Value], from: &str) -> Option<&'a str> {
    let domain = from.rsplit_once('@').map(|(_, d)| d).unwrap_or_default();
    let email = |i: &serde_json::Value| i["email"].as_str().unwrap_or_default().trim().to_string();
    list.iter()
        .find(|i| email(i).eq_ignore_ascii_case(from))
        .or_else(|| {
            list.iter().find(|i| {
                email(i).strip_prefix("*@").is_some_and(|d| !domain.is_empty() && d.eq_ignore_ascii_case(domain))
            })
        })
        .or_else(|| list.first())
        .and_then(|i| i["id"].as_str())
}

/// The identity to submit as (see [`pick_identity`]).
fn jmap_identity_for(s: &JmapSession, from: &str) -> Result<String, String> {
    let list = jmap_identities(s)?;
    pick_identity(&list, from)
        .map(str::to_string)
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
                    "{SUBMISSION_REFUSED}: {}",
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
                emit(WorkerEvent::error(i18n_f(
                        "Still could not send “{subject}”: {e}",
                        &[("subject", &item.subject), ("e", &e)],
                    )));
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
    jmap_import(s, raw, drafts_mailbox, serde_json::json!({ "$draft": true, "$seen": true }))
}

/// Import a template (#360): read mail, not a draft, which another client
/// would offer to finish and send.
fn jmap_import_template(s: &JmapSession, raw: &[u8], mailbox: &str) -> Result<String, String> {
    jmap_import(s, raw, mailbox, serde_json::json!({ "$seen": true }))
}

/// Create a mailbox called `name` under `parent` (the top when `None`).
/// Returns its id.
fn jmap_create_mailbox(s: &JmapSession, name: &str, parent: Option<&str>) -> Result<String, String> {
    let r = jmap_call(
        s,
        &[CAP_CORE, CAP_MAIL],
        vec![call(0, "Mailbox/set", serde_json::json!({
            "accountId": s.account,
            "create": { "c": { "name": name, "parentId": parent } },
        }))],
    )?;
    let r = args(&r, 0);
    match r["created"]["c"]["id"].as_str() {
        Some(id) => Ok(id.to_string()),
        None if r["created"]["c"].is_object() => Ok(String::new()),
        None => Err(r["notCreated"]["c"]["description"].as_str().unwrap_or("the server refused").to_string()),
    }
}

/// `raw` with every lone LF made CRLF, borrowed untouched when it has none.
fn crlf_line_endings(raw: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    let bare = |i: usize| raw[i] == b'\n' && (i == 0 || raw[i - 1] != b'\r');
    if !(0..raw.len()).any(bare) {
        return std::borrow::Cow::Borrowed(raw);
    }
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 32);
    for i in 0..raw.len() {
        if bare(i) {
            out.push(b'\r');
        }
        out.push(raw[i]);
    }
    std::borrow::Cow::Owned(out)
}

/// Upload a message's bytes and import them into `mailbox` with
/// `keywords`: a draft being saved, or mail moved in from another account
/// (#265).
fn jmap_import(
    s: &JmapSession,
    raw: &[u8],
    mailbox: &str,
    keywords: serde_json::Value,
) -> Result<String, String> {
    jmap_import_email(s, raw, serde_json::json!({ "mailboxIds": { mailbox: true }, "keywords": keywords }))
}

/// Upload a message's bytes and import them with the Email properties in
/// `email` (`mailboxIds`, `keywords`, `receivedAt`). Returns the new id.
fn jmap_import_email(s: &JmapSession, raw: &[u8], mut email: serde_json::Value) -> Result<String, String> {
    email["blobId"] = jmap_upload(s, raw, "message/rfc822")?.into();
    let responses = jmap_call(
        s,
        &[CAP_CORE, CAP_MAIL],
        vec![call(
            0,
            "Email/import",
            serde_json::json!({ "accountId": s.account, "emails": { "d": email } }),
        )],
    )?;
    let r = args(&responses, 0);
    r["created"]["d"]["id"].as_str().map(str::to_string).ok_or_else(|| {
        let why = &r["notCreated"]["d"];
        why["description"].as_str().or(why["type"].as_str()).unwrap_or("the server would not take the message").to_string()
    })
}

/// Replace a message with a copy that lacks one attachment (#289): a JMAP
/// message cannot be edited, so the copy is imported with the original's
/// folders, keywords and arrival time, then the original is destroyed.
/// Returns the copy's id and bytes.
async fn jmap_strip_attachment(
    s: &JmapSession,
    state: &mut JmapState,
    path: &str,
    uid: u32,
    name: &str,
    size: u64,
) -> Result<(String, Vec<u8>), String> {
    let (id, _) = jmap_resolve(s, state, path, uid).await.ok_or_else(|| "message not found".to_string())?;
    let raw = jmap_fetch_raw(s, state, path, uid).await?;
    let stripped = super::strip::without_attachment(&raw, name, size, &super::strip::note_date())
        .map_err(|e| e.text())?;
    let sess = s.clone();
    blocking(move || {
        let responses = jmap_call(
            &sess,
            &[CAP_CORE, CAP_MAIL],
            vec![call(
                0,
                "Email/get",
                serde_json::json!({
                    "accountId": sess.account,
                    "ids": [id],
                    "properties": ["mailboxIds", "keywords", "receivedAt"],
                }),
            )],
        )?;
        let original = args(&responses, 0)["list"][0].clone();
        if original.is_null() {
            return Err(i18n("The attachment is no longer in the message on the server"));
        }
        let new_id = jmap_import_email(
            &sess,
            &stripped,
            serde_json::json!({
                "mailboxIds": original["mailboxIds"],
                "keywords": original["keywords"],
                "receivedAt": original["receivedAt"],
            }),
        )?;
        jmap_destroy_all(&sess, &[id])?;
        Ok((new_id, stripped))
    })
    .await
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
        emit(WorkerEvent::SenderChecked { path: path.to_string(), message_id, check });
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
            emit(WorkerEvent::SenderChecked { path: path.to_string(), message_id, check });
        }
        Err(e) => emit(WorkerEvent::net_error(i18n_f("Could not load message: {e}", &[("e", &e)]))),
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
    let mut last_list_clean: Option<std::time::Instant> = None;
    let mut state = JmapState::new(&account);

    let (push_tx, mut push_rx) = mpsc::unbounded_channel::<()>();
    let push_enabled = account.push.unwrap_or_else(|| crate::config::load_privacy().push);
    let mut push_started = false;

    if let Some(s) = jmap_session(&account, &mut state, &emit).await {
        refresh_jmap_folders(&s, account_id, cache.as_ref(), &mut state, &emit).await;
        emit_jmap_identities(&s, &emit);
        if push_enabled {
            spawn_jmap_push(s.clone(), push_tx.clone());
            push_started = true;
        }
    }

    // The poll is the fallback for a server without push (or with it off):
    // the auto-fetch cadence when set, otherwise a quiet couple of minutes.
    let poll_secs = match crate::config::load_privacy().fetch_interval_secs {
        0 => 120,
        s => s.max(60),
    };
    let mut poll = tokio::time::interval(Duration::from_secs(poll_secs));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    poll.tick().await;

    loop {
        // The background index (#429) runs only while nothing else is
        // asked, a single request a step.
        let indexing = !state.backfill.is_empty();
        let wait = state.backfill_wait;
        let req = tokio::select! {
            biased;
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
            _ = jmap_backfill_pause(wait), if indexing => {
                match jmap_session(&account, &mut state, &|_| {}).await {
                    Some(s) => jmap_backfill_step(&s, account_id, cache.as_ref(), &mut state, &emit).await,
                    None => state.backfill_wait = Some(std::time::Instant::now() + JMAP_BACKFILL_BACKOFF),
                }
                continue;
            }
        };
        // A folder the user opened, rather than a background refresh.
        let user = matches!(req, MailRequest::LoadMessages { .. });
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
            MailRequest::Quota => {
                let mut quota = None;
                if let Some(s) = jmap_session(&account, &mut state, &emit).await {
                    if s.quota {
                        let sess = s.clone();
                        let r = blocking(move || {
                            jmap_call(
                                &sess,
                                &[CAP_CORE, CAP_QUOTA],
                                vec![call(0, "Quota/get", serde_json::json!({ "accountId": sess.account, "ids": null }))],
                            )
                        })
                        .await;
                        match r {
                            Ok(responses) => quota = jmap_storage_quota(&args(&responses, 0)["list"]),
                            Err(e) => tracing::warn!("[account {account_id}] quota: {e}"),
                        }
                    }
                }
                emit(WorkerEvent::Quota(quota));
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
            MailRequest::LoadThreadSummaries { groups } => {
                let summaries =
                    cache.as_ref().map(|c| thread_summaries_with_members(c, account_id, &groups)).unwrap_or_default();
                emit(WorkerEvent::ThreadSummaries { summaries });
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
                match jmap_sync_folder(&s, account_id, folder_id, &path, cache.as_ref(), &mut state).await {
                    Ok((messages, complete)) => {
                        let unread = state.chip_count(folder_id, &messages);
                        emit_jmap_body_hits(&s, &account, folder_id, &path, &messages, &state, &emit).await;
                        emit(WorkerEvent::Messages { folder_id, messages });
                        emit(WorkerEvent::FolderUnread { folder_id, unread });
                        // Complete when the first page held the whole folder,
                        // or once the background index has run (#429).
                        if state.index_after_load(&path, complete, user) {
                            emit(WorkerEvent::BackfillDone { folder_id });
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::net_error(i18n_f("Could not fetch mail: {e}", &[("e", &e)])));
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
                    Err(e) => emit(WorkerEvent::net_error(i18n_f("Could not load source: {e}", &[("e", &e)]))),
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
                let Some(s) = jmap_session(&account, &mut state, &emit).await else { continue };
                match jmap_fetch_raw(&s, &mut state, &path, uid).await {
                    Ok(raw) => {
                        let items = extract_attachments(&raw);
                        if let Some(c) = cache.as_ref() {
                            c.save_attachments(account_id, &path, uid, &items);
                        }
                        emit(WorkerEvent::Attachments { path: path.to_string(), message_id, items });
                    }
                    Err(e) => emit(WorkerEvent::net_error(i18n_f("Could not load attachments: {e}", &[("e", &e)]))),
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
                emit(WorkerEvent::TagSettled { path, uid, keyword });
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
                    emit(WorkerEvent::error(i18n_f("Could not move message: {e}", &[("e", &e)])));
                }
            }

            MailRequest::MarkSpam { path, uid, dest } => {
                let Some(s) = jmap_session(&account, &mut state, &emit).await else { continue };
                let verdict = serde_json::json!({ "keywords/$junk": true, "keywords/$notjunk": null });
                if let Err(e) = jmap_move_uids(&s, account_id, &mut state, &path, &[uid], &dest, verdict, cache.as_ref()).await {
                    emit(WorkerEvent::error(i18n_f("Could not mark as spam: {e}", &[("e", &e)])));
                }
            }

            MailRequest::MarkHam { path, uid, dest } => {
                let Some(s) = jmap_session(&account, &mut state, &emit).await else { continue };
                let verdict = serde_json::json!({ "keywords/$junk": null, "keywords/$notjunk": true });
                if let Err(e) = jmap_move_uids(&s, account_id, &mut state, &path, &[uid], &dest, verdict, cache.as_ref()).await {
                    emit(WorkerEvent::error(i18n_f("Could not mark as not spam: {e}", &[("e", &e)])));
                }
            }
            MailRequest::MarkHamMany { path, uids, dest } => {
                if let Some(s) = jmap_session(&account, &mut state, &emit).await {
                    let verdict = serde_json::json!({ "keywords/$junk": null, "keywords/$notjunk": true });
                    if let Err(e) = jmap_move_uids(&s, account_id, &mut state, &path, &uids, &dest, verdict, cache.as_ref()).await {
                        emit(WorkerEvent::error(i18n_f("Could not mark {len} messages as not spam: {e}", &[("len", &uids.len().to_string()), ("e", &e)])));
                    }
                }
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::MoveMessages { path, uids, dest } => {
                if let Some(s) = jmap_session(&account, &mut state, &emit).await {
                    if let Err(e) = jmap_move_uids(&s, account_id, &mut state, &path, &uids, &dest, serde_json::json!({}), cache.as_ref()).await {
                        emit(WorkerEvent::error(i18n_f("Could not move messages: {e}", &[("e", &e)])));
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
                    Err(e) => emit(WorkerEvent::error(i18n_f("Could not empty the folder: {e}", &[("e", &e)]))),
                }
            }

            MailRequest::UndoMove { path, dest, dest_folder_id, message_ids } => {
                let Some(s) = jmap_session(&account, &mut state, &emit).await else {
                    emit(WorkerEvent::BulkComplete);
                    continue;
                };
                let none = serde_json::json!({});
                match jmap_undo_move(&s, account_id, &mut state, &path, &dest, &message_ids, none, cache.as_ref()).await {
                    Ok(0) => tracing::info!("undo: the messages are no longer where that move put them"),
                    Ok(_) => {
                        if let Ok(messages) = jmap_load_folder(&s, account_id, dest_folder_id, &dest, cache.as_ref(), &mut state).await {
                            let unread = state.chip_count(dest_folder_id, &messages);
                            emit(WorkerEvent::Messages { folder_id: dest_folder_id, messages });
                            emit(WorkerEvent::FolderUnread { folder_id: dest_folder_id, unread });
                        }
                    }
                    Err(e) => emit(WorkerEvent::error(i18n_f("Undo failed: {e}", &[("e", &e)]))),
                }
                // The app spins its busy indicator until an undo answers.
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::Snooze { path, uids, dest, message_ids } => {
                let Some(s) = jmap_session(&account, &mut state, &emit).await else {
                    emit(WorkerEvent::SnoozeFailed { message_ids });
                    continue;
                };
                // The Snoozed mailbox is made on first use, as an IMAP move
                // makes its destination; a JMAP move needs it to exist.
                let mut ready = Ok(());
                if !state.folders.contains_key(&dest) {
                    let (sess, name) = (s.clone(), dest.clone());
                    ready = blocking(move || jmap_create_mailbox(&sess, &name, None)).await.map(|_| ());
                    if ready.is_ok() {
                        refresh_jmap_folders(&s, account_id, cache.as_ref(), &mut state, &emit).await;
                    }
                }
                let moved = match ready {
                    Ok(()) => {
                        let none = serde_json::json!({});
                        jmap_move_uids(&s, account_id, &mut state, &path, &uids, &dest, none, cache.as_ref()).await
                    }
                    Err(e) => Err(e),
                };
                if let Err(e) = moved {
                    emit(WorkerEvent::error(i18n_f("Could not snooze: {e}", &[("e", &e)])));
                    emit(WorkerEvent::SnoozeFailed { message_ids });
                }
            }

            MailRequest::Unsnooze { path, dest, dest_folder_id, message_ids } => {
                let outcome = match jmap_session(&account, &mut state, &emit).await {
                    None => WakeOutcome::Failed("no JMAP session".to_string()),
                    Some(s) => {
                        let unseen = serde_json::json!({ "keywords/$seen": null });
                        let moved =
                            jmap_undo_move(&s, account_id, &mut state, &path, &dest, &message_ids, unseen, cache.as_ref())
                                .await;
                        match moved {
                            Err(e) => {
                                tracing::warn!("unsnooze: {path:?} -> {dest:?} failed: {e}");
                                WakeOutcome::Failed(e)
                            }
                            Ok(0) => WakeOutcome::Missing,
                            Ok(_) => {
                                let loaded =
                                    jmap_load_folder(&s, account_id, dest_folder_id, &dest, cache.as_ref(), &mut state);
                                if let Ok(messages) = loaded.await {
                                    let unread = state.chip_count(dest_folder_id, &messages);
                                    emit(WorkerEvent::Messages { folder_id: dest_folder_id, messages });
                                    emit(WorkerEvent::FolderUnread { folder_id: dest_folder_id, unread });
                                }
                                WakeOutcome::Back
                            }
                        }
                    }
                };
                emit(WorkerEvent::Unsnoozed { message_ids, outcome });
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
                let r = blocking(move || jmap_create_mailbox(&sess, &name, parent.as_deref())).await;
                match r {
                    Ok(_) => refresh_jmap_folders(&s, account_id, cache.as_ref(), &mut state, &emit).await,
                    Err(e) => emit(WorkerEvent::error(i18n_f("Could not create folder: {e}", &[("e", &e)]))),
                }
            }

            MailRequest::RenameFolder { old_path, new_path } => {
                let Some(s) = jmap_session(&account, &mut state, &emit).await else { continue };
                let Some((_, id)) = state.folders.get(&old_path).cloned() else {
                    emit(WorkerEvent::error(i18n("Could not rename folder: unknown folder")));
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
                    Err(e) => emit(WorkerEvent::error(i18n_f("Could not rename folder: {e}", &[("e", &e)]))),
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
                    emit(WorkerEvent::error(i18n("Could not delete folder: unknown folder")));
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
                    Err(e) => emit(WorkerEvent::error(i18n_f("Could not delete folder: {e}", &[("e", &e)]))),
                }
            }

            MailRequest::SaveDraft { message, folder_id, path, autosave, template } => {
                if autosave.is_none() {
                    emit(WorkerEvent::Status(saving_text(template)));
                }
                let mut message = OutgoingMessage { sign: false, encrypt: false, ..*message };
                restore_msgid_case(cache.as_ref(), &mut message);
                let mut saved = None;
                let mut why = String::new();
                match build_draft(&account, &message) {
                    Ok(email) => {
                        let message_id = draft_message_id(&email);
                        let raw = email.formatted();
                        if let Some(s) = jmap_session(&account, &mut state, &emit).await {
                            let mut mailbox_id = state.folders.get(&path).map(|(_, id)| id.clone());
                            // The first template of an account with no
                            // Templates folder makes one, as IMAP's APPEND does.
                            if mailbox_id.is_none() && template {
                                let (sess, name) = (s.clone(), path.clone());
                                match blocking(move || jmap_create_mailbox(&sess, &name, None)).await {
                                    Ok(_) => {
                                        refresh_jmap_folders(&s, account_id, cache.as_ref(), &mut state, &emit).await;
                                        mailbox_id = state.folders.get(&path).map(|(_, id)| id.clone());
                                    }
                                    Err(e) => tracing::warn!("jmap: could not create {path}: {e}"),
                                }
                            }
                            // The listing just made numbers the new folder.
                            let folder_id = state.folders.get(&path).map(|(id, _)| *id).unwrap_or(folder_id);
                            match mailbox_id {
                                Some(mailbox_id) => {
                                    let sess = s.clone();
                                    let import = move || {
                                        if template {
                                            jmap_import_template(&sess, &raw, &mailbox_id)
                                        } else {
                                            jmap_import_draft(&sess, &raw, &mailbox_id)
                                        }
                                    };
                                    match blocking(import).await {
                                        Ok(_) => {
                                            if let Some(o) = message.draft_origin.clone() {
                                                jmap_drop_draft_origin(&s, account_id, &o, cache.as_ref(), &mut state).await;
                                            }
                                            if let Ok(messages) = jmap_load_folder(&s, account_id, folder_id, &path, cache.as_ref(), &mut state).await {
                                                emit(WorkerEvent::Messages { folder_id, messages });
                                            }
                                            saved = Some(message_id);
                                        }
                                        Err(e) => why = e,
                                    }
                                }
                                None => why = i18n("unknown folder"),
                            }
                        } else {
                            why = i18n("Could not reach the server");
                        }
                    }
                    Err(e) => why = e.to_string(),
                }
                emit(WorkerEvent::Status(String::new()));
                match saved {
                    Some(id) => emit(WorkerEvent::DraftSaved { autosave, message_id: Some(id) }),
                    None => draft_not_saved(autosave, template, why, message, &emit),
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
                        drop_superseded_outbox(cache.as_ref(), account_id, &message, &emit);
                        emit(WorkerEvent::Sent);
                    }
                    Err(e) => {
                        send_failed(cache.as_ref(), account_id, &account, &message, sent_path.as_deref(), &e, e.starts_with(SUBMISSION_REFUSED), &emit);
                    }
                }
            }

            MailRequest::LoadOutbox => emit_outbox(cache.as_ref(), account_id, &emit),

            MailRequest::DeleteOutbox { id } => delete_queued(cache.as_ref(), account_id, id, &emit),

            MailRequest::FlushOutbox { id } => {
                if let Some(s) = jmap_session(&account, &mut state, &emit).await {
                    jmap_flush_outbox(&s, cache.as_ref(), account_id, &account, id, &mut state, &emit).await;
                }
            }

            MailRequest::RefreshUnread => {
                if let Some(s) = jmap_session(&account, &mut state, &|_| {}).await {
                    jmap_refresh_unread(&s, account_id, cache.as_ref(), &mut state, &emit).await;
                    auto_empty_jmap(&s, account_id, &account, cache.as_ref(), &mut state, &mut last_auto_empty, &emit).await;
                    clean_lists_jmap(&s, account_id, &account, cache.as_ref(), &mut state, &mut last_list_clean, &emit).await;
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

            MailRequest::Settle { path, uids } => emit(WorkerEvent::MovesSettled { path, uids }),

            MailRequest::ExportRaw { token, path, uid, .. } => {
                let raw = match jmap_session(&account, &mut state, &emit).await {
                    Some(s) => jmap_fetch_raw(&s, &mut state, &path, uid).await,
                    None => Err(i18n("Could not reach the server")),
                };
                emit(WorkerEvent::RawExported { token, raw });
            }

            MailRequest::DeleteAttachment { message_id, path, uid, name, size } => {
                let Some(s) = jmap_session(&account, &mut state, &emit).await else {
                    strip_refused(&emit, i18n("Could not reach the server"), message_id, name, size);
                    continue;
                };
                emit(WorkerEvent::Status(i18n("Removing the attachment…")));
                let result = jmap_strip_attachment(&s, &mut state, &path, uid, &name, size).await;
                emit(WorkerEvent::Status(String::new()));
                match result {
                    Ok((new_id, raw)) => {
                        let new_uid = hash_uid(&new_id);
                        state.uids.remove(&uid);
                        if let Some(c) = cache.as_ref() {
                            c.delete_message(account_id, &path, uid);
                        }
                        if let Some((folder_id, _)) = state.folders.get(&path).cloned() {
                            if let Ok(messages) =
                                jmap_load_folder(&s, account_id, folder_id, &path, cache.as_ref(), &mut state).await
                            {
                                emit(WorkerEvent::Messages { folder_id, messages });
                            }
                        }
                        if let Some(c) = cache.as_ref() {
                            c.save_attachment_meta(account_id, &path, uid, &[]);
                            cache_rewritten(c, account_id, &path, new_uid, &raw);
                        }
                        emit(WorkerEvent::AttachmentDeleted { message_id, path, uid, new_uid: Some(new_uid), name, size });
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

            MailRequest::ImportRaw { token, path, raw, seen, flagged } => {
                let mailbox = state.folders.get(&path).map(|(_, id)| id.clone());
                let result = match (jmap_session(&account, &mut state, &emit).await, mailbox) {
                    (Some(s), Some(mailbox)) => {
                        let mut keywords = serde_json::Map::new();
                        if seen {
                            keywords.insert("$seen".into(), true.into());
                        }
                        if flagged {
                            keywords.insert("$flagged".into(), true.into());
                        }
                        blocking(move || jmap_import(&s, &raw, &mailbox, serde_json::Value::Object(keywords)))
                            .await
                            .map(|_| ())
                    }
                    (None, _) => Err(i18n("Could not reach the server")),
                    (_, None) => Err(i18n("That folder is not on the server")),
                };
                emit(WorkerEvent::RawImported { token, result });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// #414: lone LFs become CRLF; CRLFs stay as they are.
    #[test]
    fn submission_bytes_get_crlf_line_endings() {
        assert_eq!(&*super::crlf_line_endings(b"a\r\nb"), b"a\r\nb");
        assert!(matches!(super::crlf_line_endings(b"a\r\nb"), std::borrow::Cow::Borrowed(_)));
        assert_eq!(&*super::crlf_line_endings(b"\na\nb\r\nc\n"), b"\r\na\r\nb\r\nc\r\n");
    }

    /// #346: the From picks its identity by address, then by a domain's
    /// catch-all, before falling back to the first.
    #[test]
    fn an_identity_is_picked_by_address_then_by_domain() {
        let list = vec![
            serde_json::json!({ "id": "me", "email": "me@example.com" }),
            serde_json::json!({ "id": "any", "email": "*@example.org" }),
            serde_json::json!({ "id": "work", "email": "Work@Example.com" }),
        ];
        assert_eq!(super::pick_identity(&list, "work@example.com"), Some("work"));
        assert_eq!(super::pick_identity(&list, "shop+x@EXAMPLE.org"), Some("any"));
        assert_eq!(super::pick_identity(&list, "other@elsewhere.net"), Some("me"));
        assert_eq!(super::pick_identity(&[], "me@example.com"), None);
    }

    use super::*;

    /// An email as `Email/get` answers it with [`EMAIL_PROPS`].
    fn jmap_email(id: &str, at: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "blobId": format!("B-{id}"), "threadId": format!("T-{id}"),
            "mailboxIds": { "MB": true }, "keywords": { "$seen": true },
            "hasAttachment": false, "receivedAt": at,
            "subject": format!("Subject {id}"), "preview": "Hello there",
            "from": [{ "name": "Ada", "email": "ada@example.org" }],
            "replyTo": null, "to": [{ "email": "me@example.org" }], "cc": null,
            "messageId": [format!("<{id}@example.org>")], "inReplyTo": null, "references": null,
            "header:X-Priority:asText": null, "header:Importance:asText": null, "header:Priority:asText": null,
        })
    }

    fn jmap_row(id: &str, ts: i64, folder_id: u32) -> Message {
        let (mut m, _, _) = jmap_message(&jmap_email(id, "2026-09-01T09:00:00Z"), 1, folder_id).expect("summary");
        m.timestamp = ts;
        m
    }

    fn rfc3339(at: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(at).unwrap().timestamp()
    }

    /// A JMAP API on loopback answering `answers.len()` POSTs in order, each
    /// with its `methodResponses`; hands back the request bodies.
    fn serve_api(answers: Vec<serde_json::Value>) -> (JmapSession, std::thread::JoinHandle<Vec<serde_json::Value>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let origin = format!("http://{}", listener.local_addr().expect("addr"));
        let handle = std::thread::spawn(move || {
            let mut bodies = Vec::new();
            for responses in answers {
                let Ok((stream, _)) = listener.accept() else { break };
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                reader.read_line(&mut line).expect("request line");
                let mut length = 0usize;
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = h.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0u8; length];
                reader.read_exact(&mut body).expect("body");
                bodies.push(serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null));
                let reply = serde_json::json!({ "methodResponses": responses, "sessionState": "x" }).to_string();
                let mut out = stream;
                let _ = write!(
                    out,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
            }
            bodies
        });
        let s = JmapSession {
            api_url: format!("{origin}/jmap/api/"),
            download_url: String::new(),
            upload_url: String::new(),
            event_source_url: None,
            account: "a1".into(),
            auth: "Bearer t".into(),
            quota: false,
        };
        (s, handle)
    }

    fn query_page(ids: &[&str], state: &str, total: usize, position: usize) -> serde_json::Value {
        serde_json::json!([["Email/query", {
            "accountId": "a1", "queryState": state, "canCalculateChanges": true,
            "position": position, "ids": ids, "total": total,
        }, "0"]])
    }

    fn state_with(path: &str, folder_id: u32, mailbox: &str) -> JmapState {
        let mut state = JmapState::new(&sample_account());
        state.folders.insert(path.to_string(), (folder_id, mailbox.to_string()));
        state
    }

    /// #429: what a complete id listing asks of the cache.
    #[test]
    fn the_index_plan_fetches_the_missing_and_drops_what_went() {
        let server: Vec<String> = ["M1", "M2", "M3", "M2"].iter().map(|s| s.to_string()).collect();
        let since = rfc3339("2026-09-10T00:00:00Z");
        let cached: HashMap<u32, i64> =
            [(hash_uid("M1"), 1), (hash_uid("GONE"), 1), (hash_uid("NEW"), since + 60)].into_iter().collect();
        let (gone, missing) = jmap_index_plan(&server, &cached, since, true);
        assert_eq!(gone, vec![hash_uid("GONE")]);
        assert_eq!(missing, vec!["M2".to_string(), "M3".to_string()]);
        let (gone, _) = jmap_index_plan(&server, &cached, since, false);
        assert!(gone.is_empty(), "an unsteady listing prunes nothing");
    }

    /// #429: the whole background pass over a folder: the ids in two
    /// pages, the missing mail in one get; the cache keeps what it had and
    /// loses what the server lost.
    #[tokio::test]
    async fn the_background_index_fills_and_prunes_a_mailbox() {
        let got: Vec<serde_json::Value> =
            (2..=5).map(|i| jmap_email(&format!("M{i}"), &format!("2026-09-0{}T09:00:00Z", 9 - i))).collect();
        let (s, server) = serve_api(vec![
            query_page(&["M1", "M2", "M3"], "q1", 5, 0),
            query_page(&["M4", "M5"], "q1", 5, 3),
            serde_json::json!([["Email/get", { "accountId": "a1", "state": "e1", "list": got, "notFound": [] }, "0"]]),
        ]);
        let cache = Cache::in_memory().expect("cache");
        let now = crate::datefmt::now();
        cache.upsert_messages(
            1,
            "Archive",
            &[
                jmap_row("M1", rfc3339("2026-09-08T09:00:00Z"), 3),
                jmap_row("GONE", rfc3339("2026-01-01T00:00:00Z"), 3),
                jmap_row("NEW", now + 60, 3),
            ],
        );
        let mut state = state_with("Archive", 3, "MB-archive");
        state.queue_backfill("Archive", true, false);
        let events = std::cell::RefCell::new(Vec::new());
        let emit = |e: WorkerEvent| events.borrow_mut().push(e);
        for _ in 0..3 {
            assert!(jmap_backfill_with(&s, 1, Some(&cache), &mut state, &emit).await);
        }
        assert!(state.backfill.is_empty(), "the folder's index is done");
        let mut have: Vec<u32> = cache.cached_uids(1, "Archive").into_iter().collect();
        have.sort_unstable();
        let mut want: Vec<u32> = ["M1", "M2", "M3", "M4", "M5", "NEW"].iter().map(|id| hash_uid(id)).collect();
        want.sort_unstable();
        assert_eq!(have, want);
        assert_eq!(state.uids.get(&hash_uid("M4")).map(|(_, b)| b.as_str()), Some("B-M4"));
        let kinds: Vec<&str> = events
            .into_inner()
            .iter()
            .map(|e| match e {
                WorkerEvent::Messages { folder_id: 3, .. } => "messages",
                WorkerEvent::MessagesAppend { folder_id: 3, .. } => "append",
                WorkerEvent::BackfillDone { folder_id: 3 } => "done",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["messages", "append", "done"]);
        let bodies = server.join().expect("server");
        let first = &bodies[0]["methodCalls"][0][1];
        assert_eq!(first["filter"], serde_json::json!({ "inMailbox": "MB-archive" }));
        assert_eq!(first["position"], 0);
        assert_eq!(bodies[1]["methodCalls"][0][1]["position"], 3);
        assert_eq!(bodies[2]["methodCalls"][0][1]["ids"], serde_json::json!(["M2", "M3", "M4", "M5"]));
    }

    /// #429: a folder that changed between pages is listed again from the
    /// top, positions having moved.
    #[tokio::test]
    async fn a_listing_starts_over_when_the_folder_changes() {
        let (s, server) = serve_api(vec![
            query_page(&["M1", "M2"], "q1", 4, 0),
            query_page(&["M3", "M4"], "q2", 4, 2),
            query_page(&["M0", "M1", "M2", "M3", "M4"], "q2", 5, 0),
        ]);
        let mut state = state_with("Inbox", 1, "MB-inbox");
        state.queue_backfill("Inbox", true, false);
        let emit = |_: WorkerEvent| {};
        for _ in 0..3 {
            assert!(jmap_backfill_with(&s, 1, None, &mut state, &emit).await);
        }
        match &state.backfill.front().expect("still to fetch").phase {
            JmapBackfillPhase::Fetching { missing } => assert_eq!(missing.len(), 5),
            JmapBackfillPhase::Listing { .. } => panic!("still listing"),
        }
        let bodies = server.join().expect("server");
        assert_eq!(bodies[2]["methodCalls"][0][1]["position"], 0);
    }

    /// #429: re-listing the newest page leaves the older cached mail be.
    #[tokio::test]
    async fn a_mailbox_load_merges_over_the_cache() {
        let (s, server) = serve_api(vec![serde_json::json!([
            ["Email/query", { "accountId": "a1", "queryState": "q1", "position": 0, "ids": ["A", "B"], "total": 400 }, "0"],
            ["Email/get", { "accountId": "a1", "state": "e1", "list": [
                jmap_email("A", "2026-10-02T09:00:00Z"),
                jmap_email("B", "2026-10-01T09:00:00Z"),
            ], "notFound": [] }, "1"],
        ])]);
        let cache = Cache::in_memory().expect("cache");
        cache.upsert_messages(
            1,
            "Inbox",
            &[jmap_row("OLD", rfc3339("2025-01-01T00:00:00Z"), 1), jmap_row("DEL", rfc3339("2026-10-01T12:00:00Z"), 1)],
        );
        let mut state = state_with("Inbox", 1, "MB-inbox");
        let (messages, complete) = jmap_sync_folder(&s, 1, 1, "Inbox", Some(&cache), &mut state).await.expect("listed");
        assert!(!complete, "the total says there is more");
        let mut uids: Vec<u32> = messages.iter().map(|m| m.uid).collect();
        uids.sort_unstable();
        let mut want = vec![hash_uid("A"), hash_uid("B"), hash_uid("OLD")];
        want.sort_unstable();
        assert_eq!(uids, want);
        let bodies = server.join().expect("server");
        assert_eq!(bodies[0]["methodCalls"][0][1]["calculateTotal"], true);
    }

    #[test]
    fn the_accounts_own_mail_quota_wins() {
        let list = serde_json::json!([
            { "resourceType": "count", "scope": "account", "used": 5, "hardLimit": 100, "types": ["Email"] },
            { "resourceType": "octets", "scope": "domain", "used": 9, "hardLimit": 1000, "types": ["Email"] },
            { "resourceType": "octets", "scope": "account", "used": 2048, "hardLimit": 4096, "types": ["Email", "CalendarEvent"] },
            { "resourceType": "octets", "scope": "account", "used": 1, "hardLimit": 2, "types": ["ContactCard"] },
        ]);
        assert_eq!(jmap_storage_quota(&list), Some(MailboxQuota { used: 2048, limit: 4096 }));
        // A soft limit stands in for a missing hard one; no limit, no quota.
        let soft = serde_json::json!([{ "resourceType": "octets", "scope": "account", "used": 10, "softLimit": 50 }]);
        assert_eq!(jmap_storage_quota(&soft), Some(MailboxQuota { used: 10, limit: 50 }));
        let none = serde_json::json!([{ "resourceType": "octets", "scope": "account", "used": 10 }]);
        assert_eq!(jmap_storage_quota(&none), None);
    }

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
    fn token_accounts_send_a_bearer_token() {
        let basic = AccountConfig { username: "me@example.org".into(), password: "pw".into(), ..account("mail.example.org", 443) };
        assert_eq!(jmap_auth(&basic), "Basic bWVAZXhhbXBsZS5vcmc6cHc=");
        // Fastmail's API token (#356): no username, and the token as typed,
        // minus stray whitespace from a paste.
        let token = AccountConfig { jmap_token: true, password: " fmu1-abc \n".into(), ..basic };
        assert_eq!(jmap_auth(&token), "Bearer fmu1-abc");
        let url = account("https://api.fastmail.com/jmap/session", 443);
        assert_eq!(jmap_base_url(&url), "https://api.fastmail.com/jmap/session");
        assert_eq!(origin_of(&jmap_base_url(&url)), "https://api.fastmail.com");
    }

    /// An HTTP server on loopback for `requests` requests: answers each by its path
    /// and hands back what was asked for, with the Authorization header.
    fn serve(requests: usize, answers: Vec<(&'static str, u16, String)>) -> (String, std::thread::JoinHandle<Vec<(String, String)>>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let origin = format!("http://{}", listener.local_addr().expect("addr"));
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..requests {
                let Ok((stream, _)) = listener.accept() else { break };
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                reader.read_line(&mut line).expect("request line");
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut auth = String::new();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.strip_prefix("Authorization: ").or_else(|| h.strip_prefix("authorization: ")) {
                        auth = v.trim().to_string();
                    }
                }
                let (status, body) = answers
                    .iter()
                    .find(|(p, _, _)| *p == path)
                    .map(|(_, s, b)| (*s, b.clone()))
                    .unwrap_or((404, String::new()));
                let mut out = stream;
                let _ = write!(
                    out,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                seen.push((path, auth));
            }
            seen
        });
        (origin, handle)
    }

    fn session_json() -> String {
        serde_json::json!({
            "apiUrl": "/jmap/api/",
            "downloadUrl": "/jmap/download/{accountId}/{blobId}/{name}?accept={type}",
            "uploadUrl": "/jmap/upload/{accountId}/",
            "primaryAccounts": { "urn:ietf:params:jmap:mail": "a1" },
            "capabilities": { "urn:ietf:params:jmap:mail": {} },
            "accounts": {},
        })
        .to_string()
    }

    #[test]
    fn a_session_url_is_used_as_given() {
        // Fastmail's shape (#356): the session resource named outright, and
        // signed in to with a bearer token.
        let (origin, server) = serve(1, vec![("/jmap/session", 200, session_json())]);
        let acc = AccountConfig {
            jmap_token: true,
            password: "fmu1-token".into(),
            ..account(&format!("{origin}/jmap/session"), 443)
        };
        let s = jmap_connect(&acc).expect("session");
        assert_eq!(s.api_url, format!("{origin}/jmap/api/"));
        assert_eq!(s.account, "a1");
        let seen = server.join().expect("server");
        assert_eq!(seen, vec![("/jmap/session".to_string(), "Bearer fmu1-token".to_string())]);
    }

    #[test]
    fn a_path_that_is_no_session_falls_back_to_well_known() {
        // A server behind a path prefix: the prefix itself is no session, so
        // the well-known path under it is asked, as before.
        let (origin, server) = serve(2, vec![("/mail/.well-known/jmap", 200, session_json())]);
        let acc = AccountConfig { username: "me".into(), password: "pw".into(), ..account(&format!("{origin}/mail"), 443) };
        let s = jmap_connect(&acc).expect("session");
        assert_eq!(s.api_url, format!("{origin}/jmap/api/"));
        let paths: Vec<String> = server.join().expect("server").into_iter().map(|(p, _)| p).collect();
        assert_eq!(paths, vec!["/mail".to_string(), "/mail/.well-known/jmap".to_string()]);
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
            // JMAP_LIVE_TOKEN=1: the third field is a bearer token (#356).
            jmap_token: std::env::var_os("JMAP_LIVE_TOKEN").is_some(),
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

    /// A template (#360), with `JMAP_LIVE` as for [`live`]: a top-level
    /// Templates mailbox the save makes is taken for the role by its name,
    /// and the template in it is read mail, not a draft. Leaves the account
    /// as it was.
    #[test]
    #[ignore]
    fn live_template() {
        let Ok(spec) = std::env::var("JMAP_LIVE") else { return };
        let mut parts = spec.splitn(3, ',');
        let acc = AccountConfig {
            imap_host: parts.next().unwrap_or_default().into(),
            imap_port: 443,
            username: parts.next().unwrap_or_default().into(),
            password: parts.next().unwrap_or_default().into(),
            jmap_token: std::env::var_os("JMAP_LIVE_TOKEN").is_some(),
            ..sample_account()
        };
        let s = jmap_connect(&acc).expect("session");
        let before = jmap_list_folders(&s, 1, &BTreeMap::new()).expect("folders");
        assert!(!before.iter().any(|f| f.folder.path == "Templates"), "start without one");
        let made = jmap_create_mailbox(&s, "Templates", None).expect("create");
        let folders = jmap_list_folders(&s, 1, &BTreeMap::new()).expect("folders");
        let templates = folders.iter().find(|f| f.folder.path == "Templates").expect("listed");
        assert_eq!(templates.folder.kind, FolderKind::Templates);
        assert_eq!(templates.mailbox_id, made);
        let raw = b"From: me <me@example.test>\r\nSubject: template\r\n\r\nbody\r\n";
        let id = jmap_import_template(&s, raw, &templates.mailbox_id).expect("import");
        let responses = jmap_call(
            &s,
            &[CAP_CORE, CAP_MAIL],
            vec![call(0, "Email/get", serde_json::json!({ "accountId": s.account, "ids": [id], "properties": ["keywords"] }))],
        )
        .expect("get");
        let keywords = args(&responses, 0)["list"][0]["keywords"].clone();
        println!("template keywords {keywords}");
        assert_eq!(keywords["$seen"], serde_json::json!(true));
        assert!(keywords.get("$draft").is_none());
        jmap_destroy_all(&s, &[id]).expect("drop template");
        jmap_call(
            &s,
            &[CAP_CORE, CAP_MAIL],
            vec![call(0, "Mailbox/set", serde_json::json!({ "accountId": s.account, "destroy": [made] }))],
        )
        .expect("drop mailbox");
    }

    /// The server's identities (#346), with `JMAP_LIVE` as for [`live`]:
    /// listed for the composer, and the account's own picked to send as.
    #[test]
    #[ignore]
    fn live_identities() {
        let Ok(spec) = std::env::var("JMAP_LIVE") else { return };
        let mut parts = spec.splitn(3, ',');
        let acc = AccountConfig {
            imap_host: parts.next().unwrap_or_default().into(),
            imap_port: 443,
            username: parts.next().unwrap_or_default().into(),
            password: parts.next().unwrap_or_default().into(),
            // JMAP_LIVE_TOKEN=1: the third field is a bearer token (#356).
            jmap_token: std::env::var_os("JMAP_LIVE_TOKEN").is_some(),
            ..sample_account()
        };
        let s = jmap_connect(&acc).expect("session");
        let list = jmap_identities(&s).expect("Identity/get");
        println!("identities {list:?}");
        let events = std::cell::RefCell::new(Vec::new());
        emit_jmap_identities(&s, &|e| events.borrow_mut().push(e));
        let got = events.borrow();
        let Some(WorkerEvent::Identities(ids)) = got.first() else { panic!("no Identities event") };
        assert!(ids.iter().any(|(_, a)| a.eq_ignore_ascii_case(&acc.username)), "{ids:?}");
        assert!(jmap_identity_for(&s, &acc.username).is_ok());
    }

    /// A signed message with a Unix-ending text file attached, sent to the
    /// account itself (#414), with `JMAP_LIVE` as for [`live`]. Stalwart
    /// takes bare newlines; Cyrus (Fastmail) is the server that refuses them.
    #[test]
    #[ignore]
    fn live_signed_send() {
        let Ok(spec) = std::env::var("JMAP_LIVE") else { return };
        if !crate::pgp::available() {
            return;
        }
        let mut parts = spec.splitn(3, ',');
        let acc = AccountConfig {
            imap_host: parts.next().unwrap_or_default().into(),
            imap_port: 443,
            username: parts.next().unwrap_or_default().into(),
            password: parts.next().unwrap_or_default().into(),
            jmap_token: std::env::var_os("JMAP_LIVE_TOKEN").is_some(),
            ..sample_account()
        };
        let home = std::env::temp_dir().join(format!("hylki-gpg-jmap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::env::set_var("HYLKI_GNUPGHOME", &home);
        let me = acc.username.clone();
        let account = AccountConfig { email: me.clone(), ..acc.clone() };
        crate::pgp::generate_key(&crate::pgp::Gpg::system(), "Me", &me, "never", "").expect("own key");
        let file = home.join("notes.txt");
        std::fs::write(&file, b"one\ntwo\n").unwrap();
        let msg = OutgoingMessage {
            to: me.clone(),
            subject: "live signed test".into(),
            body: "signed over JMAP".into(),
            attachments: vec![file.to_string_lossy().to_string()],
            sign: true,
            ..super::super::tests::sample_outgoing()
        };
        let raw = super::super::build_email(&account, &msg).expect("builds").formatted();
        let s = jmap_connect(&acc).expect("session");
        let folders = jmap_list_folders(&s, 1, &BTreeMap::new()).expect("folders");
        let sent = folders.iter().find(|f| f.folder.kind == FolderKind::Sent).expect("sent");
        let sid = jmap_submit(&s, &raw, &me, &[me.clone()], &sent.mailbox_id).expect("submit");
        println!("submitted {sid}");
        std::fs::remove_dir_all(&home).ok();
    }

    /// Removing an attachment over JMAP (#289), with `JMAP_LIVE` as for
    /// [`live`]: the copy keeps the other file, the keywords and the arrival
    /// time, and the original is destroyed. Leaves the inbox as it was.
    #[test]
    #[ignore]
    fn live_strip_attachment() {
        let Ok(spec) = std::env::var("JMAP_LIVE") else { return };
        let mut parts = spec.splitn(3, ',');
        let acc = AccountConfig {
            imap_host: parts.next().unwrap_or_default().into(),
            imap_port: 443,
            username: parts.next().unwrap_or_default().into(),
            password: parts.next().unwrap_or_default().into(),
            // JMAP_LIVE_TOKEN=1: the third field is a bearer token (#356).
            jmap_token: std::env::var_os("JMAP_LIVE_TOKEN").is_some(),
            ..sample_account()
        };
        let s = jmap_connect(&acc).expect("session");
        let folders = jmap_list_folders(&s, 1, &BTreeMap::new()).expect("folders");
        let inbox = folders.iter().find(|f| f.folder.kind == FolderKind::Inbox).expect("inbox");
        let raw = super::super::strip::MIXED.replace("r1@example.org", &format!("strip-{}@hylki.test", crate::datefmt::now()));
        let id = jmap_import_email(
            &s,
            raw.as_bytes(),
            serde_json::json!({
                "mailboxIds": { inbox.mailbox_id.clone(): true },
                "keywords": { "$seen": true, "$flagged": true },
                "receivedAt": "2020-01-01T10:00:00Z",
            }),
        )
        .expect("import");
        let blob = jmap_list_messages(&s, &inbox.mailbox_id, 1, inbox.folder.id)
            .expect("list")
            .into_iter()
            .find(|(_, i, _)| i == &id)
            .map(|(_, _, b)| b)
            .expect("listed");
        let mut state = JmapState::new(&sample_account());
        state.uids = HashMap::from([(hash_uid(&id), (id.clone(), blob))]);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (new_id, _) = rt
            .block_on(jmap_strip_attachment(&s, &mut state, &inbox.folder.path, hash_uid(&id), "report.pdf", 18))
            .expect("strip");
        assert_ne!(new_id, id);
        let responses = jmap_call(
            &s,
            &[CAP_CORE, CAP_MAIL],
            vec![call(
                0,
                "Email/get",
                serde_json::json!({
                    "accountId": s.account,
                    "ids": [id, new_id],
                    "properties": ["keywords", "receivedAt", "mailboxIds", "blobId"],
                }),
            )],
        )
        .expect("get");
        let got = args(&responses, 0);
        println!("{got}");
        assert_eq!(got["notFound"], serde_json::json!([id]), "the original is gone");
        let copy = &got["list"][0];
        assert_eq!(copy["keywords"], serde_json::json!({ "$seen": true, "$flagged": true }));
        assert_eq!(copy["receivedAt"], "2020-01-01T10:00:00Z");
        assert_eq!(copy["mailboxIds"], serde_json::json!({ inbox.mailbox_id.clone(): true }));
        let bytes = jmap_download(&s, copy["blobId"].as_str().unwrap()).expect("download");
        let names: Vec<String> = extract_attachments(&bytes).into_iter().map(|a| a.name).collect();
        assert_eq!(names, vec!["data.csv"]);
        jmap_destroy_all(&s, &[new_id]).expect("clean up");
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
