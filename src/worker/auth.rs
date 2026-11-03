//! Credentials at connect time: OAuth access tokens (refreshed and cached
//! per refresh token) and the password, read again from GOA or the keyring.

use std::time::Duration;

use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};
use crate::worker::WorkerEvent;

/// The SASL identity for XOAUTH2 (the IMAP/SMTP username, else the email).
pub(super) fn oauth_user(account: &AccountConfig) -> String {
    if account.username.trim().is_empty() {
        account.email.clone()
    } else {
        account.username.clone()
    }
}

/// Fetch a fresh OAuth access token for this account — from GNOME Online Accounts
/// (imported) or by refreshing a natively-added account's stored refresh token.
pub(super) async fn fetch_oauth_token(account: &AccountConfig) -> Option<String> {
    // The keyring and the remembered tokens answer at once; only a refresh
    // goes over the network, and it goes off the async threads.
    if account.goa_id.is_none() {
        let settings = account.oauth_settings.clone()?;
        let refresh = crate::config::load_oauth_refresh(&account.email)?;
        if let Some(token) = cached_access_token(&refresh) {
            return Some(token);
        }
        let account = account.clone();
        return tokio::task::spawn_blocking(move || {
            refreshed_access_token(&account, &settings, refresh)
        })
        .await
        .ok()
        .flatten();
    }
    let account = account.clone();
    tokio::task::spawn_blocking(move || oauth_token_blocking(&account))
        .await
        .ok()
        .flatten()
}

/// [`fetch_oauth_token`] for a plain thread (blocking): the composer's
/// directory search asks from one (#410). It shares the remembered access
/// tokens, so it seldom costs a refresh of its own.
pub(crate) fn oauth_token_blocking(account: &AccountConfig) -> Option<String> {
    if let Some(goa_id) = account.goa_id.as_deref() {
        return crate::services::goa::oauth_token(goa_id);
    }
    // Natively-added OAuth account: refresh with the keyring-stored refresh token.
    // The client credentials saved with the account MUST be used — a refresh token
    // is bound to the OAuth client that issued it, so switching clients requires
    // re-adding the account (a fresh sign-in), not swapping creds here.
    let settings = account.oauth_settings.as_ref()?;
    let refresh = crate::config::load_oauth_refresh(&account.email)?;
    if let Some(token) = cached_access_token(&refresh) {
        return Some(token);
    }
    refreshed_access_token(account, settings, refresh)
}

/// A new access token from the provider for `refresh` (blocking).
fn refreshed_access_token(
    account: &AccountConfig,
    settings: &crate::config::OAuthSettings,
    refresh: String,
) -> Option<String> {
    let email = &account.email;
    let user = oauth_user(account);
    let fresh = crate::services::oauth::refresh_access_token_as(settings, &refresh, Some(&user))
        .map_err(|e| tracing::warn!("OAuth token refresh for {email} failed: {e}"))
        .ok()?;
    // The provider spent the old refresh token and handed out another:
    // that one is what works from now on.
    let refresh = match fresh.refresh_token {
        Some(rotated) => match crate::config::store_oauth_refresh(email, &rotated) {
            Ok(()) => rotated,
            Err(e) => {
                tracing::warn!("could not keep the new OAuth refresh token for {email}: {e}");
                refresh
            }
        },
        None => refresh,
    };
    if let Some(secs) = fresh.expires_in {
        remember_access_token(&refresh, &fresh.access_token, secs);
    }
    Some(fresh.access_token)
}

/// Access tokens minted for natively-added OAuth accounts, by the refresh
/// token they came from, with when to stop using them. Every IMAP
/// connection, SMTP send and Graph request asked the provider for a new one
/// before; a token is good for an hour. Keyed by refresh token, so a fresh
/// sign-in never picks up the old account's token.
static ACCESS_TOKENS: std::sync::Mutex<
    Option<std::collections::HashMap<String, (String, std::time::Instant)>>,
> = std::sync::Mutex::new(None);

/// How long before its stated expiry a token is no longer handed out, so a
/// long sync that starts with it does not run past the end.
const ACCESS_TOKEN_MARGIN: u64 = 5 * 60;

fn cached_access_token(refresh: &str) -> Option<String> {
    let mut g = ACCESS_TOKENS.lock().ok()?;
    let map = g.get_or_insert_with(Default::default);
    match map.get(refresh) {
        Some((token, until)) if std::time::Instant::now() < *until => Some(token.clone()),
        Some(_) => {
            map.remove(refresh);
            None
        }
        None => None,
    }
}

fn remember_access_token(refresh: &str, token: &str, expires_in: u64) {
    let Some(secs) = expires_in
        .checked_sub(ACCESS_TOKEN_MARGIN)
        .filter(|s| *s > 0)
    else {
        return;
    };
    if let Ok(mut g) = ACCESS_TOKENS.lock() {
        let until = std::time::Instant::now() + Duration::from_secs(secs);
        g.get_or_insert_with(Default::default)
            .insert(refresh.to_string(), (token.to_string(), until));
    }
}

