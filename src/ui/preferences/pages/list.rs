//! Settings → Message List: avatars, layout, preview lines, swipes and
//! the single-line columns.

use adw::prelude::*;

use super::super::{PrefInput, PrefOutput, Preferences, ViewSender};
use crate::i18n::i18n;

/// The page, and the rows on it that the window reaches later.
#[derive(Debug)]
pub(in crate::ui::preferences) struct ListPage {
    pub(in crate::ui::preferences) page: adw::PreferencesPage,
    pub(in crate::ui::preferences) avatars_row: adw::SwitchRow,
    pub(in crate::ui::preferences) own_mailbox_face_row: adw::SwitchRow,
    pub(in crate::ui::preferences) list_layout_row: adw::ComboRow,
    pub(in crate::ui::preferences) list_follows_window_row: adw::SwitchRow,
    pub(in crate::ui::preferences) preview_lines_row: adw::ComboRow,
    pub(in crate::ui::preferences) group_by_date_row: adw::SwitchRow,
    pub(in crate::ui::preferences) pills_below_row: adw::SwitchRow,
    pub(in crate::ui::preferences) list_search_open_row: adw::SwitchRow,
    pub(in crate::ui::preferences) list_search_place_row: adw::ComboRow,
    pub(in crate::ui::preferences) list_bulk_place_row: adw::ComboRow,
    pub(in crate::ui::preferences) tag_row_tint_row: adw::SwitchRow,
    pub(in crate::ui::preferences) swipe_enabled_row: adw::SwitchRow,
    pub(in crate::ui::preferences) swipe_reversed_row: adw::SwitchRow,
    pub(in crate::ui::preferences) swipe_sensitivity_row: adw::SpinRow,
    pub(in crate::ui::preferences) columns_group: adw::PreferencesGroup,
    pub(in crate::ui::preferences) list_headings_row: adw::SwitchRow,
    pub(in crate::ui::preferences) list_fit_row: adw::SwitchRow,
    pub(in crate::ui::preferences) column_editor_box: gtk::Box,
}

impl ListPage {
    pub(in crate::ui::preferences) fn build(model: &Preferences, sender: &ViewSender) -> Self {
        relm4::view! {
            page = adw::PreferencesPage {
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Message List"),

                    #[name = "avatars_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Show sender avatars"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleAvatars(row.is_active()));
                        },
                    },

                    // Your own mail (#189): its mailbox's
                    // face, or the circle any sender gets.
                    #[name = "own_mailbox_face_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_sensitive: model.avatars,
                        set_title: &i18n("Sent mail uses your account circle"),
                        set_subtitle: &i18n("The account's picture or emoji, as in the sidebar."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetOwnMailboxFace(row.is_active()));
                        },
                    },

                    #[name = "list_layout_row"]
                    adw::ComboRow {
                        set_title: &i18n("Layout"),
                        set_subtitle: &i18n("Automatic uses single lines in wide windows."),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeListLayout(row.selected()));
                        },
                    },

                    // The list's width, or its share of the window,
                    // as the window resizes (#648).
                    #[name = "list_follows_window_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Resize the list with the window"),
                        set_subtitle: &i18n("Off, it keeps its width and the reader resizes."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetListFollowsWindow(row.is_active()));
                        },
                    },

                    #[name = "preview_lines_row"]
                    adw::ComboRow {
                        set_title: &i18n("Preview lines"),
                        set_subtitle: &i18n("Off also stops downloading previews."),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangePreviewLines(row.selected()));
                        },
                    },

                    #[name = "group_by_date_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Group by date"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetGroupByDate(row.is_active()));
                        },
                    },

                    #[name = "pills_below_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Tags and labels under the text"),
                        set_subtitle: &i18n("Gives the subject its full width."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetPillsBelow(row.is_active()));
                        },
                    },

                    #[name = "list_search_open_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Keep the search field open"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetListSearchOpen(row.is_active()));
                        },
                    },

                    #[name = "list_search_place_row"]
                    adw::ComboRow {
                        set_title: &i18n("Search field position"),
                        connect_selected_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetListSearchBottom(row.selected() == 1));
                        },
                    },

                    #[name = "list_bulk_place_row"]
                    adw::ComboRow {
                        set_title: &i18n("Selection bar position"),
                        connect_selected_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetListBulkBarBottom(row.selected() == 1));
                        },
                    },

                    #[name = "tag_row_tint_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Tint tagged messages"),
                        set_subtitle: &i18n("In the color of its first tag."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetTagRowTint(row.is_active()));
                        },
                    },

                    #[name = "swipe_enabled_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Swipe actions"),
                        set_subtitle: &i18n("Left deletes, right archives. Drag with the mouse or swipe with two fingers."),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleSwipeEnabled(row.is_active()));
                        },
                    },

                    #[name = "swipe_reversed_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_sensitive: model.swipe_enabled,
                        set_title: &i18n("Reverse swipe directions"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetSwipeReversed(row.is_active()));
                        },
                    },

                    #[name = "swipe_sensitivity_row"]
                    adw::SpinRow {
                        // Kept current by update_view.
                        set_sensitive: model.swipe_enabled,
                        set_title: &i18n("Trackpad swipe sensitivity"),
                        set_subtitle: &i18n("Raise it if swipes stop short. Mouse drags are unaffected."),
                        connect_value_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetSwipeSensitivity(row.value()));
                        },
                    },

                },

                // The single-line list's columns (#334): two
                // drop zones of draggable chips, filled in init.
                #[name = "columns_group"]
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Columns"),
                    set_description: Some(
                        &i18n("For the single-line layout. Drag columns to reorder them or out to remove them. Due Date shows only for Microsoft 365 mail."),
                    ),
                    // Kept current by update_view.
                    set_sensitive: model.list_single_possible,

                    #[name = "list_headings_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Show column headings"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetListHeadings(row.is_active()));
                        },
                    },

                    #[name = "list_fit_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Fit columns to the list"),
                        set_subtitle: &i18n("Off, a narrow list scrolls sideways."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetListColumnScroll(!row.is_active()));
                        },
                    },

                    #[name = "column_editor_box"]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 12,
                        // Clear of the switch's card above.
                        set_margin_top: 12,

                        gtk::Button {
                            set_label: &i18n("Restore Defaults"),
                            set_halign: gtk::Align::End,
                            set_valign: gtk::Align::Center,
                            connect_clicked => PrefInput::ColumnRestore,
                        },
                    },
                },
            }
        }
        Self {
            page,
            avatars_row,
            own_mailbox_face_row,
            list_layout_row,
            list_follows_window_row,
            preview_lines_row,
            group_by_date_row,
            pills_below_row,
            list_search_open_row,
            list_search_place_row,
            list_bulk_place_row,
            tag_row_tint_row,
            swipe_enabled_row,
            swipe_reversed_row,
            swipe_sensitivity_row,
            columns_group,
            list_headings_row,
            list_fit_row,
            column_editor_box,
        }
    }

    /// The view's `#[watch]` setters for this page, in the order the
    /// view had them (see the module comment of `pages`).
    pub(in crate::ui::preferences) fn update_view(&self, model: &Preferences) {
        self.own_mailbox_face_row.set_sensitive(model.avatars);
        self.swipe_reversed_row.set_sensitive(model.swipe_enabled);
        self.swipe_sensitivity_row
            .set_sensitive(model.swipe_enabled);
        self.columns_group.set_sensitive(model.list_single_possible);
    }
}
