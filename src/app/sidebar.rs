//! The sidebar: its outputs, its header in rail mode, and the state it keeps
//! (collapsed accounts, folder order, sizes).

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::i18n::i18n_f;
use crate::models::{Account, Folder, FolderKind};

use super::demo::demo_mode;
use super::model::AppModel;
use super::msg::AppMsg;

/// Trim the sidebar header in the icon-only rail so it no longer forces a minimum
/// width wider than the rail: hide the (redundant) window-control buttons and tag
/// the header so its Compose/Menu buttons shrink to fit (see `.rail-header` in the
/// stylesheet). The reader pane's header still carries the window's close button,
/// so nothing becomes unreachable.
pub(super) fn set_sidebar_header_compact(
    header: &adw::HeaderBar,
    title: &gtk::Label,
    menu: &gtk::MenuButton,
    refresh: &gtk::Button,
    compact: bool,
) {
    header.set_show_start_title_buttons(!compact);
    header.set_show_end_title_buttons(!compact);
    // The menu is always in exactly one of three spots — packed end
    // (expanded), the title slot (rail), or packed start (peek) — and
    // HeaderBar::remove detaches it from any of them, so transitions are
    // free to start from whichever state is current. Refresh is either packed
    // start (expanded / peek) or unparented (rail, which stacks its own
    // refresh under the menu instead) — only remove it while it is a child.
    header.remove(menu);
    if refresh.parent().is_some() {
        header.remove(refresh);
    }
    menu.set_margin_start(0);
    // In the rail there is no title to show, so the menu button takes the title
    // slot — the only position a header bar centres — instead of hugging the
    // right edge of an 80px strip. Both widgets are held by the model, so the
    // one being displaced survives being unparented here.
    if compact {
        header.add_css_class("rail-header");
        header.set_title_widget(Some(menu));
    } else {
        header.remove_css_class("rail-header");
        header.set_title_widget(Some(title));
        header.pack_end(menu);
        header.pack_start(refresh);
    }
    title.set_visible(!compact);
}

/// How the sidebar layout (sidebar.toml: folder order, favorites, folded
/// tree nodes) names one account's folder: "email\tpath".
pub(super) fn folder_key(email: &str, path: &str) -> String {
    format!("{email}\t{path}")
}

impl AppModel {
    /// Persist the sidebar's per-account state (order, collapse, custom-folders
    /// expansion, and icon-only mode).
    pub(super) fn save_sidebar_state(&self) {
        config::save_sidebar_state(&config::SidebarState {
            order: self.account_order.clone(),
            collapsed: self.collapsed.clone(),
            folders_expanded: self.folders_expanded.clone(),
            icon_only: self.sidebar_collapsed,
            tree_collapsed: self.tree_collapsed.clone(),
            folder_order: self.folder_order.clone(),
            unified_expanded: self.unified_expanded,
            filtered_expanded: self.filtered_expanded,
            tags_expanded: self.tags_expanded,
            starred_expanded: self.starred_expanded,
            sent_expanded: self.sent_expanded,
            drafts_expanded: self.drafts_expanded,
            archive_expanded: self.archive_expanded,
            junk_expanded: self.junk_expanded,
            trash_expanded: self.trash_expanded,
            snoozed_expanded: self.snoozed_expanded,
            favorites: self.favorites.clone(),
            favorites_expanded: self.favorites_expanded,
            smart_expanded: self.smart_expanded,
            contacts_expanded: self.contacts_expanded,
            contacts_folded: self.contacts_folded.clone(),
            filtered_expanded_accounts: self.filtered_expanded_accounts.clone(),
            tags_expanded_accounts: self.tags_expanded_accounts.clone(),
        });
    }

    /// Keep a run of sibling folders in its new order. The run goes to the
    /// end of the list; only the order within a run counts, so the others
    /// stand as they were.
    fn keep_folder_order(&mut self, account_id: u32, paths: &[String]) {
        self.store_folder_order(account_id, paths);
        self.save_sidebar_state();
        self.rebuild_sidebar();
    }

    fn store_folder_order(&mut self, account_id: u32, paths: &[String]) {
        let Some(email) = self.email_of(account_id) else {
            return;
        };
        let keys: Vec<String> = paths.iter().map(|p| folder_key(&email, p)).collect();
        self.folder_order.retain(|k| !keys.contains(k));
        self.folder_order.extend(keys);
    }

    /// The folder order an account chose for itself, if any.
    pub(super) fn own_folder_sort(&self, account_id: u32) -> Option<config::FolderSort> {
        self.account_config(account_id).and_then(|c| c.folder_sort)
    }

