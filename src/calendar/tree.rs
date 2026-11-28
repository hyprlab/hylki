//! An iCalendar document as a tree of components, each keeping its content
//! lines exactly as they arrived.
//!
//! An event edited here goes back to the server it came from, and that
//! server (or another client: Outlook, Apple Calendar, Thunderbird) may have
//! written properties Hylki never reads: `X-MICROSOFT-CDO-…`, `X-APPLE-…`,
//! `CONFERENCE`, a second `ATTACH`. Rebuilding the event from the fields the
//! editor knows would drop them all. So an edit loads the component as lines,
//! replaces only the properties it changed and writes the rest back verbatim.

use crate::mail::invite::ics::{escape, fold, split_line, Line};

/// One component (`VCALENDAR`, `VEVENT`, `VALARM`…) with its own properties
/// and the components nested in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Node {
    /// Upper case: `VEVENT`.
    pub(crate) name: String,
    /// Each property line, unfolded, as written.
    pub(crate) lines: Vec<String>,
    pub(crate) children: Vec<Node>,
}

impl Node {
    pub(crate) fn new(name: &str) -> Node {
        Node {
            name: name.to_ascii_uppercase(),
            ..Node::default()
        }
    }

    /// Read every top-level component in `text`. Lines outside any component
    /// are dropped, as is a component left unclosed at the end.
    pub(crate) fn parse_all(text: &str) -> Vec<Node> {
        let mut out = Vec::new();
        let mut stack: Vec<Node> = Vec::new();
        for raw in unfolded_lines(text) {
            let Some(line) = split_line(&raw) else {
                continue;
            };
            if line.name.eq_ignore_ascii_case("BEGIN") {
                stack.push(Node::new(line.value.trim()));
                continue;
            }
            if line.name.eq_ignore_ascii_case("END") {
                let Some(done) = stack.pop() else {
                    continue;
                };
                match stack.last_mut() {
                    Some(parent) => parent.children.push(done),
                    None => out.push(done),
                }
                continue;
            }
            if let Some(node) = stack.last_mut() {
                node.lines.push(raw);
            }
        }
        out
    }

    /// The first top-level component in `text`.
    pub(crate) fn parse(text: &str) -> Option<Node> {
        Node::parse_all(text).into_iter().next()
    }

    /// Each property, read.
    pub(crate) fn props(&self) -> impl Iterator<Item = Line> + '_ {
        self.lines.iter().filter_map(|l| split_line(l))
    }

    /// The first property called `name`.
    pub(crate) fn prop(&self, name: &str) -> Option<Line> {
        self.props().find(|l| l.name.eq_ignore_ascii_case(name))
    }

    /// Every property called `name`.
    pub(crate) fn all(&self, name: &str) -> Vec<Line> {
        self.props()
            .filter(|l| l.name.eq_ignore_ascii_case(name))
            .collect()
    }

    /// The text value of `name`, unescaped and trimmed, or empty.
    pub(crate) fn text(&self, name: &str) -> String {
        self.prop(name)
            .map(|l| l.value.trim().to_string())
            .unwrap_or_default()
    }

    /// Drop every property called `name`.
    pub(crate) fn remove(&mut self, name: &str) {
        self.lines.retain(|l| {
            split_line(l).is_none_or(|line| !line.name.eq_ignore_ascii_case(name))
        });
    }

    /// Put `line` (`NAME;PARAM=x:value`, already escaped) in place of every
    /// property of its name, where the first of them was, or at the end.
    pub(crate) fn replace(&mut self, line: String) {
        let name = split_line(&line).map(|l| l.name).unwrap_or_default();
        let at = self.lines.iter().position(|l| {
            split_line(l).is_some_and(|x| x.name.eq_ignore_ascii_case(&name))
        });
        self.remove(&name);
        match at {
            Some(i) if i <= self.lines.len() => self.lines.insert(i, line),
            _ => self.lines.push(line),
        }
    }

    /// Set a TEXT property, or drop it when `value` is empty.
    pub(crate) fn set_text(&mut self, name: &str, value: &str) {
        if value.trim().is_empty() {
            self.remove(name);
        } else {
            self.replace(format!("{name}:{}", escape(value)));
        }
    }

    /// Add a line after the rest.
    pub(crate) fn push(&mut self, line: String) {
        self.lines.push(line);
    }

    /// The component written out with CRLF line ends, folded at 75 octets.
    pub(crate) fn write(&self) -> String {
        let mut out = String::new();
        self.write_into(&mut out);
        out
    }

    fn write_into(&self, out: &mut String) {
        out.push_str(&format!("BEGIN:{}\r\n", self.name));
        for line in &self.lines {
            out.push_str(&fold(line));
            out.push_str("\r\n");
        }
        for child in &self.children {
            child.write_into(out);
        }
        out.push_str(&format!("END:{}\r\n", self.name));
    }
}

/// The content lines of `text`, continuations joined, blank lines dropped.
/// `unfold` would also split each line apart; the tree keeps the text.
fn unfolded_lines(text: &str) -> Vec<String> {
    let mut joined: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        match line.strip_prefix([' ', '\t']) {
            Some(rest) if !joined.is_empty() => {
                if let Some(last) = joined.last_mut() {
                    last.push_str(rest);
                }
            }
            _ => {
                if !line.trim().is_empty() {
                    joined.push(line.to_string());
                }
            }
        }
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENT: &str = "BEGIN:VEVENT\r\nUID:a@b\r\nSUMMARY:Standup\\, daily\r\n\
        X-MICROSOFT-CDO-BUSYSTATUS:BUSY\r\nDESCRIPTION:A long line that goes on and on and on \
        and on past the seventy-five octet limit of a content line\r\nBEGIN:VALARM\r\n\
        TRIGGER:-PT15M\r\nACTION:DISPLAY\r\nEND:VALARM\r\nEND:VEVENT\r\n";

    #[test]
    fn a_component_reads_and_writes_back_unchanged() {
        let node = Node::parse(EVENT).unwrap();
        assert_eq!(node.name, "VEVENT");
        assert_eq!(node.text("SUMMARY"), "Standup, daily");
        assert_eq!(node.children.len(), 1);
        assert_eq!(node.children[0].text("TRIGGER"), "-PT15M");
        let again = Node::parse(&node.write()).unwrap();
        assert_eq!(again, node);
        // Folding happened on the way out.
        assert!(node.write().contains("\r\n "));
    }

    #[test]
    fn replacing_a_property_keeps_the_others_and_its_place() {
        let mut node = Node::parse(EVENT).unwrap();
        node.set_text("SUMMARY", "Standup; weekly");
        assert_eq!(node.lines[1], "SUMMARY:Standup\\; weekly");
        assert_eq!(node.text("X-MICROSOFT-CDO-BUSYSTATUS"), "BUSY");
        node.set_text("SUMMARY", "");
        assert!(node.prop("SUMMARY").is_none());
        node.replace("LOCATION:Room 1".into());
        assert_eq!(node.lines.last().unwrap(), "LOCATION:Room 1");
    }

    #[test]
    fn several_components_and_stray_lines() {
        let text = "junk\nBEGIN:VCALENDAR\nVERSION:2.0\nBEGIN:VTIMEZONE\nTZID:X\nEND:VTIMEZONE\n\
            BEGIN:VEVENT\nUID:1\nEND:VEVENT\nEND:VCALENDAR\nBEGIN:VEVENT\nUID:2\nEND:VEVENT\n";
        let all = Node::parse_all(text);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].children.len(), 2);
        assert_eq!(all[1].text("UID"), "2");
    }
}
