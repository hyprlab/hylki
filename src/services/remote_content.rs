//! Remote content (#474): the images, stylesheets and fonts a message loads
//! from the web, fetched by the app rather than by WebKit and kept on disk.
//!
//! The reader's web context runs WebKit's document-viewer cache model (#106),
//! which turns the HTTP cache off, so a message shown a second time fetched all
//! of its pictures again. Allowed remote URLs are now rewritten to
//! `hylki-http(s)://` (see `route`), and the handler registered for those
//! schemes answers from `~/.cache/hylki/remote`, fetching on a miss. That also
//! makes fetching ahead possible: with the Privacy switch on, the next
//! messages in the list have their remote content fetched while the current
//! one is read.
//!
//! The fetches send no cookies and no Referer, which is less than WebKit sent.

mod fetch;
mod store;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gtk::{gio, glib};

pub(crate) use store::Entry;

/// The schemes the reader's markup is rewritten to.
const SCHEMES: [&str; 2] = ["hylki-http", "hylki-https"];

/// At most this many URLs are fetched ahead for one message, and for one
/// selection: a newsletter can carry hundreds of spacer images.
const AHEAD_PER_MESSAGE: usize = 60;
const AHEAD_TOTAL: usize = 200;

/// `url`, as written in the markup, rewritten to the cached scheme. `None` for
/// anything but `http`, `https` and protocol-relative URLs.
pub(crate) fn route(url: &str) -> Option<String> {
    let lower = url.get(..6).unwrap_or(url).to_ascii_lowercase();
    if url.starts_with("//") {
        Some(format!("hylki-https:{url}"))
    } else if lower.starts_with("https:") {
        Some(format!("hylki-https:{}", &url[6..]))
    } else if lower.starts_with("http:") {
        Some(format!("hylki-http:{}", &url[5..]))
    } else {
        None
    }
}

/// The web URL behind a routed one, as WebKit asks for it.
fn origin_url(routed: &str) -> Option<String> {
    let rest = routed.strip_prefix("hylki-")?;
    let lower = rest.get(..6).unwrap_or(rest).to_ascii_lowercase();
    (lower.starts_with("https:") || lower.starts_with("http:")).then(|| canonical(rest))
}

/// One spelling per URL, so the markup's form (fetched ahead) and WebKit's
/// (asked for by the reader) meet on the same cache entry: the fragment
/// dropped, scheme and host lower case, an empty path written `/`, spaces
/// and non-ASCII percent-encoded as WebKit sends them. A URL this misses is
/// fetched twice, nothing worse.
fn canonical(url: &str) -> String {
    let url = url.trim();
    let url = url.split('#').next().unwrap_or(url);
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let (authority, path) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    // A routed URL's scheme is not one WebKit knows, so it percent-encodes
    // a non-ASCII host instead of leaving it for IDNA: decoded back here, or
    // an image on such a host could not be fetched.
    let authority = crate::encoding::percent::decode(authority, false);
    let mut out = format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        authority.to_ascii_lowercase()
    );
    if !path.starts_with('/') {
        out.push('/');
    }
    for b in path.bytes() {
        if b <= b' ' || b >= 0x7f || matches!(b, b'"' | b'<' | b'>' | b'`') {
            out.push_str(&format!("%{b:02X}"));
        } else {
            out.push(b as char);
        }
    }
    out
}

/// A URL as the markup writes it, made absolute and canonical. Entities
/// decoded first: the HTML parser decodes `&amp;` before WebKit asks.
fn markup_url(raw: &str) -> Option<String> {
    let decoded = crate::encoding::unescape_html(raw.trim());
    let absolute = if decoded.starts_with("//") {
        format!("https:{decoded}")
    } else {
        decoded
    };
    let lower = absolute.get(..6).unwrap_or(&absolute).to_ascii_lowercase();
    (lower.starts_with("https:") || lower.starts_with("http:")).then(|| canonical(&absolute))
}

thread_local! {
    /// The reader's requests waiting on a load already under way, by URL.
    static WAITING: RefCell<HashMap<String, Vec<webkit6::URISchemeRequest>>> =
        RefCell::new(HashMap::new());
}

/// Answer the cached schemes on `ctx`. Marked secure, so the `https` page
/// does not refuse them as mixed content, and CORS-enabled, so a web font
/// fetched through them is allowed to load.
pub(crate) fn register(ctx: &webkit6::WebContext) {
    for scheme in SCHEMES {
        ctx.register_uri_scheme(scheme, handle);
        if let Some(sm) = ctx.security_manager() {
            sm.register_uri_scheme_as_secure(scheme);
            sm.register_uri_scheme_as_cors_enabled(scheme);
        }
    }
    fetch::set_user_agent(
        webkit6::Settings::new()
            .user_agent()
            .map(|s| s.to_string())
            .unwrap_or_default(),
    );
}

/// Let `view`'s pages load from the cached schemes. WebKitGTK shows a
/// custom scheme only to a page it may request it from, which by default is
/// a page on that scheme; the allowlist is what opens it to the reader's
/// `https://hylki.localhost` documents.
pub(crate) fn allow_in(view: &webkit6::WebView) {
    use webkit6::prelude::WebViewExt;
    view.set_cors_allowlist(&["hylki-http://*/*", "hylki-https://*/*"]);
}

