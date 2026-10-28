//! The account editor: opening it, its live preview, its folder and push
//! rows, and the secrets read from the keyring while it is open.

use super::form::{apply_protocol, form_protocol, read_account, trimmed, SENT_COPY_HINT};
use super::providers::{brand_for_account, manual_brand, provider_at, ProviderKind};
use super::*;

/// What the keyring holds for one account: its password, its own SMTP
/// password when SMTP is separate, and each alias's SMTP password (by the
/// alias address, #34). Read off the main thread when the editor opens, so
/// the Settings window never waits on the keyring.
#[derive(Debug, Clone)]
pub(crate) struct AccountSecrets {
    email: String,
    password: String,
    smtp_password: String,
    aliases: Vec<(String, String)>,
}

impl AccountSecrets {
    /// The keyring entries `acc` lacks (blocking; run off the main thread).
    fn load(acc: &AccountConfig) -> AccountSecrets {
        let password = if acc.oauth || !acc.password.is_empty() {
            acc.password.clone()
        } else {
            crate::config::load_password(&acc.email).unwrap_or_default()
        };
        let smtp_password = if acc.smtp_separate && acc.smtp_password.is_empty() {
            crate::config::load_smtp_password(&acc.email).unwrap_or_default()
        } else {
            acc.smtp_password.clone()
        };
        let aliases = acc
            .aliases
            .iter()
            .filter(|al| al.has_own_smtp() && al.smtp_password.is_empty())
            .map(|al| {
                let addr = al.address();
                let pw =
                    crate::config::load_alias_smtp_password(&acc.email, &addr).unwrap_or_default();
                (addr, pw)
            })
            .collect();
        AccountSecrets {
            email: acc.email.clone(),
            password,
            smtp_password,
            aliases,
        }
    }

    /// Whether `acc` still needs anything from the keyring.
    pub(super) fn needed(acc: &AccountConfig) -> bool {
        (!acc.oauth && acc.password.is_empty())
            || (acc.smtp_separate && acc.smtp_password.is_empty())
            || acc
                .aliases
                .iter()
                .any(|al| al.has_own_smtp() && al.smtp_password.is_empty())
    }

    /// Write the secrets into `acc` where it has none.
    pub(super) fn apply(&self, acc: &mut AccountConfig) {
        if acc.password.is_empty() {
            acc.password = self.password.clone();
        }
        if acc.smtp_password.is_empty() {
            acc.smtp_password = self.smtp_password.clone();
        }
        for al in acc.aliases.iter_mut() {
            if al.smtp_password.is_empty() {
                if let Some((_, pw)) = self.aliases.iter().find(|(a, _)| *a == al.address()) {
                    al.smtp_password = pw.clone();
                }
            }
        }
    }
}

impl AccountsWindow {
    /// Fill the editor's Special Folders combos (#82) for `acc` (None = a new
    /// account, whose folders aren't known yet): "Automatic" plus the account's
    /// live folder list, with any saved assignment selected.
    fn populate_folder_combos(
        &mut self,
        widgets: &AccountsWindowWidgets,
        acc: Option<&AccountConfig>,
    ) {
        let all: Vec<(String, String)> = acc
            .and_then(|a| self.folders_by_email.get(&a.email))
            .cloned()
            .unwrap_or_default();
        // The Inbox is never offered as a role (#136): giving it one took
        // its own role away, and the account lost its inbox.
        let choices: Vec<(String, String)> = all
            .iter()
            .filter(|(path, _)| !path.eq_ignore_ascii_case("INBOX"))
            .cloned()
            .collect();
        self.populate_sent_copy_combo(widgets, acc, &all);
        let automatic = i18n("Automatic");
        let mut labels: Vec<&str> = vec![automatic.as_str()];
        labels.extend(choices.iter().map(|(_, display)| display.as_str()));
        self.folder_paths = choices.iter().map(|(path, _)| path.clone()).collect();
        for (role, row) in [
            ("sent", &widgets.form.folder_sent_row),
            ("drafts", &widgets.form.folder_drafts_row),
            ("templates", &widgets.form.folder_templates_row),
            ("trash", &widgets.form.folder_trash_row),
            ("junk", &widgets.form.folder_junk_row),
            ("archive", &widgets.form.folder_archive_row),
            ("snoozed", &widgets.form.folder_snoozed_row),
        ] {
            row.set_model(Some(&gtk::StringList::new(&labels)));
            row.set_list_factory(Some(&non_ellipsizing_factory()));
            let selected = acc
                .and_then(|a| a.folder_roles.get(role))
                .and_then(|path| self.folder_paths.iter().position(|p| p == path))
                .map(|i| i as u32 + 1)
                .unwrap_or(0);
            row.set_selected(selected);
        }
    }

