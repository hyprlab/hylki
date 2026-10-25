//! Changing messages on Microsoft 365: resolving a uid to its Graph id,
//! moves, flags, deleting for good, removing an attachment, raw bytes.

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::i18n;
use crate::worker::graph::http::{
    graph_delete_req, graph_get_bytes, graph_get_json, graph_send_json, graph_token, GRAPH_BASE,
};
use crate::worker::graph::index::{graph_load_folder, GraphState, GRAPH_RELIST_AFTER};
use crate::worker::graph::messages::{graph_find_by_msgid, graph_list_ids, graph_list_messages};
use crate::worker::ids::hash_uid;
use crate::worker::{blocking, WorkerEvent};

/// Delete messages for good, one Graph request each; the cache and the uid
/// map forget every one that went.
pub(super) async fn graph_purge_uids(
    account: &AccountConfig,
    account_id: u32,
    state: &mut GraphState,
    path: &str,
    uids: Vec<u32>,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    for uid in uids {
        let deleted = match graph_resolve(account, state, path, uid, emit).await {
            Some((token, gid)) => {
                let url = format!("{GRAPH_BASE}/me/messages/{gid}");
                blocking(move || graph_delete_req(&token, &url))
                    .await
                    .is_ok()
            }
            None => false,
        };
        if deleted {
            state.uids.remove(&uid);
            if let Some(c) = cache {
                c.delete_message(account_id, path, uid);
            }
        }
    }
}

/// Delete the draft a message was opened from, on the server and in the
/// cache, once it has been sent, scheduled or saved as a new version.
pub(super) async fn graph_drop_draft_origin(
    account: &AccountConfig,
    state: &mut GraphState,
    account_id: u32,
    origin: &crate::models::DraftOrigin,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    if origin.account_id != account_id {
        return;
    }
    if let Some((tok, gid)) = graph_resolve(account, state, &origin.path, origin.uid, emit).await {
        let url = format!("{GRAPH_BASE}/me/messages/{gid}");
        let _ = blocking(move || graph_delete_req(&tok, &url)).await;
    }
    if let Some(c) = cache {
        c.delete_message(account_id, &origin.path, origin.uid);
    }
}

/// Resolve a message uid to (token, Graph id), re-listing the folder once if
/// the uid isn't in the map (fresh start from cache, or a moved message).
async fn graph_resolve(
    account: &AccountConfig,
    state: &mut GraphState,
    path: &str,
    uid: u32,
    emit: &impl Fn(WorkerEvent),
) -> Option<(String, String)> {
    let token = graph_token(account, emit).await?;
    if let Some(gid) = state.uids.get(&uid) {
        return Some((token, gid.clone()));
    }
    // Not indexed yet: list the folder (fills the uid map) and try again. The
    // account/folder ids only label the discarded summaries, so zeros are fine.
    let _ = graph_load_folder(&token, 0, 0, path, None, state).await;
    if let Some(gid) = state.uids.get(&uid) {
        return Some((token, gid.clone()));
    }
    // Older than the first page (#429), from an earlier session's index:
    // the folder's id listing names every message in it, once a while.
    let recent =
        |at: Option<&std::time::Instant>| at.is_some_and(|at| at.elapsed() < GRAPH_RELIST_AFTER);
    if !recent(state.listed.get(path)) && !recent(state.looked_up.get(path)) {
        if let Some((_, folder_gid)) = state.folders.get(path).cloned() {
            let (t, base) = (token.clone(), state.base.clone());
            if let Ok(ids) = blocking(move || graph_list_ids(&t, &base, &folder_gid)).await {
                for (id, _) in ids {
                    state.uids.insert(hash_uid(&id), id);
                }
                state
                    .looked_up
                    .insert(path.to_string(), std::time::Instant::now());
            }
        }
    }
    state.uids.get(&uid).map(|gid| (token, gid.clone()))
}

/// Delete the attachment called `name` (nearest `size` among namesakes)
/// from a message (#289).
pub(super) async fn graph_delete_attachment(
    account: &AccountConfig,
    state: &mut GraphState,
    path: &str,
    uid: u32,
    name: &str,
    size: u64,
    emit: &impl Fn(WorkerEvent),
) -> Result<(), String> {
    let (token, gid) = graph_resolve(account, state, path, uid, emit)
        .await
        .ok_or_else(|| "message not found".to_string())?;
    let name = name.to_string();
    blocking(move || {
        let url = format!("{GRAPH_BASE}/me/messages/{gid}/attachments?$select=id,name,size");
        let listed = graph_get_json(&token, &url)?;
        let aid = listed["value"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|a| a["name"].as_str() == Some(name.as_str()))
            .min_by_key(|a| a["size"].as_u64().unwrap_or(0).abs_diff(size))
            .and_then(|a| a["id"].as_str().map(str::to_string))
            .ok_or_else(|| i18n("The attachment is no longer in the message on the server"))?;
        graph_delete_req(
            &token,
            &format!("{GRAPH_BASE}/me/messages/{gid}/attachments/{aid}"),
        )
    })
    .await
}

