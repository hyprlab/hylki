//! Talking JMAP: the session resource, method calls batched in one POST,
//! blob upload and download, error text, and the request log for the
//! console.

use std::time::Duration;

use crate::config::AccountConfig;
use crate::i18n::i18n_f;
use crate::models::MailboxQuota;
use crate::worker::auth::sign_in_password_blocking;
use crate::worker::jmap::index::JmapState;
use crate::worker::jmap::ops::crlf_line_endings;
use crate::worker::{blocking, WorkerEvent};

pub(super) const CAP_CORE: &str = "urn:ietf:params:jmap:core";

pub(super) const CAP_MAIL: &str = "urn:ietf:params:jmap:mail";

pub(super) const CAP_SUBMISSION: &str = "urn:ietf:params:jmap:submission";

/// RFC 9425: how much room the account has (#298).
pub(super) const CAP_QUOTA: &str = "urn:ietf:params:jmap:quota";

/// What the session resource told us, plus the credentials to keep using.
#[derive(Clone, Debug)]
pub(super) struct JmapSession {
    pub(super) api_url: String,
    pub(super) download_url: String,
    pub(super) upload_url: String,
    pub(super) event_source_url: Option<String>,
    /// The JMAP account id (the primary mail account of the sign-in).
    pub(super) account: String,
    /// The `Authorization` header value.
    pub(super) auth: String,
    /// Whether the server answers `Quota/get` (RFC 9425).
    pub(super) quota: bool,
    /// Whether a mailbox can be made at the top (RFC 8621 §1.3.1's
    /// `mayCreateTopLevelMailbox`), and how deep they may go
    /// (`maxMailboxDepth`, `None` for no limit) (#554).
    pub(super) top_level: bool,
    pub(super) max_depth: Option<usize>,
}

/// The server's base URL from the account's incoming-server field: a bare
/// host gets `https://` (and the port when it is not 443); a URL with its
/// scheme is used as typed, so a server on plain HTTP on a private network
/// can be reached.
pub(super) fn jmap_base_url(account: &AccountConfig) -> String {
    let host = account.imap_host.trim().trim_end_matches('/');
    if host.starts_with("http://") || host.starts_with("https://") {
        return host.to_string();
    }
    match account.imap_port {
        0 | 443 => format!("https://{host}"),
        port => format!("https://{host}:{port}"),
    }
}

/// `scheme://host[:port]` of a URL (the whole string when it has no path).
pub(super) fn origin_of(url: &str) -> &str {
    match url.find("://") {
        Some(i) => match url[i + 3..].find('/') {
            Some(j) => &url[..i + 3 + j],
            None => url,
        },
        None => url,
    }
}

pub(super) fn jmap_auth(account: &AccountConfig) -> String {
    // Fastmail takes an API token, never the account password (#356).
    if account.jmap_token {
        return format!("Bearer {}", account.password.trim());
    }
    let user = if account.username.is_empty() {
        &account.email
    } else {
        &account.username
    };
    let b64 = crate::encoding::base64_encode(format!("{user}:{}", account.password).as_bytes());
    format!("Basic {b64}")
}

/// Read a ureq error into something a user can act on.
pub(super) fn jmap_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(401, _) => {
            "the server refused the sign-in (401): check the username and password, or the API token".to_string()
        }
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            // JMAP problem details (RFC 7807) carry a `detail`; surface that.
            let msg = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| {
                    v["detail"].as_str().or(v["title"].as_str()).map(str::to_string)
                })
                .unwrap_or_else(|| body.chars().take(200).collect());
            format!("the server returned {code}: {msg}")
        }
        other => other.to_string(),
    }
}

/// The JMAP conversation for the console: method names in, verdict out (the
/// credentials travel in a header and are never logged).
pub(super) fn jmap_wire(what: &str) {
    tracing::debug!(target: "hylki::jmap", "> {what}");
}

