//! A message an assistant asks Hylki to prepare, checked before it goes to
//! the app: every recipient an email address, the text plain. The app
//! opens it in a composer ([`crate::mcp::AppCall::Draft`]); nothing here
//! or there sends it. Also what an assistant is shown of a composer that
//! is open ([`OpenDraft`]), and a change it asks for in one
//! ([`DraftEdit`]), which waits there for the user to apply.

use crate::models::Message;

/// A message to prepare, its recipients checked.
#[derive(Debug)]
pub(crate) struct Draft {
    pub(crate) kind: DraftKind,
    /// Each "Name <address>" or a bare address.
    pub(crate) to: Vec<String>,
    pub(crate) cc: Vec<String>,
    pub(crate) bcc: Vec<String>,
    /// A new message's; a reply or a forward takes the original's.
    pub(crate) subject: String,
    /// What the assistant wrote, plain text.
    pub(crate) text: String,
}

#[derive(Debug)]
pub(crate) enum DraftKind {
    /// A new message, from this account (its id), or from the one a new
    /// message starts from.
    New {
        account_id: Option<u32>,
    },
    /// An answer to the original, to everyone it went to with `all`.
    Reply {
        original: Box<Original>,
        all: bool,
    },
    Forward {
        original: Box<Original>,
    },
}

/// The message answered or forwarded, with its body (HTML) to quote.
#[derive(Debug)]
pub(crate) struct Original {
    pub(crate) message: Message,
    pub(crate) path: String,
    pub(crate) body: String,
}

/// What an assistant asks to change in an open composer: each field given
/// takes the place of what is there, the rest stay. It waits in the
/// composer until the user applies or rejects it; nothing changes before.
#[derive(Debug, Clone, Default)]
pub(crate) struct DraftEdit {
    pub(crate) to: Option<Vec<String>>,
    pub(crate) cc: Option<Vec<String>>,
    pub(crate) bcc: Option<Vec<String>>,
    pub(crate) subject: Option<String>,
    /// Plain text, in place of what is written above the signature and
    /// any quoted message.
    pub(crate) text: Option<String>,
}

impl DraftEdit {
    pub(crate) fn is_empty(&self) -> bool {
        self.to.is_none()
            && self.cc.is_none()
            && self.bcc.is_none()
            && self.subject.is_none()
            && self.text.is_none()
    }
}

/// What the user did with the last edit an assistant asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditOutcome {
    Applied,
    Rejected,
}

/// A composer open in Hylki, as it stands, for an assistant to read.
#[derive(Debug, Clone)]
pub(crate) struct OpenDraft {
    /// The composer's own id, which the app knows it by.
    pub(crate) compose_id: u32,
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) cc: String,
    pub(crate) bcc: String,
    pub(crate) subject: String,
    /// "new", "reply", "forward" or "template".
    pub(crate) kind: &'static str,
    /// An assistant prepared it.
    pub(crate) by_assistant: bool,
    /// The attached files' names.
    pub(crate) attachments: Vec<String>,
    /// It is to go out encrypted: its text stays on this computer.
    pub(crate) encrypted: bool,
    /// "rich", "plain", "markdown" or "html".
    pub(crate) format: &'static str,
    /// What is written above the signature and any quote, as text; only
    /// when it was asked for, and never for a message to be encrypted.
    pub(crate) text: Option<String>,
    /// An edit waiting for the user.
    pub(crate) pending: Option<DraftEdit>,
    /// What became of the last one.
    pub(crate) last_edit: Option<EditOutcome>,
}

/// The most text an assistant hands over for one message.
const TEXT_MAX: usize = 256 * 1024;
/// More recipients than a message from an assistant should have.
const RECIPIENTS_MAX: usize = 100;

/// `entries`, each one or more recipients, as one recipient each, or why
/// one is not an email address. A name alone is not guessed into one.
pub(super) fn recipients(entries: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for entry in entries.iter().map(|e| e.trim()).filter(|e| !e.is_empty()) {
        let parsed = crate::worker::parse_recipients(entry);
        if parsed.is_empty() {
            return Err(not_an_address(entry));
        }
        for (name, addr) in parsed {
            if !is_address(&addr) {
                return Err(not_an_address(entry));
            }
            out.push(crate::worker::format_recipient(&name, &addr));
        }
    }
    if out.len() > RECIPIENTS_MAX {
        return Err(format!(
            "{} recipients are more than one message prepared this way may have ({RECIPIENTS_MAX})",
            out.len()
        ));
    }
    Ok(out)
}

fn not_an_address(entry: &str) -> String {
    format!(
        "\"{entry}\" is not an email address. Find the address with find_contacts, or ask the \
         user for it; never guess one."
    )
}

/// Whether `addr` is a whole address, as Send will want it.
fn is_address(addr: &str) -> bool {
    let addr = addr.trim();
    addr.contains('@') && addr.parse::<lettre::Address>().is_ok()
}

/// The text checked: plain, with no more than a message's worth.
pub(super) fn text(body: &str) -> Result<String, String> {
    let body = body.replace("\r\n", "\n");
    if body.len() > TEXT_MAX {
        return Err(format!(
            "the text is {} bytes, more than the {TEXT_MAX} one message may have",
            body.len()
        ));
    }
    Ok(body.trim_end().to_string())
}

/// A subject on one line.
pub(super) fn subject(subject: &str) -> String {
    subject.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Plain text as the composer's HTML: a block for each paragraph, its line
/// ends kept, with an empty line between paragraphs, as typed.
pub(crate) fn text_html(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let paragraphs: Vec<String> = text
        .split("\n\n")
        .map(|p| p.trim_matches('\n'))
        .filter(|p| !p.trim().is_empty())
        .map(|p| format!("<div>{}</div>", crate::mail::quote::escaped_lines(p)))
        .collect();
    paragraphs.join("<div><br></div>")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn recipients_must_be_addresses() {
        assert_eq!(
            recipients(&list(&[
                "Ada Lovelace <ada@example.org>",
                " bo@example.com ",
                ""
            ])),
            Ok(list(&["Ada Lovelace <ada@example.org>", "bo@example.com"]))
        );
        // Two in one entry are two.
        assert_eq!(
            recipients(&list(&["a@example.org, b@example.org"]))
                .unwrap()
                .len(),
            2
        );
        for name in ["Ada", "Ada Lovelace <ada>", "ada@", "@example.org"] {
            let e = recipients(&list(&[name])).unwrap_err();
            assert!(e.contains("find_contacts"), "{name}: {e}");
        }
        let many: Vec<String> = (0..=RECIPIENTS_MAX)
            .map(|i| format!("p{i}@example.org"))
            .collect();
        assert!(recipients(&many).is_err());
    }

    #[test]
    fn text_goes_in_as_paragraphs_with_its_markup_escaped() {
        assert_eq!(
            text_html("Hi Ada,\r\n\r\nThe <b>plan</b>:\nfirst & then\n\n\n\nBo\n"),
            "<div>Hi Ada,</div><div><br></div><div>The &lt;b&gt;plan&lt;/b&gt;:<br>first &amp; \
             then</div><div><br></div><div>Bo</div>"
        );
        assert_eq!(text_html("  \n\n"), "");
    }

    #[test]
    fn a_subject_is_one_line() {
        assert_eq!(subject(" Lunch\r\n on  Friday "), "Lunch on Friday");
    }
}
