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
/// Summaries listed per folder (matches the POP3 path's indexing appetite).
pub(super) const GRAPH_INDEX_CAP: usize = 300;

/// One Graph mail folder, flattened out of the tree.
struct GraphFolder {
    graph_id: String,
    folder: Folder,
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

fn graph_get_json(token: &str, url: &str) -> Result<serde_json::Value, String> {
    graph_wire("GET", url);
    let r = ureq::get(url)
        .set("Authorization", &graph_auth(token))
        .call()
        .map_err(graph_err)
        .and_then(|resp| resp.into_json().map_err(|e| e.to_string()));
    graph_wired("GET", url, &r);
    r
}

fn graph_get_bytes(token: &str, url: &str) -> Result<Vec<u8>, String> {
    graph_wire("GET", url);
    let resp = ureq::get(url)
        .set("Authorization", &graph_auth(token))
        .call()
        .map_err(graph_err);
    graph_wired("GET", url, &resp);
    let resp = resp?;
    let mut out = Vec::new();
    use std::io::Read;
    // Raw MIME can be large; cap well above any sane message (64 MB).
    resp.into_reader()
        .take(64 * 1024 * 1024)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    Ok(out)
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
    let mut out = Vec::new();
    let mut url = first_url.to_string();
    loop {
        let page = graph_get_json(token, &url)?;
        if let Some(items) = page["value"].as_array() {
            out.extend(items.iter().cloned());
        }
        if out.len() >= cap {
            out.truncate(cap);
            return Ok(out);
        }
        match page["@odata.nextLink"].as_str() {
            Some(next) => url = next.to_string(),
            None => return Ok(out),
        }
    }
}

/// List the account's mail folders (tree flattened, well-known roles mapped),
/// sorted and id-numbered exactly like the IMAP path's folder list.
fn graph_list_folders(
    token: &str,
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
        if let Ok(v) = graph_get_json(token, &format!("{GRAPH_BASE}/me/mailFolders/{wk}?$select=id"))
        {
            if let Some(id) = v["id"].as_str() {
                roles.insert(id.to_string(), kind);
            }
        }
    }

    const SELECT: &str =
        "$select=id,displayName,childFolderCount,unreadItemCount,totalItemCount";
    let roots = graph_paged(
        token,
        &format!("{GRAPH_BASE}/me/mailFolders?$top=100&{SELECT}"),
        200,
    )?;

