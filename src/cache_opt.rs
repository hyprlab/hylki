//! SQLite read concurrency and query folding for `cache.rs`.
//!
//! These are the functions to call from `cache.rs`, not a replacement for it.
//! Two jobs: make reads never block a checkpoint, and stop the per-page extra
//! query that `load_messages` used to issue for attachment flags.

use rusqlite::{params, Connection, Result};

// ---------------------------------------------------------------------------
// Pragmas
// ---------------------------------------------------------------------------

/// Extra writer pragmas. Safe to run anywhere after `Connection::open`; the
/// existing `journal_mode = WAL` in `Cache::open` stays where it is.
pub fn configure_sqlite_engine(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA synchronous = NORMAL;
         PRAGMA mmap_size = 268435456;
         PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -64000;
         PRAGMA wal_autocheckpoint = 1000;",
    )
}

/// Reader-side pragmas for a read-only WAL connection. Call from
/// `Cache::read_connection`, right after the `busy_timeout`.
pub fn tune_read(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA mmap_size = 268435456;
         PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -16000;
         PRAGMA query_only = ON;",
    )
}

/// Run on idle, not after every write.
pub fn optimize(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA optimize;")
}

// ---------------------------------------------------------------------------
// Folded page query
// ---------------------------------------------------------------------------

/// One row of the message list, with the paperclip already corrected.
pub struct PageRow {
    pub rowid: i64,
    pub uid: u32,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub preview: String,
    pub date: String,
    pub ts: i64,
    pub unread: bool,
    pub starred: bool,
    pub has_attachment: bool,
    pub message_id: String,
    pub references: String,
    pub reply_to: String,
    pub to: String,
    pub cc: String,
    pub keywords: Vec<String>,
    pub importance: crate::models::Importance,
    pub due: i64,
    pub labels: Vec<String>,
}

/// Replaces `load_messages` plus its separate `attachmentless_uids` query with
/// one statement.
///
/// The paperclip rule is the one `attachmentless_uids` implements: a message
/// that has been fetched and found to hold nothing must not wear a paperclip,
/// even though its summary flag said otherwise. That is `attachments_checked`
/// present AND no row in `attachments` — *not* `attachment_meta`, which also
/// has a row for an attachment that was only ever described.
///
/// The table is aliased `messages` throughout because `cache::KEYWORDS_COL`
/// names it literally.
pub fn load_messages_page(
    conn: &Connection,
    account_id: u32,
    folder_path: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<PageRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT messages.rowid, messages.uid, messages.subject, messages.from_name,
                messages.from_addr, messages.preview, messages.date, messages.ts,
                messages.unread, messages.starred,
                CASE
                  WHEN messages.has_attachment = 1
                   AND EXISTS (SELECT 1 FROM attachments_checked ac
                               WHERE ac.account_id  = messages.account_id
                                 AND ac.folder_path = messages.folder_path
                                 AND ac.uid         = messages.uid)
                   AND NOT EXISTS (SELECT 1 FROM attachments a
                               WHERE a.account_id  = messages.account_id
                                 AND a.folder_path = messages.folder_path
                                 AND a.uid         = messages.uid)
                  THEN 0 ELSE messages.has_attachment
                END AS has_attachment,
                messages.message_id, messages.references_, messages.reply_to,
                messages.recipients, messages.cc,
                messages.keywords, messages.importance, messages.due, messages.gmail_labels
         FROM messages
         WHERE messages.account_id = ?1 AND messages.folder_path = ?2
         ORDER BY messages.uid DESC
         LIMIT ?3 OFFSET ?4",
    )?;

    let rows = stmt.query_map(
        params![account_id, folder_path, limit as i64, offset as i64],
        |row| {
            Ok(PageRow {
                rowid: row.get(0)?,
                uid: row.get(1)?,
                subject: row.get(2)?,
                from_name: row.get(3)?,
                from_addr: row.get(4)?,
                preview: row.get(5)?,
                date: row.get(6)?,
                ts: row.get(7)?,
                unread: row.get::<_, i64>(8)? != 0,
                starred: row.get::<_, i64>(9)? != 0,
                has_attachment: row.get::<_, i64>(10)? != 0,
                message_id: row.get(11)?,
                references: row.get(12)?,
                reply_to: row.get(13)?,
                to: row.get(14)?,
                cc: row.get(15)?,
                keywords: crate::cache::split_keywords(row.get(16)?),
                importance: crate::models::Importance::from_i64(row.get(17)?),
                due: row.get(18)?,
                labels: crate::cache::split_labels(row.get(19)?),
            })
        },
    )?;

    rows.collect()
}

