//! Rendering and attachment extraction over real-world message shapes.

use super::*;
use crate::worker::render::attachments::INLINE_ATTACHMENT_MIN;
use crate::worker::render::body::{image_mime, linkify};

/// Issue #9's message: an Apple Mail PDF marked `inline` with a filename and
/// no Content-ID, inside a top-level multipart/mixed.
const INLINE_PDF: &str = concat!(
    "Content-Type: multipart/mixed; boundary=\"Apple-Mail=_A9BB\"\r\n\r\n",
    "--Apple-Mail=_A9BB\r\nContent-Type: text/plain\r\n\r\nSee attached.\r\n",
    "--Apple-Mail=_A9BB\r\n",
    "Content-Disposition: inline;\r\n\tfilename=\"Report.pdf\"\r\n",
    "Content-Type: application/pdf;\r\n\tname=\"Report.pdf\"\r\n",
    "Content-Transfer-Encoding: base64\r\n\r\n",
    "JVBERi0xLjQNCiWio4+T\r\n",
    "--Apple-Mail=_A9BB--\r\n",
);

/// The same file, but nested under a multipart/alternative — the shape a
/// forwarded Apple Mail message takes, where the top-level Content-Type says
/// nothing about attachments.
const NESTED_INLINE_PDF: &str = concat!(
    "Content-Type: multipart/alternative; boundary=outer\r\n\r\n",
    "--outer\r\nContent-Type: text/plain\r\n\r\nPlease see attached.\r\n",
    "--outer\r\nContent-Type: multipart/mixed; boundary=inner\r\n\r\n",
    "--inner\r\nContent-Type: text/html\r\n\r\n<p>Please see attached.</p>\r\n",
    "--inner\r\nContent-Type: application/pdf; name=\"Report.pdf\"\r\n",
    "Content-Disposition: inline; filename=\"Report.pdf\"\r\n",
    "Content-Transfer-Encoding: base64\r\n\r\n",
    "JVBERi0xLjQNCiWio4+T\r\n",
    "--inner--\r\n--outer--\r\n",
);

#[test]
fn an_inline_pdf_is_listed_as_an_attachment() {
    // Both shapes: the file is `inline` with no Content-ID, which is what the
    // reporter's Apple Mail message looked like.
    for (name, raw) in [("top level", INLINE_PDF), ("nested", NESTED_INLINE_PDF)] {
        let found = extract_attachments(raw.as_bytes());
        assert_eq!(found.len(), 1, "{name}");
        assert_eq!(found[0].name, "Report.pdf", "{name}");
        assert!(found[0].data.starts_with(b"%PDF-1.4"), "{name}");
    }
}

#[test]
fn a_body_load_reports_the_attachments_the_summary_missed() {
    // The paperclip is guessed before the body is fetched, and the guess
    // misses the nested shape: the top-level type is multipart/alternative,
    // which says nothing. Fetching the body is what settles it — the same
    // check `load_body` runs before emitting HasAttachments.
    let has_attachments = |raw: &str| !extract_attachments(raw.as_bytes()).is_empty();
    assert!(has_attachments(NESTED_INLINE_PDF));
    assert!(has_attachments(INLINE_PDF));

    // The header-only guess, for comparison: right about the first, wrong
    // about the second — which is why the body has the last word.
    let top_level_says_mixed = |raw: &str| {
        mail_parser::MessageParser::default()
            .parse(raw.as_bytes())
            .and_then(|p| {
                p.header("Content-Type")
                    .and_then(|h| h.as_content_type())
                    .map(|ct| {
                        ct.ctype().eq_ignore_ascii_case("multipart")
                            && ct
                                .subtype()
                                .is_some_and(|s| s.eq_ignore_ascii_case("mixed"))
                    })
            })
            .unwrap_or(false)
    };
    assert!(top_level_says_mixed(INLINE_PDF));
    assert!(!top_level_says_mixed(NESTED_INLINE_PDF));
}

#[test]
fn decoration_still_does_not_earn_a_paperclip() {
    // The correction must not undo 1.4.1's fix: a newsletter whose only extra
    // parts are small inline `cid:` images has no attachments.
    assert!(extract_attachments(CID_NEWSLETTER.as_bytes()).is_empty());
}

