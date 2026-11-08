//! Unlocking the mail cache at startup (`cache::at_rest`). Its key is read
//! from the keyring before the window is built; when that fails, nothing
//! opens the cache unencrypted instead, and this asks the user what to do
//! before the app goes on.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;

use crate::cache::at_rest::{self, FileState, Key, KeyStore, Problem};
use crate::i18n::i18n;

mod progress;

/// How long the keyring gets to answer, unlock prompt included, before the
/// user is asked. Nothing is on screen while it waits.
const KEYRING_WAIT: Duration = Duration::from_secs(90);

/// The cache key's place in the system keyring.
struct Keyring;

impl KeyStore for Keyring {
    fn read(&self) -> Result<Option<String>, String> {
        crate::config::read_cache_key(KEYRING_WAIT)
    }

    fn store(&self, key: &Key) -> Result<(), String> {
        crate::config::store_cache_key(key.hex(), KEYRING_WAIT)
    }
}

/// What the user chose for a cache that stays locked.
enum Choice {
    Retry,
    Quit,
    /// Delete the cache and download the mail again.
    Reset,
    /// Turn encryption off, deleting an encrypted cache nothing can open.
    Unencrypted,
}

/// Unlock the cache, asking the user for as long as it cannot be. False
/// when they chose to quit.
pub(crate) fn unlock_cache() -> bool {
    // The demo keeps its mail in memory and never opens the real cache, so
    // it has no key to read; the cache stays locked for its whole run.
    if std::env::var_os("HYLKI_DEMO").is_some() {
        return true;
    }
    let db = match crate::cache::Cache::db_path() {
        Ok(db) => db,
        Err(e) => {
            // Opening fails the same way later, and every caller copes.
            tracing::error!("cache: no place for the mail cache: {e}");
            return true;
        }
    };
    loop {
        let encrypt = crate::config::cache_encrypted();
        let problem = match progress::prepare(&db, encrypt) {
            progress::Outcome::Done(Ok(protection)) => {
                at_rest::unlock(protection);
                return true;
            }
            progress::Outcome::Done(Err(problem)) => problem,
            progress::Outcome::Quit => {
                tracing::info!("cache: quit while the cache was being unlocked");
                return false;
            }
        };
        tracing::error!("cache: {} stays locked: {problem}", db.display());
        let encrypted_file = at_rest::file_state(&db).is_ok_and(|s| s == FileState::Encrypted);
        match ask(&problem, encrypt, encrypted_file) {
            Choice::Retry => {}
            Choice::Quit => return false,
            Choice::Reset => remove(&db),
            Choice::Unencrypted => {
                crate::config::save_cache_encrypted(false);
                if encrypted_file {
                    remove(&db);
                }
            }
        }
    }
}

fn remove(db: &std::path::Path) {
    match at_rest::remove_cache(db) {
        Ok(()) => tracing::warn!("cache: {} removed at the user's request", db.display()),
        Err(e) => tracing::error!("cache: could not remove {}: {e}", db.display()),
    }
}

/// Ask until the user settles on a choice; a destructive one is confirmed
/// first, and a refused confirmation asks again.
fn ask(problem: &Problem, encrypt: bool, encrypted_file: bool) -> Choice {
    if !init_gtk() {
        tracing::error!("cache: no display to ask on; quitting");
        return Choice::Quit;
    }
    // Turning encryption off only helps while the keyring is out of reach
    // and encryption is on; otherwise a fresh cache is what can help.
    let offer_unencrypted = encrypt && matches!(problem, Problem::KeyringUnavailable(_));
    loop {
        let (heading, body) = explain(problem);
        let dialog = adw::AlertDialog::new(Some(&heading), Some(&body));
        dialog.add_response("quit", &i18n("Quit"));
        if offer_unencrypted {
            dialog.add_response("unencrypted", &i18n("Store Unencrypted…"));
        } else {
            dialog.add_response("reset", &i18n("Reset Cache…"));
        }
        dialog.add_response("retry", &i18n("Try Again"));
        dialog.set_response_appearance("retry", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("retry"));
        dialog.set_close_response("quit");
        match choose(&dialog).as_str() {
            "retry" => return Choice::Retry,
            "reset" if confirm_reset() => return Choice::Reset,
            "unencrypted" if confirm_unencrypted(encrypted_file) => return Choice::Unencrypted,
            "reset" | "unencrypted" => {}
            _ => return Choice::Quit,
        }
    }
}

