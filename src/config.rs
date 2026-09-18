//! Account configuration.
//!
//! Account metadata (name, email, servers, username) lives in
//! `~/.config/hylki/accounts.toml`. Passwords are kept in the system keyring
//! (secret-service, e.g. gnome-keyring) — never written to disk. The `password`
//! field is read from the TOML if present (older configs / manual setup) and
//! migrated into the keyring on first use, then stripped from the file.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Write a config file that only its owner can read, without ever having
/// existed as anything else.
///
/// `fs::write` then `set_permissions` leaves a window in which the file sits on
/// disk with whatever the umask allowed — brief, but these files carry
/// hostnames, usernames, OAuth client secrets and correspondent lists. Creating
/// with the mode already set closes it.
fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(contents.as_bytes())?;
    // An existing file keeps its old mode through `OpenOptions::mode`, which
    // only applies at creation — so tighten regardless.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// XDG base directories, with one twist: the beta channel running inside its
/// own Flatpak sandbox (app ID co.hyprlab.Hylki.Beta) would get an empty
/// `~/.var/app/co.hyprlab.Hylki.Beta` tree of its own — so it redirects to the
/// STABLE app's tree instead, sharing accounts, settings and the mail cache
/// with the stable install. The keyring service name is identical across
/// channels, so credentials are shared through the same redirection-free path.
/// Outside Flatpak (host builds) both channels already share `~/.config/hylki`
/// et al., so no redirection is needed or done.
fn shared_base(own: fn() -> Option<PathBuf>, sub: &str) -> Option<PathBuf> {
    if cfg!(feature = "beta")
        && std::env::var("FLATPAK_ID").is_ok_and(|id| id == "co.hyprlab.Hylki.Beta")
        && stable_data_present()
    {
        return Some(dirs::home_dir()?.join(".var/app/co.hyprlab.Hylki").join(sub));
    }
    own()
}

/// Whether the shared flatpak directory is actually reachable. Flatpak
/// silently SKIPS a `--filesystem` grant whose host path doesn't exist, which
/// left `~/.var/app/co.hyprlab.Hylki` pointing at the sandbox's throwaway
/// tmpfs on beta-only installs — accounts "saved" there and vanished on quit
/// (issue #83). The real fix is the manifest's `:create` suffix, which makes
/// flatpak create the host directory itself — a beta-first install thereby
/// establishes the standard persistent home a later stable install picks up,
/// in either install order. This check remains as defence in depth: should
/// the mount ever be missing anyway (an old manifest, a stripped-down
/// installation), the beta falls back to its own persistent home rather than
/// writing into the tmpfs. Decided once at first use, so our own writes
/// creating the path on the tmpfs mid-session can't flip it.
fn stable_data_present() -> bool {
    use std::sync::OnceLock;
    static PRESENT: OnceLock<bool> = OnceLock::new();
    *PRESENT.get_or_init(|| {
        dirs::home_dir()
            .map(|h| h.join(".var/app/co.hyprlab.Hylki").is_dir())
            .unwrap_or(false)
    })
}

pub fn config_base() -> Option<PathBuf> {
    shared_base(dirs::config_dir, "config")
}

/// The interface language the user chose in Settings, as a locale code
/// ("fr", "en"); empty = the system's own (#179). Its own small file
/// rather than a preference in privacy.toml: it is read before anything
/// else starts, when no TOML has been parsed yet.
pub fn load_language() -> String {
    language_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

pub fn save_language(lang: &str) {
    let Some(path) = language_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if lang.trim().is_empty() {
        let _ = std::fs::remove_file(&path);
    } else {
        let _ = std::fs::write(&path, format!("{}\n", lang.trim()));
    }
}

fn language_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("language"))
}

pub fn cache_base() -> Option<PathBuf> {
    shared_base(dirs::cache_dir, "cache")
}

pub fn data_base() -> Option<PathBuf> {
    shared_base(dirs::data_dir, "data")
}

/// Where the accounts' avatar pictures live (#162).
pub fn avatars_dir() -> Option<PathBuf> {
    data_base().map(|d| d.join("avatars"))
}

/// The picture behind an account's `avatar` name, if the file is there.
pub fn avatar_path(name: &str) -> Option<PathBuf> {
    let p = avatars_dir()?.join(name);
    p.is_file().then_some(p)
}

/// Drop avatar pictures no account refers to any more. A file younger than
/// ten minutes is left alone: it may be a picture just chosen in an
/// account editor that has not been saved yet.
fn prune_avatars(accounts: &[AccountConfig]) {
    let Some(dir) = avatars_dir() else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let used: std::collections::HashSet<&str> =
        accounts.iter().filter_map(|a| a.avatar.as_deref()).collect();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if used.contains(name) {
            continue;
        }
        let fresh = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < std::time::Duration::from_secs(600));
        if !fresh {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Service name used for keyring entries; password items are keyed by email.
const KEYRING_SERVICE: &str = "co.hyprlab.Hylki";

/// Incoming-mail protocol for an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Imap,
    Pop3,
    /// Microsoft Graph (REST) — Microsoft 365 accounts imported from GNOME
    /// Online Accounts, whose token is Graph-scoped and can't speak IMAP
    /// (issue #36). No servers to configure; everything runs over
    /// graph.microsoft.com with the GOA token.
    Graph,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AccountConfig {
    pub name: String,
    pub email: String,
    /// Incoming-mail protocol (IMAP or POP3).
    #[serde(default)]
    pub protocol: Protocol,
    /// Incoming server host (IMAP or POP3, per `protocol`).
    pub imap_host: String,
    /// Incoming server port (IMAP or POP3, per `protocol`).
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    /// SMTP server. If empty, derived from `imap_host` (imap.* → smtp.*).
    #[serde(default)]
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    pub username: String,
    /// Read from TOML if present (legacy/manual), but never written back —
    /// passwords belong in the keyring. Usually empty after the first run.
    #[serde(default, skip_serializing)]
    pub password: String,
    /// Use distinct SMTP credentials instead of the IMAP ones.
    #[serde(default)]
    pub smtp_separate: bool,
    /// SMTP username (used only when `smtp_separate`).
    #[serde(default)]
    pub smtp_username: String,
    /// SMTP password — kept in the keyring (separate entry), never on disk.
    #[serde(default, skip_serializing)]
    pub smtp_password: String,
    /// Sidebar avatar background colour ("#rrggbb"). Falls back to the auto accent.
    #[serde(default)]
    pub color: Option<String>,
    /// Sidebar avatar emoji; when absent, the account-name initials are shown.
    #[serde(default)]
    pub emoji: Option<String>,
    /// Sidebar avatar picture (#162): the file name of a scaled copy kept
    /// under the data directory's `avatars/`. Shown before the emoji and
    /// the initials when set and the file is present.
    #[serde(default)]
    pub avatar: Option<String>,
    /// Show this mailbox's own Gravatar (#189), ahead of the picture, the
    /// emoji and the initials — and fall back to them when the address has
    /// none. Off by default: looking it up sends a hash of the address to
    /// Automattic, which is the account owner's own call to make, not a
    /// default (the separate Privacy switch covers other people's mail).
    #[serde(default)]
    pub gravatar: bool,
    /// Composition signature appended to new messages from this account.
    #[serde(default)]
    pub signature: Option<String>,
    /// Whether `signature` is HTML (vs. plain text).
    #[serde(default)]
    pub signature_html: bool,
    /// How this account is labelled in the UI (e.g. the All Inboxes view).
    /// When unset, the email address is shown.
    #[serde(default)]
    pub label: Option<String>,
    /// Send-as aliases: extra From identities offered in the composer (#34).
    /// Older configs stored these as plain "Name <address>" strings; both forms
    /// are accepted on load, and saved back as tables.
    #[serde(default, deserialize_with = "deserialize_aliases")]
    pub aliases: Vec<AliasConfig>,
    /// Whether the account is active. Disabled accounts stay configured but don't
    /// connect, sync, or appear in the sidebar.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// When imported from GNOME Online Accounts, the GOA account id (so its
    /// settings/credentials trace back to the system account).
    #[serde(default)]
    pub goa_id: Option<String>,
    /// Set while this GOA account's Mail service is switched off in GNOME
    /// Settings: the account is paused (`enabled` forced off) rather than
    /// removed, so its local settings survive until Mail comes back on.
    #[serde(default)]
    pub goa_mail_disabled: bool,
    /// What `enabled` was when the Mail pause began; restored when it ends.
    #[serde(default = "default_enabled")]
    pub goa_enabled_before_mail_disabled: bool,
    /// Authenticate with OAuth2 (XOAUTH2) instead of a stored password. The token
    /// comes from GOA (`goa_id`) or, for accounts added directly in Hylki, from
    /// refreshing `oauth_settings` with the keyring-stored refresh token.
    #[serde(default)]
    pub oauth: bool,
    /// OAuth2 endpoints/client for a natively-added OAuth account (no GOA).
    #[serde(default)]
    pub oauth_settings: Option<OAuthSettings>,
    /// OAuth2 refresh token — kept in the keyring, never on disk. Transient in
    /// memory (like `password`); stored on save.
    #[serde(default, skip_serializing)]
    pub oauth_refresh: String,
    /// Per-account IMAP push override (#91): Some(true/false) wins over the
    /// global "Instant new mail" switch; None follows it. Lets an account on
    /// a server that mishandles IDLE opt out without costing push elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<bool>,
    /// Manual special-folder assignments (#82), applied over auto-detection:
    /// role → full folder path. Roles: "sent", "drafts", "trash", "junk",
    /// "archive". Empty = fully automatic.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub folder_roles: std::collections::BTreeMap<String, String>,
    /// Where copies of sent mail are filed (#199): a full folder path, or
    /// `None` for the Sent folder. This is only a destination, not a role —
    /// the folder keeps whatever it already is, so the Inbox can be chosen
    /// without the account losing its inbox the way a "Sent" role assignment
    /// would (#136). Ignored by Microsoft 365, which files its own copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_copy_path: Option<String>,
    /// The server files its own copy of outgoing mail, so Hylki must not add
    /// one. Gmail does this for anything sent through its SMTP, which leaves
    /// two copies of every message: Google's, and the one Hylki appends.
    /// Off by default, because a server that does *not* do it and a Hylki
    /// that has stopped appending means no sent mail is kept at all.
    #[serde(default, skip_serializing_if = "is_false")]
    pub server_saves_sent: bool,
    /// Auto-empty (#140): mail in the Junk folder older than this many days
    /// is deleted for good at each sync. 0 = never.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub empty_junk_days: u32,
    /// The same for the Trash folder.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub empty_trash_days: u32,
    /// The OpenPGP key (fingerprint) that signs mail from this account and
    /// opens what is encrypted to it (#133); `None` = the key whose address
    /// matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pgp_key: Option<String>,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

/// A send-as alias (#34): an extra From identity the composer offers. By
/// default the mail still leaves through the account's own SMTP; an alias may
/// instead carry its own SMTP transport (host, credentials), so mail sent as
/// the alias goes out through the alias's provider — the forwarded-mailbox
/// setup where e.g. Gmail would otherwise rewrite the sender.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AliasConfig {
    /// The From identity: "Name <address>" or a bare address.
    pub identity: String,
    /// The alias's own SMTP server; empty = send through the account's SMTP.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub smtp_host: String,
    #[serde(default = "default_smtp_port", skip_serializing_if = "is_default_smtp_port")]
    pub smtp_port: u16,
    /// SMTP username (used only when `smtp_host` is set).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub smtp_username: String,
    /// SMTP password — kept in the keyring (keyed by account + alias address),
    /// never on disk.
    #[serde(default, skip_serializing)]
    pub smtp_password: String,
}

impl Default for AliasConfig {
    fn default() -> Self {
        Self {
            identity: String::new(),
            smtp_host: String::new(),
            smtp_port: default_smtp_port(),
            smtp_username: String::new(),
            smtp_password: String::new(),
        }
    }
}

impl AliasConfig {
    /// The bare address inside `identity`.
    pub fn address(&self) -> String {
        split_identity(&self.identity).1
    }

    /// Whether mail sent as this alias leaves through the alias's own SMTP
    /// server rather than the account's.
    pub fn has_own_smtp(&self) -> bool {
        !self.smtp_host.trim().is_empty()
    }
}

fn is_default_smtp_port(port: &u16) -> bool {
    *port == default_smtp_port()
}

/// "Name <addr>" or a bare address → (name, addr).
pub fn split_identity(s: &str) -> (String, String) {
    match s.split_once('<') {
        Some((n, rest)) => (
            n.trim().trim_matches('"').to_string(),
            rest.trim_end_matches('>').trim().to_string(),
        ),
        None => (String::new(), s.trim().to_string()),
    }
}

/// Aliases were plain strings before they could carry their own SMTP; accept
/// either form so existing configs keep loading.
fn deserialize_aliases<'de, D>(deserializer: D) -> Result<Vec<AliasConfig>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Entry {
        Plain(String),
        Full(AliasConfig),
    }
    Ok(Vec::<Entry>::deserialize(deserializer)?
        .into_iter()
        .map(|e| match e {
            Entry::Plain(identity) => AliasConfig { identity, ..AliasConfig::default() },
            Entry::Full(alias) => alias,
        })
        .collect())
}

/// OAuth2 client configuration for an account added directly in Hylki.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct OAuthSettings {
    pub auth_url: String,
    pub token_url: String,
    pub client_id: String,
    /// Optional; public/installed clients often have none.
    #[serde(default)]
    pub client_secret: String,
    pub scopes: String,
}

fn default_enabled() -> bool {
    true
}

impl AccountConfig {
    /// The account's UI label: the custom label, or the email address.
    pub fn display_label(&self) -> String {
        match self.label.as_deref() {
            Some(l) if !l.trim().is_empty() => l.to_string(),
            _ => self.email.clone(),
        }
    }
}

fn default_imap_port() -> u16 {
    993
}

fn default_smtp_port() -> u16 {
    587
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ConfigFile {
    #[serde(default)]
    accounts: Vec<AccountConfig>,
}

/// Demo mode's edited stand-in accounts (`~/.config/hylki/demo-accounts.toml`):
/// what the Accounts panel changed on the sample accounts (colour, emoji,
/// picture, label), kept apart from the real accounts file so the demo
/// stays a demo. `None` when there is none, or it is empty.
pub fn load_demo_accounts() -> Option<Vec<AccountConfig>> {
    let path = demo_accounts_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let cfg = toml::from_str::<ConfigFile>(&text).ok()?;
    (!cfg.accounts.is_empty()).then_some(cfg.accounts)
}

pub fn save_demo_accounts(accounts: &[AccountConfig]) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    let path =
        demo_accounts_path().ok_or_else(|| Error::new(ErrorKind::NotFound, "no config directory"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = ConfigFile { accounts: accounts.to_vec() };
    let toml = toml::to_string_pretty(&file).map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
    write_private(&path, &toml)
}

fn demo_accounts_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("demo-accounts.toml"))
}

/// Path to the accounts config file (`~/.config/hylki/accounts.toml`).
pub fn path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("accounts.toml"))
}

/// Returns the configured accounts, or `None` if there is no usable config
/// (missing file, parse error, or empty list) — in which case the app falls
/// back to the offline sample backend.
pub fn load() -> Option<Vec<AccountConfig>> {
    let path = path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    match toml::from_str::<ConfigFile>(&text) {
        Ok(mut cfg) if !cfg.accounts.is_empty() => {
            // Heal pre-#36 Microsoft 365 imports: a GOA OAuth account with no
            // incoming server could never connect over IMAP (GNOME's ms_graph
            // provider serves none) — it is a Graph account.
            for a in &mut cfg.accounts {
                if a.protocol == Protocol::Imap
                    && a.oauth
                    && a.goa_id.is_some()
                    && a.imap_host.trim().is_empty()
                {
                    tracing::info!("{}: empty-host GOA OAuth account — using Microsoft Graph", a.email);
                    a.protocol = Protocol::Graph;
                }
            }
            tracing::info!("loaded {} account(s) from {}", cfg.accounts.len(), path.display());
            Some(cfg.accounts)
        }
        Ok(_) => None,
        Err(e) => {
            tracing::error!("failed to parse {}: {e}", path.display());
            None
        }
    }
}

/// Write account metadata to disk (no passwords) and store each password in the
/// keyring.
///
/// Passwords live only in the keyring, so an in-memory `AccountConfig` loaded
/// from disk has an empty `password`. We must NEVER store an empty password —
/// doing so would wipe the keyring entry of any account that wasn't just edited.
pub fn save(accounts: &[AccountConfig]) -> std::io::Result<()> {
    write_config(accounts)?;
    prune_avatars(accounts);
    for account in accounts {
        if !account.password.is_empty() {
            if let Err(e) = store_password(&account.email, &account.password) {
                tracing::error!("could not store password for {}: {e}", account.email);
            }
        }
        // Same empty-guard for the separate SMTP password.
        if account.smtp_separate && !account.smtp_password.is_empty() {
            if let Err(e) = store_smtp_password(&account.email, &account.smtp_password) {
                tracing::error!("could not store SMTP password for {}: {e}", account.email);
            }
        }
        // And for each alias that sends through its own SMTP (#34).
        for alias in &account.aliases {
            if alias.has_own_smtp() && !alias.smtp_password.is_empty() {
                if let Err(e) = store_alias_smtp_password(
                    &account.email,
                    &alias.address(),
                    &alias.smtp_password,
                ) {
                    tracing::error!(
                        "could not store SMTP password for alias {} of {}: {e}",
                        alias.address(),
                        account.email
                    );
                }
            }
        }
        // OAuth refresh token (never overwrite a stored one with an empty value).
        if !account.oauth_refresh.is_empty() {
            if let Err(e) = store_oauth_refresh(&account.email, &account.oauth_refresh) {
                tracing::error!("could not store OAuth token for {}: {e}", account.email);
            }
        }
    }
    Ok(())
}

