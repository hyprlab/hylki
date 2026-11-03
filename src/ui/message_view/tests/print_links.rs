//! Printing, preview URIs, filenames and link previews.

use super::*;
use adw::prelude::*;

#[test]
fn the_printed_page_carries_the_header() {
    // On screen these facts live in a GTK pane, which WebKit cannot print —
    // so they have to be in the document itself (issue #16).
    let m = msg_for_print();
    let header = print_header_html(Some(&m));
    assert!(header.contains("Quarterly numbers"), "{header}");
    assert!(
        header.contains("Ada Lovelace &lt;ada@example.com&gt;"),
        "{header}"
    );
    assert!(header.contains("me@example.com"), "{header}");
    assert!(header.contains("carol@example.com"), "{header}");
    assert!(header.contains("From:") && header.contains("To:") && header.contains("Date:"));
}

#[test]
fn the_printed_header_is_text_not_markup() {
    let mut m = msg_for_print();
    m.subject = "<script>alert(1)</script>".into();
    m.cc = String::new();
    let header = print_header_html(Some(&m));
    assert!(!header.contains("<script>"), "{header}");
    assert!(header.contains("&lt;script&gt;"), "{header}");
    // An empty field is left out rather than printed as a blank line.
    assert!(!header.contains("Cc:"), "{header}");
    // No message at all: nothing to describe.
    assert_eq!(print_header_html(None), "");
}

#[test]
fn printing_inlines_the_message_instead_of_framing_it() {
    // The bug: printed pages carried the reader's iframe scrollbars and were
    // cut off at the frame's on-screen height, because a print engine draws an
    // iframe as a box rather than paginating what is inside it.
    let doc = print_document(
        "<div class=\"hylki-print-hdr\">HEADER</div>",
        &[(
            String::new(),
            "<!DOCTYPE html><html><head><style>p{color:red}</style></head>\
             <body class=\"x\"><p>Hello</p></body></html>"
                .to_string(),
        )],
        false,
    );
    assert!(!doc.contains("<iframe"), "{doc}");
    assert!(doc.contains("HEADER"));
    assert!(doc.contains("<p>Hello</p>"));
    // The message keeps its own styling on paper.
    assert!(doc.contains("p{color:red}"));
    // Long URLs wrap rather than running off the sheet, and wide content is
    // scaled down instead of being clipped.
    assert!(doc.contains("overflow-wrap:anywhere"));
    assert!(doc.contains("img,table,pre{max-width:100% !important;}"));
    // Still no scripts, and blocked remote content stays blocked.
    assert!(doc.contains("Content-Security-Policy"));
}

#[test]
fn a_bare_image_is_capped_at_the_card_unless_the_message_says_otherwise() {
    let doc = inject_csp(
        "<html><head><style>img{max-width:none}</style></head><body><img src=\"data:image/png;base64,AA\"></body></html>",
        false,
        false,
    );
    let ours = doc
        .find("img{max-width:100%;}")
        .expect("the default image cap");
    let theirs = doc.find("img{max-width:none}").unwrap();
    assert!(
        ours < theirs,
        "the message's own rule must come later and win: {doc}"
    );
    // A body with no head at all gets it too.
    assert!(inject_csp("<img src=x>", false, true).contains("img{max-width:100%;}"));
}

#[test]
fn inlining_keeps_everything_but_the_structure() {
    assert_eq!(
        inline_body("<!doctype html><html><head><meta charset=\"utf-8\"></head><body id=\"a\">hi</body></html>"),
        "<meta charset=\"utf-8\">hi"
    );
    // A tag that merely starts like one of them is content, not structure.
    assert_eq!(
        inline_body("<bodyguard>x</bodyguard>"),
        "<bodyguard>x</bodyguard>"
    );
    assert_eq!(
        inline_body("<p>plain fragment</p>"),
        "<p>plain fragment</p>"
    );
    // Unterminated tags must not swallow the rest of the message.
    assert_eq!(inline_body("text < 5 and > 2"), "text < 5 and > 2");
}

#[test]
fn one_message_cannot_restyle_the_whole_printout() {
    // A conversation prints as one document, so a sender's body rules have to
    // land on that sender's block and nothing else.
    let out = scope_styles(
        "<style>body{font-family:monospace}\
         @media print{html,body>p{color:red}}\
         .bodyguard{margin:0}</style><p>hi</p>",
        ".hylki-print-m1",
    );
    assert!(
        out.contains(".hylki-print-m1{font-family:monospace}"),
        "{out}"
    );
    assert!(
        out.contains("@media print{.hylki-print-m1,.hylki-print-m1>p{color:red}}"),
        "{out}"
    );
    // Only whole tag names: a class that merely contains "body" is untouched,
    // and so are declarations that mention one.
    assert!(out.contains(".bodyguard{margin:0}"), "{out}");
    assert!(out.contains("<p>hi</p>"));
    assert!(!scope_styles("<p>body{x}</p>", ".hylki-print-m0").contains("hylki-print-m0"));
    // Each message gets a class of its own, so the first sender's rules stop
    // at the first message.
    let doc = print_document(
        "",
        &[
            (String::new(), "<style>body{color:red}</style>one".into()),
            (String::new(), "two".into()),
        ],
        false,
    );
    assert!(doc.contains(".hylki-print-m0{color:red}"), "{doc}");
    assert!(doc.contains("hylki-print-msg hylki-print-m1"), "{doc}");
}

