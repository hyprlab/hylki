//! Mail kept offline (#536) on an IMAP account: a rung of the idle ladder
//! below the background index. Each step fetches one batch from one folder
//! and yields to the reader. The queue and the pauses are shared with the
//! other backends, in [`crate::worker::offline`].

use super::ladder::Idle;
use super::retry::load_raws_retry;
use super::{Emit, ImapWorker, BODY_FETCH_BATCH};
use crate::worker::offline::{keep, RECHECK};
use crate::worker::preempt::interrupt_read_for_reader;

impl<E: Emit> ImapWorker<E> {
    /// One step of the offline download: queue what the period still lacks
    /// if nothing is queued, then download one batch.
    pub(super) async fn keep_offline_batch(&mut self) -> Idle {
        let Some(cache) = self.cache.as_ref() else {
            self.offline.rest(RECHECK);
            return Idle::Again;
        };
        let account_id = self.account_id;
        let attachments = self.account.offline_attachments;
        if !self.offline.fill(cache, account_id, &self.account) {
            return Idle::Again;
        }
        let Some((path, uids)) = self.offline.next_batch(BODY_FETCH_BATCH) else {
            return Idle::Again;
        };
        let result = interrupt_read_for_reader(
            load_raws_retry(&mut self.session, &self.account, &path, &uids),
            &mut self.rx,
            &mut self.backlog,
        )
        .await;
        match result {
            None => {
                // The reader came first; this batch goes again after it.
                self.offline.restore(&path, &uids);
            }
            Some(Ok(raws)) => {
                self.offline.succeeded();
                for uid in &uids {
                    let kept = raws
                        .get(uid)
                        .is_some_and(|raw| keep(cache, account_id, &path, *uid, raw, attachments));
                    if !kept {
                        self.offline.not_kept(&path, *uid);
                    }
                }
            }
            Some(Err(e)) => self.offline.failed(&path, &uids, &e.to_string()),
        }
        Idle::Again
    }
}
