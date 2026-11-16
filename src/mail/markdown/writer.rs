//! HTML back to Markdown source.

use super::dom::{parse, Elem, Node};

/// HTML to Markdown source. Used when the composer switches format, and to
/// write the plain-text alternative of a message composed as HTML — Markdown
/// being, by design, the form of plain text that still reads as itself.
pub(crate) fn from_html(html: &str) -> String {
    let nodes = parse(html);
    let md = blocks(&nodes);
    md.trim_matches('\n').to_string()
}

/// The plain-text alternative for an HTML body.
pub(crate) fn html_to_text(html: &str) -> String {
    from_html(html)
}

/// Is this element a block, as far as the Markdown writer is concerned?
pub(super) fn is_block(name: &str) -> bool {
    matches!(
        name,
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
            | "blockquote"
            | "pre"
            | "hr"
            | "table"
            | "dl"
            | "dt"
            | "dd"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "main"
            | "aside"
            | "figure"
            | "figcaption"
            | "body"
            | "html"
            | "form"
            | "nav"
            | "tr"
            | "thead"
            | "tbody"
            | "tfoot"
    )
}

/// Render nodes in block context: each block on its own, paragraphs made out
/// of the inline runs between them.
fn blocks(nodes: &[Node]) -> String {
    let refs: Vec<&Node> = nodes.iter().collect();
    blocks_ref(&refs)
}

fn blocks_ref(nodes: &[&Node]) -> String {
    let mut out = String::new();
    let mut pending: Vec<&Node> = Vec::new();

    fn flush(pending: &mut Vec<&Node>, out: &mut String) {
        if pending.is_empty() {
            return;
        }
        let text = inlines(pending.iter().copied());
        pending.clear();
        let text = text.trim_matches(|c: char| c == ' ' || c == '\n');
        if !text.is_empty() {
            push_block(out, text);
        }
    }

    for node in nodes {
        match node {
            Node::Elem(e) if is_block(&e.name) || e.name == "table" => {
                flush(&mut pending, &mut out);
                let rendered = block(e);
                if !rendered.trim().is_empty() {
                    push_block(&mut out, &rendered);
                }
            }
            other => pending.push(other),
        }
    }
    flush(&mut pending, &mut out);
    out
}

/// Append one block, separated from the last by a blank line.
fn push_block(out: &mut String, text: &str) {
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(text.trim_end_matches('\n'));
}

/// Whether a blockquote is WebKit's Indent rather than a quote: it styles
/// its own as `margin: 0 0 0 40px; border: none; padding: 0px;`.
fn is_indent(e: &Elem) -> bool {
    e.attr("style").is_some_and(|s| {
        s.replace(' ', "")
            .to_ascii_lowercase()
            .contains("border:none")
    })
}

/// One block element as Markdown.
fn block(e: &Elem) -> String {
    match e.name.as_str() {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = e.name[1..].parse::<usize>().unwrap_or(1);
            let text = inlines(e.children.iter()).trim().to_string();
            let id = e
                .attr("id")
                .filter(|s| !s.is_empty())
                .map(|s| format!(" {{#{s}}}"))
                .unwrap_or_default();
            format!("{} {text}{id}", "#".repeat(level))
        }
        "hr" => "---".to_string(),
        "pre" => {
            // The fence keeps the code exactly; a `language-x` class on the
            // inner <code> is the info string it came from.
            let lang = find_code_language(&e.children).unwrap_or_default();
            let text = raw_text(&e.children);
            let text = text.trim_matches('\n');
            let fence = if text.contains("```") { "````" } else { "```" };
            format!("{fence}{lang}\n{text}\n{fence}")
        }
        // The composer's Indent is a blockquote too, with no border (#358):
        // an indent, which plain text and Markdown leave as the paragraphs.
        "blockquote" if is_indent(e) => blocks(&e.children),
        "blockquote" => prefix_lines(&blocks(&e.children), "> "),
        "ul" | "ol" => list(e),
        "li" => blocks(&e.children),
        "dl" => definition_list(e),
        "dt" => inlines(e.children.iter()).trim().to_string(),
        "dd" => format!(": {}", inlines(e.children.iter()).trim()),
        "table" => table(e),
        _ => blocks(&e.children),
    }
}

