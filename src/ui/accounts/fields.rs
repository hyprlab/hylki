//! The account editor's fields as they change: the circle, the provider
//! and protocol, the connection test, and what the app reports about the
//! account (folders, quota, server identities).

use super::*;

impl AccountsWindow {
    pub(super) fn on_debug_provider(&mut self, label: String, widgets: &AccountsWindowWidgets) {
        if let Some(i) = PROVIDERS.iter().position(|p| p.label == label) {
            widgets.form.provider_row.set_selected(i as u32);
        }
    }

    pub(super) fn on_email_changed(&mut self, widgets: &AccountsWindowWidgets) {
        let email = trimmed(&widgets.form.email_row);
        let label = widgets.form.label_row.text().to_string();
        // Mirror while the label is still tracking the email (or empty);
        // once the user types a custom label, stop.
        if label.is_empty() || label == self.label_synced {
            widgets.form.label_row.set_text(&email);
        }
        self.label_synced = email;
    }

    pub(super) fn on_set_emoji(&mut self, text: String, widgets: &AccountsWindowWidgets) {
        self.emoji = Some(text);
        self.refresh_preview(widgets);
    }

    pub(super) fn on_clear_glyph(&mut self, widgets: &AccountsWindowWidgets) {
        if self.picture_mode {
            self.avatar = None;
        } else {
            self.emoji = None;
        }
        self.refresh_preview(widgets);
    }

    pub(super) fn on_set_server_saves_sent(&mut self, on: bool, widgets: &AccountsWindowWidgets) {
        widgets.form.folder_sent_copy_row.set_sensitive(!on);
        widgets.form.reply_copies_row.set_sensitive(!on);
    }

    pub(super) fn on_set_folder_sort(
        &mut self,
        email: String,
        sort: Option<crate::config::FolderSort>,
        widgets: &AccountsWindowWidgets,
    ) {
        if let Some(i) = self.accounts.iter().position(|a| a.email == email) {
            self.accounts[i].folder_sort = sort;
            if self.editing == Some(i) {
                widgets
                    .form
                    .folder_sort_row
                    .set_selected(sort.map_or(0, |s| s.index() + 1));
            }
        }
    }

    pub(super) fn on_set_own_gravatar(
        &mut self,
        on: bool,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        // Look it up as soon as it is asked for, so the preview can
        // answer rather than waiting for the account to be saved.
        let address = trimmed(&widgets.form.email_row);
        if on && !address.is_empty() && crate::services::avatar::wants_own_gravatar(&address) {
            sender.oneshot_command(async move {
                let outcome = tokio::task::spawn_blocking({
                    let address = address.clone();
                    move || crate::services::avatar::fetch_own_gravatar(&address)
                })
                .await
                .unwrap_or(crate::services::avatar::FetchOutcome::Retry);
                AccountsCmd::OwnGravatar {
                    email: address,
                    outcome,
                }
            });
        }
        self.refresh_preview(widgets);
    }

    pub(super) fn on_set_picture_mode(&mut self, on: bool, widgets: &AccountsWindowWidgets) {
        if self.picture_mode != on {
            self.picture_mode = on;
            self.refresh_preview(widgets);
        }
    }

