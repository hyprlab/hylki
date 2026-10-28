//! Opening the cache: the database file and its permissions, the in-memory
//! demo cache, the upgrade from every earlier version, and moving rows when
//! accounts change places.

use rusqlite::{params, Connection};
use std::time::Duration;

use super::schema::{
    ACCOUNT_TABLES, DOWNLOADED_TABLES, LAYOUT_VERSION, RENDER_VERSION, SCHEMA, SCHEMA_VERSION,
};
use super::Cache;

/// Narrow an existing path's permissions to `mode`, if it exists.
///
/// Best-effort by design: a missing sidecar or a filesystem without Unix modes
/// is not a reason to refuse to open the cache.
#[cfg(unix)]
fn restrict(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    if path.exists() {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
}

#[cfg(not(unix))]
fn restrict(_path: &std::path::Path, _mode: u32) {}

impl Cache {
    /// A cache that lives only in memory, for the offline demo: the demo's
    /// sample mail must never reach the real `cache.db`, and running it through
    /// the same schema and the same queries is the point — the gallery's
    /// paging, search and sort are SQL, so a demo that bypassed them would
    /// exercise nothing.
    pub(crate) fn in_memory() -> rusqlite::Result<Cache> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        let _ = conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"));
        Ok(Cache { conn })
    }

    /// Make the cached rows follow the accounts they belong to: `accounts` is
    /// every configured account's id and address. An account's id is its
    /// place in the list, so removing one moves every account after it up a
    /// place, and the next one took over the removed account's mail, folders
    /// and bodies. Each account's rows now move with it, and the rows of an
    /// account that is gone, or whose address changed, are dropped. Run
    /// before the workers start, in one transaction.
    ///
    /// A cache from before this knew no owners: it is taken as it stands.
    pub(crate) fn claim_accounts(&self, accounts: &[(u32, String)]) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let owners: std::collections::HashMap<u32, String> = {
            let mut stmt = tx.prepare("SELECT account_id, email FROM account_owner")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let wanted: Vec<(u32, String)> = accounts
            .iter()
            .map(|(id, e)| (*id, e.trim().to_lowercase()))
            .collect();
        // Two accounts on one address could not be told apart by it.
        let distinct = wanted
            .iter()
            .map(|(_, e)| e)
            .collect::<std::collections::HashSet<_>>()
            .len()
            == wanted.len();
        let moved =
            wanted.iter().any(|(id, e)| owners.get(id) != Some(e)) || owners.len() > wanted.len();
        if !owners.is_empty() && distinct && moved {
            let keep: Vec<u32> = wanted
                .iter()
                .filter(|(id, e)| owners.get(id) == Some(e))
                .map(|(id, _)| *id)
                .collect();
            let moves: Vec<(u32, u32)> = wanted
                .iter()
                .filter(|(id, _)| !keep.contains(id))
                .filter_map(|(id, e)| {
                    owners
                        .iter()
                        .find(|(_, o)| *o == e)
                        .map(|(from, _)| (*from, *id))
                })
                .collect();
            let keep_list = keep
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            for table in ACCOUNT_TABLES {
                // Through negative ids, so a move never meets rows still
                // waiting to leave the place it goes to.
                for (from, to) in &moves {
                    tx.execute(
                        &format!("UPDATE {table} SET account_id = ?1 WHERE account_id = ?2"),
                        params![-i64::from(*to), from],
                    )?;
                }
                tx.execute(&format!("DELETE FROM {table} WHERE account_id > 0 AND account_id NOT IN ({keep_list})"), [])?;
                tx.execute(
                    &format!("UPDATE {table} SET account_id = -account_id WHERE account_id < 0"),
                    [],
                )?;
            }
            tracing::info!(?moves, kept = ?keep, "cache: rows follow the accounts to their new places");
        }
        tx.execute("DELETE FROM account_owner", [])?;
        for (id, email) in &wanted {
            tx.execute(
                "INSERT INTO account_owner (account_id, email) VALUES (?1, ?2)",
                params![id, email],
            )?;
        }
        tx.commit()
    }

    /// A separate WAL reader over an already-initialized cache. Background
    /// lookups need another connection, not another migration and cleanup pass.
    pub(crate) fn read_connection(&self) -> rusqlite::Result<Cache> {
        let path = self.conn.path().ok_or_else(|| {
            rusqlite::Error::InvalidPath(std::path::PathBuf::from("cache has no database file"))
        })?;
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        Ok(Cache { conn })
    }

    /// Open (creating if needed) the cache DB at `~/.local/share/hylki/cache.db`.
    pub(crate) fn open() -> rusqlite::Result<Cache> {
        // No `temp_dir` fallback: this database holds message bodies, attachment
        // bytes and the harvested address book, and a shared world-writable
        // directory is the wrong place for any of it. Without a data dir there
        // is no acceptable location, so fail instead of picking a bad one.
        let path = crate::config::data_base()
            .ok_or_else(|| {
                rusqlite::Error::InvalidPath(std::path::PathBuf::from(
                    "no XDG data directory for the mail cache",
                ))
            })?
            .join("hylki");
        let _ = std::fs::create_dir_all(&path);
        restrict(&path, 0o700);
        Self::open_at(&path.join("cache.db"))
    }

    /// Open (creating if needed) the cache database at `db`, bring it to the
    /// current schema, and share it in WAL mode.
    pub(super) fn open_at(db: &std::path::Path) -> rusqlite::Result<Cache> {
        let conn = Connection::open(db)?;
        // The whole mailbox lives here, so it gets at least the care
        // `accounts.toml` gets.
        restrict(db, 0o600);
        Self::migrate(&conn)?;

        // Concurrency: multiple account workers share this file.
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let _ = conn.busy_timeout(Duration::from_secs(5));

        // SQLite creates the WAL sidecars itself, and only once WAL mode is on —
        // hence here rather than beside the `Connection::open` above. They hold
        // the same message data mid-transaction, so they get the same mode. The
        // 0700 on the directory is what actually keeps other users out; this is
        // in case the cache is ever moved somewhere less private.
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut side = db.as_os_str().to_os_string();
            side.push(suffix);
            restrict(std::path::Path::new(&side), 0o600);
        }

        Ok(Cache { conn })
    }

    /// Bring a database of any earlier version to the current schema: drop
    /// what an older build stored wrongly, add the columns added since, and
    /// record the version.
    fn migrate(conn: &Connection) -> rusqlite::Result<()> {
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        drop_stale_tables(conn, version);
        conn.execute_batch(SCHEMA)?;
        for sql in ADDED_COLUMNS {
            let _ = conn.execute(sql, []);
        }
        clean_on_every_open(conn);
        Self::repair_rows_from(conn, version);
        let _ = conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"));
        Ok(())
    }

    /// The one-time repairs of rows an older build stored, by the version
    /// the database was at.
    fn repair_rows_from(conn: &Connection, version: i64) {
        // Upgrading in place (layout current, message index preserved) rather
        // than wiping and re-syncing the whole mailbox.
        let upgrading_index = (LAYOUT_VERSION..SCHEMA_VERSION).contains(&version);
        if upgrading_index {
            Self::redecode_encoded_subjects(conn);
        }
        if upgrading_index && version < 20 {
            Self::clean_quoted_names(conn);
        }
        if (LAYOUT_VERSION..21).contains(&version) {
            Self::recheck_suspicious_senders(conn);
        }
        // `attachment_meta` is the gallery's record of every attachment that
        // *exists*; `attachments` holds the few whose bytes were downloaded.
        // Seed the first from the second so files already in hand keep showing
        // while the scan works back through the archive.
        if version < 14 {
            Self::seed_attachment_meta(conn);
        }
        // Re-queue the messages a batched parse failure wrote off. Messages
        // that really do hold nothing are simply asked about once more and
        // marked again, so this costs a rescan and settles.
        // A message's `text/calendar` part is an attachment from v19 on, so
        // every list drawn up before that is one part short and every
        // "nothing to see here" mark was made under the old rule. Forget the
        // marks rather than the lists: the scan re-describes each message
        // from its BODYSTRUCTURE (no bytes downloaded) and replaces the list
        // it finds, so nothing is lost while it works back.
        if version < 19 {
            let _ =
                conn.execute_batch("DELETE FROM attachment_scan; DELETE FROM attachments_checked;");
        }
        if (14..15).contains(&version) {
            let _ = conn.execute(
                "DELETE FROM attachment_scan WHERE NOT EXISTS (\
                     SELECT 1 FROM attachment_meta am \
                     WHERE am.account_id = attachment_scan.account_id \
                       AND am.folder_path = attachment_scan.folder_path \
                       AND am.uid = attachment_scan.uid)",
                [],
            );
        }
    }

    /// One-time upgrade fix: earlier builds cached a message's subject verbatim
    /// when the RFC 2047 decoder aborted on an over-long encoded-word (a single
    /// `=?utf-8?Q?…?=` far past the 75-char limit, as Mailchimp emits). Now that
    /// [`crate::worker::decode_header`] decodes those, re-decode any subject
    /// still stored as a raw encoded-word — in place, so no re-sync is needed.
    pub(super) fn redecode_encoded_subjects(conn: &Connection) {
        let rows: Vec<(u32, String, u32, String)> = {
            let Ok(mut stmt) = conn.prepare(
                "SELECT account_id, folder_path, uid, subject FROM messages \
                 WHERE subject LIKE '%=?%?=%'",
            ) else {
                return;
            };
            let Ok(mapped) =
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            else {
                return;
            };
            mapped.flatten().collect()
        };
        for (account_id, folder_path, uid, subject) in rows {
            let decoded = crate::worker::decode_header(subject.as_bytes());
            if decoded != subject {
                let _ = conn.execute(
                    "UPDATE messages SET subject = ?1 \
                     WHERE account_id = ?2 AND folder_path = ?3 AND uid = ?4",
                    params![decoded, account_id, folder_path, uid],
                );
            }
        }
    }

    /// Drop the sender checks stored as suspicious, with the bodies (and
    /// attachment lists) served beside them, so the next open fetches the
    /// message and checks it again under the current rules (v21). A cached
    /// body is never re-fetched on its own, and a check is only made at a
    /// fetch, so the pair goes together; verdicts of any other kind stay.
    pub(super) fn recheck_suspicious_senders(conn: &Connection) {
        delete_downloaded_where_check(conn, "s.trust = 'suspicious'");
        let _ = conn.execute("DELETE FROM sender_checks WHERE trust = 'suspicious'", []);
    }

    /// Clean the names and subjects an earlier build stored as the server
    /// quoted them (#312). Only rows holding a backslash or a leading quote
    /// are read, which is a handful in a large mailbox.
    pub(super) fn clean_quoted_names(conn: &Connection) {
        let rows: Vec<(i64, String, String)> = {
            let Ok(mut stmt) = conn.prepare(
                "SELECT rowid, from_name, subject FROM messages \
                 WHERE from_name LIKE '%\\%' OR from_name LIKE '\"%' OR subject LIKE '%\\%'",
            ) else {
                return;
            };
            let Ok(mapped) = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))) else {
                return;
            };
            mapped.flatten().collect()
        };
        for (rowid, name, subject) in rows {
            let clean_name = crate::worker::clean_display_name(&name);
            let clean_subject = crate::worker::unescape_quoted_str(&subject);
            if clean_name != name || clean_subject != subject {
                let _ = conn.execute(
                    "UPDATE messages SET from_name = ?1, subject = ?2 WHERE rowid = ?3",
                    params![clean_name, clean_subject, rowid],
                );
            }
        }
        // A reply to such a sender recorded the name as it was shown.
        let names: Vec<(String, String)> = {
            let Ok(mut stmt) = conn.prepare(
                "SELECT email, name FROM addresses WHERE name LIKE '%\\%' OR name LIKE '\"%'",
            ) else {
                return;
            };
            let Ok(mapped) = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))) else {
                return;
            };
            mapped.flatten().collect()
        };
        for (email, name) in names {
            let clean = crate::worker::clean_display_name(&name);
            if clean != name {
                let _ = conn.execute(
                    "UPDATE addresses SET name = ?1 WHERE email = ?2",
                    params![clean, email],
                );
            }
        }
    }

    /// Give every already-downloaded attachment a `attachment_meta` row, and
    /// mark its message scanned: the bytes are in hand, so there is nothing the
    /// server could tell us about it. Runs once, on the upgrade to schema v14.
    fn seed_attachment_meta(conn: &Connection) {
        let rows: Vec<(u32, String, u32, u32, String, i64)> = {
            let Ok(mut stmt) = conn.prepare(
                "SELECT account_id, folder_path, uid, idx, name, LENGTH(data) FROM attachments",
            ) else {
                return;
            };
            let Ok(mapped) = stmt.query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            }) else {
                return;
            };
            mapped.filter_map(|r| r.ok()).collect()
        };
        for (account_id, folder_path, uid, idx, name, size) in rows {
            let _ = conn.execute(
                "INSERT OR IGNORE INTO attachment_meta                  (account_id, folder_path, uid, idx, name, mime, size, section, bucket, keywords)                  VALUES (?1, ?2, ?3, ?4, ?5, '', ?6, '', ?7, ?8)",
                params![
                    account_id,
                    &folder_path,
                    uid,
                    idx,
                    &name,
                    size,
                    crate::models::type_bucket(&name),
                    crate::models::type_keywords(&name),
                ],
            );
            let _ = conn.execute(
                "INSERT OR IGNORE INTO attachment_scan (account_id, folder_path, uid)                  VALUES (?1, ?2, ?3)",
                params![account_id, &folder_path, uid],
            );
        }
    }
}

