//! An assistant's search of an IMAP server: `UID SEARCH TEXT`, one folder a
//! step on the idle ladder as for the user's own (#509). What the index does
//! not hold yet has its headers fetched and saved, the way the background
//! index saves them, so the assistant can go on to name it.

use std::collections::VecDeque;

use super::backfill::fetch_summaries_by_uid;
use super::fetch_mode::{is_parse_error, step_fetch_mode};
use super::ladder::Idle;
use super::text_search::{folders_to_search, text_query};
use super::wire::{search_uids, select_for_read};
use super::{Emit, ImapWorker, BACKFILL_CHUNK};
use crate::worker::{ServerFound, ServerSearch};

/// How many searches wait at once: each holds a place on the connection, so
/// an assistant asking in a loop gets told no rather than filling the queue.
const QUEUED_MAX: usize = 4;

/// A search under way: the folders still to ask.
pub(super) struct ServerJob {
    ask: ServerSearch,
    paths: VecDeque<String>,
    /// Whether the folder at the front has been tried once already.
    retried: bool,
}

impl<E: Emit> ImapWorker<E> {
    /// Queue a search, or say why it cannot be made.
    pub(super) fn on_ask_server(&mut self, ask: ServerSearch) {
        if !crate::desktop::power::reaches(&self.account.imap_host) {
            ask.refuse(self.account_id, "the server cannot be reached right now");
            return;
        }
        if self.server_searches.len() >= QUEUED_MAX {
            ask.refuse(self.account_id, "this server is busy with earlier searches");
            return;
        }
        let paths = folders_to_search(self.cache.as_ref(), self.account_id, ask.paths.clone());
        self.server_searches.push_back(ServerJob {
            ask,
            paths: paths.into(),
            retried: false,
        });
    }

    /// One step: search the next folder and say what it holds.
    pub(super) async fn search_server_folder(&mut self) -> Idle {
        let Some(job) = self.server_searches.front_mut() else {
            return Idle::Again;
        };
        // The assistant stopped waiting, or has every folder's answer.
        let (Some(path), false) = (job.paths.front().cloned(), job.ask.answer.is_closed()) else {
            let finished = self.server_searches.pop_front();
            if let Some(job) = finished {
                let _ = job.ask.answer.send(ServerFound {
                    account_id: self.account_id,
                    done: true,
                    ..Default::default()
                });
            }
            return Idle::Again;
        };
        let Some(session) = self.session.as_mut() else {
            return Idle::Again;
        };
        let query = text_query(&job.ask.terms);
        let result = match select_for_read(session, &path).await {
            Ok(()) => search_uids(session, &query).await,
            Err(e) => Err(e),
        };
        let mut part = ServerFound {
            account_id: self.account_id,
            ..Default::default()
        };
        match result {
            Ok(found) => {
                let mut uids: Vec<u32> = found.into_iter().collect();
                part.found = uids.len();
                uids.sort_unstable_by(|a, b| b.cmp(a));
                uids.truncate(job.ask.keep);
                part.hits = uids.iter().map(|uid| (path.clone(), *uid)).collect();
                if let Err(e) = self.index_missing(&path, &uids).await {
                    part.trouble = Some(format!("{path}: {e}"));
                }
            }
            // The server answered: this folder cannot be searched so.
            Err(
                e @ (async_imap::error::Error::No(_)
                | async_imap::error::Error::Bad(_)
                | async_imap::error::Error::Parse(_)),
            ) => {
                tracing::info!("mcp: {path} refused the search: {e}");
                part.trouble = Some(format!("{path} cannot be searched"));
            }
            // The connection went: once more over a new one.
            Err(e) => {
                tracing::info!("mcp: the connection dropped searching {path}: {e}");
                self.session = None;
                if !job.retried {
                    job.retried = true;
                    return Idle::Again;
                }
                part.trouble = Some(format!("the connection dropped searching {path}"));
            }
        }
        if let Some(job) = self.server_searches.front_mut() {
            job.paths.pop_front();
            job.retried = false;
            let _ = job.ask.answer.send(part);
        }
        Idle::Again
    }

    /// Fetch and save the headers of what the index does not hold, on the
    /// way of asking that this server can read. Nothing here marks mail read.
    async fn index_missing(&mut self, path: &str, uids: &[u32]) -> Result<(), String> {
        let Some(cache) = self.cache.as_ref() else {
            return Err("there is no index to save to".into());
        };
        let known: std::collections::HashSet<u32> = cache
            .indexed_message_ids(self.account_id, path, uids)
            .into_iter()
            .map(|(uid, _)| uid)
            .collect();
        let missing: Vec<u32> = uids
            .iter()
            .copied()
            .filter(|uid| !known.contains(uid))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        tracing::debug!("mcp: {path}: {} found mail not indexed", missing.len());
        for chunk in missing.chunks(BACKFILL_CHUNK) {
            let Some(session) = self.session.as_mut() else {
                return Err("the connection dropped".into());
            };
            let mut messages =
                fetch_summaries_by_uid(self.account_id, session, 0, path, chunk, self.use_envelope)
                    .await;
            if messages.as_ref().is_err_and(is_parse_error)
                && step_fetch_mode(self.account_id, &mut self.use_envelope)
            {
                messages = fetch_summaries_by_uid(
                    self.account_id,
                    session,
                    0,
                    path,
                    chunk,
                    self.use_envelope,
                )
                .await;
            }
            match messages {
                Ok(messages) => {
                    if let Some(cache) = self.cache.as_ref() {
                        cache.upsert_messages(self.account_id, path, &messages);
                    }
                }
                Err(e) => {
                    // It may have left the connection mid-reply.
                    self.session = None;
                    return Err(format!("could not fetch the mail it found: {e}"));
                }
            }
        }
        Ok(())
    }
}
