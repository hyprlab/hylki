//! Searching on the mail server, when the user asks for it (the search
//! bar's server button): IMAP `UID SEARCH` per folder, Microsoft Graph
//! `$search` over one folder or the whole mailbox. Nothing is sent to a
//! server for a search the user keeps local.
//!
//! A search runs on its own thread and connection, so it never waits behind
//! the account's sync and the sync never waits behind it. Results come back
//! a page at a time; after each page the search waits for the user's Load
//! More (or for the handle to be dropped, which ends it). Every request it
//! makes only reads: folders are opened with `EXAMINE`, bodies are never
//! fetched, and flags are never touched.
//!
//! Limits are the servers' own and are said out loud rather than worked
//! around: Microsoft Graph returns at most 1,000 results for one `$search`
//! and has no way to page past them, and it cannot search Reply-To. IMAP
//! has no attachment key and no notion of "has an attachment".

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::*;
use crate::mail_search::{Field, Node, Query};

/// Messages handed over per page, before the search waits for Load More.
const RESULT_PAGE: usize = 200;

/// Messages one IMAP FETCH asks for.
const FETCH_CHUNK: usize = 50;

/// Graph's own page size for a `$search`.
const GRAPH_SEARCH_TOP: usize = 50;

/// The most results one Microsoft Graph `$search` is read for. The
/// documentation says up to 1,000 (learn.microsoft.com/graph/search-query-parameter).
pub(super) const GRAPH_SEARCH_LIMIT: usize = 1000;

/// A `$search` that brought this many results may have been cut short. A
/// Microsoft 365 mailbox searched live answered every search with at most 275
/// (250 by date, and up to 25 it ranks as top results), whatever the page
/// size, and said nothing of the rest; so a search is taken as complete only
/// below this. Past it, the same search is asked again for mail sent on or
/// before the day of its oldest result, which is not paging Graph lacks but
/// a narrower question it answers in full.
pub(super) const GRAPH_SEARCH_MAYBE_CUT: usize = 250;

/// The day of the oldest `sentDateTime` among search results.
pub(super) fn oldest_sent_day(items: &[serde_json::Value]) -> Option<chrono::NaiveDate> {
    items
        .iter()
        .filter_map(|v| v["sentDateTime"].as_str())
        .filter_map(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&chrono::Utc).date_naive())
        .min()
}

/// What a search on the server reports.
#[derive(Debug)]
pub enum ServerSearchEvent {
    /// Matches, with the folder each is in. A message the local cache holds
    /// comes from the cache; the rest are read from the server.
    Found(Vec<(String, Message)>),
    /// The page is complete. `more` when the server has further results the
    /// user can ask for; `note` says what the search could not do.
    Done { more: bool, note: Option<String> },
    /// The search could not run.
    Failed(String),
}

/// A running search. Dropping it ends the search.
pub struct ServerSearchHandle {
    cancelled: Arc<AtomicBool>,
    more: tokio::sync::mpsc::UnboundedSender<()>,
}

impl ServerSearchHandle {
    /// Ask for the next page.
    pub fn more(&self) {
        let _ = self.more.send(());
    }
}

impl Drop for ServerSearchHandle {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

/// Start a search on `account`'s server, over `folder` or every folder.
pub fn spawn_server_search(
    account_id: u32,
    account: AccountConfig,
    query: Query,
    folder: Option<String>,
    emit: impl Fn(ServerSearchEvent) + Send + 'static,
) -> ServerSearchHandle {
    let cancelled = Arc::new(AtomicBool::new(false));
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let flag = cancelled.clone();
    let spawned = std::thread::Builder::new().name(format!("hylki-search-{account_id}")).spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => return emit(ServerSearchEvent::Failed(e.to_string())),
        };
        let mut pager = Pager { cancelled: flag, more: rx, emit: &emit };
        rt.block_on(async {
            match account.protocol {
                crate::config::Protocol::Imap => imap_search(account_id, &account, &query, folder, &mut pager).await,
                crate::config::Protocol::Graph => graph_search(account_id, &account, &query, folder, &mut pager).await,
                _ => {
                    (pager.emit)(ServerSearchEvent::Failed(i18n("This account's server cannot be searched from Hylki")))
                }
            }
        });
    });
    if let Err(e) = spawned {
        tracing::warn!("server search: no thread: {e}");
    }
    ServerSearchHandle { cancelled, more: tx }
}

