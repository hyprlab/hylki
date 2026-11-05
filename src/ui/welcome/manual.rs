//! The manual account form: its providers, testing and adding an account,
//! and the browser sign-in for OAuth providers.

use super::*;

/// The providers the manual form offers: the shared table less Google and
/// Microsoft, which come in through the Online Accounts page before it.
pub(super) fn wizard_providers() -> impl Iterator<Item = &'static Provider> {
    PROVIDERS.iter().filter(|p| p.wizard_listed())
}

/// The manual form's provider at a drop-down index.
fn wizard_provider(sel: u32) -> Option<&'static Provider> {
    wizard_providers().nth(sel as usize)
}

/// Whether the user typed anything into the manual form. Servers count only
/// where they differ from what the chosen provider fills in, and the ports,
/// always filled in, not at all.
pub(super) fn form_has_input(widgets: &WelcomeWidgets) -> bool {
    let (preset_host, _, preset_smtp, _) = wizard_provider(widgets.provider_row.selected())
        .map(|p| p.wizard_servers())
        .unwrap_or_default();
    let typed = |row: &adw::EntryRow| row.is_visible() && !row.text().trim().is_empty();
    typed(&widgets.name_row)
        || typed(&widgets.email_row)
        || typed(widgets.pass_row.upcast_ref())
        || typed(&widgets.totp_row)
        || typed(&widgets.oauth_client_id_row)
        || typed(widgets.oauth_secret_row.upcast_ref())
        || typed(&widgets.oauth_auth_url_row)
        || typed(&widgets.oauth_token_url_row)
        || typed(&widgets.oauth_scope_row)
        || widgets.host_row.text().trim() != preset_host
        || (widgets.smtp_row.is_visible() && widgets.smtp_row.text().trim() != preset_smtp)
}

impl Welcome {
    /// Custom OAuth: sign in through the browser with the form's client,
    /// then add the account with the refresh token that comes back (the
    /// connection test is skipped, as in Settings: it reads the token from
    /// the keyring, where it lands only once the account is saved).
    fn sign_in_and_add(
        &mut self,
        widgets: &WelcomeWidgets,
        sender: &ComponentSender<Self>,
        email: String,
        host: String,
        smtp: String,
    ) {
        let text = |row: &adw::EntryRow| row.text().trim().to_string();
        let builtin =
            wizard_provider(widgets.provider_row.selected()).and_then(|p| p.wizard_builtin_oauth());
        let protocol = if builtin.is_some() {
            Protocol::Graph
        } else {
            Protocol::Imap
        };
        let settings = builtin.unwrap_or_else(|| crate::config::OAuthSettings {
            auth_url: text(&widgets.oauth_auth_url_row),
            token_url: text(&widgets.oauth_token_url_row),
            client_id: text(&widgets.oauth_client_id_row),
            client_secret: widgets.oauth_secret_row.text().to_string(),
            scopes: text(&widgets.oauth_scope_row),
            redirect_uri: String::new(),
        });
        // Microsoft needs nothing typed: the sign-in says who it is.
        if protocol != Protocol::Graph
            && (email.is_empty()
                || host.is_empty()
                || settings.client_id.is_empty()
                || settings.auth_url.is_empty()
                || settings.token_url.is_empty())
        {
            widgets
                .status_lbl
                .set_css_classes(&["welcome-hint", "error"]);
            widgets.status_lbl.set_text(&i18n(
                "Enter your email address, the client ID and both OAuth URLs first.",
            ));
            return;
        }
        let account = AccountConfig {
            name: widgets.name_row.text().trim().to_string(),
            email: email.clone(),
            protocol,
            imap_host: if protocol == Protocol::Graph {
                String::new()
            } else {
                host
            },
            imap_port: widgets.port_row.text().trim().parse().unwrap_or(993),
            smtp_host: if protocol == Protocol::Graph {
                String::new()
            } else {
                smtp
            },
            smtp_port: widgets.smtp_port_row.text().trim().parse().unwrap_or(587),
            username: email,
            oauth: true,
            oauth_settings: Some(settings.clone()),
            ..blank_account()
        };
        self.test_seq += 1;
        let seq = self.test_seq;
        widgets.add_btn.set_sensitive(false);
        widgets.test_spinner.set_visible(true);
        widgets.test_spinner.start();
        widgets.status_lbl.set_css_classes(&["welcome-hint"]);
        widgets
            .status_lbl
            .set_text(&i18n("Opening browser… complete sign-in there."));
        sender.oneshot_command(async move {
            let result = tokio::task::spawn_blocking(move || {
                let refresh = crate::services::oauth::run_flow(&settings)?.refresh_token;
                if settings.token_url.contains("microsoftonline") {
                    let (name, address, refresh) =
                        crate::services::oauth::microsoft_whoami(&settings, &refresh, None)?;
                    return Ok((refresh, Some((name, address))));
                }
                Ok((refresh, None))
            })
            .await
            .unwrap_or_else(|_| Err("sign-in task failed".into()));
            WelcomeCmd::SignedIn {
                account: Box::new(account),
                result,
                seq,
            }
        });
    }

