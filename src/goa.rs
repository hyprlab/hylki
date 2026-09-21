//! GNOME Online Accounts (GOA) integration.
//!
//! Reads mail-capable accounts configured in GNOME Settings → Online Accounts via
//! the `org.gnome.OnlineAccounts` D-Bus service (session bus), so the user can
//! enable them in Hylki without re-entering server settings. Password-based
//! providers (generic IMAP/SMTP) have their password retrieved from GOA and stored
//! in Hylki's keyring on import; OAuth2 providers (Gmail, Microsoft) authenticate
//! with a GOA-issued access token (XOAUTH2) fetched fresh at connect time.

use std::collections::{HashMap, HashSet};

use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::config::{AccountConfig, Protocol};

const GOA_DEST: &str = "org.gnome.OnlineAccounts";
const GOA_PATH: &str = "/org/gnome/OnlineAccounts";
const IFACE_ACCOUNT: &str = "org.gnome.OnlineAccounts.Account";
const IFACE_MAIL: &str = "org.gnome.OnlineAccounts.Mail";
const IFACE_PASSWORD: &str = "org.gnome.OnlineAccounts.PasswordBased";
const IFACE_OAUTH2: &str = "org.gnome.OnlineAccounts.OAuth2Based";

/// A mail account discovered in GNOME Online Accounts.
#[derive(Debug, Clone)]
pub struct GoaMailAccount {
    /// GOA account id (e.g. "account_1699…").
    pub id: String,
    pub email: String,
    pub name: String,
    pub provider: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_user: String,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_user: String,
    pub smtp_separate: bool,
    /// Whether GOA can hand us a password (generic IMAP).
    pub password_based: bool,
    /// Whether the provider uses OAuth2 — imported accounts authenticate with a
    /// GOA-issued token (XOAUTH2) fetched fresh at connect time.
    pub oauth2: bool,
    /// GOA's `ProviderType` ("google", "ms_graph", "imap_smtp", …). The
    /// Microsoft 365 (`ms_graph`) provider serves no IMAP — its token is scoped
    /// to the Graph API only, so mail runs over Graph (issue #36).
    pub provider_type: String,
}

impl GoaMailAccount {
    /// Turn a discovered GOA account into a Hylki [`AccountConfig`]. Pass the
    /// password for password-based providers, or `oauth = true` for OAuth ones
    /// (the token is fetched from GOA at connect time).
    pub fn to_config(&self, password: String, oauth: bool) -> AccountConfig {
        AccountConfig {
            folder_roles: Default::default(),
            hidden_folders: Vec::new(),
            folders_seeded: false,
            sent_copy_path: None,
            server_saves_sent: false,
            empty_junk_days: 0,
            empty_trash_days: 0,
            pgp_key: None,
            name: if self.name.trim().is_empty() {
                self.email.clone()
            } else {
                self.name.clone()
            },
            email: self.email.clone(),
            // Microsoft 365 (`ms_graph`) has no IMAP — mail goes over the Graph
            // API with the GOA token (issue #36).
            protocol: if self.provider_type == "ms_graph" {
                Protocol::Graph
            } else {
                Protocol::Imap
            },
            imap_host: self.imap_host.clone(),
            imap_port: self.imap_port,
            smtp_host: self.smtp_host.clone(),
            smtp_port: self.smtp_port,
            username: if self.imap_user.is_empty() {
                self.email.clone()
            } else {
                self.imap_user.clone()
            },
            password,
            smtp_separate: self.smtp_separate,
            tls_accept_hostname_mismatch: false,
            smtp_username: self.smtp_user.clone(),
            smtp_password: String::new(),
            color: None,
            emoji: None,
            avatar: None,
            gravatar: false,
            signature: None,
            signature_html: false,
            label: None,
            aliases: Vec::new(),
            enabled: true,
            goa_id: Some(self.id.clone()),
            goa_mail_disabled: false,
            goa_enabled_before_mail_disabled: true,
            oauth,
            oauth_settings: None,
            oauth_refresh: String::new(),
        push: None,
        }
    }
}