/// Hands results over a page at a time.
struct Pager<'a, F: Fn(ServerSearchEvent)> {
    cancelled: Arc<AtomicBool>,
    more: tokio::sync::mpsc::UnboundedReceiver<()>,
    emit: &'a F,
}

impl<F: Fn(ServerSearchEvent)> Pager<'_, F> {
    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// End a page and wait for Load More. `false` when the search is over
    /// (the handle was dropped) instead.
    async fn page_done(&mut self) -> bool {
        (self.emit)(ServerSearchEvent::Done { more: true, note: None });
        self.more.recv().await.is_some() && !self.cancelled()
    }
}

/// An IMAP quoted string, and whether it needs `CHARSET UTF-8`.
fn imap_quoted(s: &str) -> (String, bool) {
    let clean: String = s.chars().filter(|c| !matches!(c, '\r' | '\n')).collect();
    let ascii = clean.is_ascii();
    (format!("\"{}\"", clean.replace('\\', "\\\\").replace('"', "\\\"")), !ascii)
}

/// `14-Oct-2026`, the date form IMAP takes (RFC 3501 `date`).
fn imap_date(d: chrono::NaiveDate) -> String {
    use chrono::Datelike;
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{}-{}-{}", d.day(), MONTHS[d.month0() as usize], d.year())
}

/// The `UID SEARCH` keys for a query (RFC 3501 §6.4.4). IMAP matches every
/// string key as a case-insensitive substring, as the local search does.
/// `before:`/`after:` become `BEFORE`/`SINCE`, which compare the date the
/// server received the message. Errors name what IMAP cannot search.
pub(super) fn imap_criteria(q: &Query) -> Result<String, String> {
    fn node(n: &Node, utf8: &mut bool) -> Result<String, String> {
        Ok(match n {
            Node::Term { field, text } => {
                let (quoted, wide) = imap_quoted(text);
                *utf8 |= wide;
                match field {
                    // TEXT covers the header and the body; an attachment's
                    // name sits in its part's header, which TEXT also reads.
                    Field::Any | Field::Attachment => format!("TEXT {quoted}"),
                    Field::From => format!("FROM {quoted}"),
                    Field::To => format!("TO {quoted}"),
                    Field::Cc => format!("CC {quoted}"),
                    Field::ReplyTo => format!("HEADER Reply-To {quoted}"),
                    Field::Subject => format!("SUBJECT {quoted}"),
                    Field::Body => format!("BODY {quoted}"),
                }
            }
            Node::Before(d) => format!("BEFORE {}", imap_date(*d)),
            Node::After(d) => format!("SINCE {}", imap_date(*d)),
            Node::HasAttachment => {
                return Err(i18n("has:attachment cannot be searched on an IMAP server; it is checked locally only"))
            }
            Node::And(v) => {
                let parts: Result<Vec<String>, String> = v.iter().map(|c| node(c, utf8)).collect();
                format!("({})", parts?.join(" "))
            }
            Node::Or(v) => {
                // IMAP's OR takes two keys: fold the rest in from the right.
                let mut parts: Vec<String> = v.iter().map(|c| node(c, utf8)).collect::<Result<_, _>>()?;
                let mut acc = parts.pop().unwrap_or_default();
                while let Some(left) = parts.pop() {
                    acc = format!("OR {left} {acc}");
                }
                acc
            }
            Node::Not(c) => format!("NOT {}", node(c, utf8)?),
        })
    }
    let root = q.root.as_ref().ok_or_else(|| i18n("Nothing to search for"))?;
    let mut utf8 = false;
    let keys = node(root, &mut utf8)?;
    Ok(if utf8 { format!("CHARSET UTF-8 {keys}") } else { keys })
}

