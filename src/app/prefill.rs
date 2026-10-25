//! What a composer opens with when it answers, forwards or resumes a message:
//! recipients, subject, threading headers and the quoted original.

use crate::i18n::{i18n, i18n_f};
use crate::models::Message;
use crate::ui::compose::ComposePrefill;

use crate::mail::attribution::attribution;
use crate::mail::quote::{body_is_plain_text, escaped_lines, quoted_original};

/// A reply's subject: the original's, with one "Re:" in front.
pub(super) fn reply_subject(subject: &str) -> String {
    if subject.to_lowercase().starts_with("re:") {
        subject.to_string()
    } else {
        format!("Re: {subject}")
    }
}

/// `header` is the line above the quote as Settings has it, "" for the
/// default (#528).
pub(super) fn reply_prefill(m: &Message, header: &str) -> ComposePrefill {
    let subject = reply_subject(&m.subject);
    let attribution = attribution(header, &m.from_name, &m.from_addr, m.timestamp, &m.date);
    ComposePrefill {
        // A Reply-To header is the sender saying "answer me here instead" —
        // a mailing list, a no-reply address with a monitored counterpart. It
        // wins over the From address whenever it is present.
        to: if m.reply_to.is_empty() {
            m.from_addr.clone()
        } else {
            m.reply_to.clone()
        },
        cc: String::new(),
        subject,
        // Who the original went to, so a mail addressed to one of the
        // account's send-as aliases is answered from that alias (#34).
        reply_addressed_to: format!("{}, {}", m.to, m.cc),
        // The original is quoted as it was written, the way a forward quotes
        // it (#52). Flattening it to text collapsed every level of quoting
        // into one bar, so an answer to an answer lost the shape of the
        // conversation it was answering (#248).
        body_html: quoted_original(&attribution, &m.body),
        original_plain: body_is_plain_text(&m.body),
        // What makes this a reply rather than a new conversation: In-Reply-To
        // names the parent, References carries the chain it belongs to.
        in_reply_to: m.message_id.clone(),
        references: thread_chain(m),
        ..Default::default()
    }
}

/// Reply-all: To = the original sender (or their Reply-To); Cc = every other
/// recipient of the original, de-duplicated, without the sender and without
/// any of `own`, the account's addresses (#448). An answer to a message the
/// account sent goes to that message's recipients instead, as other mail
/// programs do: answering yourself is never what Reply All means.
pub(super) fn reply_all_prefill(m: &Message, own: &[String], header: &str) -> ComposePrefill {
    let mut prefill = reply_prefill(m, header);
    let split = |list: &str| -> Vec<String> {
        list.split(',')
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_string)
            .collect()
    };
    let mut to: Vec<String> = Vec::new();
    let mut cc: Vec<String> = Vec::new();
    let push = |list: &mut Vec<String>, a: &str, taken: &[String]| {
        let b = bare_address(a);
        if !taken
            .iter()
            .chain(list.iter())
            .any(|x| bare_address(x) == b)
        {
            list.push(a.to_string());
        }
    };
    if is_own_address(&m.from_addr, own) {
        for a in split(&m.to) {
            if !is_own_address(&a, own) {
                push(&mut to, &a, &[]);
            }
        }
        for a in split(&m.cc) {
            if !is_own_address(&a, own) {
                push(&mut cc, &a, &to);
            }
        }
        if !to.is_empty() {
            prefill.to = to.join(", ");
        } else if !cc.is_empty() {
            prefill.to = cc.remove(0);
        }
        prefill.cc = cc.join(", ");
        return prefill;
    }
    // Whoever is already in To (the Reply-To address when one exists) must not
    // be repeated in Cc.
    let to_now = split(&prefill.to);
    for a in split(&m.to).into_iter().chain(split(&m.cc)) {
        if is_own_address(&a, own) || bare_address(&a) == bare_address(&m.from_addr) {
            continue;
        }
        push(&mut cc, &a, &to_now);
    }
    prefill.cc = cc.join(", ");
    prefill
}

/// The References chain for a reply to `m`: whatever `m` already referenced,
/// with `m` itself appended, de-duplicated and in order.
fn thread_chain(m: &Message) -> String {
    let mut chain: Vec<&str> = Vec::new();
    for id in m
        .references
        .split_whitespace()
        .chain(std::iter::once(m.message_id.as_str()))
    {
        if !id.is_empty() && !chain.contains(&id) {
            chain.push(id);
        }
    }
    chain.join(" ")
}