/// Rewrite the config file from the current accounts (dropping any plaintext
/// passwords still on disk). Used after migrating a legacy password.
pub fn strip_passwords_on_disk() {
    if let Some(accounts) = load() {
        if let Err(e) = write_config(&accounts) {
            tracing::warn!("could not rewrite config without passwords: {e}");
        }
    }
}

fn write_config(accounts: &[AccountConfig]) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};

    let path = path().ok_or_else(|| Error::new(ErrorKind::NotFound, "no config directory"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let file = ConfigFile {
        accounts: accounts.to_vec(),
    };
    let toml =
        toml::to_string_pretty(&file).map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
    write_private(&path, &toml)?;

    tracing::info!("saved {} account(s) to {}", accounts.len(), path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Keyring (secret-service)
// ---------------------------------------------------------------------------

fn keyring_entry(key: &str) -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, key)
}

/// Keyring key for an account's separate SMTP password.
fn smtp_key(email: &str) -> String {
    format!("smtp:{email}")
}

/// Keyring key for an alias's own SMTP password (#34). The alias address is
/// lowercased so lookups can't miss on letter case.
fn alias_smtp_key(email: &str, alias_addr: &str) -> String {
    format!("smtp-alias:{email}:{}", alias_addr.to_lowercase())
}

/// Keyring key for a natively-added OAuth account's refresh token.
fn oauth_key(email: &str) -> String {
    format!("oauth:{email}")
}

pub fn store_oauth_refresh(email: &str, token: &str) -> keyring::Result<()> {
    keyring_entry(&oauth_key(email))?.set_password(token)
}

pub fn load_oauth_refresh(email: &str) -> Option<String> {
    load_key(&oauth_key(email))
}

pub fn store_password(email: &str, password: &str) -> keyring::Result<()> {
    keyring_entry(email)?.set_password(password)
}

/// Cloud attachments (#144): an account's app password, by `CloudAccount::key`.
pub fn store_cloud_password(key: &str, password: &str) -> keyring::Result<()> {
    keyring_entry(key)?.set_password(password)
}

pub fn load_cloud_password(key: &str) -> Option<String> {
    load_key(key)
}

pub fn delete_cloud_password(key: &str) {
    if let Ok(e) = keyring_entry(key) {
        let _ = e.delete_credential();
    }
}

/// Write a settings file readable by the owner only (for modules outside
/// this one that keep their own file, like `cloud.toml`).
pub fn write_private_file(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    write_private(path, contents)
}

pub fn load_password(email: &str) -> Option<String> {
    load_key(email)
}

pub fn store_smtp_password(email: &str, password: &str) -> keyring::Result<()> {
    keyring_entry(&smtp_key(email))?.set_password(password)
}

pub fn load_smtp_password(email: &str) -> Option<String> {
    load_key(&smtp_key(email))
}

pub fn store_alias_smtp_password(
    email: &str,
    alias_addr: &str,
    password: &str,
) -> keyring::Result<()> {
    keyring_entry(&alias_smtp_key(email, alias_addr))?.set_password(password)
}

pub fn load_alias_smtp_password(email: &str, alias_addr: &str) -> Option<String> {
    load_key(&alias_smtp_key(email, alias_addr))
}

pub fn delete_alias_smtp_password(email: &str, alias_addr: &str) {
    delete_key(&alias_smtp_key(email, alias_addr));
}

/// Drop every keyring entry an account owns: its password(s), OAuth token, and
/// each alias's own SMTP password. Prefer this over bare [`delete_password`]
/// whenever the account's config is still at hand — the alias entries are keyed
/// by address, which only the config knows.
pub fn delete_account_secrets(account: &AccountConfig) {
    delete_password(&account.email);
    for alias in &account.aliases {
        delete_alias_smtp_password(&account.email, &alias.address());
    }
}

fn load_key(key: &str) -> Option<String> {
    match keyring_entry(key).and_then(|e| e.get_password()) {
        Ok(password) => Some(password),
        Err(keyring::Error::NoEntry) => load_legacy_key(key),
        Err(e) => {
            tracing::warn!("could not read keyring entry for {key}: {e}");
            None
        }
    }
}

/// Fall back to an entry stored under an earlier name's service (Vireo,
/// then Veem — see `legacy::PREDECESSORS`), copying it to the current
/// service so accounts added there keep working. The old entry is left as
/// it is: the old app may still be installed and using it.
fn load_legacy_key(key: &str) -> Option<String> {
    for pred in crate::legacy::PREDECESSORS {
        let Ok(old) = keyring::Entry::new(pred.keyring_service, key) else { continue };
        let Ok(password) = old.get_password() else { continue };
        if password.is_empty() {
            continue;
        }
        match keyring_entry(key).and_then(|new| new.set_password(&password)) {
            Ok(()) => tracing::info!("keyring entry for {key} carried over from {}", pred.name),
            Err(e) => tracing::warn!("could not carry the keyring entry for {key} over: {e}"),
        }
        return Some(password);
    }
    None
}

pub fn delete_password(email: &str) {
    delete_key(email);
    // Also drop the account's separate SMTP password and OAuth token, if any.
    delete_key(&smtp_key(email));
    delete_key(&oauth_key(email));
}

fn delete_key(key: &str) {
    if let Ok(entry) = keyring_entry(key) {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => tracing::warn!("could not delete keyring entry for {key}: {e}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Privacy settings (remote-content allowlist)
// ---------------------------------------------------------------------------

/// The app's own theme: follow the system, or force light/dark regardless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AppTheme {
    #[default]
    System,
    Light,
    Dark,
}

/// Which icon the tray item wears (issue #116).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrayIcon {
    /// The app icon.
    #[default]
    Hylki,
    /// `mail-unread-symbolic` in white, for dark panels.
    EnvelopeLight,
    /// `mail-unread-symbolic` in black, for light panels.
    EnvelopeDark,
}

/// How email message content is themed, independent of the app UI theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageTheme {
    /// Follow the system / app light-dark preference.
    #[default]
    System,
    Light,
    Dark,
}

impl MessageTheme {
    /// Forced dark flag for message content, or `None` to follow the system.
    pub fn dark_override(self) -> Option<bool> {
        match self {
            MessageTheme::System => None,
            MessageTheme::Light => Some(false),
            MessageTheme::Dark => Some(true),
        }
    }
}

/// The reader's own typography and colours, laid over the sender's (#56).
///
/// What the reader applies: `font` is the Pango description to set mail in
/// (`None` = the sender's fonts stand), `colors` forces the reader's own text
/// and ground colours over the sender's. Built by the app from the three
/// stored preferences, with the interface font filled in where none was
/// chosen, so the reader never has to ask GTK.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ReaderStyle {
    /// Pango description ("Cantarell 11") of the font every message is set
    /// in; `None` leaves the sender's fonts and sizes alone.
    pub font: Option<String>,
    /// Ignore the sender's text and background colours.
    pub colors: bool,
    /// Pango description of the font plain-text messages are set in
    /// (#181); `None` leaves them in the reader's default.
    pub plain_font: Option<String>,
}

impl ReaderStyle {
    /// The sender's formatting stands entirely.
    pub const NONE: ReaderStyle = ReaderStyle { font: None, colors: false, plain_font: None };

    /// Whether anything of the sender's is overridden.
    pub fn active(&self) -> bool {
        self.font.is_some() || self.colors || self.plain_font.is_some()
    }
}

/// How dates are written: the system's own arrangement, or one the user picked
/// regardless of it (#32).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DateStyle {
    /// Follow the locale: its field order, month names and separators.
    #[default]
    System,
    /// Aug 23, 2026
    MonthFirst,
    /// 23 Aug 2026
    DayFirst,
    /// 2026 Aug 23
    YearFirst,
}

/// Whether the clock runs to 12 or 24, or follows the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ClockStyle {
    #[default]
    System,
    /// 5:40 PM
    Twelve,
    /// 17:40
    TwentyFour,
}

#[derive(Debug, Deserialize, Serialize)]
struct PrivacyFile {
    #[serde(default)]
    allowed_senders: Vec<String>,
    /// Whether remote content (images, trackers) is auto-loaded for every new
    /// message, not just those from allowed senders. Off by default, since
    /// remote content can be used to track when and where a message is read.
    #[serde(default)]
    auto_remote_content: bool,
    /// Whether to load sender avatars from Gravatar (off by default — it sends
    /// a hash of each sender's email to a third party).
    #[serde(default)]
    gravatar: bool,
    /// Whether the coloured avatars are drawn in the message list and the
    /// reader (#29 — they cost horizontal room on a small screen).
    #[serde(default = "default_avatars")]
    avatars: bool,
    /// Whether a sender's site icon may be fetched to fill their circle (#30).
    #[serde(default)]
    sender_logos: bool,
    /// Whether the mail you sent wears its mailbox's face — the account's
    /// Gravatar, picture or emoji (#189) — instead of the circle any other
    /// sender would get.
    #[serde(default = "default_own_mailbox_face")]
    own_mailbox_face: bool,
    /// How dates are written (#32).
    #[serde(default)]
    date_style: DateStyle,
    /// Whether the clock runs to 12 or 24 (#32).
    #[serde(default)]
    clock_style: ClockStyle,
    /// Seconds between automatic mail checks; 0 = manual only.
    #[serde(default = "default_fetch_interval")]
    fetch_interval_secs: u64,
    /// Whether to use IMAP IDLE push for instant new-mail delivery.
    #[serde(default = "default_push")]
    push: bool,
    /// Addresses or whole domains whose incoming mail is auto-deleted (to Trash).
    /// Stored lowercased; a bare domain like "spam.com" matches any sender there.
    #[serde(default)]
    blacklist: Vec<String>,
    /// Seconds the message-list actions palette stays open after the cursor
    /// leaves it before auto-collapsing. (A prior `palette_delay_ms` setting in
    /// milliseconds is intentionally not migrated — its meaning has changed.)
    #[serde(default = "default_palette_collapse")]
    palette_collapse_secs: u64,
    /// Seconds a message card's actions palette stays open after the cursor
    /// leaves it. Separate from the list's: cards are read at a different
    /// pace from a list being skimmed.
    #[serde(default = "default_palette_collapse")]
    card_palette_collapse_secs: u64,
    /// Group messages into conversation threads in the list.
    #[serde(default = "default_threading")]
    threading: bool,
    /// Whether conversation threads start expanded in the message list
    /// (collapsed to their newest message by default).
    #[serde(default)]
    threads_expanded: bool,
    /// Whether a conversation row can expand into its member rows in the
    /// message list. Off: the row keeps its count chip and chevron, but the
    /// thread itself opens only in the reading pane's cards.
    #[serde(default = "default_thread_expansion")]
    thread_expansion: bool,
    /// Whether the reading pane shows a conversation newest-message-first.
    #[serde(default)]
    thread_newest_first: bool,
    /// Whether the reader always shows the recipients line under the sender.
    #[serde(default)]
    always_show_recipients: bool,
    /// Whether a lone message renders as an inset card like a conversation's
    /// messages (#57); off keeps the full-bleed view.
    #[serde(default = "default_single_message_card")]
    single_message_card: bool,
    /// Each conversation message lists its own attachments beneath its body
    /// (#213), so which file came with which message is never in doubt.
    #[serde(default = "default_card_attachments")]
    card_attachments: bool,
    /// The attachment drawer beneath the reader, gathering every attachment
    /// in the open conversation (#213).
    #[serde(default = "default_attachment_drawer")]
    attachment_drawer: bool,
    /// Whether deleting a whole selected conversation asks for confirmation
    /// first.
    #[serde(default = "default_confirm_thread_delete")]
    confirm_thread_delete: bool,
    /// How email content is themed (independent of the app UI theme).
    #[serde(default)]
    message_theme: MessageTheme,
    /// Set every message in the reader's own font instead of the sender's
    /// (#56).
    #[serde(default)]
    override_fonts: bool,
    /// That font, as a Pango description ("Cantarell 11"); empty = the
    /// interface font.
    #[serde(default)]
    reader_font: String,
    /// Ignore the sender's text and background colours (#56).
    #[serde(default)]
    override_colors: bool,
    /// Show plain-text messages in a monospace font (#181).
    #[serde(default)]
    plain_monospace: bool,
    /// That font, as a Pango description; empty = the desktop's monospace font.
    #[serde(default)]
    plain_font: String,
    /// Whether to post desktop notifications (new mail, error alerts).
    #[serde(default = "default_notifications")]
    notifications: bool,
    /// Whether new-mail notifications name the sender and subject. On by
    /// default — that is what makes them useful — but GNOME draws notifications
    /// on the lock screen, so turning it off is worth offering.
    #[serde(default = "default_notification_content")]
    notification_content: bool,
    /// Whether the sidebar's pinned footer shows the "Attachments" row (the
    /// gallery of every account's attachments).
    #[serde(default = "default_show_attachments")]
    show_attachments: bool,
    /// Whether the sidebar's pinned footer shows the "Contacts" shortcut row
    /// (above Attachments); it opens the app-wide contacts browser.
    #[serde(default = "default_show_contacts")]
    show_contacts: bool,
    /// Whether the combined Accounts & Preferences window opens showing the
    /// Accounts view instead of Preferences (the default).
    #[serde(default)]
    settings_open_accounts: bool,
    /// Whether a conversation card's action icons stay hidden until the card
    /// is hovered (expanded via their ⋯ toggle). Off = always shown, unless
    /// `card_actions_auto` shows them automatically on hover.
    #[serde(default = "default_card_actions_hover")]
    card_actions_hover: bool,
    /// With the ⋯ toggle off: show the action icons automatically while the
    /// card is hovered (rather than always).
    #[serde(default = "default_card_actions_auto")]
    card_actions_auto: bool,
    /// Whether the message list rows carry an actions palette at all. Off
    /// removes the ⋯ line entirely, returning its space to the row.
    #[serde(default = "default_list_palette")]
    list_palette: bool,
    /// Whether the message list's actions palette opens on row hover, without
    /// needing the ⋯ click.
    #[serde(default)]
    list_palette_hover: bool,
    /// Whether the ⋯ on a message card opens the card's menu in place of
    /// sliding its actions palette out.
    #[serde(default)]
    card_palette_menu: bool,
    /// Whether message rows take a sideways swipe at all (#92, PR #135).
    #[serde(default = "default_swipe_enabled")]
    swipe_enabled: bool,
    /// Swap the message list's swipe-gesture sides: off (default) swipes
    /// left to delete and right to archive, on reverses them.
    #[serde(default)]
    swipe_reversed: bool,
    /// How far a trackpad's two-finger swipe has to travel before a row
    /// commits (higher = shorter swipe). Trackpads differ enough that one
    /// fixed figure suits nobody, hence the knob; mouse and touchscreen
    /// drags follow the finger 1:1 and ignore this.
    #[serde(default = "default_swipe_sensitivity")]
    swipe_sensitivity: f64,
    /// Whether "New message" opens inline over the reading pane (like a
    /// reply) rather than in its own window.
    #[serde(default = "default_compose_inline")]
    compose_inline: bool,
    /// Whether the inline reply panel shows its From, To and Subject rows
    /// from the start (#154); off, a button in its header reveals them.
    #[serde(default)]
    reply_fields: bool,
    /// The identity new messages are sent from (#157): an account's or
    /// alias's address, or empty for the account of the open folder (the
    /// original behaviour). Replies keep answering from the address the
    /// original was sent to, whatever this says.
    #[serde(default)]
    compose_default_from: String,
    /// The inset-card default for lone messages (#57, #153) was applied
    /// once to installs that predate it. Set on the first load that did so.
    #[serde(default)]
    single_card_default_applied: bool,
    /// Whether pasting into the composer strips the clipboard's formatting
    /// (the default). Off, a paste keeps its formatting. The editor's context
    /// menu always offers both, whichever way this is set.
    #[serde(default = "default_paste_plain")]
    paste_plain: bool,
    /// New messages start as plain text, without formatting (#180). Kept
    /// written so a version that predates `compose_format` still opens its
    /// composer the way this one was left.
    #[serde(default)]
    compose_plain: bool,
    /// What new messages are written in: rich text, Markdown, HTML
    /// source, or plain text. Absent on installs that predate the choice,
    /// where `compose_plain` above still says which of the two it is.
    #[serde(default)]
    compose_format: Option<ComposeFormat>,
    /// Where the split reply opens in the reading pane (#212).
    #[serde(default)]
    reply_position: ReplyPosition,
    /// Whether the composer underlines misspelled words as you type.
    #[serde(default = "default_spellcheck")]
    spellcheck: bool,
    /// Languages to check against, comma-separated (e.g. "en_US, de_DE").
    /// Empty = follow the session locale (WebKit's own default).
    #[serde(default)]
    spellcheck_langs: String,
    /// Hovering the icon rail (narrow-window or user-collapsed) floats the
    /// full sidebar out over the panes without needing the expand button.
    #[serde(default)]
    sidebar_hover_expand: bool,
    /// Reopen with the accounts, folders and sections as they were left.
    /// Off starts every launch with everything folded up.
    #[serde(default = "default_on")]
    remember_sidebar: bool,
    /// Reopen collapsed to the icon rail if that is how it was left. Off
    /// starts every launch with the full sidebar.
    #[serde(default = "default_on")]
    remember_rail: bool,
    /// Icon rail: a dot for unread mail in place of the count. On by
    /// default, as are the fold-ups below.
    #[serde(default = "default_on")]
    rail_dots: bool,
    /// Icon rail: the sections folded up by themselves when the sidebar
    /// collapses, one switch each.
    #[serde(default)]
    rail_fold: RailFold,
    /// The app chrome's theme: follow the system, or force light/dark.
    #[serde(default)]
    app_theme: AppTheme,
    /// The appearance theme's id: a bundled palette (see `theme.rs`) painted
    /// over libadwaita's colours, or "system" for the stock GNOME look.
    /// Empty — a file written before themes existed — means the same.
    #[serde(default)]
    theme: String,
    /// Lines of message text shown under the subject in the list: 0 turns the
    /// preview off entirely, and stops it being fetched.
    #[serde(default = "default_preview_lines")]
    preview_lines: u32,
    /// Single-key shortcuts (j/k, r, a, d…) without a modifier. Off by default:
    /// a stray keystroke shouldn't archive mail for someone who never asked.
    #[serde(default)]
    single_key_shortcuts: bool,
    /// Keep running after the window is closed, so new mail still arrives and
    /// notifies. Off by default: closing a window is expected to quit.
    #[serde(default)]
    run_in_background: bool,
    /// Start at login (only meaningful with `run_in_background`).
    #[serde(default)]
    autostart: bool,
    /// Publish a tray icon (StatusNotifierItem) for desktops that draw one
    /// (issue #116). Off by default: GNOME has no tray without an extension.
    #[serde(default)]
    tray: bool,
    /// Which icon the tray item shows.
    #[serde(default)]
    tray_icon: TrayIcon,
    /// Whether the tray menu lists unread inbox mail, each a row that opens
    /// the message.
    #[serde(default = "default_tray_mail")]
    tray_mail: bool,
    /// Whether to say anything at all when remote content is blocked. Off hides
    /// the banner; it never changes what is blocked, only whether you're told.
    #[serde(default = "default_show_remote_banner")]
    show_remote_banner: bool,
    /// Whether the sidebar offers the unified "All Inboxes" section at all
    /// (it only ever appears with more than one enabled account).
    #[serde(default = "default_show_unified")]
    show_unified: bool,
    /// The old single switch for All Inboxes' unread chip, kept so a file
    /// written before `unified_chips` still reads as it was set.
    #[serde(default = "default_unified_chip")]
    unified_chip: bool,
    /// Which unified rows wear their total-unread chip while folded up
    /// (expanded, the rows beneath carry the counts and the total is never
    /// shown). One switch per row: All Inboxes, Starred, Drafts, Archive,
    /// Filtered Folders. Sent never counts.
    #[serde(default)]
    unified_chips: UnifiedChips,
    /// Whether the unified section lists the folders that filter rules file
    /// into, in a Filtered Folders section of its own. Off hides the section.
    #[serde(default = "default_unified_filtered")]
    unified_filtered: bool,
    /// The unified section's Starred / Sent / Drafts rows, one switch each
    /// (All Inboxes is `show_unified` above).
    #[serde(default)]
    unified_kinds: UnifiedKinds,
    /// Whether the unified section lists the tags (every account's mail with
    /// the tag). Off hides that section; each account keeps its own.
    #[serde(default = "default_on")]
    unified_tags: bool,
    /// Whether the account sections are shown at all. Off leaves the unified
    /// section alone, for those who only ever work from it.
    #[serde(default = "default_on")]
    show_accounts: bool,
    /// Where the Filtered Folders section sits (#71 follow-up): inside All
    /// Inboxes, or in the scrolling sidebar above or below the accounts.
    #[serde(default)]
    filtered_placement: SectionPlacement,
    /// Where the Tags section sits, the same three choices.
    #[serde(default)]
    tags_placement: SectionPlacement,
    /// Whether the sidebar's disclosure chevrons (All Inboxes, account
    /// headers) LEAD their rows; off puts them back at the row's end.
    #[serde(default = "default_chevrons_left")]
    chevrons_left: bool,
    /// Console mode (#status-bar): the verbose activity console is offered in
    /// the status bar. Off by default.
    #[serde(default)]
    console_mode: bool,
    /// Read-marking policy (#100).
    #[serde(default)]
    read_mark: ReadMark,
    /// GNOME Files hand-off (#188 follow-up): what the files open into,
    /// what happens over the size limit, and the limit itself in MB.
    #[serde(default)]
    files_action: FilesAction,
    #[serde(default)]
    files_large: FilesLarge,
    #[serde(default = "default_files_limit_mb")]
    files_limit_mb: u32,
}