    /// Fill the "Save a copy of sent mail in" combo (#199): "Disabled" plus
    /// every one of the account's folders, with the saved choice selected. The
    /// Inbox is listed here — filing a copy somewhere does not re-label it.
    fn populate_sent_copy_combo(
        &mut self,
        widgets: &AccountsWindowWidgets,
        acc: Option<&AccountConfig>,
        all: &[(String, String)],
    ) {
        // "Disabled", not "Automatic": the rows around it auto-detect a folder
        // from the server's markings, and this one detects nothing. It is an
        // override that is either set or not, and unset means the Sent folder
        // above keeps taking the copies as it always has.
        let disabled = i18n("Disabled");
        let mut labels: Vec<&str> = vec![disabled.as_str()];
        labels.extend(all.iter().map(|(_, display)| display.as_str()));
        self.sent_copy_paths = all.iter().map(|(path, _)| path.clone()).collect();
        let row = &widgets.form.folder_sent_copy_row;
        row.set_model(Some(&gtk::StringList::new(&labels)));
        row.set_list_factory(Some(&non_ellipsizing_factory()));
        let selected = acc
            .and_then(|a| a.sent_copy_path.as_ref())
            .and_then(|path| self.sent_copy_paths.iter().position(|p| p == path))
            .map(|i| i as u32 + 1)
            .unwrap_or(0);
        row.set_selected(selected);
        // Two backends file the copy themselves, or not at all, and ignore
        // this setting — say so rather than offer a choice that does nothing.
        let unsupported = match acc.map(|a| a.protocol) {
            Some(Protocol::Graph) => Some(i18n(
                "Microsoft 365 files its own copy in Sent Items; this cannot be changed.",
            )),
            Some(Protocol::Pop3) => Some(i18n(
                "POP3 accounts have no server folders to save a copy in.",
            )),
            _ => None,
        };
        // The switch above may already have taken this row out of play.
        let server_saves = acc.is_some_and(|a| a.server_saves_sent);
        widgets.form.server_saves_row.set_active(server_saves);
        row.set_sensitive(unsupported.is_none() && !server_saves);
        row.set_subtitle(&unsupported.unwrap_or_else(|| i18n(SENT_COPY_HINT)));
        widgets.form.reply_copies_row.set_sensitive(!server_saves);
    }

    /// The "Save a copy of sent mail in" combo's current choice: a folder path,
    /// or `None` when disabled, which leaves the copy to the Sent role.
    pub(super) fn read_sent_copy_path(&self, widgets: &AccountsWindowWidgets) -> Option<String> {
        // Nothing is known about this account's folders yet (it has never
        // connected, or is switched off), so the combo holds only "Sent
        // folder" — which is not the user clearing the setting. Keep what
        // was saved rather than silently dropping it on an unrelated edit.
        if self.sent_copy_paths.is_empty() {
            return self
                .editing
                .and_then(|i| self.accounts.get(i))
                .and_then(|a| a.sent_copy_path.clone());
        }
        let sel = widgets.form.folder_sent_copy_row.selected();
        if sel == 0 {
            return None;
        }
        self.sent_copy_paths.get(sel as usize - 1).cloned()
    }

    /// The Special Folders combos' current assignments: role → folder path,
    /// omitting everything left on Automatic.
    pub(super) fn read_folder_roles(
        &self,
        widgets: &AccountsWindowWidgets,
    ) -> std::collections::BTreeMap<String, String> {
        let mut roles = std::collections::BTreeMap::new();
        for (role, row) in [
            ("sent", &widgets.form.folder_sent_row),
            ("drafts", &widgets.form.folder_drafts_row),
            ("templates", &widgets.form.folder_templates_row),
            ("trash", &widgets.form.folder_trash_row),
            ("junk", &widgets.form.folder_junk_row),
            ("archive", &widgets.form.folder_archive_row),
            ("snoozed", &widgets.form.folder_snoozed_row),
        ] {
            let sel = row.selected();
            if sel > 0 {
                if let Some(path) = self.folder_paths.get(sel as usize - 1) {
                    roles.insert(role.to_string(), path.clone());
                }
            }
        }
        roles
    }

