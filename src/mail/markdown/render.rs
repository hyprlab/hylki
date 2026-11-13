//! Markdown source to the HTML part of a message.

use super::emoji::emoji;
use super::linkify::{find_urls, UrlKind};
use crate::encoding::escape_html;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// The dialect. Deliberately without smart punctuation (it would change the
/// characters the user typed), maths (`$` is a currency sign far more often
/// than it opens a formula) and metadata blocks (a leading `---` in a
/// message is a horizontal rule, not front matter).
fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_GFM
        | Options::ENABLE_DEFINITION_LIST
        | Options::ENABLE_SUPERSCRIPT
        | Options::ENABLE_SUBSCRIPT
}

/// Markdown source to the HTML part of a message.
pub(crate) fn to_html(src: &str) -> String {
    let events: Vec<Event> = extend(Parser::new_ext(src, options()).collect());
    let mut html = String::with_capacity(src.len() * 3 / 2);
    pulldown_cmark::html::push_html(&mut html, events.into_iter());
    inline_styles(&html)
}

/// The three extended items pulldown-cmark leaves to the host, applied to
/// the text runs of the stream (never inside code, never inside a link):
/// `==highlight==`, `:emoji:` shortcodes, and bare URLs and email addresses.
///
/// `==` is tracked across events, not within one, so a highlight may wrap
/// other inline markup (`==**shouting**==`) and still close.
fn extend(events: Vec<Event>) -> Vec<Event<'static>> {
    let events = coalesce(events);
    let mut out: Vec<Event<'static>> = Vec::with_capacity(events.len());
    let mut code_depth = 0usize;
    let mut link_depth = 0usize;
    let mut mark_open = false;
    for ev in events {
        match ev {
            Event::Start(Tag::CodeBlock(kind)) => {
                code_depth += 1;
                out.push(Event::Start(Tag::CodeBlock(own_code_kind(kind))));
            }
            Event::End(TagEnd::CodeBlock) => {
                code_depth = code_depth.saturating_sub(1);
                out.push(Event::End(TagEnd::CodeBlock));
            }
            Event::Start(tag @ Tag::Link { .. }) => {
                link_depth += 1;
                out.push(Event::Start(own_tag(tag)));
            }
            Event::End(TagEnd::Link) => {
                link_depth = link_depth.saturating_sub(1);
                out.push(Event::End(TagEnd::Link));
            }
            Event::Text(t) if code_depth == 0 => {
                let text = emoji(&t);
                push_text(&mut out, &text, link_depth == 0, &mut mark_open);
            }
            other => out.push(own(other)),
        }
    }
    // An unbalanced `==` leaves the highlight open; close it rather than
    // hand a stray tag to the recipient's client.
    if mark_open {
        out.push(Event::InlineHtml("</mark>".into()));
    }
    out
}

/// Join neighbouring text events into one run. The parser splits text at
/// characters it *might* have used as a delimiter (`^` and `~` among them),
/// which would otherwise hide a `H~2~O` from the pass below by handing it
/// over in three pieces.
fn coalesce(events: Vec<Event>) -> Vec<Event<'static>> {
    let mut out: Vec<Event<'static>> = Vec::with_capacity(events.len());
    for ev in events {
        match (out.last_mut(), ev) {
            (Some(Event::Text(prev)), Event::Text(t)) => {
                let mut joined = prev.to_string();
                joined.push_str(&t);
                *prev = joined.into();
            }
            (_, ev) => out.push(own(ev)),
        }
    }
    out
}

/// Clone an event into an owned one (the parser borrows the source).
fn own(ev: Event) -> Event<'static> {
    match ev {
        Event::Text(s) => Event::Text(s.into_string().into()),
        Event::Code(s) => Event::Code(s.into_string().into()),
        Event::Html(s) => Event::Html(s.into_string().into()),
        Event::InlineHtml(s) => Event::InlineHtml(s.into_string().into()),
        Event::FootnoteReference(s) => Event::FootnoteReference(s.into_string().into()),
        Event::SoftBreak => Event::SoftBreak,
        Event::HardBreak => Event::HardBreak,
        Event::Rule => Event::Rule,
        Event::TaskListMarker(b) => Event::TaskListMarker(b),
        Event::InlineMath(s) => Event::InlineMath(s.into_string().into()),
        Event::DisplayMath(s) => Event::DisplayMath(s.into_string().into()),
        Event::Start(t) => Event::Start(own_tag(t)),
        Event::End(t) => Event::End(t),
    }
}

