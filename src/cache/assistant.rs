//! The reads the assistant server ([`crate::mcp`]) makes: the newest of a
//! folder, starred mail wherever it is filed, one message by its place, and
//! a search within what the user shares.

use rusqlite::{params, params_from_iter, types::Value};

use super::text_index::fts_term;
use super::{message_columns, message_from_row, Cache};
use crate::models::Message;
use crate::ui::message_list::Query;

/// One folder a search is narrowed to.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ScopedFolder {
    Path(String),
    /// Starred mail wherever it is filed.
    Starred,
}

/// One account a search may look in, as Settings allows it.
#[derive(Debug, Clone)]
pub(crate) struct MailScope {
    pub(crate) account_id: u32,
    /// Its message text may be searched. Without that a search must not say
    /// what the text holds, even by what it finds.
    pub(crate) read_bodies: bool,
    /// The folders it shares, and so the only ones looked in. Named rather
    /// than the others left out, as the cache can hold rows for folders the
    /// account does not have: those of another account, from before the
    /// numbers shifted when one was removed.
    pub(crate) shared_paths: Vec<String>,
    /// Only this folder.
    pub(crate) folder: Option<ScopedFolder>,
}

/// Results are narrowed once more to one copy of each message, so ask for
/// more than are kept: a Gmail message is listed once per label.
const COPIES_PER_MESSAGE: usize = 3;

/// `term` as a LIKE pattern matching it anywhere, its wildcards taken as
/// themselves (with `ESCAPE '\'`).
fn like_anywhere(term: &str) -> String {
    let mut out = String::with_capacity(term.len() + 2);
    out.push('%');
    for c in term.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

impl Cache {
    /// A folder's `limit` newest messages, newest first; only the unread
    /// ones when `unread_only`.
    pub(crate) fn newest_messages(
        &self,
        account_id: u32,
        folder_path: &str,
        unread_only: bool,
        limit: usize,
    ) -> Vec<Message> {
        let run = || -> rusqlite::Result<Vec<Message>> {
            let cols = message_columns();
            let unread = if unread_only { "AND unread = 1" } else { "" };
            let mut stmt = self.conn.prepare(&format!(
                "SELECT {cols} FROM messages WHERE account_id = ?1 AND folder_path = ?2 {unread} \
                 ORDER BY ts DESC LIMIT ?3"
            ))?;
            let rows = stmt.query_map(params![account_id, folder_path, limit as i64], |row| {
                message_from_row(row, 0, account_id, 0)
            })?;
            rows.collect()
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache newest_messages failed: {e}");
            Vec::new()
        })
    }

    /// An account's `limit` newest starred messages in any folder it has but
    /// Junk and Trash, with the folder each is in.
    pub(crate) fn newest_starred(
        &self,
        account_id: u32,
        unread_only: bool,
        limit: usize,
    ) -> Vec<(String, Message)> {
        let run = || -> rusqlite::Result<Vec<(String, Message)>> {
            let cols = message_columns();
            let unread = if unread_only { "AND unread = 1" } else { "" };
            let mut stmt = self.conn.prepare(&format!(
                "SELECT folder_path, {cols} FROM messages \
                 WHERE account_id = ?1 AND starred = 1 {unread} \
                 AND EXISTS (SELECT 1 FROM folders f WHERE f.account_id = messages.account_id \
                     AND f.path = messages.folder_path AND f.kind NOT IN (5, 6)) \
                 ORDER BY ts DESC LIMIT ?2"
            ))?;
            let rows = stmt.query_map(params![account_id, limit as i64], |row| {
                Ok((row.get(0)?, message_from_row(row, 1, account_id, 0)?))
            })?;
            rows.collect()
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache newest_starred failed: {e}");
            Vec::new()
        })
    }

    /// The summary of one message, by its place.
    pub(crate) fn message_at(
        &self,
        account_id: u32,
        folder_path: &str,
        uid: u32,
    ) -> Option<Message> {
        let cols = message_columns();
        self.conn
            .query_row(
                &format!(
                    "SELECT {cols} FROM messages \
                     WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3"
                ),
                params![account_id, folder_path, uid],
                |row| message_from_row(row, 0, account_id, 0),
            )
            .ok()
    }

    /// The `limit` newest messages `query` finds within `scopes`, one copy
    /// of each, as (account, folder, message). Every word has to be found
    /// somewhere in the message, as in the search field (#509).
    pub(crate) fn find_messages(
        &self,
        scopes: &[MailScope],
        query: &Query,
        limit: usize,
        offset: usize,
    ) -> Vec<(u32, String, Message)> {
        if scopes.is_empty() {
            return Vec::new();
        }
        let (sql, args) = find_sql(
            scopes,
            query,
            (limit + offset).saturating_mul(COPIES_PER_MESSAGE),
        );
        let run = || -> rusqlite::Result<Vec<(u32, String, Message)>> {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(args.iter()), |row| {
                let account: u32 = row.get(0)?;
                Ok((account, row.get(1)?, message_from_row(row, 2, account, 0)?))
            })?;
            rows.collect()
        };
        let mut seen = std::collections::HashSet::new();
        run()
            .unwrap_or_else(|e| {
                tracing::warn!("cache find_messages failed: {e}");
                Vec::new()
            })
            .into_iter()
            .filter(|(account, path, m)| {
                if m.message_id.is_empty() {
                    seen.insert((*account, format!("{path}|{}", m.uid)))
                } else {
                    seen.insert((*account, m.message_id.clone()))
                }
            })
            .skip(offset)
            .take(limit)
            .collect()
    }
}