#[test]
fn linkify_wraps_http_and_https() {
    assert_eq!(
        linkify("see http://example.com now"),
        "see <a href=\"http://example.com\">http://example.com</a> now"
    );
    assert_eq!(
        linkify("https://a.b/c?d=1"),
        "<a href=\"https://a.b/c?d=1\">https://a.b/c?d=1</a>"
    );
}

#[test]
fn linkify_prefixes_bare_www_with_https() {
    assert_eq!(
        linkify("go to www.example.com today"),
        "go to <a href=\"https://www.example.com\">www.example.com</a> today"
    );
}

#[test]
fn linkify_escapes_and_does_not_double_link() {
    // The surrounding <> are escaped; the URL inside is linked once.
    assert_eq!(
        linkify("<http://x.com>"),
        "&lt;<a href=\"http://x.com\">http://x.com</a>&gt;"
    );
    // A query '&' is escaped in both href and text.
    assert_eq!(
        linkify("http://x.com/?a=1&b=2"),
        "<a href=\"http://x.com/?a=1&amp;b=2\">http://x.com/?a=1&amp;b=2</a>"
    );
}

#[test]
fn linkify_trims_trailing_punctuation_but_keeps_balanced_parens() {
    assert_eq!(
        linkify("visit http://x.com."),
        "visit <a href=\"http://x.com\">http://x.com</a>."
    );
    assert_eq!(
        linkify("(see http://x.com)"),
        "(see <a href=\"http://x.com\">http://x.com</a>)"
    );
    // A balanced ')' belongs to the URL (e.g. a Wikipedia article).
    assert_eq!(
        linkify("http://en.wikipedia.org/wiki/Foo_(bar)"),
        "<a href=\"http://en.wikipedia.org/wiki/Foo_(bar)\">http://en.wikipedia.org/wiki/Foo_(bar)</a>"
    );
}

#[test]
fn linkify_ignores_scheme_inside_a_word() {
    // No word boundary before "http", so it's not a link (just escaped text).
    assert_eq!(linkify("shttp://x.com"), "shttp://x.com");
    assert!(!linkify("email hi@www.x").contains("<a "));
}

#[test]
fn linkify_never_forges_a_dangerous_scheme() {
    // "javascript:" isn't one of our recognized prefixes, so it stays plain text.
    let out = linkify("javascript:alert(1)");
    assert!(!out.contains("<a "), "got: {out}");
}

/// A decrypted entity with protected headers (#493): the part that only
/// repeats the Subject is neither shown nor listed as a file.
#[test]
fn protected_headers_legacy_part_is_hidden() {
    let entity = |legacy_type: &str| {
        format!(
            "Content-Type: multipart/mixed; boundary=\"pp\"; protected-headers=\"v1\"\r\n\
             Subject: Plans\r\n\r\n\
             --pp\r\nContent-Type: {legacy_type}; protected-headers=\"v1\"\r\n\
             Content-Disposition: inline\r\n\r\nSubject: Plans\r\n\r\n\
             --pp\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\nSee you there.\r\n--pp--\r\n"
        )
    };
    for legacy_type in ["text/plain; charset=UTF-8", "text/rfc822-headers"] {
        let raw = entity(legacy_type);
        let body = extract_body(raw.as_bytes());
        assert!(body.contains("See you there."), "{body}");
        assert!(!body.contains("Subject: Plans"), "{legacy_type}: {body}");
        assert!(
            extract_attachments(raw.as_bytes()).is_empty(),
            "{legacy_type}"
        );
    }
    // The same first part without the flag on the root is ordinary text.
    let raw = entity("text/plain; charset=UTF-8").replacen("; protected-headers=\"v1\"", "", 1);
    assert!(extract_body(raw.as_bytes()).contains("Subject: Plans"));
}

#[test]
fn extract_body_linkifies_plain_text_mail() {
    let raw = b"Content-Type: text/plain\r\n\r\nRead http://example.com/x for more.";
    let body = extract_body(raw);
    assert!(
        body.contains("<a href=\"http://example.com/x\">http://example.com/x</a>"),
        "body was: {body}"
    );
}

