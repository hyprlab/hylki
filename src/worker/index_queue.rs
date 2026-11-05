//! The queue of background-index passes (#429) that the Graph and JMAP
//! paths keep: one pass per folder, the folder on screen first, and a
//! folder indexed a while ago listed again when it is opened.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::models::{FolderKind, Message};
use crate::worker::folders::chip_count_of;
use crate::worker::ids::hash_uid;
use crate::worker::reconcile::listing_gone;

/// One folder's background-index pass.
pub(super) trait IndexJob {
    /// A pass over `path`; `told` when the list already knows the folder's
    /// index is complete.
    fn start(path: &str, told: bool) -> Self;
    fn path(&self) -> &str;
}

/// What a backend's state lends the queue: its passes, when each folder was
/// last indexed, its folder list and its Drafts folder.
pub(super) trait IndexQueue {
    type Job: IndexJob;
    /// How long a folder's index is trusted before opening the folder
    /// indexes it again, catching mail deleted elsewhere.
    const RELIST_AFTER: Duration;

    /// The folders whose background index is still to run, the one at the
    /// front in progress.
    fn jobs(&mut self) -> &mut VecDeque<Self::Job>;
    /// When each folder's background index last finished, by path.
    fn listed(&mut self) -> &mut HashMap<String, Instant>;
    /// Every listed folder as (folder id, path).
    fn folder_ids(&self) -> Vec<(u32, String)>;
    /// The account's hidden folders (#239).
    fn hidden(&self) -> &[String];
    /// The Drafts folder's (folder id, path).
    fn drafts(&self) -> Option<&(u32, String)>;

    /// Queue a folder's background index unless it is queued already.
    /// `front` puts it at the head of the queue (back there, if it was
    /// queued): the folder on screen goes before the rest of the mailbox.
    fn queue_backfill(&mut self, path: &str, front: bool, told: bool) {
        let jobs = self.jobs();
        if let Some(i) = jobs.iter().position(|j| j.path() == path) {
            if front && i > 0 {
                if let Some(job) = jobs.remove(i) {
                    jobs.push_front(job);
                }
            }
            return;
        }
        let job = Self::Job::start(path, told);
        if front {
            jobs.push_front(job);
        } else {
            jobs.push_back(job);
        }
    }

    /// Queue every listed, unhidden folder's background index, the Inbox
    /// and the other special folders first, as the folder list orders them.
    fn queue_all_backfills(&mut self) {
        let mut folders = self.folder_ids();
        folders.sort();
        for (_, path) in folders {
            if !self.listed().contains_key(&path)
                && !crate::models::folder_is_hidden(&path, Some("/"), self.hidden())
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
            self.jobs().retain(|j| j.path() != path);
            self.listed().insert(path.to_string(), Instant::now());
            return true;
        }
        match self.listed().get(path) {
            Some(at) => {
                if user && at.elapsed() >= Self::RELIST_AFTER {
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
        let kind = match self.drafts() {
            Some((id, _)) if *id == folder_id => FolderKind::Drafts,
            _ => FolderKind::Custom,
        };
        chip_count_of(kind, messages)
    }
}

/// Pause the background index after a failed step, until `wait`.
pub(super) async fn index_pause(wait: Option<Instant>) {
    if let Some(at) = wait {
        tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await;
    }
}

/// What a folder's complete id snapshot (newest first) asks of the cache,
/// for a backend whose uids are hashed server ids (Tuta, Proton): the rows
/// to drop, and the runs of mail to fetch as (offset, length), none longer
/// than `chunk`. `since` is when the snapshot was asked for.
pub(super) fn snapshot_plan(
    ids: &[String],
    cached: &HashMap<u32, i64>,
    since: i64,
    chunk: usize,
) -> (Vec<u32>, Vec<(usize, usize)>) {
    let server: HashSet<u32> = ids.iter().map(|id| hash_uid(id)).collect();
    let gone = listing_gone(cached, &server, since);
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (at, id) in ids.iter().enumerate() {
        if cached.contains_key(&hash_uid(id)) {
            continue;
        }
        match runs.last_mut() {
            Some((start, len)) if *start + *len == at && *len < chunk => *len += 1,
            _ => runs.push((at, 1)),
        }
    }
    (gone, runs)
}
