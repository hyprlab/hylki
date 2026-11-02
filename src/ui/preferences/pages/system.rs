//! Settings → System: tray and launcher, storage, the GNOME Files
//! extension and links.

use adw::prelude::*;

use super::super::system::{link_group_description, nautilus_status_text, storage_text};
use super::super::{PrefInput, PrefOutput, Preferences, ViewSender};
use crate::i18n::i18n;

/// The page, and the rows on it that the window reaches later.
#[derive(Debug)]
pub(in crate::ui::preferences) struct SystemPage {
    pub(in crate::ui::preferences) page: adw::PreferencesPage,
    pub(in crate::ui::preferences) language_row: adw::ComboRow,
    pub(in crate::ui::preferences) renderer_row: adw::ComboRow,
    pub(in crate::ui::preferences) background_row: adw::SwitchRow,
    pub(in crate::ui::preferences) autostart_row: adw::SwitchRow,
    pub(in crate::ui::preferences) fullscreen_row: adw::SwitchRow,
    pub(in crate::ui::preferences) tray_row: adw::SwitchRow,
    pub(in crate::ui::preferences) tray_icon_row: adw::ComboRow,
    pub(in crate::ui::preferences) tray_mail_row: adw::SwitchRow,
    pub(in crate::ui::preferences) launcher_badge_row: adw::ComboRow,
    pub(in crate::ui::preferences) single_key_row: adw::SwitchRow,
    pub(in crate::ui::preferences) gmail_keys_row: adw::SwitchRow,
    pub(in crate::ui::preferences) status_bar_row: adw::ComboRow,
    pub(in crate::ui::preferences) console_mode_row: adw::SwitchRow,
    pub(in crate::ui::preferences) storage_row: adw::ActionRow,
    pub(in crate::ui::preferences) storage_remove_btn: gtk::Button,
    pub(in crate::ui::preferences) keep_downloads_row: adw::ComboRow,
    pub(in crate::ui::preferences) nautilus_row: adw::ActionRow,
    pub(in crate::ui::preferences) nautilus_remove_btn: gtk::Button,
    pub(in crate::ui::preferences) nautilus_install_btn: gtk::Button,
    pub(in crate::ui::preferences) nautilus_cmd_row: adw::EntryRow,
    pub(in crate::ui::preferences) files_action_row: adw::ComboRow,
    pub(in crate::ui::preferences) files_large_row: adw::ComboRow,
    pub(in crate::ui::preferences) files_limit_row: adw::SpinRow,
    pub(in crate::ui::preferences) link_browser_row: adw::ComboRow,
}

impl SystemPage {
    pub(in crate::ui::preferences) fn build(model: &Preferences, sender: &ViewSender) -> Self {
        relm4::view! {
            page = adw::PreferencesPage {
                add = &adw::PreferencesGroup {
                    set_title: &i18n("System"),

                    #[name = "language_row"]
                    adw::ComboRow {
                        set_title: &i18n("Language"),
                        set_subtitle: &i18n("Applies after a restart."),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeLanguage(row.selected()));
                        },
                    },

                    #[name = "renderer_row"]
                    adw::ComboRow {
                        set_title: &i18n("Renderer"),
                        set_subtitle: &i18n("Try OpenGL or Software on older graphics. Applies after a restart."),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeRenderer(row.selected()));
                        },
                    },

