//! The folders of a Proton account: every sync lists the system folders,
//! the user's folders and the user's labels, each with its role and unread
//! count, so the list here is built from whatever sync came last.

use std::collections::{BTreeMap, HashMap};

use crate::models::{Folder, FolderKind};
use crate::worker::folders::number_folders;
use crate::worker::proton::wire::FolderRow;

/// One Proton folder or label, placed in the tree.
#[derive(Debug, Clone)]
pub(super) struct ProtonFolder {
    /// The label id the helper takes back.
    pub(super) label_id: String,
    pub(super) folder: Folder,
}

/// The kind of folder a Proton role is. Only a role names a special
/// folder: a user folder called "Templates" stays Custom, since neither a
/// template nor a snooze can be filed on a Proton account (no draft upload,
/// no folder creation).
fn role_kind(role: &str) -> FolderKind {
    match role {
        "inbox" => FolderKind::Inbox,
        "sent" => FolderKind::Sent,
        "drafts" => FolderKind::Drafts,
        "trash" => FolderKind::Trash,
        "junk" => FolderKind::Junk,
        "archive" => FolderKind::Archive,
        "starred" => FolderKind::Starred,
        // All Mail, the user's folders and labels, and any role a newer
        // helper adds: still browsable.
        _ => FolderKind::Custom,
    }
}

/// How deep a folder may sit before the rest of its branch is dropped: far
/// past any real mailbox, and a guard against a parent loop.
const DEPTH_MAX: usize = 64;

/// A folder's path: its parents' labels and its own, joined with '/', a
/// '/' in a label written as its look-alike. `None` for a folder under a
/// missing parent, or deeper than [`DEPTH_MAX`].
fn path_of(row: &FolderRow, by_id: &HashMap<&str, &FolderRow>) -> Option<String> {
    let mut parts = vec![crate::models::slashed_leaf(&row.label)];
    let mut parent = row.parent.as_str();
    while !parent.is_empty() {
        if parts.len() > DEPTH_MAX {
            return None;
        }
        let up = by_id.get(parent)?;
        parts.push(crate::models::slashed_leaf(&up.label));
        parent = up.parent.as_str();
    }
    parts.reverse();
    Some(parts.join("/"))
}

/// The account's folders from a sync's rows, ordered and numbered like
/// every other account's, with the manual Special Folders assignments
/// (#82) applied and the hidden ones (#239) left out. The unread counts
/// are the server's own.
pub(super) fn proton_folders(
    rows: &[FolderRow],
    account_id: u32,
    assignments: &BTreeMap<String, String>,
    hidden: &[String],
) -> Vec<ProtonFolder> {
    let by_id: HashMap<&str, &FolderRow> = rows.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut out: Vec<ProtonFolder> = Vec::new();
    for row in rows {
        let Some(path) = path_of(row, &by_id) else {
            continue;
        };
        // Two folders by one name under one parent would share a path,
        // which keys the cache: the first listed keeps it. System folders
        // come first, so a user folder named "Inbox" never takes its place.
        if out.iter().any(|f| f.folder.path == path) {
            continue;
        }
        let name = path.rsplit('/').next().unwrap_or(&path).to_string();
        out.push(ProtonFolder {
            label_id: row.id.clone(),
            folder: Folder {
                id: 0, // assigned by order below
                account_id,
                name,
                unread: row.unread,
                path,
                kind: role_kind(&row.role),
                childless: false,
            },
        });
    }
    out.retain(|f| !crate::models::folder_is_hidden(&f.folder.path, Some("/"), hidden));
    number_folders(&mut out, |f| &f.folder, |f| &mut f.folder);
    let mut folders: Vec<Folder> = out.iter().map(|f| f.folder.clone()).collect();
    crate::models::assign_folder_roles(assignments, &mut folders);
    for (pf, f) in out.iter_mut().zip(folders) {
        pf.folder.kind = f.kind;
    }
    out
}
