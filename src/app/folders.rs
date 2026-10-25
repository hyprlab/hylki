//! Folder lists: special-folder roles, where sent copies go, unread counts
//! across a re-listing, and keys that follow a renamed folder.

use std::collections::HashMap;

use relm4::prelude::*;

use crate::config;
use crate::i18n::{i18n, i18n_f};
use crate::models::{Folder, FolderKind, Message};
use crate::ui::notifications::NotifyInput;
use crate::ui::sidebar::SidebarInput;
use crate::worker::MailRequest;

use super::model::AppModel;
use super::sidebar::folder_key;
use super::undo::UndoStep;

/// Apply an account's manual special-folder assignments (#82) over the
/// auto-detected kinds (see [`crate::models::assign_folder_roles`]), then
/// re-sort into the fixed role order. Folder ids are untouched — they are
/// referenced from cached messages. The worker already applies the same
/// assignments to every listing it sends; this keeps the live lists right
/// between a Settings save and the reconnect's fresh LIST.
pub(super) fn apply_folder_roles(
    roles: &std::collections::BTreeMap<String, String>,
    folders: &mut [Folder],
) {
    if roles.is_empty() {
        return;
    }
    crate::models::assign_folder_roles(roles, folders);
    folders.sort_by(|a, b| {
        crate::worker::folder_order(a.kind)
            .cmp(&crate::worker::folder_order(b.kind))
            .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
    });
}

/// The folder a sent copy is filed in (#199): the chosen one when the account
/// still has it, else the Sent folder, else nowhere. A choice whose folder was
/// renamed or removed server-side falls back rather than appending into a
/// mailbox the server would have to create under an old name.
fn pick_sent_copy(chosen: Option<&str>, folders: &[Folder]) -> Option<String> {
    chosen
        .filter(|path| folders.iter().any(|f| f.path == *path))
        .map(str::to_string)
        .or_else(|| {
            folders
                .iter()
                .find(|f| f.kind == FolderKind::Sent)
                .map(|f| f.path.clone())
        })
}

/// Unread counts for a fresh folder listing of `account_id`, as
/// (folder id, count). Merged by PATH, and a zero never overwrites a known
/// count: a refresh's per-folder STATUS can fail silently (Gmail right after
/// a RENAME answers zeros), and adopting them wholesale wiped every unread
/// chip. A genuine zero re-asserts through the per-folder sync events.
/// `known` is the app's own count per (account, folder id), which wins over
/// what the old listing said.
pub(super) fn merged_unread(
    account_id: u32,
    old: Option<&[Folder]>,
    known: &HashMap<(u32, u32), u32>,
    folders: &[Folder],
) -> Vec<(u32, u32)> {
    let prev_by_path: HashMap<&str, u32> = old
        .map(|old| {
            old.iter()
                .map(|f| {
                    let n = known.get(&(account_id, f.id)).copied().unwrap_or(f.unread);
                    (f.path.as_str(), n)
                })
                .collect()
        })
        .unwrap_or_default();
    folders
        .iter()
        .map(|f| {
            let known = prev_by_path.get(f.path.as_str()).copied().unwrap_or(0);
            (f.id, if f.unread > 0 { f.unread } else { known })
        })
        .collect()
}

/// Whether two listings show the same folders: same ids, paths, kinds and
/// names, in the same order. Unread counts do not count.
pub(super) fn same_folder_list(old: &[Folder], new: &[Folder]) -> bool {
    old.len() == new.len()
        && old
            .iter()
            .zip(new)
            .all(|(a, b)| a.id == b.id && a.path == b.path && a.kind == b.kind && a.name == b.name)
}

/// A sidebar key ("email\tpath", `key_prefix` being "email\t") after the
/// folder `path` became `new_path`: the folder's own key and its children's
/// follow it, any other key is left alone.
fn rekey_folder_key(key: &mut String, key_prefix: &str, path: &str, new_path: &str, delim: char) {
    let Some(rest) = key.strip_prefix(key_prefix) else {
        return;
    };
    if rest == path {
        *key = format!("{key_prefix}{new_path}");
    } else if let Some(r) = rest.strip_prefix(&format!("{path}{delim}")) {
        *key = format!("{key_prefix}{new_path}{delim}{r}");
    }
}