pub(super) fn jmap_wired<T>(what: &str, r: &Result<T, String>) {
    match r {
        Ok(_) => tracing::debug!(target: "hylki::jmap", "< OK ({what})"),
        Err(e) => tracing::warn!(target: "hylki::jmap", "< {e} ({what})"),
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .build()
}

/// Fetch the session resource: where the API lives and which account is
/// the mailbox.
pub(super) fn jmap_connect(account: &AccountConfig) -> Result<JmapSession, String> {
    let base = jmap_base_url(account);
    let origin = origin_of(&base).to_string();
    // A password the worker could not read at start is read again (#375).
    let filled;
    let account = if account.password.is_empty() {
        filled = AccountConfig {
            password: sign_in_password_blocking(account, false)?,
            ..account.clone()
        };
        &filled
    } else {
        account
    };
    let auth = jmap_auth(account);
    // A server given with a path may be the session resource itself
    // (Fastmail's is https://api.fastmail.com/jmap/session, #356); when it
    // is not, the well-known path under it is tried, as for a bare host.
    let well_known = format!("{base}/.well-known/jmap");
    let v = if base.len() > origin.len() {
        match fetch_session(&base, &origin, &auth) {
            Ok(v) if v["apiUrl"].is_string() => v,
            Err(e) if e.contains("(401)") => return Err(e),
            _ => fetch_session(&well_known, &origin, &auth)?,
        }
    } else {
        fetch_session(&well_known, &origin, &auth)?
    };
    // A server names itself in the session: absolute URLs on the host it
    // was set up with. An account whose server is given as a URL, scheme
    // and all, is pointed at that origin on purpose (a server on a private
    // network, or behind a tunnel, reached other than by its public name),
    // so the typed origin stands in for the one the server advertises for
    // itself. Only that one: a URL on another host (Fastmail serves blobs
    // from a separate one) is left alone.
    let typed_origin = account
        .imap_host
        .trim()
        .starts_with("http")
        .then(|| origin.clone());
    let advertised = v["apiUrl"]
        .as_str()
        .map(|u| origin_of(u).to_string())
        .unwrap_or_default();
    let absolute = |u: &str| -> String {
        if !(u.starts_with("http://") || u.starts_with("https://")) {
            return format!("{origin}/{}", u.trim_start_matches('/'));
        }
        match &typed_origin {
            Some(mine) if !advertised.is_empty() && origin_of(u) == advertised => {
                format!("{mine}{}", &u[advertised.len()..])
            }
            _ => u.to_string(),
        }
    };
    let api_url = v["apiUrl"]
        .as_str()
        .map(absolute)
        .ok_or("the session resource has no apiUrl")?;
    let download_url = v["downloadUrl"]
        .as_str()
        .map(absolute)
        .ok_or("the session resource has no downloadUrl")?;
    let upload_url = v["uploadUrl"]
        .as_str()
        .map(absolute)
        .ok_or("the session resource has no uploadUrl")?;
    let event_source_url = v["eventSourceUrl"].as_str().map(absolute);
    // The mail account: the primary one for mail, else the first personal
    // account that has the mail capability.
    let account_id = v["primaryAccounts"][CAP_MAIL]
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            v["accounts"].as_object().and_then(|accts| {
                accts
                    .iter()
                    .find(|(_, a)| a["accountCapabilities"][CAP_MAIL].is_object())
                    .map(|(id, _)| id.clone())
            })
        })
        .ok_or("the sign-in has no mail account on this server")?;
    if !v["capabilities"][CAP_MAIL].is_object() {
        return Err("the server does not offer JMAP for mail".into());
    }
    let quota = v["capabilities"][CAP_QUOTA].is_object();
    let mail = &v["accounts"][&account_id]["accountCapabilities"][CAP_MAIL];
    let top_level = mail["mayCreateTopLevelMailbox"].as_bool().unwrap_or(true);
    let max_depth = mail["maxMailboxDepth"]
        .as_u64()
        .map(|d| d as usize)
        .filter(|d| *d > 0);
    Ok(JmapSession {
        api_url,
        download_url,
        upload_url,
        event_source_url,
        account: account_id,
        auth,
        quota,
        top_level,
        max_depth,
    })
}

