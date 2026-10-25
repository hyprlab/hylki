//! The writing direction of an outgoing message (#524).
//!
//! The composer shows each paragraph in the direction of its own text
//! (`dir="auto"` on the body, `unicode-bidi: plaintext` on its blocks), but
//! that is the editor's style sheet and it does not travel: most mail clients
//! lay out a message without a `dir` as left to right, so Hebrew, Arabic or
//! Persian arrived aligned the wrong way. On sending, each block whose text
//! runs the other way from its surroundings is given a `dir`, and a message
//! that starts right to left is wrapped in `<div dir="rtl">`, so the
//! recipient sees what was written.

use markup5ever_rcdom::{Handle, Node, NodeData, RcDom};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Ltr,
    Rtl,
}

impl Dir {
    fn attr(self) -> &'static str {
        match self {
            Dir::Ltr => "ltr",
            Dir::Rtl => "rtl",
        }
    }
}

/// Elements that take the direction of their own lines of text, as the
/// composer's blocks do. Text inside a nested block is that block's own.
const PARAGRAPHS: &[&str] = &[
    "address",
    "article",
    "aside",
    "caption",
    "center",
    "dd",
    "details",
    "div",
    "dt",
    "figcaption",
    "figure",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "li",
    "main",
    "nav",
    "p",
    "pre",
    "section",
    "summary",
    "td",
    "th",
];
/// Elements whose layout follows their whole content, as `dir="auto"`
/// reads it: a quoted English message under a Hebrew reply keeps its own
/// side for the quote bar, and its tables are not mirrored.
const CONTAINERS: &[&str] = &["blockquote", "dl", "menu", "ol", "table", "ul"];
/// Further elements that start a block of their own, so their text is not
/// a line of the element around them.
const OTHER_BLOCKS: &[&str] = &[
    "colgroup", "fieldset", "form", "hr", "tbody", "tfoot", "thead", "tr",
];
/// Elements whose content is not text that is read.
const UNREAD: &[&str] = &[
    "bdi", "math", "script", "style", "svg", "template", "textarea",
];

/// `html` (a body's content) with the direction of its text written into it.
/// A message with no right-to-left letter in it comes back as it was, byte
/// for byte; so does a block that already says its direction.
pub(crate) fn stamp(html: &str) -> String {
    if !html.chars().any(|c| strong(c) == Some(Dir::Rtl)) {
        return html.to_string();
    }
    use html5ever::tendril::TendrilSink;
    let dom = html5ever::parse_document(RcDom::default(), Default::default())
        .one(format!("<body>{html}</body>"));
    let Some(body) = find(&dom.document, "body") else {
        return html.to_string();
    };
    let base = first_strong(&body, false).unwrap_or(Dir::Ltr);
    split_lines(&body, base);
    walk(&body, base);
    let inner = serialize_children(&body);
    match base {
        Dir::Rtl => format!("<div dir=\"rtl\">{inner}</div>"),
        Dir::Ltr => inner,
    }
}

/// The direction a character sets for the line it starts, if it is a
/// strong one: a letter, or one of the marks written to force a direction.
/// Digits, punctuation and spaces take their direction from around them.
fn strong(c: char) -> Option<Dir> {
    match c {
        '\u{200E}' => Some(Dir::Ltr),
        '\u{200F}' | '\u{061C}' => Some(Dir::Rtl),
        c if !c.is_alphabetic() => None,
        // Hebrew, Arabic, Syriac, Thaana, N'Ko, Samaritan, Mandaic and
        // their presentation forms, then the right-to-left blocks of the
        // supplementary planes (historic scripts, Adlam, Mende Kikakui,
        // Arabic mathematical letters).
        '\u{0590}'..='\u{08FF}'
        | '\u{FB1D}'..='\u{FDFF}'
        | '\u{FE70}'..='\u{FEFE}'
        | '\u{10800}'..='\u{10FFF}'
        | '\u{1E800}'..='\u{1EFFF}' => Some(Dir::Rtl),
        _ => Some(Dir::Ltr),
    }
}

