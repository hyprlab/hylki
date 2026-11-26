//! Signing an account in: the browser or in-app OAuth flow, the extra
//! consents for the directory and labels, and the Online Accounts panel.

use super::form::trimmed;
use super::messages::SignInResult;
use super::providers::provider_at;
use super::*;

/// The editor's sign-in: in a window of Hylki's own for a redirect Hylki
/// cannot listen on (#329), otherwise in the browser. `user` is the address
/// typed, which picks the identity broker's account and, with `hint`, the
/// account Microsoft signs in (#410). The answer comes back through `wrap`.
fn sign_in(
    root: &adw::Bin,
    sender: &ComponentSender<AccountsWindow>,
    settings: OAuthSettings,
    user: Option<String>,
    hint: bool,
    wrap: fn(SignInResult) -> AccountsCmd,
) {
    if !settings.redirect_uri.trim().is_empty() {
        let parent = root.root().and_downcast::<gtk::Window>();
        let s = sender.clone();
        crate::ui::web_signin::run(
            parent.as_ref(),
            &settings.clone(),
            user.clone(),
            hint,
            move |answer| {
                s.oneshot_command(async move {
                    let r = tokio::task::spawn_blocking(move || {
                        let (request, code) = answer?;
                        let refresh =
                            crate::services::oauth::exchange_code(&settings, &request, &code)?
                                .refresh_token;
                        let (name, address, refresh) = crate::services::oauth::microsoft_whoami(
                            &settings,
                            &refresh,
                            user.as_deref(),
                        )?;
                        Ok((refresh, Some((name, address))))
                    })
                    .await
                    .unwrap_or_else(|_| Err("sign-in task failed".into()));
                    wrap(r)
                });
            },
        );
        return;
    }
    sender.oneshot_command(async move {
        let r = tokio::task::spawn_blocking(move || {
            let hint = user.as_deref().filter(|_| hint);
            let refresh = crate::services::oauth::run_flow_as(&settings, hint)?.refresh_token;
            // Microsoft says who signed in, so the account is named
            // for the mailbox reached (#329).
            if settings.token_url.contains("microsoftonline") {
                let (name, address, refresh) =
                    crate::services::oauth::microsoft_whoami(&settings, &refresh, None)?;
                return Ok((refresh, Some((name, address))));
            }
            Ok((refresh, None))
        })
        .await
        .unwrap_or_else(|_| Err("sign-in task failed".into()));
        wrap(r)
    });
}

/// Open GNOME Settings → Online Accounts. Uses D-Bus app activation so it works
/// both natively and inside a Flatpak (with `--talk-name=org.gnome.Settings`);
/// falls back to the CLI on non-GNOME/older setups.
pub(crate) fn open_online_accounts() {
    if activate_online_accounts_panel().is_err() {
        let _ = std::process::Command::new("gnome-control-center")
            .arg("online-accounts")
            .spawn();
    }
}

fn activate_online_accounts_panel() -> Result<(), gtk::glib::Error> {
    let conn = gtk::gio::bus_get_sync(gtk::gio::BusType::Session, gtk::gio::Cancellable::NONE)?;
    // org.freedesktop.Application.ActivateAction(action: s, parameter: av, a{sv}).
    // GNOME Settings' "launch-panel" action takes a (sav): (panel_id, extra_args).
    let panel = ("online-accounts", Vec::<gtk::glib::Variant>::new()).to_variant();
    let params: Vec<gtk::glib::Variant> = vec![panel];
    let platform: std::collections::HashMap<String, gtk::glib::Variant> =
        std::collections::HashMap::new();
    let args = ("launch-panel", params, platform).to_variant();
    conn.call_sync(
        Some("org.gnome.Settings"),
        "/org/gnome/Settings",
        "org.freedesktop.Application",
        "ActivateAction",
        Some(&args),
        None,
        gtk::gio::DBusCallFlags::NONE,
        -1,
        gtk::gio::Cancellable::NONE,
    )?;
    Ok(())
}

impl AccountsWindow {
    /// A sign-in that asked for more scopes (#410, #411) finished: its
    /// refresh token is the one to save, and a new account takes its
    /// address and name from it.
    fn take_consent(
        &mut self,
        widgets: &AccountsWindowWidgets,
        result: SignInResult,
    ) -> Result<(), String> {
        // A saved account signs in as itself: another mailbox picked in
        // the browser would swap the account's token.
        let saved = self
            .editing
            .and_then(|i| self.accounts.get(i))
            .filter(|a| a.oauth && a.goa_id.is_none())
            .map(|a| a.email.clone());
        let (refresh, who) = result.and_then(|(refresh, who)| match (&saved, &who) {
            (Some(email), Some((_, address)))
                if !address.is_empty() && !address.eq_ignore_ascii_case(email) =>
            {
                Err(i18n_f(
                    "Signed in as {address}, not {email}.",
                    &[("address", address), ("email", email)],
                ))
            }
            _ => Ok((refresh, who)),
        })?;
        self.pending_oauth_refresh = Some(refresh);
        // A new account was signed in by this as well.
        if let Some((name, address)) = who {
            if !address.is_empty() {
                widgets.form.email_row.set_text(&address);
            }
            if !name.is_empty() && widgets.form.name_row.text().trim().is_empty() {
                widgets.form.name_row.set_text(&name);
            }
        }
        Ok(())
    }

