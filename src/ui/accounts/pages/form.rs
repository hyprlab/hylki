//! The account editor's form: the account and its servers, appearance,
//! aliases, signatures, syncing, Microsoft 365, folders and OpenPGP.
//! Filled and read by `form`, wired by `setup`.

use adw::prelude::*;
use relm4::prelude::*;

use super::super::form::{REPLY_COPIES_HINT, SENT_COPY_HINT};
use super::super::{AccountsInput, AccountsWindow};
use crate::i18n::i18n;

/// The page, and the widgets on it that the panel reaches later.
#[derive(Debug)]
pub(in crate::ui::accounts) struct EditorForm {
    pub(in crate::ui::accounts) page: adw::PreferencesPage,
    pub(in crate::ui::accounts) provider_mark: gtk::Box,
    pub(in crate::ui::accounts) goa_banner: adw::PreferencesGroup,
    pub(in crate::ui::accounts) goa_enabled_row: adw::SwitchRow,
    pub(in crate::ui::accounts) quota_group: adw::PreferencesGroup,
    pub(in crate::ui::accounts) quota_row: adw::ActionRow,
    pub(in crate::ui::accounts) quota_bar: gtk::LevelBar,
    pub(in crate::ui::accounts) label_row: adw::EntryRow,
    pub(in crate::ui::accounts) provider_row: adw::ComboRow,
    pub(in crate::ui::accounts) name_row: adw::EntryRow,
    pub(in crate::ui::accounts) email_row: adw::EntryRow,
    pub(in crate::ui::accounts) protocol_row: adw::ComboRow,
    pub(in crate::ui::accounts) host_row: adw::EntryRow,
    pub(in crate::ui::accounts) port_row: adw::EntryRow,
    pub(in crate::ui::accounts) smtp_row: adw::EntryRow,
    pub(in crate::ui::accounts) smtp_port_row: adw::EntryRow,
    pub(in crate::ui::accounts) user_row: adw::EntryRow,
    pub(in crate::ui::accounts) pass_row: adw::PasswordEntryRow,
    pub(in crate::ui::accounts) jmap_token_row: adw::SwitchRow,
    pub(in crate::ui::accounts) oauth_client_id_row: adw::EntryRow,
    pub(in crate::ui::accounts) oauth_secret_row: adw::PasswordEntryRow,
    pub(in crate::ui::accounts) oauth_auth_url_row: adw::EntryRow,
    pub(in crate::ui::accounts) oauth_token_url_row: adw::EntryRow,
    pub(in crate::ui::accounts) oauth_scope_row: adw::EntryRow,
    pub(in crate::ui::accounts) ms_advanced: adw::ExpanderRow,
    pub(in crate::ui::accounts) ms_client_row: adw::EntryRow,
    pub(in crate::ui::accounts) ms_tenant_row: adw::EntryRow,
    pub(in crate::ui::accounts) ms_scopes_row: adw::EntryRow,
    pub(in crate::ui::accounts) ms_redirect_row: adw::EntryRow,
    pub(in crate::ui::accounts) oauth_signin_btn: gtk::Button,
    pub(in crate::ui::accounts) oauth_status: gtk::Label,
    pub(in crate::ui::accounts) goa_hint: gtk::Box,
    pub(in crate::ui::accounts) smtp_separate_row: adw::SwitchRow,
    pub(in crate::ui::accounts) smtp_user_row: adw::EntryRow,
    pub(in crate::ui::accounts) smtp_pass_row: adw::PasswordEntryRow,
    pub(in crate::ui::accounts) tls_mismatch_row: adw::SwitchRow,
    pub(in crate::ui::accounts) in_unified_row: adw::SwitchRow,
    pub(in crate::ui::accounts) test_btn: gtk::Button,
    pub(in crate::ui::accounts) test_result: gtk::Label,
    pub(in crate::ui::accounts) preview_disc: gtk::Box,
    pub(in crate::ui::accounts) color_btn: gtk::ColorDialogButton,
    pub(in crate::ui::accounts) gravatar_row: adw::SwitchRow,
    pub(in crate::ui::accounts) mode_glyph_btn: gtk::ToggleButton,
    pub(in crate::ui::accounts) mode_picture_btn: gtk::ToggleButton,
    pub(in crate::ui::accounts) emoji_row: adw::ActionRow,
    pub(in crate::ui::accounts) emoji_clear_btn: gtk::Button,
    pub(in crate::ui::accounts) picture_row: adw::ActionRow,
    pub(in crate::ui::accounts) avatar_clear_btn: gtk::Button,
    pub(in crate::ui::accounts) aliases_list: gtk::ListBox,
    pub(in crate::ui::accounts) sig_choice_row: adw::ComboRow,
    pub(in crate::ui::accounts) sig_name_row: adw::EntryRow,
    pub(in crate::ui::accounts) sig_default_row: adw::SwitchRow,
    pub(in crate::ui::accounts) sig_holder: gtk::Box,
    pub(in crate::ui::accounts) push_row: adw::ComboRow,
    pub(in crate::ui::accounts) empty_junk_row: adw::ComboRow,
    pub(in crate::ui::accounts) empty_trash_row: adw::ComboRow,
    pub(in crate::ui::accounts) clean_lists_row: adw::ComboRow,
    pub(in crate::ui::accounts) offline_mail_row: adw::ComboRow,
    pub(in crate::ui::accounts) offline_attachments_row: adw::SwitchRow,
    pub(in crate::ui::accounts) graph_group: adw::PreferencesGroup,
    pub(in crate::ui::accounts) graph_directory_row: adw::SwitchRow,
    pub(in crate::ui::accounts) directory_only_row: adw::SwitchRow,
    pub(in crate::ui::accounts) graph_labels_row: adw::SwitchRow,
    pub(in crate::ui::accounts) graph_status: gtk::Label,
    pub(in crate::ui::accounts) folder_sort_row: adw::ComboRow,
    pub(in crate::ui::accounts) folder_sent_row: adw::ComboRow,
    pub(in crate::ui::accounts) folder_sent_copy_row: adw::ComboRow,
    pub(in crate::ui::accounts) reply_copies_row: adw::ComboRow,
    pub(in crate::ui::accounts) server_saves_row: adw::SwitchRow,
    pub(in crate::ui::accounts) folder_drafts_row: adw::ComboRow,
    pub(in crate::ui::accounts) folder_templates_row: adw::ComboRow,
    pub(in crate::ui::accounts) folder_trash_row: adw::ComboRow,
    pub(in crate::ui::accounts) folder_junk_row: adw::ComboRow,
    pub(in crate::ui::accounts) folder_archive_row: adw::ComboRow,
    pub(in crate::ui::accounts) folder_snoozed_row: adw::ComboRow,
    pub(in crate::ui::accounts) hidden_list: gtk::ListBox,
    pub(in crate::ui::accounts) hidden_empty: gtk::Label,
    pub(in crate::ui::accounts) pgp_key_row: adw::ComboRow,
    pub(in crate::ui::accounts) sign_default_row: adw::SwitchRow,
    pub(in crate::ui::accounts) hide_subject_row: adw::SwitchRow,
    pub(in crate::ui::accounts) keyring_note: gtk::Label,
}

