//! Sending over SMTP with lettre: the transport for an account or an alias
//! with its own server, TLS mode, and which refusals are permanent.

use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

use crate::config::AccountConfig;
use crate::worker::auth::{fetch_oauth_token, oauth_user, sign_in_password};
use crate::worker::compose::build::build_email;
use crate::worker::compose::editable::with_bcc;
use crate::worker::tls::is_loopback_host;
use crate::worker::{offline_text, OutgoingMessage};

pub(super) type SmtpError = Box<dyn std::error::Error + Send + Sync>;

/// How one SMTP endpoint is secured: TLS from the first byte or a STARTTLS
/// upgrade, and how far its certificate is checked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SmtpTls {
    pub(super) implicit: bool,
    pub(super) accept_hostname_mismatch: bool,
    pub(super) accept_invalid_certs: bool,
}

impl SmtpTls {
    /// By the port alone: 465 is implicit TLS, everything else (587, etc.)
    /// STARTTLS.
    pub(super) fn by_port(port: u16, accept_hostname_mismatch: bool) -> Self {
        SmtpTls {
            implicit: port == 465,
            accept_hostname_mismatch,
            accept_invalid_certs: false,
        }
    }

    /// The account's own server: as GNOME Online Accounts records it where
    /// it does (#254), else by the port.
    pub(super) fn for_account(account: &AccountConfig) -> Self {
        let by_port = SmtpTls::by_port(account.smtp_port, account.tls_accept_hostname_mismatch);
        match &account.security {
            Some(sec) => SmtpTls {
                implicit: !sec.smtp_starttls,
                accept_invalid_certs: sec.smtp_accept_invalid_certs,
                ..by_port
            },
            None => by_port,
        }
    }
}

/// TLS settings for an SMTP connection, matching [`tls_connector`]: a local
/// bridge's self-signed certificate is accepted, and one the user accepted
/// in GNOME Settings (#254), a certificate in another name where the
/// account asks for it (#246), every other host is verified.
pub(super) fn smtp_tls_parameters(
    host: &str,
    tls: SmtpTls,
) -> Result<lettre::transport::smtp::client::TlsParameters, lettre::transport::smtp::Error> {
    use lettre::transport::smtp::client::TlsParameters;
    let accept_hostname_mismatch = tls.accept_hostname_mismatch;
    if is_loopback_host(host) || tls.accept_invalid_certs {
        TlsParameters::builder(host.to_string())
            .dangerous_accept_invalid_certs(true)
            .dangerous_accept_invalid_hostnames(true)
            .build()
    } else if accept_hostname_mismatch {
        TlsParameters::builder(host.to_string())
            .dangerous_accept_invalid_hostnames(true)
            .build()
    } else {
        TlsParameters::new(host.to_string())
    }
}

/// A TLS-configured transport builder for one SMTP endpoint. A loopback
/// bridge signs its own certificate (see `is_loopback_host`), so the relay
/// builders' verification would reject it — TLS stays required, only the
/// certificate checks are relaxed.
fn smtp_transport_builder(
    host: &str,
    port: u16,
    smtp_tls: SmtpTls,
) -> Result<lettre::transport::smtp::AsyncSmtpTransportBuilder, SmtpError> {
    let implicit_tls = smtp_tls.implicit;
    let relaxed = smtp_tls.accept_hostname_mismatch || smtp_tls.accept_invalid_certs;
    let builder = if is_loopback_host(host) || relaxed {
        let tls = smtp_tls_parameters(host, smtp_tls)?;
        let mode = if implicit_tls {
            lettre::transport::smtp::client::Tls::Wrapper(tls)
        } else {
            lettre::transport::smtp::client::Tls::Required(tls)
        };
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host).tls(mode)
    } else if implicit_tls {
        AsyncSmtpTransport::<Tokio1Executor>::relay(host)?
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)?
    };
    Ok(builder.port(port))
}

