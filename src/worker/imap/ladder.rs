//! The idle ladder: what the main connection does when no request is
//! waiting, in priority order. Each rung does one bounded piece of work and
//! returns, so the next request is never kept waiting for long.

use super::backfill::{restore_interrupted_backfill, run_one_backfill, run_one_refs_repair};
use super::connect::connect_and_list;
use super::folders::refresh_folder_names;
use super::gmail::run_one_label_fill;
use super::idle::{idle_wait, IdleOutcome};
use super::prefetch::{
    queue_attachment_prefetch, queue_body_prefetch, run_one_body_prefetch, run_one_prefetch,
};
use super::retry::load_messages_retry;
use super::sweep::{auto_empty_imap, clean_lists_imap, emit_body_hits, refresh_unread_counts};
use super::watch::watch_changed_folders;
use super::{Emit, ImapWorker, UNREAD_SWEEP_MIN};
use crate::worker::preempt::interrupt_read_for_reader;
use crate::worker::{MailRequest, WorkerEvent};

/// What a rung of the idle ladder came back with.
pub(super) enum Idle {
    /// A request arrived while waiting: serve it.
    Request(MailRequest),
    /// A piece of idle work was done: look for requests, then climb again.
    Again,
    /// The UI hung up.
    Stop,
}

impl<E: Emit> ImapWorker<E> {
    /// Climb the ladder: a search of the server's text (#509), body
    /// prefetch, then folder names, the unread sweep
    /// (with auto-empty and list clean-up), attachment prefetch, References
    /// repair and Gmail label fill (only once the index is complete), the
    /// background index (connecting first if need be), the offline period's
    /// download (#536), the IDLE resync, IDLE
    /// itself, and otherwise a plain wait for the next request.
    pub(super) async fn idle(&mut self) -> Idle {
        // Offline, the queues keep for when the network is back; on
        // a metered connection the work done ahead of the user waits
        // for an unmetered one (#442).
        let net = crate::desktop::power::reaches(&self.account.imap_host);
        let ahead = net && !crate::desktop::power::saving_data();
        if self.text_search.is_some() && net {
            // A search the user is waiting on (#509), before anything done
            // ahead of them.
            match self.session {
                Some(_) => self.search_one_folder().await,
                None => self.connect_or_wait().await,
            }
        } else if !self.body_prefetch.is_empty() && ahead {
            self.prefetch_one_body().await
        } else if let (true, true, Some(session)) =
            (self.folder_refresh_due, net, self.session.as_mut())
        {
            self.folder_refresh_due = false;
            refresh_folder_names(
                self.account_id,
                &self.account,
                session,
                self.cache.as_ref(),
                &self.emit,
            )
            .await;
            Idle::Again
        } else if self.sweep_due && self.session.is_some() && net {
            self.sweep_unread().await
        } else if !self.prefetch.is_empty() && ahead {
            run_one_prefetch(
                &mut self.prefetch,
                &mut self.session,
                &self.account,
                self.account_id,
                self.cache.as_ref(),
                &self.emit,
            )
            .await;
            self.pending_resync = true;
            Idle::Again
        } else if !self.refs_repair.is_empty()
            && self.session.is_some()
            && self.backfill.is_empty()
            && ahead
        {
            // Lower priority than indexing new mail: repair the threading
            // references of already-indexed replies, a chunk at a time.
            run_one_refs_repair(
                &mut self.refs_repair,
                &mut self.session,
                self.account_id,
                self.cache.as_ref(),
                &self.emit,
            )
            .await;
            Idle::Again
        } else if !self.label_fill.is_empty()
            && self.session.is_some()
            && self.backfill.is_empty()
            && ahead
        {
            run_one_label_fill(
                &mut self.label_fill,
                &mut self.session,
                self.account_id,
                self.cache.as_ref(),
                &self.emit,
            )
            .await;
            Idle::Again
        } else if !self.backfill.is_empty() && ahead {
            // Index the rest of the mailbox in the background. Connect first
            // for cached-folder accounts (which connect lazily); if still
            // offline, wait for a request instead of spinning.
            if self.session.is_none() {
                self.connect_or_wait().await
            } else {
                self.index_one_chunk().await
            }
        } else if ahead && self.offline.due(&self.account) {
            // The account's offline period (#536), once the index is in.
            if self.session.is_none() {
                self.connect_or_wait().await
            } else {
                self.keep_offline_batch().await
            }
        } else if self.push_enabled && self.session.is_some() && self.idle_folder.is_some() && net {
            self.idle_on_folder().await
        } else {
            self.wait_for_request().await
        }
    }

