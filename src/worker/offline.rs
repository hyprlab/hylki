//! Mail kept offline (#536): with a period chosen for the account, every
//! message of it is downloaded in the background, newest first, so it opens
//! with no network. Written to the disk cache only: unlike the newest mail's
//! prefetch, nothing is pushed to the UI, which would hold it all in memory.
//!
//! Each backend runs it below its background index, so the list and search
//! are complete first, a little at a time so a click never waits long. What
//! is shared is here: the queue of what the period lacks, the pauses after a
//! server refuses (Gmail caps a day's IMAP downloads), and writing a message
//! to the cache as opening it would.

use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::models::FolderKind;
use crate::worker::render::attachments::attachments_of;
use crate::worker::render::body_cacheable;
use crate::worker::render_raw;

/// How many messages one look at the cache queues.
const CANDIDATES: usize = 200;

/// After the period is complete, how long until the cache is asked again:
/// new mail, or a period that moved on, has more to download.
pub(super) const RECHECK: Duration = Duration::from_secs(15 * 60);

/// The same for an account copying its mail to On This Computer (#552):
/// new mail is looked for every minute, so it is there soon after it came.
const COPY_RECHECK: Duration = Duration::from_secs(60);

/// The longest wait after a failure, and the one after a server says the
/// account is over its limit.
const MAX_BACKOFF: Duration = Duration::from_secs(30 * 60);
const OVER_QUOTA: Duration = Duration::from_secs(60 * 60);

/// A batch that failed this many times in a row is given up on for the
/// session, so one message the server cannot hand over does not stop the rest.
const GIVE_UP: u32 = 3;

#[derive(Default)]
pub(super) struct Offline {
    /// Messages to download, newest first (folder path, uid).
    queue: VecDeque<(String, u32)>,
    /// Messages that could not be kept this session: OpenPGP mail, which is
    /// never cached, or ones the server would not hand over.
    tried: HashSet<(String, u32)>,
    /// Nothing to do before this: the period is complete, or the server
    /// refused.
    rest_until: Option<Instant>,
    /// Failed batches in a row.
    failures: u32,
}

impl Offline {
    /// Whether the account has a period to keep, or mail to copy to On
    /// This Computer (#552).
    pub(super) fn wanted(account: &AccountConfig) -> bool {
        account.offline_mail.since(0).is_some() || account.copy_to_local
    }

    /// Whether there is a period to download and it is time to work on it.
    pub(super) fn due(&self, account: &AccountConfig) -> bool {
        Self::wanted(account) && self.rest_until.is_none_or(|t| Instant::now() >= t)
    }

    /// When the next step may run, for a loop that sleeps until then.
    pub(super) fn wait(&self) -> Option<Instant> {
        self.rest_until
    }

    pub(super) fn rest(&mut self, for_: Duration) {
        self.rest_until = Some(Instant::now() + for_);
    }

    /// Back off after a failed batch: doubling from 30 s, an hour when the
    /// server says the account is over its limit.
    fn backoff(&mut self, error: &str) {
        self.failures += 1;
        let lower = error.to_ascii_lowercase();
        let wait = if lower.contains("overquota") || lower.contains("bandwidth") {
            OVER_QUOTA
        } else {
            Duration::from_secs(30 << self.failures.min(6)).min(MAX_BACKOFF)
        };
        tracing::info!(
            "offline mail: download paused for {}s after an error: {error}",
            wait.as_secs()
        );
        self.rest(wait);
    }

    /// Queue what the period still lacks, if nothing is queued. False when
    /// the period is all on disk, and then nothing is asked for a while.
    pub(super) fn fill(&mut self, cache: &Cache, account_id: u32, account: &AccountConfig) -> bool {
        if !self.queue.is_empty() {
            return true;
        }
        let since = account.offline_mail.since(crate::datefmt::now());
        if since.is_none() && !account.copy_to_local {
            return false;
        }
        let skip = skipped_folders(cache, account_id);
        let tried = &self.tried;
        let found: Vec<(String, u32)> = cache
            .offline_missing(
                account_id,
                since,
                account.offline_attachments,
                account.copy_to_local,
                &skip,
                CANDIDATES + tried.len(),
            )
            .into_iter()
            .filter(|m| !tried.contains(m))
            .collect();
        if account.copy_to_local {
            // What the status bar says is left to copy (#552).
            let left = if found.is_empty() {
                0
            } else {
                cache
                    .local_copy_pending(account_id, &skip)
                    .saturating_sub(tried.len())
            };
            crate::worker::local::copy::set_pending(account_id, left);
        }
        if found.is_empty() {
            tracing::debug!(account_id, "offline mail: the period is all on disk");
            self.rest(if account.copy_to_local {
                COPY_RECHECK
            } else {
                RECHECK
            });
            return false;
        }
        self.queue.extend(found);
        true
    }

    /// The next message to download.
    pub(super) fn next(&mut self) -> Option<(String, u32)> {
        self.queue.pop_front()
    }

    /// The uids queued behind the front for `path`, in order.
    pub(super) fn queued_in<'a>(&'a self, path: &'a str) -> impl Iterator<Item = u32> + 'a {
        self.queue
            .iter()
            .filter(move |(p, _)| p == path)
            .map(|(_, u)| *u)
    }

