//! The dialog for a keyring that holds back an account's password (#573):
//! one dialog for every account it concerns, kept up to date while it is
//! open, since a locked keyring keeps every account waiting at once.

use std::cell::RefCell;

use adw::prelude::*;
use relm4::ComponentSender;

use super::{AppModel, AppMsg};
use crate::i18n::{i18n, i18n_f};
use crate::worker::KeyringRead;

/// What the dialog on screen says, and the dialog.
#[derive(Default)]
struct Alert {
    /// Accounts whose read has not answered yet, by label.
    silent: Vec<String>,
    /// Accounts whose read failed, by label, with the keyring's error.
    failed: Vec<(String, String)>,
    #[allow(deprecated)]
    dialog: Option<adw::MessageDialog>,
}

thread_local! {
    /// GTK lives on the main thread, and so does the one dialog.
    static ALERT: RefCell<Alert> = RefCell::default();
}

impl AppModel {
    /// A worker's keyring read went wrong, or answered after all.
    #[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
    pub(super) fn on_keyring_read(
        &self,
        account_id: u32,
        read: KeyringRead,
        sender: &ComponentSender<Self>,
    ) {
        let label = self.account_label(account_id);
        let (silent, failed, dialog) = ALERT.with_borrow_mut(|alert| {
            match read {
                KeyringRead::Silent if !alert.silent.contains(&label) => {
                    alert.silent.push(label);
                }
                KeyringRead::Silent => {}
                KeyringRead::Answered => alert.silent.retain(|l| *l != label),
                KeyringRead::Failed(error) => {
                    alert.silent.retain(|l| *l != label);
                    alert.failed.retain(|(l, _)| *l != label);
                    alert.failed.push((label, error));
                }
            }
            if alert.silent.is_empty() && alert.failed.is_empty() {
                // Every account it waited for has its password: nothing to say.
                return (Vec::new(), Vec::new(), alert.dialog.take());
            }
            (
                alert.silent.clone(),
                alert.failed.clone(),
                alert.dialog.clone(),
            )
        });
        // The dialog's own handler borrows ALERT, so it is driven from here,
        // outside the borrow.
        if silent.is_empty() && failed.is_empty() {
            if let Some(dialog) = dialog {
                dialog.close();
            }
            return;
        }
        let dialog = dialog.unwrap_or_else(|| {
            let dialog = adw::MessageDialog::new(Some(&self.window), None, None);
            dialog.add_response("close", &i18n("Close"));
            dialog.set_close_response("close");
            let s = sender.clone();
            dialog.connect_response(None, move |_, response| {
                ALERT.with_borrow_mut(|alert| *alert = Alert::default());
                if response == "retry" {
                    s.input(AppMsg::ReconnectAccounts);
                }
            });
            ALERT.with_borrow_mut(|alert| alert.dialog = Some(dialog.clone()));
            dialog.present();
            dialog
        });
        // Only an error is worth a retry: a silent keyring is still being
        // waited for, and a new read would queue behind the one that waits.
        if !failed.is_empty() && !dialog.has_response("retry") {
            dialog.add_response("retry", &i18n("Retry"));
            dialog.set_response_appearance("retry", adw::ResponseAppearance::Suggested);
            dialog.set_default_response(Some("retry"));
        }
        dialog.set_heading(Some(&heading(&failed)));
        dialog.set_body(&body(&silent, &failed));
    }
}

fn heading(failed: &[(String, String)]) -> String {
    if failed.is_empty() {
        i18n("Waiting for the keyring")
    } else {
        i18n("Couldn’t read the password")
    }
}

fn body(silent: &[String], failed: &[(String, String)]) -> String {
    // A keyring that is down gives every account the same error: say it once.
    let mut errors: Vec<(&str, Vec<&str>)> = Vec::new();
    for (account, error) in failed {
        match errors.iter_mut().find(|(e, _)| e == error) {
            Some((_, accounts)) => accounts.push(account),
            None => errors.push((error, vec![account])),
        }
    }
    let mut lines: Vec<String> = errors
        .iter()
        .map(|(error, accounts)| {
            i18n_f(
                "The keyring gave an error for {accounts}: {error}",
                &[("accounts", &accounts.join(", ")), ("error", error)],
            )
        })
        .collect();
    if !silent.is_empty() {
        lines.push(i18n_f(
            "The keyring hasn’t answered for {accounts}. Mail connects once it does.",
            &[("accounts", &silent.join(", "))],
        ));
    }
    lines.push(if failed.is_empty() {
        i18n("Unlock the keyring, for example the Login keyring in Passwords and Keys.")
    } else {
        i18n("Unlock the keyring, for example the Login keyring in Passwords and Keys, then choose Retry.")
    });
    lines.join("\n\n")
}
