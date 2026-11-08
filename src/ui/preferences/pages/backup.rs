//! Settings → Backup: export and import of the settings.

use adw::prelude::*;

use super::super::{PrefInput, ViewSender};
use crate::i18n::i18n;

/// The page, and the rows on it that the window reaches later.
#[derive(Debug)]
pub(in crate::ui::preferences) struct BackupPage {
    pub(in crate::ui::preferences) page: adw::PreferencesPage,
}

impl BackupPage {
    pub(in crate::ui::preferences) fn build(sender: &ViewSender) -> Self {
        relm4::view! {
            page = adw::PreferencesPage {
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Backup"),
                    set_description: Some(
                        i18n("Accounts, preferences, filters, tags and layout, in one file. Passwords, OAuth client secrets and OpenPGP keys are not included.").as_str()
                    ),

                    adw::ActionRow {
                        set_title: &i18n("Export settings"),
                        set_activatable: true,
                        connect_activated => PrefInput::ExportSettings,
                        add_suffix = &gtk::Image {
                            set_icon_name: Some("go-next-symbolic"),
                        },
                    },

                    adw::ActionRow {
                        set_title: &i18n("Import settings"),
                        set_subtitle: &i18n("Replaces everything in place. Don't remove accounts first: that deletes their passwords."),
                        set_activatable: true,
                        connect_activated => PrefInput::ImportSettings,
                        add_suffix = &gtk::Image {
                            set_icon_name: Some("go-next-symbolic"),
                        },
                    },
                },
            }
        }
        Self { page }
    }
}