/// The KQL for a Graph `$search` (the part inside its quotes). Values with
/// spaces are quoted; a quote or backslash in a value cannot be passed
/// through safely and becomes a space.
pub(super) fn graph_kql(q: &Query) -> Result<String, String> {
    fn value(text: &str) -> String {
        let clean: String = text.chars().map(|c| if matches!(c, '"' | '\\') { ' ' } else { c }).collect();
        let clean = clean.trim().to_string();
        if clean.contains(char::is_whitespace) {
            format!("\\\"{clean}\\\"")
        } else {
            clean
        }
    }
    fn node(n: &Node) -> Result<String, String> {
        Ok(match n {
            Node::Term { field, text } => {
                let v = value(text);
                match field {
                    // Graph's default fields are from, subject and body; the
                    // local search also looks at the recipients and the
                    // attachment names, so the server is asked for those too.
                    Field::Any => format!("(subject:{v} OR body:{v} OR participants:{v} OR attachment:{v})"),
                    Field::From => format!("from:{v}"),
                    Field::To => format!("to:{v}"),
                    Field::Cc => format!("cc:{v}"),
                    Field::Subject => format!("subject:{v}"),
                    Field::Body => format!("body:{v}"),
                    Field::Attachment => format!("attachment:{v}"),
                    Field::ReplyTo => {
                        return Err(i18n(
                            "Microsoft 365 cannot search Reply-To on the server; it is checked locally only",
                        ))
                    }
                }
            }
            Node::Before(d) => format!("received<{}", d.format("%Y-%m-%d")),
            Node::After(d) => format!("received>={}", d.format("%Y-%m-%d")),
            Node::HasAttachment => "hasAttachments:true".to_string(),
            Node::And(v) => format!("({})", v.iter().map(node).collect::<Result<Vec<_>, _>>()?.join(" AND ")),
            Node::Or(v) => format!("({})", v.iter().map(node).collect::<Result<Vec<_>, _>>()?.join(" OR ")),
            Node::Not(c) => format!("NOT {}", node(c)?),
        })
    }
    node(q.root.as_ref().ok_or_else(|| i18n("Nothing to search for"))?)
}

async fn imap_search<F: Fn(ServerSearchEvent)>(
    account_id: u32,
    account: &AccountConfig,
    query: &Query,
    folder: Option<String>,
    pager: &mut Pager<'_, F>,
) {
    let criteria = match imap_criteria(query) {
        Ok(c) => c,
        Err(e) => return (pager.emit)(ServerSearchEvent::Failed(e)),
    };
    let mut sess = match connect(account).await {
        Ok(s) => s,
        Err(e) => return (pager.emit)(ServerSearchEvent::Failed(e.to_string())),
    };
    let folders: Vec<String> = match folder {
        Some(f) => vec![f],
        None => match list_folders(account_id, &mut sess, &account.folder_roles, &account.hidden_folders).await {
            Ok(fs) => fs.into_iter().map(|f| f.path).collect(),
            Err(e) => return (pager.emit)(ServerSearchEvent::Failed(e.to_string())),
        },
    };
    let cache = Cache::open().ok();
    let mut skipped = Vec::new();
    // Every folder is searched first (one round trip each); what is cached
    // is handed over at once, and what is not is read a page at a time.
    let mut to_fetch: Vec<(String, Vec<u32>)> = Vec::new();
    for path in folders {
        if pager.cancelled() {
            return;
        }
        if exam(&mut sess, &path).await.is_err() {
            skipped.push(path);
            continue;
        }
        let found = match search_uids(&mut sess, &criteria).await {
            Ok(set) => set,
            Err(e) => {
                tracing::warn!("[account {account_id}] server search in {path} failed: {e}");
                skipped.push(path);
                continue;
            }
        };
        if found.is_empty() {
            continue;
        }
        let mut uids: Vec<u32> = found.into_iter().collect();
        uids.sort_unstable_by(|a, b| b.cmp(a));
        let local = cache.as_ref().map(|c| c.messages_by_uid(account_id, &path, &uids)).unwrap_or_default();
        let have: std::collections::HashSet<u32> = local.iter().map(|m| m.uid).collect();
        if !local.is_empty() {
            (pager.emit)(ServerSearchEvent::Found(local.into_iter().map(|m| (path.clone(), m)).collect()));
        }
        let missing: Vec<u32> = uids.into_iter().filter(|u| !have.contains(u)).collect();
        if !missing.is_empty() {
            to_fetch.push((path, missing));
        }
    }
    let mut in_page = 0usize;
    for (path, uids) in to_fetch {
        if exam(&mut sess, &path).await.is_err() {
            skipped.push(path);
            continue;
        }
        for chunk in uids.chunks(FETCH_CHUNK) {
            if in_page >= RESULT_PAGE {
                if !pager.page_done().await {
                    return;
                }
                in_page = 0;
                // The connection may have sat idle; the folder is opened again.
                if exam(&mut sess, &path).await.is_err() {
                    skipped.push(path.clone());
                    break;
                }
            }
            if pager.cancelled() {
                return;
            }
            match imap_summaries(account_id, &mut sess, chunk).await {
                Ok(found) => {
                    in_page += found.len();
                    (pager.emit)(ServerSearchEvent::Found(found.into_iter().map(|m| (path.clone(), m)).collect()));
                }
                Err(e) => tracing::warn!("[account {account_id}] server search: reading results in {path} failed: {e}"),
            }
        }
    }
    let _ = sess.logout().await;
    let note = (!skipped.is_empty())
        .then(|| i18n_f("These folders could not be searched: {folders}", &[("folders", &skipped.join(", "))]));
    (pager.emit)(ServerSearchEvent::Done { more: false, note });
}

