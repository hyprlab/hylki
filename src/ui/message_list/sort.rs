//! The list's orders: comparing two messages by the chosen sort, the keys
//! the Tags and Correspondents columns sort by, and the sort a column
//! heading picks.

use crate::i18n::i18n;
use crate::models::Message;

/// Order two messages by the chosen sort (ties fall back to date).
pub(super) fn message_cmp(a: &Message, b: &Message, order: SortOrder) -> std::cmp::Ordering {
    let name_of = |m: &Message| {
        crate::contacts::shown_name_or_addr(&m.from_name, &m.from_addr).to_lowercase()
    };
    match order {
        SortOrder::DateNewest => b.timestamp.cmp(&a.timestamp),
        SortOrder::DateOldest => a.timestamp.cmp(&b.timestamp),
        SortOrder::Sender => name_of(a)
            .cmp(&name_of(b))
            .then(b.timestamp.cmp(&a.timestamp)),
        SortOrder::Subject => normalize_subject(&a.subject)
            .cmp(&normalize_subject(&b.subject))
            .then(b.timestamp.cmp(&a.timestamp)),
        // `true` sorts after `false`, so compare b-vs-a to put unread/flagged first.
        SortOrder::UnreadFirst => b.unread.cmp(&a.unread).then(b.timestamp.cmp(&a.timestamp)),
        SortOrder::FlaggedFirst => b
            .starred
            .cmp(&a.starred)
            .then(b.timestamp.cmp(&a.timestamp)),
        // The single line's other columns (#334).
        SortOrder::Recipients => {
            let to = |m: &Message| crate::ui::message_row::recipient_names(&m.to).to_lowercase();
            to(a).cmp(&to(b)).then(b.timestamp.cmp(&a.timestamp))
        }
        SortOrder::Account => a
            .account_id
            .cmp(&b.account_id)
            .then(b.timestamp.cmp(&a.timestamp)),
        SortOrder::Attachment => b
            .has_attachment
            .cmp(&a.has_attachment)
            .then(b.timestamp.cmp(&a.timestamp)),
        SortOrder::Replied => (b.answered, b.forwarded)
            .cmp(&(a.answered, a.forwarded))
            .then(b.timestamp.cmp(&a.timestamp)),
        SortOrder::Importance => b
            .importance
            .to_i64()
            .cmp(&a.importance.to_i64())
            .then(b.timestamp.cmp(&a.timestamp)),
        // Soonest due first; mail with no due date after all that has one.
        SortOrder::Due => (a.due == 0, a.due)
            .cmp(&(b.due == 0, b.due))
            .then(b.timestamp.cmp(&a.timestamp)),
        // Both are put in order by the rebuild, which knows a conversation's
        // writers and the tags' names (#443); one message alone goes by its
        // sender and its keywords.
        SortOrder::Correspondents => name_of(a)
            .cmp(&name_of(b))
            .then(b.timestamp.cmp(&a.timestamp)),
        SortOrder::Tags => {
            let tags = |m: &Message| m.keywords.join(",").to_lowercase();
            tags(a).cmp(&tags(b)).then(b.timestamp.cmp(&a.timestamp))
        }
    }
}

/// What the Tags column sorts by (#443): the names of the tags a message
/// wears, as its row shows them. Empty for a message without any.
pub(super) fn tag_sort_key(m: &Message, tags: &[crate::config::Tag]) -> String {
    let names: Vec<String> = m
        .pills(tags)
        .iter()
        .map(|p| match p {
            crate::models::Pill::Tag(t) => t.name.to_lowercase(),
            crate::models::Pill::Label(name) => name.to_lowercase(),
        })
        .collect();
    names.join(", ")
}

/// Order two sort keys of text, A to Z, an empty key after every other.
pub(super) fn text_key_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    (a.is_empty(), a).cmp(&(b.is_empty(), b))
}

/// The order a click on a column's heading sorts by (#334). In Sent the
/// sender column names the recipients, and sorts by them.
pub(super) fn column_sort(column: crate::config::ListColumn, show_recipient: bool) -> SortOrder {
    use crate::config::ListColumn as C;
    match column {
        C::Star => SortOrder::FlaggedFirst,
        C::Sender if show_recipient => SortOrder::Recipients,
        C::Sender => SortOrder::Sender,
        C::Recipients => SortOrder::Recipients,
        C::Subject => SortOrder::Subject,
        C::Attachment => SortOrder::Attachment,
        C::Replied => SortOrder::Replied,
        C::Importance => SortOrder::Importance,
        C::Account => SortOrder::Account,
        C::Due => SortOrder::Due,
        C::Date => SortOrder::DateNewest,
        C::Correspondents => SortOrder::Correspondents,
        C::Tags => SortOrder::Tags,
    }
}