    /// The next batch: the front message's folder, and up to `max` of that
    /// folder's queued messages with it.
    pub(super) fn next_batch(&mut self, max: usize) -> Option<(String, Vec<u32>)> {
        let path = self.queue.front()?.0.clone();
        let mut uids = Vec::new();
        self.queue.retain(|(p, u)| {
            if *p == path && uids.len() < max {
                uids.push(*u);
                false
            } else {
                true
            }
        });
        Some((path, uids))
    }

    /// Put an unfinished batch back at the front.
    pub(super) fn restore(&mut self, path: &str, uids: &[u32]) {
        for uid in uids.iter().rev() {
            self.queue.push_front((path.to_string(), *uid));
        }
    }

    /// A batch came down: the pauses start over.
    pub(super) fn succeeded(&mut self) {
        self.failures = 0;
    }

    /// A batch failed: pause, then try it again, or after a few failures in
    /// a row leave it for the session.
    pub(super) fn failed(&mut self, path: &str, uids: &[u32], error: &str) {
        if !self.paused(path, uids, error) {
            self.restore(path, uids);
        }
    }

    /// [`Offline::failed`] for a backend that does not queue: pause, and
    /// true when the batch has been left for the session.
    pub(super) fn paused(&mut self, path: &str, uids: &[u32], error: &str) -> bool {
        self.backoff(error);
        if self.failures < GIVE_UP {
            return false;
        }
        for uid in uids {
            self.not_kept(path, *uid);
        }
        true
    }

    /// A message that is not kept this session.
    pub(super) fn not_kept(&mut self, path: &str, uid: u32) {
        self.tried.insert((path.to_string(), uid));
    }

    pub(super) fn was_tried(&self, path: &str, uid: u32) -> bool {
        self.tried.contains(&(path.to_string(), uid))
    }

    /// How many messages are not kept this session, to ask the cache for
    /// that many more candidates.
    pub(super) fn tried_count(&self) -> usize {
        self.tried.len()
    }
}

/// The folders whose mail is not kept offline: what is binned, junk, and
/// drafts, which open in the composer from the server's copy.
fn skipped_folders(cache: &Cache, account_id: u32) -> Vec<String> {
    cache
        .load_folders(account_id)
        .into_iter()
        .filter(|f| {
            matches!(
                f.kind,
                FolderKind::Trash | FolderKind::Junk | FolderKind::Drafts
            )
        })
        .map(|f| f.path)
        .collect()
}

/// Write one downloaded message to the cache, as opening it would: body,
/// sender verdict, the paperclip as fact, and with `attachments` the files.
/// False for OpenPGP mail, which is left for an explicit open (#133).
pub(super) fn keep(
    cache: &Cache,
    account_id: u32,
    path: &str,
    uid: u32,
    raw: &[u8],
    attachments: bool,
) -> bool {
    // Encrypted mail is copied as it is; it is only never rendered here.
    crate::worker::local::copy::copy(cache, account_id, path, uid, raw);
    if crate::mail::pgp::detect(raw).is_some() {
        return false;
    }
    let (body, check, has_attachments) = render_raw(raw);
    if !body_cacheable(&check) {
        return false;
    }
    cache.save_body(account_id, path, uid, &body);
    cache.save_sender_check(account_id, path, uid, &check);
    cache.set_has_attachment(account_id, path, uid, has_attachments);
    if attachments {
        let (files, _) = attachments_of(raw);
        cache.save_attachments(account_id, path, uid, &files);
        cache.mark_attachments_checked(account_id, path, uid);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_is_one_folders_messages_in_order() {
        let mut o = Offline::default();
        o.queue.extend([
            ("INBOX".to_string(), 9),
            ("Sent".to_string(), 4),
            ("INBOX".to_string(), 7),
        ]);
        assert_eq!(o.next_batch(10), Some(("INBOX".to_string(), vec![9, 7])));
        assert_eq!(o.queue, [("Sent".to_string(), 4)]);
        o.restore("INBOX", &[9, 7]);
        assert_eq!(o.queue.front(), Some(&("INBOX".to_string(), 9)));
        assert_eq!(o.queue.len(), 3);
        assert_eq!(o.queued_in("INBOX").collect::<Vec<_>>(), [9, 7]);
    }

    #[test]
    fn a_server_over_its_limit_is_left_for_an_hour() {
        let mut o = Offline::default();
        o.backoff("NO [OVERQUOTA] Account exceeded command or bandwidth limits");
        let left = o.rest_until.unwrap() - Instant::now();
        assert!(left > Duration::from_secs(3500));
        let mut o = Offline::default();
        o.backoff("connection reset");
        let left = o.rest_until.unwrap() - Instant::now();
        assert!(left <= Duration::from_secs(60));
    }

    #[test]
    fn a_batch_failing_again_and_again_is_left_for_the_session() {
        let mut o = Offline::default();
        for _ in 0..GIVE_UP - 1 {
            o.failed("INBOX", &[3], "timeout");
            assert_eq!(o.next(), Some(("INBOX".to_string(), 3)));
        }
        o.failed("INBOX", &[3], "timeout");
        assert!(o.next().is_none());
        assert!(o.was_tried("INBOX", 3));
    }
}