    /// The mark over the form: a GNOME Online Account's comes from the
    /// account itself (its picker is hidden), otherwise the picker's, with
    /// a manual account's tile naming the protocol chosen below it.
    pub(super) fn refresh_provider_mark(&self, widgets: &AccountsWindowWidgets) {
        let p = provider_at(widgets.form.provider_row.selected());
        let editing_goa = self
            .editing
            .and_then(|i| self.accounts.get(i))
            .filter(|a| a.goa_id.is_some());
        let brand = editing_goa.map(brand_for_account).unwrap_or_else(|| {
            if p.kind == ProviderKind::Manual {
                manual_brand(form_protocol(widgets))
            } else {
                p.brand
            }
        });
        while let Some(child) = widgets.form.provider_mark.first_child() {
            widgets.form.provider_mark.remove(&child);
        }
        widgets
            .form
            .provider_mark
            .append(&crate::desktop::brand::mark(
                brand,
                56,
                crate::desktop::brand::GENERIC_MAIL,
            ));
    }

    /// Push is IMAP IDLE or a JMAP event stream. Microsoft 365 polls and
    /// POP3 has neither, so the row would be a switch that does nothing
    /// (#329). An account from Online Accounts keeps its saved protocol: its
    /// provider row isn't filled in, and reads as a plain IMAP account.
    pub(super) fn show_push_row(&self, widgets: &AccountsWindowWidgets) {
        let protocol = match self.editing.and_then(|i| self.accounts.get(i)) {
            Some(a) if a.goa_id.is_some() => a.protocol,
            _ => form_protocol(widgets),
        };
        widgets
            .form
            .push_row
            .set_visible(matches!(protocol, Protocol::Imap | Protocol::Jmap));
        // The list-mail search is a server search for a header, which
        // Microsoft 365 does not offer and POP3 has no folders for.
        widgets
            .form
            .clean_lists_row
            .set_visible(matches!(protocol, Protocol::Imap | Protocol::Jmap));
        // Every backend keeps a period offline (#536), but a POP3 message
        // only ever comes down whole, attachments and all.
        widgets
            .form
            .offline_attachments_row
            .set_visible(protocol != Protocol::Pop3);
        widgets
            .form
            .reply_copies_row
            .set_visible(matches!(protocol, Protocol::Imap | Protocol::Jmap));
        // GNOME Online Accounts asked Microsoft for its own scopes, and
        // Hylki cannot add the directory's to a token GNOME holds.
        let from_goa = self
            .editing
            .and_then(|i| self.accounts.get(i))
            .is_some_and(|a| a.goa_id.is_some());
        widgets
            .form
            .graph_group
            .set_visible(protocol == Protocol::Graph);
        widgets.form.graph_directory_row.set_sensitive(!from_goa);
        widgets.form.graph_labels_row.set_sensitive(!from_goa);
        widgets.form.graph_group.set_description(Some(&if from_goa {
            i18n("Only for an account added in Hylki with Microsoft 365 / Outlook as the provider, not from GNOME Online Accounts.")
        } else {
            i18n("Asks Microsoft for access. Some organizations need an administrator to approve it.")
        }));
    }

    /// A fresh editor: answers from the keyring for the last one no longer
    /// apply.
    fn new_editor_session(&mut self) {
        self.editor_session += 1;
        self.editor_secrets = None;
        self.secrets_reading = false;
        self.save_waiting = false;
    }

    /// Read `acc`'s secrets off the main thread; they arrive as
    /// [`AccountsCmd::Secrets`].
    pub(super) fn read_secrets(&mut self, acc: &AccountConfig, sender: &ComponentSender<Self>) {
        self.secrets_reading = true;
        let (acc, session) = (acc.clone(), self.editor_session);
        sender.oneshot_command(async move {
            let email = acc.email.clone();
            let secrets = tokio::task::spawn_blocking(move || AccountSecrets::load(&acc))
                .await
                .unwrap_or(AccountSecrets {
                    email,
                    password: String::new(),
                    smtp_password: String::new(),
                    aliases: Vec::new(),
                });
            AccountsCmd::Secrets(session, secrets)
        });
    }

