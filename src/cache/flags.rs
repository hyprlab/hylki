//! Message state: read, starred, answered, attachments, and keywords
//! (tags), including the tags kept only in the cache.

use rusqlite::params;

use crate::models::Message;

use super::{split_keywords, Cache, KEYWORDS_COL};

impl Cache {
    pub(crate) fn set_unread(&self, account_id: u32, folder_path: &str, uid: u32, unread: bool) {
        let _ = self.conn.execute(
            "UPDATE messages SET unread = ?1 WHERE account_id = ?2 AND folder_path = ?3 AND uid = ?4",
            params![unread, account_id, folder_path, uid],
        );
    }

    /// Correct a message's attachment flag once its body has proved the answer,
    /// so the paperclip survives a restart rather than being re-guessed.
    pub(crate) fn set_has_attachment(
        &self,
        account_id: u32,
        folder_path: &str,
        uid: u32,
        has: bool,
    ) {
        let _ = self.conn.execute(
            "UPDATE messages SET has_attachment = ?4 \
             WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
            params![account_id, folder_path, uid, has],
        );
    }

    pub(crate) fn mark_folder_read(&self, account_id: u32, folder_path: &str) {
        let _ = self.conn.execute(
            "UPDATE messages SET unread = 0 WHERE account_id = ?1 AND folder_path = ?2",
            params![account_id, folder_path],
        );
    }

    pub(crate) fn set_starred(&self, account_id: u32, folder_path: &str, uid: u32, starred: bool) {
        let _ = self.conn.execute(
            "UPDATE messages SET starred = ?1 WHERE account_id = ?2 AND folder_path = ?3 AND uid = ?4",
            params![starred, account_id, folder_path, uid],
        );
    }

    /// Mark a cached row replied to, or forwarded (#454).
    pub(crate) fn set_answered(
        &self,
        account_id: u32,
        folder_path: &str,
        uid: u32,
        forwarded: bool,
    ) {
        let column = if forwarded { "forwarded" } else { "answered" };
        let _ = self.conn.execute(
            &format!("UPDATE messages SET {column} = 1 WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3"),
            params![account_id, folder_path, uid],
        );
    }

    /// Every cached row of a folder as `(uid, starred, unread)`, what a
    /// flag check against the server (#532) is set against.
    pub(crate) fn flag_states(&self, account_id: u32, folder_path: &str) -> Vec<(u32, bool, bool)> {
        let run = || -> rusqlite::Result<Vec<(u32, bool, bool)>> {
            let mut stmt = self.conn.prepare(
                "SELECT uid, starred, unread FROM messages \
                 WHERE account_id = ?1 AND folder_path = ?2",
            )?;
            let rows = stmt.query_map(params![account_id, folder_path], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            rows.collect()
        };
        run().unwrap_or_default()
    }

    /// Whether a cached row is marked replied to and forwarded (#454).
    pub(crate) fn answer_state(
        &self,
        account_id: u32,
        folder_path: &str,
        uid: u32,
    ) -> (bool, bool) {
        self.conn
            .query_row(
                "SELECT answered, forwarded FROM messages WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
                params![account_id, folder_path, uid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or_default()
    }

    /// Add or drop a keyword on a cached row (the server copy just changed).
    pub(crate) fn set_keyword(
        &self,
        account_id: u32,
        folder_path: &str,
        uid: u32,
        keyword: &str,
        add: bool,
    ) -> bool {
        let mut current = self.keywords_of(account_id, folder_path, uid);
        current.retain(|k| !k.eq_ignore_ascii_case(keyword));
        if add {
            current.push(keyword.to_string());
        }
        self.conn
            .execute(
                "UPDATE messages SET keywords = ?1 WHERE account_id = ?2 AND folder_path = ?3 AND uid = ?4",
                params![current.join(" "), account_id, folder_path, uid],
            )
            .is_ok_and(|n| n > 0)
    }

    /// The uids in one folder whose cached server-side keywords include
    /// `keyword` (the keyword re-sync of #166 diffs this against the
    /// server's own search).
    pub(crate) fn uids_with_keyword(
        &self,
        account_id: u32,
        folder_path: &str,
        keyword: &str,
    ) -> Vec<u32> {
        let needle = format!(" {} ", keyword.to_ascii_lowercase());
        let run = || -> rusqlite::Result<Vec<u32>> {
            let mut stmt = self.conn.prepare(
                "SELECT uid FROM messages \
                 WHERE account_id = ?1 AND folder_path = ?2 \
                   AND instr(' ' || lower(keywords) || ' ', ?3) > 0",
            )?;
            let rows = stmt.query_map(params![account_id, folder_path, needle], |r| r.get(0))?;
            rows.collect()
        };
        run().unwrap_or_default()
    }

    /// The server-side keywords cached for one message (local tags excluded).
    pub(crate) fn keywords_of(&self, account_id: u32, folder_path: &str, uid: u32) -> Vec<String> {
        self.conn
            .query_row(
                "SELECT keywords FROM messages WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
                params![account_id, folder_path, uid],
                |r| r.get::<_, String>(0),
            )
            .map(split_keywords)
            .unwrap_or_default()
    }

    /// Add or drop a tag kept in Hylki only (#71), for accounts whose server
    /// can't hold it: POP3, or IMAP without `\*` in PERMANENTFLAGS. Keyed by
    /// Message-ID so the tag follows the message between folders and
    /// survives a re-sync of the row.
    pub(crate) fn set_local_tag(
        &self,
        account_id: u32,
        message_id: &str,
        keyword: &str,
        add: bool,
    ) {
        if message_id.is_empty() {
            return;
        }
        let _ = if add {
            self.conn.execute(
                "INSERT OR IGNORE INTO local_tags (account_id, message_id, keyword) VALUES (?1, ?2, ?3)",
                params![account_id, message_id, keyword],
            )
        } else {
            self.conn.execute(
                "DELETE FROM local_tags WHERE account_id = ?1 AND message_id = ?2 AND lower(keyword) = lower(?3)",
                params![account_id, message_id, keyword],
            )
        };
    }

    /// Fold the account's locally-kept tags into freshly fetched summaries, so
    /// what the worker hands the app matches what a cache load would show.
    pub(crate) fn apply_local_tags(&self, account_id: u32, messages: &mut [Message]) {
        let run = || -> rusqlite::Result<Vec<(String, String)>> {
            let mut stmt = self
                .conn
                .prepare("SELECT message_id, keyword FROM local_tags WHERE account_id = ?1")?;
            let rows = stmt.query_map(params![account_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        };
        let local = run().unwrap_or_default();
        if local.is_empty() {
            return;
        }
        for m in messages.iter_mut().filter(|m| !m.message_id.is_empty()) {
            for (_, kw) in local.iter().filter(|(id, _)| *id == m.message_id) {
                if !m.has_keyword(kw) {
                    m.keywords.push(kw.clone());
                }
            }
        }
    }

    /// How many cached messages of the account carry `keyword` (server or
    /// local), for the tag finder's report where the server itself gives no
    /// count (Microsoft 365 categories).
    pub(crate) fn count_with_keyword(&self, account_id: u32, keyword: &str) -> usize {
        let needle = format!(" {} ", keyword.to_ascii_lowercase());
        let sql = format!(
            "SELECT count(*) FROM messages \
             WHERE account_id = ?1 AND instr(' ' || lower({KEYWORDS_COL}) || ' ', ?2) > 0"
        );
        self.conn
            .query_row(&sql, params![account_id, needle], |row| {
                row.get::<_, i64>(0)
            })
            .map(|n| n.max(0) as usize)
            .unwrap_or(0)
    }
}
