//! Mailboxes: listing them with their roles and numbering, unread counts,
//! and creating one.

use std::collections::{BTreeMap, HashMap};

use crate::cache::Cache;
use crate::i18n::i18n_f;
use crate::models::{Folder, FolderKind};
use crate::worker::folders::number_folders;
use crate::worker::jmap::index::JmapState;
use crate::worker::jmap::wire::{args, call, jmap_call, JmapSession, CAP_CORE, CAP_MAIL};
use crate::worker::{blocking, chip_counts_all, snoozed_counts, WorkerEvent};

/// One mailbox, flattened out of the tree.
pub(super) struct JmapFolder {
    pub(super) mailbox_id: String,
    pub(super) folder: Folder,
    /// Every message in it, read or not.
    pub(super) total: u32,
}

pub(super) fn role_kind(role: &str) -> FolderKind {
    match role {
        "inbox" => FolderKind::Inbox,
        "sent" => FolderKind::Sent,
        "drafts" => FolderKind::Drafts,
        "trash" => FolderKind::Trash,
        "junk" => FolderKind::Junk,
        "archive" => FolderKind::Archive,
        // Fastmail's, from the email-snooze draft: the same idea (#527).
        "snoozed" => FolderKind::Snoozed,
        _ => FolderKind::Custom,
    }
}

/// List the account's mailboxes (tree flattened into `/` paths, roles
/// mapped), sorted and id-numbered exactly like the IMAP path's folder list.
pub(super) fn jmap_list_folders(
    s: &JmapSession,
    account_id: u32,
    assignments: &BTreeMap<String, String>,
) -> Result<Vec<JmapFolder>, String> {
    let responses = jmap_call(
        s,
        &[CAP_CORE, CAP_MAIL],
        vec![call(
            0,
            "Mailbox/get",
            serde_json::json!({ "accountId": s.account, "ids": null }),
        )],
    )?;
    let list = args(&responses, 0)["list"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let by_id: HashMap<String, &serde_json::Value> = list
        .iter()
        .filter_map(|m| m["id"].as_str().map(|id| (id.to_string(), m)))
        .collect();
    // A mailbox's path is its ancestors' names and its own, joined with '/',
    // which is what the sidebar's hierarchy expects. A name with a slash in
    // it would read as nesting; the character is swapped for a look-alike.
    let name_of =
        |m: &serde_json::Value| -> String { m["name"].as_str().unwrap_or("?").replace('/', "∕") };
    let mut out: Vec<JmapFolder> = Vec::new();
    let mut counts: HashMap<String, (u32, u32)> = HashMap::new();
    for m in &list {
        let Some(id) = m["id"].as_str() else { continue };
        let mut names = vec![name_of(m)];
        let mut parent = m["parentId"].as_str();
        let mut depth = 0;
        while let Some(pid) = parent {
            let Some(p) = by_id.get(pid) else { break };
            names.push(name_of(p));
            parent = p["parentId"].as_str();
            depth += 1;
            if depth > 10 {
                break;
            }
        }
        names.reverse();
        let path = names.join("/");
        let name = names.last().cloned().unwrap_or_default();
        let kind = match m["role"].as_str().map(role_kind) {
            Some(kind) => kind,
            // No role names templates (#360): a top-level one goes by name.
            None if names.len() == 1 && crate::models::is_templates_name(&name) => {
                FolderKind::Templates
            }
            None if names.len() == 1 && crate::models::is_snoozed_name(&name) => {
                FolderKind::Snoozed
            }
            None => FolderKind::Custom,
        };
        let count = |field: &str| m[field].as_i64().unwrap_or(0).max(0) as u32;
        counts.insert(
            id.to_string(),
            (count("totalEmails"), count("unreadEmails")),
        );
        out.push(JmapFolder {
            total: count("totalEmails"),
            mailbox_id: id.to_string(),
            folder: Folder {
                id: 0,
                account_id,
                name,
                path,
                kind,
                unread: 0,
            },
        });
    }
    number_folders(&mut out, |f| &f.folder, |f| &mut f.folder);
    // Manual Special Folders assignments (#82) over the server's roles,
    // before the chips are counted, as the IMAP listing does.
    let mut folders: Vec<Folder> = out.iter().map(|f| f.folder.clone()).collect();
    crate::models::assign_folder_roles(assignments, &mut folders);
    for (jf, f) in out.iter_mut().zip(folders) {
        jf.folder.kind = f.kind;
        let (total, unseen) = counts.get(&jf.mailbox_id).copied().unwrap_or_default();
        jf.folder.unread = if chip_counts_all(f.kind) {
            total
        } else {
            unseen
        };
    }
    Ok(out)
}

/// Re-list the folders, emit them, remember the path → mailbox-id map.
pub(super) async fn refresh_jmap_folders(
    s: &JmapSession,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
) {
    let sess = s.clone();
    let roles = state.roles.clone();
    match blocking(move || jmap_list_folders(&sess, account_id, &roles)).await {
        Ok(mut list) => {
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
            &[("e", &e)],
        ))),
    }
}

/// Re-list the folders for their unread counts and push them, quietly: this
/// rides the background poll and the push channel.
pub(super) async fn jmap_refresh_unread(
    s: &JmapSession,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
) {
    let sess = s.clone();
    let roles = state.roles.clone();
    let Ok(mut list) = blocking(move || jmap_list_folders(&sess, account_id, &roles)).await else {
        return;
    };
    list.retain(|f| !crate::models::folder_is_hidden(&f.folder.path, Some("/"), &state.hidden));
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

/// Create a mailbox called `name` under `parent` (the top when `None`).
/// Returns its id.
pub(super) fn jmap_create_mailbox(
    s: &JmapSession,
    name: &str,
    parent: Option<&str>,
) -> Result<String, String> {
    let r = jmap_call(
        s,
        &[CAP_CORE, CAP_MAIL],
        vec![call(
            0,
            "Mailbox/set",
            serde_json::json!({
                "accountId": s.account,
                "create": { "c": { "name": name, "parentId": parent } },
            }),
        )],
    )?;
    let r = args(&r, 0);
    match r["created"]["c"]["id"].as_str() {
        Some(id) => Ok(id.to_string()),
        None if r["created"]["c"].is_object() => Ok(String::new()),
        None => Err(r["notCreated"]["c"]["description"]
            .as_str()
            .unwrap_or("the server refused")
            .to_string()),
    }
}

/// Make the top-level mailbox `path` when the account has none by that
/// name, as an IMAP move makes its destination: the first template (#360),
/// archive or snooze (#527) of an account without one. The listing that
/// follows numbers it.
pub(super) async fn jmap_ensure_mailbox(
    s: &JmapSession,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
    path: &str,
) {
    if state.folders.contains_key(path) || path.contains('/') {
        return;
    }
    let (sess, name) = (s.clone(), path.to_string());
    match blocking(move || jmap_create_mailbox(&sess, &name, None)).await {
        Ok(_) => refresh_jmap_folders(s, account_id, cache, state, emit).await,
        Err(e) => tracing::warn!("jmap: could not create {path}: {e}"),
    }
}
