//! Housekeeping on Microsoft 365: category colors, auto-empty (#140) and
//! body search hits for filters (#191).

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::models::{FolderKind, Message};
use crate::worker::chores::{
    auto_empty_due, auto_empty_roles, graph_before_date, role_folder_path,
};
use crate::worker::graph::folders::graph_refresh_unread;
use crate::worker::graph::http::{graph_delete_req, graph_paged};
use crate::worker::graph::index::GraphState;
use crate::worker::graph::messages::GRAPH_FIRST_PAGE;
use crate::worker::ids::hash_uid;
use crate::worker::{blocking, WorkerEvent};

/// A Microsoft 365 category color (`preset0`…`preset24`) as the nearest
/// `#rrggbb`; `None` for "none" or anything unknown.
pub(super) fn graph_preset_color(preset: &str) -> Option<String> {
    let hex = match preset.to_ascii_lowercase().as_str() {
        "preset0" => "#c01c28",  // red
        "preset1" => "#e66100",  // orange
        "preset2" => "#865e3c",  // brown
        "preset3" => "#f5c211",  // yellow
        "preset4" => "#2ec27e",  // green
        "preset5" => "#0e9aa7",  // teal
        "preset6" => "#6b8e23",  // olive
        "preset7" => "#1c71d8",  // blue
        "preset8" => "#813d9c",  // purple
        "preset9" => "#b5127a",  // cranberry
        "preset10" => "#5d7c99", // steel
        "preset11" => "#3f5567", // dark steel
        "preset12" => "#77767b", // gray
        "preset13" => "#5e5c64", // dark gray
        "preset14" => "#241f31", // black
        "preset15" => "#a51d2d", // dark red
        "preset16" => "#c64600", // dark orange
        "preset17" => "#63452c", // dark brown
        "preset18" => "#e5a50a", // dark yellow
        "preset19" => "#26a269", // dark green
        "preset20" => "#0b7c85", // dark teal
        "preset21" => "#55701c", // dark olive
        "preset22" => "#1a5fb4", // dark blue
        "preset23" => "#613583", // dark purple
        "preset24" => "#8f0e5d", // dark cranberry
        _ => return None,
    };
    Some(hex.to_string())
}

/// Auto-empty (#140) for a Microsoft 365 account, by the well-known folder
/// names and `receivedDateTime`.
pub(super) async fn auto_empty_graph(
    token: &str,
    account_id: u32,
    account: &AccountConfig,
    cache: Option<&Cache>,
    state: &mut GraphState,
    last: &mut Option<std::time::Instant>,
    emit: &impl Fn(WorkerEvent),
) {
    if !auto_empty_due(account, last) {
        return;
    }
    let folders = cache
        .map(|c| c.load_folders(account_id))
        .unwrap_or_default();
    let mut purged_any = false;
    for (kind, role, days) in auto_empty_roles(account) {
        let well_known = match kind {
            FolderKind::Junk => "junkemail",
            _ => "deleteditems",
        };
        let path = role_folder_path(account, &folders, kind, role);
        let mailbox = &state.mailbox;
        let url = format!(
            "{mailbox}/mailFolders/{well_known}/messages\
             ?$filter=receivedDateTime%20le%20{}&$select=id&$top=100",
            graph_before_date(days)
        );
        let t = token.to_string();
        let items = blocking(move || graph_paged(&t, &url, GRAPH_FIRST_PAGE)).await;
        let ids: Vec<String> = match items {
            Ok(items) => items
                .iter()
                .filter_map(|v| v["id"].as_str().map(str::to_string))
                .collect(),
            Err(e) => {
                tracing::warn!(
                    "auto-empty: could not list {well_known} on account {account_id}: {e}"
                );
                continue;
            }
        };
        if ids.is_empty() {
            continue;
        }
        let mut deleted = 0usize;
        for gid in ids {
            let t = token.to_string();
            let url = format!("{}/messages/{gid}", state.mailbox);
            let ok = blocking(move || graph_delete_req(&t, &url)).await.is_ok();
            if ok {
                deleted += 1;
                let uid = hash_uid(&gid);
                state.uids.remove(&uid);
                if let (Some(c), Some(path)) = (cache, path.as_deref()) {
                    c.delete_message(account_id, path, uid);
                }
            }
        }
        tracing::info!(
            "auto-empty: deleted {deleted} message(s) older than {days} days from {well_known} on account {account_id}"
        );
        purged_any |= deleted > 0;
    }
    if purged_any {
        graph_refresh_unread(token, account_id, cache, state, emit).await;
    }
}

/// Microsoft Graph's side of [`emit_body_hits`]: one `$search="body:…"`
/// listing per alternative over the inbox, intersected with the listed
/// messages (Graph ids hash to the uids the app knows).
pub(super) async fn emit_graph_body_hits(
    token: &str,
    account: &AccountConfig,
    folder_id: u32,
    path: &str,
    messages: &[Message],
    state: &GraphState,
    emit: &impl Fn(WorkerEvent),
) {
    if messages.is_empty() || state.inbox.as_ref().map(|(_, p)| p.as_str()) != Some(path) {
        return;
    }
    let needles = crate::config::filter_body_needles(&account.email);
    if needles.is_empty() {
        return;
    }
    let Some((_, gid)) = state.folders.get(path).cloned() else {
        return;
    };
    let listed: std::collections::HashSet<u32> = messages.iter().map(|m| m.uid).collect();
    let mut hits: std::collections::HashMap<u32, Vec<String>> = Default::default();
    for needle in needles {
        // KQL: the term in quotes; a quote inside it would end the term.
        let term = format!("\"body:{}\"", needle.replace('"', " "));
        let mailbox = &state.mailbox;
        let url = format!(
            "{mailbox}/mailFolders/{gid}/messages?$search={}&$select=id&$top=250",
            crate::encoding::percent::encode(&term)
        );
        let t = token.to_string();
        let found = blocking(move || graph_paged(&t, &url, 250)).await;
        match found {
            Ok(items) => {
                for uid in items
                    .iter()
                    .filter_map(|v| v["id"].as_str())
                    .map(hash_uid)
                    .filter(|u| listed.contains(u))
                {
                    hits.entry(uid).or_default().push(needle.clone());
                }
            }
            Err(e) => tracing::warn!("filter: Graph body search for {needle:?} failed: {e}"),
        }
    }
    tracing::info!(
        "filter: Graph body search over {} messages hit {}",
        listed.len(),
        hits.len()
    );
    emit(WorkerEvent::BodyHits { folder_id, hits });
}