/// An SMTP transport, configured but not yet connected: the account's own, or —
/// when `alias` is given — the alias's separate server and credentials (#34).
async fn smtp_transport(
    account: &AccountConfig,
    alias: Option<&crate::config::AliasConfig>,
) -> Result<AsyncSmtpTransport<Tokio1Executor>, SmtpError> {
    if let Some(alias) = alias {
        // An alias's own transport is always password-authenticated, never the
        // account's OAuth: it is a different provider's server, which knows
        // nothing of the account's tokens. The password lives in the keyring
        // and is fetched here (the in-memory config never carries it).
        let password = if alias.smtp_password.is_empty() {
            crate::config::load_alias_smtp_password(&account.email, &alias.address()).ok_or_else(
                || -> SmtpError {
                    format!(
                        "no SMTP password stored for the alias {} — re-enter it in \
                         Accounts",
                        alias.address()
                    )
                    .into()
                },
            )?
        } else {
            alias.smtp_password.clone()
        };
        let tls = SmtpTls::by_port(alias.smtp_port, account.tls_accept_hostname_mismatch);
        return Ok(
            smtp_transport_builder(alias.smtp_host.trim(), alias.smtp_port, tls)?
                .credentials(Credentials::new(alias.smtp_username.clone(), password))
                .build(),
        );
    }
    let host = smtp_host(account);
    let mut builder =
        smtp_transport_builder(&host, account.smtp_port, SmtpTls::for_account(account))?;
    if account.oauth {
        // XOAUTH2: the "password" is a fresh OAuth token from GOA.
        let token = fetch_oauth_token(account)
            .await
            .ok_or_else(|| -> SmtpError {
                "could not get an OAuth token from GNOME Online Accounts".into()
            })?;
        let user = oauth_user(account);
        builder = builder
            .credentials(Credentials::new(user, token))
            .authentication(vec![
                lettre::transport::smtp::authentication::Mechanism::Xoauth2,
            ]);
    } else if account.security.as_ref().is_some_and(|s| s.smtp_no_auth) {
        // GNOME Online Accounts says this server takes mail without a
        // sign-in (#254); offering one it does not advertise would fail.
    } else {
        // Use the separate SMTP credentials when configured, else the IMAP ones.
        let password = sign_in_password(account, true).await?;
        let creds = if account.smtp_separate {
            Credentials::new(account.smtp_username.clone(), password)
        } else {
            Credentials::new(account.username.clone(), password)
        };
        builder = builder.credentials(creds);
    }
    Ok(builder.build())
}

/// Send an already-built message: the bytes that go on the wire, plus the
/// envelope they are addressed with. Retrying an Outbox message goes through
/// here, so what is retried is byte-for-byte what was composed.
pub(super) async fn send_raw_smtp(
    account: &AccountConfig,
    envelope: &lettre::address::Envelope,
    raw: &[u8],
) -> Result<(), SmtpError> {
    // The envelope sender names the identity this leaves as. When it is an
    // alias with its own SMTP, the mail goes out through that transport (#34);
    // Outbox retries come through here too, so a queued alias send retries on
    // the alias's server.
    let alias = envelope
        .from()
        .and_then(|f| account.alias_with_own_smtp(f.as_ref()));
    let host = alias
        .map(|a| a.smtp_host.trim().to_string())
        .unwrap_or_else(|| smtp_host(account));
    if !crate::desktop::power::reaches(&host) {
        return Err(offline_text().into());
    }
    let mailer = smtp_transport(account, alias).await?;
    // The SMTP leg for the console: size and recipient count, then the verdict.
    tracing::debug!(
        target: "hylki::smtp",
        "> send {} bytes to {} recipient(s) via {}",
        raw.len(),
        envelope.to().len(),
        alias.map(|a| a.smtp_host.as_str()).unwrap_or(&smtp_host(account)),
    );
    let r = mailer.send_raw(envelope, raw).await;
    match &r {
        Ok(_) => tracing::debug!(target: "hylki::smtp", "< OK (send)"),
        Err(e) => tracing::warn!(target: "hylki::smtp", "< {e} (send)"),
    }
    r?;
    Ok(())
}

/// A permanent (5xx) SMTP answer other than a failed sign-in. A wrong
/// password is fixed in Settings and the queued message then goes; a refused
/// recipient never will.
pub(super) fn smtp_refused(e: &SmtpError) -> bool {
    e.downcast_ref::<lettre::transport::smtp::Error>()
        .filter(|e| e.is_permanent())
        .and_then(|e| e.status())
        .is_some_and(|code| !(530..540).contains(&u16::from(code)))
}

/// Send `msg`, and hand back the bytes to file as its Sent copy: what went
/// out, with the Bcc put back (#476).
pub(super) async fn send_smtp(
    account: &AccountConfig,
    msg: &OutgoingMessage,
) -> Result<Vec<u8>, SmtpError> {
    let email = build_email(account, msg)?;
    let raw = email.formatted();
    send_raw_smtp(account, email.envelope(), &raw).await?;
    Ok(with_bcc(&raw, &msg.bcc))
}

