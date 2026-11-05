//! Opt-in diagnostics on stderr, off unless `HYLKI_TUTA_DEBUG=1`. With
//! the variable unset nothing is printed at all. What is printed is the Debug
//! form of an SDK error and the name of the stage it broke, passed through
//! [`redact`] first: addresses, ids, tokens and key bytes never leave the
//! process. Callers pass errors and stage names only, never message content.

use std::{fmt::Debug, sync::LazyLock};

static ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("HYLKI_TUTA_DEBUG").is_some_and(|v| v == "1"));

/// Whether diagnostics are on for this run.
pub fn enabled() -> bool {
    *ENABLED
}

/// Report that `stage` failed with `error`.
pub fn failed(stage: &str, error: &dyn Debug) {
    if enabled() {
        note(&format!("{stage} failed: {error:?}"));
    }
}

/// A `map_err` adapter: report the error against `stage`, then fail with
/// `code`, the only thing the caller ever sees.
pub fn fail<E: Debug>(stage: &'static str, code: &'static str) -> impl FnOnce(E) -> &'static str {
    move |error| {
        failed(stage, &error);
        code
    }
}

/// Print one redacted diagnostic line.
pub fn note(text: &str) {
    if enabled() {
        eprintln!("hylki-tuta: {}", redact(text));
    }
}

/// Characters that make up one redaction unit: enough to keep an address,
/// a base64/base64url token or an id in one piece.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '@' | '.' | '_' | '-' | '+' | '=' | '/' | '%')
}

/// `text` with everything secret-shaped replaced. A word holding an `@` is an
/// address; a word of eight or more characters holding a digit is an id,
/// token or key; a bracketed list of four or more numbers is key or body
/// bytes. Plain words -- type names, error names, service paths -- stay.
pub fn redact(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '[' {
            if let Some(len) = byte_list_len(&chars[i..]) {
                out.push_str("[<bytes>]");
                i += len;
                continue;
            }
        }
        if !is_word(c) {
            out.push(c);
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_word(chars[i]) {
            i += 1;
        }
        let word = &chars[start..i];
        if word.contains(&'@') {
            out.push_str("<address>");
        } else if word.len() >= 8 && word.iter().any(char::is_ascii_digit) {
            out.push_str("<redacted>");
        } else {
            out.extend(word);
        }
    }
    out
}

/// The length of a `[n, n, n, n, ...]` list at the start of `chars`, when it
/// holds at least four numbers and nothing else.
fn byte_list_len(chars: &[char]) -> Option<usize> {
    let end = chars.iter().position(|&c| c == ']')?;
    let inner = &chars[1..end];
    let is_numbers = inner
        .iter()
        .all(|c| c.is_ascii_digit() || matches!(c, ',' | ' '));
    let commas = inner.iter().filter(|&&c| c == ',').count();
    (is_numbers && commas >= 3).then_some(end + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_ids_and_key_bytes_are_redacted() {
        assert_eq!(
            redact("InternalSdkError { error_message: \"no group for ada.b+x@tuta.com\" }"),
            "InternalSdkError { error_message: \"no group for <address>\" }"
        );
        assert_eq!(
            redact("token Ab3dEf9hIjK_lm-n/op+q== left"),
            "token <redacted> left"
        );
        assert_eq!(
            redact("mail O1sd9dd----0/O1sd9e2--3-0 missing"),
            "mail <redacted> missing"
        );
        assert_eq!(redact("key [12, 200, 3, 44, 5]"), "key [<bytes>]");
    }

    #[test]
    fn error_names_and_paths_survive() {
        let text = "POST /rest/tutanota/senddraftservice -> 412 ServerResponseError { \
                    source: PreconditionFailedError(None) } [1, 2]";
        assert_eq!(redact(text), text);
        assert_eq!(redact("Aes256 v2 115"), "Aes256 v2 115");
    }
}
