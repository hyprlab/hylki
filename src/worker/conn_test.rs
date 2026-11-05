//! The connection tests the Accounts window and the welcome assistant run:
//! sign in to each server with the given settings, send nothing.

use lettre::transport::smtp::authentication::Credentials;

use crate::config::AccountConfig;
use crate::worker::auth::{fetch_oauth_token, oauth_user, sign_in_password};
use crate::worker::imap::connect::connect;
use crate::worker::jmap;
use crate::worker::pop3::client::Pop3;
use crate::worker::smtp::{smtp_host, smtp_tls_parameters, SmtpTls};

/// Outcome of a credential/connection test for the Accounts window.
#[derive(Debug)]
pub(crate) struct ConnTest {
    /// Incoming server (IMAP or POP3, per the account's protocol).
    pub(crate) incoming: Result<(), String>,
    pub(crate) smtp: Result<(), String>,
}

/// Test that the account's servers accept the given credentials (no mail sent).
async fn test_connection(account: &AccountConfig) -> ConnTest {
    let incoming = match account.protocol {
        crate::config::Protocol::Pop3 => test_pop3(account).await,
        crate::config::Protocol::Jmap => jmap::test_jmap(account).await,
        crate::config::Protocol::Tuta => crate::worker::tuta::test_tuta(account).await,
        crate::config::Protocol::Proton => crate::worker::proton::test_proton(account).await,
        _ => test_imap(account).await,
    };
    // A JMAP, Tuta or Proton account sends through the server itself:
    // there is no SMTP to test, and the Accounts window leaves that line out.
    let smtp = if matches!(
        account.protocol,
        crate::config::Protocol::Jmap
            | crate::config::Protocol::Tuta
            | crate::config::Protocol::Proton
    ) {
        Ok(())
    } else {
        test_smtp(account).await
    };
    ConnTest { incoming, smtp }
}

async fn test_pop3(account: &AccountConfig) -> Result<(), String> {
    let mut pop = Pop3::connect(account).await?;
    pop.login(&account.username, &sign_in_password(account, false).await?)
        .await?;
    pop.quit().await;
    Ok(())
}

/// Blocking wrapper around [`test_connection`] that spins up its own Tokio
/// runtime — call it from `spawn_blocking` so the IMAP/SMTP sockets have an I/O
/// reactor regardless of the caller's runtime.
pub(crate) fn test_connection_blocking(account: AccountConfig) -> ConnTest {
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt.block_on(test_connection(&account)),
        Err(e) => ConnTest {
            incoming: Err(e.to_string()),
            smtp: Err(e.to_string()),
        },
    }
}

async fn test_imap(account: &AccountConfig) -> Result<(), String> {
    // Stringify the (non-Send) error before any further await so the returned
    // future stays Send (required by relm4's command runner).
    let mut session = connect(account).await.map_err(|e| e.to_string())?;
    let _ = session.logout().await;
    Ok(())
}

/// Connect to the SMTP server and authenticate (then quit) — verifies the send
/// credentials without delivering anything.
async fn test_smtp(account: &AccountConfig) -> Result<(), String> {
    use lettre::transport::smtp::authentication::Mechanism;

    let host = smtp_host(account);
    // Authenticate the same way the send path does: XOAUTH2 with a fresh token
    // for OAuth accounts, otherwise the password (SMTP-specific if configured).
    if !account.oauth && account.security.as_ref().is_some_and(|s| s.smtp_no_auth) {
        return smtp_auth_check(
            &host,
            account.smtp_port,
            None,
            &[],
            SmtpTls::for_account(account),
        )
        .await;
    }
    let (creds, mechanisms): (Credentials, &[Mechanism]) = if account.oauth {
        let token = fetch_oauth_token(account)
            .await
            .ok_or_else(|| "could not get an OAuth token".to_string())?;
        (
            Credentials::new(oauth_user(account), token),
            &[Mechanism::Xoauth2],
        )
    } else {
        let password = sign_in_password(account, true).await?;
        let user = if account.smtp_separate {
            &account.smtp_username
        } else {
            &account.username
        };
        (
            Credentials::new(user.clone(), password),
            &[Mechanism::Plain, Mechanism::Login],
        )
    };
    smtp_auth_check(
        &host,
        account.smtp_port,
        Some(&creds),
        mechanisms,
        SmtpTls::for_account(account),
    )
    .await
}

/// Test a send-as alias's own SMTP server and credentials (#34): connect,
/// authenticate, quit. Used by the alias editor's Test button; an alias without
/// its own transport has nothing of its own to test.
async fn test_alias_smtp(
    account_email: &str,
    alias: &crate::config::AliasConfig,
) -> Result<(), String> {
    use lettre::transport::smtp::authentication::Mechanism;

    let password = if alias.smtp_password.is_empty() {
        crate::config::load_alias_smtp_password(account_email, &alias.address()).unwrap_or_default()
    } else {
        alias.smtp_password.clone()
    };
    let creds = Credentials::new(alias.smtp_username.clone(), password);
    smtp_auth_check(
        alias.smtp_host.trim(),
        alias.smtp_port,
        Some(&creds),
        &[Mechanism::Plain, Mechanism::Login],
        SmtpTls::by_port(
            alias.smtp_port,
            account_accepts_hostname_mismatch(account_email),
        ),
    )
    .await
}

/// The account's name-check waiver (#246), by its address: the alias test
/// is handed the address alone.
fn account_accepts_hostname_mismatch(account_email: &str) -> bool {
    crate::config::load()
        .unwrap_or_default()
        .iter()
        .find(|a| a.email.eq_ignore_ascii_case(account_email))
        .is_some_and(|a| a.tls_accept_hostname_mismatch)
}

/// Blocking wrapper around [`test_alias_smtp`] — call from `spawn_blocking`,
/// like [`test_connection_blocking`].
pub(crate) fn test_alias_smtp_blocking(
    account_email: String,
    alias: crate::config::AliasConfig,
) -> Result<(), String> {
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt.block_on(test_alias_smtp(&account_email, &alias)),
        Err(e) => Err(e.to_string()),
    }
}

/// Shared SMTP credential check: connect (implicit TLS or STARTTLS, as
/// `smtp_tls` says), authenticate unless there are no credentials to, quit.
async fn smtp_auth_check(
    host: &str,
    port: u16,
    creds: Option<&Credentials>,
    mechanisms: &[lettre::transport::smtp::authentication::Mechanism],
    smtp_tls: SmtpTls,
) -> Result<(), String> {
    use lettre::transport::smtp::client::AsyncSmtpConnection;
    use lettre::transport::smtp::extension::ClientId;

    let hello = ClientId::default();
    let tls = smtp_tls_parameters(host, smtp_tls).map_err(|e| e.to_string())?;
    // A (host, port) pair resolves bare IPv6 addresses correctly; a "host:port"
    // string would mis-parse their colons.
    let addr = (host, port);
    let timeout = Some(std::time::Duration::from_secs(20));

    let mut conn = if smtp_tls.implicit {
        AsyncSmtpConnection::connect_tokio1(addr, timeout, &hello, Some(tls), None)
            .await
            .map_err(|e| e.to_string())?
    } else {
        let mut conn = AsyncSmtpConnection::connect_tokio1(addr, timeout, &hello, None, None)
            .await
            .map_err(|e| e.to_string())?;
        conn.starttls(tls, &hello)
            .await
            .map_err(|e| e.to_string())?;
        conn
    };
    let result = match creds {
        Some(creds) => conn
            .auth(mechanisms, creds)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string()),
        None => Ok(()),
    };
    let _ = conn.quit().await;
    result
}
