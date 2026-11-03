//! Remote-content detection and stripping. One walk over the HTML finds
//! every URL that would fetch from the network; the banner and the blocking
//! both use it, so they cannot disagree.

/// Every URL in `html` that would cause a fetch from a remote host, as byte
/// ranges into `html`.
///
/// One walk feeds both the detector and the stripper, so the banner and the
/// blocking can no longer disagree about what counts as remote. The pair of
/// substring lists this replaces had already drifted: `SRC="HTTP://…"` was
/// detected but not stripped, and `src = "http://…"` (whitespace around `=`,
/// which HTML permits) was neither.
fn remote_url_spans(html: &str) -> Vec<(usize, usize)> {
    let b = html.as_bytes();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let closing = j < b.len() && b[j] == b'/';
        if closing {
            j += 1;
        }
        let name_start = j;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'-' || b[j] == b':') {
            j += 1;
        }
        if j == name_start {
            // Not a tag (a comment, a stray `<`, an entity) — nothing to parse.
            i += 1;
            continue;
        }
        let tag = html[name_start..j].to_ascii_lowercase();

        // Attributes, up to the closing `>`.
        while j < b.len() && b[j] != b'>' {
            if b[j].is_ascii_whitespace() || b[j] == b'/' {
                j += 1;
                continue;
            }
            let an_start = j;
            while j < b.len() && !matches!(b[j], b'=' | b'>' | b'/') && !b[j].is_ascii_whitespace()
            {
                j += 1;
            }
            if j == an_start {
                j += 1;
                continue;
            }
            let attr = String::from_utf8_lossy(&b[an_start..j]).to_ascii_lowercase();

            // `=` may be surrounded by whitespace; an attribute may have no value.
            let mut k = j;
            while k < b.len() && b[k].is_ascii_whitespace() {
                k += 1;
            }
            if k >= b.len() || b[k] != b'=' {
                continue;
            }
            k += 1;
            while k < b.len() && b[k].is_ascii_whitespace() {
                k += 1;
            }
            let (vs, ve) = if k < b.len() && (b[k] == b'"' || b[k] == b'\'') {
                let q = b[k];
                k += 1;
                let start = k;
                while k < b.len() && b[k] != q {
                    k += 1;
                }
                let end = k;
                if k < b.len() {
                    k += 1;
                }
                (start, end)
            } else {
                let start = k;
                while k < b.len() && b[k] != b'>' && !b[k].is_ascii_whitespace() {
                    k += 1;
                }
                (start, k)
            };
            j = k;

            match attr.as_str() {
                // "url 1x, url 2x, …" — the URL is each candidate's first token.
                "srcset" | "imagesrcset" => {
                    let mut at = vs;
                    while at < ve {
                        let mut end = at;
                        while end < ve && b[end] != b',' {
                            end += 1;
                        }
                        push_url(b, at, end, &mut spans);
                        at = end + 1;
                    }
                }
                "style" => push_css(b, vs, ve, &mut spans),
                _ if fetches(&tag, &attr) => push_url(b, vs, ve, &mut spans),
                _ => {}
            }
        }

        // A `<style>` element's body carries `url()` and `@import`. Only the
        // opening tag starts one — treating `</style>` as another would take the
        // rest of the document for stylesheet text and skip past everything in
        // it, which is how the `<img>` after a `<style>` block went unstripped.
        if tag == "style" && !closing {
            let start = (j + 1).min(b.len());
            let end = find_ci(b, b"</style", start).unwrap_or(b.len());
            push_css(b, start, end, &mut spans);
            j = end;
        }

        i = if j > i { j } else { i + 1 };
    }

    // Only spans that land on character boundaries can be sliced or replaced.
    spans.retain(|(s, e)| s < e && html.is_char_boundary(*s) && html.is_char_boundary(*e));
    spans.sort_unstable();
    spans.dedup();
    spans
}

/// Whether an attribute on this element causes a fetch on its own.
fn fetches(tag: &str, attr: &str) -> bool {
    match attr {
        "src" | "poster" | "background" | "lowsrc" | "dynsrc" | "codebase" | "xlink:href" => true,
        "data" => tag == "object",
        // `href` fetches on `<link>` (stylesheets, preloads, icons) and on SVG
        // `<image>`/`<use>`. On `<a>` it is a destination the user must click,
        // and treating those as remote content would put the banner on nearly
        // every message and so teach people to ignore it.
        "href" => matches!(tag, "link" | "image" | "use"),
        _ => false,
    }
}

/// Record `b[start..end]` (trimmed) if it points at a remote host.
fn push_url(b: &[u8], start: usize, end: usize, spans: &mut Vec<(usize, usize)>) {
    let mut s = start;
    let mut e = end.min(b.len());
    while s < e && b[s].is_ascii_whitespace() {
        s += 1;
    }
    // For `srcset` the candidate is "<url> <descriptor>"; the URL ends at the
    // first space. For a plain attribute there is no descriptor to trim.
    let mut u = s;
    while u < e && !b[u].is_ascii_whitespace() {
        u += 1;
    }
    e = u;
    if s < e && is_remote_url(&b[s..e]) {
        spans.push((s, e));
    }
}

