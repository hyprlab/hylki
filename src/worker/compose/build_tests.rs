use super::*;

use crate::worker::render::attachments::extract_attachments;
use crate::worker::test_support::{sample_account, sample_outgoing};
use crate::worker::CalendarPart;

/// `alt` arrives from outside on any quoted reply and lands in a header,
/// so a quote, a backslash or a control character never reaches one, and
/// a value that is no filename at all names nothing.
#[test]
fn an_alt_that_is_not_a_plain_filename_names_nothing() {
    let html = "<img src='data:image/png;base64,AAAA' alt='%s'>";
    for bad in ["a\"b", "a\\b", "a\nb", "", ".", ".."] {
        let doc = html.replace("%s", bad);
        let (_, imgs) = extract_data_images(&doc);
        assert_eq!(imgs.len(), 1, "{doc}");
        assert_eq!(imgs[0].name.as_deref(), None, "{doc}");
    }
    // A path is not rejected but reduced: only its last segment can name
    // the part, so a traversal cannot escape into the header.
    let (_, ok) =
        extract_data_images("<img src='data:image/png;base64,AAAA' alt='../../etc/report.png'>");
    assert_eq!(ok[0].name.as_deref(), Some("report.png"));
}

/// The lift leaves anything that is not a well-formed quoted base64 image
/// data URI exactly as written.
#[test]
fn extract_data_images_leaves_odd_uris_alone() {
    let html = "<p>data:image/png is a scheme</p><img src=\"data:image/png;base65,zzz\">";
    let (out, imgs) = extract_data_images(html);
    assert_eq!(out, html);
    assert!(imgs.is_empty());

    // Single-quoted attributes lift too.
    let (out, imgs) = extract_data_images("<img src='data:image/jpeg;base64,AAAA'>");
    assert!(out.contains("src='cid:"), "{out}");
    assert_eq!(imgs.len(), 1);
    assert_eq!(imgs[0].mime, "image/jpeg");
    assert_eq!(imgs[0].data, vec![0, 0, 0]);
}

/// A draft saved before any address is typed: the bytes build without a
/// To header, and the send path still refuses the same message.
#[test]
fn recipient_less_draft_builds_but_send_does_not() {
    let account = sample_account();
    let mut msg = sample_outgoing();
    msg.body = "half-written".into();
    let raw = build_draft(&account, &msg)
        .expect("draft builds")
        .formatted();
    let text = String::from_utf8_lossy(&raw);
    assert!(!text.contains("\r\nTo:"), "no To header: {text}");
    assert!(text.contains("half-written"), "{text}");
    let err = build_email(&account, &msg)
        .expect_err("send needs a recipient")
        .to_string();
    assert!(err.contains("missing destination"), "{err}");
    // With a recipient the draft is built the ordinary way.
    msg.to = "Peer <peer@hylki.invalid>".into();
    let raw = build_draft(&account, &msg).expect("builds").formatted();
    assert!(String::from_utf8_lossy(&raw).contains("peer@hylki.invalid"));
}

