//! Percent-encoding (RFC 3986) for the URLs, `mailto:` links and `cid:`
//! references Hylki builds and reads.
//!
//! Encoding is GLib's. Decoding is not: `glib::Uri::unescape_string` refuses
//! the whole string over one malformed escape, and what is decoded here is
//! often written by hand, where a bare `%` ("100% off") sits beside real
//! escapes. This decoder turns every valid `%XX` back into its byte and keeps
//! anything else as it is.

/// Everything but the unreserved characters escaped: a query value or one
/// path segment.
pub fn encode(s: &str) -> String {
    gtk::glib::Uri::escape_string(s, None, false).into()
}

/// A whole path, keeping its slashes.
pub fn encode_path(s: &str) -> String {
    gtk::glib::Uri::escape_string(s, Some("/"), false).into()
}

/// `%XX` back to bytes, read as UTF-8 (lossily). `plus_is_space` reads `+`
/// as a space, as an HTML form writes it; a `mailto:` address keeps its `+`
/// (plus-addressing), and a path or a `cid:` has no such convention.
pub fn decode(s: &str, plus_is_space: bool) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                    continue;
                }
                out.push(b'%');
            }
            b'+' if plus_is_space => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_keeps_only_the_unreserved_set() {
        assert_eq!(encode("\"body:opt out\""), "%22body%3Aopt%20out%22");
        assert_eq!(encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(encode("caf\u{e9}"), "caf%C3%A9");
        assert_eq!(encode_path("/Hylki/Q3 report.pdf"), "/Hylki/Q3%20report.pdf");
    }

    #[test]
    fn decoding_takes_what_is_valid_and_keeps_the_rest() {
        assert_eq!(decode("caf%C3%A9%40x", false), "caf\u{e9}@x");
        assert_eq!(decode("100% off%20now", false), "100% off now");
        assert_eq!(decode("list+a%2Bb", false), "list+a+b");
        assert_eq!(decode("go+away", true), "go away");
        assert_eq!(decode("%+1%4", false), "%+1%4");
        assert_eq!(decode("%41", false), "A");
    }
}
