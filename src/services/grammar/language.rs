//! Which language each part of a message is checked in: the spelling
//! languages as the server names them, and the paragraphs of the message
//! grouped by the one each is written in.

/// The server's code for a spelling language ("fr_FR" → "fr-FR"): the same
/// variety when it checks that, else the language, else any variety of it.
/// `None` when it does not check the language at all. With no list from
/// the server the code goes as it is.
pub(super) fn server_code(spelling: &str, supported: &[String]) -> Option<String> {
    let code = spelling
        .split(['.', '@'])
        .next()
        .unwrap_or_default()
        .trim()
        .replace('_', "-");
    if code.is_empty() {
        return None;
    }
    if supported.is_empty() {
        return Some(code);
    }
    let base = code
        .split('-')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    supported
        .iter()
        .find(|s| s.eq_ignore_ascii_case(&code))
        .or_else(|| supported.iter().find(|s| s.eq_ignore_ascii_case(&base)))
        .or_else(|| {
            supported
                .iter()
                .find(|s| s.to_ascii_lowercase().starts_with(&format!("{base}-")))
        })
        .cloned()
}

/// A run of the text checked in one language, and where it starts in
/// UTF-16 code units.
#[derive(Debug, PartialEq)]
pub(super) struct Piece<'a> {
    pub(super) offset: usize,
    pub(super) text: &'a str,
    pub(super) language: String,
}

/// `text` cut into runs of paragraphs in one language each, of `max`
/// characters at most unless a single paragraph is longer. With one
/// language that is the language of every run; with none, or with
/// paragraphs too short to tell, the server is asked to work it out.
pub(super) fn pieces<'a>(text: &'a str, codes: &[String], max: usize) -> Vec<Piece<'a>> {
    let blocks = paragraphs(text);
    let mut languages: Vec<Option<String>> = blocks
        .iter()
        .map(|&(a, b)| match codes {
            [] => None,
            [one] => Some(one.clone()),
            _ => detect(&text[a..b], codes),
        })
        .collect();
    // A paragraph too short to tell ("Thanks,") goes with the one before
    // it, or the first one after it at the start of the message.
    let mut last = languages.iter().flatten().next().cloned();
    for l in &mut languages {
        match l {
            Some(known) => last = Some(known.clone()),
            None => l.clone_from(&last),
        }
    }
    let mut out: Vec<Piece> = Vec::new();
    let mut start = 0usize;
    let mut chars = 0usize;
    for (i, &(a, b)) in blocks.iter().enumerate() {
        let language = languages[i].clone().unwrap_or_else(|| "auto".into());
        let len = text[a..b].chars().count();
        let joins = out
            .last()
            .is_some_and(|p| p.language == language && chars + len <= max);
        if joins {
            let p = out.last_mut().expect("checked above");
            p.text = &text[start..b];
            chars += len;
        } else {
            start = a;
            chars = len;
            out.push(Piece {
                offset: text[..a].encode_utf16().count(),
                text: &text[a..b],
                language,
            });
        }
    }
    out.retain(|p| !p.text.trim().is_empty());
    out
}

/// The paragraphs of `text` as byte ranges, each with the blank lines after
/// it, so that together they are the whole text.
fn paragraphs(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' && bytes.get(i + 1) == Some(&b'\n') {
            let mut end = i;
            while end < bytes.len() && bytes[end] == b'\n' {
                end += 1;
            }
            out.push((start, end));
            start = end;
            i = end;
        } else {
            i += 1;
        }
    }
    if start < text.len() {
        out.push((start, text.len()));
    }
    out
}

