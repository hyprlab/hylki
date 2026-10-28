//! Mail kept offline (#536): which messages of an account's chosen period
//! are not on disk yet, and sparing a period when old downloads are removed.

use rusqlite::params_from_iter;
use rusqlite::types::Value;

use super::schema::DOWNLOADED_TABLES;
use super::Cache;

/// Whether `table` holds a row for message `m` or another copy of it: Gmail
/// files one message under every label, and one download serves them all.
/// The copies are found by Message-ID; left to itself, SQLite searched them
/// by account, a scan of the mailbox for every message, minutes on 100,000.
fn kept_in(table: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM {table} t WHERE t.account_id = m.account_id \
           AND t.folder_path = m.folder_path AND t.uid = m.uid) \
         OR (m.message_id != '' AND EXISTS (SELECT 1 FROM messages s \
           INDEXED BY messages_by_message_id JOIN {table} t \
           ON t.account_id = s.account_id AND t.folder_path = s.folder_path AND t.uid = s.uid \
           WHERE s.message_id = m.message_id AND s.account_id = m.account_id \
           AND s.from_addr = m.from_addr AND s.ts = m.ts)))"
    )
}

/// A candidate row: folder path, uid, and the Message-ID, sender and date
/// that tell copies of one message apart from different messages.
type Candidate = (String, u32, String, String, i64);

impl Cache {
    /// Up to `limit` messages of an account dated `since` or later that are
    /// not kept yet, newest first, as (folder path, uid): no body on disk,
    /// or, with `attachments`, a paperclip whose files were never fetched.
    /// Folders in `skip` are left out, and of several copies of one message
    /// only the first is listed.
    pub(crate) fn offline_missing(
        &self,
        account_id: u32,
        since: i64,
        attachments: bool,
        skip: &[String],
        limit: usize,
    ) -> Vec<(String, u32)> {
        let missing = if attachments {
            format!(
                "(NOT {} OR (m.has_attachment = 1 AND NOT {}))",
                kept_in("bodies"),
                kept_in("attachments_checked")
            )
        } else {
            format!("NOT {}", kept_in("bodies"))
        };
        let skipped = if skip.is_empty() {
            String::new()
        } else {
            format!(
                " AND m.folder_path NOT IN ({})",
                vec!["?"; skip.len()].join(", ")
            )
        };
        let sql = format!(
            "SELECT m.folder_path, m.uid, m.message_id, m.from_addr, m.ts FROM messages m \
             WHERE m.account_id = ? AND m.ts >= ?{skipped} AND {missing} \
             ORDER BY m.ts DESC LIMIT ?"
        );
        let mut args: Vec<Value> = vec![Value::from(i64::from(account_id)), Value::from(since)];
        args.extend(skip.iter().map(|p| Value::from(p.clone())));
        args.push(Value::from(limit as i64));
        let run = || -> rusqlite::Result<Vec<Candidate>> {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(args), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?;
            rows.collect()
        };
        let rows = run().unwrap_or_else(|e| {
            tracing::warn!("cache offline_missing failed: {e}");
            Vec::new()
        });
        let mut seen = std::collections::HashSet::new();
        rows.into_iter()
            .filter(|(_, _, id, from, ts)| {
                id.is_empty() || seen.insert((id.clone(), from.clone(), *ts))
            })
            .map(|(path, uid, ..)| (path, uid))
            .collect()
    }

