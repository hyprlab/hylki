//! The list preview: a few lines of text out of the first bytes of a body
//! part, whatever its charset, transfer encoding or multipart nesting.

/// Whether a (lowercased) Content-Type can hold text to preview, or contain
/// a part that does: text itself, a multipart, or an enclosed message. An
/// image, an archive or a document is a file, whatever its bytes decode to.
pub(super) fn text_like(ctype: &str) -> bool {
    ctype.starts_with("text/") || ctype.starts_with("multipart/") || ctype.starts_with("message/")
}

/// Whether decoded body bytes are a file rather than text: control
/// characters that no text carries (a zip opens `PK\x03\x04`, a PDF's
/// streams are full of them). A row showing such bytes as letters is worse
/// than a row showing nothing.
fn looks_binary(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(512)];
    if sample.is_empty() {
        return false;
    }
    let control = sample
        .iter()
        .filter(|&&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0c) || b == 0x7f)
        .count();
    control > 0 && (sample.contains(&0) || control * 20 > sample.len())
}

/// Longest preview stored per message.
const PREVIEW_CHARS: usize = 240;

/// Turn the first bytes of a message's body part into a one-line snippet for the
/// message list.
///
/// What arrives is raw MIME, cut off mid-stream: still transfer-encoded, perhaps
/// HTML, and (being a prefix) possibly ending mid-character or mid-tag. The
/// transfer encoding is declared in headers this fetch doesn't include, so it is
/// inferred from the bytes themselves — wrongly guessing plain text for base64
/// would show gibberish in the list, which is worse than showing nothing. The
/// charset is the one the part's MIME headers declared, when the caller has
/// them.
pub(super) fn preview_from_part(
    bytes: &[u8],
    charset: Option<&str>,
    encoding: Option<&str>,
) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    // Section 1 can be a whole nested multipart rather than a body: descend to the
    // text inside it, which also carries the encoding and charset in its own
    // headers instead of leaving them to be guessed.
    if let Some(inner) = text_in_multipart(bytes, 0) {
        return finish_preview(inner);
    }
    // What the part says it is, when it says: the headers ride along with the
    // preview fetch, and a declaration beats any amount of inspection.
    let declared = encoding.map(|e| e.trim().to_ascii_lowercase());
    if let Some(e) = declared.as_deref().filter(|e| {
        e.starts_with("base64")
            || e.starts_with("quoted-printable")
            || matches!(*e, "7bit" | "8bit" | "binary")
    }) {
        let text = decode_mime_body(bytes, e, charset);
        if looks_binary(text.as_bytes()) {
            return String::new();
        }
        return finish_preview(text);
    }
    let decoded = if looks_like_base64(bytes) {
        decode_base64_prefix(bytes)
    } else if bytes.contains(&b'=') {
        decode_quoted_printable(bytes)
    } else {
        bytes.to_vec()
    };
    // Whatever the headers said or failed to say, a file's bytes are not a
    // preview (#241).
    if looks_binary(&decoded) {
        return String::new();
    }

    finish_preview(decode_text(&decoded, charset))
}

/// Text from a body's bytes, in the charset its headers declared (#159).
///
/// Everything here used to be read as UTF-8, so a bank's `iso-8859-2` mail
/// showed a replacement character for every accented letter in the list while
/// the reader — which hands the whole message to mail-parser — showed it fine.
/// mail-parser's charset table is used here too; UTF-8 and ASCII are read
/// directly.
fn decode_text(bytes: &[u8], charset: Option<&str>) -> String {
    let charset = charset
        .map(|c| c.trim().trim_matches('"').to_ascii_lowercase())
        .filter(|c| !c.is_empty());
    let declared_unicode = matches!(
        charset.as_deref(),
        Some("utf-8" | "utf8" | "us-ascii" | "ascii")
    );
    if let Some(cs) = charset.as_deref().filter(|_| !declared_unicode) {
        if let Some(decode) = mail_parser::decoders::charsets::map::charset_decoder(cs.as_bytes()) {
            return decode(bytes);
        }
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        // A fetch of the first N bytes can end mid-character: keep the whole
        // characters and drop the fragment rather than show it as noise.
        Err(e) if e.error_len().is_none() => {
            String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned()
        }
        // Declared UTF-8 that isn't: replacement characters mark the damage.
        Err(_) if declared_unicode => String::from_utf8_lossy(bytes).into_owned(),
        // 8-bit text with no (usable) charset: read it as Windows-1252, as
        // browsers and mail clients do, which at least shows Western European
        // accents rather than a row of replacement characters.
        Err(_) => mail_parser::decoders::charsets::single_byte::decoder_cp1252(bytes),
    }
}