/// The address in a recipient as written ("Ana <ana@x.org>" or the bare
/// address), lowercased.
fn bare_address(recipient: &str) -> String {
    let r = recipient.trim();
    let addr = match (r.rfind('<'), r.rfind('>')) {
        (Some(a), Some(b)) if a < b => &r[a + 1..b],
        _ => r,
    };
    addr.trim().trim_matches('"').to_lowercase()
}

/// Whether `recipient` is one of `own` (the account's address and its
/// send-as aliases), a `+tag` form of one included.
fn is_own_address(recipient: &str, own: &[String]) -> bool {
    let addr = bare_address(recipient);
    if addr.is_empty() {
        return false;
    }
    let untagged = match (addr.split_once('@'), addr.find('+')) {
        (Some((local, domain)), Some(_)) => {
            format!("{}@{domain}", local.split('+').next().unwrap_or(local))
        }
        _ => addr.clone(),
    };
    own.iter()
        .map(|o| bare_address(o))
        .any(|o| !o.is_empty() && (o == addr || o == untagged))
}

/// A forward's subject: the original's, with one "Fwd:" in front.
pub(super) fn forward_subject(subject: &str) -> String {
    if subject.to_lowercase().starts_with("fwd:") {
        subject.to_string()
    } else {
        format!("Fwd: {subject}")
    }
}

/// The header a forward quotes the original under, in the app's language
/// (#528). The dashes stay outside the translation: the composer's scripts
/// find the header by them.
pub(super) fn forward_header(m: &Message) -> String {
    let from = if m.from_name.trim().is_empty() {
        m.from_addr.clone()
    } else {
        format!("{} <{}>", m.from_name, m.from_addr)
    };
    [
        format!("---------- {} ----------", i18n("Forwarded message")),
        i18n_f("From: {from}", &[("from", &from)]),
        i18n_f("Date: {date}", &[("date", &m.datetime_full())]),
        i18n_f("Subject: {subject}", &[("subject", &m.subject)]),
    ]
    .join("\n")
}

pub(super) fn forward_prefill(m: &Message) -> ComposePrefill {
    let subject = forward_subject(&m.subject);
    let header = forward_header(m);
    // Forward the body with its formatting, sanitized (issue #52), so a
    // forwarded invoice still looks like the invoice.
    let body_html = quoted_original(&header, &m.body);
    ComposePrefill {
        to: String::new(),
        cc: String::new(),
        subject,
        body_html,
        original_plain: body_is_plain_text(&m.body),
        forwards: m.message_id.clone(),
        ..Default::default()
    }
}

/// A message handed back from Send, as a composer's prefill: by the worker
/// when nothing could keep it (#340), or by Undo Send (#434). A file that can
/// no longer be read, the usual reason for the first, is left out: the error
/// just shown names it.
pub(super) fn unsent_prefill(m: crate::worker::OutgoingMessage) -> ComposePrefill {
    let body_html = if m.html.trim().is_empty() {
        m.body
            .split("\n\n")
            .map(|p| format!("<p>{}</p>", escaped_lines(p)))
            .collect()
    } else {
        m.html
    };
    let attachments = m
        .attachments
        .iter()
        .map(std::path::PathBuf::from)
        .filter(|p| std::fs::File::open(p).is_ok())
        .collect();
    ComposePrefill {
        to: m.to,
        cc: m.cc,
        bcc: m.bcc,
        subject: m.subject,
        body_html,
        attachments,
        in_reply_to: m.in_reply_to,
        references: m.references,
        forwards: m.forwards,
        draft_origin: m.draft_origin,
        encrypt: m.encrypt,
        outbox_origin: m.outbox_origin,
        reply_addressed_to: String::new(),
        from_address: m
            .from_alias
            .as_deref()
            .map(|f| crate::config::split_identity(f).1)
            .unwrap_or_default(),
        custom_from: m.from_alias.clone().unwrap_or_default(),
        send_at: m.send_at,
        cloud_uploads: Vec::new(),
        inline_files: Vec::new(),
        block_remote_images: false,
        resumed: true,
        no_signature: false,
        // A template that failed to save comes back cut loose from it.
        template: false,
        original_plain: false,
    }
}