fn handle(request: &webkit6::URISchemeRequest) {
    let Some(url) = request.uri().and_then(|u| origin_url(&u)) else {
        respond(request, None);
        return;
    };
    let joined = WAITING.with(|w| {
        let mut w = w.borrow_mut();
        match w.get_mut(&url) {
            Some(waiting) => {
                waiting.push(request.clone());
                true
            }
            None => {
                w.insert(url.clone(), vec![request.clone()]);
                false
            }
        }
    });
    if joined {
        return;
    }
    glib::spawn_future_local(async move {
        let entry = load(url.clone()).await;
        let waiting = WAITING
            .with(|w| w.borrow_mut().remove(&url))
            .unwrap_or_default();
        for request in waiting {
            respond(&request, entry.as_ref());
        }
    });
}

/// The disk first, on a blocking thread of GIO's (a hit is a short read), then
/// the network, on the fetch pool, whose slow requests must not hold up
/// GIO's own file work.
async fn load(url: String) -> Option<Arc<Entry>> {
    let key = url.clone();
    if let Ok(Some(hit)) = gio::spawn_blocking(move || store::read(&key)).await {
        return Some(Arc::new(hit));
    }
    let (tx, rx) = futures::channel::oneshot::channel();
    fetch::submit(url, fetch::Priority::Now, Some(tx));
    rx.await.ok().flatten()
}

/// The bytes of an entry, shared with GLib without a copy.
struct Shared(Arc<Entry>);

impl AsRef<[u8]> for Shared {
    fn as_ref(&self) -> &[u8] {
        &self.0.bytes
    }
}

fn respond(request: &webkit6::URISchemeRequest, entry: Option<&Arc<Entry>>) {
    let Some(entry) = entry else {
        request.finish_error(&mut glib::Error::new(
            gio::IOErrorEnum::NotFound,
            "remote content unavailable",
        ));
        return;
    };
    let bytes = glib::Bytes::from_owned(Shared(entry.clone()));
    let stream = gio::MemoryInputStream::from_bytes(&bytes);
    let response = webkit6::URISchemeResponse::new(&stream, bytes.len() as i64);
    response.set_status(200, None);
    if !entry.content_type.is_empty() {
        response.set_content_type(&entry.content_type);
    }
    let headers = webkit6::soup::MessageHeaders::new(webkit6::soup::MessageHeadersType::Response);
    // The frame's origin is `hylki.localhost`; a font is only used across
    // origins when the response says it may be.
    headers.append("Access-Control-Allow-Origin", "*");
    response.set_http_headers(headers);
    request.finish_with_response(&response);
}

/// Fetch the remote content of these message bodies ahead of the reader,
/// in place of whatever was queued for the last selection. Only bodies whose
/// remote content is allowed belong here; the caller decides that, and
/// whether fetching ahead is on at all.
pub(crate) fn fetch_ahead<'a>(bodies: impl IntoIterator<Item = &'a str>) {
    let mut urls: Vec<String> = Vec::new();
    if crate::desktop::power::fetches_ahead() {
        for body in bodies {
            let mut n = 0;
            for raw in crate::ui::message_view::remote_urls(body) {
                if n == AHEAD_PER_MESSAGE || urls.len() == AHEAD_TOTAL {
                    break;
                }
                if let Some(url) = markup_url(raw) {
                    if !urls.contains(&url) {
                        urls.push(url);
                        n += 1;
                    }
                }
            }
        }
    }
    fetch::replace_ahead(urls);
}

#[cfg(test)]
mod tests {
    use super::{canonical, markup_url, origin_url, route};

    #[test]
    fn routes_web_urls_and_keeps_the_rest() {
        assert_eq!(
            route("https://a.example/x.png").as_deref(),
            Some("hylki-https://a.example/x.png")
        );
        assert_eq!(
            route("HTTP://a.example/x.png").as_deref(),
            Some("hylki-http://a.example/x.png")
        );
        assert_eq!(
            route("//a.example/x.png").as_deref(),
            Some("hylki-https://a.example/x.png")
        );
        assert_eq!(route("ftp://a.example/x"), None);
        assert_eq!(route("data:image/png;base64,AA"), None);
    }

    #[test]
    fn a_routed_url_comes_back_as_it_went() {
        for url in [
            "https://a.example/x.png?w=1&h=2",
            "http://a.example/",
            "https://a.example/b%20c.png",
        ] {
            let routed = route(url).unwrap();
            assert_eq!(origin_url(&routed).as_deref(), Some(url));
        }
        assert_eq!(origin_url("https://a.example/"), None);
    }

    #[test]
    fn markup_and_webkit_spellings_meet() {
        // What WebKit asks for after parsing the markup.
        let asked = origin_url("hylki-https://img.example/a/b.png?x=1&y=%C3%A9").unwrap();
        assert_eq!(
            markup_url("https://IMG.example/a/b.png?x=1&amp;y=é#top").as_deref(),
            Some(asked.as_str())
        );
        assert_eq!(
            markup_url("//img.example").as_deref(),
            Some("https://img.example/")
        );
        assert_eq!(markup_url("cid:part1"), None);
        // WebKit's spelling of a non-ASCII host on a scheme it doesn't know.
        assert_eq!(
            origin_url("hylki-https://%C3%A9x.example/a.png").as_deref(),
            markup_url("https://éx.example/a.png").as_deref()
        );
    }

    #[test]
    fn canonical_is_settled() {
        let once = canonical("https://Host.example/a b?q=ü");
        assert_eq!(once, "https://host.example/a%20b?q=%C3%BC");
        assert_eq!(canonical(&once), once);
    }
}
