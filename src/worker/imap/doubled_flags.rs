//! Drop flags written with two leading backslashes (`\\Seen`) from the
//! server's flag lists before async-imap reads them (#672).
//!
//! Purelymail can send these beside the real system flags. They are not
//! valid IMAP, and imap-proto fails the whole FLAGS or FETCH response on
//! them, which stops the folder opening. They are dropped, not read as the
//! system flag they resemble: they are stray keywords on the server, and
//! taking `\\Seen` for `\Seen` would show unread mail as read, with no way
//! to clear it from here.

use super::namespace::literal_len;

/// Splits the bytes off the wire into lines and literals, and rewrites the
/// lines that hold a flag with a doubled backslash. Literal bytes (message
/// text) always pass through untouched.
#[derive(Debug, Default)]
pub(super) struct DoubledFlags {
    /// The line being put together, up to its LF.
    line: Vec<u8>,
    /// Bytes of a literal still to come.
    literal: usize,
    /// Whether a drop has been logged on this connection yet.
    logged: bool,
}

impl DoubledFlags {
    /// Take `bytes` off the wire, adding to `out` what goes on to async-imap.
    pub(super) fn feed(&mut self, mut bytes: &[u8], out: &mut Vec<u8>) {
        while !bytes.is_empty() {
            if self.literal > 0 {
                let n = self.literal.min(bytes.len());
                out.extend_from_slice(&bytes[..n]);
                self.literal -= n;
                bytes = &bytes[n..];
                continue;
            }
            let Some(lf) = bytes.iter().position(|&b| b == b'\n') else {
                self.line.extend_from_slice(bytes);
                return;
            };
            self.line.extend_from_slice(&bytes[..=lf]);
            bytes = &bytes[lf + 1..];
            let text = self.line.strip_suffix(b"\n").unwrap_or(&self.line);
            let text = text.strip_suffix(b"\r").unwrap_or(text);
            self.literal = literal_len(text).unwrap_or(0);
            if repair(&self.line, out) && !self.logged {
                self.logged = true;
                tracing::info!("imap: dropped flags with a doubled backslash (#672)");
            }
            self.line.clear();
        }
    }

    /// The stream has ended: a partial line goes on as it is.
    pub(super) fn finish(&mut self, out: &mut Vec<u8>) {
        out.append(&mut self.line);
    }
}

