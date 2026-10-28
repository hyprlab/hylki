//! The HTML body: every anchor, scored. A small hand-rolled tag reader,
//! enough for anchors, their attributes and their text.

use super::action::parse_mailto;
use super::body::{has_strong, link_score, squash, BodyFindings};
use crate::encoding::unescape_html;

/// Every anchor in an HTML body, scored.
pub(super) fn scan_html(html: &str) -> BodyFindings {
    let mut out = BodyFindings::default();
    let lower = html.to_ascii_lowercase();
    let mut pos = 0;
    while let Some(rel) = lower[pos..].find("<a") {
        let start = pos + rel;
        let after = start + 2;
        // `<a ` or `<a>`, not `<abbr>`/`<address>`.
        if !lower[after..].starts_with(|c: char| c.is_whitespace() || c == '>') {
            pos = after;
            continue;
        }
        let Some(tag_end_rel) = lower[after..].find('>') else {
            break;
        };
        let tag_end = after + tag_end_rel;
        let tag = &html[start..tag_end];
        let close = lower[tag_end + 1..].find("</a").map(|r| tag_end + 1 + r);
        let inner_end = close.unwrap_or_else(|| (tag_end + 1 + 400).min(html.len()));
        let inner_end = floor_char(html, inner_end);
        let inner = &html[tag_end + 1..inner_end];
        pos = inner_end;

        let Some(href) = attr(tag, "href").map(|h| unescape_html(&h)) else {
            continue;
        };
        let href = href.trim().to_string();
        let mut text = tag_text(inner);
        for a in ["title", "aria-label"] {
            if let Some(v) = attr(tag, a) {
                text.push(' ');
                text.push_str(&unescape_html(&v));
            }
        }
        // The sentence around the link, for a "click here". Both sides
        // matter: "to unsubscribe, click here" and "click here to
        // unsubscribe" are equally common, and only reading backwards
        // missed every message written the second way. The trailing side
        // stops at the next link so one anchor never borrows the next
        // one's words.
        let ctx_start = floor_char(html, start.saturating_sub(600));
        let before = tag_text(&html[ctx_start..start]);
        let before: String = before
            .chars()
            .rev()
            .take(160)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let tail_from = match close {
            Some(c) => lower[c..].find('>').map(|r| c + r + 1).unwrap_or(inner_end),
            None => inner_end,
        };
        let tail_from = floor_char(html, tail_from);
        let tail_to = floor_char(html, (tail_from + 600).min(html.len()));
        let tail = &html[tail_from..tail_to];
        let tail = match lower[tail_from..tail_to].find("<a") {
            Some(next) => &tail[..next],
            None => tail,
        };
        let after: String = tag_text(tail).chars().take(160).collect();
        let context = format!("{before} {after}");

        let lower_href = href.to_ascii_lowercase();
        if lower_href.starts_with("mailto:") {
            let sq = squash(&format!("{text} {href}"));
            if (has_strong(&sq) || has_strong(&squash(&context)))
                && out.mailto.is_none()
                && parse_mailto(&href).is_some()
            {
                out.mailto = Some(href);
            }
        } else if lower_href.starts_with("http://") || lower_href.starts_with("https://") {
            let score = link_score(&text, &href, &context);
            out.offer_web(href, score);
        }
    }
    out
}

/// Back `i` off to a char boundary.
fn floor_char(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// An attribute's raw value out of a tag's text (`<a href="…" title='…'>`).
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(rel) = lower[from..].find(name) {
        let at = from + rel;
        from = at + name.len();
        let preceded = at > 0 && lower.as_bytes()[at - 1].is_ascii_whitespace();
        if !preceded {
            continue;
        }
        let rest = lower[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let value_at = tag.len() - rest.len();
        let rest_orig = tag[value_at..].trim_start();
        let value_at = tag.len() - rest_orig.len();
        let value = match rest_orig.chars().next() {
            Some(q @ ('"' | '\'')) => {
                let body = &tag[value_at + 1..];
                match body.find(q) {
                    Some(e) => &body[..e],
                    None => body,
                }
            }
            _ => {
                let e = rest_orig
                    .find(|c: char| c.is_whitespace() || c == '>')
                    .unwrap_or(rest_orig.len());
                &rest_orig[..e]
            }
        };
        return Some(value.to_string());
    }
    None
}

/// The text of a run of HTML: tags dropped (an image's alt text kept),
/// entities decoded, whitespace collapsed.
fn tag_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        out.push(' ');
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else {
            rest = "";
            break;
        };
        let tag = &after[..gt];
        let lower = tag.trim_start().to_ascii_lowercase();
        if lower.starts_with("img") {
            if let Some(alt) = attr(tag, "alt").or_else(|| attr(tag, "title")) {
                out.push_str(&alt);
                out.push(' ');
            }
        }
        // A style or script block's content is not text.
        if lower.starts_with("style") || lower.starts_with("script") {
            let close = after[gt..].to_ascii_lowercase().find("</");
            rest = match close {
                Some(c) => &after[gt + c..],
                None => "",
            };
            continue;
        }
        rest = &after[gt + 1..];
    }
    out.push_str(rest);
    let decoded = unescape_html(&out);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities_and_attributes_are_read() {
        assert_eq!(
            unescape_html("a &amp; b &#39;c&#x27; &nbsp;d &unknown; e"),
            "a & b 'c'  d &unknown; e"
        );
        assert_eq!(
            attr("<a class=x href='https://a/b' title=\"T\">", "href").as_deref(),
            Some("https://a/b")
        );
        assert_eq!(
            attr("<a data-href=\"no\" href=https://a/c>", "href").as_deref(),
            Some("https://a/c")
        );
        assert_eq!(
            tag_text("Un<b>sub</b>scribe <img alt=\"now\"> <style>p{}</style>x"),
            "Un sub scribe now x"
        );
    }
}