fn default_chevrons_left() -> bool {
    // Right for new installs (Jason, 2026-08-30): the classic trailing
    // placement; Left is the opt-in. Only a privacy.toml missing the key
    // sees this — every save writes all fields.
    false
}

fn default_show_unified() -> bool {
    true
}

fn default_unified_chip() -> bool {
    true
}

fn default_unified_filtered() -> bool {
    true
}

fn default_fetch_interval() -> u64 {
    300
}

fn default_push() -> bool {
    true
}

fn default_threading() -> bool {
    true
}

fn default_thread_expansion() -> bool {
    // Off for new installs (2026-08-30): conversations expand in the reading
    // pane's cards; the list keeps its count chip without in-list expansion.
    // Only a privacy.toml MISSING this key sees the default — every save
    // writes all fields, so an existing install's choice is pinned.
    false
}

fn default_card_attachments() -> bool {
    true
}

fn default_attachment_drawer() -> bool {
    true
}

fn default_single_message_card() -> bool {
    // On for new installs (Jason, 2026-08-31): lone messages get the same
    // inset card as conversations. Only a privacy.toml MISSING this key sees
    // the default — every save writes all fields, so an existing install's
    // choice is pinned.
    true
}

fn default_confirm_thread_delete() -> bool {
    true
}

fn default_list_palette() -> bool {
    true
}

fn default_show_remote_banner() -> bool {
    true
}

fn default_palette_collapse() -> u64 {
    5
}

fn default_notification_content() -> bool {
    true
}

fn default_notifications() -> bool {
    true
}

fn default_show_contacts() -> bool {
    true
}

fn default_show_attachments() -> bool {
    true
}

fn default_card_actions_hover() -> bool {
    true
}

fn default_card_actions_auto() -> bool {
    // Off for new installs (2026-08-30): card actions wait behind the ⋯
    // toggle (card_actions_hover) rather than appearing on hover.
    false
}

fn default_paste_plain() -> bool {
    true
}

fn default_spellcheck() -> bool {
    true
}

fn default_compose_inline() -> bool {
    true
}

fn default_preview_lines() -> u32 {
    1
}

fn default_own_mailbox_face() -> bool {
    true
}

fn default_avatars() -> bool {
    true
}

impl Default for PrivacyFile {
    fn default() -> Self {
        Self {
            allowed_senders: Vec::new(),
            auto_remote_content: false,
            show_remote_banner: default_show_remote_banner(),
            show_unified: default_show_unified(),
            unified_chip: default_unified_chip(),
            unified_chips: UnifiedChips::default(),
            unified_filtered: default_unified_filtered(),
            unified_kinds: UnifiedKinds::default(),
            unified_tags: true,
            show_accounts: true,
            filtered_placement: SectionPlacement::default(),
            tags_placement: SectionPlacement::default(),
            chevrons_left: default_chevrons_left(),
            console_mode: false,
            read_mark: ReadMark::default(),
            files_action: FilesAction::default(),
            files_large: FilesLarge::default(),
            files_limit_mb: default_files_limit_mb(),
            gravatar: false,
            avatars: default_avatars(),
            own_mailbox_face: default_own_mailbox_face(),
            sender_logos: false,
            date_style: DateStyle::default(),
            clock_style: ClockStyle::default(),
            fetch_interval_secs: default_fetch_interval(),
            push: default_push(),
            blacklist: Vec::new(),
            palette_collapse_secs: default_palette_collapse(),
            card_palette_collapse_secs: default_palette_collapse(),
            threading: default_threading(),
            threads_expanded: false,
            thread_expansion: default_thread_expansion(),
            thread_newest_first: false,
            always_show_recipients: false,
            single_message_card: default_single_message_card(),
            card_attachments: default_card_attachments(),
            attachment_drawer: default_attachment_drawer(),
            confirm_thread_delete: default_confirm_thread_delete(),
            message_theme: MessageTheme::default(),
            override_fonts: false,
            reader_font: String::new(),
            override_colors: false,
            plain_monospace: false,
            plain_font: String::new(),
            notifications: default_notifications(),
            notification_content: default_notification_content(),
            show_attachments: default_show_attachments(),
            show_contacts: default_show_contacts(),
            settings_open_accounts: false,
            card_actions_hover: default_card_actions_hover(),
            card_actions_auto: default_card_actions_auto(),
            list_palette: default_list_palette(),
            list_palette_hover: false,
            card_palette_menu: false,
            swipe_enabled: default_swipe_enabled(),
            swipe_reversed: false,
            swipe_sensitivity: default_swipe_sensitivity(),
            compose_inline: default_compose_inline(),
            reply_fields: false,
            compose_default_from: String::new(),
            single_card_default_applied: false,
            paste_plain: default_paste_plain(),
            compose_plain: false,
            compose_format: None,
            reply_position: ReplyPosition::default(),
            spellcheck: default_spellcheck(),
            spellcheck_langs: String::new(),
            sidebar_hover_expand: false,
            remember_sidebar: true,
            remember_rail: true,
            rail_dots: true,
            rail_fold: RailFold::default(),
            app_theme: AppTheme::default(),
            theme: String::new(),
            preview_lines: default_preview_lines(),
            single_key_shortcuts: false,
            run_in_background: false,
            autostart: false,
            tray: false,
            tray_icon: TrayIcon::default(),
            tray_mail: default_tray_mail(),
        }
    }
}

fn privacy_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("privacy.toml"))
}

fn load_privacy() -> PrivacyFile {
    let Some(path) = privacy_path() else {
        return PrivacyFile::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return PrivacyFile::default();
    };
    let mut file = toml::from_str::<PrivacyFile>(&text).unwrap_or_default();
    // The inset card for a lone message became the default on 2026-08-31
    // for new installs only: every save writes every key, so an install
    // from before then stayed on the full-bleed view without ever choosing
    // it (#153). Apply the default once and remember that it was.
    if !file.single_card_default_applied {
        file.single_card_default_applied = true;
        file.single_message_card = true;
        if let Ok(toml) = toml::to_string_pretty(&file) {
            let _ = write_private(&path, &toml);
        }
    }
    file
}

/// Whether the inline reply panel shows From, To and Subject from the start (#154).
pub fn load_reply_fields() -> bool {
    load_privacy().reply_fields
}

/// The address new messages are sent from; empty = the open folder's account.
pub fn load_compose_default_from() -> String {
    load_privacy().compose_default_from
}

/// Senders whose messages may auto-load remote content. Stored lowercased.
pub fn load_allowed_senders() -> Vec<String> {
    load_privacy().allowed_senders
}

/// Whether remote content is auto-loaded for every new message.
pub fn load_auto_remote_content() -> bool {
    load_privacy().auto_remote_content
}

pub fn load_show_remote_banner() -> bool {
    load_privacy().show_remote_banner
}

/// Whether Gravatar avatar loading is enabled.
pub fn load_gravatar() -> bool {
    load_privacy().gravatar
}

/// Whether the avatars are shown in the list and the reader.
pub fn load_avatars() -> bool {
    load_privacy().avatars
}

/// Whether your own messages wear their mailbox's face (#189).
pub fn load_own_mailbox_face() -> bool {
    load_privacy().own_mailbox_face
}

/// Whether sender logos are fetched from senders' own domains.
pub fn load_sender_logos() -> bool {
    load_privacy().sender_logos
}

/// How dates are written, and on what clock.
pub fn load_date_format() -> (DateStyle, ClockStyle) {
    let p = load_privacy();
    (p.date_style, p.clock_style)
}

/// Seconds between automatic mail checks (0 = manual only).
pub fn load_fetch_interval() -> u64 {
    load_privacy().fetch_interval_secs
}

/// Whether IMAP IDLE push is enabled.
pub fn load_push() -> bool {
    load_privacy().push
}

/// Senders/domains whose incoming mail is auto-deleted. Stored lowercased.
pub fn load_blacklist() -> Vec<String> {
    load_privacy().blacklist
}

/// Seconds the message list's actions palette stays open after the cursor
/// leaves it.
pub fn load_palette_collapse() -> u64 {
    load_privacy().palette_collapse_secs
}

/// Seconds a message card's actions palette stays open after the cursor
/// leaves it.
pub fn load_card_palette_collapse() -> u64 {
    load_privacy().card_palette_collapse_secs
}

/// Whether messages are grouped into conversation threads.
pub fn load_threading() -> bool {
    load_privacy().threading
}

/// Whether conversation threads start expanded (collapsed by default).
pub fn load_threads_expanded() -> bool {
    load_privacy().threads_expanded
}

/// Whether conversation rows can expand into their members in the list.
pub fn load_thread_newest_first() -> bool {
    load_privacy().thread_newest_first
}

pub fn load_always_show_recipients() -> bool {
    load_privacy().always_show_recipients
}

pub fn load_show_unified() -> bool {
    load_privacy().show_unified
}

/// The unified rows' unread-chip switches. A file from before the
/// per-row switches carried one switch, for All Inboxes; it still counts.
pub fn load_unified_chips() -> UnifiedChips {
    let p = load_privacy();
    let mut chips = p.unified_chips;
    chips.all_inboxes = chips.all_inboxes && p.unified_chip;
    chips
}

pub fn load_unified_filtered() -> bool {
    load_privacy().unified_filtered
}

pub fn load_unified_kinds() -> UnifiedKinds {
    load_privacy().unified_kinds
}

pub fn load_unified_tags() -> bool {
    load_privacy().unified_tags
}

pub fn load_show_accounts() -> bool {
    load_privacy().show_accounts
}

/// Which of the unified section's folder rows are shown besides All
/// Inboxes: each combines that folder across every account, and opens to
/// the accounts' own. All on until switched off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnifiedKinds {
    #[serde(default = "default_on")]
    pub starred: bool,
    #[serde(default = "default_on")]
    pub sent: bool,
    #[serde(default = "default_on")]
    pub drafts: bool,
    #[serde(default = "default_on")]
    pub archive: bool,
}

impl Default for UnifiedKinds {
    fn default() -> Self {
        UnifiedKinds { starred: true, sent: true, drafts: true, archive: true }
    }
}

/// Which unified rows show their total-unread chip while folded up
/// (Settings → Sidebar → Unified → Unread counts). All on until switched
/// off; Sent has no chip anywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnifiedChips {
    #[serde(default = "default_on")]
    pub all_inboxes: bool,
    #[serde(default = "default_on")]
    pub starred: bool,
    #[serde(default = "default_on")]
    pub drafts: bool,
    #[serde(default = "default_on")]
    pub archive: bool,
    #[serde(default = "default_on")]
    pub filtered: bool,
}

impl Default for UnifiedChips {
    fn default() -> Self {
        UnifiedChips { all_inboxes: true, starred: true, drafts: true, archive: true, filtered: true }
    }
}

impl UnifiedChips {
    /// Whether the row for `kind` shows its chip (Sent never does).
    pub fn has(self, kind: crate::models::FolderKind) -> bool {
        use crate::models::FolderKind::*;
        match kind {
            Inbox => self.all_inboxes,
            Starred => self.starred,
            Drafts => self.drafts,
            Archive => self.archive,
            _ => false,
        }
    }
}

impl UnifiedKinds {
    pub const NONE: UnifiedKinds =
        UnifiedKinds { starred: false, sent: false, drafts: false, archive: false };

    pub fn any(self) -> bool {
        self.starred || self.sent || self.drafts || self.archive
    }

    /// Whether the row for `kind` is on (only Starred, Sent and Drafts have
    /// one; anything else is `false`).
    pub fn has(self, kind: crate::models::FolderKind) -> bool {
        use crate::models::FolderKind::*;
        match kind {
            Starred => self.starred,
            Sent => self.sent,
            Drafts => self.drafts,
            Archive => self.archive,
            _ => false,
        }
    }

    /// The kinds with a row, in sidebar order.
    pub fn listed(self) -> Vec<crate::models::FolderKind> {
        use crate::models::FolderKind::*;
        [Starred, Sent, Drafts, Archive].into_iter().filter(|k| self.has(*k)).collect()
    }
}

pub fn load_filtered_placement() -> SectionPlacement {
    load_privacy().filtered_placement
}

pub fn load_tags_placement() -> SectionPlacement {
    load_privacy().tags_placement
}