    /// Show/hide credential rows based on the Authentication combo, and pre-fill
    /// server settings for known OAuth providers.
    /// Adapt the editor to the selected provider: show server + credential fields
    /// for password providers, the OAuth sign-in for OAuth providers, and fill in
    /// the servers for known providers.
    pub(super) fn apply_provider(&self, widgets: &AccountsWindowWidgets) {
        let p = provider_at(widgets.form.provider_row.selected());
        self.refresh_provider_mark(widgets);
        let is_password = p.is_password();
        let is_oauth = p.is_oauth();
        let is_custom = matches!(p.kind, ProviderKind::CustomOAuth);
        // Google or Microsoft with no built-in (or user-supplied) OAuth client:
        // there's nothing to sign in with, so guide the user to GNOME Online
        // Accounts instead. Microsoft always lands here since its embedded
        // client was removed (issue #36) — mail runs over GOA + Graph.
        let needs_goa = p
            .oauth_name()
            .filter(|_| matches!(p.kind, ProviderKind::Google | ProviderKind::Microsoft))
            .is_some_and(|n| {
                crate::services::oauth::provider_credentials(n)
                    .0
                    .trim()
                    .is_empty()
            });
        // Google/Microsoft servers come from the built-in preset (hidden). Custom
        // OAuth still needs its server addresses and client details entered.
        let show_servers = is_password || is_custom;

        let hint = if p.hint.is_empty() {
            String::new()
        } else {
            i18n(p.hint)
        };
        widgets.form.provider_row.set_subtitle(&hint);

        // Server/credential fields (password or Custom-OAuth manual servers).
        // JMAP is the JMAP entries' own protocol, not a choice.
        widgets
            .form
            .protocol_row
            .set_visible(is_password && p.kind != ProviderKind::Jmap);
        widgets.form.host_row.set_visible(show_servers);
        widgets.form.port_row.set_visible(show_servers);
        widgets.form.smtp_row.set_visible(show_servers);
        widgets.form.smtp_port_row.set_visible(show_servers);
        widgets.form.user_row.set_visible(is_password);
        widgets.form.pass_row.set_visible(is_password);
        widgets.form.smtp_separate_row.set_visible(is_password);
        widgets.form.tls_mismatch_row.set_visible(is_password);
        widgets.form.test_btn.set_visible(is_password);
        if is_oauth {
            widgets.form.smtp_separate_row.set_active(false);
        }

        // OAuth: the user just signs in. Google with no client falls back to the
        // GNOME Online Accounts panel, which replaces the sign-in + identity fields.
        widgets.form.name_row.set_visible(!needs_goa);
        widgets.form.email_row.set_visible(!needs_goa);
        widgets.form.goa_hint.set_visible(needs_goa);
        widgets
            .form
            .oauth_signin_btn
            .set_visible(is_oauth && !needs_goa);
        widgets.form.oauth_client_id_row.set_visible(is_custom);
        widgets.form.oauth_secret_row.set_visible(is_custom);
        widgets.form.oauth_auth_url_row.set_visible(is_custom);
        widgets.form.oauth_token_url_row.set_visible(is_custom);
        widgets.form.oauth_scope_row.set_visible(is_custom);
        widgets
            .form
            .ms_advanced
            .set_visible(p.kind == ProviderKind::Microsoft && !needs_goa);
        if !is_oauth || needs_goa {
            widgets.form.oauth_status.set_visible(false);
        }

        // Auto-fill IMAP/SMTP: known password providers from the preset table,
        // Google/Microsoft from the OAuth preset (filled but hidden, so the saved
        // account still carries the right servers). Manual/Custom are left alone.
        let servers = match p.kind {
            ProviderKind::Preset => Some((p.imap_host, p.imap_port, p.smtp_host, p.smtp_port)),
            ProviderKind::Google | ProviderKind::Microsoft => p
                .oauth_name()
                .and_then(crate::services::oauth::preset)
                .map(|o| (o.imap_host, o.imap_port, o.smtp_host, o.smtp_port)),
            ProviderKind::Manual | ProviderKind::CustomOAuth | ProviderKind::Jmap => None,
        };
        if let Some((ih, ip, sh, sp)) = servers {
            widgets.form.protocol_row.set_selected(0); // IMAP
            widgets.form.host_row.set_text(ih);
            widgets.form.port_row.set_text(&ip.to_string());
            widgets.form.smtp_row.set_text(sh);
            widgets.form.smtp_port_row.set_text(&sp.to_string());
        }
        if p.kind == ProviderKind::Jmap {
            // Fastmail's session URL comes with its entry; the generic entry
            // leaves the server to the user, taking back a preset's address.
            let host = widgets.form.host_row.text();
            if !p.imap_host.is_empty() {
                widgets.form.host_row.set_text(p.imap_host);
            } else if PROVIDERS.iter().any(|q| {
                q.kind == ProviderKind::Jmap && !q.imap_host.is_empty() && q.imap_host == host
            }) {
                widgets.form.host_row.set_text("");
            }
            widgets.form.port_row.set_text("443");
            widgets.form.smtp_row.set_text("");
            widgets.form.jmap_token_row.set_active(p.wizard_token());
        }
        apply_protocol(widgets);
    }