/// Stop handing out this account's remembered access token: the server
/// turned it down, so the next sign-in asks the provider for a new one.
pub(crate) fn forget_access_token(account: &AccountConfig) {
    if account.goa_id.is_some() || account.oauth_settings.is_none() {
        return;
    }
    let Some(refresh) = crate::config::load_oauth_refresh(&account.email) else {
        return;
    };
    if let Ok(mut g) = ACCESS_TOKENS.lock() {
        if let Some(map) = g.as_mut() {
            map.remove(&refresh);
        }
    }
}

/// The password an account signs in with: the one the worker holds or, when
/// it holds none, the keyring's, read again at every sign-in. The worker reads
/// the keyring once when it starts, and a keyring locked or still starting at
/// login then left the account signing in with an empty password until Hylki
/// restarted (#375). A keyring that cannot be read is reported as such, and
/// no sign-in goes out without the password.
pub(super) fn sign_in_password_blocking(
    account: &AccountConfig,
    smtp: bool,
) -> Result<String, String> {
    let separate = smtp && account.smtp_separate;
    let held = if separate {
        &account.smtp_password
    } else {
        &account.password
    };
    if !held.is_empty() || account.oauth {
        return Ok(held.clone());
    }
    let read = if separate {
        crate::config::read_smtp_password(&account.email)
    } else {
        crate::config::read_password(&account.email)
    };
    read.map(Option::unwrap_or_default).map_err(|e| {
        tracing::warn!("could not read the keyring for {}: {e}", account.email);
        i18n_f(
            "Could not read the password for {email} from the keyring ({e}). Hylki tries again at the next connection.",
            &[("email", &account.email), ("e", &e)],
        )
    })
}

/// [`sign_in_password_blocking`] off the async runtime's threads: a keyring
/// that does not answer holds its caller for the whole D-Bus timeout.
pub(super) async fn sign_in_password(
    account: &AccountConfig,
    smtp: bool,
) -> Result<String, String> {
    let held = if smtp && account.smtp_separate {
        &account.smtp_password
    } else {
        &account.password
    };
    if !held.is_empty() || account.oauth {
        return Ok(held.clone());
    }
    let account = account.clone();
    tokio::task::spawn_blocking(move || sign_in_password_blocking(&account, smtp))
        .await
        .map_err(|e| e.to_string())?
}

/// The account password from the keyring; a legacy plaintext one is moved
/// into the keyring first (and stripped from disk).
pub(super) fn load_or_migrate_password(account: &mut AccountConfig) {
    if account.password.is_empty() {
        if let Some(pw) = crate::config::load_password(&account.email) {
            account.password = pw;
        }
    } else {
        let _ = crate::config::store_password(&account.email, &account.password);
        crate::config::strip_passwords_on_disk();
    }
}

/// How long a keyring read may stay silent before the user is told. It keeps
/// waiting afterwards: a locked keyring answers once it is unlocked.
const KEYRING_SLOW: std::time::Duration = std::time::Duration::from_secs(10);

/// Read `email`'s password (or its separate SMTP one) from the keyring off the
/// async runtime's threads, and report to the UI when the keyring errors or
/// has not answered after [`KEYRING_SLOW`]. `None` when there is no password
/// to use, whatever the reason.
pub(super) async fn read_keyring_password(
    email: &str,
    smtp: bool,
    emit: &impl Fn(WorkerEvent),
) -> Option<String> {
    let owner = email.to_string();
    let mut read = tokio::task::spawn_blocking(move || {
        if smtp {
            crate::config::read_smtp_password(&owner)
        } else {
            crate::config::read_password(&owner)
        }
    });
    let answer = match tokio::time::timeout(KEYRING_SLOW, &mut read).await {
        Ok(answer) => answer,
        Err(_) => {
            emit(WorkerEvent::KeyringFailed {
                email: email.to_string(),
                detail: i18n("The keyring has not answered for 10 seconds. It may be locked and waiting to be unlocked. Hylki keeps waiting."),
            });
            read.await
        }
    };
    match answer {
        Ok(Ok(password)) => password,
        Ok(Err(e)) => {
            emit(WorkerEvent::KeyringFailed {
                email: email.to_string(),
                detail: e,
            });
            None
        }
        Err(e) => {
            emit(WorkerEvent::KeyringFailed {
                email: email.to_string(),
                detail: format!("the keyring read failed: {e}"),
            });
            None
        }
    }
}

/// The separate SMTP password from the keyring, when the account has one.
pub(super) fn load_smtp_password(account: &mut AccountConfig) {
    if account.smtp_separate && account.smtp_password.is_empty() {
        if let Some(pw) = crate::config::load_smtp_password(&account.email) {
            account.smtp_password = pw;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::worker::test_support::sample_account;

    /// A password the worker holds is used as it is, and an OAuth account
    /// needs none, so neither reads the keyring (#375).
    #[test]
    fn sign_in_password_prefers_the_held_one() {
        let held = AccountConfig {
            password: "pw".into(),
            smtp_separate: true,
            smtp_password: "smtp".into(),
            ..sample_account()
        };
        assert_eq!(sign_in_password_blocking(&held, false).as_deref(), Ok("pw"));
        assert_eq!(
            sign_in_password_blocking(&held, true).as_deref(),
            Ok("smtp")
        );
        let oauth = AccountConfig {
            oauth: true,
            password: String::new(),
            ..sample_account()
        };
        assert_eq!(sign_in_password_blocking(&oauth, false).as_deref(), Ok(""));
    }
}