    /// How an account's custom folders are sorted: its own choice, or
    /// Settings'.
    pub(super) fn folder_sort_of(&self, account_id: u32) -> config::FolderSort {
        self.own_folder_sort(account_id).unwrap_or(self.folder_sort)
    }

    /// An account's folders in the order the sidebar shows them.
    fn shown_folders(&self, account_id: u32) -> Vec<Folder> {
        let prefix = self
            .email_of(account_id)
            .map(|e| format!("{e}\t"))
            .unwrap_or_default();
        let order: Vec<String> = self
            .folder_order
            .iter()
            .filter_map(|k| k.strip_prefix(&prefix).map(String::from))
            .collect();
        crate::ui::sidebar::order_folders(
            self.folders.get(&account_id).cloned().unwrap_or_default(),
            &order,
            self.folder_sort_of(account_id),
        )
    }

    /// Give an account its own folder order (`None` follows Settings), in
    /// the file and in the Settings window's copy of the account, which
    /// would otherwise put the old one back when its editor is saved.
    fn set_account_folder_sort(&mut self, account_id: u32, sort: Option<config::FolderSort>) {
        let demo = self.config.is_empty() && demo_mode();
        let accounts = if demo {
            &mut self.demo_config
        } else {
            &mut self.config
        };
        let Some(cfg) = accounts.get_mut(account_id.saturating_sub(1) as usize) else {
            return;
        };
        if cfg.folder_sort == sort {
            return;
        }
        cfg.folder_sort = sort;
        let email = cfg.email.clone();
        let saved = if demo {
            config::save_demo_accounts(&self.demo_config).map_err(|e| e.to_string())
        } else {
            config::save(&self.config).map_err(|e| e.to_string())
        };
        if let Err(e) = saved {
            self.notice_error(i18n_f("Could not save account: {e}", &[("e", &e)]));
        }
        if let Some(acc) = &self.accounts_win {
            acc.emit(crate::ui::accounts::AccountsInput::SetFolderSort { email, sort });
        }
    }

    /// Dragging a custom folder is arranging them by hand, so an account
    /// sorted another way switches to Custom Order, every custom folder kept
    /// first where the old sort showed it: only the dragged one moves.
    fn adopt_custom_order(&mut self, account_id: u32) {
        if self.folder_sort_of(account_id) == config::FolderSort::Custom {
            return;
        }
        let shown: Vec<String> = self
            .shown_folders(account_id)
            .into_iter()
            .filter(|f| f.kind == FolderKind::Custom)
            .map(|f| f.path)
            .collect();
        self.store_folder_order(account_id, &shown);
        let sort =
            (self.folder_sort != config::FolderSort::Custom).then_some(config::FolderSort::Custom);
        self.set_account_folder_sort(account_id, sort);
    }