                    #[name = "background_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Keep running in the background"),
                        set_subtitle: &i18n("Closing the window hides it. Quit from Background Apps."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetRunInBackground(row.is_active()));
                        },
                    },

                    #[name = "autostart_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Start at login"),
                        set_subtitle: &i18n("Starts hidden."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetAutostart(row.is_active()));
                        },
                    },

                    #[name = "fullscreen_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Start fullscreen"),
                        set_subtitle: &i18n("F11 switches fullscreen on and off."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetStartFullscreen(row.is_active()));
                        },
                    },

                    #[name = "tray_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Show a tray icon"),
                        set_subtitle: &i18n("To use on GNOME, install an AppIndicator extension."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetTray(row.is_active()));
                        },
                    },

                    #[name = "tray_icon_row"]
                    adw::ComboRow {
                        set_title: &i18n("Tray icon"),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeTrayIcon(row.selected()));
                        },
                    },

                    #[name = "tray_mail_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Unread mail in the tray menu"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetTrayMail(row.is_active()));
                        },
                    },

                    #[name = "launcher_badge_row"]
                    adw::ComboRow {
                        set_title: &i18n("Unread count on the app icon"),
                        set_subtitle: &i18n("To use on GNOME, install a dock extension such as Dash to Dock."),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeLauncherBadge(row.selected()));
                        },
                    },

                    #[name = "single_key_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Single-key shortcuts"),
                        set_subtitle: &i18n("Press Ctrl+? to list all shortcuts."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetSingleKey(row.is_active()));
                        },
                    },

                    #[name = "gmail_keys_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Gmail keys"),
                        set_subtitle: &i18n("Swaps the single-key shortcuts for Gmail equivalents."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetGmailKeys(row.is_active()));
                        },
                    },

                    #[name = "status_bar_row"]
                    adw::ComboRow {
                        set_title: &i18n("Status bar"),
                        connect_selected_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetStatusBarTop(row.selected() == 1));
                        },
                    },

                    #[name = "console_mode_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Console mode"),
                        set_subtitle: &i18n("Shows a console in the status bar."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetConsoleMode(row.is_active()));
                        },
                    },

                    adw::ActionRow {
                        set_title: &i18n("Export log"),
                        set_subtitle: &i18n("For bug reports. Addresses are cut to their domain for privacy."),
                        set_activatable: true,
                        connect_activated => PrefInput::ExportLog,
                        add_suffix = &gtk::Image {
                            set_icon_name: Some("go-next-symbolic"),
                        },
                    },
                },

                // What downloaded mail takes up, and how long it
                // stays (#394).
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Storage"),

                    #[name = "storage_row"]
                    adw::ActionRow {
                        set_title: &i18n("Downloaded mail"),
                        // Kept current by update_view.
                        set_subtitle: &storage_text(model.storage),
                        #[name = "storage_remove_btn"]
                        add_suffix = &gtk::Button {
                            set_label: &i18n("Remove…"),
                            set_valign: gtk::Align::Center,
                            add_css_class: "destructive-action",
                            // Kept current by update_view.
                            set_sensitive: model.storage.is_some_and(|(b, f, _)| b + f > 0),
                            connect_clicked => PrefInput::RemoveDownloads,
                        },
                    },

                    #[name = "keep_downloads_row"]
                    adw::ComboRow {
                        set_title: &i18n("Keep downloaded mail"),
                        set_subtitle: &i18n("Mail inside an account's offline period is kept."),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeKeepDownloads(row.selected()));
                        },
                    },
                },

                // The Files right-click extension (#188).
                add = &adw::PreferencesGroup {
                    set_title: &i18n("GNOME Files Integration"),
                    set_description: Some(
                        i18n("Adds “Send with Hylki” to the right-click menu in Files. Needs the nautilus-python package and a restart of Files.").as_str()
                    ),

                    #[name = "nautilus_row"]
                    adw::ActionRow {
                        set_title: &i18n("Right-click menu entry"),
                        // Kept current by update_view.
                        set_subtitle: &nautilus_status_text(&model.nautilus),
                        #[name = "nautilus_remove_btn"]
                        add_suffix = &gtk::Button {
                            set_label: &i18n("Remove"),
                            set_valign: gtk::Align::Center,
                            // Kept current by update_view.
                            set_visible: model.nautilus.status != crate::desktop::nautilus_ext::Status::NotInstalled,
                            connect_clicked => PrefInput::NautilusRemove,
                        },
                        #[name = "nautilus_install_btn"]
                        add_suffix = &gtk::Button {
                            // Kept current by update_view.
                            set_label: &nautilus_install_label(model),
                            set_valign: gtk::Align::Center,
                            add_css_class: "suggested-action",
                            // Kept current by update_view.
                            set_visible: model.nautilus.status != crate::desktop::nautilus_ext::Status::Installed,
                            connect_clicked => PrefInput::NautilusInstall,
                        },
                    },

                    // The loader's install command, ready to paste, while
                    // there is no sign Files can load the extension.
                    #[name = "nautilus_cmd_row"]
                    adw::EntryRow {
                        set_title: &i18n("Install nautilus-python first"),
                        set_text: model.nautilus_cmd.unwrap_or_default(),
                        set_editable: false,
                        add_css_class: "monospace",
                        // Kept current by update_view.
                        set_visible: nautilus_cmd_wanted(model),
                        add_suffix = &gtk::Button {
                            set_icon_name: "edit-copy-symbolic",
                            set_valign: gtk::Align::Center,
                            set_tooltip_text: Some(i18n("Copy").as_str()),
                            add_css_class: "flat",
                            connect_clicked[cmd = model.nautilus_cmd.unwrap_or_default().to_string()] => move |b| {
                                b.clipboard().set_text(&cmd);
                                b.set_icon_name("verified-checkmark-symbolic");
                                let b = b.clone();
                                gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
                                    b.set_icon_name("edit-copy-symbolic");
                                });
                            },
                        },
                    },

                    adw::ActionRow {
                        set_title: &i18n("Restart Files"),
                        set_subtitle: &i18n("Closes every Files window, like “nautilus -q”."),
                        add_suffix = &gtk::Button {
                            set_label: &i18n("Restart"),
                            set_valign: gtk::Align::Center,
                            connect_clicked => PrefInput::NautilusRestartFiles,
                        },
                    },

                    // What the handed-in files open into, and
                    // what happens when they are big.
                    #[name = "files_action_row"]
                    adw::ComboRow {
                        set_title: &i18n("Default Send with Hylki behavior"),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeFilesAction(row.selected()));
                        },
                    },

                    #[name = "files_large_row"]
                    adw::ComboRow {
                        set_title: &i18n("Default large attachments behavior"),
                        set_subtitle: &i18n("Without a cloud storage account, files are attached."),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeFilesLarge(row.selected()));
                        },
                    },

                    #[name = "files_limit_row"]
                    adw::SpinRow {
                        set_title: &i18n("Size limit"),
                        set_subtitle: &i18n("In MB, for all the files together."),
                        set_adjustment: Some(&gtk::Adjustment::new(20.0, 1.0, 5000.0, 1.0, 10.0, 0.0)),
                        set_numeric: true,
                        connect_value_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeFilesLimit(row.value().round() as u32));
                        },
                    },
                },

                // Where a link in a message goes (#232).
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Links"),
                    set_description: Some(link_group_description(&model.browsers).as_str()),

                    // Its handler is connected after the list
                    // is filled below, not here: giving a
                    // ComboRow its model moves the selection
                    // to the first item, and that notification
                    // would read as the user choosing
                    // "System default".
                    #[name = "link_browser_row"]
                    adw::ComboRow {
                        set_title: &i18n("Open links in"),
                    },
                },
            }
        }
        Self {
            page,
            language_row,
            renderer_row,
            background_row,
            autostart_row,
            fullscreen_row,
            tray_row,
            tray_icon_row,
            tray_mail_row,
            launcher_badge_row,
            single_key_row,
            gmail_keys_row,
            status_bar_row,
            console_mode_row,
            storage_row,
            storage_remove_btn,
            keep_downloads_row,
            nautilus_row,
            nautilus_remove_btn,
            nautilus_install_btn,
            nautilus_cmd_row,
            files_action_row,
            files_large_row,
            files_limit_row,
            link_browser_row,
        }
    }

    /// The view's `#[watch]` setters for this page, in the order the
    /// view had them (see the module comment of `pages`).
    pub(in crate::ui::preferences) fn update_view(&self, model: &Preferences) {
        self.storage_row.set_subtitle(&storage_text(model.storage));
        self.storage_remove_btn
            .set_sensitive(model.storage.is_some_and(|(b, f, _)| b + f > 0));
        self.nautilus_row
            .set_subtitle(&nautilus_status_text(&model.nautilus));
        self.nautilus_remove_btn.set_visible(
            model.nautilus.status != crate::desktop::nautilus_ext::Status::NotInstalled,
        );
        self.nautilus_install_btn
            .set_label(&nautilus_install_label(model));
        self.nautilus_install_btn
            .set_visible(model.nautilus.status != crate::desktop::nautilus_ext::Status::Installed);
        self.nautilus_cmd_row
            .set_visible(nautilus_cmd_wanted(model));
    }
}

/// The Files extension's install button: Update over an older copy.
fn nautilus_install_label(model: &Preferences) -> String {
    if model.nautilus.status == crate::desktop::nautilus_ext::Status::Outdated {
        i18n("Update")
    } else {
        i18n("Install")
    }
}

/// Whether the loader's install command is shown: there is one for this
/// system, and no sign that Files can load the extension.
fn nautilus_cmd_wanted(model: &Preferences) -> bool {
    model.nautilus_cmd.is_some() && !model.nautilus.loaded && model.nautilus.loader != Some(true)
}
