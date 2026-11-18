//! The background index (#429): loading a folder's newest mail, then the
//! rest of it a request at a time.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::models::{FolderKind, Message};
use crate::worker::ids::hash_uid;
use crate::worker::index_queue::{IndexJob, IndexQueue};
use crate::worker::jmap::folders::{jmap_list_folders, JmapFolder};
use crate::worker::jmap::messages::{jmap_get_messages, jmap_id_page, jmap_list_newest};
use crate::worker::jmap::wire::JmapSession;
use crate::worker::reconcile::{listing_gone, window_gone};
use crate::worker::{blocking, WorkerEvent};

/// Summaries one background step asks `Email/get` for.
const JMAP_BACKFILL_CHUNK: usize = 250;

/// How long a folder's id listing is trusted before opening the folder
/// lists it again, catching mail deleted elsewhere (#429).
pub(super) const JMAP_RELIST_AFTER: Duration = Duration::from_secs(15 * 60);

/// The pause after a background step fails, before the next try.
pub(super) const JMAP_BACKFILL_BACKOFF: Duration = Duration::from_secs(60);

/// A listing's `restarts` once the folder kept changing under it: listed
/// to the end all the same, but nothing is pruned by it.
const JMAP_UNSTEADY: u32 = u32::MAX;

/// Per-account state the JMAP loop threads through its handlers.
pub(super) struct JmapState {
    /// The session, once the server has been reached.
    pub(super) session: Option<JmapSession>,
    /// Hylki folder path → (folder id, mailbox id), from the last listing.
    pub(super) folders: HashMap<String, (u32, String)>,
    /// Message uid (hashed JMAP id) → (JMAP id, blob id).
    pub(super) uids: HashMap<u32, (String, String)>,
    /// The Drafts folder's (folder id, path).
    drafts: Option<(u32, String)>,
    /// The Inbox's (folder id, path), for the new-mail poll.
    pub(super) inbox: Option<(u32, String)>,
    /// The Sent folder's (folder id, path).
    pub(super) sent: Option<(u32, String)>,
    /// The account's manual Special Folders assignments (#82).
    pub(super) roles: BTreeMap<String, String>,
    /// The account's hidden folders (#239).
    pub(super) hidden: Vec<String>,
    /// The folders whose background index (#429) is still to run, the one
    /// at the front in progress.
    pub(super) backfill: std::collections::VecDeque<JmapBackfill>,
    /// When each folder's background index last finished, by path.
    listed: HashMap<String, std::time::Instant>,
    /// Message uid → JMAP id for mail an id listing named, whose blob id
    /// one `Email/get` gives when it is wanted.
    pub(super) listed_ids: HashMap<u32, String>,
    /// When a message lookup last listed each folder's ids, by path.
    pub(super) looked_up: HashMap<String, std::time::Instant>,
    /// No background step before this, after one failed.
    pub(super) backfill_wait: Option<std::time::Instant>,
    /// Each Sent folder's message count at the last listing (#675).
    pub(super) sent_totals: HashMap<String, u32>,
    /// Sent folders whose count moved, to list again on the next tick.
    pub(super) sent_due: Vec<(u32, String)>,
}

/// The background index of one folder (#429): its id listing a page a
/// step, then its missing mail a chunk a step.
pub(super) struct JmapBackfill {
    path: String,
    pub(super) phase: JmapBackfillPhase,
    /// Whether the list has already been told the folder's index is done.
    told: bool,
    /// Steps that failed in a row.
    failures: u32,
}

pub(super) enum JmapBackfillPhase {
    /// Paging through the ids, newest first, from a listing begun at
    /// `since`. `query_state` is the server's for the first page: a page
    /// under another state was cut from a different list, and starts the
    /// listing over (`restarts` times at most, then the pass prunes
    /// nothing).
    Listing {
        ids: Vec<String>,
        query_state: Option<String>,
        since: i64,
        restarts: u32,
    },
    Fetching {
        missing: std::collections::VecDeque<String>,
    },
}

