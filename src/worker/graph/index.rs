//! The background index (#429): loading a folder's newest mail, then the
//! rest of it a request at a time, in runs of missing mail.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::models::{FolderKind, Message};
use crate::worker::graph::folders::{graph_list_folders, GraphFolder};
use crate::worker::graph::http::{graph_get_json_opt, graph_paged, graph_token, GRAPH_BASE};
use crate::worker::graph::messages::{
    graph_id_page, graph_ids_url, graph_list_messages, graph_message, GRAPH_MSG_SELECT,
};
use crate::worker::ids::hash_uid;
use crate::worker::index_queue::{IndexJob, IndexQueue};
use crate::worker::reconcile::{listing_gone, window_gone};
use crate::worker::{blocking, WorkerEvent};

/// Summaries one background step asks for (#429): one request's worth,
/// so a click is never queued behind more than that.
pub(super) const GRAPH_BACKFILL_CHUNK: usize = 500;

/// A run of missing mail shorter than this is fetched message by message
/// rather than by its dates.
const GRAPH_RANGE_MIN: usize = 3;

/// How long a folder's id listing is trusted before opening the folder
/// lists it again, catching mail deleted elsewhere (#429).
pub(super) const GRAPH_RELIST_AFTER: Duration = Duration::from_secs(15 * 60);

/// The pause after a background step fails, before the next try.
const GRAPH_BACKFILL_BACKOFF: Duration = Duration::from_secs(60);

/// A stretch of mail the cache lacks (#429), consecutive in the folder's
/// newest-first order, with the arrival times that bound it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct GraphRun {
    pub(super) ids: Vec<String>,
    pub(super) newest: String,
    pub(super) oldest: String,
}

/// What a folder's complete id listing (newest first) asks of the cache:
/// the rows to drop, and the runs of mail to fetch, newest first, none
/// longer than [`GRAPH_BACKFILL_CHUNK`]. `since` is when the listing began.
pub(super) fn graph_index_plan(
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
            _ => runs.push(GraphRun {
                ids: vec![id.clone()],
                newest: at.clone(),
                oldest: at.clone(),
            }),
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
    let dated =
        run.ids.len() >= GRAPH_RANGE_MIN && !run.newest.is_empty() && !run.oldest.is_empty();
    let items = if dated {
        let url = format!(
            "{base}/me/mailFolders/{folder_graph_id}/messages\
             ?$filter=receivedDateTime%20ge%20{}%20and%20receivedDateTime%20le%20{}\
             &$orderby=receivedDateTime%20desc&$top={GRAPH_BACKFILL_CHUNK}&{GRAPH_MSG_SELECT}",
            crate::encoding::percent::encode(&run.oldest),
            crate::encoding::percent::encode(&run.newest),
        );
        // Bounded, should many messages share the run's end dates.
        graph_paged(token, &url, run.ids.len() + GRAPH_BACKFILL_CHUNK)?
    } else {
        let mut items = Vec::new();
        for id in &run.ids {
            let url = format!("{base}/me/messages/{id}?{GRAPH_MSG_SELECT}");
            items.extend(graph_get_json_opt(token, &url)?);
        }
        items
    };
    Ok(items
        .iter()
        .filter_map(|v| graph_message(v, account_id, folder_id))
        .collect())
}

/// The background index of one folder (#429): its id-only listing a page a
/// step, then its runs of missing mail a run a step.
pub(super) struct GraphBackfill {
    pub(super) path: String,
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
    Listing {
        next: Option<String>,
        ids: Vec<(String, String)>,
        since: i64,
    },
    Fetching {
        runs: std::collections::VecDeque<GraphRun>,
    },
}

impl GraphBackfill {
    pub(super) fn new(path: &str, told: bool) -> GraphBackfill {
        GraphBackfill {
            path: path.to_string(),
            phase: GraphBackfillPhase::Listing {
                next: None,
                ids: Vec::new(),
                since: crate::datefmt::now(),
            },
            told,
            failures: 0,
        }
    }
}

impl IndexJob for GraphBackfill {
    fn start(path: &str, told: bool) -> Self {
        GraphBackfill::new(path, told)
    }

    fn path(&self) -> &str {
        &self.path
    }
}

impl IndexQueue for GraphState {
    type Job = GraphBackfill;
    const RELIST_AFTER: Duration = GRAPH_RELIST_AFTER;

    fn jobs(&mut self) -> &mut VecDeque<GraphBackfill> {
        &mut self.backfill
    }

    fn listed(&mut self) -> &mut HashMap<String, Instant> {
        &mut self.listed
    }

    fn folder_ids(&self) -> Vec<(u32, String)> {
        self.folders
            .iter()
            .map(|(path, (id, _))| (*id, path.clone()))
            .collect()
    }

    fn hidden(&self) -> &[String] {
        &self.hidden
    }

    fn drafts(&self) -> Option<&(u32, String)> {
        self.drafts.as_ref()
    }
}