fn get_str(map: &HashMap<String, OwnedValue>, key: &str) -> String {
    map.get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or_default()
        .to_string()
}

fn get_bool(map: &HashMap<String, OwnedValue>, key: &str) -> bool {
    map.get(key).and_then(|v| bool::try_from(v).ok()).unwrap_or(false)
}

/// Split a GOA `ImapHost`/`SmtpHost` value into host and port. GOA stores a
/// custom port inside the host string ("mail.example.com:1143", or
/// "[2001:db8::1]:1993" for IPv6); with no port present, `default_port` applies.
fn host_and_port(value: String, default_port: u16) -> (String, u16) {
    if let Some(rest) = value.strip_prefix('[') {
        if let Some((host, suffix)) = rest.split_once(']') {
            let port = suffix
                .strip_prefix(':')
                .and_then(|p| p.parse().ok())
                .unwrap_or(default_port);
            return (host.to_string(), port);
        }
    }
    // A single colon separates host from port; more than one means a bare IPv6
    // address with no port at all.
    if value.matches(':').count() == 1 {
        if let Some((host, port)) = value.rsplit_once(':') {
            return (host.to_string(), port.parse().unwrap_or(default_port));
        }
    }
    (value, default_port)
}

type ManagedObjects =
    HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

/// List mail-capable GNOME Online Accounts. Returns an empty list if GOA isn't
/// running or has no mail accounts — never errors into the UI.
pub fn list_mail_accounts() -> Vec<GoaMailAccount> {
    match try_list() {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("GOA discovery skipped: {e}");
            Vec::new()
        }
    }
}