/// The `charset=` parameter of a (lowercased) Content-Type value.
pub(super) fn charset_param(ctype: &str) -> Option<String> {
    ctype
        .split(';')
        .skip(1)
        .find_map(|p| {
            let (k, v) = p.split_once('=')?;
            (k.trim() == "charset").then(|| v.trim().trim_matches('"').to_string())
        })
        .filter(|c| !c.is_empty())
}

/// Decode as much of a (possibly truncated) base64 body as forms whole groups.
fn decode_base64_prefix(body: &[u8]) -> Vec<u8> {
    let clean: String = body
        .iter()
        .filter(|b| !b.is_ascii_whitespace())
        .map(|&b| b as char)
        .collect();
    // Only whole 4-character groups can be decoded; the tail of a truncated
    // fetch is dropped rather than turned into noise.
    let usable = &clean[..clean.len() - clean.len() % 4];
    crate::encoding::base64_decode(usable).unwrap_or_default()
}

/// Tidy decoded body text into the single line the list shows.
pub(super) fn finish_preview(decoded: String) -> String {
    // An HTML part becomes readable text; a plain part passes through.
    let plain = if decoded.contains('<') && decoded.contains('>') {
        crate::mail::quote::message_text(&decoded)
    } else {
        decoded
    };

    // Quoted replies say nothing about *this* message, so skip those lines.
    let body: String = plain
        .lines()
        .filter(|line| !line.trim_start().starts_with('>'))
        .collect::<Vec<_>>()
        .join("\n");
    let body = strip_link_noise(&body);
    // Words only: a plain part can be written in entities and an HTML part
    // can be escaped twice, and the preheader padding either one carries
    // is invisible characters a collapse alone would keep (#683).
    let collapsed = crate::mail::preview_text::one_line(&body);
    if let Some(p) = pgp_preview(&collapsed) {
        return p;
    }
    collapsed.chars().take(PREVIEW_CHARS).collect()
}

/// What the list says for an OpenPGP-encrypted message (#133), whose first
/// text is otherwise the PGP/MIME version stub ("Version: 1") or the
/// armour itself — neither of which says anything a reader wants to see.
pub(super) fn pgp_preview(collapsed: &str) -> Option<String> {
    let t = collapsed.trim();
    let stub = t.to_ascii_lowercase();
    let stub = stub.trim_start_matches("version:").trim();
    if stub == "1" || t.starts_with("-----BEGIN PGP MESSAGE-----") {
        return Some(crate::models::ENCRYPTED_PREVIEW.to_string());
    }
    None
}

/// Drop the link furniture that plain-text alternatives are built from, so the
/// preview starts where the message does.
///
/// A converter renders a link as `text ( url )`, so a mail whose first element is
/// a linked logo — every marketing template — begins with a bare tracking URL.
/// Hylki showed that URL as the preview where Apple Mail shows the greeting. The
/// parenthesised URLs go, and any URL-only lines left at the front go with them;
/// if that is the whole message, it stays as it was, since a link is better than
/// an empty row.
fn strip_link_noise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        let after = rest[open + 1..].trim_start();
        let is_url = after.starts_with("http://") || after.starts_with("https://");
        let close = rest[open..].find(')').map(|i| open + i);
        match (is_url, close) {
            // `( https://… )` — a rendered link, and nothing else in the brackets.
            (true, Some(close)) if !rest[open..close].trim_end_matches(')').contains('\n') => {
                out.push_str(&rest[..open]);
                rest = &rest[close + 1..];
            }
            _ => {
                out.push_str(&rest[..=open]);
                rest = &rest[open + 1..];
            }
        }
    }
    out.push_str(rest);

    // Whatever is left at the top that is only a link.
    let trimmed: String = out
        .lines()
        .skip_while(|line| {
            let l = line.trim();
            l.is_empty() || (is_url(l) && l.split_whitespace().count() == 1)
        })
        .collect::<Vec<_>>()
        .join("\n");

    if trimmed.trim().is_empty() {
        text.to_string()
    } else {
        trimmed
    }
}

/// Whether a word is a bare URL.
fn is_url(word: &str) -> bool {
    word.starts_with("http://") || word.starts_with("https://")
}

