//! Settings → General: checking for mail, notifications and their
//! sound, and the buttons a notification carries.

use adw::prelude::*;

use super::super::{PrefInput, PrefOutput, Preferences, ViewSender};
use crate::i18n::i18n;

/// The page, and the rows on it that the window reaches later.
#[derive(Debug)]
pub(in crate::ui::preferences) struct GeneralPage {
    pub(in crate::ui::preferences) page: adw::PreferencesPage,
    pub(in crate::ui::preferences) fetch_row: adw::ComboRow,
    pub(in crate::ui::preferences) push_row: adw::SwitchRow,
    pub(in crate::ui::preferences) metered_row: adw::SwitchRow,
    pub(in crate::ui::preferences) notifications_row: adw::SwitchRow,
    pub(in crate::ui::preferences) notification_content_row: adw::SwitchRow,
    pub(in crate::ui::preferences) notify_receipts_row: adw::SwitchRow,
    pub(in crate::ui::preferences) sound_row: adw::SwitchRow,
    pub(in crate::ui::preferences) sound_choice_row: adw::ComboRow,
    pub(in crate::ui::preferences) sound_file_row: adw::ActionRow,
    pub(in crate::ui::preferences) sound_focused_row: adw::SwitchRow,
    pub(in crate::ui::preferences) notify_mark_read_row: adw::SwitchRow,
    pub(in crate::ui::preferences) notify_archive_row: adw::SwitchRow,
    pub(in crate::ui::preferences) notify_delete_row: adw::SwitchRow,
    pub(in crate::ui::preferences) notify_reply_row: adw::SwitchRow,
    pub(in crate::ui::preferences) notify_forward_row: adw::SwitchRow,
    pub(in crate::ui::preferences) notify_spam_row: adw::SwitchRow,
}

impl GeneralPage {
    pub(in crate::ui::preferences) fn build(model: &Preferences, sender: &ViewSender) -> Self {
        relm4::view! {
            page = adw::PreferencesPage {
                add = &adw::PreferencesGroup {
                    set_title: &i18n("General"),

                    #[name = "fetch_row"]
                    adw::ComboRow {
                        set_title: &i18n("Check for new mail"),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeFetchInterval(row.selected()));
                        },
                    },

                    #[name = "push_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Instant new mail (push)"),
                        set_subtitle: &i18n("Microsoft 365 accounts check at the interval above."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetPush(row.is_active()));
                        },
                    },

