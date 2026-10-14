//! The local full-text index behind the search field.
//!
//! The list's own rows carry a short preview (Graph and JMAP keep 200
//! characters, IMAP 240), which is a display line rather than something to
//! search: a word past it was never found. This index holds, per message, the
//! headers the search field names (subject, sender, To, Cc, Reply-To), the
//! whole text of a body that is stored locally, and the names and text of the
//! attachments stored locally. It knows nothing the cache does not already
//! hold, so nothing is fetched for it, and OpenPGP mail, whose body and
//! attachments are never cached, contributes its headers only.
//!
//! FTS5 holds the words and not the text (`content=''`): the text is in the
//! cache already, and a second copy of every body would double the database.
//! Each message has a row in `search_docs` whose `docid` is its rowid in the
//! index. Triggers on `messages`, `bodies`, `attachments` and
//! `attachment_meta` mark a row out of date (`dirty` counts up) or remove it,
//! whichever code path changed the cache, and [`Cache::index_pending`] works
//! through the marked rows a batch at a time on a background thread. A row
//! re-marked while its batch was being read keeps its mark, because only the
//! count that was read is cleared.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, OptionalExtension};

use super::Cache;
use crate::mail_search::Field;

/// Bump to rebuild the index from scratch on the next open (the layout or
/// what goes into a column changed).
const SEARCH_INDEX_VERSION: i64 = 1;

