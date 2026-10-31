//! Saving, pausing and removing the account in the editor.

use super::*;

impl AccountsWindow {
    pub(super) fn on_toggle_current_enabled(
        &mut self,
        enabled: bool,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        if let Some(i) = self.editing {
            // Same un-import semantics as the list toggle; the editor
            // page closes since its account is no longer in Hylki.
            if !enabled && self.accounts.get(i).is_some_and(|a| a.goa_id.is_some()) {
                self.unimport_goa(i, widgets, &sender);
                widgets.nav.pop();
                return;
            }
            if let Some(acc) = self.accounts.get_mut(i) {
                if acc.enabled != enabled {
                    acc.enabled = enabled;
                    let email = acc.email.clone();
                    let _ = sender.output(AccountsOutput::EnabledChanged { email, enabled });
                    self.rebuild_account_list(&widgets.accounts.accounts_list, &sender);
                }
            }
        }
    }

    pub(super) fn on_save(
        &mut self,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        // Pull the signature HTML out of the editor first (async), then
        // finish saving in SaveWithSig.
        let s = sender.clone();
        self.read_signature(widgets, move |html| {
            s.input_sender().emit(AccountsInput::SaveWithSig(html))
        });
    }

    pub(super) fn on_save_with_sig(
        &mut self,
        sig_html: String,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        widgets.form.host_row.remove_css_class("error");
        let mut account = read_account(widgets, self.saved_emoji(), self.saved_avatar());
        account.aliases = self.alias_edits.clone();
        account.hidden_folders = self.hidden_edits.clone();
        account.folder_roles = self.read_folder_roles(widgets);
        account.sent_copy_path = self.read_sent_copy_path(widgets);
        account.server_saves_sent = widgets.form.server_saves_row.is_active();
        if let Some(shown) = self.sig_edits.get_mut(self.sig_shown) {
            shown.html = sig_html;
        }
        let (signatures, default) = self.saved_signatures();
        account.set_signatures(signatures, default);

        // Editing preserves the enabled state; GOA accounts keep their
        // (GOA-driven) OAuth mechanism regardless of the Authentication combo.
        let editing_orig = self.editing.and_then(|i| self.accounts.get(i)).cloned();
        if let Some(orig) = &editing_orig {
            keep_from_original(&mut account, orig);
        }

        // Only once Microsoft has approved the scopes; an account from
        // GNOME Online Accounts cannot have them.
        account.graph_directory &= account.goa_id.is_none() && self.directory_granted;
        account.graph_label_names &= account.goa_id.is_none() && self.labels_granted;

        self.apply_form_auth(&mut account, widgets);

        // Validation.
        let oauth_ready = self.check_oauth_ready(&account, editing_orig.as_ref(), widgets);
        // Saved before the keyring answered, or with the field left
        // as it was: the stored secrets stand. Not read yet, they
        // are waited for, and Save runs again once they are here.
        if AccountSecrets::needed(&account) {
            match &self.editor_secrets {
                Some(secrets) => secrets.apply(&mut account),
                None => {
                    if !self.secrets_reading {
                        self.read_secrets(&account, &sender);
                    }
                    self.save_waiting = true;
                    return;
                }
            }
        }
        let password_ok = account.oauth || !account.password.is_empty();
        // A GOA account's connection fields were all restored from the
        // original above (GNOME owns them; a Graph account rightly has
        // no IMAP host at all) — validating them would only block the
        // fields Hylki does own: label, signature, color, aliases.
        // On This Computer (#552) has no connection at all.
        let is_goa_edit = account.goa_id.is_some() || account.is_local();
        if !is_goa_edit
            && (account.imap_host.is_empty()
                || account.username.is_empty()
                || !password_ok
                || !oauth_ready
                || (account.smtp_separate
                    && (account.smtp_username.is_empty() || account.smtp_password.is_empty())))
        {
            widgets.form.host_row.add_css_class("error");
            return;
        }
        self.pending_oauth_refresh = None;

        let original_email = self
            .editing
            .and_then(|i| self.accounts.get(i))
            .map(|a| a.email.clone());
        match self.editing {
            Some(i) if i < self.accounts.len() => self.accounts[i] = account.clone(),
            _ => self.accounts.push(account.clone()),
        }
        self.rebuild_account_list(&widgets.accounts.accounts_list, &sender);
        let wants_local = account.copy_to_local;
        let _ = sender.output(AccountsOutput::Saved {
            original_email,
            account: Box::new(account),
        });
        // Copying to On This Computer (#552) adds it when there is none.
        if wants_local {
            self.on_add_local(widgets, sender.clone());
        }
        widgets.nav.pop();
    }

    /// Native account: authentication comes from the provider dropdown.
    fn apply_form_auth(&self, account: &mut AccountConfig, widgets: &AccountsWindowWidgets) {
        let is_oauth = provider_at(widgets.form.provider_row.selected()).is_oauth();
        if account.goa_id.is_none() {
            if is_oauth {
                account.oauth = true;
                account.oauth_settings = Some(self.oauth_settings_from_form(widgets));
                if account.username.trim().is_empty() {
                    account.username = account.email.clone();
                }
                // A fresh sign-in supplies a refresh token; otherwise keep
                // the one already in the keyring (edit without re-signing).
                if let Some(rt) = self.pending_oauth_refresh.clone() {
                    account.oauth_refresh = rt;
                }
            } else {
                account.oauth = false;
                account.oauth_settings = None;
            }
        }
    }

