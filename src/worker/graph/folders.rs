//! The mail folders of a Microsoft account: listing them with their roles
//! and numbering, unread counts, and the poll for new mail in the Inbox.

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::i18n_f;
use crate::models::{Folder, FolderKind};
use crate::worker::folders::number_folders;
use crate::worker::graph::chores::emit_graph_body_hits;
use crate::worker::graph::http::{
    graph_get_json, graph_paged, graph_send_json, graph_token, GRAPH_BASE,
};
use crate::worker::graph::index::{graph_load_folder, GraphState};
use crate::worker::{blocking, chip_counts_all, snoozed_counts, WorkerEvent};

/// One Graph mail folder, flattened out of the tree.
pub(super) struct GraphFolder {
    pub(super) graph_id: String,
    pub(super) folder: Folder,
    /// Every message in it, read or not.
    pub(super) total: u32,
}

/// List the account's mail folders (tree flattened, well-known roles mapped),
/// sorted and id-numbered exactly like the IMAP path's folder list.
pub(super) fn graph_list_folders(
    token: &str,
    account_id: u32,
    assignments: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<GraphFolder>, String> {
    // Well-known folders name the roles; everything else is Custom. A role
    // folder some account type lacks (e.g. archive) just 404s — skip it.
    let mut roles: std::collections::HashMap<String, FolderKind> = Default::default();
    for (wk, kind) in [
        ("inbox", FolderKind::Inbox),
        ("sentitems", FolderKind::Sent),
        ("drafts", FolderKind::Drafts),
        ("deleteditems", FolderKind::Trash),
        ("junkemail", FolderKind::Junk),
        ("archive", FolderKind::Archive),
    ] {
        if let Ok(v) = graph_get_json(
            token,
            &format!("{GRAPH_BASE}/me/mailFolders/{wk}?$select=id"),
        ) {
            if let Some(id) = v["id"].as_str() {
                roles.insert(id.to_string(), kind);
            }
        }
    }

    const SELECT: &str = "$select=id,displayName,childFolderCount,unreadItemCount,totalItemCount";
    let roots = graph_paged(
        token,
        &format!("{GRAPH_BASE}/me/mailFolders?$top=100&{SELECT}"),
        200,
    )?;

    // Flatten the tree breadth-first; paths join with '/' like the sidebar's
    // hierarchy expects. Depth and total are capped defensively.
    let mut out: Vec<GraphFolder> = Vec::new();
    // Graph folder id → (every message, unread): the chip picks one once the
    // kinds are final (a manual Drafts assignment counts every draft).
    let mut counts: std::collections::HashMap<String, (u32, u32)> = Default::default();
    let mut queue: Vec<(serde_json::Value, String, u8)> =
        roots.into_iter().map(|v| (v, String::new(), 0u8)).collect();
    while let Some((v, prefix, depth)) = queue.pop() {
        let Some(gid) = v["id"].as_str() else {
            continue;
        };
        let name = v["displayName"].as_str().unwrap_or("?").to_string();
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let kind = match roles.get(gid) {
            Some(kind) => *kind,
            // No well-known folder is for templates (#360): a top-level
            // one goes by name.
            None if prefix.is_empty() && crate::models::is_templates_name(&name) => {
                FolderKind::Templates
            }
            None if prefix.is_empty() && crate::models::is_snoozed_name(&name) => {
                FolderKind::Snoozed
            }
            None => FolderKind::Custom,
        };
        if v["childFolderCount"].as_i64().unwrap_or(0) > 0 && depth < 4 && out.len() < 400 {
            if let Ok(children) = graph_paged(
                token,
                &format!("{GRAPH_BASE}/me/mailFolders/{gid}/childFolders?$top=100&{SELECT}"),
                200,
            ) {
                queue.extend(children.into_iter().map(|c| (c, path.clone(), depth + 1)));
            }
        }
        let count = |field: &str| v[field].as_i64().unwrap_or(0).max(0) as u32;
        counts.insert(
            gid.to_string(),
            (count("totalItemCount"), count("unreadItemCount")),
        );
        out.push(GraphFolder {
            total: count("totalItemCount"),
            graph_id: gid.to_string(),
            folder: Folder {
                id: 0, // assigned by order below
                account_id,
                name,
                path,
                kind,
                unread: 0, // filled in below, once the kinds are final
            },
        });
    }

    number_folders(&mut out, |f| &f.folder, |f| &mut f.folder);
    // Manual Special Folders assignments (#82) over the well-known roles,
    // before the chips are counted — as the IMAP listing does.
    let mut folders: Vec<Folder> = out.iter().map(|f| f.folder.clone()).collect();
    crate::models::assign_folder_roles(assignments, &mut folders);
    for (gf, f) in out.iter_mut().zip(folders) {
        gf.folder.kind = f.kind;
        let (total, unseen) = counts.get(&gf.graph_id).copied().unwrap_or_default();
        gf.folder.unread = if chip_counts_all(f.kind) {
            total
        } else {
            unseen
        };
    }
    Ok(out)
}

/// One poll tick: refresh the Inbox and emit it. The app diffs the arriving
/// list against its cache, so this drives both the visible refresh and the
/// desktop notification for genuinely new mail. Token failures stay quiet here
/// — a signed-out account already errors on every interactive action.
pub(super) async fn graph_poll_inbox(
    account_id: u32,
    account: &AccountConfig,
    cache: Option<&Cache>,
    state: &mut GraphState,
    emit: &impl Fn(WorkerEvent),
) {
    let quiet = |_: WorkerEvent| {};
    let Some(token) = graph_token(account, &quiet).await else {
        return;
    };
    if state.inbox.is_none() {
        // The startup folder listing may have failed (offline launch).
        refresh_graph_folders(&token, account_id, cache, state, emit).await;
    }
    let Some((folder_id, path)) = state.inbox.clone() else {
        return;
    };
    if let Ok(messages) =
        graph_load_folder(&token, account_id, folder_id, &path, cache, state).await
    {
        let unread = messages.iter().filter(|m| m.unread).count() as u32;
        emit_graph_body_hits(&token, account, folder_id, &path, &messages, state, emit).await;
        emit(WorkerEvent::Messages {
            folder_id,
            messages,
        });
        emit(WorkerEvent::FolderUnread { folder_id, unread });
    }
    // The poll only re-syncs the inbox; mail filed by server-side rules lands
    // in other folders without passing through it. Re-list for every folder's
    // unreadItemCount so their chips keep pace too.
    graph_refresh_unread(&token, account_id, cache, state, emit).await;
}

/// Re-list the folders for their server-side unread counts and push them, but
/// stay silent on failure — this runs on the background poll, where a transient
/// network error is not worth a banner (unlike [`refresh_graph_folders`]).
///
/// Emits the merged folder list first (so a renamed/new folder gets fresh ids),
/// then one [`WorkerEvent::FolderUnread`] per folder: the per-folder event is
/// the path allowed to assert a genuine zero, which the app's SetFolders merge
/// deliberately ignores.
pub(super) async fn graph_refresh_unread(
    token: &str,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut GraphState,
    emit: &impl Fn(WorkerEvent),
) {
    let t = token.to_string();
    let roles = state.roles.clone();
    let Ok(list) = blocking(move || graph_list_folders(&t, account_id, &roles)).await else {
        return;
    };
    if list.is_empty() {
        return;
    }
    state.adopt_folders(&list);
    let snoozed = snoozed_counts(&list, |f| (&f.folder, f.total));
    let folders: Vec<Folder> = list.into_iter().map(|f| f.folder).collect();
    if let Some(c) = cache {
        c.save_folders(account_id, &folders);
    }
    let counts: Vec<(u32, u32)> = folders.iter().map(|f| (f.id, f.unread)).collect();
    emit(WorkerEvent::Folders(folders));
    for (folder_id, unread) in counts {
        emit(WorkerEvent::FolderUnread { folder_id, unread });
    }
    for ev in snoozed {
        emit(ev);
    }
}

/// Re-list the folders, emit them, remember the path → Graph-id map.
pub(super) async fn refresh_graph_folders(
    token: &str,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut GraphState,
    emit: &impl Fn(WorkerEvent),
) {
    let t = token.to_string();
    let roles = state.roles.clone();
    let r = blocking(move || graph_list_folders(&t, account_id, &roles)).await;
    match r {
        Ok(mut list) => {
            // Hidden folders (#239) leave here, before the ids settle.
            list.retain(|f| {
                !crate::models::folder_is_hidden(&f.folder.path, Some("/"), &state.hidden)
            });
            state.adopt_folders(&list);
            let folders: Vec<Folder> = list.into_iter().map(|f| f.folder).collect();
            if let Some(c) = cache {
                c.save_folders(account_id, &folders);
            }
            emit(WorkerEvent::Folders(folders));
        }
        Err(e) => emit(WorkerEvent::net_error(i18n_f(
            "Could not list folders: {e}",
            &[("e", &(e).to_string())],
        ))),
    }
}

/// Make the top-level folder `path` when the account has none by that
/// name, as an IMAP move makes its destination: the first template (#360),
/// archive or snooze (#527) of an account without one. The listing that
/// follows numbers it.
pub(super) async fn graph_ensure_folder(
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut GraphState,
    path: &str,
    emit: &impl Fn(WorkerEvent),
) {
    if state.folders.contains_key(path) || path.contains('/') {
        return;
    }
    let Some(token) = graph_token(account, emit).await else {
        return;
    };
    let (t, url) = (token.clone(), format!("{GRAPH_BASE}/me/mailFolders"));
    let body = serde_json::json!({ "displayName": path });
    match blocking(move || graph_send_json(&t, "POST", &url, &body)).await {
        Ok(_) => refresh_graph_folders(&token, account_id, cache, state, emit).await,
        Err(e) => tracing::warn!("graph: could not create {path}: {e}"),
    }
}
