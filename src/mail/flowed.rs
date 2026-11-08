//! `text/plain; format=flowed` (RFC 3676): plain text whose paragraphs the
//! sender broke at about 72 characters with a space before each break, so a
//! reader can join them again and wrap them to its own width (#606).
//!
//! Thunderbird, Apple Mail and Evolution send plain text this way. Shown as it
//! arrives, every paragraph stops a third of the way across the reader.

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
    use super::{flowed_params, unflow};

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