impl EditorForm {
    pub(in crate::ui::accounts) fn build(sender: &ComponentSender<AccountsWindow>) -> Self {
        relm4::view! {
            page = adw::PreferencesPage {
                // GNOME Online Accounts owns this account's servers and
                // credentials; Hylki only mirrors them. Saying so where
                // the greyed-out fields are is worth more than leaving
                // the user to work out why they can't type.
                // GNOME Online Accounts owns this account's servers and
                // credentials; Hylki only mirrors them, and can hide it
                // locally. Both facts belong together, above the fields
                // they explain.
                // The provider's mark over the form, following
                // the Provider picker (a protocol tile for manual
                // IMAP and custom OAuth). Filled in apply_provider.
                add = &adw::PreferencesGroup {
                    #[name = "provider_mark"]
                    gtk::Box {
                        set_halign: gtk::Align::Center,
                        set_height_request: 56,
                        set_margin_bottom: 6,
                    },
                },

                #[name = "goa_banner"]
                add = &adw::PreferencesGroup {
                    set_visible: false,
                    set_title: &i18n("GNOME Online Account"),

                    #[name = "goa_enabled_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Show in Hylki"),
                        set_subtitle: &i18n("Off returns it to the import list. It stays in GNOME Online Accounts."),
                        connect_active_notify[sender] => move |row| {
                            sender.input_sender().emit(AccountsInput::ToggleCurrentEnabled(row.is_active()));
                        },
                    },

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 12,
                        set_halign: gtk::Align::Start,
                        set_margin_top: 12,

                        gtk::Label {
                            set_label: &i18n("Managed by GNOME Online Accounts. Its address, servers and password are changed in Settings \u{2192} Online Accounts."),
                            set_xalign: 0.0,
                            set_halign: gtk::Align::Start,
                            set_wrap: true,
                            add_css_class: "dim-label",
                        },

                        gtk::Button {
                            set_label: &i18n("Open Online Accounts\u{2026}"),
                            set_halign: gtk::Align::Start,
                            connect_clicked => AccountsInput::OpenOnlineAccounts,
                        },
                    },
                },

                // The mailbox's storage (#298): shown once the
                // server has said how much is in use, never for
                // one that keeps no limit or cannot say.
                #[name = "quota_group"]
                add = &adw::PreferencesGroup {
                    set_visible: false,

                    #[name = "quota_row"]
                    adw::ActionRow {
                        set_title: &i18n("Storage"),
                        add_css_class: "property",

                        #[name = "quota_bar"]
                        add_suffix = &gtk::LevelBar {
                            set_valign: gtk::Align::Center,
                            set_width_request: 160,
                            set_min_value: 0.0,
                            set_max_value: 1.0,
                        },
                    },
                },

                add = &adw::PreferencesGroup {
                    set_title: &i18n("Mail Account"),

                    // The nickname first: it is the one thing
                    // most edits are for, and it names the
                    // account wherever it is listed, the address
                    // standing in while there is none. Where it
                    // shows up is said in the title, the way the
                    // other rows carry their hints.
                    #[name = "label_row"]
                    adw::EntryRow {
                        set_title: &i18n("Nickname"),
                    },

                    // Then the provider; the rest of the form
                    // adapts (server fields vs. OAuth sign-in).
                    #[name = "provider_row"]
                    adw::ComboRow {
                        set_title: &i18n("Provider"),
                        connect_selected_notify => AccountsInput::ProviderChanged,
                    },
                    #[name = "name_row"]
                    adw::EntryRow { set_title: &i18n("Display Name") },
                    #[name = "email_row"]
                    adw::EntryRow {
                        set_title: &i18n("Email Address"),
                        set_input_purpose: gtk::InputPurpose::Email,
                    },
                    #[name = "protocol_row"]
                    adw::ComboRow {
                        set_title: &i18n("Incoming Protocol"),
                        connect_selected_notify => AccountsInput::ProtocolChanged,
                    },
                    #[name = "host_row"]
                    adw::EntryRow { set_title: &i18n("Incoming Server") },
                    #[name = "port_row"]
                    adw::EntryRow {
                        set_title: &i18n("Port (IMAP 993 / POP3 995)"),
                        set_input_purpose: gtk::InputPurpose::Digits,
                    },
                    #[name = "smtp_row"]
                    adw::EntryRow { set_title: &i18n("SMTP Server (optional)") },
                    #[name = "smtp_port_row"]
                    adw::EntryRow {
                        set_title: &i18n("SMTP Port (default 587)"),
                        set_input_purpose: gtk::InputPurpose::Digits,
                    },
                    #[name = "user_row"]
                    adw::EntryRow { set_title: &i18n("Username") },
                    #[name = "pass_row"]
                    adw::PasswordEntryRow { set_title: &i18n("Password") },
                    // JMAP with a bearer token in place of the
                    // username and password (#356).
                    #[name = "jmap_token_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Sign in with an API token"),
                        set_subtitle: &i18n("For servers, like Fastmail, that take a token instead of a password."),
                        set_visible: false,
                        connect_active_notify => AccountsInput::ProtocolChanged,
                    },

                    // ---- OAuth fields (shown when Authentication is an OAuth option) ----
                    #[name = "oauth_client_id_row"]
                    adw::EntryRow {
                        set_title: &i18n("OAuth Client ID"),
                        set_visible: false,
                    },
                    #[name = "oauth_secret_row"]
                    adw::PasswordEntryRow {
                        set_title: &i18n("OAuth Client Secret (optional)"),
                        set_visible: false,
                    },
                    #[name = "oauth_auth_url_row"]
                    adw::EntryRow {
                        set_title: &i18n("Authorization URL"),
                        set_visible: false,
                    },
                    #[name = "oauth_token_url_row"]
                    adw::EntryRow {
                        set_title: &i18n("Token URL"),
                        set_visible: false,
                    },
                    #[name = "oauth_scope_row"]
                    adw::EntryRow {
                        set_title: &i18n("Scopes (space-separated)"),
                        set_visible: false,
                    },
                    // Microsoft's own sign-in, made with another app
                    // registration (#329): one an organization has
                    // approved, say. Empty rows keep Hylki's.
                    #[name = "ms_advanced"]
                    adw::ExpanderRow {
                        set_title: &i18n("Advanced"),
                        set_subtitle: &i18n("Sign in with another app registration"),
                        set_visible: false,
                        add_row: ms_client_row = &adw::EntryRow {
                            set_title: &i18n("Client ID"),
                        },
                        add_row: ms_tenant_row = &adw::EntryRow {
                            set_title: &i18n("Tenant (common, organizations, consumers or an ID)"),
                        },
                        add_row: ms_scopes_row = &adw::EntryRow {
                            set_title: &i18n("Scopes (space-separated)"),
                        },
                        add_row: ms_redirect_row = &adw::EntryRow {
                            set_title: &i18n("Redirect URI"),
                            // A redirect of its own signs in in
                            // Hylki's window, not the browser.
                            connect_changed[signin = oauth_signin_btn.clone()] => move |row| {
                                signin.set_label(&if row.text().trim().is_empty() {
                                    i18n("Sign In with Browser")
                                } else {
                                    i18n("Sign In")
                                });
                            },
                        },
                    },
                    #[name = "oauth_signin_btn"]
                    gtk::Button {
                        set_label: &i18n("Sign In with Browser"),
                        set_halign: gtk::Align::Start,
                        set_margin_top: 16,
                        set_visible: false,
                        add_css_class: "suggested-action",
                        connect_clicked => AccountsInput::OAuthSignIn,
                    },
                    #[name = "oauth_status"]
                    gtk::Label {
                        set_visible: false,
                        set_halign: gtk::Align::Start,
                        set_xalign: 0.0,
                        set_wrap: true,
                    },

                    // Shown for Google/Microsoft when no built-in/own OAuth
                    // client is available: point the user at GNOME Online
                    // Accounts (the only sign-in path for these providers).
                    #[name = "goa_hint"]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 12,
                        set_margin_top: 8,
                        set_visible: false,

                        gtk::Label {
                            set_wrap: true,
                            set_xalign: 0.0,
                            add_css_class: "dim-label",
                            set_label: &i18n("Google and Microsoft sign in through GNOME Online Accounts. Sign in there, then reopen this window and switch the account on under GNOME Online Accounts."),
                        },
                        gtk::Button {
                            set_label: &i18n("Open Online Accounts…"),
                            set_halign: gtk::Align::Start,
                            add_css_class: "suggested-action",
                            connect_clicked => AccountsInput::OpenOnlineAccounts,
                        },
                    },

