//! The fetch-mode ladder (#226): the ways of asking a server for summaries,
//! stepped down when its replies cannot be parsed, remembered per account;
//! and the error text such a reply leaves.

use crate::i18n::i18n_f;

/// Accounts whose server answers the summary fetch with something our IMAP
/// parser cannot read once the list-preview items (`BODY.PEEK[1]<0.N>` and
/// `BODY.PEEK[1.MIME]`) ride along (#226, Mailfence). The summary fetch is
/// retried without them (see [`step_fetch_mode`]) and, once it works that
/// way, every preview read for the account is skipped for the session: a
/// reply the parser rejects leaves the connection unusable, and the repair
/// reads ask for the same items.
static PREVIEW_ITEMS_REJECTED: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<u32>>,
> = std::sync::LazyLock::new(Default::default);

/// Whether the server has rejected the preview items for this account.
pub(super) fn previews_rejected(account_id: u32) -> bool {
    PREVIEW_ITEMS_REJECTED
        .lock()
        .map(|r| r.contains(&account_id))
        .unwrap_or(false)
}

/// Whether the account's fetches should read list previews at all: the
/// setting is on and the server has not rejected the preview items.
pub(super) fn inline_previews_wanted(account_id: u32) -> bool {
    (cfg!(test) || crate::config::load_preview_lines() > 0) && !previews_rejected(account_id)
}

/// Start the account at the summary-fetch mode that worked last time
/// (#226), so a server that rejects the default is not failed against on
/// every launch: two unreadable fetches and two reconnects before the list
/// appeared, in the Mailfence log. Returns the `use_envelope` to begin with.
pub(super) fn seed_fetch_mode(account_id: u32, email: &str) -> bool {
    let (use_envelope, rejected) = crate::config::load_fetch_mode(email);
    if rejected {
        if let Ok(mut r) = PREVIEW_ITEMS_REJECTED.lock() {
            r.insert(account_id);
        }
    }
    if !use_envelope || rejected {
        tracing::info!(
            target: "hylki::imap",
            "summary fetch: starting with envelope={use_envelope} previews={} (remembered)",
            !rejected
        );
    }
    use_envelope
}

/// A parser error with its byte-array dump cut out: the same reply follows
/// as text, which is the readable half, and the numbers were most of a
/// 160 KB log for one user (#226).
pub(super) fn compact_parse_error(text: &str) -> String {
    let Some(start) = text.find("input: [") else {
        return text.to_string();
    };
    let Some(end) = text[start..].find(']').map(|i| start + i + 1) else {
        return text.to_string();
    };
    format!(
        "{}input: [{} bytes]{}",
        &text[..start],
        text[start + 8..end - 1].split(',').count(),
        &text[end..]
    )
}

/// Whether the error is the IMAP parser giving up on a server's reply, as
/// opposed to the connection dropping or the server saying NO.
pub(super) fn is_parse_error(e: &async_imap::error::Error) -> bool {
    match e {
        async_imap::error::Error::Parse(_) => true,
        async_imap::error::Error::Io(io) => io.to_string().contains(" during parsing of "),
        _ => false,
    }
}

/// After a summary fetch whose reply the parser rejected, move to the next
/// way of asking for the same summaries. Returns false once every way has
/// been tried. The order keeps the structured ENVELOPE as long as possible
/// and previews as long as possible:
///
/// 1. ENVELOPE + BODYSTRUCTURE with previews (the default)
/// 2. raw headers with previews (iCloud's non-compliant ENVELOPE)
/// 3. ENVELOPE + BODYSTRUCTURE without previews (Mailfence, #226: the
///    preview items make its whole FETCH reply unreadable)
/// 4. raw headers without previews
///
/// With previews switched off in Settings, steps 2 and 3 collapse into one.
pub(super) fn step_fetch_mode(account_id: u32, use_envelope: &mut bool) -> bool {
    let previews = inline_previews_wanted(account_id);
    match (*use_envelope, previews) {
        (true, true) => {
            *use_envelope = false;
        }
        (false, true) => {
            if let Ok(mut r) = PREVIEW_ITEMS_REJECTED.lock() {
                r.insert(account_id);
            }
            *use_envelope = true;
        }
        (true, false) => {
            *use_envelope = false;
        }
        (false, false) => return false,
    }
    tracing::info!(
        target: "hylki::imap",
        "summary fetch: reply unreadable; retrying with envelope={} previews={}",
        *use_envelope,
        inline_previews_wanted(account_id)
    );
    true
}

