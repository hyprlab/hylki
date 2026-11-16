//! Bare web addresses, and the addresses written around an `@`, in
//! Markdown text.

#[derive(Clone)]
pub(super) enum UrlKind {
    /// Carries its own scheme.
    Web,
    /// `www.` with no scheme.
    Bare,
    /// An email address, Matrix ID or fediverse handle, with its link.
    At(String),
}

/// Byte ranges of the links in `text`. Trailing punctuation is left out, so
/// a sentence ending "see https://example.com." keeps its full stop.
pub(super) fn find_urls(text: &str) -> Vec<(usize, usize, UrlKind)> {
    let b = text.as_bytes();
    let mut found = Vec::new();
    let mut i = 0usize;
    let mut last_end = 0usize;
    while i < b.len() {
        // Byte stepping: never slice mid-character.
        if !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        // Checked before the word-boundary rule below, because an email
        // address's `@` is never at the start of a word.
        if let Some((start, end, href)) = crate::mail::at_link::at_link(text, last_end, i) {
            found.push((start, end, UrlKind::At(href)));
            i = end;
            last_end = end;
            continue;
        }
        // A URL only ever starts at a word boundary.
        if text[..i].chars().next_back().is_none_or(is_break) {
            let rest = &text[i..];
            let kind = if rest.starts_with("https://") || rest.starts_with("http://") {
                Some(UrlKind::Web)
            } else if rest.starts_with("www.") {
                Some(UrlKind::Bare)
            } else {
                None
            };
            if let Some(kind) = kind {
                let end = i + rest
                    .find(|c: char| c.is_whitespace() || c == '<' || c == '>')
                    .unwrap_or(rest.len());
                let end = trim_trailing(text, i, end);
                // `www.name@host` is an email address, found at its `@`.
                let www_mail = matches!(kind, UrlKind::Bare)
                    && text[i..end]
                        .split('/')
                        .next()
                        .is_some_and(|h| h.contains('@'));
                if end > i && !www_mail {
                    found.push((i, end, kind));
                    i = end;
                    last_end = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    found
}

fn is_break(c: char) -> bool {
    c.is_whitespace() || matches!(c, '(' | '[' | '<' | '"' | '\'')
}

/// Pull sentence punctuation (and an unbalanced closing bracket) back off
/// the end of a link.
fn trim_trailing(text: &str, start: usize, mut end: usize) -> usize {
    while end > start {
        let c = text[..end].chars().next_back().unwrap_or(' ');
        let drop = matches!(
            c,
            '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\'' | '*' | '_'
        ) || (c == ')'
            && text[start..end].matches('(').count() < text[start..end].matches(')').count());
        if !drop {
            break;
        }
        end -= c.len_utf8();
    }
    end
}
