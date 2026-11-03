//! Settings → Sidebar: the unified rows, the sections and the icon
//! rail.

use adw::prelude::*;

use super::super::{PrefInput, PrefOutput, Preferences, ViewSender};
use crate::i18n::i18n;

/// The page, and the rows on it that the window reaches later.
#[derive(Debug)]
pub(in crate::ui::preferences) struct SidebarPage {
    pub(in crate::ui::preferences) page: adw::PreferencesPage,
    pub(in crate::ui::preferences) start_view_row: adw::ComboRow,
    pub(in crate::ui::preferences) show_accounts_row: adw::SwitchRow,
    pub(in crate::ui::preferences) folder_sort_row: adw::ComboRow,
    pub(in crate::ui::preferences) chevron_side_row: adw::ComboRow,
    pub(in crate::ui::preferences) account_click_row: adw::SwitchRow,
    pub(in crate::ui::preferences) show_attachments_row: adw::SwitchRow,
    pub(in crate::ui::preferences) sidebar_hover_expand_row: adw::SwitchRow,
    pub(in crate::ui::preferences) remember_sidebar_row: adw::SwitchRow,
    pub(in crate::ui::preferences) remember_rail_row: adw::SwitchRow,
    pub(in crate::ui::preferences) seen_counts_row: adw::SwitchRow,
    pub(in crate::ui::preferences) show_unified_row: adw::SwitchRow,
    pub(in crate::ui::preferences) unified_starred_row: adw::SwitchRow,
    pub(in crate::ui::preferences) unified_sent_row: adw::SwitchRow,
    pub(in crate::ui::preferences) unified_drafts_row: adw::SwitchRow,
    pub(in crate::ui::preferences) unified_archive_row: adw::SwitchRow,
    pub(in crate::ui::preferences) unified_junk_row: adw::SwitchRow,
    pub(in crate::ui::preferences) unified_trash_row: adw::SwitchRow,
    pub(in crate::ui::preferences) unified_snoozed_row: adw::SwitchRow,
    pub(in crate::ui::preferences) unified_chips_row: adw::ExpanderRow,
    pub(in crate::ui::preferences) unified_chip_all_inboxes_row: adw::SwitchRow,
    pub(in crate::ui::preferences) unified_chip_starred_row: adw::SwitchRow,
    pub(in crate::ui::preferences) unified_chip_drafts_row: adw::SwitchRow,
    pub(in crate::ui::preferences) unified_chip_archive_row: adw::SwitchRow,
    pub(in crate::ui::preferences) unified_chip_junk_row: adw::SwitchRow,
    pub(in crate::ui::preferences) unified_chip_trash_row: adw::SwitchRow,
    pub(in crate::ui::preferences) unified_chip_snoozed_row: adw::SwitchRow,
    pub(in crate::ui::preferences) section_style_row: adw::ComboRow,
    pub(in crate::ui::preferences) filtered_placement_row: adw::ComboRow,
    pub(in crate::ui::preferences) tags_placement_row: adw::ComboRow,
    pub(in crate::ui::preferences) favorites_placement_row: adw::ComboRow,
    pub(in crate::ui::preferences) contacts_placement_row: adw::ComboRow,
    pub(in crate::ui::preferences) rail_dots_row: adw::SwitchRow,
    pub(in crate::ui::preferences) rail_fold_row: adw::ExpanderRow,
    pub(in crate::ui::preferences) rail_fold_accounts_row: adw::SwitchRow,
    pub(in crate::ui::preferences) rail_fold_all_inboxes_row: adw::SwitchRow,
    pub(in crate::ui::preferences) rail_fold_starred_row: adw::SwitchRow,
    pub(in crate::ui::preferences) rail_fold_sent_row: adw::SwitchRow,
    pub(in crate::ui::preferences) rail_fold_drafts_row: adw::SwitchRow,
    pub(in crate::ui::preferences) rail_fold_archive_row: adw::SwitchRow,
    pub(in crate::ui::preferences) rail_fold_junk_row: adw::SwitchRow,
    pub(in crate::ui::preferences) rail_fold_trash_row: adw::SwitchRow,
    pub(in crate::ui::preferences) rail_fold_snoozed_row: adw::SwitchRow,
    pub(in crate::ui::preferences) rail_fold_filtered_row: adw::SwitchRow,
    pub(in crate::ui::preferences) rail_fold_tags_row: adw::SwitchRow,
}

