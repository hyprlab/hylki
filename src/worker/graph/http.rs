//! Talking to Microsoft Graph: the access token, requests with throttling
//! honoured, error text, paging, and the request log for the console.

use std::sync::OnceLock;
use std::time::Duration;

use crate::config::AccountConfig;
use crate::i18n::i18n_f;
use crate::services::graph_api::bearer;
use crate::worker::auth::fetch_oauth_token;
use crate::worker::WorkerEvent;

pub(super) use crate::services::graph_api::GRAPH_BASE;

/// How many times a throttled request waits and tries again.
const GRAPH_THROTTLE_TRIES: u32 = 4;

/// How long a request may wait to connect, or for the next bytes of an
/// answer, before it fails.
const GRAPH_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const GRAPH_IO_TIMEOUT: Duration = Duration::from_secs(60);

/// What every request here goes through. ureq's default agent waits on a
/// read forever: a download whose connection died under a suspend left the
/// worker blocked in it, every poll and Refresh queued behind, until the
/// account was switched off and on (#618). A read that hears nothing for a
/// minute fails instead, and the next poll dials again.
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| build_agent(GRAPH_IO_TIMEOUT))
}

fn build_agent(io: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(GRAPH_CONNECT_TIMEOUT)
        .timeout_read(io)
        .timeout_write(io)
        .build()
}

/// Read a ureq error into something a user can act on (status + body snippet).
pub(super) fn graph_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            // Graph errors are JSON with a nested message; surface just that.
            let msg = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                .unwrap_or_else(|| body.chars().take(200).collect());
            format!("Microsoft Graph returned {code}: {msg}")
        }
        other => other.to_string(),
    }
}

/// The Microsoft Graph conversation for the console: method and URL (the
/// token travels in a header and is never logged), then the verdict.
fn graph_wire(method: &str, url: &str) {
    tracing::debug!(target: "hylki::graph", "> {method} {}", url.strip_prefix(GRAPH_BASE).unwrap_or(url));
}

fn graph_wired<T>(method: &str, url: &str, r: &Result<T, String>) {
    let path = url.strip_prefix(GRAPH_BASE).unwrap_or(url);
    match r {
        Ok(_) => tracing::debug!(target: "hylki::graph", "< OK ({method} {path})"),
        Err(e) => tracing::warn!(target: "hylki::graph", "< {e} ({method} {path})"),
    }
}

/// How long Microsoft asks a throttled client to wait: `Retry-After`, in
/// seconds, on a 429 or a 503, within reason. `None` for any other answer.
pub(super) fn graph_throttle_wait(e: &ureq::Error) -> Option<Duration> {
    match e {
        ureq::Error::Status(429 | 503, resp) => {
            let secs = resp
                .header("Retry-After")
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(5);
            Some(Duration::from_secs(secs.min(120)))
        }
        _ => None,
    }
}

/// A GET that respects throttling: a 429 or 503 waits as long as the
/// server asks and tries again, a few times, before it counts as failed.
/// The background index (#429) is what makes this matter: it is the first
/// thing here to send many requests in a row.
fn graph_get(token: &str, url: &str) -> Result<ureq::Response, String> {
    let mut tries = 0;
    loop {
        match agent().get(url).set("Authorization", &bearer(token)).call() {
            Err(e) if tries < GRAPH_THROTTLE_TRIES => {
                let Some(wait) = graph_throttle_wait(&e) else {
                    return Err(graph_err(e));
                };
                tries += 1;
                tracing::info!(target: "hylki::graph", "throttled, waiting {} s", wait.as_secs());
                std::thread::sleep(wait);
            }
            r => return r.map_err(graph_err),
        }
    }
}

pub(super) fn graph_get_json(token: &str, url: &str) -> Result<serde_json::Value, String> {
    graph_wire("GET", url);
    let r = graph_get(token, url).and_then(|resp| resp.into_json().map_err(|e| e.to_string()));
    graph_wired("GET", url, &r);
    r
}

/// [`graph_get_json`], with a 404 as `None`: a message that has gone since
/// it was listed (moved, deleted) is no failure.
pub(super) fn graph_get_json_opt(
    token: &str,
    url: &str,
) -> Result<Option<serde_json::Value>, String> {
    match graph_get_json(token, url) {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.starts_with("Microsoft Graph returned 404") => Ok(None),
        Err(e) => Err(e),
    }
}

