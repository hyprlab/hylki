//! A raw message as the HTML the reader shows: every display part in order,
//! inline and cid: images as data: URIs, plain text escaped and linkified.

use crate::encoding::escape_html;

/// Extract a renderable HTML body from a raw RFC 822 message. A lone HTML part is
/// used directly; anything else is composed part by part.
///
/// A message can carry more than one display body. Apple Mail (iPhone) sends photo
/// mail as a multipart/mixed that interleaves text parts with the images, so taking
/// only the first body — as `body_html(0)` does — renders the message blank. Walk
/// every display part in order instead, embedding inline images as `data:` URIs:
/// the reader's CSP permits those even while remote content is blocked, and the
/// bytes arrived with the message, so nothing is fetched from the network.
///
/// HTML bodies that reference their images as `cid:` (Gmail's `multipart/related`
/// photo mail, most newsletters) get the same treatment: nothing in the reader can
/// resolve a `cid:` URL, so each reference is rewritten to the `data:` URI of the
/// part it names — otherwise the image renders as its broken-image alt text.
///
/// Render a raw message to the HTML the reader displays. Public so the Outbox
/// can show a queued message without a round trip to the server — the bytes are
/// already on disk.
pub(crate) fn extract_body(raw: &[u8]) -> String {
    use mail_parser::{MessageParser, MimeHeaders, PartType};

    /// Total decoded bytes of inline images embedded into one message body.
    const INLINE_IMAGE_BUDGET: usize = 16 * 1024 * 1024;

    let Some(parsed) = MessageParser::default().parse(raw) else {
        return wrap_plain(&String::from_utf8_lossy(raw));
    };

    // The protected-headers stand-in that only repeats the Subject (#493).
    let legacy = crate::mail::pgp::legacy_display_part(&parsed);
    let bodies: Vec<_> = parsed
        .html_body
        .iter()
        .filter(|&&id| Some(id) != legacy)
        .filter_map(|&id| parsed.part(id))
        .collect();
    let mut budget = INLINE_IMAGE_BUDGET;
    // Content-IDs rendered in place by `inline_cid_images`, so the loop below
    // doesn't also append them as standalone images.
    let mut embedded: Vec<String> = Vec::new();

    // A single HTML part is already a complete document — pass it through with only
    // its `cid:` references resolved, so the sender's own layout and styling survive.
    if let [only] = bodies.as_slice() {
        if let PartType::Html(html) = &only.body {
            return inline_cid_images(html, &parsed.parts, &mut budget, &mut embedded);
        }
    }

    // Resolve every HTML body first: an image part can appear *before* the body
    // that references it, and the loop below needs to know it was consumed.
    let rendered: Vec<Option<String>> = bodies
        .iter()
        .map(|part| match &part.body {
            PartType::Html(html) => Some(inline_cid_images(
                html,
                &parsed.parts,
                &mut budget,
                &mut embedded,
            )),
            _ => None,
        })
        .collect();

    let mut inner = String::new();
    for (part, html) in bodies.iter().zip(&rendered) {
        if let Some(html) = html {
            inner.push_str(html);
            continue;
        }
        match &part.body {
            PartType::Text(text) if !text.trim().is_empty() => {
                inner.push_str("<div class=\"hylki-plain\">");
                inner.push_str(&linkify(&display_text(part, text)));
                inner.push_str("</div>");
            }
            PartType::Binary(bytes) | PartType::InlineBinary(bytes) => {
                // Already rendered where the HTML placed it — don't repeat it.
                if part
                    .content_id()
                    .is_some_and(|id| embedded.iter().any(|e| e == id))
                {
                    continue;
                }
                // Embedding inflates by ~4/3 and the result is cached on disk, so
                // stop inlining past a budget. Oversized images are still listed
                // as attachments and can be opened from there.
                if let Some(mime) = image_mime(part) {
                    if let Some(left) = budget.checked_sub(bytes.len()) {
                        budget = left;
                        inner.push_str(&format!(
                            "<img class=\"hylki-inline\" src=\"data:{mime};base64,{}\">",
                            crate::encoding::base64_encode(bytes)
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    if inner.trim().is_empty() {
        let text = match parsed.text_part(0) {
            Some(
                part @ mail_parser::MessagePart {
                    body: PartType::Text(text),
                    ..
                },
            ) => Some(display_text(part, text)),
            _ => parsed.body_text(0),
        };
        return match text {
            Some(text) if !text.trim().is_empty() => wrap_plain(&text),
            _ => wrap_plain("(no readable content)"),
        };
    }
    wrap_fragment(&inner)
}

/// The text of a plain part as the reader shows it: a `format=flowed` part
/// with its soft line breaks joined (#606), any other as it is.
fn display_text<'a>(
    part: &mail_parser::MessagePart<'_>,
    text: &'a str,
) -> std::borrow::Cow<'a, str> {
    match crate::mail::flowed::flowed_params(part) {
        Some(delsp) => crate::mail::flowed::unflow(text, delsp).into(),
        None => text.into(),
    }
}

/// Rewrite `cid:` resource references in `html` to `data:` URIs built from the
/// message's own parts.
///
/// A `cid:` URL names another MIME part of the same message (RFC 2392). WebKit has
/// no handler for the scheme, so an untouched `<img src="cid:…">` renders as its
/// alt text — for Gmail photo mail (`multipart/related`) that means the filename
/// shows where the picture should be. The bytes are already in hand, so swapping in
/// a `data:` URI costs no network access and needs no change to the reader's CSP.
///
/// Only image parts are resolved (the reader can't display anything else inline),
/// each is charged against `budget` once however many times it's referenced, and
/// every Content-ID actually embedded is recorded in `embedded`. References that
/// can't be resolved are left as they are: an unresolved `cid:` is no worse than
/// before, and a rewrite that guessed wrong would be.
pub(in crate::worker) fn inline_cid_images(
    html: &str,
    parts: &[mail_parser::MessagePart<'_>],
    budget: &mut usize,
    embedded: &mut Vec<String>,
) -> String {
    use mail_parser::{MimeHeaders, PartType};

    let bytes = html.as_bytes();
    if !bytes.windows(4).any(|w| w.eq_ignore_ascii_case(b"cid:")) {
        return html.to_string();
    }

    // A Content-ID matches case-insensitively and with any angle brackets stripped;
    // senders aren't consistent about either between the header and the reference.
    let normalize = |id: &str| {
        id.trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .to_ascii_lowercase()
    };
    // cid -> data URI, or `None` for one we've already failed to resolve. Built on
    // first reference so a cid used twice is embedded (and charged) only once.
    let mut resolved: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();

    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    let mut copied = 0;
    while i + 4 <= bytes.len() {
        if !bytes[i..i + 4].eq_ignore_ascii_case(b"cid:") {
            i += 1;
            continue;
        }
        // Only rewrite in resource position (`src="cid:…"`, `src=cid:…`,
        // `url(cid:…)`) so prose that happens to mention a cid is left alone, and
        // never in an `href` — a link is handed to the external browser on click,
        // where a megabyte-long `data:` URL is worse than a dead `cid:` one.
        if i == 0 || !matches!(bytes[i - 1], b'"' | b'\'' | b'=' | b'(') || is_href(bytes, i) {
            i += 1;
            continue;
        }
        // The reference runs to the closing quote/bracket/whitespace. Every
        // terminator is ASCII, so these stay on `char` boundaries.
        let start = i + 4;
        let end = bytes[start..]
            .iter()
            .position(|b| matches!(b, b'"' | b'\'' | b'>' | b')' | b' ' | b'\t' | b'\r' | b'\n'))
            .map_or(html.len(), |n| start + n);
        // `cid:` values are percent-encoded when they contain URI-reserved
        // characters (Gmail's ids embed an `@`, which some senders write as `%40`).
        let key = normalize(&crate::encoding::percent::decode(&html[start..end], false));
        if key.is_empty() {
            i = end.max(i + 1);
            continue;
        }

        let uri = resolved.entry(key.clone()).or_insert_with(|| {
            let part = parts
                .iter()
                .find(|p| p.content_id().is_some_and(|id| normalize(id) == key))?;
            let data = match &part.body {
                PartType::Binary(data) | PartType::InlineBinary(data) => data,
                _ => return None,
            };
            let mime = image_mime(part)?;
            *budget = budget.checked_sub(data.len())?;
            embedded.push(part.content_id()?.to_string());
            Some(format!(
                "data:{mime};base64,{}",
                crate::encoding::base64_encode(data)
            ))
        });

        match uri {
            Some(uri) => {
                out.push_str(&html[copied..i]);
                out.push_str(uri);
                copied = end;
                i = end;
            }
            // Unresolvable: leave the reference verbatim.
            None => i = end.max(i + 1),
        }
    }
    out.push_str(&html[copied..]);
    out
}

/// Whether the value starting at `at` is the target of an `href=` attribute,
/// looking back past the quote and any whitespace around the `=`.
fn is_href(bytes: &[u8], at: usize) -> bool {
    let mut j = at;
    while j > 0
        && matches!(
            bytes[j - 1],
            b'"' | b'\'' | b'=' | b' ' | b'\t' | b'\r' | b'\n'
        )
    {
        j -= 1;
    }
    j >= 4 && bytes[j - 4..j].eq_ignore_ascii_case(b"href")
}

/// The `image/<subtype>` MIME type of a part, if it is an image we can inline.
/// The subtype is validated so it can't break out of the `data:` URI.
pub(super) fn image_mime(part: &mail_parser::MessagePart) -> Option<String> {
    use mail_parser::MimeHeaders;
    let ty = part.content_type()?;
    if !ty.ctype().eq_ignore_ascii_case("image") {
        return None;
    }
    let subtype = ty.subtype()?;
    let safe = subtype
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    safe.then(|| format!("image/{}", subtype.to_ascii_lowercase()))
}

/// HTML-escape plain text and turn bare URLs, email addresses, Matrix IDs and
/// fediverse handles into clickable links. Runs on raw (unescaped) text:
/// non-link spans are escaped as usual; each link becomes an `<a href>` whose
/// scheme is always http(s) or mailto (a bare `www.` host is prefixed with
/// `https://`), so no `javascript:`-style link can be forged. Links open in the
/// external browser via the reader's navigation policy, addresses in a composer.
pub(super) fn linkify(text: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    while let Some((start, end, href)) = next_url(text, i) {
        out.push_str(&escape_html(&text[i..start]));
        out.push_str(&format!(
            "<a href=\"{}\">{}</a>",
            escape_html(&href),
            escape_html(&text[start..end])
        ));
        i = end;
    }
    out.push_str(&escape_html(&text[i..]));
    out
}

/// Find the next link at or after `from`, returning `(start, end, href)`.
fn next_url(text: &str, from: usize) -> Option<(usize, usize, String)> {
    let mut i = from;
    while i < text.len() {
        if !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        if let Some(link) = crate::mail::at_link::at_link(text, from, i) {
            return Some(link);
        }
        let rest = &text[i..];
        let (prefix, add_https) = if rest.starts_with("https://") {
            ("https://", false)
        } else if rest.starts_with("http://") {
            ("http://", false)
        } else if rest.starts_with("www.") {
            ("www.", true)
        } else {
            i += 1;
            continue;
        };
        // Only match at a boundary — the start, after whitespace, or after an
        // opening bracket/quote — so "shttp://", "awww." and "hi@www.x" (an email)
        // aren't linked.
        let boundary = text[..i].chars().next_back().is_none_or(|c| {
            c.is_whitespace() || matches!(c, '(' | '<' | '[' | '{' | '"' | '\'' | '|')
        });
        let end = consume_url(text, i);
        // `www.name@host` is an email address, found at its `@`.
        let www_mail = add_https
            && text[i..end]
                .split('/')
                .next()
                .is_some_and(|h| h.contains('@'));
        // Reject a scheme with nothing (usable) after it.
        if boundary && !www_mail && end > i + prefix.len() {
            let url = &text[i..end];
            let href = if add_https {
                format!("https://{url}")
            } else {
                url.to_string()
            };
            return Some((i, end, href));
        }
        i += prefix.len();
    }
    None
}

/// The end index of the URL that begins at `start`: consume non-terminator chars,
/// then trim trailing sentence punctuation (keeping a `)` that balances a `(`).
fn consume_url(text: &str, start: usize) -> usize {
    let mut end = start;
    for (off, ch) in text[start..].char_indices() {
        if ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '`' | '\'' | '\\') {
            break;
        }
        end = start + off + ch.len_utf8();
    }
    let url = &text[start..end];
    let mut trimmed = url;
    loop {
        match trimmed.chars().last() {
            Some(')') => {
                if trimmed.matches(')').count() > trimmed.matches('(').count() {
                    trimmed = &trimmed[..trimmed.len() - 1];
                } else {
                    break; // balanced — part of the URL (e.g. a Wikipedia link)
                }
            }
            Some('.' | ',' | ';' | ':' | '!' | '?' | ']' | '}') => {
                trimmed = &trimmed[..trimmed.len() - 1];
            }
            _ => break,
        }
    }
    start + trimmed.len()
}

/// Wrap plain text in a minimal, readable HTML document. Colors are left to
/// the `color-scheme` the reader injects, so the message follows the
/// light/dark theme — and no padding is baked in: the reader injects the
/// default inset at render time, so it stays tunable without invalidating
/// every cached body (bodies are stored rendered; see cache SCHEMA_VERSION).
pub(crate) fn wrap_plain(text: &str) -> String {
    let escaped = linkify(text);
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><style>\
         body{{margin:0;font:14px/1.5 system-ui,sans-serif;\
         white-space:pre-wrap;word-wrap:break-word}}\
         </style></head><body>{escaped}</body></html>"
    )
}

/// Wrap composed body fragments (text blocks and inline images) in a document.
/// Padding is the reader's injected default, as in [`wrap_plain`].
fn wrap_fragment(inner: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><style>\
         body{{margin:0;font:14px/1.5 system-ui,sans-serif}}\
         .hylki-plain{{white-space:pre-wrap;word-wrap:break-word}}\
         .hylki-inline{{display:block;max-width:100%;height:auto;\
         margin:12px 0;border-radius:6px}}\
         </style></head><body>{inner}</body></html>"
    )
}
