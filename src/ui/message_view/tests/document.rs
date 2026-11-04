//! The conversation document: cards, headers, selection and escaping.

use super::*;

/// A conversation is a stack of cards in the order it is handed, so the
/// reader shows the message that started it first and the newest last.
#[test]
fn a_conversation_renders_one_card_per_message_in_order() {
    let mut first = msg_for_print();
    first.body = "<p>opening</p>".into();
    let mut second = msg_for_print();
    second.id = 2;
    second.from_name = "Grace Hopper".into();
    second.body = "<p>reply</p>".into();

    let doc = MessageView::conversation_document(
        &[first, second],
        &std::collections::HashMap::new(),
        &Default::default(),
        &Default::default(),
        &[],
        "#3584e4",
        true,
        false,
        false,
        false,
        &crate::config::ReaderStyle::NONE,
        &Default::default(),
        false,
        100,
    );
    assert_eq!(
        doc.matches("<section class=\"hylki-msg\"").count(),
        2,
        "each message gets its own card: {doc}"
    );
    let ada = doc.find("Ada Lovelace").expect("first sender present");
    let grace = doc.find("Grace Hopper").expect("second sender present");
    assert!(ada < grace, "cards must keep the order they were handed");
    assert!(
        doc.contains("<body class=\"hylki-conv\">"),
        "conversation padding"
    );
    // The cards sit on the deeper page — the same color the spinner and the
    // cover behind the WebView are painted, so the handover is invisible.
    assert!(
        doc.contains(&format!("background:{}", PAGE.0)),
        "page ground: {doc}"
    );
}

/// A card from a mailbox of your own wears the face that mailbox was given
/// (#189) — the picture it was handed, or its emoji on the account color
/// — while every other sender keeps their tinted initials.
#[test]
fn a_card_from_your_own_mailbox_wears_that_mailboxs_face() {
    let mut mine = msg_for_print();
    mine.id = 2;
    mine.from_name = "Me".into();
    mine.from_addr = "me@example.com".into();
    let build = |thread: &[Message]| {
        MessageView::conversation_document(
            thread,
            &std::collections::HashMap::new(),
            &Default::default(),
            &Default::default(),
            &[],
            "#3584e4",
            true,
            false,
            false,
            false,
            &crate::config::ReaderStyle::NONE,
            &Default::default(),
            false,
            100,
        )
    };

    crate::services::avatar::set_own_faces([(
        "Me@Example.com".to_string(),
        crate::services::avatar::OwnFace {
            gravatar: false,
            picture: None,
            emoji: Some("\u{1F98A}".to_string()),
            color: "#e66100".to_string(),
        },
    )]);
    let doc = build(std::slice::from_ref(&mine));
    // Drawn with a renderer where there is one, as markup where there is
    // not — either way it is the mailbox's own face, not the initials
    // circle a stranger's address would be tinted into.
    assert!(
        doc.contains("\u{1F98A}") || doc.contains("src=\"data:image/png;base64,"),
        "the mailbox's emoji is the face: {doc}"
    );
    assert!(!doc.contains("hsl("), "and not the per-sender tint: {doc}");
    // Someone else's card is untouched by any of this.
    let doc = build(&[msg_for_print()]);
    assert!(
        !doc.contains("\u{1F98A}"),
        "another sender keeps their own circle: {doc}"
    );

    // A picture wins over the emoji, and rides along embedded: the
    // document is handed the image, never a path it could go and read.
    let dir = std::env::temp_dir().join(format!("hylki-own-face-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory to write the picture in");
    let path = dir.join("avatar.png");
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, true, 8, 64, 64)
        .expect("a pixbuf");
    pixbuf.fill(0xe661_00ff);
    pixbuf.savev(&path, "png", &[]).expect("a picture on disk");
    crate::services::avatar::set_own_faces([(
        "me@example.com".to_string(),
        crate::services::avatar::OwnFace {
            gravatar: false,
            picture: Some(path.clone()),
            emoji: Some("\u{1F98A}".to_string()),
            color: "#e66100".to_string(),
        },
    )]);
    let doc = build(std::slice::from_ref(&mine));
    assert!(
        doc.contains("<img class=\"hylki-ava\" src=\"data:image/png;base64,"),
        "the picture is embedded in the card: {doc}"
    );
    assert!(
        !doc.contains("\u{1F98A}"),
        "the picture wins over the emoji: {doc}"
    );
    assert!(
        !doc.contains(path.to_string_lossy().as_ref()),
        "the file is never linked: {doc}"
    );

    crate::services::avatar::set_own_faces(std::iter::empty());
    let _ = std::fs::remove_file(&path);
}