/// Start GTK for a window of this module's own. No window exists yet: GTK
/// is started here only when there is something to show, and the app's own
/// start later finds it running. False when there is no display.
fn init_gtk() -> bool {
    if gtk::init().is_err() {
        return false;
    }
    adw::init().ok();
    true
}

/// The dialog's heading and text for `problem`.
fn explain(problem: &Problem) -> (String, String) {
    match problem {
        Problem::KeyringUnavailable(_) => (
            i18n("Keyring Unavailable"),
            i18n("Mail kept on this computer is encrypted with a key stored in the keyring, and the keyring is locked or did not answer. Unlock it, then try again."),
        ),
        Problem::KeyMissing => (
            i18n("Mail Cache Key Missing"),
            i18n("Mail kept on this computer is encrypted, and the keyring no longer holds its key. If the keyring was replaced, restore the old one and try again. Otherwise reset the cache to download your mail again."),
        ),
        Problem::WrongKey(_) => (
            i18n("Mail Cache Cannot Be Unlocked"),
            i18n("The key in the keyring does not open the mail kept on this computer. If the keyring was replaced, restore the old one and try again. Otherwise reset the cache to download your mail again."),
        ),
        Problem::Conversion(_) => (
            i18n("Mail Cache Not Converted"),
            i18n("The mail kept on this computer could not be converted, and was left as it was. Check that the disk has free space, then try again."),
        ),
        Problem::Storage(_) => (
            i18n("Mail Cache Unreadable"),
            i18n("The mail kept on this computer could not be read. Try again, or reset the cache to download your mail again."),
        ),
    }
}

/// What is lost with the cache, said before anything is deleted.
fn loss_warning() -> String {
    i18n("Mail that exists only on this computer is lost: a POP3 account's mail, and unsent mail in the Outbox.")
}

fn confirm_reset() -> bool {
    let body = format!(
        "{}\n\n{}",
        i18n("Mail kept on this computer is deleted and downloaded again from the server."),
        loss_warning()
    );
    let dialog = adw::AlertDialog::new(Some(&i18n("Reset the Mail Cache?")), Some(&body));
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("reset", &i18n("Reset"));
    dialog.set_response_appearance("reset", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    choose(&dialog) == "reset"
}

/// What storing mail unencrypted gives away; Settings says it too.
pub(crate) fn unencrypted_warning() -> String {
    i18n("Anyone who can read your files, or a backup of them, can then read the mail kept on this computer, including mail that is end-to-end encrypted on the server.")
}

fn confirm_unencrypted(encrypted_file: bool) -> bool {
    let mut body = format!(
        "{} {}",
        unencrypted_warning(),
        i18n("Encryption can be turned back on in Settings → Privacy.")
    );
    if encrypted_file {
        body.push_str("\n\n");
        body.push_str(&i18n(
            "The encrypted cache cannot be opened without the keyring, so it is deleted and your mail is downloaded again.",
        ));
        body.push(' ');
        body.push_str(&loss_warning());
    }
    let dialog = adw::AlertDialog::new(Some(&i18n("Store Mail Unencrypted?")), Some(&body));
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("unencrypted", &i18n("Store Unencrypted"));
    dialog.set_response_appearance("unencrypted", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    choose(&dialog) == "unencrypted"
}

/// Show `dialog` on its own and wait for its answer. There is no window to
/// attach it to yet, and nothing else for the main loop to do meanwhile.
fn choose(dialog: &adw::AlertDialog) -> String {
    let main_loop = gtk::glib::MainLoop::new(None, false);
    let chosen = Rc::new(RefCell::new(String::new()));
    dialog.connect_response(None, {
        let main_loop = main_loop.clone();
        let chosen = chosen.clone();
        move |_, response| {
            *chosen.borrow_mut() = response.to_string();
            main_loop.quit();
        }
    });
    dialog.present(None::<&gtk::Widget>);
    main_loop.run();
    chosen.take()
}
