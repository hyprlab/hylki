//! The tag rows' and smart folders' unread chips (#717). They are counted
//! from the cache, which takes a mark only once the server has it, so every
//! mark made here moves them at once and the recount behind confirms it.
//! A recount that read the cache before a mark reached it would put the old
//! number back for a moment, as if the mark had not taken: such a result is
//! dropped, and the recount the mark's settling asks for replaces it.

use std::collections::HashMap;
use std::time::Instant;

use crate::models::{FolderKind, Message};
use crate::ui::sidebar::{SidebarInput, TagUnread};

use super::model::AppModel;
use super::pending::PENDING_SEEN_MAX;

/// The chips' counts as last shown, and what the next recount is checked
/// against.
#[derive(Default)]
pub(super) struct ViewCounts {
    tags: TagUnread,
    smart: HashMap<String, u32>,
    /// Whether a recount has been shown: before it, there is no number to
    /// move, and a mark is left to the first recount.
    counted: bool,
    /// Bumped by every mark made here.
    marks: u64,
    /// For the recount running: `marks` when it began, and whether a write
    /// was still on its way to the cache then.
    started: Option<(u64, bool)>,
    /// Tags set or taken off, on their way to the server and the cache, by
    /// account, folder path, UID and lower-case keyword.
    tag_writes: HashMap<(u32, String, u32, String), Instant>,
    /// Folders marked read as a whole, by account and folder id.
    folder_writes: HashMap<(u32, u32), Instant>,
}

impl AppModel {
    /// A message here went from `before` to `after` (read state or tags):
    /// move the chips it counts toward now.
    pub(super) fn nudge_view_counts(&mut self, path: &str, before: &Message, after: &Message) {
        let nothing_counted = self.tags.is_empty() && self.smart_folders.is_empty();
        if !self.view_counts.counted || nothing_counted {
            return;
        }
        let account_id = before.account_id;
        let mut tags_moved = false;
        let kind = self.folder_by_path(account_id, path).map(|f| f.kind);
        if !matches!(kind, Some(FolderKind::Trash | FolderKind::Junk)) {
            let labels: Vec<(String, String)> = self
                .tag_label_folders(account_id)
                .into_iter()
                .map(|(kw, f)| (kw, f.path.clone()))
                .collect();
            let unified = self.in_every_account_view(account_id);
            for t in &self.tags {
                let in_label = labels.iter().any(|(kw, p)| *kw == t.keyword && p == path);
                let counts = |m: &Message| m.unread && (in_label || m.has_tag(t));
                let (was, now) = (counts(before), counts(after));
                if was == now {
                    continue;
                }
                let scopes = [Some(account_id)]
                    .into_iter()
                    .chain(unified.then_some(None));
                for scope in scopes {
                    let n = self
                        .view_counts
                        .tags
                        .entry((scope, t.keyword.clone()))
                        .or_default();
                    *n = if now { *n + 1 } else { n.saturating_sub(1) };
                }
                tags_moved = true;
            }
        }
        let mut smart_moved = false;
        for f in self.smart_folders.clone() {
            let in_scope = self.smart_scope(&f).iter().any(|(a, include, exclude)| {
                *a == account_id
                    && include
                        .as_ref()
                        .is_none_or(|paths| paths.iter().any(|p| p == path))
                    && !exclude.iter().any(|p| p == path)
            });
            if !in_scope {
                continue;
            }
            let ctx = self.smart_context(&f);
            let counts = |m: &Message| m.unread && ctx.matches(&f, path, m);
            let (was, now) = (counts(before), counts(after));
            if was == now {
                continue;
            }
            let n = self.view_counts.smart.entry(f.id.clone()).or_default();
            *n = if now { *n + 1 } else { n.saturating_sub(1) };
            smart_moved = true;
        }
        if !(tags_moved || smart_moved) {
            return;
        }
        self.view_counts.marks += 1;
        if tags_moved {
            self.sidebars_emit(SidebarInput::SetTagUnread(self.view_counts.tags.clone()));
        }
        if smart_moved {
            self.sidebars_emit(SidebarInput::SetSmartUnread(self.view_counts.smart.clone()));
        }
    }