    /// Tuta: sign in through the helper, which opens the session the
    /// account keeps in place of the password. A second factor brings out
    /// the code row, and the next Test & Add sends the code with it.
    fn sign_in_with_helper(
        &mut self,
        widgets: &WelcomeWidgets,
        sender: &ComponentSender<Self>,
        email: String,
        password: String,
    ) {
        if email.is_empty() || password.is_empty() {
            widgets
                .status_lbl
                .set_css_classes(&["welcome-hint", "error"]);
            widgets
                .status_lbl
                .set_text(&i18n("Enter your email address and password first."));
            return;
        }
        let account = AccountConfig {
            name: widgets.name_row.text().trim().to_string(),
            email: email.clone(),
            protocol: Protocol::Tuta,
            imap_port: 0,
            smtp_port: 0,
            username: email.clone(),
            ..blank_account()
        };
        let code = totp_code(&widgets.totp_row);
        self.test_seq += 1;
        let seq = self.test_seq;
        widgets.test_spinner.set_visible(true);
        widgets.test_spinner.start();
        widgets.status_lbl.set_css_classes(&["welcome-hint"]);
        widgets.status_lbl.set_text(&i18n("Signing in…"));
        sender.oneshot_command(async move {
            let result = tokio::task::spawn_blocking(move || {
                worker::helper_login_blocking(Protocol::Tuta, &email, &password, code.as_deref())
            })
            .await
            .unwrap_or_else(|_| {
                Err(worker::HelperError {
                    code: "helper_failed".into(),
                })
            });
            WelcomeCmd::HelperSignedIn {
                account: Box::new(account),
                result,
                seq,
            }
        });
    }
}

impl Welcome {
    pub(super) fn on_provider_changed(&mut self, widgets: &WelcomeWidgets) {
        let sel = widgets.provider_row.selected();
        let oauth = wizard_provider(sel).is_some_and(|p| p.wizard_is_oauth());
        // Microsoft brings its own client and servers (#329): only
        // the address is asked for, and even that is filled in by
        // the sign-in.
        let custom = wizard_provider(sel).is_some_and(|p| p.wizard_is_custom_oauth());
        let builtin = wizard_provider(sel).is_some_and(|p| p.wizard_builtin_oauth().is_some());
        // Tuta has no servers to show, and asks for a code only once a
        // sign-in comes back wanting one.
        let helper = wizard_provider(sel).is_some_and(|p| p.wizard_protocol() == Protocol::Tuta);
        widgets.totp_row.set_text("");
        widgets.totp_row.set_visible(false);
        // A newer choice supersedes a sign-in still waiting in the
        // browser; its late result is ignored.
        self.test_seq += 1;
        widgets.test_spinner.stop();
        widgets.test_spinner.set_visible(false);
        widgets.add_btn.set_sensitive(true);
        widgets.status_lbl.set_text("");
        widgets.pass_row.set_visible(!oauth);
        for row in [
            widgets.oauth_client_id_row.upcast_ref::<gtk::Widget>(),
            widgets.oauth_secret_row.upcast_ref(),
            widgets.oauth_auth_url_row.upcast_ref(),
            widgets.oauth_token_url_row.upcast_ref(),
            widgets.oauth_scope_row.upcast_ref(),
        ] {
            row.set_visible(custom);
        }
        widgets.server_exp.set_visible(!builtin && !helper);
        widgets.add_btn.set_label(&if oauth {
            i18n("Sign In & Add")
        } else {
            i18n("Test & Add")
        });
        if let Some(p) = wizard_provider(sel) {
            let (ih, ip, sh, sp) = p.wizard_servers();
            let jmap = p.wizard_protocol() == Protocol::Jmap;
            if !ih.is_empty() {
                widgets.host_row.set_text(ih);
                widgets.port_row.set_text(&ip.to_string());
                widgets.smtp_row.set_text(sh);
                widgets.smtp_port_row.set_text(&sp.to_string());
                widgets.server_exp.set_expanded(false);
            } else {
                widgets.host_row.set_text("");
                widgets.smtp_row.set_text("");
                widgets.port_row.set_text(if jmap { "443" } else { "993" });
                widgets.server_exp.set_expanded(true);
            }
            // A JMAP server is one address, and it sends the mail
            // itself: no SMTP rows.
            widgets.host_row.set_title(&if jmap {
                i18n("Server (host name or URL)")
            } else {
                i18n("IMAP server")
            });
            widgets.port_row.set_title(&if jmap {
                i18n("Port")
            } else {
                i18n("IMAP port")
            });
            widgets.smtp_row.set_visible(!jmap);
            widgets.smtp_port_row.set_visible(!jmap);
            // Fastmail over JMAP takes an API token, not the password (#356).
            widgets.pass_row.set_title(&if p.wizard_token() {
                i18n("API Token")
            } else {
                i18n("Password")
            });
            let hint = p.wizard_hint();
            widgets.hint_lbl.set_visible(!hint.is_empty());
            widgets.hint_lbl.set_text(&i18n(hint));
        }
    }