                    #[name = "metered_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Save data on metered connections"),
                        set_subtitle: &i18n("New mail still arrives. Nothing else is fetched ahead."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetMeteredSaveData(row.is_active()));
                        },
                    },
                },

                add = &adw::PreferencesGroup {
                    set_title: &i18n("Notifications"),

                    #[name = "notifications_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Desktop notifications"),
                        set_subtitle: &i18n("For new mail and errors, while the window is in the background."),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleNotifications(row.is_active()));
                        },
                    },

                    #[name = "notification_content_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_sensitive: model.notifications,
                        set_title: &i18n("Show sender and subject"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetNotificationContent(row.is_active()));
                        },
                    },

                    // When a receipt comes back for mail sent (#520).
                    #[name = "notify_receipts_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_sensitive: model.notifications,
                        set_title: &i18n("Read receipts"),
                        set_subtitle: &i18n("When someone opens mail you asked a receipt for."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetNotifyReceipts(row.is_active()));
                        },
                    },

                    // A sound with each new-mail notification
                    // (#292): a built-in one or a file of the
                    // user's own.
                    #[name = "sound_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_sensitive: model.notifications,
                        set_title: &i18n("Sound for new mail"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleSound(row.is_active()));
                        },
                    },

                    #[name = "sound_choice_row"]
                    adw::ComboRow {
                        // Kept current by update_view.
                        set_sensitive: model.notifications && model.sound.enabled,
                        set_title: &i18n("Sound"),
                        add_suffix = &gtk::Button {
                            set_icon_name: "media-playback-start-symbolic",
                            set_tooltip_text: Some(i18n("Play").as_str()),
                            set_valign: gtk::Align::Center,
                            add_css_class: "flat",
                            connect_clicked[sender] => move |_| {
                                sender.input(PrefInput::PlaySound);
                            },
                        },
                        connect_selected_notify[sender] => move |row| {
                            sender.input(PrefInput::ChangeSound(row.selected()));
                        },
                    },

                    #[name = "sound_file_row"]
                    adw::ActionRow {
                        // Kept current by update_view.
                        set_visible: model.sound.sound == crate::config::CUSTOM_SOUND,
                        // Kept current by update_view.
                        set_sensitive: model.notifications && model.sound.enabled,
                        set_title: &i18n("Sound file"),
                        // Kept current by update_view.
                        set_subtitle: &custom_sound_name(model),
                        add_suffix = &gtk::Button {
                            set_label: &i18n("Choose…"),
                            set_valign: gtk::Align::Center,
                            connect_clicked[sender] => move |_| {
                                sender.input(PrefInput::ChooseSound);
                            },
                        },
                    },

                    // GNOME shows no notification for the app
                    // in front, so by default there is no sound
                    // either; this plays it anyway (#337).
                    #[name = "sound_focused_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_sensitive: model.notifications && model.sound.enabled,
                        set_title: &i18n("Also play while Hylki is in front"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleSoundFocused(row.is_active()));
                        },
                    },

                },

                // The buttons a single-message notification
                // carries (#244); a summary of several has none.
                // Three at most: once three are on, the rest
                // grey out until one is switched off.
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Notification Buttons"),
                    set_description: Some(&i18n("Up to three buttons on a notification about one message.")),

                    #[name = "notify_mark_read_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_sensitive: model.button_row_sensitive(crate::config::NotificationButton::MarkRead),
                        set_title: &i18n("Mark as Read"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleNotificationButton(crate::config::NotificationButton::MarkRead, row.is_active()));
                        },
                    },

                    #[name = "notify_archive_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_sensitive: model.button_row_sensitive(crate::config::NotificationButton::Archive),
                        set_title: &i18n("Archive"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleNotificationButton(crate::config::NotificationButton::Archive, row.is_active()));
                        },
                    },

                    #[name = "notify_delete_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_sensitive: model.button_row_sensitive(crate::config::NotificationButton::Delete),
                        set_title: &i18n("Delete"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleNotificationButton(crate::config::NotificationButton::Delete, row.is_active()));
                        },
                    },

                    #[name = "notify_reply_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_sensitive: model.button_row_sensitive(crate::config::NotificationButton::Reply),
                        set_title: &i18n("Reply"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleNotificationButton(crate::config::NotificationButton::Reply, row.is_active()));
                        },
                    },

                    #[name = "notify_forward_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_sensitive: model.button_row_sensitive(crate::config::NotificationButton::Forward),
                        set_title: &i18n("Forward"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleNotificationButton(crate::config::NotificationButton::Forward, row.is_active()));
                        },
                    },

                    #[name = "notify_spam_row"]
                    adw::SwitchRow {
                        // Kept current by update_view.
                        set_sensitive: model.button_row_sensitive(crate::config::NotificationButton::Spam),
                        set_title: &i18n("Mark as Spam"),
                        connect_active_notify[sender] => move |row| {
                            sender.input(PrefInput::ToggleNotificationButton(crate::config::NotificationButton::Spam, row.is_active()));
                        },
                    },
                },
            }
        }
        Self {
            page,
            fetch_row,
            push_row,
            metered_row,
            notifications_row,
            notification_content_row,
            notify_receipts_row,
            sound_row,
            sound_choice_row,
            sound_file_row,
            sound_focused_row,
            notify_mark_read_row,
            notify_archive_row,
            notify_delete_row,
            notify_reply_row,
            notify_forward_row,
            notify_spam_row,
        }
    }

    /// The view's `#[watch]` setters for this page, in the order the
    /// view had them (see the module comment of `pages`).
    pub(in crate::ui::preferences) fn update_view(&self, model: &Preferences) {
        self.notification_content_row
            .set_sensitive(model.notifications);
        self.notify_receipts_row.set_sensitive(model.notifications);
        self.sound_row.set_sensitive(model.notifications);
        self.sound_choice_row
            .set_sensitive(model.notifications && model.sound.enabled);
        self.sound_file_row
            .set_visible(model.sound.sound == crate::config::CUSTOM_SOUND);
        self.sound_file_row
            .set_sensitive(model.notifications && model.sound.enabled);
        self.sound_file_row.set_subtitle(&custom_sound_name(model));
        self.sound_focused_row
            .set_sensitive(model.notifications && model.sound.enabled);
        self.notify_mark_read_row
            .set_sensitive(model.button_row_sensitive(crate::config::NotificationButton::MarkRead));
        self.notify_archive_row
            .set_sensitive(model.button_row_sensitive(crate::config::NotificationButton::Archive));
        self.notify_delete_row
            .set_sensitive(model.button_row_sensitive(crate::config::NotificationButton::Delete));
        self.notify_reply_row
            .set_sensitive(model.button_row_sensitive(crate::config::NotificationButton::Reply));
        self.notify_forward_row
            .set_sensitive(model.button_row_sensitive(crate::config::NotificationButton::Forward));
        self.notify_spam_row
            .set_sensitive(model.button_row_sensitive(crate::config::NotificationButton::Spam));
    }
}

/// The Sound file row's subtitle: the custom sound's file name.
fn custom_sound_name(model: &Preferences) -> String {
    model
        .custom_sound
        .as_ref()
        .and_then(|p| p.file_name())
        .map_or_else(|| i18n("None"), |n| n.to_string_lossy().into_owned())
}
