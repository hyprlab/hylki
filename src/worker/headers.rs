//! Header values for display: RFC 2047 decoding, quoted display names, and
//! the date labels list rows show.

/// Format a unix timestamp the same way [`format_date`] labels mail dates.
pub(super) fn label_from_timestamp(ts: i64) -> String {
    let now = crate::datefmt::now();
    if crate::datefmt::day_key(ts) == crate::datefmt::day_key(now) {
        crate::datefmt::time(ts)
    } else if crate::datefmt::year(ts) == crate::datefmt::year(now) {
        crate::datefmt::day_month(ts)
    } else {
        crate::datefmt::day_month_year(ts)
    }
}

/// Undo an IMAP quoted string's backslash escapes. imap-proto hands ENVELOPE
/// strings back as they were on the wire, so a server that keeps the header's
/// own quotes sends `"\"Sender Name\""` and the backslashes would reach the
/// list (#312).
pub(super) fn unescape_quoted(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut bytes = raw.iter().copied().peekable();
    while let Some(b) = bytes.next() {
        // Only the two escapes a quoted string has: a backslash before
        // anything else came in a literal and is the name's own.
        match bytes.peek() {
            Some(&next @ (b'\\' | b'"')) if b == b'\\' => {
                out.push(next);
                bytes.next();
            }
            _ => out.push(b),
        }
    }
    out
}

/// [`unescape_quoted`] for a string already stored.
pub(crate) fn unescape_quoted_str(s: &str) -> String {
    if s.contains('\\') {
        String::from_utf8_lossy(&unescape_quoted(s.as_bytes())).into_owned()
    } else {
        s.to_string()
    }
}

/// A display name without the quotes its header wrapped it in. RFC 3501 has
/// the server give the phrase alone, but some pass `"Sender Name"` through
/// whole (#312). Called on stored names too, which may still carry the
/// escapes an earlier build kept.
pub(crate) fn clean_display_name(name: &str) -> String {
    let name = unescape_quoted_str(name);
    let trimmed = name.trim();
    trimmed
        .strip_prefix('"')
        .and_then(|n| n.strip_suffix('"'))
        .map(str::trim)
        .unwrap_or(trimmed)
        .to_string()
}

/// The name to keep for the first Reply-To mailbox (#641): its display
/// name, cleaned, or nothing when there is no Reply-To worth keeping or
/// the name only repeats the address.
pub(crate) fn reply_to_display_name(reply_to: &str, name: Option<&str>) -> String {
    if reply_to.is_empty() {
        return String::new();
    }
    let first = reply_to.split(',').next().unwrap_or("").trim();
    let name = clean_display_name(name.unwrap_or(""));
    if name.eq_ignore_ascii_case(first) {
        String::new()
    } else {
        name
    }
}

