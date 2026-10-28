//! Local SQLite cache for folders, message summaries, and bodies.
//!
//! The cache lets the app show mail instantly on startup, read offline, and
//! avoid re-fetching message bodies. Each per-account worker opens its own
//! connection (so the `!Send` `rusqlite::Connection` never crosses threads);
//! WAL mode keeps concurrent access from contending. Everything is keyed by
//! account so two accounts can both have an "INBOX". It is strictly
//! best-effort: any error is logged and degrades to "no cache".

use rusqlite::Connection;

use crate::models::Message;

mod addresses;
mod bodies;
mod flags;
mod gallery;
mod messages;
mod offline;
mod open;
mod outbox;
mod schema;
mod smart;
mod text_index;
mod threads;

pub(crate) use gallery::*;
pub(crate) use messages::*;

#[cfg(test)]
use schema::*;

#[cfg(test)]
mod gallery_tests;
#[cfg(test)]
mod schema_tests;
#[cfg(test)]
mod test_util;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod thread_tests;

/// A message's keywords as one column: the server's, then any tag kept
/// locally for the same Message-ID (POP3, or an IMAP server that refuses
/// custom keywords). `local_tags` is small and keyed for this lookup, so the
/// correlated subquery is an index seek per row.
const KEYWORDS_COL: &str =
    "messages.keywords || ' ' || COALESCE((SELECT group_concat(lt.keyword, ' ') \
    FROM local_tags lt WHERE lt.account_id = messages.account_id \
    AND messages.message_id <> '' AND lt.message_id = messages.message_id), '')";

/// Split a keywords column back into the list a [`Message`] carries.
fn split_keywords(col: String) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for k in col.split_whitespace() {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(k)) {
            out.push(k.to_string());
        }
    }
    out
}

/// The columns a [`Message`] is read from, in the order
/// [`message_from_row`] expects them.
fn message_columns() -> String {
    format!(
        "uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, \
         recipients, cc, message_id, references_, preview, reply_to, {KEYWORDS_COL}, \
         importance, due, gmail_labels, answered, forwarded, bcc"
    )
}

/// A message read from a row selecting [`message_columns`], starting at
/// column `first`. The body is never part of a summary row.
fn message_from_row(
    row: &rusqlite::Row,
    first: usize,
    account_id: u32,
    folder_id: u32,
) -> rusqlite::Result<Message> {
    let col = |n: usize| first + n;
    let uid: u32 = row.get(col(0))?;
    let mut m = Message {
        id: uid,
        account_id,
        folder_id,
        uid,
        from_name: row.get(col(1))?,
        from_addr: row.get(col(2))?,
        reply_to: row.get(col(14))?,
        to: row.get(col(9))?,
        cc: row.get(col(10))?,
        bcc: row.get(col(21))?,
        subject: row.get(col(3))?,
        preview: row.get(col(13))?,
        body: String::new(),
        date: row.get(col(4))?,
        timestamp: row.get(col(5))?,
        unread: row.get(col(6))?,
        starred: row.get(col(7))?,
        answered: row.get(col(19))?,
        forwarded: row.get(col(20))?,
        keywords: split_keywords(row.get(col(15))?),
        has_attachment: row.get(col(8))?,
        message_id: row.get(col(11))?,
        references: row.get(col(12))?,
        importance: crate::models::Importance::from_i64(row.get(col(16))?),
        due: row.get(col(17))?,
        labels: split_labels(row.get(col(18))?),
    };
    // Rows written before NUL-scrubbing existed may still carry one; GTK
    // labels abort on interior NULs.
    m.scrub_nuls();
    Ok(m)
}

/// Gmail labels as one column. A label can hold a space, so they are kept
/// apart by the unit separator, which no mailbox name contains.
const LABEL_SEP: char = '\u{1f}';

fn join_labels(labels: &[String]) -> String {
    labels.join(&LABEL_SEP.to_string())
}

fn split_labels(col: String) -> Vec<String> {
    col.split(LABEL_SEP)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// One connection to the cache database. Each worker thread opens its own;
/// the methods live in the child modules by what they touch.
pub(crate) struct Cache {
    conn: Connection,
}
