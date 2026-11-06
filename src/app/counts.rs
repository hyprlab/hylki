//! Unread counts: which folders count toward an account's total, and
//! keeping the counted folders' lists in step with their counts.

use crate::models::{Folder, FolderKind};
use crate::ui::sidebar::UnifiedFolderRef;
use crate::worker::MailRequest;

use super::model::AppModel;

impl AppModel {
    /// The folders an account's own "Filtered Folders" section lists: the
    /// destination of every one of its rules, in rule order, without
    /// repeats — whether or not the rule opted into the unified section.
    /// An inbox destination is the account's Inbox row already.
    pub(super) fn account_filtered_folders(&self, account_id: u32) -> Vec<Folder> {
        let mut out: Vec<Folder> = Vec::new();
        let Some(folders) = self.folders.get(&account_id) else {
            return out;
        };
        let Some(email) = self.email_of(account_id) else {
            return out;
        };
        let rules = self
            .filters
            .iter()
            .filter(|r| r.account_email.eq_ignore_ascii_case(&email));
        for r in rules {
            let Some(f) = folders.iter().find(|f| f.path == r.dest_path) else {
                continue;
            };
            if f.kind == FolderKind::Inbox || out.iter().any(|o| o.id == f.id) {
                continue;
            }
            let mut folder = f.clone();
            folder.unread = self.folder_unread_of(f);
            out.push(folder);
        }
        out
    }

    /// The folders whose unread mail makes up an account's unread total: its
    /// inbox, then the destination of every filter rule marked to count
    /// (#116), in rule order and without repeats. Trash and Junk never
    /// count, whatever a rule says: mail filed there is being discarded.
    pub(super) fn counted_folders(&self, account_id: u32) -> Vec<&Folder> {
        let mut out: Vec<&Folder> = Vec::new();
        if let Some(inbox) = self.inbox_of(account_id) {
            out.push(inbox);
        }
        let Some(folders) = self.folders.get(&account_id) else {
            return out;
        };
        let Some(email) = self.email_of(account_id) else {
            return out;
        };
        let rules = self
            .filters
            .iter()
            .filter(|r| r.count_unread && r.account_email.eq_ignore_ascii_case(&email));
        for r in rules {
            let Some(f) = folders.iter().find(|f| f.path == r.dest_path) else {
                continue;
            };
            if matches!(f.kind, FolderKind::Trash | FolderKind::Junk)
                || out.iter().any(|o| o.id == f.id)
            {
                continue;
            }
            out.push(f);
        }
        out
    }

    /// The favorite folders (#367) as the sidebar draws them, in the order
    /// they were added. One whose account or folder is not listed (offline
    /// with nothing cached, hidden) is left out until it is.
    pub(super) fn favorite_folder_refs(&self) -> Vec<UnifiedFolderRef> {
        self.favorites
            .iter()
            .filter_map(|key| {
                let (email, path) = key.split_once('\t')?;
                let account = self.accounts.iter().find(|a| a.email == email)?;
                let f = self.folder_by_path(account.id, path)?;
                let mut folder = f.clone();
                folder.unread = self.folder_unread_of(f);
                Some(UnifiedFolderRef {
                    account_id: account.id,
                    folder,
                })
            })
            .collect()
    }

    /// Whether the Filters section is in the sidebar.
    pub(super) fn filtered_shown(&self) -> bool {
        if self.focus.enabled {
            self.focus.show_filters
        } else {
            self.filtered_placement.shown()
        }
    }

