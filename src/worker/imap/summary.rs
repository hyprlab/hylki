//! Message summaries from a FETCH reply: ENVELOPE when the server's replies
//! parse, the raw headers when they do not (#226).

use async_imap::types::{Fetch, Flag};

use crate::models::Message;
use crate::worker::headers::{bytes_to_string, format_date, format_timestamp, unescape_quoted};
use crate::worker::ids::{merge_msgids, normalize_msgid, normalize_msgids};
use crate::worker::imap::keywords::{custom_flags, FORWARDED_KEYWORD};
use crate::worker::imap::structure::structure_has_attachment;
use crate::worker::summary::{mp_importance, mp_thread_ids};
use crate::worker::{clean_display_name, decode_header};

/// Build a message summary from a raw header block (mail-parser), for servers
/// whose structured ENVELOPE our IMAP parser can't handle.
pub(super) fn summary_from_headers(account_id: u32, fetch: &Fetch, folder_id: u32) -> Message {
    use mail_parser::MessageParser;

    let uid = fetch.uid.unwrap_or(0);
    let flags: Vec<Flag> = fetch.flags().collect();
    let unread = !flags.iter().any(|f| matches!(f, Flag::Seen));
    let starred = flags.iter().any(|f| matches!(f, Flag::Flagged));
    let answered = flags.iter().any(|f| matches!(f, Flag::Answered));
    let keywords = custom_flags(&flags);
    let forwarded = keywords
        .iter()
        .any(|k| k.eq_ignore_ascii_case(FORWARDED_KEYWORD));

    let raw = fetch.header().unwrap_or(&[]);
    let parsed = MessageParser::default().parse(raw);

    let mp_first = |a: Option<&mail_parser::Address>| -> (String, String) {
        a.and_then(|a| a.first())
            .map(|addr| {
                let email = addr.address().unwrap_or_default().to_string();
                let name = addr.name().map(|s| s.to_string()).filter(|s| !s.is_empty());
                (name.unwrap_or_else(|| email.clone()), email)
            })
            .unwrap_or_else(|| ("Unknown".to_string(), String::new()))
    };
    let mp_list = |a: Option<&mail_parser::Address>| -> String {
        a.map(|a| {
            a.iter()
                .filter_map(|addr| addr.address().map(|s| s.to_string()))
                .filter(|e| !e.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
    };

    let (from_name, from_addr) = parsed
        .as_ref()
        .map(|p| mp_first(p.from()))
        .unwrap_or_else(|| ("Unknown".to_string(), String::new()));
    let subject = parsed
        .as_ref()
        .and_then(|p| p.subject())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(no subject)".to_string());
    let (date, timestamp) = parsed
        .as_ref()
        .and_then(|p| p.date())
        .map(|d| {
            let ts = d.to_timestamp();
            (format_timestamp(ts), ts)
        })
        .filter(|(_, ts)| *ts > 0)
        .unwrap_or_else(|| internal_date_summary(fetch));
    let to = parsed.as_ref().map(|p| mp_list(p.to())).unwrap_or_default();
    let cc = parsed.as_ref().map(|p| mp_list(p.cc())).unwrap_or_default();
    let bcc = parsed
        .as_ref()
        .map(|p| mp_list(p.bcc()))
        .unwrap_or_default();
    let reply_to = parsed
        .as_ref()
        .map(|p| mp_list(p.reply_to()))
        .unwrap_or_default();
    let reply_to = if reply_to.eq_ignore_ascii_case(&from_addr) {
        String::new()
    } else {
        reply_to
    };
    let reply_to_name = crate::worker::headers::reply_to_display_name(
        &reply_to,
        parsed
            .as_ref()
            .and_then(|p| p.reply_to())
            .and_then(|a| a.first())
            .and_then(|a| a.name()),
    );

    // Best-effort attachment hint from the top-level Content-Type (BODYSTRUCTURE
    // isn't available on this path). multipart/mixed is the usual attachment case.
    let has_attachment = parsed
        .as_ref()
        .and_then(|p| p.header("Content-Type"))
        .and_then(|h| h.as_content_type())
        .map(|ct| {
            ct.ctype().eq_ignore_ascii_case("multipart")
                && ct
                    .subtype()
                    .is_some_and(|s| s.eq_ignore_ascii_case("mixed"))
        })
        .unwrap_or(false);

    let (message_id, references) = mp_thread_ids(parsed.as_ref());

    let mut msg = Message {
        id: uid,
        account_id,
        folder_id,
        uid,
        from_name,
        from_addr,
        reply_to,
        reply_to_name,
        to,
        cc,
        bcc,
        subject,
        preview: String::new(),
        body: String::new(),
        date,
        timestamp,
        unread,
        starred,
        answered,
        forwarded,
        keywords,
        has_attachment,
        message_id,
        references,
        importance: mp_importance(parsed.as_ref()),
        due: 0,
        labels: Vec::new(),
    };
    msg.scrub_nuls();
    msg
}

pub(super) fn build_summary(account_id: u32, fetch: &Fetch, folder_id: u32) -> Message {
    let uid = fetch.uid.unwrap_or(0);
    let flags: Vec<Flag> = fetch.flags().collect();
    let unread = !flags.iter().any(|f| matches!(f, Flag::Seen));
    let starred = flags.iter().any(|f| matches!(f, Flag::Flagged));
    let answered = flags.iter().any(|f| matches!(f, Flag::Answered));
    let keywords = custom_flags(&flags);
    let forwarded = keywords
        .iter()
        .any(|k| k.eq_ignore_ascii_case(FORWARDED_KEYWORD));

    let env = fetch.envelope();
    let (from_name, from_addr) = env
        .and_then(|e| e.from.as_ref())
        .and_then(|v| v.first())
        .map(address_parts)
        .unwrap_or_else(|| ("Unknown".to_string(), String::new()));
    let subject = env
        .and_then(|e| e.subject.as_deref())
        .map(|s| decode_header(&unescape_quoted(s)))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(no subject)".to_string());
    let (date, timestamp) = env
        .and_then(|e| e.date.as_deref())
        .map(format_date)
        .filter(|(_, ts)| *ts > 0)
        .unwrap_or_else(|| internal_date_summary(fetch));
    let to = address_list(env.and_then(|e| e.to.as_ref()));
    let cc = address_list(env.and_then(|e| e.cc.as_ref()));
    // Only a sender's own copy carries one (#476).
    let bcc = address_list(env.and_then(|e| e.bcc.as_ref()));
    // Where the sender asked replies to go. Servers echo From here when the
    // header is absent; storing that echo would be noise, so keep it only
    // when it actually differs.
    let reply_to = address_list(env.and_then(|e| e.reply_to.as_ref()));
    let reply_to = if reply_to.eq_ignore_ascii_case(&from_addr) {
        String::new()
    } else {
        reply_to
    };
    let reply_to_name = crate::worker::headers::reply_to_display_name(
        &reply_to,
        env.and_then(|e| e.reply_to.as_ref())
            .and_then(|v| v.first())
            .map(address_parts)
            .map(|(name, _)| name)
            .as_deref(),
    );

    let has_attachment = fetch
        .bodystructure()
        .map(structure_has_attachment)
        .unwrap_or(false);

    // Message-ID + References + In-Reply-To drive accurate threading. References
    // isn't in the ENVELOPE, so it rides along as its own header fetch
    // ([`REFS_FETCH_ITEM`]) — without it only the immediate parent is known, and
    // threads break wherever that parent lives in another folder.
    let message_id = env
        .and_then(|e| e.message_id.as_deref())
        .map(normalize_msgid)
        .unwrap_or_default();
    let in_reply_to = env
        .and_then(|e| e.in_reply_to.as_deref())
        .map(normalize_msgids)
        .unwrap_or_default();
    let references = merge_msgids(&references_of(fetch), &in_reply_to);

    let mut msg = Message {
        id: uid,
        account_id,
        folder_id,
        uid,
        from_name,
        from_addr,
        reply_to,
        reply_to_name,
        to,
        cc,
        bcc,
        subject,
        preview: String::new(),
        body: String::new(),
        date,
        timestamp,
        unread,
        starred,
        answered,
        forwarded,
        keywords,
        has_attachment,
        message_id,
        references,
        importance: importance_of(fetch),
        due: 0,
        labels: Vec::new(),
    };
    msg.scrub_nuls();
    msg
}

/// The headers of a `HEADER.FIELDS` block, unfolded, as (name, value).
///
/// What comes back is a small header block (the requested headers that the
/// message has, each folded across lines like any other, terminated by a
/// blank line) or nothing at all.
fn header_fields(raw: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(raw);
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = out.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            out.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    out
}

/// The headers a fetch that asked for [`REFS_FETCH_ITEM`] brought back.
pub(super) fn fetched_fields(fetch: &Fetch) -> Vec<(String, String)> {
    use async_imap::imap_proto::types::{MessageSection, SectionPath};
    fetch
        .section(&SectionPath::Full(MessageSection::Header))
        .map(header_fields)
        .unwrap_or_default()
}

/// The `References:` value from a fetch that asked for [`REFS_FETCH_ITEM`],
/// normalized into the same space-separated form as the ENVELOPE ids.
pub(super) fn references_of(fetch: &Fetch) -> String {
    fetched_fields(fetch)
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("references"))
        .map(|(_, value)| normalize_msgids(value.as_bytes()))
        .unwrap_or_default()
}

/// The importance the priority headers of a [`REFS_FETCH_ITEM`] fetch give.
fn importance_of(fetch: &Fetch) -> crate::models::Importance {
    let fields = fetched_fields(fetch);
    crate::models::Importance::from_headers(|name| {
        fields
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    })
}

/// The `Message-ID:` value from a fetch that asked for
/// `HEADER.FIELDS (MESSAGE-ID)`, normalized like the ENVELOPE ids so it
/// compares equal to the ids the app stores.
pub(super) fn message_id_of(fetch: &Fetch) -> String {
    use async_imap::imap_proto::types::{MessageSection, SectionPath};
    let Some(raw) = fetch.section(&SectionPath::Full(MessageSection::Header)) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(raw);
    let Some((name, value)) = text.split_once(':') else {
        return String::new();
    };
    if !name.trim().eq_ignore_ascii_case("message-id") {
        return String::new();
    }
    normalize_msgid(value.as_bytes())
}

/// Join an envelope address list into "a@x.com, b@y.com" (emails only).
fn address_list(addrs: Option<&Vec<async_imap::imap_proto::types::Address>>) -> String {
    addrs
        .map(|v| {
            v.iter()
                .map(|a| address_parts(a).1)
                .filter(|e| !e.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn address_parts(addr: &async_imap::imap_proto::types::Address) -> (String, String) {
    let mailbox = addr
        .mailbox
        .as_deref()
        .map(bytes_to_string)
        .unwrap_or_default();
    let host = addr
        .host
        .as_deref()
        .map(bytes_to_string)
        .unwrap_or_default();
    let email = if host.is_empty() {
        mailbox.clone()
    } else {
        format!("{mailbox}@{host}")
    };
    let name = addr
        .name
        .as_deref()
        .map(|n| clean_display_name(&decode_header(&unescape_quoted(n))))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| email.clone());
    (name, email)
}

/// Fall back to the server's `INTERNALDATE` (the delivery date) when a message
/// carries no parseable `Date:` header. Some senders omit `Date:` entirely,
/// which would otherwise leave the row with a blank date label and a zero sort
/// timestamp (sinking it to the bottom of the list). Returns `("", 0)` if the
/// server didn't supply an INTERNALDATE either.
fn internal_date_summary(fetch: &Fetch) -> (String, i64) {
    match fetch.internal_date() {
        Some(dt) => {
            let ts = dt.timestamp();
            (format_timestamp(ts), ts)
        }
        None => (String::new(), 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_name_with_the_headers_quotes_parses_clean() {
        use async_imap::imap_proto::{AttributeValue, Response};
        let line = b"* 1 FETCH (ENVELOPE (NIL \"S\" ((\"\\\"Sender Name\\\"\" NIL \"some\" \"mail.tld\")) \
    NIL NIL NIL NIL NIL NIL NIL))\r\n";
        let (_, resp) = async_imap::imap_proto::parser::parse_response(line).unwrap();
        let Response::Fetch(_, attrs) = resp else {
            panic!("not a FETCH")
        };
        let Some(AttributeValue::Envelope(env)) = attrs.first() else {
            panic!("no ENVELOPE")
        };
        let from = env.from.as_ref().unwrap();
        assert_eq!(
            address_parts(&from[0]),
            ("Sender Name".to_string(), "some@mail.tld".to_string())
        );
    }

    #[test]
    fn header_fields_unfold_each_requested_header() {
        // #334: References and the priority headers come back in one block.
        let raw =
            b"References: <a@x>\r\n <b@y>\r\nX-Priority: 1 (Highest)\r\nImportance: high\r\n\r\n";
        let fields = header_fields(raw);
        assert_eq!(fields.len(), 3);
        assert_eq!(
            fields[0],
            ("References".to_string(), "<a@x> <b@y>".to_string())
        );
        assert_eq!(normalize_msgids(fields[0].1.as_bytes()), "a@x b@y");
        assert_eq!(
            fields[1],
            ("X-Priority".to_string(), "1 (Highest)".to_string())
        );
        assert!(header_fields(b"").is_empty());
    }

    #[test]
    fn header_references_are_normalized_like_envelope_ids() {
        // Folded exactly as a server sends it, angle brackets and mixed case.
        assert_eq!(
            normalize_msgids(b"<A@x>\r\n <b@Y>\r\n"),
            "a@x b@y",
            "ids should be unwrapped, lowercased and space-joined"
        );
    }
}
