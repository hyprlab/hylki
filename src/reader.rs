//! Reader view: a message stripped to what it says.
//!
//! The reader pane's Reader View toggle shows every message in the open
//! conversation without the sender's design: no stylesheets, no layout
//! tables, no colours, no hidden preview text, no tracking pixels. What is
//! left — headings, paragraphs, lists, quotes, links, real images, data
//! tables — is set in one uniform sheet that follows the app's theme.
//!
//! The work is done on a parsed DOM, not on the string: the sender's markup
//! is walked once and a fresh, minimal tree is built from it, so nothing of
//! the original attributes, styles or structure survives except what this
//! module chooses to emit. The result is still shown in the same sandboxed
//! frame as any other message (no scripts, the CSP, the remote-content
//! policy), so the reader view narrows what is shown; it never widens what
//! is allowed.

use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};

/// The reader document for one message body: the extracted content in a
/// full HTML document carrying the reader sheet. `body` is the message as
/// cached (HTML, or the plain-text wrapper the worker builds).
pub fn render(body: &str, dark: bool, accent: &str) -> String {
    format!(
        // The content sits directly in the body, no wrapper: the wrapper
        // script's quote fold looks for a quote's topmost ancestor *under
        // the body*, and for content before it; a single wrapper would be
        // that ancestor, with nothing before it, and no quote would fold.
        "<!doctype html><html><head><meta charset=\"utf-8\"><style>{css}</style></head>\
         <body>{content}</body></html>",
        css = stylesheet(dark, accent),
        content = extract(body),
    )
}

/// The message's content as a clean HTML fragment (see the module notes).
pub fn extract(body: &str) -> String {
    // A body with no markup at all is plain text, cached as it came: shown
    // line for line, as the ordinary view shows it (see `body_html`).
    if !body.contains('<') {
        return format!("<div class=\"vireo-plain\">{}</div>", escape_text(body.trim_matches('\n')));
    }
    let dom = html5ever::parse_document(RcDom::default(), Default::default()).one(body);
    let mut out = Vec::new();
    walk(&dom.document, &mut out, false, false);
    let out = tidy(out, true);
    let mut html = String::with_capacity(body.len() / 2);
    for node in &out {
        node.write(&mut html);
    }
    html
}

/// The reader's own sheet: one measure, one type scale, theme colours.
fn stylesheet(dark: bool, accent: &str) -> String {
    let (scheme, fg, muted) = if dark {
        ("dark", "#e6e6e6", "#b0b0b0")
    } else {
        ("light", "#1a1a1a", "#5c5c5c")
    };
    const RULE: &str = "rgba(128,128,128,0.35)";
    const SOFT: &str = "rgba(128,128,128,0.12)";
    format!(
        ":root{{color-scheme:{scheme};}}\
         html{{margin:0;padding:0;background:transparent;}}\
         body{{color:{fg};font:15px/1.6 system-ui,\"Adwaita Sans\",Cantarell,sans-serif;\
           overflow-wrap:anywhere;-webkit-font-smoothing:antialiased;background:transparent;\
           max-width:44em;margin:0 auto;padding:22px 26px 26px;box-sizing:border-box;}}\
         p,ul,ol,dl,blockquote,pre,table,figure,.vireo-plain{{margin:0 0 1em;}}\
         body>:last-child{{margin-bottom:0;}}\
         h1,h2,h3,h4,h5,h6{{margin:1.4em 0 0.5em;line-height:1.25;font-weight:700;}}\
         h1{{font-size:1.5em;}}h2{{font-size:1.3em;}}h3{{font-size:1.15em;}}\
         h4,h5,h6{{font-size:1em;}}\
         body>:first-child{{margin-top:0;}}\
         a{{color:{accent};text-decoration:none;}}a:hover{{text-decoration:underline;}}\
         img{{display:block;max-width:100%;height:auto;margin:1em auto;border-radius:8px;}}\
         a img{{margin:0.5em 0;}}\
         blockquote{{padding:0.1em 0 0.1em 1em;border-left:3px solid {RULE};color:{muted};}}\
         blockquote blockquote{{margin:0.5em 0;}}\
         pre,code,kbd,samp{{font-family:ui-monospace,\"Adwaita Mono\",monospace;font-size:0.9em;}}\
         pre{{background:{SOFT};padding:12px 14px;border-radius:8px;overflow-x:auto;\
           white-space:pre-wrap;}}\
         code,kbd,samp{{background:{SOFT};padding:0.1em 0.3em;border-radius:4px;}}\
         pre code{{background:none;padding:0;}}\
         hr{{border:0;border-top:1px solid {RULE};margin:1.6em 0;}}\
         ul,ol{{padding-left:1.5em;}}li{{margin:0.25em 0;}}\
         dt{{font-weight:700;}}dd{{margin:0 0 0.5em 1.5em;}}\
         table{{border-collapse:collapse;max-width:100%;font-size:0.95em;display:block;\
           overflow-x:auto;}}\
         th,td{{padding:6px 10px;border-bottom:1px solid {RULE};text-align:left;\
           vertical-align:top;}}\
         th{{font-weight:700;}}\
         figcaption,small{{font-size:0.85em;color:{muted};}}\
         .vireo-plain{{white-space:pre-wrap;}}\
         mark{{background:rgba(229,165,10,0.35);color:inherit;}}"
    )
}