pub(super) fn kind_label(kind: FolderKind) -> String {
    match kind {
        FolderKind::Archive => i18n("archive"),
        FolderKind::Trash => i18n("trash"),
        _ => i18n("destination"),
    }
}

impl AppModel {
    /// One of an account's folders, by id.
    pub(super) fn folder_by_id(&self, account_id: u32, folder_id: u32) -> Option<&Folder> {
        self.folders
            .get(&account_id)?
            .iter()
            .find(|f| f.id == folder_id)
    }

    /// One of an account's folders, by path.
    pub(super) fn folder_by_path(&self, account_id: u32, path: &str) -> Option<&Folder> {
        self.folders
            .get(&account_id)?
            .iter()
            .find(|f| f.path == path)
    }

    /// The path of one of an account's folders, by id.
    pub(super) fn folder_path(&self, account_id: u32, folder_id: u32) -> Option<String> {
        self.folder_by_id(account_id, folder_id)
            .map(|f| f.path.clone())
    }

    /// Whether the given folder is the account's Drafts folder.
    pub(super) fn is_drafts_folder(&self, account_id: u32, folder_id: u32) -> bool {
        self.folder_kind(account_id, folder_id) == Some(FolderKind::Drafts)
    }

    /// The kind of a folder by id, if known.
    pub(super) fn folder_kind(&self, account_id: u32, folder_id: u32) -> Option<FolderKind> {
        self.folder_by_id(account_id, folder_id).map(|f| f.kind)
    }

    /// The Drafts folder a draft of `account_id` is saved to: the existing
    /// one, else a default path the worker creates.
    pub(super) fn drafts_target(&self, account_id: u32) -> Option<(u32, String)> {
        self.folders
            .get(&account_id)
            .and_then(|fs| fs.iter().find(|f| f.kind == FolderKind::Drafts))
            .map(|f| (f.id, f.path.clone()))
            .or_else(|| {
                self.default_folder_path(account_id, FolderKind::Drafts)
                    .map(|p| (0, p))
            })
    }

    /// The Templates folder a template of `account_id` is saved to (#360):
    /// the existing one, else a default path the worker creates.
    pub(super) fn templates_target(&self, account_id: u32) -> Option<(u32, String)> {
        self.folder_of_kind(account_id, FolderKind::Templates)
            .map(|f| (f.id, f.path.clone()))
            .or_else(|| {
                self.default_folder_path(account_id, FolderKind::Templates)
                    .map(|p| (0, p))
            })
    }

    /// Destination path for `kind` on an account: its existing folder of that kind,
    /// else a sensible default (the worker creates it on the server on first move).
    pub(super) fn folder_path_for(&self, account_id: u32, kind: FolderKind) -> Option<String> {
        self.folders
            .get(&account_id)
            .and_then(|fs| fs.iter().find(|f| f.kind == kind))
            .map(|f| f.path.clone())
            .or_else(|| self.default_folder_path(account_id, kind))
    }

    /// Make a folder at `path`. Optimistic: it appears in the sidebar right
    /// away (a server round-trip took seconds); the worker's confirming
    /// refresh matches the prediction and repaints nothing.
    pub(super) fn create_folder(&mut self, account_id: u32, path: String) {
        let delim = self.folder_delimiter(account_id);
        let leaf = path.rsplit(delim).next().unwrap_or(&path).to_string();
        if !self
            .folders
            .get(&account_id)
            .is_some_and(|fs| fs.iter().any(|f| f.path == path))
        {
            self.folders.entry(account_id).or_default().push(Folder {
                id: 0,
                account_id,
                name: crate::mail::mutf7::decode(&leaf),
                path: path.clone(),
                kind: FolderKind::Custom,
                unread: 0,
            });
            self.normalize_folders(account_id);
            self.rebuild_sidebar();
        }
        self.send_to(account_id, MailRequest::CreateFolder { path });
    }