/// Each card names everyone the message went to, collapsed behind a chip so
/// the header stays one line tall — and a recipient list is header text an
/// attacker controls, so it must land escaped in the trusted wrapper.
#[test]
fn every_card_carries_its_recipients_escaped_and_collapsed() {
    let mut first = msg_for_print();
    first.to = "me@example.com, <script>alert(1)</script>@evil.test".into();
    let mut second = msg_for_print();
    second.id = 2;
    second.cc = String::new();

    let doc = MessageView::conversation_document(
        &[first, second],
        &std::collections::HashMap::new(),
        &Default::default(),
        &Default::default(),
        &[],
        "#3584e4",
        true,
        false,
        false,
        false,
        &crate::config::ReaderStyle::NONE,
        &Default::default(),
        false,
        100,
    );
    assert_eq!(
        doc.matches("class=\"hylki-rcpt\" hidden").count(),
        2,
        "each card gets a collapsed To/Cc block: {doc}"
    );
    assert_eq!(
        doc.matches("class=\"hylki-rcpt-toggle\"").count(),
        2,
        "each card gets a recipients chip: {doc}"
    );
    // 3 addresses on the first card (2 To + 1 Cc), 1 on the second.
    assert!(
        doc.contains(">3 recipients<"),
        "recipient count on the chip: {doc}"
    );
    assert!(
        doc.contains(">1 recipient<"),
        "singular for one recipient: {doc}"
    );
    assert!(
        !doc.contains("<script>alert(1)</script>"),
        "recipients must be escaped: {doc}"
    );
    assert!(
        doc.contains("&lt;script&gt;"),
        "escaped form present: {doc}"
    );
}

/// A message that names no recipients at all gets neither chip nor block.
#[test]
fn a_card_with_no_recipients_shows_no_chip() {
    let mut only = msg_for_print();
    only.to = String::new();
    only.cc = " ".into();
    let mut second = msg_for_print();
    second.id = 2;
    let doc = MessageView::conversation_document(
        &[only, second],
        &std::collections::HashMap::new(),
        &Default::default(),
        &Default::default(),
        &[],
        "#3584e4",
        true,
        false,
        false,
        false,
        &crate::config::ReaderStyle::NONE,
        &Default::default(),
        false,
        100,
    );
    assert_eq!(
        doc.matches("<button type=\"button\" class=\"hylki-rcpt-toggle\"")
            .count(),
        1,
        "only the second card: {doc}"
    );
}

#[test]
fn a_card_offers_remote_content_both_ways_and_only_where_there_is_any() {
    let mut with_remote = msg_for_print();
    with_remote.body = "<p>Hello</p><img src=\"https://example.com/track.gif\">".into();
    let mut without = msg_for_print();
    without.id = 2;
    without.body = "<p>Just words.</p>".into();
    let doc_for = |restrict: bool| {
        MessageView::conversation_document(
            &[with_remote.clone(), without.clone()],
            &std::collections::HashMap::new(),
            &Default::default(),
            &Default::default(),
            &[],
            "#3584e4",
            restrict,
            false,
            false,
            false,
            &crate::config::ReaderStyle::NONE,
            &Default::default(),
            false,
            100,
        )
    };
    // Only the card with something to load carries the button.
    let blocked = doc_for(true);
    assert_eq!(
        blocked.matches("data-act=\"remote\"").count(),
        1,
        "{blocked}"
    );
    assert!(blocked.contains("Show remote content"), "{blocked}");
    assert!(
        !blocked.contains("class=\"hylki-act on\" data-act=\"remote\""),
        "not lit while blocked"
    );
    // Showing it, the same button offers to put the block back, and is lit.
    let shown = doc_for(false);
    assert_eq!(shown.matches("data-act=\"remote\"").count(), 1, "{shown}");
    assert!(shown.contains("Block remote content"), "{shown}");
    assert!(
        shown.contains("class=\"hylki-act on\" data-act=\"remote\""),
        "lit while showing: {shown}"
    );
}