/// Fetch a message's raw RFC 822 bytes.
pub(super) async fn graph_fetch_raw(
    account: &AccountConfig,
    state: &mut GraphState,
    path: &str,
    uid: u32,
    emit: &impl Fn(WorkerEvent),
) -> Result<Vec<u8>, String> {
    let (token, gid) = graph_resolve(account, state, path, uid, emit)
        .await
        .ok_or_else(|| "message not found".to_string())?;
    let url = format!("{GRAPH_BASE}/me/messages/{gid}/$value");
    blocking(move || graph_get_bytes(&token, &url)).await
}

/// PATCH one message (read state, flag). Errors are logged, not surfaced — the
/// optimistic UI state already changed and a stale flag is not worth a dialog.
pub(super) async fn graph_patch_message(
    account: &AccountConfig,
    state: &mut GraphState,
    path: &str,
    uid: u32,
    body: serde_json::Value,
    emit: &impl Fn(WorkerEvent),
) {
    let Some((token, gid)) = graph_resolve(account, state, path, uid, emit).await else {
        return;
    };
    let url = format!("{GRAPH_BASE}/me/messages/{gid}");
    let r = blocking(move || graph_send_json(&token, "PATCH", &url, &body)).await;
    if let Err(e) = r {
        tracing::warn!("graph: could not update message flags: {e}");
    }
}

/// Move messages to another folder. The Graph id changes in transit; the moved
/// entries leave the uid map and the destination re-indexes on its next load.
pub(super) async fn graph_move_uids(
    account: &AccountConfig,
    account_id: u32,
    state: &mut GraphState,
    path: &str,
    uids: &[u32],
    dest: &str,
    cache: Option<&Cache>,
) -> Result<(), String> {
    let quiet = |_e: WorkerEvent| {};
    let token = graph_token(account, &quiet)
        .await
        .ok_or_else(|| "no sign-in token from GNOME Online Accounts".to_string())?;
    let dest_gid = match state.folders.get(dest) {
        Some((_, gid)) => gid.clone(),
        None => return Err(format!("unknown folder {dest}")),
    };
    for &uid in uids {
        let Some(gid) = state.uids.get(&uid).cloned() else {
            continue;
        };
        let t = token.clone();
        let url = format!("{GRAPH_BASE}/me/messages/{gid}/move");
        let body = serde_json::json!({ "destinationId": dest_gid });
        blocking(move || graph_send_json(&t, "POST", &url, &body)).await?;
        state.uids.remove(&uid);
        if let Some(c) = cache {
            c.delete_message(account_id, path, uid);
        }
    }
    Ok(())
}

/// Undo a move: the messages are in `path` (where the move put them) with new
/// Graph ids — find them by Internet Message-ID and move them back to `dest`,
/// patched with `patch` first when there is one (snoozed mail comes back
/// unread, #527).
pub(super) async fn graph_undo_move(
    account: &AccountConfig,
    account_id: u32,
    state: &mut GraphState,
    path: &str,
    dest: &str,
    message_ids: &[String],
    patch: Option<serde_json::Value>,
    cache: Option<&Cache>,
) -> Result<usize, String> {
    let quiet = |_e: WorkerEvent| {};
    let token = graph_token(account, &quiet)
        .await
        .ok_or_else(|| "no sign-in token from GNOME Online Accounts".to_string())?;
    let (folder_id, gid) = state
        .folders
        .get(path)
        .cloned()
        .ok_or_else(|| format!("unknown folder {path}"))?;
    let wanted: std::collections::HashSet<&str> = message_ids.iter().map(|s| s.as_str()).collect();

    let (t, base, folder_gid) = (token.clone(), state.base.clone(), gid.clone());
    let (listed, _) =
        blocking(move || graph_list_messages(&t, &base, &folder_gid, account_id, folder_id))
            .await?;

    let mut uids = Vec::new();
    let mut found: std::collections::HashSet<String> = Default::default();
    for (m, mgid) in listed {
        if wanted.contains(m.message_id.as_str()) {
            state.uids.insert(m.uid, mgid);
            uids.push(m.uid);
            found.insert(m.message_id);
        }
    }
    // A message keeps its arrival date when it moves, so an old one lands
    // below the newest page (#429): those are asked for by Message-ID.
    for mid in wanted.iter().filter(|mid| !found.contains(**mid)) {
        let exact = cache
            .map(|c| c.exact_msgids(mid))
            .unwrap_or_else(|| mid.to_string());
        let (t, base, folder_gid) = (token.clone(), state.base.clone(), gid.clone());
        match blocking(move || graph_find_by_msgid(&t, &base, &folder_gid, &exact)).await {
            Ok(hits) => {
                for (m, mgid) in hits {
                    if !uids.contains(&m.uid) {
                        state.uids.insert(m.uid, mgid);
                        uids.push(m.uid);
                    }
                }
            }
            Err(e) => tracing::warn!("undo: could not look up <{mid}> in {path}: {e}"),
        }
    }
    if uids.is_empty() {
        return Ok(0);
    }
    if let Some(body) = patch {
        for &uid in &uids {
            graph_patch_message(account, state, path, uid, body.clone(), &quiet).await;
        }
    }
    let n = uids.len();
    graph_move_uids(account, account_id, state, path, &uids, dest, cache).await?;
    Ok(n)
}