/// Where a sidebar section (Filtered Folders, Tags) is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionPlacement {
    /// Inside the All Inboxes block, folding away with it. With All
    /// Inboxes hidden (a single account) this reads as `AboveAccounts`.
    #[default]
    AllInboxes,
    /// In the scrolling sidebar, above the first account.
    AboveAccounts,
    /// In the scrolling sidebar, after the last account.
    BelowAccounts,
}

pub fn load_chevrons_left() -> bool {
    load_privacy().chevrons_left
}

pub fn load_console_mode() -> bool {
    load_privacy().console_mode
}

/// When an opened message is marked read (#100).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadMark {
    /// The moment it is shown (conversation members as they scroll into view).
    #[default]
    Shown,
    /// After it has been in view for a couple of seconds.
    Delay,
    /// Never automatically; only an explicit mark-as-read.
    Manual,
}

pub fn load_read_mark() -> ReadMark {
    load_privacy().read_mark
}

/// What a hand-off of files from GNOME Files ("Send with Hylki", "Open
/// With Hylki", "Email…") opens them into.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesAction {
    /// A dialog offers the three below, every time.
    #[default]
    Ask,
    /// A new message with the files attached.
    New,
    /// Pick one of the saved drafts and attach the files to it.
    Draft,
    /// Pick a message to reply to, with the files attached.
    Reply,
}

/// What happens to handed-in files that add up to more than the limit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesLarge {
    /// A dialog asks, every time.
    #[default]
    Ask,
    /// Attach them regardless.
    Attach,
    /// Upload them to cloud storage and put the links in the message.
    Cloud,
}

/// The GNOME Files hand-off preferences (Settings → System → GNOME Files).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilesPrefs {
    pub action: FilesAction,
    pub large: FilesLarge,
    /// The size, in MB (decimal, like the sizes the composer shows), that
    /// the files together must stay under to be attached without a word.
    pub limit_mb: u32,
}

impl Default for FilesPrefs {
    fn default() -> Self {
        Self { action: FilesAction::Ask, large: FilesLarge::Ask, limit_mb: default_files_limit_mb() }
    }
}

impl FilesPrefs {
    /// The limit in bytes.
    pub fn limit_bytes(&self) -> u64 {
        self.limit_mb as u64 * 1_000_000
    }
}

fn default_files_limit_mb() -> u32 {
    20
}

pub fn load_files_prefs() -> FilesPrefs {
    let f = load_privacy();
    FilesPrefs { action: f.files_action, large: f.files_large, limit_mb: f.files_limit_mb.max(1) }
}


/// A mail filter rule (#47): file matching inbox arrivals into a folder,
/// Evolution-style, applied client-side whenever Hylki syncs the inbox.
///
/// A rule holds one or more conditions (#192). The first lives at the top
/// level as `field`/`matcher`/`value`, exactly where versions that knew only
/// one condition read it, so a filters file written here still loads there
/// (with the rest of the conditions ignored); `more` holds the others.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FilterRule {
    /// What to call the rule (#197). Optional: an unnamed rule is still
    /// listed by its conditions, the way every rule was before names
    /// existed. Kept first so it reads first in `filters.toml`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The account this rule (and its destination folder) belongs to.
    pub account_email: String,
    /// The first condition: what it inspects…
    pub field: FilterField,
    /// …how…
    pub matcher: FilterMatch,
    /// …and against what. Commas separate alternatives, any one of which
    /// matches (#192): "invoice, receipt".
    pub value: String,
    /// The conditions after the first (#192).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub more: Vec<FilterCondition>,
    /// Whether one matching condition is enough (true) or every condition
    /// must match (false, the default).
    #[serde(default, skip_serializing_if = "is_false")]
    pub any: bool,
    /// Destination folder path on the account. Empty leaves the mail where it
    /// is (a rule that only tags, #71).
    #[serde(default)]
    pub dest_path: String,
    /// The keyword of a [`Tag`] to put on matching mail (#71); empty tags
    /// nothing. Tagging and filing combine: the tag goes on first, and the
    /// move carries it along.
    #[serde(default)]
    pub tag: String,
    /// Whether the destination folder's unread mail counts toward the unread
    /// total (the All Inboxes chip, the tray icon and its menu, the
    /// Background Apps status), as inbox mail does (#116). Trash and Junk
    /// destinations never count, whatever this says.
    #[serde(default = "count_unread_default")]
    pub count_unread: bool,
}

fn count_unread_default() -> bool {
    true
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One condition of a [`FilterRule`]: what to look at, how, and for what.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FilterCondition {
    pub field: FilterField,
    pub matcher: FilterMatch,
    /// Commas separate alternatives; any one of them matching is a match.
    pub value: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterField {
    FromAddress,
    FromName,
    Subject,
    Recipients,
    /// The Reply-To header, or the From address when there is none (#191).
    ReplyTo,
    /// The message text (#191). Searched on the server as the inbox syncs
    /// (IMAP `SEARCH BODY`, Microsoft Graph `$search`), so nothing is
    /// downloaded for it; the list preview and a cached body are checked
    /// too. Always a "contains" match, whatever the matcher says: that is
    /// the only search a server offers.
    Body,
}

impl FilterField {
    /// Every field, in the order the editor lists them.
    pub const ALL: [FilterField; 6] = [
        FilterField::FromAddress,
        FilterField::FromName,
        FilterField::Subject,
        FilterField::Recipients,
        FilterField::ReplyTo,
        FilterField::Body,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterMatch {
    Contains,
    Equals,
    StartsWith,
    EndsWith,
}

impl FilterMatch {
    /// Every matcher, in the order the editor lists them.
    pub const ALL: [FilterMatch; 4] =
        [FilterMatch::Contains, FilterMatch::Equals, FilterMatch::StartsWith, FilterMatch::EndsWith];
}

/// What a message offers the conditions to look at.
#[derive(Clone, Copy, Debug, Default)]
pub struct FilterInput<'a> {
    pub from_addr: &'a str,
    pub from_name: &'a str,
    pub subject: &'a str,
    /// To and Cc together.
    pub recipients: &'a str,
    /// The Reply-To header; empty when absent.
    pub reply_to: &'a str,
    /// The message text on hand: the list preview, or the body when the
    /// list carries it.
    pub body: &'a str,
    /// The body alternatives the server confirmed for this message (#191),
    /// lowercased as [`FilterCondition::alternatives`] hands them out.
    pub body_hits: &'a [String],
}

impl FilterCondition {
    /// The alternatives a value names (#192): its comma-separated pieces,
    /// trimmed and lowercased, empties dropped. A value without a comma is
    /// one alternative.
    pub fn alternatives(value: &str) -> Vec<String> {
        value
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Case-insensitive match against one message.
    pub fn matches(&self, input: &FilterInput) -> bool {
        let alts = Self::alternatives(&self.value);
        if alts.is_empty() {
            return false;
        }
        if self.field == FilterField::Body {
            // The server's word (a hit) or the text on hand; a rule set up
            // on a backend without a search still works off the preview.
            let text = input.body.to_lowercase();
            return alts
                .iter()
                .any(|a| input.body_hits.iter().any(|h| h == a) || text.contains(a.as_str()));
        }
        let hay = match self.field {
            FilterField::FromAddress => input.from_addr,
            FilterField::FromName => input.from_name,
            FilterField::Subject => input.subject,
            FilterField::Recipients => input.recipients,
            FilterField::ReplyTo if input.reply_to.trim().is_empty() => input.from_addr,
            FilterField::ReplyTo => input.reply_to,
            FilterField::Body => unreachable!(),
        }
        .to_lowercase();
        // The recipients are a list. "Contains" reads it whole, names
        // included; the other matchers hold each recipient up on its own,
        // so "is x@y" matches mail sent to x@y and someone else, and "ends
        // with @y" finds any one recipient there, not only the last (#201).
        let hays = if self.field == FilterField::Recipients && self.matcher != FilterMatch::Contains {
            mailboxes(&hay)
        } else {
            vec![hay]
        };
        alts.iter().any(|needle| {
            hays.iter().any(|hay| match self.matcher {
                FilterMatch::Contains => hay.contains(needle.as_str()),
                FilterMatch::Equals => hay == needle,
                FilterMatch::StartsWith => hay.starts_with(needle.as_str()),
                FilterMatch::EndsWith => hay.ends_with(needle.as_str()),
            })
        })
    }
}

/// The pieces of a comma-separated recipient list a matcher can be held
/// against: every address on its own, and every display name on its own.
/// `Ann <ann@shop.example>, bob@shop.example` gives `ann`, `ann@shop.example`
/// and `bob@shop.example`.
fn mailboxes(list: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in list.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match (part.rfind('<'), part.rfind('>')) {
            (Some(lt), Some(gt)) if lt < gt => {
                let name = part[..lt].trim().trim_matches('"').trim();
                if !name.is_empty() {
                    out.push(name.to_string());
                }
                out.push(part[lt + 1..gt].trim().to_string());
            }
            _ => out.push(part.to_string()),
        }
    }
    out
}

impl FilterRule {
    /// Every condition, the first included.
    pub fn conditions(&self) -> Vec<FilterCondition> {
        let mut all = Vec::with_capacity(1 + self.more.len());
        all.push(FilterCondition { field: self.field, matcher: self.matcher, value: self.value.clone() });
        all.extend(self.more.iter().cloned());
        all
    }

    /// Replace the conditions: the first goes to the top level, the rest to
    /// `more`. Returns false (and changes nothing) when `conds` is empty.
    pub fn set_conditions(&mut self, mut conds: Vec<FilterCondition>) -> bool {
        if conds.is_empty() {
            return false;
        }
        let first = conds.remove(0);
        self.field = first.field;
        self.matcher = first.matcher;
        self.value = first.value;
        self.more = conds;
        true
    }

    /// What to call the rule in a list or a log line (#197): its name, or
    /// its conditions when it has none.
    pub fn label(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_string();
        }
        self.conditions()
            .iter()
            .map(|c| format!("{:?} {:?} {}", c.field, c.matcher, c.value))
            .collect::<Vec<_>>()
            .join(if self.any { " or " } else { " and " })
    }

    /// The body alternatives this rule needs a server search for (#191).
    pub fn body_needles(&self) -> Vec<String> {
        self.conditions()
            .iter()
            .filter(|c| c.field == FilterField::Body)
            .flat_map(|c| FilterCondition::alternatives(&c.value))
            .collect()
    }

    /// Case-insensitive match against a message: every condition must hold,
    /// or any one of them with `any` set.
    pub fn matches(&self, input: &FilterInput) -> bool {
        let conds = self.conditions();
        if self.any {
            conds.iter().any(|c| c.matches(input))
        } else {
            conds.iter().all(|c| c.matches(input))
        }
    }
}

/// Every body alternative the rules of `email` search for (#191), lowercased
/// and deduplicated, read fresh from disk: the worker asks at each inbox
/// sync, so a rule added in Settings counts from the next sync on.
pub fn filter_body_needles(email: &str) -> Vec<String> {
    let mut out: Vec<String> = load_filters()
        .iter()
        .filter(|r| r.account_email.eq_ignore_ascii_case(email))
        .flat_map(|r| r.body_needles())
        .collect();
    out.sort();
    out.dedup();
    out
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct FiltersFile {
    #[serde(default)]
    rules: Vec<FilterRule>,
}

fn filters_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("filters.toml"))
}

pub fn load_filters() -> Vec<FilterRule> {
    let Some(path) = filters_path() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    toml::from_str::<FiltersFile>(&text).map(|f| f.rules).unwrap_or_default()
}

pub fn save_filters(rules: &[FilterRule]) {
    let Some(path) = filters_path() else { return };
    let file = FiltersFile { rules: rules.to_vec() };
    match toml::to_string_pretty(&file) {
        Ok(toml) => {
            if let Err(e) = write_private(&path, &toml) {
                tracing::warn!("could not save filters: {e}");
            }
        }
        Err(e) => tracing::warn!("could not serialize filters: {e}"),
    }
}

/// A tag (#71): a name and a colour for one IMAP keyword. Keywords are the
/// standard's own per-message user flags, kept on the server beside `\Seen`
/// and `\Flagged`, so a tag set here is the same tag Thunderbird, Apple Mail
/// or a webmail shows — and theirs show here once a tag names their keyword
/// (Thunderbird's built-in five are `$label1`…`$label5`). Microsoft 365
/// stores them as categories; POP3 and servers that refuse custom keywords
/// keep them in Hylki's own cache instead.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tag {
    /// What the user sees.
    pub name: String,
    /// The keyword the tag is stored as: an IMAP atom (no spaces, quotes,
    /// parentheses, brackets, backslashes or non-ASCII), case-insensitive.
    pub keyword: String,
    /// `#rrggbb`.
    pub color: String,
}

/// The tag palette, GNOME's colour set at its middle strength.
pub const TAG_COLORS: &[&str] = &[
    "#1c71d8", "#2ec27e", "#f5c211", "#e66100", "#c01c28", "#813d9c", "#865e3c", "#77767b",
];

impl Tag {
    /// The characters an IMAP keyword (an atom) may hold.
    pub fn is_keyword_char(c: char) -> bool {
        c.is_ascii_graphic() && !matches!(c, '(' | ')' | '{' | '}' | '"' | '\\' | '%' | '*' | ']' | '[')
    }

    /// Whether `s` can be sent to a server as a keyword.
    pub fn valid_keyword(s: &str) -> bool {
        !s.is_empty() && s.chars().all(Self::is_keyword_char)
    }

    /// The keyword a tag named `name` gets unless the user picks one: the
    /// name with spaces turned to underscores and anything an atom cannot
    /// carry dropped ("To Do" → `To_Do`, "Réunion" → `Runion`).
    pub fn keyword_for(name: &str) -> String {
        name.trim()
            .chars()
            .map(|c| if c == ' ' { '_' } else { c })
            .filter(|c| Self::is_keyword_char(*c))
            .collect()
    }

    /// The CSS class carrying this tag's colour (see the app's tag stylesheet):
    /// the keyword reduced to what a class name may hold.
    pub fn css_class(&self) -> String {
        tag_css_class(&self.keyword)
    }