/// Deletes what was downloaded for every message whose sender check
/// matches `condition` (SQL over the check, aliased `s`). Errors are
/// ignored, as for the rest of the upgrade.
fn delete_downloaded_where_check(conn: &Connection, condition: &str) {
    for table in DOWNLOADED_TABLES {
        let _ = conn.execute(
            &format!(
                "DELETE FROM {table} WHERE EXISTS (SELECT 1 FROM sender_checks s \
                 WHERE s.account_id = {table}.account_id AND s.folder_path = {table}.folder_path \
                 AND s.uid = {table}.uid AND {condition})"
            ),
            [],
        );
    }
}

/// Drop the tables an older build filled in a way this one cannot use:
/// every table of a layout before [`LAYOUT_VERSION`], and the rendered
/// bodies and verdicts (with the attachment lists before v13) of one before
/// [`RENDER_VERSION`].
fn drop_stale_tables(conn: &Connection, version: i64) {
    if version < LAYOUT_VERSION {
        let _ = conn.execute_batch(
            "DROP TABLE IF EXISTS folders;\
             DROP TABLE IF EXISTS messages;\
             DROP TABLE IF EXISTS bodies;\
             DROP TABLE IF EXISTS body_text;\
             DROP TABLE IF EXISTS sender_checks;\
             DROP TABLE IF EXISTS attachments;\
             DROP TABLE IF EXISTS attachments_checked;",
        );
    } else if version < RENDER_VERSION {
        // The layout is current but `bodies` holds HTML rendered by an older
        // build. `LoadBody` serves that cache without ever re-fetching, so a
        // stale entry would survive forever — drop it and let it re-render on
        // next open. The message index is kept: it's expensive to rebuild.
        let _ =
            conn.execute_batch("DROP TABLE IF EXISTS bodies; DROP TABLE IF EXISTS body_text; DROP TABLE IF EXISTS sender_checks;");
        // Attachment lists cached by builds before v13 are wrong in place
        // (small declared files missing, blobs duplicated per Gmail label),
        // and `attachments_checked` would keep them from ever re-fetching.
        // The re-fetch is bounded: the prefetch re-touches only the newest
        // messages per folder, the rest heal lazily on open.
        if version < 13 {
            let _ = conn.execute_batch(
                "DROP TABLE IF EXISTS attachments; DROP TABLE IF EXISTS attachments_checked;",
            );
        }
    }
}

