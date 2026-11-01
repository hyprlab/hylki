//! Drafts and templates kept as their own bytes, so one opens in the editor
//! at once instead of after a fetch from the server.

use rusqlite::params;

use super::Cache;

impl Cache {
    /// A draft's bytes as kept, if they are still this message's: the same
    /// Message-ID and date at the same place.
    pub(crate) fn load_draft_copy(
        &self,
        account_id: u32,
        folder_path: &str,
        uid: u32,
        message_id: &str,
        ts: i64,
    ) -> Option<Vec<u8>> {
        self.conn
            .query_row(
                "SELECT raw FROM draft_copies WHERE account_id = ?1 AND folder_path = ?2 \
                 AND uid = ?3 AND message_id = ?4 AND ts = ?5",
                params![account_id, folder_path, uid, message_id, ts],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .ok()
    }

    /// The drafts of a folder kept already, as (uid, Message-ID, date).
    pub(crate) fn draft_copies_in(
        &self,
        account_id: u32,
        folder_path: &str,
    ) -> Vec<(u32, String, i64)> {
        let run = || -> rusqlite::Result<Vec<(u32, String, i64)>> {
            let mut stmt = self.conn.prepare(
                "SELECT uid, message_id, ts FROM draft_copies \
                 WHERE account_id = ?1 AND folder_path = ?2",
            )?;
            let rows = stmt
                .query_map(params![account_id, folder_path], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .collect();
            rows
        };
        run().unwrap_or_default()
    }

    /// Keep a draft's bytes, in place of any copy at the same place.
    pub(crate) fn save_draft_copy(
        &self,
        account_id: u32,
        folder_path: &str,
        uid: u32,
        message_id: &str,
        ts: i64,
        raw: &[u8],
    ) {
        if let Err(e) = self.conn.execute(
            "INSERT OR REPLACE INTO draft_copies (account_id, folder_path, uid, message_id, ts, raw) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![account_id, folder_path, uid, message_id, ts, raw],
        ) {
            tracing::warn!("cache save_draft_copy failed: {e}");
        }
    }

    /// Drop the copies of drafts a folder no longer holds: those sent,
    /// deleted, or saved again under a new UID.
    pub(crate) fn prune_draft_copies(&self, account_id: u32, folder_path: &str, kept: &[u32]) {
        let gone: Vec<u32> = self
            .draft_copies_in(account_id, folder_path)
            .into_iter()
            .map(|(uid, _, _)| uid)
            .filter(|uid| !kept.contains(uid))
            .collect();
        for uid in gone {
            let _ = self.conn.execute(
                "DELETE FROM draft_copies WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
                params![account_id, folder_path, uid],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_draft_copy_serves_only_the_message_it_was_taken_from() {
        let cache = Cache::in_memory().expect("cache");
        cache.save_draft_copy(1, "Drafts", 7, "a@x", 100, b"raw a");
        assert_eq!(
            cache.load_draft_copy(1, "Drafts", 7, "a@x", 100).as_deref(),
            Some(&b"raw a"[..])
        );
        // Another message at the same UID, or the same one changed.
        assert!(cache.load_draft_copy(1, "Drafts", 7, "b@x", 100).is_none());
        assert!(cache.load_draft_copy(1, "Drafts", 7, "a@x", 200).is_none());
        cache.save_draft_copy(1, "Drafts", 8, "c@x", 300, b"raw c");
        cache.prune_draft_copies(1, "Drafts", &[8]);
        assert_eq!(
            cache.draft_copies_in(1, "Drafts"),
            vec![(8, "c@x".to_string(), 300)]
        );
    }
}