pub(super) const SEARCH_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS search_state (
    key   TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS search_docs (
    docid       INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id  INTEGER NOT NULL,
    folder_path TEXT    NOT NULL,
    uid         INTEGER NOT NULL,
    dirty       INTEGER NOT NULL DEFAULT 1,
    UNIQUE (account_id, folder_path, uid)
);
CREATE INDEX IF NOT EXISTS search_docs_dirty ON search_docs (docid) WHERE dirty > 0;
CREATE VIRTUAL TABLE IF NOT EXISTS search_fts USING fts5(
    subject, sender, to_, cc, reply_to, body, attachments,
    content = '', contentless_delete = 1,
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TABLE IF NOT EXISTS attachment_text (
    account_id  INTEGER NOT NULL,
    folder_path TEXT    NOT NULL,
    uid         INTEGER NOT NULL,
    idx         INTEGER NOT NULL,
    status      INTEGER NOT NULL,
    text        TEXT    NOT NULL DEFAULT '',
    PRIMARY KEY (account_id, folder_path, uid, idx)
);
CREATE TRIGGER IF NOT EXISTS search_msg_ins AFTER INSERT ON messages BEGIN
    INSERT INTO search_docs (account_id, folder_path, uid) VALUES (new.account_id, new.folder_path, new.uid)
    ON CONFLICT (account_id, folder_path, uid) DO UPDATE SET dirty = dirty + 1;
END;
CREATE TRIGGER IF NOT EXISTS search_msg_upd AFTER UPDATE OF subject, from_name, from_addr, recipients, cc, reply_to, preview ON messages
WHEN old.subject IS NOT new.subject OR old.from_name IS NOT new.from_name OR old.from_addr IS NOT new.from_addr
  OR old.recipients IS NOT new.recipients OR old.cc IS NOT new.cc OR old.reply_to IS NOT new.reply_to
  OR old.preview IS NOT new.preview BEGIN
    UPDATE search_docs SET dirty = dirty + 1
    WHERE account_id = new.account_id AND folder_path = new.folder_path AND uid = new.uid;
END;
CREATE TRIGGER IF NOT EXISTS search_msg_del AFTER DELETE ON messages BEGIN
    DELETE FROM search_fts WHERE rowid IN (SELECT docid FROM search_docs
        WHERE account_id = old.account_id AND folder_path = old.folder_path AND uid = old.uid);
    DELETE FROM search_docs WHERE account_id = old.account_id AND folder_path = old.folder_path AND uid = old.uid;
    DELETE FROM attachment_text WHERE account_id = old.account_id AND folder_path = old.folder_path AND uid = old.uid;
END;
CREATE TRIGGER IF NOT EXISTS search_body_ins AFTER INSERT ON bodies BEGIN
    UPDATE search_docs SET dirty = dirty + 1
    WHERE account_id = new.account_id AND folder_path = new.folder_path AND uid = new.uid;
END;
CREATE TRIGGER IF NOT EXISTS search_body_del AFTER DELETE ON bodies BEGIN
    UPDATE search_docs SET dirty = dirty + 1
    WHERE account_id = old.account_id AND folder_path = old.folder_path AND uid = old.uid;
END;
CREATE TRIGGER IF NOT EXISTS search_att_ins AFTER INSERT ON attachments BEGIN
    UPDATE search_docs SET dirty = dirty + 1
    WHERE account_id = new.account_id AND folder_path = new.folder_path AND uid = new.uid;
END;
CREATE TRIGGER IF NOT EXISTS search_att_del AFTER DELETE ON attachments BEGIN
    DELETE FROM attachment_text
    WHERE account_id = old.account_id AND folder_path = old.folder_path AND uid = old.uid AND idx = old.idx;
    UPDATE search_docs SET dirty = dirty + 1
    WHERE account_id = old.account_id AND folder_path = old.folder_path AND uid = old.uid;
END;
CREATE TRIGGER IF NOT EXISTS search_meta_ins AFTER INSERT ON attachment_meta BEGIN
    UPDATE search_docs SET dirty = dirty + 1
    WHERE account_id = new.account_id AND folder_path = new.folder_path AND uid = new.uid;
END;
CREATE TRIGGER IF NOT EXISTS search_meta_del AFTER DELETE ON attachment_meta BEGIN
    UPDATE search_docs SET dirty = dirty + 1
    WHERE account_id = old.account_id AND folder_path = old.folder_path AND uid = old.uid;
END;
";

/// Most characters of one message's body the index reads. A body longer than
/// this is a log dump or a mailing-list digest; its start is what anyone
/// searches it for.
const BODY_INDEX_CHARS: usize = 1_000_000;

/// Most characters of one attachment's extracted text the index reads.
pub const ATTACHMENT_INDEX_CHARS: usize = 1_000_000;

/// How one stored attachment's text extraction went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractStatus {
    /// Text was found and stored.
    Text = 1,
    /// The file has no text layer (a scanned PDF): nothing to read without
    /// OCR, which Hylki does not do.
    NoText = 2,
    /// The file could not be read, was too large, or took too long.
    Failed = 3,
}

/// One attachment stored locally whose text has not been extracted yet.
pub struct PendingAttachment {
    pub account_id: u32,
    pub folder_path: String,
    pub uid: u32,
    pub idx: u32,
    pub name: String,
    pub data: Vec<u8>,
}

/// What [`Cache::search_hits`] found: for each searched term, the messages
/// whose indexed text holds it, as `(account_id, folder_path, uid)`.
pub type TermHits = HashMap<(Field, String), HashSet<(u32, String, u32)>>;

/// Make the index's tables and triggers, and rebuild the index from the
/// stored messages when it is new or its version changed. Best effort: a
/// cache whose SQLite lacks FTS5 searches the list's own fields only.
pub(super) fn ensure_schema(conn: &rusqlite::Connection) {
    if let Err(e) = conn.execute_batch(SEARCH_SCHEMA) {
        tracing::warn!("search index unavailable: {e}");
        return;
    }
    let version = || -> Option<i64> {
        conn.query_row("SELECT value FROM search_state WHERE key = 'version'", [], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    };
    if version() == Some(SEARCH_INDEX_VERSION) {
        return;
    }
    // Several threads open the cache at start-up; the first to take the
    // write lock builds the index, and the others find it built.
    let _ = conn.busy_timeout(std::time::Duration::from_secs(10));
    if conn.execute_batch("BEGIN IMMEDIATE;").is_err() {
        return;
    }
    if version() == Some(SEARCH_INDEX_VERSION) {
        let _ = conn.execute_batch("COMMIT;");
        return;
    }
    // Everything is indexed again: every stored message gets a fresh row,
    // marked out of date, for the background indexer to fill.
    let rebuilt = conn.execute_batch(&format!(
        "DELETE FROM search_fts;
         DELETE FROM search_docs;
         INSERT INTO search_docs (account_id, folder_path, uid, dirty)
             SELECT account_id, folder_path, uid, 1 FROM messages;
         INSERT OR REPLACE INTO search_state (key, value) VALUES ('version', {SEARCH_INDEX_VERSION});
         COMMIT;"
    ));
    if let Err(e) = rebuilt {
        let _ = conn.execute_batch("ROLLBACK;");
        tracing::warn!("search index could not be rebuilt: {e}");
    }
}

/// The FTS5 expression for one term: the term as a phrase, the last word
/// matching any word it starts, inside the columns the field covers. `None`
/// when the term holds no word to look for (punctuation only).
pub fn fts_expression(field: Field, text: &str) -> Option<String> {
    if !text.chars().any(char::is_alphanumeric) {
        return None;
    }
    let phrase = format!("\"{}\"*", text.replace('"', "\"\""));
    let columns = match field {
        Field::Any => return Some(phrase),
        Field::From => "sender",
        Field::To => "to_",
        Field::Cc => "cc",
        Field::ReplyTo => "reply_to",
        Field::Subject => "subject",
        Field::Body => "body",
        Field::Attachment => "attachments",
    };
    Some(format!("{{{columns}}} : {phrase}"))
}

impl Cache {
    /// Index up to `batch` messages whose index row is out of date. Returns
    /// how many were indexed, so the caller knows whether to go on at once or
    /// rest. Bodies are turned to text before the write lock is taken, so a
    /// batch holds it only for the inserts.
    pub fn index_pending(&self, batch: usize) -> rusqlite::Result<usize> {
        struct Doc {
            docid: i64,
            dirty: i64,
            account_id: u32,
            folder_path: String,
            uid: u32,
        }
        let docs: Vec<Doc> = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT docid, dirty, account_id, folder_path, uid FROM search_docs WHERE dirty > 0 LIMIT ?1",
            )?;
            let rows = stmt.query_map([batch as i64], |r| {
                Ok(Doc {
                    docid: r.get(0)?,
                    dirty: r.get(1)?,
                    account_id: r.get(2)?,
                    folder_path: r.get(3)?,
                    uid: r.get(4)?,
                })
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        if docs.is_empty() {
            return Ok(0);
        }
        // `None` for a row whose message is gone: its index entry goes too.
        let mut columns: Vec<(&Doc, Option<[String; 7]>)> = Vec::with_capacity(docs.len());
        for d in &docs {
            columns.push((d, self.index_columns(d.account_id, &d.folder_path, d.uid)?));
        }
        let tx = self.conn.unchecked_transaction()?;
        for (d, cols) in &columns {
            // The row must still be the one that was read, unchanged since: a
            // docid is never reused (AUTOINCREMENT), and a row marked again
            // meanwhile keeps its mark for the next batch, so nothing older
            // is written over newer work.
            let current = tx
                .query_row(
                    "SELECT 1 FROM search_docs WHERE docid = ?1 AND account_id = ?2 AND folder_path = ?3 \
                     AND uid = ?4 AND dirty = ?5",
                    params![d.docid, d.account_id, d.folder_path, d.uid, d.dirty],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !current {
                continue;
            }
            tx.execute("DELETE FROM search_fts WHERE rowid = ?1", [d.docid])?;
            match cols {
                Some(c) => {
                    tx.execute(
                        "INSERT INTO search_fts (rowid, subject, sender, to_, cc, reply_to, body, attachments) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![d.docid, c[0], c[1], c[2], c[3], c[4], c[5], c[6]],
                    )?;
                    tx.execute("UPDATE search_docs SET dirty = 0 WHERE docid = ?1", [d.docid])?;
                }
                None => {
                    tx.execute("DELETE FROM search_docs WHERE docid = ?1", [d.docid])?;
                }
            }
        }
        tx.commit()?;
        Ok(docs.len())
    }

    /// What one message puts in each index column, read from the cache.
    fn index_columns(&self, account_id: u32, folder_path: &str, uid: u32) -> rusqlite::Result<Option<[String; 7]>> {
        let head = self
            .conn
            .query_row(
                "SELECT subject, from_name, from_addr, recipients, cc, reply_to, preview FROM messages \
                 WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
                params![account_id, folder_path, uid],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        format!("{} {}", r.get::<_, String>(1)?, r.get::<_, String>(2)?),
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((subject, sender, to, cc, reply_to, preview)) = head else { return Ok(None) };
        // Nothing of an OpenPGP message's content reaches the cache, and so
        // nothing reaches the index; this holds even for a row stored before
        // that rule, which the cache's own clean-up on open removes as well.
        let protected = crate::models::preview_is_encrypted(&preview) || self.is_pgp(account_id, folder_path, uid);
        let mut body = String::new();
        let mut attachments = String::new();
        if !protected {
            let stored: Option<String> = self
                .conn
                .query_row(
                    "SELECT body FROM bodies WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
                    params![account_id, folder_path, uid],
                    |r| r.get(0),
                )
                .optional()?;
            body = match stored {
                Some(html) => truncate_chars(crate::app::message_text(&html), BODY_INDEX_CHARS),
                // The preview is all there is until the body is downloaded.
                None => preview,
            };
            let mut names: Vec<String> = Vec::new();
            for sql in [
                "SELECT name FROM attachment_meta WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
                "SELECT name FROM attachments WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
            ] {
                let mut stmt = self.conn.prepare_cached(sql)?;
                for name in stmt.query_map(params![account_id, folder_path, uid], |r| r.get::<_, String>(0))? {
                    let name = name?;
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
            attachments = names.join("\n");
            let mut stmt = self.conn.prepare_cached(
                "SELECT text FROM attachment_text WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3 \
                 AND status = 1 ORDER BY idx",
            )?;
            for text in stmt.query_map(params![account_id, folder_path, uid], |r| r.get::<_, String>(0))? {
                attachments.push('\n');
                attachments.push_str(&text?);
            }
        }
        Ok(Some([subject, sender, to, cc, reply_to, body, attachments]))
    }

    fn is_pgp(&self, account_id: u32, folder_path: &str, uid: u32) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM sender_checks WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3 AND pgp != ''",
                params![account_id, folder_path, uid],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    /// How many messages wait to be indexed, for the status line.
    pub fn index_backlog(&self) -> usize {
        self.conn
            .query_row("SELECT count(*) FROM search_docs WHERE dirty > 0", [], |r| r.get::<_, i64>(0))
            .map(|n| n as usize)
            .unwrap_or(0)
    }

    /// For each term, the messages whose indexed text holds it, limited to
    /// `accounts` (every account when empty) and, when given, one folder.
    /// A term the index cannot look for (punctuation only) has no entry.
    pub fn search_hits(&self, terms: &[(Field, String)], accounts: &[u32], folder: Option<&str>) -> TermHits {
        let mut out = TermHits::new();
        for (field, text) in terms {
            let Some(expr) = fts_expression(*field, text) else { continue };
            let run = || -> rusqlite::Result<HashSet<(u32, String, u32)>> {
                let mut stmt = self.conn.prepare_cached(
                    "SELECT d.account_id, d.folder_path, d.uid FROM search_fts f \
                     JOIN search_docs d ON d.docid = f.rowid WHERE search_fts MATCH ?1",
                )?;
                let rows = stmt
                    .query_map([&expr], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, String>(1)?, r.get::<_, u32>(2)?)))?;
                let mut set = HashSet::new();
                for row in rows {
                    let (a, p, u) = row?;
                    if (accounts.is_empty() || accounts.contains(&a)) && folder.is_none_or(|f| f == p) {
                        set.insert((a, p, u));
                    }
                }
                Ok(set)
            };
            match run() {
                Ok(set) => {
                    out.insert((*field, text.clone()), set);
                }
                Err(e) => tracing::warn!("search index lookup failed: {e}"),
            }
        }
        out
    }

    /// Attachments stored locally that the index could read text from (PDF
    /// and DOCX by name) and has not tried yet, up to `limit`, smallest
    /// first so a long backlog shows progress. Never OpenPGP mail, whose
    /// attachments the IMAP path does not store and the others might store
    /// as ciphertext.
    pub fn attachments_to_extract(&self, limit: usize, max_bytes: usize) -> rusqlite::Result<Vec<PendingAttachment>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT a.account_id, a.folder_path, a.uid, a.idx, a.name, a.data FROM attachments a \
             WHERE (lower(a.name) LIKE '%.pdf' OR lower(a.name) LIKE '%.docx') \
               AND NOT EXISTS (SELECT 1 FROM attachment_text t WHERE t.account_id = a.account_id \
                   AND t.folder_path = a.folder_path AND t.uid = a.uid AND t.idx = a.idx) \
               AND NOT EXISTS (SELECT 1 FROM sender_checks s WHERE s.account_id = a.account_id \
                   AND s.folder_path = a.folder_path AND s.uid = a.uid AND s.pgp != '') \
             ORDER BY length(a.data) LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(PendingAttachment {
                account_id: r.get(0)?,
                folder_path: r.get(1)?,
                uid: r.get(2)?,
                idx: r.get(3)?,
                name: r.get(4)?,
                data: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            let mut a = row?;
            if a.data.len() > max_bytes {
                // Recorded as failed without reading it: too large to read.
                a.data.clear();
            }
            out.push(a);
        }
        Ok(out)
    }

    /// Store what extracting one attachment's text gave. The message's index
    /// row is marked out of date so the text reaches the index.
    pub fn save_attachment_text(
        &self,
        a: &PendingAttachment,
        status: ExtractStatus,
        text: &str,
    ) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        // Only while the attachment is still stored: one removed meanwhile
        // must not leave its text behind.
        tx.execute(
            "INSERT OR REPLACE INTO attachment_text (account_id, folder_path, uid, idx, status, text) \
             SELECT ?1, ?2, ?3, ?4, ?5, ?6 WHERE EXISTS (SELECT 1 FROM attachments \
                 WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3 AND idx = ?4)",
            params![
                a.account_id,
                a.folder_path,
                a.uid,
                a.idx,
                status as i64,
                truncate_chars(text.to_string(), ATTACHMENT_INDEX_CHARS)
            ],
        )?;
        tx.execute(
            "UPDATE search_docs SET dirty = dirty + 1 WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
            params![a.account_id, a.folder_path, a.uid],
        )?;
        tx.commit()
    }
}

fn truncate_chars(mut s: String, max: usize) -> String {
    if let Some((cut, _)) = s.char_indices().nth(max) {
        s.truncate(cut);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::FolderKind;

    fn cache() -> Cache {
        let c = Cache::in_memory().unwrap();
        c.conn
            .execute(
                "INSERT INTO folders (account_id, path, name, kind, unread, ord) VALUES (1, 'INBOX', 'INBOX', ?1, 0, 0)",
                [super::super::kind_to_i64(FolderKind::Inbox)],
            )
            .unwrap();
        c
    }

    #[allow(clippy::too_many_arguments)]
    fn add(c: &Cache, folder: &str, uid: u32, subject: &str, to: &str, cc: &str, reply_to: &str, preview: &str) {
        c.conn
            .execute(
                "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, recipients, cc, subject, date, ts, \
                 unread, starred, has_attachment, preview, reply_to) \
                 VALUES (1, ?1, ?2, 'Ann Sender', 'ann@example.org', ?3, ?4, ?5, '', 100, 0, 0, 0, ?6, ?7)",
                params![folder, uid, to, cc, subject, preview, reply_to],
            )
            .unwrap();
    }

    fn index_all(c: &Cache) {
        while c.index_pending(50).unwrap() > 0 {}
    }

    fn hits(c: &Cache, field: Field, text: &str) -> Vec<u32> {
        let terms = [(field, text.to_string())];
        let mut v: Vec<u32> = c
            .search_hits(&terms, &[], None)
            .remove(&(field, text.to_string()))
            .unwrap_or_default()
            .into_iter()
            .map(|(_, _, u)| u)
            .collect();
        v.sort_unstable();
        v
    }

    /// A word far past the 200 and 240 characters a preview keeps is found
    /// once the whole body is stored, and not before.
    #[test]
    fn a_word_past_the_preview_is_found_in_the_stored_body() {
        let c = cache();
        let filler = "lorem ipsum ".repeat(40); // 480 characters
        for cut in [199usize, 200, 201, 239, 240, 241] {
            let uid = cut as u32;
            let text = format!("{} needle{uid}", &filler[..cut]);
            add(&c, "INBOX", uid, "Hello", "", "", "", &text.chars().take(200).collect::<String>());
            c.save_body(1, "INBOX", uid, &format!("<!doctype html><p>{text}</p>"));
        }
        index_all(&c);
        for cut in [199u32, 200, 201, 239, 240, 241] {
            assert_eq!(hits(&c, Field::Any, &format!("needle{cut}")), vec![cut], "word after {cut} characters");
            assert_eq!(hits(&c, Field::Body, &format!("needle{cut}")), vec![cut]);
        }
    }

    #[test]
    fn recipients_are_their_own_columns() {
        let c = cache();
        add(&c, "INBOX", 1, "Status", "dana@example.org", "", "", "");
        add(&c, "INBOX", 2, "Status", "", "carol@example.org", "", "");
        add(&c, "INBOX", 3, "Status", "", "", "list@example.org", "");
        index_all(&c);
        assert_eq!(hits(&c, Field::To, "dana"), vec![1]);
        assert_eq!(hits(&c, Field::Cc, "carol"), vec![2]);
        assert_eq!(hits(&c, Field::ReplyTo, "list"), vec![3]);
        assert_eq!(hits(&c, Field::Any, "carol"), vec![2]);
        assert!(hits(&c, Field::To, "carol").is_empty());
    }

    #[test]
    fn words_match_by_their_beginning_and_ignore_accents() {
        let c = cache();
        add(&c, "INBOX", 1, "Rechnung für März", "", "", "", "");
        add(&c, "INBOX", 2, "Ελληνικά νέα", "", "", "", "");
        index_all(&c);
        assert_eq!(hits(&c, Field::Subject, "rechn"), vec![1]);
        assert_eq!(hits(&c, Field::Subject, "marz"), vec![1]);
        assert_eq!(hits(&c, Field::Subject, "fur marz"), vec![1], "a phrase");
        // Case folds in every script; accents are folded for Latin letters
        // only, which is what SQLite's tokenizer knows.
        assert_eq!(hits(&c, Field::Any, "ΕΛΛΗΝΙΚΆ"), vec![2]);
        assert!(hits(&c, Field::Any, "nothing-here").is_empty());
    }

    #[test]
    fn the_index_follows_changes_moves_and_deletions() {
        let c = cache();
        add(&c, "INBOX", 1, "Old subject", "", "", "", "");
        index_all(&c);
        assert_eq!(hits(&c, Field::Subject, "old"), vec![1]);

        c.conn.execute("UPDATE messages SET subject = 'New subject' WHERE uid = 1", []).unwrap();
        index_all(&c);
        assert!(hits(&c, Field::Subject, "old").is_empty());
        assert_eq!(hits(&c, Field::Subject, "new"), vec![1]);

        // A move is a delete in one folder and an insert in the other.
        c.delete_message(1, "INBOX", 1);
        add(&c, "Archive", 7, "New subject", "", "", "", "");
        index_all(&c);
        let found = c.search_hits(&[(Field::Subject, "new".into())], &[], None);
        let set = &found[&(Field::Subject, "new".to_string())];
        assert_eq!(set.len(), 1);
        assert!(set.contains(&(1, "Archive".to_string(), 7)));

        c.delete_message(1, "Archive", 7);
        index_all(&c);
        assert!(hits(&c, Field::Subject, "new").is_empty());
        assert_eq!(c.index_backlog(), 0);
    }

    /// A message deleted and another stored straight after never share an
    /// index row, so a batch read before the change cannot write the old
    /// message's words onto the new one.
    #[test]
    fn an_index_row_is_never_reused() {
        let c = cache();
        add(&c, "INBOX", 1, "First", "", "", "", "");
        let first: i64 = c.conn.query_row("SELECT docid FROM search_docs", [], |r| r.get(0)).unwrap();
        c.delete_message(1, "INBOX", 1);
        add(&c, "Archive", 2, "Second", "", "", "", "");
        let second: i64 = c.conn.query_row("SELECT docid FROM search_docs", [], |r| r.get(0)).unwrap();
        assert_ne!(first, second);
    }

    /// The preview stands in for a body not stored, so a new preview is
    /// indexed again, and an old one's words stop matching.
    #[test]
    fn a_new_preview_is_indexed_again() {
        let c = cache();
        add(&c, "INBOX", 1, "Hi", "", "", "", "about the walrus");
        index_all(&c);
        assert_eq!(hits(&c, Field::Body, "walrus"), vec![1]);
        c.conn.execute("UPDATE messages SET preview = ?1 WHERE uid = 1", [crate::models::ENCRYPTED_PREVIEW]).unwrap();
        assert_eq!(c.index_backlog(), 1);
        index_all(&c);
        assert!(hits(&c, Field::Body, "walrus").is_empty());
    }

    #[test]
    fn a_resync_with_the_same_headers_leaves_the_index_alone() {
        let c = cache();
        add(&c, "INBOX", 1, "Same", "", "", "", "");
        index_all(&c);
        c.conn.execute("UPDATE messages SET subject = 'Same', unread = 1 WHERE uid = 1", []).unwrap();
        assert_eq!(c.index_backlog(), 0, "nothing searchable changed");
    }

    #[test]
    fn a_removed_body_leaves_the_index() {
        let c = cache();
        add(&c, "INBOX", 1, "Hi", "", "", "", "short preview");
        c.save_body(1, "INBOX", 1, "<p>short preview and a distant zebra</p>");
        index_all(&c);
        assert_eq!(hits(&c, Field::Body, "zebra"), vec![1]);
        c.remove_downloaded(None).unwrap();
        index_all(&c);
        assert!(hits(&c, Field::Body, "zebra").is_empty(), "only what is stored is searched");
        assert_eq!(hits(&c, Field::Body, "short"), vec![1], "the preview stays");
    }

    #[test]
    fn openpgp_mail_contributes_its_headers_only() {
        let c = cache();
        add(&c, "INBOX", 1, "Secret plans", "", "", "", crate::models::ENCRYPTED_PREVIEW);
        // A body stored by some older path must still stay out.
        c.conn
            .execute(
                "INSERT INTO bodies (account_id, folder_path, uid, body) VALUES (1, 'INBOX', 1, '<p>launch codes</p>')",
                [],
            )
            .unwrap();
        index_all(&c);
        assert_eq!(hits(&c, Field::Subject, "secret"), vec![1]);
        assert!(hits(&c, Field::Any, "launch").is_empty());
    }

    #[test]
    fn attachment_names_and_extracted_text_are_searchable() {
        let c = cache();
        add(&c, "INBOX", 1, "Files", "", "", "", "");
        c.save_attachment_meta(
            1,
            "INBOX",
            1,
            &[crate::models::AttachmentMeta {
                idx: 0,
                name: "Quarterly-Budget.pdf".into(),
                mime: String::new(),
                size: 10,
                section: "2".into(),
            }],
        );
        c.save_attachments(
            1,
            "INBOX",
            1,
            &[crate::models::Attachment { name: "Quarterly-Budget.pdf".into(), data: b"%PDF".to_vec() }],
        );
        index_all(&c);
        assert_eq!(hits(&c, Field::Attachment, "budget"), vec![1]);
        assert_eq!(hits(&c, Field::Any, "quarterly"), vec![1]);

        let pending = c.attachments_to_extract(10, 1 << 20).unwrap();
        assert_eq!(pending.len(), 1);
        c.save_attachment_text(&pending[0], ExtractStatus::Text, "Projected revenue for Lisbon").unwrap();
        assert!(c.attachments_to_extract(10, 1 << 20).unwrap().is_empty(), "tried once");
        index_all(&c);
        assert_eq!(hits(&c, Field::Attachment, "lisbon"), vec![1]);
        assert!(hits(&c, Field::Body, "lisbon").is_empty());

        // The text leaves with the attachment.
        c.save_attachments(1, "INBOX", 1, &[]);
        index_all(&c);
        assert!(hits(&c, Field::Attachment, "lisbon").is_empty());
    }

    #[test]
    fn hits_are_scoped_to_accounts_and_a_folder() {
        let c = cache();
        add(&c, "INBOX", 1, "Alpha", "", "", "", "");
        add(&c, "Archive", 2, "Alpha", "", "", "", "");
        index_all(&c);
        let t = [(Field::Any, "alpha".to_string())];
        assert_eq!(c.search_hits(&t, &[], Some("Archive"))[&t[0]].len(), 1);
        assert_eq!(c.search_hits(&t, &[1], None)[&t[0]].len(), 2);
        assert!(c.search_hits(&t, &[2], None)[&t[0]].is_empty());
    }

    #[test]
    fn a_term_of_punctuation_is_not_sent_to_the_index() {
        assert_eq!(fts_expression(Field::Any, "-"), None);
        assert_eq!(fts_expression(Field::Any, "a\"b"), Some("\"a\"\"b\"*".into()));
        assert_eq!(fts_expression(Field::Cc, "x"), Some("{cc} : \"x\"*".into()));
    }

    /// How long a large mailbox takes to index and to search, for checking
    /// by hand: `cargo test --release index_timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn index_timing() {
        let c = cache();
        let words = ["budget", "meeting", "invoice", "travel", "review", "launch", "zebra", "report"];
        let n = 100_000u32;
        let t = std::time::Instant::now();
        {
            let tx = c.conn.unchecked_transaction().unwrap();
            for uid in 1..=n {
                let w = words[uid as usize % words.len()];
                tx.execute(
                    "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, recipients, cc, subject, date, ts, \
                     unread, starred, has_attachment, preview) VALUES (1, 'INBOX', ?1, 'Ann', 'ann@x', 'bob@x', '', ?2, '', ?1, 0, 0, 0, '')",
                    params![uid, format!("Subject {uid} {w}")],
                )
                .unwrap();
                let body = format!("<p>{}</p><p>token{uid} {w}</p>", "lorem ipsum dolor sit amet ".repeat(80));
                tx.execute(
                    "INSERT INTO bodies (account_id, folder_path, uid, body) VALUES (1, 'INBOX', ?1, ?2)",
                    params![uid, body],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        eprintln!("stored {n} messages with ~2 KB bodies in {:?}", t.elapsed());
        let t = std::time::Instant::now();
        while c.index_pending(200).unwrap() > 0 {}
        eprintln!("indexed in {:?}", t.elapsed());
        for q in ["zebra", "token99999", "lorem", "nothingatall"] {
            let t = std::time::Instant::now();
            let n = c.search_hits(&[(Field::Any, q.to_string())], &[], None).values().map(|s| s.len()).sum::<usize>();
            eprintln!("{q:>12}: {n} hits in {:?}", t.elapsed());
        }
        let q = crate::mail_search::Query::parse("zebra from:ann").unwrap();
        let t = std::time::Instant::now();
        let (rows, _) = crate::mail_search::search_cache(&c, &q, &[1], None, true);
        eprintln!("whole-cache search 'zebra from:ann': {} rows in {:?}", rows.len(), t.elapsed());
    }

    #[test]
    fn many_messages_index_in_batches() {
        let c = cache();
        for uid in 1..=1205u32 {
            add(&c, "INBOX", uid, &format!("Report {uid}"), "", "", "", "");
        }
        assert_eq!(c.index_backlog(), 1205);
        let mut rounds = 0;
        while c.index_pending(500).unwrap() > 0 {
            rounds += 1;
        }
        assert_eq!(rounds, 3);
        assert_eq!(hits(&c, Field::Subject, "report").len(), 1205);
    }
}
