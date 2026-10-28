//! The accounts: accounts.toml, demo-accounts.toml and the account
//! pictures.
//!
//! Account metadata (name, email, servers, username) lives in
//! `~/.config/hylki/accounts.toml`. Passwords are kept in the system keyring
//! (see [`super::secrets`]), never written to disk. The `password` field is
//! read from the TOML if present (older configs, manual setup) and migrated
//! into the keyring on first use, then stripped from the file.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::prefs::{OfflineMail, ReplyCopies};
use super::secrets::{
    keyring_later, store_alias_smtp_password, store_oauth_refresh, store_password,
    store_smtp_password,
};
use super::sections::FolderSort;
use super::{config_file, data_base, default_on, is_false, is_true, is_zero, write_private};

/// Where the accounts' avatar pictures live (#162).
pub(crate) fn avatars_dir() -> Option<PathBuf> {
    data_base().map(|d| d.join("avatars"))
}

/// The picture behind an account's `avatar` name, if the file is there.
pub(crate) fn avatar_path(name: &str) -> Option<PathBuf> {
    let p = avatars_dir()?.join(name);
    p.is_file().then_some(p)
}

/// Drop avatar pictures no account refers to any more. A file younger than
/// ten minutes is left alone: it may be a picture just chosen in an
/// account editor that has not been saved yet.
fn prune_avatars(accounts: &[AccountConfig]) {
    let Some(dir) = avatars_dir() else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let used: std::collections::HashSet<&str> = accounts
        .iter()
        .filter_map(|a| a.avatar.as_deref())
        .collect();
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

/// A protocol this build does not know reads as IMAP: an account written by
/// another version (the EWS accounts of 1.44.0-beta.5) then shows up unable
/// to connect, where it can be removed, instead of failing the whole file and
/// with it every account.
fn known_protocol<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Protocol, D::Error> {
    let name = String::deserialize(d)?;
    Ok(Protocol::deserialize(
        serde::de::value::StrDeserializer::<serde::de::value::Error>::new(&name),
    )
    .unwrap_or_else(|_| {
        tracing::warn!("unknown account protocol {name:?}, read as IMAP");
        Protocol::Imap
    }))
}

/// Incoming-mail protocol for an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Protocol {
    #[default]
    Imap,
    Pop3,
    /// Microsoft Graph (REST) — Microsoft 365 accounts imported from GNOME
    /// Online Accounts, whose token is Graph-scoped and can't speak IMAP
    /// (issue #36). No servers to configure; everything runs over
    /// graph.microsoft.com with the GOA token.
    Graph,
    /// JMAP (RFC 8620/8621) over HTTPS (issue #245): Stalwart, Fastmail. The
    /// server is `imap_host` (a host, or a URL with its scheme) and the
    /// session resource is found at `/.well-known/jmap`; sending goes
    /// through the server too, so the SMTP fields are unused.
    Jmap,
}