    /// The emoji to save: the editor's, unless the circle is set to show a
    /// picture.
    pub(super) fn saved_emoji(&self) -> Option<String> {
        if self.picture_mode {
            None
        } else {
            self.emoji.clone().filter(|e| !e.is_empty())
        }
    }

    /// The picture to save: the editor's, when the circle is set to show
    /// one.
    pub(super) fn saved_avatar(&self) -> Option<String> {
        if self.picture_mode {
            self.avatar.clone()
        } else {
            None
        }
    }

    /// Redraw the editor's preview circle and show the rows of the chosen
    /// mode: the accent color, and the picture, emoji or initials the
    /// sidebar would show — the initials from the label, else the name,
    /// else the email, as the sidebar derives them.
    /// Everything an account editor holds, as one comparable string: what
    /// Save would write, less the signature — that lives in a WebView and
    /// answers only asynchronously (`RichEditor::is_dirty` covers it).
    fn editor_fingerprint(&self, widgets: &AccountsWindowWidgets) -> String {
        let account = read_account(widgets, self.saved_emoji(), self.saved_avatar());
        format!(
            "{account:?}|{:?}|{:?}|{:?}|{:?}|{}|{}|{}",
            self.alias_edits,
            self.hidden_edits,
            self.read_folder_roles(widgets),
            self.read_sent_copy_path(widgets),
            widgets.form.server_saves_row.is_active(),
            widgets.form.provider_row.selected(),
            self.pending_oauth_refresh.is_some(),
        )
    }

    /// Whether anything in the open account editor differs from what it was
    /// opened with. Leaving an untouched editor must not ask about saving;
    /// with nothing recorded, assume it was touched rather than risk
    /// dropping an edit.
    pub(super) fn editor_touched(&self, widgets: &AccountsWindowWidgets) -> bool {
        self.sig_touched
            || self
                .editor_seed
                .as_ref()
                .is_none_or(|seed| *seed != self.editor_fingerprint(widgets))
    }

    pub(super) fn refresh_preview(&self, widgets: &AccountsWindowWidgets) {
        let color = crate::color::to_hex(&widgets.form.color_btn.rgba());
        self.preview_css.load_from_string(&format!(
            ".account-preview-disc {{ background-color: {color}; outline: 3px solid {color}; outline-offset: 2px; }}"
        ));
        let disc = &widgets.form.preview_disc;
        while let Some(child) = disc.first_child() {
            disc.remove(&child);
        }
        let initials = || {
            let label = trimmed(&widgets.form.label_row);
            let name = trimmed(&widgets.form.name_row);
            let email = trimmed(&widgets.form.email_row);
            let shown = if !label.is_empty() && label != email {
                label
            } else if name.is_empty() {
                email.clone()
            } else {
                name
            };
            crate::ui::initials::glyph_picture(
                &crate::ui::sidebar::account_initials(&shown, &email),
                &color,
                0.47,
                72,
            )
        };
        let picture = self
            .saved_avatar()
            .and_then(|n| crate::config::avatar_path(&n));
        let emoji = self.saved_emoji();
        // The Gravatar this address has, when it was asked for and found
        // (#189) — the same order the sidebar circle and the cards use.
        let gravatar = widgets
            .form
            .gravatar_row
            .is_active()
            .then(|| crate::services::avatar::own_gravatar(&trimmed(&widgets.form.email_row)))
            .flatten();
        let glyph: gtk::Widget = match (&gravatar, &picture, &emoji) {
            (Some(texture), ..) => crate::ui::initials::picture_from_texture(texture, 72).upcast(),
            (None, Some(path), _) => crate::ui::initials::avatar_picture(path, 72).upcast(),
            (None, None, Some(em)) => {
                crate::ui::initials::glyph_picture(em, &color, 0.55, 72).upcast()
            }
            _ => initials().upcast(),
        };
        disc.append(&glyph);
        widgets.form.emoji_row.set_visible(!self.picture_mode);
        widgets.form.picture_row.set_visible(self.picture_mode);
        widgets.form.emoji_clear_btn.set_visible(emoji.is_some());
        widgets.form.avatar_clear_btn.set_visible(picture.is_some());
    }
}

