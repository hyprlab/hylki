//! The reads the assistant server ([`crate::mcp`]) makes: the newest of a
//! folder, starred mail wherever it is filed, one message by its place, and
//! a search of subjects and senders across accounts.

use rusqlite::params;

use super::{message_columns, message_from_row, Cache};
use crate::models::Message;

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

    /// An account's `limit` newest starred messages in any folder but Junk
    /// and Trash, with the folder each is in.
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
                 AND NOT EXISTS (SELECT 1 FROM folders f WHERE f.account_id = messages.account_id \
                     AND f.path = messages.folder_path AND f.kind IN (5, 6)) \
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

    /// The `limit` newest messages whose subject or sender holds `term`,
    /// in one account or (`None`) every one, as (account, folder, message).
    pub(crate) fn find_in_headers(
        &self,
        account_id: Option<u32>,
        term: &str,
        limit: usize,
    ) -> Vec<(u32, String, Message)> {
        let run = || -> rusqlite::Result<Vec<(u32, String, Message)>> {
            let cols = message_columns();
            let mut stmt = self.conn.prepare(&format!(
                "SELECT account_id, folder_path, {cols} FROM messages \
                 WHERE (?1 IS NULL OR account_id = ?1) \
                 AND (subject LIKE ?2 ESCAPE '\\' OR from_name LIKE ?2 ESCAPE '\\' \
                      OR from_addr LIKE ?2 ESCAPE '\\') \
                 ORDER BY ts DESC LIMIT ?3"
            ))?;
            let rows = stmt.query_map(
                params![account_id, like_anywhere(term), limit as i64],
                |row| {
                    let account: u32 = row.get(0)?;
                    Ok((account, row.get(1)?, message_from_row(row, 2, account, 0)?))
                },
            )?;
            rows.collect()
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache find_in_headers failed: {e}");
            Vec::new()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::like_anywhere;

    #[test]
    fn a_term_is_found_anywhere_and_its_wildcards_are_its_own() {
        assert_eq!(like_anywhere("invoice"), "%invoice%");
        assert_eq!(like_anywhere("50%_off\\"), "%50\\%\\_off\\\\%");
    }
}