/// `-` or `1.` items, with continuation lines indented under their marker.
fn list(e: &Elem) -> String {
    let ordered = e.name == "ol";
    let mut n: usize = e.attr("start").and_then(|s| s.parse().ok()).unwrap_or(1);
    let mut out = String::new();
    for child in &e.children {
        let Node::Elem(li) = child else { continue };
        if li.name != "li" {
            continue;
        }
        let marker = if ordered {
            let m = format!("{n}. ");
            n += 1;
            m
        } else {
            "- ".to_string()
        };
        // A task list keeps its box: the checkbox is an <input> the writer
        // put there, and `- [x]` is how it goes back to Markdown.
        let (box_mark, body) = task_marker(li);
        let text = blocks_ref(&body);
        let indent = " ".repeat(marker.chars().count());
        let mut lines = text.lines();
        let first = lines.next().unwrap_or("");
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("{marker}{box_mark}{first}"));
        for line in lines {
            out.push('\n');
            if line.is_empty() {
                continue;
            }
            out.push_str(&indent);
            out.push_str(line);
        }
    }
    out
}

/// `("[x] " | "[ ] " | "", the item's remaining children)`.
fn task_marker(li: &Elem) -> (String, Vec<&Node>) {
    let mut rest: Vec<&Node> = Vec::new();
    let mut mark = String::new();
    for child in &li.children {
        match child {
            Node::Elem(e) if e.name == "input" && e.attr("type") == Some("checkbox") => {
                mark = if e.attrs.iter().any(|(k, _)| k == "checked") {
                    "[x] ".into()
                } else {
                    "[ ] ".into()
                };
            }
            other => rest.push(other),
        }
    }
    (mark, rest)
}

fn definition_list(e: &Elem) -> String {
    let mut out = String::new();
    for child in &e.children {
        let Node::Elem(row) = child else { continue };
        match row.name.as_str() {
            "dt" => {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(inlines(row.children.iter()).trim());
            }
            "dd" => {
                out.push_str("\n: ");
                out.push_str(inlines(row.children.iter()).trim());
            }
            _ => {}
        }
    }
    out
}

/// A GFM pipe table. Alignment comes from the cells' own `text-align`, which
/// is what [`to_html`] wrote there in the first place.
fn table(e: &Elem) -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut aligns: Vec<&'static str> = Vec::new();
    let mut header = 0usize;
    collect_rows(&e.children, &mut rows, &mut aligns, &mut header);
    if rows.is_empty() {
        return String::new();
    }
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    aligns.resize(cols, "");
    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        let mut cells: Vec<String> = row.clone();
        cells.resize(cols, String::new());
        out.push_str(&format!("| {} |\n", cells.join(" | ")));
        if i == 0 {
            let rule: Vec<String> = aligns
                .iter()
                .map(|a| match *a {
                    "center" => ":---:".to_string(),
                    "right" => "---:".to_string(),
                    "left" => ":---".to_string(),
                    _ => "---".to_string(),
                })
                .collect();
            out.push_str(&format!("| {} |\n", rule.join(" | ")));
        }
    }
    // A table with no header row still needs one: an empty first row keeps
    // it a table instead of collapsing into prose.
    if header == 0 {
        let empty: Vec<String> = (0..cols).map(|_| String::new()).collect();
        out = format!(
            "| {} |\n| {} |\n{}",
            empty.join(" | "),
            vec!["---"; cols].join(" | "),
            out
        );
    }
    out.trim_end().to_string()
}

fn collect_rows(
    nodes: &[Node],
    rows: &mut Vec<Vec<String>>,
    aligns: &mut Vec<&'static str>,
    header: &mut usize,
) {
    for node in nodes {
        let Node::Elem(e) = node else { continue };
        match e.name.as_str() {
            "thead" | "tbody" | "tfoot" => {
                if e.name == "thead" {
                    *header = 1;
                }
                collect_rows(&e.children, rows, aligns, header)
            }
            "tr" => {
                let mut row = Vec::new();
                for cell in &e.children {
                    let Node::Elem(c) = cell else { continue };
                    if c.name != "td" && c.name != "th" {
                        continue;
                    }
                    if rows.is_empty() {
                        aligns.push(alignment(c));
                    }
                    // A newline inside a cell would end the table row.
                    row.push(
                        inlines(c.children.iter())
                            .trim()
                            .replace('\n', " ")
                            .replace('|', "\\|"),
                    );
                }
                rows.push(row);
            }
            _ => collect_rows(&e.children, rows, aligns, header),
        }
    }
}