    /// The folders the unified "Filtered Folders" section lists: the
    /// destination of every rule, in account order then rule order, without
    /// repeats; nothing when Settings → Sidebar has the section off. An
    /// inbox destination is already an All Inboxes row and is skipped, and
    /// an account left out of the unified section (#267) lists its rules'
    /// folders under its own section only.
    pub(super) fn unified_folder_refs(&self) -> Vec<UnifiedFolderRef> {
        let mut out: Vec<UnifiedFolderRef> = Vec::new();
        if !self.filtered_shown() {
            return out;
        }
        for email in self.ordered_emails() {
            let Some(account) = self.accounts.iter().find(|a| a.email == email) else {
                continue;
            };
            if !self.in_unified(account.id) {
                continue;
            }
            let Some(folders) = self.folders.get(&account.id) else {
                continue;
            };
            let rules = self
                .filters
                .iter()
                .filter(|r| r.account_email.eq_ignore_ascii_case(&email));
            for r in rules {
                let Some(f) = folders.iter().find(|f| f.path == r.dest_path) else {
                    continue;
                };
                if f.kind == FolderKind::Inbox
                    || out
                        .iter()
                        .any(|o| o.account_id == account.id && o.folder.id == f.id)
                {
                    continue;
                }
                let mut folder = f.clone();
                folder.unread = self.folder_unread_of(f);
                out.push(UnifiedFolderRef {
                    account_id: account.id,
                    folder,
                });
            }
        }
        out
    }

    /// The identity of [`unified_folder_refs`](Self::unified_folder_refs),
    /// for telling whether a rule change moved anything in or out.
    pub(super) fn unified_folder_keys(&self) -> Vec<(u32, u32)> {
        self.unified_folder_refs()
            .into_iter()
            .map(|r| (r.account_id, r.folder.id))
            .collect()
    }

    pub(super) fn folder_unread_of(&self, f: &Folder) -> u32 {
        self.folder_unread
            .get(&(f.account_id, f.id))
            .copied()
            .unwrap_or(0)
    }

    /// An account's unread total over its [`counted_folders`](Self::counted_folders).
    fn counted_unread(&self, account_id: u32) -> u32 {
        self.counted_folders(account_id)
            .iter()
            .map(|f| self.folder_unread_of(f))
            .sum()
    }

    /// The number behind the tray icon's dot and menu and the Background
    /// Apps status: every account's counted unread mail.
    pub(super) fn unified_unread(&self) -> u32 {
        self.accounts
            .iter()
            .map(|a| self.counted_unread(a.id))
            .sum()
    }

    /// The number behind the tray icon's dot and its menu: every
    /// account's inbox alone, whether or not the account is in the unified
    /// section (#267). A filter's destination has its own row (under
    /// Filters, and in its account), with its own chip, so it is not
    /// counted here as well.
    pub(super) fn inboxes_unread(&self) -> u32 {
        self.accounts
            .iter()
            .filter_map(|a| self.inbox_of(a.id))
            .map(|f| self.folder_unread_of(f))
            .sum()
    }

    /// The number behind the unified Inboxes chip: the inboxes that view
    /// merges, which leaves out the accounts kept apart from it (#267).
    pub(super) fn unified_inboxes_unread(&self) -> u32 {
        self.accounts
            .iter()
            .filter(|a| self.in_unified(a.id))
            .filter_map(|a| self.inbox_of(a.id))
            .map(|f| self.folder_unread_of(f))
            .sum()
    }

    /// Whether an account's mail is merged into the unified section (#267).
    /// An account with no config (never the case outside a test) is in.
    pub(super) fn in_unified(&self, account_id: u32) -> bool {
        self.account_config(account_id).is_none_or(|c| c.in_unified)
    }

    /// Counts set aside while marks or moves were on their way (#333, #336):
    /// with those stored, sync each such folder that is clear now. A count
    /// was dropped because it could be stale, but it may also have been the
    /// only sign of new mail: the open folder's IDLE had already reported
    /// and the watcher only speaks when the count moves again.
    pub(super) fn resync_dropped_unread(&mut self, account_id: u32) {
        let ready: Vec<u32> = self
            .dropped_unread
            .iter()
            .filter(|(a, _)| *a == account_id)
            .map(|(_, f)| *f)
            .filter(|f| {
                !self.pending_seen_in_folder(account_id, *f)
                    && !self
                        .folder_path(account_id, *f)
                        .is_some_and(|p| self.pending_moves_in(account_id, &p))
            })
            .collect();
        if ready.is_empty() {
            return;
        }
        for folder_id in &ready {
            self.dropped_unread.remove(&(account_id, *folder_id));
            self.sync_background_folder(account_id, *folder_id);
        }
        self.send_to(account_id, MailRequest::RefreshUnread);
    }

