//! Changing and reading single messages on a Tuta account: resolving a uid
//! to its compound id, bodies, read state and moves, and the poll.

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::models::Message;
use crate::worker::helper::HelperError;
use crate::worker::ids::{merge_msgids, TUTA_CONV};
use crate::worker::summary_from_raw;
use crate::worker::tuta::index::{
    apply_first_page, run, take_folders, tuta_load_folder, TutaState, TUTA_FIRST_PAGE,
};
use crate::worker::tuta::mime::mime_message;
use crate::worker::tuta::wire::{mark_read, message, move_ids, sync};
use crate::worker::WorkerEvent;

/// A message uid's compound id, listing the folder once when the uid map
/// does not have it (a start from the cache, or a message moved since):
/// the first page of a listing names every id in the folder.
pub(super) async fn tuta_resolve(
    account: &AccountConfig,
    account_id: u32,
    state: &mut TutaState,
    path: &str,
    uid: u32,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) -> Result<String, HelperError> {
    if let Some(id) = state.uids.get(&uid) {
        return Ok(id.clone());
    }
    let folder_id = state.folders.get(path).map_or(0, |(id, _)| *id);
    tuta_load_folder(account, account_id, folder_id, path, cache, state, emit).await?;
    state
        .uids
        .get(&uid)
        .cloned()
        .ok_or_else(|| HelperError::new("message_gone"))
}