    // Flatten the tree breadth-first; paths join with '/' like the sidebar's
    // hierarchy expects. Depth and total are capped defensively.
    let mut out: Vec<GraphFolder> = Vec::new();
    // Graph folder id → (every message, unread): the chip picks one once the
    // kinds are final (a manual Drafts assignment counts every draft).
    let mut counts: std::collections::HashMap<String, (u32, u32)> = Default::default();
    let mut queue: Vec<(serde_json::Value, String, u8)> =
        roots.into_iter().map(|v| (v, String::new(), 0u8)).collect();
    while let Some((v, prefix, depth)) = queue.pop() {
        let Some(gid) = v["id"].as_str() else { continue };
        let name = v["displayName"].as_str().unwrap_or("?").to_string();
        let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        let kind = roles.get(gid).copied().unwrap_or(FolderKind::Custom);
        if v["childFolderCount"].as_i64().unwrap_or(0) > 0 && depth < 4 && out.len() < 400 {
            if let Ok(children) = graph_paged(
                token,
                &format!("{GRAPH_BASE}/me/mailFolders/{gid}/childFolders?$top=100&{SELECT}"),
                200,
            ) {
                queue.extend(children.into_iter().map(|c| (c, path.clone(), depth + 1)));
            }
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
fn graph_message(v: &serde_json::Value, account_id: u32, folder_id: u32) -> Option<(Message, String)> {
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
    };
    Some((msg, gid))
}

const GRAPH_MSG_SELECT: &str = "$select=id,internetMessageId,conversationId,subject,bodyPreview,\
                                from,replyTo,toRecipients,ccRecipients,receivedDateTime,isRead,\
                                flag,hasAttachments,categories";

/// List a folder's newest summaries (newest first).
fn graph_list_messages(
    token: &str,
    folder_graph_id: &str,
    account_id: u32,
    folder_id: u32,
) -> Result<Vec<(Message, String)>, String> {
    let url = format!(
        "{GRAPH_BASE}/me/mailFolders/{folder_graph_id}/messages\
         ?$top=100&$orderby=receivedDateTime%20desc&{GRAPH_MSG_SELECT}"
    );
    let items = graph_paged(token, &url, GRAPH_INDEX_CAP)?;
    Ok(items.iter().filter_map(|v| graph_message(v, account_id, folder_id)).collect())
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
}

impl GraphState {
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
    }
}

pub(super) async fn run_graph(
    account_id: u32,
    account: AccountConfig,
    mut rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent),
) {
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
    let mut state = GraphState {
        folders: Default::default(),
        uids: Default::default(),
        drafts: None,
        inbox: None,
        roles: account.folder_roles.clone(),
        hidden: account.hidden_folders.clone(),
    };

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
        let req = tokio::select! {
            req = rx.recv() => match req {
                Some(req) => req,
                None => break,
            },
            _ = poll.tick() => {
                graph_poll_inbox(account_id, &account, cache.as_ref(), &mut state, &emit).await;
                continue;
            }
        };
        match req {
            MailRequest::Locate { message_id } => {
                // No folders to search here beyond what the cache already
                // held: answer "not found" so the app can report the miss.
                emit(WorkerEvent::Located { message_id: message_id.clone(), hit: None });
            }
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
                    .map(|c| c.thread_summaries(account_id, &groups))
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
                match graph_load_folder(
                    &token, account_id, folder_id, &path, cache.as_ref(), &mut state,
                )
                .await
                {
                    Ok(messages) => {
                        let unread = state.chip_count(folder_id, &messages);
                        emit_graph_body_hits(&token, &account, folder_id, &path, &messages, &state, &emit)
                            .await;
                        emit(WorkerEvent::Messages { folder_id, messages });
                        emit(WorkerEvent::FolderUnread { folder_id, unread });
                        // Graph loads the whole folder in one pass — there is no
                        // background backfill, so the index is complete now.
                        // Without this the list's "Loading more…" tail spinner
                        // never clears on folders with less than a page of mail
                        // (an emptied inbox most visibly).
                        emit(WorkerEvent::BackfillDone { folder_id });
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
                    emit(WorkerEvent::Body { message_id, path, body });
                    if let Some(check) = check {
                        emit(WorkerEvent::SenderChecked { message_id, check });
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
                        emit(WorkerEvent::Body { message_id, path, body });
                        emit(WorkerEvent::SenderChecked { message_id, check });
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
                            emit(WorkerEvent::SenderChecked { message_id, check });
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
                            emit(WorkerEvent::SenderChecked { message_id, check });
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
                        emit(WorkerEvent::Attachments { message_id, items });
                        continue;
                    }
                }
                if !download {
                    emit(WorkerEvent::AttachmentsPending { message_id });
                    continue;
                }
                match graph_fetch_raw(&account, &mut state, &path, uid, &emit).await {
                    Ok(raw) => {
                        let items = extract_attachments(&raw);
                        if let Some(c) = cache.as_ref() {
                            c.save_attachments(account_id, &path, uid, &items);
                        }
                        emit(WorkerEvent::Attachments { message_id, items });
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
                match graph_load_folder(&token, account_id, folder_id, &path, cache.as_ref(), &mut state).await {
                    Ok(messages) => {
                        let uids: Vec<u32> = messages.iter().map(|m| m.uid).collect();
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

            MailRequest::SaveDraft { message, folder_id, path } => {
                emit(WorkerEvent::Status(i18n("Saving draft…")));
                let mut message = OutgoingMessage { sign: false, encrypt: false, ..*message };
                restore_msgid_case(cache.as_ref(), &mut message);
                let saved = match build_draft(&account, &message) {
                    Ok(email) => {
                        let raw = email.formatted();
                        match graph_token(&account, &emit).await {
                            Some(token) => {
                                let t = token.clone();
                                let url = format!("{GRAPH_BASE}/me/messages");
                                let r = blocking(move || {
                                    graph_post_mime(&t, &url, &raw)
                                }).await;
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
                                        true
                                    }
                                    Err(e) => {
                                        emit(WorkerEvent::error(i18n_f("Could not save draft: {e}", &[("e", &(e).to_string())])));
                                        false
                                    }
                                }
                            }
                            None => false,
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::error(i18n_f("Could not save draft: {e}", &[("e", &(e).to_string())])));
                        false
                    }
                };
                emit(WorkerEvent::Status(String::new()));
                if saved {
                    emit(WorkerEvent::DraftSaved);
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
                                    &token, account_id, sent.id, &sent.path, cache.as_ref(), &mut state,
                                )
                                .await
                                {
                                    c.upsert_messages(account_id, &sent.path, &messages);
                                    emit(WorkerEvent::Messages { folder_id: sent.id, messages });
                                }
                            }
                        }
                        emit(WorkerEvent::Sent);
                    }
                    Err(e) => {
                        emit(WorkerEvent::Status(String::new()));
                        send_failed(cache.as_ref(), account_id, &account, &message, None, &e, &emit);
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

            MailRequest::ExportRaw { token, path, uid } => {
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
async fn graph_token(account: &AccountConfig, emit: &impl Fn(WorkerEvent)) -> Option<String> {
    match fetch_oauth_token(account).await {
        Some(t) => Some(t),
        None => {
            emit(WorkerEvent::net_error(format!(
                    "GNOME Online Accounts could not provide a sign-in token for {}. Open \
                     Settings → Online Accounts and sign in again.",
                    account.email
                )));
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

/// List a folder's summaries, refresh the uid map and the cache.
async fn graph_load_folder(
    token: &str,
    account_id: u32,
    folder_id: u32,
    path: &str,
    cache: Option<&Cache>,
    state: &mut GraphState,
) -> Result<Vec<Message>, String> {
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

    let t = token.to_string();
    let listed = blocking(move || {
        graph_list_messages(&t, &gid, account_id, folder_id)
    }).await?;

    let mut messages = Vec::with_capacity(listed.len());
    for (m, gid) in listed {
        state.uids.insert(m.uid, gid);
        messages.push(m);
    }
    if let Some(c) = cache {
        c.save_messages(account_id, path, &messages);
    }
    Ok(messages)
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
    // Not indexed yet: list the folder (fills the uid map) and try again. The
    // account/folder ids only label the discarded summaries, so zeros are fine.
    let _ = graph_load_folder(&token, 0, 0, path, None, state).await;
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

    let t = token.clone();
    let listed = blocking(move || {
        graph_list_messages(&t, &gid, account_id, folder_id)
    }).await?;

    let mut uids = Vec::new();
    for (m, mgid) in listed {
        if wanted.contains(m.message_id.as_str()) {
            state.uids.insert(m.uid, mgid);
            uids.push(m.uid);
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
        let items = blocking(move || graph_paged(&t, &url, GRAPH_INDEX_CAP)).await;
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
    for needle in needles {
        // KQL: the term in quotes; a quote inside it would end the term.
        let term = format!("\"body:{}\"", needle.replace('"', " "));
        let url = format!(
            "{GRAPH_BASE}/me/mailFolders/{gid}/messages?$search={}&$select=id&$top=250",
            crate::percent::encode(&term)
        );
        let t = token.to_string();
        let found = blocking(move || graph_paged(&t, &url, 250)).await;
        match found {
            Ok(items) => {
                for uid in items
                    .iter()
                    .filter_map(|v| v["id"].as_str())
                    .map(hash_uid)
                    .filter(|u| listed.contains(u))
                {
                    hits.entry(uid).or_default().push(needle.clone());
                }
            }
            Err(e) => tracing::warn!("filter: Graph body search for {needle:?} failed: {e}"),
        }
    }
    tracing::info!("filter: Graph body search over {} messages hit {}", listed.len(), hits.len());
    emit(WorkerEvent::BodyHits { folder_id, hits });
}
