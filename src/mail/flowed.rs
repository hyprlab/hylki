//! `text/plain; format=flowed` (RFC 3676): plain text whose paragraphs the
//! sender broke at about 72 characters with a space before each break, so a
//! reader can join them again and wrap them to its own width (#606).
//!
//! Thunderbird, Apple Mail and Evolution send plain text this way. Shown as it
//! arrives, every paragraph stops a third of the way across the reader.
//! Hylki sends its own plain text the same way: wrapped for a reader that
//! knows nothing of flowed text, joined again by one that does.

use mail_parser::{MessagePart, MimeHeaders};

/// How a plain-text part was flowed, read from its Content-Type: `None` for a
/// part that is not `format=flowed`, otherwise whether `delsp=yes` (the space
/// before each soft break is not part of the text).
pub(crate) fn flowed_params(part: &MessagePart<'_>) -> Option<bool> {
    let attrs = part.content_type()?.attributes()?;
    let value = |name: &str| {
        attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim())
    };
    value("format")
        .is_some_and(|v| v.eq_ignore_ascii_case("flowed"))
        .then(|| value("delsp").is_some_and(|v| v.eq_ignore_ascii_case("yes")))
}

/// The text of a flowed part with its soft breaks joined, each paragraph on
/// one line. Quoted paragraphs keep their `>` marks, written once in front of
/// the joined line.
pub(crate) fn unflow(text: &str, delsp: bool) -> String {
    let mut out = String::with_capacity(text.len());
    // The paragraph being joined: its quote depth and its text so far.
    let mut open: Option<(usize, String)> = None;

    // A final line break ends the last line rather than opening another.
    let body = text.strip_suffix('\n').unwrap_or(text);
    for line in body.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let depth = line.bytes().take_while(|&b| b == b'>').count();
        let rest = &line[depth..];
        // Space-stuffing: a sender puts a space before a line that opens with
        // a space, a `>` or "From ", and the receiver takes it off again.
        let rest = rest.strip_prefix(' ').unwrap_or(rest);
        // The signature separator ends in a space but is never a soft break.
        let soft = rest.ends_with(' ') && rest != "-- ";
        let content = if soft && delsp {
            &rest[..rest.len() - 1]
        } else {
            rest
        };

        // A paragraph left open by a soft break carries on only at the same
        // depth: a change of depth ends it where it stood.
        let mut para = match open.take() {
            Some((d, para)) if d == depth => para,
            Some((d, para)) => {
                finish(&mut out, d, &para);
                String::new()
            }
            None => String::new(),
        };
        para.push_str(content);
        if soft {
            open = Some((depth, para));
        } else {
            finish(&mut out, depth, &para);
        }
    }
    if let Some((d, para)) = open {
        finish(&mut out, d, &para);
    }
    // Every `finish` ends its line with a break, the last one included, which
    // the text may not have had.
    if !text.ends_with('\n') {
        out.pop();
    }
    out
}

/// The text of a plain part as it is read: a flowed part with its soft
/// breaks joined, any other as it is.
pub(crate) fn readable<'a>(part: &MessagePart<'_>, text: &'a str) -> std::borrow::Cow<'a, str> {
    match flowed_params(part) {
        Some(delsp) => unflow(text, delsp).into(),
        None => text.into(),
    }
}

/// How long a flowed line may be, its soft-break space included: the
/// width RFC 3676 recommends.
const FLOW_WIDTH: usize = 72;