#[test]
fn a_printed_conversation_says_who_sent_what() {
    let mut a = msg_for_print();
    a.from_name = "Alfonso".into();
    a.from_addr = "a@example.com".into();
    let head = print_message_header_html(&a);
    assert!(head.contains("Alfonso &lt;a@example.com&gt;"), "{head}");
    assert!(head.contains(&attr_escape(&a.datetime_full())));
}

#[test]
fn a_preview_uri_is_escaped() {
    // Subjects become filenames, and mail subjects are full of spaces and
    // brackets: "file://" + path is not a URI, and GIO opens nothing.
    let path = std::path::Path::new("/tmp/vireo-print/[hyprlab] Sync 1.11.0.pdf");
    let uri = gtk::gio::File::for_path(path).uri().to_string();
    assert!(!uri.contains(' '), "{uri}");
    assert!(uri.contains("%20"), "{uri}");
    assert!(uri.starts_with("file:///tmp/vireo-print/"), "{uri}");
}

#[test]
fn print_filenames_survive_real_subjects() {
    // The subject names the print job and seeds the filename when printing to
    // a file, so it must not carry path separators or other characters a
    // filesystem would refuse.
    assert_eq!(sanitize_filename("Quarterly numbers"), "Quarterly numbers");
    assert_eq!(
        sanitize_filename("Invoice 3/4 <urgent>"),
        "Invoice 3-4 -urgent-"
    );
    assert_eq!(sanitize_filename("../../etc/passwd"), "etc-passwd");
    // Nothing usable left, or nothing to begin with.
    assert_eq!(sanitize_filename("   "), "Message");
    assert_eq!(sanitize_filename(""), "Message");
    assert_eq!(sanitize_filename("..."), "Message");
}

#[test]
fn print_filenames_are_bounded() {
    let long = "word ".repeat(100);
    assert!(sanitize_filename(&long).chars().count() <= 120);
}

#[test]
fn decodes_a_base64_data_uri() {
    let (mime, data) = decode_data_uri("data:image/jpeg;base64,/9j/4AAQ").expect("decodes");
    assert_eq!(mime, "image/jpeg");
    assert_eq!(&data[..3], b"\xff\xd8\xff"); // JPEG magic
}

#[test]
fn rejects_uris_it_cannot_save_locally() {
    // Remote images keep WebKit's own (download-backed) menu item.
    assert!(decode_data_uri("https://example.com/a.png").is_none());
    // Non-base64 `data:` URIs aren't something the reader emits.
    assert!(decode_data_uri("data:image/png,rawbytes").is_none());
    assert!(decode_data_uri("not a uri").is_none());
}

#[test]
fn link_preview_shows_the_plain_url_when_nothing_is_amiss() {
    assert_eq!(
        link_destination("https://example.com/a", Some("Read more")),
        "https://example.com/a"
    );
    // Link text that matches where it goes is not a mismatch.
    assert_eq!(
        link_destination("https://example.com/a", Some("example.com")),
        "https://example.com/a"
    );
    // Subdomains belong to the same site.
    assert_eq!(
        link_destination("https://mail.example.com/a", Some("example.com")),
        "https://mail.example.com/a"
    );
}

#[test]
fn link_preview_calls_out_text_claiming_another_site() {
    let shown = link_destination("https://evil.example/login", Some("https://paypal.com"));
    assert!(shown.contains("paypal.com"), "{shown}");
    assert!(shown.contains("evil.example"), "{shown}");
    assert!(shown.contains('⚠'), "{shown}");
}

#[test]
fn ordinary_link_text_is_never_mistaken_for_a_host() {
    assert_eq!(host_of("Click here to sign in"), None);
    assert_eq!(host_of("mailto:someone@example.com"), None);
    assert_eq!(
        host_of("https://example.com/path"),
        Some("example.com".into())
    );
    assert_eq!(host_of("example.com/path"), Some("example.com".into()));
    // Userinfo can't be used to disguise the real host.
    assert_eq!(
        host_of("https://paypal.com@evil.example/"),
        Some("evil.example".into())
    );
}

#[test]
fn suggests_a_filename_from_the_mime_type() {
    assert_eq!(default_image_name("image/jpeg"), "image.jpg");
    assert_eq!(default_image_name("image/png"), "image.png");
    assert_eq!(default_image_name("image/svg+xml"), "image.svg");
}

#[test]
fn names_a_remote_picture_from_its_address() {
    let png = "image/png";
    assert_eq!(
        remote_image_name("https://a.example/img/banner.png?w=600", png),
        "banner.png"
    );
    assert_eq!(
        remote_image_name("https://a.example/My%20Photo.JPG", "image/jpeg"),
        "My Photo.JPG"
    );
    // An ID with no extension, the host alone, or a hidden name: from the type.
    assert_eq!(
        remote_image_name("https://t.example/o/8f3a2b", png),
        "image.png"
    );
    assert_eq!(
        remote_image_name("https://a.example/", "image/jpeg; q=1"),
        "image.jpg"
    );
    assert_eq!(
        remote_image_name("https://a.example/.png", png),
        "image.png"
    );
    // A decoded slash is not a path.
    assert_eq!(
        remote_image_name("https://a.example/x%2F..%2Fy.png", png),
        "image.png"
    );
    assert_eq!(remote_image_name("https://a.example/x", ""), "image");
}

#[test]
fn a_cached_picture_gives_back_its_web_address() {
    assert_eq!(
        crate::services::remote_content::web_url("hylki-https://img.example/a.png").as_deref(),
        Some("https://img.example/a.png")
    );
    assert!(crate::services::remote_content::web_url("https://img.example/a.png").is_none());
    assert!(crate::services::remote_content::web_url("data:image/png;base64,AA").is_none());
}