    /// Hide folders (#239): remembered on the account, taken off the
    /// sidebar at once, and the worker told so its next listing, and every
    /// sync after it, leaves them out. A hidden folder that was open gives
    /// way to nothing, like a deleted one.
    pub(super) fn hide_folders(&mut self, account_id: u32, paths: Vec<String>) {
        let Some(cfg) = self.config.get_mut(account_id as usize - 1) else {
            return;
        };
        let mut changed = false;
        for path in paths {
            if !cfg.hidden_folders.contains(&path) {
                cfg.hidden_folders.push(path);
                changed = true;
            }
        }
        if !changed {
            return;
        }
        let hidden = cfg.hidden_folders.clone();
        if let Err(e) = config::save(&self.config) {
            self.notice_error(i18n_f(
                "Could not save account: {e}",
                &[("e", &(e).to_string())],
            ));
        }
        if self.selected.as_ref().is_some_and(|s| {
            s.account_id == account_id && crate::models::folder_is_hidden(&s.path, None, &hidden)
        }) {
            self.blank_for_folder_change();
        }
        if let Some(folders) = self.folders.get_mut(&account_id) {
            folders.retain(|f| !crate::models::folder_is_hidden(&f.path, None, &hidden));
        }
        self.rebuild_sidebar();
        self.send_to(account_id, MailRequest::SetHiddenFolders { paths: hidden });
    }

    /// Remove a folder, its messages going to Trash first so nothing is lost
    /// even when this is undoing a folder someone has since filed mail into.
    pub(super) fn delete_folder(&mut self, account_id: u32, path: String) {
        let trash = self.folder_path_for(account_id, FolderKind::Trash);
        // If the deleted folder is currently open, clear the view.
        if self
            .selected
            .as_ref()
            .is_some_and(|s| s.account_id == account_id && s.path == path)
        {
            self.blank_for_folder_change();
        }
        // A deleted folder leaves Favorites (#367), with what was inside it.
        if let Some(email) = self.email_of(account_id) {
            let own = folder_key(&email, &path);
            let inside = format!("{own}{}", self.folder_delimiter(account_id));
            let before = self.favorites.len();
            self.favorites
                .retain(|k| *k != own && !k.starts_with(&inside));
            if self.favorites.len() != before {
                self.save_sidebar_state();
                self.rebuild_sidebar();
            }
        }
        self.send_to(account_id, MailRequest::DeleteFolder { path, trash });
    }

    /// Move a folder under `dest` ("" for the top level). Returns its new
    /// path when the move went ahead, `None` when it was refused.
    ///
    /// Move a custom folder under a new parent (or "" = the account's top
    /// level) via IMAP RENAME, after checking the move makes sense. The server
    /// carries any sub-hierarchy along with it.
    pub(super) fn move_folder(
        &mut self,
        account_id: u32,
        path: String,
        dest: String,
    ) -> Option<String> {
        let complain = |me: &Self, text: &str| {
            me.notifications.emit(NotifyInput::Push {
                text: text.to_string(),
                error: true,
                connectivity: false,
            });
        };
        let delim = self.folder_delimiter(account_id);
        // Into itself or its own subtree: there is no such place.
        if dest == path || dest.starts_with(&format!("{path}{delim}")) {
            complain(self, "A folder can't be moved into itself.");
            return None;
        }
        let folders = self.folders.get(&account_id).cloned().unwrap_or_default();
        // Only your own folders (or the top level) can hold other folders —
        // moving one under Sent or Trash is never what a drop meant.
        if !dest.is_empty()
            && !folders
                .iter()
                .any(|f| f.path == dest && f.kind == FolderKind::Custom)
        {
            return None;
        }
        let leaf = path.rsplit(delim).next().unwrap_or(&path).to_string();
        let new_path = if dest.is_empty() {
            format!("{}{leaf}", self.folder_namespace(account_id))
        } else {
            format!("{dest}{delim}{leaf}")
        };
        if new_path == path {
            return None;
        }
        if folders.iter().any(|f| f.path == new_path) {
            complain(self, &format!("A folder named {leaf:?} is already there."));
            return None;
        }
        self.apply_folder_rename(
            account_id,
            path,
            new_path.clone(),
            Some(i18n("Move Folder")),
        );
        Some(new_path)
    }

