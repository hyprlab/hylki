//! Settings → Smart Folders: the list of saved searches.

use adw::prelude::*;
use relm4::prelude::*;

use super::super::{AccountsInput, AccountsWindow};
use crate::i18n::i18n;

/// The page, and the widgets on it that the panel reaches later.
#[derive(Debug)]
pub(in crate::ui::accounts) struct SmartPage {
    pub(in crate::ui::accounts) page: adw::PreferencesPage,
    pub(in crate::ui::accounts) smart_list: gtk::ListBox,
}

impl SmartPage {
    pub(in crate::ui::accounts) fn build(sender: &ComponentSender<AccountsWindow>) -> Self {
        relm4::view! {
            page = adw::PreferencesPage {
                // Smart folders (#417): saved searches.
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Smart Folders"),
                    set_description: Some(
                        i18n("Each lists the messages that match its conditions, in the sidebar under the unified rows.").as_str()
                    ),
                    #[wrap(Some)]
                    set_header_suffix = &gtk::Button {
                        set_label: &i18n("Add Smart Folder…"),
                        set_valign: gtk::Align::Start,
                        set_margin_start: 24,
                        connect_clicked => AccountsInput::AddSmartFolder,
                    },

                    #[name = "smart_list"]
                    gtk::ListBox {
                        add_css_class: "boxed-list",
                        set_selection_mode: gtk::SelectionMode::None,
                    },
                },
            }
        }
        Self { page, smart_list }
    }
}
