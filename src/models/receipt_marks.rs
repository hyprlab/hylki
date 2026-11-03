//! What sent mail shows of its read receipts (#520): the grey checkmarks
//! of a message that asked for one, green once one came back.

use std::collections::{HashMap, HashSet};

use crate::i18n::{i18n, i18n_f};

use super::Message;

/// The sent mail waiting for a read receipt, and the receipts that came
/// back, by Message-ID in lower case: a server may hand a Message-ID back
/// in another case than it was sent in.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct ReceiptMarks {
    pub(crate) asked: HashSet<String>,
    /// Who read each message, and when (unix seconds, 0 for unknown).
    pub(crate) read: HashMap<String, Vec<(String, i64)>>,
}

/// One message's checkmarks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReceiptMark {
    /// Green: a receipt came back.
    pub(crate) read: bool,
    /// Who read it and when, or that nobody has said yet.
    pub(crate) tooltip: String,
}

impl ReceiptMarks {
    /// The checkmarks `m` wears, if it asked for a receipt or has one.
    pub(crate) fn of(&self, m: &Message) -> Option<ReceiptMark> {
        if m.message_id.is_empty() || (self.asked.is_empty() && self.read.is_empty()) {
            return None;
        }
        let key = m.message_id.to_lowercase();
        match self.read.get(&key).filter(|r| !r.is_empty()) {
            Some(readers) => Some(ReceiptMark {
                read: true,
                tooltip: read_tooltip(m, readers),
            }),
            None if self.asked.contains(&key) => Some(ReceiptMark {
                read: false,
                tooltip: i18n("No read receipt yet"),
            }),
            None => None,
        }
    }
}

/// "Read by Ann, 28 Oct 2026 at 11:06", or for mail to several people,
/// how many of them read it and then each of them.
fn read_tooltip(m: &Message, readers: &[(String, i64)]) -> String {
    let recipients: Vec<(String, String)> = [&m.to, &m.cc]
        .into_iter()
        .flat_map(|f| crate::worker::parse_recipients(f))
        .collect();
    let name = |addr: &str| {
        recipients
            .iter()
            .find(|(_, a)| a.eq_ignore_ascii_case(addr))
            .map(|(n, _)| n.trim())
            .filter(|n| !n.is_empty())
            .unwrap_or(addr)
            .to_string()
    };
    let line = |(addr, at): &(String, i64)| {
        let who = name(addr);
        if *at > 0 {
            i18n_f(
                "{who}, {when}",
                &[("who", &who), ("when", &crate::datefmt::date_time(*at))],
            )
        } else {
            who
        }
    };
    let mut total: Vec<String> = recipients.iter().map(|(_, a)| a.to_lowercase()).collect();
    total.sort();
    total.dedup();
    let total = total.len().max(readers.len());
    if total <= 1 {
        return i18n_f("Read by {reader}", &[("reader", &line(&readers[0]))]);
    }
    let mut out = i18n_f(
        "Read by {count} of {total}",
        &[
            ("count", &readers.len().to_string()),
            ("total", &total.to_string()),
        ],
    );
    for r in readers {
        out.push('\n');
        out.push_str(&line(r));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(to: &str, cc: &str) -> Message {
        Message {
            message_id: "ABC@example.com".into(),
            to: to.into(),
            cc: cc.into(),
            ..crate::models::test_util::msg(1, 1, 1)
        }
    }

    #[test]
    fn mail_that_asked_is_grey_until_a_receipt_comes_back() {
        let mut marks = ReceiptMarks::default();
        let m = sent("Ann <ann@example.org>", "");
        assert_eq!(marks.of(&m), None);
        marks.asked.insert("abc@example.com".into());
        assert!(!marks.of(&m).unwrap().read);
        marks.read.insert(
            "abc@example.com".into(),
            vec![("ann@example.org".into(), 0)],
        );
        let mark = marks.of(&m).unwrap();
        assert!(mark.read);
        assert_eq!(mark.tooltip, "Read by Ann");
    }

    #[test]
    fn mail_to_several_people_says_how_many_read_it() {
        let marks = ReceiptMarks {
            read: [(
                "abc@example.com".to_string(),
                vec![("bob@example.org".to_string(), 0)],
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let m = sent(
            "Ann <ann@example.org>, bob@example.org",
            "Cy <cy@example.org>",
        );
        assert_eq!(
            marks.of(&m).unwrap().tooltip,
            "Read by 1 of 3\nbob@example.org"
        );
    }
}