    /// Bring an account's local folder list back to exactly the shape the
    /// worker reports: pin the freshest unread counts onto the folders, apply
    /// the worker's sort, reassign index-based ids, and re-key the unread map
    /// and the selection. Run after any optimistic mutation (move, rename,
    /// create) so the confirming server refresh is a recognised no-op.
    fn normalize_folders(&mut self, account_id: u32) {
        let Some(folders) = self.folders.get_mut(&account_id) else {
            return;
        };
        for f in folders.iter_mut() {
            if let Some(u) = self.folder_unread.get(&(account_id, f.id)) {
                f.unread = *u;
            }
        }
        folders.sort_by(|a, b| {
            crate::worker::folder_order(a.kind)
                .cmp(&crate::worker::folder_order(b.kind))
                .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
        });
        for (i, f) in folders.iter_mut().enumerate() {
            f.id = i as u32 + 1;
        }
        self.folder_unread.retain(|(a, _), _| *a != account_id);
        for f in folders.iter() {
            self.folder_unread.insert((account_id, f.id), f.unread);
        }
        if let Some(sel) = self.selected.as_mut() {
            if sel.account_id == account_id {
                if let Some(f) = folders.iter().find(|f| f.path == sel.path) {
                    sel.folder_id = f.id;
                }
            }
        }
    }

    /// The shared optimistic machinery behind both folder moves and renames:
    /// clear the view if the affected subtree is open, reshape the local
    /// folder list exactly as the worker will report it back, re-key
    /// everything id- or path-addressed, and hand the RENAME to the server.
    pub(super) fn apply_folder_rename(
        &mut self,
        account_id: u32,
        path: String,
        new_path: String,
        record: Option<String>,
    ) {
        // Recorded as the rename that puts it back. `None` while an undo or
        // redo is doing exactly that — it keeps its own history.
        if let Some(what) = record {
            self.push_undo(
                account_id,
                what,
                UndoStep::RenameFolder {
                    from: new_path.clone(),
                    to: path.clone(),
                },
            );
        }
        let delim = self.folder_delimiter(account_id);
        // If the affected folder (or one of its children) is open, clear the
        // view — its path is about to stop existing.
        if self.selected.as_ref().is_some_and(|s| {
            s.account_id == account_id
                && (s.path == path || s.path.starts_with(&format!("{path}{delim}")))
        }) {
            self.blank_for_folder_change();
        }

        self.reshape_renamed_tree(account_id, &path, &new_path, delim);
        self.rekey_sidebar_state(account_id, &path, &new_path, delim);
        self.follow_renamed_selection(account_id, &path, &new_path, delim);
        self.rebuild_sidebar();
        if let Some(sel) = self.selected.clone().filter(|s| s.account_id == account_id) {
            self.sidebars_emit(SidebarInput::SelectFolderRow {
                account_id,
                path: sel.path.clone(),
            });
        }
        self.send_to(
            account_id,
            MailRequest::RenameFolder {
                old_path: path,
                new_path,
            },
        );
        // The worker runs requests in order: the reload queues behind the
        // RENAME and refills the cleared view from the folder's new name.
        if let Some(sel) = self.selected.clone().filter(|s| s.account_id == account_id) {
            self.send_to(
                account_id,
                MailRequest::LoadMessages {
                    folder_id: sel.folder_id,
                    path: sel.path,
                },
            );
        }
    }

    /// Optimistic: reshape the local tree NOW so the sidebar shows the move
    /// instantly; the server rename confirms in the background. The reshape
    /// mirrors the worker exactly — same subtree rewrite the server does,
    /// same sort, same index-assigned ids — so the confirming refresh is an
    /// identical list and SetFolders repaints nothing.
    fn reshape_renamed_tree(&mut self, account_id: u32, path: &str, new_path: &str, delim: char) {
        let old_prefix = format!("{path}{delim}");
        let new_prefix = format!("{new_path}{delim}");
        if let Some(folders) = self.folders.get_mut(&account_id) {
            for f in folders.iter_mut() {
                if f.path == path {
                    f.path = new_path.to_string();
                    let leaf = f.path.rsplit(delim).next().unwrap_or(&f.path);
                    f.name = crate::mail::mutf7::decode(leaf);
                } else if let Some(rest) = f.path.strip_prefix(&old_prefix) {
                    f.path = format!("{new_prefix}{rest}");
                }
            }
        }
        self.normalize_folders(account_id);
    }