impl SidebarPage {
    pub(in crate::ui::preferences) fn build(model: &Preferences, sender: &ViewSender) -> Self {
        relm4::view! {
            page = adw::PreferencesPage {
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Sidebar"),

                    #[name = "start_view_row"]
                    adw::ComboRow {
                        set_title: &i18n("Open at startup"),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeStartView(row.selected()));
                        },
                    },

                    #[name = "show_accounts_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_active: model.show_accounts,
                        set_title: &i18n("Accounts in the sidebar"),
                        set_subtitle: &i18n("Off leaves only the unified section. Toggle with Ctrl+Shift+A or the main menu."),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleShowAccounts(row.is_active()));
                        },
                    },

                    #[name = "folder_sort_row"]
                    adw::ComboRow {
                        set_title: &i18n("Folder order"),
                        set_subtitle: &i18n("Dragging a folder switches to Custom Order."),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeFolderSort(row.selected()));
                        },
                    },

                    #[name = "chevron_side_row"]
                    adw::ComboRow {
                        set_title: &i18n("Chevron placement"),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeChevronSide(row.selected()));
                        },
                    },

                    #[name = "account_click_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Fold accounts with one click"),
                        set_subtitle: &i18n("When off, only its chevron or a double click activates the fold."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetAccountClickFolds(row.is_active()));
                        },
                    },

                    #[name = "show_attachments_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Show Attachments Browser"),
                        set_subtitle: &i18n("Browse every account's attachments in one place."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetAttachmentsRow(row.is_active()));
                        },
                    },

                    #[name = "sidebar_hover_expand_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Expand the sidebar on hover"),
                        set_subtitle: &i18n("Hovering the icon rail floats the full sidebar out."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetSidebarHoverExpand(row.is_active()));
                        },
                    },

                    #[name = "remember_sidebar_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Remember the sidebar layout"),
                        set_subtitle: &i18n("Off starts every launch folded up."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetRememberSidebar(row.is_active()));
                        },
                    },

                    #[name = "remember_rail_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Remember icon rail state"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetRememberRail(row.is_active()));
                        },
                    },

                    #[name = "seen_counts_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Highlight only new unread mail"),
                        set_subtitle: &i18n("A count turns grey once you have looked at the folder."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetSeenCounts(row.is_active()));
                        },
                    },
                },

                add = &adw::PreferencesGroup {
                    set_title: &i18n("Unified"),
                    set_description: Some(
                        &i18n("Shown only with more than one account."),
                    ),

                    #[name = "show_unified_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Inboxes"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleShowUnified(row.is_active()));
                        },
                    },

                    #[name = "unified_starred_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Starred"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleUnifiedStarred(row.is_active()));
                        },
                    },

                    #[name = "unified_sent_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Sent"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleUnifiedSent(row.is_active()));
                        },
                    },

                    #[name = "unified_drafts_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Drafts"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleUnifiedDrafts(row.is_active()));
                        },
                    },

                    #[name = "unified_archive_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Archive"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleUnifiedArchive(row.is_active()));
                        },
                    },

                    #[name = "unified_junk_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Junk"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleUnifiedJunk(row.is_active()));
                        },
                    },

                    #[name = "unified_trash_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Trash"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleUnifiedTrash(row.is_active()));
                        },
                    },

                    #[name = "unified_snoozed_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Snoozed"),
                        set_subtitle: &i18n("Shown only while mail is snoozed."),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleUnifiedSnoozed(row.is_active()));
                        },
                    },

                    #[name = "unified_chips_row"]
                    adw::ExpanderRow {
                        set_title: &i18n("Unread counts"),
                        set_subtitle: &i18n("Shown on the unified rows while they are folded up."),
                        set_expanded: false,

                        #[name = "unified_chip_all_inboxes_row"]
                        add_row = &adw::SwitchRow {
                            set_title: &i18n("Inboxes"),
                            connect_active_notify[sender] => move |row| {
                                sender.input(PrefInput::ToggleUnifiedChipAllInboxes(row.is_active()));
                            },
                        },
                        #[name = "unified_chip_starred_row"]
                        add_row = &adw::SwitchRow {
                            set_title: &i18n("Starred"),
                            connect_active_notify[sender] => move |row| {
                                sender.input(PrefInput::ToggleUnifiedChipStarred(row.is_active()));
                            },
                        },
                        #[name = "unified_chip_drafts_row"]
                        add_row = &adw::SwitchRow {
                            set_title: &i18n("Drafts"),
                            connect_active_notify[sender] => move |row| {
                                sender.input(PrefInput::ToggleUnifiedChipDrafts(row.is_active()));
                            },
                        },
                        #[name = "unified_chip_archive_row"]
                        add_row = &adw::SwitchRow {
                            set_title: &i18n("Archive"),
                            connect_active_notify[sender] => move |row| {
                                sender.input(PrefInput::ToggleUnifiedChipArchive(row.is_active()));
                            },
                        },
                        #[name = "unified_chip_junk_row"]
                        add_row = &adw::SwitchRow {
                            set_title: &i18n("Junk"),
                            connect_active_notify[sender] => move |row| {
                                sender.input(PrefInput::ToggleUnifiedChipJunk(row.is_active()));
                            },
                        },
                        #[name = "unified_chip_trash_row"]
                        add_row = &adw::SwitchRow {
                            set_title: &i18n("Trash"),
                            connect_active_notify[sender] => move |row| {
                                sender.input(PrefInput::ToggleUnifiedChipTrash(row.is_active()));
                            },
                        },
                        #[name = "unified_chip_snoozed_row"]
                        add_row = &adw::SwitchRow {
                            set_title: &i18n("Snoozed"),
                            connect_active_notify[sender] => move |row| {
                                sender.input(PrefInput::ToggleUnifiedChipSnoozed(row.is_active()));
                            },
                        },
                    },
                },

                add = &adw::PreferencesGroup {
                    set_title: &i18n("Sections"),

                    #[name = "section_style_row"]
                    adw::ComboRow {
                        set_title: &i18n("Section style"),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeSectionStyle(row.selected()));
                        },
                    },

                    #[name = "filtered_placement_row"]
                    adw::ComboRow {
                        set_title: &i18n("Filters placement"),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeFilteredPlacement(row.selected()));
                        },
                    },

                    #[name = "tags_placement_row"]
                    adw::ComboRow {
                        set_title: &i18n("Tags placement"),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeTagsPlacement(row.selected()));
                        },
                    },

                    #[name = "favorites_placement_row"]
                    adw::ComboRow {
                        set_title: &i18n("Favorites placement"),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeFavoritesPlacement(row.selected()));
                        },
                    },

                    #[name = "contacts_placement_row"]
                    adw::ComboRow {
                        set_title: &i18n("Contacts placement"),
                        set_subtitle: &i18n("'In the footer' disables expand/collapse."),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeContactsPlacement(row.selected()));
                        },
                    },
                },

                add = &adw::PreferencesGroup {
                    set_title: &i18n("Icon rail"),

                    #[name = "rail_dots_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Unread dots instead of counts"),
                        set_subtitle: &i18n("The count stays in the tooltip."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetRailDots(row.is_active()));
                        },
                    },

                    #[name = "rail_fold_row"]
                    adw::ExpanderRow {
                        set_title: &i18n("Fold up expanded items"),
                        set_subtitle: &i18n("The items below start folded up in the rail. Long-press to open one."),
                        set_show_enable_switch: true,
                        set_expanded: false,
                        connect_enable_expansion_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleRailFoldEnabled(row.enables_expansion()));
                        },

                        #[name = "rail_fold_accounts_row"]
                        add_row = &adw::SwitchRow {
                            set_title: &i18n("All accounts"),
                            connect_active_notify[sender] => move |row| {
                                sender.input(PrefInput::ToggleRailFoldAccounts(row.is_active()));
                            },
                        },
                        #[name = "rail_fold_all_inboxes_row"]
                        add_row = &adw::SwitchRow {
                            set_title: &i18n("Inboxes"),
                            connect_active_notify[sender] => move |row| {
                                sender.input(PrefInput::ToggleRailFoldAllInboxes(row.is_active()));
                            },
                        },
                        #[name = "rail_fold_starred_row"]
                        add_row = &adw::SwitchRow {
                            set_title: &i18n("Starred"),
                            connect_active_notify[sender] => move |row| {
                                sender.input(PrefInput::ToggleRailFoldStarred(row.is_active()));
                            },
                        },
                        #[name = "rail_fold_sent_row"]
                        add_row = &adw::SwitchRow {
                            set_title: &i18n("Sent"),
                            connect_active_notify[sender] => move |row| {
                                sender.input(PrefInput::ToggleRailFoldSent(row.is_active()));
                            },
                        },
                        #[name = "rail_fold_drafts_row"]
                        add_row = &adw::SwitchRow {
                            set_title: &i18n("Drafts"),
                            connect_active_notify[sender] => move |row| {
                                sender.input(PrefInput::ToggleRailFoldDrafts(row.is_active()));
                            },
                        },
                        #[name = "rail_fold_archive_row"]
                        add_row = &adw::SwitchRow {
                            set_title: &i18n("Archive"),
                            connect_active_notify[sender] => move |row| {
                                sender.input(PrefInput::ToggleRailFoldArchive(row.is_active()));
                            },
                        },
                        #[name = "rail_fold_junk_row"]
                        add_row = &adw::SwitchRow {
                            set_title: &i18n("Junk"),
                            connect_active_notify[sender] => move |row| {
                                sender.input(PrefInput::ToggleRailFoldJunk(row.is_active()));
                            },
                        },
                        #[name = "rail_fold_trash_row"]
                        add_row = &adw::SwitchRow {
                            set_title: &i18n("Trash"),
                            connect_active_notify[sender] => move |row| {
                                sender.input(PrefInput::ToggleRailFoldTrash(row.is_active()));
                            },
                        },
                        #[name = "rail_fold_snoozed_row"]
                        add_row = &adw::SwitchRow {
                            set_title: &i18n("Snoozed"),
                            connect_active_notify[sender] => move |row| {
                                sender.input(PrefInput::ToggleRailFoldSnoozed(row.is_active()));
                            },
                        },
                        #[name = "rail_fold_filtered_row"]
                        add_row = &adw::SwitchRow {
                            set_title: &i18n("Filters"),
                            connect_active_notify[sender] => move |row| {
                                sender.input(PrefInput::ToggleRailFoldFiltered(row.is_active()));
                            },
                        },
                        #[name = "rail_fold_tags_row"]
                        add_row = &adw::SwitchRow {
                            set_title: &i18n("Tags"),
                            connect_active_notify[sender] => move |row| {
                                sender.input(PrefInput::ToggleRailFoldTags(row.is_active()));
                            },
                        },
                    },
                },
            }
        }
        Self {
            page,
            start_view_row,
            show_accounts_row,
            folder_sort_row,
            chevron_side_row,
            account_click_row,
            show_attachments_row,
            sidebar_hover_expand_row,
            remember_sidebar_row,
            remember_rail_row,
            seen_counts_row,
            show_unified_row,
            unified_starred_row,
            unified_sent_row,
            unified_drafts_row,
            unified_archive_row,
            unified_junk_row,
            unified_trash_row,
            unified_snoozed_row,
            unified_chips_row,
            unified_chip_all_inboxes_row,
            unified_chip_starred_row,
            unified_chip_drafts_row,
            unified_chip_archive_row,
            unified_chip_junk_row,
            unified_chip_trash_row,
            unified_chip_snoozed_row,
            section_style_row,
            filtered_placement_row,
            tags_placement_row,
            favorites_placement_row,
            contacts_placement_row,
            rail_dots_row,
            rail_fold_row,
            rail_fold_accounts_row,
            rail_fold_all_inboxes_row,
            rail_fold_starred_row,
            rail_fold_sent_row,
            rail_fold_drafts_row,
            rail_fold_archive_row,
            rail_fold_junk_row,
            rail_fold_trash_row,
            rail_fold_snoozed_row,
            rail_fold_filtered_row,
            rail_fold_tags_row,
        }
    }

    /// The view's `#[watch]` setters for this page, in the order the
    /// view had them (see the module comment of `pages`).
    pub(in crate::ui::preferences) fn update_view(&self, model: &Preferences) {
        self.show_accounts_row.set_active(model.show_accounts);
    }
}