/// GET the session resource at `url`. A well-known path answers with a
/// redirect to it (a 307 on Stalwart), followed here by hand, credentials
/// included: ureq drops the Authorization header when it follows one
/// itself, and the session then comes back for nobody.
fn fetch_session(url: &str, origin: &str, auth: &str) -> Result<serde_json::Value, String> {
    let no_follow = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .redirects(0)
        .build();
    let mut url = url.to_string();
    let mut hops = 0;
    loop {
        jmap_wire("GET session");
        let resp = no_follow
            .get(&url)
            .set("Authorization", auth)
            .set("Accept", "application/json")
            .call()
            .map_err(jmap_err);
        jmap_wired("GET session", &resp);
        let resp = resp?;
        if (300..400).contains(&resp.status()) {
            let Some(next) = resp.header("Location").map(str::to_string) else {
                return Err(format!(
                    "the server redirected ({}) without saying where",
                    resp.status()
                ));
            };
            hops += 1;
            if hops > 5 {
                return Err("the server keeps redirecting the session request".into());
            }
            url = if next.starts_with("http://") || next.starts_with("https://") {
                next
            } else {
                format!("{origin}/{}", next.trim_start_matches('/'))
            };
            continue;
        }
        return resp.into_json().map_err(|e| e.to_string());
    }
}

/// One API request: the method calls, in order, with their responses back
/// in the same order. A method-level error comes back as an `Err` naming
/// the call, since nothing after it would make sense.
pub(super) fn jmap_call(
    s: &JmapSession,
    using: &[&str],
    calls: Vec<serde_json::Value>,
) -> Result<Vec<serde_json::Value>, String> {
    let names: Vec<String> = calls
        .iter()
        .filter_map(|c| c[0].as_str().map(str::to_string))
        .collect();
    let what = names.join(" + ");
    jmap_wire(&what);
    let body = serde_json::json!({ "using": using, "methodCalls": calls });
    let r: Result<serde_json::Value, String> = agent()
        .post(&s.api_url)
        .set("Authorization", &s.auth)
        .set("Accept", "application/json")
        .send_json(body)
        .map_err(jmap_err)
        .and_then(|resp| resp.into_json().map_err(|e| e.to_string()));
    jmap_wired(&what, &r);
    let v = r?;
    let responses = v["methodResponses"]
        .as_array()
        .cloned()
        .ok_or_else(|| "the server's reply has no methodResponses".to_string())?;
    for resp in &responses {
        if resp[0].as_str() == Some("error") {
            let kind = resp[1]["type"].as_str().unwrap_or("unknown error");
            let desc = resp[1]["description"].as_str().unwrap_or("");
            let tag = resp[2].as_str().unwrap_or("");
            let call = names
                .get(tag.parse::<usize>().unwrap_or(usize::MAX))
                .cloned()
                .unwrap_or_default();
            return Err(if desc.is_empty() {
                format!("{call}: {kind}")
            } else {
                format!("{call}: {kind} ({desc})")
            });
        }
    }
    Ok(responses)
}

/// The mailbox size out of a `Quota/get` list (RFC 9425): the account's own
/// quota on octets that covers mail (the `Email` data type; Stalwart lists
/// it beside calendars and files), preferred over a domain's or the
/// server's; the hard limit, else the soft one.
pub(super) fn jmap_storage_quota(list: &serde_json::Value) -> Option<MailboxQuota> {
    let quotas = list.as_array()?;
    let covers_mail = |q: &serde_json::Value| {
        q["types"]
            .as_array()
            .is_none_or(|t| t.is_empty() || t.iter().any(|x| x.as_str() == Some("Email")))
    };
    let rank = |q: &serde_json::Value| match q["scope"].as_str() {
        Some("account") => 0,
        Some("domain") => 1,
        _ => 2,
    };
    quotas
        .iter()
        .filter(|q| q["resourceType"].as_str() == Some("octets") && covers_mail(q))
        .filter_map(|q| {
            let limit = q["hardLimit"]
                .as_u64()
                .filter(|n| *n > 0)
                .or_else(|| q["softLimit"].as_u64().filter(|n| *n > 0))?;
            Some((
                rank(q),
                MailboxQuota {
                    used: q["used"].as_u64().unwrap_or(0),
                    limit,
                },
            ))
        })
        .min_by_key(|(r, _)| *r)
        .map(|(_, q)| q)
}