/// Which of `codes` a paragraph is written in, when it is long enough to
/// tell. Runs offline.
fn detect(paragraph: &str, codes: &[String]) -> Option<String> {
    if paragraph.chars().filter(|c| c.is_alphabetic()).count() < 20 {
        return None;
    }
    let langs: Vec<(whatlang::Lang, &String)> = codes
        .iter()
        .filter_map(|c| Some((lang_of(c)?, c)))
        .collect();
    let mut allow: Vec<whatlang::Lang> = langs.iter().map(|(l, _)| *l).collect();
    allow.dedup();
    if allow.len() < 2 {
        return langs.first().map(|(_, c)| (*c).clone());
    }
    let found = whatlang::Detector::with_allowlist(allow)
        .detect(paragraph)?
        .lang();
    langs
        .iter()
        .find(|(l, _)| *l == found)
        .map(|(_, c)| (*c).clone())
}

/// The detector's language for a server code ("fr-FR").
fn lang_of(code: &str) -> Option<whatlang::Lang> {
    let iso3 = match code.split('-').next()?.to_ascii_lowercase().as_str() {
        "ar" => "ara",
        "be" => "bel",
        "ca" => "cat",
        "da" => "dan",
        "de" => "deu",
        "el" => "ell",
        "en" => "eng",
        "eo" => "epo",
        "es" => "spa",
        "fa" => "pes",
        "fr" => "fra",
        "it" => "ita",
        "ja" => "jpn",
        "km" => "khm",
        "nl" => "nld",
        "pl" => "pol",
        "pt" => "por",
        "ro" => "ron",
        "ru" => "rus",
        "sk" => "slk",
        "sl" => "slv",
        "sv" => "swe",
        "ta" => "tam",
        "tl" => "tgl",
        "uk" => "ukr",
        "zh" => "cmn",
        _ => return None,
    };
    whatlang::Lang::from_code(iso3)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(codes: &[&str]) -> Vec<String> {
        codes.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn spelling_languages_become_the_servers_codes() {
        let supported = list(&[
            "de", "de-DE", "en", "en-US", "en-GB", "es", "fr", "fr-FR", "nl-BE",
        ]);
        assert_eq!(server_code("fr_FR", &supported).as_deref(), Some("fr-FR"));
        assert_eq!(
            server_code("en_GB.UTF-8", &supported).as_deref(),
            Some("en-GB")
        );
        assert_eq!(server_code("es_MX", &supported).as_deref(), Some("es"));
        assert_eq!(server_code("nl", &supported).as_deref(), Some("nl-BE"));
        assert_eq!(server_code("nb_NO", &supported), None);
        assert_eq!(server_code("de_AT", &[]).as_deref(), Some("de-AT"));
    }

    #[test]
    fn each_paragraph_goes_in_its_own_language() {
        let text = "Bonjour à tous, je vous écris au sujet de la réunion de demain matin.\n\n\
                    Merci,\n\n\
                    Hello everyone, I am writing about the meeting tomorrow morning.";
        let got = pieces(text, &list(&["en-US", "fr-FR"]), 15_000);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].language, "fr-FR");
        assert!(got[0].text.ends_with("Merci,\n\n"), "{got:?}");
        assert_eq!(got[1].language, "en-US");
        assert_eq!(
            got[1].offset,
            text[..text.find("Hello").unwrap()].encode_utf16().count()
        );
    }

    #[test]
    fn one_language_is_one_piece_and_none_asks_the_server() {
        let text = "One.\n\nTwo.";
        let got = pieces(text, &list(&["en-US"]), 15_000);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, text);
        assert_eq!(pieces(text, &[], 15_000)[0].language, "auto");
        // Too short to tell between two languages.
        assert_eq!(
            pieces("Merci !", &list(&["en-US", "fr-FR"]), 15_000)[0].language,
            "auto"
        );
    }

    #[test]
    fn long_text_goes_in_several_requests() {
        let text = "aaaa\n\nbbbb\n\ncccc";
        let got = pieces(text, &list(&["en-US"]), 12);
        let texts: Vec<&str> = got.iter().map(|p| p.text).collect();
        assert_eq!(texts, ["aaaa\n\nbbbb\n\n", "cccc"]);
        assert_eq!(got[1].offset, 12);
    }
}