/// How an account's servers are secured, as GNOME Online Accounts records
/// it for the accounts it holds (#254). Hylki otherwise infers the TLS mode
/// from the port (993 and 465 open with TLS, the rest upgrade), which a
/// server on a port of its own defeats, and verifies every certificate,
/// which a home server's self-signed one fails even after the user
/// accepted it in GNOME Settings.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub(crate) struct ServerSecurity {
    /// IMAP starts in plain text and upgrades with STARTTLS.
    #[serde(default)]
    pub(crate) imap_starttls: bool,
    /// SMTP starts in plain text and upgrades with STARTTLS.
    #[serde(default)]
    pub(crate) smtp_starttls: bool,
    /// The IMAP server's certificate was accepted in GNOME Settings though
    /// it does not verify.
    #[serde(default)]
    pub(crate) imap_accept_invalid_certs: bool,
    /// The same for the SMTP server.
    #[serde(default)]
    pub(crate) smtp_accept_invalid_certs: bool,
    /// The SMTP server takes mail without signing in.
    #[serde(default)]
    pub(crate) smtp_no_auth: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct AccountConfig {
    pub(crate) name: String,
    pub(crate) email: String,
    /// Incoming-mail protocol (IMAP, POP3, Graph or JMAP).
    #[serde(default, deserialize_with = "known_protocol")]
    pub(crate) protocol: Protocol,
    /// Incoming server host (IMAP, POP3 or JMAP, per `protocol`; a JMAP
    /// account may give a URL with its scheme).
    pub(crate) imap_host: String,
    /// Incoming server port (IMAP, POP3 or JMAP, per `protocol`).
    #[serde(default = "default_imap_port")]
    pub(crate) imap_port: u16,
    /// SMTP server. If empty, derived from `imap_host` (imap.* → smtp.*).
    #[serde(default)]
    pub(crate) smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub(crate) smtp_port: u16,
    pub(crate) username: String,
    /// Read from TOML if present (legacy/manual), but never written back —
    /// passwords belong in the keyring. Usually empty after the first run.
    #[serde(default, skip_serializing)]
    pub(crate) password: String,
    /// Use distinct SMTP credentials instead of the IMAP ones.
    #[serde(default)]
    pub(crate) smtp_separate: bool,
    /// A JMAP account that signs in with an API token, sent as a bearer
    /// token, rather than a username and password: Fastmail's way (#356).
    /// The token is kept where the password would be.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) jmap_token: bool,
    /// A JMAP account set up from the generic JMAP entry rather than
    /// Stalwart's: marked by the JMAP tile, not Stalwart's logo. Off for
    /// the accounts made before the two were apart, all of them Stalwart's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) jmap_generic: bool,
    /// Accept a TLS certificate issued for a different host name than the
    /// one connected to (#246): shared hosting serves mail for many domains
    /// under one certificate in the host's own name. The chain is still
    /// verified; only the name check is waived, for this account's IMAP,
    /// POP3 and SMTP alike. Off unless the connection test says the names
    /// differ.
    #[serde(default)]
    pub(crate) tls_accept_hostname_mismatch: bool,
    /// How the servers are secured, where GNOME Online Accounts says so
    /// (#254); `None` goes by the port numbers, as for every account set
    /// up in Hylki.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) security: Option<ServerSecurity>,
    /// SMTP username (used only when `smtp_separate`).
    #[serde(default)]
    pub(crate) smtp_username: String,
    /// SMTP password — kept in the keyring (separate entry), never on disk.
    #[serde(default, skip_serializing)]
    pub(crate) smtp_password: String,
    /// Sidebar avatar background color ("#rrggbb"). Falls back to the auto accent.
    #[serde(default)]
    pub(crate) color: Option<String>,
    /// Sidebar avatar emoji; when absent, the account-name initials are shown.
    #[serde(default)]
    pub(crate) emoji: Option<String>,
    /// Sidebar avatar picture (#162): the file name of a scaled copy kept
    /// under the data directory's `avatars/`. Shown before the emoji and
    /// the initials when set and the file is present.
    #[serde(default)]
    pub(crate) avatar: Option<String>,
    /// Show this mailbox's own Gravatar (#189), ahead of the picture, the
    /// emoji and the initials — and fall back to them when the address has
    /// none. Off by default: looking it up sends a hash of the address to
    /// Automattic, which is the account owner's own call to make, not a
    /// default (the separate Privacy switch covers other people's mail).
    #[serde(default)]
    pub(crate) gravatar: bool,
    /// The signature put in new messages from this account: the default
    /// one of `signatures`, written here too so an older Hylki still finds
    /// it. A config from before #445 has only this.
    #[serde(default)]
    pub(crate) signature: Option<String>,
    /// Whether `signature` is HTML (vs. plain text).
    #[serde(default)]
    pub(crate) signature_html: bool,
    /// Every signature the account has (#445), in the order the composer
    /// offers them. Read through [`AccountConfig::signature_set`], which
    /// also covers a config that has only `signature`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) signatures: Vec<NamedSignature>,
    /// Which of `signatures` new messages start with. None: they start
    /// without one, and the composer's signature button puts one in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) default_signature: Option<usize>,
    /// How this account is labelled in the UI (e.g. the All Inboxes view).
    /// When unset, the email address is shown.
    #[serde(default)]
    pub(crate) label: Option<String>,
    /// Send-as aliases: extra From identities offered in the composer (#34).
    /// Older configs stored these as plain "Name <address>" strings; both forms
    /// are accepted on load, and saved back as tables.
    #[serde(default, deserialize_with = "deserialize_aliases")]
    pub(crate) aliases: Vec<AliasConfig>,
    /// Whether the account is active. Disabled accounts stay configured but don't
    /// connect, sync, or appear in the sidebar.
    #[serde(default = "default_on")]
    pub(crate) enabled: bool,
    /// When imported from GNOME Online Accounts, the GOA account id (so its
    /// settings/credentials trace back to the system account).
    #[serde(default)]
    pub(crate) goa_id: Option<String>,
    /// Set while this GOA account's Mail service is switched off in GNOME
    /// Settings: the account is paused (`enabled` forced off) rather than
    /// removed, so its local settings survive until Mail comes back on.
    #[serde(default)]
    pub(crate) goa_mail_disabled: bool,
    /// What `enabled` was when the Mail pause began; restored when it ends.
    #[serde(default = "default_on")]
    pub(crate) goa_enabled_before_mail_disabled: bool,
    /// Authenticate with OAuth2 (XOAUTH2) instead of a stored password. The token
    /// comes from GOA (`goa_id`) or, for accounts added directly in Hylki, from
    /// refreshing `oauth_settings` with the keyring-stored refresh token.
    #[serde(default)]
    pub(crate) oauth: bool,
    /// OAuth2 endpoints/client for a natively-added OAuth account (no GOA).
    #[serde(default)]
    pub(crate) oauth_settings: Option<OAuthSettings>,
    /// OAuth2 refresh token — kept in the keyring, never on disk. Transient in
    /// memory (like `password`); stored on save.
    #[serde(default, skip_serializing)]
    pub(crate) oauth_refresh: String,
    /// Per-account IMAP push override (#91): Some(true/false) wins over the
    /// global "Instant new mail" switch; None follows it. Lets an account on
    /// a server that mishandles IDLE opt out without costing push elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) push: Option<bool>,
    /// Manual special-folder assignments (#82), applied over auto-detection:
    /// role → full folder path. Roles: "sent", "drafts", "trash", "junk",
    /// "archive". Empty = fully automatic.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub(crate) folder_roles: std::collections::BTreeMap<String, String>,
    /// Folders kept out of the sidebar and out of every sync (#239), as
    /// full paths; a sub-folder follows its parent. Managed from a folder's
    /// context menu and the account editor's Hidden Folders list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) hidden_folders: Vec<String>,
    /// Whether the first folder listing has been looked over for Exchange's
    /// non-mail folders (#239), which are hidden once, on that look. Kept
    /// so a folder the user brought back is not hidden again.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) folders_seeded: bool,
    /// Where copies of sent mail are filed (#199): a full folder path, or
    /// `None` for the Sent folder. This is only a destination, not a role —
    /// the folder keeps whatever it already is, so the Inbox can be chosen
    /// without the account losing its inbox the way a "Sent" role assignment
    /// would (#136). Ignored by Microsoft 365, which files its own copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sent_copy_path: Option<String>,
    /// The server files its own copy of outgoing mail, so Hylki must not add
    /// one. Gmail does this for anything sent through its SMTP, which leaves
    /// two copies of every message: Google's, and the one Hylki appends.
    /// Off by default, because a server that does *not* do it and a Hylki
    /// that has stopped appending means no sent mail is kept at all.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) server_saves_sent: bool,
    /// Where the copy of a reply is filed (#468): with the rest of sent
    /// mail, or beside the message it answers, as Evolution can. Decided
    /// when the reply goes out, from wherever the original is by then.
    #[serde(default, skip_serializing_if = "ReplyCopies::is_default")]
    pub(crate) reply_copies: ReplyCopies,
    /// Auto-empty (#140): mail in the Junk folder older than this many days
    /// is deleted for good at each sync. 0 = never.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) empty_junk_days: u32,
    /// The same for the Trash folder.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) empty_trash_days: u32,
    /// Mailing-list mail (#407): Inbox messages with a List-Unsubscribe
    /// header, not starred, older than this many days go to the Trash at
    /// each sync. 0 = never.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) clean_lists_days: u32,
    /// How much mail is kept on disk ahead of being opened (#536).
    #[serde(default, skip_serializing_if = "OfflineMail::is_default")]
    pub(crate) offline_mail: OfflineMail,
    /// Whether that mail's attachments are kept too.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) offline_attachments: bool,
    /// The OpenPGP key (fingerprint) that signs mail from this account and
    /// opens what is encrypted to it (#133); `None` = the key whose address
    /// matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pgp_key: Option<String>,
    /// Whether this account's mail is merged into the unified section
    /// (#267): Inboxes, Starred, Sent, Drafts, Archive, Filters and Tags.
    /// Off keeps it to its own section; the tray and new-mail notifications
    /// still count it, as they answer for every account. Written only when
    /// off, so older files read the same.
    #[serde(default = "default_on", skip_serializing_if = "is_true")]
    pub(crate) in_unified: bool,
    /// This account's own folder order in the sidebar; `None` follows
    /// Settings → Sidebar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) folder_sort: Option<FolderSort>,
    /// New messages, replies and forwards from this account open with
    /// OpenPGP signing on (#267); the composer's toggle still turns it off
    /// for one message.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) sign_by_default: bool,
    /// Encrypted mail from this account keeps its subject outside, where
    /// anyone who handles the message can read it, rather than inside with
    /// "..." outside (#493). Off by default, as in Thunderbird. Kept the
    /// negative way round so every account built in code hides it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) plain_subject: bool,
    /// The composer's recipient suggestions also search the organization's
    /// directory through Microsoft Graph (#410). Only for a Microsoft 365
    /// account signed in within Hylki: the scopes it needs are asked for
    /// when it is turned on, and a token from GNOME Online Accounts cannot
    /// be given them.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) graph_directory: bool,
    /// Writing from this account, the suggestions are its directory's and
    /// its own mail's only (#410): a work account then offers no one from
    /// GNOME Contacts, an LDAP directory or another account's mail.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) graph_directory_only: bool,
    /// A message's sensitivity label whose name the message leaves out is
    /// named from the organization's list of labels, read through
    /// Microsoft Graph (#411). Like `graph_directory`, only for an account
    /// signed in within Hylki, with its scope asked for when turned on.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) graph_label_names: bool,
}

