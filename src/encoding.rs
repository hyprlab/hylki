//! Text encodings the app writes and reads: base64 (with the lenient
//! decoder mail needs), percent-encoding, the HTML text escape, and the
//! iCalendar and vCard TEXT escapes. One home for each, so every
//! module encodes the same way.

mod base64;
pub(crate) mod percent;
mod text_value;

pub(crate) use self::base64::{base64_decode, base64_encode, base64_lines};
pub(crate) use text_value::{escape_text_value, unescape_text_value};

/// Text escaped for HTML, for element content and quoted attribute values
/// alike: `&`, `<`, `>`, `'` and `"` become entities, as do the C0 and C1
/// control characters XML forbids (GLib's markup escape).
pub(crate) fn escape_html(text: &str) -> String {
    gtk::glib::markup_escape_text(text).into()
}

/// Plain text as HTML for the composer, a `<br>` for each line break.
/// Indents and runs of spaces become no-break spaces, and a tab the ones
/// to the next stop of eight, as the composer keeps them where they are
/// typed or pasted: in HTML they would collapse to a single space, or to
/// nothing at the start of a line (#635). What is sent turns them back
/// into spaces.
pub(crate) fn text_lines_html(text: &str) -> String {
    text.replace("\r\n", "\n")
        .split(['\n', '\r'])
        .map(|line| escape_html(&keep_spaces(line)))
        .collect::<Vec<_>>()
        .join("<br>")
}

/// `line` with its indent, runs of spaces and tabs as no-break spaces.
fn keep_spaces(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0;
    let mut prev_blank = true;
    for c in line.chars() {
        match c {
            '\t' => {
                let n = 8 - col % 8;
                out.extend(std::iter::repeat_n('\u{a0}', n));
                col += n;
            }
            ' ' if prev_blank => {
                out.push('\u{a0}');
                col += 1;
            }
            _ => {
                out.push(c);
                col += 1;
            }
        }
        prev_blank = matches!(c, ' ' | '\t' | '\u{a0}');
    }
    out
}

/// The handful of entities mail bodies use, decoded: the named five, `&nbsp;`
/// and numeric references. Anything else is left as written.
pub(crate) fn unescape_html(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let Some(semi) = after.find(';').filter(|&i| i <= 10) else {
            out.push('&');
            rest = after;
            continue;
        };
        let name = &after[..semi];
        let replacement = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => name
                .strip_prefix('#')
                .and_then(|n| {
                    if let Some(h) = n.strip_prefix(['x', 'X']) {
                        u32::from_str_radix(h, 16).ok()
                    } else {
                        n.parse::<u32>().ok()
                    }
                })
                .and_then(char::from_u32),
        };
        match replacement {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::{escape_html, text_lines_html};

    /// Indents and spaced-out text keep every space, as no-break spaces a
    /// single space between words does not need (#635).
    #[test]
    fn text_lines_keep_their_indents() {
        let html = text_lines_html("List:\r\n - Foo\n    - <Sub>\n\tx  y\n\nend ");
        assert_eq!(
            html,
            "List:<br>\u{a0}- Foo<br>\u{a0}\u{a0}\u{a0}\u{a0}- &lt;Sub&gt;<br>\
             \u{a0}\u{a0}\u{a0}\u{a0}\u{a0}\u{a0}\u{a0}\u{a0}x \u{a0}y<br><br>end "
        );
        assert_eq!(
            text_lines_html("ab\tc"),
            "ab\u{a0}\u{a0}\u{a0}\u{a0}\u{a0}\u{a0}c"
        );
    }

    /// The corpus the reader, the reader's document, Markdown and the
    /// composer's cloud links were all escaped with before they shared
    /// this function (each was `markup_escape_text(s).into()`).
    #[test]
    fn html_escape_covers_markup_quotes_and_control_characters() {
        let cases = [
            ("", ""),
            ("plain text", "plain text"),
            ("a < b > c", "a &lt; b &gt; c"),
            ("Tom & Jerry", "Tom &amp; Jerry"),
            ("&amp; stays escaped", "&amp;amp; stays escaped"),
            (
                "\"double\" 'single'",
                "&quot;double&quot; &apos;single&apos;",
            ),
            (
                "<script>alert(1)</script>",
                "&lt;script&gt;alert(1)&lt;/script&gt;",
            ),
            ("line\nbreak\ttab\rreturn", "line\nbreak\ttab\rreturn"),
            ("bell\u{7}", "bell&#x7;"),
            ("del\u{7f}c1\u{85}nel", "del&#x7f;c1\u{85}nel"),
            ("Grüße, 日本, 🙂", "Grüße, 日本, 🙂"),
        ];
        for (text, html) in cases {
            assert_eq!(escape_html(text), html, "{text:?}");
        }
    }
}
