//! Characterization tests: they pin what the reader's document builders and
//! HTML helpers produce today, quirks included, so the restructure can move
//! the code without changing a byte of what the WebView is handed.
//!
//! The golden files in `testdata/` are compared byte for byte (after the
//! random CSP nonce is normalised). `HYLKI_BLESS_GOLDEN=1 cargo test` writes
//! them afresh; that is only right when the output is meant to change.

use super::*;

/// Compare `actual` with the golden file `name`, or write it when blessing.
fn golden(name: &str, actual: &str) {
    let path = format!(
        "{}/src/ui/message_view/testdata/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    if std::env::var_os("HYLKI_BLESS_GOLDEN").is_some() {
        std::fs::write(&path, actual).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path).expect("golden file present");
    if expected != actual {
        let at = expected
            .bytes()
            .zip(actual.bytes())
            .position(|(a, b)| a != b)
            .unwrap_or(expected.len().min(actual.len()));
        let from = at.saturating_sub(80);
        panic!(
            "{name} differs at byte {at}:\n expected …{}…\n   actual …{}…",
            &expected[from..(at + 80).min(expected.len())],
            &actual[from..(at + 80).min(actual.len())],
        );
    }
}

/// The CSP nonce is random per render; replace its value with a fixed token.
fn normalise_nonce(doc: &str) -> String {
    let mut out = String::with_capacity(doc.len());
    let mut rest = doc;
    loop {
        let next = ["nonce-", "nonce=\""]
            .iter()
            .filter_map(|m| rest.find(m).map(|i| (i, m.len())))
            .min();
        let Some((i, len)) = next else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..i + len]);
        rest = &rest[i + len..];
        let end = rest.find(['\'', '"']).unwrap_or(rest.len());
        out.push_str("NONCE");
        rest = &rest[end..];
    }
}