    /// Account emails in display order: those listed in `account_order` first
    /// (in that order), then any remaining accounts by id.
    pub(super) fn ordered_emails(&self) -> Vec<String> {
        let mut result: Vec<String> = Vec::new();
        for email in &self.account_order {
            if self.accounts.iter().any(|a| &a.email == email) && !result.contains(email) {
                result.push(email.clone());
            }
        }
        let mut rest: Vec<&Account> = self
            .accounts
            .iter()
            .filter(|a| !result.contains(&a.email))
            .collect();
        rest.sort_by_key(|a| a.id);
        for a in rest {
            result.push(a.email.clone());
        }
        result
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_sidebar(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::AccountFolded { id, folded } => {
                // The sidebar already animated it; just record the state (a
                // rebuild here would interrupt the animation).
                if let Some(email) = self.email_of(id) {
                    let was = self.collapsed.contains(&email);
                    if was != folded {
                        if folded {
                            self.collapsed.push(email);
                        } else {
                            self.collapsed.retain(|e| *e != email);
                        }
                        self.save_sidebar_state();
                    }
                }
            }

            AppMsg::ToggleCustomFolders(account_id) => {
                // The sidebar animated the toggle locally; record the new state
                // (the "folders_expanded" list holds accounts whose custom
                // folders are revealed; absence means hidden, the default).
                if let Some(email) = self.email_of(account_id) {
                    if let Some(pos) = self.folders_expanded.iter().position(|e| *e == email) {
                        self.folders_expanded.remove(pos);
                    } else {
                        self.folders_expanded.push(email);
                    }
                    self.save_sidebar_state();
                }
            }

            AppMsg::ReorderFolders { account_id, paths } => {
                let custom = self.folders.get(&account_id).is_some_and(|fs| {
                    fs.iter()
                        .any(|f| f.kind == FolderKind::Custom && paths.contains(&f.path))
                });
                if custom {
                    self.adopt_custom_order(account_id);
                }
                self.keep_folder_order(account_id, &paths);
            }

            AppMsg::PlaceFolder {
                account_id,
                path,
                dest,
                target,
                after,
            } => {
                self.adopt_custom_order(account_id);
                // Moved on the server first (the same move as a drop on a
                // folder, with its checks and its undo); only once that has
                // gone ahead does it take its place beside `target`.
                let Some(new_path) = self.move_folder(account_id, path, dest) else {
                    return;
                };
                let shown = self.shown_folders(account_id);
                let hierarchy = crate::ui::sidebar::Hierarchy {
                    delimiter: self.folder_delimiter(account_id),
                    namespace: self.folder_namespace(account_id),
                };
                if let Some(paths) =
                    crate::ui::sidebar::placed_run(&shown, &hierarchy, &new_path, &target, after)
                {
                    self.keep_folder_order(account_id, &paths);
                }
            }

            AppMsg::SetAccountFolderSort { account_id, sort } => {
                self.set_account_folder_sort(account_id, sort);
                self.rebuild_sidebar();
            }

            AppMsg::FolderNodeCollapsed {
                account_id,
                path,
                collapsed,
            } => {
                // The sidebar already reshaped its rows; just remember it.
                if let Some(email) = self
                    .accounts
                    .iter()
                    .find(|a| a.id == account_id)
                    .map(|a| a.email.clone())
                {
                    let key = folder_key(&email, &path);
                    if collapsed {
                        if !self.tree_collapsed.contains(&key) {
                            self.tree_collapsed.push(key);
                        }
                    } else {
                        self.tree_collapsed.retain(|k| *k != key);
                    }
                    self.save_sidebar_state();
                }
            }

            msg @ AppMsg::SidebarSectionsOpen { .. } => self.on_sidebar_sections_open(msg),

            AppMsg::AccountsReordered(emails) => {
                // Display order only (by email) — no reconnect needed.
                if !emails.is_empty() {
                    self.account_order = emails;
                    self.save_sidebar_state();
                    self.rebuild_sidebar();
                }
            }

            AppMsg::ToggleAccountTags(account_id) => {
                if let Some(email) = self.email_of(account_id) {
                    if let Some(pos) = self.tags_expanded_accounts.iter().position(|e| *e == email)
                    {
                        self.tags_expanded_accounts.remove(pos);
                    } else {
                        self.tags_expanded_accounts.push(email);
                    }
                    self.save_sidebar_state();
                }
            }
            _ => unreachable!("not routed to on_sidebar"),
        }
    }

    /// The sidebar's All Inboxes / Filtered Folders / Tags sections were
    /// opened or folded — record it with the layout.
    fn on_sidebar_sections_open(&mut self, msg: AppMsg) {
        let AppMsg::SidebarSectionsOpen {
            all_inboxes,
            filtered,
            tags,
            starred,
            sent,
            drafts,
            archive,
            junk,
            trash,
            snoozed,
            favorites,
            smart,
            contacts,
        } = msg
        else {
            unreachable!("only SidebarSectionsOpen comes here")
        };
        let now = [
            all_inboxes,
            filtered,
            tags,
            starred,
            sent,
            drafts,
            archive,
            junk,
            trash,
            snoozed,
            favorites,
            smart,
            contacts,
        ];
        let was = [
            self.unified_expanded,
            self.filtered_expanded,
            self.tags_expanded,
            self.starred_expanded,
            self.sent_expanded,
            self.drafts_expanded,
            self.archive_expanded,
            self.junk_expanded,
            self.trash_expanded,
            self.snoozed_expanded,
            self.favorites_expanded,
            self.smart_expanded,
            self.contacts_expanded,
        ];
        if now != was {
            self.unified_expanded = all_inboxes;
            self.filtered_expanded = filtered;
            self.tags_expanded = tags;
            self.starred_expanded = starred;
            self.sent_expanded = sent;
            self.drafts_expanded = drafts;
            self.archive_expanded = archive;
            self.junk_expanded = junk;
            self.trash_expanded = trash;
            self.snoozed_expanded = snoozed;
            self.favorites_expanded = favorites;
            self.smart_expanded = smart;
            self.contacts_expanded = contacts;
            self.save_sidebar_state();
        }
    }
}
