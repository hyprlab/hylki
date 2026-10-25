use crate::contacts::{SuggestedGroup, Suggestion};

fn suggestion(name: &str, email: &str, score: u32) -> Suggestion {
    Suggestion {
        name: name.into(),
        email: email.into(),
        score,
        ..Default::default()
    }
}

#[test]
fn initial_suggestions_keep_addresses_added_while_loading() {
    let mut current = vec![
        suggestion("Ann", "ANN@example.org", 2),
        suggestion("Bob", "bob@example.org", 1),
    ];
    let mut contact = suggestion("Ann Contact", "ann@example.org", 5);
    contact.from_contacts = true;
    contact.own = true;
    let mut group = suggestion("Book Club", "", 0);
    group.group = Some(std::sync::Arc::new(SuggestedGroup {
        members: Vec::new(),
        hide_addresses: true,
    }));
    super::suggest::merge_loaded_suggestions(&mut current, vec![contact, group]);
    assert_eq!(current.len(), 3);
    let ann = &current[0];
    assert_eq!(ann.name, "Ann Contact");
    assert_eq!(ann.score, 5);
    assert!(ann.from_contacts && ann.own);
    assert!(current[1].group.is_some());
    assert_eq!(current[2].email, "bob@example.org");
}

#[test]
fn initial_suggestions_keep_newer_names_and_frequency() {
    let mut current = vec![suggestion("Ann", "ann@example.org", 3)];
    super::suggest::merge_loaded_suggestions(
        &mut current,
        vec![suggestion("ann@example.org", "ann@example.org", 1)],
    );
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].name, "Ann");
    assert_eq!(current[0].score, 3);
    super::suggest::merge_loaded_suggestions(&mut current, Vec::new());
    assert_eq!(current[0].name, "Ann");
}

#[test]
fn a_markdown_reply_has_a_line_to_write_on_above_the_quote() {
    let reply = "<p><br></p><p class=\"hylki-quote-attr\">On Monday, Ann wrote:</p><blockquote><p>Hi</p></blockquote>";
    let md = super::markdown_body(reply);
    assert!(md.starts_with("\n\nOn Monday, Ann wrote:"), "{md:?}");
    // A new message stays empty, and written text is left as it is.
    assert_eq!(super::markdown_body("<p><br></p>"), "");
    assert!(super::markdown_body("<p>Hello</p>").starts_with("Hello"));
}

use super::signature::{sig_html, sig_source};
use crate::ui::rich_editor::SourceKind;

#[test]
fn the_separator_line_follows_the_setting() {
    assert_eq!(
        sig_html("Ann", false),
        "<div class=\"hylki-sig\"><br>Ann</div>"
    );
    assert_eq!(
        sig_html("Ann", true),
        "<div class=\"hylki-sig\"><br>-- <br>Ann</div>"
    );
    assert!(!sig_source(SourceKind::Markdown, "Ann", false).contains("-- "));
    assert!(sig_source(SourceKind::Markdown, "Ann", true).starts_with("\n\n-- \n"));
    assert_eq!(sig_source(SourceKind::Markdown, "", true), "");
}

// Characterization tests (restructure, area D).

#[test]
fn pinned_an_html_source_signature_is_the_rich_block() {
    assert_eq!(
        sig_source(SourceKind::Html, "Ann\nB", true),
        "\n<div class=\"hylki-sig\"><br>-- <br>Ann<br>B</div>\n"
    );
    // Anything with a `<` is taken as HTML.
    assert_eq!(
        sig_source(SourceKind::Markdown, "Ann\nB <x>", false),
        "\n\nAnn B\n"
    );
}

#[test]
fn pinned_froms_compare_by_name_and_address() {
    assert!(super::send::same_from(
        "Ann <ann@x.org>",
        "\"Ann\" <ANN@x.org>"
    ));
    assert!(super::send::same_from(
        "Ann <ann@x.org>",
        "Ann  <ann@x.org>"
    ));
    assert!(!super::send::same_from("Ann <ann@x.org>", "ann@x.org"));
    assert!(!super::send::same_from("", ""));
}