fn message(id: u32) -> Message {
    Message {
        id,
        account_id: 7,
        folder_id: 3,
        uid: 100 + id,
        from_name: format!("Sender {id}"),
        from_addr: format!("sender{id}@golden.example"),
        reply_to: String::new(),
        reply_to_name: String::new(),
        to: "\"Doe, Jane\" <jane@golden.example>, bob@golden.example".into(),
        cc: String::new(),
        bcc: String::new(),
        subject: "Golden <subject> & more".into(),
        preview: String::new(),
        body: format!(
            "<p style=\"color:#000\">body {id} <img src=\"https://x.example/p.png\"></p>"
        ),
        date: format!("0{id}:00"),
        timestamp: 0,
        unread: false,
        starred: false,
        answered: false,
        forwarded: false,
        keywords: Vec::new(),
        has_attachment: false,
        message_id: String::new(),
        references: String::new(),
        importance: Default::default(),
        due: 0,
        labels: Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn document(
    thread: &[Message],
    selected: &[(u32, u32)],
    restrict: bool,
    dark: bool,
    always_show_recipients: bool,
    single_message_card: bool,
    reader: bool,
    zoom: u32,
) -> String {
    let mut labels = std::collections::HashMap::new();
    labels.insert((7, 2), "Other <folder>".to_string());
    let mut heights = std::collections::HashMap::new();
    heights.insert((7, 2), 321);
    let mut sender_style = std::collections::HashSet::new();
    sender_style.insert((7, 1));
    let doc = MessageView::conversation_document(
        thread,
        &labels,
        &Default::default(),
        &heights,
        selected,
        "#3584e4",
        restrict,
        dark,
        always_show_recipients,
        single_message_card,
        &crate::config::ReaderStyle::NONE,
        &sender_style,
        reader,
        zoom,
    );
    normalise_nonce(&doc)
}

#[test]
fn golden_single_message_full_bleed() {
    golden(
        "single.html",
        &document(&[message(1)], &[], true, false, false, false, false, 100),
    );
}

#[test]
fn golden_single_message_carded_dark() {
    let mut m = message(1);
    m.unread = true;
    m.cc = "carol@golden.example".into();
    golden(
        "single_carded_dark.html",
        &document(&[m], &[], false, true, true, true, false, 120),
    );
}

#[test]
fn golden_two_card_conversation() {
    let mut second = message(2);
    second.folder_id = 2;
    second.unread = true;
    second.starred = true;
    golden(
        "two_cards.html",
        &document(
            &[message(1), second],
            &[(7, 2)],
            true,
            false,
            false,
            false,
            false,
            100,
        ),
    );
}

/// Three cards with every side channel filled: tags, attachments, banners,
/// a translation, a folded card and a template.
#[test]
fn golden_three_cards_with_side_channels() {
    LIVE_TAGS.with(|t| {
        *t.borrow_mut() = vec![crate::config::Tag {
            name: "Work <x>".into(),
            keyword: "work".into(),
            color: "#33d17a".into(),
        }]
    });
    LIVE_ATTS.with(|a| {
        a.borrow_mut().insert(
            (7, 1),
            vec![CardAttachment {
                name: "report <q3>.pdf".into(),
                size: 123_456,
            }],
        );
    });
    LIVE_UNSUB.with(|u| {
        u.borrow_mut()
            .insert((7, 2), "<span>unsub banner</span>".into());
    });
    LIVE_INVITE.with(|i| {
        i.borrow_mut()
            .insert((7, 3), "<span>invite banner</span>".into());
    });
    LIVE_TRANSLATE.with(|t| {
        t.borrow_mut().insert(
            (7, 1),
            (
                "<span>tr banner</span>".into(),
                Some(("<p>translated</p>".into(), false)),
            ),
        );
    });
    LIVE_FOLDBAR.with(|f| {
        f.borrow_mut()
            .insert((7, 2), "<span>fold bar</span>".into());
    });
    LIVE_FOLDED.with(|f| {
        f.borrow_mut().insert((7, 2));
    });
    LIVE_TEMPLATES.with(|t| {
        t.borrow_mut().insert((7, 3));
    });
    LIVE_TRANSLATE_ON.with(|t| t.set(true));

    let mut first = message(1);
    first.keywords = vec!["work".into()];
    first.labels = vec!["Receipts".into()];
    first.has_attachment = true;
    let mut second = message(2);
    second.answered = true;
    second.body = "plain text body\nsecond line".into();
    let mut third = message(3);
    third.forwarded = true;
    third.bcc = "hidden@golden.example".into();
    let doc = document(
        &[first, second, third],
        &[(7, 1), (7, 3)],
        false,
        true,
        true,
        false,
        true,
        90,
    );
    golden("three_cards.html", &doc);
}

#[test]
fn golden_print_document() {
    let mut m = message(1);
    m.cc = "carol@golden.example".into();
    let header = print_header_html(Some(&m));
    let messages = vec![
        (
            print_message_header_html(&m),
            "<html><head><style>body{color:red} .body p{x:1}</style></head>\
             <body><p>one</p></body></html>"
                .to_string(),
        ),
        (
            print_message_header_html(&message(2)),
            "plain text".to_string(),
        ),
    ];
    golden(
        "print.html",
        &normalise_nonce(&print_document(&header, &messages, false)),
    );
}

#[test]
fn css_colors_parse_as_today() {
    let p = |t: &str, bare: bool| parse_css_color(t, bare);
    assert_eq!(p("#fff", false), Some([1.0, 1.0, 1.0, 1.0]));
    assert_eq!(
        p("#00000080", false).map(|c| (c[3] * 255.0).round()),
        Some(128.0)
    );
    assert_eq!(p("ffffff", false), None);
    assert_eq!(p("ffffff", true), Some([1.0, 1.0, 1.0, 1.0]));
    assert_eq!(p("rgb(255, 0, 0)", false), Some([1.0, 0.0, 0.0, 1.0]));
    assert_eq!(p("rgba(0,0,255,0.5)", false), Some([0.0, 0.0, 1.0, 0.5]));
    assert_eq!(p("rgb(100%, 0%, 0%)", false), Some([1.0, 0.0, 0.0, 1.0]));
    assert_eq!(p("WHITE", false), Some([1.0, 1.0, 1.0, 1.0]));
    // `transparent` is not one of the named colours it knows.
    assert_eq!(p("transparent", false), None);
    assert_eq!(p("currentColor", false), None);
    assert_eq!(p("#ggg", false), None);
    assert_eq!(p("", false), None);
}

#[test]
fn address_lists_split_outside_quotes_only() {
    assert_eq!(
        split_addr_list("\"Doe, Jane\" <j@x>, b@y"),
        vec!["\"Doe, Jane\" <j@x>", " b@y"]
    );
    assert_eq!(split_addr_list(""), vec![""]);
    assert_eq!(split_addr_list("a,,b"), vec!["a", "", "b"]);
    // An unbalanced quote swallows the rest of the list.
    assert_eq!(split_addr_list("\"a, b, c"), vec!["\"a, b, c"]);
}

#[test]
fn small_banner_rows_are_stable() {
    assert_eq!(
        template_row_html((7, 9)),
        "<div class=\"hylki-unsub hylki-tpl\"><span class=\"hylki-unsub-text\">This message is a template.</span><button type=\"button\" class=\"hylki-unsub-btn hylki-tpl-btn\" data-key=\"7:9\" data-tpl=\"edit\" title=\"Change the template itself\">Edit Template</button><button type=\"button\" class=\"hylki-unsub-btn hylki-tpl-btn\" data-key=\"7:9\" data-tpl=\"use\" title=\"Start a new message from a copy of the template\">Use Template</button></div>"
    );
    assert_eq!(
        unsub_row_html((1, 2), "<b>x</b>"),
        "<div class=\"hylki-unsub\" data-key=\"1:2\"><b>x</b></div>"
    );
    assert_eq!(
        invite_row_html((1, 2), ""),
        "<div class=\"hylki-inv\" data-key=\"1:2\"></div>"
    );
    assert_eq!(answer_note("plain".into(), "dated".into(), 0), "plain");
    assert_eq!(answer_note("plain".into(), "dated".into(), -5), "plain");
    assert_eq!(answer_note("plain".into(), "dated".into(), 1), "dated");
}

#[test]
fn tag_and_attachment_chips_are_stable() {
    let tags = vec![crate::config::Tag {
        name: "A&B".into(),
        keyword: "ab".into(),
        color: "#ffffff".into(),
    }];
    assert_eq!(
        tag_chips_html(&tags, &["ab".into()], &["Inbox <x>".into()]),
        "<span class=\"hylki-tag\" style=\"background:#ffffff;color:rgba(0,0,0,0.85)\">A&amp;B</span><span class=\"hylki-tag hylki-label\" style=\"background:#2ec27e;color:#ffffff\" title=\"Inbox &lt;x&gt;\">Inbox &lt;x&gt;</span>"
    );
    assert_eq!(tag_chips_html(&[], &[], &[]), "");
    let atts = [CardAttachment {
        name: "a<b>.zip".into(),
        size: 2048,
    }];
    // The icons are empty here: the resource bundle is not registered in tests.
    assert_eq!(
        att_chips_html((3, 4), &atts),
        "<span class=\"hylki-attw\"><button type=\"button\" class=\"hylki-attc ftype-archive\" data-key=\"3:4\" data-idx=\"0\" title=\"Open\"><span class=\"hylki-attn\">a&lt;b&gt;.zip</span><span class=\"hylki-attz\">2.0\u{a0}kB</span></button><button type=\"button\" class=\"hylki-attsave\" data-key=\"3:4\" data-idx=\"0\" title=\"Save…\"></button></span>"
    );
    assert_eq!(
        att_row_html((3, 4), None),
        "<div class=\"hylki-atts\" data-key=\"3:4\"></div>"
    );
}

#[test]
fn link_hosts_compare_as_today() {
    assert_eq!(
        mismatched_host("https://evil.example/x", "paypal.com"),
        Some("paypal.com".into())
    );
    assert_eq!(
        mismatched_host("https://mail.example.com/", "example.com"),
        None
    );
    assert_eq!(
        mismatched_host("https://example.com/", "www.example.com"),
        None
    );
    assert_eq!(mismatched_host("https://a.example/", "click here"), None);
    assert_eq!(mismatched_host("mailto:x@y.example", "z.example"), None);
    assert_eq!(
        mismatched_host("https://a.example/", "HTTP://B.Example:80/x"),
        Some("b.example".into())
    );
    assert_eq!(
        mismatched_host("https://a.example/", "ftp://b.example"),
        None
    );
    assert_eq!(host_of("user@b.example."), Some("b.example".into()));
}

#[test]
fn remote_urls_are_classified_as_today() {
    let r = |u: &str| is_remote_url(u.as_bytes());
    assert!(r("//cdn.example/x"));
    assert!(r("https://x"));
    assert!(r("HTTP://x"));
    assert!(r("ftp://x"));
    assert!(r("weird+scheme.x-y:z"));
    assert!(!r("data:image/png;base64,AAAA"));
    assert!(!r("CID:part1"));
    assert!(!r("blocked://x"));
    assert!(!r("mailto:a@b"));
    assert!(!r("tel:1"));
    assert!(!r("about:blank"));
    assert!(!r("javascript:void(0)"));
    assert!(!r("/relative/path"));
    assert!(!r("img.png"));
    assert!(!r(":nothing"));
    assert!(!r("  "));
}

#[test]
fn stylesheets_scope_to_their_block_as_today() {
    assert_eq!(
        scope_css(
            "html, body { x:1 } .body p { y:2 } @media print { body > p { z:3 } }",
            ".m0"
        ),
        ".m0, .m0 { x:1 } .body p { y:2 } @media print { .m0 > p { z:3 } }"
    );
    assert_eq!(
        scope_selectors("bodyguard, #body, BODY", ".m"),
        "bodyguard, #body, .m"
    );
    assert_eq!(scope_selectors("div%html", ".m"), "div%html");
    assert_eq!(
        scope_styles("<STYLE>body{a:b}</style><p>body</p><style>html{c:d}", ".k"),
        "<STYLE>.k{a:b}</style><p>body</p><style>.k{c:d}"
    );
}