/// The text of the first readable part of a MIME multipart, or `None` if this
/// isn't one.
///
/// `text/plain` is preferred and `text/html` taken only if there is no plain
/// alternative, which matches what the reader shows. Nesting is followed a couple
/// of levels deep — enough for `mixed(alternative(…))` — and no further, since a
/// preview is not worth unbounded recursion through a hostile message.
pub(super) fn text_in_multipart(part: &[u8], depth: usize) -> Option<String> {
    if depth > 3 {
        return None;
    }
    // The boundary opens the part — or follows a short preamble ("This is a
    // multi-part message in MIME format.") when what arrived is a whole body
    // rather than a part. A boundary delimiter, not a message that merely
    // opens with a dash: two dashes, more after them, no spaces.
    let boundary = part
        .split(|&b| b == b'\n')
        .take(10)
        .map(|line| {
            let end = line
                .iter()
                .rposition(|b| !b.is_ascii_whitespace())
                .map_or(0, |i| i + 1);
            &line[..end]
        })
        .find(|line| line.starts_with(b"--") && line.len() >= 3 && !line.contains(&b' '))?;
    let mut html: Option<String> = None;
    for chunk in split_on(part, boundary).skip(1) {
        let start = chunk
            .iter()
            .position(|b| !matches!(b, b'\r' | b'\n'))
            .unwrap_or(chunk.len());
        let chunk = &chunk[start..];
        if chunk.starts_with(b"--") {
            break; // closing delimiter: no more parts
        }
        let Some((headers, body)) = split_mime_headers(chunk) else {
            continue;
        };
        let headers = String::from_utf8_lossy(headers);
        let ctype = mime_header(&headers, "content-type").unwrap_or_default();
        let encoding = mime_header(&headers, "content-transfer-encoding").unwrap_or_default();
        let charset = charset_param(&ctype);
        if ctype.starts_with("multipart/") {
            if let Some(inner) = text_in_multipart(body, depth + 1) {
                return Some(inner);
            }
        } else if ctype.starts_with("text/plain") {
            return Some(decode_mime_body(body, &encoding, charset.as_deref()));
        } else if ctype.starts_with("text/html") && html.is_none() {
            html = Some(decode_mime_body(body, &encoding, charset.as_deref()));
        }
    }
    html
}

/// The pieces of `hay` between occurrences of `sep`, like `str::split`.
fn split_on<'a>(hay: &'a [u8], sep: &'a [u8]) -> impl Iterator<Item = &'a [u8]> {
    let mut rest = Some(hay);
    std::iter::from_fn(move || {
        let cur = rest?;
        match cur.windows(sep.len()).position(|w| w == sep) {
            Some(i) => {
                rest = Some(&cur[i + sep.len()..]);
                Some(&cur[..i])
            }
            None => {
                rest = None;
                Some(cur)
            }
        }
    })
}

/// Split a MIME part into its header block and its body.
fn split_mime_headers(part: &[u8]) -> Option<(&[u8], &[u8])> {
    if let Some(i) = part.windows(4).position(|w| w == b"\r\n\r\n") {
        return Some((&part[..i], &part[i + 4..]));
    }
    part.windows(2)
        .position(|w| w == b"\n\n")
        .map(|i| (&part[..i], &part[i + 2..]))
}

/// One header's value, lowercased, from a MIME part's header block.
pub(super) fn mime_header(headers: &str, name: &str) -> Option<String> {
    // Unfold first: a parameter on a continuation line ("Content-Type:
    // text/html;\r\n\tcharset=iso-8859-2") belongs to the header above it.
    let unfolded = headers
        .replace("\r\n", "\n")
        .replace("\n ", " ")
        .replace("\n\t", " ");
    unfolded
        .lines()
        .find(|l| {
            // Compare as bytes: a header block reaches us through
            // `from_utf8_lossy`, so a line can carry a multibyte character
            // that `name.len()` falls inside — slicing there would panic and
            // take the account's mail thread with it. A name is ASCII, so a
            // byte-wise match also proves `name.len()` is a char boundary,
            // which is what makes the slice below safe.
            l.len() > name.len()
                && l.as_bytes()[..name.len()].eq_ignore_ascii_case(name.as_bytes())
                && l[name.len()..].trim_start().starts_with(':')
        })
        .map(|l| {
            l[name.len()..].trim_start()[1..]
                .trim()
                .to_ascii_lowercase()
        })
}

/// Decode a part's body according to the encoding and charset it declares.
fn decode_mime_body(body: &[u8], encoding: &str, charset: Option<&str>) -> String {
    let bytes = if encoding.starts_with("base64") {
        decode_base64_prefix(body)
    } else if encoding.starts_with("quoted-printable") {
        decode_quoted_printable(body)
    } else {
        body.to_vec()
    };
    decode_text(&bytes, charset)
}

