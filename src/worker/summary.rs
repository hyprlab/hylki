//! A message summary (the list row) from a whole raw message, for POP3 and
//! for mail the app already holds, which have no ENVELOPE to read.

use crate::models::Message;
use crate::worker::headers::label_from_timestamp;

/// Extract (message_id, references) from a parsed message for threading. References
/// combines In-Reply-To and References, normalized (no angle brackets, lowercased).
pub(super) fn mp_thread_ids(parsed: Option<&mail_parser::Message>) -> (String, String) {
    use mail_parser::HeaderValue;
    let norm = |s: &str| {
        let exact = s
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .trim();
        crate::cache::note_msgid_case(exact);
        exact.to_ascii_lowercase()
    };
    let collect = |hv: &HeaderValue| -> Vec<String> {
        match hv {
            HeaderValue::Text(t) => vec![norm(t)],
            HeaderValue::TextList(v) => v.iter().map(|t| norm(t)).collect(),
            _ => Vec::new(),
        }
    };
    let Some(p) = parsed else {
        return (String::new(), String::new());
    };
    let message_id = p.message_id().map(norm).unwrap_or_default();
    let mut refs: Vec<String> = Vec::new();
    for id in collect(p.in_reply_to())
        .into_iter()
        .chain(collect(p.references()))
    {
        if !id.is_empty() && !refs.contains(&id) {
            refs.push(id);
        }
    }
    (message_id, refs.join(" "))
}

/// The importance a parsed message's priority headers give.
pub(super) fn mp_importance(parsed: Option<&mail_parser::Message>) -> crate::models::Importance {
    crate::models::Importance::from_headers(|name| parsed.and_then(|p| p.header_raw(name)))
}

/// Build a message summary from a full RFC 822 message (POP3 has no ENVELOPE).
pub(crate) fn summary_from_raw(account_id: u32, folder_id: u32, uid: u32, raw: &[u8]) -> Message {
    use mail_parser::MessageParser;
    let parsed = MessageParser::default().parse(raw);
    let addr = parsed
        .as_ref()
        .and_then(|p| p.from())
        .and_then(|a| a.first());
    let from_addr = addr
        .and_then(|a| a.address())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let from_name = addr
        .and_then(|a| a.name())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            if from_addr.is_empty() {
                "Unknown".to_string()
            } else {
                from_addr.clone()
            }
        });
    let subject = parsed
        .as_ref()
        .and_then(|p| p.subject())
        .filter(|s| !s.is_empty())
        .unwrap_or("(no subject)")
        .to_string();
    let timestamp = parsed
        .as_ref()
        .and_then(|p| p.date())
        .map(|d| d.to_timestamp())
        .unwrap_or(0);
    let date = label_from_timestamp(timestamp);
    let addr_list = |list: Option<&mail_parser::Address>| -> String {
        list.map(|a| {
            a.iter()
                .filter_map(|x| x.address())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
    };
    let to = addr_list(parsed.as_ref().and_then(|p| p.to()));
    let cc = addr_list(parsed.as_ref().and_then(|p| p.cc()));
    let bcc = addr_list(parsed.as_ref().and_then(|p| p.bcc()));
    let reply_to = addr_list(parsed.as_ref().and_then(|p| p.reply_to()));
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
    let has_attachment = parsed
        .as_ref()
        .map(|p| p.attachment_count() > 0)
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
        unread: true,
        starred: false,
        answered: false,
        forwarded: false,
        keywords: Vec::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_ids_list_in_reply_to_before_references() {
        let raw = b"Message-ID: <ABC@Example.COM>\r\n\
    In-Reply-To: <Parent@x>\r\n\
    References: <Root@x> <PARENT@x>\r\n\
    Subject: t\r\n\r\nbody\r\n";
        let parsed = mail_parser::MessageParser::default().parse(&raw[..]);
        assert_eq!(
            mp_thread_ids(parsed.as_ref()),
            ("abc@example.com".to_string(), "parent@x root@x".to_string())
        );
        assert_eq!(mp_thread_ids(None), (String::new(), String::new()));
    }

    #[test]
    fn a_pop3_summary_comes_from_the_raw_message() {
        let raw = b"From: Ann <ann@x.example>\r\n\
    To: b@y.example, c@z.example\r\n\
    Cc: d@w.example\r\n\
    Reply-To: ANN@x.example\r\n\
    Subject: Hello\r\n\
    Date: Tue, 1 Jul 2003 10:52:37 +0200\r\n\
    Message-ID: <M1@x>\r\n\
    X-Priority: 1\r\n\r\nbody\r\n";
        let m = summary_from_raw(3, 1, 42, raw);
        assert_eq!((m.id, m.account_id, m.folder_id, m.uid), (42, 3, 1, 42));
        assert_eq!(m.from_name, "Ann");
        assert_eq!(m.from_addr, "ann@x.example");
        assert_eq!(m.to, "b@y.example, c@z.example");
        assert_eq!(m.cc, "d@w.example");
        // A Reply-To that only repeats From is dropped.
        assert_eq!(m.reply_to, "");
        assert_eq!(m.subject, "Hello");
        assert_eq!(m.timestamp, 1057049557);
        assert_eq!(m.date, label_from_timestamp(1057049557));
        assert_eq!(m.message_id, "m1@x");
        assert!(m.unread);
        assert!(!m.starred && !m.answered && !m.has_attachment);
        assert!(m.preview.is_empty() && m.body.is_empty());

        let bare = summary_from_raw(3, 1, 7, b"\r\nno headers\r\n");
        assert_eq!(bare.from_name, "Unknown");
        assert_eq!(bare.from_addr, "");
        assert_eq!(bare.subject, "(no subject)");
        assert_eq!(bare.timestamp, 0);

        let nameless = summary_from_raw(3, 1, 7, b"From: a@x\r\n\r\n");
        assert_eq!(nameless.from_name, "a@x");
    }

    /// A web form's mail keeps the name of whoever filled it in (#641).
    #[test]
    fn the_first_reply_to_name_is_kept() {
        let raw = b"From: Example Site <no-reply@site.example>\r\n\
    Reply-To: \"Doe, Jane\" <jane@x.example>, desk@site.example\r\n\
    Subject: Contact form\r\n\r\nbody\r\n";
        let m = summary_from_raw(1, 1, 1, raw);
        assert_eq!(m.reply_to, "jane@x.example, desk@site.example");
        assert_eq!(m.reply_to_name, "Doe, Jane");

        let bare = summary_from_raw(
            1,
            1,
            2,
            b"From: Site <no-reply@site.example>\r\nReply-To: jane@x.example\r\n\r\nx\r\n",
        );
        assert_eq!(bare.reply_to, "jane@x.example");
        assert_eq!(bare.reply_to_name, "");

        let echo = summary_from_raw(
            1,
            1,
            3,
            b"From: Ann <ann@x.example>\r\nReply-To: Ann <ann@x.example>\r\n\r\nx\r\n",
        );
        assert_eq!(
            (echo.reply_to.as_str(), echo.reply_to_name.as_str()),
            ("", "")
        );
    }
}
