//! Folders and message summaries: listing, saving, the exact case of
//! Message-IDs, UID bookkeeping and deletion.

use rusqlite::{params, Connection};

use crate::models::{Folder, FolderKind, Message};

use super::{join_labels, message_columns, message_from_row, split_labels, Cache};

/// Message-IDs are stored lowercased so a thread matches however a client
/// spelt an id, but the wire is case-sensitive (RFC 5322): a reply whose
/// In-Reply-To names the parent in the wrong case is a reply to nothing for
/// any server that looks the parent up by its exact id. Proton Bridge does —
/// it files the copy of a reply sent through it without In-Reply-To or
/// References when the lookup misses, so the reply never joined its
/// conversation. Every exact spelling seen while parsing headers is noted
/// here and written to the cache with the next batch of messages, and a
/// message about to be sent gets its ids spelt back the way they arrived
/// ([`Cache::exact_msgids`]). Only ids that actually differ from their
/// lowercase form are kept.
#[cfg(not(test))]
static MSGID_CASE_PENDING: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, String>>,
> = std::sync::OnceLock::new();

#[cfg(not(test))]
pub(super) fn msgid_case_pending(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, String>> {
    MSGID_CASE_PENDING.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Under test, a map per test thread. The tests run side by side, and any
/// of them that parses mail notes into the map and any that saves messages
/// drains it, so a test reading back its own notes (or checking that a
/// batch drained them) could see another test's at any moment. Each test
/// runs on a thread of its own, so its map holds only what it did.
#[cfg(test)]
pub(super) fn msgid_case_pending(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, String>> {
    thread_local! {
        static PENDING: &'static std::sync::Mutex<std::collections::HashMap<String, String>> =
            Box::leak(Box::default());
    }
    PENDING.with(|pending| *pending)
}

/// The most a parse can note before a batch of messages carries it to disk;
/// past this the notes are dropped rather than growing without bound.
const MSGID_CASE_PENDING_CAP: usize = 50_000;

/// Remember how `exact` (one Message-ID, no angle brackets) is spelt, when
/// that differs from the lowercase form the cache stores.
pub(crate) fn note_msgid_case(exact: &str) {
    if exact.is_empty() || !exact.bytes().any(|b| b.is_ascii_uppercase()) {
        return;
    }
    let mut pending = msgid_case_pending()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if pending.len() >= MSGID_CASE_PENDING_CAP {
        return;
    }
    pending
        .entry(exact.to_ascii_lowercase())
        .or_insert_with(|| exact.to_string());
}

/// Compare folder lists ignoring the volatile id, but including unread counts so
/// a changed count re-emits the list and refreshes the sidebar badges.
pub(crate) fn folders_equal(a: &[Folder], b: &[Folder]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.path == y.path && x.name == y.name && x.kind == y.kind && x.unread == y.unread
        })
}

pub(super) fn kind_to_i64(kind: FolderKind) -> i64 {
    match kind {
        FolderKind::Inbox => 0,
        FolderKind::Starred => 1,
        FolderKind::Sent => 2,
        FolderKind::Drafts => 3,
        FolderKind::Archive => 4,
        FolderKind::Junk => 5,
        FolderKind::Trash => 6,
        FolderKind::Custom => 7,
        // After Custom: the numbers are stored, and the older ones keep
        // their meaning.
        FolderKind::Templates => 8,
        FolderKind::Snoozed => 9,
    }
}

pub(super) fn kind_from_i64(v: i64) -> FolderKind {
    match v {
        0 => FolderKind::Inbox,
        1 => FolderKind::Starred,
        2 => FolderKind::Sent,
        3 => FolderKind::Drafts,
        4 => FolderKind::Archive,
        5 => FolderKind::Junk,
        6 => FolderKind::Trash,
        8 => FolderKind::Templates,
        9 => FolderKind::Snoozed,
        _ => FolderKind::Custom,
    }
}

impl Cache {
    /// Every cached copy of a message with this (normalized: no brackets,
    /// lowercase) Message-ID, as `(account_id, folder_path, uid)`, newest
    /// first — a `mid:` link (#130) is resolved from here before any server
    /// is asked.
    pub(crate) fn locate_by_message_id(&self, message_id: &str) -> Vec<(u32, String, u32)> {
        if message_id.is_empty() {
            return Vec::new();
        }
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT account_id, folder_path, uid FROM messages WHERE message_id = ?1 ORDER BY ts DESC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([message_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    pub(crate) fn load_folders(&self, account_id: u32) -> Vec<Folder> {
        let run = || -> rusqlite::Result<Vec<Folder>> {
            let mut stmt = self.conn.prepare(
                "SELECT path, name, kind, unread FROM folders WHERE account_id = ?1 ORDER BY ord",
            )?;
            let rows = stmt.query_map([account_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, u32>(3)?,
                ))
            })?;
            let mut folders = Vec::new();
            for (i, r) in rows.enumerate() {
                let (path, name, kind, unread) = r?;
                folders.push(Folder {
                    id: i as u32 + 1,
                    account_id,
                    name,
                    path,
                    kind: kind_from_i64(kind),
                    unread,
                });
            }
            Ok(folders)
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache load_folders failed: {e}");
            Vec::new()
        })
    }

    pub(crate) fn save_folders(&self, account_id: u32, folders: &[Folder]) {
        let run = || -> rusqlite::Result<()> {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute("DELETE FROM folders WHERE account_id = ?1", [account_id])?;
            for (i, f) in folders.iter().enumerate() {
                tx.execute(
                    "INSERT INTO folders (account_id, path, name, kind, unread, ord)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        account_id,
                        f.path,
                        f.name,
                        kind_to_i64(f.kind),
                        f.unread,
                        i as i64
                    ],
                )?;
            }
            tx.commit()
        };
        if let Err(e) = run() {
            tracing::warn!("cache save_folders failed: {e}");
        }
    }

    /// Every message in an account from `addr`, as (folder path, uid): what
    /// Unsubscribe offers to delete along with leaving the list (#407).
    pub(crate) fn messages_from_address(&self, account_id: u32, addr: &str) -> Vec<(String, u32)> {
        let run = || -> rusqlite::Result<Vec<(String, u32)>> {
            let mut stmt = self
                .conn
                .prepare("SELECT folder_path, uid FROM messages WHERE account_id = ?1 AND lower(from_addr) = lower(?2)")?;
            let rows = stmt.query_map(params![account_id, addr.trim()], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
            rows.collect()
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache messages_from_address failed: {e}");
            Vec::new()
        })
    }

    pub(crate) fn load_messages(
        &self,
        account_id: u32,
        folder_path: &str,
        folder_id: u32,
    ) -> Vec<Message> {
        self.load_folder(account_id, folder_path, folder_id, "ORDER BY uid DESC")
    }

    /// How many messages a folder holds.
    pub(crate) fn count_messages(&self, account_id: u32, folder_path: &str) -> usize {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE account_id = ?1 AND folder_path = ?2",
                params![account_id, folder_path],
                |r| r.get::<_, i64>(0),
            )
            .map_or(0, |n| n as usize)
    }

    /// A folder's `limit` newest messages, newest first: what a list shows
    /// at once while the whole folder is read.
    pub(crate) fn load_newest(
        &self,
        account_id: u32,
        folder_path: &str,
        folder_id: u32,
        limit: usize,
    ) -> Vec<Message> {
        self.load_folder(
            account_id,
            folder_path,
            folder_id,
            &format!("ORDER BY ts DESC LIMIT {limit}"),
        )
    }

    fn load_folder(
        &self,
        account_id: u32,
        folder_path: &str,
        folder_id: u32,
        order: &str,
    ) -> Vec<Message> {
        let run = || -> rusqlite::Result<Vec<Message>> {
            let cols = message_columns();
            let mut stmt = self.conn.prepare(&format!(
                "SELECT {cols}
                 FROM messages WHERE account_id = ?1 AND folder_path = ?2 {order}",
            ))?;
            let rows = stmt.query_map(params![account_id, folder_path], |row| {
                let m = message_from_row(row, 0, account_id, folder_id)?;
                Ok(m)
            })?;
            rows.collect()
        };
        let mut messages = run().unwrap_or_else(|e| {
            tracing::warn!("cache load_messages failed: {e}");
            Vec::new()
        });
        // Correct false "has attachment" flags: a message we've already fetched
        // and found to hold no real attachments (e.g. iCloud marketing mail that
        // is multipart/mixed but only wraps inline `cid:` images) should not show
        // the paperclip, even though its summary flag said otherwise.
        let attachmentless = self.attachmentless_uids(account_id, folder_path);
        if !attachmentless.is_empty() {
            for m in messages.iter_mut() {
                if m.has_attachment && attachmentless.contains(&m.uid) {
                    m.has_attachment = false;
                }
            }
        }
        messages
    }

    /// UIDs whose attachments have been fetched (`attachments_checked`) but which
    /// turned out to hold no real attachments — used to correct false "has
    /// attachment" summary flags.
    pub(crate) fn attachmentless_uids(
        &self,
        account_id: u32,
        folder_path: &str,
    ) -> std::collections::HashSet<u32> {
        let run = || -> rusqlite::Result<std::collections::HashSet<u32>> {
            let mut stmt = self.conn.prepare(
                "SELECT ac.uid FROM attachments_checked ac
                 WHERE ac.account_id = ?1 AND ac.folder_path = ?2
                   AND NOT EXISTS (
                       SELECT 1 FROM attachments a
                       WHERE a.account_id = ac.account_id
                         AND a.folder_path = ac.folder_path
                         AND a.uid = ac.uid
                   )",
            )?;
            let rows =
                stmt.query_map(params![account_id, folder_path], |row| row.get::<_, u32>(0))?;
            rows.collect()
        };
        run().unwrap_or_default()
    }

    pub(crate) fn save_messages(&self, account_id: u32, folder_path: &str, messages: &[Message]) {
        let run = || -> rusqlite::Result<()> {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute(
                "DELETE FROM messages WHERE account_id = ?1 AND folder_path = ?2",
                params![account_id, folder_path],
            )?;
            for m in messages {
                insert_message(&tx, INSERT_MESSAGE, account_id, folder_path, m)?;
            }
            tx.commit()
        };
        if let Err(e) = run() {
            tracing::warn!("cache save_messages failed: {e}");
        }
    }

    /// Insert-or-replace message summaries without clearing the folder first.
    /// Used to grow the search index (fast first page + background backfill)
    /// without wiping already-indexed messages.
    pub(crate) fn upsert_messages(&self, account_id: u32, folder_path: &str, messages: &[Message]) {
        let run = || -> rusqlite::Result<()> {
            let tx = self.conn.unchecked_transaction()?;
            // The exact spellings noted while these (and any earlier) headers
            // were parsed ride along in the same transaction.
            let noted: Vec<(String, String)> = {
                let mut pending = msgid_case_pending()
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                pending.drain().collect()
            };
            for (lower, exact) in &noted {
                tx.execute(
                    "INSERT OR REPLACE INTO msgid_case (lower, exact) VALUES (?1, ?2)",
                    params![lower, exact],
                )?;
            }
            let upsert = format!(
                "{INSERT_MESSAGE} \
                 ON CONFLICT(account_id, folder_path, uid) DO UPDATE SET \
                 from_name = excluded.from_name, \
                 from_addr = excluded.from_addr, \
                 subject = excluded.subject, \
                 date = excluded.date, \
                 ts = excluded.ts, \
                 unread = excluded.unread, \
                 starred = excluded.starred, \
                 has_attachment = excluded.has_attachment, \
                 recipients = excluded.recipients, \
                 cc = excluded.cc, \
                 message_id = excluded.message_id, \
                 references_ = excluded.references_, \
                 preview = CASE WHEN excluded.preview = '' THEN messages.preview ELSE excluded.preview END, \
                 reply_to = excluded.reply_to, \
                 keywords = excluded.keywords, \
                 importance = excluded.importance, \
                 due = excluded.due, \
                 gmail_labels = excluded.gmail_labels, \
                 answered = excluded.answered, \
                 forwarded = excluded.forwarded, \
                 bcc = excluded.bcc"
            );
            for m in messages {
                // Upsert rather than REPLACE so an empty preview cannot erase one
                // already stored: the background backfill re-fetches summaries
                // without asking for a body slice, and every message it touched
                // lost its preview.
                insert_message(&tx, &upsert, account_id, folder_path, m)?;
            }
            tx.commit()
        };
        if let Err(e) = run() {
            tracing::warn!("cache upsert_messages failed: {e}");
        }
    }

    /// The exact spelling of one stored (lowercased) Message-ID, if a header
    /// parsed so far spelt it with capitals; `None` means the lowercase form
    /// is the spelling (or the only one known).
    fn exact_msgid(&self, lower: &str) -> Option<String> {
        if let Some(hit) = msgid_case_pending()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(lower)
        {
            return Some(hit.clone());
        }
        self.conn
            .query_row(
                "SELECT exact FROM msgid_case WHERE lower = ?1",
                params![lower],
                |r| r.get::<_, String>(0),
            )
            .ok()
    }

    /// A space-separated list of stored Message-IDs (In-Reply-To, References)
    /// spelt the way each arrived, for a header about to go on the wire. Ids
    /// never seen with capitals go out as stored.
    pub(crate) fn exact_msgids(&self, ids: &str) -> String {
        ids.split_whitespace()
            .map(|id| self.exact_msgid(id).unwrap_or_else(|| id.to_string()))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The set of message UIDs already cached for a folder (for backfill diffing).
    pub(crate) fn cached_uids(
        &self,
        account_id: u32,
        folder_path: &str,
    ) -> std::collections::HashSet<u32> {
        let run = || -> rusqlite::Result<std::collections::HashSet<u32>> {
            let mut stmt = self
                .conn
                .prepare("SELECT uid FROM messages WHERE account_id = ?1 AND folder_path = ?2")?;
            let rows =
                stmt.query_map(params![account_id, folder_path], |row| row.get::<_, u32>(0))?;
            rows.collect()
        };
        run().unwrap_or_default()
    }

    /// Each cached message's UID with its timestamp, for a folder: what a
    /// Graph or JMAP listing is reconciled against (#429), since a row newer
    /// than the listing itself must not be taken for one deleted.
    pub(crate) fn cached_uid_times(
        &self,
        account_id: u32,
        folder_path: &str,
    ) -> std::collections::HashMap<u32, i64> {
        let run = || -> rusqlite::Result<std::collections::HashMap<u32, i64>> {
            let mut stmt = self.conn.prepare(
                "SELECT uid, ts FROM messages WHERE account_id = ?1 AND folder_path = ?2",
            )?;
            let rows = stmt.query_map(params![account_id, folder_path], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, i64>(1)?))
            })?;
            rows.collect()
        };
        run().unwrap_or_default()
    }

    /// Up to `limit` cached uids in a folder below `below_uid`, newest first.
    pub(crate) fn uids_below(
        &self,
        account_id: u32,
        folder_path: &str,
        below_uid: u32,
        limit: usize,
    ) -> Vec<u32> {
        let run = || -> rusqlite::Result<Vec<u32>> {
            let mut stmt = self.conn.prepare(
                "SELECT uid FROM messages WHERE account_id = ?1 AND folder_path = ?2 AND uid < ?3 \
                 ORDER BY uid DESC LIMIT ?4",
            )?;
            let rows = stmt.query_map(
                params![account_id, folder_path, below_uid, limit as i64],
                |row| row.get(0),
            )?;
            rows.collect()
        };
        run().unwrap_or_default()
    }

    /// Add Gmail labels to one cached message, or take them off (#374): what
    /// tagging on Gmail changed, before the next sync says it again.
    pub(crate) fn set_gmail_label(
        &self,
        account_id: u32,
        folder_path: &str,
        uid: u32,
        labels: &[String],
        add: bool,
    ) {
        let current: String = self
            .conn
            .query_row(
                "SELECT gmail_labels FROM messages WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
                params![account_id, folder_path, uid],
                |r| r.get(0),
            )
            .unwrap_or_default();
        let mut list = split_labels(current);
        list.retain(|l| !labels.iter().any(|x| x.eq_ignore_ascii_case(l)));
        if add {
            list.extend(labels.iter().cloned());
        }
        let _ = self.conn.execute(
            "UPDATE messages SET gmail_labels = ?1 WHERE account_id = ?2 AND folder_path = ?3 AND uid = ?4",
            params![join_labels(&list), account_id, folder_path, uid],
        );
    }

    /// Store the Gmail labels read for these uids; returns how many rows
    /// changed.
    pub(crate) fn set_gmail_labels(
        &self,
        account_id: u32,
        folder_path: &str,
        labels: &std::collections::HashMap<u32, Vec<String>>,
    ) -> usize {
        let run = || -> rusqlite::Result<usize> {
            let tx = self.conn.unchecked_transaction()?;
            let mut changed = 0;
            for (uid, l) in labels {
                changed += tx.execute(
                    "UPDATE messages SET gmail_labels = ?1 \
                     WHERE account_id = ?2 AND folder_path = ?3 AND uid = ?4 AND gmail_labels <> ?1",
                    params![join_labels(l), account_id, folder_path, uid],
                )?;
            }
            tx.commit()?;
            Ok(changed)
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache set_gmail_labels failed: {e}");
            0
        })
    }

    /// Gmail files one message under every label it carries, and moving it
    /// to Trash or Spam takes all of them off: the copies cached under All
    /// Mail, Important and the rest are gone from the server too, but only
    /// the folder the move started in is ever told. Left behind, they put
    /// the conversation back together, as blank cards, the next time a
    /// reply arrived (#257). Drops every cached copy of the messages at
    /// `uids` in `from_path`, found by Message-ID, except the one in
    /// `keep_path` (where they went) and the rows in `from_path` itself,
    /// which the caller removes. Returns how many were dropped.
    pub(crate) fn drop_label_copies(
        &self,
        account_id: u32,
        from_path: &str,
        uids: &[u32],
        keep_path: &str,
    ) -> usize {
        let mut copies: Vec<(String, u32)> = Vec::new();
        for uid in uids {
            let msgid: Option<String> = self
                .conn
                .query_row(
                    "SELECT message_id FROM messages WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
                    params![account_id, from_path, uid],
                    |row| row.get(0),
                )
                .ok();
            let Some(msgid) = msgid.filter(|m| !m.is_empty()) else {
                continue;
            };
            let Ok(mut stmt) = self.conn.prepare(
                "SELECT folder_path, uid FROM messages \
                 WHERE account_id = ?1 AND message_id = ?2 AND folder_path <> ?3 AND folder_path <> ?4",
            ) else {
                continue;
            };
            let found: Vec<(String, u32)> = stmt
                .query_map(params![account_id, msgid, from_path, keep_path], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
                })
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default();
            copies.extend(found);
        }
        for (path, uid) in &copies {
            self.delete_message(account_id, path, *uid);
        }
        copies.len()
    }

    pub(crate) fn delete_message(&self, account_id: u32, folder_path: &str, uid: u32) {
        for table in ["messages", "bodies", "attachments", "attachments_checked"] {
            let _ = self.conn.execute(
                &format!(
                    "DELETE FROM {table} WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3"
                ),
                params![account_id, folder_path, uid],
            );
        }
    }
}

/// Writes every summary column of a message, `?1` to `?24` in the order
/// [`insert_message`] binds them.
const INSERT_MESSAGE: &str = "INSERT INTO messages \
     (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, \
     has_attachment, recipients, cc, message_id, references_, preview, reply_to, keywords, \
     importance, due, gmail_labels, answered, forwarded, bcc) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, \
     ?19, ?20, ?21, ?22, ?23, ?24)";

/// Runs `sql`, an [`INSERT_MESSAGE`] (with or without a conflict clause),
/// for one message.
fn insert_message(
    conn: &Connection,
    sql: &str,
    account_id: u32,
    folder_path: &str,
    m: &Message,
) -> rusqlite::Result<usize> {
    conn.execute(
        sql,
        params![
            account_id,
            folder_path,
            m.uid,
            m.from_name,
            m.from_addr,
            m.subject,
            m.date,
            m.timestamp,
            m.unread,
            m.starred,
            m.has_attachment,
            m.to,
            m.cc,
            m.message_id,
            m.references,
            m.preview,
            m.reply_to,
            m.keywords.join(" "),
            m.importance.to_i64(),
            m.due,
            join_labels(&m.labels),
            m.answered,
            m.forwarded,
            m.bcc
        ],
    )
}