/// A worker error with a parser dump made readable. When a server's reply
/// cannot be read, the IMAP parser's error carries the whole reply twice,
/// as a byte array and as text: ten thousand numbers for one message's
/// headers, filling the status bar (#226). The log keeps all of it (see
/// `AppMsg::Error`); this is what the person sees instead.
pub(crate) fn readable_error(text: &str) -> String {
    const MARK: &str = " during parsing of \"";
    let Some(start) = text.find("Error(Error { input: [") else {
        return text.to_string();
    };
    let Some(reply) = text[start..].find(MARK).map(|i| start + i + MARK.len()) else {
        return text.to_string();
    };
    let head = text[..start].trim_end().trim_end_matches("io:").trim_end();
    let reply = &text[reply..];
    // The reply is the parser's `{:?}` of it, so line ends are the two
    // characters `\r\n`.
    let first_line = reply.split("\\r\\n").next().unwrap_or(reply);
    let mut excerpt: String = first_line.chars().take(96).collect();
    if excerpt.len() < first_line.len() {
        excerpt.push('…');
    }
    let what = i18n_f(
        "the server’s reply could not be read (it began: {reply})",
        &[("reply", &excerpt)],
    );
    if head.is_empty() {
        what
    } else {
        format!("{head} {what}")
    }
}

#[cfg(test)]
mod compact_parse_error_tests {
    use super::compact_parse_error;

    #[test]
    fn drops_the_byte_dump_and_keeps_the_text() {
        let e = "io: Error(Error { input: [42, 32, 55, 57, 56], code: Tag }) during parsing of \"* 798 FETCH\"";
        assert_eq!(
            compact_parse_error(e),
            "io: Error(Error { input: [5 bytes], code: Tag }) during parsing of \"* 798 FETCH\""
        );
        assert_eq!(compact_parse_error("connection lost"), "connection lost");
    }
}

#[cfg(test)]
mod fetch_mode_tests {
    use super::*;

    #[test]
    fn readable_error_keeps_the_first_line_of_the_reply() {
        let text = "Could not load INBOX: io: Error(Error { input: [42, 32, 55, 57, 56, 32, 70], code: Tag }) during parsing of \"* 798 FETCH (FLAGS (\\\\Seen) UID 4321 BODY[HEADER] {9821}\\r\\nReceived: from a\\r\\n\"";
        let out = readable_error(text);
        assert!(
            out.starts_with("Could not load INBOX: the server’s reply could not be read"),
            "{out}"
        );
        assert!(
            out.contains("* 798 FETCH (FLAGS (\\\\Seen) UID 4321 BODY[HEADER] {9821}"),
            "{out}"
        );
        assert!(!out.contains("Received:"), "{out}");
        assert!(!out.contains("42, 32"), "{out}");
    }

    #[test]
    fn readable_error_leaves_other_errors_alone() {
        assert_eq!(
            readable_error("Could not load INBOX: connection lost"),
            "Could not load INBOX: connection lost"
        );
    }

    #[test]
    fn ladder_visits_every_way_of_asking_once() {
        // A fresh account id so the test does not share the set with others.
        let account_id = 900_226;
        let mut use_envelope = true;
        // 1 → 2: raw headers, previews kept.
        assert!(step_fetch_mode(account_id, &mut use_envelope));
        assert!(!use_envelope);
        // 2 → 3: back to ENVELOPE, previews dropped.
        assert!(step_fetch_mode(account_id, &mut use_envelope));
        assert!(use_envelope);
        assert!(!inline_previews_wanted(account_id));
        // 3 → 4: raw headers, still no previews.
        assert!(step_fetch_mode(account_id, &mut use_envelope));
        assert!(!use_envelope);
        // 4: nothing left.
        assert!(!step_fetch_mode(account_id, &mut use_envelope));
        assert!(!use_envelope);
    }
}