/// Columns added to a table after it was first created, in the order they
/// came. `CREATE TABLE IF NOT EXISTS` leaves an existing table alone, so
/// each is added in place rather than dropping the message index, which
/// would cost every user a full re-sync; the error when a column is already
/// there is the expected outcome, not a problem.
const ADDED_COLUMNS: [&str; 15] = [
    // The list preview.
    "ALTER TABLE messages ADD COLUMN preview TEXT NOT NULL DEFAULT ''",
    // Send Later (#145): when a queued message is due; NULL sends at once.
    "ALTER TABLE outbox ADD COLUMN send_at INTEGER",
    // And the message a queued forward forwards (#454).
    "ALTER TABLE outbox ADD COLUMN forwards TEXT NOT NULL DEFAULT ''",
    // Same in-place treatment for `reply_to` (added later still): existing
    // rows carry an empty value until their folder's recent window
    // re-syncs, and Reply falls back to the sender until then.
    "ALTER TABLE messages ADD COLUMN reply_to TEXT NOT NULL DEFAULT ''",
    // And for `keywords` (tags, #71): existing rows read as untagged until
    // their folder syncs again and the flags come down with the rest.
    "ALTER TABLE messages ADD COLUMN keywords TEXT NOT NULL DEFAULT ''",
    // And the list's Importance and Due columns (#334): mail already
    // cached reads as normal, and undated, until it is fetched again.
    "ALTER TABLE messages ADD COLUMN importance INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE messages ADD COLUMN due INTEGER NOT NULL DEFAULT 0",
    // And Gmail's labels (#374): mail already cached shows none until its
    // folder's recent window syncs, or the backfill reaches it.
    "ALTER TABLE messages ADD COLUMN gmail_labels TEXT NOT NULL DEFAULT ''",
    // And replied and forwarded (#454), the same way.
    "ALTER TABLE messages ADD COLUMN answered INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE messages ADD COLUMN forwarded INTEGER NOT NULL DEFAULT 0",
    // And the Bcc of a sent copy (#476).
    "ALTER TABLE messages ADD COLUMN bcc TEXT NOT NULL DEFAULT ''",
    // And the OpenPGP verdict (#133) beside the sender check, as JSON;
    // empty for a message that carried none. Kept as a marker: nothing
    // OpenPGP is cached, so a body that was (by the first build of the
    // feature, which cached signed mail) is dropped here, and the
    // message is fetched and verified afresh at its next open.
    "ALTER TABLE sender_checks ADD COLUMN pgp TEXT NOT NULL DEFAULT ''",
    // And the meeting invitation (#223) beside it, as JSON; empty for a
    // message that carries none.
    "ALTER TABLE sender_checks ADD COLUMN invite TEXT NOT NULL DEFAULT ''",
    // And the sensitivity label (#411). A check stored before has none:
    // its message shows its label once it is fetched again.
    "ALTER TABLE sender_checks ADD COLUMN label TEXT NOT NULL DEFAULT ''",
    // And the read receipt asked for (#520). A check stored before has
    // none, so mail that arrived earlier never asks.
    "ALTER TABLE sender_checks ADD COLUMN receipt TEXT NOT NULL DEFAULT ''",
];

/// The cleanups run at every open, whatever the version.
fn clean_on_every_open(conn: &Connection) {
    delete_downloaded_where_check(conn, "s.pgp != ''");
    // Previews cached by a build that showed MIME machinery or a tracking
    // link instead of the message: a multipart's boundary ("--b2=_cipk…") or
    // the rendered link a marketing mail opens with ("( https://…"). Clearing
    // them shows nothing until the folder syncs again, which beats showing
    // either.
    let _ = conn.execute(
        "UPDATE messages SET preview = ''              WHERE preview LIKE '--%' OR preview LIKE '( http%'",
        [],
    );
    // Previews cached as the PGP/MIME version stub (#133) become the
    // encrypted-message marker the list draws a lock for.
    let _ = conn.execute(
        "UPDATE messages SET preview = ?1 \
         WHERE preview LIKE 'Version: 1' OR preview = 'Encrypted message'",
        [crate::models::ENCRYPTED_PREVIEW],
    );
}
