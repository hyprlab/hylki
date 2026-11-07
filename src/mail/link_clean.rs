//! Link trackers: what a link carries about its reader, taken off before
//! the browser opens it. Campaign tags (`utm_source`, `fbclid`, `mc_eid` and
//! the like) tell the site which mail, list and often which subscriber the
//! click came from, and a redirector (Google's `/url`, Facebook's `l.php`)
//! logs the click on its way to the real address. Both are rewritten here
//! from the link alone: a redirector is unwrapped only when its target is
//! written into the link, so nothing is fetched to find out where it goes.
//!
//! Outlook Safe Links stay wrapped: Microsoft Defender checks the target
//! when the link is clicked, and a work account may depend on that check.

use crate::encoding::percent;

/// Query parameters that only track, compared without case. A name ending
/// in `*` is a prefix.
const TRACKING: &[&str] = &[
    "utm_*",
    "fbclid",
    "gclid",
    "gclsrc",
    "dclid",
    "gbraid",
    "wbraid",
    "msclkid",
    "yclid",
    "twclid",
    "ttclid",
    "igshid",
    "li_fat_id",
    "mc_cid",
    "mc_eid",
    "_hsenc",
    "_hsmi",
    "__hssc",
    "__hstc",
    "__hsfp",
    "hsctatracking",
    "mkt_tok",
    "oly_anon_id",
    "oly_enc_id",
    "vero_id",
    "vero_conv",
    "_openstat",
    "ck_subscriber_id",
    "ml_subscriber",
    "ml_subscriber_hash",
    "rb_clickid",
    "s_cid",
    "wickedid",
    "_kx",
    "sc_cid",
];

/// How many redirectors deep a link is unwrapped: a redirect inside another
/// is real, more than that is a loop or a puzzle.
const MAX_UNWRAP: usize = 4;

/// `uri` without its redirectors and tracking parameters. Anything that is
/// not an http or https address comes back as it was.
pub(crate) fn clean_link(uri: &str) -> String {
    let mut current = uri.trim().to_string();
    for _ in 0..MAX_UNWRAP {
        match unwrap_redirect(&current) {
            Some(target) => current = target,
            None => break,
        }
    }
    strip_tracking(&current)
}

/// The parts of an http(s) URL this needs: the host, lowercased and
/// without user or port, and where the query and the fragment start.
struct Parts<'a> {
    host: String,
    path: &'a str,
    /// Everything before the `?` (or the whole URL up to the fragment).
    head: &'a str,
    query: Option<&'a str>,
    /// The fragment with its `#`, or "".
    fragment: &'a str,
}

fn parts(uri: &str) -> Option<Parts<'_>> {
    let lower = uri.get(..8)?.to_ascii_lowercase();
    let rest_at = if lower.starts_with("https://") {
        8
    } else if lower.starts_with("http://") {
        7
    } else {
        return None;
    };
    let (before_fragment, fragment) = match uri.find('#') {
        Some(i) => uri.split_at(i),
        None => (uri, ""),
    };
    let (head, query) = match before_fragment.split_once('?') {
        Some((h, q)) => (h, Some(q)),
        None => (before_fragment, None),
    };
    let after_scheme = head.get(rest_at..)?;
    let (authority, path) = match after_scheme.find('/') {
        Some(i) => after_scheme.split_at(i),
        None => (after_scheme, ""),
    };
    let host = authority.rsplit('@').next().unwrap_or("");
    // A bracketed IPv6 literal keeps its colons; otherwise a colon starts
    // the port.
    let host = if host.starts_with('[') {
        host.split(']')
            .next()
            .map(|h| format!("{h}]"))
            .unwrap_or_default()
    } else {
        host.split(':').next().unwrap_or("").to_string()
    };
    Some(Parts {
        host: host.trim_end_matches('.').to_ascii_lowercase(),
        path,
        head,
        query,
        fragment,
    })
}

/// The address a known redirector link carries, when it carries one that is
/// itself a web address.
fn unwrap_redirect(uri: &str) -> Option<String> {
    let p = parts(uri)?;
    let param = redirect_param(&p.host, p.path)?;
    let raw = p.query?.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        name.eq_ignore_ascii_case(param).then_some(value)
    })?;
    let target = percent::decode(raw, false);
    let target = target.trim();
    // Only a whole web address is followed: anything else the redirector
    // would have had to look up, or is not a link at all.
    let lower = target.to_ascii_lowercase();
    let is_web = lower.starts_with("https://") || lower.starts_with("http://");
    if !is_web || target.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    parts(target)?;
    Some(target.to_string())
}

/// Which query parameter holds the target, for a known redirector.
fn redirect_param(host: &str, path: &str) -> Option<&'static str> {
    let path = path.trim_end_matches('/');
    let bare = host.strip_prefix("www.").unwrap_or(host);
    if is_google(bare) && path.eq_ignore_ascii_case("/url") {
        return Some("q");
    }
    if matches!(
        host,
        "l.facebook.com" | "lm.facebook.com" | "l.messenger.com"
    ) && path.eq_ignore_ascii_case("/l.php")
    {
        return Some("u");
    }
    if matches!(bare, "youtube.com" | "m.youtube.com") && path.eq_ignore_ascii_case("/redirect") {
        return Some("q");
    }
    None
}

