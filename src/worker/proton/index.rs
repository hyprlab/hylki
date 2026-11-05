//! A Proton account's state, its folder listings, the event log that keeps
//! them current, and the background index (#429): a folder's newest page
//! first, then the rest of it a page at a time, in runs of missing mail.
//!
//! Every helper call is a process of its own, so a step is one call. A
//! folder's first listing carries its complete id snapshot, which says what
//! the cache lacks and what the server no longer has. After that, a listing
//! asked `since` the last event reports what changed anywhere in the
//! mailbox, and a changed message lands in every folder whose label it
//! carries and leaves the ones it no longer does: a message's id is the
//! same in every folder and across moves.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::cache::Cache;
use crate::config::{AccountConfig, LiveSession};
use crate::models::{Folder, FolderKind, Message};
use crate::worker::helper::{process_transport, HelperClient, HelperError, Transport};
use crate::worker::ids::hash_uid;
use crate::worker::index_queue::{snapshot_plan, IndexJob, IndexQueue};
use crate::worker::proton::folders::{proton_folders, ProtonFolder};
use crate::worker::proton::messages::{label_ids, proton_message};
use crate::worker::proton::wire::{sync, Changes, Start, SyncPage, SPEC};
use crate::worker::reconcile::listing_gone;
use crate::worker::WorkerEvent;

/// The newest headers a folder load lists, for a quick first paint.
pub(super) const PROTON_FIRST_PAGE: usize = 100;

/// Headers one background step asks for.
pub(super) const PROTON_BACKFILL_CHUNK: usize = 200;

/// How long a folder's index is trusted before opening it indexes it
/// again. The event log reports deletions, so this only catches what a
/// lost event dropped.
const PROTON_RELIST_AFTER: Duration = Duration::from_secs(15 * 60);

/// The pause after a background step fails, before the next try.
const PROTON_BACKFILL_BACKOFF: Duration = Duration::from_secs(60);

/// How many pages of the event log one poll follows; the rest wait for the
/// next poll, so a long absence does not hold up the user's requests.
pub(super) const EVENT_ROUNDS: usize = 5;

/// Per-account state the Proton loop threads through its handlers.
pub(super) struct ProtonState {
    /// Hylki folder path → (folder id, Proton label id), from the last sync.
    pub(super) folders: HashMap<String, (u32, String)>,
    /// Each listed folder's kind, by path.
    pub(super) kinds: HashMap<String, FolderKind>,
    /// Message uid (hashed Proton id) → Proton id.
    pub(super) uids: HashMap<u32, String>,
    /// The Drafts folder's (folder id, path), for its chip count.
    drafts: Option<(u32, String)>,
    /// The account's manual Special Folders assignments (#82).
    pub(super) roles: BTreeMap<String, String>,
    /// The account's hidden folders (#239).
    pub(super) hidden: Vec<String>,
    /// How the helper is reached: the installed one, or a test's fake.
    pub(super) transport: Transport,
    /// The account's session, shared with everything else that calls the
    /// helper for it (see [`LiveSession`]).
    pub(super) live: Arc<LiveSession>,
    /// The event the next listing asks changes `since`; `None` until the
    /// first listing of this run.
    pub(super) event_id: Option<String>,
    /// The folders whose background index is still to run, the one at the
    /// front in progress.
    pub(super) backfill: VecDeque<ProtonBackfill>,
    /// When each folder's background index last finished, by path.
    pub(super) listed: HashMap<String, Instant>,
    /// No background step before this, after one failed.
    pub(super) backfill_wait: Option<Instant>,
}

impl ProtonState {
    pub(super) fn new(account: &AccountConfig) -> ProtonState {
        ProtonState {
            folders: HashMap::new(),
            kinds: HashMap::new(),
            uids: HashMap::new(),
            drafts: None,
            roles: account.folder_roles.clone(),
            hidden: account.hidden_folders.clone(),
            transport: process_transport(SPEC),
            live: crate::config::live_session(&account.email),
            event_id: None,
            backfill: VecDeque::new(),
            listed: HashMap::new(),
            backfill_wait: None,
        }
    }

