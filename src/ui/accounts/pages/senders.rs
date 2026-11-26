//! Settings → Senders: the allowed and the blocked senders. Their lists are
//! the factories' own boxes, handed in.

use adw::prelude::*;
use relm4::prelude::*;

use super::super::{AccountsInput, AccountsWindow};
use crate::i18n::i18n;

/// The page: nothing on it is reached later, the lists being the
/// factories' own.
#[derive(Debug)]
pub(in crate::ui::accounts) struct SendersPage {
    pub(in crate::ui::accounts) page: adw::PreferencesPage,
}

impl SendersPage {
    pub(in crate::ui::accounts) fn build(
        sender: &ComponentSender<AccountsWindow>,
        senders_box: &gtk::ListBox,
        blacklist_box: &gtk::ListBox,
    ) -> Self {
        relm4::view! {
            page = adw::PreferencesPage {
                // A search box over the long lists (#141): rows
                // that do not match the text are hidden.
                add = &adw::PreferencesGroup {
                    #[name = "senders_search"]
                    gtk::SearchEntry {
                        set_placeholder_text: Some(i18n("Search senders").as_str()),
                        connect_search_changed[sender] => move |entry| {
                            sender.input_sender().emit(AccountsInput::SearchSenders(entry.text().to_string()));
                        },
                    },
                },

                add = &adw::PreferencesGroup {
                    set_title: &i18n("Allowed Senders"),
                    set_description: Some(
                        i18n("Messages from these senders load remote content \
                         automatically.").as_str()
                    ),

                    #[name = "add_sender_row"]
                    adw::EntryRow {
                        set_title: &i18n("Email address"),
                        set_input_purpose: gtk::InputPurpose::Email,
                        set_show_apply_button: false,
                        connect_entry_activated[sender] => move |row| {
                            sender.input_sender().emit(AccountsInput::AddSenderText(row.text().to_string()));
                            row.set_text("");
                        },

                        add_suffix = &gtk::Button {
                            set_icon_name: "list-add-symbolic",
                            set_tooltip_text: Some(i18n("Allow this sender").as_str()),
                            set_valign: gtk::Align::Center,
                            add_css_class: "flat",
                            connect_clicked[sender, add_sender_row] => move |_| {
                                sender.input_sender().emit(AccountsInput::AddSenderText(
                                    add_sender_row.text().to_string(),
                                ));
                                add_sender_row.set_text("");
                            },
                        },
                    },

                    #[local_ref]
                    senders_box -> gtk::ListBox {
                        add_css_class: "boxed-list",
                        add_css_class: "sender-list",
                        set_selection_mode: gtk::SelectionMode::None,
                    },
                },

                add = &adw::PreferencesGroup {
                    set_title: &i18n("Blocked Senders"),
                    set_description: Some(
                        i18n("Their mail is moved to Trash on arrival. A domain, such as “example.com”, blocks every address in it.").as_str()
                    ),

                    #[name = "add_blacklist_row"]
                    adw::EntryRow {
                        set_title: &i18n("Address or domain"),
                        set_show_apply_button: false,
                        connect_entry_activated[sender] => move |row| {
                            sender.input_sender().emit(AccountsInput::AddBlacklistText(row.text().to_string()));
                            row.set_text("");
                        },

                        add_suffix = &gtk::Button {
                            set_icon_name: "list-add-symbolic",
                            set_tooltip_text: Some(i18n("Block this sender").as_str()),
                            set_valign: gtk::Align::Center,
                            add_css_class: "flat",
                            connect_clicked[sender, add_blacklist_row] => move |_| {
                                sender.input_sender().emit(AccountsInput::AddBlacklistText(
                                    add_blacklist_row.text().to_string(),
                                ));
                                add_blacklist_row.set_text("");
                            },
                        },
                    },

                    #[local_ref]
                    blacklist_box -> gtk::ListBox {
                        add_css_class: "boxed-list",
                        add_css_class: "sender-list",
                        set_selection_mode: gtk::SelectionMode::None,
                    },
                },
            }
        }
        Self { page }
    }
}
