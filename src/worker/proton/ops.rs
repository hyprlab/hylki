//! Changing and reading single messages on a Proton account: resolving a
//! uid to its Proton id, bodies, read state, stars and moves, and the poll.

use std::collections::HashMap;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::worker::helper::{HelperClient, HelperError};
use crate::worker::proton::index::{
    apply_first_page, emit_folder, proton_load_folder, run, take_changes, take_folders,
    ProtonState, EVENT_ROUNDS, PROTON_FIRST_PAGE,
};
use crate::worker::proton::wire::{mark_read, message, move_ids, star, sync, Start};
use crate::worker::summary_from_raw;
use crate::worker::WorkerEvent;

/// A message uid's Proton id, listing the folder once when the uid map
/// does not have it (a start from the cache): the folder's first listing
/// names every id in it.
pub(super) async fn proton_resolve(
    account: &AccountConfig,
    account_id: u32,
    state: &mut ProtonState,
    path: &str,
    uid: u32,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) -> Result<String, HelperError> {
    if let Some(id) = state.uids.get(&uid) {
        return Ok(id.clone());
    }
    let folder_id = state.folders.get(path).map_or(0, |(id, _)| *id);
    proton_load_folder(account, account_id, folder_id, path, cache, state, emit).await?;
    state
        .uids
        .get(&uid)
        .cloned()
        .ok_or_else(|| HelperError::new("message_gone"))
}

/// Resolve every uid it can, as (Proton ids, uid by id).
async fn resolve_all(
    account: &AccountConfig,
    account_id: u32,
    state: &mut ProtonState,
    path: &str,
    uids: &[u32],
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) -> (Vec<String>, HashMap<String, u32>) {
    let mut ids = Vec::with_capacity(uids.len());
    let mut by_id = HashMap::new();
    for &uid in uids {
        if let Ok(id) = proton_resolve(account, account_id, state, path, uid, cache, emit).await {
            by_id.insert(id.clone(), uid);
            ids.push(id);
        }
    }
    (ids, by_id)
}

/// A message's MIME, decrypted and put together by the helper. What it
/// shows of the threading (References) and its preview go into the cached
/// row, which a listing cannot fill.
pub(super) async fn proton_fetch_raw(
    account: &AccountConfig,
    account_id: u32,
    state: &mut ProtonState,
    path: &str,
    uid: u32,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) -> Result<Vec<u8>, HelperError> {
    let id = proton_resolve(account, account_id, state, path, uid, cache, emit).await?;
    let client = state.client(account)?;
    let (raw, _) = run(move || message(&client, &id)).await?;
    if let Some(c) = cache {
        note_threading(c, account_id, path, uid, &raw);
    }
    Ok(raw)
}

/// The row's Message-ID and References from its MIME.
pub(super) fn note_threading(cache: &Cache, account_id: u32, path: &str, uid: u32, raw: &[u8]) {
    let parsed = summary_from_raw(account_id, 0, uid, raw);
    if parsed.message_id.is_empty() {
        return;
    }
    cache.set_threading(
        account_id,
        path,
        uid,
        &parsed.message_id,
        &parsed.references,
        &parsed.preview,
    );
}

/// Run a blocking mutation; a task that panicked applied nothing.
async fn mutation(
    f: impl FnOnce() -> (Vec<String>, Option<HelperError>) + Send + 'static,
) -> (Vec<String>, Option<HelperError>) {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|_| (Vec::new(), Some(HelperError::new("helper_failed"))))
}

/// The uids of the Proton ids a mutation applied.
fn applied_uids(applied: &[String], by_id: &HashMap<String, u32>) -> Vec<u32> {
    applied
        .iter()
        .filter_map(|id| by_id.get(id).copied())
        .collect()
}

/// Mark messages read or unread on the server, then settle the cache by
/// what the helper says it applied: an id it did not get to keeps the
/// state the server has, and the next poll shows it. Errors are logged:
/// the list already changed, and a stale read mark is not worth a dialog.
#[allow(clippy::too_many_arguments)]
pub(super) async fn proton_set_seen(
    account: &AccountConfig,
    account_id: u32,
    state: &mut ProtonState,
    path: &str,
    uids: &[u32],
    seen: bool,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) -> Vec<u32> {
    let (ids, by_id) = resolve_all(account, account_id, state, path, uids, cache, emit).await;
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
    let (applied, failed) = mutation(move || mark_read(&client, &ids, seen)).await;
    if let Some(e) = failed {
        tracing::warn!(
            "[account {account_id}] {} message(s) in {path} not marked {}: {}",
            uids.len() - applied.len(),
            if seen { "read" } else { "unread" },
            e.code
        );
    }
    let applied = applied_uids(&applied, &by_id);
    if let Some(c) = cache {
        for uid in &applied {
            c.set_unread(account_id, path, *uid, !seen);
        }
    }
    applied
}