/// Each card carries its own Reply/Reply all/Forward, keyed to that message
/// — the toolbar's buttons are disabled in a conversation precisely because
/// they could not say which message they meant.
#[test]
fn every_card_carries_its_own_actions_keyed_to_that_message() {
    let first = msg_for_print();
    let mut second = msg_for_print();
    second.id = 2;
    let doc = MessageView::conversation_document(
        &[first, second],
        &std::collections::HashMap::new(),
        &Default::default(),
        &Default::default(),
        &[],
        "#3584e4",
        true,
        false,
        false,
        false,
        &crate::config::ReaderStyle::NONE,
        &Default::default(),
        false,
        100,
    );
    for act in ["reply", "replyall", "forward"] {
        assert_eq!(
            doc.matches(&format!("data-act=\"{act}\"")).count(),
            2,
            "one {act} button per card: {doc}"
        );
    }
    // Keyed per message, so the action can't land on the wrong one.
    assert!(doc.contains("data-act=\"reply\" data-key=\"1:1\""), "{doc}");
    assert!(doc.contains("data-act=\"reply\" data-key=\"1:2\""), "{doc}");
}

/// An unread message in a conversation is marked with its dot; reading is
/// click-driven now, so no scroll sentinel exists for any message.
#[test]
fn only_unread_conversation_messages_are_marked() {
    let read = msg_for_print();
    let mut unread = msg_for_print();
    unread.id = 2;
    unread.unread = true;

    let doc = MessageView::conversation_document(
        &[read, unread],
        &std::collections::HashMap::new(),
        &Default::default(),
        &Default::default(),
        &[],
        "#3584e4",
        true,
        false,
        false,
        false,
        &crate::config::ReaderStyle::NONE,
        &Default::default(),
        false,
        100,
    );
    assert_eq!(
        doc.matches("class=\"hylki-dot\"").count(),
        1,
        "one dot: {doc}"
    );
    assert_eq!(
        doc.matches("class=\"hylki-end\"").count(),
        0,
        "no sentinel: {doc}"
    );
    // Keyed to the unread message, so reading it can't clear another's.
    assert!(
        doc.contains("class=\"hylki-dot\" data-key=\"1:2\""),
        "{doc}"
    );
}

/// Marking a message unread while its conversation is open must survive the
/// message being in view: it keeps its dot, but no sentinel, so nothing
/// reads it back until the conversation is opened afresh.
#[test]
fn a_deliberately_unread_message_keeps_its_mark() {
    let read = msg_for_print();
    let mut unread = msg_for_print();
    unread.id = 2;
    unread.unread = true;
    let suppressed: std::collections::HashSet<(u32, u32)> = [(1u32, 2u32)].into();

    let doc = MessageView::conversation_document(
        &[read, unread],
        &std::collections::HashMap::new(),
        &suppressed,
        &Default::default(),
        &[],
        "#3584e4",
        true,
        false,
        false,
        false,
        &crate::config::ReaderStyle::NONE,
        &Default::default(),
        false,
        100,
    );
    assert_eq!(
        doc.matches("class=\"hylki-dot\"").count(),
        1,
        "still marked: {doc}"
    );
    assert_eq!(
        doc.matches("class=\"hylki-end\"").count(),
        0,
        "no sentinel: {doc}"
    );
}

/// A frame opens at the height it had last time, so a reopened conversation
/// lays out on its first frame rather than every card jumping once measured.
#[test]
fn a_known_frame_height_is_used_on_reopen() {
    let a = msg_for_print();
    let mut b = msg_for_print();
    b.id = 2;
    let heights: std::collections::HashMap<(u32, u32), u32> = [((1u32, 2u32), 640u32)].into();

    let doc = MessageView::conversation_document(
        &[a, b],
        &Default::default(),
        &Default::default(),
        &heights,
        &[],
        "#3584e4",
        true,
        false,
        false,
        false,
        &crate::config::ReaderStyle::NONE,
        &Default::default(),
        false,
        100,
    );
    assert!(
        doc.contains("style=\"height:640px\""),
        "known height used: {doc}"
    );
    // The unmeasured one opens collapsed rather than at the browser's
    // 150px default, so its card shows nothing until its document is in.
    assert_eq!(
        doc.matches("style=\"height:0px\"").count(),
        1,
        "unknown one collapsed: {doc}"
    );
    assert_eq!(doc.matches("style=\"height:").count(), 2, "one each");
    // Keyed per message, so a height can't be applied to the wrong frame.
    assert!(doc.contains("data-key=\"1:2\""), "{doc}");
}