    /// Build the OAuth client config from the form. Google/Microsoft use built-in
    /// endpoints + credentials; "Custom OAuth" uses the user-entered fields.
    pub(super) fn oauth_settings_from_form(
        &self,
        widgets: &AccountsWindowWidgets,
    ) -> OAuthSettings {
        let provider = provider_at(widgets.form.provider_row.selected()).oauth_name();
        if let Some(name) = provider {
            let p = crate::services::oauth::preset(name)
                .expect("oauth_name only names providers oauth has a preset for");
            let (client_id, client_secret) = crate::services::oauth::provider_credentials(name);
            let mut settings = OAuthSettings {
                auth_url: p.auth_url.to_string(),
                token_url: p.token_url.to_string(),
                client_id,
                client_secret,
                scopes: p.scopes.to_string(),
                redirect_uri: String::new(),
            };
            if name == "microsoft" {
                crate::services::oauth::microsoft_advanced(
                    &mut settings,
                    &trimmed(&widgets.form.ms_client_row),
                    &trimmed(&widgets.form.ms_tenant_row),
                    &trimmed(&widgets.form.ms_scopes_row),
                    &trimmed(&widgets.form.ms_redirect_row),
                );
                // Built from the preset, the scopes would lose the
                // directory's at the next sign-in or refresh (#410).
                if self.directory_granted {
                    settings.scopes =
                        crate::services::oauth::with_directory_scopes(&settings.scopes);
                }
                if self.labels_granted {
                    settings.scopes = crate::services::oauth::with_scopes(
                        &settings.scopes,
                        &crate::services::oauth::MICROSOFT_LABEL_SCOPES,
                    );
                }
            }
            settings
        } else {
            OAuthSettings {
                auth_url: trimmed(&widgets.form.oauth_auth_url_row),
                token_url: trimmed(&widgets.form.oauth_token_url_row),
                client_id: trimmed(&widgets.form.oauth_client_id_row),
                client_secret: widgets.form.oauth_secret_row.text().to_string(),
                scopes: trimmed(&widgets.form.oauth_scope_row),
                redirect_uri: String::new(),
            }
        }
    }
}

impl AccountsWindow {
    pub(super) fn on_oauth_sign_in(
        &mut self,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
        root: &adw::Bin,
    ) {
        let settings = self.oauth_settings_from_form(widgets);
        if settings.client_id.trim().is_empty()
            || settings.auth_url.is_empty()
            || settings.token_url.is_empty()
        {
            widgets.form.oauth_status.set_visible(true);
            widgets.form.oauth_status.set_css_classes(&["error"]);
            widgets.form.oauth_status.set_label(&i18n(
                "Enter a client ID first, and the endpoints for a custom provider.",
            ));
            return;
        }
        widgets.form.oauth_signin_btn.set_sensitive(false);
        widgets.form.oauth_status.set_visible(true);
        widgets.form.oauth_status.set_css_classes(&["dim-label"]);
        let in_window = !settings.redirect_uri.trim().is_empty();
        widgets.form.oauth_status.set_label(&if in_window {
            i18n("Complete the sign-in in the window that opened.")
        } else {
            i18n("Opening browser… complete sign-in there.")
        });
        // The address typed so far names the identity broker's
        // account, where one is in charge of sign-ins.
        let user = Some(widgets.form.email_row.text().trim().to_string()).filter(|u| !u.is_empty());
        sign_in(root, &sender, settings, user, false, AccountsCmd::OAuth);
    }

    pub(super) fn on_set_graph_directory(
        &mut self,
        on: bool,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
        root: &adw::Bin,
    ) {
        if on == self.directory_on {
            return;
        }
        self.directory_on = on;
        widgets.form.graph_status.set_visible(false);
        // Turning it off keeps the scopes: they cost nothing, and
        // turning it on again then asks nothing.
        if !on || self.directory_granted {
            return;
        }
        let mut settings = self.oauth_settings_from_form(widgets);
        if settings.client_id.trim().is_empty() || !settings.token_url.contains("microsoftonline") {
            return;
        }
        settings.scopes = crate::services::oauth::with_directory_scopes(&settings.scopes);
        widgets.form.graph_directory_row.set_sensitive(false);
        widgets.form.oauth_signin_btn.set_sensitive(false);
        widgets.form.graph_status.set_visible(true);
        widgets.form.graph_status.set_css_classes(&["dim-label"]);
        widgets
            .form
            .graph_status
            .set_label(&if settings.redirect_uri.trim().is_empty() {
                i18n("Opening browser… approve access to the directory there.")
            } else {
                i18n("Approve access to the directory in the window that opened.")
            });
        // Straight to the account being edited: Microsoft then asks
        // only for what it has not approved yet.
        let user = Some(widgets.form.email_row.text().trim().to_string()).filter(|u| !u.is_empty());
        sign_in(
            root,
            &sender,
            settings,
            user,
            true,
            AccountsCmd::DirectoryConsent,
        );
    }