/// One of an account's signatures (#445), named so the composer can offer
/// it beside the others.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub(crate) struct NamedSignature {
    pub(crate) name: String,
    /// The signature as HTML.
    pub(crate) html: String,
}

/// A send-as alias (#34): an extra From identity the composer offers. By
/// default the mail still leaves through the account's own SMTP; an alias may
/// instead carry its own SMTP transport (host, credentials), so mail sent as
/// the alias goes out through the alias's provider — the forwarded-mailbox
/// setup where e.g. Gmail would otherwise rewrite the sender.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct AliasConfig {
    /// The From identity: "Name <address>" or a bare address.
    pub(crate) identity: String,
    /// The alias's own SMTP server; empty = send through the account's SMTP.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) smtp_host: String,
    #[serde(
        default = "default_smtp_port",
        skip_serializing_if = "is_default_smtp_port"
    )]
    pub(crate) smtp_port: u16,
    /// SMTP username (used only when `smtp_host` is set).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) smtp_username: String,
    /// SMTP password — kept in the keyring (keyed by account + alias address),
    /// never on disk.
    #[serde(default, skip_serializing)]
    pub(crate) smtp_password: String,
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
    pub(crate) fn address(&self) -> String {
        split_identity(&self.identity).1
    }

    /// Whether mail sent as this alias leaves through the alias's own SMTP
    /// server rather than the account's.
    pub(crate) fn has_own_smtp(&self) -> bool {
        !self.smtp_host.trim().is_empty()
    }
}

