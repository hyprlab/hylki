//! What is typed in the search field (#509): words that must each be found
//! somewhere in a message, in any order, a quoted phrase found as written,
//! and a few operators that narrow by one part of the message.

/// The most words a query matches by: one bit each in a message's match.
pub(crate) const MAX_TERMS: usize = 63;

/// A parsed query, everything lowercased.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Query {
    /// Words and quoted phrases, each to be found in the subject, the
    /// people, the preview, or the body.
    pub(crate) terms: Vec<String>,
    /// `from:`: in the sender's name or address.
    pub(crate) from: Vec<String>,
    /// `to:`: in a recipient's name or address, Cc and Bcc too.
    pub(crate) to: Vec<String>,
    /// `subject:`: in the subject.
    pub(crate) subject: Vec<String>,
    /// `in:`: in the name of the folder the message is filed in.
    pub(crate) folder: Vec<String>,
    /// `has:attachment`.
    pub(crate) attachment: bool,
    /// `is:unread`, `is:read`.
    pub(crate) unread: Option<bool>,
    /// `is:starred`.
    pub(crate) starred: bool,
}

/// Split a query into its pieces: runs of non-space, with a quoted stretch
/// kept whole (the quotes dropped) wherever it starts, so `from:"ann lee"`
/// is one piece. Each piece says whether any of it was quoted.
fn pieces(text: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut piece = String::new();
    let mut quoted = false;
    let mut in_quotes = false;
    for c in text.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                quoted = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if !piece.is_empty() {
                    out.push((std::mem::take(&mut piece), quoted));
                }
                quoted = false;
            }
            c => piece.push(c),
        }
    }
    if !piece.is_empty() {
        out.push((piece, quoted));
    }
    out
}

impl Query {
    pub(crate) fn parse(text: &str) -> Query {
        let mut q = Query::default();
        for (piece, quoted) in pieces(&text.to_lowercase()) {
            let operator = piece
                .split_once(':')
                .filter(|(_, value)| !value.is_empty() || quoted);
            let value = |v: &str| v.trim().to_string();
            match operator {
                Some(("from", v)) => q.from.push(value(v)),
                Some(("to" | "cc" | "bcc", v)) => q.to.push(value(v)),
                Some(("subject", v)) => q.subject.push(value(v)),
                Some(("in" | "folder", v)) => q.folder.push(value(v)),
                Some(("has", "attachment" | "attachments")) => q.attachment = true,
                Some(("is", "unread")) => q.unread = Some(true),
                Some(("is", "read")) => q.unread = Some(false),
                Some(("is", "starred")) => q.starred = true,
                // Anything else is a word: a time, a link, "re:".
                _ => {
                    let term = piece.trim().to_string();
                    if !term.is_empty() && q.terms.len() < MAX_TERMS && !q.terms.contains(&term) {
                        q.terms.push(term);
                    }
                }
            }
        }
        q.from.retain(|v| !v.is_empty());
        q.to.retain(|v| !v.is_empty());
        q.subject.retain(|v| !v.is_empty());
        q.folder.retain(|v| !v.is_empty());
        q
    }

    /// Whether the query asks for nothing at all.
    pub(crate) fn is_empty(&self) -> bool {
        *self == Query::default()
    }

    /// The bit of a message's match that every term found sets.
    pub(crate) fn all_terms(&self) -> u64 {
        (1u64 << self.terms.len()) - 1
    }

    /// Match one message's [`search text`](super::search::search_text_of):
    /// `None` when an operator on its text rules it out, otherwise one bit
    /// for each term the text holds.
    pub(crate) fn match_text(&self, text: &str) -> Option<u64> {
        if !(self.from.is_empty() && self.to.is_empty() && self.subject.is_empty())
            && !self.match_parts(text)
        {
            return None;
        }
        let mut found = 0u64;
        for (i, term) in self.terms.iter().enumerate() {
            if text.contains(term.as_str()) {
                found |= 1 << i;
            }
        }
        Some(found)
    }

    /// Whether the operators on one part of the text let it through.
    fn match_parts(&self, text: &str) -> bool {
        let mut parts = text.split('\u{0}');
        let subject = parts.next().unwrap_or("");
        let from_name = parts.next().unwrap_or("");
        let from_addr = parts.next().unwrap_or("");
        let recipients = parts.next().unwrap_or("");
        let has = |hay: &str, all: &[String]| all.iter().all(|v| hay.contains(v.as_str()));
        let from_ok = self
            .from
            .iter()
            .all(|v| from_name.contains(v.as_str()) || from_addr.contains(v.as_str()));
        from_ok && has(recipients, &self.to) && has(subject, &self.subject)
    }

    /// The operators on what the message is rather than what it says.
    pub(crate) fn match_flags(&self, m: &crate::models::Message) -> bool {
        (!self.attachment || m.has_attachment)
            && self.unread.is_none_or(|u| m.unread == u)
            && (!self.starred || m.starred)
    }

    /// Whether a folder of this name is one `in:` asks for.
    pub(crate) fn match_folder(&self, name: &str) -> bool {
        let name = name.to_lowercase();
        self.folder.iter().all(|v| name.contains(v.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_phrases_and_operators_are_told_apart() {
        let q = Query::parse(
            r#"Rossi mario "Invoice 44" from:ann to:"Lee K" in:Archive has:attachment is:unread re: 10:30"#,
        );
        assert_eq!(q.terms, ["rossi", "mario", "invoice 44", "re:", "10:30"]);
        assert_eq!(q.from, ["ann"]);
        assert_eq!(q.to, ["lee k"]);
        assert_eq!(q.folder, ["archive"]);
        assert!(q.attachment);
        assert_eq!(q.unread, Some(true));
        assert!(Query::parse("  ").is_empty());
        assert!(Query::parse("from:").terms == ["from:"]);
    }

    #[test]
    fn each_word_is_found_anywhere_and_in_any_order() {
        let text = crate::ui::message_list::search::search_text_parts(
            "Quarterly report",
            "Mario Rossi",
            "mario@example.com",
            "ann lee ann@x.test",
            "the numbers are in",
        );
        let q = Query::parse("rossi mario");
        assert_eq!(q.match_text(&text), Some(q.all_terms()));
        let q = Query::parse("\"rossi mario\" numbers");
        assert_eq!(q.match_text(&text), Some(0b10), "a phrase is as written");
        // A phrase never runs from one part into the next.
        let q = Query::parse("\"report mario\"");
        assert_eq!(q.match_text(&text), Some(0));
    }

    #[test]
    fn operators_look_in_their_own_part() {
        let text = crate::ui::message_list::search::search_text_parts(
            "Lunch",
            "Ann Lee",
            "ann@x.test",
            "bob stone bob@y.test",
            "with bob",
        );
        assert!(Query::parse("from:ann").match_text(&text).is_some());
        assert!(Query::parse("from:bob").match_text(&text).is_none());
        assert!(Query::parse("to:stone").match_text(&text).is_some());
        assert!(Query::parse("to:ann").match_text(&text).is_none());
        assert!(Query::parse("subject:lunch").match_text(&text).is_some());
        assert!(Query::parse("subject:bob").match_text(&text).is_none());
    }
}