// ---------------------------------------------------------------------------
// The output tree
// ---------------------------------------------------------------------------

/// A node of the rebuilt document. Only what this module emits can appear.
enum Out {
    Text(String),
    Elem(Elem),
}

struct Elem {
    tag: &'static str,
    attrs: Vec<(&'static str, String)>,
    kids: Vec<Out>,
}

impl Elem {
    fn new(tag: &'static str) -> Elem {
        Elem { tag, attrs: Vec::new(), kids: Vec::new() }
    }

    fn is_block(&self) -> bool {
        matches!(
            self.tag,
            "p" | "div"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "ul"
                | "ol"
                | "li"
                | "dl"
                | "dt"
                | "dd"
                | "blockquote"
                | "pre"
                | "hr"
                | "table"
                | "tr"
                | "td"
                | "th"
                | "figure"
                | "figcaption"
        )
    }

    /// Whitespace inside is meaningful (shown as written).
    fn keeps_space(&self) -> bool {
        self.tag == "pre" || self.attrs.iter().any(|(k, v)| *k == "class" && v == "vireo-plain")
    }
}

impl Out {
    fn write(&self, into: &mut String) {
        match self {
            Out::Text(t) => into.push_str(&escape_text(t)),
            Out::Elem(e) => {
                into.push('<');
                into.push_str(e.tag);
                for (k, v) in &e.attrs {
                    into.push(' ');
                    into.push_str(k);
                    into.push_str("=\"");
                    into.push_str(&escape_attr(v));
                    into.push('"');
                }
                into.push('>');
                if matches!(e.tag, "br" | "hr" | "img") {
                    return;
                }
                for k in &e.kids {
                    k.write(into);
                }
                into.push_str("</");
                into.push_str(e.tag);
                into.push('>');
            }
        }
    }

    /// Anything a reader would see: text that is not blank, or a picture /
    /// rule. Line breaks on their own are not content.
    fn has_content(&self) -> bool {
        match self {
            Out::Text(t) => !is_blank(t),
            Out::Elem(e) => match e.tag {
                "img" | "hr" => true,
                "br" => false,
                _ => e.kids.iter().any(Out::has_content),
            },
        }
    }

    fn is_block(&self) -> bool {
        matches!(self, Out::Elem(e) if e.is_block())
    }

    fn is_br(&self) -> bool {
        matches!(self, Out::Elem(e) if e.tag == "br")
    }

    fn is_blank_text(&self) -> bool {
        matches!(self, Out::Text(t) if is_blank(t))
    }
}

/// Whitespace, no-break spaces and the invisible characters preview text
/// is padded with count as nothing.
fn is_blank(t: &str) -> bool {
    t.chars().all(|c| {
        c.is_whitespace() || matches!(c, '\u{a0}' | '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{feff}' | '\u{ad}' | '\u{2007}' | '\u{202f}')
    })
}

fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;")
}

// ---------------------------------------------------------------------------
// The walk: sender's DOM → output tree
// ---------------------------------------------------------------------------

/// The class and id markers kept on an element: what the wrapper's quote
/// fold and the plain-text sheet look for. Nothing else of the sender's
/// classes survives.
const KEPT_CLASSES: [&str; 5] =
    ["gmail_quote", "gmail_signature", "moz-signature", "vireo-quote-attr", "vireo-plain"];
const KEPT_IDS: [&str; 2] = ["divRplyFwdMsg", "Signature"];

fn attr<'a>(attrs: &'a [html5ever::Attribute], name: &str) -> Option<&'a str> {
    attrs.iter().find(|a| &*a.name.local == name).map(|a| &*a.value)
}