/// Star or unstar a message on the server (Proton's Starred label). The
/// cache follows the request at once, as the list did, and goes back when
/// the server did not take it; the error is the caller's to say.
#[allow(clippy::too_many_arguments)]
pub(super) async fn proton_set_starred(
    account: &AccountConfig,
    account_id: u32,
    state: &mut ProtonState,
    path: &str,
    uid: u32,
    starred: bool,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) -> Result<(), HelperError> {
    if let Some(c) = cache {
        c.set_starred(account_id, path, uid, starred);
    }
    let starred_on_server = async {
        let id = proton_resolve(account, account_id, state, path, uid, cache, emit).await?;
        let client = state.client(account)?;
        let ids = vec![id];
        match mutation(move || star(&client, &ids, starred)).await {
            (applied, _) if !applied.is_empty() => Ok(()),
            (_, Some(e)) => Err(e),
            (_, None) => Err(HelperError::new("invalid_response")),
        }
    }
    .await;
    if let Err(e) = &starred_on_server {
        tracing::warn!(
            "[account {account_id}] could not {} message {uid} in {path}: {}",
            if starred { "star" } else { "unstar" },
            e.code
        );
        if let Some(c) = cache {
            c.set_starred(account_id, path, uid, !starred);
        }
    }
    starred_on_server
}

/// Move messages from `path` to the folder `dest`. A moved row leaves the
/// source at once; the destination is listed again (Proton keeps the id
/// across the move, so the row comes back under the same uid there). The
/// error, when not every message went.
#[allow(clippy::too_many_arguments)]
pub(super) async fn proton_move_uids(
    account: &AccountConfig,
    account_id: u32,
    state: &mut ProtonState,
    path: &str,
    uids: &[u32],
    dest: &str,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) -> Result<(), HelperError> {
    let (dest_id, dest_label) = state
        .folders
        .get(dest)
        .cloned()
        .ok_or_else(|| HelperError::new("folder_not_found"))?;
    let mut ids = Vec::with_capacity(uids.len());
    let mut by_id = HashMap::new();
    for &uid in uids {
        let id = proton_resolve(account, account_id, state, path, uid, cache, emit).await?;
        by_id.insert(id.clone(), uid);
        ids.push(id);
    }
    let client = state.client(account)?;
    let (applied, failed) = mutation(move || move_ids(&client, &ids, &dest_label)).await;
    let moved = applied_uids(&applied, &by_id);
    for uid in &moved {
        if let Some(c) = cache {
            c.delete_message(account_id, path, *uid);
        }
    }
    if !moved.is_empty() {
        match proton_load_folder(account, account_id, dest_id, dest, cache, state, emit).await {
            Ok(messages) => emit_folder(dest_id, messages, state, emit),
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
                uids.len() - moved.len(),
                e.code
            );
            Err(e)
        }
        None => Ok(()),
    }
}

/// One poll: a listing of the Inbox asked from the last event, which
/// brings the folder tree and what changed anywhere in the mailbox, so a
/// folder made in Proton's own apps or mail read there shows up without a
/// call of its own. The event log is followed for a few pages at most.
pub(super) async fn proton_poll(
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut ProtonState,
    emit: &impl Fn(WorkerEvent),
) -> Result<(), HelperError> {
    for _ in 0..EVENT_ROUNDS {
        let client = state.client(account)?;
        let since = crate::datefmt::now();
        let event = state.event_id.clone();
        let page = run(move || {
            sync(
                &client,
                None,
                PROTON_FIRST_PAGE,
                Start::Newest,
                event.as_deref(),
            )
        })
        .await?;
        take_folders(&page, account_id, cache, state, emit);
        let more = take_changes(&page, account_id, cache, state, emit);
        // The folder the helper listed: the one whose Proton label is the
        // Inbox, whatever a manual assignment (#82) made of it here.
        if let Some((folder_id, path)) = state.folder_of_label(&page.folder) {
            let (messages, _) =
                apply_first_page(&page, account_id, folder_id, &path, since, cache, state);
            emit_folder(folder_id, messages, state, emit);
        }
        if !more {
            break;
        }
    }
    Ok(())
}

/// The connection test's one call: a one-header page of the Inbox from
/// the second message, which spares the id snapshot's walk of the folder.
/// Blocking.
pub(super) fn probe(client: &HelperClient) -> Result<(), HelperError> {
    sync(client, None, 1, Start::Offset(1), None).map(|_| ())
}
