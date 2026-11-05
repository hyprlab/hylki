//! A Tuta account's state, its folder listings and the background index
//! (#429): a folder's newest page first, then the rest of it a page at a
//! time, in runs of missing mail.
//!
//! Every helper call is a process that signs in again, so a step is one
//! call: the first page of a sync carries the folder's complete id
//! snapshot, which says both what the cache lacks and what the server no
//! longer has.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::models::{Folder, FolderKind, Message};
use crate::worker::helper::{process_transport, HelperClient, HelperError, Transport};
use crate::worker::ids::hash_uid;
use crate::worker::index_queue::{IndexJob, IndexQueue};
use crate::worker::reconcile::listing_gone;
use crate::worker::tuta::folders::{tuta_folders, TutaFolder};
use crate::worker::tuta::messages::{header_labels, tuta_message};
use crate::worker::tuta::wire::{sync, LabelRow, SyncPage, SPEC};
use crate::worker::WorkerEvent;

/// The newest headers a folder load lists, for a quick first paint. The
/// helper loads each header with a request of its own, so a page is kept
/// well inside its 90-second limit.
pub(super) const TUTA_FIRST_PAGE: usize = 100;

/// Headers one background step asks for.
pub(super) const TUTA_BACKFILL_CHUNK: usize = 200;

/// How long a folder's index is trusted before opening it indexes it
/// again, catching mail deleted elsewhere.
const TUTA_RELIST_AFTER: Duration = Duration::from_secs(15 * 60);

/// The pause after a background step fails, before the next try.
const TUTA_BACKFILL_BACKOFF: Duration = Duration::from_secs(60);

/// Per-account state the Tuta loop threads through its handlers.
pub(super) struct TutaState {
    /// Hylki folder path → (folder id, Tuta folder id), from the last sync.
    pub(super) folders: HashMap<String, (u32, String)>,
    /// Each listed folder's kind, by path.
    pub(super) kinds: HashMap<String, FolderKind>,
    /// Message uid (hashed compound id) → compound id.
    pub(super) uids: HashMap<u32, String>,
    /// The Drafts folder's (folder id, path), for its chip count.
    drafts: Option<(u32, String)>,
    /// The account's manual Special Folders assignments (#82).
    pub(super) roles: BTreeMap<String, String>,
    /// The account's hidden folders (#239).
    pub(super) hidden: Vec<String>,
    /// How the helper is reached: the installed one, or a test's fake.
    pub(super) transport: Transport,
    /// The folders whose background index is still to run, the one at the
    /// front in progress.
    pub(super) backfill: VecDeque<TutaBackfill>,
    /// When each folder's background index last finished, by path.
    pub(super) listed: HashMap<String, Instant>,
    /// No background step before this, after one failed.
    pub(super) backfill_wait: Option<Instant>,
    /// The account's labels by id, from the last sync that listed them.
    pub(super) labels: HashMap<String, LabelRow>,
}

impl TutaState {
    pub(super) fn new(account: &AccountConfig) -> TutaState {
        TutaState {
            folders: HashMap::new(),
            kinds: HashMap::new(),
            uids: HashMap::new(),
            drafts: None,
            roles: account.folder_roles.clone(),
            hidden: account.hidden_folders.clone(),
            transport: process_transport(SPEC),
            backfill: VecDeque::new(),
            listed: HashMap::new(),
            backfill_wait: None,
            labels: HashMap::new(),
        }
    }

    /// A client for the account's stored session.
    pub(super) fn client(&self, account: &AccountConfig) -> Result<HelperClient, HelperError> {
        HelperClient::new(
            SPEC,
            self.transport.clone(),
            &account.email,
            &account.helper_session,
        )
    }

    /// The path of the folder of `kind`, if the account has one.
    pub(super) fn path_of(&self, kind: FolderKind) -> Option<String> {
        let mut paths: Vec<&String> = self
            .kinds
            .iter()
            .filter(|(_, k)| **k == kind)
            .map(|(p, _)| p)
            .collect();
        paths.sort();
        paths.first().map(|p| p.to_string())
    }