/// Turn a `PageRow` into a `Message`, with the NUL scrub `load_messages` does.
/// `folder_id` is the caller's to fill, since the page query does not know it.
pub fn page_row_into_message(row: PageRow, account_id: u32, folder_id: u32) -> crate::models::Message {
    let mut m = crate::models::Message {
        id: row.uid,
        account_id,
        folder_id,
        uid: row.uid,
        from_name: row.from_name,
        from_addr: row.from_addr,
        reply_to: row.reply_to,
        to: row.to,
        cc: row.cc,
        subject: row.subject,
        preview: row.preview,
        body: String::new(),
        date: row.date,
        timestamp: row.ts,
        unread: row.unread,
        starred: row.starred,
        keywords: row.keywords,
        has_attachment: row.has_attachment,
        message_id: row.message_id,
        references: row.references,
        importance: row.importance,
        due: row.due,
        labels: row.labels,
    };
    m.scrub_nuls();
    m
}

// ---------------------------------------------------------------------------
// thread_members (optional step — see the integration notes)
// ---------------------------------------------------------------------------

pub fn ensure_thread_members(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS thread_members (
             account_id  INTEGER NOT NULL,
             thread_key  TEXT    NOT NULL,
             message_id  TEXT    NOT NULL,
             folder_path TEXT    NOT NULL,
             uid         INTEGER NOT NULL,
             ts          INTEGER NOT NULL,
             from_name   TEXT    NOT NULL DEFAULT '',
             from_addr   TEXT    NOT NULL DEFAULT '',
             preview     TEXT    NOT NULL DEFAULT '',
             PRIMARY KEY (account_id, thread_key, message_id, folder_path, uid)
         );
         CREATE INDEX IF NOT EXISTS thread_members_by_key
             ON thread_members (account_id, thread_key, ts DESC);",
    )
}

/// The conversation key: the root of the References chain when there is one,
/// else the message's own id. Matches what `thread_member_rowids` matches on.
pub fn thread_key(message_id: &str, references: &str) -> String {
    references
        .split_whitespace()
        .next()
        .filter(|r| !r.is_empty())
        .unwrap_or(message_id)
        .to_string()
}

#[allow(clippy::too_many_arguments)]
pub fn upsert_thread_member(
    conn: &Connection,
    account_id: u32,
    key: &str,
    message_id: &str,
    folder_path: &str,
    uid: u32,
    ts: i64,
    from_name: &str,
    from_addr: &str,
    preview: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO thread_members
             (account_id, thread_key, message_id, folder_path, uid, ts, from_name, from_addr, preview)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![account_id, key, message_id, folder_path, uid, ts, from_name, from_addr, preview],
    )?;
    Ok(())
}

/// Badge count for one conversation, excluding Trash, Junk and Templates —
/// the same exclusions `thread_summaries` applies.
pub fn thread_count(conn: &Connection, account_id: u32, key: &str) -> Result<u32> {
    conn.query_row(
        "SELECT COUNT(DISTINCT message_id) FROM thread_members
         WHERE account_id = ?1 AND thread_key = ?2
           AND folder_path NOT IN ('Trash', 'Junk', 'Templates')",
        params![account_id, key],
        |r| r.get(0),
    )
}

