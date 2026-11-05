//! Export and import of every setting as one TOML bundle (#50).

use std::path::PathBuf;

use super::accounts::{load, save, AccountConfig};
use super::filters::{load_filters, save_filters, FilterRule};
use super::privacy::{load_privacy, privacy_path, PrivacyFile};
use super::state::{state_path, StateFile};
use super::tags::{load_tags, save_tags, Tag};
use super::window::{sidebar_path, window_path, SidebarFile, WindowFile};
use super::{read_toml, write_private};

/// A portable settings bundle (#50): every configuration file Hylki keeps —
/// preferences, accounts (colors, emoji, labels, aliases, folder roles and
/// per-account push included), filters, tags, cloud storage accounts,
/// sidebar layout, window/pane state (the app icon choice with it), and
/// the words taught to the spell checker. Passwords and tokens never
/// appear — their fields are skip_serializing, and they live in the
/// keyring, not on disk; OpenPGP keys live in GnuPG's keyring, and the
/// optional oauth.toml (a client id and secret the user supplied by hand)
/// stays out for the same reason. An account's own OAuth client secret is
/// left out as well (see [`strip_client_secrets`]).
///
/// Sections added after the first release are optional, so a bundle from
/// before them imports without wiping what it does not mention.
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct SettingsBundle {
    pub(super) version: u32,
    pub(super) privacy: PrivacyFile,
    #[serde(default)]
    pub(super) accounts: Vec<AccountConfig>,
    #[serde(default)]
    pub(super) filters: Vec<FilterRule>,
    #[serde(default)]
    pub(super) tags: Vec<Tag>,
    #[serde(default)]
    pub(super) sidebar: Option<SidebarFile>,
    #[serde(default)]
    pub(super) window: Option<WindowFile>,
    #[serde(default)]
    pub(super) state: Option<StateFile>,
    /// Cloud storage accounts (#144 onward); their sign-ins stay in the
    /// keyring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cloud: Option<Vec<crate::services::cloud::CloudAccount>>,
    /// The spell checker's personal words, per language (#114).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) spell_words: Option<std::collections::BTreeMap<String, Vec<String>>>,
}

/// Serialize a bundle section back to its config file.
fn write_file_struct<T: serde::Serialize>(
    path: Option<PathBuf>,
    value: &Option<T>,
) -> Result<(), String> {
    let (Some(path), Some(value)) = (path, value) else {
        return Ok(());
    };
    let toml = toml::to_string_pretty(value).map_err(|e| e.to_string())?;
    write_private(&path, &toml).map_err(|e| e.to_string())
}

/// The current configuration as a TOML bundle for Export Settings.
pub(crate) fn export_bundle() -> Result<String, String> {
    let mut accounts = load().unwrap_or_default();
    strip_client_secrets(&mut accounts);
    let bundle = SettingsBundle {
        version: 1,
        privacy: load_privacy(),
        accounts,
        filters: load_filters(),
        tags: load_tags(),
        sidebar: read_toml(sidebar_path()),
        window: read_toml(window_path()),
        state: read_toml(state_path()),
        cloud: Some(crate::services::cloud::load_accounts()),
        spell_words: Some(crate::desktop::spell::all_personal_words()),
    };
    toml::to_string_pretty(&bundle).map_err(|e| e.to_string())
}

/// A bundle is a backup that gets mailed, synced and left in Downloads, and
/// an OAuth client secret in it lets whoever finds it pose as the app the
/// user registered, so it stays on this computer like the tokens do.
pub(super) fn strip_client_secrets(accounts: &mut [AccountConfig]) {
    for oauth in accounts
        .iter_mut()
        .filter_map(|a| a.oauth_settings.as_mut())
    {
        oauth.client_secret.clear();
    }
}

/// An imported account with no client secret keeps the one this computer
/// already has for the same address and client, so restoring a bundle over
/// a working setup does not break its sign-in.
pub(super) fn keep_client_secrets(imported: &mut [AccountConfig], local: &[AccountConfig]) {
    for account in imported {
        let Some(oauth) = account.oauth_settings.as_mut() else {
            continue;
        };
        if !oauth.client_secret.is_empty() {
            continue;
        }
        let known = local
            .iter()
            .filter(|l| l.email.eq_ignore_ascii_case(&account.email))
            .filter_map(|l| l.oauth_settings.as_ref())
            .find(|l| l.client_id == oauth.client_id && l.token_url == oauth.token_url);
        if let Some(known) = known {
            oauth.client_secret = known.client_secret.clone();
        }
    }
}

/// Parse and persist an exported bundle: privacy.toml and accounts.toml are
/// replaced (via the same writers the app uses; the keyring is untouched
/// since imported accounts carry no secrets). Returns the account count.
pub(crate) fn import_bundle(text: &str) -> Result<usize, String> {
    let mut bundle: SettingsBundle = toml::from_str(text).map_err(|e| e.to_string())?;
    if bundle.version != 1 {
        return Err(format!("unsupported bundle version {}", bundle.version));
    }
    keep_client_secrets(&mut bundle.accounts, &load().unwrap_or_default());
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
        crate::services::cloud::save_accounts(cloud);
    }
    // Words are merged, never removed: a backup can only teach the
    // checker more.
    if let Some(words) = &bundle.spell_words {
        for (lang, list) in words {
            crate::desktop::spell::merge_personal_words(lang, list);
        }
    }
    Ok(bundle.accounts.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::accounts::{blank_account, OAuthSettings};

    fn oauth_account(email: &str, client_id: &str, secret: &str) -> AccountConfig {
        let mut a = blank_account();
        a.email = email.into();
        a.oauth = true;
        a.oauth_settings = Some(OAuthSettings {
            token_url: "https://token.example.org".into(),
            client_id: client_id.into(),
            client_secret: secret.into(),
            ..Default::default()
        });
        a
    }

    #[test]
    fn a_bundle_carries_no_client_secret_and_an_import_keeps_the_local_one() {
        let mut exported = vec![
            oauth_account("ann@example.org", "app", "s3cret"),
            blank_account(),
        ];
        strip_client_secrets(&mut exported);
        let text = toml::to_string_pretty(&exported[0]).unwrap();
        assert!(!text.contains("s3cret"), "{text}");

        let local = vec![
            oauth_account("ANN@example.org", "app", "s3cret"),
            oauth_account("bob@example.org", "other", "bobs"),
        ];
        let mut imported = vec![
            oauth_account("ann@example.org", "app", ""),
            // Another client: its secret is not this one's.
            oauth_account("bob@example.org", "new-app", ""),
            // A bundle from before the rule keeps what it carries.
            oauth_account("cy@example.org", "app", "own"),
        ];
        keep_client_secrets(&mut imported, &local);
        let secret = |a: &AccountConfig| a.oauth_settings.as_ref().unwrap().client_secret.clone();
        assert_eq!(secret(&imported[0]), "s3cret");
        assert_eq!(secret(&imported[1]), "");
        assert_eq!(secret(&imported[2]), "own");
    }
}
