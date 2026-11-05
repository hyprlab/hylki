//! The mail folders of a Tuta account: every sync lists the whole tree,
//! with each system folder's role, so the list here is built from whatever
//! sync came last.

use std::collections::{BTreeMap, HashMap};

use crate::models::{Folder, FolderKind};
use crate::worker::folders::number_folders;
use crate::worker::tuta::wire::FolderRow;

/// One Tuta folder, placed in the tree.
#[derive(Debug, Clone)]
pub(super) struct TutaFolder {
    pub(super) tuta_id: String,
    pub(super) folder: Folder,
}

/// The kind of a Tuta folder: its role, which names every system folder.
/// Tuta has no Templates or Snoozed folder, so those are user folders
/// Hylki makes at the top of the tree (#360, #527) and go by name there,
/// as on Microsoft 365; any other user folder stays Custom.
fn folder_kind(row: &FolderRow) -> FolderKind {
    match row.role.as_str() {
        "inbox" => FolderKind::Inbox,
        "sent" => FolderKind::Sent,
        "drafts" => FolderKind::Drafts,
        "trash" => FolderKind::Trash,
        "junk" => FolderKind::Junk,
        "archive" => FolderKind::Archive,
        "other" if row.parent.is_empty() && crate::models::is_templates_name(&row.label) => {
            FolderKind::Templates
        }
        "other" if row.parent.is_empty() && crate::models::is_snoozed_name(&row.label) => {
            FolderKind::Snoozed
        }
        // Any other folder, and any role a newer helper adds: still
        // browsable.
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
/// (#82) applied and the hidden ones (#239) left out. Each keeps the unread
/// count `cached` has for its path until its own listing says otherwise.
pub(super) fn tuta_folders(
    rows: &[FolderRow],
    account_id: u32,
    assignments: &BTreeMap<String, String>,
    hidden: &[String],
    cached: &[Folder],
) -> Vec<TutaFolder> {
    let by_id: HashMap<&str, &FolderRow> = rows.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut out: Vec<TutaFolder> = Vec::new();
    for row in rows {
        let Some(path) = path_of(row, &by_id) else {
            continue;
        };
        // Two folders by one name under one parent would share a path,
        // which keys the cache: the first listed keeps it.
        if out.iter().any(|f| f.folder.path == path) {
            continue;
        }
        let name = path.rsplit('/').next().unwrap_or(&path).to_string();
        out.push(TutaFolder {
            tuta_id: row.id.clone(),
            folder: Folder {
                id: 0, // assigned by order below
                account_id,
                name,
                unread: cached
                    .iter()
                    .find(|f| f.path == path)
                    .map_or(0, |f| f.unread),
                path,
                kind: folder_kind(row),
                childless: false,
            },
        });
    }
    out.retain(|f| !crate::models::folder_is_hidden(&f.folder.path, Some("/"), hidden));
    number_folders(&mut out, |f| &f.folder, |f| &mut f.folder);
    let mut folders: Vec<Folder> = out.iter().map(|f| f.folder.clone()).collect();
    crate::models::assign_folder_roles(assignments, &mut folders);
    for (tf, f) in out.iter_mut().zip(folders) {
        tf.folder.kind = f.kind;
    }
    out
}
