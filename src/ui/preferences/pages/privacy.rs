//! Settings → Privacy.

use adw::prelude::*;

use super::super::{PrefOutput, ViewSender};
use crate::i18n::i18n;

/// The page, and the rows on it that the window reaches later.
#[derive(Debug)]
pub(in crate::ui::preferences) struct PrivacyPage {
    pub(in crate::ui::preferences) page: adw::PreferencesPage,
    pub(in crate::ui::preferences) auto_remote_content_row: adw::SwitchRow,
    pub(in crate::ui::preferences) remote_ahead_row: adw::SwitchRow,
    pub(in crate::ui::preferences) clean_links_row: adw::SwitchRow,
    pub(in crate::ui::preferences) show_remote_banner_row: adw::SwitchRow,
    pub(in crate::ui::preferences) show_spoof_banner_row: adw::SwitchRow,
    pub(in crate::ui::preferences) gravatar_row: adw::SwitchRow,
    pub(in crate::ui::preferences) sender_logos_row: adw::SwitchRow,
    pub(in crate::ui::preferences) receipts_row: adw::ComboRow,
}

impl PrivacyPage {
    pub(in crate::ui::preferences) fn build(sender: &ViewSender) -> Self {
        relm4::view! {
            page = adw::PreferencesPage {
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Privacy"),
                    set_description: Some(
                        i18n("Hylki collects no telemetry and sends no analytics.").as_str()
                    ),

                    #[name = "auto_remote_content_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Always load remote content"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetAutoRemoteContent(row.is_active()));
                        },
                    },

                    #[name = "remote_ahead_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Pre-load remote content for the next messages"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetRemoteAhead(row.is_active()));
                        },
                    },

                    #[name = "clean_links_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Remove trackers from links"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetCleanLinks(row.is_active()));
                        },
                    },

                    #[name = "show_remote_banner_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Warn when remote content is blocked"),
                        set_subtitle: &i18n("Off hides the banner. Remote content stays blocked."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetShowRemoteBanner(row.is_active()));
                        },
                    },

                    #[name = "show_spoof_banner_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Warn when the addressing doesn't match"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetShowSpoofBanner(row.is_active()));
                        },
                    },

                    #[name = "gravatar_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Use Gravatar when a contact has no photo"),
                        set_subtitle: &i18n("Sends a hash of the sender's address to Gravatar."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetGravatar(row.is_active()));
                        },
                    },

                    #[name = "sender_logos_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Show sender logos"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetSenderLogos(row.is_active()));
                        },
                    },

                    #[name = "receipts_row"]
                    adw::ComboRow {
                        set_title: &i18n("Send read receipts"),
                        set_model: Some(&gtk::StringList::new(&[
                            &i18n("Ask each time"),
                            &i18n("Always"),
                            &i18n("Never"),
                        ])),
                        connect_selected_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetReceipts(
                                crate::ui::preferences::choices::receipts_at(row.selected()),
                            ));
                        },
                    },

                },
            }
        }
        Self {
            page,
            auto_remote_content_row,
            remote_ahead_row,
            clean_links_row,
            show_remote_banner_row,
            show_spoof_banner_row,
            gravatar_row,
            sender_logos_row,
            receipts_row,
        }
    }
}