impl AccountsWindow {
    pub(super) fn on_add_account(
        &mut self,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        self.editing = None;
        self.new_editor_session();
        self.emoji = None;
        self.avatar = None;
        widgets.form.gravatar_row.set_active(false);
        self.picture_mode = false;
        widgets.form.mode_glyph_btn.set_active(true);
        self.label_synced = String::new();
        self.pending_oauth_refresh = None;
        self.directory_granted = false;
        self.directory_on = false;
        self.labels_granted = false;
        self.labels_on = false;
        self.close_alias_dialog();
        self.alias_edits.clear();
        self.server_identities.clear();
        self.rebuild_alias_list(&widgets.form.aliases_list, &sender);
        clear_editor(widgets);
        self.show_push_row(widgets);
        self.populate_folder_combos(widgets, None);
        set_connection_editable(widgets, true);
        widgets.form.goa_banner.set_visible(false);
        widgets.form.keyring_note.set_visible(true);
        self.apply_provider(widgets);
        self.load_signatures(widgets, Vec::new(), None);
        widgets.form.color_btn.set_rgba(&parse_color(DEFAULT_COLOR));
        self.refresh_preview(widgets);
        widgets.remove_btn.set_visible(false);
        // A prior GOA edit may have hidden the provider picker.
        widgets.form.provider_row.set_visible(true);
        self.editor_seed = Some(self.editor_fingerprint(widgets));
        mount_editor(widgets);
        widgets.nav.push_by_tag("editor");
    }

    pub(super) fn on_edit_account_by_email(
        &mut self,
        email: String,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        let Some(i) = self.accounts.iter().position(|a| a.email == email) else {
            return;
        };
        let editor_up = widgets
            .nav
            .visible_page()
            .and_then(|p| p.tag())
            .is_some_and(|tag| tag == "editor");
        if editor_up {
            if self.editing == Some(i) {
                return;
            }
            // Another account's editor is up: back to the list first,
            // since a page can't be pushed while it is in the stack.
            widgets.nav.pop();
        }
        sender.input_sender().emit(AccountsInput::EditAccount(i));
    }

    pub(super) fn on_edit_account(
        &mut self,
        i: usize,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        let Some(acc) = self.accounts.get(i).cloned() else {
            return;
        };
        self.editing = Some(i);
        self.new_editor_session();
        self.pending_oauth_refresh = None;
        self.set_graph_switches(widgets, &acc);
        self.close_alias_dialog();
        self.alias_edits = acc.aliases.clone();
        self.server_identities.clear();
        self.rebuild_alias_list(&widgets.form.aliases_list, &sender);
        self.hidden_edits = acc.hidden_folders.clone();
        self.rebuild_hidden_list(widgets, &sender);
        fill_editor(widgets, &acc);
        self.show_push_row(widgets);
        // Hidden until this account's server answers; a page opened
        // before it must not show the last account's figure.
        show_quota(widgets, None);
        let _ = sender.output(AccountsOutput::WantQuota(acc.email.clone()));
        let _ = sender.output(AccountsOutput::WantIdentities(acc.email.clone()));
        // The secrets come from the keyring now, off the main thread,
        // and land in the fields when they arrive (Save waits for
        // them should it come first).
        if AccountSecrets::needed(&acc) {
            self.read_secrets(&acc, &sender);
        }
        self.populate_folder_combos(widgets, Some(&acc));
        self.apply_provider(widgets);
        // Label mirrors the email until customized.
        self.label_synced = acc.email.clone();
        let (signatures, default) = acc.signature_set();
        self.load_signatures(widgets, signatures, default);
        self.fill_face(widgets, &acc);
        show_goa_state(widgets, &acc);
        self.editor_seed = Some(self.editor_fingerprint(widgets));
        mount_editor(widgets);
        widgets.nav.push_by_tag("editor");
    }