fn try_list() -> Result<Vec<GoaMailAccount>, String> {
    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let reply = conn
        .call_method(
            Some(GOA_DEST),
            GOA_PATH,
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .map_err(|e| e.to_string())?;
    let (objects,): (ManagedObjects,) = reply.body().deserialize().map_err(|e| e.to_string())?;

    let mut out = Vec::new();
    for ifaces in objects.values() {
        let (Some(account), Some(mail)) =
            (ifaces.get(IFACE_ACCOUNT), ifaces.get(IFACE_MAIL))
        else {
            continue;
        };
        // Skip accounts whose Mail service is turned off in GNOME Settings.
        if get_bool(account, "MailDisabled") {
            continue;
        }

        let imap_ssl = get_bool(mail, "ImapUseSsl");
        let smtp_ssl = get_bool(mail, "SmtpUseSsl");
        let email = {
            let e = get_str(mail, "EmailAddress");
            if e.is_empty() {
                get_str(account, "PresentationIdentity")
            } else {
                e
            }
        };
        if email.is_empty() {
            continue;
        }
        let imap_user = get_str(mail, "ImapUserName");
        let smtp_user = get_str(mail, "SmtpUserName");
        // The host strings may carry a custom port ("host:1143"); without one,
        // the conventional port for the advertised transport applies (993/143
        // for IMAP, 465 for implicit-TLS SMTP, 587 for STARTTLS).
        let (imap_host, imap_port) =
            host_and_port(get_str(mail, "ImapHost"), if imap_ssl { 993 } else { 143 });
        let (smtp_host, smtp_port) =
            host_and_port(get_str(mail, "SmtpHost"), if smtp_ssl { 465 } else { 587 });

        out.push(GoaMailAccount {
            id: get_str(account, "Id"),
            email,
            name: {
                let n = get_str(mail, "Name");
                if n.is_empty() {
                    get_str(account, "PresentationIdentity")
                } else {
                    n
                }
            },
            provider: get_str(account, "ProviderName"),
            imap_host,
            imap_port,
            imap_user: imap_user.clone(),
            smtp_host,
            smtp_port,
            smtp_user: smtp_user.clone(),
            smtp_separate: !smtp_user.is_empty() && smtp_user != imap_user,
            password_based: ifaces.contains_key(IFACE_PASSWORD),
            oauth2: ifaces.contains_key(IFACE_OAUTH2),
            provider_type: get_str(account, "ProviderType"),
        });
    }
    Ok(out)
}

/// A GNOME Online Accounts account whose token can reach the provider's
/// file storage: Microsoft 365 (OneDrive). Google is left out on purpose:
/// GOA's Google token carries no Drive scope on every system (Fedora
/// builds the Files feature out), so Drive is not offered.
#[derive(Debug, Clone)]
pub struct GoaFilesAccount {
    pub id: String,
    /// The account's e-mail, or its presentation identity.
    pub email: String,
    /// GOA's `ProviderType`: "ms_graph".
    pub provider_type: String,
    /// Whether the account's Files service is switched on in GNOME
    /// Settings; off means the user did not mean files to be reachable.
    pub files_enabled: bool,
}

/// List the GOA accounts that can serve cloud attachments (#144): every
/// OAuth2 Microsoft 365 account, Files switch or not. Empty when GOA is
/// not around. Blocking.
pub fn list_files_accounts() -> Vec<GoaFilesAccount> {
    let list = || -> Result<Vec<GoaFilesAccount>, String> {
        let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
        let reply = conn
            .call_method(
                Some(GOA_DEST),
                GOA_PATH,
                Some("org.freedesktop.DBus.ObjectManager"),
                "GetManagedObjects",
                &(),
            )
            .map_err(|e| e.to_string())?;
        let (objects,): (ManagedObjects,) = reply.body().deserialize().map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for ifaces in objects.values() {
            let Some(account) = ifaces.get(IFACE_ACCOUNT) else { continue };
            if !ifaces.contains_key(IFACE_OAUTH2) {
                continue;
            }
            let provider_type = get_str(account, "ProviderType");
            if provider_type != "ms_graph" {
                continue;
            }
            let email = {
                let e = ifaces.get(IFACE_MAIL).map(|m| get_str(m, "EmailAddress")).unwrap_or_default();
                if e.is_empty() {
                    get_str(account, "PresentationIdentity")
                } else {
                    e
                }
            };
            out.push(GoaFilesAccount {
                id: get_str(account, "Id"),
                email,
                provider_type,
                files_enabled: !get_bool(account, "FilesDisabled"),
            });
        }
        out.sort_by(|a, b| a.email.cmp(&b.email));
        Ok(out)
    };
    match list() {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("GOA files discovery skipped: {e}");
            Vec::new()
        }
    }
}

/// What currently exists in GNOME Online Accounts, as far as reconciliation
/// cares: which account objects are present at all, and which of them have their
/// Mail service switched off in GNOME Settings.
#[derive(Debug, Clone)]
pub struct GoaLiveState {
    /// Ids of every GOA account object, mail-capable or not — so merely
    /// disabling Mail doesn't count as removal.
    pub account_ids: HashSet<String>,
    /// Ids whose Mail service is explicitly disabled in GNOME Settings.
    pub disabled_mail_ids: HashSet<String>,
}