    /// A watcher or sweep reported a changed unread count for `folder_id`.
    /// When that is one of the account's counted folders (its inbox, or a
    /// folder a counting filter files into) and the worker's IDLE sits
    /// elsewhere, the folder's message list used to refresh only when it
    /// was next opened: the tray menu said "No unread mail" under a count
    /// that said otherwise, and mail arriving in the inbox raised no
    /// notification, since both read the list. Ask for a quiet resync so
    /// the list follows the count. An open inbox (alone or as All Inboxes)
    /// or an open folder is the IDLE folder, whose own resync covers it.
    pub(super) fn sync_background_folder(&self, account_id: u32, folder_id: u32) {
        let Some(folder) = self
            .counted_folders(account_id)
            .into_iter()
            .find(|f| f.id == folder_id)
        else {
            return;
        };
        // A folder that is open — on its own or as part of a unified view —
        // is not skipped (#170). It used to be, on the assumption that the
        // worker's IDLE on it would deliver the new list; but only push
        // accounts IDLE, and a polled account's Inboxes slice then waited a
        // whole poll interval for mail its unread chip already counted.
        // When IDLE did deliver it, this re-fetch of the first page comes
        // back identical and changes nothing (the Messages handler skips an
        // unchanged list).
        let path = folder.path.clone();
        self.send_to(account_id, MailRequest::SyncFolder { folder_id, path });
    }

    /// Index an account's sent mail without waiting for its folder to be
    /// opened (#236). A conversation pulls the user's own replies in from
    /// the cache, and the cache only held a folder once that folder had been
    /// shown: a reply sent from another client stayed out of every
    /// conversation until Sent was visited by hand. Asked once per account
    /// per run, as soon as its folders are known; the worker lists the first
    /// page and indexes the rest behind it. Both the folder the server files
    /// sent mail in and the one Hylki copies to, when they differ.
    pub(super) fn index_sent_folders(&mut self, account_id: u32) {
        if self.sent_index_requested.contains(&account_id) {
            return;
        }
        let mut targets: Vec<(u32, String)> = self
            .folder_of_kind(account_id, FolderKind::Sent)
            .map(|f| (f.id, f.path.clone()))
            .into_iter()
            .collect();
        if let Some(path) = self.sent_copy_path(account_id) {
            let copy = self
                .folder_by_path(account_id, &path)
                .map(|f| (f.id, f.path.clone()));
            if let Some(copy) = copy {
                if !targets.iter().any(|(id, _)| *id == copy.0) {
                    targets.push(copy);
                }
            }
        }
        // No sent folder listed yet: ask again when the next folder list
        // arrives rather than never.
        if targets.is_empty() {
            return;
        }
        self.sent_index_requested.insert(account_id);
        for (folder_id, path) in targets {
            self.send_to(account_id, MailRequest::SyncFolder { folder_id, path });
        }
    }

    /// Counted folders (#116) whose lists have never been loaded this run:
    /// fetch them quietly, so the tray menu can show their unread mail
    /// without waiting for their counts to move.
    pub(super) fn sync_unloaded_counted_folders(&self) {
        for a in &self.accounts {
            for f in self.counted_folders(a.id) {
                if !self.message_cache.contains_key(&(a.id, f.id)) {
                    self.send_to(
                        a.id,
                        MailRequest::SyncFolder {
                            folder_id: f.id,
                            path: f.path.clone(),
                        },
                    );
                }
            }
        }
    }
}