    pub(super) fn on_test_and_add(
        &mut self,
        widgets: &WelcomeWidgets,
        sender: ComponentSender<Self>,
    ) {
        let email = widgets.email_row.text().trim().to_string();
        let password = widgets.pass_row.text().to_string();
        // Derive missing servers from the address's domain — the common
        // convention, and the expander is right there to correct it.
        let provider = wizard_provider(widgets.provider_row.selected());
        let oauth = provider.is_some_and(|p| p.wizard_is_oauth());
        let protocol = provider.map(|p| p.wizard_protocol()).unwrap_or_default();
        if protocol == Protocol::Tuta {
            self.sign_in_with_helper(widgets, &sender, email, password);
            return;
        }
        let domain = email.split('@').nth(1).unwrap_or("").to_string();
        let mut host = widgets.host_row.text().trim().to_string();
        // A JMAP server's address is the user's to give; imap.<domain>
        // is the convention only for IMAP.
        if host.is_empty() && !domain.is_empty() && protocol != Protocol::Jmap {
            host = format!("imap.{domain}");
            widgets.host_row.set_text(&host);
        }
        let mut smtp = widgets.smtp_row.text().trim().to_string();
        if smtp.is_empty() && !domain.is_empty() && protocol != Protocol::Jmap {
            smtp = format!("smtp.{domain}");
            widgets.smtp_row.set_text(&smtp);
        }
        if oauth {
            self.sign_in_and_add(widgets, &sender, email, host, smtp);
            return;
        }
        if email.is_empty() || password.is_empty() || host.is_empty() {
            widgets
                .status_lbl
                .set_css_classes(&["welcome-hint", "error"]);
            widgets
                .status_lbl
                .set_text(&i18n("Enter your email address and password first."));
            return;
        }
        let account = AccountConfig {
            name: widgets.name_row.text().trim().to_string(),
            email: email.clone(),
            protocol,
            imap_host: host,
            imap_port: widgets
                .port_row
                .text()
                .trim()
                .parse()
                .unwrap_or(if protocol == Protocol::Jmap { 443 } else { 993 }),
            smtp_host: smtp,
            smtp_port: widgets.smtp_port_row.text().trim().parse().unwrap_or(587),
            username: email,
            password,
            jmap_token: provider.is_some_and(|p| p.wizard_token()),
            jmap_generic: provider.is_some_and(|p| p.wizard_jmap_generic()),
            ..blank_account()
        };
        self.test_seq += 1;
        let seq = self.test_seq;
        widgets.test_spinner.set_visible(true);
        widgets.test_spinner.start();
        widgets.status_lbl.set_css_classes(&["welcome-hint"]);
        widgets
            .status_lbl
            .set_text(&i18n("Checking the connection…"));
        sender.oneshot_command(async move {
            let test = {
                let account = account.clone();
                tokio::task::spawn_blocking(move || worker::test_connection_blocking(account))
                    .await
                    .unwrap_or_else(|_| ConnTest {
                        incoming: Err("test could not run".into()),
                        smtp: Err("test could not run".into()),
                    })
            };
            WelcomeCmd::Tested {
                account: Box::new(account),
                result: test,
                seq,
            }
        });
    }