    /// Highest priority: get new mail's body cached so opening it is
    /// instant (no network wait).
    async fn prefetch_one_body(&mut self) -> Idle {
        let interrupted_body = self.body_prefetch.front().cloned();
        let result = interrupt_read_for_reader(
            run_one_body_prefetch(
                &mut self.body_prefetch,
                &mut self.session,
                &self.account,
                self.account_id,
                self.cache.as_ref(),
                &mut self.body_emitted,
                &self.emit,
            ),
            &mut self.rx,
            &mut self.backlog,
        )
        .await;
        if result.is_none() {
            if let Some(body) = interrupted_body {
                if !self.body_prefetch.contains(&body) {
                    self.body_prefetch.push_front(body);
                }
            }
            tracing::debug!(
                account_id = self.account_id,
                "body prefetch yielded to the reader"
            );
        }
        Idle::Again
    }

    /// The unread chips' re-count, owed since IDLE last woke (or left half
    /// done by a request): folder by folder, yielding to whatever arrives,
    /// its place kept. A finished sweep runs the auto-empty and list
    /// clean-up after it.
    async fn sweep_unread(&mut self) -> Idle {
        let account_id = self.account_id;
        let sess = self
            .session
            .as_mut()
            .expect("the ladder sweeps only with a session");
        let (changed, complete) = refresh_unread_counts(
            account_id,
            sess,
            self.cache.as_ref(),
            self.idle_folder.as_ref().map(|(_, p)| p.as_str()),
            &mut self.sweep_baseline,
            &mut self.sweep_pending,
            &self.rx,
            &self.emit,
        )
        .await;
        watch_changed_folders(&mut self.watchers, &self.account, &changed, &self.emit);
        if complete {
            self.sweep_due = false;
            self.last_unread_sweep = Some(std::time::Instant::now());
            auto_empty_imap(
                account_id,
                &self.account,
                sess,
                self.cache.as_ref(),
                &mut self.last_auto_empty,
                &self.emit,
            )
            .await;
            clean_lists_imap(
                account_id,
                &self.account,
                sess,
                self.cache.as_ref(),
                &mut self.last_list_clean,
                &self.emit,
            )
            .await;
        }
        Idle::Again
    }

    /// A request beats the handshake. The first thing the UI asks for at
    /// startup is the visible folder, which the cache can answer without a
    /// network round trip; awaiting the connect here left the message list
    /// empty until the IMAP handshake finished (or timed out). Dropping the
    /// connect future only cancels this attempt; the next idle pass starts
    /// a fresh one.
    async fn connect_or_wait(&mut self) -> Idle {
        tokio::select! {
            biased;
            req = self.rx.recv() => match req {
                Some(req) => Idle::Request(req),
                None => Idle::Stop,
            },
            connected = connect_and_list(
                self.account_id,
                &self.account,
                self.cache.as_ref(),
                &self.emit,
            ) => {
                if connected.is_some() {
                    self.session = connected;
                    return Idle::Again;
                }
                // Still offline: wait for a request rather than
                // spinning on back-to-back reconnect attempts.
                match self.rx.recv().await {
                    Some(req) => Idle::Request(req),
                    None => Idle::Stop,
                }
            }
        }
    }