    /// The name to offer for a keyword found on a server (the tag finder):
    /// Thunderbird's five built-ins get Thunderbird's names; anything else
    /// sheds a leading `$`, turns `_`/`-` into spaces and capitalises each
    /// word (`travel_plans` → "Travel Plans", `$Receipts` → "Receipts").
    pub fn name_for_keyword(keyword: &str) -> String {
        match keyword.to_ascii_lowercase().as_str() {
            "$label1" => return "Important".to_string(),
            "$label2" => return "Work".to_string(),
            "$label3" => return "Personal".to_string(),
            "$label4" => return "To Do".to_string(),
            "$label5" => return "Later".to_string(),
            _ => {}
        }
        let bare = keyword.trim_start_matches('$');
        let name: String = bare
            .split(['_', '-'])
            .filter(|w| !w.is_empty())
            .map(|w| {
                let mut cs = w.chars();
                match cs.next() {
                    Some(first) => first.to_uppercase().chain(cs).collect::<String>(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        if name.is_empty() { keyword.to_string() } else { name }
    }

    /// The colour to offer for a found keyword: Thunderbird's built-ins get
    /// their Thunderbird colours (as the palette has them); the rest take
    /// the first palette colour not in `taken`, cycling once every colour is.
    pub fn color_for_keyword(keyword: &str, taken: &[String]) -> String {
        let fixed = match keyword.to_ascii_lowercase().as_str() {
            "$label1" => Some(TAG_COLORS[4]), // red
            "$label2" => Some(TAG_COLORS[3]), // orange
            "$label3" => Some(TAG_COLORS[1]), // green
            "$label4" => Some(TAG_COLORS[0]), // blue
            "$label5" => Some(TAG_COLORS[5]), // purple
            _ => None,
        };
        if let Some(c) = fixed {
            return c.to_string();
        }
        TAG_COLORS
            .iter()
            .find(|c| !taken.iter().any(|t| t.eq_ignore_ascii_case(c)))
            .copied()
            .unwrap_or(TAG_COLORS[taken.len() % TAG_COLORS.len()])
            .to_string()
    }
}

/// The CSS class for a keyword's colour: `tag-` plus the keyword lowercased,
/// with every character a class name can't carry turned into its code so two
/// keywords never share a class.
pub fn tag_css_class(keyword: &str) -> String {
    let mut out = String::from("tag-");
    for c in keyword.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            out.push_str(&format!("_{:x}", c as u32));
        }
    }
    out
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct TagsFile {
    #[serde(default)]
    tags: Vec<Tag>,
}

fn tags_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("tags.toml"))
}

pub fn load_tags() -> Vec<Tag> {
    let Some(path) = tags_path() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    toml::from_str::<TagsFile>(&text).map(|f| f.tags).unwrap_or_default()
}

pub fn save_tags(tags: &[Tag]) {
    let Some(path) = tags_path() else { return };
    let file = TagsFile { tags: tags.to_vec() };
    match toml::to_string_pretty(&file) {
        Ok(toml) => {
            if let Err(e) = write_private(&path, &toml) {
                tracing::warn!("could not save tags: {e}");
            }
        }
        Err(e) => tracing::warn!("could not serialize tags: {e}"),
    }
}

/// A portable settings bundle (#50): every configuration file Hylki keeps —
/// preferences, accounts (colours, emoji, labels, aliases, folder roles and
/// per-account push included), filters, tags, cloud storage accounts,
/// sidebar layout, window/pane state (the app icon choice with it), and
/// the words taught to the spell checker. Passwords and tokens never
/// appear — their fields are skip_serializing, and they live in the
/// keyring, not on disk; OpenPGP keys live in GnuPG's keyring, and the
/// optional oauth.toml (a client id and secret the user supplied by hand)
/// stays out for the same reason.
///
/// Sections added after the first release are optional, so a bundle from
/// before them imports without wiping what it does not mention.
#[derive(serde::Serialize, serde::Deserialize)]
struct SettingsBundle {
    version: u32,
    privacy: PrivacyFile,
    #[serde(default)]
    accounts: Vec<AccountConfig>,
    #[serde(default)]
    filters: Vec<FilterRule>,
    #[serde(default)]
    tags: Vec<Tag>,
    #[serde(default)]
    sidebar: Option<SidebarFile>,
    #[serde(default)]
    window: Option<WindowFile>,
    #[serde(default)]
    state: Option<StateFile>,
    /// Cloud storage accounts (#144 onward); their sign-ins stay in the
    /// keyring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cloud: Option<Vec<crate::cloud::CloudAccount>>,
    /// The spell checker's personal words, per language (#114).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    spell_words: Option<std::collections::BTreeMap<String, Vec<String>>>,
}

/// Parse one of the config directory's TOML files into its struct (None when
/// absent or unreadable — the bundle simply omits it).
fn read_file_struct<T: serde::de::DeserializeOwned>(path: Option<PathBuf>) -> Option<T> {
    let text = std::fs::read_to_string(path?).ok()?;
    toml::from_str(&text).ok()
}

/// Serialize a bundle section back to its config file.
fn write_file_struct<T: serde::Serialize>(
    path: Option<PathBuf>,
    value: &Option<T>,
) -> Result<(), String> {
    let (Some(path), Some(value)) = (path, value) else { return Ok(()) };
    let toml = toml::to_string_pretty(value).map_err(|e| e.to_string())?;
    write_private(&path, &toml).map_err(|e| e.to_string())
}

/// The current configuration as a TOML bundle for Export Settings.
pub fn export_bundle() -> Result<String, String> {
    let bundle = SettingsBundle {
        version: 1,
        privacy: load_privacy(),
        accounts: load().unwrap_or_default(),
        filters: load_filters(),
        tags: load_tags(),
        sidebar: read_file_struct(sidebar_path()),
        window: read_file_struct(window_path()),
        state: read_file_struct(state_path()),
        cloud: Some(crate::cloud::load_accounts()),
        spell_words: Some(crate::spell::all_personal_words()),
    };
    toml::to_string_pretty(&bundle).map_err(|e| e.to_string())
}

/// Parse and persist an exported bundle: privacy.toml and accounts.toml are
/// replaced (via the same writers the app uses; the keyring is untouched
/// since imported accounts carry no secrets). Returns the account count.
pub fn import_bundle(text: &str) -> Result<usize, String> {
    let bundle: SettingsBundle = toml::from_str(text).map_err(|e| e.to_string())?;
    if bundle.version != 1 {
        return Err(format!("unsupported bundle version {}", bundle.version));
    }
    let path = privacy_path().ok_or("no config directory")?;
    let toml = toml::to_string_pretty(&bundle.privacy).map_err(|e| e.to_string())?;
    write_private(&path, &toml).map_err(|e| e.to_string())?;
    save(&bundle.accounts).map_err(|e| e.to_string())?;
    save_filters(&bundle.filters);
    save_tags(&bundle.tags);
    write_file_struct(sidebar_path(), &bundle.sidebar)?;
    write_file_struct(window_path(), &bundle.window)?;
    write_file_struct(state_path(), &bundle.state)?;
    if let Some(cloud) = &bundle.cloud {
        crate::cloud::save_accounts(cloud);
    }
    // Words are merged, never removed: a backup can only teach the
    // checker more.
    if let Some(words) = &bundle.spell_words {
        for (lang, list) in words {
            crate::spell::merge_personal_words(lang, list);
        }
    }
    Ok(bundle.accounts.len())
}

pub fn load_single_message_card() -> bool {
    load_privacy().single_message_card
}

pub fn load_card_attachments() -> bool {
    load_privacy().card_attachments
}

pub fn load_attachment_drawer() -> bool {
    load_privacy().attachment_drawer
}

pub fn load_thread_expansion() -> bool {
    load_privacy().thread_expansion
}

/// Whether deleting a whole selected conversation asks for confirmation.
pub fn load_confirm_thread_delete() -> bool {
    load_privacy().confirm_thread_delete
}

/// How email message content is themed.
pub fn load_message_theme() -> MessageTheme {
    load_privacy().message_theme
}

/// The three reader-override preferences (#56): font switch, font, colour switch.
pub fn load_reader_override() -> (bool, String, bool) {
    let p = load_privacy();
    (p.override_fonts, p.reader_font, p.override_colors)
}

/// Plain-text messages in monospace (#181): the switch and the font.
pub fn load_plain_style() -> (bool, String) {
    let p = load_privacy();
    (p.plain_monospace, p.plain_font)
}

/// What a message is written in. Rich text is the WYSIWYG editor;
/// Markdown and HTML are source views that are converted on the way out;
/// plain text sends no HTML part at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComposeFormat {
    #[default]
    Rich,
    Markdown,
    Html,
    Plain,
}

impl ComposeFormat {
    /// Whether this format is edited as source rather than as rich text.
    pub fn is_source(self) -> bool {
        matches!(self, ComposeFormat::Markdown | ComposeFormat::Html)
    }
}

/// Where the split reply opens in the reading pane (#212): above the
/// messages (the original placement), below them, or wherever the newest
/// message is — above with "newest message first", below otherwise, so the
/// editor always continues the conversation in its reading direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyPosition {
    #[default]
    Top,
    Bottom,
    Follow,
}

pub fn load_reply_position() -> ReplyPosition {
    load_privacy().reply_position
}

/// What new messages start out as, falling back to the plain-text
/// switch this setting replaced (#180).
pub fn load_compose_format() -> ComposeFormat {
    let p = load_privacy();
    p.compose_format.unwrap_or(if p.compose_plain {
        ComposeFormat::Plain
    } else {
        ComposeFormat::Rich
    })
}

/// Whether desktop notifications (new mail, error alerts) are enabled.
pub fn load_notifications() -> bool {
    load_privacy().notifications
}

/// Whether new-mail notifications may name the sender and subject.
pub fn load_notification_content() -> bool {
    load_privacy().notification_content
}

/// Whether the sidebar shows the "Attachments" row.
pub fn load_show_attachments() -> bool {
    load_privacy().show_attachments
}

pub fn load_show_contacts() -> bool {
    load_privacy().show_contacts
}

/// Whether the settings window opens on the Accounts view (vs Preferences).
pub fn load_settings_open_accounts() -> bool {
    load_privacy().settings_open_accounts
}

/// Whether conversation card actions hide until hovered.
pub fn load_card_actions_hover() -> bool {
    load_privacy().card_actions_hover
}

/// With the ⋯ toggle off: whether card actions appear automatically on hover.
pub fn load_card_actions_auto() -> bool {
    load_privacy().card_actions_auto
}

/// Whether the message list rows carry an actions palette at all.
pub fn load_list_palette() -> bool {
    load_privacy().list_palette
}

/// Whether the list's actions palette opens on row hover (no ⋯ click).
pub fn load_list_palette_hover() -> bool {
    load_privacy().list_palette_hover
}

/// Whether a message card's ⋯ opens the card menu instead of sliding its
/// actions palette out.
pub fn load_card_palette_menu() -> bool {
    load_privacy().card_palette_menu
}

fn default_swipe_enabled() -> bool {
    true
}

/// Whether message rows take a sideways swipe (archive / delete).
pub fn load_swipe_enabled() -> bool {
    load_privacy().swipe_enabled
}

/// Whether the message list's swipe-gesture sides are swapped.
pub fn load_swipe_reversed() -> bool {
    load_privacy().swipe_reversed
}

/// The narrowest and widest trackpad swipe sensitivity the setting offers,
/// also the clamp a hand-edited file is held to (0 would divide by zero).
pub const SWIPE_SENSITIVITY_MIN: f64 = 1.0;
pub const SWIPE_SENSITIVITY_MAX: f64 = 10.0;

fn default_swipe_sensitivity() -> f64 {
    // libadwaita spends a fixed 400px of scroll on a full swipe whatever the
    // row asks for, so at 1.0 a trackpad needs 240px of two-finger travel to
    // reach the commit distance — more than most touchpads can give in one
    // go. 3.5 puts the row roughly under the fingers instead.
    3.5
}

/// How far a trackpad's two-finger swipe has to travel to fire the action
/// (higher = shorter swipe).
pub fn load_swipe_sensitivity() -> f64 {
    load_privacy()
        .swipe_sensitivity
        .clamp(SWIPE_SENSITIVITY_MIN, SWIPE_SENSITIVITY_MAX)
}

/// Whether "New message" composes inline over the reading pane.
pub fn load_compose_inline() -> bool {
    load_privacy().compose_inline
}

/// Whether pasting into the composer strips the clipboard's formatting.
pub fn load_paste_plain() -> bool {
    load_privacy().paste_plain
}

/// Whether the composer checks spelling as you type.
pub fn load_spellcheck() -> bool {
    load_privacy().spellcheck
}

/// The configured spell-checking languages (comma-separated; empty = locale).
pub fn load_spellcheck_langs() -> String {
    load_privacy().spellcheck_langs
}

pub fn load_sidebar_hover_expand() -> bool {
    load_privacy().sidebar_hover_expand
}

pub fn load_remember_sidebar() -> bool {
    load_privacy().remember_sidebar
}

pub fn load_remember_rail() -> bool {
    load_privacy().remember_rail
}

pub fn load_rail_dots() -> bool {
    load_privacy().rail_dots
}

pub fn load_rail_fold() -> RailFold {
    load_privacy().rail_fold
}

/// "Fold up expanded items" (Settings → Sidebar → Icon rail): while the
/// sidebar is collapsed to its icon rail, the items ticked here show folded
/// up and stay so — their saved expansion is untouched and returns when the
/// sidebar expands. Each item has its own switch under the master; all are
/// on until switched off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RailFold {
    /// The master switch: off, nothing folds for the rail.
    #[serde(default = "default_on")]
    pub enabled: bool,
    /// Every account's folder list.
    #[serde(default = "default_on")]
    pub accounts: bool,
    /// The per-account inbox list under All Inboxes.
    #[serde(default = "default_on")]
    pub all_inboxes: bool,
    /// The unified Starred / Sent / Drafts rows' account lists.
    #[serde(default = "default_on")]
    pub starred: bool,
    #[serde(default = "default_on")]
    pub sent: bool,
    #[serde(default = "default_on")]
    pub drafts: bool,
    #[serde(default = "default_on")]
    pub archive: bool,
    /// The Filtered Folders section.
    #[serde(default = "default_on")]
    pub filtered: bool,
    /// The Tags section.
    #[serde(default = "default_on")]
    pub tags: bool,
}

fn default_on() -> bool {
    true
}

impl Default for RailFold {
    fn default() -> Self {
        RailFold {
            enabled: true,
            accounts: true,
            all_inboxes: true,
            starred: true,
            sent: true,
            drafts: true,
            archive: true,
            filtered: true,
            tags: true,
        }
    }
}

impl RailFold {
    /// Whether the accounts fold for the rail.
    pub fn folds_accounts(self) -> bool {
        self.enabled && self.accounts
    }

    /// Whether the unified row for `kind` folds for the rail (Inbox is All
    /// Inboxes; only Starred, Sent and Drafts have rows besides it).
    pub fn folds_kind(self, kind: crate::models::FolderKind) -> bool {
        use crate::models::FolderKind::*;
        self.enabled
            && match kind {
                Inbox => self.all_inboxes,
                Starred => self.starred,
                Sent => self.sent,
                Drafts => self.drafts,
                Archive => self.archive,
                _ => false,
            }
    }

    pub fn folds_filtered(self) -> bool {
        self.enabled && self.filtered
    }

    pub fn folds_tags(self) -> bool {
        self.enabled && self.tags
    }
}

pub fn load_app_theme() -> AppTheme {
    load_privacy().app_theme
}

/// The appearance theme's id; "system" (the stock look) when unset.
pub fn load_theme() -> String {
    let id = load_privacy().theme;
    if id.is_empty() {
        crate::theme::SYSTEM_ID.to_string()
    } else {
        id
    }
}

/// Lines of message text shown under the subject in the list; 0 means previews
/// are off. Clamped in case the file was edited by hand.
pub fn load_preview_lines() -> u32 {
    load_privacy().preview_lines.min(3)
}

/// Whether single-key (modifier-free) shortcuts are enabled.
pub fn load_single_key_shortcuts() -> bool {
    load_privacy().single_key_shortcuts
}

/// Whether Hylki keeps running once its window is closed.
pub fn load_run_in_background() -> bool {
    load_privacy().run_in_background
}

/// Whether Hylki starts at login (background running only).
pub fn load_autostart() -> bool {
    let p = load_privacy();
    p.run_in_background && p.autostart
}

/// Whether Hylki publishes a tray icon.
pub fn load_tray() -> bool {
    load_privacy().tray
}

/// Which icon the tray item shows.
pub fn load_tray_icon() -> TrayIcon {
    load_privacy().tray_icon
}

fn default_tray_mail() -> bool {
    true
}

/// Whether the tray menu lists unread inbox mail.
pub fn load_tray_mail() -> bool {
    load_privacy().tray_mail
}

/// Persist all app settings together (so no field is clobbered).
#[allow(clippy::too_many_arguments)]
pub fn save_privacy(
    senders: &[String],
    auto_remote_content: bool,
    gravatar: bool,
    avatars: bool,
    own_mailbox_face: bool,
    sender_logos: bool,
    date_style: DateStyle,
    clock_style: ClockStyle,
    fetch_interval_secs: u64,
    push: bool,
    blacklist: &[String],
    palette_collapse_secs: u64,
    card_palette_collapse_secs: u64,
    threading: bool,
    threads_expanded: bool,
    thread_expansion: bool,
    thread_newest_first: bool,
    always_show_recipients: bool,
    single_message_card: bool,
    card_attachments: bool,
    attachment_drawer: bool,
    confirm_thread_delete: bool,
    message_theme: MessageTheme,
    override_fonts: bool,
    reader_font: String,
    override_colors: bool,
    plain_monospace: bool,
    plain_font: String,
    notifications: bool,
    notification_content: bool,
    show_attachments: bool,
    show_contacts: bool,
    settings_open_accounts: bool,
    card_actions_hover: bool,
    card_actions_auto: bool,
    list_palette: bool,
    list_palette_hover: bool,
    card_palette_menu: bool,
    swipe_enabled: bool,
    swipe_reversed: bool,
    swipe_sensitivity: f64,
    compose_inline: bool,
    reply_fields: bool,
    compose_default_from: &str,
    paste_plain: bool,
    compose_format: ComposeFormat,
    reply_position: ReplyPosition,
    spellcheck: bool,
    spellcheck_langs: String,
    preview_lines: u32,
    single_key_shortcuts: bool,
    run_in_background: bool,
    autostart: bool,
    tray: bool,
    tray_icon: TrayIcon,
    tray_mail: bool,
    show_remote_banner: bool,
    sidebar_hover_expand: bool,
    remember_sidebar: bool,
    remember_rail: bool,
    rail_dots: bool,
    rail_fold: RailFold,
    app_theme: AppTheme,
    theme: String,
    show_unified: bool,
    unified_chips: UnifiedChips,
    unified_filtered: bool,
    unified_kinds: UnifiedKinds,
    unified_tags: bool,
    show_accounts: bool,
    filtered_placement: SectionPlacement,
    tags_placement: SectionPlacement,
    chevrons_left: bool,
    console_mode: bool,
    read_mark: ReadMark,
    files: FilesPrefs,
) {
    let Some(path) = privacy_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = PrivacyFile {
        allowed_senders: senders.to_vec(),
        auto_remote_content,
        gravatar,
        avatars,
        own_mailbox_face,
        sender_logos,
        date_style,
        clock_style,
        fetch_interval_secs,
        push,
        blacklist: blacklist.to_vec(),
        palette_collapse_secs,
        card_palette_collapse_secs,
        threading,
        threads_expanded,
        thread_expansion,
        thread_newest_first,
        always_show_recipients,
        single_message_card,
        card_attachments,
        attachment_drawer,
        confirm_thread_delete,
        message_theme,
        override_fonts,
        reader_font,
        override_colors,
        plain_monospace,
        plain_font,
        notifications,
        notification_content,
        show_attachments,
        show_contacts,
        settings_open_accounts,
        card_actions_hover,
        card_actions_auto,
        list_palette,
        list_palette_hover,
        card_palette_menu,
        swipe_enabled,
        swipe_reversed,
        swipe_sensitivity,
        compose_inline,
        reply_fields,
        compose_default_from: compose_default_from.to_string(),
        paste_plain,
        // Both are written: the boolean is what an older version reads.
        compose_plain: compose_format == ComposeFormat::Plain,
        compose_format: Some(compose_format),
        reply_position,
        spellcheck,
        // Every save is after the first load, which applied it.
        single_card_default_applied: true,
        spellcheck_langs,
        preview_lines,
        single_key_shortcuts,
        run_in_background,
        autostart,
        tray,
        tray_icon,
        tray_mail,
        show_remote_banner,
        sidebar_hover_expand,
        remember_sidebar,
        remember_rail,
        rail_dots,
        rail_fold,
        app_theme,
        theme,
        show_unified,
        unified_chip: unified_chips.all_inboxes,
        unified_chips,
        unified_filtered,
        unified_kinds,
        unified_tags,
        show_accounts,
        filtered_placement,
        tags_placement,
        chevrons_left,
        console_mode,
        read_mark,
        files_action: files.action,
        files_large: files.large,
        files_limit_mb: files.limit_mb,
    };
    match toml::to_string_pretty(&file) {
        Ok(toml) => {
            if let Err(e) = write_private(&path, &toml) {
                tracing::warn!("could not save privacy settings: {e}");
            }
        }
        Err(e) => tracing::warn!("could not serialize privacy settings: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Sidebar state (account display order + collapsed accounts), keyed by email
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, Serialize)]
struct SidebarFile {
    /// Account emails in the user's preferred display order.
    #[serde(default)]
    order: Vec<String>,
    /// Account emails whose folder list is collapsed.
    #[serde(default)]
    collapsed: Vec<String>,
    /// Account emails whose custom-folders section is expanded (default hidden).
    #[serde(default)]
    folders_expanded: Vec<String>,
    /// Whether the whole sidebar is in icon-only (collapsed) mode.
    #[serde(default)]
    icon_only: bool,
    /// Collapsed folder-tree nodes, as "email\tpath" entries.
    #[serde(default)]
    tree_collapsed: Vec<String>,
    /// The three sections' open state; open when the file predates them,
    /// which is how they always started.
    #[serde(default = "default_on")]
    unified_expanded: bool,
    #[serde(default = "default_on")]
    filtered_expanded: bool,
    #[serde(default = "default_on")]
    tags_expanded: bool,
    /// The unified Starred / Sent / Drafts rows' account lists; closed
    /// until opened.
    #[serde(default)]
    starred_expanded: bool,
    #[serde(default)]
    sent_expanded: bool,
    #[serde(default)]
    drafts_expanded: bool,
    #[serde(default)]
    archive_expanded: bool,
    /// Account emails whose own Filtered Folders / Tags sections are open
    /// (closed by default, like their custom folders).
    #[serde(default)]
    filtered_expanded_accounts: Vec<String>,
    #[serde(default)]
    tags_expanded_accounts: Vec<String>,
}

fn sidebar_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("sidebar.toml"))
}

