//! Settings → Accounts: the mail accounts, and the GNOME Online Accounts
//! ones that can be switched on.

use adw::prelude::*;
use relm4::prelude::*;

use super::super::{AccountsInput, AccountsWindow};
use crate::i18n::i18n;

/// The page, and the widgets on it that the panel reaches later.
#[derive(Debug)]
pub(in crate::ui::accounts) struct AccountsPage {
    pub(in crate::ui::accounts) page: adw::PreferencesPage,
    pub(in crate::ui::accounts) goa_refresh_btn: gtk::Button,
    pub(in crate::ui::accounts) accounts_list: gtk::ListBox,
    pub(in crate::ui::accounts) goa_group: adw::PreferencesGroup,
    pub(in crate::ui::accounts) local_group: adw::PreferencesGroup,
    pub(in crate::ui::accounts) goa_list: gtk::ListBox,
}

impl AccountsPage {
    pub(in crate::ui::accounts) fn build(sender: &ComponentSender<AccountsWindow>) -> Self {
        relm4::view! {
            page = adw::PreferencesPage {
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Mail Accounts"),
                    set_description: Some(
                        i18n("Drag to reorder them in the sidebar.").as_str()
                    ),
                    // At the header's end, as the Tags and
                    // Cloud Storage panels place theirs.
                    #[wrap(Some)]
                    set_header_suffix = &gtk::Box {
                        set_orientation: gtk::Orientation::Horizontal,
                        set_spacing: 6,
                        set_valign: gtk::Align::Start,
                        set_halign: gtk::Align::End,
                        set_margin_start: 24,
                        // An account just added in GNOME
                        // Settings, without waiting for GOA
                        // to say so.
                        #[name = "goa_refresh_btn"]
                        gtk::Button {
                            set_icon_name: "view-refresh-symbolic",
                            set_tooltip_text: Some(&i18n("Check GNOME Online Accounts for new accounts")),
                            connect_clicked => AccountsInput::RefreshGoa,
                        },
                        gtk::Button {
                            set_label: &i18n("Add Account…"),
                            set_size_request: (130, -1),
                            connect_clicked => AccountsInput::AddAccount,
                        },
                    },

                    #[name = "accounts_list"]
                    gtk::ListBox {
                        add_css_class: "boxed-list",
                        set_selection_mode: gtk::SelectionMode::None,
                        connect_row_activated[sender] => move |_, row| {
                            sender.input_sender().emit(AccountsInput::EditAccount(row.index() as usize));
                        },
                    },
                },

                // On This Computer (#552): folders on this computer
                // rather than a server, offered until there is one.
                #[name = "local_group"]
                add = &adw::PreferencesGroup {
                    set_title: &i18n("On This Computer"),
                    set_description: Some(
                        i18n("Store mail in folders that live on this computer. Move or copy mail from any account to here.").as_str()
                    ),
                    set_visible: false,
                    #[wrap(Some)]
                    set_header_suffix = &gtk::Button {
                        set_label: &i18n("Add"),
                        set_valign: gtk::Align::Start,
                        connect_clicked => AccountsInput::AddLocal,
                    },
                },

                #[name = "goa_group"]
                add = &adw::PreferencesGroup {
                    set_title: &i18n("GNOME Online Accounts"),
                    set_description: Some(
                        i18n("Mail accounts from GNOME Settings. Switch one on to use it.").as_str()
                    ),
                    set_visible: false,

                    #[name = "goa_list"]
                    gtk::ListBox {
                        add_css_class: "boxed-list",
                        set_selection_mode: gtk::SelectionMode::None,
                    },
                },
            }
        }
        Self {
            page,
            goa_refresh_btn,
            accounts_list,
            goa_group,
            local_group,
            goa_list,
        }
    }
}
