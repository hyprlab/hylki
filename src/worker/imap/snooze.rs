//! Finding moved mail again by Message-ID, for an undone move and for
//! snoozed mail coming back (#527).

use std::collections::HashSet;

use async_imap::types::Fetch;
use futures::TryStreamExt;

use super::{ensured, After, Emit, ImapWorker};
use crate::worker::imap::mutate::move_messages;
use crate::worker::imap::retry::load_messages_retry;
use crate::worker::imap::summary::message_id_of;
use crate::worker::imap::wire::{search_uids, sel, store_uids, uid_set};
use crate::worker::imap::ImapSession;
use crate::worker::WorkerEvent;

/// The UIDs `message_ids` have in `path`, where a move put them: their UIDs
/// changed in transit, their Message-IDs did not. Leaves `path` selected.
pub(super) async fn find_by_message_ids(
    sess: &mut ImapSession,
    path: &str,
    message_ids: &[String],
) -> Result<Vec<u32>, async_imap::error::Error> {
    let exists = sel(sess, path).await?.exists;
    let mut uids: Vec<u32> = Vec::new();
    for id in message_ids {
        uids.extend(search_uids(sess, format!("HEADER Message-ID \"{id}\"")).await?);
    }
    uids.sort_unstable();
    uids.dedup();
    // Some servers (iCloud among them) can't be trusted with HEADER
    // searches: the index lags behind a just-landed move, and our stored
    // ids are lowercased while the server's match may be case-sensitive.
    // FETCH has neither problem: before declaring the messages gone, read
    // the newest headers (a move lands at the top of the UID space) and
    // match Message-IDs locally.
    if uids.is_empty() && exists > 0 {
        let wanted: HashSet<&str> = message_ids.iter().map(String::as_str).collect();
        let lo = exists.saturating_sub(199).max(1);
        let fetches: Vec<Fetch> = sess
            .fetch(
                format!("{lo}:{exists}"),
                "(UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])",
            )
            .await?
            .try_collect()
            .await?;
        uids.extend(
            fetches
                .iter()
                .filter_map(|f| f.uid.filter(|_| wanted.contains(message_id_of(f).as_str()))),
        );
    }
    Ok(uids)
}

impl<E: Emit> ImapWorker<E> {
    /// Snoozed mail's time has come (#527): unread again, back where it
    /// was, and that folder listed so the app sees it arrive.
    pub(super) async fn on_unsnooze(
        &mut self,
        path: String,
        dest: String,
        dest_folder_id: u32,
        message_ids: Vec<String>,
    ) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let use_envelope = &mut self.use_envelope;
        let moved: Result<Vec<u32>, async_imap::error::Error> = async {
            let sess = ensured(session);
            let uids = find_by_message_ids(sess, &path, &message_ids).await?;
            if uids.is_empty() {
                return Ok(uids);
            }
            // Back as new mail is: unread, so it is seen to have come back.
            let _: Vec<Fetch> = store_uids(sess, uid_set(&uids), "-FLAGS (\\Seen)")
                .await?
                .try_collect()
                .await?;
            move_messages(sess, &path, &uids, &dest).await?;
            Ok(uids)
        }
        .await;
        let uids = match moved {
            Ok(uids) => uids,
            Err(e) => {
                tracing::warn!("snooze: could not bring {message_ids:?} back from {path:?}: {e}");
                emit(WorkerEvent::Unsnoozed {
                    message_ids,
                    done: false,
                });
                return After::lost_if(true);
            }
        };
        if uids.is_empty() {
            tracing::info!("snooze: {message_ids:?} no longer in {path:?}");
        } else {
            if let Some(c) = cache.as_ref() {
                for uid in &uids {
                    c.delete_message(account_id, &path, *uid);
                }
            }
            if let Ok(messages) = load_messages_retry(
                account_id,
                session,
                account,
                dest_folder_id,
                &dest,
                use_envelope,
                cache.as_ref(),
            )
            .await
            {
                if let Some(c) = cache.as_ref() {
                    c.upsert_messages(account_id, &dest, &messages);
                }
                emit(WorkerEvent::Messages {
                    folder_id: dest_folder_id,
                    messages,
                });
            }
        }
        emit(WorkerEvent::Unsnoozed {
            message_ids,
            done: true,
        });
        After::Done
    }
}
