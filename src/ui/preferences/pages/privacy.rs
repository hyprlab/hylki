//! Settings → Privacy, with the connection for AI assistants.

use adw::prelude::*;

use super::super::{PrefOutput, Preferences, ViewSender};
use crate::i18n::i18n;

/// The page, and the rows on it that the window reaches later.
#[derive(Debug)]
pub(in crate::ui::preferences) struct PrivacyPage {
    pub(in crate::ui::preferences) page: adw::PreferencesPage,
    pub(in crate::ui::preferences) auto_remote_content_row: adw::SwitchRow,
    pub(in crate::ui::preferences) remote_ahead_row: adw::SwitchRow,
    pub(in crate::ui::preferences) show_remote_banner_row: adw::SwitchRow,
    pub(in crate::ui::preferences) show_spoof_banner_row: adw::SwitchRow,
    pub(in crate::ui::preferences) gravatar_row: adw::SwitchRow,
    pub(in crate::ui::preferences) sender_logos_row: adw::SwitchRow,
    pub(in crate::ui::preferences) receipts_row: adw::ComboRow,
    pub(in crate::ui::preferences) assistant_row: adw::SwitchRow,
    pub(in crate::ui::preferences) assistant_status_row: adw::ActionRow,
    pub(in crate::ui::preferences) assistant_cmd_row: adw::EntryRow,
}

impl PrivacyPage {
    pub(in crate::ui::preferences) fn build(model: &Preferences, sender: &ViewSender) -> Self {
        let command = crate::mcp::connect_command();
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
                        set_subtitle: &i18n("Remote content can tell a sender when and where you read."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetAutoRemoteContent(row.is_active()));
                        },
                    },

                    #[name = "remote_ahead_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Pre-load remote content for the next messages"),
                        set_subtitle: &i18n("Their senders can tell they were opened."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetRemoteAhead(row.is_active()));
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
                        set_subtitle: &i18n("The badge beside the sender stays. A possible forgery always shows the banner."),
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
                        set_subtitle: &i18n("Most are fetched from the sender's domain, which learns your IP address."),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetSenderLogos(row.is_active()));
                        },
                    },

                    #[name = "receipts_row"]
                    adw::ComboRow {
                        set_title: &i18n("Send read receipts"),
                        set_subtitle: &i18n("A receipt tells the sender when you opened the message."),
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

                // An assistant's way in (crate::mcp): reading, looking up
                // contacts and preparing messages, never sending them.
                add = &adw::PreferencesGroup {
                    set_title: &i18n("AI Assistants"),
                    set_description: Some(
                        i18n("Through the Model Context Protocol. An assistant never sends: each message it prepares opens in a window for you to check and send.").as_str()
                    ),

                    #[name = "assistant_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Let AI assistants read mail and prepare messages while Hylki is running"),
                        connect_active_notify[sender] => move |row| {
                            let _ = sender.output(PrefOutput::SetAssistantAccess(row.is_active()));
                        },
                    },

                    #[name = "assistant_status_row"]
                    adw::ActionRow {
                        set_title: &i18n("Status"),
                        // Kept current by update_view.
                        set_subtitle: &model.assistant_status,
                    },

                    // The command that adds Hylki to an assistant, ready
                    // to paste into a terminal.
                    #[name = "assistant_cmd_row"]
                    adw::EntryRow {
                        set_title: &i18n("Add to Claude Code"),
                        set_text: &command,
                        set_editable: false,
                        add_css_class: "monospace",
                        add_suffix = &gtk::Button {
                            set_icon_name: "edit-copy-symbolic",
                            set_valign: gtk::Align::Center,
                            set_tooltip_text: Some(i18n("Copy").as_str()),
                            add_css_class: "flat",
                            connect_clicked[command = command.clone()] => move |b| {
                                b.clipboard().set_text(&command);
                                b.set_icon_name("verified-checkmark-symbolic");
                                let b = b.clone();
                                gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
                                    b.set_icon_name("edit-copy-symbolic");
                                });
                            },
                        },
                    },
                },
            }
        }
        Self {
            page,
            auto_remote_content_row,
            remote_ahead_row,
            show_remote_banner_row,
            show_spoof_banner_row,
            gravatar_row,
            sender_logos_row,
            receipts_row,
            assistant_row,
            assistant_status_row,
            assistant_cmd_row,
        }
    }

    /// The view's `#[watch]` setters for this page (see the module
    /// comment of `pages`).
    pub(in crate::ui::preferences) fn update_view(&self, model: &Preferences) {
        self.assistant_status_row
            .set_subtitle(&model.assistant_status);
    }
}
