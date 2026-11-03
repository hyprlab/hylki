//! Which of an account's messages are copied to On This Computer (#552), so
//! the background download asks only for the rest.

use rusqlite::params;

use super::Cache;

/// One copied message: its Message-ID, or where it has none, its folder
/// and uid.
pub(crate) type CopyKey = (String, String, u32);

impl Cache {
    /// Note that a message of `account_id` is copied.
    pub(crate) fn note_local_copy(&self, account_id: u32, key: &CopyKey) {
        self.note_local_copies(account_id, std::slice::from_ref(key));
    }

    /// Note many at once: the local tree's list read back in.
    pub(crate) fn note_local_copies(&self, account_id: u32, keys: &[CopyKey]) {
        let run = || -> rusqlite::Result<()> {
            let tx = self.conn.unchecked_transaction()?;
            {
                let mut stmt = tx.prepare(
                    "INSERT OR IGNORE INTO local_copies (account_id, message_id, folder_path, uid) \
                     VALUES (?1, ?2, ?3, ?4)",
                )?;
                for (message_id, path, uid) in keys {
                    let (path, uid) = if message_id.is_empty() {
                        (path.as_str(), *uid)
                    } else {
                        ("", 0)
                    };
                    stmt.execute(params![account_id, message_id, path, uid])?;
                }
            }
            tx.commit()
        };
        if let Err(e) = run() {
            tracing::warn!("cache note_local_copies failed: {e}");
        }
    }

    /// Whether a message is noted as copied: by its Message-ID, or where it
    /// has none, by its place.
    pub(crate) fn is_local_copy(&self, account_id: u32, key: &CopyKey) -> bool {
        let (message_id, path, uid) = key;
        let (path, uid) = if message_id.is_empty() {
            (path.as_str(), *uid)
        } else {
            ("", 0)
        };
        self.conn
            .query_row(
                "SELECT 1 FROM local_copies WHERE account_id = ?1 AND message_id = ?2 \
                 AND folder_path = ?3 AND uid = ?4",
                params![account_id, message_id, path, uid],
                |_| Ok(()),
            )
            .is_ok()
    }

    /// How many of the account's messages are noted as copied.
    pub(crate) fn local_copy_count(&self, account_id: u32) -> usize {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM local_copies WHERE account_id = ?1",
                params![account_id],
                |r| r.get::<_, i64>(0),
            )
            .map_or(0, |n| n as usize)
    }

    /// A message's read and starred state and its Message-ID, as listed.
    pub(crate) fn message_state(
        &self,
        account_id: u32,
        folder_path: &str,
        uid: u32,
    ) -> Option<(bool, bool, String)> {
        self.conn
            .query_row(
                "SELECT unread, starred, message_id FROM messages \
                 WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
                params![account_id, folder_path, uid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok()
    }
}

/// The condition, on a `messages` row `m`, that it is copied.
pub(super) const COPIED: &str = "EXISTS (SELECT 1 FROM local_copies c \
     WHERE c.account_id = m.account_id AND ((m.message_id != '' AND c.message_id = m.message_id) \
     OR (m.message_id = '' AND c.message_id = '' AND c.folder_path = m.folder_path AND c.uid = m.uid)))";