fn is_default_smtp_port(port: &u16) -> bool {
    *port == default_smtp_port()
}

/// "Name <addr>" or a bare address → (name, addr).
pub(crate) fn split_identity(s: &str) -> (String, String) {
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
            Entry::Plain(identity) => AliasConfig {
                identity,
                ..AliasConfig::default()
            },
            Entry::Full(alias) => alias,
        })
        .collect())
}

/// OAuth2 client configuration for an account added directly in Hylki.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub(crate) struct OAuthSettings {
    pub(crate) auth_url: String,
    pub(crate) token_url: String,
    pub(crate) client_id: String,
    /// Optional; public/installed clients often have none.
    #[serde(default)]
    pub(crate) client_secret: String,
    pub(crate) scopes: String,
    /// A redirect the client is registered with other than a loopback one,
    /// such as Microsoft's `…/oauth2/nativeclient` (#329). Empty: Hylki's
    /// own loopback listener, with the sign-in in the browser. Otherwise the
    /// sign-in runs in a window of Hylki's, which catches the redirect.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) redirect_uri: String,
}

impl AccountConfig {
    /// The account's signatures and the one new messages start with. A
    /// config written before #445 holds a single `signature`, which is
    /// both.
    pub(crate) fn signature_set(&self) -> (Vec<NamedSignature>, Option<usize>) {
        if self.signatures.is_empty() {
            return match self.signature.as_deref().filter(|s| !s.trim().is_empty()) {
                Some(html) => (
                    vec![NamedSignature {
                        name: crate::i18n::i18n("Signature"),
                        html: html.to_string(),
                    }],
                    Some(0),
                ),
                None => (Vec::new(), None),
            };
        }
        let default = self
            .default_signature
            .filter(|i| *i < self.signatures.len());
        (self.signatures.clone(), default)
    }

