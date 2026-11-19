//! The outbox: messages queued to send.

use rusqlite::params;

use super::Cache;

impl Cache {
    /// Queue a message that failed to send. Returns its Outbox id.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn queue_outbox(
        &self,
        account_id: u32,
        from_addr: &str,
        rcpts: &[String],
        recipients: &str,
        subject: &str,
        preview: &str,
        raw: &[u8],
        sent_path: Option<&str>,
        error: &str,
        send_at: Option<i64>,
    ) -> Option<u32> {
        let queued_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.conn
            .execute(
                "INSERT INTO outbox(account_id, from_addr, rcpts, recipients, subject, \
                 preview, raw, sent_path, queued_at, attempts, last_error, send_at) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?11)",
                params![
                    account_id,
                    from_addr,
                    rcpts.join("\n"),
                    recipients,
                    subject,
                    preview,
                    raw,
                    sent_path,
                    queued_at,
                    error,
                    send_at,
                ],
            )
            .map_err(|e| tracing::warn!("could not queue the message: {e}"))
            .ok()?;
        Some(self.conn.last_insert_rowid() as u32)
    }

    /// Note the message a queued forward forwards (#454).
    pub(crate) fn set_outbox_forwards(&self, id: u32, forwards: &str) {
        let _ = self.conn.execute(
            "UPDATE outbox SET forwards = ?1 WHERE id = ?2",
            params![forwards, id],
        );
    }

    /// Everything waiting for this account, oldest first (the order it is sent in).
    pub(crate) fn outbox_items(&self, account_id: u32) -> Vec<crate::models::OutboxItem> {
        let mut stmt = match self.conn.prepare(
            "SELECT id, account_id, from_addr, rcpts, recipients, subject, preview, raw, \
             sent_path, queued_at, attempts, last_error, send_at, forwards FROM outbox \
             WHERE account_id = ?1 ORDER BY queued_at, id",
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("could not read the outbox: {e}");
                return Vec::new();
            }
        };
        let rows = stmt.query_map(params![account_id], |r| {
            let rcpts: String = r.get(3)?;
            Ok(crate::models::OutboxItem {
                id: r.get::<_, i64>(0)? as u32,
                account_id: r.get::<_, i64>(1)? as u32,
                from_addr: r.get(2)?,
                rcpts: rcpts.lines().map(str::to_string).collect(),
                recipients: r.get(4)?,
                subject: r.get(5)?,
                preview: r.get(6)?,
                raw: r.get(7)?,
                sent_path: r.get(8)?,
                queued_at: r.get(9)?,
                attempts: r.get::<_, i64>(10)? as u32,
                last_error: r.get(11)?,
                send_at: r.get(12)?,
                forwards: r.get(13)?,
                status: String::new(),
            })
        });
        match rows {
            Ok(rows) => rows.filter_map(Result::ok).collect(),
            Err(e) => {
                tracing::warn!("could not read the outbox: {e}");
                Vec::new()
            }
        }
    }

    /// Drop a queued message — it went out, or the user discarded it.
    pub(crate) fn delete_outbox(&self, id: u32) {
        let _ = self
            .conn
            .execute("DELETE FROM outbox WHERE id = ?1", params![id]);
    }

    /// Hold a queued message until `at`, with nothing gone wrong.
    pub(crate) fn reschedule_outbox(&self, id: u32, at: i64) {
        let _ = self.conn.execute(
            "UPDATE outbox SET send_at = ?2, last_error = '' WHERE id = ?1",
            params![id, at],
        );
    }

    /// Record another failed attempt, so the list can say what went wrong.
    pub(crate) fn record_outbox_failure(&self, id: u32, error: &str) {
        let _ = self.conn.execute(
            "UPDATE outbox SET attempts = attempts + 1, last_error = ?2 WHERE id = ?1",
            params![id, error],
        );
    }
}