fn own_tag(t: Tag) -> Tag<'static> {
    use pulldown_cmark::Tag::*;
    match t {
        Paragraph => Paragraph,
        Heading {
            level,
            id,
            classes,
            attrs,
        } => Heading {
            level,
            id: id.map(|s| s.into_string().into()),
            classes: classes
                .into_iter()
                .map(|s| s.into_string().into())
                .collect(),
            attrs: attrs
                .into_iter()
                .map(|(k, v)| (k.into_string().into(), v.map(|v| v.into_string().into())))
                .collect(),
        },
        BlockQuote(k) => BlockQuote(k),
        CodeBlock(CodeBlockKind::Fenced(s)) => {
            CodeBlock(CodeBlockKind::Fenced(s.into_string().into()))
        }
        CodeBlock(CodeBlockKind::Indented) => CodeBlock(CodeBlockKind::Indented),
        HtmlBlock => HtmlBlock,
        List(n) => List(n),
        Item => Item,
        FootnoteDefinition(s) => FootnoteDefinition(s.into_string().into()),
        DefinitionList => DefinitionList,
        DefinitionListTitle => DefinitionListTitle,
        DefinitionListDefinition => DefinitionListDefinition,
        Table(a) => Table(a),
        TableHead => TableHead,
        TableRow => TableRow,
        TableCell => TableCell,
        Emphasis => Emphasis,
        Strong => Strong,
        Strikethrough => Strikethrough,
        Superscript => Superscript,
        Subscript => Subscript,
        Link {
            link_type,
            dest_url,
            title,
            id,
        } => Link {
            link_type,
            dest_url: dest_url.into_string().into(),
            title: title.into_string().into(),
            id: id.into_string().into(),
        },
        Image {
            link_type,
            dest_url,
            title,
            id,
        } => Image {
            link_type,
            dest_url: dest_url.into_string().into(),
            title: title.into_string().into(),
            id: id.into_string().into(),
        },
        MetadataBlock(k) => MetadataBlock(k),
    }
}

fn own_code_kind(k: CodeBlockKind) -> CodeBlockKind<'static> {
    match k {
        CodeBlockKind::Fenced(s) => CodeBlockKind::Fenced(s.into_string().into()),
        CodeBlockKind::Indented => CodeBlockKind::Indented,
    }
}

/// Split one text run into events, opening and closing `<mark>` on `==` and
/// turning bare URLs and email addresses into links. `linkable` is false
/// inside an existing link, where another `<a>` would nest.
fn push_text(out: &mut Vec<Event<'static>>, text: &str, linkable: bool, mark_open: &mut bool) {
    let mut rest = text;
    while let Some(at) = rest.find("==") {
        let (head, tail) = rest.split_at(at);
        push_scripts(out, head, linkable);
        out.push(Event::InlineHtml(
            if *mark_open {
                "</mark>"
            } else {
                // The colors are named outright: a recipient reading on a
                // dark ground would otherwise get black-on-black from a
                // `background` alone.
                "<mark style=\"background:#fff3a3;color:#000\">"
            }
            .into(),
        ));
        *mark_open = !*mark_open;
        rest = &tail[2..];
    }
    push_scripts(out, rest, linkable);
}

