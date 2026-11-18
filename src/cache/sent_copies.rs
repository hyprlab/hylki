//! Copies of sent messages that the server would not take into Sent yet.

use rusqlite::params;

use super::Cache;

/// A sent message waiting for its copy to be filed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingSentCopy {
    pub(crate) id: u32,
    pub(crate) raw: Vec<u8>,
    /// The folder of the message it answers, tried before `sent_path`.
    pub(crate) reply_path: Option<String>,
    pub(crate) sent_path: Option<String>,
    pub(crate) attempts: u32,
    pub(crate) last_error: String,
}

impl Cache {
    /// Keep a sent message whose copy could not be filed. Returns its id.
    pub(crate) fn queue_sent_copy(
        &self,
        account_id: u32,
        raw: &[u8],
        reply_path: Option<&str>,
        sent_path: Option<&str>,
        error: &str,
    ) -> Option<u32> {
        self.conn
            .execute(
                "INSERT INTO sent_copies(account_id, raw, reply_path, sent_path, queued_at, \
                 attempts, last_error) VALUES(?1, ?2, ?3, ?4, ?5, 1, ?6)",
                params![
                    account_id,
                    raw,
                    reply_path,
                    sent_path,
                    crate::datefmt::now(),
                    error
                ],
            )
            .map_err(|e| tracing::warn!("could not keep the sent copy: {e}"))
            .ok()?;
        Some(self.conn.last_insert_rowid() as u32)
    }

    /// The copies waiting for this account, oldest first.
    pub(crate) fn pending_sent_copies(&self, account_id: u32) -> Vec<PendingSentCopy> {
        let run = || -> rusqlite::Result<Vec<PendingSentCopy>> {
            let mut stmt = self.conn.prepare(
                "SELECT id, raw, reply_path, sent_path, attempts, last_error FROM sent_copies \
                 WHERE account_id = ?1 ORDER BY queued_at, id",
            )?;
            let rows = stmt.query_map(params![account_id], |r| {
                Ok(PendingSentCopy {
                    id: r.get::<_, i64>(0)? as u32,
                    raw: r.get(1)?,
                    reply_path: r.get(2)?,
                    sent_path: r.get(3)?,
                    attempts: r.get::<_, i64>(4)? as u32,
                    last_error: r.get(5)?,
                })
            })?;
            rows.collect()
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("could not read the kept sent copies: {e}");
            Vec::new()
        })
    }

    /// The copy is filed at last.
    pub(crate) fn delete_sent_copy(&self, id: u32) {
        let _ = self
            .conn
            .execute("DELETE FROM sent_copies WHERE id = ?1", params![id]);
    }

    /// Another attempt failed.
    pub(crate) fn record_sent_copy_failure(&self, id: u32, error: &str) {
        let _ = self.conn.execute(
            "UPDATE sent_copies SET attempts = attempts + 1, last_error = ?2 WHERE id = ?1",
            params![id, error],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kept_sent_copy_round_trips_until_it_is_filed() {
        let c = Cache::in_memory().unwrap();
        let id = c
            .queue_sent_copy(
                1,
                b"From: me\r\n\r\nbody",
                Some("INBOX"),
                Some("Sent"),
                "NO [OVERQUOTA]",
            )
            .expect("kept");
        assert!(c.pending_sent_copies(2).is_empty());

        c.record_sent_copy_failure(id, "connection reset");
        let copies = c.pending_sent_copies(1);
        assert_eq!(
            copies,
            vec![PendingSentCopy {
                id,
                raw: b"From: me\r\n\r\nbody".to_vec(),
                reply_path: Some("INBOX".into()),
                sent_path: Some("Sent".into()),
                attempts: 2,
                last_error: "connection reset".into(),
            }]
        );

        c.delete_sent_copy(id);
        assert!(c.pending_sent_copies(1).is_empty());
    }
}
