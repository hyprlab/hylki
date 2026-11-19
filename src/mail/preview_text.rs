//! What is left of a preview once the words are all that remain: no tags,
//! no comments, no entities, no invisible padding (#683).
//!
//! An HTML part goes through html5ever before it gets here, but markup
//! reaches the list by other roads too: a plain-text alternative written
//! with entities (`Iberia &zwnj; &zwnj; …`, a marketing preheader), a
//! server's own preview of such a part (JMAP, Graph), a double-escaped
//! HTML part, or a row cached before this pass existed.

use html5ever::data::NAMED_ENTITIES;

/// Elements that sit inside a word (`<b>T</b>uesday`) and so leave no gap
/// when they go. Every other tag is a break between words.
const INLINE: [&str; 20] = [
    "a", "abbr", "b", "big", "code", "em", "font", "i", "mark", "s", "small", "span", "strike",
    "strong", "sub", "sup", "tt", "u", "wbr", "o:p",
];

/// Characters that take up no room but are not whitespace either, so
/// a collapse leaves them behind: marketing "preheader padding" strings
/// hundreds of them together to push the real text out of inbox previews,
/// and a row showing them is a blank gap ending in an ellipsis.
pub(crate) fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200b}'
            ..='\u{200d}' // zero-width space and joiners
            | '\u{2060}' // word joiner
            | '\u{feff}' // zero-width no-break space
            | '\u{00ad}' // soft hyphen
            | '\u{034f}' // combining grapheme joiner
            | '\u{2800}' // Braille blank, another padder
    )
}