    pub(super) fn on_tested(
        &mut self,
        account: Box<AccountConfig>,
        result: ConnTest,
        seq: u32,
        widgets: &WelcomeWidgets,
        sender: ComponentSender<Self>,
    ) {
        if seq != self.test_seq {
            // A newer test was started (the user corrected a field and
            // clicked again); this stale verdict no longer matters.
            return;
        }
        widgets.test_spinner.stop();
        widgets.test_spinner.set_visible(false);
        let email = account.email.clone();
        match (&result.incoming, &result.smtp) {
            (Ok(_), Ok(_)) => {
                self.added.push(account.email.clone());
                widgets
                    .status_lbl
                    .set_css_classes(&["welcome-hint", "success"]);
                widgets.status_lbl.set_text(&i18n_f(
                    "✓ {email} — connected and added",
                    &[("email", &(email).to_string())],
                ));
                widgets.pass_row.set_text("");
                widgets.email_row.set_text("");
                let _ = sender.output(WelcomeOutput::AddAccount(account));
                self.manual_added = true;
                self.refresh_footer(widgets);
            }
            (Err(e), _) => {
                widgets
                    .status_lbl
                    .set_css_classes(&["welcome-hint", "error"]);
                widgets.status_lbl.set_text(&i18n_f(
                    "{email} — mail server: {e}",
                    &[("email", &(email).to_string()), ("e", &(e).to_string())],
                ));
            }
            (_, Err(e)) => {
                widgets
                    .status_lbl
                    .set_css_classes(&["welcome-hint", "error"]);
                widgets.status_lbl.set_text(&i18n_f(
                    "{email} — sending (SMTP): {e}",
                    &[("email", &(email).to_string()), ("e", &(e).to_string())],
                ));
            }
        }
    }

    pub(super) fn on_signed_in(
        &mut self,
        mut account: Box<AccountConfig>,
        result: Result<(String, Option<(String, String)>), String>,
        seq: u32,
        widgets: &WelcomeWidgets,
        sender: ComponentSender<Self>,
    ) {
        if seq != self.test_seq {
            return;
        }
        widgets.test_spinner.stop();
        widgets.test_spinner.set_visible(false);
        widgets.add_btn.set_sensitive(true);
        match result {
            Ok((refresh, who)) => {
                // Microsoft's account is named for the mailbox the
                // sign-in reached (#329).
                if let Some((name, address)) = who {
                    if !address.is_empty() {
                        account.email = address.clone();
                        account.username = address;
                    }
                    if account.name.is_empty() {
                        account.name = name;
                    }
                }
                let email = account.email.clone();
                account.oauth_refresh = refresh;
                self.added.push(email.clone());
                widgets
                    .status_lbl
                    .set_css_classes(&["welcome-hint", "success"]);
                widgets.status_lbl.set_text(&i18n_f(
                    "✓ {email} — signed in and added",
                    &[("email", &email)],
                ));
                widgets.email_row.set_text("");
                let _ = sender.output(WelcomeOutput::AddAccount(account));
                self.manual_added = true;
                self.refresh_footer(widgets);
            }
            Err(e) => {
                widgets
                    .status_lbl
                    .set_css_classes(&["welcome-hint", "error"]);
                widgets
                    .status_lbl
                    .set_text(&i18n_f("Sign-in failed: {e}", &[("e", &e)]));
            }
        }
    }