/// An iPhone photo mail: multipart/mixed interleaving an empty text part, an
/// *inline* JPEG, and the signature. Exactly the shape Apple Mail produces.
const IPHONE_PHOTO: &str = concat!(
    "Content-Type: multipart/mixed; boundary=Apple-Mail-32B9517E\r\n",
    "Content-Transfer-Encoding: 7bit\r\n",
    "From: Alex Doe <alex@example.com>\r\n",
    "Subject: Panda\r\n",
    "X-Mailer: iPhone Mail (23F84)\r\n",
    "\r\n",
    "--Apple-Mail-32B9517E\r\n",
    "Content-Type: text/plain;\r\n\tcharset=us-ascii\r\n",
    "Content-Transfer-Encoding: 7bit\r\n\r\n\r\n\r\n",
    "--Apple-Mail-32B9517E\r\n",
    "Content-Type: image/jpeg;\r\n\tname=C21AA3E8.jpeg;\r\n",
    "\tx-apple-part-url=4F5E768A\r\n",
    "Content-Disposition: inline;\r\n\tfilename=C21AA3E8.jpeg\r\n",
    "Content-Transfer-Encoding: base64\r\n\r\n",
    "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBk=\r\n",
    "\r\n",
    "--Apple-Mail-32B9517E\r\n",
    "Content-Type: text/plain;\r\n\tcharset=us-ascii\r\n",
    "Content-Transfer-Encoding: 7bit\r\n\r\n",
    "\r\n\r\nAlex Doe\r\nSent from my iPhone\r\n",
    "--Apple-Mail-32B9517E--\r\n",
);

/// A newsletter: HTML body plus a `cid:` logo referenced from it.
const CID_NEWSLETTER: &str = concat!(
    "Content-Type: multipart/related; boundary=R\r\n",
    "Subject: News\r\n\r\n",
    "--R\r\n",
    "Content-Type: text/html; charset=utf-8\r\n\r\n",
    "<p>hi <img src=\"cid:logo\"></p>\r\n",
    "--R\r\n",
    "Content-Type: image/png; name=logo.png\r\n",
    "Content-ID: <logo>\r\n",
    "Content-Disposition: inline; filename=logo.png\r\n",
    "Content-Transfer-Encoding: base64\r\n\r\n",
    "iVBORw0KGgo=\r\n",
    "--R--\r\n",
);

#[test]
fn iphone_photo_body_keeps_text_and_inlines_the_image() {
    let body = extract_body(IPHONE_PHOTO.as_bytes());
    // The signature lives in the *third* part; taking only the first left the
    // message blank.
    assert!(body.contains("Sent from my iPhone"), "body was: {body}");
    // The photo is embedded, not dropped or fetched from the network.
    assert!(
        body.contains("src=\"data:image/jpeg;base64,/9j/4AAQ"),
        "body was: {body}"
    );
}

#[test]
fn iphone_photo_is_listed_as_an_attachment() {
    let found = extract_attachments(IPHONE_PHOTO.as_bytes());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "C21AA3E8.jpeg");
    assert!(!found[0].data.is_empty());
}

/// Gmail photo mail: multipart/related wrapping a plain+HTML alternative and
/// the picture the HTML references by Content-ID.
const GMAIL_RELATED_PHOTO: &str = concat!(
    "Content-Type: multipart/related; boundary=\"OUT\"\r\n",
    "Subject: Share A Day NYC\r\n\r\n",
    "--OUT\r\n",
    "Content-Type: multipart/alternative; boundary=\"IN\"\r\n\r\n",
    "--IN\r\n",
    "Content-Type: text/plain; charset=\"UTF-8\"\r\n\r\n",
    "[image: SAD_POSTCARD_FRONT.JPEG]\r\n",
    "--IN\r\n",
    "Content-Type: text/html; charset=\"UTF-8\"\r\n\r\n",
    "<div dir=\"ltr\"><img src=\"cid:ii_mslv0bge0\" ",
    "alt=\"SAD_POSTCARD_FRONT.JPEG\" width=\"420\" height=\"542\"><br></div>\r\n",
    "--IN--\r\n",
    "--OUT\r\n",
    "Content-Type: image/jpeg; name=\"SAD_POSTCARD_FRONT.JPEG\"\r\n",
    "Content-Disposition: inline; filename=\"SAD_POSTCARD_FRONT.JPEG\"\r\n",
    "Content-Transfer-Encoding: base64\r\n",
    "Content-ID: <ii_mslv0bge0>\r\n\r\n",
    "/9j/4AAQSkZJRg==\r\n",
    "--OUT--\r\n",
);