/// A method call tagged by its position, so an error can be traced back.
pub(super) fn call(idx: usize, name: &str, args: serde_json::Value) -> serde_json::Value {
    serde_json::json!([name, args, idx.to_string()])
}

/// The arguments of the response at `idx`.
pub(super) fn args(responses: &[serde_json::Value], idx: usize) -> serde_json::Value {
    responses
        .get(idx)
        .map(|r| r[1].clone())
        .unwrap_or(serde_json::Value::Null)
}

/// Fill a URL template's `{name}` slots.
pub(super) fn fill_template(template: &str, slots: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (k, v) in slots {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// The raw bytes of a blob, capped well above any sane message (64 MB).
pub(super) fn jmap_download(s: &JmapSession, blob_id: &str) -> Result<Vec<u8>, String> {
    let url = fill_template(
        &s.download_url,
        &[
            ("accountId", &s.account),
            ("blobId", blob_id),
            ("type", "message%2Frfc822"),
            ("name", "message.eml"),
        ],
    );
    jmap_wire("GET blob");
    let resp = agent()
        .get(&url)
        .set("Authorization", &s.auth)
        .call()
        .map_err(jmap_err);
    jmap_wired("GET blob", &resp);
    let resp = resp?;
    let mut out = Vec::new();
    use std::io::Read;
    resp.into_reader()
        .take(64 * 1024 * 1024)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// Upload raw bytes; the blob id to import or attach.
pub(super) fn jmap_upload(
    s: &JmapSession,
    raw: &[u8],
    content_type: &str,
) -> Result<String, String> {
    // Cyrus (Fastmail) refuses a message with a lone LF anywhere in it:
    // "Message contains bare newlines" (#414). Our builder writes CRLF
    // throughout; mail moved in from a laxer server may not.
    let raw = if content_type == "message/rfc822" {
        crlf_line_endings(raw)
    } else {
        raw.into()
    };
    let url = fill_template(&s.upload_url, &[("accountId", &s.account)]);
    jmap_wire("POST upload");
    let r: Result<serde_json::Value, String> = agent()
        .post(&url)
        .set("Authorization", &s.auth)
        .set("Content-Type", content_type)
        .send_bytes(&raw)
        .map_err(jmap_err)
        .and_then(|resp| resp.into_json().map_err(|e| e.to_string()));
    jmap_wired("POST upload", &r);
    r?["blobId"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "the upload reply has no blobId".into())
}

/// The session, connecting on first use (and after a reconnect dropped it).
/// A failure is reported through `emit` and leaves the state untouched, so
/// the next request tries again.
pub(super) async fn jmap_session(
    account: &AccountConfig,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
) -> Option<JmapSession> {
    if let Some(s) = &state.session {
        return Some(s.clone());
    }
    // The Refresh button already says the network is down (#442).
    if !crate::desktop::power::reaches_url(&jmap_base_url(account)) {
        return None;
    }
    let a = account.clone();
    match blocking(move || jmap_connect(&a)).await {
        Ok(s) => {
            state.session = Some(s.clone());
            emit(WorkerEvent::Hierarchy(
                crate::models::FolderHierarchy::slashed(s.top_level),
            ));
            Some(s)
        }
        Err(e) => {
            emit(WorkerEvent::net_error(i18n_f(
                "Could not reach the JMAP server: {e}",
                &[("e", &e)],
            )));
            None
        }
    }
}

/// Test that the server accepts the credentials and offers mail.
pub(in crate::worker) async fn test_jmap(account: &AccountConfig) -> Result<(), String> {
    let a = account.clone();
    blocking(move || jmap_connect(&a)).await.map(|_| ())
}