/// The summaries of `uids` in the folder just examined, as a folder listing
/// would build them. Falls back to the raw headers for servers whose
/// ENVELOPE the parser refuses (iCloud).
async fn imap_summaries(
    account_id: u32,
    sess: &mut ImapSession,
    uids: &[u32],
) -> Result<Vec<Message>, async_imap::error::Error> {
    let set = uid_set(uids);
    let items = format!("(UID ENVELOPE FLAGS BODYSTRUCTURE INTERNALDATE{REFS_FETCH_ITEM})");
    let envelope: Result<Vec<Fetch>, _> = match fetch_uids(sess, &set, &items).await {
        Ok(stream) => stream.try_collect().await,
        Err(e) => Err(e),
    };
    let fetches = match envelope {
        Ok(f) => return Ok(f.iter().map(|f| build_summary(account_id, f, 0)).collect()),
        Err(_) => {
            fetch_uids(sess, &set, "(UID FLAGS BODY.PEEK[HEADER] INTERNALDATE)")
                .await?
                .try_collect::<Vec<Fetch>>()
                .await?
        }
    };
    Ok(fetches.iter().map(|f| summary_from_headers(account_id, f, 0)).collect())
}

async fn graph_search<F: Fn(ServerSearchEvent)>(
    account_id: u32,
    account: &AccountConfig,
    query: &Query,
    folder: Option<String>,
    pager: &mut Pager<'_, F>,
) {
    let kql = match graph_kql(query) {
        Ok(k) => k,
        Err(e) => return (pager.emit)(ServerSearchEvent::Failed(e)),
    };
    let Some(token) = graph_token(account, &|_: WorkerEvent| {}).await else {
        return (pager.emit)(ServerSearchEvent::Failed(i18n("Microsoft 365 did not give Hylki a sign-in token")));
    };
    graph_search_with(&token, GRAPH_BASE, account_id, account, &kql, folder, pager).await
}