impl JmapBackfill {
    pub(super) fn new(path: &str, told: bool) -> JmapBackfill {
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

impl IndexJob for JmapBackfill {
    fn start(path: &str, told: bool) -> Self {
        JmapBackfill::new(path, told)
    }

    fn path(&self) -> &str {
        &self.path
    }
}

impl IndexQueue for JmapState {
    type Job = JmapBackfill;
    const RELIST_AFTER: Duration = JMAP_RELIST_AFTER;

    fn jobs(&mut self) -> &mut VecDeque<JmapBackfill> {
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

impl JmapState {
    pub(super) fn new(account: &AccountConfig) -> JmapState {
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
            sent_totals: HashMap::new(),
            sent_due: Vec::new(),
        }
    }

    pub(super) fn adopt_folders(&mut self, list: &[JmapFolder]) {
        self.folders = list
            .iter()
            .map(|f| (f.folder.path.clone(), (f.folder.id, f.mailbox_id.clone())))
            .collect();
        let find = |kind: FolderKind| {
            list.iter()
                .find(|f| f.folder.kind == kind)
                .map(|f| (f.folder.id, f.folder.path.clone()))
        };
        self.drafts = find(FolderKind::Drafts);
        self.inbox = find(FolderKind::Inbox);
        self.sent = find(FolderKind::Sent);
        // Every folder is indexed in the background, as an IMAP account's
        // is, so search reaches the whole mailbox (#429).
        self.queue_all_backfills();
    }

    /// The path of a mailbox id, from the last listing.
    pub(super) fn path_of(&self, mailbox_id: &str) -> Option<(u32, String)> {
        self.folders
            .iter()
            .find(|(_, (_, id))| id == mailbox_id)
            .map(|(p, (fid, _))| (*fid, p.clone()))
    }
}

/// What a folder's complete id listing asks of the cache (#429): the rows
/// to drop, and the ids to fetch, newest first.
pub(super) fn jmap_index_plan(
    server: &[String],
    cached: &HashMap<u32, i64>,
    since: i64,
    prune: bool,
) -> (Vec<u32>, Vec<String>) {
    let uids: HashSet<u32> = server.iter().map(|id| hash_uid(id)).collect();
    let gone = if prune {
        listing_gone(cached, &uids, since)
    } else {
        Vec::new()
    };
    let mut seen = HashSet::new();
    let missing = server
        .iter()
        .filter(|id| !cached.contains_key(&hash_uid(id)) && seen.insert(id.as_str()))
        .cloned()
        .collect();
    (gone, missing)
}

/// List a folder's newest summaries, refresh the uid map and the cache, and
/// answer with the cached folder as it now stands.
pub(super) async fn jmap_load_folder(
    s: &JmapSession,
    account_id: u32,
    folder_id: u32,
    path: &str,
    cache: Option<&Cache>,
    state: &mut JmapState,
) -> Result<Vec<Message>, String> {
    jmap_sync_folder(s, account_id, folder_id, path, cache, state)
        .await
        .map(|(m, _)| m)
}

/// [`jmap_load_folder`], saying too whether the listing was the whole
/// folder (it fitted in [`JMAP_FIRST_PAGE`]).
pub(super) async fn jmap_sync_folder(
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
    let (_, mailbox_id) = state
        .folders
        .get(path)
        .cloned()
        .ok_or_else(|| format!("unknown folder {path}"))?;
    let sess = s.clone();
    let (listed, more) =
        blocking(move || jmap_list_newest(&sess, &mailbox_id, account_id, folder_id)).await?;
    let mut messages = Vec::with_capacity(listed.len());
    for (m, id, blob) in listed {
        state.uids.insert(m.uid, (id, blob));
        messages.push(m);
    }
    let Some(c) = cache else {
        return Ok((messages, !more));
    };
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

/// One step of the background index (#429): one request for the folder at
/// the front of the queue, a page of its ids or a chunk of its missing
/// mail, as on the Graph path. A failure leaves the job where it is and
/// pauses the index for a while.
pub(super) async fn jmap_backfill_step(
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
                emit(WorkerEvent::BackfillDone {
                    folder_id: *folder_id,
                });
            }
        }
    }
}

/// A folder's whole listing against the cache: its ids noted, the cached
/// rows the server no longer lists dropped (when `prune`) and the list sent
/// again, and the ids still to fetch.
fn settle_listing(
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut JmapState,
    path: &str,
    folder_id: u32,
    ids: &[String],
    since: i64,
    prune: bool,
    emit: &impl Fn(WorkerEvent),
) -> Vec<String> {
    state
        .listed_ids
        .extend(ids.iter().map(|id| (hash_uid(id), id.clone())));
    let cached = cache
        .map(|c| c.cached_uid_times(account_id, path))
        .unwrap_or_default();
    let (gone, missing) = jmap_index_plan(ids, &cached, since, prune);
    tracing::info!(
        "[account {account_id}] index of {}: {} on the server, {} to fetch, {} gone",
        path,
        ids.len(),
        missing.len(),
        gone.len()
    );
    if let (Some(c), false) = (cache, gone.is_empty()) {
        for uid in &gone {
            c.delete_message(account_id, path, *uid);
            state.uids.remove(uid);
        }
        emit(WorkerEvent::Messages {
            folder_id,
            messages: c.load_messages(account_id, path, folder_id),
        });
    }
    missing
}

/// [`jmap_backfill_step`]'s work; false when the request failed.
pub(super) async fn jmap_backfill_with(
    s: &JmapSession,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
) -> bool {
    let Some(mut job) = state.backfill.pop_front() else {
        return true;
    };
    let Some((folder_id, mailbox_id)) = state.folders.get(&job.path).cloned() else {
        return true;
    };
    match &mut job.phase {
        JmapBackfillPhase::Listing {
            ids,
            query_state,
            since,
            restarts,
        } => {
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
            let missing = settle_listing(
                account_id, cache, state, &job.path, folder_id, ids, *since, prune, emit,
            );
            job.phase = JmapBackfillPhase::Fetching {
                missing: missing.into(),
            };
        }
        JmapBackfillPhase::Fetching { missing } => {
            let chunk: Vec<String> = missing.iter().take(JMAP_BACKFILL_CHUNK).cloned().collect();
            let sess = s.clone();
            let ids = chunk.clone();
            let fetched =
                match blocking(move || jmap_get_messages(&sess, &ids, account_id, folder_id)).await
                {
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
                emit(WorkerEvent::MessagesAppend {
                    folder_id,
                    messages,
                });
            }
        }
    }
    if matches!(&job.phase, JmapBackfillPhase::Fetching { missing } if missing.is_empty()) {
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
