//! Accounts: saving and removing them, their secrets in the keyring, and
//! keeping GNOME Online Accounts imports in step.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config::{self, AccountConfig};
use crate::i18n::{i18n, i18n_f};
use crate::ui::preferences::PrefOutput;

use super::demo::demo_mode;
use super::folders::apply_folder_roles;
use super::model::AppModel;
use super::msg::AppMsg;
use super::theme::{apply_app_theme, WIZARD_HOLDS_LIGHT};

/// Whether saving `new` over `old` changes only what the app itself draws
/// or puts in the composer: the signature, the color, emoji or picture, the
/// Gravatar switch, the label, or a place in the unified section. Workers
/// never read those, so the account need not reconnect for them (#382). A
/// password counts as changed unless it is the one already stored, which
/// [`secrets_kept`] asks the keyring.
fn look_only_change(old: &AccountConfig, new: &AccountConfig) -> bool {
    let mut probe = new.clone();
    probe.signature = old.signature.clone();
    probe.signature_html = old.signature_html;
    probe.signatures = old.signatures.clone();
    probe.default_signature = old.default_signature;
    probe.color = old.color.clone();
    probe.emoji = old.emoji.clone();
    probe.avatar = old.avatar.clone();
    probe.gravatar = old.gravatar;
    probe.label = old.label.clone();
    probe.in_unified = old.in_unified;
    // Compared as values: a map's order on the way out is no change.
    match (toml::Value::try_from(&probe), toml::Value::try_from(old)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Whether `new` keeps the secrets of `old`. The editor hands every secret
/// back, read from the keyring, while the config in hand may hold none:
/// compare with what is stored. Blocking: run it on the keyring thread.
fn secrets_kept(old: &AccountConfig, new: &AccountConfig) -> bool {
    let kept = |new: &str, old: &str, stored: &dyn Fn() -> Option<String>| {
        new.is_empty() || new == old || stored().as_deref() == Some(new)
    };
    kept(&new.password, &old.password, &|| {
        config::load_password(&old.email)
    }) && kept(&new.smtp_password, &old.smtp_password, &|| {
        config::load_smtp_password(&old.email)
    }) && kept(&new.oauth_refresh, &old.oauth_refresh, &|| {
        config::load_oauth_refresh(&old.email)
    }) && new.aliases.iter().all(|alias| {
        let addr = alias.address();
        let before = old
            .aliases
            .iter()
            .find(|a| a.address().eq_ignore_ascii_case(&addr))
            .map_or("", |a| a.smtp_password.as_str());
        kept(&alias.smtp_password, before, &|| {
            config::load_alias_smtp_password(&old.email, &addr)
        })
    })
}

/// Drop the keyring entries of the accounts `removed` names, on the keyring
/// thread. `config` still holds them: their alias entries are keyed by
/// addresses only it knows.
pub(super) fn forget_account_secrets(removed: &[String], config: &[AccountConfig]) {
    let gone: Vec<(String, Option<AccountConfig>)> = removed
        .iter()
        .map(|email| {
            (
                email.clone(),
                config.iter().find(|c| &c.email == email).cloned(),
            )
        })
        .collect();
    config::keyring_later(move || {
        for (email, account) in gone {
            match account {
                Some(account) => config::delete_account_secrets(&account),
                None => config::delete_password(&email),
            }
        }
    });
}

/// On the way out: hide the windows, then let the keyring thread finish
/// what was queued for it, which a keyring waiting on its user can make take
/// a while. Up to ten seconds, so a keyring that never answers cannot keep
/// the process alive. Nothing queued, nothing waits.
pub(crate) fn finish_keyring_writes() {
    let limit = std::time::Duration::from_secs(10);
    if config::keyring_drain(std::time::Duration::ZERO) {
        return;
    }
    let tops = gtk::Window::toplevels();
    for w in (0..tops.n_items())
        .filter_map(|i| tops.item(i))
        .filter_map(|o| o.downcast::<gtk::Window>().ok())
    {
        w.set_visible(false);
    }
    if let Some(display) = gtk::gdk::Display::default() {
        display.flush();
    }
    tracing::info!("waiting for the keyring before quitting");
    if !config::keyring_drain(limit) {
        tracing::warn!(
            "quit with keyring work still queued: the keyring did not answer in {limit:?}"
        );
    }
}

/// What [`reconcile_goa`] changed: accounts dropped outright (their GOA account
/// is gone), and whether any account was paused or resumed because its Mail
/// service was toggled in GNOME Settings.
#[derive(Default)]
pub(super) struct GoaReconcile {
    pub(super) removed: Vec<String>,
    pub(super) paused_changed: bool,
    /// An account's servers or their security changed in GNOME Settings.
    pub(super) servers_changed: bool,
}

/// Reconcile imported accounts against GNOME Online Accounts: drop the ones
/// whose GOA account no longer exists, and pause — rather than remove — the ones
/// whose Mail service is switched off there, restoring their previous enabled
/// state when it comes back on. Pausing keeps every local setting (label,
/// color, signature, sidebar state) intact. The servers follow GOA's too,
/// so an edit in GNOME Settings needs no re-import (#254). `live` is a snapshot the caller
/// obtained while GOA was reachable — when it isn't, skip reconciliation
/// entirely, so a momentarily-unavailable GOA never wipes imported accounts.
pub(super) fn reconcile_goa(
    config: &mut Vec<AccountConfig>,
    live: &crate::services::goa::GoaLiveState,
) -> GoaReconcile {
    let mut outcome = GoaReconcile::default();
    config.retain(|c| match &c.goa_id {
        Some(id) if !live.account_ids.contains(id) => {
            outcome.removed.push(c.email.clone());
            false
        }
        _ => true,
    });
    for c in config.iter_mut() {
        let Some(id) = c.goa_id.clone() else { continue };
        if let Some(g) = live.mail.get(&id) {
            if g.apply_servers(c) {
                tracing::info!("servers for {} updated from GNOME Online Accounts", c.email);
                outcome.servers_changed = true;
            }
        }
        let mail_disabled = live.disabled_mail_ids.contains(&id);
        if mail_disabled && !c.goa_mail_disabled {
            c.goa_mail_disabled = true;
            c.goa_enabled_before_mail_disabled = c.enabled;
            c.enabled = false;
            outcome.paused_changed = true;
        } else if !mail_disabled && c.goa_mail_disabled {
            c.goa_mail_disabled = false;
            c.enabled = c.goa_enabled_before_mail_disabled;
            outcome.paused_changed = true;
        }
    }
    outcome
}

/// config::save() only logs keyring errors, so confirm the password
/// can actually be read back. If not, the Secret Service is not persisting
/// it: the user is told how to fix it instead of silently "saving" an
/// account that will not stay logged in. Queued after the save's own
/// writes.
fn verify_secret_stored(email: String, secret: String, sender: &ComponentSender<AppModel>) {
    let sender = sender.clone();
    config::keyring_then(
        move || config::load_password(&email).as_deref() == Some(secret.as_str()),
        move |stored| {
            if !stored {
                sender.input(AppMsg::ShowKeyringHelp { problem: true });
            }
        },
    );
}

impl AppModel {
    /// Launch (or re-present) the welcome wizard: the first run's greeting,
    /// the HYLKI_WELCOME review mode, and — on beta builds only — the burger
    /// menu's Welcome Wizard entry for testers.
    pub(super) fn open_wizard(&mut self, sender: &ComponentSender<Self>) {
        use crate::ui::welcome::{Welcome, WelcomeOutput};
        if let Some(w) = self.welcome.as_ref().filter(|w| w.widget().is_visible()) {
            w.widget().present();
            return;
        }
        let welcome =
            Welcome::builder()
                .launch(())
                .forward(sender.input_sender(), |out| match out {
                    WelcomeOutput::AddAccount(account) => AppMsg::AccountSaved {
                        original_email: None,
                        account,
                    },
                    WelcomeOutput::ImportGoa(account) => AppMsg::ImportGoaAccount(account),
                    WelcomeOutput::Prefs(p) => AppMsg::ApplyWelcomePrefs(p),
                    WelcomeOutput::Done => AppMsg::PresentWindow,
                    WelcomeOutput::Language(code) => AppMsg::WizardLanguage(code),
                });
        welcome.widget().set_transient_for(Some(&self.window));
        welcome.widget().set_modal(true);
        // Light only while it is up; the preference returns when it goes,
        // however it goes (finished, closed, or replaced on a restart).
        WIZARD_HOLDS_LIGHT.store(true, std::sync::atomic::Ordering::Relaxed);
        apply_app_theme(self.app_theme);
        welcome.widget().connect_unmap(|_| {
            WIZARD_HOLDS_LIGHT.store(false, std::sync::atomic::Ordering::Relaxed);
            apply_app_theme(config::load_privacy().app_theme);
        });
        // On a true first run the main window stays hidden (see main.rs)
        // until the wizard finishes — or is dismissed.
        {
            let s = sender.clone();
            welcome.widget().connect_close_request(move |_| {
                s.input(AppMsg::PresentWindow);
                gtk::glib::Propagation::Proceed
            });
        }
        welcome.widget().present();
        self.welcome = Some(welcome);
    }

    /// Explain how to set up the system keyring (Secret Service) so passwords
    /// persist across restarts, and — on Linux Mint / Cinnamon — how to stop the
    /// keyring asking for an unlock password at every login.
    ///
    /// `problem` is true when this is shown because a save actually failed;
    /// false for the proactive one-time tip (which offers "Don't show again").
    #[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
    fn show_keyring_help(&self, problem: bool) {
        let mint = crate::desktop::platform::is_mint_cinnamon();

        let heading = if problem {
            i18n("Hylki couldn’t save your password")
        } else {
            i18n("Keyring setup on Linux Mint")
        };

        let mut body = String::new();
        if problem {
            body.push_str(
                "Hylki stores account passwords in the system keyring (the Secret \
                 Service), never on disk. The keyring didn’t accept the password, so \
                 this account won’t stay signed in after you close Hylki.\n\n",
            );
        } else {
            body.push_str(
                "Hylki keeps your account passwords in the system keyring (the Secret \
                 Service) rather than on disk. On Linux Mint with Cinnamon the keyring \
                 sometimes needs a one-time setup so passwords persist — and so it \
                 doesn’t ask you to unlock it at every login.\n\n",
            );
        }

        if mint {
            body.push_str(
                "Set it up:\n\
                 1. Install the keyring tools if needed:\n\
                 \u{2003}sudo apt install gnome-keyring seahorse\n\
                 2. Open “Passwords and Keys” (Seahorse) and make sure a keyring named \
                 “Login” exists and is set as Default (right-click → Set as Default).\n\n\
                 Stop it asking for a password at each login — pick one:\n\
                 • Recommended: set the Login keyring’s password to match your user \
                 login password (right-click the Login keyring → Change Password), and \
                 log in with your password rather than using automatic login. The \
                 keyring then unlocks automatically when you log in.\n\
                 • Or, to remove the prompt entirely even with automatic login: set the \
                 Login keyring’s password to blank (Change Password → leave the new \
                 password empty). This is convenient, but your saved passwords are then \
                 stored unencrypted at rest — only do this on a machine you trust.",
            );
            if crate::desktop::platform::is_flatpak() {
                body.push_str(
                    "\n\nNote: run these steps on the host system (not inside the \
                     Flatpak) — Hylki uses whatever keyring your desktop provides.",
                );
            }
        } else {
            body.push_str(
                "Make sure a Secret Service keyring is installed, running, and \
                 unlocked — for example install “gnome-keyring” and “seahorse” \
                 (Passwords and Keys), then create a default “Login” keyring and set \
                 its password to your login password so it unlocks automatically.",
            );
        }

        let dialog = adw::MessageDialog::new(Some(&self.window), Some(&heading), Some(&body));
        dialog.add_response("ok", &i18n("Got it"));
        dialog.set_default_response(Some("ok"));
        // The proactive tip is a one-time thing: mark it seen once dismissed (by
        // any means) so it never nags again. A real save failure always shows.
        if !problem {
            dialog.connect_response(None, |_, _| config::dismiss_mint_keyring_help());
        }
        dialog.present();
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_accounts(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::OpenAccounts => self.open_settings_window(&sender, true, false),

            AppMsg::AddFirstAccount => self.open_settings_window(&sender, true, true),
            AppMsg::AccountSaved {
                original_email,
                account,
            } => self.on_account_saved(original_email, account, sender),

            AppMsg::ShowKeyringHelp { problem } => self.show_keyring_help(problem),
            AppMsg::ReconnectAccounts => self.reconnect_all(&sender),
            AppMsg::ShowCarryOverNotice(from) => crate::ui::carry_over::show(&self.window, from),

            AppMsg::AccountEnabledChanged { email, enabled } => {
                if let Some(slot) = self.config.iter_mut().find(|c| c.email == email) {
                    if slot.enabled != enabled {
                        slot.enabled = enabled;
                        if let Err(e) = config::save(&self.config) {
                            self.notice_error(i18n_f(
                                "Could not save account: {e}",
                                &[("e", &(e).to_string())],
                            ));
                        }
                        // Respawn workers so the account starts/stops syncing.
                        self.reconnect_all(&sender);
                    }
                }
            }

            AppMsg::ApplyWelcomePrefs(p) => {
                config::mark_wizard_completed();
                // Route every choice through its normal handler (each saves and
                // updates the live UI); drop the wizard controller afterwards.
                sender.input(AppMsg::Pref(PrefOutput::SetAutoRemoteContent(
                    !p.block_remote,
                )));
                sender.input(AppMsg::Pref(PrefOutput::SetGravatar(p.gravatar)));
                sender.input(AppMsg::Pref(PrefOutput::SetSenderLogos(p.sender_logos)));
                sender.input(AppMsg::Pref(PrefOutput::SetNotificationContent(
                    p.notification_content,
                )));
                sender.input(AppMsg::Pref(PrefOutput::SetPreviewLines(p.preview_lines)));
                sender.input(AppMsg::Pref(PrefOutput::SetAvatars(p.avatars)));
                sender.input(AppMsg::Pref(PrefOutput::SetThreading(p.threading)));
                if !p.app_icon.is_empty() {
                    sender.input(AppMsg::Pref(PrefOutput::SetAppIcon(p.app_icon)));
                }
                self.welcome = None;
            }

            AppMsg::ImportGoaAccount(account) => {
                // Enable a GNOME Online Account in Hylki (or re-enable if already
                // imported). Its password came from GOA and is stored in the keyring.
                let email = account.email.clone();
                if let Some(slot) = self.config.iter_mut().find(|c| c.email == email) {
                    slot.enabled = true;
                    // Re-importing refreshes the GOA-owned connection details in
                    // place — e.g. a broken pre-#36 Microsoft 365 import (empty
                    // hosts, IMAP) repairs into a Graph account — keeping the
                    // slot (account ids are config indices) and the cosmetics.
                    if account.goa_id.is_some() {
                        slot.protocol = account.protocol;
                        slot.imap_host = account.imap_host.clone();
                        slot.imap_port = account.imap_port;
                        slot.smtp_host = account.smtp_host.clone();
                        slot.smtp_port = account.smtp_port;
                        slot.username = account.username.clone();
                        slot.smtp_separate = account.smtp_separate;
                        slot.smtp_username = account.smtp_username.clone();
                        slot.oauth = account.oauth;
                        slot.oauth_settings = account.oauth_settings.clone();
                        slot.oauth_refresh = account.oauth_refresh.clone();
                        slot.goa_id = account.goa_id.clone();
                    }
                } else {
                    self.config.push(*account);
                }
                match config::save(&self.config) {
                    Ok(()) => self.reconnect_all(&sender),
                    Err(e) => self.notice_error(i18n_f(
                        "Could not import account: {e}",
                        &[("e", &(e).to_string())],
                    )),
                }
            }

            AppMsg::ImportLocalMail { sources, mbox } => {
                let local = self
                    .config
                    .iter()
                    .position(|c| c.is_local() && c.enabled)
                    .map(|i| i as u32 + 1);
                match local {
                    Some(id) => {
                        self.send_to(id, crate::worker::MailRequest::ImportMail { sources, mbox })
                    }
                    None => self.notice_error(i18n("On This Computer is switched off")),
                }
            }

            AppMsg::AccountRemoved { email } => {
                // Drop the keyring entries while the config (which knows the
                // alias addresses) is still around.
                forget_account_secrets(std::slice::from_ref(&email), &self.config);
                self.config.retain(|c| c.email != email);
                self.account_order.retain(|e| *e != email);
                self.collapsed.retain(|e| *e != email);
                if let Err(e) = config::save(&self.config) {
                    tracing::error!("could not save config: {e}");
                }
                self.save_sidebar_state();
                self.reconnect_all(&sender);
            }

            AppMsg::GoaChanged(live) => {
                // An account added there shows in Settings' import list.
                if let Some(acc) = &self.accounts_win {
                    acc.emit(crate::ui::accounts::AccountsInput::RefreshGoa);
                }
                // GOA changed in GNOME Settings. Drop any imported account that
                // no longer exists there; pause/resume any whose Mail service
                // was toggled. (Adding an account to GOA never auto-imports —
                // that stays a manual choice.)
                // Snapshot first: reconcile removes accounts from the config,
                // and the alias-password keyring entries are keyed by data
                // (the alias addresses) only the config holds.
                let before = self.config.clone();
                let outcome = reconcile_goa(&mut self.config, &live);
                if !outcome.removed.is_empty() {
                    forget_account_secrets(&outcome.removed, &before);
                    for email in &outcome.removed {
                        self.account_order.retain(|e| e != email);
                        self.collapsed.retain(|e| e != email);
                        self.folders_expanded.retain(|e| e != email);
                        self.filtered_expanded_accounts.retain(|e| e != email);
                        self.tags_expanded_accounts.retain(|e| e != email);
                    }
                    self.save_sidebar_state();
                }
                if !outcome.removed.is_empty() || outcome.paused_changed || outcome.servers_changed
                {
                    if let Err(e) = config::save(&self.config) {
                        tracing::error!("could not save config after GOA change: {e}");
                    }
                    self.reconnect_all(&sender);
                }
            }

            AppMsg::SetAccount(account) => {
                if let Some(existing) = self.accounts.iter_mut().find(|a| a.id == account.id) {
                    *existing = account;
                } else {
                    // Layout not remembered: an account starts folded up,
                    // wherever it came from (config, GOA, the demo). Only
                    // on arrival, so opening it by hand sticks for the
                    // session.
                    if !self.remember_sidebar && !self.collapsed.contains(&account.email) {
                        self.collapsed.push(account.email.clone());
                    }
                    self.accounts.push(account);
                    self.accounts.sort_by_key(|a| a.id);
                }
                self.rebuild_sidebar();
            }
            _ => unreachable!("not routed to on_accounts"),
        }
    }

    /// An account added or edited in Settings or the welcome wizard.
    fn on_account_saved(
        &mut self,
        original_email: Option<String>,
        account: Box<AccountConfig>,
        sender: ComponentSender<Self>,
    ) {
        // Whether the account joined or left the unified section
        // (#267): the merged views open now are redrawn without it.
        let unified_changed = original_email
            .as_ref()
            .and_then(|orig| self.effective_config().iter().find(|c| &c.email == orig))
            .is_some_and(|old| old.in_unified != account.in_unified);
        if self.config.is_empty() && demo_mode() {
            self.save_demo_account(
                original_email.as_deref(),
                *account,
                unified_changed,
                &sender,
            );
            return;
        }
        let open_unified = self.unified.then_some(self.unified_view);
        let new_email = account.email.clone();
        let display_only = self.look_only_edit(original_email.as_deref(), &account, &sender);
        // Remember the secret we expect to persist, so we can verify the
        // keyring actually stored it (a silent keyring failure would
        // otherwise leave the account unable to log in after a restart).
        let expected_secret = (!account.password.is_empty()).then(|| account.password.clone());
        self.forget_dropped_alias_secrets(original_email.as_deref(), &account);
        self.store_account(original_email, *account);
        match config::save(&self.config) {
            Ok(()) => {
                if let Some(secret) = expected_secret {
                    verify_secret_stored(new_email, secret, &sender);
                }
                self.save_sidebar_state();
                // Changed Special Folders assignments (#82) land on the
                // live lists at once; the reconnect's fresh LIST then
                // confirms them (and restores auto-detection for any
                // role set back to Automatic).
                for (i, cfg) in self.config.iter().enumerate() {
                    if let Some(folders) = self.folders.get_mut(&(i as u32 + 1)) {
                        apply_folder_roles(&cfg.folder_roles, folders);
                    }
                }
                self.rebuild_sidebar();
                // A picture, an emoji or the Gravatar switch may have
                // changed with this edit (#189).
                self.warm_own_gravatars(&sender);
                self.refresh_faces();
                if !display_only {
                    self.reconnect_all(&sender);
                }
                if unified_changed {
                    self.reload_unified_views(open_unified, &sender);
                }
            }
            Err(e) => self.notice_error(i18n_f(
                "Could not save account: {e}",
                &[("e", &(e).to_string())],
            )),
        }
    }

    /// Demo mode: the edit lands on the in-memory stand-in (so a new
    /// color, emoji or picture shows in the sidebar and the reader at
    /// once) and nothing is written or reconnected.
    fn save_demo_account(
        &mut self,
        original_email: Option<&str>,
        account: AccountConfig,
        unified_changed: bool,
        sender: &ComponentSender<Self>,
    ) {
        let slot =
            original_email.and_then(|orig| self.demo_config.iter_mut().find(|c| c.email == orig));
        match slot {
            Some(slot) => *slot = account,
            None => self.demo_config.push(account),
        }
        if let Err(e) = config::save_demo_accounts(&self.demo_config) {
            tracing::warn!("could not save the demo accounts: {e}");
        }
        self.rebuild_sidebar();
        self.warm_own_gravatars(sender);
        self.refresh_faces();
        if unified_changed {
            self.reload_unified_views(self.unified.then_some(self.unified_view), sender);
        }
    }

    /// Whether an edit only changes how the account looks or signs
    /// (#382): it is applied in place, as reconnecting every account for
    /// it emptied the list and the reader until they had synced. Whether
    /// its secrets changed too is the keyring's to say, asked now, before
    /// the save replaces them: the keyring thread answers in order. A
    /// changed one reconnects then.
    fn look_only_edit(
        &self,
        original_email: Option<&str>,
        account: &AccountConfig,
        sender: &ComponentSender<Self>,
    ) -> bool {
        let look_only = original_email
            .and_then(|orig| self.config.iter().find(|c| c.email == orig))
            .filter(|old| look_only_change(old, account))
            .cloned();
        let display_only = look_only.is_some();
        if let Some(old) = look_only {
            let (new, email, sender) = (account.clone(), account.email.clone(), sender.clone());
            config::keyring_then(
                move || secrets_kept(&old, &new),
                move |kept| {
                    if kept {
                        tracing::info!("{email}: saved without reconnecting, only its look or signature changed");
                    } else {
                        sender.input(AppMsg::ReconnectAccounts);
                    }
                },
            );
        }
        display_only
    }

    /// An alias dropped (or switched back to the account's SMTP) in an
    /// edit must not leave its SMTP password behind in the keyring, and
    /// an email rename re-keys every alias entry, so the old ones all go.
    /// config::save() stores the current set fresh.
    fn forget_dropped_alias_secrets(&self, original_email: Option<&str>, account: &AccountConfig) {
        let new_email = &account.email;
        if let Some(orig) = original_email {
            if let Some(old) = self.config.iter().find(|c| c.email == orig) {
                for old_alias in &old.aliases {
                    let kept = old.email == *new_email
                        && account.aliases.iter().any(|n| {
                            n.has_own_smtp()
                                && n.address().eq_ignore_ascii_case(&old_alias.address())
                        });
                    if !kept {
                        let (email, address) = (old.email.clone(), old_alias.address());
                        config::keyring_later(move || {
                            config::delete_alias_smtp_password(&email, &address)
                        });
                    }
                }
            }
        }
    }

    /// Puts the account in the config: over the one it was (matched by
    /// its previous email), else at the end. An email change follows into
    /// the display-order and collapsed lists.
    fn store_account(&mut self, original_email: Option<String>, account: AccountConfig) {
        let new_email = account.email.clone();
        match original_email {
            // Editing an existing account (matched by its previous email).
            Some(orig) => {
                if let Some(slot) = self.config.iter_mut().find(|c| c.email == orig) {
                    *slot = account;
                } else {
                    self.config.push(account);
                }
                // Track an email change in the display-order/collapsed lists.
                if orig != new_email {
                    for e in self
                        .account_order
                        .iter_mut()
                        .chain(self.collapsed.iter_mut())
                    {
                        if *e == orig {
                            *e = new_email.clone();
                        }
                    }
                }
            }
            // Adding a new account.
            None => self.config.push(account),
        }
    }

    /// One-time, dismissible keyring setup tip for Linux Mint / Cinnamon, where
    /// the Secret Service often needs configuring so passwords persist and the
    /// keyring auto-unlocks at login. Only shown once the user actually has an
    /// account (so it isn't the very first thing a new user sees), and never
    /// again after "Don't show again".
    pub(super) fn show_startup_notices(&self, sender: &ComponentSender<Self>) {
        if !self.config.is_empty()
            && crate::desktop::platform::is_mint_cinnamon()
            && !config::mint_keyring_help_dismissed()
        {
            sender.input(AppMsg::ShowKeyringHelp { problem: false });
        }

        // An earlier install's data came across at this start (see
        // `legacy`): say so once, and take its start-at-login over — the
        // portal's autostart entry is per app ID, so it has to be asked
        // for again under this one.
        if let Some(from) = crate::desktop::legacy::migrated_from() {
            if self.run_in_background.get() && self.autostart {
                crate::desktop::background::request(true);
            }
            sender.input(AppMsg::ShowCarryOverNotice(from));
        } else if std::env::var_os("HYLKI_SHOWCASE_CARRY_OVER").is_some() {
            // Opens the notice as if Vireo's data had just come across, for
            // checking it by hand.
            sender.input(AppMsg::ShowCarryOverNotice(
                &crate::desktop::legacy::PREDECESSORS[0],
            ));
        }
    }

    /// With no accounts, no worker events will populate the sidebar, so render
    /// its empty state (the "Add first account" prompt) up front — and greet
    /// a first run with the welcome wizard (src/ui/welcome.rs).
    pub(super) fn open_wizard_if_needed(&mut self, sender: &ComponentSender<Self>) {
        // A restart the wizard's language pick asked for: the wizard comes
        // back once, whatever mode this is; the flag must not reach a
        // later restart (the icon change's), so it is cleared here.
        let wizard_again = std::env::var_os(crate::WIZARD_AGAIN_VAR).is_some();
        if wizard_again {
            std::env::remove_var(crate::WIZARD_AGAIN_VAR);
        }
        if self.config.is_empty() || std::env::var("HYLKI_WELCOME").is_ok() || wizard_again {
            self.rebuild_sidebar();
            // HYLKI_WELCOME=1 forces the wizard over an existing config, for
            // design review and screenshots. A wizard already completed once
            // (Start Reading pressed, even with no account added) doesn't
            // come back on its own — a restart right after it must not loop.
            if std::env::var("HYLKI_WELCOME").is_ok()
                || wizard_again
                || (!demo_mode() && !config::wizard_completed())
            {
                self.open_wizard(sender);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::demo::demo_account_configs;

    /// Saving a new signature or color leaves the account connected (#382);
    /// a server, a name or a new password reconnects it.
    #[test]
    fn display_only_account_edits() {
        let old = demo_account_configs().remove(0);
        let mut new = old.clone();
        new.set_signatures(
            vec![crate::config::NamedSignature {
                name: "Short".into(),
                html: "Jason".into(),
            }],
            None,
        );
        new.color = Some("#ff0000".into());
        new.in_unified = !old.in_unified;
        // The editor hands the secret back; the same one is no change.
        new.password = old.password.clone();
        assert!(super::look_only_change(&old, &new) && super::secrets_kept(&old, &new));
        let mut host = new.clone();
        host.imap_host = "imap.elsewhere.org".into();
        assert!(!(super::look_only_change(&old, &host) && super::secrets_kept(&old, &host)));
        let mut name = new.clone();
        name.name = "Someone Else".into();
        assert!(!(super::look_only_change(&old, &name) && super::secrets_kept(&old, &name)));
        let mut alias = new.clone();
        alias.aliases.push(crate::config::AliasConfig {
            identity: "me@alias.org".into(),
            ..Default::default()
        });
        assert!(!(super::look_only_change(&old, &alias) && super::secrets_kept(&old, &alias)));
    }

    #[test]
    fn goa_reconcile_drops_pauses_and_resumes() {
        let mut config = demo_account_configs();
        config.truncate(3);
        config[0].goa_id = Some("gone".into());
        config[1].goa_id = Some("paused".into());
        config[1].enabled = true;
        config[2].goa_id = None;
        let removed_email = config[0].email.clone();
        let mut live = crate::services::goa::GoaLiveState {
            account_ids: ["paused".to_string()].into(),
            disabled_mail_ids: ["paused".to_string()].into(),
            mail: Default::default(),
        };
        let out = reconcile_goa(&mut config, &live);
        assert_eq!(out.removed, vec![removed_email]);
        assert!(out.paused_changed && !out.servers_changed);
        assert_eq!(config.len(), 2);
        assert!(config[0].goa_mail_disabled && !config[0].enabled);
        assert!(config[0].goa_enabled_before_mail_disabled);
        let again = reconcile_goa(&mut config, &live);
        assert!(!again.paused_changed && again.removed.is_empty());
        live.disabled_mail_ids.clear();
        let back = reconcile_goa(&mut config, &live);
        assert!(back.paused_changed);
        assert!(!config[0].goa_mail_disabled && config[0].enabled);
    }
}