    /// The Microsoft directory and label switches, on only when the
    /// account's sign-in carries their scopes (on without them would only
    /// be refused).
    fn set_graph_switches(&mut self, widgets: &AccountsWindowWidgets, acc: &AccountConfig) {
        self.directory_granted = acc.goa_id.is_none()
            && acc.oauth
            && acc
                .oauth_settings
                .as_ref()
                .is_some_and(|s| crate::services::oauth::has_directory_scopes(&s.scopes));
        self.directory_on = acc.graph_directory && self.directory_granted;
        widgets
            .form
            .graph_directory_row
            .set_active(self.directory_on);
        widgets
            .form
            .directory_only_row
            .set_active(acc.graph_directory_only);
        self.labels_granted = acc.goa_id.is_none()
            && acc.oauth
            && acc.oauth_settings.as_ref().is_some_and(|s| {
                crate::services::oauth::has_scopes(
                    &s.scopes,
                    &crate::services::oauth::MICROSOFT_LABEL_SCOPES,
                )
            });
        self.labels_on = acc.graph_label_names && self.labels_granted;
        widgets.form.graph_labels_row.set_active(self.labels_on);
    }

    /// The account's face: its color, emoji or picture, and Gravatar.
    fn fill_face(&mut self, widgets: &AccountsWindowWidgets, acc: &AccountConfig) {
        widgets
            .form
            .color_btn
            .set_rgba(&parse_color(acc.color.as_deref().unwrap_or(DEFAULT_COLOR)));
        self.emoji = acc.emoji.clone();
        self.avatar = acc.avatar.clone();
        widgets.form.gravatar_row.set_active(acc.gravatar);
        self.picture_mode = acc.avatar.is_some();
        if self.picture_mode {
            widgets.form.mode_picture_btn.set_active(true);
        } else {
            widgets.form.mode_glyph_btn.set_active(true);
        }
        self.refresh_preview(widgets);
    }

    pub(super) fn on_secrets_read(
        &mut self,
        session: u64,
        secrets: AccountSecrets,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        if session != self.editor_session {
            return;
        }
        self.secrets_reading = false;
        self.editor_secrets = Some(secrets.clone());
        let editor_up = widgets
            .nav
            .visible_page()
            .and_then(|p| p.tag())
            .is_some_and(|tag| tag == "editor");
        // Queued, so it runs once the fields below are filled.
        if std::mem::take(&mut self.save_waiting) && editor_up {
            sender.input_sender().emit(AccountsInput::Save);
        }
        // Still editing that account: fill in what the user hasn't
        // typed over meanwhile.
        let Some(i) = self.editing else { return };
        if self
            .accounts
            .get(i)
            .is_none_or(|a| a.email != secrets.email)
        {
            return;
        }
        // The passwords arrive after the editor opened, so they would
        // read as an edit the user never made. Re-take the seed below
        // when nothing has actually been typed yet.
        let untouched = !self.editor_touched(widgets);
        if let Some(acc) = self.accounts.get_mut(i) {
            secrets.apply(acc);
        }
        if widgets.form.pass_row.text().is_empty() {
            widgets.form.pass_row.set_text(&secrets.password);
        }
        if widgets.form.smtp_pass_row.text().is_empty() {
            widgets.form.smtp_pass_row.set_text(&secrets.smtp_password);
        }
        for al in self.alias_edits.iter_mut() {
            if al.smtp_password.is_empty() {
                if let Some((_, pw)) = secrets.aliases.iter().find(|(a, _)| *a == al.address()) {
                    al.smtp_password = pw.clone();
                }
            }
        }
        if untouched {
            self.editor_seed = Some(self.editor_fingerprint(widgets));
        }
    }
}

