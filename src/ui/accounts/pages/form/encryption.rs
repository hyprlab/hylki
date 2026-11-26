//! The account editor's OpenPGP and S/MIME groups, which the form places
//! after the folders.

use adw::prelude::*;

use crate::i18n::i18n;

pub(super) struct Encryption {
    pub(super) openpgp_group: adw::PreferencesGroup,
    pub(super) pgp_key_row: adw::ComboRow,
    pub(super) sign_default_row: adw::SwitchRow,
    pub(super) hide_subject_row: adw::SwitchRow,
    pub(super) autocrypt_row: adw::SwitchRow,
    pub(super) autocrypt_mutual_row: adw::SwitchRow,
    pub(super) smime_group: adw::PreferencesGroup,
    pub(super) smime_sign_row: adw::SwitchRow,
}

impl Encryption {
    pub(super) fn build() -> Self {
        relm4::view! {
            // OpenPGP (#133): which of the user's keys this account signs
            // and decrypts with.
            openpgp_group = adw::PreferencesGroup {
                set_title: &i18n("OpenPGP"),
                set_description: Some(
                    i18n("Keys are managed under OpenPGP in the sidebar.").as_str()
                ),
                #[name = "pgp_key_row"]
                adw::ComboRow {
                    set_title: &i18n("Key"),
                    set_subtitle: &i18n("Automatic uses the key whose address matches."),
                },
                // For an account whose mail is always signed (#267): the
                // composer's toggle starts on, and still turns it off for
                // one message.
                #[name = "sign_default_row"]
                adw::SwitchRow {
                    set_title: &i18n("Sign messages by default"),
                },
                // Protected headers (#493): on, an encrypted message says
                // "..." where its subject would be.
                #[name = "hide_subject_row"]
                adw::SwitchRow {
                    set_title: &i18n("Hide the subject of encrypted mail"),
                    set_subtitle: &i18n("Only the recipients see the subject. Others see “…”."),
                },
                // Autocrypt Level 1: the key goes out with the mail, and
                // keys that come in that way are kept apart from the
                // user's keyring.
                #[name = "autocrypt_row"]
                adw::SwitchRow {
                    set_title: &i18n("Send your key with your mail"),
                    set_subtitle: &i18n("Autocrypt. Keys others send this way are kept too."),
                },
                #[name = "autocrypt_mutual_row"]
                adw::SwitchRow {
                    set_title: &i18n("Encrypt by default"),
                    set_subtitle: &i18n("Mail to people who ask for the same is encrypted."),
                },
            },

            // S/MIME (#478): signing only, through gpgsm. On, the Send
            // menu's Sign with S/MIME starts ticked, as OpenPGP's Sign
            // does for an account that signs by default.
            smime_group = adw::PreferencesGroup {
                set_title: &i18n("S/MIME"),
                set_description: Some(
                    i18n("Certificates are managed with gpgsm.").as_str()
                ),
                #[name = "smime_sign_row"]
                adw::SwitchRow {
                    set_title: &i18n("Sign messages by default"),
                    set_subtitle: &i18n("The Send menu turns it off for one message."),
                },
            },
        }
        // A preference for encryption means nothing without the key going
        // out with the mail.
        autocrypt_row
            .bind_property("active", &autocrypt_mutual_row, "sensitive")
            .sync_create()
            .build();
        Self {
            openpgp_group,
            pgp_key_row,
            sign_default_row,
            hide_subject_row,
            autocrypt_row,
            autocrypt_mutual_row,
            smime_group,
            smime_sign_row,
        }
    }
}
