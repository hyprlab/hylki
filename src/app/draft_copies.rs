//! Drafts and templates kept as their own bytes, fetched ahead of time, so
//! one opens in the editor at once. Opening a draft needs everything it was
//! saved with (its From, Bcc, threading headers and files), which only its
//! bytes carry; fetched on the click, the editor waited on the server, for
//! seconds when the worker was busy.

use crate::models::{FolderKind, Message};
use crate::worker::MailRequest;

use super::model::AppModel;

/// How many of a folder's drafts are fetched ahead, the newest first.
const AHEAD: usize = 50;

/// A draft larger than this is fetched when it is opened, not kept: its
/// bytes would sit in the cache, written from the UI thread.
const LARGEST: usize = 8 * 1024 * 1024;

/// A draft on its way into the cache.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DraftFetch {
    account_id: u32,
    path: String,
    uid: u32,
    message_id: String,
    ts: i64,
}

impl AppModel {
    /// A Drafts or Templates folder was listed: drop the copies of drafts
    /// it no longer holds, and fetch those not kept yet.
    pub(super) fn fetch_drafts_ahead(
        &mut self,
        account_id: u32,
        folder_id: u32,
        messages: &[Message],
    ) {
        if !matches!(
            self.folder_kind(account_id, folder_id),
            Some(FolderKind::Drafts | FolderKind::Templates)
        ) {
            return;
        }
        let (Some(path), Some(cache)) =
            (self.folder_path(account_id, folder_id), self.cache.as_ref())
        else {
            return;
        };
        let uids: Vec<u32> = messages.iter().map(|m| m.uid).collect();
        cache.prune_draft_copies(account_id, &path, &uids);
        let kept = cache.draft_copies_in(account_id, &path);
        let mut wanted: Vec<&Message> = messages
            .iter()
            .filter(|m| {
                !kept
                    .iter()
                    .any(|(u, mid, ts)| (*u, mid, *ts) == (m.uid, &m.message_id, m.timestamp))
            })
            .collect();
        wanted.sort_by_key(|m| std::cmp::Reverse(m.timestamp));
        for m in wanted.into_iter().take(AHEAD) {
            let fetch = DraftFetch {
                account_id,
                path: path.clone(),
                uid: m.uid,
                message_id: m.message_id.clone(),
                ts: m.timestamp,
            };
            if self.draft_fetches.values().any(|f| *f == fetch) {
                continue;
            }
            let token = self.next_transfer;
            self.next_transfer += 1;
            self.draft_fetches.insert(token, fetch);
            // Not for the reader: a draft clicked meanwhile goes ahead of
            // these (see `worker::preempt`).
            self.send_to(
                account_id,
                MailRequest::ExportRaw {
                    token,
                    path: path.clone(),
                    uid: m.uid,
                    for_reader: false,
                },
            );
        }
    }

    /// The bytes fetched ahead under `token`, into the cache. False when
    /// the token is not one of these.
    pub(super) fn keep_fetched_draft(&mut self, token: u64, raw: &Result<Vec<u8>, String>) -> bool {
        let Some(f) = self.draft_fetches.remove(&token) else {
            return false;
        };
        if let Ok(raw) = raw {
            tracing::debug!(target: "hylki::drafts", "draft {} fetched ahead: {} bytes", f.uid, raw.len());
            self.keep_draft(&f.path, f.account_id, f.uid, &f.message_id, f.ts, raw);
        }
        true
    }

    /// Keep a draft's bytes, unless they are too large to.
    pub(super) fn keep_draft(
        &self,
        path: &str,
        account_id: u32,
        uid: u32,
        message_id: &str,
        ts: i64,
        raw: &[u8],
    ) {
        if raw.is_empty() || raw.len() > LARGEST {
            return;
        }
        if let Some(cache) = self.cache.as_ref() {
            cache.save_draft_copy(account_id, path, uid, message_id, ts, raw);
        }
    }

    /// A draft's bytes as kept, if they are still its own.
    pub(super) fn kept_draft(&self, m: &Message, path: &str) -> Option<Vec<u8>> {
        self.cache
            .as_ref()?
            .load_draft_copy(m.account_id, path, m.uid, &m.message_id, m.timestamp)
    }
}
