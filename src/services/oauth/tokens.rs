//! Talking to the token endpoint: redeeming a code, refreshing an access
//! token, and asking Microsoft Graph who signed in.

use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;

use crate::config::OAuthSettings;
use crate::services::graph_api::{bearer, GRAPH_BASE};

use super::flow::Authorization;

/// The token endpoint and the sign-in's `/me`, with deadlines: a refresh
/// runs ahead of every mail request once the access token expires, so one
/// that waited forever on a dead connection stopped the account's mail with
/// it (#618).
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(30))
            .timeout_write(Duration::from_secs(30))
            .build()
    })
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

/// What a refresh gives back.
pub(crate) struct Refreshed {
    pub(crate) access_token: String,
    /// A replacement for the refresh token that was spent, when the provider
    /// rotates them (Microsoft does, on every refresh). Keeping the old one
    /// works only until it expires, about 90 days on, and then the account
    /// has to sign in again.
    pub(crate) refresh_token: Option<String>,
    /// Seconds the access token is good for, when the provider says.
    pub(crate) expires_in: Option<u64>,
}

/// Result of a completed sign-in (the refresh token to persist).
pub(crate) struct FlowResult {
    pub(crate) refresh_token: String,
}

/// Redeem an authorization code for the refresh token (blocking).
pub(crate) fn exchange_code(
    settings: &OAuthSettings,
    request: &Authorization,
    code: &str,
) -> Result<FlowResult, String> {
    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", &request.redirect),
        ("client_id", &settings.client_id),
        ("code_verifier", &request.verifier),
    ];
    if !settings.client_secret.is_empty() {
        form.push(("client_secret", &settings.client_secret));
    }
    let token: TokenResponse = agent()
        .post(&settings.token_url)
        .send_form(&form)
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())?;

    if token.refresh_token.is_empty() {
        return Err("the provider did not return a refresh token".into());
    }
    Ok(FlowResult {
        refresh_token: token.refresh_token,
    })
}

/// Who signed in to a Microsoft account (blocking): the display name and
/// address Graph reports, and the refresh token to keep, rotated by the
/// refresh this takes. Fills the account in after a sign-in, so it is named
/// by the mailbox actually reached rather than by what was typed.
pub(crate) fn microsoft_whoami(
    settings: &OAuthSettings,
    refresh: &str,
    user: Option<&str>,
) -> Result<(String, String, String), String> {
    let fresh = refresh_access_token_as(settings, refresh, user)?;
    let me: serde_json::Value = agent()
        .get(&format!(
            "{GRAPH_BASE}/me?$select=displayName,mail,userPrincipalName"
        ))
        .set("Authorization", &bearer(&fresh.access_token))
        .call()
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())?;
    let name = me["displayName"].as_str().unwrap_or_default().to_string();
    let address = me["mail"]
        .as_str()
        .filter(|m| !m.is_empty())
        .or_else(|| me["userPrincipalName"].as_str())
        .unwrap_or_default()
        .to_string();
    Ok((
        name,
        address,
        fresh.refresh_token.unwrap_or_else(|| refresh.to_string()),
    ))
}

/// Mint a fresh access token from a stored refresh token (blocking).
pub(crate) fn refresh_access_token(
    settings: &OAuthSettings,
    refresh_token: &str,
) -> Result<Refreshed, String> {
    refresh_access_token_as(settings, refresh_token, None)
}

/// [`refresh_access_token`] for the account `user` signs in as. Behind
/// Conditional Access that wants a managed device, Entra checks every
/// refresh again: the identity broker's device cookie goes with it, as a
/// header of its own name (#329, see `ms_broker`).
pub(crate) fn refresh_access_token_as(
    settings: &OAuthSettings,
    refresh_token: &str,
    user: Option<&str>,
) -> Result<Refreshed, String> {
    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", &settings.client_id),
    ];
    if !settings.client_secret.is_empty() {
        form.push(("client_secret", &settings.client_secret));
    }
    let mut post = agent().post(&settings.token_url);
    if let Some(cookie) = crate::services::ms_broker::sso_cookie(settings, user) {
        post = post.set(&cookie.name, &cookie.value);
    }
    let token: TokenResponse = post
        .send_form(&form)
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())?;
    if token.access_token.is_empty() {
        return Err("no access token in refresh response".into());
    }
    let rotated = Some(token.refresh_token).filter(|t| !t.is_empty() && t != refresh_token);
    Ok(Refreshed {
        access_token: token.access_token,
        refresh_token: rotated,
        expires_in: token.expires_in,
    })
}
