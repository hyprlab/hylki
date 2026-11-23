//! Microsoft 365 shared mailboxes (#701): the check made when one is added.
//!
//! Graph cannot list the shared mailboxes a user has been given; Outlook
//! learns of them through Exchange's own discovery. So the address is typed,
//! and before it becomes an account the mailbox is opened once with the
//! owner's token, so a mailbox the user cannot open says so at once instead
//! of becoming an account that never loads.

use std::sync::OnceLock;
use std::time::Duration;

use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};
use crate::services::graph_api::{bearer, GRAPH_BASE};

const TIMEOUT: Duration = Duration::from_secs(30);

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| ureq::AgentBuilder::new().timeout(TIMEOUT).build())
}

/// Whether `address` can stand in a Graph path as a mailbox: one `@` with
/// something on each side, and nothing a URL would read as its own
/// (a slash, a query, a fragment, a space or an escape).
pub(crate) fn valid_address(address: &str) -> bool {
    let address = address.trim();
    let Some((local, domain)) = address.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && !address
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "/?#%\\<>\"".contains(c))
}

/// The shared mailbox at `address`, opened with `owner`'s sign-in: a new
/// account named as the organization names the mailbox. `Err` is a sentence
/// for the user.
pub(crate) fn check(owner: &AccountConfig, address: &str) -> Result<AccountConfig, String> {
    let address = address.trim();
    let token = crate::worker::oauth_token_blocking(owner).ok_or_else(|| {
        i18n_f(
            "GNOME Online Accounts could not provide a sign-in token for {email}. Open Settings → Online Accounts and sign in again.",
            &[("email", owner.email.as_str())],
        )
    })?;
    let root = format!("{GRAPH_BASE}/users/{address}");
    get(&token, &format!("{root}/mailFolders/inbox?$select=id")).map_err(|code| {
        let names = [("address", address), ("email", owner.email.as_str())];
        match code {
            Some(404) => i18n_f("Microsoft 365 has no mailbox at {address}.", &names),
            Some(401 | 403) => i18n_f(
                "{email} has no access to {address}. Your organization gives access to a shared mailbox in Exchange.",
                &names,
            ),
            _ => i18n("Microsoft 365 could not be reached. Try again later."),
        }
    })?;
    // The mailbox's name, as the organization's directory has it; the
    // address stands in when the directory does not say.
    let name = get(&token, &format!("{root}?$select=displayName"))
        .ok()
        .and_then(|v| v["displayName"].as_str().map(str::to_string))
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| address.to_string());
    Ok(account(owner, address, name))
}

/// The account a shared mailbox becomes: Microsoft 365 over Graph with the
/// owner's sign-in, and nothing else of the owner's (its signature, color
/// and folder settings are the owner's own).
pub(crate) fn account(owner: &AccountConfig, address: &str, name: String) -> AccountConfig {
    AccountConfig {
        name,
        email: address.to_string(),
        protocol: crate::config::Protocol::Graph,
        imap_host: owner.imap_host.clone(),
        imap_port: owner.imap_port,
        smtp_host: owner.smtp_host.clone(),
        smtp_port: owner.smtp_port,
        username: owner.username.clone(),
        smtp_separate: owner.smtp_separate,
        smtp_username: owner.smtp_username.clone(),
        security: owner.security.clone(),
        goa_id: owner.goa_id.clone(),
        oauth: true,
        shared_mailbox: true,
        ..crate::config::blank_account()
    }
}

/// A GET whose failure is only its status, when there is one.
fn get(token: &str, url: &str) -> Result<serde_json::Value, Option<u16>> {
    tracing::debug!(target: "hylki::graph", "> GET {}", url.strip_prefix(GRAPH_BASE).unwrap_or(url));
    match agent().get(url).set("Authorization", &bearer(token)).call() {
        Ok(resp) => resp.into_json().map_err(|_| None),
        Err(ureq::Error::Status(code, _)) => {
            tracing::warn!(target: "hylki::graph", "< HTTP {code} (shared mailbox check)");
            Err(Some(code))
        }
        Err(e) => {
            tracing::warn!(target: "hylki::graph", "< {e} (shared mailbox check)");
            Err(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_plain_address_is_taken() {
        assert!(valid_address("team@example.com"));
        assert!(valid_address("  sales.eu@contoso.co.uk "));
        for bad in [
            "",
            "team",
            "@example.com",
            "team@",
            "team@example",
            "team@.com",
            "a@b@example.com",
            "team@example.com/mailFolders",
            "team@example.com?x=1",
            "te am@example.com",
            "team%40example.com",
        ] {
            assert!(!valid_address(bad), "{bad}");
        }
    }

    /// The new account opens the mailbox with the owner's GNOME Online
    /// Account, and takes nothing of the owner's own.
    #[test]
    fn the_account_borrows_only_the_sign_in() {
        let owner = AccountConfig {
            name: "Ann Lee".into(),
            email: "ann@example.com".into(),
            username: "ann@example.com".into(),
            protocol: crate::config::Protocol::Graph,
            goa_id: Some("account_1".into()),
            oauth: true,
            color: Some("#3584e4".into()),
            signature: Some("Ann".into()),
            ..crate::config::blank_account()
        };
        let shared = account(&owner, "team@example.com", "Team".into());
        assert_eq!(shared.email, "team@example.com");
        assert_eq!(shared.name, "Team");
        assert_eq!(shared.goa_id.as_deref(), Some("account_1"));
        assert!(shared.shared_mailbox && shared.oauth);
        assert_eq!(shared.protocol, crate::config::Protocol::Graph);
        assert!(shared.color.is_none() && shared.signature.is_none());
    }
}