/// SMTP host: the configured value, or derived from the IMAP host.
pub(super) fn smtp_host(account: &AccountConfig) -> String {
    let configured = account.smtp_host.trim();
    if !configured.is_empty() {
        configured.to_string()
    } else if let Some(rest) = account.imap_host.strip_prefix("imap") {
        format!("smtp{rest}")
    } else {
        account.imap_host.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::worker::test_support::sample_account;

    #[test]
    fn only_an_alias_with_its_own_smtp_switches_transport() {
        use crate::config::AliasConfig;
        let account = AccountConfig {
            email: "me@example.com".into(),
            aliases: vec![
                AliasConfig {
                    identity: "Plain <plain@fwd.example>".into(),
                    ..Default::default()
                },
                AliasConfig {
                    identity: "Ann Work <ann@work.example>".into(),
                    smtp_host: "smtp.work.example".into(),
                    smtp_port: 465,
                    smtp_username: "ann@work.example".into(),
                    ..Default::default()
                },
            ],
            ..sample_account()
        };
        // A plain alias, and the account's own address, keep the account transport.
        assert!(account.alias_with_own_smtp("plain@fwd.example").is_none());
        assert!(account.alias_with_own_smtp("me@example.com").is_none());
        // The per-SMTP alias resolves — case-insensitively, as addresses are.
        let hit = account
            .alias_with_own_smtp("Ann@Work.example")
            .expect("matches");
        assert_eq!(hit.smtp_host, "smtp.work.example");
        // An address that is nobody's alias resolves to the account transport.
        assert!(account
            .alias_with_own_smtp("stranger@else.example")
            .is_none());
        // A full "Name <address>" identity resolves the same way.
        assert!(account
            .alias_with_own_smtp("Ann <ann@work.example>")
            .is_some());
    }

    #[test]
    fn an_alias_with_its_own_smtp_always_gets_a_sent_copy() {
        use crate::config::AliasConfig;
        let account = AccountConfig {
            email: "me@example.com".into(),
            server_saves_sent: true,
            aliases: vec![
                AliasConfig {
                    identity: "plain@fwd.example".into(),
                    ..Default::default()
                },
                AliasConfig {
                    identity: "Ann <ann@work.example>".into(),
                    smtp_host: "smtp.work.example".into(),
                    ..Default::default()
                },
            ],
            ..sample_account()
        };
        // Through the account's SMTP the server keeps the copy.
        assert!(account.server_files_sent(None));
        assert!(account.server_files_sent(Some("Plain <plain@fwd.example>")));
        // Through the alias's own server it never sees the message (#640).
        assert!(!account.server_files_sent(Some("Ann <ann@work.example>")));
        assert!(account.files_some_sent());
        let off = AccountConfig {
            server_saves_sent: false,
            ..account
        };
        assert!(!off.server_files_sent(None));
    }

    #[test]
    fn an_alias_server_that_files_sent_mail_gets_no_second_copy() {
        use crate::config::AliasConfig;
        let alias = |server_saves_sent| AliasConfig {
            identity: "Ann <ann@work.example>".into(),
            smtp_host: "smtp.work.example".into(),
            server_saves_sent,
            ..Default::default()
        };
        // The alias's switch decides for its own server, either way round
        // from the account's (#647).
        let account = AccountConfig {
            email: "me@example.com".into(),
            server_saves_sent: false,
            aliases: vec![alias(true)],
            ..sample_account()
        };
        assert!(!account.server_files_sent(None));
        assert!(account.server_files_sent(Some("ann@work.example")));
        assert!(account.files_some_sent());
        // Both servers file their own: nothing is left to this client.
        let both = AccountConfig {
            server_saves_sent: true,
            ..account
        };
        assert!(both.server_files_sent(None));
        assert!(!both.files_some_sent());
        // A plain alias leaves through the account's SMTP, so a switch it
        // cannot show changes nothing.
        let plain = AccountConfig {
            server_saves_sent: false,
            aliases: vec![AliasConfig {
                identity: "plain@fwd.example".into(),
                server_saves_sent: true,
                ..Default::default()
            }],
            ..both
        };
        assert!(!plain.server_files_sent(Some("plain@fwd.example")));
    }
}