#[test]
fn cid_resources_are_not_listed_as_attachments() {
    assert!(extract_attachments(CID_NEWSLETTER.as_bytes()).is_empty());
}

/// A `multipart/related` whose `cid:` image decodes to `bytes` bytes.
fn cid_mail_with_image_of(bytes: usize) -> String {
    // base64 of `bytes` zeroes: 4 chars per 3 bytes, rounded up.
    let payload = crate::encoding::base64_encode(&vec![0u8; bytes]);
    concat!(
        "Content-Type: multipart/related; boundary=R\r\n",
        "Subject: Photo\r\n\r\n",
        "--R\r\n",
        "Content-Type: text/html; charset=utf-8\r\n\r\n",
        "<p><img src=\"cid:pic\"></p>\r\n",
        "--R\r\n",
        "Content-Type: image/jpeg; name=\"pic.jpg\"\r\n",
        "Content-ID: <pic>\r\n",
        "Content-Disposition: inline; filename=\"pic.jpg\"\r\n",
        "Content-Transfer-Encoding: base64\r\n\r\n",
    )
    .to_string()
        + &payload
        + "\r\n--R--\r\n"
}

#[test]
fn large_cid_image_is_listed_as_an_attachment() {
    // A photo someone emailed you arrives in the same shape as a newsletter
    // logo; it has to be saveable, so size decides.
    let found = extract_attachments(cid_mail_with_image_of(INLINE_ATTACHMENT_MIN).as_bytes());
    assert_eq!(found.len(), 1, "found: {found:?}");
    assert_eq!(found[0].name, "pic.jpg");
    assert_eq!(found[0].data.len(), INLINE_ATTACHMENT_MIN);
}

#[test]
fn small_cid_image_is_still_only_decoration() {
    let raw = cid_mail_with_image_of(INLINE_ATTACHMENT_MIN - 1);
    assert!(extract_attachments(raw.as_bytes()).is_empty());
}

/// A message in the shape Gmail's composer sends: every uploaded file is
/// marked `Content-Disposition: attachment` *and* stamped with an `<f_…>`
/// Content-ID, whatever its size.
fn gmail_mail_with_files(files: &[(&str, &str, usize)]) -> String {
    let mut raw = String::from("Content-Type: multipart/mixed; boundary=M\r\n");
    raw.push_str("Subject: files\r\n\r\n");
    raw.push_str("--M\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n");
    raw.push_str("Body text.\r\n");
    for (i, (name, ctype, bytes)) in files.iter().enumerate() {
        raw.push_str("--M\r\n");
        raw.push_str(&format!("Content-Type: {ctype}; name=\"{name}\"\r\n"));
        raw.push_str(&format!(
            "Content-Disposition: attachment; filename=\"{name}\"\r\n"
        ));
        raw.push_str("Content-Transfer-Encoding: base64\r\n");
        raw.push_str(&format!("Content-ID: <f_a1b2c3d4{i}>\r\n\r\n"));
        raw.push_str(&crate::encoding::base64_encode(&vec![0u8; *bytes]));
        raw.push_str("\r\n");
    }
    raw.push_str("--M--\r\n");
    raw
}

/// Two small files and one large one are sent together; only the large one
/// came back, because the small parts carried a Content-ID. A part the sender
/// declared an attachment is never decoration, however small.
#[test]
fn small_declared_attachments_survive_their_content_id() {
    let raw = gmail_mail_with_files(&[
        ("first.pdf", "application/pdf", 16_384),
        ("second.pdf", "application/pdf", 40_960),
        (
            "large.bin",
            "application/octet-stream",
            INLINE_ATTACHMENT_MIN + 1,
        ),
    ]);
    let found = extract_attachments(raw.as_bytes());
    let names: Vec<&str> = found.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(
        names,
        ["first.pdf", "second.pdf", "large.bin"],
        "found: {names:?}"
    );
    assert_eq!(found[0].data.len(), 16_384);
    assert_eq!(found[1].data.len(), 40_960);
}

/// One attachment, its Content-Disposition supplied verbatim.
fn mail_with_disposition(disposition: &str) -> String {
    format!(
        concat!(
            "Content-Type: multipart/mixed; boundary=B\r\n",
            "Subject: file\r\n\r\n",
            "--B\r\n",
            "Content-Type: application/octet-stream\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "Content-Disposition: {disposition}\r\n\r\n",
            "AAAAAA==\r\n",
            "--B--\r\n",
        ),
        disposition = disposition
    )
}

