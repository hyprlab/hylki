//! The unified Starred view for accounts that star by flag rather than by
//! folder.

use std::collections::HashMap;

use relm4::prelude::*;

use crate::config;
use crate::models::{FolderKind, Message};

use super::demo::demo_mode;
use super::model::AppModel;
use super::msg::AppMsg;
use super::smart::SMART_VIEW_LIMIT;
use super::types::UnifiedView;

impl AppModel {
    /// The accounts in the unified section that keep a star as a flag on
    /// each message, wherever it is filed, with no Starred folder (#482):
    /// most servers (Gmail and Proton Bridge have one). An account whose
    /// folders are not known yet waits for them: it may turn out to have one.
    pub(super) fn flag_starred_accounts(&self) -> Vec<u32> {
        if !self.unified_kinds_wanted().has(FolderKind::Starred) {
            return Vec::new();
        }
        self.accounts
            .iter()
            .filter(|a| self.in_unified(a.id))
            .filter(|a| self.folders.get(&a.id).is_some_and(|f| !f.is_empty()))
            .filter(|a| self.folder_of_kind(a.id, FolderKind::Starred).is_none())
            .map(|a| a.id)
            .collect()
    }

    /// Those of [`flag_starred_accounts`] the open unified Starred view
    /// lists, none when another view is open.
    pub(super) fn starred_by_flag(&self) -> Vec<u32> {
        if !self.unified || self.unified_view != UnifiedView::Kind(FolderKind::Starred) {
            return Vec::new();
        }
        let mut ids = self.flag_starred_accounts();
        ids.retain(|a| self.unified_account.is_none_or(|x| x == *a));
        ids
    }

    /// Read the starred mail of every account in [`flag_starred_accounts`]
    /// from the on-disk index off the main thread, every folder but Trash
    /// and Junk, answering with [`AppMsg::UnifiedStarredLoaded`]. It runs
    /// whether or not the view is open, so opening it lists the mail at once.
    pub(super) fn refresh_unified_starred(&mut self, sender: &ComponentSender<Self>) {
        let accounts = self.flag_starred_accounts();
        if accounts.is_empty() || demo_mode() && self.config.is_empty() {
            return;
        }
        if self.unified_starred_loading {
            self.unified_starred_dirty = true;
            return;
        }
        self.unified_starred_loading = true;
        let scope: Vec<(u32, Vec<String>)> = accounts
            .into_iter()
            .map(|id| {
                let skip = self.folders.get(&id).into_iter().flatten();
                let skip = skip.filter(|f| matches!(f.kind, FolderKind::Trash | FolderKind::Junk));
                (id, skip.map(|f| f.path.clone()).collect())
            })
            .collect();
        let s = sender.input_sender().clone();
        std::thread::spawn(move || {
            let t = std::time::Instant::now();
            let mut rows: HashMap<u32, Vec<(String, Message)>> = HashMap::new();
            if let Ok(cache) = crate::cache::Cache::open() {
                let starred = config::SmartPrefilter {
                    starred: Some(true),
                    ..Default::default()
                };
                for (account_id, exclude) in scope {
                    let found = cache.smart_scan(
                        account_id,
                        None,
                        &exclude,
                        starred,
                        SMART_VIEW_LIMIT,
                        |_| true,
                    );
                    rows.insert(account_id, found);
                }
            }
            tracing::debug!("unified starred: read the index in {:?}", t.elapsed());
            let _ = s.send(AppMsg::UnifiedStarredLoaded { rows });
        });
    }

    /// `refresh_unified_starred` a few seconds from now, once for however
    /// many folders sync meanwhile.
    pub(super) fn refresh_unified_starred_soon(&mut self, sender: &ComponentSender<Self>) {
        if std::mem::replace(&mut self.unified_starred_due, true) {
            return;
        }
        let s = sender.input_sender().clone();
        gtk::glib::timeout_add_seconds_local_once(5, move || {
            let _ = s.send(AppMsg::RefreshUnifiedStarred);
        });
    }

    /// Fill the open unified Starred view's slices from
    /// `unified_starred_rows`, under the folder each message sits in, so
    /// starring, moving and reading it go through that folder as anywhere
    /// else. Whether the slices changed.
    pub(super) fn fill_unified_starred(&mut self) -> bool {
        let flagged = self.starred_by_flag();
        if flagged.is_empty() {
            return false;
        }
        let mut found: Vec<Message> = Vec::new();
        for account_id in &flagged {
            let Some(folders) = self.folders.get(account_id) else {
                continue;
            };
            for (path, m) in self
                .unified_starred_rows
                .get(account_id)
                .into_iter()
                .flatten()
            {
                if !m.starred {
                    continue;
                }
                let Some(f) = folders.iter().find(|f| f.path == *path) else {
                    continue;
                };
                let mut m = m.clone();
                m.folder_id = f.id;
                // A folder open this session knows better what was read,
                // replied to or tagged since the index was read; the star
                // is this list's own, which starring keeps.
                if let Some(c) = self
                    .message_cache
                    .get(&(m.account_id, f.id))
                    .and_then(|ms| ms.iter().find(|c| c.uid == m.uid))
                {
                    m.unread = c.unread;
                    m.answered = c.answered;
                    m.forwarded = c.forwarded;
                    m.keywords = c.keywords.clone();
                    m.labels = c.labels.clone();
                }
                found.push(m);
            }
        }
        // One row for a message filed twice, the inbox copy where there is one.
        self.newest_once(&mut found);
        let mut slices: HashMap<(u32, u32), Vec<Message>> = HashMap::new();
        for m in found {
            slices
                .entry((m.account_id, m.folder_id))
                .or_default()
                .push(m);
        }
        let mut fresh: HashMap<(u32, u32), Vec<Message>> = HashMap::new();
        for ((account_id, folder_id), messages) in slices {
            let messages = self.apply_pending_seen(account_id, folder_id, messages);
            let messages = self.apply_pending_answers(account_id, folder_id, messages);
            let messages = self.apply_pending_tags(account_id, folder_id, messages);
            fresh.insert((account_id, folder_id), messages);
        }
        let held = self
            .unified_slices
            .iter()
            .filter(|((a, _), _)| flagged.contains(a));
        let same = held.clone().count() == fresh.len()
            && held.into_iter().all(|(k, v)| fresh.get(k) == Some(v));
        if same {
            return false;
        }
        self.unified_slices.retain(|(a, _), _| !flagged.contains(a));
        self.unified_slices.extend(fresh);
        true
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_unified_starred(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::RefreshUnifiedStarred => {
                self.unified_starred_due = false;
                self.refresh_unified_starred(&sender);
            }

            AppMsg::UnifiedStarredLoaded { rows } => {
                self.unified_starred_loading = false;
                self.unified_starred_rows.extend(rows);
                if self.fill_unified_starred() {
                    self.unified_dirty.set(true);
                    self.queue_unified_emit(&sender);
                }
                if std::mem::take(&mut self.unified_starred_dirty) {
                    self.refresh_unified_starred(&sender);
                }
            }
            _ => unreachable!("not routed to on_unified_starred"),
        }
    }
}