    /// Store `signatures`, with `default` the one new messages start with,
    /// and keep `signature` in step for an older Hylki.
    pub(crate) fn set_signatures(
        &mut self,
        signatures: Vec<NamedSignature>,
        default: Option<usize>,
    ) {
        let default = default.filter(|i| *i < signatures.len());
        self.signature = default.map(|i| signatures[i].html.clone());
        self.signature_html = true;
        self.default_signature = default;
        self.signatures = signatures;
    }

    /// The account's UI label: the custom label, or the email address.
    pub(crate) fn display_label(&self) -> String {
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
pub(super) struct ConfigFile {
    #[serde(default)]
    pub(super) accounts: Vec<AccountConfig>,
}

/// A fully-defaulted account for a new-account form to build on (the
/// wizard's, Settings' and the providers'), and for tests.
pub(crate) fn blank_account() -> AccountConfig {
    AccountConfig {
        name: String::new(),
        email: String::new(),
        protocol: Default::default(),
        imap_host: String::new(),
        imap_port: 993,
        smtp_host: String::new(),
        smtp_port: 587,
        username: String::new(),
        password: String::new(),
        smtp_separate: false,
        jmap_token: false,
        jmap_generic: false,
        tls_accept_hostname_mismatch: false,
        security: None,
        smtp_username: String::new(),
        smtp_password: String::new(),
        color: None,
        emoji: None,
        avatar: None,
        gravatar: false,
        signature: None,
        signature_html: false,
        signatures: Vec::new(),
        default_signature: None,
        label: None,
        aliases: Vec::new(),
        enabled: true,
        goa_id: None,
        goa_mail_disabled: false,
        goa_enabled_before_mail_disabled: true,
        oauth: false,
        oauth_settings: None,
        oauth_refresh: String::new(),
        push: None,
        folder_roles: Default::default(),
        hidden_folders: Vec::new(),
        folders_seeded: false,
        sent_copy_path: None,
        server_saves_sent: false,
        reply_copies: Default::default(),
        empty_junk_days: 0,
        empty_trash_days: 0,
        clean_lists_days: 0,
        offline_mail: Default::default(),
        offline_attachments: false,
        pgp_key: None,
        in_unified: true,
        folder_sort: None,
        sign_by_default: false,
        plain_subject: false,
        graph_directory: false,
        graph_directory_only: false,
        graph_label_names: false,
    }
}

/// Demo mode's edited stand-in accounts (`~/.config/hylki/demo-accounts.toml`):
/// what the Accounts panel changed on the sample accounts (color, emoji,
/// picture, label), kept apart from the real accounts file so the demo
/// stays a demo. `None` when there is none, or it is empty.
pub(crate) fn load_demo_accounts() -> Option<Vec<AccountConfig>> {
    let path = demo_accounts_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let cfg = toml::from_str::<ConfigFile>(&text).ok()?;
    (!cfg.accounts.is_empty()).then_some(cfg.accounts)
}

pub(crate) fn save_demo_accounts(accounts: &[AccountConfig]) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    let path = demo_accounts_path()
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "no config directory"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = ConfigFile {
        accounts: accounts.to_vec(),
    };
    let toml = toml::to_string_pretty(&file).map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
    write_private(&path, &toml)
}

fn demo_accounts_path() -> Option<PathBuf> {
    config_file("demo-accounts.toml")
}

/// Path to the accounts config file (`~/.config/hylki/accounts.toml`).
pub(crate) fn path() -> Option<PathBuf> {
    config_file("accounts.toml")
}

/// Returns the configured accounts, or `None` if there is no usable config
/// (missing file, parse error, or empty list) — in which case the app falls
/// back to the offline sample backend.
pub(crate) fn load() -> Option<Vec<AccountConfig>> {
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
                    tracing::info!(
                        "{}: empty-host GOA OAuth account — using Microsoft Graph",
                        a.email
                    );
                    a.protocol = Protocol::Graph;
                }
            }
            tracing::info!(
                "loaded {} account(s) from {}",
                cfg.accounts.len(),
                path.display()
            );
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
///
/// The file is written at once; the passwords are queued for the keyring
/// thread (see [`keyring_later`]), since this is called from the main thread
/// and a keyring can take as long as its user does to answer. A keyring read
/// queued after this call sees them.
pub(crate) fn save(accounts: &[AccountConfig]) -> std::io::Result<()> {
    write_config(accounts)?;
    prune_avatars(accounts);
    let accounts = accounts.to_vec();
    keyring_later(move || store_secrets(&accounts));
    Ok(())
}