    /// A tag is on its way to the server for this message.
    pub(super) fn note_tag_write(&mut self, account_id: u32, path: &str, uid: u32, keyword: &str) {
        self.view_counts.tag_writes.insert(
            (account_id, path.to_string(), uid, keyword.to_lowercase()),
            Instant::now(),
        );
    }

    /// The server has the tag, and the cache with it.
    pub(super) fn tag_write_settled(
        &mut self,
        account_id: u32,
        path: &str,
        uid: u32,
        keyword: &str,
    ) {
        self.view_counts.tag_writes.remove(&(
            account_id,
            path.to_string(),
            uid,
            keyword.to_lowercase(),
        ));
    }

    /// A folder is being marked read as a whole: what its loaded mail
    /// counted goes now.
    pub(super) fn note_folder_read(&mut self, account_id: u32, folder_id: u32) {
        let Some(path) = self.folder_path(account_id, folder_id) else {
            return;
        };
        let unread: Vec<Message> = self
            .message_cache
            .get(&(account_id, folder_id))
            .map(|msgs| msgs.iter().filter(|m| m.unread).cloned().collect())
            .unwrap_or_default();
        for m in unread {
            let mut read = m.clone();
            read.unread = false;
            self.nudge_view_counts(&path, &m, &read);
        }
        self.view_counts
            .folder_writes
            .insert((account_id, folder_id), Instant::now());
    }

    /// The worker reported a folder's count: a whole-folder mark on it is
    /// in the cache by then. Whether one was waiting.
    pub(super) fn folder_write_settled(&mut self, account_id: u32, folder_id: u32) -> bool {
        self.view_counts
            .folder_writes
            .remove(&(account_id, folder_id))
            .is_some()
    }

    /// Whether a mark made here may not be in the cache yet. One that never
    /// settles (a lost connection) stops counting after a minute.
    fn marks_unsettled(&self) -> bool {
        let fresh = |at: &Instant| at.elapsed() < PENDING_SEEN_MAX;
        self.pending_seen.values().any(|(_, at)| fresh(at))
            || self.view_counts.tag_writes.values().any(fresh)
            || self.view_counts.folder_writes.values().any(fresh)
    }

    /// A recount is starting: note what it can be trusted against.
    pub(super) fn view_recount_started(&mut self) {
        self.view_counts.started = Some((self.view_counts.marks, self.marks_unsettled()));
    }

    /// A recount's answer: shown, unless a mark it may not have seen was made
    /// meanwhile or was still on its way when it began.
    pub(super) fn view_recount_done(&mut self, smart: HashMap<String, u32>, tags: TagUnread) {
        if !trusted(self.view_counts.started.take(), self.view_counts.marks) {
            return;
        }
        self.view_counts.counted = true;
        if self.view_counts.smart != smart {
            self.sidebars_emit(SidebarInput::SetSmartUnread(smart.clone()));
            self.view_counts.smart = smart;
        }
        if self.view_counts.tags != tags {
            self.sidebars_emit(SidebarInput::SetTagUnread(tags.clone()));
            self.view_counts.tags = tags;
        }
    }
}

/// Whether a recount may be shown: it began after the last mark made here
/// (`marks` then, against `marks_now`) with every write in the cache. One
/// with no start noted (the demo's, counted on the spot) always may.
fn trusted(started: Option<(u64, bool)>, marks_now: u64) -> bool {
    started.is_none_or(|(marks, unsettled)| !unsettled && marks == marks_now)
}

#[cfg(test)]
mod tests {
    use super::trusted;

    /// A recount that may have read the cache before a mark reached it
    /// would put the old number back on the chip (#717).
    #[test]
    fn a_recount_older_than_a_mark_is_not_shown() {
        assert!(trusted(Some((3, false)), 3));
        assert!(!trusted(Some((3, false)), 4), "a mark made while it ran");
        assert!(!trusted(Some((3, true)), 3), "a write still on its way");
        assert!(trusted(None, 7));
    }
}