    /// A client for the account's live session, seeded from the keyring's
    /// copy when this run has none yet.
    pub(super) fn client(&self, account: &AccountConfig) -> Result<HelperClient, HelperError> {
        HelperClient::shared(
            SPEC,
            self.transport.clone(),
            &account.email,
            self.live.clone(),
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

    /// The (folder id, path) of the folder Proton knows as `label`.
    pub(super) fn folder_of_label(&self, label: &str) -> Option<(u32, String)> {
        self.folders
            .iter()
            .find(|(_, (_, l))| l == label)
            .map(|(path, (id, _))| (*id, path.clone()))
    }

    pub(super) fn adopt_folders(&mut self, list: &[ProtonFolder]) {
        self.folders = list
            .iter()
            .map(|f| (f.folder.path.clone(), (f.folder.id, f.label_id.clone())))
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

    /// The server asked for everything to be listed again: every folder's
    /// index runs anew and fetches every header, not only the missing ones,
    /// so read marks and stars it missed are put right too.
    fn relist_all(&mut self) {
        self.listed.clear();
        self.queue_all_backfills();
        for job in &mut self.backfill {
            job.full = true;
            job.plan = None;
        }
    }
}

impl IndexQueue for ProtonState {
    type Job = ProtonBackfill;
    const RELIST_AFTER: Duration = PROTON_RELIST_AFTER;

    fn jobs(&mut self) -> &mut VecDeque<ProtonBackfill> {
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

/// How the rest of a folder is fetched after its first page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Plan {
    /// Runs of missing mail as (offset, length), from the id snapshot.
    Runs(VecDeque<(usize, usize)>),
    /// A folder too large for a snapshot, walked page after page from the
    /// last id listed.
    Walk(String),
}

/// The background index of one folder: its first page and id snapshot,
/// then its runs of missing mail, a page a step.
pub(super) struct ProtonBackfill {
    pub(super) path: String,
    /// `None` until the first page is listed.
    pub(super) plan: Option<Plan>,
    /// Fetch every header, not only the ones the cache lacks.
    pub(super) full: bool,
    /// Whether the list has already been told the folder's index is done.
    told: bool,
    /// Steps that failed in a row.
    failures: u32,
}

impl IndexJob for ProtonBackfill {
    fn start(path: &str, told: bool) -> Self {
        ProtonBackfill {
            path: path.to_string(),
            plan: None,
            full: false,
            told,
            failures: 0,
        }
    }

    fn path(&self) -> &str {
        &self.path
    }
}

/// The rows of a page, with what only a read body showed carried over the
/// listing: the References and the preview (see `Cache::set_threading`).
/// Read state and the star are the server's.
pub(super) fn carry_over(listed: &mut [Message], cached: &[Message]) {
    let by_uid: HashMap<u32, &Message> = cached.iter().map(|m| (m.uid, m)).collect();
    for m in listed {
        let Some(old) = by_uid.get(&m.uid) else {
            continue;
        };
        if m.message_id.is_empty() {
            m.message_id = old.message_id.clone();
        }
        if m.references.is_empty() {
            m.references = old.references.clone();
        }
        if m.preview.is_empty() {
            m.preview = old.preview.clone();
        }
    }
}

/// Headers as rows of `folder_id`, their ids noted in the uid map.
pub(super) fn page_messages(
    headers: &[serde_json::Value],
    account_id: u32,
    folder_id: u32,
    state: &mut ProtonState,
) -> Vec<Message> {
    let mut messages = Vec::with_capacity(headers.len());
    for header in headers {
        if let Some((m, id)) = proton_message(header, account_id, folder_id) {
            state.uids.insert(m.uid, id);
            messages.push(m);
        }
    }
    messages
}

/// Write rows into the cache, over what it had: what a read body showed
/// survives.
fn merge_rows(
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
/// it changed.
pub(super) fn take_folders(
    page: &SyncPage,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut ProtonState,
    emit: &impl Fn(WorkerEvent),
) {
    let list = proton_folders(&page.folders, account_id, &state.roles, &state.hidden);
    if list.is_empty() {
        return;
    }
    state.adopt_folders(&list);
    let folders: Vec<Folder> = list.into_iter().map(|f| f.folder).collect();
    let cached = cache
        .map(|c| c.load_folders(account_id))
        .unwrap_or_default();
    if crate::cache::folders_equal(&folders, &cached) {
        return;
    }
    if let Some(c) = cache {
        c.save_folders(account_id, &folders);
    }
    emit(WorkerEvent::Folders(folders));
}

/// Fold what changed since the last event into the cache: a deleted
/// message leaves every folder, a changed one is written into each folder
/// whose label it carries and leaves the others (a move, an unstar, a
/// message archived). The folders touched, as (folder id, path).
pub(super) fn apply_changes(
    changes: &Changes,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut ProtonState,
) -> Vec<(u32, String)> {
    let deleted: Vec<u32> = changes.deleted.iter().map(|id| hash_uid(id)).collect();
    for uid in &deleted {
        state.uids.remove(uid);
    }
    let mut updated: Vec<(Message, Vec<String>)> = Vec::with_capacity(changes.updated.len());
    for header in &changes.updated {
        if let Some((message, id)) = proton_message(header, account_id, 0) {
            state.uids.insert(message.uid, id);
            let labels = label_ids(header).into_iter().map(str::to_string).collect();
            updated.push((message, labels));
        }
    }
    let Some(c) = cache else {
        return Vec::new();
    };
    if deleted.is_empty() && updated.is_empty() {
        return Vec::new();
    }
    let mut touched = Vec::new();
    for (path, (folder_id, label)) in &state.folders {
        let held = c.cached_uid_times(account_id, path);
        let mut changed = false;
        for uid in &deleted {
            if held.contains_key(uid) {
                c.delete_message(account_id, path, *uid);
                changed = true;
            }
        }
        let mut rows = Vec::new();
        for (message, labels) in &updated {
            if labels.iter().any(|l| l == label) {
                let mut row = message.clone();
                row.folder_id = *folder_id;
                rows.push(row);
            } else if held.contains_key(&message.uid) {
                c.delete_message(account_id, path, message.uid);
                changed = true;
            }
        }
        if !rows.is_empty() {
            merge_rows(c, account_id, *folder_id, path, rows);
            changed = true;
        }
        if changed {
            touched.push((*folder_id, path.clone()));
        }
    }
    touched.sort();
    touched
}

/// What a sync's event part means for the cache. With `changes`, they are
/// applied (or, when the server lost track, every folder is listed again)
/// and the event moves on; the folders the changes touched go to the list.
/// Without, the reply's event is taken only when this run has none: a
/// listing that did not ask for changes must not skip the ones it missed.
/// Whether more events wait.
pub(super) fn take_changes(
    page: &SyncPage,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut ProtonState,
    emit: &impl Fn(WorkerEvent),
) -> bool {
    let Some(changes) = &page.changes else {
        if state.event_id.is_none() {
            state.event_id = Some(page.event_id.clone());
        }
        return false;
    };
    state.event_id = Some(page.event_id.clone());
    if changes.refresh {
        tracing::info!("[account {account_id}] the server asked for a full resync");
        state.relist_all();
        return false;
    }
    for (folder_id, path) in apply_changes(changes, account_id, cache, state) {
        if let Some(c) = cache {
            emit_folder(
                folder_id,
                c.load_messages(account_id, &path, folder_id),
                state,
                emit,
            );
        }
    }
    changes.more
}

/// A folder's rows to the list, and its chip number.
pub(super) fn emit_folder(
    folder_id: u32,
    messages: Vec<Message>,
    state: &ProtonState,
    emit: &impl Fn(WorkerEvent),
) {
    let unread = state.chip_count(folder_id, &messages);
    emit(WorkerEvent::Messages {
        folder_id,
        messages,
    });
    emit(WorkerEvent::FolderUnread { folder_id, unread });
}

/// A folder's first page into the cache: the rows it lists and, when it
/// carries the folder's id snapshot, the cached rows the snapshot no longer
/// names dropped. The folder as the cache now holds it, and whether the
/// page was all of it.
pub(super) fn apply_first_page(
    page: &SyncPage,
    account_id: u32,
    folder_id: u32,
    path: &str,
    since: i64,
    cache: Option<&Cache>,
    state: &mut ProtonState,
) -> (Vec<Message>, bool) {
    let messages = page_messages(&page.headers, account_id, folder_id, state);
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
        }
    }
    merge_rows(c, account_id, folder_id, path, messages);
    (c.load_messages(account_id, path, folder_id), complete)
}

/// List a folder's newest page and fold it into the cache, with what
/// changed elsewhere since the last event: the folder as it now stands,
/// and whether the page was all of it. Once this run knows an event, the
/// listing asks from it, which also spares the id snapshot's walk of the
/// whole folder.
pub(super) async fn proton_sync_folder(
    account: &AccountConfig,
    account_id: u32,
    folder_id: u32,
    path: &str,
    cache: Option<&Cache>,
    state: &mut ProtonState,
    emit: &impl Fn(WorkerEvent),
) -> Result<(Vec<Message>, bool), HelperError> {
    let (_, label) = state
        .folders
        .get(path)
        .cloned()
        .ok_or_else(|| HelperError::new("folder_not_found"))?;
    let client = state.client(account)?;
    let since = crate::datefmt::now();
    let event = state.event_id.clone();
    let page = run(move || {
        sync(
            &client,
            Some(&label),
            PROTON_FIRST_PAGE,
            Start::Newest,
            event.as_deref(),
        )
    })
    .await?;
    take_folders(&page, account_id, cache, state, emit);
    take_changes(&page, account_id, cache, state, emit);
    // The folder ids may have been numbered again.
    let folder_id = state.folders.get(path).map_or(folder_id, |(id, _)| *id);
    Ok(apply_first_page(
        &page, account_id, folder_id, path, since, cache, state,
    ))
}

/// [`proton_sync_folder`], the folder alone.
pub(super) async fn proton_load_folder(
    account: &AccountConfig,
    account_id: u32,
    folder_id: u32,
    path: &str,
    cache: Option<&Cache>,
    state: &mut ProtonState,
    emit: &impl Fn(WorkerEvent),
) -> Result<Vec<Message>, HelperError> {
    proton_sync_folder(account, account_id, folder_id, path, cache, state, emit)
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
pub(super) async fn proton_backfill_step(
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut ProtonState,
    emit: &impl Fn(WorkerEvent),
) {
    if proton_backfill_with(account, account_id, cache, state, emit).await {
        state.backfill_wait = None;
        return;
    }
    state.backfill_wait = Some(Instant::now() + PROTON_BACKFILL_BACKOFF);
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

/// [`proton_backfill_step`]'s work; false when the call failed, the job
/// then still at the front of the queue.
pub(super) async fn proton_backfill_with(
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut ProtonState,
    emit: &impl Fn(WorkerEvent),
) -> bool {
    let Some(mut job) = state.backfill.pop_front() else {
        return true;
    };
    // A folder gone or hidden since it was queued has nothing left to index.
    let Some((folder_id, label)) = state.folders.get(&job.path).cloned() else {
        return true;
    };
    let client = match state.client(account) {
        Ok(client) => client,
        Err(_) => {
            state.backfill.push_front(job);
            return false;
        }
    };
    let since = crate::datefmt::now();
    let (limit, start) = match &job.plan {
        None => (PROTON_BACKFILL_CHUNK, Start::Newest),
        Some(Plan::Runs(runs)) => match runs.front() {
            Some((offset, len)) => (*len, Start::Offset(*offset)),
            None => (0, Start::Newest),
        },
        Some(Plan::Walk(cursor)) => (PROTON_BACKFILL_CHUNK, Start::After(cursor.clone())),
    };
    let page = if limit == 0 {
        None
    } else {
        match run(move || sync(&client, Some(&label), limit, start, None)).await {
            Ok(page) => Some(page),
            Err(e) => {
                tracing::warn!("[account {account_id}] index of {}: {}", job.path, e.code);
                state.backfill.push_front(job);
                return false;
            }
        }
    };
    job.failures = 0;
    if let Some(page) = &page {
        take_changes(page, account_id, cache, state, emit);
        index_page(
            page, account_id, folder_id, since, cache, state, &mut job, emit,
        );
    }
    let done = match &job.plan {
        Some(Plan::Runs(runs)) => runs.is_empty(),
        Some(Plan::Walk(_)) => false,
        None => true,
    };
    if done {
        state.listed.insert(job.path.clone(), Instant::now());
        if !job.told {
            emit(WorkerEvent::BackfillDone { folder_id });
        }
    } else {
        state.backfill.push_front(job);
    }
    true
}

/// One listed page of a background index, into the cache and the job's
/// plan: the first page plans the rest from its id snapshot (or, past the
/// snapshot's limit, walks on from its last id); a later page is merged
/// and the plan moves on.
#[allow(clippy::too_many_arguments)]
fn index_page(
    page: &SyncPage,
    account_id: u32,
    folder_id: u32,
    since: i64,
    cache: Option<&Cache>,
    state: &mut ProtonState,
    job: &mut ProtonBackfill,
    emit: &impl Fn(WorkerEvent),
) {
    let messages = page_messages(&page.headers, account_id, folder_id, state);
    let walk_on = |page: &SyncPage| {
        page.next_cursor
            .clone()
            .map_or(Plan::Runs(VecDeque::new()), Plan::Walk)
    };
    match job.plan.take() {
        None => {
            if let Some(ids) = &page.all_ids {
                for id in ids {
                    state.uids.insert(hash_uid(id), id.clone());
                }
            }
            let mut runs = VecDeque::new();
            let next = match (&page.all_ids, cache) {
                (Some(ids), Some(c)) => {
                    let listed: HashMap<u32, i64> = messages.iter().map(|m| (m.uid, 0)).collect();
                    if !messages.is_empty() {
                        merge_rows(c, account_id, folder_id, &job.path, messages);
                    }
                    let cached = c.cached_uid_times(account_id, &job.path);
                    let (gone, _) = snapshot_plan(ids, &cached, since, PROTON_BACKFILL_CHUNK);
                    // A full pass fetches what is cached too, all but the
                    // page just merged.
                    let known = if job.full { &listed } else { &cached };
                    let (_, missing) = snapshot_plan(ids, known, since, PROTON_BACKFILL_CHUNK);
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
                    Plan::Runs(runs)
                }
                (Some(_), None) => Plan::Runs(runs),
                (None, _) => {
                    if let Some(c) = cache {
                        if !messages.is_empty() {
                            let messages =
                                merge_rows(c, account_id, folder_id, &job.path, messages);
                            emit(WorkerEvent::MessagesAppend {
                                folder_id,
                                messages,
                            });
                        }
                    }
                    walk_on(page)
                }
            };
            job.plan = Some(next);
        }
        Some(Plan::Runs(mut runs)) => {
            runs.pop_front();
            if let (Some(c), false) = (cache, messages.is_empty()) {
                let messages = merge_rows(c, account_id, folder_id, &job.path, messages);
                emit(WorkerEvent::MessagesAppend {
                    folder_id,
                    messages,
                });
            }
            job.plan = Some(Plan::Runs(runs));
        }
        Some(Plan::Walk(_)) => {
            if let (Some(c), false) = (cache, messages.is_empty()) {
                let messages = merge_rows(c, account_id, folder_id, &job.path, messages);
                emit(WorkerEvent::MessagesAppend {
                    folder_id,
                    messages,
                });
            }
            job.plan = Some(walk_on(page));
        }
    }
}