/// Emit `node` and what is under it into `out`. `pre` keeps whitespace as
/// written; `in_link` drops nested links (they are invalid, and a table of
/// links inside a link is a common newsletter mistake).
fn walk(node: &Handle, out: &mut Vec<Out>, pre: bool, in_link: bool) {
    match &node.data {
        NodeData::Document => {
            for c in node.children.borrow().iter() {
                walk(c, out, pre, in_link);
            }
        }
        NodeData::Text { contents } => {
            let text = contents.borrow();
            if pre {
                out.push(Out::Text(text.to_string()));
            } else {
                out.push(Out::Text(collapse_space(&text)));
            }
        }
        NodeData::Element { name, attrs, .. } => {
            let tag: String = (*name.local).to_ascii_lowercase();
            let attrs = attrs.borrow();
            if is_hidden(&attrs) {
                return;
            }
            let children = |out: &mut Vec<Out>, pre: bool, in_link: bool| {
                for c in node.children.borrow().iter() {
                    walk(c, out, pre, in_link);
                }
            };
            let push_elem = |out: &mut Vec<Out>, mut e: Elem, pre: bool, in_link: bool| {
                mark(&mut e, &attrs);
                children(&mut e.kids, pre, in_link);
                out.push(Out::Elem(e));
            };
            match tag.as_str() {
                // Dropped with everything inside: code, styling, chrome and
                // the parts of a page a message has no business carrying.
                "script" | "style" | "head" | "title" | "meta" | "link" | "template"
                | "noscript" | "iframe" | "frame" | "frameset" | "object" | "embed"
                | "applet" | "svg" | "math" | "canvas" | "form" | "input" | "button"
                | "select" | "textarea" | "option" | "datalist" | "map" | "area" | "base"
                | "audio" | "video" | "source" | "track" | "picture" | "o:p" | "xml" => {}
                "br" => out.push(Out::Elem(Elem::new("br"))),
                "hr" => out.push(Out::Elem(Elem::new("hr"))),
                "img" => {
                    if let Some(img) = image(&attrs) {
                        out.push(Out::Elem(img));
                    }
                }
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    let tag: &'static str = match tag.as_str() {
                        "h1" => "h1",
                        "h2" => "h2",
                        "h3" => "h3",
                        "h4" => "h4",
                        "h5" => "h5",
                        _ => "h6",
                    };
                    push_elem(out, Elem::new(tag), pre, in_link);
                }
                "p" => push_elem(out, Elem::new("p"), pre, in_link),
                "ul" | "menu" => push_elem(out, Elem::new("ul"), pre, in_link),
                "ol" => push_elem(out, Elem::new("ol"), pre, in_link),
                "li" => push_elem(out, Elem::new("li"), pre, in_link),
                "dl" => push_elem(out, Elem::new("dl"), pre, in_link),
                "dt" => push_elem(out, Elem::new("dt"), pre, in_link),
                "dd" => push_elem(out, Elem::new("dd"), pre, in_link),
                "blockquote" => {
                    let mut e = Elem::new("blockquote");
                    if attr(&attrs, "type").is_some_and(|t| t.eq_ignore_ascii_case("cite")) {
                        e.attrs.push(("type", "cite".into()));
                    }
                    push_elem(out, e, pre, in_link);
                }
                "pre" | "listing" | "xmp" | "plaintext" => {
                    push_elem(out, Elem::new("pre"), true, in_link)
                }
                "figure" => push_elem(out, Elem::new("figure"), pre, in_link),
                "figcaption" | "caption" => {
                    push_elem(out, Elem::new("figcaption"), pre, in_link)
                }
                "table" => {
                    if is_data_table(node) {
                        push_elem(out, Elem::new("table"), pre, in_link);
                    } else {
                        // A layout table: its cells become plain blocks, in
                        // reading order, and the grid is gone.
                        push_elem(out, Elem::new("div"), pre, in_link);
                    }
                }
                "tr" => {
                    if in_data_table(node) {
                        push_elem(out, Elem::new("tr"), pre, in_link);
                    } else {
                        children(out, pre, in_link);
                    }
                }
                "td" | "th" => {
                    if in_data_table(node) {
                        let tag: &'static str = if tag == "th" { "th" } else { "td" };
                        push_elem(out, Elem::new(tag), pre, in_link);
                    } else {
                        push_elem(out, Elem::new("div"), pre, in_link);
                    }
                }
                "thead" | "tbody" | "tfoot" | "colgroup" | "col" => {
                    if tag == "colgroup" || tag == "col" {
                        return;
                    }
                    children(out, pre, in_link);
                }
                "div" | "section" | "article" | "main" | "header" | "footer" | "aside"
                | "nav" | "center" | "address" | "details" | "summary" | "fieldset"
                | "legend" | "body" | "html" => {
                    // A plain-text part keeps its line structure (#181): the
                    // worker wraps it in `.vireo-plain`, and so does this.
                    // (`body_html` puts the class on `<body>` itself for a
                    // plain-text message; that body becomes the div.)
                    let plain = has_class(&attrs, "vireo-plain");
                    if (tag == "body" || tag == "html") && !plain {
                        children(out, pre, in_link);
                    } else {
                        push_elem(out, Elem::new("div"), pre || plain, in_link);
                    }
                }
                "a" => {
                    match (in_link, link_href(&attrs)) {
                        (false, Some(href)) => {
                            let mut e = Elem::new("a");
                            e.attrs.push(("href", href));
                            if let Some(t) = attr(&attrs, "title").filter(|t| !t.trim().is_empty()) {
                                e.attrs.push(("title", t.trim().to_string()));
                            }
                            push_elem(out, e, pre, true);
                        }
                        _ => children(out, pre, in_link),
                    }
                }
                "strong" | "b" => push_elem(out, Elem::new("strong"), pre, in_link),
                "em" | "i" | "cite" | "var" | "dfn" => push_elem(out, Elem::new("em"), pre, in_link),
                "u" | "ins" => push_elem(out, Elem::new("u"), pre, in_link),
                "s" | "strike" | "del" => push_elem(out, Elem::new("s"), pre, in_link),
                "sub" => push_elem(out, Elem::new("sub"), pre, in_link),
                "sup" => push_elem(out, Elem::new("sup"), pre, in_link),
                "code" | "kbd" | "samp" | "tt" => push_elem(out, Elem::new("code"), pre, in_link),
                "mark" => push_elem(out, Elem::new("mark"), pre, in_link),
                "small" => push_elem(out, Elem::new("small"), pre, in_link),
                "q" => push_elem(out, Elem::new("q"), pre, in_link),
                "abbr" => {
                    let mut e = Elem::new("abbr");
                    if let Some(t) = attr(&attrs, "title").filter(|t| !t.trim().is_empty()) {
                        e.attrs.push(("title", t.trim().to_string()));
                    }
                    push_elem(out, e, pre, in_link);
                }
                // Everything else — span, font, label, o:p, custom tags —
                // is a container the reader sees through.
                _ => children(out, pre, in_link),
            }
        }
        NodeData::Doctype { .. } | NodeData::Comment { .. } | NodeData::ProcessingInstruction { .. } => {}
    }
}