    /// Collapsed tree nodes, favorites and the folder order follow the
    /// moved subtree to its new home.
    fn rekey_sidebar_state(&mut self, account_id: u32, path: &str, new_path: &str, delim: char) {
        if let Some(email) = self
            .accounts
            .iter()
            .find(|a| a.id == account_id)
            .map(|a| a.email.clone())
        {
            let key_prefix = format!("{email}\t");
            for k in self.tree_collapsed.iter_mut() {
                rekey_folder_key(k, &key_prefix, path, new_path, delim);
            }
            // So do favorites (#367), keeping their place in the list.
            for k in self.favorites.iter_mut() {
                rekey_folder_key(k, &key_prefix, path, new_path, delim);
            }
            // So does the folder order. A renamed folder keeps its place; one
            // moved under another parent has left the siblings it was placed
            // among, and joins its new ones at the end.
            let parent = |p: &str| p.rsplit_once(delim).map(|(head, _)| head.to_string());
            let moved = parent(path) != parent(new_path);
            let own_key = format!("{key_prefix}{path}");
            if moved {
                self.folder_order.retain(|k| *k != own_key);
            }
            for k in self.folder_order.iter_mut() {
                rekey_folder_key(k, &key_prefix, path, new_path, delim);
            }
            self.save_sidebar_state();
        }
    }

    /// The open folder follows its rename (or its parent's): the
    /// selection used to keep the old path, and every auto-fetch asked
    /// the server for a mailbox that no longer existed.
    fn follow_renamed_selection(
        &mut self,
        account_id: u32,
        path: &str,
        new_path: &str,
        delim: char,
    ) {
        let old_prefix = format!("{path}{delim}");
        let new_prefix = format!("{new_path}{delim}");
        let followed = self.selected.as_ref().and_then(|s| {
            if s.account_id != account_id {
                return None;
            }
            if s.path == path {
                Some(new_path.to_string())
            } else {
                s.path
                    .strip_prefix(&old_prefix)
                    .map(|rest| format!("{new_prefix}{rest}"))
            }
        });
        if let Some(moved) = followed {
            let id = self.folder_by_path(account_id, &moved).map(|f| f.id);
            if let (Some(sel), Some(id)) = (self.selected.as_mut(), id) {
                sel.path = moved.clone();
                sel.folder_id = id;
            }
        }
    }

    /// The account's hierarchy delimiter, inferred from its folder paths (the
    /// namespace prefix's last character when there is one, else the first
    /// common delimiter any path uses; '/' as the fallback).
    pub(super) fn folder_delimiter(&self, account_id: u32) -> char {
        let ns = self.folder_namespace(account_id);
        if let Some(c) = ns.chars().last() {
            if matches!(c, '/' | '.' | '\\') {
                return c;
            }
        }
        // Prefer '/' over '.' over '\' across the whole list, so one folder
        // with a dotted display name can't masquerade as the hierarchy
        // separator on a slash-delimited server.
        let fs = self.folders.get(&account_id);
        for d in ['/', '.', '\\'] {
            if fs.is_some_and(|fs| fs.iter().any(|f| f.path.contains(d))) {
                return d;
            }
        }
        '/'
    }

    /// The mailbox namespace prefix for an account: "INBOX<delim>" on servers
    /// that nest everything under INBOX (Dovecot-style), otherwise "". Only
    /// the INBOX root counts — deriving it from any nested folder's parent
    /// (as this once did) made "top level" mean "under whichever sub-folder
    /// happened to be listed first" on servers like iCloud, so new folders
    /// and header-dropped moves landed inside a random folder.
    pub(super) fn folder_namespace(&self, account_id: u32) -> String {
        let Some(folders) = self.folders.get(&account_id) else {
            return String::new();
        };
        Self::infer_folder_namespace(folders)
    }