/// `H~2~O` and `X^2^`: the guide's own subscript and superscript examples
/// are inside a word, which is exactly where the parser declines to read
/// them (its delimiters must flank). The flanking cases are already events
/// by the time this runs, so what is left in a text run is the intraword
/// form, matched only when it is short, unbroken and unambiguous — a lone
/// `~` in a path, a `~~strikethrough~~`, or a caret with a space after it
/// all stay the characters they are.
fn push_scripts(out: &mut Vec<Event<'static>>, text: &str, linkable: bool) {
    let mut rest = text;
    while let Some((at, marker, close)) = find_script(rest) {
        push_linkified(out, &rest[..at], linkable);
        let tag = if marker == '~' { "sub" } else { "sup" };
        out.push(Event::InlineHtml(
            format!("<{tag}>{}</{tag}>", escape_html(&rest[at + 1..close])).into(),
        ));
        rest = &rest[close + 1..];
    }
    push_linkified(out, rest, linkable);
}

/// `(marker offset, marker, closing-marker offset)` of the first
/// subscript/superscript run in `text`.
fn find_script(text: &str) -> Option<(usize, char, usize)> {
    const MAX: usize = 32;
    let b = text.as_bytes();
    for (i, c) in text.char_indices() {
        if c != '~' && c != '^' {
            continue;
        }
        if b.get(i + 1) == Some(&(c as u8)) {
            continue; // `~~` is strikethrough
        }
        let after = &text[i + 1..];
        let Some(end) = after.find(c) else { continue };
        let inner = &after[..end];
        if inner.is_empty()
            || inner.len() > MAX
            || inner.chars().any(char::is_whitespace)
            || inner.ends_with(c)
        {
            continue;
        }
        return Some((i, c, i + 1 + end));
    }
    None
}

/// Bare `https://…`, `www.…`, `name@host`, `@name:host` and `@name@host`
/// runs become links; everything else stays text (and is escaped by the HTML
/// writer).
fn push_linkified(out: &mut Vec<Event<'static>>, text: &str, linkable: bool) {
    if text.is_empty() {
        return;
    }
    if !linkable {
        out.push(Event::Text(text.to_string().into()));
        return;
    }
    let mut last = 0usize;
    for (start, end, kind) in find_urls(text) {
        if start > last {
            out.push(Event::Text(text[last..start].to_string().into()));
        }
        let shown = &text[start..end];
        let href = match kind {
            UrlKind::Web => shown.to_string(),
            UrlKind::Bare => format!("https://{shown}"),
            UrlKind::At(href) => href,
        };
        out.push(Event::InlineHtml(
            format!(
                "<a href=\"{}\">{}</a>",
                escape_html(&href),
                escape_html(shown)
            )
            .into(),
        ));
        last = end;
    }
    if last < text.len() {
        out.push(Event::Text(text[last..].to_string().into()));
    }
}

/// The look of what Markdown produces, written onto each element with
/// css-inline, since mail clients drop `<style>` blocks. Fenced code is a
/// block: `pre code` takes back the inline-code treatment.
const SHEET: &str = "\
    code{background:rgba(128,128,128,0.12);padding:1px 4px;border-radius:4px;\
      font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:0.95em}\
    pre{background:rgba(128,128,128,0.12);padding:10px 12px;border-radius:6px;\
      overflow:auto;white-space:pre-wrap;word-wrap:break-word}\
    pre code{background:none;padding:0;border-radius:0}\
    table{border-collapse:collapse;margin:8px 0}\
    th{border:1px solid rgba(128,128,128,0.45);padding:6px 10px;\
      background:rgba(128,128,128,0.12);text-align:left}\
    td{border:1px solid rgba(128,128,128,0.45);padding:6px 10px}\
    blockquote{margin:0 0 0 8px;padding-left:10px;border-left:3px solid rgba(128,128,128,0.4)}\
    hr{border:none;border-top:1px solid rgba(128,128,128,0.4);margin:14px 0}\
    img{max-width:100%;height:auto}\
    dt{font-weight:600;margin-top:6px}\
    dd{margin:0 0 4px 20px}\
    kbd{border:1px solid rgba(128,128,128,0.5);border-radius:4px;padding:0 4px;\
      font-family:ui-monospace,monospace;font-size:0.9em}\
    input[type=checkbox]{margin-right:6px}\
    div.footnote-definition{font-size:0.92em;opacity:0.85}";

fn inline_styles(html: &str) -> String {
    css_inline::inline_fragment(html, SHEET).unwrap_or_else(|_| html.to_string())
}