/// Carry the quote/signature/plain markers over, and nothing else.
fn mark(e: &mut Elem, attrs: &[html5ever::Attribute]) {
    if let Some(class) = attr(attrs, "class") {
        let kept: Vec<&str> = class
            .split_ascii_whitespace()
            .filter(|c| KEPT_CLASSES.contains(c))
            .collect();
        if !kept.is_empty() {
            e.attrs.push(("class", kept.join(" ")));
        }
    }
    if let Some(id) = attr(attrs, "id") {
        if KEPT_IDS.contains(&id.trim()) {
            e.attrs.push(("id", id.trim().to_string()));
        }
    }
}

fn has_class(attrs: &[html5ever::Attribute], class: &str) -> bool {
    attr(attrs, "class").is_some_and(|c| c.split_ascii_whitespace().any(|x| x == class))
}

/// Runs of whitespace become one space, as a browser would show them.
fn collapse_space(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
            if !space {
                out.push(' ');
                space = true;
            }
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

/// The `style` attribute's declarations, lowercased, as (property, value)
/// with `!important` and surrounding space removed.
fn declarations(style: &str) -> Vec<(String, String)> {
    style
        .split(';')
        .filter_map(|d| {
            let (k, v) = d.split_once(':')?;
            let k = k.trim().to_ascii_lowercase();
            let v = v.to_ascii_lowercase().replace("!important", "");
            Some((k, v.trim().to_string()))
        })
        .collect()
}

/// A CSS length that amounts to nothing: `0`, `0px`, `0.0em`…
fn is_zero_length(v: &str) -> bool {
    let n: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    !n.is_empty() && n.parse::<f64>().is_ok_and(|n| n == 0.0)
}

/// A length in pixels (a bare number counts as pixels, as HTML attributes
/// do); `None` for anything else (percentages, em…).
fn px(v: &str) -> Option<f64> {
    let v = v.trim();
    let n: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let rest = v[n.len()..].trim();
    if n.is_empty() || !(rest.is_empty() || rest.eq_ignore_ascii_case("px")) {
        return None;
    }
    n.parse().ok()
}

/// Whether the sender hid this element: preview text a client shows in the
/// list but never in the message, Outlook-only blocks, collapsed spacers.
fn is_hidden(attrs: &[html5ever::Attribute]) -> bool {
    if attr(attrs, "hidden").is_some() {
        return true;
    }
    let Some(style) = attr(attrs, "style") else { return false };
    let decls = declarations(style);
    let get = |k: &str| decls.iter().rev().find(|(p, _)| p == k).map(|(_, v)| v.as_str());
    if get("display").is_some_and(|v| v == "none")
        || get("visibility").is_some_and(|v| v == "hidden")
        || get("mso-hide").is_some_and(|v| v == "all")
        || get("opacity").is_some_and(|v| v.parse::<f64>().is_ok_and(|n| n == 0.0))
    {
        return true;
    }
    // A zero-size box that clips its content is a preheader by another
    // name. Only with the clipping: a zero font size on its own is how
    // MJML and its like build every column and cell (their children set
    // their own), so it says nothing about what is shown.
    let clipped = get("overflow").is_some_and(|v| v == "hidden");
    clipped
        && (get("max-height").is_some_and(is_zero_length)
            || get("height").is_some_and(is_zero_length)
            || get("width").is_some_and(is_zero_length))
}

/// A picture worth showing: a real source, and bigger than a tracking pixel.
fn image(attrs: &[html5ever::Attribute]) -> Option<Elem> {
    let src = attr(attrs, "src")?.trim();
    let lower = src.to_ascii_lowercase();
    let allowed = lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("cid:")
        || lower.starts_with("data:image/");
    if !allowed {
        return None;
    }
    let mut width = attr(attrs, "width").and_then(px);
    let mut height = attr(attrs, "height").and_then(px);
    if let Some(style) = attr(attrs, "style") {
        for (k, v) in declarations(style) {
            match k.as_str() {
                "width" => width = px(&v).or(width),
                "height" => height = px(&v).or(height),
                _ => {}
            }
        }
    }
    if width.is_some_and(|w| w <= 2.0) || height.is_some_and(|h| h <= 2.0) {
        return None;
    }
    let mut e = Elem::new("img");
    e.attrs.push(("src", src.to_string()));
    if let Some(alt) = attr(attrs, "alt").map(str::trim).filter(|a| !a.is_empty()) {
        e.attrs.push(("alt", alt.to_string()));
    }
    Some(e)
}

/// The link's target, or `None` when it is not one the reader will follow
/// (no target at all, or a scheme that means code rather than a place).
fn link_href(attrs: &[html5ever::Attribute]) -> Option<String> {
    let href = attr(attrs, "href")?.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    let scheme: String = href
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        .collect::<String>()
        .to_ascii_lowercase();
    let has_scheme = href[scheme.len()..].starts_with(':');
    if !has_scheme {
        // Relative to nothing: a message has no page to resolve it against.
        return None;
    }
    if matches!(scheme.as_str(), "javascript" | "vbscript" | "data" | "file" | "blob") {
        return None;
    }
    Some(href.to_string())
}

fn element_name(node: &Handle) -> Option<String> {
    match &node.data {
        NodeData::Element { name, .. } => Some((*name.local).to_ascii_lowercase()),
        _ => None,
    }
}

/// Whether a `tr`/`td`/`th` sits in a table the reader keeps as a grid.
fn in_data_table(node: &Handle) -> bool {
    let mut cur = node.parent.take();
    node.parent.set(cur.clone());
    while let Some(weak) = cur {
        let Some(p) = weak.upgrade() else { return false };
        if element_name(&p).as_deref() == Some("table") {
            return is_data_table(&p);
        }
        cur = p.parent.take();
        p.parent.set(cur.clone());
    }
    false
}

/// A table that holds data rather than a layout: headed cells, or a real
/// grid of short cells with nothing block-like inside them. A table with
/// one column, one row, or with paragraphs, pictures or other tables in its
/// cells is a layout, and its cells are read one after another.
fn is_data_table(table: &Handle) -> bool {
    let mut rows = 0usize;
    let mut widest = 0usize;
    let mut headed = false;
    let mut blocky = false;
    fn scan(
        node: &Handle,
        rows: &mut usize,
        widest: &mut usize,
        headed: &mut bool,
        blocky: &mut bool,
    ) {
        for c in node.children.borrow().iter() {
            let Some(name) = element_name(c) else { continue };
            match name.as_str() {
                "tr" => {
                    *rows += 1;
                    let mut cells = 0;
                    for cell in c.children.borrow().iter() {
                        match element_name(cell).as_deref() {
                            Some("td") | Some("th") => {
                                cells += 1;
                                if element_name(cell).as_deref() == Some("th") {
                                    *headed = true;
                                }
                                if has_block_inside(cell) {
                                    *blocky = true;
                                }
                            }
                            _ => {}
                        }
                    }
                    *widest = (*widest).max(cells);
                }
                "thead" | "tbody" | "tfoot" => scan(c, rows, widest, headed, blocky),
                _ => {}
            }
        }
    }
    scan(table, &mut rows, &mut widest, &mut headed, &mut blocky);
    if blocky {
        return false;
    }
    headed || (rows >= 2 && widest >= 2)
}

fn has_block_inside(node: &Handle) -> bool {
    for c in node.children.borrow().iter() {
        if let Some(name) = element_name(c) {
            if matches!(
                name.as_str(),
                "table" | "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "ul" | "ol"
                    | "img" | "blockquote" | "pre" | "hr" | "center"
            ) {
                return true;
            }
            if has_block_inside(c) {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Tidying: the output tree → paragraphs
// ---------------------------------------------------------------------------

/// Turn the raw walk into something that reads: empty boxes go, runs of
/// inline content become paragraphs, a double line break is a paragraph
/// break, and a box holding a single box is that box.
fn tidy(kids: Vec<Out>, root: bool) -> Vec<Out> {
    let mut out = Vec::with_capacity(kids.len());
    for k in kids {
        match k {
            Out::Text(t) => out.push(Out::Text(t)),
            Out::Elem(mut e) => {
                if matches!(e.tag, "br" | "hr" | "img") {
                    out.push(Out::Elem(e));
                    continue;
                }
                if e.keeps_space() {
                    // Shown as written; only an entirely blank one goes.
                    let node = Out::Elem(e);
                    if node.has_content() {
                        out.push(node);
                    }
                    continue;
                }
                e.kids = tidy(std::mem::take(&mut e.kids), false);
                if matches!(e.tag, "div" | "blockquote" | "li" | "td" | "th" | "dd" | "figure") {
                    e.kids = paragraphs(std::mem::take(&mut e.kids), e.tag != "div");
                }
                // A heading (or paragraph) holds a line, not boxes: blocks a
                // sender nested in one are opened up, a line break between.
                if matches!(e.tag, "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p")
                    && e.kids.iter().any(Out::is_block)
                {
                    e.kids = trim_run(flatten_blocks(std::mem::take(&mut e.kids)));
                }
                let node = Out::Elem(e);
                if !node.has_content() {
                    continue;
                }
                match node {
                    // A box that only holds boxes is structure, not content:
                    // its boxes stand on their own. (Only a marked one — a
                    // quote, a signature — keeps its wrapper.)
                    Out::Elem(e) if e.tag == "div" && e.attrs.is_empty() => out.extend(e.kids),
                    // Inline formatting around blocks (a link around a whole
                    // card) is turned inside out: the formatting goes into
                    // each block, and the blocks stand on their own.
                    Out::Elem(e) if !e.is_block() && e.kids.iter().any(Out::is_block) => {
                        let shell = Elem { tag: e.tag, attrs: e.attrs.clone(), kids: Vec::new() };
                        out.extend(wrap_inline(&shell, e.kids));
                    }
                    n => out.push(n),
                }
            }
        }
    }
    let out = merge_text(out);
    if root {
        paragraphs(out, false)
    } else {
        out
    }
}

/// Every block among `kids` replaced by its content, with a line break
/// where one block met the next.
fn flatten_blocks(kids: Vec<Out>) -> Vec<Out> {
    let mut out = Vec::new();
    for k in kids {
        match k {
            Out::Elem(e) if e.is_block() => {
                if e.tag == "hr" {
                    continue;
                }
                if out.iter().any(Out::has_content) {
                    out.push(Out::Elem(Elem::new("br")));
                }
                out.extend(flatten_blocks(e.kids));
            }
            k => out.push(k),
        }
    }
    out
}

/// `kids` wrapped in a copy of `shell` (an inline element) — block by block
/// where they hold blocks, so no block ever sits inside an inline element.
fn wrap_inline(shell: &Elem, kids: Vec<Out>) -> Vec<Out> {
    let clone = |kids: Vec<Out>| Out::Elem(Elem { tag: shell.tag, attrs: shell.attrs.clone(), kids });
    if !kids.iter().any(Out::is_block) {
        return vec![clone(kids)];
    }
    let mut out = Vec::new();
    let mut run = Vec::new();
    for k in kids {
        match k {
            Out::Elem(mut e) if e.is_block() => {
                if run.iter().any(Out::has_content) {
                    out.push(clone(std::mem::take(&mut run)));
                } else {
                    run.clear();
                }
                if e.tag != "hr" {
                    e.kids = wrap_inline(shell, std::mem::take(&mut e.kids));
                }
                out.push(Out::Elem(e));
            }
            k => run.push(k),
        }
    }
    if run.iter().any(Out::has_content) {
        out.push(clone(run));
    }
    out
}

/// Adjacent text runs joined, so later passes see one string.
fn merge_text(kids: Vec<Out>) -> Vec<Out> {
    let mut out: Vec<Out> = Vec::with_capacity(kids.len());
    for k in kids {
        match (out.last_mut(), k) {
            (Some(Out::Text(prev)), Out::Text(t)) => prev.push_str(&t),
            (_, k) => out.push(k),
        }
    }
    out
}

/// Wrap runs of inline content between blocks into paragraphs, splitting a
/// run at a double line break. With `keep_lone`, a list of one inline run
/// stays unwrapped (an `li` or a cell reads better without a `p` inside).
fn paragraphs(kids: Vec<Out>, keep_lone: bool) -> Vec<Out> {
    let has_block = kids.iter().any(Out::is_block);
    if keep_lone && !has_block {
        return trim_run(kids);
    }
    let mut out = Vec::new();
    let mut run: Vec<Out> = Vec::new();
    let flush = |run: &mut Vec<Out>, out: &mut Vec<Out>| {
        for piece in split_breaks(std::mem::take(run)) {
            let piece = trim_run(piece);
            if piece.iter().any(Out::has_content) {
                out.push(Out::Elem(Elem { tag: "p", attrs: Vec::new(), kids: piece }));
            }
        }
    };
    for k in kids {
        if k.is_block() {
            flush(&mut run, &mut out);
            out.push(k);
        } else {
            run.push(k);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Split an inline run at two or more consecutive line breaks (blank text
/// between them notwithstanding).
fn split_breaks(run: Vec<Out>) -> Vec<Vec<Out>> {
    let mut pieces = vec![Vec::new()];
    let mut it = run.into_iter().peekable();
    while let Some(item) = it.next() {
        if item.is_br() {
            let mut gap = vec![item];
            let mut breaks = 1;
            while it.peek().is_some_and(|n| n.is_br() || n.is_blank_text()) {
                let n = it.next().unwrap();
                if n.is_br() {
                    breaks += 1;
                }
                gap.push(n);
            }
            if breaks >= 2 {
                pieces.push(Vec::new());
            } else {
                pieces.last_mut().unwrap().extend(gap);
            }
            continue;
        }
        pieces.last_mut().unwrap().push(item);
    }
    pieces
}

/// Drop blank text and line breaks at both ends of a run, and trim the
/// leading space of its first text and trailing space of its last.
fn trim_run(mut run: Vec<Out>) -> Vec<Out> {
    while run.first().is_some_and(|k| k.is_br() || k.is_blank_text()) {
        run.remove(0);
    }
    while run.last().is_some_and(|k| k.is_br() || k.is_blank_text()) {
        run.pop();
    }
    if let Some(Out::Text(t)) = run.first_mut() {
        *t = t.trim_start().to_string();
    }
    if let Some(Out::Text(t)) = run.last_mut() {
        *t = t.trim_end().to_string();
    }
    run
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_scripts_and_attributes_are_gone() {
        let html = "<html><head><style>p{color:red}</style><script>alert(1)</script></head>\
                    <body style=\"background:#000\"><p class=\"x\" style=\"color:red\" \
                    onclick=\"go()\">Hello <span style=\"font-size:40px\">world</span></p></body></html>";
        let out = extract(html);
        assert_eq!(out, "<p>Hello world</p>");
    }

    #[test]
    fn hidden_preview_text_is_dropped() {
        let html = "<div style=\"display:none;max-height:0;overflow:hidden\">Preview text you \
                    should not see</div><div style=\"mso-hide:all\">Outlook only</div>\
                    <div style=\"opacity:0\">ghost</div><div style=\"height:0;overflow:hidden\">pad</div>\
                    <div hidden>h</div><p>The real message.</p>";
        let out = extract(html);
        assert_eq!(out, "<p>The real message.</p>");
    }

    #[test]
    fn a_zero_font_size_column_is_not_hidden() {
        // MJML sets font-size:0px on every column and cell; the text inside
        // sets its own size and is very much meant to be read.
        let html = "<div style=\"font-size:0px;text-align:left;display:inline-block;width:100%\">\
                    <table><tr><td style=\"font-size:0px;padding:10px 25px\"><div style=\"font-size:16px\">Convention reminder</div></td></tr></table></div>\
                    <div style=\"max-height:0px\">not clipped, shown</div>";
        assert_eq!(extract(html), "<p>Convention reminder</p><p>not clipped, shown</p>");
    }

    #[test]
    fn a_heading_holds_a_line_not_boxes() {
        let html = "<h2><p>Your package</p><p>was delivered!</p></h2><p><div>a</div><div>b</div></p>";
        assert_eq!(extract(html), "<h2>Your package<br>was delivered!</h2><p>a</p><p>b</p>");
    }

    #[test]
    fn tracking_pixels_go_and_real_pictures_stay() {
        let html = "<p>Hi</p><img src=\"https://t.example/open.gif\" width=\"1\" height=\"1\">\
                    <img src=\"https://x.example/a.png\" style=\"width:1px;height:1px\">\
                    <img src=\"https://x.example/photo.jpg\" alt=\"A photo\" width=\"600\" \
                    style=\"border:2px solid red\"><img src=\"cid:part1\"><img src=\"logo.png\">";
        let out = extract(html);
        assert_eq!(
            out,
            "<p>Hi</p><p><img src=\"https://x.example/photo.jpg\" alt=\"A photo\"><img src=\"cid:part1\"></p>"
        );
    }

    #[test]
    fn a_layout_table_reads_as_blocks() {
        let html = "<table width=\"600\"><tr><td bgcolor=\"#fff\"><p>First</p></td></tr>\
                    <tr><td><table><tr><td>Nested</td></tr></table></td></tr></table>";
        let out = extract(html);
        assert!(!out.contains("<table"), "{out}");
        assert_eq!(out, "<p>First</p><p>Nested</p>");
    }

    #[test]
    fn a_data_table_keeps_its_grid() {
        let html = "<table><tr><th>Item</th><th>Price</th></tr><tr><td>Tea</td><td>3</td></tr></table>";
        let out = extract(html);
        assert_eq!(
            out,
            "<table><tr><th>Item</th><th>Price</th></tr><tr><td>Tea</td><td>3</td></tr></table>"
        );
        let plain = "<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>";
        assert!(extract(plain).starts_with("<table>"), "{}", extract(plain));
    }

    #[test]
    fn quote_markers_survive_for_the_fold() {
        let html = "<div>Reply text</div><div class=\"gmail_quote foo\">On Monday x wrote:\
                    <blockquote type=\"cite\" style=\"margin:0\">Earlier</blockquote></div>";
        let out = extract(html);
        assert!(out.contains("<div class=\"gmail_quote\">"), "{out}");
        assert!(out.contains("<blockquote type=\"cite\">"), "{out}");
        assert!(!out.contains("foo"), "{out}");
    }

    #[test]
    fn links_keep_their_target_and_nothing_dangerous() {
        let html = "<p><a href=\"https://a.example/x\" style=\"color:red\" target=\"_blank\">go</a> \
                    <a href=\"javascript:alert(1)\">bad</a> <a href=\"#top\">top</a> \
                    <a href=\"mailto:x@y.z\">mail</a></p>";
        let out = extract(html);
        assert_eq!(
            out,
            "<p><a href=\"https://a.example/x\">go</a> bad top <a href=\"mailto:x@y.z\">mail</a></p>"
        );
    }

    #[test]
    fn plain_text_keeps_its_lines() {
        let html = "<html><body><div class=\"vireo-plain\">line one\n\n  indented\n&gt; quoted</div></body></html>";
        let out = extract(html);
        assert_eq!(out, "<div class=\"vireo-plain\">line one\n\n  indented\n&gt; quoted</div>");
    }

    #[test]
    fn raw_plain_text_keeps_its_lines_too() {
        let out = extract("Hi,\n\nTwo lines & a line\n  indented.\n");
        assert_eq!(out, "<div class=\"vireo-plain\">Hi,\n\nTwo lines &amp; a line\n  indented.</div>");
        // The ordinary view's own plain wrapper (`body_html`) reads the same.
        let wrapped = "<!doctype html><html><head></head><body class=\"vireo-plain\">a\n\nb</body></html>";
        assert_eq!(extract(wrapped), "<div class=\"vireo-plain\">a\n\nb</div>");
    }

    #[test]
    fn double_breaks_become_paragraphs() {
        let html = "<div>First line<br>still first<br><br>Second<br/>\n<br/>Third</div>";
        let out = extract(html);
        assert_eq!(out, "<p>First line<br>still first</p><p>Second</p><p>Third</p>");
    }

    #[test]
    fn empty_boxes_and_spacers_are_dropped() {
        let html = "<div>&nbsp;</div><div><div></div></div><p><br></p><table><tr><td>&nbsp;</td></tr></table>\
                    <div><span> </span>Text</div>";
        let out = extract(html);
        assert_eq!(out, "<p>Text</p>");
    }

    #[test]
    fn text_is_escaped_and_whitespace_collapsed() {
        let html = "<p>a &lt; b   &amp;\n\n c</p>";
        assert_eq!(extract(html), "<p>a &lt; b &amp; c</p>");
    }

    #[test]
    fn inline_formatting_is_unified() {
        let html = "<p><b>bold</b> <i>it</i> <font color=\"red\">red</font> <tt>code</tt> <strike>no</strike></p>";
        assert_eq!(
            extract(html),
            "<p><strong>bold</strong> <em>it</em> red <code>code</code> <s>no</s></p>"
        );
    }

    #[test]
    fn the_document_carries_the_sheet_and_theme() {
        let doc = render("<p>x</p>", true, "#3584e4");
        assert!(doc.contains("color-scheme:dark"), "{doc}");
        assert!(doc.contains("#3584e4"), "{doc}");
        assert!(doc.contains("<body><p>x</p></body>"), "{doc}");
        let light = render("<p>x</p>", false, "#3584e4");
        assert!(light.contains("color-scheme:light"), "{light}");
    }

    /// `HYLKI_READER_PROBE=<file> cargo test --bin hylki reader::tests::probe_file -- --ignored --nocapture`
    /// prints what the reader makes of a cached body dumped to a file.
    #[test]
    #[ignore]
    fn probe_file() {
        let Ok(path) = std::env::var("HYLKI_READER_PROBE") else { return };
        let body = std::fs::read_to_string(path).unwrap();
        println!("{}", extract(&body));
    }

    #[test]
    fn nested_links_are_flattened() {
        let html = "<a href=\"https://a.example/\"><table><tr><td><a href=\"https://b.example/\">in</a></td></tr></table></a>";
        let out = extract(html);
        assert_eq!(out.matches("<a ").count(), 1, "{out}");
    }
}
