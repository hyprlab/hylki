//! What the sidebar lists: accounts, their folders in the chosen order,
//! and the folder choices the account editor offers.

use relm4::prelude::*;

use crate::config::{self, AccountConfig};
use crate::models::Folder;
use crate::ui::message_list::MessageListInput;
use crate::ui::sidebar::{SectionData, SidebarInput};

use super::model::AppModel;

impl AppModel {
    /// The Special Folders choices per account email (#82): every folder of
    /// every configured account, for the settings editor's combos.
    pub(super) fn folder_choice_map(
        &self,
    ) -> std::collections::HashMap<String, Vec<(String, String)>> {
        let mut map = std::collections::HashMap::new();
        // The demo's accounts live only at the backend, so give the
        // Accounts panel's stand-in configs their folders as well.
        let cfgs: &[AccountConfig] = self.effective_config();
        for (i, cfg) in cfgs.iter().enumerate() {
            let id = i as u32 + 1;
            let list = self
                .folders
                .get(&id)
                .map(|fs| {
                    fs.iter()
                        .map(|f| (f.path.clone(), f.name.clone()))
                        .collect()
                })
                .unwrap_or_default();
            map.insert(cfg.email.clone(), list);
        }
        map
    }

    /// [`folder_choice_map`], but with `account_id`'s list taken from `fresh`
    /// (a SetFolders payload not yet stored on self).
    pub(super) fn folder_choice_map_with(
        &self,
        account_id: u32,
        fresh: &[Folder],
    ) -> std::collections::HashMap<String, Vec<(String, String)>> {
        let mut map = self.folder_choice_map();
        if let Some(cfg) = self.config.get(account_id as usize - 1) {
            map.insert(
                cfg.email.clone(),
                fresh
                    .iter()
                    .map(|f| (f.path.clone(), f.name.clone()))
                    .collect(),
            );
        }
        map
    }

    /// Push the current accounts + folders to the sidebar, in the user's chosen
    /// order and with each account's collapsed state.
    pub(super) fn rebuild_sidebar(&self) {
        // The accounts are being redrawn because something about them changed;
        // the faces their own mail wears follow from the same place (#189).
        self.refresh_own_faces();
        let order = self.ordered_emails();
        let sections: Vec<SectionData> = order
            .iter()
            .filter_map(|email| self.section_data(email))
            .collect();
        // Count enabled accounts from config, not just the workers that have
        // reported in: at startup the accounts stream in one by one, and
        // counting only the connected ones made the first sidebar build look
        // single-account — its default selection then landed on that account's
        // inbox (possibly inside a collapsed section, so nothing visibly
        // highlighted) instead of the "All Inboxes" the app should open with.
        let multi_account = self.multi_account();
        let show_unified = self.unified_inboxes_shown();
        // The other unified rows are as pointless with one account.
        let unified_kinds = if multi_account {
            self.unified_kinds_listed()
        } else {
            config::UnifiedKinds::NONE
        };
        self.snoozed_row.set(unified_kinds.snoozed);
        let unified_unread = self.unified_inboxes_unread();
        let unified_folders = self.unified_folder_refs();
        let favorite_folders = self.favorite_folder_refs();
        self.sidebars_emit(SidebarInput::SetContents {
            sections,
            show_unified,
            unified_kinds,
            show_accounts: self.show_accounts,
            unified_chips: self.unified_chips,
            chevrons_left: self.chevrons_left,
            account_click_folds: self.account_click_folds,
            rail_dots: self.rail_dots_now(),
            rail_fold: self.rail_fold,
            unified_unread,
            unified_folders,
            favorite_folders,
            tags: self.tags.clone(),
            section_style: self.section_style,
            filtered_placement: self.filtered_placement,
            tags_placement: self.tags_placement,
            favorites_placement: self.favorites_placement,
            contacts_placement: self.contacts_placement,
            smart_folders: self
                .smart_folders
                .iter()
                .map(|f| (f.id.clone(), f.name.clone()))
                .collect(),
        });

        self.sync_list_accounts();
    }

    /// What the sidebar draws for the account `email`: its folders with
    /// the server's unread counts, its look, its tree's folded nodes and
    /// order, and its place in the unified section.
    fn section_data(&self, email: &str) -> Option<SectionData> {
        let account = self.accounts.iter().find(|a| a.email == email)?.clone();
        // Use the server-side unread count (accurate beyond the loaded
        // window) for each folder's badge.
        let folders = self
            .folders
            .get(&account.id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|mut f| {
                if let Some(n) = self.folder_unread.get(&(account.id, f.id)) {
                    f.unread = *n;
                }
                f
            })
            .collect();
        let color = self.account_color(account.id);
        let emoji = self.account_emoji(account.id);
        let avatar = self.account_avatar(account.id);
        // This account's collapsed tree nodes, keyed "email\tpath".
        let prefix = format!("{email}\t");
        let tree_collapsed = self
            .tree_collapsed
            .iter()
            .filter_map(|k| k.strip_prefix(&prefix).map(String::from))
            .collect();
        let folder_order = self
            .folder_order
            .iter()
            .filter_map(|k| k.strip_prefix(&prefix).map(String::from))
            .collect();
        let folder_sort = self.folder_sort_of(account.id);
        let own_folder_sort = self.own_folder_sort(account.id);
        let hierarchy = self.drawn_hierarchy(account.id);
        let filtered = self.account_filtered_folders(account.id);
        let has_filters = self
            .filters
            .iter()
            .any(|r| r.account_email.eq_ignore_ascii_case(&account.email));
        let gmail = self.gmail_labels(account.id);
        Some(SectionData {
            gmail,
            in_unified: self.in_unified(account.id),
            has_filters,
            collapsed: self.collapsed.iter().any(|e| e == email),
            custom_expanded: self.folders_expanded.iter().any(|e| e == email),
            tags_expanded: self.tags_expanded_accounts.iter().any(|e| e == email),
            color,
            emoji,
            avatar,
            account,
            folders,
            filtered,
            tree_collapsed,
            folder_order,
            hierarchy,
            folder_sort,
            own_folder_sort,
            default_folder_sort: self.folder_sort,
        })
    }

    /// Keep the list's per-account tint colors in sync, and what its
    /// Account and Due columns need to know (#334).
    fn sync_list_accounts(&self) {
        let colors: std::collections::HashMap<u32, String> = self
            .accounts
            .iter()
            .map(|a| (a.id, self.account_color(a.id)))
            .collect();
        self.message_list
            .emit(MessageListInput::SetAccountColors(colors));
        // The column is narrow: the nickname alone, which an address would
        // only push out of view.
        let names: std::collections::HashMap<u32, String> = self
            .accounts
            .iter()
            .map(|a| {
                let nickname = self
                    .account_config(a.id)
                    .map(|c| c.display_label())
                    .unwrap_or_else(|| a.label.clone());
                (a.id, nickname)
            })
            .collect();
        let graph: std::collections::HashSet<u32> = self
            .accounts
            .iter()
            .filter(|a| {
                self.saved_account_config(a.id)
                    .is_some_and(|c| c.protocol == config::Protocol::Graph)
            })
            .map(|a| a.id)
            .collect();
        self.message_list
            .emit(MessageListInput::SetAccountNames { names, graph });
    }
}
