//! Settings → Appearance: style, text size, the theme and app-icon
//! galleries and the reader toolbar.

use adw::prelude::*;

use super::super::{PrefInput, PrefOutput, ViewSender};
use crate::i18n::i18n;

/// The page, and the rows on it that the window reaches later.
#[derive(Debug)]
pub(in crate::ui::preferences) struct AppearancePage {
    pub(in crate::ui::preferences) page: adw::PreferencesPage,
    pub(in crate::ui::preferences) app_theme_row: adw::ComboRow,
    pub(in crate::ui::preferences) text_scale_row: adw::ComboRow,
    pub(in crate::ui::preferences) theme_row: adw::PreferencesRow,
    pub(in crate::ui::preferences) app_icon_row: adw::PreferencesRow,
    pub(in crate::ui::preferences) desktop_icons_row: adw::SwitchRow,
    pub(in crate::ui::preferences) settings_open_row: adw::ComboRow,
    pub(in crate::ui::preferences) toolbar_editor_box: gtk::Box,
    pub(in crate::ui::preferences) toolbar_fold_list: gtk::ListBox,
    pub(in crate::ui::preferences) toolbar_fold_row: adw::SwitchRow,
}

impl AppearancePage {
    pub(in crate::ui::preferences) fn build(sender: &ViewSender) -> Self {
        relm4::view! {
            page = adw::PreferencesPage {
                add = &adw::PreferencesGroup {
                    // Rendered as Pango markup — a bare "&" breaks it.
                    set_title: &i18n("Appearance"),

                    #[name = "app_theme_row"]
                    adw::ComboRow {
                        set_title: &i18n("Style"),
                        set_subtitle: &i18n("Messages have their own setting under Reading."),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeAppTheme(row.selected()));
                        },
                    },

                    #[name = "text_scale_row"]
                    adw::ComboRow {
                        set_title: &i18n("Text size"),
                        set_subtitle: &i18n("Adjusts the interface and message text size."),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeTextScale(row.selected()));
                        },
                    },

                    // The theme gallery; its content is built
                    // in init (a grid the view! macro can't
                    // declare), like the app-icon strip below.
                    #[name = "theme_row"]
                    adw::PreferencesRow {
                        set_title: &i18n("Theme"),
                        set_activatable: false,
                        set_focusable: false,
                    },

                    // The app-icon gallery; its content is built in init
                    // (a grid of textures the view! macro can't declare).
                    #[name = "app_icon_row"]
                    adw::PreferencesRow {
                        set_title: &i18n("App icon"),
                        set_activatable: false,
                        set_focusable: false,
                    },

                    #[name = "desktop_icons_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Use your desktop environment's icon set"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetDesktopIcons(row.is_active()));
                        },
                    },

                    #[name = "settings_open_row"]
                    adw::ComboRow {
                        set_title: &i18n("This window opens to"),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeSettingsOpen(row.selected()));
                        },
                    },

                },

                // The reader toolbar's buttons: three drop zones
                // (left group, right group, not shown) of
                // draggable chips, filled in init from the saved
                // layout. Every drop is applied and saved at once.
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Toolbar"),
                    set_description: Some(
                        &i18n("Drag buttons between the groups. The right group folds into ⋯ when the pane is narrow."),
                    ),
                    #[name = "toolbar_editor_box"]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 12,

                        gtk::Button {
                            set_label: &i18n("Restore Defaults"),
                            set_halign: gtk::Align::End,
                            set_valign: gtk::Align::Center,
                            connect_clicked => PrefInput::ToolbarRestore,
                        },

                        // Moved under the zones once they are built.
                        #[name = "toolbar_fold_list"]
                        gtk::ListBox {
                            add_css_class: "boxed-list",
                            set_selection_mode: gtk::SelectionMode::None,

                            #[name = "toolbar_fold_row"]
                            adw::SwitchRow {
                                set_title: &i18n("Always fold the right group into ⋯"),
                                connect_active_notify[sender] => move |row| {
                                    let _ = sender.output(PrefOutput::SetToolbarFoldRight(row.is_active()));
                                },
                            },
                        },
                    },
                },
            }
        }
        Self {
            page,
            app_theme_row,
            text_scale_row,
            theme_row,
            app_icon_row,
            desktop_icons_row,
            settings_open_row,
            toolbar_editor_box,
            toolbar_fold_list,
            toolbar_fold_row,
        }
    }
}
