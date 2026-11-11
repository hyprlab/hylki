//! Repair doubled backslashes only at flag-token starts in FETCH FLAGS.
//!
//! This is a streaming syntax scanner, not a response buffer: even a literal
//! larger than memory goes straight through, with only its remaining length
//! retained. Recognition state survives arbitrary read boundaries.

#[derive(Debug, Default)]
pub(super) struct FetchFlags {
    syntax: Syntax,
    quoted: bool,
    escaped: bool,
    marker: Marker,
    literal: usize,
    depth: usize,
    brackets: usize,
    name: usize,
    flag_start: bool,
    flag_slash: bool,
}

#[derive(Debug, Default)]
enum Syntax {
    #[default]
    Start,
    Star,
    NumberStart,
    Number,
    Fetch(usize),
    Kind(usize, u8),
    Other,
    Text,
    Name,
    Value,
    FlagsOpen,
    Flags,
}

#[derive(Debug, Default)]
enum Marker {
    #[default]
    None,
    Digits(usize, bool),
    Plus(usize),
    Close(usize),
    Cr(usize),
}

impl FetchFlags {
    /// Compact only repaired prefixes in place. Literal runs are skipped as a
    /// whole, and bytes preceding the first repair are never copied.
    pub(super) fn normalize(&mut self, bytes: &mut [u8]) -> usize {
        let mut at = 0;
        let mut copied = 0;
        let mut kept = 0;
        while at < bytes.len() {
            if self.literal > 0 {
                let n = self.literal.min(bytes.len() - at);
                self.literal -= n;
                at += n;
                continue;
            }
            if self.byte(bytes[at]) {
                if kept != copied {
                    bytes.copy_within(copied..at, kept);
                }
                kept += at - copied;
                copied = at + 1;
            }
            at += 1;
        }
        if kept != copied {
            bytes.copy_within(copied..at, kept);
        }
        kept + at - copied
    }

    /// Return true only for the redundant second backslash.
    fn byte(&mut self, b: u8) -> bool {
        if self.quoted {
            if self.escaped {
                self.escaped = false;
            } else if b == b'\\' {
                self.escaped = true;
            } else if b == b'"' {
                self.quoted = false;
            }
            return false;
        }
        // A literal marker's CRLF is not the end of its response. Checked
        // arithmetic avoids interpreting an overflowing length as a short one.
        let marker = std::mem::take(&mut self.marker);
        self.marker = match marker {
            Marker::Digits(n, _) if b.is_ascii_digit() => n
                .checked_mul(10)
                .and_then(|n| n.checked_add(usize::from(b - b'0')))
                .map_or(Marker::None, |n| Marker::Digits(n, true)),
            Marker::Digits(n, true) if b == b'+' => Marker::Plus(n),
            Marker::Digits(n, true) | Marker::Plus(n) if b == b'}' => Marker::Close(n),
            Marker::Close(n) if b == b'\r' => Marker::Cr(n),
            Marker::Cr(n) if b == b'\n' => {
                self.literal = n;
                return false;
            }
            _ => Marker::None,
        };
        if !matches!(self.marker, Marker::None) {
            return false;
        }
        if b == b'\n' {
            self.syntax = Syntax::Start;
            self.depth = 0;
            self.brackets = 0;
            return false;
        }
        // Tagged replies, continuations and untagged status responses contain
        // free-form response text, not quoted strings or literal markers.
        if !matches!(self.syntax, Syntax::Text) {
            if b == b'"' {
                self.quoted = true;
                self.flag_start = false;
                self.flag_slash = false;
                return false;
            }
            if b == b'{' {
                self.marker = Marker::Digits(0, false);
                return false;
            }
        }
        match self.syntax {
            Syntax::Start => {
                self.syntax = if b == b'*' { Syntax::Star } else { Syntax::Text };
            }
            Syntax::Star => {
                self.syntax = if b == b' ' { Syntax::NumberStart } else { Syntax::Text };
            }
            Syntax::NumberStart => {
                self.syntax = if b.is_ascii_digit() {
                    Syntax::Number
                } else {
                    Syntax::Kind(1, Self::status_mask(0, b, 0x1f))
                };
            }
            Syntax::Kind(n, mask) => {
                self.syntax = if b == b' ' || b == b'\r' {
                    let status = [b"OK".as_slice(), b"NO", b"BAD", b"BYE", b"PREAUTH"]
                        .iter()
                        .enumerate()
                        .any(|(i, word)| mask & (1 << i) != 0 && word.len() == n);
                    if status { Syntax::Text } else { Syntax::Other }
                } else {
                    Syntax::Kind(n.saturating_add(1), Self::status_mask(n, b, mask))
                };
            }
            Syntax::Number => {
                if !b.is_ascii_digit() {
                    self.syntax = if b == b' ' { Syntax::Fetch(0) } else { Syntax::Other };
                }
            }
            Syntax::Fetch(n) => {
                const HEAD: &[u8] = b"FETCH (";
                self.syntax = if b.to_ascii_uppercase() != HEAD[n] {
                    Syntax::Other
                } else if n + 1 == HEAD.len() {
                    self.name = 0;
                    Syntax::Name
                } else {
                    Syntax::Fetch(n + 1)
                };
            }
            Syntax::Name => match b {
                b'[' => self.brackets = self.brackets.saturating_add(1),
                b']' => self.brackets = self.brackets.saturating_sub(1),
                b' ' if self.brackets == 0 => {
                    if self.name != 0 {
                        self.syntax = if self.name == 5 { Syntax::FlagsOpen } else { Syntax::Value };
                        self.depth = 0;
                        self.name = 0;
                    }
                }
                b')' if self.brackets == 0 => self.syntax = Syntax::Other,
                _ => {
                    self.name = if self.name < 5
                        && b.to_ascii_uppercase() == b"FLAGS"[self.name]
                    {
                        self.name + 1
                    } else {
                        6
                    };
                }
            },
            Syntax::Value => match b {
                b'(' => self.depth = self.depth.saturating_add(1),
                b')' if self.depth > 0 => {
                    self.depth -= 1;
                    if self.depth == 0 {
                        self.syntax = Syntax::Name;
                    }
                }
                b')' => self.syntax = Syntax::Other,
                b' ' if self.depth == 0 => self.syntax = Syntax::Name,
                _ => {}
            },
            Syntax::FlagsOpen => {
                if b == b'(' {
                    self.syntax = Syntax::Flags;
                    self.flag_start = true;
                    self.flag_slash = false;
                } else if b != b' ' {
                    self.syntax = Syntax::Other;
                }
            }
            Syntax::Flags => {
                if b == b')' {
                    self.syntax = Syntax::Name;
                } else if b == b' ' {
                    self.flag_start = true;
                    self.flag_slash = false;
                } else {
                    let redundant = self.flag_slash && b == b'\\';
                    self.flag_slash = self.flag_start && b == b'\\';
                    self.flag_start = false;
                    return redundant;
                }
            }
            Syntax::Other | Syntax::Text => {}
        }
        false
    }

    fn status_mask(at: usize, b: u8, mask: u8) -> u8 {
        [b"OK".as_slice(), b"NO", b"BAD", b"BYE", b"PREAUTH"]
            .iter()
            .enumerate()
            .fold(mask, |mask, (i, word)| {
                if word.get(at).is_some_and(|c| c.eq_ignore_ascii_case(&b)) {
                    mask
                } else {
                    mask & !(1 << i)
                }
            })
    }
}
