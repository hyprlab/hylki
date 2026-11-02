//! The printed document (#16): every message inlined into one plain page
//! under a header that exists only on paper.

use super::html::body_html;
use super::remote::{route_remote, strip_remote};
use super::style::{inject_csp, inject_reader_style, reader_style_css};
use super::MessageView;
use crate::encoding::escape_html;
use crate::i18n::i18n;
use crate::models::Message;

/// Remove a document's structural tags, keeping everything inside them.
///
/// Printing cannot use the reader's sandboxed iframes: a print engine draws an
/// iframe at its on-screen size — scrollbars included — and clips whatever does
/// not fit, so a long message came out as one cropped page. Inlining each
/// message into the printed document instead lets it flow across pages.
///
/// `<style>` and `<meta>` are deliberately kept: a message's own CSS is what
/// makes it look like itself on paper too.
pub(super) fn inline_body(doc: &str) -> String {
    let mut out = String::with_capacity(doc.len());
    let lower = doc.to_ascii_lowercase();
    let mut i = 0;
    while i < doc.len() {
        if lower[i..].starts_with("<!doctype") {
            match doc[i..].find('>') {
                Some(end) => {
                    i += end + 1;
                    continue;
                }
                None => break,
            }
        }
        let structural = ["<html", "</html", "<head", "</head", "<body", "</body"];
        if let Some(tag) = structural.iter().find(|t| lower[i..].starts_with(**t)) {
            // Only a real tag: "<bodyguard" is content, "<body class=…" is not.
            let after = i + tag.len();
            let next = doc.as_bytes().get(after).copied().unwrap_or(b'>');
            if next == b'>' || next.is_ascii_whitespace() || next == b'/' {
                match doc[i..].find('>') {
                    Some(end) => {
                        i += end + 1;
                        continue;
                    }
                    None => break,
                }
            }
        }
        let ch = doc[i..].chars().next().unwrap_or('\u{fffd}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Point a message's own `html`/`body` rules at the block it prints in.
///
/// Inlining costs the isolation an iframe gave: with every message in one
/// document, one sender's `body{font-family:monospace}` would restyle the whole
/// printout, including the next message in the conversation. Redirecting those
/// selectors to `.hylki-print-msg` keeps the rule working where it is meant to
/// and nowhere else. Bare type selectors (`p`, `a`) still reach across, which is
/// the remaining price of printing a thread as one page.
pub(super) fn scope_styles(doc: &str, block: &str) -> String {
    let lower = doc.to_ascii_lowercase();
    let mut out = String::with_capacity(doc.len());
    let mut rest = 0;
    while let Some(open) = lower[rest..].find("<style").map(|i| i + rest) {
        let Some(body_start) = lower[open..].find('>').map(|i| open + i + 1) else {
            break;
        };
        let end = lower[body_start..]
            .find("</style")
            .map(|i| body_start + i)
            .unwrap_or(doc.len());
        out.push_str(&doc[rest..body_start]);
        out.push_str(&scope_css(&doc[body_start..end], block));
        rest = end;
    }
    out.push_str(&doc[rest..]);
    out
}

/// Rewrite `html`/`body` in every selector of a stylesheet.
pub(super) fn scope_css(css: &str, block: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut prelude = String::new();
    // What each open brace holds: rules (an at-rule such as `@media`) or
    // declarations. Only the former contains further selectors.
    let mut stack: Vec<bool> = Vec::new();
    let in_rules = |stack: &Vec<bool>| stack.last().copied().unwrap_or(true);
    for ch in css.chars() {
        match ch {
            '{' => {
                let holds_rules = prelude.trim_start().starts_with('@');
                if in_rules(&stack) {
                    out.push_str(&scope_selectors(&prelude, block));
                } else {
                    out.push_str(&prelude);
                }
                prelude.clear();
                stack.push(holds_rules);
                out.push('{');
            }
            '}' => {
                out.push_str(&prelude);
                prelude.clear();
                stack.pop();
                out.push('}');
            }
            _ => prelude.push(ch),
        }
    }
    out.push_str(&prelude);
    out
}

/// Replace whole-word `html`/`body` in a selector list with the message's block.
pub(super) fn scope_selectors(prelude: &str, block: &str) -> String {
    let bytes = prelude.as_bytes();
    let mut out = String::with_capacity(prelude.len());
    let mut i = 0;
    while i < prelude.len() {
        let word = ["html", "body"]
            .into_iter()
            .find(|w| prelude[i..].to_ascii_lowercase().starts_with(w));
        // A tag name, not part of `.body`, `#body` or `bodyguard`.
        let boundary_before = i == 0
            || !matches!(bytes[i - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'#' | b'%');
        if let Some(word) = word {
            let after = bytes.get(i + word.len()).copied();
            let boundary_after = !matches!(
                after,
                Some(b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_')
            );
            if boundary_before && boundary_after {
                out.push_str(block);
                i += word.len();
                continue;
            }
        }
        let ch = prelude[i..].chars().next().unwrap_or('\u{fffd}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Rules for the printed document, which is a plain page rather than a stack of
/// frames: long URLs wrap instead of running off the sheet, images shrink to the
/// page, and each message after the first starts a new one.
const PRINT_DOCUMENT_STYLES: &str = "\
    html,body{background:#fff;color:#000;margin:0;padding:0;}\
    body{font:11pt/1.5 system-ui,sans-serif;overflow-wrap:anywhere;}\
    img,table,pre{max-width:100% !important;}\
    img{height:auto !important;}\
    pre{white-space:pre-wrap;}\
    .hylki-print-msg + .hylki-print-msg{border-top:1pt solid #999;margin-top:12pt;padding-top:12pt;}\
    .hylki-print-msghdr{font:bold 10pt/1.45 system-ui,sans-serif;color:#000;margin:0 0 8pt;}\
    .hylki-print-hdr{display:block;padding:0 0 10pt;margin:0 0 12pt;\
      border-bottom:1pt solid #999;font:10pt/1.45 system-ui,sans-serif;color:#000;}\
    .hylki-print-subject{font-size:14pt;font-weight:700;margin:0 0 8pt;}\
    .hylki-print-row{margin:0 0 2pt;}\
    .hylki-print-label{font-weight:700;}";

/// The class naming the `n`th message's block in the printed document.
fn print_block_class(n: usize) -> String {
    format!("hylki-print-m{n}")
}

/// Assemble the printed page: the header, then every message inlined into it.
pub(super) fn print_document(
    header: &str,
    messages: &[(String, String)],
    allow_remote: bool,
) -> String {
    let mut body = header.to_string();
    for (n, (head, doc)) in messages.iter().enumerate() {
        let block = print_block_class(n);
        body.push_str(&format!("<article class=\"hylki-print-msg {block}\">"));
        body.push_str(head);
        body.push_str(&scope_styles(&inline_body(doc), &format!(".{block}")));
        body.push_str("</article>");
    }
    // The same content policy the reader's frames get: no scripts, and remote
    // content only when the sender is trusted.
    inject_csp(
        &format!(
            "<!doctype html><html><head><meta charset=\"utf-8\">\
             <style>{PRINT_DOCUMENT_STYLES}</style></head><body>{body}</body></html>"
        ),
        allow_remote,
        false,
    )
}

/// Who sent one message of a printed conversation, and when.
pub(super) fn print_message_header_html(m: &Message) -> String {
    let from = if m.from_addr.is_empty() {
        escape_html(&m.from_name)
    } else if m.from_name.trim().is_empty() {
        escape_html(&m.from_addr)
    } else {
        format!(
            "{} &lt;{}&gt;",
            escape_html(&m.from_name),
            escape_html(&m.from_addr)
        )
    };
    format!(
        "<div class=\"hylki-print-msghdr\">{from} — {date}</div>",
        date = escape_html(&m.datetime_full())
    )
}

/// The header block that only appears on paper: subject, who it is from and to,
/// and when.
///
/// On screen these facts are in the pane above the message, which is a GTK widget
/// and therefore cannot be printed — WebKit prints the document it is showing,
/// and that document is the body alone. Printed mail without a sender or a date
/// is close to useless (issue #16), so the same facts go into the document and
/// are hidden with `@media`.
pub(super) fn print_header_html(message: Option<&Message>) -> String {
    // Newest first, so a conversation is described by the message on top.
    let Some(m) = message else {
        return String::new();
    };
    let row = |label: &str, value: &str| -> String {
        if value.trim().is_empty() {
            return String::new();
        }
        format!(
            "<div class=\"hylki-print-row\"><span class=\"hylki-print-label\">{}</span> {}</div>",
            escape_html(label),
            escape_html(value)
        )
    };
    let from = if m.from_addr.trim().is_empty() {
        m.from_name.clone()
    } else if m.from_name.trim().is_empty() || m.from_name == m.from_addr {
        m.from_addr.clone()
    } else {
        format!("{} <{}>", m.from_name, m.from_addr)
    };
    let subject = if m.subject.trim().is_empty() {
        i18n("(no subject)")
    } else {
        m.subject.clone()
    };
    format!(
        "<div class=\"hylki-print-hdr\">\
           <div class=\"hylki-print-subject\">{subject}</div>{from}{to}{cc}{bcc}{date}\
         </div>",
        subject = escape_html(&subject),
        from = row(&i18n("From:"), &from),
        to = row(&i18n("To:"), &m.to),
        cc = row(&i18n("Cc:"), &m.cc),
        bcc = row(&i18n("Bcc:"), &m.bcc),
        date = row(&i18n("Date:"), &m.datetime_full()),
    )
}

/// A message subject reduced to something usable as a filename: no separators,
/// no leading dots, and short enough for any filesystem.
pub(super) fn sanitize_filename(subject: &str) -> String {
    let cleaned: String = subject
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '-'
            } else {
                c
            }
        })
        .collect();
    // Leading dots would make a hidden file; leading dashes are what a stripped
    // path separator leaves behind, and look like a command-line flag.
    let cleaned = cleaned.trim().trim_start_matches(['.', '-']).trim();
    if cleaned.is_empty() {
        return "Message".to_string();
    }
    cleaned
        .chars()
        .take(120)
        .collect::<String>()
        .trim_end()
        .to_string()
}

impl MessageView {
    /// What to call the print job — the subject, or something rather than nothing.
    pub(super) fn job_name(&self) -> String {
        self.current
            .as_ref()
            .map(|m| self.real_subject(m))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| i18n("Message"))
    }

    /// The document that gets printed: the header, then each message inlined
    /// into the page.
    ///
    /// Not the reader's document. That one puts every message in a sandboxed
    /// iframe, which is right on screen — an email's CSS cannot escape it — but
    /// wrong on paper, where a print engine draws the frame at its on-screen size
    /// with its scrollbars and clips the rest.
    pub(super) fn print_document_html(&self) -> String {
        // In a conversation the top header describes the first message only, so
        // each one says who sent it and when — as the reader's own per-message
        // headers do on screen.
        let conversation = self.thread.len() > 1;
        let messages: Vec<(String, String)> = self
            .thread
            .iter()
            .enumerate()
            .map(|(n, m)| {
                // Reader View prints what it shows: the content alone.
                let doc = if self.reader_mode {
                    crate::mail::reader::render(&m.body, false, &self.accent_hex())
                } else {
                    body_html(&m.body)
                };
                let doc = if self.remote_allowed {
                    route_remote(&doc)
                } else {
                    strip_remote(&doc)
                };
                // The reader's own fonts and colors (#56) reach paper too,
                // scoped to this message's block; a card shown with the
                // sender's formatting prints that way as well.
                let style = if self.sender_style.contains(&(m.account_id, m.id)) {
                    &crate::config::ReaderStyle::NONE
                } else {
                    &self.reader_style
                };
                let scope = format!(".{}", print_block_class(n));
                let doc = inject_reader_style(
                    &doc,
                    &reader_style_css(style, false, &self.accent_hex(), &scope),
                );
                let head = if conversation {
                    print_message_header_html(m)
                } else {
                    String::new()
                };
                (head, doc)
            })
            .collect();
        print_document(&self.print_header_html(), &messages, self.remote_allowed)
    }

    /// The header block that only appears on paper (see [`print_header_html`]).
    pub(super) fn print_header_html(&self) -> String {
        let first = self
            .thread
            .first()
            .or(self.current.as_ref())
            .map(|m| Message {
                subject: self.real_subject(m),
                ..m.clone()
            });
        print_header_html(first.as_ref())
    }
}