    pub(super) fn on_helper_signed_in(
        &mut self,
        mut account: Box<AccountConfig>,
        result: Result<String, worker::HelperError>,
        seq: u32,
        widgets: &WelcomeWidgets,
        sender: ComponentSender<Self>,
    ) {
        if seq != self.test_seq {
            return;
        }
        widgets.test_spinner.stop();
        widgets.test_spinner.set_visible(false);
        let session = match result {
            Ok(session) => session,
            Err(e) => return show_sign_in_failure(widgets, &e),
        };
        // The session is what the account keeps; the password it bought
        // is not stored, and is cleared from the form with the code.
        account.helper_session = session;
        let email = account.email.clone();
        self.added.push(email.clone());
        widgets
            .status_lbl
            .set_css_classes(&["welcome-hint", "success"]);
        widgets.status_lbl.set_text(&i18n_f(
            "✓ {email} — signed in and added",
            &[("email", &email)],
        ));
        widgets.pass_row.set_text("");
        widgets.totp_row.set_text("");
        widgets.totp_row.set_visible(false);
        widgets.email_row.set_text("");
        let _ = sender.output(WelcomeOutput::AddAccount(account));
        self.manual_added = true;
        self.refresh_footer(widgets);
    }
}

/// Why a Tuta sign-in did not finish, under the button. A second factor is
/// a step rather than a failure: the code row comes out, and the password
/// stays so the next Test & Add can send both. The helper's text already
/// says what failed, so it stands alone.
fn show_sign_in_failure(widgets: &WelcomeWidgets, error: &worker::HelperError) {
    let text = reveal_code_row(&widgets.totp_row, error);
    let classes: &[&str] = if error.needs_second_factor() {
        &["welcome-hint"]
    } else {
        &["welcome-hint", "error"]
    };
    widgets.status_lbl.set_css_classes(classes);
    widgets.status_lbl.set_text(&text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wizard_lists_every_provider_but_google() {
        assert_eq!(wizard_providers().count(), PROVIDERS.len() - 1);
        assert!(wizard_providers().all(|p| p.wizard_label() != "Google (Gmail)"));
        assert!(wizard_provider(0).is_some_and(|p| p.wizard_is_manual()));
        assert!(wizard_provider(PROVIDERS.len() as u32).is_none());
    }

    /// The wizard's form for Tuta: no servers, the code row hidden until a
    /// sign-in asks for a second factor, and hidden again for another
    /// provider. Ignored by default: it needs a display. Run it on its own
    /// (GTK is initialised once per process, from one thread):
    ///
    /// ```text
    /// cargo test -- --ignored the_wizard_asks_for_a_tuta_code_only_when_needed
    /// ```
    #[test]
    #[ignore = "needs a display"]
    fn the_wizard_asks_for_a_tuta_code_only_when_needed() {
        gtk::init().expect("a display");
        adw::init().expect("libadwaita");
        let settle = || while gtk::glib::MainContext::default().iteration(false) {};
        let wizard = Welcome::builder().launch(()).detach();
        let tuta = wizard_providers()
            .position(|p| p.wizard_protocol() == Protocol::Tuta)
            .expect("Tuta in the wizard") as u32;
        wizard.widgets().provider_row.set_selected(tuta);
        settle();
        {
            let w = wizard.widgets();
            // The rows' own visibility: the window itself is never shown.
            assert!(w.email_row.get_visible());
            assert!(w.pass_row.get_visible());
            assert!(!w.server_exp.get_visible());
            assert!(!w.totp_row.get_visible());
            assert_eq!(w.add_btn.label().as_deref(), Some("Test & Add"));

            // Tuta wants a second factor: the code row comes out, with a
            // hint rather than an error.
            w.email_row.set_text("me@tuta.com");
            w.pass_row.set_text("secret");
            show_sign_in_failure(
                &w,
                &worker::HelperError {
                    code: "second_factor_required".into(),
                },
            );
            assert!(w.totp_row.get_visible());
            assert!(!w.status_lbl.has_css_class("error"));
            assert_eq!(w.pass_row.text(), "secret");
            w.totp_row.set_text("123 456");
            assert_eq!(totp_code(&w.totp_row).as_deref(), Some("123456"));

            // Any other answer is an error, and leaves the row as it was.
            show_sign_in_failure(
                &w,
                &worker::HelperError {
                    code: "invalid_totp".into(),
                },
            );
            assert!(w.status_lbl.has_css_class("error"));
            assert!(w.totp_row.get_visible());
        }
        // Another provider puts the code row away.
        let manual = wizard_providers()
            .position(|p| p.wizard_is_manual())
            .expect("the manual entry") as u32;
        wizard.widgets().provider_row.set_selected(manual);
        settle();
        let w = wizard.widgets();
        assert!(!w.totp_row.get_visible());
        assert!(w.server_exp.get_visible());
    }
}