/// A selected message is outlined in the accent color, and only that one.
#[test]
fn a_selected_card_is_outlined_in_the_accent() {
    let a = msg_for_print();
    let mut b = msg_for_print();
    b.id = 2;
    let doc = MessageView::conversation_document(
        &[a, b],
        &Default::default(),
        &Default::default(),
        &Default::default(),
        &[(1, 2)],
        "#ff8800",
        true,
        false,
        false,
        false,
        &crate::config::ReaderStyle::NONE,
        &Default::default(),
        false,
        100,
    );
    assert!(
        doc.contains("class=\"hylki-msg selected\" data-key=\"1:2\""),
        "{doc}"
    );
    assert_eq!(
        doc.matches("hylki-msg selected").count(),
        1,
        "only the selected one"
    );
    assert!(
        doc.contains("box-shadow:0 0 0 2px #ff8800"),
        "outlined in the accent: {doc}"
    );
}

/// A lone message gets the same in-document header a conversation card
/// does — its own action icons included, since the toolbar carries no
/// per-message actions — but goes full-bleed: no card gutter, so it reads
/// as a message, not a card.
#[test]
fn a_single_message_keeps_the_header_but_goes_full_bleed() {
    let doc = MessageView::conversation_document(
        &[msg_for_print()],
        &std::collections::HashMap::new(),
        &Default::default(),
        &Default::default(),
        &[],
        "#3584e4",
        true,
        false,
        false,
        false,
        &crate::config::ReaderStyle::NONE,
        &Default::default(),
        false,
        100,
    );
    assert!(
        doc.contains("<section class=\"hylki-msg\""),
        "message chrome: {doc}"
    );
    assert!(
        doc.contains("class=\"hylki-msg-hdr\""),
        "in-document header: {doc}"
    );
    assert!(
        !doc.contains("<body class=\"hylki-conv\">"),
        "no card gutter: {doc}"
    );
    assert!(
        doc.contains("data-act=\"reply\""),
        "card actions present: {doc}"
    );
    // Full-bleed also means the plain ground, not the cards' deeper page.
    assert!(
        doc.contains(&format!("background:{}", GROUND.0)),
        "plain ground: {doc}"
    );
}

/// With the "single messages as cards" preference on (#57), a lone
/// message renders exactly like a one-message conversation: carded, on
/// the deeper page ground.
#[test]
fn a_single_message_cards_up_when_asked() {
    let doc = MessageView::conversation_document(
        &[msg_for_print()],
        &std::collections::HashMap::new(),
        &Default::default(),
        &Default::default(),
        &[],
        "#3584e4",
        true,
        false,
        false,
        true,
        &crate::config::ReaderStyle::NONE,
        &Default::default(),
        false,
        100,
    );
    assert!(
        doc.contains("<body class=\"hylki-conv\">"),
        "card gutter: {doc}"
    );
    assert!(
        doc.contains(&format!("background:{}", PAGE.0)),
        "deeper page ground: {doc}"
    );
}

#[test]
fn a_message_from_another_folder_is_labelled_with_it() {
    let a = msg_for_print(); // the message on screen
    let mut b = msg_for_print();
    b.id = 2;
    b.folder_id = 3; // pulled in from Sent
    let labels = std::collections::HashMap::from([((1u32, 2u32), "Sent".to_string())]);
    let doc = MessageView::conversation_document(
        &[a, b],
        &labels,
        &Default::default(),
        &Default::default(),
        &[],
        "#3584e4",
        true,
        false,
        false,
        false,
        &crate::config::ReaderStyle::NONE,
        &Default::default(),
        false,
        100,
    );
    assert_eq!(
        doc.matches("class=\"hylki-folder\"").count(),
        // on the message that came from Sent, and never on the one the
        // reader is already showing the folder of.
        1,
        "only the message from another folder should carry a folder badge"
    );
    assert!(
        doc.contains(">Sent</span>"),
        "the badge should name the folder"
    );
}

