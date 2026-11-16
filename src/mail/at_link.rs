//! The addresses written around an `@` in plain text, made into links: an
//! email address, a Matrix ID (`@name:server.org`) and a fediverse handle
//! (`@name@server.org`, as Mastodon writes one) (#653).

/// The link the `@` at byte `at` of `text` belongs to: `(start, end, href)`.
/// `from` is where the text not yet linked starts, so an address never
/// reaches back into a link already made.
pub(crate) fn at_link(text: &str, from: usize, at: usize) -> Option<(usize, usize, String)> {
    if text.as_bytes().get(at) != Some(&b'@') {
        return None;
    }
    // An `@` that starts a word is a handle's; one inside a word splits an
    // email address.
    match text[..at].chars().next_back() {
        Some(c) if c.is_alphanumeric() || is_local_char(c) => email(text, from, at),
        _ => handle(text, at),
    }
}

/// The link `text` makes when the whole of it is one address, as an
/// autolink writes it.
pub(crate) fn whole_link(text: &str) -> Option<String> {
    let at = text.find('@')?;
    at_link(text, 0, at)
        .filter(|&(start, end, _)| start == 0 && end == text.len())
        .map(|(_, _, href)| href)
}

fn email(text: &str, from: usize, at: usize) -> Option<(usize, usize, String)> {
    let local_len = text[from..at]
        .chars()
        .rev()
        .take_while(|&c| is_local_char(c))
        .map(char::len_utf8)
        .sum::<usize>();
    let local = text[at - local_len..at].trim_start_matches('.');
    let start = at - local.len();
    if local.is_empty() || local.ends_with('.') {
        return None;
    }
    // Glued to a word or to another `@`: not an address of its own.
    if text[..start]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || c == '@')
    {
        return None;
    }
    let end = domain_end(text, at + 1)?;
    if text[end..].starts_with('@') {
        return None;
    }
    Some((start, end, format!("mailto:{}", &text[start..end])))
}

/// `@name@server.org` opens the profile on its server, the address every
/// fediverse server answers; `@name:server.org` opens matrix.to, which
/// hands the ID to whichever Matrix client the reader uses.
fn handle(text: &str, at: usize) -> Option<(usize, usize, String)> {
    let name_start = at + 1;
    let name_len = text[name_start..]
        .find(|c: char| !is_handle_char(c))
        .unwrap_or(text.len() - name_start);
    let name = &text[name_start..name_start + name_len];
    if name.is_empty() || name.starts_with('.') || name.ends_with('.') {
        return None;
    }
    let sep = name_start + name_len;
    match text[sep..].chars().next()? {
        '@' if !name.contains(['/', '=', '+']) => {
            let end = domain_end(text, sep + 1)?;
            if text[end..].starts_with('@') {
                return None;
            }
            let host = &text[sep + 1..end];
            Some((at, end, format!("https://{host}/@{name}")))
        }
        ':' => {
            let mut end = domain_end(text, sep + 1)?;
            // A server name may carry a port.
            if let Some(port) = text[end..].strip_prefix(':') {
                let digits = port.bytes().take_while(u8::is_ascii_digit).count();
                if (1..=5).contains(&digits) {
                    end += 1 + digits;
                }
            }
            Some((at, end, format!("https://matrix.to/#/{}", &text[at..end])))
        }
        _ => None,
    }
}

/// The end of the host name starting at `start`: two or more labels, the
/// last of them letters only, as a top-level domain is (`xn--` for one in
/// another script). A full stop or hyphen after it is the sentence's.
fn domain_end(text: &str, start: usize) -> Option<usize> {
    let rest = &text[start..];
    let len = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '.' || c == '-'))
        .unwrap_or(rest.len());
    let host = rest[..len].trim_end_matches(['.', '-']);
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2
        || labels
            .iter()
            .any(|l| l.is_empty() || l.starts_with('-') || l.ends_with('-'))
    {
        return None;
    }
    let tld = labels[labels.len() - 1];
    let letters = tld.chars().count() >= 2 && tld.chars().all(char::is_alphabetic);
    let punycode = tld.len() > 4 && tld[..4].eq_ignore_ascii_case("xn--");
    (letters || punycode).then_some(start + host.len())
}

/// What the part of an address before its `@` is written with in practice;
/// the rarer characters RFC 5322 allows would take quotes and brackets of
/// the surrounding sentence into the link.
fn is_local_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-')
}

/// A Matrix localpart's characters; a fediverse name takes the same less
/// `/`, `=` and `+`.
fn is_handle_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '=' | '/' | '+')
}

#[cfg(test)]
mod tests {
    use super::{at_link, whole_link};

    /// Every link in `text`, scanning as the linkifiers do.
    fn links(text: &str) -> Vec<(&str, String)> {
        let mut out = Vec::new();
        let mut from = 0;
        let mut i = 0;
        while i < text.len() {
            if let Some((start, end, href)) = at_link(text, from, i) {
                out.push((&text[start..end], href));
                from = end;
                i = end;
            } else {
                i += 1;
            }
        }
        out
    }

    #[test]
    fn the_report_653() {
        let text = " * email: foo@bar.red\n * matrix: @foo:bar.net\n * mastodon: @foo@bar.io\n";
        assert_eq!(
            links(text),
            vec![
                ("foo@bar.red", "mailto:foo@bar.red".to_string()),
                (
                    "@foo:bar.net",
                    "https://matrix.to/#/@foo:bar.net".to_string()
                ),
                ("@foo@bar.io", "https://bar.io/@foo".to_string()),
            ]
        );
    }

    #[test]
    fn sentence_punctuation_stays_out() {
        assert_eq!(
            links("Write to ada.l+x@mail.example.com."),
            vec![(
                "ada.l+x@mail.example.com",
                "mailto:ada.l+x@mail.example.com".to_string()
            )]
        );
        assert_eq!(
            links("(@ann@social.example.org)"),
            vec![(
                "@ann@social.example.org",
                "https://social.example.org/@ann".to_string()
            )]
        );
        assert_eq!(
            links("<bob@example.com>, then"),
            vec![("bob@example.com", "mailto:bob@example.com".to_string())]
        );
    }

    #[test]
    fn a_matrix_server_keeps_its_port() {
        assert_eq!(
            links("@a.b_c:chat.example.org:8448 or"),
            vec![(
                "@a.b_c:chat.example.org:8448",
                "https://matrix.to/#/@a.b_c:chat.example.org:8448".to_string()
            )]
        );
    }

    #[test]
    fn not_addresses() {
        for text in [
            "@john said hi",
            "@john: see above",
            "user@localhost",
            "x@1.2.3.4",
            "hi@www.x",
            "@decorator.call()",
            "a@b@c.example.com",
            "café@example.com",
            "@foo@bar",
            "foo@.example.com",
            ".@example.com",
        ] {
            assert_eq!(links(text), vec![], "{text}");
        }
    }

    #[test]
    fn a_whole_address_is_recognised() {
        assert_eq!(
            whole_link("@foo@bar.io").as_deref(),
            Some("https://bar.io/@foo")
        );
        assert_eq!(whole_link("foo@bar.io x"), None);
    }
}
