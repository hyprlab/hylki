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

/// The most results Microsoft Graph returns for one `$search` request
/// (learn.microsoft.com/graph/search-query-parameter).
pub(super) const GRAPH_SEARCH_LIMIT: usize = 1000;

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
    let (t, roles) = (token.clone(), account.folder_roles.clone());
    let folders = match blocking(move || graph_list_folders(&t, account_id, &roles)).await {
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
    let search = crate::percent::encode(&format!("\"{kql}\""));
    let mut url = Some(format!(
        "{GRAPH_BASE}{scope}/messages?$search={search}&$top={GRAPH_SEARCH_TOP}&{GRAPH_MSG_SELECT},parentFolderId"
    ));
    let cache = Cache::open().ok();
    let (mut total, mut in_page, mut elsewhere) = (0usize, 0usize, 0usize);
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
            total += 1;
            // A folder Hylki does not list (hidden, or not mail) cannot open
            // the message.
            let Some(path) = v["parentFolderId"].as_str().and_then(|g| by_gid.get(g)) else {
                elsewhere += 1;
                continue;
            };
            let Some((m, gid)) = graph_message(v, account_id, 0) else { continue };
            remember_search_id(&account.email, m.uid, &gid);
            let m = cache.as_ref().and_then(|c| c.messages_by_uid(account_id, path, &[m.uid]).pop()).unwrap_or(m);
            found.push((path.clone(), m));
        }
        in_page += found.len();
        if !found.is_empty() {
            (pager.emit)(ServerSearchEvent::Found(found));
        }
        url = page["@odata.nextLink"].as_str().map(str::to_string);
    }
    let mut notes = Vec::new();
    if total >= GRAPH_SEARCH_LIMIT {
        notes.push(i18n_f(
            "Microsoft 365 returns at most {n} results for one search; narrow the search to find older ones",
            &[("n", &GRAPH_SEARCH_LIMIT.to_string())],
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