/// `google.com` or a country's Google (`google.de`, `google.co.uk`,
/// `google.com.au`), and no other host that merely starts with "google.".
fn is_google(host: &str) -> bool {
    let Some(tld) = host.strip_prefix("google.") else {
        return false;
    };
    let country = |s: &str| s.len() == 2 && s.bytes().all(|b| b.is_ascii_lowercase());
    tld == "com"
        || country(tld)
        || tld
            .split_once('.')
            .is_some_and(|(second, cc)| matches!(second, "co" | "com") && country(cc))
}

fn is_tracking(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    TRACKING.iter().any(|t| match t.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == *t,
    })
}

/// `uri` with the tracking parameters taken out of its query, the rest left
/// byte for byte as it was.
fn strip_tracking(uri: &str) -> String {
    let Some(p) = parts(uri) else {
        return uri.to_string();
    };
    let Some(query) = p.query else {
        return uri.to_string();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let name = pair.split_once('=').map_or(*pair, |(n, _)| n);
            !is_tracking(name)
        })
        .collect();
    if kept.len() == query.split('&').count() {
        return uri.to_string();
    }
    let mut out = p.head.to_string();
    if kept.iter().any(|k| !k.is_empty()) {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    out.push_str(p.fragment);
    out
}

#[cfg(test)]
mod tests {
    use super::clean_link;

    #[test]
    fn links_lose_their_trackers_and_redirectors() {
        for (link, opened) in [
            // Tracking parameters go; the rest stays as written, in order.
            (
                "https://shop.example/item?id=7&utm_source=news&utm_medium=email",
                "https://shop.example/item?id=7",
            ),
            (
                "https://shop.example/?UTM_Campaign=x&q=a%20b&fbclid=IwAR0",
                "https://shop.example/?q=a%20b",
            ),
            (
                "https://shop.example/p?gclid=1&mc_eid=2&mc_cid=3",
                "https://shop.example/p",
            ),
            (
                "https://shop.example/p?_hsenc=a&_hsmi=b#reviews",
                "https://shop.example/p#reviews",
            ),
            (
                "https://shop.example/p?msclkid=1&ref=friend#top",
                "https://shop.example/p?ref=friend#top",
            ),
            // Nothing to take: the very same text.
            ("https://example.org/a?b=c&d", "https://example.org/a?b=c&d"),
            ("https://example.org/", "https://example.org/"),
            // A name that only looks like one is a site's own.
            (
                "https://example.org/?utmost=1&gclidx=2",
                "https://example.org/?utmost=1&gclidx=2",
            ),
            // A fragment is not a query.
            (
                "https://example.org/#utm_source=x",
                "https://example.org/#utm_source=x",
            ),
            // Outlook Safe Links are left for Defender to check, the
            // tracking inside them too.
            (
                "https://eur01.safelinks.protection.outlook.com/?url=https%3A%2F%2Fnews.example%2Fstory%3Fid%3D3%26utm_source%3Dnl&data=05%7C01&sdata=abc&reserved=0",
                "https://eur01.safelinks.protection.outlook.com/?url=https%3A%2F%2Fnews.example%2Fstory%3Fid%3D3%26utm_source%3Dnl&data=05%7C01&sdata=abc&reserved=0",
            ),
            // Google's redirect, in a country's Google too.
            (
                "https://www.google.com/url?q=https://example.org/page&sa=D&ust=1&usg=AOv",
                "https://example.org/page",
            ),
            (
                "https://www.google.co.uk/url?sa=t&q=https%3A%2F%2Fexample.org%2F",
                "https://example.org/",
            ),
            // Facebook's and YouTube's.
            (
                "https://l.facebook.com/l.php?u=https%3A%2F%2Fexample.org%2F%3Ffbclid%3Dx&h=AT0",
                "https://example.org/",
            ),
            (
                "https://www.youtube.com/redirect?event=video&q=https%3A%2F%2Fexample.org%2Fdocs",
                "https://example.org/docs",
            ),
            // Google's redirect around Facebook's: both come off.
            (
                "https://www.google.com/url?q=https%3A%2F%2Fl.facebook.com%2Fl.php%3Fu%3Dhttps%253A%252F%252Fexample.org%252Fx",
                "https://example.org/x",
            ),
            // A redirector whose target is not a web address, or not in the
            // link at all, is left for the browser to follow.
            (
                "https://www.google.com/url?q=javascript%3Aalert(1)",
                "https://www.google.com/url?q=javascript%3Aalert(1)",
            ),
            (
                "https://l.facebook.com/l.php?h=AT0",
                "https://l.facebook.com/l.php?h=AT0",
            ),
            // Hosts that only resemble a redirector are not one.
            (
                "https://google.evil.example/url?q=https://example.org/",
                "https://google.evil.example/url?q=https://example.org/",
            ),
            (
                "https://l.facebook.com.evil.example/l.php?u=https://example.org/",
                "https://l.facebook.com.evil.example/l.php?u=https://example.org/",
            ),
            // Only web links are touched.
            (
                "mailto:list@example.org?subject=hi&utm_source=x",
                "mailto:list@example.org?subject=hi&utm_source=x",
            ),
            (
                "HTTP://Example.org:8080/a?utm_source=x&b=1",
                "HTTP://Example.org:8080/a?b=1",
            ),
        ] {
            assert_eq!(clean_link(link), opened, "{link}");
        }
    }
}