/// A message's MIME, put together from the helper's decrypted parts. What
/// it shows of the threading (Message-ID, References) and its preview go
/// into the cached row, which a listing cannot fill.
pub(super) async fn tuta_fetch_raw(
    account: &AccountConfig,
    account_id: u32,
    state: &mut TutaState,
    path: &str,
    uid: u32,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) -> Result<Vec<u8>, HelperError> {
    let id = tuta_resolve(account, account_id, state, path, uid, cache, emit).await?;
    let client = state.client(account)?;
    let (raw, conversation) = run(move || {
        let reply = message(&client, &id)?;
        let conversation = reply
            .get("header")
            .and_then(|h| h.get("conversation"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        Ok((mime_message(&reply)?, conversation))
    })
    .await?;
    if let Some(c) = cache {
        note_threading(c, account_id, path, uid, &raw, &conversation);
    }
    Ok(raw)
}

/// The row's Message-ID and References from its MIME, with the
/// conversation token kept beside them so it stays in the thread it was
/// listed in.
fn note_threading(
    cache: &Cache,
    account_id: u32,
    path: &str,
    uid: u32,
    raw: &[u8],
    conversation: &str,
) {
    let parsed = summary_from_raw(account_id, 0, uid, raw);
    if parsed.message_id.is_empty() {
        return;
    }
    let token = if conversation.is_empty() {
        String::new()
    } else {
        format!("{TUTA_CONV}{conversation}")
    };
    cache.set_threading(
        account_id,
        path,
        uid,
        &parsed.message_id,
        &merge_msgids(&parsed.references, &token),
        &parsed.preview,
    );
}

/// Mark messages read or unread on the server, then settle the cache by
/// what the helper says it applied: an id it did not get to keeps the
/// state the server has, and the next listing shows it. Errors are logged:
/// the list already changed, and a stale read mark is not worth a dialog.
pub(super) async fn tuta_set_seen(
    account: &AccountConfig,
    account_id: u32,
    state: &mut TutaState,
    path: &str,
    uids: &[u32],
    seen: bool,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) -> Vec<u32> {
    let mut ids = Vec::with_capacity(uids.len());
    let mut by_id = std::collections::HashMap::new();
    for &uid in uids {
        if let Ok(id) = tuta_resolve(account, account_id, state, path, uid, cache, emit).await {
            by_id.insert(id.clone(), uid);
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return Vec::new();
    }
    let client = match state.client(account) {
        Ok(client) => client,
        Err(e) => {
            tracing::warn!(
                "[account {account_id}] could not mark mail in {path}: {}",
                e.code
            );
            return Vec::new();
        }
    };
    let (applied, failed) = seen_on_server(client, ids, seen).await;
    if let Some(e) = failed {
        tracing::warn!(
            "[account {account_id}] {} message(s) in {path} not marked {}: {}",
            uids.len() - applied.len(),
            if seen { "read" } else { "unread" },
            e.code
        );
    }
    let applied: Vec<u32> = applied
        .iter()
        .filter_map(|id| by_id.get(id).copied())
        .collect();
    if let Some(c) = cache {
        for uid in &applied {
            c.set_unread(account_id, path, *uid, !seen);
        }
    }
    applied
}

async fn seen_on_server(
    client: crate::worker::helper::HelperClient,
    ids: Vec<String>,
    seen: bool,
) -> (Vec<String>, Option<HelperError>) {
    tokio::task::spawn_blocking(move || mark_read(&client, &ids, seen))
        .await
        .unwrap_or_else(|_| (Vec::new(), Some(HelperError::new("helper_failed"))))
}

/// Move messages from `path` to the folder `dest`. A moved row leaves the
/// source at once; its new id is never predicted (Tuta may file a moved
/// mail in another list), so the destination is listed again, which is
/// where the moved mail is found. The error, when not every message went.
pub(super) async fn tuta_move_uids(
    account: &AccountConfig,
    account_id: u32,
    state: &mut TutaState,
    path: &str,
    uids: &[u32],
    dest: &str,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) -> Result<(), HelperError> {
    let (dest_id, dest_tuta) = state
        .folders
        .get(dest)
        .cloned()
        .ok_or_else(|| HelperError::new("folder_not_found"))?;
    let mut ids = Vec::with_capacity(uids.len());
    let mut by_id = std::collections::HashMap::new();
    for &uid in uids {
        let id = tuta_resolve(account, account_id, state, path, uid, cache, emit).await?;
        by_id.insert(id.clone(), uid);
        ids.push(id);
    }
    let client = state.client(account)?;
    let (applied, failed) =
        tokio::task::spawn_blocking(move || move_ids(&client, &ids, &dest_tuta))
            .await
            .unwrap_or_else(|_| (Vec::new(), Some(HelperError::new("helper_failed"))));
    // What a read body showed of a moved row, for the destination's row:
    // where Tuta kept the id, the uid is the same there, and the moved mail
    // stays threaded and can be found again by an undo.
    let moved: Vec<u32> = applied
        .iter()
        .filter_map(|id| by_id.get(id).copied())
        .collect();
    let known: Vec<Message> = cache
        .map(|c| {
            c.load_messages(account_id, path, 0)
                .into_iter()
                .filter(|m| moved.contains(&m.uid) && !m.message_id.is_empty())
                .collect()
        })
        .unwrap_or_default();
    for uid in &moved {
        state.uids.remove(uid);
        if let Some(c) = cache {
            c.delete_message(account_id, path, *uid);
        }
    }
    if !moved.is_empty() {
        match tuta_load_folder(account, account_id, dest_id, dest, cache, state, emit).await {
            Ok(mut messages) => {
                for m in messages.iter_mut().filter(|m| m.message_id.is_empty()) {
                    let Some(old) = known.iter().find(|k| k.uid == m.uid) else {
                        continue;
                    };
                    if let Some(c) = cache {
                        c.set_threading(
                            account_id,
                            dest,
                            m.uid,
                            &old.message_id,
                            &old.references,
                            &old.preview,
                        );
                    }
                    m.message_id = old.message_id.clone();
                    m.references = old.references.clone();
                    m.preview = old.preview.clone();
                }
                emit(WorkerEvent::Messages {
                    folder_id: dest_id,
                    messages,
                })
            }
            Err(e) => tracing::warn!(
                "[account {account_id}] could not list {dest} after a move: {}",
                e.code
            ),
        }
    }
    match failed {
        Some(e) => {
            tracing::warn!(
                "[account {account_id}] could not move {} message(s) from {path} to {dest}: {}",
                uids.len() - applied.len(),
                e.code
            );
            Err(e)
        }
        None => Ok(()),
    }
}

/// One poll: a sync of the Inbox, which also brings the folder tree, so a
/// folder made or renamed in Tuta's own app shows up with no second call.
/// The caller says whether a failure is worth a banner.
pub(super) async fn tuta_poll(
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut TutaState,
    emit: &impl Fn(WorkerEvent),
) -> Result<(), HelperError> {
    let client = state.client(account)?;
    let since = crate::datefmt::now();
    let page = run(move || sync(&client, None, TUTA_FIRST_PAGE, 0)).await?;
    take_folders(&page, account_id, cache, state, emit);
    // The folder the helper listed: the one whose Tuta role is the Inbox,
    // whatever a manual assignment (#82) made of it here.
    let listed = state
        .folders
        .iter()
        .find(|(_, (_, tuta_id))| *tuta_id == page.folder)
        .map(|(path, (id, _))| (*id, path.clone()));
    let Some((folder_id, path)) = listed else {
        return Ok(());
    };
    let (messages, _) = apply_first_page(&page, account_id, folder_id, &path, since, cache, state);
    emit_folder(folder_id, messages, state, emit);
    Ok(())
}

/// A folder's rows to the list, and its chip number.
pub(super) fn emit_folder(
    folder_id: u32,
    messages: Vec<Message>,
    state: &TutaState,
    emit: &impl Fn(WorkerEvent),
) {
    use crate::worker::index_queue::IndexQueue;
    let unread = state.chip_count(folder_id, &messages);
    emit(WorkerEvent::Messages {
        folder_id,
        messages,
    });
    emit(WorkerEvent::FolderUnread { folder_id, unread });
}