#[test]
fn a_folder_label_cannot_smuggle_markup_into_the_header() {
    // Folder names come from the server (a mailbox can be called anything),
    // and this document is the trusted wrapper around the sandboxed frames.
    let a = msg_for_print();
    let mut b = msg_for_print();
    b.id = 2;
    let labels =
        std::collections::HashMap::from([((1u32, 2u32), "<img src=x onerror=alert(1)>".into())]);
    let doc = MessageView::conversation_document(
        &[a, b],
        &labels,
        &Default::default(),
        &Default::default(),
        &[],
        "#3584e4",
        true,
        false,
        false,
        false,
        &crate::config::ReaderStyle::NONE,
        &Default::default(),
        false,
        100,
    );
    assert!(
        !doc.contains("<img src=x"),
        "the label must be escaped, not rendered"
    );
    assert!(doc.contains("&lt;img src=x"));
}

#[test]
fn a_sender_cannot_put_markup_in_the_conversation_header() {
    // The wrapper document has JavaScript enabled (it sizes the message
    // iframes) and the iframes are same-origin, so anything that executes
    // here can read every message body in the thread. A `From:` display name
    // is attacker-controlled and RFC 2047-decoded, so it can carry any bytes
    // at all — it has to reach the page as text, never as markup.
    let mut a = msg_for_print();
    a.from_name = "<script>x=1</script>".into();
    a.from_addr = "<img src=y onerror=z>@example.com".into();
    let mut b = msg_for_print();
    b.id = 2;
    // Two messages: the per-message headers only render in conversation mode.
    let doc = MessageView::conversation_document(
        &[a, b],
        &Default::default(),
        &Default::default(),
        &Default::default(),
        &[],
        "#3584e4",
        true,
        false,
        false,
        false,
        &crate::config::ReaderStyle::NONE,
        &Default::default(),
        false,
        100,
    );

    assert!(!doc.contains("<script>x=1"), "{doc}");
    assert!(!doc.contains("<img src=y"), "{doc}");
    assert!(doc.contains("&lt;script&gt;x=1&lt;/script&gt;"), "{doc}");
    assert!(doc.contains("onerror=z&gt;"), "{doc}");
}

/// A folded card hides its frame, and a hidden frame still loads its whole
/// document: a long conversation folded down to its newest card rendered
/// every message at once. The folded card's frame keeps its document aside
/// until the card opens; an open card's frame loads it as before.
#[test]
fn a_folded_card_does_not_load_its_message_until_opened() {
    let mut first = msg_for_print();
    first.body = "<p>opening</p>".into();
    let mut second = msg_for_print();
    second.id = 2;
    second.body = "<p>reply</p>".into();
    LIVE_FOLDED.with(|f| *f.borrow_mut() = [(1, 1)].into_iter().collect());

    let doc = MessageView::conversation_document(
        &[first, second],
        &std::collections::HashMap::new(),
        &Default::default(),
        &Default::default(),
        &[],
        "#3584e4",
        true,
        false,
        false,
        false,
        &crate::config::ReaderStyle::NONE,
        &Default::default(),
        false,
        100,
    );
    LIVE_FOLDED.with(|f| f.borrow_mut().clear());

    let frame = |key: &str| {
        let at = doc
            .find(&format!("<iframe class=\"hylki-frame\" data-key=\"{key}\""))
            .expect("the card's frame");
        doc[at..doc[at..].find("</iframe>").map(|e| at + e).unwrap()].to_string()
    };
    let folded = frame("1:1");
    assert!(
        folded.contains(" data-hylki-srcdoc=\"") && !folded.contains(" srcdoc=\""),
        "a folded card keeps its document aside: {folded}"
    );
    let open = frame("1:2");
    assert!(
        open.contains(" srcdoc=\"") && !open.contains("data-hylki-srcdoc"),
        "an open card loads its document: {open}"
    );
}