/// Lower-case the subject and strip any leading run of reply/forward prefixes
/// (`Re:`, `Fwd:`, …) so subject-sorting keeps a topic and its replies adjacent.
pub(super) fn normalize_subject(subject: &str) -> String {
    const PREFIXES: &[&str] = &["re:", "fwd:", "fw:", "aw:", "sv:", "antw:", "wg:"];
    let mut s = subject.trim();
    loop {
        let lower = s.to_ascii_lowercase();
        let mut stripped = false;
        for p in PREFIXES {
            if lower.starts_with(p) {
                s = s[p.len()..].trim_start();
                stripped = true;
                break;
            }
        }
        if !stripped {
            break;
        }
    }
    s.trim().to_ascii_lowercase()
}

/// The Correspondents column (#334): who wrote in a conversation, newest
/// first and each once, you as "me". The newest leads because it is who the
/// conversation waits on, as the row's sender does (#443). A conversation
/// only you have written in, a message in Sent say, names who it went to
/// instead.
pub(super) fn correspondents(msgs: &[&Message]) -> String {
    let mut sorted = msgs.to_vec();
    sorted.sort_by_key(|m| std::cmp::Reverse(m.timestamp));
    let mut seen = std::collections::HashSet::new();
    let mut names: Vec<String> = Vec::new();
    let mut others = false;
    for m in &sorted {
        let own = crate::services::avatar::account_face(&m.from_addr).is_some();
        let key = if own {
            String::new()
        } else {
            m.from_addr.to_ascii_lowercase()
        };
        if !seen.insert(key) {
            continue;
        }
        if own {
            names.push(i18n("me"));
        } else {
            others = true;
            names
                .push(crate::contacts::shown_name_or_addr(&m.from_name, &m.from_addr).into_owned());
        }
    }
    if !others {
        if let Some(newest) = sorted.first() {
            let to = crate::ui::message_row::recipient_names(&newest.to);
            if !to.is_empty() {
                return format!("To: {to}");
            }
        }
    }
    names.join(", ")
}

/// How the message list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SortOrder {
    DateNewest,
    DateOldest,
    Sender,
    Subject,
    UnreadFirst,
    FlaggedFirst,
    Recipients,
    Account,
    Attachment,
    Importance,
    Due,
    Correspondents,
    Tags,
    Replied,
}

impl SortOrder {
    pub(super) const KEYS: [(SortOrder, &'static str); 14] = [
        (SortOrder::DateNewest, "date_newest"),
        (SortOrder::DateOldest, "date_oldest"),
        (SortOrder::Sender, "sender"),
        (SortOrder::Subject, "subject"),
        (SortOrder::UnreadFirst, "unread"),
        (SortOrder::FlaggedFirst, "flagged"),
        (SortOrder::Recipients, "recipients"),
        (SortOrder::Account, "account"),
        (SortOrder::Attachment, "attachment"),
        (SortOrder::Importance, "importance"),
        (SortOrder::Due, "due"),
        (SortOrder::Correspondents, "correspondents"),
        (SortOrder::Tags, "tags"),
        (SortOrder::Replied, "replied"),
    ];

    pub(crate) fn from_key(key: &str) -> Self {
        SortOrder::KEYS
            .iter()
            .find(|(_, k)| *k == key)
            .map(|(o, _)| *o)
            .unwrap_or(SortOrder::DateNewest)
    }

    pub(crate) fn key(self) -> &'static str {
        SortOrder::KEYS
            .iter()
            .find(|(o, _)| *o == self)
            .map(|(_, k)| *k)
            .unwrap_or("date_newest")
    }

    /// Whether the order runs from the most to the least: newest first,
    /// starred first and so on, against A to Z.
    pub(super) fn downwards(self) -> bool {
        matches!(
            self,
            SortOrder::DateNewest
                | SortOrder::UnreadFirst
                | SortOrder::FlaggedFirst
                | SortOrder::Attachment
                | SortOrder::Importance
                | SortOrder::Replied
        )
    }
}