/// Sidebar state persisted across restarts.
#[derive(Debug, Default)]
pub struct SidebarState {
    /// Account emails in display order.
    pub order: Vec<String>,
    /// Account emails whose folder list is collapsed.
    pub collapsed: Vec<String>,
    /// Account emails whose custom-folders section is expanded (default hidden).
    pub folders_expanded: Vec<String>,
    /// Whether the sidebar is in icon-only mode.
    pub icon_only: bool,
    /// Collapsed folder-tree nodes, as "email\tpath" entries.
    pub tree_collapsed: Vec<String>,
    /// Whether the per-account inbox list under All Inboxes is open.
    pub unified_expanded: bool,
    /// Whether the Filtered Folders section is open.
    pub filtered_expanded: bool,
    /// Whether the Tags section is open.
    pub tags_expanded: bool,
    /// The unified Starred / Sent / Drafts rows' account lists.
    pub starred_expanded: bool,
    pub sent_expanded: bool,
    pub drafts_expanded: bool,
    pub archive_expanded: bool,
    /// Account emails whose own Filtered Folders / Tags sections are open.
    pub filtered_expanded_accounts: Vec<String>,
    pub tags_expanded_accounts: Vec<String>,
}

pub fn load_sidebar_state() -> SidebarState {
    let Some(path) = sidebar_path() else {
        return SidebarState::default();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return SidebarState::default();
    };
    toml::from_str::<SidebarFile>(&text)
        .map(|s| SidebarState {
            order: s.order,
            collapsed: s.collapsed,
            folders_expanded: s.folders_expanded,
            icon_only: s.icon_only,
            tree_collapsed: s.tree_collapsed,
            unified_expanded: s.unified_expanded,
            filtered_expanded: s.filtered_expanded,
            tags_expanded: s.tags_expanded,
            starred_expanded: s.starred_expanded,
            sent_expanded: s.sent_expanded,
            drafts_expanded: s.drafts_expanded,
            archive_expanded: s.archive_expanded,
            filtered_expanded_accounts: s.filtered_expanded_accounts,
            tags_expanded_accounts: s.tags_expanded_accounts,
        })
        .unwrap_or_default()
}

pub fn save_sidebar_state(state: &SidebarState) {
    let Some(path) = sidebar_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = SidebarFile {
        order: state.order.clone(),
        collapsed: state.collapsed.clone(),
        folders_expanded: state.folders_expanded.clone(),
        icon_only: state.icon_only,
        tree_collapsed: state.tree_collapsed.clone(),
        unified_expanded: state.unified_expanded,
        filtered_expanded: state.filtered_expanded,
        tags_expanded: state.tags_expanded,
        starred_expanded: state.starred_expanded,
        sent_expanded: state.sent_expanded,
        drafts_expanded: state.drafts_expanded,
        archive_expanded: state.archive_expanded,
        filtered_expanded_accounts: state.filtered_expanded_accounts.clone(),
        tags_expanded_accounts: state.tags_expanded_accounts.clone(),
    };
    match toml::to_string_pretty(&file) {
        Ok(toml) => {
            if let Err(e) = write_private(&path, &toml) {
                tracing::warn!("could not save sidebar state: {e}");
            }
        }
        Err(e) => tracing::warn!("could not serialize sidebar state: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Window state (size + maximized). Position/monitor can't be persisted on
// Wayland — the compositor owns window placement.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
struct WindowFile {
    width: i32,
    height: i32,
    #[serde(default)]
    maximized: bool,
}

impl Default for WindowFile {
    fn default() -> Self {
        Self { width: 1280, height: 840, maximized: false }
    }
}

fn window_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("window.toml"))
}

/// Returns the saved `(width, height, maximized)`, or sensible defaults.
pub fn load_window_state() -> (i32, i32, bool) {
    let file = window_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| toml::from_str::<WindowFile>(&t).ok())
        .unwrap_or_default();
    // Guard against absurd/zero sizes from a bad file.
    let width = if file.width >= 360 { file.width } else { 1280 };
    let height = if file.height >= 300 { file.height } else { 840 };
    (width, height, file.maximized)
}

pub fn save_window_state(width: i32, height: i32, maximized: bool) {
    let Some(path) = window_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = WindowFile { width, height, maximized };
    if let Ok(toml) = toml::to_string_pretty(&file) {
        let _ = std::fs::write(&path, toml);
    }
}

// ---------------------------------------------------------------------------
// Keyring health check + one-time setup-help flag
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, Serialize)]
struct StateFile {
    /// Set once the user dismisses the Linux Mint keyring setup tip.
    #[serde(default)]
    mint_keyring_help_dismissed: bool,
    /// In-message attachment drawer: collapsed (showing only its header).
    #[serde(default)]
    drawer_collapsed: bool,
    /// Expanded attachment-drawer height in px (the dragged split).
    #[serde(default = "default_drawer_height")]
    drawer_height: i32,
    /// Attachment drawer shows an alphabetical list instead of the thumbnail grid.
    #[serde(default)]
    drawer_list_view: bool,
    /// The drawer's list view sorts Z→A instead of A→Z.
    #[serde(default)]
    drawer_sort_desc: bool,
    /// Attachments gallery shows a table instead of the thumbnail grid.
    #[serde(default)]
    gallery_table_view: bool,
    /// Attachments gallery thumbnail cell width in px.
    #[serde(default = "default_gallery_thumb_width")]
    gallery_thumb_width: i32,
    /// Attachments gallery sort criterion (the sort dropdown's row index).
    #[serde(default)]
    gallery_sort: u32,
    /// Attachments gallery: pull from Archive folders.
    #[serde(default = "default_on")]
    gallery_archive: bool,
    /// Attachments gallery: pull from folders that are neither Inbox nor
    /// Archive (custom folders and Starred).
    #[serde(default = "default_on")]
    gallery_other: bool,
    /// Attachments gallery: folders the user ticked or unticked by hand in the
    /// folder list, as "<account id>\t<path>\t<0|1>". Only folders that differ
    /// from their kind's default are stored, so a new folder on the server
    /// follows the default rather than an entry that predates it.
    #[serde(default)]
    gallery_folders: Vec<String>,
    /// Message-list pane width in px (#28 — it reset every launch).
    #[serde(default = "default_list_pane_width")]
    list_pane_width: i32,
    /// Contacts view: the contact-list pane's width in px.
    #[serde(default = "default_contacts_pane_width")]
    contacts_pane_width: i32,
    /// The About window's height, remembering the user's vertical resize.
    /// (The settings window's is fixed since its two-pane layout, #141; an
    /// old `prefs_height` key in the file is ignored.)
    #[serde(default = "default_about_height")]
    about_height: i32,
    /// Split-reply panel height in px (the dragged divider). 0 = never
    /// dragged: the panel opens at its computed default.
    #[serde(default)]
    split_reply_height: i32,
    /// The welcome wizard has been completed once (Start Reading pressed),
    /// so an install with no accounts is not greeted with it again — a
    /// restart right after the wizard, for the app icon, must not loop.
    #[serde(default)]
    wizard_completed: bool,
    /// The icon generation whose default has been asserted over the stored
    /// choice (see `app_icon::ICON_GENERATION`): a release that brings a new
    /// authoritative icon bumps the constant, and the first start on it puts
    /// that icon on every install once, whatever was chosen before.
    #[serde(default)]
    app_icon_generation: u32,
    /// The chosen app icon (an id from `app_icon::catalog`). Absent until
    /// the first start of a build that offers the choice settles it — see
    /// `app_icon::init_on_startup`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    app_icon: Option<String>,
}

fn default_about_height() -> i32 {
    740
}

fn default_list_pane_width() -> i32 {
    350
}

fn default_contacts_pane_width() -> i32 {
    280
}

fn default_drawer_height() -> i32 {
    160
}

fn default_gallery_thumb_width() -> i32 {
    230
}

fn state_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("state.toml"))
}

/// Read the remembered UI state. A missing or unreadable file falls back to
/// deserializing an empty document, not to `StateFile::default()`: the derived
/// Default hands every field its type's zero, which is not what the
/// `#[serde(default = …)]` on each one says a first run should get (a pane
/// width of 0, a gallery that pulls from nothing). Every field carries a serde
/// default, so the empty document always parses.
fn load_state() -> StateFile {
    state_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| toml::from_str::<StateFile>(&t).ok())
        .or_else(|| toml::from_str::<StateFile>("").ok())
        .unwrap_or_default()
}

fn save_state(state: &StateFile) {
    let Some(path) = state_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(toml) = toml::to_string_pretty(state) {
        let _ = std::fs::write(&path, toml);
    }
}

/// Whether the welcome wizard has been completed before.
pub fn wizard_completed() -> bool {
    load_state().wizard_completed
}

pub fn mark_wizard_completed() {
    let mut s = load_state();
    if !s.wizard_completed {
        s.wizard_completed = true;
        save_state(&s);
    }
}

/// The chosen app icon id, if one has been settled.
pub fn load_app_icon() -> Option<String> {
    load_state().app_icon.filter(|s| !s.is_empty())
}

pub fn load_app_icon_generation() -> u32 {
    load_state().app_icon_generation
}

pub fn save_app_icon_generation(generation: u32) {
    let mut s = load_state();
    s.app_icon_generation = generation;
    save_state(&s);
}

pub fn save_app_icon(id: &str) {
    let mut s = load_state();
    s.app_icon = Some(id.to_string());
    save_state(&s);
}

/// Whether this install has any settings on disk at all — how a build that
/// changes a default tells an existing install from a fresh one.
pub fn settings_on_disk() -> bool {
    let Some(dir) = config_base().map(|b| b.join("hylki")) else { return false };
    ["accounts.toml", "privacy.toml", "state.toml", "sidebar.toml", "window.toml"]
        .iter()
        .any(|f| dir.join(f).exists())
}

/// Whether the one-time Mint keyring setup tip has already been dismissed.
pub fn mint_keyring_help_dismissed() -> bool {
    load_state().mint_keyring_help_dismissed
}

/// Persist that the user dismissed the Mint keyring setup tip ("Don't show again").
pub fn dismiss_mint_keyring_help() {
    let mut state = load_state();
    state.mint_keyring_help_dismissed = true;
    save_state(&state);
}

/// Persisted state of the in-message attachment drawer.
#[derive(Debug, Clone, Copy)]
pub struct DrawerState {
    /// Expanded content height in px.
    pub height: i32,
    /// Whether the drawer is collapsed to just its header.
    pub collapsed: bool,
    /// Thumbnail edge in px.
    pub thumb: i32,
    /// Show an alphabetical list instead of the thumbnail grid.
    pub list_view: bool,
    /// Sort the list Z→A instead of A→Z.
    pub sort_desc: bool,
}

impl Default for DrawerState {
    fn default() -> Self {
        Self { height: 160, collapsed: false, thumb: 56, list_view: false, sort_desc: false }
    }
}

/// Load the attachment drawer's remembered state. The collapsed flag, view
/// settings and dragged height are persisted; thumbnail size always starts at
/// its default.
pub fn load_drawer_state() -> DrawerState {
    let s = load_state();
    DrawerState {
        collapsed: s.drawer_collapsed,
        list_view: s.drawer_list_view,
        sort_desc: s.drawer_sort_desc,
        height: s.drawer_height.clamp(96, 4000),
        ..DrawerState::default()
    }
}

/// Persist whether the attachment drawer is collapsed.
pub fn save_drawer_collapsed(collapsed: bool) {
    let mut s = load_state();
    s.drawer_collapsed = collapsed;
    save_state(&s);
}

/// Persist the attachment drawer's expanded (dragged) height.
pub fn save_drawer_height(height: i32) {
    let mut s = load_state();
    s.drawer_height = height.clamp(96, 4000);
    save_state(&s);
}

/// Persist the attachment drawer's view mode (list vs. thumbnail grid).
pub fn save_drawer_list_view(list_view: bool) {
    let mut s = load_state();
    s.drawer_list_view = list_view;
    save_state(&s);
}

/// Persist the attachment drawer's list sort direction.
pub fn save_drawer_sort_desc(desc: bool) {
    let mut s = load_state();
    s.drawer_sort_desc = desc;
    save_state(&s);
}

/// The attachments gallery's remembered view settings:
/// (table view, thumbnail width px, sort index).
pub fn load_gallery_view() -> (bool, i32, u32) {
    let s = load_state();
    (
        s.gallery_table_view,
        s.gallery_thumb_width.clamp(140, 380),
        s.gallery_sort,
    )
}

/// Persist whether the attachments gallery shows the table view.
pub fn save_gallery_table_view(table: bool) {
    let mut s = load_state();
    s.gallery_table_view = table;
    save_state(&s);
}

/// Persist the attachments gallery's thumbnail width.
pub fn save_gallery_thumb_width(width: i32) {
    let mut s = load_state();
    s.gallery_thumb_width = width;
    save_state(&s);
}

/// Persist the attachments gallery's sort criterion (dropdown row index).
pub fn save_gallery_sort(sort: u32) {
    let mut s = load_state();
    s.gallery_sort = sort;
    save_state(&s);
}

/// One per-folder gallery override as it is stored: "<account id>\t<path>\t<0|1>".
/// A mailbox path can hold almost any byte a server chooses, but never a tab.
fn encode_gallery_folder(id: u32, path: &str, on: bool) -> String {
    format!("{id}\t{path}\t{}", u8::from(on))
}

/// Read back [`encode_gallery_folder`]; `None` for an entry that is not in that
/// shape, so a hand-edited or older file loses only the bad line.
fn decode_gallery_folder(entry: &str) -> Option<(u32, String, bool)> {
    let mut parts = entry.splitn(3, '\t');
    let id: u32 = parts.next()?.parse().ok()?;
    let path = parts.next()?;
    let on = parts.next()?;
    if path.is_empty() {
        return None;
    }
    Some((id, path.to_string(), on == "1"))
}

/// The attachments gallery's remembered folder scope: (pull from Archive,
/// pull from other folders, per-folder overrides as (account id, path, on)).
pub fn load_gallery_scope() -> (bool, bool, Vec<(u32, String, bool)>) {
    let s = load_state();
    let folders = s
        .gallery_folders
        .iter()
        .filter_map(|e| decode_gallery_folder(e))
        .collect();
    (s.gallery_archive, s.gallery_other, folders)
}

/// Persist the attachments gallery's folder scope.
pub fn save_gallery_scope(archive: bool, other: bool, folders: &[(u32, String, bool)]) {
    let mut s = load_state();
    s.gallery_archive = archive;
    s.gallery_other = other;
    s.gallery_folders = folders
        .iter()
        .map(|(id, path, on)| encode_gallery_folder(*id, path, *on))
        .collect();
    save_state(&s);
}

