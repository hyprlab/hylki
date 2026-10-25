//! Card snippets: direction, the menu lookup, recipients, invitations and
//! attachment rows.

use super::*;

/// A message takes the direction of its text unless it names one (#366).
#[test]
fn a_message_takes_its_own_direction() {
    let plain = auto_direction(&body_html("سلام\nHello"));
    assert!(
        plain.contains("<body dir=\"auto\" class=\"hylki-plain\">"),
        "{plain}"
    );
    assert!(
        plain.contains("body,.hylki-plain{unicode-bidi:plaintext;}"),
        "{plain}"
    );

    let html = auto_direction("<html><head></head><body class=\"x\"><p>x</p></body></html>");
    assert!(html.contains("<body dir=\"auto\" class=\"x\">"), "{html}");
    assert!(!html.contains("unicode-bidi"), "{html}");

    let parts = auto_direction("<html><body><div class=\"hylki-plain\">a</div></body></html>");
    assert!(
        parts.contains(".hylki-plain{unicode-bidi:plaintext;}"),
        "{parts}"
    );
    assert!(!parts.contains("body,.hylki-plain"), "{parts}");

    // The sender's own direction stands, on either element.
    for own in [
        "<html dir=\"rtl\"><body><p>x</p></body></html>",
        "<html><body DIR='ltr'><p>x</p></body></html>",
        "<html><body style=\"x\" dir = \"rtl\"><p>x</p></body></html>",
    ] {
        assert_eq!(auto_direction(own), own);
    }
    // Not a dir attribute, and not a body tag.
    let tricky =
        auto_direction("<html><body data-dir=\"x\"><bodyguard>y</bodyguard></body></html>");
    assert!(
        tricky.contains("<body dir=\"auto\" data-dir=\"x\">"),
        "{tricky}"
    );
    // No body tag: the root carries it.
    let bare = auto_direction("<html><p>x</p></html>");
    assert!(bare.starts_with("<html dir=\"auto\">"), "{bare}");
}

