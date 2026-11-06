//! Links in a message: which ones reach the desktop, what the hover preview
//! says about them, and saving `data:` images.

use relm4::prelude::*;

/// The HYLKI_SHOWCASE_CLICK_LINK probe: find the first web link in any
/// message frame, say what the top document has at that point (the frame
/// itself when nothing covers it), and click the link.
pub(super) const CLICK_LINK_PROBE: &str =
    "(function(){var fs=document.querySelectorAll('iframe.hylki-frame');\
for(var i=0;i<fs.length;i++){var d=fs[i].contentDocument;if(!d)continue;\
var a=d.querySelector('a[href^=\"http\"]');if(!a)continue;a.scrollIntoView({block:'center'});\
var fr=fs[i].getBoundingClientRect(),r=a.getBoundingClientRect();\
var x=fr.left+r.left+r.width/2,y=fr.top+r.top+r.height/2;var el=document.elementFromPoint(x,y);\
var who=el?el.tagName+(el.className?'.'+el.className:''):'nothing';a.click();\
return 'clicked '+a.href+' at '+Math.round(x)+','+Math.round(y)+' over '+who;}\
return 'no web link in any frame';})()";

/// Whether a clicked link may be handed to the desktop's default handler.
///
/// An allowlist, not a blocklist: every scheme a desktop registers is a program
/// that would be started with a sender-controlled argument, and there is no way
/// to enumerate the dangerous ones ahead of time.
pub(super) fn is_launchable_uri(uri: &str) -> bool {
    match uri.split_once(':') {
        Some((scheme, rest)) => {
            matches!(
                scheme.to_ascii_lowercase().as_str(),
                "http" | "https" | "mailto"
            ) && !rest.is_empty()
        }
        None => false,
    }
}

/// What to show in the link preview: the destination, plus an explicit warning
/// when the link's visible text claims a different site than it goes to — the
/// oldest phishing trick there is (`click here: paypal.com` pointing elsewhere).
pub(super) fn link_destination(uri: &str, label: Option<&str>) -> String {
    match label.and_then(|l| mismatched_host(uri, l)) {
        Some(claimed) => format!(
            "{uri}  ⚠ looks like \"{claimed}\" but goes to {}",
            host_of(uri).unwrap_or_default()
        ),
        None => uri.to_string(),
    }
}

/// The host a link's visible text claims, when that text is itself a URL or bare
/// hostname pointing somewhere other than the link's real target.
pub(super) fn mismatched_host(uri: &str, label: &str) -> Option<String> {
    let claimed = host_of(label.trim())?;
    let actual = host_of(uri)?;
    // Compare from the right so `mail.example.com` matches `example.com`.
    let same = actual == claimed
        || actual.ends_with(&format!(".{claimed}"))
        || claimed.ends_with(&format!(".{actual}"));
    (!same).then_some(claimed)
}

/// The hostname of a URL, or of a bare hostname like `paypal.com/login`.
pub(super) fn host_of(text: &str) -> Option<String> {
    let text = text.trim();
    // A scheme has no dots, which is what separates `mailto:` from a bare
    // `example.com:8080`. Only web links have a host worth comparing.
    let rest = match text.split_once(':') {
        Some((scheme, rest))
            if !scheme.is_empty()
                && !scheme.contains('.')
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-')) =>
        {
            if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
                return None;
            }
            rest.trim_start_matches("//")
        }
        _ => text,
    };
    let host = rest
        .split(['/', '?', '#'])
        .next()?
        .rsplit('@') // strip any userinfo
        .next()?
        .split(':')
        .next()?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let looks_like_host = host.contains('.')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    looks_like_host.then_some(host)
}

/// Split a `data:<mime>;base64,<payload>` URI into its MIME type and bytes.
/// Returns `None` for any other scheme, or for a `data:` URI that isn't base64
/// (the reader only ever emits base64 ones).
pub(super) fn decode_data_uri(uri: &str) -> Option<(String, Vec<u8>)> {
    let rest = uri
        .strip_prefix("data:")
        .or_else(|| uri.strip_prefix("DATA:"))?;
    let (meta, payload) = rest.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    let data = gtk::glib::base64_decode(payload);
    (!data.is_empty()).then(|| (mime.to_ascii_lowercase(), data))
}

/// The content type and bytes of a picture the message carries itself: a
/// `data:` URI, or one lifted out of its body to be served apart. `None` for
/// a remote picture, which is only an address here.
pub(super) fn own_picture(uri: &str) -> Option<(String, Vec<u8>)> {
    decode_data_uri(uri).or_else(|| crate::services::inline_images::picture(uri))
}

/// A sensible filename to pre-fill the save dialog with. The original name isn't
/// recoverable from a `data:` URI — the properly named copy is in the attachment
/// drawer — so offer `image.<ext>` derived from the MIME type.
pub(super) fn default_image_name(mime: &str) -> String {
    let ext = match mime {
        "image/jpeg" => "jpg",
        "image/svg+xml" => "svg",
        other => other.rsplit('/').next().unwrap_or("img"),
    };
    format!("image.{ext}")
}

/// The name to pre-fill when saving a remote picture: the last part of its
/// address when that looks like a file name (`banner.png`), else what
/// [`default_image_name`] makes of the content type. Tracking and CDN
/// addresses often end in an ID with no extension.
pub(super) fn remote_image_name(url: &str, content_type: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let path = path.split_once("://").map_or(path, |(_, rest)| rest);
    let last = path.split_once('/').map_or("", |(_, p)| p);
    let last = crate::encoding::percent::decode(last.rsplit('/').next().unwrap_or(""), false);
    let named = last.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && (1..=5).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
    }) && !last.contains(['/', '\\', '\0'])
        && !last.starts_with('.');
    if named {
        last
    } else {
        let mime = content_type.split(';').next().unwrap_or("").trim();
        let mime = mime.to_ascii_lowercase();
        match mime.strip_prefix("image/") {
            Some(sub) if !sub.is_empty() => default_image_name(&mime),
            _ => "image".to_string(),
        }
    }
}