/// [`graph_search`] with its token, against `base` (a test's own server).
async fn graph_search_with<F: Fn(ServerSearchEvent)>(
    token: &str,
    base: &str,
    account_id: u32,
    account: &AccountConfig,
    kql: &str,
    folder: Option<String>,
    pager: &mut Pager<'_, F>,
) {
    let token = token.to_string();
    let (t, b, roles) = (token.clone(), base.to_string(), account.folder_roles.clone());
    let folders = match blocking(move || graph_list_folders_at(&t, &b, account_id, &roles)).await {
        Ok(f) => f,
        Err(e) => return (pager.emit)(ServerSearchEvent::Failed(e)),
    };
    let by_gid: std::collections::HashMap<String, String> =
        folders.iter().map(|f| (f.graph_id.clone(), f.folder.path.clone())).collect();
    let scope = match &folder {
        Some(path) => match folders.iter().find(|f| &f.folder.path == path) {
            Some(f) => format!("/me/mailFolders/{}", f.graph_id),
            None => return (pager.emit)(ServerSearchEvent::Failed(i18n("The folder was not found on the server"))),
        },
        None => "/me".to_string(),
    };
    let cache = Cache::open().ok();
    let (mut in_page, mut elsewhere) = (0usize, 0usize);
    let mut seen: std::collections::HashSet<String> = Default::default();
    let mut window: Option<chrono::NaiveDate> = None;
    let mut cut_at: Option<chrono::NaiveDate> = None;
    // One round per window of dates, newest first; see GRAPH_SEARCH_MAYBE_CUT.
    for _ in 0..1000 {
        let query = match window {
            None => kql.to_string(),
            Some(day) => format!("({kql}) AND sent<={}", day.format("%Y-%m-%d")),
        };
        let search = crate::percent::encode(&format!("\"{query}\""));
        let mut url = Some(format!(
            "{base}{scope}/messages?$search={search}&$top={GRAPH_SEARCH_TOP}&{GRAPH_MSG_SELECT},sentDateTime,parentFolderId"
        ));
        let mut round: Vec<serde_json::Value> = Vec::new();
        while let Some(next) = url.take() {
            if pager.cancelled() {
                return;
            }
            if in_page >= RESULT_PAGE {
                if !pager.page_done().await {
                    return;
                }
                in_page = 0;
            }
            let t = token.clone();
            let page = match blocking(move || graph_get_json(&t, &next)).await {
                Ok(p) => p,
                Err(e) => return (pager.emit)(ServerSearchEvent::Failed(e)),
            };
            let mut found = Vec::new();
            for v in page["value"].as_array().into_iter().flatten() {
                round.push(v.clone());
                let Some(id) = v["id"].as_str() else { continue };
                if !seen.insert(id.to_string()) {
                    continue;
                }
                // A folder Hylki does not list (hidden, or not mail) cannot
                // open the message.
                let Some(path) = v["parentFolderId"].as_str().and_then(|g| by_gid.get(g)) else {
                    elsewhere += 1;
                    continue;
                };
                let Some((m, gid)) = graph_message(v, account_id, 0) else { continue };
                remember_search_id(&account.email, m.uid, &gid);
                let m =
                    cache.as_ref().and_then(|c| c.messages_by_uid(account_id, path, &[m.uid]).pop()).unwrap_or(m);
                found.push((path.clone(), m));
            }
            in_page += found.len();
            if !found.is_empty() {
                (pager.emit)(ServerSearchEvent::Found(found));
            }
            url = page["@odata.nextLink"].as_str().map(str::to_string);
        }
        if round.len() < GRAPH_SEARCH_MAYBE_CUT {
            break;
        }
        match oldest_sent_day(&round) {
            Some(day) if window != Some(day) => window = Some(day),
            // A whole day the server will not answer in full: the rest of it,
            // and what lies before, cannot be reached by narrowing further.
            day => {
                cut_at = day.or(window);
                break;
            }
        }
    }
    let mut notes = Vec::new();
    if let Some(day) = cut_at {
        notes.push(i18n_f(
            "Microsoft 365 did not return every result sent on or before {day}; narrow the search to find them",
            &[("day", &day.format("%Y-%m-%d").to_string())],
        ));
    }
    if elsewhere > 0 {
        notes.push(i18n_f("{n} results are in folders Hylki does not show", &[("n", &elsewhere.to_string())]));
    }
    (pager.emit)(ServerSearchEvent::Done { more: false, note: (!notes.is_empty()).then(|| notes.join(". ")) });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imap(q: &str) -> Result<String, String> {
        imap_criteria(&Query::parse(q).unwrap())
    }

    fn kql(q: &str) -> Result<String, String> {
        graph_kql(&Query::parse(q).unwrap())
    }

    /// A Graph search pages through `@odata.nextLink`, files each result
    /// under its folder, skips the ones in folders Hylki does not list, and,
    /// when the server cut a search short (it answered with 250 or more),
    /// asks again for older mail and hands over only what is new.
    #[tokio::test]
    async fn a_graph_search_pages_files_and_goes_back_by_date() {
        use super::graph::tests_support::{answer, folder_json, role_answers, serve};
        let result = |i: usize, parent: &str, day: u32| {
            serde_json::json!({ "id": format!("S{i}"), "subject": format!("Hit {i}"), "parentFolderId": parent,
                                "receivedDateTime": format!("2026-10-{day:02}T10:00:00Z"),
                                "sentDateTime": format!("2026-10-{day:02}T10:00:00Z"),
                                "from": { "emailAddress": { "address": "a@x" } } })
        };
        // 275 newest (one in a hidden folder), the last 25 sent on the 10th.
        let first: Vec<_> =
            (0..275).map(|i| result(i, if i == 7 { "HIDDEN" } else { "F1" }, if i < 250 { 14 } else { 10 })).collect();
        // Then on or before the 10th: those 25 again and 100 older, in Archive.
        let older: Vec<_> = (250..375).map(|i| result(i, if i < 275 { "F1" } else { "F2" }, if i < 275 { 10 } else { 3 })).collect();
        let mut answers = role_answers();
        answers.extend([
            answer("/me/mailFolders?$top=100", 200, serde_json::json!({
                "value": [folder_json("F1", "Inbox", 0), folder_json("F2", "Archive", 0)] })),
            answer("sent%3C%3D2026-10-10", 200, serde_json::json!({ "value": older })),
            answer("/me/messages?$search=", 200, serde_json::json!({
                "value": first[..200], "@odata.nextLink": "{origin}/me/messages?$search=x&$skiptoken=2" })),
            answer("$skiptoken=2", 200, serde_json::json!({ "value": first[200..] })),
        ]);
        let (origin, server) = serve(answers);
        let events = std::sync::Mutex::new(Vec::new());
        let emit = |e: ServerSearchEvent| events.lock().unwrap().push(e);
        let (more_tx, more_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut pager = Pager { cancelled: Arc::new(AtomicBool::new(false)), more: more_rx, emit: &emit };
        // Every page past RESULT_PAGE waits for Load More; ask ahead.
        for _ in 0..10 {
            more_tx.send(()).unwrap();
        }
        let account = super::super::sample_account();
        graph_search_with("t", &origin, 1, &account, "subject:hit", None, &mut pager).await;
        drop(server);
        let events = events.into_inner().unwrap();
        let found: Vec<&(String, Message)> = events
            .iter()
            .filter_map(|e| match e { ServerSearchEvent::Found(v) => Some(v), _ => None })
            .flatten()
            .collect();
        assert_eq!(found.len(), 374, "every result once, but the one in a hidden folder");
        assert_eq!(found.iter().filter(|(p, _)| p == "Archive").count(), 100);
        let mut uids: Vec<u32> = found.iter().map(|(_, m)| m.uid).collect();
        uids.sort_unstable();
        uids.dedup();
        assert_eq!(uids.len(), 374, "no duplicates");
        match events.last().unwrap() {
            ServerSearchEvent::Done { more: false, note: Some(note) } => {
                assert!(note.contains("1 results are in folders"), "{note}");
                assert!(!note.contains("did not return"), "{note}");
            }
            other => panic!("ended with {other:?}"),
        }
    }

    #[test]
    fn imap_keys_follow_the_query() {
        assert_eq!(imap("invoice").unwrap(), r#"TEXT "invoice""#);
        assert_eq!(imap("from:ann to:bob cc:carol").unwrap(), r#"(FROM "ann" TO "bob" CC "carol")"#);
        assert_eq!(imap("subject:\"q3 plan\" body:x").unwrap(), r#"(SUBJECT "q3 plan" BODY "x")"#);
        assert_eq!(imap("replyto:list").unwrap(), r#"HEADER Reply-To "list""#);
        assert_eq!(imap("a OR b OR c").unwrap(), r#"OR TEXT "a" OR TEXT "b" TEXT "c""#);
        assert_eq!(imap("a -b").unwrap(), r#"(TEXT "a" NOT TEXT "b")"#);
        assert_eq!(imap("after:2026-10-01 before:2026-10-14").unwrap(), "(SINCE 1-Oct-2026 BEFORE 14-Oct-2026)");
    }

    #[test]
    fn imap_keys_quote_and_declare_utf8() {
        assert_eq!(imap("größe").unwrap(), r#"CHARSET UTF-8 TEXT "größe""#);
        assert_eq!(imap(r#""say \"hi\"""#).unwrap(), r#"TEXT "say \"hi\"""#);
        assert_eq!(imap(r#"subject:a\b"#).unwrap(), r#"SUBJECT "a\\b""#);
    }

    #[test]
    fn imap_says_what_it_cannot_search() {
        assert!(imap("has:attachment").is_err());
    }

    #[test]
    fn graph_kql_follows_the_query() {
        assert_eq!(kql("from:ann subject:report").unwrap(), "(from:ann AND subject:report)");
        assert_eq!(kql("to:bob OR cc:carol").unwrap(), "(to:bob OR cc:carol)");
        assert_eq!(kql("body:\"two words\"").unwrap(), r#"body:\"two words\""#);
        assert_eq!(
            kql("invoice").unwrap(),
            "(subject:invoice OR body:invoice OR participants:invoice OR attachment:invoice)"
        );
        assert_eq!(kql("after:2026-10-01 has:attachment -spam").unwrap(),
            "(received>=2026-10-01 AND hasAttachments:true AND NOT (subject:spam OR body:spam OR participants:spam OR attachment:spam))");
        assert_eq!(kql("attachment:budget.xlsx").unwrap(), "attachment:budget.xlsx");
    }

    #[test]
    fn graph_kql_cannot_carry_a_quote_or_reply_to() {
        assert_eq!(kql(r#"subject:"a \"b\" c""#).unwrap(), r#"subject:\"a  b  c\""#);
        assert!(kql("replyto:list").is_err());
    }
}
