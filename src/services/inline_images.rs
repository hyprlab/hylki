//! The pictures a message carries in its body, served to the reader from
//! memory instead of riding in its page.
//!
//! The worker writes the pictures a message shows by Content-ID into its body
//! as `data:` URIs, and some senders embed theirs that way: a body with a few
//! photos runs to megabytes of base64. The reader builds each card's document
//! in a run of passes over the body (remote content, dark colors, its own
//! sheet, the security policy, escaping into the frame's `srcdoc`), and WebKit
//! then parses the result twice, as the wrapper's attribute and as the
//! frame's document. Lifted out before all that, each picture is a short
//! `hylki-inline:` URL, and WebKit fetches its bytes once.
//!
//! A picture copied out of the reader with the text around it keeps that URL,
//! and the composer turns it back into a `data:` URI as it lands, as it does a
//! pasted `blob:` image, so what is sent carries its own bytes.

use std::cell::RefCell;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use gtk::{gio, glib};

use crate::ram_cache::{RamCache, Weigh};

/// The scheme lifted pictures are served under.
const SCHEME: &str = "hylki-inline";

/// What comes before a picture's key in its URL.
const PREFIX: &str = "hylki-inline://picture/";

/// What lets a page load lifted pictures, for its view's CORS allowlist: the
/// reader's, and the composer's for a picture pasted from it.
pub(crate) const CORS_PATTERN: &str = "hylki-inline://*/*";

/// Base64 shorter than this stays in the page: an icon costs less there than
/// a request does.
const LIFT_FROM: usize = 2048;

/// The base64 the store may hold. A page built again lifts its pictures
/// again, so a picture dropped from here is back as soon as it is shown.
const BUDGET: usize = 64 * 1024 * 1024;

/// One lifted picture: its content type, and its bytes still in base64. They
/// are decoded when WebKit asks for them, off the main thread.
#[derive(Clone)]
struct Picture {
    mime: String,
    base64: Arc<str>,
}

impl Weigh for Picture {
    fn weight(&self) -> usize {
        self.base64.len()
    }
}

thread_local! {
    /// Lifted pictures by the hash of their base64. Main-thread only, as
    /// pages are built and the scheme is answered there.
    static STORE: RefCell<RamCache<u64, Picture>> = RefCell::new(RamCache::new(BUDGET));
}

/// A `data:image/…;base64,…` URI found in a document.
struct DataImage<'a> {
    /// Where the URI starts and ends in the document.
    start: usize,
    end: usize,
    mime: &'a str,
    payload: &'a str,
}

/// `html` with each large `data:image/…;base64,…` URI replaced by a
/// `hylki-inline:` URL for the same picture, handed back as it was when it
/// holds none.
pub(crate) fn lift(html: String) -> String {
    let mut out = String::new();
    let mut copied = 0;
    let mut from = 0;
    while let Some(found) = find_data_image(&html, from) {
        from = found.end;
        if found.payload.len() < LIFT_FROM {
            continue;
        }
        out.push_str(&html[copied..found.start]);
        out.push_str(&url(keep(found.mime, found.payload)));
        copied = found.end;
    }
    if copied == 0 {
        return html;
    }
    out.push_str(&html[copied..]);
    out
}

/// The first `data:image/…;base64,…` URI in `html` from `from` on. Only one
/// that ends cleanly is taken, at a quote, a bracket or an entity: base64
/// broken over lines stays in the page, where it works as it always did.
fn find_data_image(html: &str, mut from: usize) -> Option<DataImage<'_>> {
    loop {
        let start = from + html.get(from..)?.find("data:")?;
        from = start + "data:".len();
        let rest = &html[from..];
        // The media type and its parameters, up to the comma: never long.
        let Some(comma) = rest.bytes().take(128).position(|b| b == b',') else {
            continue;
        };
        let header = &rest[..comma];
        let is_image = header
            .get(..6)
            .is_some_and(|h| h.eq_ignore_ascii_case("image/"));
        let is_base64 = header
            .get(header.len().saturating_sub(7)..)
            .is_some_and(|h| h.eq_ignore_ascii_case(";base64"));
        let mime = header.split(';').next().unwrap_or_default();
        let plain_mime = mime
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'+' | b'-' | b'.'));
        if !(is_image && is_base64 && plain_mime) {
            continue;
        }
        let payload_start = from + comma + 1;
        let len = html[payload_start..]
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
            .count();
        let end = payload_start + len;
        if !matches!(html.as_bytes().get(end), Some(b'"' | b'\'' | b')' | b'&')) {
            continue;
        }
        return Some(DataImage {
            start,
            end,
            mime,
            payload: &html[payload_start..end],
        });
    }
}

/// Keep a picture for the scheme to serve, and return its key.
fn keep(mime: &str, payload: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    payload.hash(&mut hasher);
    let key = hasher.finish();
    STORE.with(|store| {
        let mut store = store.borrow_mut();
        if !store.contains_key(&key) {
            let picture = Picture {
                mime: mime.to_ascii_lowercase(),
                base64: payload.into(),
            };
            store.insert(key, picture);
        }
    });
    key
}

fn url(key: u64) -> String {
    format!("{PREFIX}{key:016x}")
}

fn key_of(url: &str) -> Option<u64> {
    u64::from_str_radix(url.strip_prefix(PREFIX)?, 16).ok()
}

/// The lifted picture at `url`, while the store has it.
fn stored(url: &str) -> Option<Picture> {
    let key = key_of(url)?;
    STORE.with(|store| store.borrow().get(&key).cloned())
}