/// Per-account state the Graph loop threads through its handlers.
pub(super) struct GraphState {
    /// Hylki folder path → (folder id, Graph folder id), from the last listing.
    pub(super) folders: std::collections::HashMap<String, (u32, String)>,
    /// Message uid (hashed Graph id) → Graph message id.
    pub(super) uids: std::collections::HashMap<u32, String>,
    /// The Drafts folder's (folder id, path), for draft reloads after a send.
    drafts: Option<(u32, String)>,
    /// The Inbox's (folder id, path), for the new-mail poll.
    pub(super) inbox: Option<(u32, String)>,
    /// The account's manual Special Folders assignments (#82), applied to
    /// every listing.
    pub(super) roles: std::collections::BTreeMap<String, String>,
    /// The account's hidden folders (#239), left out of every listing.
    pub(super) hidden: Vec<String>,
    /// Where Graph is: [`GRAPH_BASE`], or a test's own server.
    pub(super) base: String,
    /// The folders whose background index (#429) is still to run, the one
    /// at the front in progress.
    pub(super) backfill: std::collections::VecDeque<GraphBackfill>,
    /// When each folder's background index last finished, by path.
    pub(super) listed: std::collections::HashMap<String, std::time::Instant>,
    /// When a message lookup last listed each folder's ids, by path.
    pub(super) looked_up: std::collections::HashMap<String, std::time::Instant>,
    /// No background step before this, after one failed.
    pub(super) backfill_wait: Option<std::time::Instant>,
    /// Each Sent folder's message count at the last listing (#675).
    pub(super) sent_totals: std::collections::HashMap<String, u32>,
    /// Sent folders whose count moved, to list again on the next tick.
    pub(super) sent_due: Vec<(u32, String)>,
}

impl GraphState {
    pub(super) fn new(account: &AccountConfig) -> GraphState {
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
            sent_totals: Default::default(),
            sent_due: Vec::new(),
        }
    }

    pub(super) fn adopt_folders(&mut self, list: &[GraphFolder]) {
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

/// List a folder's newest summaries, refresh the uid map and the cache, and
/// answer with the cached folder as it now stands.
pub(super) async fn graph_load_folder(
    token: &str,
    account_id: u32,
    folder_id: u32,
    path: &str,
    cache: Option<&Cache>,
    state: &mut GraphState,
) -> Result<Vec<Message>, String> {
    graph_sync_folder(token, account_id, folder_id, path, cache, state)
        .await
        .map(|(m, _)| m)
}

/// [`graph_load_folder`], saying too whether the listing was the whole
/// folder (it fitted in [`GRAPH_FIRST_PAGE`]).
pub(super) async fn graph_sync_folder(
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
        if let Ok(list) = blocking(move || graph_list_folders(&t, account_id, &roles)).await {
            state.adopt_folders(&list);
        }
    }
    let (_, gid) = state
        .folders
        .get(path)
        .cloned()
        .ok_or_else(|| format!("unknown folder {path}"))?;

    let (t, base) = (token.to_string(), state.base.clone());
    let (listed, more) =
        blocking(move || graph_list_messages(&t, &base, &gid, account_id, folder_id)).await?;

    let mut messages = Vec::with_capacity(listed.len());
    for (m, gid) in listed {
        state.uids.insert(m.uid, gid);
        messages.push(m);
    }
    let Some(c) = cache else {
        return Ok((messages, !more));
    };
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

/// One step of the background index (#429): one request for the folder at
/// the front of the queue, a page of its ids or a run of its missing mail.
/// The cache gains what came, and the list hears of it as an append; the
/// folder's index is done once its last run is in.
///
/// The id listing, rather than a delta query, is what tells the cache what
/// the server no longer has: it is the listing every folder load already
/// makes, asked for ids alone, and keeps no sync state to store, expire or
/// recover from.
pub(super) async fn graph_backfill_step(
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
    if let Some(job) = state.backfill.front_mut() {
        job.failures += 1;
        // Offline, or refused: after a few tries the list stops promising
        // more (#218), and the index carries on when it can.
        if job.failures >= 3 && !job.told {
            job.told = true;
            if let Some((folder_id, _)) = state.folders.get(&job.path) {
                emit(WorkerEvent::BackfillDone {
                    folder_id: *folder_id,
                });
            }
        }
    }
}

/// [`graph_backfill_step`] with its token; false when the request failed,
/// the job then still at the front of the queue to try again.
pub(super) async fn graph_backfill_with(
    token: &str,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut GraphState,
    emit: &impl Fn(WorkerEvent),
) -> bool {
    let Some(mut job) = state.backfill.pop_front() else {
        return true;
    };
    // A folder gone or hidden since it was queued has nothing left to index.
    let Some((folder_id, gid)) = state.folders.get(&job.path).cloned() else {
        return true;
    };
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
            let cached = cache
                .map(|c| c.cached_uid_times(account_id, &job.path))
                .unwrap_or_default();
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
                emit(WorkerEvent::Messages {
                    folder_id,
                    messages: c.load_messages(account_id, &job.path, folder_id),
                });
            }
            job.failures = 0;
            job.phase = GraphBackfillPhase::Fetching { runs: runs.into() };
        }
        GraphBackfillPhase::Fetching { runs } => {
            let Some(run) = runs.front().cloned() else {
                return true;
            };
            let t = token.to_string();
            let fetched =
                blocking(move || graph_fetch_run(&t, &base, &gid, &run, account_id, folder_id))
                    .await;
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
                emit(WorkerEvent::MessagesAppend {
                    folder_id,
                    messages,
                });
            }
        }
    }
    if matches!(&job.phase, GraphBackfillPhase::Fetching { runs } if runs.is_empty()) {
        state
            .listed
            .insert(job.path.clone(), std::time::Instant::now());
        if !job.told {
            emit(WorkerEvent::BackfillDone { folder_id });
        }
    } else {
        state.backfill.push_front(job);
    }
    true
}
