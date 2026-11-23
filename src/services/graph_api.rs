//! Microsoft Graph: what every client of it sends the same way, the
//! address and the bearer token. The clients (the mail backend in
//! `worker::graph`, OneDrive in `cloud::onedrive`, `graph_labels`,
//! `graph_directory` and the sign-in's `oauth::microsoft_whoami`) keep their
//! own agents, timeouts, throttling and error wording, which differ on
//! purpose: each says what went wrong in its own setting.

/// The v1.0 endpoint. The sensitivity labels are read from the beta one
/// (`graph_labels`), which no other client uses.
pub(crate) const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// Where an account's mail is in Graph, the prefix of every mail request:
/// the signed-in user's own (`/me`), or a shared mailbox opened with that
/// user's sign-in (#701), by its address. The address was checked for
/// characters a path cannot hold when the mailbox was added.
pub(crate) fn mailbox_root(account: &crate::config::AccountConfig) -> String {
    if account.shared_mailbox {
        format!("{GRAPH_BASE}/users/{}", account.email.trim())
    } else {
        format!("{GRAPH_BASE}/me")
    }
}

/// The `Authorization` header's value for an access token.
pub(crate) fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The address and header the five clients each spelt out before.
    #[test]
    fn every_client_asks_the_same_endpoint_with_a_bearer_token() {
        assert_eq!(GRAPH_BASE, "https://graph.microsoft.com/v1.0");
        assert_eq!(bearer("eyJ0.abc"), "Bearer eyJ0.abc");
        assert_eq!(
            format!("{GRAPH_BASE}/me?$select=displayName,mail,userPrincipalName"),
            "https://graph.microsoft.com/v1.0/me?$select=displayName,mail,userPrincipalName"
        );
    }

    /// A shared mailbox (#701) is reached under its own address, with the
    /// owner's token; every other account's mail is its own.
    #[test]
    fn a_shared_mailbox_is_asked_for_by_its_address() {
        let mut account = crate::config::AccountConfig {
            email: "team@example.com".into(),
            ..crate::config::blank_account()
        };
        assert_eq!(
            mailbox_root(&account),
            "https://graph.microsoft.com/v1.0/me"
        );
        account.shared_mailbox = true;
        assert_eq!(
            mailbox_root(&account),
            "https://graph.microsoft.com/v1.0/users/team@example.com"
        );
    }
}
