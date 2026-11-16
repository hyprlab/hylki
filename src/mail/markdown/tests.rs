use super::dom::plain_text;
use super::outgoing::{pretty_html, sanitize_outgoing};
use super::render::to_html;
use super::writer::{from_html, html_to_text};

/// A preview is the body's words: no head, no styles, and a `<header>`
/// element is content like any other.
#[test]
fn plain_text_keeps_the_words_and_the_breaks() {
    let html = "<html><head><title>T</title><style>p{}</style></head><body>\
                <header>Hello   <b>there</b></header><p>One<br>two &amp; three</p>\
                <p></p><p></p><div>After</div><pre>a  b\n c</pre></body></html>";
    assert_eq!(
        plain_text(html),
        "Hello there\nOne\ntwo & three\n\nAfter\na  b\nc"
    );
}

/// Outlook nests a link in a link; a browser closes the first, leaving
/// it empty. Only the one with text is written.
#[test]
fn nested_links_leave_no_empty_link() {
    let html = r#"<p>Em: <a href="mailto:j@x.example"><a href="mailto:j@x.example">j@x.example</a></a></p>"#;
    assert_eq!(from_html(html), "Em: j@x.example");
    assert_eq!(
        from_html(r#"<p><a href="https://x.example/"></a>text</p>"#),
        "text"
    );
}

/// Assert `html` contains `needle`, with the whole rendering in the
/// failure so a broken case reads itself.
fn has(html: &str, needle: &str) {
    assert!(html.contains(needle), "expected {needle:?} in:\n{html}");
}

// -- markdownguide.org: basic syntax ---------------------------------

#[test]
fn headings() {
    has(&to_html("# One"), "<h1>One</h1>");
    has(&to_html("###### Six"), "<h6>Six</h6>");
    has(&to_html("Setext\n======"), "<h1>Setext</h1>");
    has(&to_html("Setext\n------"), "<h2>Setext</h2>");
}

#[test]
fn emphasis() {
    has(&to_html("**bold**"), "<strong>bold</strong>");
    has(&to_html("__bold__"), "<strong>bold</strong>");
    has(&to_html("*italic*"), "<em>italic</em>");
    has(&to_html("_italic_"), "<em>italic</em>");
    has(&to_html("***both***"), "<em><strong>both</strong></em>");
}

#[test]
fn blockquotes_lists_and_rules() {
    has(&to_html("> quoted"), "<blockquote");
    has(&to_html("> outer\n>> nested"), "<blockquote");
    has(&to_html("- one\n- two"), "<ul>");
    has(&to_html("1. one\n2. two"), "<ol>");
    has(&to_html("---"), "<hr");
    has(&to_html("***"), "<hr");
}

#[test]
fn code_links_and_images() {
    has(&to_html("`inline`"), "<code");
    has(&to_html("    indented block"), "<pre");
    has(
        &to_html("[text](https://example.com)"),
        "href=\"https://example.com\"",
    );
    has(
        &to_html("[text](https://example.com \"Title\")"),
        "title=\"Title\"",
    );
    has(
        &to_html("<https://example.com>"),
        "href=\"https://example.com\"",
    );
    has(&to_html("![alt](cid:pic)"), "<img");
    has(
        &to_html("[ref][1]\n\n[1]: https://example.com"),
        "href=\"https://example.com\"",
    );
}

#[test]
fn hard_break_and_escapes() {
    has(&to_html("one  \ntwo"), "<br>");
    has(&to_html(r"\*not emphasis\*"), "*not emphasis*");
}

// -- markdownguide.org: extended syntax -------------------------------

#[test]
fn tables_with_alignment() {
    let html = to_html("| a | b |\n| :- | ---: |\n| 1 | 2 |");
    has(&html, "<table");
    has(&html, "text-align: right");
    has(&html, "<th");
}

#[test]
fn fenced_code_keeps_its_language() {
    let html = to_html("```rust\nfn main() {}\n```");
    has(&html, "<pre");
    has(&html, "language-rust");
    has(&html, "fn main() {}");
}

#[test]
fn footnotes() {
    let html = to_html("Here[^1].\n\n[^1]: The note.");
    has(&html, "footnote-reference");
    has(&html, "The note.");
}

#[test]
fn heading_ids() {
    has(&to_html("### Heading {#custom-id}"), "id=\"custom-id\"");
}

#[test]
fn definition_lists() {
    let html = to_html("Term\n: The definition");
    has(&html, "<dt");
    has(&html, "<dd");
}

#[test]
fn strikethrough_task_lists_sub_and_sup() {
    has(&to_html("~~gone~~"), "<del>gone</del>");
    has(&to_html("- [x] done\n- [ ] not"), "type=\"checkbox\"");
    has(&to_html("H~2~O"), "<sub>2</sub>");
    has(&to_html("X^2^"), "<sup>2</sup>");
}

#[test]
fn highlight_emoji_and_bare_urls() {
    has(&to_html("==important=="), "<mark");
    has(&to_html("==**both**=="), "<mark");
    has(&to_html("ship it :rocket:"), "\u{1F680}");
    has(&to_html("unknown :nonsense: stays"), ":nonsense:");
    has(
        &to_html("see https://example.com/x today"),
        "href=\"https://example.com/x\"",
    );
    has(
        &to_html("see www.example.com."),
        "href=\"https://www.example.com\"",
    );
    has(
        &to_html("mail ada@example.com now"),
        "href=\"mailto:ada@example.com\"",
    );
}

#[test]
fn handles_become_links_and_read_back_bare() {
    // #653: `@foo@bar.io` linked `@foo` as an email address.
    let html = to_html("mastodon @foo@bar.io, matrix @foo:bar.net");
    has(&html, "href=\"https://bar.io/@foo\"");
    has(&html, "href=\"https://matrix.to/#/@foo:bar.net\"");
    assert!(!html.contains("mailto:"));
    assert_eq!(
        from_html("<p><a href=\"https://bar.io/@foo\">@foo@bar.io</a></p>"),
        "@foo@bar.io"
    );
}

#[test]
fn a_url_keeps_the_sentence_punctuation_out_of_the_link() {
    let html = to_html("Read https://example.com/page, then stop.");
    has(&html, "href=\"https://example.com/page\"");
    has(&html, ", then stop.");
}

#[test]
fn links_after_multibyte_whitespace_and_symbols() {
    // A no-break space right before an address used to slice
    // mid-character (#479).
    has(
        &to_html("mail\u{a0}ada@example.com"),
        "href=\"mailto:ada@example.com\"",
    );
    has(
        &to_html("svp\u{a0}www.example.com"),
        "href=\"https://www.example.com\"",
    );
    // `†` ends in the byte 0xA0, which is not a word break.
    assert!(!to_html("\u{2020}www.example.com").contains("href"));
}

#[test]
fn code_is_left_alone() {
    let html = to_html("```\nhttps://example.com :rocket: ==x==\n```");
    has(&html, "https://example.com :rocket: ==x==");
    assert!(!html.contains("<mark"), "{html}");
}

#[test]
fn styles_ride_on_the_tags() {
    let html = to_html("| a |\n| - |\n| 1 |");
    has(&html, "<table style=");
    has(&html, "border-collapse");
    // The writer's own alignment survives the merge into one attribute
    // — two `style`s on a tag and the second is thrown away.
    let aligned = to_html("| a |\n| ---: |\n| 1 |");
    has(&aligned, "padding: 6px 10px;text-align: right");
    assert!(
        !aligned.contains("text-align: left"),
        "the sheet's left gave way: {aligned}"
    );
    for tag in ["<th ", "<td "] {
        let at = aligned.find(tag).unwrap();
        let open = &aligned[at..at + aligned[at..].find('>').unwrap()];
        assert_eq!(open.matches("style=").count(), 1, "{open}");
    }
}

/// A fence is a block: the `pre` carries the ground, and the `code`
/// inside does not get inline code's own ground and padding as well.
#[test]
fn fenced_code_is_not_inline_code() {
    let html = to_html("```\nlet a = 1;\n```\n\nand `b`");
    let pre = &html[html.find("<pre").unwrap()..html.find("</pre>").unwrap()];
    let inner = &pre[pre.find("<code").unwrap()..];
    assert!(
        pre.starts_with("<pre style=\"background: rgba(128,128,128,0.12)"),
        "{html}"
    );
    assert!(
        inner.contains("background: none") && inner.contains("padding: 0"),
        "{html}"
    );
    let inline = &html[html.rfind("<code").unwrap()..];
    assert!(inline.contains("padding: 1px 4px"), "{html}");
}

#[test]
fn raw_html_passes_through() {
    has(
        &to_html("<span class=\"x\">kept</span>"),
        "<span class=\"x\">kept</span>",
    );
}

// -- HTML back to Markdown -------------------------------------------

#[test]
fn round_trips_basic_syntax() {
    let src = "# Title\n\nSome **bold** and *italic* and `code`.\n\n- one\n- two\n\n> quoted\n\n[link](https://example.com)";
    let back = from_html(&to_html(src));
    has(&back, "# Title");
    has(&back, "**bold**");
    has(&back, "*italic*");
    has(&back, "`code`");
    has(&back, "- one");
    has(&back, "> quoted");
    has(&back, "[link](https://example.com)");
}

#[test]
fn round_trips_extended_syntax() {
    let src = "| a | b |\n| --- | ---: |\n| 1 | 2 |\n\n~~gone~~\n\n- [x] done\n- [ ] not\n\n```rust\nfn f() {}\n```";
    let back = from_html(&to_html(src));
    has(&back, "| a | b |");
    has(&back, "---:");
    has(&back, "~~gone~~");
    has(&back, "- [x] done");
    has(&back, "- [ ] not");
    has(&back, "```rust");
    has(&back, "fn f() {}");
}

#[test]
fn converts_a_composed_reply() {
    // The shape the rich editor hands over for a reply.
    let html = "<div>Yes, agreed.</div><p class=\"hylki-quote-attr\">On Monday, Ada wrote:</p>\
                <blockquote><div>The original line.</div><div>And another.</div></blockquote>";
    let md = from_html(html);
    has(&md, "Yes, agreed.");
    has(&md, "> The original line.");
    has(&md, "> And another.");
}

#[test]
fn keeps_an_inline_image_and_its_name() {
    let md = from_html("<p><img src=\"data:image/png;base64,AAA\" alt=\"shot.png\"></p>");
    has(&md, "![shot.png](data:image/png;base64,AAA)");
}

#[test]
fn escapes_what_would_otherwise_be_markup() {
    let md = from_html("<p>2 * 3 * 4 and [brackets]</p>");
    has(&md, r"2 \* 3 \* 4");
    has(&md, r"\[brackets\]");
    // ...and it comes back as the text it was.
    has(&to_html(&md), "2 * 3 * 4 and [brackets]");
}

#[test]
fn entities_and_breaks() {
    assert_eq!(from_html("<p>a &amp; b &lt;c&gt;</p>"), r"a & b \<c>");
    has(&from_html("<p>one<br>two</p>"), "one  \ntwo");
}

#[test]
fn a_bare_link_reads_as_itself() {
    assert_eq!(
        from_html("<p><a href=\"https://example.com\">https://example.com</a></p>"),
        "https://example.com"
    );
    assert_eq!(
        from_html("<p><a href=\"mailto:ada@example.com\">ada@example.com</a></p>"),
        "ada@example.com"
    );
}

#[test]
fn nested_lists_indent_under_their_marker() {
    let md = from_html("<ul><li>one<ul><li>inner</li></ul></li><li>two</li></ul>");
    has(&md, "- one");
    has(&md, "  - inner");
    has(&md, "- two");
}

#[test]
fn scripts_and_styles_are_not_text() {
    let md = from_html(
        "<p>before</p><script>alert('x')</script><style>p{color:red}</style><p>after</p>",
    );
    assert!(!md.contains("alert"), "{md}");
    assert!(!md.contains("color:red"), "{md}");
    has(&md, "before");
    has(&md, "after");
}

#[test]
fn unclosed_tags_survive() {
    let md = from_html("<p>one<p>two<div>three");
    has(&md, "one");
    has(&md, "two");
    has(&md, "three");
}

// -- Outgoing HTML ----------------------------------------------------

#[test]
fn sanitizer_keeps_mail_html_and_drops_the_rest() {
    let dirty = "<table style=\"border-collapse:collapse\"><tr><td bgcolor=\"#eee\">cell</td></tr></table>\
                 <img src=\"cid:pic\" alt=\"x\"><script>alert(1)</script><p onclick=\"go()\">text</p>\
                 <style>p{color:red}</style><mark>hi</mark>";
    let clean = sanitize_outgoing(dirty);
    has(&clean, "<table");
    has(&clean, "border-collapse");
    has(&clean, "bgcolor=\"#eee\"");
    has(&clean, "cid:pic");
    has(&clean, "<mark>hi</mark>");
    assert!(!clean.contains("script"), "{clean}");
    assert!(!clean.contains("onclick"), "{clean}");
    assert!(!clean.contains("color:red"), "{clean}");
}

#[test]
fn a_task_list_survives_the_sanitizer() {
    let clean = sanitize_outgoing(&to_html("- [x] done"));
    has(&clean, "type=\"checkbox\"");
    has(&clean, "checked");
}

/// The whole outgoing path, as the composer runs it: render, then
/// sanitize. What a recipient would notice going missing has to
/// survive both halves.
#[test]
fn the_send_path_keeps_what_the_recipient_would_notice() {
    let src = "# Update\n\n| Item | Cost |\n| --- | ---: |\n| Chair | 40 |\n\n==Read this== and see https://example.com/x\n\n- [x] ordered\n- [ ] paid\n\n```sh\necho hi\n```";
    let out = sanitize_outgoing(&to_html(src));
    has(&out, "<h1>Update</h1>");
    has(&out, "border-collapse");
    has(&out, "text-align: right");
    has(&out, "background:#fff3a3");
    has(&out, "href=\"https://example.com/x\"");
    has(&out, "type=\"checkbox\"");
    has(&out, "echo hi");
    // And the plain-text alternative is the source itself, which the
    // composer sends unchanged.
    assert!(src.contains("==Read this=="));
}

#[test]
fn pretty_html_breaks_blocks_but_not_code() {
    let out = pretty_html("<p>one</p><p>two</p><pre><code>a\nb</code></pre>");
    assert!(out.lines().count() >= 3, "{out}");
    has(&out, "<pre><code>a\nb</code></pre>");
}

#[test]
fn an_indent_is_not_a_quote() {
    // The composer's Indent (#358) keeps its words, without the "> " a
    // quote gets.
    let html = "<blockquote style=\"margin: 0 0 0 40px; border: none; padding: 0px;\"><p>tail</p></blockquote>\
                <blockquote><p>quoted</p></blockquote>";
    assert_eq!(html_to_text(html), "tail\n\n> quoted");
}

// Characterization tests (restructure, area D): they pin what the code
// does today, quirks included.

const MARK: &str = "<mark style=\"background:#fff3a3;color:#000\">";

#[test]
fn pinned_a_lone_highlight_mark_closes_with_its_paragraph() {
    assert_eq!(
        to_html("a == b\n\nnext para"),
        format!("<p>a {MARK} b</mark></p>\n<p>next para</p>\n")
    );
    assert_eq!(
        to_html("==hi== there"),
        format!("<p>{MARK}hi</mark> there</p>\n")
    );
}

#[test]
fn pinned_an_unclosed_highlight_inverts_the_next_block() {
    assert_eq!(
        to_html("x ==a\n\n- item ==b== c"),
        format!("<p>x {MARK}a</mark></p>\n<ul>\n<li>item b{MARK} c</mark></li>\n</ul>\n")
    );
}

#[test]
fn pinned_links_and_images_are_written_unescaped() {
    assert_eq!(
        from_html("<p><a href=\"http://x.y/a b)\">t</a></p>"),
        "[t](http://x.y/a b))"
    );
    assert_eq!(
        from_html("<p><img alt=\"a]\" src=\"x y.png\"></p>"),
        "![a]](x y.png)"
    );
}

#[test]
fn pinned_block_markers_in_text_are_not_escaped() {
    assert_eq!(
        from_html("<p># not heading</p><p>1. not list</p><p>&gt; quote</p><p>- dash</p>"),
        "# not heading\n\n1. not list\n\n> quote\n\n- dash"
    );
    assert_eq!(
        from_html("<p>:tada: ==x== H~2~O x^2^ a_b_c</p>"),
        ":tada: ==x== H~2~O x^2^ a_b_c"
    );
}

#[test]
fn pinned_inline_markup_characters_are_backslashed() {
    let html = "<p>a*b [c] \\ d `e` &lt;f&gt;</p>";
    assert_eq!(from_html(html), "a\\*b \\[c\\] \\\\ d \\`e\\` \\<f>");
    assert_eq!(html_to_text(html), from_html(html));
}

#[test]
fn pinned_pretty_html_keeps_a_pre_block_whatever_its_case() {
    assert_eq!(
        pretty_html("<div><PRE>a\n<b>b</b></PRE><p>x</p></div>"),
        "<div>\n<PRE>a\n<b>b</b></PRE>\n<p>x</p>\n</div>"
    );
    assert_eq!(
        pretty_html("<div><pre>a\n\n\nb</pre><p>x</p></div>"),
        "<div>\n<pre>a\n\nb</pre>\n<p>x</p>\n</div>"
    );
}

#[test]
fn pinned_plain_text_trims_no_break_spaces() {
    assert_eq!(
        plain_text("<p>&nbsp;a&nbsp;&nbsp;b&nbsp;</p><p>c</p>"),
        "a b\n\nc"
    );
    assert_eq!(
        plain_text("<div>\u{a0}</div><p>x</p><br><br><br><p>y</p>"),
        "x\n\ny"
    );
}
