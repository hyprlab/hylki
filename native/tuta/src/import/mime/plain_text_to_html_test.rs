//! Upstream's converter test (tutanota `src/app-kit/mimimi`,
//! `importable_mail/plain_text_to_html_converter.rs`), unchanged.
use crate::import::mime::plain_text_to_html;

#[test]
pub fn convert_to_html() {
    assert_eq!("Test-Mail im Plain-Text und mit komischen Zeichen: &amp; \"~öä⥣Ի³@<br>weiter gehts in der naechsten Zeile",
                   plain_text_to_html("Test-Mail im Plain-Text und mit komischen Zeichen: & \"~öä⥣Ի³@\r\nweiter gehts in der naechsten Zeile"));

    assert_eq!(
        "<blockquote>simple blockquote</blockquote>",
        plain_text_to_html("> simple blockquote")
    );

    assert_eq!(
        "<blockquote>blockquote <br>with line break</blockquote>",
        plain_text_to_html("> blockquote \r\n> with line break")
    );

    assert_eq!(
        "<blockquote><blockquote>blockquote </blockquote>with line break</blockquote>",
        plain_text_to_html(">> blockquote \r\n> with line break")
    );

    assert_eq!(
        "<blockquote>blockquote <blockquote>with line break</blockquote></blockquote>",
        plain_text_to_html("> blockquote \r\n>> with line break")
    );

    assert_eq!("<blockquote><blockquote><blockquote>blockquote </blockquote></blockquote></blockquote> with line break",
                   plain_text_to_html(">>> blockquote \r\n with line break"));

    // quote without text
    assert_eq!("<blockquote></blockquote>", plain_text_to_html(">"));

    // quote without text but newline
    assert_eq!(
        "<blockquote><br></blockquote>",
        plain_text_to_html(">\r\n>")
    );
}