    pub(super) fn adopt_folders(&mut self, list: &[TutaFolder]) {
        self.folders = list
            .iter()
            .map(|f| (f.folder.path.clone(), (f.folder.id, f.tuta_id.clone())))
            .collect();
        self.kinds = list
            .iter()
            .map(|f| (f.folder.path.clone(), f.folder.kind))
            .collect();
        self.drafts = list
            .iter()
            .find(|f| f.folder.kind == FolderKind::Drafts)
            .map(|f| (f.folder.id, f.folder.path.clone()));
        self.queue_all_backfills();
    }
}

impl IndexQueue for TutaState {
    type Job = TutaBackfill;
    const RELIST_AFTER: Duration = TUTA_RELIST_AFTER;

    fn jobs(&mut self) -> &mut VecDeque<TutaBackfill> {
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

/// The background index of one folder: a snapshot of its ids, then its
/// runs of missing mail, a page a step.
pub(super) struct TutaBackfill {
    pub(super) path: String,
    /// Runs still to fetch as (offset, length) in the folder's newest-first
    /// order; `None` until the snapshot is taken.
    runs: Option<VecDeque<(usize, usize)>>,
    /// Whether the list has already been told the folder's index is done.
    told: bool,
    /// Steps that failed in a row.
    failures: u32,
}

impl IndexJob for TutaBackfill {
    fn start(path: &str, told: bool) -> Self {
        TutaBackfill {
            path: path.to_string(),
            runs: None,
            told,
            failures: 0,
        }
    }

    fn path(&self) -> &str {
        &self.path
    }
}

/// What a folder's complete id snapshot (newest first) asks of the cache:
/// the rows to drop, and the runs of mail to fetch as (offset, length),
/// none longer than [`TUTA_BACKFILL_CHUNK`]. `since` is when the snapshot
/// was asked for.
pub(super) fn tuta_index_plan(
    ids: &[String],
    cached: &HashMap<u32, i64>,
    since: i64,
) -> (Vec<u32>, Vec<(usize, usize)>) {
    let server: HashSet<u32> = ids.iter().map(|id| hash_uid(id)).collect();
    let gone = listing_gone(cached, &server, since);
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (at, id) in ids.iter().enumerate() {
        if cached.contains_key(&hash_uid(id)) {
            continue;
        }
        match runs.last_mut() {
            Some((start, len)) if *start + *len == at && *len < TUTA_BACKFILL_CHUNK => *len += 1,
            _ => runs.push((at, 1)),
        }
    }
    (gone, runs)
}

/// The rows of a page, with what only the cache knows carried over the
/// listing: Hylki's own star (Tuta has none) and the Message-ID,
/// References and preview a read body filled in (see
/// `Cache::set_threading`). A body's Message-ID wins over the listed one:
/// it came with the References the listing lacks.
pub(super) fn carry_over(listed: &mut [Message], cached: &[Message]) {
    let by_uid: HashMap<u32, &Message> = cached.iter().map(|m| (m.uid, m)).collect();
    for m in listed {
        let Some(old) = by_uid.get(&m.uid) else {
            continue;
        };
        m.starred = old.starred;
        if !old.message_id.is_empty() {
            m.message_id = old.message_id.clone();
            m.references = old.references.clone();
        }
        if m.preview.is_empty() {
            m.preview = old.preview.clone();
        }
    }
}

/// A page's labels, adopted when it lists them.
fn adopt_labels(page: &SyncPage, state: &mut TutaState) {
    if let Some(labels) = &page.labels {
        state.labels = labels.iter().map(|l| (l.id.clone(), l.clone())).collect();
    }
}

/// A page's headers as rows of `folder_id`, their ids noted in the uid map
/// and their labels named.
pub(super) fn page_messages(
    page: &SyncPage,
    account_id: u32,
    folder_id: u32,
    state: &mut TutaState,
) -> Vec<Message> {
    adopt_labels(page, state);
    let mut messages = Vec::with_capacity(page.headers.len());
    for header in &page.headers {
        if let Some((mut m, id)) = tuta_message(header, account_id, folder_id) {
            m.labels = header_labels(header, &state.labels);
            state.uids.insert(m.uid, id);
            messages.push(m);
        }
    }
    messages
}

/// Write a page's rows into the cache, over what it had: the star and
/// what a read body showed survive.
fn merge_page(
    cache: &Cache,
    account_id: u32,
    folder_id: u32,
    path: &str,
    mut messages: Vec<Message>,
) -> Vec<Message> {
    carry_over(
        &mut messages,
        &cache.load_messages(account_id, path, folder_id),
    );
    cache.upsert_messages(account_id, path, &messages);
    messages
}

/// The folder list a sync carried, adopted, saved and sent to the UI when
/// it changed, and its labels adopted.
pub(super) fn take_folders(
    page: &SyncPage,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut TutaState,
    emit: &impl Fn(WorkerEvent),
) {
    adopt_labels(page, state);
    let cached = cache
        .map(|c| c.load_folders(account_id))
        .unwrap_or_default();
    let list = tuta_folders(
        &page.folders,
        account_id,
        &state.roles,
        &state.hidden,
        &cached,
    );
    if list.is_empty() {
        return;
    }
    state.adopt_folders(&list);
    let folders: Vec<Folder> = list.into_iter().map(|f| f.folder).collect();
    if crate::cache::folders_equal(&folders, &cached) {
        return;
    }
    if let Some(c) = cache {
        c.save_folders(account_id, &folders);
    }
    emit(WorkerEvent::Folders(folders));
}

/// A first page (offset zero) of a folder into the cache: the rows it
/// lists, and the cached rows its id snapshot no longer names dropped.
/// The folder as the cache now holds it, and whether the page was all of
/// it.
pub(super) fn apply_first_page(
    page: &SyncPage,
    account_id: u32,
    folder_id: u32,
    path: &str,
    since: i64,
    cache: Option<&Cache>,
    state: &mut TutaState,
) -> (Vec<Message>, bool) {
    let messages = page_messages(page, account_id, folder_id, state);
    let complete = page
        .all_ids
        .as_ref()
        .is_some_and(|ids| ids.len() <= messages.len());
    if let Some(ids) = &page.all_ids {
        for id in ids {
            state.uids.insert(hash_uid(id), id.clone());
        }
    }
    let Some(c) = cache else {
        return (messages, complete);
    };
    if let Some(ids) = &page.all_ids {
        let server: HashSet<u32> = ids.iter().map(|id| hash_uid(id)).collect();
        let cached = c.cached_uid_times(account_id, path);
        for uid in listing_gone(&cached, &server, since) {
            c.delete_message(account_id, path, uid);
            state.uids.remove(&uid);
        }
    }
    merge_page(c, account_id, folder_id, path, messages);
    (c.load_messages(account_id, path, folder_id), complete)
}

/// List a folder's newest page and fold it into the cache: the folder as
/// it now stands, and whether the page was all of it.
pub(super) async fn tuta_sync_folder(
    account: &AccountConfig,
    account_id: u32,
    folder_id: u32,
    path: &str,
    cache: Option<&Cache>,
    state: &mut TutaState,
    emit: &impl Fn(WorkerEvent),
) -> Result<(Vec<Message>, bool), HelperError> {
    let (_, tuta_id) = state
        .folders
        .get(path)
        .cloned()
        .ok_or_else(|| HelperError::new("folder_not_found"))?;
    let client = state.client(account)?;
    let since = crate::datefmt::now();
    let page = run(move || sync(&client, Some(&tuta_id), TUTA_FIRST_PAGE, 0)).await?;
    take_folders(&page, account_id, cache, state, emit);
    // The folder ids may have been numbered again.
    let folder_id = state.folders.get(path).map_or(folder_id, |(id, _)| *id);
    Ok(apply_first_page(
        &page, account_id, folder_id, path, since, cache, state,
    ))
}

/// [`tuta_sync_folder`], the folder alone.
pub(super) async fn tuta_load_folder(
    account: &AccountConfig,
    account_id: u32,
    folder_id: u32,
    path: &str,
    cache: Option<&Cache>,
    state: &mut TutaState,
    emit: &impl Fn(WorkerEvent),
) -> Result<Vec<Message>, HelperError> {
    tuta_sync_folder(account, account_id, folder_id, path, cache, state, emit)
        .await
        .map(|(messages, _)| messages)
}

/// Run blocking helper work off the async threads; a task that panicked
/// reads as a failed helper.
pub(super) async fn run<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, HelperError> + Send + 'static,
) -> Result<T, HelperError> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|_| Err(HelperError::new("helper_failed")))
}