    /// [`Cache::remove_downloaded`] before `before` (a Unix time), but an
    /// account in `keep` keeps its downloads back to its own date: a period
    /// kept offline is not removed for being old, only to be downloaded
    /// again. Returns how many bodies went.
    pub(crate) fn remove_downloaded_sparing(
        &self,
        before: i64,
        keep: &[(u32, i64)],
    ) -> rusqlite::Result<usize> {
        // Each account's own cut-off, the older of the two dates.
        let mut cutoff = String::from("CASE m.account_id");
        let mut args: Vec<Value> = Vec::new();
        for (account_id, since) in keep {
            cutoff.push_str(" WHEN ? THEN ?");
            args.push(Value::from(i64::from(*account_id)));
            args.push(Value::from((*since).min(before)));
        }
        cutoff.push_str(" ELSE ? END");
        args.push(Value::from(before));
        let tx = self.conn.unchecked_transaction()?;
        let mut removed = 0;
        for table in DOWNLOADED_TABLES {
            let n = tx.execute(
                &format!(
                    "DELETE FROM {table} WHERE EXISTS (SELECT 1 FROM messages m \
                     WHERE m.account_id = {table}.account_id AND m.folder_path = {table}.folder_path \
                     AND m.uid = {table}.uid AND m.ts < {cutoff})"
                ),
                params_from_iter(args.iter()),
            )?;
            if table == "bodies" {
                removed = n;
            }
        }
        tx.commit()?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::add_msg;
    use super::*;

    fn set_id(c: &Cache, folder: &str, uid: u32, id: &str) {
        c.conn
            .execute(
                "UPDATE messages SET message_id = ?1 WHERE folder_path = ?2 AND uid = ?3",
                rusqlite::params![id, folder, uid],
            )
            .unwrap();
    }

    #[test]
    fn lists_what_the_period_still_lacks() {
        let c = Cache::in_memory().unwrap();
        add_msg(&c, "INBOX", 1, "a", "old", 100);
        add_msg(&c, "INBOX", 2, "a", "kept", 300);
        add_msg(&c, "INBOX", 3, "a", "new", 400);
        add_msg(&c, "Trash", 4, "a", "binned", 500);
        c.save_body(1, "INBOX", 2, "<p>kept</p>");
        let skip = vec!["Trash".to_string()];
        assert_eq!(
            c.offline_missing(1, 200, false, &skip, 10),
            vec![("INBOX".to_string(), 3)]
        );
        assert_eq!(c.offline_missing(1, i64::MIN, false, &skip, 10).len(), 2);
        // The test rows all carry a paperclip: with attachments asked for,
        // the kept body without its files is listed again.
        assert_eq!(c.offline_missing(1, 200, true, &skip, 10).len(), 2);
        c.mark_attachments_checked(1, "INBOX", 2);
        assert_eq!(c.offline_missing(1, 200, true, &skip, 10).len(), 1);
    }

    #[test]
    fn one_download_serves_every_copy() {
        let c = Cache::in_memory().unwrap();
        add_msg(&c, "INBOX", 1, "a", "s", 400);
        add_msg(&c, "[Gmail]/All Mail", 9, "a", "s", 400);
        set_id(&c, "INBOX", 1, "<x@y>");
        set_id(&c, "[Gmail]/All Mail", 9, "<x@y>");
        assert_eq!(c.offline_missing(1, 0, false, &[], 10).len(), 1);
        c.save_body(1, "[Gmail]/All Mail", 9, "<p>s</p>");
        assert!(c.offline_missing(1, 0, false, &[], 10).is_empty());
    }

    #[test]
    fn removal_spares_a_period_kept_offline() {
        let c = Cache::in_memory().unwrap();
        add_msg(&c, "INBOX", 1, "a", "old", 100);
        add_msg(&c, "INBOX", 2, "a", "older", 50);
        c.save_body(1, "INBOX", 1, "<p>old</p>");
        c.save_body(1, "INBOX", 2, "<p>older</p>");
        // Account 1 keeps everything since 80: only the message from 50 goes.
        assert_eq!(c.remove_downloaded_sparing(200, &[(1, 80)]).unwrap(), 1);
        assert!(c.load_body(1, "INBOX", 1).is_some());
        // Kept all along: nothing goes.
        assert_eq!(
            c.remove_downloaded_sparing(200, &[(1, i64::MIN)]).unwrap(),
            0
        );
        // Another account's period spares nothing here.
        assert_eq!(
            c.remove_downloaded_sparing(200, &[(2, i64::MIN)]).unwrap(),
            1
        );
    }
}