/// The page's answer to a right-click: the card it landed on (or the
/// nearest) and any selected text. An empty or broken answer names no
/// card, which the reader then fills in rather than dropping the menu.
#[test]
fn a_right_click_names_its_card() {
    assert_eq!(
        menu_lookup(r#"{"key":"2:41","sel":"  some text "}"#),
        (Some((2, 41)), Some("  some text ".to_string()))
    );
    assert_eq!(menu_lookup(r#"{"key":"","sel":"   "}"#), (None, None));
    assert_eq!(menu_lookup(""), (None, None));
    assert!(menu_skipped(r#"{"skip":1}"#));
    assert!(!menu_skipped(r#"{"key":"2:41","sel":""}"#));
    assert!(!menu_skipped(""));
}

// ===== Meeting invitations (#223) =====

/// A sent copy's Bcc recipients are listed under To and Cc, and counted
/// on the chip (#476); a message without any has no Bcc line.
#[test]
fn the_recipients_block_lists_bcc() {
    let mut m = msg_for_print();
    assert!(!recipients_html(&m, true).contains("Bcc:"));
    m.bcc = "hidden@example.com".into();
    let html = recipients_html(&m, true);
    assert!(
        html.contains("<b>Bcc:</b> <span class=\"hylki-mail\" data-mail=\"hidden@example.com\">"),
        "{html}"
    );
    assert!(html.find("<b>Cc:</b>") < html.find("<b>Bcc:</b>"));
    assert_eq!(recipient_count(&m), 3);
    assert!(print_header_html(Some(&m)).contains("hidden@example.com"));
}

/// The banner's container is emitted for every card whether or not the
/// message is about a meeting, so an invitation read after the paint has
/// somewhere to be patched into.
#[test]
fn every_card_has_somewhere_to_put_an_invitation() {
    let a = msg_for_print();
    let doc = MessageView::conversation_document(
        &[a],
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
    assert!(
        doc.contains("<div class=\"hylki-inv\" data-key=\"1:1\"></div>"),
        "{doc}"
    );
    // And above the body, not below it: the meeting is the point of the
    // message, and the calendar's own prose repeats it underneath.
    let inv = doc.find("<div class=\"hylki-inv\"").expect("the container");
    let body = doc.find("<iframe").expect("the body frame");
    assert!(inv < body, "the banner comes before the body");
}

/// The invitation row is the meeting's own markup, keyed to its card.
#[test]
fn the_invitation_row_is_keyed_to_its_card() {
    assert_eq!(
        invite_row_html((2, 9), ""),
        "<div class=\"hylki-inv\" data-key=\"2:9\"></div>"
    );
    assert!(invite_row_html((2, 9), "<b>x</b>").ends_with("<b>x</b></div>"));
}

/// A repeating meeting says so in a few words; a one-off says nothing.
#[test]
fn a_repeating_meeting_is_summarised_not_spelled_out() {
    assert_eq!(repeat_text(""), None);
    assert_eq!(
        repeat_text("FREQ=WEEKLY"),
        Some("Repeats weekly".to_string())
    );
    assert_eq!(
        repeat_text("FREQ=WEEKLY;INTERVAL=2;BYDAY=TU,TH;UNTIL=20270101T000000Z"),
        Some("Repeats every 2 weeks".to_string())
    );
    assert_eq!(
        repeat_text("FREQ=MONTHLY;BYSETPOS=-1"),
        Some("Repeats monthly".to_string())
    );
    // A rule in a form we don't spell out still says that it repeats.
    assert_eq!(repeat_text("FREQ=SECONDLY"), Some("Repeats".to_string()));
}

/// An answer from somebody else names them and what they said.
#[test]
fn a_reply_message_says_who_answered() {
    let person = |name: &str, status: &str| crate::models::InvitePerson {
        name: name.into(),
        email: "guest@example.org".into(),
        status: status.into(),
        rsvp: false,
        resource: false,
    };
    let mut inv = crate::models::Invite {
        method: "REPLY".into(),
        ..Default::default()
    };
    // The organizer's own line carries no answer; the guest's does.
    inv.attendees = vec![
        person("Chair", "NEEDS-ACTION"),
        person("Marcus Chen", "DECLINED"),
    ];
    assert_eq!(reply_note(&inv), "Marcus Chen declined this invitation.");
    inv.attendees = vec![person("Marcus Chen", "ACCEPTED")];
    assert_eq!(reply_note(&inv), "Marcus Chen accepted this invitation.");
    // Nobody with a verdict at all: say so rather than name nobody.
    inv.attendees = vec![person("Marcus Chen", "NEEDS-ACTION")];
    assert_eq!(reply_note(&inv), "Somebody answered this invitation.");
}

/// Only an invitation that is still standing and names an organizer is
/// one there is anything to answer.
#[test]
fn what_can_be_answered() {
    let organizer = Some(crate::models::InvitePerson {
        email: "chair@example.org".into(),
        ..Default::default()
    });
    let base = crate::models::Invite {
        method: "REQUEST".into(),
        uid: "u".into(),
        organizer: organizer.clone(),
        ..Default::default()
    };
    assert!(base.asks_for_an_answer());
    assert!(!crate::models::Invite {
        cancelled: true,
        ..base.clone()
    }
    .asks_for_an_answer());
    assert!(!crate::models::Invite {
        method: "REPLY".into(),
        ..base.clone()
    }
    .asks_for_an_answer());
    assert!(!crate::models::Invite {
        method: "PUBLISH".into(),
        ..base.clone()
    }
    .asks_for_an_answer());
    assert!(!crate::models::Invite {
        organizer: None,
        ..base.clone()
    }
    .asks_for_an_answer());
    // No UID: nothing an answer could name.
    assert!(!crate::models::Invite {
        uid: String::new(),
        ..base
    }
    .asks_for_an_answer());
}

// ===== Per-card attachment rows (#213) =====

/// A card's attachment chips name the message and the file's index, so a
/// click can be answered; the row exists even with nothing in it, so a
/// late arrival has somewhere to land; and a sender-chosen filename is
/// text, never markup.
#[test]
fn a_cards_attachment_row_lists_its_own_files() {
    let atts = vec![
        CardAttachment {
            name: "report.pdf".into(),
            size: 2048,
        },
        CardAttachment {
            name: "<b>x</b>.png".into(),
            size: 10,
        },
    ];
    let row = att_row_html((1, 7), Some(&atts));
    assert!(
        row.starts_with("<div class=\"hylki-atts\" data-key=\"1:7\">"),
        "{row}"
    );
    assert_eq!(row.matches("class=\"hylki-attc ").count(), 2, "{row}");
    assert!(
        row.contains("class=\"hylki-attc ftype-pdf\" data-key=\"1:7\" data-idx=\"0\""),
        "{row}"
    );
    assert!(
        row.contains("class=\"hylki-attc ftype-image\" data-key=\"1:7\" data-idx=\"1\""),
        "{row}"
    );
    assert!(
        row.contains("class=\"hylki-attsave\" data-key=\"1:7\" data-idx=\"1\""),
        "{row}"
    );
    assert!(row.contains(&crate::models::human_size(2048)), "{row}");
    assert!(!row.contains("<b>x</b>"), "filename is escaped: {row}");
    assert!(row.contains("&lt;b&gt;x&lt;/b&gt;.png"), "{row}");
    // Nothing attached: the row is there, and empty.
    assert_eq!(
        att_row_html((1, 8), None),
        "<div class=\"hylki-atts\" data-key=\"1:8\"></div>"
    );
}