/// One step of the background index: one helper call for the folder at
/// the front of the queue. Failures back off, and after a few in a row
/// the list stops promising more (#218).
pub(super) async fn tuta_backfill_step(
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut TutaState,
    emit: &impl Fn(WorkerEvent),
) {
    if tuta_backfill_with(account, account_id, cache, state, emit).await {
        state.backfill_wait = None;
        return;
    }
    state.backfill_wait = Some(Instant::now() + TUTA_BACKFILL_BACKOFF);
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

/// [`tuta_backfill_step`]'s work; false when the call failed, the job then
/// still at the front of the queue.
pub(super) async fn tuta_backfill_with(
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut TutaState,
    emit: &impl Fn(WorkerEvent),
) -> bool {
    let Some(mut job) = state.backfill.pop_front() else {
        return true;
    };
    // A folder gone or hidden since it was queued has nothing left to index.
    let Some((folder_id, tuta_id)) = state.folders.get(&job.path).cloned() else {
        return true;
    };
    let client = match state.client(account) {
        Ok(client) => client,
        Err(_) => {
            state.backfill.push_front(job);
            return false;
        }
    };
    match job.runs.as_mut() {
        None => {
            let since = crate::datefmt::now();
            let t = tuta_id.clone();
            let page = match run(move || sync(&client, Some(&t), TUTA_BACKFILL_CHUNK, 0)).await {
                Ok(page) => page,
                Err(e) => {
                    tracing::warn!("[account {account_id}] index of {}: {}", job.path, e.code);
                    state.backfill.push_front(job);
                    return false;
                }
            };
            let Some(ids) = page.all_ids.clone() else {
                tracing::warn!(
                    "[account {account_id}] index of {}: no id snapshot",
                    job.path
                );
                state.backfill.push_front(job);
                return false;
            };
            let messages = page_messages(&page, account_id, folder_id, state);
            for id in &ids {
                state.uids.insert(hash_uid(id), id.clone());
            }
            let mut runs = VecDeque::new();
            if let Some(c) = cache {
                if !messages.is_empty() {
                    merge_page(c, account_id, folder_id, &job.path, messages);
                }
                let cached = c.cached_uid_times(account_id, &job.path);
                let (gone, missing) = tuta_index_plan(&ids, &cached, since);
                tracing::info!(
                    "[account {account_id}] index of {}: {} on the server, {} to fetch in {} run(s), {} gone",
                    job.path,
                    ids.len(),
                    missing.iter().map(|(_, n)| n).sum::<usize>(),
                    missing.len(),
                    gone.len()
                );
                for uid in &gone {
                    c.delete_message(account_id, &job.path, *uid);
                    state.uids.remove(uid);
                }
                emit(WorkerEvent::Messages {
                    folder_id,
                    messages: c.load_messages(account_id, &job.path, folder_id),
                });
                runs.extend(missing);
            }
            job.failures = 0;
            job.runs = Some(runs);
        }
        Some(runs) => {
            let Some((offset, len)) = runs.front().copied() else {
                return true;
            };
            let t = tuta_id.clone();
            let page = match run(move || sync(&client, Some(&t), len, offset)).await {
                Ok(page) => page,
                Err(e) => {
                    tracing::warn!("[account {account_id}] index of {}: {}", job.path, e.code);
                    state.backfill.push_front(job);
                    return false;
                }
            };
            runs.pop_front();
            job.failures = 0;
            let messages = page_messages(&page, account_id, folder_id, state);
            if let (Some(c), false) = (cache, messages.is_empty()) {
                let messages = merge_page(c, account_id, folder_id, &job.path, messages);
                emit(WorkerEvent::MessagesAppend {
                    folder_id,
                    messages,
                });
            }
        }
    }
    if job.runs.as_ref().is_some_and(VecDeque::is_empty) {
        state.listed.insert(job.path.clone(), Instant::now());
        if !job.told {
            emit(WorkerEvent::BackfillDone { folder_id });
        }
    } else {
        state.backfill.push_front(job);
    }
    true
}
