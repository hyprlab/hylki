//! Worker events about folders: listings, unread counts, and marks the
//! server has settled.

use std::collections::HashMap;

use relm4::prelude::*;

use crate::config;
use crate::models::Folder;
use crate::ui::notifications::NotifyInput;
use crate::worker::MailRequest;

use super::folders::{apply_folder_roles, merged_unread, same_folder_list};
use super::model::AppModel;
use super::msg::AppMsg;
use super::pending::{PENDING_MOVE_MAX, PENDING_SEEN_MAX, PENDING_TAG_SETTLED};

impl AppModel {
    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_events_folders(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::SetFolders {
                account_id,
                folders,
            } => self.on_set_folders(account_id, folders, sender),

            AppMsg::FolderUnread {
                account_id,
                folder_id,
                unread,
            } => {
                // A count fetched ahead of a read mark still in the worker's
                // queue: the app's own count (adjusted when the mark was
                // made) stands until the mark is stored, and the server is
                // asked again once it is (#333).
                if self.pending_seen_in_folder(account_id, folder_id) {
                    self.dropped_unread.insert((account_id, folder_id));
                    return;
                }
                // Nothing unread left where the notification points: it is
                // answered, wherever the mail was read (#333).
                if unread == 0
                    && crate::desktop::notify::posted_for(account_id)
                        .is_some_and(|(f, _)| f == folder_id)
                {
                    crate::desktop::notify::withdraw_mail(account_id);
                }
                // Likewise while mail is being taken out of the folder: each
                // move changes the server's count before the app hears the
                // move is done, and every change started another sync of the
                // folder, which put the mail back on the list (#255).
                if self
                    .folder_path(account_id, folder_id)
                    .is_some_and(|p| self.pending_moves_in(account_id, &p))
                {
                    self.dropped_unread.insert((account_id, folder_id));
                    return;
                }
                let prev = self.folder_unread.insert((account_id, folder_id), unread);
                if prev != Some(unread) {
                    self.sync_background_folder(account_id, folder_id);
                }
                self.push_unread_counts();
            }

            AppMsg::SeenSettled {
                account_id,
                path,
                uid,
            } => {
                self.pending_seen.remove(&(account_id, path, uid));
                self.pending_seen
                    .retain(|_, (_, at)| at.elapsed() < PENDING_SEEN_MAX);
                // Counts the server sent while the marks were on their way
                // were set aside; with the last one stored, ask for fresh
                // ones, or the badge keeps the app's guess until the next
                // change (#333).
                if !self.pending_seen.keys().any(|(a, _, _)| *a == account_id) {
                    self.resync_dropped_unread(account_id);
                }
            }

            AppMsg::Answered {
                account_id,
                path,
                uid,
                forwarded,
            } => {
                let fid = self.folder_by_path(account_id, &path).map(|f| f.id);
                if let Some(fid) = fid {
                    self.show_answered(account_id, fid, uid, forwarded, true);
                }
            }

            AppMsg::TagSettled {
                account_id,
                path,
                uid,
                keyword,
            } => {
                let soon = std::time::Instant::now() + PENDING_TAG_SETTLED;
                if let Some((_, until, _)) =
                    self.pending_tags
                        .get_mut(&(account_id, path, uid, keyword.to_lowercase()))
                {
                    *until = (*until).min(soon);
                }
            }

            AppMsg::MovesSettled {
                account_id,
                path,
                uids,
            } => {
                let mut pending = self.pending_moves.borrow_mut();
                for uid in uids {
                    pending.remove(&(account_id, path.clone(), uid));
                }
                pending.retain(|_, at| at.elapsed() < PENDING_MOVE_MAX);
                drop(pending);
                if !self.pending_moves_in(account_id, &path)
                    && !self.pending_seen_in(account_id, &path)
                {
                    self.resync_dropped_unread(account_id);
                }
            }

            AppMsg::FolderUnreadByPath {
                account_id,
                path,
                unread,
            } => {
                // Resolve against the current list; a path the app no longer
                // knows (folder deleted/renamed under a live watcher) is
                // dropped rather than guessed at.
                let id = self.folder_by_path(account_id, &path).map(|f| f.id);
                if self.pending_seen_in(account_id, &path)
                    || self.pending_moves_in(account_id, &path)
                {
                    if let Some(folder_id) = id {
                        self.dropped_unread.insert((account_id, folder_id));
                    }
                    return;
                }
                if let Some(folder_id) = id {
                    if unread == 0
                        && crate::desktop::notify::posted_for(account_id)
                            .is_some_and(|(f, _)| f == folder_id)
                    {
                        crate::desktop::notify::withdraw_mail(account_id);
                    }
                    let prev = self.folder_unread.insert((account_id, folder_id), unread);
                    if prev != Some(unread) {
                        self.sync_background_folder(account_id, folder_id);
                    }
                    self.push_unread_counts();
                }
            }

            AppMsg::BodyHits {
                account_id,
                folder_id,
                hits,
            } => {
                // The whole answer for the sync about to land; the previous
                // one described mail that sync lists again anyway.
                self.body_hits.insert((account_id, folder_id), hits);
            }
            _ => unreachable!("not routed to on_events_folders"),
        }
    }

    /// An account's folder list, from the worker's cache or the server.
    fn on_set_folders(
        &mut self,
        account_id: u32,
        folders: Vec<Folder>,
        sender: ComponentSender<Self>,
    ) {
        self.notifications.emit(NotifyInput::ClearConnectivity);
        // Defence in depth against a wedged session's empty LIST (the
        // worker filters these too): never trade a real folder list
        // for nothing.
        if folders.is_empty()
            && self
                .folders
                .get(&account_id)
                .is_some_and(|old| !old.is_empty())
        {
            return;
        }
        let mut folders = folders;
        self.prepare_folder_list(account_id, &mut folders);
        // Keep the settings editor's folder choices current while open.
        if let Some(acc) = &self.accounts_win {
            acc.emit(crate::ui::accounts::AccountsInput::SetFolderChoices(
                self.folder_choice_map_with(account_id, &folders),
            ));
        }
        self.merge_folder_unread(account_id, &folders);
        // An identical list — above all the refresh confirming an
        // optimistic folder move, whose local reshape mirrors the
        // worker exactly — changes nothing on screen: skip the rebuild
        // so the sidebar's rows don't vanish and reappear for it.
        let unchanged = self
            .folders
            .get(&account_id)
            .is_some_and(|old| same_folder_list(old, &folders));
        // Ids are list positions, so a folder another client added,
        // renamed or removed shifts them (a refresh re-lists the
        // folders). The open folder and the cached lists follow their
        // folder by path; a list whose folder is gone is dropped.
        // The first listing has nothing to follow from.
        if !unchanged
            && self
                .folders
                .get(&account_id)
                .is_some_and(|old| !old.is_empty())
        {
            self.follow_folder_ids(account_id, &folders);
        }
        self.folders.insert(account_id, folders);
        self.push_template_folders();
        // Snoozed mail (#527): the rows follow the folder ids, and an
        // account listed at last can have its mail back.
        self.push_snoozed();
        self.wake_due();
        if unchanged {
            self.push_unread_counts();
        } else {
            self.rebuild_sidebar();
        }
        self.request_unified_boot(account_id);
        self.schedule_preload(&sender);
        if self.flag_starred_accounts().contains(&account_id) {
            self.refresh_unified_starred(&sender);
        }
        self.index_sent_folders(account_id);
        // A unified view merges one folder per account, and this
        // account's folders may only now have arrived — so the set the
        // completeness flag is measured over just changed. It is
        // otherwise recomputed on `BackfillDone` alone, which meant an
        // account joining after every other one had finished left the
        // list claiming a complete index, or (once its own load
        // landed) spinning with no event left to clear it (#218).
        self.push_index_complete();
    }

    /// A new listing made ready to keep: Exchange's non-mail folders
    /// hidden the first time, hidden folders left out, manual roles laid
    /// over, special folders named in the app's language.
    fn prepare_folder_list(&mut self, account_id: u32, folders: &mut Vec<Folder>) {
        // The first listing of an account is looked over once for
        // Exchange's non-mail folders (#239), which are hidden the
        // way any folder is: the account editor lists them, and can
        // bring any of them back. Noted as done either way, so a
        // folder brought back stays back.
        let seed = self
            .config
            .get(account_id as usize - 1)
            .filter(|cfg| !cfg.folders_seeded)
            .map(|_| crate::models::exchange_non_mail_folders(folders));
        if let Some(seed) = seed {
            if let Some(cfg) = self.config.get_mut(account_id as usize - 1) {
                cfg.folders_seeded = true;
                if let Err(e) = config::save(&self.config) {
                    tracing::warn!("could not note the folder look-over: {e}");
                }
            }
            if !seed.is_empty() {
                tracing::info!("account {account_id}: hiding Exchange's non-mail folders {seed:?}");
                self.hide_folders(account_id, seed);
            }
        }
        // A listing from before a hide (the cache at startup, an
        // answer already on its way) still carries what is hidden.
        if let Some(cfg) = self.config.get(account_id as usize - 1) {
            if !cfg.hidden_folders.is_empty() {
                let hidden = cfg.hidden_folders.clone();
                folders.retain(|f| !crate::models::folder_is_hidden(&f.path, None, &hidden));
            }
        }
        // Manual special-folder assignments (#82) ride over whatever
        // the worker detected.
        if let Some(cfg) = self.config.get(account_id as usize - 1) {
            apply_folder_roles(&cfg.folder_roles.clone(), folders);
        }
        // After the look-over above, which goes by the English names.
        crate::models::localize_special_folder_names(folders);
    }

    /// The new list's unread counts, never trading a known count for a
    /// zero the listing did not measure (see `merged_unread`).
    fn merge_folder_unread(&mut self, account_id: u32, folders: &[Folder]) {
        let unread = merged_unread(
            account_id,
            self.folders.get(&account_id).map(Vec::as_slice),
            &self.folder_unread,
            folders,
        );
        self.folder_unread.retain(|(a, _), _| *a != account_id);
        for (folder_id, n) in unread {
            self.folder_unread.insert((account_id, folder_id), n);
        }
    }

    /// Ids are list positions: the open folder and the cached lists
    /// follow their folder by path to its new id; a list whose folder is
    /// gone is dropped.
    fn follow_folder_ids(&mut self, account_id: u32, folders: &[Folder]) {
        let old_paths: HashMap<u32, String> = self
            .folders
            .get(&account_id)
            .map(|old| old.iter().map(|f| (f.id, f.path.clone())).collect())
            .unwrap_or_default();
        let new_ids: HashMap<&str, u32> = folders.iter().map(|f| (f.path.as_str(), f.id)).collect();
        let cached = std::mem::take(&mut self.message_cache);
        for ((a, fid), mut list) in cached {
            if a != account_id {
                self.message_cache.insert((a, fid), list);
                continue;
            }
            let Some(&id) = old_paths.get(&fid).and_then(|p| new_ids.get(p.as_str())) else {
                continue;
            };
            for m in &mut list {
                m.folder_id = id;
            }
            self.message_cache.insert((a, id), list);
        }
        if let Some(sel) = self
            .selected
            .as_mut()
            .filter(|s| s.account_id == account_id)
        {
            if let Some(&id) = new_ids.get(sel.path.as_str()) {
                sel.folder_id = id;
            }
        }
    }

    /// Launch with "All Inboxes" already selected: UnifiedSelected
    /// fired before any folder list existed, so its inbox requests
    /// went nowhere and the list sat empty until the user visited a
    /// folder by hand. The first time an account's folders arrive
    /// while the unified view is up, ask for its inbox — the cache
    /// priming already painted its slice, and this is what brings
    /// in whatever changed since the app last ran.
    fn request_unified_boot(&mut self, account_id: u32) {
        if self.unified && self.unified_boot_requested.insert(account_id) {
            let mine: Vec<(u32, u32, String)> = self
                .unified_targets()
                .into_iter()
                .filter(|(a, _, _)| *a == account_id)
                .collect();
            for (_, folder_id, path) in mine {
                self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
            }
        }
    }
}