/// Text as a `format=flowed` part (without `delsp`) carries it: each long
/// line broken after a space into lines of at most [`FLOW_WIDTH`]
/// characters, every hard break kept. A word longer than a line, such as a
/// link, is never cut. Lines inside a Markdown code fence stay whole.
pub(crate) fn flow(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 32);
    let mut fence: Option<&str> = None;
    let body = text.strip_suffix('\n').unwrap_or(text);
    for line in body.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line == "-- " {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let (depth, rest) = quote_depth(line);
        // A space left at the end of a line would read as a soft break.
        let content = rest.trim_end_matches(' ');
        let prefix_len = if depth > 0 { depth + 1 } else { 0 };
        let mark = content.trim_start();
        if depth == 0 && (mark.starts_with("```") || mark.starts_with("~~~")) {
            let tick = &mark[..3];
            fence = match fence {
                Some(open) if open == tick => None,
                None => Some(tick),
                open => open,
            };
            push_line(&mut out, depth, content);
            continue;
        }
        if fence.is_some() && depth == 0 {
            push_line(&mut out, depth, content);
            continue;
        }
        let width = FLOW_WIDTH.saturating_sub(prefix_len).max(20);
        let mut current = String::new();
        let mut count = 0;
        for piece in words(content) {
            let len = piece.chars().count();
            if count > 0 && count + len > width {
                push_line(&mut out, depth, &current);
                current.clear();
                count = 0;
            }
            current.push_str(piece);
            count += len;
        }
        push_line(&mut out, depth, &current);
    }
    if !text.ends_with('\n') {
        out.pop();
    }
    out
}

/// A line's quote depth and the text after its marks. Quotes written
/// `> > ` (the way a plain-text reply nests them) count as deeply as `>>`,
/// which is how they are sent: RFC 3676 counts only the `>` at the start.
fn quote_depth(line: &str) -> (usize, &str) {
    let bytes = line.as_bytes();
    let (mut depth, mut i) = (0, 0);
    while bytes.get(i) == Some(&b'>') {
        depth += 1;
        i += 1;
        if bytes.get(i) == Some(&b' ') && bytes.get(i + 1) == Some(&b'>') {
            i += 1;
        }
    }
    let rest = &line[i..];
    let rest = if depth > 0 {
        rest.strip_prefix(' ').unwrap_or(rest)
    } else {
        rest
    };
    (depth, rest)
}

/// The text cut after each run of spaces: every piece but the last ends
/// in the space a soft break is made at.
fn words(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let start = rest.len() - rest.trim_start_matches(' ').len();
        let word_end = rest[start..].find(' ').map_or(rest.len(), |i| start + i);
        let after = &rest[word_end..];
        let end = word_end + after.len() - after.trim_start_matches(' ').len();
        let (piece, tail) = rest.split_at(end);
        rest = tail;
        Some(piece)
    })
}

/// One flowed line: its quote marks, then the text, space-stuffed where
/// the text would otherwise be taken for a quote, an indent or a
/// mangled "From " (RFC 3676 4.4).
fn push_line(out: &mut String, depth: usize, text: &str) {
    if depth > 0 {
        out.extend(std::iter::repeat_n('>', depth));
        if !text.is_empty() {
            out.push(' ');
        }
    } else if text.starts_with(' ') || text.starts_with('>') || text.starts_with("From ") {
        out.push(' ');
    }
    out.push_str(text);
    out.push('\n');
}