    pub(super) fn on_pick_avatar(&mut self, sender: ComponentSender<Self>, root: &adw::Bin) {
        let dialog = gtk::FileDialog::builder()
            .title(i18n("Choose a Picture"))
            .build();
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&i18n("Images")));
        filter.add_pixbuf_formats();
        let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        dialog.set_filters(Some(&filters));
        dialog.set_default_filter(Some(&filter));
        // The panel is a Bin inside the Settings window; the
        // dialog wants that window.
        let parent = root.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        let s = sender.clone();
        dialog.open(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
            let Ok(file) = res else { return };
            let Some(path) = file.path() else { return };
            match import_avatar_file(&path) {
                Ok(name) => s.input_sender().emit(AccountsInput::SetAvatar(name)),
                Err(e) => tracing::warn!("could not import avatar {}: {e}", path.display()),
            }
        });
    }

    pub(super) fn on_set_avatar(&mut self, name: String, widgets: &AccountsWindowWidgets) {
        self.avatar = Some(name);
        self.refresh_preview(widgets);
    }

    pub(super) fn on_test_connection(
        &mut self,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        // A Tuta or Proton connection test is its sign-in, and it opens the
        // session the account keeps.
        if form_protocol(widgets).uses_helper() {
            self.start_helper_sign_in(widgets, &sender, false);
            return;
        }
        let account = read_account(widgets, self.saved_emoji(), self.saved_avatar());
        widgets.form.test_btn.set_sensitive(false);
        widgets.form.test_result.set_visible(true);
        widgets.form.test_result.set_css_classes(&["dim-label"]);
        widgets.form.test_result.set_label(&i18n("Testing…"));
        sender.oneshot_command(async move {
            let r = tokio::task::spawn_blocking(move || worker::test_connection_blocking(account))
                .await
                .unwrap_or_else(|_| ConnTest {
                    incoming: Err("test could not run".into()),
                    smtp: Err("test could not run".into()),
                });
            AccountsCmd::Test(r)
        });
    }

    /// Sign in to Tuta or Proton through the helper with the form's address
    /// and password and whatever else the provider has asked for by now: a
    /// code, a mailbox password, a solved challenge. The answer comes back
    /// as [`AccountsCmd::HelperSignIn`]. Save starts one as well, with
    /// `then_save`, when a password was typed but not yet signed in with.
    pub(super) fn start_helper_sign_in(
        &mut self,
        widgets: &AccountsWindowWidgets,
        sender: &ComponentSender<Self>,
        then_save: bool,
    ) {
        let protocol = form_protocol(widgets);
        let email = trimmed(&widgets.form.email_row);
        let password = widgets.form.pass_row.text().to_string();
        widgets.form.test_result.set_visible(true);
        if email.is_empty() || password.is_empty() {
            widgets.form.test_result.set_css_classes(&["error"]);
            widgets
                .form
                .test_result
                .set_label(&i18n("Enter your email address and password first."));
            return;
        }
        let (hv_token, hv_type) = self.human_check.solved();
        let login = worker::HelperLogin {
            email: email.clone(),
            password,
            totp: totp_code(&widgets.form.totp_row),
            mailbox_password: mailbox_password(&widgets.form.mailbox_pass_row),
            hv_token,
            hv_type,
        };
        widgets.form.test_btn.set_sensitive(false);
        widgets.form.test_result.set_css_classes(&["dim-label"]);
        widgets.form.test_result.set_label(&i18n("Signing in…"));
        let editor = self.editor_session;
        sender.oneshot_command(async move {
            let result = tokio::task::spawn_blocking(move || {
                worker::helper_login_blocking(protocol, &login)
            })
            .await
            .unwrap_or_else(|_| Err(worker::HelperError::new("helper_failed")));
            AccountsCmd::HelperSignIn {
                editor,
                then_save,
                result: result.map(|session| (protocol, email, session)),
            }
        });
    }

    pub(super) fn on_helper_signed_in(
        &mut self,
        editor: u64,
        then_save: bool,
        result: Result<(Protocol, String, String), worker::HelperError>,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        // Started in an editor since closed or replaced.
        if editor != self.editor_session {
            return;
        }
        widgets.form.test_btn.set_sensitive(true);
        widgets.form.test_result.set_visible(true);
        let e = match result {
            Ok(signed_in) => {
                let name = helper_name(signed_in.0);
                self.pending_helper_session = Some(signed_in);
                self.human_check.clear();
                // The session stands in for the password from here on, so
                // neither password is kept even in the form.
                widgets.form.pass_row.set_text("");
                hide_sign_in_rows(&sign_in_rows(widgets));
                widgets.form.test_result.set_css_classes(&["success"]);
                widgets
                    .form
                    .test_result
                    .set_label(&i18n_f("✓ {provider}: signed in", &[("provider", name)]));
                if then_save {
                    sender.input_sender().emit(AccountsInput::Save);
                }
                return;
            }
            Err(e) => e,
        };
        match self.human_check.stopped(&e) {
            proton_verify::HumanStep::Retry => {
                self.start_helper_sign_in(widgets, &sender, then_save);
                return;
            }
            proton_verify::HumanStep::Show => {
                self.human_check_then_save = then_save;
                self.on_verify_human(widgets, &sender);
            }
            proton_verify::HumanStep::Nothing => {}
        }
        let step = is_sign_in_step(&e);
        let text = reveal_sign_in_row(&sign_in_rows(widgets), &e);
        widgets
            .form
            .test_result
            .set_css_classes(&[if step { "dim-label" } else { "error" }]);
        widgets.form.test_result.set_label(&if step {
            text
        } else {
            let name = helper_name(form_protocol(widgets));
            i18n_f("✗ {provider}: {e}", &[("provider", name), ("e", &text)])
        });
    }

    /// Show Proton's challenge for the editor's sign-in: when a sign-in
    /// comes back asking for it, and from the Verify button after.
    pub(super) fn on_verify_human(
        &mut self,
        widgets: &AccountsWindowWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let Some(challenge) = self.human_check.challenge() else {
            return;
        };
        let input = sender.input_sender().clone();
        proton_verify::present(&widgets.form.verify_row, challenge, move |verified| {
            if let Some(verified) = verified {
                input.emit(AccountsInput::HumanVerified(verified));
            }
        });
    }

    /// The challenge was solved: sign in again at once with its token, and
    /// with the code the account may also want, so the token is not spent
    /// on a sign-in that stops there.
    pub(super) fn on_human_verified(
        &mut self,
        verified: proton_verify::Verified,
        widgets: &AccountsWindowWidgets,
        sender: &ComponentSender<Self>,
    ) {
        if !self.human_check.solve(verified) {
            return;
        }
        let then_save = std::mem::take(&mut self.human_check_then_save);
        self.start_helper_sign_in(widgets, sender, then_save);
    }

    pub(super) fn on_protocol_changed(&mut self, widgets: &AccountsWindowWidgets) {
        apply_protocol(widgets);
        self.show_push_row(widgets);
        self.refresh_provider_mark(widgets);
    }

    pub(super) fn on_provider_changed(&mut self, widgets: &AccountsWindowWidgets) {
        // Editing a GOA account: the provider dropdown is hidden and the
        // connection section deliberately not shown — but filling the
        // editor sets the dropdown, whose notify lands here right after
        // EditAccount and would undo that. GNOME owns those fields;
        // leave them hidden.
        let editing_goa = self
            .editing
            .and_then(|i| self.accounts.get(i))
            .is_some_and(|a| a.goa_id.is_some());
        if !editing_goa {
            self.apply_provider(widgets);
        }
        self.show_push_row(widgets);
    }

    pub(super) fn on_set_folder_choices(
        &mut self,
        map: std::collections::HashMap<String, Vec<(String, String)>>,
    ) {
        self.folders_by_email = map;
    }

    pub(super) fn on_server_identities(
        &mut self,
        email: String,
        identities: Vec<(String, String)>,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        let open = self
            .editing
            .and_then(|i| self.accounts.get(i))
            .is_some_and(|a| a.email.eq_ignore_ascii_case(&email));
        if open && self.server_identities != identities {
            self.server_identities = identities;
            self.rebuild_alias_list(&widgets.form.aliases_list, &sender);
        }
    }

    pub(super) fn on_quota(
        &mut self,
        email: String,
        quota: Option<crate::models::MailboxQuota>,
        widgets: &AccountsWindowWidgets,
    ) {
        let open = self
            .editing
            .and_then(|i| self.accounts.get(i))
            .is_some_and(|a| a.email.eq_ignore_ascii_case(&email));
        if open {
            show_quota(widgets, quota);
        }
    }

    pub(super) fn on_own_gravatar_fetched(
        &mut self,
        email: String,
        outcome: crate::services::avatar::FetchOutcome,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        // Cached for every other circle in the app as well; the
        // preview only redraws while that address is still in the
        // editor, and the accounts list redraws its circles so one
        // that landed after the list was built shows there too.
        let found = crate::services::avatar::cache_own_gravatar(&email, outcome);
        if found && trimmed(&widgets.form.email_row).eq_ignore_ascii_case(&email) {
            self.refresh_preview(widgets);
        }
        if found {
            self.rebuild_account_list(&widgets.accounts.accounts_list, &sender);
        }
    }

    pub(super) fn on_connection_tested(
        &mut self,
        result: ConnTest,
        widgets: &AccountsWindowWidgets,
    ) {
        let line = |label: &str, r: &Result<(), String>| {
            match r {
            Ok(()) => format!("✓ {label}: connected"),
            Err(e) if crate::worker::is_hostname_mismatch(e) => format!(
                "✗ {label}: {e}\n   {}",
                i18n("The certificate is for another name. If this server is the right one, turn on “Accept a certificate for another name” above and test again.")
            ),
            Err(e) => format!("✗ {label}: {e}"),
        }
        };
        let protocol = form_protocol(widgets);
        let incoming_label = match protocol {
            Protocol::Pop3 => "POP3",
            Protocol::Jmap => "JMAP",
            Protocol::Tuta | Protocol::Proton => helper_name(protocol),
            _ => "IMAP",
        };
        // A JMAP, Tuta or Proton account sends through its server: no SMTP
        // line.
        let text = if protocol == Protocol::Jmap || protocol.uses_helper() {
            line(incoming_label, &result.incoming)
        } else {
            format!(
                "{}\n{}",
                line(incoming_label, &result.incoming),
                line("SMTP", &result.smtp),
            )
        };
        let class = if result.incoming.is_ok() && result.smtp.is_ok() {
            "success"
        } else {
            "error"
        };
        widgets.form.test_result.set_label(&text);
        widgets.form.test_result.set_css_classes(&[class]);
        widgets.form.test_btn.set_sensitive(true);
    }
}

/// The provider a helper signs in to, as its result lines name it. A brand,
/// so not translated.
fn helper_name(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Proton => "Proton Mail",
        _ => "Tuta",
    }
}