fn name_of(node: &Handle) -> Option<String> {
    match &node.data {
        NodeData::Element { name, .. } => Some(name.local.to_string()),
        _ => None,
    }
}

fn dir_attr(node: &Handle) -> Option<String> {
    let NodeData::Element { attrs, .. } = &node.data else {
        return None;
    };
    attrs
        .borrow()
        .iter()
        .find(|a| &*a.name.local == "dir")
        .map(|a| a.value.trim().to_ascii_lowercase())
}

fn is_block(name: &str) -> bool {
    PARAGRAPHS.contains(&name) || CONTAINERS.contains(&name) || OTHER_BLOCKS.contains(&name)
}

/// The direction of the first strong character in `node`'s text. Text with
/// a `dir` of its own is set apart from it and does not count; with
/// `own_lines`, neither does text inside a nested block.
fn first_strong(node: &Handle, own_lines: bool) -> Option<Dir> {
    first_strong_in(&node.children.borrow(), own_lines)
}

fn first_strong_in(nodes: &[Handle], own_lines: bool) -> Option<Dir> {
    nodes.iter().find_map(|child| match &child.data {
        NodeData::Text { contents } => contents.borrow().chars().find_map(strong),
        NodeData::Element { .. } => {
            let name = name_of(child).unwrap_or_default();
            let skipped = UNREAD.contains(&name.as_str())
                || dir_attr(child).is_some()
                || (own_lines && is_block(&name));
            if skipped {
                None
            } else {
                first_strong(child, own_lines)
            }
        }
        _ => None,
    })
}

/// Give each block under `node` the direction of its text where that is not
/// the direction it would have from `inherited`.
fn walk(node: &Handle, inherited: Dir) {
    for child in node.children.borrow().iter() {
        let Some(name) = name_of(child) else {
            continue;
        };
        if UNREAD.contains(&name.as_str()) {
            continue;
        }
        let dir = match dir_attr(child).as_deref() {
            Some("rtl") => Dir::Rtl,
            Some("ltr") => Dir::Ltr,
            Some(_) => first_strong(child, false).unwrap_or(inherited),
            None => {
                let own = if PARAGRAPHS.contains(&name.as_str()) {
                    first_strong(child, true)
                } else if CONTAINERS.contains(&name.as_str()) {
                    first_strong(child, false)
                } else {
                    None
                };
                match own {
                    Some(d) if d != inherited => {
                        set_dir(child, d);
                        d
                    }
                    _ => inherited,
                }
            }
        };
        if dir_attr(child).is_none() && PARAGRAPHS.contains(&name.as_str()) {
            split_lines(child, dir);
        }
        walk(child, dir);
    }
}

/// Put each line of `parent`'s own text that runs against `dir` in a
/// `<div>` with its direction. A line is a run of inline content ended by a
/// `<br>` or a block, since Return makes a `<br>` (#366) and the composer
/// lays out every line by its own text: a `dir` on the block alone would
/// turn an English line under Hebrew right to left. The `<br>` that ended a
/// wrapped line goes, as the `<div>` ends the line itself; text typed
/// straight into the body is handled the same way.
fn split_lines(parent: &Handle, dir: Dir) {
    let kids: Vec<Handle> = parent.children.borrow().clone();
    let mut out: Vec<Handle> = Vec::with_capacity(kids.len());
    let mut line: Vec<Handle> = Vec::new();
    let end =
        |line: &mut Vec<Handle>, br: Option<Handle>, out: &mut Vec<Handle>| match first_strong_in(
            line, true,
        ) {
            Some(d) if d != dir => {
                let holder = new_div();
                set_dir(&holder, d);
                holder.parent.set(Some(std::rc::Rc::downgrade(parent)));
                for n in line.iter() {
                    n.parent.set(Some(std::rc::Rc::downgrade(&holder)));
                }
                holder.children.borrow_mut().append(line);
                out.push(holder);
            }
            _ => {
                out.append(line);
                out.extend(br);
            }
        };
    for kid in kids {
        match name_of(&kid).as_deref() {
            Some("br") => end(&mut line, Some(kid), &mut out),
            Some(n) if is_block(n) => {
                end(&mut line, None, &mut out);
                out.push(kid);
            }
            _ => line.push(kid),
        }
    }
    end(&mut line, None, &mut out);
    *parent.children.borrow_mut() = out;
}