/// Write one paragraph at quote depth `depth`, and the line break after it.
fn finish(out: &mut String, depth: usize, para: &str) {
    if depth > 0 {
        out.extend(std::iter::repeat_n('>', depth));
        if !para.is_empty() {
            out.push(' ');
        }
    }
    out.push_str(para);
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::{flow, flowed_params, unflow, FLOW_WIDTH};

    #[test]
    fn flow_breaks_long_lines_after_a_space() {
        let long = "word ".repeat(40);
        let long = long.trim_end();
        let flowed = flow(long);
        let lines: Vec<&str> = flowed.split('\n').collect();
        assert!(lines.len() > 1, "{flowed}");
        for line in &lines[..lines.len() - 1] {
            assert!(line.ends_with(' '), "{line:?}");
            assert!(line.chars().count() <= FLOW_WIDTH, "{line:?}");
        }
        assert!(!lines.last().unwrap().ends_with(' '));
        assert_eq!(unflow(&flowed, false), long);
    }

    #[test]
    fn flow_keeps_hard_breaks_and_short_lines() {
        let text = "Hi Ada,\n\nShort line.   \nAnother.\n";
        // Trailing spaces go: they would turn a hard break soft.
        assert_eq!(flow(text), "Hi Ada,\n\nShort line.\nAnother.\n");
    }

    #[test]
    fn flow_never_cuts_a_long_word() {
        let url = format!("https://example.com/{}", "a".repeat(100));
        let text = format!("See {url} now");
        let flowed = flow(&text);
        assert!(flowed.contains(&url), "{flowed}");
        assert_eq!(unflow(&flowed, false), text);
    }

    #[test]
    fn flow_wraps_quotes_at_their_depth() {
        let quoted = format!("> > {}", "quoted text ".repeat(12).trim_end());
        let flowed = flow(&format!("{quoted}\n>\nmine"));
        for line in flowed.lines().filter(|l| l.starts_with(">>")) {
            assert!(line.starts_with(">> "), "{line:?}");
        }
        assert_eq!(
            unflow(&flowed, false),
            format!(">> {}\n>\nmine", "quoted text ".repeat(12).trim_end())
        );
    }

    #[test]
    fn flow_stuffs_lines_that_would_be_misread() {
        assert_eq!(flow("  indented\nFrom here"), "   indented\n From here");
        assert_eq!(unflow(&flow("  indented\nFrom here"), false), "  indented\nFrom here");
    }

    #[test]
    fn flow_keeps_the_signature_separator_and_code() {
        let code = format!("```\n{}\n```", "x ".repeat(60));
        let text = format!("Bye\n-- \nJane\n{code}");
        let flowed = flow(&text);
        assert!(flowed.contains("\n-- \n"), "{flowed}");
        assert!(flowed.contains(&"x ".repeat(60).trim_end().to_string()), "{flowed}");
    }

    #[test]
    fn joins_soft_breaks_into_one_line() {
        let text =
            "Lorem ipsum dolor sit amet \r\nconsectetur adipiscing \r\nelit.\r\n\r\nNext one.\r\n";
        assert_eq!(
            unflow(text, false),
            "Lorem ipsum dolor sit amet consectetur adipiscing elit.\n\nNext one.\n"
        );
    }

    #[test]
    fn keeps_hard_breaks() {
        let text = "Line one\nLine two\n";
        assert_eq!(unflow(text, false), "Line one\nLine two\n");
    }

    #[test]
    fn delsp_drops_the_space_before_a_soft_break() {
        assert_eq!(
            unflow("Donaudampf \r\nschiff\r\n", true),
            "Donaudampfschiff\n"
        );
    }

    #[test]
    fn removes_space_stuffing() {
        assert_eq!(
            unflow("  indented\r\n >not a quote\r\n", false),
            " indented\n>not a quote\n"
        );
    }

    #[test]
    fn quotes_join_only_at_the_same_depth() {
        let text = "> quoted soft \r\n> end\r\n>> deeper soft \r\n> back out\r\nmine\r\n";
        assert_eq!(
            unflow(text, false),
            "> quoted soft end\n>> deeper soft \n> back out\nmine\n"
        );
    }

    #[test]
    fn signature_separator_is_not_a_soft_break() {
        assert_eq!(unflow("Bye\r\n-- \r\nJane\r\n", false), "Bye\n-- \nJane\n");
    }

    #[test]
    fn soft_break_at_the_very_end_is_kept_open_and_closed() {
        assert_eq!(unflow("one \r\ntwo ", false), "one two ");
    }

    #[test]
    fn empty_quote_lines_have_no_trailing_space() {
        assert_eq!(unflow(">\r\n> hi\r\n", false), ">\n> hi\n");
    }

    #[test]
    fn reads_the_content_type_parameters() {
        use mail_parser::MessageParser;
        let parse = |ct: &str| {
            let raw = format!("Content-Type: {ct}\r\n\r\nx\r\n");
            let msg = MessageParser::default().parse(raw.as_bytes()).unwrap();
            flowed_params(msg.part(0).unwrap())
        };
        assert_eq!(parse("text/plain; charset=utf-8"), None);
        assert_eq!(parse("text/plain; format=flowed"), Some(false));
        assert_eq!(
            parse("text/plain; Format=\"Flowed\"; DelSp=Yes"),
            Some(true)
        );
    }
}
