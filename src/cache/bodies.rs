//! What is downloaded per message: rendered bodies, sender checks,
//! attachment bytes, and the space they take.

use rusqlite::params;

use crate::models::Attachment;

use super::schema::DOWNLOADED_TABLES;
use super::Cache;

impl Cache {
    /// What downloaded mail takes up (#394), in bytes: the message bodies
    /// and the attachment files. The index of every message, which the list
    /// and search read, is not counted: it stays whatever is removed.
    pub(crate) fn downloaded_size(&self) -> (u64, u64) {
        let sum = |sql: &str| -> u64 {
            self.conn
                .query_row(sql, [], |r| r.get::<_, i64>(0))
                .unwrap_or(0)
                .max(0) as u64
        };
        (
            sum("SELECT COALESCE(SUM(length(body)), 0) FROM bodies"),
            sum("SELECT COALESCE(SUM(length(data)), 0) FROM attachments"),
        )
    }

    /// Remove downloaded mail (#394): the bodies and attachment files of
    /// messages older than `before` (a Unix time), or of every message with
    /// `None`. Opening one fetches it again. The message index stays, so the
    /// list and search are as they were. Returns how many bodies went.
    pub(crate) fn remove_downloaded(&self, before: Option<i64>) -> rusqlite::Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut removed = 0;
        for table in DOWNLOADED_TABLES {
            let n = match before {
                None => tx.execute(&format!("DELETE FROM {table}"), [])?,
                Some(ts) => tx.execute(
                    &format!(
                        "DELETE FROM {table} WHERE EXISTS (SELECT 1 FROM messages m \
                         WHERE m.account_id = {table}.account_id AND m.folder_path = {table}.folder_path \
                         AND m.uid = {table}.uid AND m.ts < ?1)"
                    ),
                    [ts],
                )?,
            };
            if table == "bodies" {
                removed = n;
            }
        }
        tx.commit()?;
        Ok(removed)
    }

    /// How big the cache is on disk, with its write-ahead log, in bytes.
    pub(crate) fn file_size() -> u64 {
        let Some(dir) = crate::config::data_base().map(|d| d.join("hylki")) else {
            return 0;
        };
        ["cache.db", "cache.db-wal"]
            .iter()
            .filter_map(|f| std::fs::metadata(dir.join(f)).ok())
            .map(|m| m.len())
            .sum()
    }

    /// Give the space freed by `remove_downloaded` back to the disk. Fails
    /// harmlessly while a worker is writing; the space is then reused.
    pub(crate) fn compact(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("VACUUM")
    }

    pub(crate) fn load_body(&self, account_id: u32, folder_path: &str, uid: u32) -> Option<String> {
        self.body_of(account_id, folder_path, uid).or_else(|| {
            self.sibling_copies(account_id, folder_path, uid)
                .into_iter()
                .find_map(|(p, u)| self.body_of(account_id, &p, u))
        })
    }

    fn body_of(&self, account_id: u32, folder_path: &str, uid: u32) -> Option<String> {
        self.conn
            .query_row(
                "SELECT body FROM bodies WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
                params![account_id, folder_path, uid],
                |row| row.get::<_, String>(0),
            )
            .ok()
    }

    /// Every other copy of this message in the account, as `(folder_path, uid)`.
    ///
    /// Gmail exposes labels as IMAP folders, so one message is stored several
    /// times over — INBOX, All Mail, Important — under a different UID in each.
    /// Everything keyed by (folder, uid) is therefore per-*copy*: a body read
    /// while the user was in All Mail is invisible to the INBOX copy, and its
    /// attachments would be downloaded a second time. They are the same mail, so
    /// a miss on one copy is answered from another instead of from the network.
    /// On a real cache 176 messages have a body under only some of their labels,
    /// and 31 have their attachments downloaded under only some.
    ///
    /// Copies must agree on sender and timestamp as well as Message-ID. The id is
    /// supposed to be unique, but it is written by whoever sent the mail, and
    /// spam and some list software reuse one across genuinely different messages
    /// — where serving one message's body for another would be worse than the
    /// cache miss this avoids. Gmail's own copies agree on all three (verified
    /// across a real 8300-message cache: no Message-ID there covers two
    /// different senders, subjects or timestamps), so the fallback still fires
    /// where it was meant to. Subject is deliberately not compared: a re-decoded
    /// encoded-word can rewrite it under one label and not another.
    fn sibling_copies(&self, account_id: u32, folder_path: &str, uid: u32) -> Vec<(String, u32)> {
        let run = || -> rusqlite::Result<Vec<(String, u32)>> {
            let mut stmt = self.conn.prepare(
                "SELECT m.folder_path, m.uid FROM messages m \
                 JOIN messages orig ON orig.account_id = ?1 \
                                   AND orig.folder_path = ?2 AND orig.uid = ?3 \
                 WHERE m.account_id = ?1 AND m.message_id != '' \
                   AND m.message_id = orig.message_id \
                   AND m.from_addr = orig.from_addr \
                   AND m.ts = orig.ts \
                   AND NOT (m.folder_path = ?2 AND m.uid = ?3)",
            )?;
            let rows = stmt.query_map(params![account_id, folder_path, uid], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
            rows.collect()
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache sibling_copies failed: {e}");
            Vec::new()
        })
    }

    /// Forget a message's rendered body, so the next open renders it
    /// afresh from the server's copy.
    pub(crate) fn delete_body(&self, account_id: u32, folder_path: &str, uid: u32) {
        let _ = self.conn.execute(
            "DELETE FROM bodies WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
            params![account_id, folder_path, uid],
        );
    }

    /// Store a message's rendered body, and its words for search (#509).
    pub(crate) fn save_body(&self, account_id: u32, folder_path: &str, uid: u32, body: &str) {
        let run = || -> rusqlite::Result<()> {
            let tx = self.conn.unchecked_transaction()?;
            // A replaced row is deleted without its trigger firing, so the
            // old words go here.
            if let Some(old) = self.body_rowid(account_id, folder_path, uid) {
                tx.execute("DELETE FROM body_text WHERE rowid = ?1", [old])?;
            }
            tx.execute(
                "INSERT OR REPLACE INTO bodies (account_id, folder_path, uid, body) VALUES (?1, ?2, ?3, ?4)",
                params![account_id, folder_path, uid, body],
            )?;
            self.index_body(tx.last_insert_rowid(), body)?;
            tx.commit()
        };
        if let Err(e) = run() {
            tracing::warn!("cache save_body failed: {e}");
        }
    }

    /// The cached sender-authentication verdict for a message, if one was stored
    /// when its body was fetched.
    pub(crate) fn load_sender_check(
        &self,
        account_id: u32,
        folder_path: &str,
        uid: u32,
    ) -> Option<crate::models::SenderCheck> {
        self.sender_check_of(account_id, folder_path, uid)
            .or_else(|| {
                self.sibling_copies(account_id, folder_path, uid)
                    .into_iter()
                    .find_map(|(p, u)| self.sender_check_of(account_id, &p, u))
            })
    }

    fn sender_check_of(
        &self,
        account_id: u32,
        folder_path: &str,
        uid: u32,
    ) -> Option<crate::models::SenderCheck> {
        self.conn
            .query_row(
                "SELECT trust, summary, findings, pgp, unsubscribe, invite, label, receipt, authority, from_domain FROM sender_checks \
                 WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
                params![account_id, folder_path, uid],
                |row| {
                    Ok(crate::models::SenderCheck {
                        trust: crate::models::SenderTrust::from_tag(&row.get::<_, String>(0)?),
                        summary: row.get(1)?,
                        findings: row
                            .get::<_, String>(2)?
                            .lines()
                            .filter(|l| !l.is_empty())
                            .map(str::to_string)
                            .collect(),
                        // The stored verdict is a marker only (see the
                        // cleanup in `open`): a served verdict must be fresh.
                        pgp: None,
                        unsubscribe: serde_json::from_str(&row.get::<_, String>(4)?).ok(),
                        invite: serde_json::from_str(&row.get::<_, String>(5)?).ok(),
                        label: serde_json::from_str(&row.get::<_, String>(6)?).ok(),
                        receipt: serde_json::from_str(&row.get::<_, String>(7)?).ok(),
                        read: None,
                        authority: row.get(8)?,
                        from_domain: row.get(9)?,
                    })
                },
            )
            .ok()
    }

    pub(crate) fn save_sender_check(
        &self,
        account_id: u32,
        folder_path: &str,
        uid: u32,
        check: &crate::models::SenderCheck,
    ) {
        if let Err(e) = self.conn.execute(
            "INSERT OR REPLACE INTO sender_checks \
             (account_id, folder_path, uid, trust, summary, findings, pgp, unsubscribe, invite, label, receipt, authority, from_domain) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                account_id,
                folder_path,
                uid,
                check.trust.as_tag(),
                check.summary,
                check.findings.join("\n"),
                check
                    .pgp
                    .as_ref()
                    .and_then(|p| serde_json::to_string(p).ok())
                    .unwrap_or_default(),
                check
                    .unsubscribe
                    .as_ref()
                    .and_then(|u| serde_json::to_string(u).ok())
                    .unwrap_or_default(),
                check
                    .invite
                    .as_ref()
                    .and_then(|i| serde_json::to_string(i).ok())
                    .unwrap_or_default(),
                check
                    .label
                    .as_ref()
                    .and_then(|l| serde_json::to_string(l).ok())
                    .unwrap_or_default(),
                check
                    .receipt
                    .as_ref()
                    .and_then(|r| serde_json::to_string(r).ok())
                    .unwrap_or_default(),
                check.authority,
                check.from_domain
            ],
        ) {
            tracing::warn!("cache save_sender_check failed: {e}");
        }
    }

    pub(crate) fn load_attachments(
        &self,
        account_id: u32,
        folder_path: &str,
        uid: u32,
    ) -> Vec<Attachment> {
        let own = self.attachments_of(account_id, folder_path, uid);
        if !own.is_empty() {
            return own;
        }
        // Downloaded under another of this message's labels: same mail, same
        // attachments, no reason to fetch them again.
        self.sibling_copies(account_id, folder_path, uid)
            .into_iter()
            .map(|(p, u)| self.attachments_of(account_id, &p, u))
            .find(|items| !items.is_empty())
            .unwrap_or_default()
    }

    fn attachments_of(&self, account_id: u32, folder_path: &str, uid: u32) -> Vec<Attachment> {
        let run = || -> rusqlite::Result<Vec<Attachment>> {
            let mut stmt = self.conn.prepare(
                "SELECT name, data FROM attachments
                 WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3 ORDER BY idx",
            )?;
            let rows = stmt.query_map(params![account_id, folder_path, uid], |row| {
                Ok(Attachment {
                    name: row.get(0)?,
                    data: row.get(1)?,
                })
            })?;
            rows.collect()
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache load_attachments failed: {e}");
            Vec::new()
        })
    }

    pub(crate) fn save_attachments(
        &self,
        account_id: u32,
        folder_path: &str,
        uid: u32,
        items: &[Attachment],
    ) {
        let run = || -> rusqlite::Result<()> {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute(
                "DELETE FROM attachments WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
                params![account_id, folder_path, uid],
            )?;
            for (i, a) in items.iter().enumerate() {
                tx.execute(
                    "INSERT INTO attachments (account_id, folder_path, uid, idx, name, data)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![account_id, folder_path, uid, i as i64, a.name, a.data],
                )?;
            }
            tx.commit()
        };
        if let Err(e) = run() {
            tracing::warn!("cache save_attachments failed: {e}");
        }
    }

    /// Mark a message's attachments as fetched (even if it turned out to have
    /// none), so the background prefetch never re-downloads it to re-check.
    pub(crate) fn mark_attachments_checked(&self, account_id: u32, folder_path: &str, uid: u32) {
        let _ = self.conn.execute(
            "INSERT OR IGNORE INTO attachments_checked (account_id, folder_path, uid) VALUES (?1, ?2, ?3)",
            params![account_id, folder_path, uid],
        );
    }

    /// Whether a message's attachments have already been fetched/checked.
    ///
    /// Gmail shows one message under every label it carries, so the same mail
    /// arrives as INBOX + a label + All Mail. [`load_attachments`] already reads
    /// across those copies; without the same reach here the prefetch treats each
    /// label as unfetched and downloads the message once per label, storing a
    /// full second and third copy of blobs it can already answer from.
    ///
    /// [`load_attachments`]: Self::load_attachments
    pub(crate) fn attachments_checked(&self, account_id: u32, folder_path: &str, uid: u32) -> bool {
        if self.checked_of(account_id, folder_path, uid) {
            return true;
        }
        self.sibling_copies(account_id, folder_path, uid)
            .into_iter()
            .any(|(p, u)| self.checked_of(account_id, &p, u))
    }

    fn checked_of(&self, account_id: u32, folder_path: &str, uid: u32) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM attachments_checked WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
                params![account_id, folder_path, uid],
                |_| Ok(()),
            )
            .is_ok()
    }
}