/// The About window's height: tall by default, remembering the user's own
/// vertical resize across restarts.
/// The split-reply panel's dragged height; 0 when it has never been dragged
/// (the caller computes an opening default from the pane).
pub fn load_split_reply_height() -> i32 {
    let h = load_state().split_reply_height;
    if h == 0 { 0 } else { h.clamp(220, 4000) }
}

pub fn save_split_reply_height(height: i32) {
    let mut s = load_state();
    s.split_reply_height = height.clamp(220, 4000);
    save_state(&s);
}

pub fn load_about_height() -> i32 {
    load_state().about_height.clamp(400, 4000)
}

pub fn save_about_height(height: i32) {
    let mut s = load_state();
    s.about_height = height.clamp(400, 4000);
    save_state(&s);
}

/// The message-list pane's remembered width (clamped to something sane).
pub fn load_list_pane_width() -> i32 {
    load_state().list_pane_width.clamp(324, 4000)
}

/// Persist the message-list pane's width (#28).
pub fn save_list_pane_width(width: i32) {
    let mut s = load_state();
    s.list_pane_width = width.clamp(324, 4000);
    save_state(&s);
}

/// The contacts view's remembered list-pane width (280 is also its floor).
pub fn load_contacts_pane_width() -> i32 {
    load_state().contacts_pane_width.clamp(280, 4000)
}

pub fn save_contacts_pane_width(width: i32) {
    let mut s = load_state();
    s.contacts_pane_width = width.clamp(280, 4000);
    save_state(&s);
}


#[cfg(test)]
mod tests {
    use super::{decode_gallery_folder, encode_gallery_folder, ConfigFile, PrivacyFile, StateFile};

    /// Every gallery folder override survives the trip to the file and back,
    /// including the paths servers really use: dots, slashes, spaces, UTF-7.
    #[test]
    fn a_gallery_folder_override_round_trips() {
        let cases = [
            (1, "INBOX", true),
            (2, "INBOX.Sent", false),
            (3, "Archive/2026", true),
            (7, "Saved Mail", false),
            (11, "INBOX.&AMQA5gDo-", true),
        ];
        for (id, path, on) in cases {
            let encoded = encode_gallery_folder(id, path, on);
            assert_eq!(
                decode_gallery_folder(&encoded),
                Some((id, path.to_string(), on)),
                "{path} did not survive the round trip",
            );
        }
    }

    /// A line that is not in the stored shape is dropped, not guessed at.
    #[test]
    fn a_malformed_gallery_override_is_skipped() {
        for bad in ["", "1", "1\tInbox", "notanid\tInbox\t1", "1\t\t1"] {
            assert_eq!(decode_gallery_folder(bad), None, "{bad:?} should not decode");
        }
    }

    /// A first run with no state file must land on the `#[serde(default = …)]`
    /// each field declares, not on the zero its type happens to have: the
    /// gallery's two master switches start on, and the panes have real widths.
    #[test]
    fn a_missing_state_file_falls_back_to_the_declared_defaults() {
        let fresh: StateFile = toml::from_str("").expect("an empty state document must parse");
        assert!(fresh.gallery_archive);
        assert!(fresh.gallery_other);
        assert_eq!(fresh.gallery_thumb_width, super::default_gallery_thumb_width());
        assert_eq!(fresh.list_pane_width, super::default_list_pane_width());
    }

    #[test]
    fn plain_string_aliases_still_load() {
        // The pre-per-alias-SMTP format (#34): a bare array of identity strings.
        let cfg: ConfigFile = toml::from_str(
            r#"
            [[accounts]]
            name = "Ann"
            email = "ann@example.org"
            imap_host = "imap.example.org"
            username = "ann"
            aliases = ["Ann Work <ann@work.org>", "ann@shop.org"]
            "#,
        )
        .unwrap();
        let aliases = &cfg.accounts[0].aliases;
        assert_eq!(aliases.len(), 2);
        assert_eq!(aliases[0].identity, "Ann Work <ann@work.org>");
        assert_eq!(aliases[0].address(), "ann@work.org");
        assert!(!aliases[0].has_own_smtp(), "a plain alias rides the account's SMTP");
        assert_eq!(aliases[1].address(), "ann@shop.org");
    }

    #[test]
    fn aliases_can_carry_their_own_smtp() {
        let cfg: ConfigFile = toml::from_str(
            r#"
            [[accounts]]
            name = "Ann"
            email = "ann@example.org"
            imap_host = "imap.example.org"
            username = "ann"

            [[accounts.aliases]]
            identity = "Ann Work <ann@work.org>"
            smtp_host = "smtp.work.org"
            smtp_port = 465
            smtp_username = "ann@work.org"
            "#,
        )
        .unwrap();
        let alias = &cfg.accounts[0].aliases[0];
        assert!(alias.has_own_smtp());
        assert_eq!(alias.smtp_host, "smtp.work.org");
        assert_eq!(alias.smtp_port, 465);
        assert_eq!(alias.smtp_username, "ann@work.org");
        assert!(alias.smtp_password.is_empty(), "passwords live in the keyring");
    }

    #[test]
    fn alias_smtp_password_never_reaches_disk() {
        let mut cfg: ConfigFile = toml::from_str(
            r#"
            [[accounts]]
            name = "Ann"
            email = "ann@example.org"
            imap_host = "imap.example.org"
            username = "ann"

            [[accounts.aliases]]
            identity = "ann@work.org"
            smtp_host = "smtp.work.org"
            smtp_username = "ann"
            "#,
        )
        .unwrap();
        cfg.accounts[0].aliases[0].smtp_password = "hunter2".into();
        let out = toml::to_string_pretty(&cfg).unwrap();
        assert!(!out.contains("hunter2"), "password serialized to disk: {out}");
        // And the round trip keeps the alias's transport settings.
        let back: ConfigFile = toml::from_str(&out).unwrap();
        let alias = &back.accounts[0].aliases[0];
        assert_eq!(alias.smtp_host, "smtp.work.org");
        assert_eq!(alias.smtp_port, 587, "unwritten port falls back to the default");
    }

    #[test]
    fn preview_lines_default_to_one_and_stay_in_range() {
        // An older privacy.toml has no key at all.
        let p: PrivacyFile = toml::from_str("").unwrap();
        assert_eq!(p.preview_lines, 1);
        // Hand-edited nonsense must not make the list build rows of 40 lines, or
        // of none: the setting offers 1–3 and that is what it is worth honouring.
        // 0 is a real setting — previews off — but nothing above 3 is.
        for (written, expected) in [(0, 0), (1, 1), (3, 3), (99, 3)] {
            let p: PrivacyFile =
                toml::from_str(&format!("preview_lines = {written}")).unwrap();
            assert_eq!(p.preview_lines.min(3), expected, "for {written}");
        }
    }

    #[test]
    fn notifications_default_on_when_absent() {
        // An older privacy.toml with no `notifications` key opts in by default.
        let p: PrivacyFile = toml::from_str("").unwrap();
        assert!(p.notifications);
    }

    #[test]
    fn notifications_can_be_disabled() {
        let p: PrivacyFile = toml::from_str("notifications = false").unwrap();
        assert!(!p.notifications);
    }
}

#[cfg(test)]
mod filter_tests {
    use super::*;

    fn rule(field: FilterField, matcher: FilterMatch, value: &str) -> FilterRule {
        FilterRule {
            name: String::new(),
            account_email: "a@b.c".into(),
            field,
            matcher,
            value: value.into(),
            more: Vec::new(),
            any: false,
            dest_path: "Archive".into(),
            tag: String::new(),
            count_unread: true,
        }
    }

    fn cond(field: FilterField, matcher: FilterMatch, value: &str) -> FilterCondition {
        FilterCondition { field, matcher, value: value.into() }
    }

    /// The four header fields the original rules looked at.
    fn headers<'a>(
        from_addr: &'a str,
        from_name: &'a str,
        subject: &'a str,
        recipients: &'a str,
    ) -> FilterInput<'a> {
        FilterInput { from_addr, from_name, subject, recipients, ..Default::default() }
    }

    #[test]
    fn found_keywords_get_names_and_colours() {
        // Thunderbird's built-ins keep Thunderbird's names and colours.
        assert_eq!(Tag::name_for_keyword("$label1"), "Important");
        assert_eq!(Tag::name_for_keyword("$LABEL4"), "To Do");
        assert_eq!(Tag::color_for_keyword("$label1", &[]), TAG_COLORS[4]);
        // Anything else reads as words.
        assert_eq!(Tag::name_for_keyword("travel_plans"), "Travel Plans");
        assert_eq!(Tag::name_for_keyword("$Receipts"), "Receipts");
        assert_eq!(Tag::name_for_keyword("to-do"), "To Do");
        assert_eq!(Tag::name_for_keyword("$"), "$");
        // Colours skip what is taken, then cycle.
        let taken: Vec<String> = TAG_COLORS[..2].iter().map(|c| c.to_string()).collect();
        assert_eq!(Tag::color_for_keyword("Receipts", &taken), TAG_COLORS[2]);
        let all: Vec<String> = TAG_COLORS.iter().map(|c| c.to_string()).collect();
        assert_eq!(Tag::color_for_keyword("Receipts", &all), TAG_COLORS[0]);
        // Bookkeeping keywords are not tags.
        assert!(crate::worker::is_system_keyword("$Forwarded"));
        assert!(crate::worker::is_system_keyword("NonJunk"));
        assert!(crate::worker::is_system_keyword("$MailFlagBit1"));
        assert!(!crate::worker::is_system_keyword("$label2"));
        assert!(!crate::worker::is_system_keyword("Receipts"));
    }

    #[test]
    fn tag_keywords_derive_from_names() {
        assert_eq!(Tag::keyword_for("To Do"), "To_Do");
        assert_eq!(Tag::keyword_for("  Work "), "Work");
        assert_eq!(Tag::keyword_for("Réunion (lundi)"), "Runion_lundi");
        assert!(Tag::valid_keyword("$label1"));
        assert!(!Tag::valid_keyword("has space"));
        assert!(!Tag::valid_keyword(""));
        assert_eq!(tag_css_class("$label1"), "tag-_24label1");
        assert_eq!(tag_css_class("Work"), "tag-work");
    }

    #[test]
    fn filters_match_case_insensitively_per_field() {
        let r = rule(FilterField::FromAddress, FilterMatch::Contains, "NEWS@");
        assert!(r.matches(&headers("news@example.com", "", "", "")));
        assert!(!r.matches(&headers("other@example.com", "News", "News", "News")));

        let r = rule(FilterField::Subject, FilterMatch::StartsWith, "[list]");
        assert!(r.matches(&headers("", "", "[LIST] hello", "")));
        assert!(!r.matches(&headers("", "", "re: [list] hello", "")));

        let r = rule(FilterField::Recipients, FilterMatch::Contains, "team@");
        assert!(r.matches(&headers("", "", "", "me@x.org team@x.org")));

        let r = rule(FilterField::FromName, FilterMatch::Equals, "Bank");
        assert!(r.matches(&headers("", "bank", "", "")));
        assert!(!r.matches(&headers("", "bankster", "", "")));

        // An empty needle can never match (a half-filled rule stays inert).
        let r = rule(FilterField::Subject, FilterMatch::Contains, "");
        assert!(!r.matches(&headers("x", "x", "x", "x")));
        let r = rule(FilterField::Subject, FilterMatch::Contains, " , ,");
        assert!(!r.matches(&headers("x", "x", "x", "x")));
    }

    #[test]
    fn filter_values_hold_comma_separated_alternatives() {
        // #192: "invoice, receipt" matches either; spaces around the commas
        // and empty pieces are ignored; a comma-free value is one piece.
        assert_eq!(FilterCondition::alternatives(" Invoice ,receipt,, "), ["invoice", "receipt"]);
        assert_eq!(FilterCondition::alternatives("plain"), ["plain"]);
        let r = rule(FilterField::Subject, FilterMatch::Contains, "invoice, receipt");
        assert!(r.matches(&headers("", "", "Your RECEIPT", "")));
        assert!(r.matches(&headers("", "", "invoice #12", "")));
        assert!(!r.matches(&headers("", "", "hello", "")));
        // Each alternative gets the whole matcher, not just "contains".
        let r = rule(FilterField::FromAddress, FilterMatch::EndsWith, "@a.org, @b.org");
        assert!(r.matches(&headers("x@B.org", "", "", "")));
        assert!(!r.matches(&headers("x@b.org.evil", "", "", "")));
    }

    #[test]
    fn filter_rules_combine_conditions_all_or_any() {
        // #192: a second condition narrows by default…
        let mut r = rule(FilterField::FromAddress, FilterMatch::EndsWith, "@shop.example");
        r.more.push(cond(FilterField::Subject, FilterMatch::Contains, "order"));
        assert!(r.matches(&headers("a@shop.example", "", "Order shipped", "")));
        assert!(!r.matches(&headers("a@shop.example", "", "Newsletter", "")));
        assert!(!r.matches(&headers("a@other.example", "", "Order shipped", "")));
        // …and widens with "any".
        r.any = true;
        assert!(r.matches(&headers("a@shop.example", "", "Newsletter", "")));
        assert!(r.matches(&headers("a@other.example", "", "Order shipped", "")));
        assert!(!r.matches(&headers("a@other.example", "", "Newsletter", "")));
        // A condition with nothing to match holds nothing up in "any" mode
        // and blocks an "all" rule, as an inert rule should.
        r.more.push(cond(FilterField::Subject, FilterMatch::Contains, ""));
        assert!(r.matches(&headers("a@shop.example", "", "", "")));
        r.any = false;
        assert!(!r.matches(&headers("a@shop.example", "", "Order shipped", "")));

        // The conditions round-trip through the accessors, first one included.
        let conds = r.conditions();
        assert_eq!(conds.len(), 3);
        assert_eq!(conds[0], cond(FilterField::FromAddress, FilterMatch::EndsWith, "@shop.example"));
        assert!(!r.set_conditions(Vec::new()), "a rule needs a condition");
        assert!(r.set_conditions(vec![cond(FilterField::FromName, FilterMatch::Equals, "Bank")]));
        assert_eq!(r.field, FilterField::FromName);
        assert_eq!(r.value, "Bank");
        assert!(r.more.is_empty());
    }

    #[test]
    fn filter_reply_to_falls_back_to_from() {
        // #191: Reply-To when the sender set one, From otherwise.
        let r = rule(FilterField::ReplyTo, FilterMatch::Contains, "sales@");
        let mut input = headers("noreply@shop.example", "", "", "");
        input.reply_to = "Sales@shop.example";
        assert!(r.matches(&input));
        input.reply_to = "";
        assert!(!r.matches(&input));
        input.from_addr = "sales@shop.example";
        assert!(r.matches(&input));
    }

    #[test]
    fn filter_recipient_matchers_look_at_each_recipient() {
        // #201: a rule on the recipients compared the whole To and Cc list
        // as one string, so "is x@y" never matched mail with a second
        // recipient, and "ends with @y" only ever saw the last one.
        let list = "Ann <ann@shop.example>, bob@shop.example, \"Cy, Jr\" <cy@other.example>";
        let input = headers("", "", "", list);
        let r = |m, v| rule(FilterField::Recipients, m, v);
        assert!(r(FilterMatch::Equals, "bob@shop.example").matches(&input));
        assert!(r(FilterMatch::Equals, "ANN@shop.example").matches(&input));
        assert!(r(FilterMatch::Equals, "cy@other.example").matches(&input));
        assert!(!r(FilterMatch::Equals, "shop.example").matches(&input));
        assert!(r(FilterMatch::EndsWith, "@shop.example").matches(&input));
        assert!(!r(FilterMatch::EndsWith, "@nowhere.example").matches(&input));
        assert!(r(FilterMatch::StartsWith, "cy@").matches(&input));
        assert!(r(FilterMatch::Contains, "other").matches(&input));
        // A display name counts on its own too.
        assert!(r(FilterMatch::Equals, "ann").matches(&input));
        // The single bare address most mail carries.
        assert!(r(FilterMatch::Equals, "me@shop.example").matches(&headers("", "", "", "me@shop.example")));
        assert!(!r(FilterMatch::Equals, "me@shop.example").matches(&headers("", "", "", "")));
    }

    #[test]
    fn filter_body_matches_hits_or_text_on_hand() {
        // #191: the server's search result counts, and so does the text the
        // list carries; the matcher is always "contains".
        let r = rule(FilterField::Body, FilterMatch::Equals, "unsubscribe, opt out");
        let hits = vec!["unsubscribe".to_string()];
        let mut input = headers("", "", "", "");
        input.body_hits = &hits;
        assert!(r.matches(&input));
        input.body_hits = &[];
        assert!(!r.matches(&input));
        input.body = "Click here to OPT OUT of these mails";
        assert!(r.matches(&input));
        input.body = "nothing of the sort";
        assert!(!r.matches(&input));
        assert_eq!(r.body_needles(), ["unsubscribe", "opt out"]);
        assert!(rule(FilterField::Subject, FilterMatch::Contains, "x").body_needles().is_empty());
    }

    #[test]
    fn filter_rules_read_the_single_condition_files() {
        // A file from before #192 names one condition per rule and none of
        // the new keys; it must load as a one-condition, all-must-match rule.
        let text = r#"
[[rules]]
account_email = "a@b.c"
field = "subject"
matcher = "contains"
value = "digest"
dest_path = "Lists"
"#;
        let back: FiltersFile = toml::from_str(text).unwrap();
        assert_eq!(back.rules.len(), 1);
        let r = &back.rules[0];
        assert!(r.more.is_empty());
        assert!(!r.any);
        assert_eq!(r.conditions(), vec![cond(FilterField::Subject, FilterMatch::Contains, "digest")]);
        // And a one-condition rule written now carries none of the new keys,
        // so those versions read it back too.
        let text = toml::to_string_pretty(&FiltersFile { rules: back.rules.clone() }).unwrap();
        assert!(!text.contains("more"), "{text}");
        assert!(!text.contains("any"), "{text}");
    }

    #[test]
    fn settings_bundle_roundtrip_parses() {
        // Parse-and-serialize only (no disk, no keyring): the wire format
        // itself must round-trip, secrets must never appear.
        let mut acc = AccountConfig {
            name: "A".into(),
            email: "a@b.c".into(),
            protocol: Default::default(),
            imap_host: "imap.b.c".into(),
            imap_port: 993,
            smtp_host: String::new(),
            smtp_port: 587,
            username: "a@b.c".into(),
            password: "SECRET".into(),
            smtp_separate: false,
            smtp_username: String::new(),
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
            goa_id: None,
            goa_mail_disabled: false,
            goa_enabled_before_mail_disabled: true,
            oauth: false,
            oauth_settings: None,
            oauth_refresh: "TOKEN".into(),
            push: None,
            folder_roles: Default::default(),
            sent_copy_path: None,
            server_saves_sent: false,
            empty_junk_days: 0,
            empty_trash_days: 0,
            pgp_key: None,
        };
        acc.aliases = Vec::new();
        let bundle = SettingsBundle {
            version: 1,
            privacy: PrivacyFile::default(),
            accounts: vec![acc],
            filters: Vec::new(),
            tags: Vec::new(),
            sidebar: None,
            window: None,
            state: None,
            cloud: Some(vec![crate::cloud::CloudAccount::empty()]),
            spell_words: Some(std::collections::BTreeMap::from([(
                "en_US".to_string(),
                vec!["Hylki".to_string()],
            )])),
        };
        let text = toml::to_string_pretty(&bundle).unwrap();
        assert!(!text.contains("SECRET"));
        assert!(!text.contains("TOKEN"));
        let back: SettingsBundle = toml::from_str(&text).unwrap();
        assert_eq!(back.accounts[0].email, "a@b.c");
        assert!(back.accounts[0].password.is_empty());
        // The later sections ride along, and a bundle without them (from
        // before they existed) still parses, leaving them unset.
        assert_eq!(back.cloud.as_ref().map(Vec::len), Some(1));
        assert_eq!(back.spell_words.as_ref().and_then(|w| w.get("en_US")).map(Vec::len), Some(1));
        let old: SettingsBundle = toml::from_str("version = 1\n[privacy]\n").unwrap();
        assert!(old.cloud.is_none() && old.spell_words.is_none());
        // Auto-empty (#140): "never" is left out of the file, an age is kept.
        assert!(!text.contains("empty_junk_days"), "{text}");
        assert_eq!(back.accounts[0].empty_trash_days, 0);
        let mut aged = bundle;
        aged.accounts[0].empty_trash_days = 30;
        let text = toml::to_string_pretty(&aged).unwrap();
        assert!(text.contains("empty_trash_days = 30"), "{text}");
        let back: SettingsBundle = toml::from_str(&text).unwrap();
        assert_eq!(back.accounts[0].empty_trash_days, 30);
        assert_eq!(back.accounts[0].empty_junk_days, 0);
    }

    #[test]
    fn filter_names_are_optional_and_round_trip() {
        // A rule with no name is written without the key, so a version from
        // before #197 reads the file back unchanged.
        let plain = rule(FilterField::Subject, FilterMatch::Contains, "digest");
        assert_eq!(plain.name, "");
        let text = toml::to_string_pretty(&FiltersFile { rules: vec![plain.clone()] }).unwrap();
        assert!(!text.contains("name"), "{text}");
        // A named one keeps its name across a round trip, and an unknown
        // `name` key in a file from a newer version is simply carried.
        let mut named = plain.clone();
        named.name = "Mailing lists".into();
        let text = toml::to_string_pretty(&FiltersFile { rules: vec![named.clone()] }).unwrap();
        let back: FiltersFile = toml::from_str(&text).unwrap();
        assert_eq!(back.rules, vec![named.clone()]);
        // The label falls back to the conditions when there is no name.
        assert_eq!(named.label(), "Mailing lists");
        assert!(plain.label().contains("digest"));
    }

    #[test]
    fn filter_rules_roundtrip_through_toml() {
        let mut multi = rule(FilterField::Body, FilterMatch::Contains, "unsubscribe");
        multi.more.push(cond(FilterField::ReplyTo, FilterMatch::EndsWith, "@list.example"));
        multi.any = true;
        let rules = vec![rule(FilterField::Subject, FilterMatch::EndsWith, "digest"), multi];
        let text = toml::to_string_pretty(&FiltersFile { rules: rules.clone() }).unwrap();
        let back: FiltersFile = toml::from_str(&text).unwrap();
        assert_eq!(back.rules, rules);
    }
}