/// Snapshot the live GOA account state in one `GetManagedObjects` call. Returns
/// `None` when GOA / the session bus can't be reached — callers MUST treat that
/// as "unknown", not "no accounts", or they'd wrongly prune every imported GOA
/// account whenever GOA is momentarily unavailable. Blocking.
pub fn live_state() -> Option<GoaLiveState> {
    let conn = zbus::blocking::Connection::session().ok()?;
    let reply = conn
        .call_method(
            Some(GOA_DEST),
            GOA_PATH,
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .ok()?;
    let (objects,): (ManagedObjects,) = reply.body().deserialize().ok()?;
    let mut state =
        GoaLiveState { account_ids: HashSet::new(), disabled_mail_ids: HashSet::new() };
    for account in objects.values().filter_map(|ifaces| ifaces.get(IFACE_ACCOUNT)) {
        let id = get_str(account, "Id");
        if id.is_empty() {
            continue;
        }
        if get_bool(account, "MailDisabled") {
            state.disabled_mail_ids.insert(id.clone());
        }
        state.account_ids.insert(id);
    }
    Some(state)
}

/// Watch GNOME Online Accounts for changes — account removals and property
/// edits (e.g. the Mail service being switched on or off) — and hand `on_change`
/// a fresh [`GoaLiveState`] for each. Signals arrive in bursts for a single
/// Settings edit, so they are coalesced for 300 ms and snapshotted once, on a
/// dedicated thread — never the GTK main thread. Silently no-ops if GOA or the
/// session bus is unavailable.
pub fn watch_changes<F: Fn(GoaLiveState) + Send + 'static>(on_change: F) {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let _ = std::thread::Builder::new()
        .name("goa-watch-dispatch".into())
        .spawn(move || {
            while rx.recv().is_ok() {
                // Swallow the burst, then snapshot once it goes quiet.
                while rx.recv_timeout(std::time::Duration::from_millis(300)).is_ok() {}
                if let Some(state) = live_state() {
                    on_change(state);
                }
            }
        });

    let removed_tx = tx.clone();
    let _ = std::thread::Builder::new()
        .name("goa-removed-watch".into())
        .spawn(move || {
            if let Err(e) = watch_removed_signals(&removed_tx) {
                tracing::debug!("GOA removal watch stopped: {e}");
            }
        });
    let _ = std::thread::Builder::new()
        .name("goa-properties-watch".into())
        .spawn(move || {
            if let Err(e) = watch_property_signals(&tx) {
                tracing::debug!("GOA properties watch stopped: {e}");
            }
        });
}

fn watch_removed_signals(
    tx: &std::sync::mpsc::Sender<()>,
) -> Result<(), Box<dyn std::error::Error>> {
    let conn = zbus::blocking::Connection::session()?;
    let om = zbus::blocking::fdo::ObjectManagerProxy::builder(&conn)
        .destination(GOA_DEST)?
        .path(GOA_PATH)?
        .build()?;
    // Blocks until GOA emits an InterfacesRemoved signal; ends if the bus closes.
    let mut removed = om.receive_interfaces_removed()?;
    for _ in removed.by_ref() {
        let _ = tx.send(());
    }
    Ok(())
}

/// Property edits (MailDisabled among them) don't touch the ObjectManager; they
/// surface as `PropertiesChanged` on the account objects themselves.
fn watch_property_signals(
    tx: &std::sync::mpsc::Sender<()>,
) -> Result<(), Box<dyn std::error::Error>> {
    let conn = zbus::blocking::Connection::session()?;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(GOA_DEST)?
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .path_namespace("/org/gnome/OnlineAccounts/Accounts")?
        .build();
    let signals = zbus::blocking::MessageIterator::for_match_rule(rule, &conn, Some(32))?;
    for signal in signals {
        signal?;
        let _ = tx.send(());
    }
    Ok(())
}

/// Fetch a fresh OAuth2 access token for a GOA account (by id). GOA refreshes the
/// token as needed, so this always returns a currently-valid token. Blocking.
pub fn oauth_token(goa_id: &str) -> Option<String> {
    // A failure here surfaces as an authentication error several layers up, with
    // nothing to say why. Log the D-Bus reason before discarding it.
    match try_oauth_token(goa_id) {
        Ok(token) => Some(token),
        Err(e) => {
            tracing::warn!("GOA GetAccessToken failed for {goa_id}: {e}");
            None
        }
    }
}

fn try_oauth_token(goa_id: &str) -> Result<String, String> {
    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let path = format!("/org/gnome/OnlineAccounts/Accounts/{goa_id}");
    let reply = conn
        .call_method(
            Some(GOA_DEST),
            path.as_str(),
            Some(IFACE_OAUTH2),
            "GetAccessToken",
            &(),
        )
        .map_err(|e| e.to_string())?;
    // GetAccessToken() -> (access_token: s, expires_in: i)
    let (token, _expires): (String, i32) =
        reply.body().deserialize().map_err(|e| e.to_string())?;
    if token.is_empty() {
        Err("GOA returned an empty access token".to_string())
    } else {
        Ok(token)
    }
}