/// What the editor shows of an account from GNOME Online Accounts: no
/// connection fields (GNOME owns them), and its on switch.
fn show_goa_state(widgets: &AccountsWindowWidgets, acc: &AccountConfig) {
    // GOA accounts: no "Remove" (it lives in the system) — offer an
    // enable/disable toggle and a shortcut to Online Accounts instead.
    let is_goa = acc.goa_id.is_some();
    set_connection_editable(widgets, !is_goa);
    widgets.form.goa_banner.set_visible(is_goa);
    widgets.form.keyring_note.set_visible(!is_goa);
    // GOA accounts get the same Remove flow — it removes the account
    // from Hylki only (back to the import list); GNOME keeps it.
    widgets.remove_btn.set_visible(true);
    // GNOME owns a GOA account's connection outright, so the server
    // and credential section isn't shown at all — only what Hylki
    // owns (name, label, color, signature, aliases) plus the email
    // for identification. `apply_provider` re-shows what applies the
    // next time a native account or the add-form opens the editor.
    widgets.form.provider_row.set_visible(!is_goa);
    if is_goa {
        for w in [
            widgets.form.protocol_row.upcast_ref::<gtk::Widget>(),
            widgets.form.host_row.upcast_ref(),
            widgets.form.port_row.upcast_ref(),
            widgets.form.smtp_row.upcast_ref(),
            widgets.form.smtp_port_row.upcast_ref(),
            widgets.form.user_row.upcast_ref(),
            widgets.form.pass_row.upcast_ref(),
            widgets.form.smtp_separate_row.upcast_ref(),
            widgets.form.tls_mismatch_row.upcast_ref(),
            widgets.form.smtp_user_row.upcast_ref(),
            widgets.form.smtp_pass_row.upcast_ref(),
            widgets.form.test_btn.upcast_ref(),
            widgets.form.oauth_signin_btn.upcast_ref(),
            widgets.form.oauth_status.upcast_ref(),
            widgets.form.goa_hint.upcast_ref(),
        ] {
            w.set_visible(false);
        }
        // The Google/Microsoft "use GNOME" guidance hid these; a GOA
        // account edits its display fields right here.
        widgets.form.name_row.set_visible(true);
        widgets.form.email_row.set_visible(true);
    }
    widgets.form.goa_enabled_row.set_active(acc.enabled);
    // While Mail is switched off in GNOME Settings the account is
    // paused from there, not from here — say so where the toggle is.
    widgets
        .form
        .goa_enabled_row
        .set_sensitive(!acc.goa_mail_disabled);
    widgets.form.goa_enabled_row.set_subtitle(if acc.goa_mail_disabled {
        "Paused: Mail is switched off for this account in GNOME Settings \u{2192} Online Accounts."
    } else {
        "Switching this off returns the account to the import list — it \
         stays in GNOME Online Accounts."
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_needed_only_for_what_the_account_lacks() {
        let blank = crate::config::blank_account();
        assert!(AccountSecrets::needed(&blank));
        let with_pw = AccountConfig {
            password: "pw".into(),
            ..blank.clone()
        };
        assert!(!AccountSecrets::needed(&with_pw));
        let oauth = AccountConfig {
            oauth: true,
            ..blank.clone()
        };
        assert!(!AccountSecrets::needed(&oauth));
        let separate = AccountConfig {
            smtp_separate: true,
            ..with_pw.clone()
        };
        assert!(AccountSecrets::needed(&separate));
        let alias = AccountConfig {
            aliases: vec![AliasConfig {
                identity: "Me <me@example.org>".into(),
                smtp_host: "smtp.example.org".into(),
                ..Default::default()
            }],
            ..with_pw.clone()
        };
        assert!(AccountSecrets::needed(&alias));
        let alias_without_smtp = AccountConfig {
            aliases: vec![AliasConfig {
                identity: "me@example.org".into(),
                ..Default::default()
            }],
            ..with_pw
        };
        assert!(!AccountSecrets::needed(&alias_without_smtp));
    }

    #[test]
    fn secrets_fill_only_the_empty_fields() {
        let secrets = AccountSecrets {
            email: "x@example.org".into(),
            password: "kept-pw".into(),
            smtp_password: "kept-smtp".into(),
            aliases: vec![("me@example.org".into(), "alias-pw".into())],
        };
        let mut acc = AccountConfig {
            password: "own".into(),
            aliases: vec![
                AliasConfig {
                    identity: "Me <me@example.org>".into(),
                    ..Default::default()
                },
                AliasConfig {
                    identity: "other@example.org".into(),
                    ..Default::default()
                },
                AliasConfig {
                    identity: "me@example.org".into(),
                    smtp_password: "set".into(),
                    ..Default::default()
                },
            ],
            ..crate::config::blank_account()
        };
        secrets.apply(&mut acc);
        assert_eq!(acc.password, "own");
        assert_eq!(acc.smtp_password, "kept-smtp");
        assert_eq!(acc.aliases[0].smtp_password, "alias-pw");
        assert_eq!(acc.aliases[1].smtp_password, "");
        assert_eq!(acc.aliases[2].smtp_password, "set");
    }
}