// ---------------------------------------------------------------------------
// Reader toolbar layout: which buttons the reading pane's header shows, on
// which side, in what order (Settings → Appearance → Toolbar).
// ---------------------------------------------------------------------------

/// One button of the reader header. The left group stays on the bar at every
/// width; the right group folds into the ⋯ overflow menu when the pane is
/// narrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolbarItem {
    Reply,
    ReplyAll,
    Forward,
    Star,
    Archive,
    Delete,
    Spam,
    ReadUnread,
    Tags,
    MoveTo,
    Find,
    Print,
}

impl ToolbarItem {
    pub const ALL: [ToolbarItem; 12] = [
        ToolbarItem::Reply,
        ToolbarItem::ReplyAll,
        ToolbarItem::Forward,
        ToolbarItem::Star,
        ToolbarItem::Archive,
        ToolbarItem::Delete,
        ToolbarItem::Spam,
        ToolbarItem::ReadUnread,
        ToolbarItem::Tags,
        ToolbarItem::MoveTo,
        ToolbarItem::Find,
        ToolbarItem::Print,
    ];

    /// The stable name written to `toolbar.toml`.
    pub fn key(self) -> &'static str {
        match self {
            ToolbarItem::Reply => "reply",
            ToolbarItem::ReplyAll => "reply_all",
            ToolbarItem::Forward => "forward",
            ToolbarItem::Star => "star",
            ToolbarItem::Archive => "archive",
            ToolbarItem::Delete => "delete",
            ToolbarItem::Spam => "spam",
            ToolbarItem::ReadUnread => "read_unread",
            ToolbarItem::Tags => "tags",
            ToolbarItem::MoveTo => "move_to",
            ToolbarItem::Find => "find",
            ToolbarItem::Print => "print",
        }
    }

    pub fn from_key(key: &str) -> Option<ToolbarItem> {
        ToolbarItem::ALL.iter().copied().find(|i| i.key() == key)
    }

    /// The symbolic icon the button (and the settings chip) wears.
    pub fn icon(self) -> &'static str {
        match self {
            ToolbarItem::Reply => "co.hyprlab.Hylki-mail-reply-sender-symbolic",
            ToolbarItem::ReplyAll => "co.hyprlab.Hylki-mail-reply-all-symbolic",
            ToolbarItem::Forward => "co.hyprlab.Hylki-mail-forward-symbolic",
            ToolbarItem::Star => "co.hyprlab.Hylki-non-starred-symbolic",
            ToolbarItem::Archive => "co.hyprlab.Hylki-mail-archive-symbolic",
            ToolbarItem::Delete => "co.hyprlab.Hylki-user-trash-symbolic",
            ToolbarItem::Spam => "co.hyprlab.Hylki-mail-mark-junk-symbolic",
            ToolbarItem::ReadUnread => "co.hyprlab.Hylki-mail-unread-symbolic",
            ToolbarItem::Tags => "co.hyprlab.Hylki-tag-outline-symbolic",
            ToolbarItem::MoveTo => "co.hyprlab.Hylki-folder-symbolic",
            ToolbarItem::Find => "co.hyprlab.Hylki-loupe-with-arrow-symbolic",
            ToolbarItem::Print => "co.hyprlab.Hylki-printer-symbolic",
        }
    }

    /// The untranslated label (callers pass it through `i18n`).
    pub fn label(self) -> &'static str {
        match self {
            ToolbarItem::Reply => crate::i18n::i18n_noop("Reply"),
            ToolbarItem::ReplyAll => crate::i18n::i18n_noop("Reply All"),
            ToolbarItem::Forward => crate::i18n::i18n_noop("Forward"),
            ToolbarItem::Star => crate::i18n::i18n_noop("Flag"),
            ToolbarItem::Archive => crate::i18n::i18n_noop("Archive"),
            ToolbarItem::Delete => crate::i18n::i18n_noop("Delete"),
            ToolbarItem::Spam => crate::i18n::i18n_noop("Spam"),
            ToolbarItem::ReadUnread => crate::i18n::i18n_noop("Read/Unread"),
            ToolbarItem::Tags => crate::i18n::i18n_noop("Tags"),
            ToolbarItem::MoveTo => crate::i18n::i18n_noop("Move To"),
            ToolbarItem::Find => crate::i18n::i18n_noop("Find"),
            ToolbarItem::Print => crate::i18n::i18n_noop("Print"),
        }
    }
}

/// Which side of the reader header a button sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarSide {
    Left,
    Right,
}

/// How many buttons one side of the reader toolbar holds at most.
pub const TOOLBAR_SIDE_MAX: usize = 6;

/// The reader header's layout. A button absent from both lists is hidden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderToolbar {
    pub left: Vec<ToolbarItem>,
    pub right: Vec<ToolbarItem>,
}

impl Default for ReaderToolbar {
    fn default() -> Self {
        ReaderToolbar {
            left: vec![
                ToolbarItem::Reply,
                ToolbarItem::ReplyAll,
                ToolbarItem::Forward,
                ToolbarItem::Star,
                ToolbarItem::Archive,
                ToolbarItem::Delete,
            ],
            right: vec![
                ToolbarItem::Tags,
                ToolbarItem::ReadUnread,
                ToolbarItem::Spam,
                ToolbarItem::MoveTo,
                ToolbarItem::Find,
                ToolbarItem::Print,
            ],
        }
    }
}

impl ReaderToolbar {
    /// The side an item sits on, or None when it is hidden.
    pub fn side(&self, item: ToolbarItem) -> Option<ToolbarSide> {
        if self.left.contains(&item) {
            Some(ToolbarSide::Left)
        } else if self.right.contains(&item) {
            Some(ToolbarSide::Right)
        } else {
            None
        }
    }

    /// The items on neither side, in canonical order.
    pub fn hidden(&self) -> Vec<ToolbarItem> {
        ToolbarItem::ALL
            .iter()
            .copied()
            .filter(|i| self.side(*i).is_none())
            .collect()
    }

    /// Drop repeats (an item may sit on one side only, once), and anything
    /// past a side's room.
    fn sanitize(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.left.retain(|i| seen.insert(*i));
        self.right.retain(|i| seen.insert(*i));
        self.left.truncate(TOOLBAR_SIDE_MAX);
        self.right.truncate(TOOLBAR_SIDE_MAX);
    }

    /// Whether `side` has room for one more button that is not already on
    /// it (`None`, the hidden zone, always has).
    pub fn has_room(&self, side: Option<ToolbarSide>, item: ToolbarItem) -> bool {
        let list = match side {
            Some(ToolbarSide::Left) => &self.left,
            Some(ToolbarSide::Right) => &self.right,
            None => return true,
        };
        list.contains(&item) || list.len() < TOOLBAR_SIDE_MAX
    }

    /// Take `item` out of wherever it is and put it at `index` of `side`
    /// (clamped); `side` None hides it. A full side is left as it is.
    pub fn place(&mut self, item: ToolbarItem, side: Option<ToolbarSide>, index: usize) {
        if !self.has_room(side, item) {
            return;
        }
        self.left.retain(|i| *i != item);
        self.right.retain(|i| *i != item);
        let list = match side {
            Some(ToolbarSide::Left) => &mut self.left,
            Some(ToolbarSide::Right) => &mut self.right,
            None => return,
        };
        let index = index.min(list.len());
        list.insert(index, item);
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ToolbarFile {
    #[serde(default)]
    left: Vec<String>,
    #[serde(default)]
    right: Vec<String>,
}

fn toolbar_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("toolbar.toml"))
}

/// The saved reader toolbar layout, or the default when there is none.
/// Unknown names (a newer Hylki's buttons) are skipped, not fatal.
pub fn load_reader_toolbar() -> ReaderToolbar {
    let Some(text) = toolbar_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return ReaderToolbar::default();
    };
    let Ok(file) = toml::from_str::<ToolbarFile>(&text) else {
        return ReaderToolbar::default();
    };
    let mut layout = ReaderToolbar {
        left: file.left.iter().filter_map(|k| ToolbarItem::from_key(k)).collect(),
        right: file.right.iter().filter_map(|k| ToolbarItem::from_key(k)).collect(),
    };
    layout.sanitize();
    layout
}

pub fn save_reader_toolbar(layout: &ReaderToolbar) {
    let Some(path) = toolbar_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = ToolbarFile {
        left: layout.left.iter().map(|i| i.key().to_string()).collect(),
        right: layout.right.iter().map(|i| i.key().to_string()).collect(),
    };
    if let Ok(toml) = toml::to_string_pretty(&file) {
        let _ = std::fs::write(&path, toml);
    }
}

#[cfg(test)]
mod toolbar_tests {
    use super::*;

    #[test]
    fn place_moves_between_and_within_sides() {
        let mut t = ReaderToolbar::default();
        // The default left group is full: a move onto it changes nothing.
        t.place(ToolbarItem::Print, Some(ToolbarSide::Left), 0);
        assert_eq!(t.left[0], ToolbarItem::Reply);
        assert_eq!(t.right.last(), Some(&ToolbarItem::Print));
        // Make room, then right → left, at the front.
        t.place(ToolbarItem::Star, None, 0);
        t.place(ToolbarItem::Print, Some(ToolbarSide::Left), 0);
        assert_eq!(t.left[0], ToolbarItem::Print);
        assert!(!t.right.contains(&ToolbarItem::Print));
        // Within the left group: Reply (now index 1) to the end.
        t.place(ToolbarItem::Reply, Some(ToolbarSide::Left), 99);
        assert_eq!(t.left.last(), Some(&ToolbarItem::Reply));
        assert_eq!(t.left.len(), TOOLBAR_SIDE_MAX);
        // Hidden: on neither side, listed under hidden().
        t.place(ToolbarItem::Spam, None, 0);
        assert_eq!(t.side(ToolbarItem::Spam), None);
        assert_eq!(t.hidden(), vec![ToolbarItem::Star, ToolbarItem::Spam]);
        // Back from hidden into the right group's middle.
        t.place(ToolbarItem::Spam, Some(ToolbarSide::Right), 1);
        assert_eq!(t.right[1], ToolbarItem::Spam);
        assert_eq!(t.hidden(), vec![ToolbarItem::Star]);
    }

    #[test]
    fn file_round_trip_skips_unknown_names() {
        let file: ToolbarFile = toml::from_str("left = [\"reply\", \"bogus\", \"delete\"]\nright = [\"print\"]\n").unwrap();
        let mut layout = ReaderToolbar {
            left: file.left.iter().filter_map(|k| ToolbarItem::from_key(k)).collect(),
            right: file.right.iter().filter_map(|k| ToolbarItem::from_key(k)).collect(),
        };
        layout.sanitize();
        assert_eq!(layout.left, vec![ToolbarItem::Reply, ToolbarItem::Delete]);
        assert_eq!(layout.right, vec![ToolbarItem::Print]);
        for item in ToolbarItem::ALL {
            assert_eq!(ToolbarItem::from_key(item.key()), Some(item));
        }
    }
}

#[cfg(test)]
mod files_prefs_tests {
    use super::*;

    #[test]
    fn missing_keys_default_to_asking() {
        let file: PrivacyFile = toml::from_str("compose_inline = true\n").unwrap();
        assert_eq!(file.files_action, FilesAction::Ask);
        assert_eq!(file.files_large, FilesLarge::Ask);
        assert_eq!(file.files_limit_mb, 20);
    }

    #[test]
    fn keys_round_trip_by_name() {
        let file: PrivacyFile =
            toml::from_str("files_action = \"reply\"\nfiles_large = \"cloud\"\nfiles_limit_mb = 50\n").unwrap();
        assert_eq!(file.files_action, FilesAction::Reply);
        assert_eq!(file.files_large, FilesLarge::Cloud);
        assert_eq!(file.files_limit_mb, 50);
        let text = toml::to_string(&file).unwrap();
        assert!(text.contains("files_action = \"reply\""), "{text}");
        assert!(text.contains("files_large = \"cloud\""), "{text}");
        let prefs = FilesPrefs { action: FilesAction::New, large: FilesLarge::Attach, limit_mb: 3 };
        assert_eq!(prefs.limit_bytes(), 3_000_000);
    }
}