pub(super) fn graph_get_bytes(token: &str, url: &str) -> Result<Vec<u8>, String> {
    graph_wire("GET", url);
    let resp = graph_get(token, url);
    graph_wired("GET", url, &resp);
    let resp = resp?;
    let mut out = Vec::new();
    use std::io::Read;
    // Raw MIME can be large; cap well above any sane message (64 MB).
    resp.into_reader()
        .take(64 * 1024 * 1024)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// POST/PATCH a JSON body; an empty 2xx response comes back as `Null`.
pub(super) fn graph_send_json(
    token: &str,
    method: &str,
    url: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    graph_wire(method, url);
    let resp = agent()
        .request(method, url)
        .set("Authorization", &bearer(token))
        .send_json(body.clone())
        .map_err(graph_err);
    graph_wired(method, url, &resp);
    let text = resp?.into_string().map_err(|e| e.to_string())?;
    if text.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
}

/// POST raw MIME (base64, `text/plain` content type — Graph's MIME format) to
/// `sendMail` or a create-message endpoint.
pub(super) fn graph_post_mime(
    token: &str,
    url: &str,
    raw: &[u8],
) -> Result<serde_json::Value, String> {
    let b64 = crate::encoding::base64_encode(raw);
    graph_wire("POST(mime)", url);
    let resp = agent()
        .post(url)
        .set("Authorization", &bearer(token))
        .set("Content-Type", "text/plain")
        .send_string(&b64)
        .map_err(graph_err);
    graph_wired("POST(mime)", url, &resp);
    let text = resp?.into_string().map_err(|e| e.to_string())?;
    if text.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
}

pub(super) fn graph_delete_req(token: &str, url: &str) -> Result<(), String> {
    graph_wire("DELETE", url);
    let r = agent()
        .delete(url)
        .set("Authorization", &bearer(token))
        .call()
        .map_err(graph_err)
        .map(|_| ());
    graph_wired("DELETE", url, &r);
    r
}

/// Follow `@odata.nextLink` pagination, collecting `value` arrays up to `cap`.
pub(super) fn graph_paged(
    token: &str,
    first_url: &str,
    cap: usize,
) -> Result<Vec<serde_json::Value>, String> {
    graph_paged_more(token, first_url, cap).map(|(items, _)| items)
}

/// [`graph_paged`], saying too whether the server had more than `cap`.
pub(super) fn graph_paged_more(
    token: &str,
    first_url: &str,
    cap: usize,
) -> Result<(Vec<serde_json::Value>, bool), String> {
    let mut out = Vec::new();
    let mut url = first_url.to_string();
    loop {
        let page = graph_get_json(token, &url)?;
        if let Some(items) = page["value"].as_array() {
            out.extend(items.iter().cloned());
        }
        let next = page["@odata.nextLink"].as_str();
        if out.len() >= cap {
            let more = out.len() > cap || next.is_some();
            out.truncate(cap);
            return Ok((out, more));
        }
        match next {
            Some(next) => url = next.to_string(),
            None => return Ok((out, false)),
        }
    }
}

/// A fresh GOA token, or a user-actionable error.
pub(super) async fn graph_token(
    account: &AccountConfig,
    emit: &impl Fn(WorkerEvent),
) -> Option<String> {
    // Offline, no token is no fault of the sign-in; the Refresh button
    // already says the network is down (#442).
    if !crate::desktop::power::reaches_internet() {
        return None;
    }
    match fetch_oauth_token(account).await {
        Some(t) => Some(t),
        None => {
            // Signed in through Hylki's own Microsoft app (#329) rather than
            // GNOME Online Accounts: it is Hylki's sign-in to renew.
            let email = [("email", account.email.as_str())];
            emit(WorkerEvent::net_error(if account.goa_id.is_some() {
                i18n_f(
                    "GNOME Online Accounts could not provide a sign-in token for {email}. Open Settings → Online Accounts and sign in again.",
                    &email,
                )
            } else {
                i18n_f(
                    "Microsoft did not accept the saved sign-in for {email}. Open Hylki's Settings → Mail Accounts and sign in to it again.",
                    &email,
                )
            }));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server that takes the request and never answers (#618): the read
    /// fails at its deadline rather than holding the worker for good.
    #[test]
    fn a_silent_server_times_out_instead_of_hanging() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let held = std::thread::spawn(move || {
            let (conn, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_secs(3));
            drop(conn);
        });
        let started = std::time::Instant::now();
        let r = build_agent(Duration::from_millis(300))
            .get(&format!("http://{addr}/"))
            .call();
        assert!(r.is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        held.join().unwrap();
    }
}