/// The content type and bytes of the lifted picture at `url`, for the
/// reader's Save Image and Copy Image.
pub(crate) fn picture(url: &str) -> Option<(String, Vec<u8>)> {
    let picture = stored(url)?;
    let bytes = crate::encoding::base64_decode(&picture.base64)?;
    Some((picture.mime, bytes))
}

/// Serve lifted pictures on `ctx`. Marked secure, or the `https` pages would
/// refuse them as mixed content, and CORS-enabled, so the composer can fetch
/// a pasted one to keep its bytes.
pub(crate) fn register(ctx: &webkit6::WebContext) {
    ctx.register_uri_scheme(SCHEME, handle);
    if let Some(security) = ctx.security_manager() {
        security.register_uri_scheme_as_secure(SCHEME);
        security.register_uri_scheme_as_cors_enabled(SCHEME);
    }
}

fn handle(request: &webkit6::URISchemeRequest) {
    let Some(picture) = request.uri().and_then(|uri| stored(&uri)) else {
        not_found(request);
        return;
    };
    let request = request.clone();
    glib::spawn_future_local(async move {
        let base64 = picture.base64.clone();
        let decoded = gio::spawn_blocking(move || crate::encoding::base64_decode(&base64))
            .await
            .ok()
            .flatten();
        let Some(bytes) = decoded else {
            not_found(&request);
            return;
        };
        let bytes = glib::Bytes::from_owned(bytes);
        let stream = gio::MemoryInputStream::from_bytes(&bytes);
        let response = webkit6::URISchemeResponse::new(&stream, bytes.len() as i64);
        response.set_status(200, None);
        response.set_content_type(&picture.mime);
        // The pages are `hylki.localhost`'s; the composer's fetch of a pasted
        // picture is cross-origin.
        let headers =
            webkit6::soup::MessageHeaders::new(webkit6::soup::MessageHeadersType::Response);
        headers.append("Access-Control-Allow-Origin", "*");
        response.set_http_headers(headers);
        request.finish_with_response(&response);
    });
}

fn not_found(request: &webkit6::URISchemeRequest) {
    request.finish_error(&mut glib::Error::new(
        gio::IOErrorEnum::NotFound,
        "no such picture",
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(len: usize) -> String {
        "QUJD".repeat(len / 4)
    }

    #[test]
    fn a_large_picture_is_lifted_and_kept() {
        let b64 = payload(4096);
        let html = format!(r#"<p><img src="data:image/png;base64,{b64}" alt="x"></p>"#);
        let lifted = lift(html);
        assert!(!lifted.contains("base64"), "{lifted}");
        let url = lifted
            .split('"')
            .find(|s| s.starts_with(PREFIX))
            .expect("a hylki-inline URL");
        assert_eq!(lifted, format!(r#"<p><img src="{url}" alt="x"></p>"#));
        let picture = stored(url).expect("the picture is kept");
        assert_eq!(picture.mime, "image/png");
        assert_eq!(&*picture.base64, b64);
        let (mime, bytes) = super::picture(url).expect("Save Image finds it");
        assert_eq!((mime.as_str(), &bytes[..3]), ("image/png", &b"ABC"[..]));
    }

    #[test]
    fn small_and_unfinished_pictures_stay_in_the_page() {
        let small = format!(r#"<img src="data:image/gif;base64,{}">"#, payload(64));
        assert_eq!(lift(small.clone()), small);
        // Broken over lines: what follows the break would be left behind.
        let wrapped = format!(
            "<img src=\"data:image/png;base64,{}\n{}\">",
            payload(4096),
            payload(8)
        );
        assert_eq!(lift(wrapped.clone()), wrapped);
    }

    #[test]
    fn only_base64_images_are_lifted() {
        let svg_text = format!(r#"<img src="data:image/svg+xml,{}">"#, payload(4096));
        assert_eq!(lift(svg_text.clone()), svg_text);
        let not_image = format!(r#"<a href="data:text/plain;base64,{}">"#, payload(4096));
        assert_eq!(lift(not_image.clone()), not_image);
        let prose = "data: none of it, and image/png on its own".to_string();
        assert_eq!(lift(prose.clone()), prose);
    }

    #[test]
    fn css_and_escaped_quotes_end_a_picture_too() {
        let b64 = payload(4096);
        for html in [
            format!("<div style=\"background:url(data:image/jpeg;base64,{b64})\"></div>"),
            format!(
                "<div style=\"background:url(&quot;data:image/jpeg;base64,{b64}&quot;)\"></div>"
            ),
            format!("<img src='data:image/webp;base64,{b64}'>"),
        ] {
            let lifted = lift(html.clone());
            assert!(
                lifted.contains(PREFIX) && !lifted.contains("base64"),
                "{html}"
            );
        }
    }

    #[test]
    fn the_same_picture_twice_is_one_url() {
        let b64 = payload(4096);
        let html = format!(
            r#"<img src="data:image/png;base64,{b64}"><img src="data:image/png;base64,{b64}">"#
        );
        let lifted = lift(html);
        let urls: Vec<&str> = lifted
            .split('"')
            .filter(|s| s.starts_with(PREFIX))
            .collect();
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0], urls[1]);
    }

    #[test]
    fn a_url_names_its_key() {
        assert_eq!(key_of(&url(0xfeed)), Some(0xfeed));
        assert_eq!(key_of("hylki-https://example.org/x"), None);
    }
}
