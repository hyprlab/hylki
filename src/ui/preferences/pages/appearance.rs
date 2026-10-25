//! Settings → Appearance: style, text size, the theme and app-icon
//! galleries, the reader toolbar and Focus Mode.

use adw::prelude::*;

use super::super::{PrefInput, PrefOutput, Preferences, ViewSender};
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
    pub(in crate::ui::preferences) focus_row: adw::SwitchRow,
    pub(in crate::ui::preferences) focus_start_row: adw::SwitchRow,
    pub(in crate::ui::preferences) focus_toolbar_row: adw::SwitchRow,
    pub(in crate::ui::preferences) focus_list_header_row: adw::SwitchRow,
    pub(in crate::ui::preferences) focus_accounts_row: adw::SwitchRow,
    pub(in crate::ui::preferences) focus_unified_row: adw::SwitchRow,
    pub(in crate::ui::preferences) focus_avatars_row: adw::SwitchRow,
    pub(in crate::ui::preferences) focus_hide_preview_row: adw::SwitchRow,
    pub(in crate::ui::preferences) focus_preview_row: adw::SwitchRow,
    pub(in crate::ui::preferences) focus_subject_row: adw::SwitchRow,
    pub(in crate::ui::preferences) focus_rail_row: adw::SwitchRow,
    pub(in crate::ui::preferences) focus_reader_row: adw::SwitchRow,
}

impl AppearancePage {
    pub(in crate::ui::preferences) fn build(model: &Preferences, sender: &ViewSender) -> Self {
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

                // Focus Mode: the master switch and its parts.
                // Every row applies at once, animated.
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Focus Mode"),
                    set_description: Some(
                        &i18n("The parts switched on below hide while Focus Mode is on. Activated by Ctrl+Shift+F or the main menu."),
                    ),

                    #[name = "focus_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_active: model.focus.enabled,
                        set_title: &i18n("Focus Mode"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::Enabled, row.is_active()));
                        },
                    },
                    #[name = "focus_start_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_active: model.focus.start_focused,
                        set_title: &i18n("Start in Focus Mode"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::StartFocused, row.is_active()));
                        },
                    },
                    #[name = "focus_toolbar_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_active: model.focus.reader_toolbar,
                        set_title: &i18n("Fold the reading pane's toolbar"),
                        set_subtitle: &i18n("Every button goes into its ⋯ menu."),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::ReaderToolbar, row.is_active()));
                        },
                    },
                    #[name = "focus_list_header_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_active: model.focus.list_header,
                        set_title: &i18n("Fold the message list's header"),
                        set_subtitle: &i18n("Everything but the sidebar toggle goes into ⋯."),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::ListHeader, row.is_active()));
                        },
                    },
                    #[name = "focus_accounts_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_active: model.focus.hide_accounts,
                        set_title: &i18n("Hide the accounts in the sidebar"),
                        set_subtitle: &i18n("Only the unified section stays."),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::HideAccounts, row.is_active()));
                        },
                    },
                    #[name = "focus_unified_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_active: model.focus.fold_unified,
                        set_title: &i18n("Fold up the unified rows"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::FoldUnified, row.is_active()));
                        },
                    },
                    #[name = "focus_avatars_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_active: model.focus.hide_avatars,
                        set_title: &i18n("Hide sender avatars"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::HideAvatars, row.is_active()));
                        },
                    },
                    #[name = "focus_hide_preview_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_active: model.focus.hide_preview,
                        set_title: &i18n("Hide the preview text"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::HidePreview, row.is_active()));
                        },
                    },
                    #[name = "focus_preview_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_active: model.focus.one_preview_line,
                        // Nothing left to cap once the preview
                        // text is hidden altogether.
                        // Kept current by update_view.
                        set_sensitive: !model.focus.hide_preview,
                        set_title: &i18n("One line of preview text"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::OnePreviewLine, row.is_active()));
                        },
                    },
                    #[name = "focus_subject_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_active: model.focus.hide_subject,
                        set_title: &i18n("Hide the subject"),
                        set_subtitle: &i18n("Tag chips hide with it."),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::HideSubject, row.is_active()));
                        },
                    },
                    #[name = "focus_rail_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_active: model.focus.rail_sidebar,
                        set_title: &i18n("Fold the sidebar to the icon rail"),
                        set_subtitle: &i18n("Hovering it slides the full sidebar out."),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::RailSidebar, row.is_active()));
                        },
                    },
                    #[name = "focus_reader_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_active: model.focus.reader_view,
                        set_title: &i18n("Reader View"),
                        set_subtitle: &i18n("Open messages in Reader View."),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::ReaderView, row.is_active()));
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
            focus_row,
            focus_start_row,
            focus_toolbar_row,
            focus_list_header_row,
            focus_accounts_row,
            focus_unified_row,
            focus_avatars_row,
            focus_hide_preview_row,
            focus_preview_row,
            focus_subject_row,
            focus_rail_row,
            focus_reader_row,
        }
    }

    /// The view's `#[watch]` setters for this page, in the order the
    /// view had them (see the module comment of `pages`).
    pub(in crate::ui::preferences) fn update_view(&self, model: &Preferences) {
        self.focus_row.set_active(model.focus.enabled);
        self.focus_start_row.set_active(model.focus.start_focused);
        self.focus_toolbar_row
            .set_active(model.focus.reader_toolbar);
        self.focus_list_header_row
            .set_active(model.focus.list_header);
        self.focus_accounts_row
            .set_active(model.focus.hide_accounts);
        self.focus_unified_row.set_active(model.focus.fold_unified);
        self.focus_avatars_row.set_active(model.focus.hide_avatars);
        self.focus_hide_preview_row
            .set_active(model.focus.hide_preview);
        self.focus_preview_row
            .set_active(model.focus.one_preview_line);
        self.focus_preview_row
            .set_sensitive(!model.focus.hide_preview);
        self.focus_subject_row.set_active(model.focus.hide_subject);
        self.focus_rail_row.set_active(model.focus.rail_sidebar);
        self.focus_reader_row.set_active(model.focus.reader_view);
    }
}