/// The search as SQL and its arguments: nothing the user or an assistant
/// typed goes into the text.
fn find_sql(scopes: &[MailScope], query: &Query, limit: usize) -> (String, Vec<Value>) {
    let mut args: Vec<Value> = Vec::new();
    let mut each_scope = Vec::new();
    for scope in scopes {
        args.push(Value::Integer(scope.account_id.into()));
        let mut clause = String::from("(account_id = ?");
        let marks = vec!["?"; scope.shared_paths.len()].join(", ");
        clause.push_str(&format!(" AND folder_path IN ({marks})"));
        args.extend(scope.shared_paths.iter().cloned().map(Value::Text));
        match &scope.folder {
            Some(ScopedFolder::Path(path)) => {
                clause.push_str(" AND folder_path = ?");
                args.push(Value::Text(path.clone()));
            }
            Some(ScopedFolder::Starred) => clause.push_str(" AND starred = 1"),
            None => {}
        }
        clause.push(')');
        each_scope.push(clause);
    }
    let mut wheres = vec![format!("({})", each_scope.join(" OR "))];

    let like = |columns: &[&str], term: &str, args: &mut Vec<Value>| -> String {
        let any: Vec<String> = columns
            .iter()
            .map(|c| {
                args.push(Value::Text(like_anywhere(term)));
                format!("{c} LIKE ? ESCAPE '\\'")
            })
            .collect();
        any.join(" OR ")
    };
    let readable: Vec<String> = scopes
        .iter()
        .filter(|s| s.read_bodies)
        .map(|s| s.account_id.to_string())
        .collect();
    for term in &query.terms {
        let mut found = like(
            &[
                "subject",
                "from_name",
                "from_addr",
                "recipients",
                "cc",
                "preview",
            ],
            term,
            &mut args,
        );
        // One letter is in nearly every body, as in the search field.
        if let Some(fts) =
            fts_term(term).filter(|_| term.chars().count() >= 2 && !readable.is_empty())
        {
            found.push_str(&format!(
                " OR (account_id, folder_path, uid) IN (SELECT b.account_id, b.folder_path, b.uid \
                 FROM body_text t JOIN bodies b ON b.rowid = t.rowid \
                 WHERE body_text MATCH ? AND b.account_id IN ({}))",
                readable.join(",")
            ));
            args.push(Value::Text(fts));
        }
        wheres.push(format!("({found})"));
    }
    for term in &query.from {
        let c = like(&["from_name", "from_addr"], term, &mut args);
        wheres.push(format!("({c})"));
    }
    for term in &query.to {
        let c = like(&["recipients", "cc", "bcc"], term, &mut args);
        wheres.push(format!("({c})"));
    }
    for term in &query.subject {
        let c = like(&["subject"], term, &mut args);
        wheres.push(format!("({c})"));
    }
    for term in &query.folder {
        let c = like(&["folder_path"], term, &mut args);
        wheres.push(format!("({c})"));
    }
    if query.attachment {
        wheres.push("has_attachment = 1".into());
    }
    if let Some(unread) = query.unread {
        wheres.push(format!("unread = {}", i32::from(unread)));
    }
    if query.starred {
        wheres.push("starred = 1".into());
    }
    args.push(Value::Integer(limit as i64));
    let cols = message_columns();
    (
        format!(
            "SELECT account_id, folder_path, {cols} FROM messages WHERE {} \
             ORDER BY ts DESC LIMIT ?",
            wheres.join(" AND ")
        ),
        args,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> Cache {
        let cache = Cache::in_memory().unwrap();
        let message = |uid, subject: &str, from: &str, ts| Message {
            id: uid,
            uid,
            subject: subject.into(),
            from_addr: from.into(),
            message_id: format!("{uid}@example.org"),
            timestamp: ts,
            ..Message::blank()
        };
        cache.upsert_messages(
            1,
            "INBOX",
            &[
                message(1, "March invoice", "billing@example.org", 100),
                message(2, "Invoice for April", "billing@example.org", 300),
                message(3, "Lunch", "ada@example.org", 200),
            ],
        );
        cache.upsert_messages(
            1,
            "Spam",
            &[message(9, "Invoice prize", "x@example.net", 400)],
        );
        cache.upsert_messages(
            2,
            "INBOX",
            &[message(4, "Other invoice", "bo@example.org", 500)],
        );
        cache.save_body(1, "INBOX", 3, "<p>the secret word is pineapple</p>");
        cache
    }

    fn scope(account_id: u32) -> MailScope {
        MailScope {
            account_id,
            read_bodies: true,
            shared_paths: vec!["INBOX".into(), "Spam".into()],
            folder: None,
        }
    }

    fn found(cache: &Cache, scopes: &[MailScope], query: &str) -> Vec<u32> {
        cache
            .find_messages(scopes, &Query::parse(query), 20, 0)
            .into_iter()
            .map(|(_, _, m)| m.uid)
            .collect()
    }

    #[test]
    fn every_word_must_be_found_newest_first() {
        let cache = cache();
        // A phrase search would find neither: the words are apart, in any order.
        assert_eq!(found(&cache, &[scope(1)], "invoice april"), [2]);
        assert_eq!(found(&cache, &[scope(1)], "april invoice"), [2]);
        assert_eq!(found(&cache, &[scope(1)], "invoice"), [9, 2, 1]);
        assert_eq!(found(&cache, &[scope(1)], "from:billing march"), [1]);
        assert!(found(&cache, &[scope(1)], "invoice lunch").is_empty());
    }

    #[test]
    fn a_search_goes_further_back_with_an_offset() {
        let cache = cache();
        let uids = |offset| -> Vec<u32> {
            cache
                .find_messages(&[scope(1)], &Query::parse("invoice"), 2, offset)
                .into_iter()
                .map(|(_, _, m)| m.uid)
                .collect()
        };
        assert_eq!(uids(0), [9, 2]);
        assert_eq!(uids(2), [1]);
        assert!(uids(3).is_empty());
    }

    #[test]
    fn a_search_stays_in_what_is_shared() {
        let cache = cache();
        // Another account is not in scope.
        assert!(!found(&cache, &[scope(1)], "invoice").contains(&4));
        assert_eq!(found(&cache, &[scope(1), scope(2)], "other"), [4]);
        // A hidden folder's mail is not found.
        let hidden = MailScope {
            shared_paths: vec!["INBOX".into()],
            ..scope(1)
        };
        assert_eq!(found(&cache, &[hidden], "invoice"), [2, 1]);
        // Nor is anything outside the one folder asked for.
        let one = MailScope {
            folder: Some(ScopedFolder::Path("Spam".into())),
            ..scope(1)
        };
        assert_eq!(found(&cache, &[one], "invoice"), [9]);
    }

    #[test]
    fn text_is_searched_only_where_it_may_be_read() {
        let cache = cache();
        assert_eq!(found(&cache, &[scope(1)], "pineapple"), [3]);
        let no_reading = MailScope {
            read_bodies: false,
            ..scope(1)
        };
        // What a search finds would say what the text holds.
        assert!(found(&cache, &[no_reading], "pineapple").is_empty());
    }

    #[test]
    fn what_is_typed_is_never_part_of_the_sql() {
        let cache = cache();
        let evil = "x') OR 1=1 --";
        assert!(found(&cache, &[scope(1)], evil).is_empty());
        let (sql, _) = find_sql(&[scope(1)], &Query::parse(evil), 5);
        assert!(!sql.contains("1=1"), "{sql}");
    }

    #[test]
    fn one_copy_of_a_message_is_listed() {
        let cache = cache();
        let copy = Message {
            id: 77,
            uid: 77,
            subject: "March invoice".into(),
            message_id: "1@example.org".into(),
            timestamp: 100,
            ..Message::blank()
        };
        cache.upsert_messages(1, "All Mail", &[copy]);
        assert_eq!(found(&cache, &[scope(1)], "march").len(), 1);
    }

    #[test]
    fn a_term_is_found_anywhere_and_its_wildcards_are_its_own() {
        assert_eq!(like_anywhere("invoice"), "%invoice%");
        assert_eq!(like_anywhere("50%_off\\"), "%50\\%\\_off\\\\%");
    }
}