/// Populate `thread_members` from `messages` for one account. Run once after
/// the table is created; `claim_accounts` must learn the table first.
pub fn backfill_thread_members(conn: &Connection, account_id: u32) -> Result<usize> {
    let rows: Vec<(String, String, String, u32, i64, String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT message_id, references_, folder_path, uid, ts, from_name, from_addr, preview
             FROM messages WHERE account_id = ?1",
        )?;
        let mapped = stmt.query_map([account_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))
        })?;
        mapped.collect::<Result<_>>()?
    };
    let tx = conn.unchecked_transaction()?;
    let mut n = 0;
    for (message_id, references, folder_path, uid, ts, from_name, from_addr, preview) in rows {
        if message_id.is_empty() {
            continue;
        }
        let key = thread_key(&message_id, &references);
        upsert_thread_member(
            &tx, account_id, &key, &message_id, &folder_path, uid, ts, &from_name, &from_addr, &preview,
        )?;
        n += 1;
    }
    tx.commit()?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE messages (
                 account_id INTEGER NOT NULL, folder_path TEXT NOT NULL, uid INTEGER NOT NULL,
                 from_name TEXT NOT NULL, from_addr TEXT NOT NULL, recipients TEXT NOT NULL DEFAULT '',
                 cc TEXT NOT NULL DEFAULT '', subject TEXT NOT NULL, date TEXT NOT NULL,
                 ts INTEGER NOT NULL, unread INTEGER NOT NULL, starred INTEGER NOT NULL,
                 has_attachment INTEGER NOT NULL, message_id TEXT NOT NULL DEFAULT '',
                 references_ TEXT NOT NULL DEFAULT '', preview TEXT NOT NULL DEFAULT '',
                 reply_to TEXT NOT NULL DEFAULT '', keywords TEXT NOT NULL DEFAULT '',
                 importance INTEGER NOT NULL DEFAULT 0, due INTEGER NOT NULL DEFAULT 0,
                 gmail_labels TEXT NOT NULL DEFAULT '',
                 PRIMARY KEY (account_id, folder_path, uid));
             CREATE TABLE attachments (
                 account_id INTEGER NOT NULL, folder_path TEXT NOT NULL, uid INTEGER NOT NULL,
                 idx INTEGER NOT NULL, name TEXT NOT NULL, data BLOB NOT NULL,
                 PRIMARY KEY (account_id, folder_path, uid, idx));
             CREATE TABLE attachments_checked (
                 account_id INTEGER NOT NULL, folder_path TEXT NOT NULL, uid INTEGER NOT NULL,
                 PRIMARY KEY (account_id, folder_path, uid));",
        )
        .unwrap();
        conn
    }

    fn add(conn: &Connection, uid: u32, flagged: bool) {
        conn.execute(
            "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject,
                 date, ts, unread, starred, has_attachment)
             VALUES (1, 'INBOX', ?1, 'F', 'f@x', 'S', '', ?1, 0, 0, ?2)",
            params![uid, flagged as i64],
        )
        .unwrap();
    }

    /// A paperclip survives unless the message was fetched and found empty.
    #[test]
    fn the_paperclip_only_clears_when_checked_and_empty() {
        let conn = fixture();
        add(&conn, 1, true); // checked, empty -> cleared
        add(&conn, 2, true); // checked, holds bytes -> kept
        add(&conn, 3, true); // never checked -> kept
        conn.execute("INSERT INTO attachments_checked VALUES (1, 'INBOX', 1)", []).unwrap();
        conn.execute("INSERT INTO attachments_checked VALUES (1, 'INBOX', 2)", []).unwrap();
        conn.execute(
            "INSERT INTO attachments VALUES (1, 'INBOX', 2, 0, 'a.pdf', x'00')",
            [],
        )
        .unwrap();

        let page = load_messages_page(&conn, 1, "INBOX", 10, 0).unwrap();
        let flag = |uid: u32| page.iter().find(|r| r.uid == uid).unwrap().has_attachment;
        assert!(!flag(1), "fetched and empty");
        assert!(flag(2), "fetched and holds a file");
        assert!(flag(3), "never fetched");
    }

    #[test]
    fn page_is_newest_uid_first() {
        let conn = fixture();
        for uid in [1u32, 5, 3] {
            add(&conn, uid, false);
        }
        let page = load_messages_page(&conn, 1, "INBOX", 10, 0).unwrap();
        assert_eq!(page.iter().map(|r| r.uid).collect::<Vec<_>>(), vec![5, 3, 1]);
    }

    #[test]
    fn thread_key_prefers_the_root_reference() {
        assert_eq!(thread_key("<c@x>", "<a@x> <b@x>"), "<a@x>");
        assert_eq!(thread_key("<c@x>", ""), "<c@x>");
    }
}
