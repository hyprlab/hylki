//! The iCalendar content-line layer (RFC 5545 3.1 and 3.3.11): unfolding,
//! splitting a line into name, parameters and value, TEXT escapes, and
//! folding on the way out.

// iCalendar's TEXT escapes are vCard's (crate::encoding).
pub(super) use crate::encoding::escape_text_value as escape;
use crate::encoding::unescape_text_value as unescape;

/// One unfolded content line: `NAME;PARAM=VALUE:value`.
#[derive(Debug, Default)]
pub(super) struct Line {
    pub(super) name: String,
    params: Vec<(String, String)>,
    /// The value with iCalendar's text escapes undone (`\n`, `\,`, `\;`).
    pub(super) value: String,
    /// The value exactly as written, for the fields that are not text
    /// (a date, an address) and must not be un-escaped.
    pub(super) raw: String,
}

impl Line {
    pub(super) fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Split a document into content lines, joining the continuations.
///
/// RFC 5545 folds any line longer than 75 octets by breaking it and starting
/// the next with a single space or tab, which is *not* part of the value —
/// a long description or a URL is routinely split mid-word.
pub(super) fn unfold(ics: &str) -> Vec<Line> {
    let mut joined: Vec<String> = Vec::new();
    for raw in ics.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        match line.strip_prefix([' ', '\t']) {
            Some(rest) => {
                if let Some(last) = joined.last_mut() {
                    last.push_str(rest);
                    continue;
                }
                joined.push(rest.to_string());
            }
            None => joined.push(line.to_string()),
        }
    }
    joined.iter().filter_map(|l| split_line(l)).collect()
}

/// Read one content line. The name and its parameters end at the first colon
/// that is not inside a quoted parameter value — `TZID="Europe/Paris"` and
/// `mailto:someone@example.org` both put colons where a naive split breaks.
fn split_line(line: &str) -> Option<Line> {
    if line.trim().is_empty() {
        return None;
    }
    let mut quoted = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let (head, value) = match colon {
        Some(i) => (&line[..i], &line[i + 1..]),
        None => (line, ""),
    };
    let mut parts = split_unquoted(head, ';');
    let name = parts.next()?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let params = parts
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((k.trim().to_string(), unquote(v.trim()).to_string()))
        })
        .collect();
    Some(Line {
        name,
        params,
        value: unescape(value),
        raw: value.to_string(),
    })
}

/// Split on a separator that is outside double quotes.
fn split_unquoted(text: &str, sep: char) -> impl Iterator<Item = &str> {
    let mut quoted = false;
    let mut bounds = vec![0usize];
    for (i, c) in text.char_indices() {
        match c {
            '"' => quoted = !quoted,
            c if c == sep && !quoted => bounds.push(i),
            _ => {}
        }
    }
    bounds.push(text.len());
    let mut out = Vec::new();
    for pair in bounds.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let piece = &text[a..b];
        out.push(piece.strip_prefix(sep).unwrap_or(piece));
    }
    out.into_iter()
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
}

/// Fold a content line at 75 octets, as RFC 5545 asks, breaking only
/// between characters so a multi-byte one is never split.
pub(crate) fn fold(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + line.len() / 70);
    let mut width = 0;
    for c in line.chars() {
        let n = c.len_utf8();
        if width + n > 73 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += n;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The TEXT escapes both ways, on a corpus shared with the vCard reader
    /// (RFC 5545 3.3.11 and RFC 6350 3.4 are the same rule).
    #[test]
    fn text_escapes_read_and_write_as_they_always_have() {
        let read = [
            ("a\\,b\\;c\\\\d\\ne\\Nf", "a,b;c\\d\ne\nf"),
            ("trailing\\", "trailing\\"),
            ("\\x\\:", "x:"),
            ("plain", "plain"),
            ("", ""),
        ];
        for (raw, text) in read {
            assert_eq!(unescape(raw), text, "{raw:?}");
            assert_eq!(crate::contacts::unescape_vcard_text(raw), text, "{raw:?}");
        }
        assert_eq!(escape("a,b;c\\d\ne\r\nf"), "a\\,b\\;c\\\\d\\ne\\nf");
        assert_eq!(escape("é: ok"), "é: ok");
        let text = "Room 2, floor 3; bring \\ notes\nthanks";
        assert_eq!(unescape(&escape(text)), text);
    }

    /// Lines break before 74 octets, continuations carry one space, and a
    /// multi-byte character is never split.
    #[test]
    fn folding_breaks_at_73_octets_between_characters() {
        let a = |n: usize| "a".repeat(n);
        assert_eq!(fold(&a(73)), a(73));
        assert_eq!(fold(&a(74)), format!("{}\r\n a", a(73)));
        assert_eq!(fold(&a(146)), format!("{}\r\n {}\r\n a", a(73), a(72)));
        let e = "é".repeat(37);
        assert_eq!(fold(&e), format!("{}\r\n é", "é".repeat(36)));
        assert_eq!(fold(""), "");
    }

    #[test]
    fn content_lines_split_at_the_first_unquoted_colon() {
        let line = split_line("ATTENDEE;CN=\"Doe; Jane\";ROLE=REQ:mailto:j@x.org").unwrap();
        assert_eq!(line.name, "ATTENDEE");
        assert_eq!(
            line.params,
            vec![
                ("CN".to_string(), "Doe; Jane".to_string()),
                ("ROLE".to_string(), "REQ".to_string())
            ]
        );
        assert_eq!(line.value, "mailto:j@x.org");
        let line = split_line("DTSTART;TZID=\"America/New_York\":20261102T090000").unwrap();
        assert_eq!(line.param("tzid"), Some("America/New_York"));
        assert_eq!(line.raw, "20261102T090000");
        // A parameter without `=` is dropped.
        let line = split_line("X-THING;bad; K = V :v").unwrap();
        assert_eq!(line.params, vec![("K".to_string(), "V".to_string())]);
        // The value is unescaped; the raw form keeps the escapes.
        let line = split_line("SUMMARY:a\\,b").unwrap();
        assert_eq!((line.value.as_str(), line.raw.as_str()), ("a,b", "a\\,b"));
        // No colon: a name with an empty value.
        let line = split_line("NOCOLON").unwrap();
        assert_eq!((line.name.as_str(), line.value.as_str()), ("NOCOLON", ""));
        assert!(split_line(":value").is_none());
        assert!(split_line("   ").is_none());
    }

    #[test]
    fn unfolding_joins_continuations_and_drops_blank_lines() {
        let lines = unfold("A:1\r\n B\r\n\tC\n\r\nD;X=y:2\n ");
        let got: Vec<(&str, &str)> = lines
            .iter()
            .map(|l| (l.name.as_str(), l.value.as_str()))
            .collect();
        assert_eq!(got, vec![("A", "1BC"), ("D", "2")]);
        // A continuation with nothing before it starts a line of its own.
        let lines = unfold(" X:1");
        assert_eq!(
            (lines[0].name.as_str(), lines[0].value.as_str()),
            ("X", "1")
        );
    }
}