/// Whether a chunk looks like base64 rather than text: base64's alphabet only,
/// and long enough that a short plain word can't be mistaken for it.
///
/// An `=` is padding, and padding only ever closes a base64 stream — so one
/// anywhere else says this is not base64 however well the rest fits. That is
/// what quoted-printable looks like from here: `=E2=80=87=CD=8F` repeated is
/// nothing but letters, digits and `=`, and reading a marketing preheader's
/// worth of it as base64 turned a Cloudflare newsletter's preview into a row
/// of replacement characters. A truncated fetch of real base64 simply has no
/// padding in it at all.
fn looks_like_base64(bytes: &[u8]) -> bool {
    let mut significant = 0;
    let mut padding = 0;
    for &c in bytes {
        if c.is_ascii_whitespace() {
            continue;
        }
        if !(c.is_ascii_alphanumeric() || c == b'+' || c == b'/' || c == b'=') {
            return false;
        }
        if c == b'=' {
            padding += 1;
        } else if padding > 0 {
            // Alphabet after padding: the `=` was not closing anything.
            return false;
        }
        significant += 1;
    }
    significant >= 40 && padding <= 2
}

/// Decode quoted-printable to the bytes it stands for, leaving anything
/// malformed as written (a truncated fetch can end mid-escape).
fn decode_quoted_printable(src: &[u8]) -> Vec<u8> {
    // The fetch behind a preview is a slice of a part, so it ends wherever the
    // byte count ran out — routinely in the middle of an escape. Left alone
    // that stub reaches the reader as literal text ("… =E"), so drop it: a
    // trailing "=" (which in quoted-printable is a soft line break anyway) and
    // a trailing "=" plus one hex digit. Anything else is malformed rather
    // than cut short, and is left as written.
    let src = match src.len().checked_sub(2).map(|at| &src[at..]) {
        Some(&[b'=', h]) if h.is_ascii_hexdigit() => &src[..src.len() - 2],
        _ if src.last() == Some(&b'=') => &src[..src.len() - 1],
        _ => src,
    };
    let mut buf = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'=' {
            // "=\r\n" is a soft line break: the line continues.
            if src.get(i + 1) == Some(&b'\r') && src.get(i + 2) == Some(&b'\n') {
                i += 3;
                continue;
            }
            if src.get(i + 1) == Some(&b'\n') {
                i += 2;
                continue;
            }
            let hex = src.get(i + 1..i + 3).and_then(|h| {
                std::str::from_utf8(h)
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            });
            match hex {
                Some(b) => {
                    buf.push(b);
                    i += 3;
                    continue;
                }
                None => {
                    buf.push(b'=');
                    i += 1;
                    continue;
                }
            }
        }
        buf.push(src[i]);
        i += 1;
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_skips_the_link_a_marketing_mail_opens_with() {
        // Cloudways' text/plain alternative, verbatim in shape: the header logo's
        // link renders as a bare URL above the greeting, and that URL was the
        // whole preview.
        let part = concat!(
            "--751d69df691580924654d5924db69f0ae9507550b8b16fd8b2d83daf1d3f\r\n",
            "Content-Transfer-Encoding: quoted-printable\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "( https://www.cloudways.com/en?utm_campaign=3DAPI+Keys+being+replaced+by+Ac=\r\n",
            "cess+Tokens&utm_medium=3Demail_action )\r\n",
            "\r\n",
            "Hi Camp crystal clear,\r\n",
            "\r\n",
            "We have a quick update on how you connect your AI agent to Cloudways.\r\n",
            "--751d69df691580924654d5924db69f0ae9507550b8b16fd8b2d83daf1d3f--\r\n",
        );
        let preview = preview_from_part(part.as_bytes(), None, None);
        assert!(preview.starts_with("Hi Camp crystal clear,"), "{preview}");
        assert!(!preview.contains("utm_campaign"), "{preview}");
    }

    #[test]
    fn a_zip_as_the_whole_body_yields_no_preview() {
        // #241: a DMARC report is a single application/zip part. Base64 or
        // not, its bytes are a file, not a snippet.
        let zip = b"PK\x03\x04\x14\x00\x00\x00\x08\x00\x8a\x1eGoogle report\x00\x00\x12\x03\x00PK\x01\x02";
        let b64 = crate::encoding::base64_encode(zip);
        assert_eq!(preview_from_part(b64.as_bytes(), None, Some("base64")), "");
        assert_eq!(preview_from_part(b64.as_bytes(), None, None), "");
        assert_eq!(preview_from_part(zip, None, Some("binary")), "");
        // Ordinary text with a tab and line breaks is not "binary".
        assert_eq!(
            preview_from_part(b"Hello\tthere\r\nSecond line", None, None),
            "Hello there Second line"
        );
        assert!(!text_like("application/zip"));
        assert!(text_like("multipart/mixed; boundary=x"));
    }

    #[test]
    fn preview_drops_rendered_links_but_keeps_their_text() {
        assert_eq!(
            preview_from_part(
                b"Generate your Access Token ( https://example.com/api?a=1 )Got questions?",
                None,
                None
            ),
            "Generate your Access Token Got questions?"
        );
        // Brackets that are not a link are left exactly as written.
        assert_eq!(
            preview_from_part(b"Lunch (the usual place) at noon", None, None),
            "Lunch (the usual place) at noon"
        );
        // A message that is nothing but a link still shows it: better than a
        // blank row.
        assert_eq!(
            preview_from_part(b"https://example.com/only", None, None),
            "https://example.com/only"
        );
    }

    #[test]
    fn preview_descends_into_a_nested_multipart() {
        // ProtonMail sends mixed(alternative(plain, html), attachment), so
        // `BODY[1]` is the whole alternative container rather than any text — and
        // the preview showed its boundary line ("--b2=_cipkIEq1…") instead of the
        // message. The bytes below are that fetch, verbatim in shape.
        let part = concat!(
            "--b2=_cipkIEq1WkCIMIbpGDVyYc52x8YElrqa8uRU7GKJ8\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "SGVsbG8sIEkgcmVjZW50bHkgaW5zdGFsbGVkIFZpcmVvIGFmdGVyIHJlYWRpbmcgYWJvdXQgaXQg\r\n",
            "b24gdGhlIG9tZyF1YnVudHUgd2Vic2l0ZS4=\r\n",
            "\r\n",
            "--b2=_cipkIEq1WkCIMIbpGDVyYc52x8YElrqa8uRU7GKJ8\r\n",
            "Content-Type: text/html; charset=utf-8\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "PGRpdj5IZWxsbzwvZGl2Pg==\r\n",
            "--b2=_cipkIEq1WkCIMIbpGDVyYc52x8YElrqa8uRU7GKJ8--\r\n",
        );
        assert_eq!(
            preview_from_part(part.as_bytes(), None, None),
            "Hello, I recently installed Vireo after reading about it on the omg!ubuntu website."
        );
        // Nothing of the MIME machinery reaches the list.
        assert!(!preview_from_part(part.as_bytes(), None, None).contains("--b2="));
    }

    #[test]
    fn preview_prefers_plain_text_but_settles_for_html() {
        let html_only = concat!(
            "--x\r\n",
            "Content-Type: text/html; charset=utf-8\r\n",
            "\r\n",
            "<div>Only markup here</div>\r\n",
            "--x--\r\n",
        );
        assert_eq!(
            preview_from_part(html_only.as_bytes(), None, None),
            "Only markup here"
        );
        // Two levels of nesting — mixed(alternative(...)) — are followed.
        let nested = concat!(
            "--outer\r\n",
            "Content-Type: multipart/alternative; boundary=\"inner\"\r\n",
            "\r\n",
            "--inner\r\n",
            "Content-Type: text/plain\r\n",
            "\r\n",
            "Buried but readable\r\n",
            "--inner--\r\n",
            "--outer--\r\n",
        );
        assert_eq!(
            preview_from_part(nested.as_bytes(), None, None),
            "Buried but readable"
        );
    }

    #[test]
    fn a_message_that_merely_starts_with_dashes_is_not_a_multipart() {
        // A signature separator, or a line of dashes, must still read as text.
        assert_eq!(
            preview_from_part(b"-- \r\nRegards,\r\nSteve", None, None),
            "-- Regards, Steve"
        );
        assert_eq!(
            preview_from_part(b"--\r\nsigned off", None, None),
            "-- signed off"
        );
    }

    /// A PGP/MIME message's first part is the "Version: 1" stub, and an
    /// inline one begins with the armour; the list says what it is instead.
    #[test]
    fn preview_names_an_encrypted_message() {
        let marker = crate::models::ENCRYPTED_PREVIEW;
        assert_eq!(preview_from_part(b"Version: 1\r\n", None, None), marker);
        assert_eq!(preview_from_part(b"version: 1", None, None), marker);
        assert_eq!(
            preview_from_part(
                b"-----BEGIN PGP MESSAGE-----\r\n\r\nhQEMA3\r\n-----END PGP MESSAGE-----\r\n",
                None,
                None
            ),
            marker
        );
        assert!(crate::models::preview_is_encrypted(marker));
        assert_eq!(crate::models::preview_display(marker), "Encrypted message");
        assert_eq!(crate::models::preview_display("Hello"), "Hello");
        assert_eq!(pgp_preview("Version 1 of the plan is attached"), None);
        assert_eq!(preview_from_part(b"Hello there", None, None), "Hello there");
    }

    #[test]
    fn preview_reads_plain_text() {
        let text = b"Here are the figures we discussed.\r\n\r\nLet me know if the Q3 line looks wrong.\r\n";
        assert_eq!(
            preview_from_part(text, None, None),
            "Here are the figures we discussed. Let me know if the Q3 line looks wrong."
        );
        assert_eq!(preview_from_part(b"", None, None), "");
    }

    #[test]
    fn a_quoted_printable_preheader_is_not_mistaken_for_base64() {
        // Cloudflare's newsletter opens its text/plain part with a long run of
        // invisible padding — U+2007 U+034F over and over — so that the preview
        // a mail client shows is the sender's chosen line rather than whatever
        // the message starts with. Quoted-printable, every byte of it spelled
        // with letters, digits and `=`: base64's alphabet exactly. Guessing
        // read it as base64 and produced a row of replacement characters.
        let mut part = b"AI Bot Controls ".to_vec();
        for _ in 0..40 {
            part.extend_from_slice(b"=E2=80=87=CD=8F ");
        }
        part.extend_from_slice(b"Updates to managing AI crawlers on your account");

        assert!(!looks_like_base64(&part), "padding runs are not base64");

        // Told what it is, which is what the part's own MIME headers say.
        let told = preview_from_part(&part, Some("UTF-8"), Some("quoted-printable"));
        assert!(
            !told.contains('\u{fffd}'),
            "no replacement characters: {told}"
        );
        assert!(told.starts_with("AI Bot Controls"), "{told}");
        assert!(told.contains("Updates to managing AI crawlers"), "{told}");

        // And the same left to the guess, now that the guess knows better.
        let guessed = preview_from_part(&part, Some("UTF-8"), None);
        assert_eq!(guessed, told);

        // Real base64 still reads as base64, truncated (no padding) or whole.
        let whole = b"VGhlIHF1aWNrIGJyb3duIGZveCBqdW1wcyBvdmVyIHRoZSBsYXp5IGRvZywgdHdpY2Uu";
        assert!(looks_like_base64(whole));
        assert!(looks_like_base64(
            b"VGhlIHF1aWNrIGJyb3duIGZveCBqdW1wcyBvdmVyIHRoZSBsYXp5IGRvZz0="
        ));

        // The slice a preview reads ends on a byte count, routinely mid-escape.
        // The stub is dropped rather than shown as text.
        let cut = b"AI Bot Controls =E2=80=87=CD=8F =E";
        let out = preview_from_part(cut, Some("UTF-8"), Some("quoted-printable"));
        assert!(
            !out.contains('='),
            "no half-decoded escape left behind: {out:?}"
        );
        assert_eq!(out.trim(), "AI Bot Controls");
        let cut = b"AI Bot Controls =E2=80=87=CD=8F =";
        let out = preview_from_part(cut, Some("UTF-8"), Some("quoted-printable"));
        assert!(!out.contains('='), "nor a bare one: {out:?}");
    }

    #[test]
    fn a_preview_of_nothing_but_preheader_padding_reaches_for_the_real_text() {
        // What the deeper BODY[TEXT] repair read sees: the whole multipart,
        // each part carrying its own headers. The text is thousands of bytes
        // past the padding, which is the point of the padding.
        let mut plain = b"AI Bot Controls ".to_vec();
        for _ in 0..250 {
            plain.extend_from_slice(b"=E2=80=87=CD=8F ");
        }
        plain.extend_from_slice(b"Updates to managing AI crawlers on your account.");
        let mut body = Vec::new();
        body.extend_from_slice(b"--BOUND\r\n");
        body.extend_from_slice(b"Content-Type: text/plain; charset=UTF-8\r\n");
        body.extend_from_slice(b"Content-Transfer-Encoding: quoted-printable\r\n\r\n");
        body.extend_from_slice(&plain);
        body.extend_from_slice(b"\r\n--BOUND--\r\n");

        let preview = preview_from_part(&body, None, None);
        assert!(
            preview.contains("Updates to managing AI crawlers"),
            "the padding is stepped over, not shown: {preview:?}",
        );
        assert!(!preview.contains('\u{fffd}'), "{preview:?}");
        assert!(
            !preview.contains('\u{034f}'),
            "joiners are dropped: {preview:?}"
        );
    }

    #[test]
    fn preview_decodes_the_transfer_encoding_it_was_not_told_about() {
        // The fetch asks for body bytes only, so the encoding has to be inferred.
        // Quoted-printable, including a soft line break mid-sentence:
        let qp = b"Caf=C3=A9 meeting at 3pm =\r\nsharp, bring the numbers";
        assert_eq!(
            preview_from_part(qp, None, None),
            "Café meeting at 3pm sharp, bring the numbers"
        );

        // Base64 — shown raw this would be gibberish in the list.
        let encoded = crate::encoding::base64_encode(
            b"Base64 bodies are common from newsletters and phones alike.",
        );
        assert_eq!(
            preview_from_part(encoded.as_bytes(), None, None),
            "Base64 bodies are common from newsletters and phones alike."
        );
    }

    #[test]
    fn preview_survives_being_cut_off_mid_stream() {
        // 2KB of a base64 part almost never lands on a group boundary; the
        // incomplete tail is dropped rather than decoded into noise.
        let full = crate::encoding::base64_encode(
            &b"The quick brown fox jumps over the lazy dog. ".repeat(4),
        );
        let truncated = &full[..full.len() - 3];
        let preview = preview_from_part(truncated.as_bytes(), None, None);
        assert!(preview.starts_with("The quick brown fox"), "{preview}");
        // An escape cut in half is dropped, not shown: the slice ends on a
        // byte count, so its last escape is routinely half there. A lone
        // trailing "=" is a soft line break in quoted-printable and goes the
        // same way.
        assert_eq!(preview_from_part(b"Total: 50=", None, None), "Total: 50");
        assert_eq!(preview_from_part(b"Total: 50=4", None, None), "Total: 50");
        // Malformed rather than cut short: "=Z" is no escape at all, and
        // whatever it is, it isn't ours to remove.
        assert_eq!(preview_from_part(b"Total: 50=Z", None, None), "Total: 50=Z");
    }

    #[test]
    fn preview_shows_words_not_the_markup_a_part_is_written_in() {
        // A plain-text alternative that kept its HTML entities (#683).
        let mut plain = b"Iberia ".to_vec();
        for _ in 0..12 {
            plain.extend_from_slice(b"&zwnj;&nbsp;");
        }
        plain.extend_from_slice(b"\r\n<p>Tomas, prepara tu viaje a Madrid</p>");
        let mut body = b"--B\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n".to_vec();
        body.extend_from_slice(&plain);
        body.extend_from_slice(b"\r\n--B--\r\n");
        assert_eq!(
            preview_from_part(&body, None, None),
            "Iberia Tomas, prepara tu viaje a Madrid"
        );
        // An HTML part escaped twice spells its tags out as text.
        let html = b"<div>Sale &amp;lt;b&amp;gt;today&amp;lt;/b&amp;gt; &amp;zwnj;only</div>";
        assert_eq!(preview_from_part(html, None, None), "Sale today only");
    }

    #[test]
    fn preview_reads_html_and_skips_quoted_replies() {
        let html = b"<html><body><p>Meeting moved to <b>Tuesday</b>.</p></body></html>";
        assert_eq!(
            preview_from_part(html, None, None),
            "Meeting moved to Tuesday."
        );
        let reply = b"Sounds good to me.\r\n\r\n> On Monday, Ada wrote:\r\n> the original text\r\n";
        assert_eq!(preview_from_part(reply, None, None), "Sounds good to me.");
    }

    #[test]
    fn preview_is_capped() {
        let long = "word ".repeat(200);
        assert_eq!(
            preview_from_part(long.as_bytes(), None, None)
                .chars()
                .count(),
            PREVIEW_CHARS
        );
    }

    #[test]
    fn short_text_is_not_mistaken_for_base64() {
        // "Meeting" is all base64 characters, but far too short to be a body.
        assert_eq!(preview_from_part(b"Meeting", None, None), "Meeting");
    }

    #[test]
    fn preview_reads_the_charset_the_part_declares() {
        // #159: a Hungarian bank's mail, quoted-printable in iso-8859-2. Read as
        // UTF-8, every accented letter was a replacement character in the list
        // while the reader showed the message fine.
        let qp = b"Tisztelt =DCgyfel=FCnk! K=E9rj=FCk, =F5rizze meg.";
        assert_eq!(
            preview_from_part(qp, Some("iso-8859-2"), None),
            "Tisztelt Ügyfelünk! Kérjük, őrizze meg."
        );
        // The 8-bit bytes as they are, no transfer encoding; label case and
        // quotes as servers send them.
        assert_eq!(
            preview_from_part(b"\xD5rizze meg", Some("\"ISO-8859-2\""), None),
            "Őrizze meg"
        );
        assert_eq!(
            preview_from_part(b"\xF5rizze", Some("windows-1250"), None),
            "őrizze"
        );
        // Base64 is decoded to bytes first, then read in the charset.
        let b64 =
            crate::encoding::base64_encode(b"Caf\xE9 meeting at three, bring the numbers please.");
        assert_eq!(
            preview_from_part(b64.as_bytes(), Some("iso-8859-1"), None),
            "Café meeting at three, bring the numbers please."
        );
        // UTF-8 still reads as UTF-8 whatever it is called.
        assert_eq!(
            preview_from_part("Café".as_bytes(), Some("UTF-8"), None),
            "Café"
        );
        assert_eq!(
            preview_from_part("Café".as_bytes(), Some("utf8"), None),
            "Café"
        );
    }

    #[test]
    fn preview_parts_of_a_multipart_carry_their_own_charset() {
        let nested = b"--b1\r\nContent-Type: text/plain; charset=\"iso-8859-2\"\r\n\
            Content-Transfer-Encoding: quoted-printable\r\n\r\n=D5rizze meg\r\n--b1--\r\n";
        assert_eq!(preview_from_part(nested, None, None), "Őrizze meg");
        // A charset on a folded continuation line still counts.
        let folded = b"--b1\r\nContent-Type: text/html;\r\n\tcharset=iso-8859-2\r\n\r\n\
            <p>\xD5rizze meg</p>\r\n--b1--\r\n";
        assert_eq!(preview_from_part(folded, None, None), "Őrizze meg");
    }

    #[test]
    fn preview_finds_the_parts_behind_a_mime_preamble() {
        // A whole body (the BODY[TEXT] retry) can open with the preamble
        // rather than the boundary.
        let body = b"This is a multi-part message in MIME format.\r\n\r\n--b1\r\n\
            Content-Type: image/png\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--b1\r\n\
            Content-Type: text/plain\r\n\r\nThe photo from Sunday\r\n--b1--\r\n";
        assert_eq!(preview_from_part(body, None, None), "The photo from Sunday");
        // But plain text that merely mentions a dashed line is still text.
        assert_eq!(
            preview_from_part(b"Hello\r\n--foo\r\nbar", None, None),
            "Hello --foo bar"
        );
    }

    #[test]
    fn preview_copes_with_bytes_that_are_not_utf8() {
        // Undeclared 8-bit text reads as Windows-1252, the way browsers do.
        assert_eq!(
            preview_from_part(b"Caf\xE9 au lait", None, None),
            "Café au lait"
        );
        // A slice that ends mid-character drops the fragment, not the row.
        let cut = &"Café".as_bytes()[..4];
        assert_eq!(preview_from_part(cut, None, None), "Caf");
        assert_eq!(preview_from_part(cut, Some("utf-8"), None), "Caf");
        // Declared UTF-8 that isn't: the damage shows as replacement characters.
        assert_eq!(
            preview_from_part(b"a\xFFb", Some("utf-8"), None),
            "a\u{fffd}b"
        );
    }

    #[test]
    fn charset_param_is_read_from_a_content_type() {
        assert_eq!(
            charset_param("text/html; charset=\"iso-8859-2\""),
            Some("iso-8859-2".into())
        );
        assert_eq!(
            charset_param("text/plain; format=flowed; charset=utf-8"),
            Some("utf-8".into())
        );
        assert_eq!(charset_param("multipart/alternative; boundary=b1"), None);
        assert_eq!(
            mime_header(
                "Content-Type: text/plain;\r\n\tcharset=us-ascii\r\nX: y",
                "content-type"
            )
            .as_deref()
            .and_then(charset_param),
            Some("us-ascii".into())
        );
    }

    /// A part header carrying a non-ASCII character where a header name's
    /// length falls mid-character used to panic the account's mail thread, so
    /// its mail never arrived and the spinner turned for good.
    #[test]
    fn a_header_line_with_a_multibyte_character_is_not_sliced_apart() {
        // "content-transfer-encoding" is 25 bytes, and byte 25 of this line
        // sits inside the two-byte character that starts at byte 24.
        let headers = "Content-Description: abc\u{f3}de\r\nContent-Transfer-Encoding: base64\r\n";
        assert_eq!(
            mime_header(headers, "content-transfer-encoding").as_deref(),
            Some("base64")
        );
        assert_eq!(mime_header(headers, "content-type"), None);
    }

    /// The same line reached through the preview path that reads it: a part of
    /// a multipart, headers decoded leniently from raw bytes.
    #[test]
    fn a_preview_survives_a_multibyte_part_header() {
        let part = concat!(
            "--b1\r\n",
            "Content-Description: abc\u{f3}de\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n\r\n",
            "Hello.\r\n",
            "--b1--\r\n",
        );
        assert_eq!(
            preview_from_part(part.as_bytes(), None, None).trim(),
            "Hello."
        );
    }
}
