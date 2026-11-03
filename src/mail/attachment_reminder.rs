//! The attachment reminder: a message that says it has something attached
//! but has nothing attached is asked about before it is sent. The words
//! that count are set in Settings → Composing. Pure functions.

/// The words looked for when the user has not changed them, in the
/// languages the app is translated into. Each one matches at the start of
/// a word, so "attach" covers "attached" and "attachment", and "allegat"
/// covers "allegato" and "allegati".
pub(crate) const DEFAULT_KEYWORDS: &str = "attach, enclos, allegat, allego, anhang, angehängt, \
beigefügt, adjunt, ci-joint, pièce jointe, pièces jointes, bijlage, bijgevoegd, anexo, anexa, \
załącz, csatol, συνημμέν";

/// The keywords in a comma-separated `list`, trimmed and lowercased, the
/// empty ones left out.
pub(crate) fn keywords(list: &str) -> Vec<String> {
    list.split(',')
        .map(|k| k.trim().to_lowercase())
        .filter(|k| !k.is_empty())
        .collect()
}

/// `list` as Settings stores and shows it: one ", " between keywords.
pub(crate) fn normalized(list: &str) -> String {
    list.split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The first word of `text` that one of `keywords` starts, as written in
/// `text` (lowercased), or `None`. Quoted lines (starting with `>`) are the
/// original being answered or forwarded, not what the user wrote, so they
/// are left out.
pub(crate) fn mentioned(text: &str, keywords: &[String]) -> Option<String> {
    if keywords.is_empty() {
        return None;
    }
    text.lines()
        .filter(|line| !line.trim_start().starts_with('>'))
        .find_map(|line| mentioned_in_line(&line.to_lowercase(), keywords))
}

/// The word of `line` (already lowercased) one of `keywords` starts.
fn mentioned_in_line(line: &str, keywords: &[String]) -> Option<String> {
    keywords.iter().find_map(|keyword| {
        line.match_indices(keyword.as_str())
            .find(|(at, _)| starts_word(line, *at))
            .map(|(at, _)| word_at(line, at, keyword.len()))
    })
}

/// Whether byte `at` of `line` is the start of a word.
fn starts_word(line: &str, at: usize) -> bool {
    line[..at]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_alphanumeric())
}

/// The word that starts at byte `at` of `line`: the matched keyword
/// (`len` bytes) and the letters that follow it.
fn word_at(line: &str, at: usize, len: usize) -> String {
    let rest = &line[at + len..];
    let end = rest
        .find(|c: char| !c.is_alphanumeric())
        .unwrap_or(rest.len());
    line[at..at + len + end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Vec<String> {
        keywords(DEFAULT_KEYWORDS)
    }

    #[test]
    fn a_keyword_matches_the_start_of_a_word() {
        let k = defaults();
        assert_eq!(
            mentioned("Ti mando in Allegato il preventivo.", &k).as_deref(),
            Some("allegato")
        );
        assert_eq!(
            mentioned("Please find the report attached.", &k).as_deref(),
            Some("attached")
        );
        assert_eq!(
            mentioned("Voir la pièce jointe.", &k).as_deref(),
            Some("pièce jointe")
        );
        assert_eq!(
            mentioned("Im Anhang findest du", &k).as_deref(),
            Some("anhang")
        );
    }

    #[test]
    fn a_keyword_inside_a_word_does_not_match() {
        let k = keywords("attach");
        assert_eq!(mentioned("He reattached nothing.", &k), None);
        assert_eq!(mentioned("Nothing to see here.", &k), None);
    }

    #[test]
    fn quoted_lines_are_left_out() {
        let k = defaults();
        let text =
            "Thanks, got it.\n\nOn Monday, Ada wrote:\n> The file is attached.\n  > > allegato";
        assert_eq!(mentioned(text, &k), None);
    }

    #[test]
    fn a_quoted_original_in_html_is_left_out() {
        let k = defaults();
        let quote = "<p class=\"hylki-quote-attr\">On Monday, Ada wrote:</p>\
            <blockquote><p>The file is attached.</p></blockquote>";
        let text = crate::mail::markdown::html_to_text(&format!("<p>Thanks!</p>{quote}"));
        assert_eq!(mentioned(&text, &k), None);
        let text =
            crate::mail::markdown::html_to_text(&format!("<p>Here it is attached.</p>{quote}"));
        assert_eq!(mentioned(&text, &k).as_deref(), Some("attached"));
    }

    #[test]
    fn no_keywords_finds_nothing() {
        assert_eq!(mentioned("attached", &keywords(" , ,")), None);
        assert_eq!(mentioned("attached", &[]), None);
    }

    #[test]
    fn the_list_is_normalized() {
        assert_eq!(keywords(" Attach ,, Allegat "), vec!["attach", "allegat"]);
        assert_eq!(normalized(" attach ,,allegat , "), "attach, allegat");
        assert_eq!(normalized(DEFAULT_KEYWORDS), DEFAULT_KEYWORDS);
    }
}
