//! Each account's folder hierarchy as its worker reports it (#554), and the
//! paths built from it: a folder made, renamed or moved goes where the
//! server's own delimiter and namespace put it.

use crate::i18n::i18n;
use crate::models::FolderHierarchy;

use super::model::AppModel;
use super::undo::UndoStep;

impl AppModel {
    /// How the account's folders nest. `None` until its worker has said:
    /// an IMAP account cached by a build from before #554, until it
    /// connects.
    pub(super) fn hierarchy(&self, account_id: u32) -> Option<&FolderHierarchy> {
        self.hierarchies.get(&account_id)
    }

    /// The hierarchy to draw the sidebar by: the reported one, else what
    /// the paths suggest until it comes.
    pub(super) fn drawn_hierarchy(&self, account_id: u32) -> FolderHierarchy {
        self.hierarchies
            .get(&account_id)
            .cloned()
            .unwrap_or_else(|| {
                FolderHierarchy::inferred(
                    self.folders.get(&account_id).map_or(&[][..], Vec::as_slice),
                )
            })
    }

    /// A folder made at `asked` is at `made` on the server: its undo, and
    /// its redo, delete the one there is.
    pub(super) fn folder_made_at(&mut self, account_id: u32, asked: &str, made: &str) {
        for entry in self.undo_stack.iter_mut().chain(self.redo_stack.iter_mut()) {
            if entry.account_id != account_id {
                continue;
            }
            match &mut entry.step {
                UndoStep::DeleteFolder { path } | UndoStep::CreateFolder { path }
                    if path == asked =>
                {
                    *path = made.to_string();
                }
                _ => {}
            }
        }
    }

    /// The hierarchy to build a new path with, or `None` after saying why
    /// there is none yet.
    pub(super) fn hierarchy_for_change(&self, account_id: u32) -> Option<FolderHierarchy> {
        let h = self.hierarchy(account_id).cloned();
        if h.is_none() {
            self.notice_error(i18n(
                "The account has to connect before its folders can change.",
            ));
        }
        h
    }
}

/// Where the folder at `p` is once the folder at `path` has become
/// `new_path`: the folder itself, or one below it, which goes along.
/// `None` for any other folder.
pub(super) fn follow_rename(
    h: &FolderHierarchy,
    p: &str,
    path: &str,
    new_path: &str,
) -> Option<String> {
    if p == path {
        Some(new_path.to_string())
    } else if h.is_under(p, path) {
        Some(format!("{new_path}{}", &p[path.len()..]))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rename_takes_only_what_is_below_by_the_real_delimiter() {
        let slash = FolderHierarchy::imap(Some('/'), Some(String::new()));
        let at = |p| follow_rename(&slash, p, "v1", "Old/v1");
        assert_eq!(at("v1").as_deref(), Some("Old/v1"));
        assert_eq!(at("v1/a/b").as_deref(), Some("Old/v1/a/b"));
        assert_eq!(at("v1.2"), None);
        assert_eq!(at("v12"), None);
        let flat = FolderHierarchy::imap(None, Some(String::new()));
        assert_eq!(follow_rename(&flat, "a/b", "a", "c"), None);
        assert_eq!(follow_rename(&flat, "a", "a", "c").as_deref(), Some("c"));
    }
}