    fn infer_folder_namespace(folders: &[Folder]) -> String {
        let delim = folders
            .iter()
            .find_map(|f| f.path.chars().find(|c| matches!(c, '/' | '.' | '\\')))
            .unwrap_or('/');
        let root = format!("INBOX{delim}");
        let customs: Vec<&Folder> = folders
            .iter()
            .filter(|f| f.kind == FolderKind::Custom)
            .collect();
        if !customs.is_empty()
            && customs.iter().all(|f| {
                f.path
                    .get(..root.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&root))
            })
        {
            root
        } else {
            String::new()
        }
    }

    /// A sensible destination path for a standard folder the account doesn't have
    /// yet (Archive/Trash/Junk), matching the account's folder namespace so the
    /// server creates it in the right place (e.g. "INBOX.Archive").
    fn default_folder_path(&self, account_id: u32, kind: FolderKind) -> Option<String> {
        let leaf = match kind {
            FolderKind::Archive => "Archive",
            FolderKind::Trash => "Trash",
            FolderKind::Junk => "Junk",
            FolderKind::Drafts => "Drafts",
            FolderKind::Templates => "Templates",
            FolderKind::Snoozed => "Snoozed",
            _ => return None,
        };
        self.folders.get(&account_id)?; // require folders to be loaded
        Some(format!("{}{leaf}", self.folder_namespace(account_id)))
    }

    /// Find a cached message by (account, id) for drag-and-drop moves.
    pub(super) fn find_cached_message(&self, account_id: u32, id: u32) -> Option<Message> {
        for ((aid, _), msgs) in self.message_cache.iter() {
            if *aid == account_id {
                if let Some(m) = msgs.iter().find(|m| m.id == id) {
                    return Some(m.clone());
                }
            }
        }
        self.unified_slices
            .iter()
            .filter(|((a, _), _)| *a == account_id)
            .flat_map(|(_, msgs)| msgs.iter())
            .find(|m| m.id == id)
            .cloned()
    }

    /// The IMAP folder path a message lives in (its account's folder by id).
    pub(super) fn resolve_folder_path(&self, m: &Message) -> Option<String> {
        self.folder_path(m.account_id, m.folder_id)
    }

    /// An account's Inbox folder, if known.
    pub(super) fn inbox_of(&self, account_id: u32) -> Option<&Folder> {
        self.folder_of_kind(account_id, FolderKind::Inbox)
    }

    /// Where a copy of outgoing mail is filed (#199): the folder chosen in the
    /// account editor if it still exists, else the Sent folder. `None` when
    /// the account has neither — the message is sent without a copy.
    pub(super) fn sent_copy_path(&self, account_id: u32) -> Option<String> {
        let cfg = self.account_config(account_id);
        // The server files its own copy: appending a second one is what makes
        // a Gmail account show every sent message twice.
        if cfg.is_some_and(|c| c.server_saves_sent) {
            return None;
        }
        let chosen = cfg.and_then(|c| c.sent_copy_path.as_deref());
        pick_sent_copy(
            chosen,
            self.folders.get(&account_id).map_or(&[], Vec::as_slice),
        )
    }

    /// An account's folder of a kind, if it has one (the first, for the
    /// special kinds a server lists once).
    pub(super) fn folder_of_kind(&self, account_id: u32, kind: FolderKind) -> Option<&Folder> {
        self.folders
            .get(&account_id)?
            .iter()
            .find(|f| f.kind == kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: u32, path: &str, unread: u32) -> Folder {
        Folder {
            id,
            account_id: 1,
            name: path.into(),
            path: path.into(),
            kind: FolderKind::Custom,
            unread,
        }
    }

    #[test]
    fn folder_roles_override_detection_and_demote_the_old_holder() {
        use crate::models::{Folder, FolderKind};
        let f = |id: u32, path: &str, kind: FolderKind| Folder {
            id,
            account_id: 1,
            name: path.to_string(),
            path: path.to_string(),
            kind,
            unread: 0,
        };
        let mut folders = vec![
            f(1, "INBOX", FolderKind::Inbox),
            f(2, "Sent Items", FolderKind::Custom),
            f(3, "Sent", FolderKind::Sent),
            f(4, "Rubbish", FolderKind::Custom),
        ];
        let roles: std::collections::BTreeMap<String, String> = [
            ("sent".to_string(), "Sent Items".to_string()),
            ("trash".to_string(), "Rubbish".to_string()),
            // A mapping whose folder no longer exists changes nothing.
            ("junk".to_string(), "Gone".to_string()),
        ]
        .into();
        super::apply_folder_roles(&roles, &mut folders);
        let kind_of = |path: &str| folders.iter().find(|f| f.path == path).unwrap().kind;
        assert_eq!(kind_of("Sent Items"), FolderKind::Sent);
        assert_eq!(kind_of("Sent"), FolderKind::Custom, "old holder demotes");
        assert_eq!(kind_of("Rubbish"), FolderKind::Trash);
        assert_eq!(kind_of("INBOX"), FolderKind::Inbox);
        // Ids survive the re-sort (cached messages reference them).
        assert_eq!(
            folders.iter().find(|f| f.path == "Sent Items").unwrap().id,
            2
        );
    }

    #[test]
    fn a_role_never_takes_the_inbox() {
        let mut folders = vec![
            Folder {
                id: 1,
                account_id: 1,
                name: "Inbox".into(),
                path: "INBOX".into(),
                kind: FolderKind::Inbox,
                unread: 0,
            },
            Folder {
                id: 2,
                account_id: 1,
                name: "Sent".into(),
                path: "Sent".into(),
                kind: FolderKind::Sent,
                unread: 0,
            },
        ];
        let mut roles = std::collections::BTreeMap::new();
        roles.insert("sent".to_string(), "INBOX".to_string());
        super::apply_folder_roles(&roles, &mut folders);
        assert_eq!(
            folders.iter().find(|f| f.path == "INBOX").unwrap().kind,
            FolderKind::Inbox
        );
        assert_eq!(
            folders.iter().find(|f| f.path == "Sent").unwrap().kind,
            FolderKind::Sent
        );
    }

    #[test]
    fn sent_copies_go_where_the_account_says_including_the_inbox() {
        let f = |id: u32, path: &str, kind: FolderKind| Folder {
            id,
            account_id: 1,
            name: path.to_string(),
            path: path.to_string(),
            kind,
            unread: 0,
        };
        let folders = vec![
            f(1, "INBOX", FolderKind::Inbox),
            f(2, "Sent", FolderKind::Sent),
            f(3, "Archive", FolderKind::Archive),
        ];
        // Nothing chosen: the Sent folder, as before.
        assert_eq!(
            super::pick_sent_copy(None, &folders).as_deref(),
            Some("Sent")
        );
        // The Inbox is a legitimate choice (#199) — unlike a role (#136), a
        // destination takes nothing away from the folder it names.
        assert_eq!(
            super::pick_sent_copy(Some("INBOX"), &folders).as_deref(),
            Some("INBOX")
        );
        assert_eq!(
            super::pick_sent_copy(Some("Archive"), &folders).as_deref(),
            Some("Archive")
        );
        // A folder that has gone from the server falls back to Sent.
        assert_eq!(
            super::pick_sent_copy(Some("Gone"), &folders).as_deref(),
            Some("Sent")
        );
        // No Sent folder and no choice: the message goes out uncopied.
        let bare = vec![f(1, "INBOX", FolderKind::Inbox)];
        assert_eq!(super::pick_sent_copy(None, &bare), None);
        assert_eq!(super::pick_sent_copy(Some("Gone"), &bare), None);
        assert_eq!(
            super::pick_sent_copy(Some("INBOX"), &bare).as_deref(),
            Some("INBOX")
        );
    }

    #[test]
    fn unread_counts_merge_by_path_and_never_drop_to_zero() {
        let old = vec![folder(1, "INBOX", 5), folder(2, "A", 3), folder(3, "B", 0)];
        let mut known = HashMap::new();
        known.insert((1, 2), 7);
        known.insert((2, 1), 99);
        let new = vec![
            folder(1, "INBOX", 0),
            folder(2, "B", 0),
            folder(3, "A", 0),
            folder(4, "C", 2),
        ];
        assert_eq!(
            merged_unread(1, Some(&old), &known, &new),
            vec![(1, 5), (2, 0), (3, 7), (4, 2)]
        );
        assert_eq!(merged_unread(1, None, &known, &new[..1]), vec![(1, 0)]);
    }

    #[test]
    fn folder_lists_compare_without_their_counts() {
        let a = vec![folder(1, "INBOX", 5), folder(2, "A", 3)];
        let mut b = a.clone();
        b[1].unread = 0;
        assert!(same_folder_list(&a, &b));
        b[1].name = "a".into();
        assert!(!same_folder_list(&a, &b));
        assert!(!same_folder_list(&a, &a[..1]));
    }

    #[test]
    fn sidebar_keys_follow_a_renamed_folder_and_its_children() {
        let rekey = |k: &str, delim| {
            let mut k = k.to_string();
            rekey_folder_key(&mut k, "me@x\t", "A/B", "A/C", delim);
            k
        };
        assert_eq!(rekey("me@x\tA/B", '/'), "me@x\tA/C");
        assert_eq!(rekey("me@x\tA/B/D", '/'), "me@x\tA/C/D");
        assert_eq!(rekey("me@x\tA/BB", '/'), "me@x\tA/BB");
        assert_eq!(rekey("you@x\tA/B", '/'), "you@x\tA/B");
        assert_eq!(rekey("me@x\tA/B.D", '.'), "me@x\tA/C.D");
    }

    #[test]
    fn folder_namespace_handles_utf8_paths() {
        use crate::models::{Folder, FolderKind};
        let custom = |path: &str| Folder {
            id: 1,
            account_id: 1,
            name: path.to_string(),
            path: path.to_string(),
            kind: FolderKind::Custom,
            unread: 0,
        };
        // Byte six falls inside a character in these top-level paths.
        for path in ["abcd摘要", "a收摘要", "📧摘要", "é日語"] {
            assert_eq!(
                super::AppModel::infer_folder_namespace(&[custom(path)]),
                "",
                "{path}"
            );
        }
        for (path, expected) in [
            ("INBOX/中文", "INBOX/"),
            ("inbox/中文", "INBOX/"),
            ("INBOX.中文", "INBOX."),
            ("INBOX\\中文", "INBOX\\"),
            ("INBOX/Projects/Nested", "INBOX/"),
            ("Other/中文", ""),
            ("收件摘要", ""),
            ("INBOX", ""),
            ("", ""),
        ] {
            assert_eq!(
                super::AppModel::infer_folder_namespace(&[custom(path)]),
                expected,
                "{path}"
            );
        }
        assert_eq!(
            super::AppModel::infer_folder_namespace(&[custom("INBOX/中文"), custom("abcd摘要")]),
            ""
        );
        assert_eq!(super::AppModel::infer_folder_namespace(&[]), "");
        let mut inbox = custom("INBOX");
        inbox.kind = FolderKind::Inbox;
        assert_eq!(
            super::AppModel::infer_folder_namespace(&[inbox.clone()]),
            ""
        );
        assert_eq!(
            super::AppModel::infer_folder_namespace(&[inbox, custom("INBOX/中文")]),
            "INBOX/"
        );
    }

    /// B17 in the map: the delimiter is the first separator found in any
    /// path, so a '/' server whose first folder has a '.' in its name gets
    /// no namespace.
    #[test]
    fn folder_namespace_takes_the_first_separator_seen() {
        let custom = |path: &str| Folder {
            id: 1,
            account_id: 1,
            name: path.into(),
            path: path.into(),
            kind: FolderKind::Custom,
            unread: 0,
        };
        assert_eq!(
            AppModel::infer_folder_namespace(&[custom("INBOX.a"), custom("INBOX.b")]),
            "INBOX."
        );
        assert_eq!(
            AppModel::infer_folder_namespace(&[custom("v1.2"), custom("INBOX/b")]),
            ""
        );
    }
}
