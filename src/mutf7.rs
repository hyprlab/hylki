//! IMAP's modified UTF-7 for mailbox names (RFC 3501 §5.1.3).
//!
//! Servers name mailboxes in US-ASCII, escaping everything else into a modified
//! BASE64 of UTF-16BE between `&` and `-`. So a Gmail label reaches us as
//! `&XfJSoGYfaAc-` and has to be shown as 已加星标 (issue #1). Two differences
//! from ordinary BASE64 matter: `,` replaces `/`, so that `/` stays available as
//! a hierarchy delimiter, and there is no `=` padding.
//!
//! Only *display* is decoded. The encoded form is the mailbox's real name on the
//! server — the string SELECT, APPEND, CREATE and the rest must be given — so it
//! is what Hylki stores and sends; [`encode`] exists for the one direction that
//! goes the other way, naming a new folder the user typed.
//!
//! Decoding is deliberately forgiving. A name that isn't valid modified UTF-7 —
//! because a server ignored the rule, or because it already speaks UTF-8 under
//! `UTF8=ACCEPT` — is passed through unchanged rather than mangled or rejected.

/// Decode a mailbox name for display. Input that isn't valid modified UTF-7 is
/// returned as-is.
pub fn decode(name: &str) -> String {
    // Nothing to do for a name with no escapes, which is the overwhelmingly
    // common case (and every ASCII name).
    if !name.contains('&') {
        return name.to_string();
    }

    let bytes = name.as_bytes();
    let mut out = String::with_capacity(name.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'&' {
            // Multi-byte input isn't modified UTF-7 at all, but copying it
            // through keeps a UTF-8 name intact instead of corrupting it.
            let rest = &name[i..];
            let ch = rest.chars().next().unwrap_or('\u{fffd}');
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        // `&-` is a literal ampersand.
        if bytes.get(i + 1) == Some(&b'-') {
            out.push('&');
            i += 2;
            continue;
        }
        match bytes[i + 1..].iter().position(|&b| b == b'-') {
            Some(end) => {
                let run = &name[i + 1..i + 1 + end];
                match decode_run(run) {
                    Some(text) => {
                        out.push_str(&text);
                        i += end + 2;
                    }
                    // Not decodable: emit the `&` and carry on from the next
                    // character, so the rest of the name still gets a chance.
                    None => {
                        out.push('&');
                        i += 1;
                    }
                }
            }
            // Unterminated run — a name that simply contains an `&`.
            None => {
                out.push('&');
                i += 1;
            }
        }
    }
    out
}

/// Modified BASE64: `,` for `/`, no padding, and no stray bits after the last
/// whole character.
const MODIFIED_BASE64: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &base64::alphabet::IMAP_MUTF7,
    base64::engine::general_purpose::NO_PAD
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::RequireNone),
);

/// Decode one `&…-` run: modified BASE64 of UTF-16BE.
fn decode_run(run: &str) -> Option<String> {
    use base64::Engine;
    let bytes = MODIFIED_BASE64.decode(run).ok().filter(|b| !b.is_empty() && b.len() % 2 == 0)?;
    let units: Vec<u16> = bytes.chunks_exact(2).map(|p| u16::from_be_bytes([p[0], p[1]])).collect();
    String::from_utf16(&units).ok()
}

/// Encode a name the user typed into the form the server expects, so a folder
/// can be created with a non-ASCII name.
pub fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending: Vec<u16> = Vec::new();

    for ch in name.chars() {
        // Printable US-ASCII stands for itself; `&` has to be escaped.
        if ('\u{20}'..='\u{7e}').contains(&ch) {
            if !pending.is_empty() {
                push_run(&mut out, &pending);
                pending.clear();
            }
            if ch == '&' {
                out.push_str("&-");
            } else {
                out.push(ch);
            }
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(ch.encode_utf16(&mut buf));
        }
    }
    if !pending.is_empty() {
        push_run(&mut out, &pending);
    }
    out
}

/// Append one `&…-` run for a stretch of non-ASCII characters.
fn push_run(out: &mut String, units: &[u16]) {
    use base64::Engine;
    let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_be_bytes()).collect();
    out.push('&');
    out.push_str(&MODIFIED_BASE64.encode(bytes));
    out.push('-');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_gmail_labels_from_issue_1() {
        // Exactly the strings in the bug report.
        assert_eq!(decode("&XfJSoGYfaAc-"), "已加星标");
        assert_eq!(decode("&g0l6Pw-"), "草稿");
        assert_eq!(decode("&XfJTOZCuTvY-"), "已匹邮件");
        assert_eq!(decode("&V4NXPpCuTvY-"), "垃圾邮件");
    }

    #[test]
    fn leaves_plain_names_alone() {
        for name in ["INBOX", "Sent", "[Gmail]/All Mail", "Работа is not encoded"] {
            assert_eq!(decode(name), name);
        }
    }

    #[test]
    fn handles_ampersands_and_mixed_names() {
        // `&-` is the escape for a literal ampersand.
        assert_eq!(decode("R&-D"), "R&D");
        assert_eq!(decode("&-"), "&");
        // Encoded runs sit inside ordinary text and can repeat.
        assert_eq!(decode("Mail/&U,BTFw-/2026"), "Mail/台北/2026");
        assert_eq!(decode("&U,BTFw- and &U,BTFw-"), "台北 and 台北");
    }

    #[test]
    fn passes_through_anything_that_isnt_valid() {
        // A bare ampersand (server ignoring the rule) must not eat the rest of
        // the name, and a broken run must not be silently dropped.
        assert_eq!(decode("R&D"), "R&D");
        assert_eq!(decode("Tom & Jerry"), "Tom & Jerry");
        assert_eq!(decode("&!!!-"), "&!!!-");
        assert_eq!(decode("&"), "&");
        // A server speaking UTF8=ACCEPT sends the name already decoded.
        assert_eq!(decode("已加星标"), "已加星标");
    }

    #[test]
    fn encodes_what_the_server_expects() {
        assert_eq!(encode("已加星标"), "&XfJSoGYfaAc-");
        assert_eq!(encode("草稿"), "&g0l6Pw-");
        assert_eq!(encode("Mail/台北/2026"), "Mail/&U,BTFw-/2026");
        assert_eq!(encode("R&D"), "R&-D");
        assert_eq!(encode("Sent"), "Sent");
    }

    #[test]
    fn round_trips() {
        for name in [
            "已加星标",
            "Mail/台北/2026",
            "R&D",
            "INBOX",
            "Ärger",
            "emoji 🙂 folder",
            "Tom & Jerry",
        ] {
            assert_eq!(decode(&encode(name)), name, "round trip failed for {name}");
        }
    }

    #[test]
    fn survives_a_surrogate_pair() {
        // Outside the BMP, so UTF-16 needs two units — the case a naive decoder
        // that assumes one unit per character gets wrong.
        let encoded = encode("🙂");
        assert_eq!(decode(&encoded), "🙂");
        assert_eq!(encoded, "&2D3eQg-");
    }
}