/// Record every remote `url(…)` and `@import "…"` in a stylesheet or `style=`.
fn push_css(b: &[u8], start: usize, end: usize, spans: &mut Vec<(usize, usize)>) {
    let end = end.min(b.len());
    let mut at = start;
    while let Some(p) = find_ci(&b[..end], b"url(", at) {
        if p >= end {
            break;
        }
        let mut k = p + 4;
        while k < end && b[k].is_ascii_whitespace() {
            k += 1;
        }
        let quote = if k < end && (b[k] == b'"' || b[k] == b'\'') {
            let q = b[k];
            k += 1;
            Some(q)
        } else {
            None
        };
        let term = quote.unwrap_or(b')');
        let vs = k;
        while k < end && b[k] != term && !(quote.is_none() && b[k].is_ascii_whitespace()) {
            k += 1;
        }
        if vs < k && is_remote_url(&b[vs..k]) {
            spans.push((vs, k));
        }
        at = (p + 4).max(k);
    }
    let mut at = start;
    while let Some(p) = find_ci(&b[..end], b"@import", at) {
        if p >= end {
            break;
        }
        let mut k = p + 7;
        while k < end && b[k].is_ascii_whitespace() {
            k += 1;
        }
        // The `url(…)` form is already covered by the loop above; this is the
        // bare-string form, `@import "//host/x.css"`.
        if k < end && (b[k] == b'"' || b[k] == b'\'') {
            let q = b[k];
            k += 1;
            let vs = k;
            while k < end && b[k] != q {
                k += 1;
            }
            if vs < k && is_remote_url(&b[vs..k]) {
                spans.push((vs, k));
            }
        }
        at = p + 7;
    }
}

/// Whether a URL reaches a remote host.
///
/// `data:` and `cid:` carry their own bytes, and a path-relative or root-relative
/// URL resolves against `hylki.localhost`, which serves nothing. A
/// protocol-relative `//host/x` does reach the network — that one is the bypass
/// the old substring list missed.
pub(super) fn is_remote_url(u: &[u8]) -> bool {
    let u = std::str::from_utf8(u).unwrap_or("").trim();
    if u.starts_with("//") {
        return true;
    }
    match u.split_once(':') {
        Some((scheme, _))
            if !scheme.is_empty()
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) =>
        {
            !matches!(
                scheme.to_ascii_lowercase().as_str(),
                "data" | "cid" | "blocked" | "mailto" | "tel" | "about" | "javascript"
            )
        }
        _ => false,
    }
}

/// Case-insensitive byte search from `from`.
fn find_ci(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() || needle.is_empty() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
        .map(|p| p + from)
}

/// Neutralize remote resource references so nothing is fetched while blocked.
/// Targets resource-loading attributes only; `<a href>` links are left intact.
pub(super) fn strip_remote(html: &str) -> String {
    let spans = remote_url_spans(html);
    if spans.is_empty() {
        return html.to_string();
    }
    let mut out = String::with_capacity(html.len());
    let mut at = 0usize;
    for (s, e) in spans {
        if s < at {
            continue; // overlapping match already rewritten
        }
        out.push_str(&html[at..s]);
        out.push_str("blocked://");
        at = e;
    }
    out.push_str(&html[at..]);
    out
}

/// Does the HTML reference remote (network-reachable) resources?
///
/// This decides whether the "remote content was blocked" banner appears, and
/// nothing more — the blocking itself follows the user's setting, not this. A
/// miss here is a missing banner, not a leaked request.
pub(super) fn has_remote_resources(html: &str) -> bool {
    !remote_url_spans(html).is_empty()
}

/// Send every remote fetch in `html` through the app's own cache (#474):
/// `https://host/x` becomes `hylki-https://host/x`, which the web context
/// hands to [`crate::services::remote_content`]. The original scheme is kept in
/// the new one's name, so a relative URL inside a fetched stylesheet still
/// resolves on the right host. Only for mail whose remote content is allowed;
/// a URL with any other scheme is left alone.
pub(super) fn route_remote(html: &str) -> String {
    let spans = remote_url_spans(html);
    if spans.is_empty() {
        return html.to_string();
    }
    let mut out = String::with_capacity(html.len() + spans.len() * 6);
    let mut at = 0usize;
    for (s, e) in spans {
        if s < at {
            continue;
        }
        out.push_str(&html[at..s]);
        let url = &html[s..e];
        out.push_str(
            &crate::services::remote_content::route(url).unwrap_or_else(|| url.to_string()),
        );
        at = e;
    }
    out.push_str(&html[at..]);
    out
}

/// The remote URLs in `html` as written in the markup, entities and all: what
/// the prefetch fetches ahead of the reader (#474).
pub(crate) fn remote_urls(html: &str) -> Vec<&str> {
    remote_url_spans(html)
        .into_iter()
        .map(|(s, e)| &html[s..e])
        .collect()
}
