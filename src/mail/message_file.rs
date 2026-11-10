//! A message kept as a file: a `.eml` saved whole, an Outlook `.msg`, or a
//! message attached to another one (#398, #622).

/// Whether a file name says it holds a message: `.eml`, the usual name for a
/// message saved whole, or Outlook's `.msg`.
pub(crate) fn has_message_suffix(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("eml") || e.eq_ignore_ascii_case("msg"))
}

/// Whether a file is a message to read, by its name or, for a name with no
/// suffix at all, by its first lines. An attached message saved before
/// #622 is called `attachment-1`, and the desktop hands such a file back to
/// the app it sniffed as mail, so the name alone cannot decide.
pub(crate) fn is_message(name: &str, data: &[u8]) -> bool {
    let suffix = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str());
    match suffix {
        Some(e) if e.eq_ignore_ascii_case("eml") => true,
        // Other programs name files .msg too (Tcl's catalogues), so only an
        // Outlook compound file counts.
        Some(e) if e.eq_ignore_ascii_case("msg") => crate::mail::outlook_msg::is_msg(data),
        Some(_) => false,
        None => starts_with_headers(data),
    }
}

/// Whether bytes open with a mail header block that has a `From:` in it.
/// GIO's sniffing knows only a few first lines (`From:`, `Received:`), and an
/// attached message often starts with `MIME-Version:` or `Date:`.
fn starts_with_headers(data: &[u8]) -> bool {
    let head = &data[..data.len().min(16 * 1024)];
    let Ok(head) = std::str::from_utf8(head).or_else(|e| {
        // The cut may fall inside a character; the headers before it are ASCII.
        std::str::from_utf8(&head[..e.valid_up_to()])
    }) else {
        return false;
    };
    let mut fields = 0;
    let mut from = false;
    for line in head.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if fields == 0 {
                return false;
            }
            continue;
        }
        let Some((field, _)) = line.split_once(':') else {
            return false;
        };
        if field.is_empty() || !field.bytes().all(|b| b.is_ascii_graphic()) {
            return false;
        }
        fields += 1;
        from |= field.eq_ignore_ascii_case("from");
    }
    from && fields >= 2
}

/// A file name for a message: its subject, without what a file name cannot
/// hold, and `.eml`.
pub(crate) fn eml_file_name(subject: &str) -> String {
    let stem = eml_stem(subject);
    if stem.is_empty() {
        "message.eml".to_string()
    } else {
        format!("{stem}.eml")
    }
}

fn eml_stem(subject: &str) -> String {
    let stem: String = subject
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    let stem = stem.trim().trim_matches('.');
    stem.chars().take(80).collect()
}

/// Whether a MIME type is a whole message carried as a part.
pub(crate) fn is_message_type(ty: &str, subtype: &str) -> bool {
    ty.eq_ignore_ascii_case("message")
        && (subtype.eq_ignore_ascii_case("rfc822") || subtype.eq_ignore_ascii_case("global"))
}

/// The name an attached message is listed and opened under (#622): the name
/// the sender gave it, with `.eml` added when it has none (Outlook names
/// one by its subject alone), or else its own subject. Without the suffix
/// it shows as a blank file and the desktop cannot tell what opens it.
pub(crate) fn attached_message_name(
    given: Option<&str>,
    subject: Option<&str>,
    n: usize,
) -> String {
    if let Some(given) = given.map(str::trim).filter(|g| !g.is_empty()) {
        return if has_message_suffix(given) {
            given.to_string()
        } else {
            format!("{given}.eml")
        };
    }
    match subject.map(eml_stem).filter(|s| !s.is_empty()) {
        Some(stem) => format!("{stem}.eml"),
        None => format!("attachment-{n}.eml"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_by_subject() {
        assert_eq!(
            eml_file_name("Re: Q3 roadmap / review?"),
            "Re- Q3 roadmap - review-.eml"
        );
        assert_eq!(eml_file_name("   "), "message.eml");
        assert_eq!(eml_file_name("..."), "message.eml");
    }

    #[test]
    fn attached_messages_get_eml() {
        assert_eq!(
            attached_message_name(None, Some("Invoice 42"), 1),
            "Invoice 42.eml"
        );
        assert_eq!(
            attached_message_name(None, Some("  "), 3),
            "attachment-3.eml"
        );
        assert_eq!(attached_message_name(None, None, 2), "attachment-2.eml");
        assert_eq!(
            attached_message_name(Some("Re: lunch"), Some("x"), 1),
            "Re: lunch.eml"
        );
        assert_eq!(
            attached_message_name(Some("saved.EML"), None, 1),
            "saved.EML"
        );
    }

    #[test]
    fn tells_a_message_by_name_or_headers() {
        assert!(is_message("A.EML", b""));
        assert!(!is_message("a.pdf", b"From: a@b\r\nTo: c@d\r\n\r\n"));
        assert!(!is_message("strings.msg", b"msgcat::mcset en hi"));
        assert!(is_message(
            "attachment-1",
            b"MIME-Version: 1.0\r\nDate: Mon, 9 Nov 2026 09:00:00 +0000\r\n\
              From: Ann <ann@example.org>\r\nSubject: hi\r\n\tthere\r\n\r\nbody"
        ));
        assert!(is_message("attachment-1", b"From: a@b\nSubject: s\n\nx"));
        // A header block needs a sender, and a line that is not a field ends it.
        assert!(!is_message(
            "attachment-1",
            b"Date: Mon\r\nSubject: s\r\n\r\n"
        ));
        assert!(!is_message("notes", b"From: me\r\nsome notes here\r\n"));
        assert!(!is_message("attachment-1", b"\x89PNG\r\n"));
        assert!(!is_message("attachment-1", b""));
    }
}