/// Copy `line` to `out`, leaving out every flag that starts with two
/// backslashes inside a `FLAGS (...)` or `PERMANENTFLAGS (...)` list.
/// Returns whether one was left out.
fn repair(line: &[u8], out: &mut Vec<u8>) -> bool {
    if !line.windows(2).any(|w| w == b"\\\\") {
        out.extend_from_slice(line);
        return false;
    }
    let mut dropped = false;
    let mut copied = 0;
    let mut i = 0;
    while i < line.len() {
        match line[i] {
            // A quoted string can hold `\\` (an escaped backslash) and text
            // that looks like a flag list: skip it whole.
            b'"' => {
                i += 1;
                while i < line.len() && line[i] != b'"' {
                    i += if line[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'(' if ends_with_flags(&line[..i]) => {
                let Some(len) = line[i + 1..].iter().position(|&b| b == b')') else {
                    break;
                };
                let list = &line[i + 1..i + 1 + len];
                let plain = !list.iter().any(|b| matches!(b, b'"' | b'(' | b'{'));
                if plain && list.windows(2).any(|w| w == b"\\\\") {
                    out.extend_from_slice(&line[copied..=i]);
                    let kept: Vec<&[u8]> = list
                        .split(|&b| b == b' ')
                        .filter(|flag| !flag.is_empty() && !flag.starts_with(b"\\\\"))
                        .collect();
                    out.extend_from_slice(&kept.join(&b' '));
                    copied = i + 1 + len;
                    dropped = true;
                    i += 1 + len;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    out.extend_from_slice(&line[copied..]);
    dropped
}

/// Whether `before` ends with `FLAGS ` (as `PERMANENTFLAGS ` does), the
/// item a flag list follows, in any case.
fn ends_with_flags(before: &[u8]) -> bool {
    before.len() >= 6 && before[before.len() - 6..].eq_ignore_ascii_case(b"FLAGS ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&[u8]]) -> Vec<u8> {
        let mut filter = DoubledFlags::default();
        let mut out = Vec::new();
        for chunk in chunks {
            filter.feed(chunk, &mut out);
        }
        filter.finish(&mut out);
        out
    }

    fn bytewise(input: &[u8]) -> Vec<u8> {
        let chunks: Vec<&[u8]> = input.chunks(1).collect();
        run(&chunks)
    }

    #[test]
    fn drops_doubled_flags_from_the_three_responses_in_the_report() {
        let cases: [(&[u8], &[u8]); 3] = [
            (
                b"* FLAGS (\\Answered \\Deleted \\Draft \\Flagged \\Seen \\\\Flagged \\\\Seen \\\\Answered)\r\n",
                b"* FLAGS (\\Answered \\Deleted \\Draft \\Flagged \\Seen)\r\n",
            ),
            (
                b"* OK [PERMANENTFLAGS (\\Answered \\Deleted \\Draft \\Flagged \\Seen \\\\Flagged \\\\Seen \\\\Answered \\*)] F\r\n",
                b"* OK [PERMANENTFLAGS (\\Answered \\Deleted \\Draft \\Flagged \\Seen \\*)] F\r\n",
            ),
            (
                b"* 1 FETCH (FLAGS (\\Answered \\Deleted \\Draft \\Flagged \\Seen \\\\Flagged \\\\Seen \\\\Answered))\r\n",
                b"* 1 FETCH (FLAGS (\\Answered \\Deleted \\Draft \\Flagged \\Seen))\r\n",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(run(&[input]), expected);
            assert_eq!(bytewise(input), expected);
        }
    }

    #[test]
    fn a_list_of_only_doubled_flags_becomes_empty() {
        assert_eq!(
            run(&[b"* 3 FETCH (UID 9 fLaGs (\\\\Seen))\r\n"]),
            b"* 3 FETCH (UID 9 fLaGs ())\r\n"
        );
    }

    #[test]
    fn literals_and_quoted_strings_are_left_alone() {
        let body: &[u8] = b"Subject: FLAGS (\\\\Seen)\r\n\r\n* 9 FETCH (FLAGS (\\\\Seen))\r\n";
        let mut input = format!(
            "* 1 FETCH (UID 7 FLAGS (\\\\Seen \\Answered custom) \
             ENVELOPE (NIL \"FLAGS (\\\\Seen)\" NIL NIL NIL NIL NIL NIL NIL NIL) BODY[] {{{}}}\r\n",
            body.len()
        )
        .into_bytes();
        input.extend_from_slice(body);
        input.extend_from_slice(b" FLAGS (\\\\Flagged \\Seen))\r\nA1 OK done\r\n");

        let mut expected = format!(
            "* 1 FETCH (UID 7 FLAGS (\\Answered custom) \
             ENVELOPE (NIL \"FLAGS (\\\\Seen)\" NIL NIL NIL NIL NIL NIL NIL NIL) BODY[] {{{}}}\r\n",
            body.len()
        )
        .into_bytes();
        expected.extend_from_slice(body);
        expected.extend_from_slice(b" FLAGS (\\Seen))\r\nA1 OK done\r\n");

        assert_eq!(run(&[&input]), expected);
        assert_eq!(bytewise(&input), expected);
    }

    #[test]
    fn other_lines_pass_unchanged() {
        let input: &[u8] = b"* OK [CAPABILITY IMAP4rev1] hi\r\n* 2 FETCH (FLAGS (\\Seen $Forwarded))\r\n+ idling\r\n* LIST (\\HasNoChildren) \"\\\\\" \"a\\\\b\"\r\n";
        assert_eq!(run(&[input]), input);
        assert_eq!(bytewise(input), input);
    }

    #[test]
    fn a_partial_line_is_held_until_its_end_or_the_stream_ends() {
        let mut filter = DoubledFlags::default();
        let mut out = Vec::new();
        filter.feed(b"* FLAGS (\\Seen \\\\Se", &mut out);
        assert!(out.is_empty());
        filter.feed(b"en)\r\n* BY", &mut out);
        assert_eq!(out, b"* FLAGS (\\Seen)\r\n");
        filter.finish(&mut out);
        assert_eq!(out, b"* FLAGS (\\Seen)\r\n* BY");
    }
}