/// Ask GOA to validate or refresh an account's credentials before reading them.
///
/// Geary does this first and it matters: GOA may need to unlock the keyring or
/// renew a token, and without it `GetPassword` can come back empty for an
/// account that is perfectly usable. Failure is not fatal — GOA often still has
/// a cached secret — so the caller carries on and lets the read decide.
fn ensure_credentials(conn: &zbus::blocking::Connection, path: &str) {
    if let Err(e) = conn.call_method(
        Some(GOA_DEST),
        path,
        Some(IFACE_ACCOUNT),
        "EnsureCredentials",
        &(),
    ) {
        tracing::debug!("GOA EnsureCredentials failed for {path}: {e}");
    }
}

/// Read one credential by id, if GOA has it.
fn password_by_id(
    conn: &zbus::blocking::Connection,
    path: &str,
    credential_id: &str,
) -> Option<String> {
    let reply = conn
        .call_method(
            Some(GOA_DEST),
            path,
            Some(IFACE_PASSWORD),
            "GetPassword",
            &(credential_id,),
        )
        .map_err(|e| tracing::debug!("GOA GetPassword({credential_id}) failed: {e}"))
        .ok()?;
    let (password,): (String,) = reply.body().deserialize().ok()?;
    (!password.is_empty()).then_some(password)
}

/// The IMAP and SMTP passwords GOA holds for a mail account.
///
/// GOA's mail provider files these under `imap-password` and `smtp-password`;
/// other providers use a plain `password`, and some builds hand back the account
/// secret whatever id they are given. All three are tried rather than assuming
/// one, because getting it wrong leaves an account that silently can't log in
/// (issue #17) — and since accounts imported from GOA no longer expose their
/// password field, there is no longer a manual way out.
pub fn mail_passwords(goa_id: &str) -> (Option<String>, Option<String>) {
    let Ok(conn) = zbus::blocking::Connection::session() else {
        tracing::warn!("GOA passwords unavailable: no session bus");
        return (None, None);
    };
    let path = format!("/org/gnome/OnlineAccounts/Accounts/{goa_id}");
    ensure_credentials(&conn, &path);

    let first = |ids: &[&str]| ids.iter().find_map(|id| password_by_id(&conn, &path, id));
    let imap = first(&["imap-password", "password", goa_id]);
    // Most servers use one password for both; only look for a separate SMTP one,
    // and fall back to the incoming password rather than sending none.
    let smtp = first(&["smtp-password"]).or_else(|| imap.clone());
    if imap.is_none() {
        tracing::warn!("GNOME Online Accounts returned no password for {goa_id}");
    }
    (imap, smtp)
}

#[cfg(test)]
mod tests {
    use super::host_and_port;

    #[test]
    fn goa_host_may_include_a_custom_port() {
        assert_eq!(
            host_and_port("mail.example.com:1143".into(), 143),
            ("mail.example.com".into(), 1143)
        );
        assert_eq!(
            host_and_port("[2001:db8::1]:1993".into(), 993),
            ("2001:db8::1".into(), 1993)
        );
    }

    #[test]
    fn bare_hosts_fall_back_to_the_default_port() {
        assert_eq!(
            host_and_port("mail.example.com".into(), 993),
            ("mail.example.com".into(), 993)
        );
        // A bare IPv6 address has many colons but no port.
        assert_eq!(host_and_port("2001:db8::1".into(), 143), ("2001:db8::1".into(), 143));
        // A bracketed IPv6 address without a port.
        assert_eq!(host_and_port("[2001:db8::1]".into(), 143), ("2001:db8::1".into(), 143));
    }

    #[test]
    fn unparseable_ports_fall_back_to_the_default() {
        assert_eq!(
            host_and_port("mail.example.com:imaps".into(), 993),
            ("mail.example.com".into(), 993)
        );
    }
}