/// A sender split one filename across two encoded-words with a space between
/// them. RFC 2047 §6.2 joins those and drops the space; keeping it yields
/// `docu ment.pdf` — a PDF with no `.pdf` suffix, so nothing can open it.
#[test]
fn a_filename_split_across_encoded_words_is_rejoined() {
    let raw =
        mail_with_disposition("attachment; filename=\"=?utf-8?q?docu?= =?utf-8?q?ment.pdf?=\"");
    let found = extract_attachments(raw.as_bytes());
    assert_eq!(found.len(), 1, "found: {found:?}");
    assert_eq!(found[0].name, "document.pdf");
}

/// The same split, folded across two lines: the separator arrives as a tab.
#[test]
fn a_filename_folded_between_encoded_words_is_rejoined() {
    let raw = mail_with_disposition(
        "attachment;\r\n\tfilename=\"=?utf-8?q?docu?=\r\n\t=?utf-8?q?ment.pdf?=\"",
    );
    let found = extract_attachments(raw.as_bytes());
    assert_eq!(found.len(), 1, "found: {found:?}");
    assert_eq!(found[0].name, "document.pdf");
}

/// `=20` is a space in the name itself, not a separator between two words.
#[test]
fn a_space_inside_one_encoded_word_is_kept() {
    let raw = mail_with_disposition("attachment; filename=\"=?utf-8?q?my=20file.pdf?=\"");
    let found = extract_attachments(raw.as_bytes());
    assert_eq!(found.len(), 1, "found: {found:?}");
    assert_eq!(found[0].name, "my file.pdf");
}

/// A plain filename with a space in it is nobody's business but the sender's.
#[test]
fn an_unencoded_filename_with_a_space_is_untouched() {
    let raw = mail_with_disposition("attachment; filename=\"my file.pdf\"");
    let found = extract_attachments(raw.as_bytes());
    assert_eq!(found.len(), 1, "found: {found:?}");
    assert_eq!(found[0].name, "my file.pdf");
}

#[test]
fn cid_image_is_embedded_in_the_html_body() {
    let body = extract_body(CID_NEWSLETTER.as_bytes());
    // Nothing can fetch a `cid:` URL, so the reference must be gone.
    assert!(!body.contains("cid:logo"), "body was: {body}");
    assert!(
        body.contains("src=\"data:image/png;base64,iVBORw0KGgo=\""),
        "body was: {body}"
    );
}

#[test]
fn gmail_related_photo_renders_the_picture_not_its_filename() {
    let body = extract_body(GMAIL_RELATED_PHOTO.as_bytes());
    assert!(!body.contains("cid:ii_mslv0bge0"), "body was: {body}");
    assert!(
        body.contains("src=\"data:image/jpeg;base64,/9j/4AAQSkZJRg==\""),
        "body was: {body}"
    );
    // The rest of the sender's markup survives the rewrite.
    assert!(body.contains("width=\"420\""), "body was: {body}");
    // The image is rendered in place, so it isn't appended a second time.
    assert_eq!(
        body.matches("/9j/4AAQSkZJRg==").count(),
        1,
        "body was: {body}"
    );
}

#[test]
fn percent_encoded_cid_reference_still_resolves() {
    let raw = concat!(
        "Content-Type: multipart/related; boundary=R\r\n\r\n",
        "--R\r\n",
        "Content-Type: text/html\r\n\r\n",
        "<img src=\"cid:part1%40mail\">\r\n",
        "--R\r\n",
        "Content-Type: image/png\r\n",
        "Content-ID: <part1@mail>\r\n",
        "Content-Transfer-Encoding: base64\r\n\r\n",
        "iVBORw0KGgo=\r\n",
        "--R--\r\n",
    );
    let body = extract_body(raw.as_bytes());
    assert!(body.contains("data:image/png;base64,"), "body was: {body}");
}