    /// One chunk of the background index. The job owns its remaining UIDs
    /// while pending. Keep its original place so cancelling a network read
    /// cannot skip a chunk of the search index.
    async fn index_one_chunk(&mut self) -> Idle {
        let interrupted_job = self.backfill.front().cloned();
        let result = interrupt_read_for_reader(
            run_one_backfill(
                &mut self.backfill,
                &mut self.session,
                &self.account,
                self.account_id,
                self.cache.as_ref(),
                &mut self.prefetch,
                &mut self.use_envelope,
                &self.emit,
            ),
            &mut self.rx,
            &mut self.backlog,
        )
        .await;
        if result.is_none() {
            if let Some(job) = interrupted_job {
                tracing::debug!(
                    account_id = self.account_id,
                    "background index yielded to the reader"
                );
                restore_interrupted_backfill(&mut self.backfill, job);
            }
        }
        Idle::Again
    }

    /// Push: resync the open folder once if work was done since the last
    /// look, then sit in IDLE on it.
    async fn idle_on_folder(&mut self) -> Idle {
        let (fid, fpath) = self
            .idle_folder
            .clone()
            .expect("the ladder idles only with a folder to idle on");
        // Catch mail delivered while the connection was busy prefetching.
        if self.pending_resync {
            self.pending_resync = false;
            return self.resync_idle_folder(fid, fpath).await;
        }
        match idle_wait(
            &mut self.session,
            &self.account,
            self.account_id,
            fid,
            &fpath,
            &mut self.rx,
            self.cache.as_ref(),
            &mut self.use_envelope,
            &mut self.body_prefetch,
            &mut self.prefetch,
            &self.body_emitted,
            &self.emit,
            1740,
        )
        .await
        {
            IdleOutcome::Request(req) => Idle::Request(req),
            IdleOutcome::Refreshed | IdleOutcome::Quiet => {
                // IDLE only watches one folder; mail filed by
                // server-side rules lands in the others without ever
                // waking it. Re-check their unread chips whenever
                // IDLE surfaces — on new mail, and on the quiet
                // timeout, which is the only clock a push-only
                // (manual-fetch) account has.
                // Owed, not run here: the idle chain runs it
                // after the new mail's body is prefetched, and
                // any request that arrives meanwhile goes first.
                if self
                    .last_unread_sweep
                    .is_none_or(|t| t.elapsed() >= UNREAD_SWEEP_MIN)
                {
                    self.sweep_due = true;
                }
                Idle::Again
            }
            IdleOutcome::Closed => Idle::Stop,
        }
    }

    /// The resync before IDLE: the open folder's newest window, with the
    /// prefetch queues, body-search hits and list it brings.
    async fn resync_idle_folder(&mut self, fid: u32, fpath: String) -> Idle {
        let account_id = self.account_id;
        let result = interrupt_read_for_reader(
            load_messages_retry(
                account_id,
                &mut self.session,
                &self.account,
                fid,
                &fpath,
                &mut self.use_envelope,
                self.cache.as_ref(),
            ),
            &mut self.rx,
            &mut self.backlog,
        )
        .await;
        if result.is_none() {
            self.pending_resync = true;
            tracing::debug!(account_id, "idle resync yielded to the reader");
            return Idle::Again;
        }
        if let Some(Ok(messages)) = result {
            if let Some(c) = self.cache.as_ref() {
                c.upsert_messages(account_id, &fpath, &messages);
            }
            queue_body_prefetch(
                &mut self.body_prefetch,
                &fpath,
                &messages,
                &self.body_emitted,
            );
            queue_attachment_prefetch(
                &mut self.prefetch,
                &fpath,
                &messages,
                self.cache.as_ref(),
                account_id,
            );
            emit_body_hits(
                &mut self.session,
                &self.account,
                account_id,
                fid,
                &fpath,
                &messages,
                self.cache.as_ref(),
                &self.emit,
            )
            .await;
            (self.emit)(WorkerEvent::Messages {
                folder_id: fid,
                messages,
            });
        }
        Idle::Again
    }

    /// Nothing to do ahead of the user: block for the next request.
    async fn wait_for_request(&mut self) -> Idle {
        match self.rx.recv().await {
            Some(req) => Idle::Request(req),
            None => Idle::Stop,
        }
    }
}
