//! Stars and read state set from another device, for the views the index
//! assembles (#532). A Smart Folder or the unified Starred view lists
//! folders that are not open, and a folder's own sync re-reads the flags
//! of its newest mail only: a star taken off older mail elsewhere stayed in
//! the view through every Refresh.

use std::time::{Duration, Instant};

use relm4::prelude::*;

use crate::models::FolderKind;
use crate::worker::MailRequest;

use super::model::AppModel;

/// How soon a folder's flags are checked again: opening a view and pressing
/// Refresh straight after need one check, not two.
const PAUSE: Duration = Duration::from_secs(15);

impl AppModel {
    /// The folders, per account, that the open Smart Folder or unified
    /// Starred view reads from the index.
    fn view_flag_paths(&self) -> Vec<(u32, Vec<String>)> {
        let all_but = |account_id: u32, skip: &[String]| -> Vec<String> {
            self.folders
                .get(&account_id)
                .into_iter()
                .flatten()
                .filter(|f| !skip.contains(&f.path))
                .map(|f| f.path.clone())
                .collect()
        };
        if let Some(folder) = self.open_smart_folder() {
            return self
                .smart_scope(folder)
                .into_iter()
                .map(|(id, include, exclude)| {
                    (id, include.unwrap_or_else(|| all_but(id, &exclude)))
                })
                .collect();
        }
        self.starred_by_flag()
            .into_iter()
            .map(|id| {
                let skip: Vec<String> = self
                    .folders
                    .get(&id)
                    .into_iter()
                    .flatten()
                    .filter(|f| matches!(f.kind, FolderKind::Trash | FolderKind::Junk))
                    .map(|f| f.path.clone())
                    .collect();
                (id, all_but(id, &skip))
            })
            .collect()
    }

    /// Have each account check the stars and read state the open view's
    /// folders hold in the index against the server, folders checked in the
    /// last few seconds aside. Answered per account with
    /// [`AppMsg::FlagsSynced`](super::msg::AppMsg::FlagsSynced).
    pub(super) fn sync_view_flags(&mut self) {
        let now = Instant::now();
        for (account_id, paths) in self.view_flag_paths() {
            let due: Vec<String> = paths
                .into_iter()
                .filter(|p| {
                    !self
                        .flag_sync_at
                        .get(&(account_id, p.clone()))
                        .is_some_and(|t| now.duration_since(*t) < PAUSE)
                })
                .collect();
            if due.is_empty() {
                continue;
            }
            for p in &due {
                self.flag_sync_at.insert((account_id, p.clone()), now);
            }
            self.send_to(account_id, MailRequest::RefreshFlags { paths: due });
        }
    }

    /// An account's flag check changed the index under `paths`: the views
    /// that read it read it again.
    pub(super) fn on_flags_synced(
        &mut self,
        account_id: u32,
        paths: &[String],
        sender: &ComponentSender<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        if self.smart_view.is_some() {
            self.refresh_smart_view(sender);
        }
        if self.flag_starred_accounts().contains(&account_id) {
            self.refresh_unified_starred(sender);
        }
        self.schedule_smart_counts(sender);
    }
}
