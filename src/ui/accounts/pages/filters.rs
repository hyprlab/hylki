//! Settings → Filters: the search box, the rules and Apply Now.

use adw::prelude::*;
use relm4::prelude::*;

use super::super::{AccountsInput, AccountsWindow};
use crate::i18n::i18n;

/// The page, and the widgets on it that the panel reaches later.
#[derive(Debug)]
pub(in crate::ui::accounts) struct FiltersPage {
    pub(in crate::ui::accounts) page: adw::PreferencesPage,
    pub(in crate::ui::accounts) apply_filters_spinner: gtk::Spinner,
    pub(in crate::ui::accounts) apply_filters_label: gtk::Label,
    pub(in crate::ui::accounts) filters_list: gtk::ListBox,
}

impl FiltersPage {
    pub(in crate::ui::accounts) fn build(sender: &ComponentSender<AccountsWindow>) -> Self {
        relm4::view! {
            page = adw::PreferencesPage {
                // A search box over the long lists (#141): rows
                // that do not match the text are hidden.
                add = &adw::PreferencesGroup {
                    #[name = "filters_search"]
                    gtk::SearchEntry {
                        set_placeholder_text: Some(i18n("Search filters").as_str()),
                        connect_search_changed[sender] => move |entry| {
                            sender.input_sender().emit(AccountsInput::SearchFilters(entry.text().to_string()));
                        },
                    },
                },

                // Mail hygiene (moved from Settings): filters,
                // the remote-content allow list, the blocklist.
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Filters"),
                    set_description: Some(
                        i18n("File or tag incoming mail automatically. Filters run on each Inbox as it syncs; Apply Now runs them over the mail already there.").as_str()
                    ),
                    // At the header's end, like the other
                    // panels' buttons.
                    #[wrap(Some)]
                    set_header_suffix = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 6,
                        set_valign: gtk::Align::Start,
                        set_halign: gtk::Align::End,
                        set_margin_start: 24,
                        gtk::Button {
                            set_label: &i18n("Add Filter…"),
                            set_size_request: (130, -1),
                            connect_clicked => AccountsInput::AddFilter,
                        },
                        // Rules normally meet mail as it
                        // arrives; this holds them up against
                        // what is already in the Inbox (#198).
                        #[name = "apply_filters_btn"]
                        gtk::Button {
                            set_size_request: (130, -1),
                            set_tooltip_text: Some(
                                i18n("Run every filter over the mail already in your inboxes").as_str()
                            ),
                            connect_clicked => AccountsInput::ApplyFilters,
                            // A spinner inside the button
                            // while the run is under way, as
                            // the tag finder has.
                            gtk::Box {
                                set_spacing: 6,
                                set_halign: gtk::Align::Center,
                                #[name = "apply_filters_spinner"]
                                gtk::Spinner {
                                    set_visible: false,
                                },
                                #[name = "apply_filters_label"]
                                gtk::Label {
                                    set_label: &i18n("Apply Now"),
                                },
                            },
                        },
                    },

                    #[name = "filters_list"]
                    gtk::ListBox {
                        add_css_class: "boxed-list",
                        set_selection_mode: gtk::SelectionMode::None,
                    },
                },
            }
        }
        Self {
            page,
            apply_filters_spinner,
            apply_filters_label,
            filters_list,
        }
    }
}
