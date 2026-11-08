//! Applying sidebar settings.

use adw::prelude::*;
use relm4::prelude::*;

use crate::ui::contacts_page::ContactsPageInput;
use crate::ui::preferences::{PrefInput, PrefOutput};

use super::model::AppModel;

impl AppModel {
    /// The settings changes the router sends here (see `AppModel::route`).
    pub(super) fn on_prefs_sidebar_pref(&mut self, pref: PrefOutput) {
        match pref {
            PrefOutput::SetFolderSort(sort) => {
                if pref!(self.folder_sort = sort) {
                    self.rebuild_sidebar();
                }
            }

            PrefOutput::SetSidebarHoverExpand(on) => {
                pref!(self.sidebar_hover_expand = on);
            }

            PrefOutput::SetRememberSidebar(on) => {
                pref!(self.remember_sidebar = on);
            }

            PrefOutput::SetRememberRail(on) => {
                pref!(self.remember_rail = on);
            }

            PrefOutput::SetSeenCounts(on) => {
                if pref!(self.seen_counts = on) {
                    // What is unread now counts as looked at: only mail
                    // from here on is new.
                    self.unread_seen.borrow_mut().clear();
                    self.push_unread_counts();
                }
            }

            PrefOutput::SetRailDots(on) => {
                if pref!(self.rail_dots = on) {
                    self.rebuild_sidebar();
                }
            }

            PrefOutput::SetRailFold(fold) => {
                if pref!(self.rail_fold = fold) {
                    // The sidebar folds (or reopens) the sections concerned
                    // itself if the rail is up.
                    self.rebuild_sidebar();
                }
            }

            PrefOutput::SetShowUnified(show) => {
                if pref!(self.show_unified_pref = show) {
                    // Rebuilds the sidebar with or without the unified section.
                    self.rebuild_sidebar();
                }
            }

            PrefOutput::SetUnifiedChips(chips) => {
                if pref!(self.unified_chips = chips) {
                    self.rebuild_sidebar();
                }
            }

            PrefOutput::SetUnifiedKinds(kinds) => {
                if pref!(self.unified_kinds = kinds) {
                    self.rebuild_sidebar();
                }
            }

            PrefOutput::SetShowAccounts(show) => {
                if pref!(self.show_accounts = show) {
                    self.rebuild_sidebar();
                }
                // Both places that offer the switch stay in step.
                if self
                    .show_accounts_action
                    .state()
                    .and_then(|v| v.get::<bool>())
                    != Some(show)
                {
                    self.show_accounts_action.set_state(&show.to_variant());
                }
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::SetShowAccounts(show));
                }
            }

            PrefOutput::SetSectionStyle(style) => {
                if pref!(self.section_style = style) {
                    self.rebuild_sidebar();
                }
            }

            PrefOutput::SetFilteredPlacement(p) => {
                if pref!(self.filtered_placement = p) {
                    self.rebuild_sidebar();
                }
            }

            PrefOutput::SetTagsPlacement(p) => {
                if pref!(self.tags_placement = p) {
                    self.rebuild_sidebar();
                }
            }

            PrefOutput::SetFavoritesPlacement(p) => {
                if pref!(self.favorites_placement = p) {
                    self.rebuild_sidebar();
                }
            }

            PrefOutput::SetContactsPlacement(p) => {
                if pref!(self.contacts_placement = p) {
                    // Without its books under it in the sidebar, Contacts
                    // picks them itself.
                    self.contacts_page.emit(ContactsPageInput::SetBookPicker(
                        !self.contacts_placement_now().shown(),
                    ));
                    self.rebuild_sidebar();
                }
            }

            PrefOutput::SetStartView(view) => {
                pref!(self.start_view = view);
            }

            PrefOutput::SetChevronsLeft(left) => {
                if pref!(self.chevrons_left = left) {
                    self.rebuild_sidebar();
                }
            }

            PrefOutput::SetAccountClickFolds(on) => {
                if pref!(self.account_click_folds = on) {
                    self.rebuild_sidebar();
                }
            }
            _ => unreachable!("not routed to on_prefs_sidebar_pref"),
        }
    }
}