/// A draft or template written for a mail merge (#447) keeps the fields
/// in To and Cc, which read back as written; a send still refuses them.
#[test]
fn a_merge_draft_keeps_its_fields() {
    let account = sample_account();
    let mut msg = sample_outgoing();
    msg.to = "{{First Name}} <{{Email}}>".into();
    msg.cc = "{{Boss}}, ann@example.com".into();
    let raw = build_draft(&account, &msg)
        .expect("draft builds")
        .formatted();
    let parsed = mail_parser::MessageParser::default()
        .parse(&raw)
        .expect("parses");
    let field = |list: Option<&mail_parser::Address>| {
        let list: Vec<String> = list
            .map(|a| {
                a.iter()
                    .map(|m| {
                        crate::worker::format_recipient(
                            m.name().unwrap_or_default(),
                            m.address().unwrap_or_default(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        crate::mail::merge::fields_from_draft(&list.join(", "))
    };
    assert_eq!(field(parsed.to()), "{{First Name}} <{{Email}}>");
    assert_eq!(field(parsed.cc()), "{{Boss}}, ann@example.com");
    assert!(build_email(&account, &msg).is_err());
}

/// OpenPGP sending (#133), end to end against a real gpg in a throwaway
/// keyring: a signed message comes out as PGP/MIME multipart/signed whose
/// signature the reader's own path verifies over lettre's exact bytes; an
/// encrypted one as multipart/encrypted that decrypts back to the body.
/// Skipped where gpg is missing.
#[test]
fn build_email_signs_and_encrypts_with_pgp_mime() {
    if !crate::mail::pgp::available() {
        eprintln!("gpg not installed; skipping");
        return;
    }
    let home = std::env::temp_dir().join(format!("hylki-gpg-send-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    // `Gpg::system()` (what build_email uses) honours this override.
    std::env::set_var("HYLKI_GNUPGHOME", &home);
    let gpg = crate::mail::pgp::Gpg::system();
    let mut account = sample_account();
    account.email = "sender@hylki.invalid".into();
    let fpr = crate::mail::pgp::generate_key(&gpg, "Sender", "sender@hylki.invalid", "never", "")
        .expect("own key");
    crate::mail::pgp::generate_key(&gpg, "Peer", "peer@hylki.invalid", "never", "")
        .expect("peer key");

    let mut msg = sample_outgoing();
    msg.to = "Peer <peer@hylki.invalid>".into();
    msg.body = "signed body text".into();
    msg.sign = true;
    let raw = build_email(&account, &msg).expect("builds").formatted();
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains("multipart/signed"), "{text}");
    assert!(text.contains("micalg=\"pgp-sha"), "{text}");
    assert!(!has_bare_lf(&raw), "{text}");
    let u = crate::mail::pgp::unwrap_message(&raw, &gpg).expect("recognised");
    assert!(
        matches!(u.status.signature, crate::models::PgpSignature::Good { .. }),
        "{:?}",
        u.status
    );
    assert!(!u.status.encrypted);
    let inner = String::from_utf8_lossy(&u.inner.unwrap()).to_string();
    assert!(inner.contains("signed body text"), "{inner}");

    // With an attachment the signed entity is the whole mixed multipart.
    let path = home.join("note.txt");
    std::fs::write(&path, b"attached").unwrap();
    msg.attachments = vec![path.to_string_lossy().to_string()];
    let raw = build_email(&account, &msg).expect("builds").formatted();
    let u = crate::mail::pgp::unwrap_message(&raw, &gpg).expect("recognised");
    assert!(
        matches!(u.status.signature, crate::models::PgpSignature::Good { .. }),
        "{:?}",
        u.status
    );
    assert!(!extract_attachments(&u.inner.unwrap()).is_empty());

    // Encrypted and signed: readable by the peer and by the sender.
    msg.attachments.clear();
    msg.encrypt = true;
    msg.body = "secret body text".into();
    msg.subject = "Secret plans".into();
    msg.bcc = "Hidden <sender@hylki.invalid>".into();
    let raw = build_email(&account, &msg).expect("builds").formatted();
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains("multipart/encrypted"), "{text}");
    assert!(
        !text.contains("secret body text"),
        "plaintext must not appear: {text}"
    );
    // Protected headers (#493): the subject is inside, "..." outside.
    assert!(
        !text.contains("Secret plans"),
        "the subject must not appear: {text}"
    );
    assert!(text.contains("Subject: ...\r\n"), "{text}");
    assert!(!has_bare_lf(&raw), "{text}");
    let u = crate::mail::pgp::unwrap_message(&raw, &gpg).expect("recognised");
    assert!(u.status.encrypted && u.status.decrypted, "{:?}", u.status);
    assert!(
        matches!(u.status.signature, crate::models::PgpSignature::Good { .. }),
        "{:?}",
        u.status
    );
    assert_eq!(u.status.subject.as_deref(), Some("Secret plans"));
    let inner = String::from_utf8_lossy(&u.inner.unwrap()).to_string();
    assert!(inner.contains("secret body text"), "{inner}");
    assert!(inner.contains("protected-headers=\"v1\""), "{inner}");
    assert!(
        inner.contains("peer@hylki.invalid"),
        "the To travels inside: {inner}"
    );
    assert!(
        !inner.contains("Hidden"),
        "Bcc must stay out of what every recipient reads: {inner}"
    );
    assert_eq!(
        extract_body(inner.as_bytes())
            .matches("secret body text")
            .count(),
        1
    );
    // A Bcc with a key of its own is in the packet without its key id,
    // which the To and the Cc would otherwise see.
    crate::mail::pgp::generate_key(&gpg, "Bcc", "bcc@hylki.invalid", "never", "").expect("bcc key");
    msg.bcc = "Hidden <bcc@hylki.invalid>".into();
    let raw = build_email(&account, &msg).expect("builds").formatted();
    let text = String::from_utf8_lossy(&raw);
    let begin = text.find("-----BEGIN PGP MESSAGE-----").expect("armour");
    let end = text.find("-----END PGP MESSAGE-----").expect("armour end");
    let mut list = std::process::Command::new("gpg")
        .env("GNUPGHOME", &home)
        .args(["--batch", "--list-only", "--list-packets"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("gpg runs");
    use std::io::Write as _;
    list.stdin
        .take()
        .unwrap()
        .write_all(text[begin..end + 25].as_bytes())
        .unwrap();
    let packets = String::from_utf8_lossy(&list.wait_with_output().unwrap().stdout).to_string();
    let keyids: Vec<&str> = packets
        .lines()
        .filter(|l| l.contains(":pubkey enc packet:"))
        .collect();
    assert_eq!(keyids.len(), 3, "peer, sender and the Bcc: {packets}");
    assert_eq!(
        keyids
            .iter()
            .filter(|l| l.ends_with("keyid 0000000000000000"))
            .count(),
        1,
        "only the Bcc is anonymous: {packets}"
    );
    let u = crate::mail::pgp::unwrap_message(&raw, &gpg).expect("recognised");
    assert!(u.status.decrypted, "{:?}", u.status);
    msg.bcc.clear();
    // An account that leaves the subject readable (#493) sends it outside.
    account.plain_subject = true;
    let raw = build_email(&account, &msg).expect("builds").formatted();
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains("Subject: Secret plans\r\n"), "{text}");
    assert!(
        !text.contains("secret body text"),
        "plaintext must not appear: {text}"
    );
    let u = crate::mail::pgp::unwrap_message(&raw, &gpg).expect("recognised");
    assert!(
        u.status.decrypted && u.status.subject.is_none(),
        "{:?}",
        u.status
    );
    account.plain_subject = false;

    // A recipient without a key is refused by name; a From without a key too.
    msg.cc = "nobody@hylki.invalid".into();
    let err = build_email(&account, &msg)
        .expect_err("no key for nobody")
        .to_string();
    assert!(err.contains("nobody@hylki.invalid"), "{err}");
    msg.cc.clear();
    account.email = "keyless@hylki.invalid".into();
    let err = build_email(&account, &msg)
        .expect_err("no own key")
        .to_string();
    assert!(err.contains("keyless@hylki.invalid"), "{err}");
    // The account's chosen key overrides the address match.
    account.pgp_key = Some(fpr);
    build_email(&account, &msg).expect("chosen key signs");

    // A draft of the same message stays plain.
    let plain = OutgoingMessage {
        sign: false,
        encrypt: false,
        ..msg.clone()
    };
    let raw = build_email(&account, &plain).expect("builds").formatted();
    assert!(String::from_utf8_lossy(&raw).contains("secret body text"));

    std::env::remove_var("HYLKI_GNUPGHOME");
    let _ = std::process::Command::new("gpgconf")
        .env("GNUPGHOME", &home)
        .args(["--kill", "all"])
        .status();
    let _ = std::fs::remove_dir_all(&home);
}

/// A reply with no In-Reply-To/References is a new conversation to every
/// client that receives it — including Hylki's own threading.
#[test]
fn reply_to_header_reaches_the_wire() {
    let mut msg = sample_outgoing();
    msg.to = "someone@example.com".into();
    msg.reply_to = "list@example.org, Other <other@example.net>".into();
    let email = build_email(&sample_account(), &msg).expect("builds");
    let raw = String::from_utf8_lossy(&email.formatted()).to_string();
    assert!(raw.contains("Reply-To:"), "Reply-To must be present: {raw}");
    assert!(raw.contains("list@example.org"), "{raw}");
    assert!(raw.contains("other@example.net"), "{raw}");
}

#[test]
fn a_reply_carries_its_threading_headers() {
    let mut msg = sample_outgoing();
    msg.to = "someone@example.com".into();
    msg.in_reply_to = "parent@example.com".into();
    msg.references = "root@example.com parent@example.com".into();
    let email = build_email(&sample_account(), &msg).expect("builds");
    let raw = String::from_utf8_lossy(&email.formatted()).to_string();
    assert!(
        raw.contains("In-Reply-To: <parent@example.com>"),
        "In-Reply-To must be present and bracketed: {raw}"
    );
    assert!(
        raw.contains("References: <root@example.com> <parent@example.com>"),
        "References must carry the whole chain, bracketed: {raw}"
    );
}

/// A message that starts a conversation must not carry empty headers.
/// Without a Message-ID of our own the server assigns one on the way out, so
/// the copy filed in Sent has none — and nothing that replies to it can be
/// linked back to it. It must also be in the sender's domain, not this
/// machine's hostname.
#[test]
fn outgoing_mail_carries_a_message_id_in_the_senders_domain() {
    let account = AccountConfig {
        email: "me@example.com".into(),
        ..sample_account()
    };
    let msg = OutgoingMessage {
        to: "you@example.org".into(),
        ..sample_outgoing()
    };
    let raw = String::from_utf8_lossy(&build_email(&account, &msg).expect("builds").formatted())
        .to_string();
    let line = raw
        .lines()
        .find(|l| l.starts_with("Message-ID:"))
        .unwrap_or_else(|| panic!("a Message-ID is set: {raw}"));
    assert!(
        line.contains("@example.com>"),
        "in the sender's domain: {line}"
    );
}

#[test]
fn a_new_message_carries_no_threading_headers() {
    let msg = OutgoingMessage {
        to: "someone@example.com".into(),
        ..sample_outgoing()
    };
    let email = build_email(&sample_account(), &msg).expect("builds");
    let raw = String::from_utf8_lossy(&email.formatted()).to_string();
    assert!(!raw.contains("In-Reply-To"), "{raw}");
    assert!(!raw.contains("References"), "{raw}");
}

#[test]
fn an_alias_replaces_from_but_not_the_transport() {
    let account = sample_account();
    let mut msg = sample_outgoing();
    msg.to = "someone@example.com".into();
    msg.from_alias = Some("Ann Work <ann@work.example>".into());
    let email = build_email(&account, &msg).expect("builds");
    let wire = String::from_utf8(email.formatted()).expect("utf8");
    let from_line = wire
        .lines()
        .find(|l| l.starts_with("From:"))
        .expect("From header");
    assert!(
        from_line.contains("ann@work.example"),
        "alias address on the wire: {from_line}"
    );
    assert!(
        !from_line.contains(&account.email),
        "account address replaced: {from_line}"
    );
    // The Message-ID follows the alias's domain, so replies thread back.
    let id_line = wire
        .lines()
        .find(|l| l.starts_with("Message-ID:"))
        .expect("id");
    assert!(
        id_line.contains("@work.example"),
        "id in alias domain: {id_line}"
    );
}

#[test]
fn build_email_carries_attachments() {
    // Issue #15: "Send failed: Invalid input" when sending with attachments.
    // "Invalid input" is lettre's AddressError, raised only by parsing a
    // "Name <addr>" string — which 1.8.x did for From/To and 1.9.0 replaced
    // with mailboxes built from parts. This pins the attachment path itself,
    // including a name that would have needed quoting and a UTF-8 filename.
    let dir = std::env::temp_dir().join(format!("hylki-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("Meeting notes, final.pdf");
    // Binary, so the encoder has to base64 it — as it would a real PDF.
    std::fs::write(
        &file,
        [
            0x25, 0x50, 0x44, 0x46, 0x2d, 0x31, 0x2e, 0x34, 0x00, 0x80, 0xff,
        ],
    )
    .expect("write attachment");

    let account = AccountConfig {
        name: "Powers, Benny".into(),
        email: "benny@example.com".into(),
        ..sample_account()
    };
    let msg = OutgoingMessage {
        to: "Ada Lovelace <ada@example.com>".into(),
        subject: "Notes".into(),
        attachments: vec![file.to_string_lossy().to_string()],
        ..sample_outgoing()
    };

    let email = build_email(&account, &msg).expect("email builds with an attachment");
    let raw = String::from_utf8_lossy(&email.formatted()).to_string();
    assert!(raw.contains("multipart/mixed"), "{raw}");
    assert!(raw.contains("application/pdf"), "{raw}");
    // The filename has a comma and a space, so it has to be quoted or encoded.
    assert!(
        raw.contains("Meeting notes, final.pdf") || raw.contains("Meeting%20notes"),
        "{raw}"
    );
    // Base64 of the bytes written above.
    assert!(raw.contains("base64"), "{raw}");
    assert!(raw.contains("JVBERi0xLjQAgP8="), "{raw}");
    std::fs::remove_dir_all(&dir).ok();
}

/// Every line on the wire ends CRLF: JMAP servers refuse a message with a
/// lone LF anywhere in it (#414).
fn has_bare_lf(raw: &[u8]) -> bool {
    raw.iter()
        .enumerate()
        .any(|(i, &b)| b == b'\n' && (i == 0 || raw[i - 1] != b'\r'))
}

/// #412: plain text with nothing attached still says it is UTF-8 text,
/// or a client that doesn't guess garbles every accent.
#[test]
fn plain_text_declares_its_charset() {
    let msg = OutgoingMessage {
        to: "ada@example.com".into(),
        body: "Plain text with öäü and 🙂\nsecond line".into(),
        ..sample_outgoing()
    };
    let raw = build_email(&sample_account(), &msg)
        .expect("email builds")
        .formatted();
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains("MIME-Version: 1.0\r\n"), "{text}");
    assert!(
        text.contains("Content-Type: text/plain; charset=utf-8\r\n"),
        "{text}"
    );
    assert!(!has_bare_lf(&raw), "{text}");
    let parsed = mail_parser::MessageParser::default()
        .parse(&raw)
        .expect("parses");
    assert_eq!(
        parsed.body_text(0).as_deref().map(str::trim_end),
        Some("Plain text with öäü and 🙂\r\nsecond line")
    );
}

/// #414: a text file with Unix line endings goes out as base64, so its
/// bytes arrive unchanged and no lone LF reaches the wire.
#[test]
fn a_unix_text_attachment_leaves_no_bare_newline() {
    let dir = std::env::temp_dir().join(format!("hylki-lf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("notes.txt");
    std::fs::write(&file, b"one\ntwo\n").unwrap();
    let msg = OutgoingMessage {
        to: "ada@example.com".into(),
        attachments: vec![file.to_string_lossy().to_string()],
        ..sample_outgoing()
    };
    let raw = build_email(&sample_account(), &msg)
        .expect("email builds")
        .formatted();
    assert!(!has_bare_lf(&raw), "{}", String::from_utf8_lossy(&raw));
    let parsed = mail_parser::MessageParser::default()
        .parse(&raw)
        .expect("parses");
    assert_eq!(
        parsed.attachment(0).map(|a| a.contents()),
        Some(&b"one\ntwo\n"[..])
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A picture pasted into the composer rides the HTML as a data: URI; the
/// wire message must carry it as an inline cid: part inside
/// multipart/related — rendered in place, absent from the recipient's
/// attachment list — with no data: URI left behind.
#[test]
fn pasted_images_go_out_as_inline_cid_parts() {
    let png = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x01, 0x02,
    ];
    let uri = format!(
        "data:image/png;base64,{}",
        crate::encoding::base64_encode(&png)
    );
    let msg = OutgoingMessage {
        to: "ada@example.com".into(),
        body: "see picture".into(),
        html: format!("<p>see</p><img src=\"{uri}\" style=\"max-width:100%\"><p>picture</p>"),
        ..sample_outgoing()
    };
    let email = build_email(&sample_account(), &msg).expect("email builds");
    let raw = String::from_utf8_lossy(&email.formatted()).to_string();
    assert!(raw.contains("multipart/related"), "{raw}");
    assert!(raw.contains("Content-ID"), "{raw}");
    assert!(raw.contains("cid:"), "{raw}");
    assert!(raw.contains("Content-Disposition: inline"), "{raw}");
    assert!(raw.contains("image/png"), "{raw}");
    assert!(!raw.contains("data:image"), "{raw}");
}

/// A picture dropped in from a file manager keeps the name it had; one
/// pasted as raw bytes has none to keep.
#[test]
fn an_images_own_filename_reaches_the_inline_part() {
    let png = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00];
    let uri = format!(
        "data:image/png;base64,{}",
        crate::encoding::base64_encode(&png)
    );
    let raw_for = |img: String| {
        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            body: "see picture".into(),
            html: format!("<p>see</p>{img}"),
            ..sample_outgoing()
        };
        let email = build_email(&sample_account(), &msg).expect("email builds");
        String::from_utf8_lossy(&email.formatted()).to_string()
    };
    let named = raw_for(format!("<img src=\"{uri}\" alt=\"holiday snap.png\">"));
    assert!(named.contains("holiday snap.png"), "{named}");
    let anonymous = raw_for(format!("<img src=\"{uri}\">"));
    assert!(
        anonymous.contains("Content-Disposition: inline"),
        "{anonymous}"
    );
    assert!(!anonymous.contains("filename"), "{anonymous}");
}

/// HTML without an embedded image keeps the old, plain alternative shape.
#[test]
fn html_without_images_stays_a_plain_alternative() {
    let msg = OutgoingMessage {
        to: "ada@example.com".into(),
        body: "hi".into(),
        html: "<p>hi</p>".into(),
        ..sample_outgoing()
    };
    let email = build_email(&sample_account(), &msg).expect("email builds");
    let raw = String::from_utf8_lossy(&email.formatted()).to_string();
    assert!(raw.contains("multipart/alternative"), "{raw}");
    assert!(!raw.contains("multipart/related"), "{raw}");
}

#[test]
fn build_email_reports_a_missing_attachment_by_name() {
    let msg = OutgoingMessage {
        to: "ada@example.com".into(),
        attachments: vec!["/nonexistent/report.pdf".into()],
        ..sample_outgoing()
    };
    let err = build_email(&sample_account(), &msg).expect_err("missing file fails");
    assert!(err.to_string().contains("report.pdf"), "{err}");
}

#[test]
fn build_email_accepts_named_recipients() {
    let account = AccountConfig {
        name: "Jason Martin".into(),
        email: "me@example.com".into(),
        ..sample_account()
    };
    let msg = OutgoingMessage {
        to: "Alfonso Lizárraga <alfonso@example.com>, plain@example.com".into(),
        ..sample_outgoing()
    };
    let email = build_email(&account, &msg).expect("email builds");
    let raw = String::from_utf8_lossy(&email.formatted()).to_string();
    assert!(raw.contains("alfonso@example.com"), "{raw}");
    assert!(raw.contains("plain@example.com"), "{raw}");
}

/// An answer to an invitation goes out as plain text beside a
/// `text/calendar` part that carries the iTIP method (#223) — a
/// calendar that cannot find the method ignores the answer.
#[test]
fn an_invitation_answer_carries_its_calendar_part() {
    let account = sample_account();
    let msg = OutgoingMessage {
        to: "chair@example.org".into(),
        subject: "Accepted: Architecture sync".into(),
        body: "Jason has accepted this invitation.".into(),
        calendar: Some(CalendarPart {
            ics: "BEGIN:VCALENDAR\r\nMETHOD:REPLY\r\nEND:VCALENDAR\r\n".into(),
            method: "REPLY".into(),
        }),
        ..sample_outgoing()
    };
    let raw = build_message(&account, &msg, false)
        .expect("builds")
        .formatted();
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains("multipart/alternative"), "{text}");
    assert!(
        text.contains("text/calendar; charset=utf-8; method=REPLY"),
        "the method rides on the part: {text}"
    );
    assert!(text.contains("METHOD:REPLY"), "{text}");
    assert!(
        text.contains("Jason has accepted this invitation."),
        "{text}"
    );
}