#[test]
fn cid_link_target_is_not_rewritten() {
    let raw = concat!(
        "Content-Type: multipart/related; boundary=R\r\n\r\n",
        "--R\r\n",
        "Content-Type: text/html\r\n\r\n",
        "<a href=\"cid:logo\"><img src=\"cid:logo\"></a>\r\n",
        "--R\r\n",
        "Content-Type: image/png\r\n",
        "Content-ID: <logo>\r\n",
        "Content-Transfer-Encoding: base64\r\n\r\n",
        "iVBORw0KGgo=\r\n",
        "--R--\r\n",
    );
    let body = extract_body(raw.as_bytes());
    assert!(body.contains("href=\"cid:logo\""), "body was: {body}");
    assert!(body.contains("src=\"data:image/png;"), "body was: {body}");
}

#[test]
fn unresolvable_cid_reference_is_left_untouched() {
    let raw = b"Content-Type: text/html\r\n\r\n<p>see cid:nope</p><img src=\"cid:gone\">";
    let body = extract_body(raw);
    // A cid with no matching part stays as it was — and a bare mention in prose
    // is never treated as a resource reference.
    assert_eq!(body, "<p>see cid:nope</p><img src=\"cid:gone\">");
}

#[test]
fn lone_html_part_passes_through_untouched() {
    let raw = b"Content-Type: text/html\r\n\r\n<p>hello</p>";
    assert_eq!(extract_body(raw), "<p>hello</p>");
}

#[test]
fn plain_text_only_message_still_renders() {
    let raw = b"Content-Type: text/plain\r\n\r\nhello <there>";
    let body = extract_body(raw);
    assert!(body.contains("hello &lt;there&gt;"), "body was: {body}");
}

/// Thunderbird's plain text: broken at 72 characters with a space before each
/// soft break. The reader joins the lines so the text wraps to the pane (#606).
#[test]
fn flowed_plain_text_joins_its_soft_breaks() {
    let raw = b"Content-Type: text/plain; charset=UTF-8; format=flowed\r\n\r\n\
        Lorem ipsum dolor sit amet consectetur adipiscing elit. Quisque faucibus \r\n\
        ex sapien vitae pellentesque sem placerat.\r\n\r\nSecond paragraph.\r\n";
    let body = extract_body(raw);
    assert!(
        body.contains("Quisque faucibus ex sapien vitae pellentesque sem placerat.\n\nSecond"),
        "body was: {body}"
    );

    // The same text, not flowed, keeps every break the sender wrote.
    let fixed = b"Content-Type: text/plain\r\n\r\nfaucibus \r\nex sapien\r\n";
    assert!(extract_body(fixed).contains("faucibus \r\nex sapien"));
}

/// The same part, read out of the raw message rather than described by
/// the server, and named the same way.
#[test]
fn an_unnamed_calendar_part_is_still_a_file() {
    let raw = concat!(
        "Content-Type: multipart/alternative; boundary=B\r\n",
        "Subject: Invitation\r\n\r\n",
        "--B\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nYou are invited.\r\n",
        "--B\r\nContent-Type: text/calendar; charset=utf-8; method=REQUEST\r\n\r\n",
        "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n--B--\r\n",
    );
    let found = extract_attachments(raw.as_bytes());
    assert_eq!(found.len(), 1);
    assert!(found[0].name.ends_with(".ics"), "{}", found[0].name);
}

#[test]
fn image_mime_rejects_a_hostile_subtype() {
    // Guards the `data:` URI against a subtype that would break out of it.
    let raw = b"Content-Type: image/\"onerror=alert(1)\r\n\r\nx";
    let parsed = mail_parser::MessageParser::default()
        .parse(raw.as_slice())
        .unwrap();
    let part = parsed.html_bodies().next().unwrap();
    assert!(image_mime(part).is_none());
}

/// #622: a message forwarded as an attachment, unnamed by its sender, is
/// listed under its own subject with `.eml`, so it opens as a message.
#[test]
fn an_attached_message_is_named_eml() {
    let raw = concat!(
        "Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n",
        "--b\r\nContent-Type: text/plain\r\n\r\nSee below.\r\n",
        "--b\r\nContent-Type: message/rfc822\r\n",
        "Content-Disposition: attachment\r\n\r\n",
        "MIME-Version: 1.0\r\nFrom: Ann <ann@example.org>\r\n",
        "Subject: Q3 / plan\r\n\r\nInner body.\r\n",
        "--b--\r\n",
    );
    let found = extract_attachments(raw.as_bytes());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "Q3 - plan.eml");
    assert!(crate::mail::message_file::is_message(
        &found[0].name,
        &found[0].data
    ));
}