                    #[name = "smtp_separate_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Separate SMTP credentials"),
                        set_subtitle: &i18n("Off uses the credentials above."),
                    },
                    #[name = "smtp_user_row"]
                    adw::EntryRow { set_title: &i18n("SMTP Username") },
                    #[name = "smtp_pass_row"]
                    adw::PasswordEntryRow { set_title: &i18n("SMTP Password") },
                    #[name = "tls_mismatch_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Accept a certificate for another name"),
                        set_subtitle: &i18n("For shared hosting. Turn on only when the connection test says the names differ."),
                    },

                    // Off keeps a mailbox read on its own out of
                    // the merged lists (#267); its own section,
                    // the tray and notifications are unchanged.
                    #[name = "in_unified_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Show in All Inboxes"),
                        set_subtitle: &i18n("Also in the unified Starred, Sent, Drafts, Archive, Filters and Tags."),
                        set_active: true,
                    },

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 6,
                        set_margin_top: 16,

                        #[name = "test_btn"]
                        gtk::Button {
                            set_label: &i18n("Test Connection"),
                            set_halign: gtk::Align::Start,
                            connect_clicked => AccountsInput::TestConnection,
                        },
                        #[name = "test_result"]
                        gtk::Label {
                            set_visible: false,
                            set_halign: gtk::Align::Start,
                            set_xalign: 0.0,
                            set_wrap: true,
                            // The text can be copied out (#246):
                            // an error is often something to
                            // paste into a bug report.
                            set_selectable: true,
                        },
                    },
                },

                add = &adw::PreferencesGroup {
                    set_title: &i18n("Appearance"),

                    // The sidebar circle as it will look, as the
                    // group's first row (a non-row child would
                    // land below the rows): the accent color,
                    // and the picture, emoji or initials it
                    // shows. Not a button — it neither activates
                    // nor selects.
                    adw::PreferencesRow {
                        set_activatable: false,
                        set_selectable: false,
                        set_can_focus: false,
                        #[wrap(Some)]
                        set_child = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 2,
                            set_halign: gtk::Align::Center,
                            set_margin_top: 10,
                            set_margin_bottom: 10,
                            set_margin_start: 10,
                            set_margin_end: 10,

                            #[name = "preview_disc"]
                            gtk::Box {
                                add_css_class: "account-circle",
                                add_css_class: "account-preview-disc",
                                set_size_request: (72, 72),
                                set_halign: gtk::Align::Center,
                                set_hexpand: false,
                                set_overflow: gtk::Overflow::Hidden,
                            },
                        },
                    },

                    // The accent stands on its own: it colors
                    // the circle, and marks the account's
                    // folders and mail elsewhere.
                    adw::ActionRow {
                        set_title: &i18n("Account accent color"),
                        #[name = "color_btn"]
                        add_suffix = &gtk::ColorDialogButton {
                            set_valign: gtk::Align::Center,
                            set_dialog: &gtk::ColorDialog::new(),
                            connect_rgba_notify[sender] => move |_| {
                                sender.input_sender().emit(AccountsInput::RefreshPreview);
                            },
                        },
                    },

                    // This mailbox's own Gravatar (#189), ahead
                    // of everything below — when the address has
                    // one. Off by default: the lookup tells a
                    // third party the address is in use here.
                    #[name = "gravatar_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Use my Gravatar"),
                        set_subtitle: &i18n("Sends gravatar.com a hash of the address."),
                        connect_active_notify[sender] => move |row| {
                            sender.input_sender().emit(AccountsInput::SetOwnGravatar(row.is_active()));
                        },
                    },

                    // What the circle shows (#162): initials or
                    // an emoji, or a picture. The row below
                    // follows the choice.
                    adw::ActionRow {
                        set_title: &i18n("Circle shows"),
                        add_suffix = &gtk::Box {
                            add_css_class: "linked",
                            set_valign: gtk::Align::Center,
                            #[name = "mode_glyph_btn"]
                            gtk::ToggleButton {
                                set_label: &i18n("Initials or emoji"),
                                set_active: true,
                            },
                            #[name = "mode_picture_btn"]
                            gtk::ToggleButton {
                                set_label: &i18n("Picture"),
                                set_group: Some(&mode_glyph_btn),
                                connect_toggled[sender] => move |b| {
                                    sender.input_sender().emit(AccountsInput::SetPictureMode(b.is_active()));
                                },
                            },
                        },
                    },

                    #[name = "emoji_row"]
                    adw::ActionRow {
                        set_title: &i18n("Emoji"),
                        set_subtitle: &i18n("Instead of initials."),

                        #[name = "emoji_btn"]
                        add_suffix = &gtk::MenuButton {
                            set_valign: gtk::Align::Center,
                            set_label: &i18n("Choose…"),
                            #[wrap(Some)]
                            set_popover = &gtk::EmojiChooser {
                                connect_emoji_picked[sender] => move |_, text| {
                                    sender.input_sender().emit(AccountsInput::SetEmoji(text.to_string()));
                                },
                            },
                        },
                        #[name = "emoji_clear_btn"]
                        add_suffix = &gtk::Button {
                            set_valign: gtk::Align::Center,
                            set_label: &i18n("Use initials"),
                            set_visible: false,
                            connect_clicked => AccountsInput::ClearGlyph,
                        },
                    },

                    #[name = "picture_row"]
                    adw::ActionRow {
                        set_title: &i18n("Picture"),
                        set_visible: false,
                        add_suffix = &gtk::Button {
                            set_valign: gtk::Align::Center,
                            set_label: &i18n("Choose…"),
                            connect_clicked => AccountsInput::PickAvatar,
                        },
                        #[name = "avatar_clear_btn"]
                        add_suffix = &gtk::Button {
                            set_valign: gtk::Align::Center,
                            set_label: &i18n("Remove"),
                            set_visible: false,
                            connect_clicked => AccountsInput::ClearGlyph,
                        },
                    },
                },

                // Send-as aliases (#34): extra From identities the
                // composer offers, and replies to an alias answer
                // from it. Each alias sends through this account's
                // SMTP, or — for a forwarded mailbox whose provider
                // would rewrite the sender — through its own.
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Send-as aliases"),
                    set_description: Some(
                        i18n("Extra addresses this account can send as.").as_str()
                    ),

                    // At the header's end, styled like the Add
                    // Filter…, Add Tag… and cloud Add Account…
                    // buttons.
                    #[wrap(Some)]
                    set_header_suffix = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_valign: gtk::Align::Start,
                        set_halign: gtk::Align::End,
                        set_margin_start: 24,
                        gtk::Button {
                            set_label: &i18n("Add Alias…"),
                            set_size_request: (130, -1),
                            connect_clicked => AccountsInput::AliasAdd,
                        },
                    },

                    #[name = "aliases_list"]
                    gtk::ListBox {
                        add_css_class: "boxed-list",
                        set_selection_mode: gtk::SelectionMode::None,
                        connect_row_activated[sender] => move |_, row| {
                            sender.input_sender().emit(AccountsInput::AliasEdit(row.index() as usize));
                        },
                    },
                },

                add = &adw::PreferencesGroup {
                    set_title: &i18n("Signatures"),
                    set_description: Some(
                        i18n("With more than one, the composer's signature button allows you to pick between them.")
                            .as_str()
                    ),
                    // A designed signature usually exists as HTML
                    // already (#120): bring it in from its file.
                    // Its source is edited in place, from the
                    // editor's own toolbar. Stacked and 130px
                    // wide, like Add Alias… and the other
                    // groups' buttons.
                    #[wrap(Some)]
                    set_header_suffix = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 6,
                        set_valign: gtk::Align::Start,
                        set_halign: gtk::Align::End,
                        set_margin_start: 24,
                        gtk::Button {
                            set_label: &i18n("Add Signature"),
                            set_size_request: (130, -1),
                            connect_clicked => AccountsInput::SigAdd,
                        },
                        gtk::Button {
                            set_label: &i18n("Import File…"),
                            set_size_request: (130, -1),
                            connect_clicked => AccountsInput::SignatureImport,
                        },
                    },

                    // Which signature the editor below holds,
                    // and its name: shown once there are two.
                    #[name = "sig_choice_row"]
                    adw::ComboRow {
                        set_title: &i18n("Signature"),
                        set_visible: false,
                        connect_selected_notify => AccountsInput::SigShow,
                    },
                    #[name = "sig_name_row"]
                    adw::EntryRow {
                        set_title: &i18n("Name"),
                        set_visible: false,
                        connect_changed => AccountsInput::SigRename,
                        add_suffix = &gtk::Button {
                            set_icon_name: "user-trash-symbolic",
                            set_valign: gtk::Align::Center,
                            set_tooltip_text: Some(&i18n("Delete Signature")),
                            add_css_class: "flat",
                            connect_clicked => AccountsInput::SigDelete,
                        },
                    },
                    #[name = "sig_default_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Start messages with this signature"),
                        connect_active_notify => AccountsInput::SigDefault,
                    },

                    #[name = "sig_holder"]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_margin_top: 6,
                    },
                },

                // Per-account push override (#91): some servers
                // mishandle IDLE, and one bad account shouldn't
                // cost the good ones their instant delivery.
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Syncing"),

                    #[name = "push_row"]
                    adw::ComboRow {
                        set_title: &i18n("Instant new mail (push)"),
                        set_subtitle: &i18n("Turn off for servers that stall on push connections."),
                    },

                    // Auto-empty (#140): mail past the chosen age in
                    // Junk / Trash is deleted for good at each sync.
                    #[name = "empty_junk_row"]
                    adw::ComboRow {
                        set_title: &i18n("Empty Junk automatically"),
                    },
                    #[name = "empty_trash_row"]
                    adw::ComboRow {
                        set_title: &i18n("Empty Trash automatically"),
                    },
                    // Mailing-list mail past the chosen age goes
                    // to the Trash (#407).
                    #[name = "clean_lists_row"]
                    adw::ComboRow {
                        set_title: &i18n("Move list mail to Trash"),
                        set_subtitle: &i18n("Inbox mail with an unsubscribe link older than this gets trashed unless it's starred."),
                    },
                    // Mail downloaded ahead of being opened (#536).
                    #[name = "offline_mail_row"]
                    adw::ComboRow {
                        set_title: &i18n("Keep mail offline"),
                        set_subtitle: &i18n("Leaves out Trash, Junk and Drafts."),
                    },
                    #[name = "offline_attachments_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Include attachments"),
                        set_subtitle: &i18n("Can consume a lot of hard drive space."),
                    },
                },

                // What a Microsoft 365 account can use from
                // Microsoft Graph, in one place: the
                // directory's suggestions (#410) and names for
                // sensitivity labels (#411). Each asks for its
                // own access when turned on.
                #[name = "graph_group"]
                add = &adw::PreferencesGroup {
                    set_visible: false,
                    set_title: &i18n("Microsoft 365"),

                    #[name = "graph_directory_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Search the organization directory"),
                        set_subtitle: &i18n("Suggests people from your organization as you type."),
                        connect_active_notify[sender] => move |row| {
                            sender.input_sender().emit(AccountsInput::SetGraphDirectory(row.is_active()));
                        },
                    },
                    #[name = "directory_only_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Suggest only from this account"),
                        set_subtitle: &i18n("Suggestions come from the directory and this account's mail only."),
                    },
                    #[name = "graph_labels_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Show sensitivity label names"),
                        set_subtitle: &i18n("Shows a label's name when a message gives only its ID."),
                        connect_active_notify[sender] => move |row| {
                            sender.input_sender().emit(AccountsInput::SetGraphLabels(row.is_active()));
                        },
                    },
                    // How the last request for access went.
                    #[name = "graph_status"]
                    gtk::Label {
                        set_visible: false,
                        set_halign: gtk::Align::Start,
                        set_xalign: 0.0,
                        set_wrap: true,
                        set_selectable: true,
                        set_margin_top: 8,
                    },
                },

                // Its own folder order, or Settings → Sidebar's.
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Folder Order"),
                    set_description: Some(
                        i18n("Also in the right-click menu of its Folders heading.").as_str()
                    ),

                    #[name = "folder_sort_row"]
                    adw::ComboRow {
                        set_title: &i18n("Sort by"),
                    },
                },

                // Manual special-folder mapping (#82): for servers
                // whose Sent/Trash/… aren't detected, pin each role
                // to one of the account's real folders. "Save
                // copies in" (#199) rides with them because it is
                // read as a refinement of Sent, but it is a
                // different kind of thing: a role says what a
                // folder *is*, and relabelling the Inbox costs the
                // account its inbox (#136), which is why the Inbox
                // is offered in that row and in none of the others.
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Special Folders"),
                    set_description: Some(
                        i18n("Follows the server's own markings unless you pick a folder. Nothing already on the server moves.").as_str()
                    ),

                    #[name = "folder_sent_row"]
                    adw::ComboRow { set_title: &i18n("Sent") },
                    #[name = "folder_sent_copy_row"]
                    adw::ComboRow {
                        set_title: &i18n("Save a copy of sent mail in"),
                        set_subtitle: &i18n(SENT_COPY_HINT),
                    },
                    // Replies beside what they answer (#468), as
                    // Evolution offers. Microsoft 365 files its
                    // own copy and POP3 has no folders, so the
                    // row is for IMAP and JMAP only.
                    #[name = "reply_copies_row"]
                    adw::ComboRow {
                        set_title: &i18n("Save a copy of replies in"),
                        set_subtitle: &i18n(REPLY_COPIES_HINT),
                    },
                    // Gmail files a copy of anything sent through
                    // its SMTP, so Hylki's append makes a second
                    // one. Off by default: a server that does not
                    // do it, paired with a Hylki that has stopped
                    // appending, keeps no sent mail at all.
                    #[name = "server_saves_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Server saves its own copy of sent mail"),
                        set_subtitle: &i18n("For Gmail and others that file sent mail themselves."),
                        connect_active_notify[sender] => move |row| {
                            sender.input_sender().emit(AccountsInput::SetServerSavesSent(row.is_active()));
                        },
                    },
                    #[name = "folder_drafts_row"]
                    adw::ComboRow { set_title: &i18n("Drafts") },
                    // Nothing on a server marks a Templates folder
                    // (#360): one called Templates is found, any
                    // other is pointed at here.
                    #[name = "folder_templates_row"]
                    adw::ComboRow { set_title: &i18n("Templates") },
                    #[name = "folder_trash_row"]
                    adw::ComboRow { set_title: &i18n("Trash") },
                    #[name = "folder_junk_row"]
                    adw::ComboRow { set_title: &i18n("Junk") },
                    #[name = "folder_archive_row"]
                    adw::ComboRow { set_title: &i18n("Archive") },
                    // Snoozed mail waits here (#527), found by name
                    // like Templates.
                    #[name = "folder_snoozed_row"]
                    adw::ComboRow { set_title: &i18n("Snoozed") },
                },

                // Hidden folders (#239): what "Hide Folder" in the
                // sidebar took out of sight, each with the way back.
                #[name = "hidden_group"]
                adw::PreferencesGroup {
                    set_title: &i18n("Hidden Folders"),
                    set_description: Some(
                        i18n("Hidden folders are not synced. Hide one from its right-click menu in the sidebar.").as_str()
                    ),
                    #[name = "hidden_list"]
                    gtk::ListBox {
                        add_css_class: "boxed-list",
                        set_selection_mode: gtk::SelectionMode::None,
                    },
                    #[name = "hidden_empty"]
                    gtk::Label {
                        set_label: &i18n("No hidden folders."),
                        add_css_class: "dim-label",
                        set_xalign: 0.0,
                    },
                },

                // OpenPGP (#133): which of the user's keys this
                // account signs and decrypts with.
                add = &adw::PreferencesGroup {
                    set_title: &i18n("OpenPGP"),
                    set_description: Some(
                        i18n("Keys are managed under OpenPGP in the sidebar.").as_str()
                    ),
                    #[name = "pgp_key_row"]
                    adw::ComboRow {
                        set_title: &i18n("Key"),
                        set_subtitle: &i18n("Automatic uses the key whose address matches."),
                    },
                    // For an account whose mail is always signed
                    // (#267): the composer's toggle starts on,
                    // and still turns it off for one message.
                    #[name = "sign_default_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Sign messages by default"),
                    },
                    // Protected headers (#493): on, an encrypted
                    // message says "..." where its subject would be.
                    #[name = "hide_subject_row"]
                    adw::SwitchRow {
                        set_title: &i18n("Hide the subject of encrypted mail"),
                        set_subtitle: &i18n("Only the recipients see the subject. Others see \"...\"."),
                    },
                },

                // For a GOA-imported account this removes it from
                // Hylki only — it stays in GNOME Online Accounts and
                // returns to the import list.
                add = &adw::PreferencesGroup {
                    // Not for a GNOME Online Account: GNOME holds its
                    // sign-in, and Google and Microsoft have no password.
                    #[name = "keyring_note"]
                    gtk::Label {
                        set_wrap: true,
                        set_xalign: 0.0,
                        add_css_class: "dim-label",
                        add_css_class: "caption",
                        set_label: &i18n("Your password is kept in the system keyring, never in plain text."),
                    },
                },
            }
        }
        Self {
            page,
            provider_mark,
            goa_banner,
            goa_enabled_row,
            quota_group,
            quota_row,
            quota_bar,
            label_row,
            provider_row,
            name_row,
            email_row,
            protocol_row,
            host_row,
            port_row,
            smtp_row,
            smtp_port_row,
            user_row,
            pass_row,
            jmap_token_row,
            oauth_client_id_row,
            oauth_secret_row,
            oauth_auth_url_row,
            oauth_token_url_row,
            oauth_scope_row,
            ms_advanced,
            ms_client_row,
            ms_tenant_row,
            ms_scopes_row,
            ms_redirect_row,
            oauth_signin_btn,
            oauth_status,
            goa_hint,
            smtp_separate_row,
            smtp_user_row,
            smtp_pass_row,
            tls_mismatch_row,
            in_unified_row,
            test_btn,
            test_result,
            preview_disc,
            color_btn,
            gravatar_row,
            mode_glyph_btn,
            mode_picture_btn,
            emoji_row,
            emoji_clear_btn,
            picture_row,
            avatar_clear_btn,
            aliases_list,
            sig_choice_row,
            sig_name_row,
            sig_default_row,
            sig_holder,
            push_row,
            empty_junk_row,
            empty_trash_row,
            clean_lists_row,
            offline_mail_row,
            offline_attachments_row,
            graph_group,
            graph_directory_row,
            directory_only_row,
            graph_labels_row,
            graph_status,
            folder_sort_row,
            folder_sent_row,
            folder_sent_copy_row,
            reply_copies_row,
            server_saves_row,
            folder_drafts_row,
            folder_templates_row,
            folder_trash_row,
            folder_junk_row,
            folder_archive_row,
            folder_snoozed_row,
            hidden_list,
            hidden_empty,
            pgp_key_row,
            sign_default_row,
            hide_subject_row,
            keyring_note,
        }
    }
}