#[test]
fn pinned_attachment_labels() {
    let pdf = std::path::Path::new("/a/b.pdf");
    assert_eq!(
        super::attachments::attachment_label("Attach {name}", pdf, 1),
        "Attach b.pdf"
    );
    assert_eq!(
        super::attachments::attachment_label("Attach {name}", pdf, 3),
        "Attach 3 attachments"
    );
    assert_eq!(
        super::attachments::attachment_label("Attach {name}", std::path::Path::new("/"), 1),
        "Attach "
    );
}

#[test]
fn pinned_format_icons() {
    use crate::config::ComposeFormat;
    assert_eq!(
        super::format::format_icon(ComposeFormat::Rich),
        "format-text-rich-symbolic"
    );
    assert_eq!(
        super::format::format_icon(ComposeFormat::Markdown),
        "markdown-symbolic"
    );
    assert_eq!(
        super::format::format_icon(ComposeFormat::Html),
        "code-symbolic"
    );
    assert_eq!(
        super::format::format_icon(ComposeFormat::Plain),
        "text-x-generic-symbolic"
    );
}

#[test]
fn pinned_key_tables() {
    use gtk::gdk::{Key, ModifierType as M};
    let cs = M::CONTROL_MASK | M::SHIFT_MASK;
    assert!(matches!(
        super::keys::composer_key(Key::A, cs),
        Some(super::ComposeInput::AttachFiles)
    ));
    assert!(matches!(
        super::keys::composer_key(Key::t, cs),
        Some(super::ComposeInput::FocusField(super::types::Field::To))
    ));
    assert!(matches!(
        super::keys::composer_key(Key::c, cs),
        Some(super::ComposeInput::FocusField(super::types::Field::Cc))
    ));
    assert!(matches!(
        super::keys::composer_key(Key::b, cs),
        Some(super::ComposeInput::FocusField(super::types::Field::Bcc))
    ));
    assert!(super::keys::composer_key(Key::a, M::CONTROL_MASK).is_none());
    assert!(super::keys::composer_key(Key::a, cs | M::ALT_MASK).is_none());
    assert!(super::keys::composer_key(Key::x, cs).is_none());

    let c = M::CONTROL_MASK;
    for (key, step) in [
        (Key::plus, 1),
        (Key::equal, 1),
        (Key::KP_Add, 1),
        (Key::minus, -1),
        (Key::KP_Subtract, -1),
        (Key::_0, 0),
        (Key::KP_0, 0),
    ] {
        assert_eq!(super::keys::zoom_key(key, c), Some(step), "{key:?}");
        assert_eq!(super::keys::zoom_key(key, c | M::SHIFT_MASK), Some(step));
        assert_eq!(super::keys::zoom_key(key, c | M::ALT_MASK), None);
        assert_eq!(super::keys::zoom_key(key, M::empty()), None);
    }
    assert_eq!(super::keys::zoom_key(Key::_1, c), None);
}

/// The translate scripts are pasted into the page as they are: pin their
/// bytes so moving them cannot change them.
#[test]
fn pinned_translate_scripts_are_unchanged() {
    fn fnv(s: &str) -> (usize, u64) {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in s.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
        (s.len(), h)
    }
    assert_eq!(
        fnv(super::format::SHOWCASE_PLAIN_LOG),
        (393, 0x667da3e64a4a676e)
    );
    assert_eq!(
        fnv(super::translate::TR_QUOTE_TEXT),
        (453, 0xc2a237f0c7dc2fab)
    );
    assert_eq!(
        fnv(super::translate::TR_RICH_BEGIN),
        (1701, 0xd3cb51e10a1c983c)
    );
    assert_eq!(
        fnv(super::translate::TR_RICH_END),
        (731, 0x866624ea47866e35)
    );
    assert_eq!(
        fnv(super::translate::TR_SOURCE_BEGIN),
        (877, 0x64fe55236df5c79f)
    );
    assert_eq!(
        fnv(super::translate::TR_SOURCE_END),
        (721, 0xdfc354ea9d805901)
    );
    assert_eq!(fnv(super::translate::TR_CANCEL), (197, 0x2cb5c4f7c3471566));
}