fn store_secrets(accounts: &[AccountConfig]) {
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
}

/// Rewrite the config file from the current accounts (dropping any plaintext
/// passwords still on disk). Used after migrating a legacy password.
pub(crate) fn strip_passwords_on_disk() {
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
    let toml = toml::to_string_pretty(&file).map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
    write_private(&path, &toml)?;

    tracing::info!("saved {} account(s) to {}", accounts.len(), path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_account_has_the_form_defaults() {
        let a = blank_account();
        assert_eq!((a.imap_port, a.smtp_port), (993, 587));
        assert_eq!(a.protocol, crate::config::Protocol::Imap);
        assert!(a.enabled && a.in_unified && a.goa_enabled_before_mail_disabled);
        assert!(!a.smtp_separate && !a.oauth && !a.gravatar && !a.server_saves_sent);
        assert!(!a.jmap_token && !a.jmap_generic && !a.tls_accept_hostname_mismatch);
        assert!(!a.sign_by_default && !a.plain_subject && !a.folders_seeded);
        assert!(!a.graph_directory && !a.graph_directory_only && !a.graph_label_names);
        assert!(!a.goa_mail_disabled && !a.signature_html);
        assert!(a.name.is_empty() && a.email.is_empty() && a.username.is_empty());
        assert!(a.imap_host.is_empty() && a.smtp_host.is_empty());
        assert!(a.password.is_empty() && a.smtp_password.is_empty());
        assert!(a.smtp_username.is_empty() && a.oauth_refresh.is_empty());
        assert!(a.goa_id.is_none() && a.push.is_none() && a.security.is_none());
        assert!(a.color.is_none() && a.emoji.is_none() && a.avatar.is_none());
        assert!(a.signature.is_none() && a.default_signature.is_none());
        assert!(a.label.is_none() && a.oauth_settings.is_none() && a.pgp_key.is_none());
        assert!(a.sent_copy_path.is_none() && a.folder_sort.is_none());
        assert!(a.signatures.is_empty() && a.aliases.is_empty() && a.hidden_folders.is_empty());
        assert_eq!(
            (a.empty_junk_days, a.empty_trash_days, a.clean_lists_days),
            (0, 0, 0)
        );
    }

    #[test]
    fn identities_split_into_name_and_address() {
        use super::split_identity;
        assert_eq!(
            split_identity("Ann Work <ann@work.example>"),
            ("Ann Work".into(), "ann@work.example".into())
        );
        assert_eq!(
            split_identity("ann@shop.example"),
            (String::new(), "ann@shop.example".into())
        );
        assert_eq!(
            split_identity("\"Quoted\" <q@x.example>"),
            ("Quoted".into(), "q@x.example".into())
        );
    }
}