fn alignment(cell: &Elem) -> &'static str {
    let style = cell.attr("style").unwrap_or("").to_ascii_lowercase();
    let align = cell.attr("align").unwrap_or("").to_ascii_lowercase();
    for want in ["center", "right", "left"] {
        if style.contains(&format!("text-align: {want}"))
            || style.contains(&format!("text-align:{want}"))
            || align == want
        {
            return match want {
                "center" => "center",
                "right" => "right",
                _ => "left",
            };
        }
    }
    ""
}

/// The `language-x` class of a fenced block's inner `<code>`.
fn find_code_language(nodes: &[Node]) -> Option<String> {
    for node in nodes {
        if let Node::Elem(e) = node {
            if e.name == "code" {
                if let Some(class) = e.attr("class") {
                    if let Some(lang) = class
                        .split_whitespace()
                        .find_map(|c| c.strip_prefix("language-"))
                    {
                        return Some(lang.to_string());
                    }
                }
            }
            if let Some(found) = find_code_language(&e.children) {
                return Some(found);
            }
        }
    }
    None
}

/// The text of a subtree with nothing escaped and no whitespace collapsing —
/// what a `<pre>` holds.
fn raw_text(nodes: &[Node]) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            Node::Text(t) => out.push_str(t),
            Node::Elem(e) if e.name == "br" => out.push('\n'),
            Node::Elem(e) => out.push_str(&raw_text(&e.children)),
        }
    }
    out
}

fn prefix_lines(text: &str, prefix: &str) -> String {
    text.lines()
        .map(|l| {
            if l.is_empty() {
                prefix.trim_end().to_string()
            } else {
                format!("{prefix}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render an inline run.
fn inlines<'a>(nodes: impl Iterator<Item = &'a Node>) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            Node::Text(t) => out.push_str(&escape_md(&collapse(t))),
            Node::Elem(e) => out.push_str(&inline(e)),
        }
    }
    out
}

/// Collapse HTML whitespace the way a renderer would.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() && c != '\u{a0}' {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    if space && !out.is_empty() {
        out.push(' ');
    }
    out
}

/// Backslash the characters that would otherwise be read as markup. `_` is
/// left alone deliberately: it is far more often part of a name than an
/// emphasis mark, and CommonMark ignores it inside a word anyway.
fn escape_md(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '*' | '`' | '[' | ']' | '<') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn inline(e: &Elem) -> String {
    let inner = || inlines(e.children.iter());
    match e.name.as_str() {
        "br" => "  \n".to_string(),
        "strong" | "b" => wrap(&inner(), "**"),
        "em" | "i" => wrap(&inner(), "*"),
        "del" | "s" | "strike" => wrap(&inner(), "~~"),
        "mark" => wrap(&inner(), "=="),
        "sup" => wrap(&inner(), "^"),
        "sub" => wrap(&inner(), "~"),
        "code" => {
            let text = raw_text(&e.children);
            let fence = if text.contains('`') { "``" } else { "`" };
            format!("{fence}{text}{fence}")
        }
        "kbd" => format!("<kbd>{}</kbd>", raw_text(&e.children)),
        "img" => {
            let alt = e.attr("alt").unwrap_or("");
            let src = e.attr("src").unwrap_or("");
            format!("![{alt}]({src})")
        }
        "a" => {
            let href = e.attr("href").unwrap_or("").to_string();
            let text = inner();
            let bare = text.trim();
            // A link with nothing to click (an empty anchor, or the outer
            // half of the <a><a> Outlook writes) is not worth writing.
            if href.is_empty() || bare.is_empty() {
                text
            } else if bare == href
                || href == format!("mailto:{bare}")
                || crate::mail::at_link::whole_link(bare).is_some_and(|h| h == href)
            {
                // An autolink reads better as itself than as a link whose
                // text and target are the same string twice.
                bare.replace('\\', "")
            } else {
                format!("[{}]({})", text.trim(), href)
            }
        }
        // Structure with no Markdown of its own: keep the text.
        _ => inner(),
    }
}

/// Wrap `text` in a marker, keeping the spaces it starts or ends with
/// outside — `** bold **` is not emphasis at all.
fn wrap(text: &str, marker: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return text.to_string();
    }
    let lead = if text.starts_with(' ') { " " } else { "" };
    let trail = if text.ends_with(' ') { " " } else { "" };
    format!("{lead}{marker}{trimmed}{marker}{trail}")
}