fn new_div() -> Handle {
    Node::new(NodeData::Element {
        name: html5ever::QualName::new(None, html5ever::ns!(html), html5ever::local_name!("div")),
        attrs: std::cell::RefCell::new(Vec::new()),
        template_contents: std::cell::RefCell::new(None),
        mathml_annotation_xml_integration_point: false,
    })
}

fn set_dir(node: &Handle, dir: Dir) {
    if let NodeData::Element { attrs, .. } = &node.data {
        attrs.borrow_mut().push(html5ever::Attribute {
            name: html5ever::QualName::new(None, html5ever::ns!(), html5ever::local_name!("dir")),
            value: dir.attr().into(),
        });
    }
}

fn find(node: &Handle, tag: &str) -> Option<Handle> {
    for child in node.children.borrow().iter() {
        if name_of(child).as_deref() == Some(tag) {
            return Some(child.clone());
        }
        if let Some(found) = find(child, tag) {
            return Some(found);
        }
    }
    None
}

fn serialize_children(node: &Handle) -> String {
    let mut out = Vec::new();
    let handle: markup5ever_rcdom::SerializableHandle = node.clone().into();
    let opts = html5ever::serialize::SerializeOpts {
        traversal_scope: html5ever::serialize::TraversalScope::ChildrenOnly(None),
        ..Default::default()
    };
    let _ = html5ever::serialize(&mut out, &handle, opts);
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_to_right_mail_is_left_alone() {
        let html = "<div>Hello <b>there</b></div><div><br></div><p class=\"x\">Bye</p>";
        assert_eq!(stamp(html), html);
        // Even markup a parser would rewrite: nothing is parsed at all.
        let odd = "<div>unclosed <i>tags";
        assert_eq!(stamp(odd), odd);
    }

    #[test]
    fn a_hebrew_message_is_wrapped_right_to_left() {
        assert_eq!(
            stamp("<div>שלום עולם</div><div>מה נשמע?</div>"),
            "<div dir=\"rtl\"><div>שלום עולם</div><div>מה נשמע?</div></div>"
        );
    }

    #[test]
    fn each_line_keeps_its_own_direction() {
        // Persian under English, the case of #366, and the other way round.
        assert_eq!(
            stamp("<div>Hello</div><div>سلام دوست من</div><div>Bye</div>"),
            "<div>Hello</div><div dir=\"rtl\">سلام دوست من</div><div>Bye</div>"
        );
        assert_eq!(
            stamp("<div>שלום</div><div>Hello</div>"),
            "<div dir=\"rtl\"><div>שלום</div><div dir=\"ltr\">Hello</div></div>"
        );
    }

    #[test]
    fn digits_and_punctuation_do_not_decide() {
        assert_eq!(
            stamp("<div>12:30, «שלום»</div>"),
            "<div dir=\"rtl\"><div>12:30, «שלום»</div></div>"
        );
        // A line with no letter at all goes with the message.
        assert_eq!(
            stamp("<div>Hi</div><div>2024</div><div>שלום</div>"),
            "<div>Hi</div><div>2024</div><div dir=\"rtl\">שלום</div>"
        );
    }

    #[test]
    fn a_block_that_says_its_direction_keeps_it() {
        assert_eq!(
            stamp("<div dir=\"ltr\">שלום</div><div>Hi</div>"),
            "<div dir=\"ltr\">שלום</div><div>Hi</div>"
        );
        // Its text does not decide the message's direction either.
        assert_eq!(
            stamp("<div><span dir=\"ltr\">Hi</span> שלום</div>"),
            "<div dir=\"rtl\"><div><span dir=\"ltr\">Hi</span> שלום</div></div>"
        );
    }

    #[test]
    fn the_font_wrapper_does_not_hide_the_lines() {
        // The compose font goes out as a <div style> around what was typed.
        assert_eq!(
            stamp("<div style=\"font: 14px Cantarell\"><div>Hi</div><div>مرحبا</div></div>"),
            "<div style=\"font: 14px Cantarell\"><div>Hi</div><div dir=\"rtl\">مرحبا</div></div>"
        );
    }

    #[test]
    fn a_quote_keeps_the_direction_of_its_text() {
        let reply = "<div>תודה!</div><p class=\"hylki-quote-attr\">Ann wrote:</p>\
            <blockquote><div>Thanks</div><table><tr><td>A</td><td>B</td></tr></table></blockquote>";
        assert_eq!(
            stamp(reply),
            "<div dir=\"rtl\"><div>תודה!</div><p class=\"hylki-quote-attr\" dir=\"ltr\">Ann wrote:</p>\
             <blockquote dir=\"ltr\"><div>Thanks</div><table><tbody><tr><td>A</td><td>B</td></tr></tbody></table></blockquote></div>"
        );
    }

    #[test]
    fn list_items_go_their_own_way() {
        assert_eq!(
            stamp("<ul><li>One</li><li>שתיים</li></ul>"),
            "<ul><li>One</li><li dir=\"rtl\">שתיים</li></ul>"
        );
        assert_eq!(
            stamp("<div>Notes:</div><ol><li>אחת</li><li>שתיים</li></ol>"),
            "<div>Notes:</div><ol dir=\"rtl\"><li>אחת</li><li>שתיים</li></ol>"
        );
    }

    #[test]
    fn text_typed_straight_into_the_body() {
        // The first line of a new message sits in the body itself until
        // Return is pressed.
        assert_eq!(
            stamp("שלום<div>עולם</div>"),
            "<div dir=\"rtl\">שלום<div>עולם</div></div>"
        );
        assert_eq!(
            stamp("Hello<br><div>x</div>שלום <b>רב</b><div>y</div>"),
            "Hello<br><div>x</div><div dir=\"rtl\">שלום <b>רב</b></div><div>y</div>"
        );
    }

    #[test]
    fn lines_broken_by_return_go_their_own_way() {
        // What #524's first test sent: three lines with a <br> after each.
        assert_eq!(
            stamp("שלום, זו בדיקה.<br>مرحبا، هذا اختبار.<br>This line is English.<br><br>"),
            "<div dir=\"rtl\">שלום, זו בדיקה.<br>مرحبا، هذا اختبار.<br>\
             <div dir=\"ltr\">This line is English.</div><br></div>"
        );
        assert_eq!(
            stamp("<div>Hi<br><b>שלום</b> רב<br>Bye</div>"),
            "<div>Hi<br><div dir=\"rtl\"><b>שלום</b> רב</div>Bye</div>"
        );
        // A blank line, or one of digits, is left as it was.
        assert_eq!(
            stamp("<p>שלום<br><br>2024<br>עולם</p>"),
            "<div dir=\"rtl\"><p>שלום<br><br>2024<br>עולם</p></div>"
        );
    }

    #[test]
    fn a_line_starting_in_the_other_script_follows_its_first_letter() {
        assert_eq!(
            stamp("<div>Hylki הוא דוא\"ל</div>"),
            "<div>Hylki הוא דוא\"ל</div>"
        );
        // A right-to-left mark says otherwise.
        assert_eq!(
            stamp("<div>\u{200F}Hylki הוא</div>"),
            "<div dir=\"rtl\"><div>\u{200F}Hylki הוא</div></div>"
        );
    }

    #[test]
    fn styles_and_scripts_are_not_text() {
        assert_eq!(
            stamp("<style>/* שלום */</style><div>Hi</div>"),
            "<style>/* שלום */</style><div>Hi</div>"
        );
    }
}