    pub(super) fn on_set_graph_labels(
        &mut self,
        on: bool,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
        root: &adw::Bin,
    ) {
        if on == self.labels_on {
            return;
        }
        self.labels_on = on;
        widgets.form.graph_status.set_visible(false);
        // Off keeps the scope, as the directory's does.
        if !on || self.labels_granted {
            return;
        }
        let mut settings = self.oauth_settings_from_form(widgets);
        if settings.client_id.trim().is_empty() || !settings.token_url.contains("microsoftonline") {
            return;
        }
        settings.scopes = crate::services::oauth::with_scopes(
            &settings.scopes,
            &crate::services::oauth::MICROSOFT_LABEL_SCOPES,
        );
        widgets.form.graph_labels_row.set_sensitive(false);
        widgets.form.oauth_signin_btn.set_sensitive(false);
        widgets.form.graph_status.set_visible(true);
        widgets.form.graph_status.set_css_classes(&["dim-label"]);
        widgets
            .form
            .graph_status
            .set_label(&if settings.redirect_uri.trim().is_empty() {
                i18n("Opening browser… approve access to the sensitivity labels there.")
            } else {
                i18n("Approve access to the sensitivity labels in the window that opened.")
            });
        let user = Some(widgets.form.email_row.text().trim().to_string()).filter(|u| !u.is_empty());
        sign_in(
            root,
            &sender,
            settings,
            user,
            true,
            AccountsCmd::LabelsConsent,
        );
    }

    pub(super) fn on_signed_in(&mut self, result: SignInResult, widgets: &AccountsWindowWidgets) {
        widgets.form.oauth_signin_btn.set_sensitive(true);
        widgets.form.oauth_status.set_visible(true);
        match result {
            Ok((refresh, who)) => {
                self.pending_oauth_refresh = Some(refresh);
                if let Some((name, address)) = who {
                    if !address.is_empty() {
                        widgets.form.email_row.set_text(&address);
                    }
                    if !name.is_empty() && widgets.form.name_row.text().trim().is_empty() {
                        widgets.form.name_row.set_text(&name);
                    }
                }
                widgets.form.oauth_status.set_css_classes(&["success"]);
                widgets
                    .form
                    .oauth_status
                    .set_label(&i18n("✓ Signed in. Save the account to finish."));
            }
            Err(e) => {
                widgets.form.oauth_status.set_css_classes(&["error"]);
                widgets
                    .form
                    .oauth_status
                    .set_label(&i18n_f("Sign-in failed: {e}", &[("e", &(e).to_string())]));
            }
        }
    }

    pub(super) fn on_directory_consent(
        &mut self,
        result: SignInResult,
        widgets: &AccountsWindowWidgets,
    ) {
        widgets.form.graph_directory_row.set_sensitive(true);
        widgets.form.oauth_signin_btn.set_sensitive(true);
        widgets.form.graph_status.set_visible(true);
        match self.take_consent(widgets, result) {
            Ok(()) => {
                self.directory_granted = true;
                widgets.form.graph_status.set_css_classes(&["success"]);
                widgets.form.graph_status.set_label(&i18n(
                    "Access to the directory approved. Save the account to finish.",
                ));
            }
            Err(e) => {
                self.directory_on = false;
                widgets.form.graph_directory_row.set_active(false);
                widgets.form.graph_status.set_css_classes(&["error"]);
                widgets.form.graph_status.set_label(&i18n_f(
                    "Access to the directory was not approved: {e}",
                    &[("e", &e)],
                ));
            }
        }
    }

    pub(super) fn on_labels_consent(
        &mut self,
        result: SignInResult,
        widgets: &AccountsWindowWidgets,
    ) {
        widgets.form.graph_labels_row.set_sensitive(true);
        widgets.form.oauth_signin_btn.set_sensitive(true);
        widgets.form.graph_status.set_visible(true);
        match self.take_consent(widgets, result) {
            Ok(()) => {
                self.labels_granted = true;
                widgets.form.graph_status.set_css_classes(&["success"]);
                widgets.form.graph_status.set_label(&i18n(
                    "Access to the sensitivity labels approved. Save the account to finish.",
                ));
            }
            Err(e) => {
                self.labels_on = false;
                widgets.form.graph_labels_row.set_active(false);
                widgets.form.graph_status.set_css_classes(&["error"]);
                widgets.form.graph_status.set_label(&i18n_f(
                    "Access to the sensitivity labels was not approved: {e}",
                    &[("e", &e)],
                ));
            }
        }
    }
}