/// The first `(name, address)` of a recipient list or identity, as the
/// worker parses them.
pub(super) fn first_recipient(list: &str) -> Option<(String, String)> {
    crate::worker::parse_recipients(list).into_iter().next()
}

/// Write attachments out to a private temp directory: the composer attaches
/// files by path, and these exist only as bytes — in a queued message's
/// stored MIME, or in a cached message being copied.
pub(super) fn stage_attachments(
    dir_name: &str,
    items: &[crate::models::Attachment],
) -> Vec<std::path::PathBuf> {
    if items.is_empty() {
        return Vec::new();
    }
    let dir = std::env::temp_dir().join(dir_name);
    if std::fs::create_dir_all(&dir).is_err() {
        return Vec::new();
    }
    let mut staged = Vec::new();
    for (i, att) in items.iter().enumerate() {
        // The name came out of a message header; keep it to a single
        // path component.
        let safe = att.name.replace(['/', '\\'], "_");
        let name = if safe.trim().is_empty() {
            format!("attachment-{}", i + 1)
        } else {
            safe
        };
        let path = dir.join(&name);
        match std::fs::write(&path, &att.data) {
            Ok(()) => staged.push(path),
            Err(e) => tracing::warn!("could not stage {name} for editing: {e}"),
        }
    }
    staged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::msg;

    /// The forward header carries the whole date, not the list's label,
    /// which for today's mail is only a time (#528).
    #[test]
    fn a_forward_names_the_full_date() {
        let mut m = msg(1, 1, 10);
        m.date = "14:03".into();
        m.timestamp = 1_790_000_000;
        m.from_addr = "ada@example.com".into();
        m.subject = "Plans".into();
        let full = crate::datefmt::date_time(m.timestamp);
        m.from_name = "Ada".into();
        assert_eq!(
            forward_header(&m),
            format!(
                "---------- Forwarded message ----------\nFrom: Ada <ada@example.com>\n\
                 Date: {full}\nSubject: Plans"
            )
        );
    }

    /// A sender's Reply-To wins over their From address, and Reply All must
    /// not smuggle the sender back in through Cc.
    #[test]
    fn reply_goes_where_the_sender_asked() {
        let mut m = msg(1, 1, 10);
        m.from_addr = "no-reply@list.example".into();
        m.reply_to = "editor@example.com".into();
        m.to = "me@example.com, other@example.com".into();

        let r = reply_prefill(&m, "");
        assert_eq!(r.to, "editor@example.com");

        let ra = reply_all_prefill(&m, &["me@example.com".to_string()], "");
        assert_eq!(ra.to, "editor@example.com");
        assert_eq!(
            ra.cc, "other@example.com",
            "no sender, no self, no repeat of To"
        );
    }

    /// Reply All leaves out every address of the account (#448): an alias,
    /// a "Name <address>" form, a +tag form and another spelling alike.
    #[test]
    fn reply_all_leaves_out_every_own_address() {
        let own = vec![
            "me@example.com".to_string(),
            "Me Alias <alias@example.org>".to_string(),
        ];
        let mut m = msg(1, 1, 10);
        m.from_addr = "ada@example.com".into();
        m.to = "Me <ME@example.com>, alias@example.org, bob@example.com".into();
        m.cc = "me+lists@example.com, carol@example.com, Bob <bob@example.com>".into();
        let ra = reply_all_prefill(&m, &own, "");
        assert_eq!(ra.to, "ada@example.com");
        assert_eq!(ra.cc, "bob@example.com, carol@example.com");
    }

    /// Reply All to a message the account sent answers its recipients, not
    /// the account itself.
    #[test]
    fn reply_all_to_your_own_message_answers_its_recipients() {
        let own = vec!["me@example.com".to_string()];
        let mut m = msg(1, 1, 10);
        m.from_addr = "me@example.com".into();
        m.to = "ada@example.com, me@example.com".into();
        m.cc = "bob@example.com".into();
        let ra = reply_all_prefill(&m, &own, "");
        assert_eq!(ra.to, "ada@example.com");
        assert_eq!(ra.cc, "bob@example.com");
        // Sent only to Cc: the first of them takes To.
        m.to = String::new();
        m.cc = "bob@example.com, carol@example.com".into();
        let ra = reply_all_prefill(&m, &own, "");
        assert_eq!(
            (ra.to.as_str(), ra.cc.as_str()),
            ("bob@example.com", "carol@example.com")
        );
    }

    /// Without a Reply-To the sender's own address is still the reply target.
    #[test]
    fn reply_falls_back_to_the_sender() {
        let mut m = msg(1, 1, 10);
        m.from_addr = "ada@example.com".into();
        assert_eq!(reply_prefill(&m, "").to, "ada@example.com");
    }

    #[test]
    fn a_reply_or_forward_knows_its_original_was_plain_text() {
        let mut m = msg(1, 1, 10);
        m.subject = "Hi".into();
        m.body = crate::worker::extract_body(b"Content-Type: text/plain\r\n\r\nHello\r\n");
        assert!(super::reply_prefill(&m, "").original_plain);
        assert!(super::reply_all_prefill(&m, &["me@example.com".to_string()], "").original_plain);
        assert!(super::forward_prefill(&m).original_plain);
        m.body = "<html><body><p>Hello</p></body></html>".into();
        assert!(!super::reply_prefill(&m, "").original_plain);
        assert!(!super::forward_prefill(&m).original_plain);
    }

    #[test]
    fn reply_subject_adds_one_re_whatever_its_case() {
        assert_eq!(reply_subject("Lunch"), "Re: Lunch");
        assert_eq!(reply_subject("re: Lunch"), "re: Lunch");
        assert_eq!(reply_subject("RE:Lunch"), "RE:Lunch");
        assert_eq!(reply_subject("Fwd: Lunch"), "Re: Fwd: Lunch");
        assert_eq!(reply_subject(""), "Re: ");
    }

    #[test]
    fn thread_chain_appends_the_message_once() {
        let mut m = msg(1, 1, 1);
        m.references = "a@x  b@x a@x".into();
        m.message_id = "b@x".into();
        assert_eq!(thread_chain(&m), "a@x b@x");
        m.message_id = String::new();
        m.references = String::new();
        assert_eq!(thread_chain(&m), "");
    }

    #[test]
    fn bare_address_takes_the_angle_part_lowercased() {
        assert_eq!(bare_address(" Ana <Ana@X.org> "), "ana@x.org");
        assert_eq!(bare_address("\"ana@x.org\""), "ana@x.org");
        assert_eq!(bare_address("<>"), "");
        assert_eq!(bare_address("a>b<c"), "a>b<c");
    }

    #[test]
    fn own_address_matches_aliases_and_plus_tags() {
        let own = vec!["Me <me@x.org>".to_string(), "alias@y.org".to_string()];
        assert!(is_own_address("ME@x.org", &own));
        assert!(is_own_address("me+news@x.org", &own));
        assert!(is_own_address("\"Alias\" <alias@y.org>", &own));
        assert!(!is_own_address("other@x.org", &own));
        assert!(!is_own_address("", &own));
        assert!(!is_own_address("me@x.org", &[String::new()]));
    }

    #[test]
    fn unsent_prefill_paragraphs_split_on_lf_only() {
        let m = crate::worker::OutgoingMessage {
            from_account_id: 1,
            from_alias: Some("Me <me@x.org>".into()),
            to: "a@x".into(),
            cc: String::new(),
            bcc: String::new(),
            reply_to: String::new(),
            subject: "S".into(),
            body: "one\r\n\r\ntwo <b>\nthree\n\nfour".into(),
            html: " ".into(),
            attachments: vec!["/nonexistent/hylki-test-file".into()],
            in_reply_to: String::new(),
            references: String::new(),
            forwards: String::new(),
            draft_origin: None,
            outbox_origin: Some(7),
            sign: false,
            encrypt: true,
            send_at: Some(5),
            calendar: None,
        };
        let p = unsent_prefill(m);
        assert_eq!(
            p.body_html,
            "<p>one\r<br>\r<br>two &lt;b&gt;<br>three</p><p>four</p>"
        );
        assert!(p.attachments.is_empty(), "unreadable files are left out");
        assert_eq!(p.from_address, "me@x.org");
        assert_eq!(p.custom_from, "Me <me@x.org>");
        assert!(p.resumed && p.encrypt && !p.template);
        assert_eq!((p.outbox_origin, p.send_at), (Some(7), Some(5)));
    }
}