pub(super) fn bytes_to_string(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Decode a header value that may be RFC 2047 encoded ("=?UTF-8?…?=").
///
/// Some senders (notably Mailchimp) emit a single encoded-word far longer than
/// RFC 2047's 75-character limit — e.g. The Marginalian's newsletter subjects.
/// The decoder aborts on over-long words by default, which would leave the raw
/// `=?utf-8?Q?…?=` gibberish in the UI; we ask it to decode them anyway, as
/// Apple Mail and Thunderbird do.
pub(crate) fn decode_header(raw: &[u8]) -> String {
    use rfc2047_decoder::{Decoder, RecoverStrategy};
    let decode = |bytes: &[u8]| -> Option<String> {
        Decoder::new()
            .too_long_encoded_word_strategy(RecoverStrategy::Decode)
            .decode(bytes)
            .ok()
    };
    let first = decode(raw).unwrap_or_else(|| String::from_utf8_lossy(raw).into_owned());
    if !first.contains("=?") {
        return first;
    }
    // Still carrying raw encoded words — some senders put literal spaces
    // inside them ("…_DPD ?="), which the decoder refuses. Repair and retry.
    repair_spaces_in_encoded_words(&first)
        .and_then(|fixed| decode(fixed.as_bytes()))
        .filter(|s| !s.contains("=?"))
        .unwrap_or(first)
}

/// Map illegal raw spaces inside RFC 2047 encoded words to their legal
/// spelling (`_` for Q encoding, dropped for B), so the word decodes instead
/// of showing as `=?UTF-8?Q?…?=` gibberish. `None` when nothing needed it.
fn repair_spaces_in_encoded_words(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    let mut repaired = false;
    while let Some(start) = rest.find("=?") {
        let (head, tail) = rest.split_at(start);
        out.push_str(head);
        let Some(end) = tail.find("?=") else {
            out.push_str(tail);
            rest = "";
            break;
        };
        let word = &tail[..end + 2];
        let parts: Vec<&str> = word.split('?').collect();
        // "=?charset?enc?text?=" splits to ["=", cs, enc, text…, "="].
        if parts.len() >= 5 && word.contains(' ') {
            let charset = parts[1];
            let enc = parts[2];
            let text = word[..word.len() - 2].splitn(4, '?').nth(3).unwrap_or("");
            let fixed = if enc.eq_ignore_ascii_case("q") {
                text.replace(' ', "_")
            } else {
                text.replace(' ', "")
            };
            if fixed != text {
                repaired = true;
            }
            out.push_str("=?");
            out.push_str(charset);
            out.push('?');
            out.push_str(enc);
            out.push('?');
            out.push_str(&fixed);
            out.push_str("?=");
        } else {
            out.push_str(word);
        }
        rest = &tail[end + 2..];
    }
    out.push_str(rest);
    repaired.then_some(out)
}

/// Compact date label from a unix timestamp (same style as [`format_date`]).
pub(super) fn format_timestamp(ts: i64) -> String {
    if ts <= 0 {
        return String::new();
    }
    label_from_timestamp(ts)
}

/// Parse an RFC 2822 date into a compact label and a sortable unix timestamp.
pub(super) fn format_date(raw: &[u8]) -> (String, i64) {
    let s = String::from_utf8_lossy(raw);
    let s = s.trim();
    let Ok(dt) = chrono::DateTime::parse_from_rfc2822(s) else {
        return (s.to_string(), 0);
    };
    (label_from_timestamp(dt.timestamp()), dt.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_names_lose_the_quotes_a_server_kept() {
        // #312: the name as imap-proto hands it back, escapes and all.
        let name = clean_display_name(&decode_header(&unescape_quoted(br#"\"Sender Name\""#)));
        assert_eq!(name, "Sender Name");
        let inner = clean_display_name(&decode_header(&unescape_quoted(br#"Ann \"Nan\" Lee"#)));
        assert_eq!(inner, r#"Ann "Nan" Lee"#);
        assert_eq!(clean_display_name("Plain Name"), "Plain Name");
        // A backslash that escapes nothing is the name's own.
        assert_eq!(unescape_quoted(br"DOM\user"), br"DOM\user".to_vec());
        assert_eq!(unescape_quoted(br#"a\\\""#), br#"a\""#.to_vec());
        // A name an earlier build stored with the escapes still in it.
        assert_eq!(clean_display_name(r#"\"Sender Name\""#), "Sender Name");
    }

    #[test]
    fn decode_header_repairs_interior_spaces() {
        // DPD's complaints department sends its display name with a literal
        // trailing space INSIDE the encoded word — illegal, and the decoder
        // used to give up and show the raw `=?UTF-8?Q?…?=` (beta 1.18.0b
        // feedback from p-mitana).
        let raw = b"=?UTF-8?Q?Dzia=C5=82_Reklamacji_DPD ?=";
        assert_eq!(decode_header(raw).trim_end(), "Dzia\u{142} Reklamacji DPD");
    }

    #[test]
    fn decode_header_handles_over_long_encoded_word() {
        // The Marginalian (Mailchimp) sends the entire subject as one Q-encoded
        // word ~250 chars long — far past RFC 2047's 75-char limit. Earlier
        // builds aborted and left the raw `=?utf-8?Q?…?=` in the UI; we now
        // decode it as Apple Mail and Thunderbird do.
        let raw = b"=?utf-8?Q?92=2Dyear=2Dold=20artist=20Sheila=20Hicks=20on=20the=20key=20to=20creative=20vitality=2C=20how=20to=20manage=20heartbreak=20like=20Frida=20Kahlo=2C=20the=20elusive=20science=20of=20the=20present=20moment?=";
        assert_eq!(
            decode_header(raw),
            "92-year-old artist Sheila Hicks on the key to creative vitality, \
             how to manage heartbreak like Frida Kahlo, the elusive science of \
             the present moment"
        );
    }

    #[test]
    fn decode_header_leaves_plain_text_untouched() {
        assert_eq!(
            decode_header(b"Just a normal subject"),
            "Just a normal subject"
        );
    }

    #[test]
    fn dates_parse_or_fall_back_to_the_text() {
        assert_eq!(
            format_date(b" Tue, 1 Jul 2003 10:52:37 +0200 ").1,
            1057049557
        );
        assert_eq!(format_date(b" not a date "), ("not a date".to_string(), 0));
        assert_eq!(format_timestamp(0), "");
        assert_eq!(format_timestamp(-5), "");
        assert_eq!(
            format_timestamp(1057049557),
            label_from_timestamp(1057049557)
        );
    }
}