    /// Whether a native OAuth account can be saved: it has a client ID and
    /// a sign-in (a fresh one, or the edited account's). When not, the form
    /// says so.
    fn check_oauth_ready(
        &self,
        account: &AccountConfig,
        editing_orig: Option<&AccountConfig>,
        widgets: &AccountsWindowWidgets,
    ) -> bool {
        if !(account.oauth && account.goa_id.is_none()) {
            return true;
        }
        let has_client = account
            .oauth_settings
            .as_ref()
            .is_some_and(|s| !s.client_id.trim().is_empty());
        let signed_in =
            self.pending_oauth_refresh.is_some() || editing_orig.is_some_and(|o| o.oauth);
        if !has_client || !signed_in {
            widgets.form.oauth_status.set_visible(true);
            widgets.form.oauth_status.set_css_classes(&["error"]);
            widgets
                .form
                .oauth_status
                .set_label(&i18n("Enter a client ID and sign in before saving"));
        }
        has_client && signed_in
    }

    pub(super) fn on_remove_current(&mut self, sender: ComponentSender<Self>, root: &adw::Bin) {
        // Confirm before this destructive, keyring-clearing action.
        let Some(i) = self.editing else { return };
        let Some(account) = self.accounts.get(i) else {
            return;
        };
        let name = if account.name.trim().is_empty() {
            account.email.clone()
        } else {
            account.name.clone()
        };
        let body = if account.is_local() {
            i18n_f(
                "Remove {name} from Hylki? Its folders and the mail in them stay on this computer, in {dir}, and come back if it is added again.",
                &[("name", &name), ("dir", &super::local::shown_dir())],
            )
        } else if account.goa_id.is_some() {
            format!(
                "Remove {name} from Hylki? It stays in GNOME Online Accounts \
                 and can be imported again from the list below. Mail on the \
                 server is not affected."
            )
        } else {
            format!(
                "Remove {name} from Hylki? Its saved password is deleted from \
                 the keyring. Mail on the server is not affected."
            )
        };
        // The panel is embedded — dialogs parent to whatever window
        // it currently sits in (the combined settings window).
        let host = root.root().and_downcast::<gtk::Window>();
        let s = sender.clone();
        crate::ui::dialog::confirm_destructive(
            host.as_ref(),
            i18n("Remove Account?").as_str(),
            &body,
            "remove",
            &i18n("Remove"),
            move || s.input_sender().emit(AccountsInput::ConfirmRemove),
        );
    }

    pub(super) fn on_confirm_remove(
        &mut self,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        if let Some(i) = self.editing {
            if i < self.accounts.len() {
                let email = self.accounts[i].email.clone();
                self.accounts.remove(i);
                self.rebuild_account_list(&widgets.accounts.accounts_list, &sender);
                let _ = sender.output(AccountsOutput::Removed { email });
                // A removed GOA import returns to the list below (and a
                // repaired-away broken entry re-offers its GOA account).
                self.goa = importable_goa_accounts(&self.accounts);
                self.rebuild_goa_list(&widgets.accounts.goa_list, &sender);
                widgets.accounts.goa_group.set_visible(!self.goa.is_empty());
            }
        }
        widgets.nav.pop();
    }
}

/// What an edit keeps from the account as it was: whether it is on, the
/// one-time folder look, its GNOME Online Accounts link, and for an
/// account from GNOME Online Accounts everything GNOME owns.
fn keep_from_original(account: &mut AccountConfig, orig: &AccountConfig) {
    account.enabled = orig.enabled;
    // The one-time look for Exchange's non-mail folders
    // (#239) stays done, or a folder brought back here
    // would be hidden again at the next listing.
    account.folders_seeded = orig.folders_seeded;
    account.goa_id = orig.goa_id.clone();
    account.goa_mail_disabled = orig.goa_mail_disabled;
    account.goa_enabled_before_mail_disabled = orig.goa_enabled_before_mail_disabled;
    if orig.goa_id.is_some() {
        account.oauth = orig.oauth;
        account.oauth_settings = orig.oauth_settings.clone();
        // GNOME Online Accounts is the source of truth for these;
        // Hylki keeps only what it owns (display name, signature,
        // color, emoji, label).
        account.email = orig.email.clone();
        account.protocol = orig.protocol;
        account.imap_host = orig.imap_host.clone();
        account.imap_port = orig.imap_port;
        account.smtp_host = orig.smtp_host.clone();
        account.smtp_port = orig.smtp_port;
        account.username = orig.username.clone();
        account.password = orig.password.clone();
        account.smtp_separate = orig.smtp_separate;
        account.smtp_username = orig.smtp_username.clone();
        account.smtp_password = orig.smtp_password.clone();
        account.security = orig.security.clone();
    }
    // On This Computer (#552): the form shows none of what makes it what
    // it is, and reads as a plain IMAP account.
    if orig.is_local() {
        let mut kept = orig.clone();
        kept.name = account.name.clone();
        kept.label = account.label.clone();
        kept.color = account.color.clone();
        kept.emoji = account.emoji.clone();
        kept.avatar = account.avatar.clone();
        kept.gravatar = account.gravatar;
        kept.in_unified = account.in_unified;
        kept.empty_junk_days = account.empty_junk_days;
        kept.empty_trash_days = account.empty_trash_days;
        kept.folder_sort = account.folder_sort;
        kept.folder_roles = account.folder_roles.clone();
        kept.hidden_folders = account.hidden_folders.clone();
        *account = kept;
    }
}