/// `text` with its markup gone and its entities decoded, and with the
/// characters that show nothing dropped, on one line.
pub(crate) fn one_line(text: &str) -> String {
    let words: String = speaking_text(text)
        .chars()
        .filter(|&c| !invisible(c))
        .collect();
    words.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether `text` still carries markup [`speaking_text`] would remove.
pub(crate) fn has_markup(text: &str) -> bool {
    text.contains(['<', '&']) && speaking_text(text) != text
}

/// `text` with every tag and comment removed and every entity decoded.
///
/// Decoding can uncover markup (`&amp;zwnj;`, `&amp;lt;br&amp;gt;`), so
/// the pass runs again while it still changes something, a few times at
/// most: a preview is words, and a tag spelled out in the text is not one.
pub(crate) fn speaking_text(text: &str) -> String {
    let mut out = text.to_string();
    for _ in 0..3 {
        if !out.contains(['<', '&']) {
            break;
        }
        let next = strip_once(&out);
        if next == out {
            break;
        }
        out = next;
    }
    out
}

fn strip_once(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(['<', '&']) {
        out.push_str(&rest[..i]);
        let at = &rest[i..];
        let skipped = if at.starts_with('<') {
            tag_len(at).map(|(len, gap)| {
                if gap {
                    out.push(' ');
                }
                len
            })
        } else {
            entity(at).map(|(len, c)| {
                out.extend(c);
                len
            })
        };
        match skipped {
            Some(len) => rest = &at[len..],
            None => {
                out.push(at.chars().next().unwrap_or_default());
                rest = &at[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The length of the tag, comment or declaration `s` opens with, and
/// whether it stands between words. `None` for a `<` that opens no markup:
/// `a < b`, `<ada@example.com>`, `<https://example.com>`.
fn tag_len(s: &str) -> Option<(usize, bool)> {
    if let Some(body) = s.strip_prefix("<!--") {
        // A comment the fetch cut off runs to the end of what arrived.
        return Some((body.find("-->").map_or(s.len(), |e| 4 + e + 3), true));
    }
    let name_start = if s[1..].starts_with(['/', '!', '?']) {
        2
    } else {
        1
    };
    let name: &str = {
        let tail = &s[name_start..];
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == ':' || c == '-'))
            .unwrap_or(tail.len());
        &tail[..end]
    };
    // Text that ends on a tag the fetch cut off (`… David Sims </`, `<di`)
    // ends there: what follows the `<` is the rest of the slice.
    let cut_off = (s.len(), true);
    if name.is_empty() && name_start == 2 && s[2..].trim().is_empty() {
        return Some(cut_off);
    }
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    let after = &s[name_start + name.len()..];
    let opens = match after.chars().next() {
        Some('>') => true,
        Some('/') => after[1..].starts_with('>'),
        Some(c) => c.is_whitespace(),
        None => return Some(cut_off),
    };
    if !opens {
        return None;
    }
    // The tag ends at the first `>` outside a quoted attribute value.
    let mut quote = None;
    for (i, c) in after.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => {
                let len = name_start + name.len() + i + 1;
                let inline = INLINE.contains(&name.to_ascii_lowercase().as_str());
                return Some((len, !inline));
            }
            (None, '<') => return None,
            _ => {}
        }
    }
    // No `>` before the end: a tag cut off among its attributes, or a `<`
    // in prose (`if a <b and c`), told apart by the attribute's `=`.
    after.contains('=').then_some(cut_off)
}

/// The length of the entity `s` opens with and what it stands for: `None`
/// for a bare `&` (`AT&T`, `R & D`). A well-formed named entity HTML does
/// not know stands for nothing.
fn entity(s: &str) -> Option<(usize, Option<char>)> {
    let semi = s[1..].find(';').filter(|&i| (1..=32).contains(&i))? + 1;
    let name = &s[1..semi];
    let len = semi + 1;
    if let Some(num) = name.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse::<u32>().ok()?,
        };
        return Some((len, char::from_u32(code).filter(|&c| c != '\0')));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let decoded = NAMED_ENTITIES
        .get(&s[1..len])
        .and_then(|&(c, _)| char::from_u32(c));
    Some((len, decoded))
}

#[cfg(test)]
mod tests {
    use super::{has_markup, one_line, speaking_text};

    #[test]
    fn a_preheader_of_entities_leaves_only_its_words() {
        let iberia = "Iberia &zwnj; &zwnj; &zwnj;&nbsp;&zwnj; &zwnj; Tomas, tu viaje";
        assert_eq!(one_line(iberia), "Iberia Tomas, tu viaje");
    }

    #[test]
    fn tags_go_and_words_stay_apart_or_together() {
        assert_eq!(one_line("Hello<br>World"), "Hello World");
        assert_eq!(one_line("<p>One</p><p>Two</p>"), "One Two");
        assert_eq!(one_line("<b>T</b>uesday"), "Tuesday");
        assert_eq!(
            one_line(r#"<a href="https://x.example/?a=1&b=2" title="a > b">Book</a> now"#),
            "Book now"
        );
        assert_eq!(one_line("Hi <!-- tracking -->there"), "Hi there");
        assert_eq!(one_line("Hi<!-- cut off by the fet"), "Hi");
        assert_eq!(
            one_line("A new newsletter from David Sims </"),
            "A new newsletter from David Sims"
        );
        assert_eq!(one_line("Read more <di"), "Read more");
        assert_eq!(
            one_line(r#"Book now <a href="https://x.example/tr"#),
            "Book now"
        );
        assert_eq!(one_line("<!DOCTYPE html><html>Text"), "Text");
    }

    #[test]
    fn entities_decode_whatever_their_spelling() {
        assert_eq!(
            speaking_text("caf&eacute; &amp; t&#233; &#x2014; ok"),
            "café & té — ok"
        );
        // Escaped twice: the first pass uncovers what the second removes.
        assert_eq!(one_line("A&amp;zwnj;B &amp;lt;br&amp;gt;C"), "AB C");
        // A name HTML does not know is code all the same.
        assert_eq!(one_line("Sale &bogus; today"), "Sale today");
    }

    #[test]
    fn text_that_only_looks_like_markup_is_left_alone() {
        for text in [
            "AT&T & R&D",
            "if a < b and b > c",
            "Ada <ada@example.com> wrote",
            "See <https://example.com/path>",
            "x<3 forever",
            "if a <b and c",
            "a <",
            "Tom & Jerry; friends",
        ] {
            assert_eq!(speaking_text(text), text);
            assert!(!has_markup(text), "{text}");
        }
        assert!(has_markup("Iberia &zwnj;"));
        assert!(has_markup("<div>x</div>"));
    }
}
