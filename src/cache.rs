//! Local SQLite cache for folders, message summaries, and bodies.
//!
//! The cache lets the app show mail instantly on startup, read offline, and
//! avoid re-fetching message bodies. Each per-account worker opens its own
//! connection (so the `!Send` `rusqlite::Connection` never crosses threads);
//! WAL mode keeps concurrent access from contending. Everything is keyed by
//! account so two accounts can both have an "INBOX". It is strictly
//! best-effort: any error is logged and degrades to "no cache".

use std::time::Duration;

use rusqlite::{params, Connection};

use crate::models::{Attachment, Folder, FolderKind, GallerySort, Message};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS folders (
    account_id INTEGER NOT NULL,
    path       TEXT    NOT NULL,
    name       TEXT    NOT NULL,
    kind       INTEGER NOT NULL,
    unread     INTEGER NOT NULL,
    ord        INTEGER NOT NULL,
    PRIMARY KEY (account_id, path)
);
CREATE TABLE IF NOT EXISTS refs_repair (
    account_id  INTEGER NOT NULL,
    folder_path TEXT    NOT NULL,
    next_uid    INTEGER NOT NULL,
    done        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account_id, folder_path)
);
CREATE TABLE IF NOT EXISTS messages (
    account_id     INTEGER NOT NULL,
    folder_path    TEXT    NOT NULL,
    uid            INTEGER NOT NULL,
    from_name      TEXT    NOT NULL,
    from_addr      TEXT    NOT NULL,
    recipients     TEXT    NOT NULL DEFAULT '',
    cc             TEXT    NOT NULL DEFAULT '',
    subject        TEXT    NOT NULL,
    date           TEXT    NOT NULL,
    ts             INTEGER NOT NULL,
    unread         INTEGER NOT NULL,
    starred        INTEGER NOT NULL,
    has_attachment INTEGER NOT NULL,
    message_id     TEXT    NOT NULL DEFAULT '',
    references_    TEXT    NOT NULL DEFAULT '',
    preview        TEXT    NOT NULL DEFAULT '',
    reply_to       TEXT    NOT NULL DEFAULT '',
    keywords       TEXT    NOT NULL DEFAULT '',
    PRIMARY KEY (account_id, folder_path, uid)
);
CREATE INDEX IF NOT EXISTS messages_by_message_id ON messages (message_id);
CREATE TABLE IF NOT EXISTS local_tags (
    account_id  INTEGER NOT NULL,
    message_id  TEXT    NOT NULL,
    keyword     TEXT    NOT NULL,
    PRIMARY KEY (account_id, message_id, keyword)
);
CREATE TABLE IF NOT EXISTS bodies (
    account_id  INTEGER NOT NULL,
    folder_path TEXT NOT NULL,
    uid         INTEGER NOT NULL,
    body        TEXT NOT NULL,
    PRIMARY KEY (account_id, folder_path, uid)
);
CREATE TABLE IF NOT EXISTS sender_checks (
    account_id  INTEGER NOT NULL,
    folder_path TEXT NOT NULL,
    uid         INTEGER NOT NULL,
    trust       TEXT NOT NULL,
    summary     TEXT NOT NULL,
    findings    TEXT NOT NULL,
    pgp         TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (account_id, folder_path, uid)
);
CREATE TABLE IF NOT EXISTS attachments (
    account_id  INTEGER NOT NULL,
    folder_path TEXT NOT NULL,
    uid         INTEGER NOT NULL,
    idx         INTEGER NOT NULL,
    name        TEXT NOT NULL,
    data        BLOB NOT NULL,
    PRIMARY KEY (account_id, folder_path, uid, idx)
);
CREATE TABLE IF NOT EXISTS addresses (
    email TEXT PRIMARY KEY,
    name  TEXT NOT NULL DEFAULT '',
    count INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS outbox (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id  INTEGER NOT NULL,
    from_addr   TEXT    NOT NULL,
    rcpts       TEXT    NOT NULL,
    recipients  TEXT    NOT NULL,
    subject     TEXT    NOT NULL,
    preview     TEXT    NOT NULL DEFAULT '',
    raw         BLOB    NOT NULL,
    sent_path   TEXT,
    queued_at   INTEGER NOT NULL,
    attempts    INTEGER NOT NULL DEFAULT 0,
    last_error  TEXT    NOT NULL DEFAULT '',
    send_at     INTEGER
);
CREATE TABLE IF NOT EXISTS attachments_checked (
    account_id  INTEGER NOT NULL,
    folder_path TEXT NOT NULL,
    uid         INTEGER NOT NULL,
    PRIMARY KEY (account_id, folder_path, uid)
);
CREATE TABLE IF NOT EXISTS attachment_meta (
    account_id  INTEGER NOT NULL,
    folder_path TEXT NOT NULL,
    uid         INTEGER NOT NULL,
    idx         INTEGER NOT NULL,
    name        TEXT    NOT NULL,
    mime        TEXT    NOT NULL DEFAULT '',
    size        INTEGER NOT NULL DEFAULT 0,
    section     TEXT    NOT NULL DEFAULT '',
    ext         TEXT    NOT NULL DEFAULT '',
    bucket      INTEGER NOT NULL DEFAULT 6,
    keywords    TEXT    NOT NULL DEFAULT '',
    PRIMARY KEY (account_id, folder_path, uid, idx)
);
CREATE TABLE IF NOT EXISTS attachment_scan (
    account_id  INTEGER NOT NULL,
    folder_path TEXT NOT NULL,
    uid         INTEGER NOT NULL,
    PRIMARY KEY (account_id, folder_path, uid)
);
CREATE INDEX IF NOT EXISTS attachment_meta_by_folder
    ON attachment_meta (account_id, folder_path);
";

/// Bump when the table layout changes; older rows are dropped on open.
/// v8: bodies are re-rendered with clickable links, so cached bodies (which
/// `LoadBody` serves without re-fetching) must be dropped and rebuilt on open.
/// v9: re-decode subjects cached as raw RFC 2047 encoded-words by builds that
/// aborted on over-long encoded-words (e.g. Mailchimp newsletters).
/// v10: bodies are re-rendered with `cid:` image references resolved to `data:`
/// URIs, so cached bodies showing a broken image must be dropped and rebuilt.
/// v11: sender authentication is computed from the raw message at the same
/// moment the body is rendered, so dropping `bodies` re-fetches both together
/// and every cached message gains a verdict.
/// v12: plain-text bodies no longer bake `padding:16px` into the cached
/// document — the reader injects the default inset at render time — so cached
/// bodies carrying the old baked padding must be dropped and re-rendered.
/// v13: builds before the declared-attachment exemption dropped small files a
/// web-Gmail sender attached, and stored one blob copy per Gmail label. The
/// checked table remembers those messages as done, so the wrong lists would
/// survive forever — drop both tables and let attachments re-fetch on demand.
/// v14: `attachment_meta`/`attachment_scan` for the gallery — purely additive,
/// so nothing cached is dropped for it (see [`RENDER_VERSION`]).
/// v15: the first attachment scan wrote off a whole batch when one message's
/// BODYSTRUCTURE would not parse, so up to 200 messages were recorded as
/// holding nothing when several held files. Messages marked as scanned with
/// nothing to show are re-queued once, to be asked about again by the scan that
/// now isolates the one message at fault.
const SCHEMA_VERSION: i64 = 15;

/// The newest version whose change altered how bodies are *rendered* or how
/// senders are checked. Opening a database older than this drops `bodies` and
/// `sender_checks` so they rebuild; a later purely-additive bump must not,
/// or every such release would cost users a full re-fetch of everything they
/// had read. Raise this only when the rendering itself changes.
const RENDER_VERSION: i64 = 13;

/// A message's keywords as one column: the server's, then any tag kept
/// locally for the same Message-ID (POP3, or an IMAP server that refuses
/// custom keywords). `local_tags` is small and keyed for this lookup, so the
/// correlated subquery is an index seek per row.
const KEYWORDS_COL: &str = "messages.keywords || ' ' || COALESCE((SELECT group_concat(lt.keyword, ' ') \
    FROM local_tags lt WHERE lt.account_id = messages.account_id \
    AND messages.message_id <> '' AND lt.message_id = messages.message_id), '')";

/// Split a keywords column back into the list a [`Message`] carries.
fn split_keywords(col: String) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for k in col.split_whitespace() {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(k)) {
            out.push(k.to_string());
        }
    }
    out
}

/// Most messages one tag view lists per account; the list pages within it.
const TAG_VIEW_LIMIT: i64 = 5000;

/// Most Message-IDs one cross-folder conversation lookup matches against. A
/// thread's ancestry is short in practice, and the references half of that query
/// is a scan — this keeps a pathological References header (some mailing lists
/// accumulate hundreds) from turning a message-open into a long one.
const THREAD_ID_LIMIT: usize = 24;

/// How many messages one conversation may pull in from other folders. A
/// conversation is rendered as a single document with every member's body
/// inlined, so this bounds both the fetching and the rendering.
const THREAD_MEMBER_LIMIT: usize = 100;

/// (Adding a *new* table needs no bump: `SCHEMA` runs `CREATE TABLE IF NOT
/// EXISTS` on every open, so an existing cache gains it in place. A bump is for
/// changing or invalidating what is already stored — it costs users a re-render
/// or a re-sync.)
///
/// The first version whose table *layout* matches the current `SCHEMA`. At or
/// above this, an upgrade only needs to drop the derived caches, not the
/// expensive message index (which would force a whole-mailbox re-sync).
const LAYOUT_VERSION: i64 = 6;

pub struct Cache {
    conn: Connection,
}

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

/// What the attachments gallery is asking for: the scope, the search and the
/// ordering, all of which have to reach the database — the archive holds far
/// more attachments than the UI can hold in memory, so filtering after the fact
/// would page through the wrong set.
pub struct GalleryQuery<'a> {
    /// `(account id, folder path)` pairs whose attachments are in scope. Empty
    /// means "every folder", which is what a gallery opened before the folder
    /// list arrives should show.
    pub folders: &'a [(u32, String)],
    /// Narrow to one account, as the footer's account dropdown does.
    pub account_id: Option<u32>,
    /// Search words; a row has to match every one of them somewhere.
    pub tokens: &'a [String],
    /// A [`crate::models::type_bucket`] value, or 0 for every type.
    pub bucket: u32,
    pub sort: GallerySort,
    pub limit: u32,
    pub offset: u32,
    /// Largest cached file whose bytes ride along with the page.
    pub data_cap: i64,
}

impl GalleryQuery<'_> {
    /// The WHERE clause and its bound values, numbering placeholders from
    /// `first` so the caller's own leading parameters are not trodden on.
    fn where_clause(&self, first: usize) -> (String, Vec<rusqlite::types::Value>) {
        use rusqlite::types::Value;
        let mut clauses: Vec<String> = Vec::new();
        let mut params: Vec<Value> = Vec::new();
        let mut n = first;
        let mut next = |params: &mut Vec<Value>, v: Value| {
            params.push(v);
            let at = n;
            n += 1;
            format!("?{at}")
        };

        if let Some(id) = self.account_id {
            let p = next(&mut params, Value::Integer(id as i64));
            clauses.push(format!("am.account_id = {p}"));
        }
        if !self.folders.is_empty() {
            let ors: Vec<String> = self
                .folders
                .iter()
                .map(|(id, path)| {
                    let a = next(&mut params, Value::Integer(*id as i64));
                    let f = next(&mut params, Value::Text(path.clone()));
                    format!("(am.account_id = {a} AND am.folder_path = {f})")
                })
                .collect();
            clauses.push(format!("({})", ors.join(" OR ")));
        }
        if self.bucket != 0 {
            let p = next(&mut params, Value::Integer(self.bucket as i64));
            clauses.push(format!("am.bucket = {p}"));
        }
        // The same columns the old in-memory haystack joined: filename, the
        // type words stored beside it, sender, subject and folder. The folder
        // is matched on its raw path rather than its decoded label, so a
        // non-ASCII mailbox name is searched as the server spells it.
        for token in self.tokens {
            let pattern = format!("%{}%", like_escape(&token.to_lowercase()));
            let fields = [
                "LOWER(am.name)",
                "am.keywords",
                "LOWER(COALESCE(m.from_name, ''))",
                "LOWER(COALESCE(m.subject, ''))",
                "LOWER(am.folder_path)",
            ];
            let ors: Vec<String> = fields
                .iter()
                .map(|f| {
                    let p = next(&mut params, Value::Text(pattern.clone()));
                    format!("{f} LIKE {p} ESCAPE '\\'")
                })
                .collect();
            clauses.push(format!("({})", ors.join(" OR ")));
        }

        let sql = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        (sql, params)
    }
}

/// Neutralise the wildcards in a user's search word so typing `%` looks for a
/// percent sign rather than matching everything.
fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The ORDER BY for a sort criterion. Every one ends with the same tie-break so
/// a row can never drift between pages: without it two files of equal size (or
/// date, or name) could swap places between one page and the next and be shown
/// twice, or not at all.
fn order_by(sort: GallerySort) -> String {
    let head = match sort {
        GallerySort::Newest => "COALESCE(m.ts, 0) DESC",
        GallerySort::Oldest => "COALESCE(m.ts, 0) ASC",
        GallerySort::Name => "am.name COLLATE NOCASE ASC",
        GallerySort::NameDesc => "am.name COLLATE NOCASE DESC",
        GallerySort::Sender => "COALESCE(m.from_name, '') COLLATE NOCASE ASC",
        GallerySort::SenderDesc => "COALESCE(m.from_name, '') COLLATE NOCASE DESC",
        GallerySort::Largest => "am.size DESC",
        GallerySort::Smallest => "am.size ASC",
        GallerySort::Type => "am.ext ASC, am.name COLLATE NOCASE ASC",
        GallerySort::TypeDesc => "am.ext DESC, am.name COLLATE NOCASE DESC",
    };
    format!("{head}, am.account_id ASC, am.uid DESC, am.idx ASC")
}

impl Cache {
    /// Every cached copy of a message with this (normalized: no brackets,
    /// lowercase) Message-ID, as `(account_id, folder_path, uid)`, newest
    /// first — a `mid:` link (#130) is resolved from here before any server
    /// is asked.
    pub fn locate_by_message_id(&self, message_id: &str) -> Vec<(u32, String, u32)> {
        if message_id.is_empty() {
            return Vec::new();
        }
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT account_id, folder_path, uid FROM messages WHERE message_id = ?1 ORDER BY ts DESC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([message_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Open (creating if needed) the cache DB at `~/.local/share/hylki/cache.db`.
    /// A cache that lives only in memory, for the offline demo: the demo's
    /// sample mail must never reach the real `cache.db`, and running it through
    /// the same schema and the same queries is the point — the gallery's
    /// paging, search and sort are SQL, so a demo that bypassed them would
    /// exercise nothing.
    pub fn in_memory() -> rusqlite::Result<Cache> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        let _ = conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"));
        Ok(Cache { conn })
    }

    pub fn open() -> rusqlite::Result<Cache> {
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
        let db = path.join("cache.db");
        let conn = Connection::open(&db)?;
        // The whole mailbox lives here, so it gets at least the care
        // `accounts.toml` gets.
        restrict(&db, 0o600);

        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        // Upgrading in place (layout current, message index preserved) rather
        // than wiping and re-syncing the whole mailbox.
        let upgrading_index = (LAYOUT_VERSION..SCHEMA_VERSION).contains(&version);
        if version < LAYOUT_VERSION {
            let _ = conn.execute_batch(
                "DROP TABLE IF EXISTS folders;\
                 DROP TABLE IF EXISTS messages;\
                 DROP TABLE IF EXISTS bodies;\
                 DROP TABLE IF EXISTS sender_checks;\
                 DROP TABLE IF EXISTS attachments;\
                 DROP TABLE IF EXISTS attachments_checked;",
            );
        } else if version < RENDER_VERSION {
            // The layout is current but `bodies` holds HTML rendered by an older
            // build. `LoadBody` serves that cache without ever re-fetching, so a
            // stale entry would survive forever — drop it and let it re-render on
            // next open. The message index is kept: it's expensive to rebuild.
            let _ = conn
                .execute_batch("DROP TABLE IF EXISTS bodies; DROP TABLE IF EXISTS sender_checks;");
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
        conn.execute_batch(SCHEMA)?;
        // `messages` predates the preview column, and `CREATE TABLE IF NOT
        // EXISTS` leaves an existing table alone. Add it in place rather than
        // dropping the index, which would cost every user a full re-sync; the
        // error when it is already there is the expected outcome, not a problem.
        let _ = conn.execute("ALTER TABLE messages ADD COLUMN preview TEXT NOT NULL DEFAULT ''", []);
        // Send Later (#145): when a queued message is due; NULL sends at once.
        let _ = conn.execute("ALTER TABLE outbox ADD COLUMN send_at INTEGER", []);
        // Same in-place treatment for `reply_to` (added later still): existing
        // rows carry an empty value until their folder's recent window
        // re-syncs, and Reply falls back to the sender until then.
        let _ =
            conn.execute("ALTER TABLE messages ADD COLUMN reply_to TEXT NOT NULL DEFAULT ''", []);
        // And for `keywords` (tags, #71): existing rows read as untagged until
        // their folder syncs again and the flags come down with the rest.
        let _ =
            conn.execute("ALTER TABLE messages ADD COLUMN keywords TEXT NOT NULL DEFAULT ''", []);
        // And the OpenPGP verdict (#133) beside the sender check, as JSON;
        // empty for a message that carried none. Kept as a marker: nothing
        // OpenPGP is cached, so a body that was (by the first build of the
        // feature, which cached signed mail) is dropped here, and the
        // message is fetched and verified afresh at its next open.
        let _ =
            conn.execute("ALTER TABLE sender_checks ADD COLUMN pgp TEXT NOT NULL DEFAULT ''", []);
        for table in ["bodies", "attachments", "attachments_checked"] {
            let _ = conn.execute(
                &format!(
                    "DELETE FROM {table} WHERE EXISTS (SELECT 1 FROM sender_checks s \
                     WHERE s.account_id = {table}.account_id AND s.folder_path = {table}.folder_path \
                     AND s.uid = {table}.uid AND s.pgp != '')"
                ),
                [],
            );
        }
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
        if upgrading_index {
            Self::redecode_encoded_subjects(&conn);
        }
        // `attachment_meta` is the gallery's record of every attachment that
        // *exists*; `attachments` holds the few whose bytes were downloaded.
        // Seed the first from the second so files already in hand keep showing
        // while the scan works back through the archive.
        if version < 14 {
            Self::seed_attachment_meta(&conn);
        }
        // Re-queue the messages a batched parse failure wrote off. Messages
        // that really do hold nothing are simply asked about once more and
        // marked again, so this costs a rescan and settles.
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
        let _ = conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"));

        // Concurrency: multiple account workers share this file.
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let _ = conn.busy_timeout(Duration::from_secs(5));

        // SQLite creates the WAL sidecars itself, and only once WAL mode is on —
        // hence here rather than beside the `Connection::open` above. They hold
        // the same message data mid-transaction, so they get the same mode. The
        // 0700 on the directory is what actually keeps other users out; this is
        // in case the cache is ever moved somewhere less private.
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut side = db.clone().into_os_string();
            side.push(suffix);
            restrict(std::path::Path::new(&side), 0o600);
        }

        Ok(Cache { conn })
    }

    /// One-time upgrade fix: earlier builds cached a message's subject verbatim
    /// when the RFC 2047 decoder aborted on an over-long encoded-word (a single
    /// `=?utf-8?Q?…?=` far past the 75-char limit, as Mailchimp emits). Now that
    /// [`crate::worker::decode_header`] decodes those, re-decode any subject
    /// still stored as a raw encoded-word — in place, so no re-sync is needed.
    fn redecode_encoded_subjects(conn: &Connection) {
        let rows: Vec<(u32, String, u32, String)> = {
            let Ok(mut stmt) = conn.prepare(
                "SELECT account_id, folder_path, uid, subject FROM messages \
                 WHERE subject LIKE '%=?%?=%'",
            ) else {
                return;
            };
            let Ok(mapped) = stmt.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            }) else {
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

    pub fn load_folders(&self, account_id: u32) -> Vec<Folder> {
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

    pub fn save_folders(&self, account_id: u32, folders: &[Folder]) {
        let run = || -> rusqlite::Result<()> {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute("DELETE FROM folders WHERE account_id = ?1", [account_id])?;
            for (i, f) in folders.iter().enumerate() {
                tx.execute(
                    "INSERT INTO folders (account_id, path, name, kind, unread, ord)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![account_id, f.path, f.name, kind_to_i64(f.kind), f.unread, i as i64],
                )?;
            }
            tx.commit()
        };
        if let Err(e) = run() {
            tracing::warn!("cache save_folders failed: {e}");
        }
    }

    pub fn load_messages(&self, account_id: u32, folder_path: &str, folder_id: u32) -> Vec<Message> {
        let run = || -> rusqlite::Result<Vec<Message>> {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, recipients, cc, message_id, references_, preview, reply_to, {KEYWORDS_COL}
                 FROM messages WHERE account_id = ?1 AND folder_path = ?2 ORDER BY uid DESC",
            ))?;
            let rows = stmt.query_map(params![account_id, folder_path], |row| {
                let uid: u32 = row.get(0)?;
                let mut m = Message {
                    id: uid,
                    account_id,
                    folder_id,
                    uid,
                    from_name: row.get(1)?,
                    from_addr: row.get(2)?,
                    reply_to: row.get(14)?,
                    to: row.get(9)?,
                    cc: row.get(10)?,
                    subject: row.get(3)?,
                    preview: row.get(13)?,
                    body: String::new(),
                    date: row.get(4)?,
                    timestamp: row.get(5)?,
                    unread: row.get(6)?,
                    starred: row.get(7)?,
                    keywords: split_keywords(row.get(15)?),
                    has_attachment: row.get(8)?,
                    message_id: row.get(11)?,
                    references: row.get(12)?,
                };
                // Rows written before NUL-scrubbing existed may still carry
                // one; GTK labels abort on interior NULs.
                m.scrub_nuls();
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
    pub fn attachmentless_uids(
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
            let rows = stmt.query_map(params![account_id, folder_path], |row| row.get::<_, u32>(0))?;
            rows.collect()
        };
        run().unwrap_or_default()
    }

    pub fn save_messages(&self, account_id: u32, folder_path: &str, messages: &[Message]) {
        let run = || -> rusqlite::Result<()> {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute(
                "DELETE FROM messages WHERE account_id = ?1 AND folder_path = ?2",
                params![account_id, folder_path],
            )?;
            for m in messages {
                tx.execute(
                    "INSERT INTO messages
                     (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, recipients, cc, message_id, references_, reply_to, keywords)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                    params![
                        account_id, folder_path, m.uid, m.from_name, m.from_addr, m.subject,
                        m.date, m.timestamp, m.unread, m.starred, m.has_attachment, m.to, m.cc,
                        m.message_id, m.references, m.reply_to, m.keywords.join(" ")
                    ],
                )?;
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
    pub fn upsert_messages(&self, account_id: u32, folder_path: &str, messages: &[Message]) {
        let run = || -> rusqlite::Result<()> {
            let tx = self.conn.unchecked_transaction()?;
            for m in messages {
                // Upsert rather than REPLACE so an empty preview cannot erase one
                // already stored: the background backfill re-fetches summaries
                // without asking for a body slice, and every message it touched
                // lost its preview.
                tx.execute(
                    "INSERT INTO messages
                     (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, recipients, cc, message_id, references_, preview, reply_to, keywords)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
                     ON CONFLICT(account_id, folder_path, uid) DO UPDATE SET
                       from_name = excluded.from_name,
                       from_addr = excluded.from_addr,
                       subject = excluded.subject,
                       date = excluded.date,
                       ts = excluded.ts,
                       unread = excluded.unread,
                       starred = excluded.starred,
                       has_attachment = excluded.has_attachment,
                       recipients = excluded.recipients,
                       cc = excluded.cc,
                       message_id = excluded.message_id,
                       references_ = excluded.references_,
                       preview = CASE WHEN excluded.preview = '' THEN messages.preview ELSE excluded.preview END,
                       reply_to = excluded.reply_to,
                       keywords = excluded.keywords",
                    params![
                        account_id, folder_path, m.uid, m.from_name, m.from_addr, m.subject,
                        m.date, m.timestamp, m.unread, m.starred, m.has_attachment, m.to, m.cc,
                        m.message_id, m.references, m.preview, m.reply_to, m.keywords.join(" ")
                    ],
                )?;
            }
            tx.commit()
        };
        if let Err(e) = run() {
            tracing::warn!("cache upsert_messages failed: {e}");
        }
    }

    /// The set of message UIDs already cached for a folder (for backfill diffing).
    pub fn cached_uids(&self, account_id: u32, folder_path: &str) -> std::collections::HashSet<u32> {
        let run = || -> rusqlite::Result<std::collections::HashSet<u32>> {
            let mut stmt = self.conn.prepare(
                "SELECT uid FROM messages WHERE account_id = ?1 AND folder_path = ?2",
            )?;
            let rows = stmt.query_map(params![account_id, folder_path], |row| row.get::<_, u32>(0))?;
            rows.collect()
        };
        run().unwrap_or_default()
    }

    /// Every cached message across the account's folders that belongs to the same
    /// conversation as `ids` — messages naming one of those ids as their own
    /// Message-ID (an ancestor), or referencing one (a descendant, e.g. the reply
    /// you sent, filed away in Sent).
    ///
    /// Returns each message with the folder path it lives in, since the caller
    /// needs that both to label it and to fetch its body. Cache-only: the point
    /// is to assemble a conversation without going near the network (#21).
    /// A message's age is not a factor — what it answers is written in its
    /// headers whenever it was sent. [`THREAD_MEMBER_LIMIT`] caps what one
    /// conversation can drag in, which is the bound that matters: every member
    /// found is a body the reader will load and render.
    pub fn messages_by_thread_ids(
        &self,
        account_id: u32,
        ids: &[String],
    ) -> Vec<(String, Message)> {
        let ids: Vec<&String> = ids.iter().filter(|i| !i.is_empty()).take(THREAD_ID_LIMIT).collect();
        if ids.is_empty() {
            return Vec::new();
        }
        // One placeholder per id, twice: matched against message_id, then looked
        // for inside the space-separated references list.
        let slots = |offset: usize| -> String {
            (0..ids.len()).map(|i| format!("?{}", offset + i + 1)).collect::<Vec<_>>().join(", ")
        };
        // Binds are pushed ids-then-padded-then-account, so the padded copies sit
        // at n+1..2n. Reading from n+2 shifted every one of these by a slot and
        // ran the last comparison against the *account id* — which, coerced to
        // text, matched every message whose References contained that digit. A
        // one-message lookup came back with thousands.
        let refs_match = (0..ids.len())
            .map(|i| format!("instr(' ' || references_ || ' ', ?{}) > 0", ids.len() + i + 1))
            .collect::<Vec<_>>()
            .join(" OR ");
        let sql = format!(
            "SELECT folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred,                     has_attachment, recipients, cc, message_id, references_, preview, reply_to, {keywords}              FROM messages             WHERE account_id = ?{account} AND (message_id IN ({in_list}) OR {refs_match})              ORDER BY ts DESC LIMIT ?{limit}",
            keywords = KEYWORDS_COL,
            account = ids.len() * 2 + 1,
            limit = ids.len() * 2 + 2,
            in_list = slots(0),
            refs_match = refs_match,
        );
        let run = || -> rusqlite::Result<Vec<(String, Message)>> {
            let mut stmt = self.conn.prepare(&sql)?;
            // Bind order: the ids themselves, then each padded with spaces so a
            // substring match can't half-match a longer id, then the account.
            let padded: Vec<String> = ids.iter().map(|i| format!(" {i} ")).collect();
            let mut binds: Vec<&dyn rusqlite::ToSql> = Vec::new();
            for i in &ids {
                binds.push(*i as &dyn rusqlite::ToSql);
            }
            for p in &padded {
                binds.push(p as &dyn rusqlite::ToSql);
            }
            binds.push(&account_id as &dyn rusqlite::ToSql);
            let limit = THREAD_MEMBER_LIMIT as i64;
            binds.push(&limit as &dyn rusqlite::ToSql);
            let rows = stmt.query_map(binds.as_slice(), |row| {
                let uid: u32 = row.get(1)?;
                let mut m = Message {
                        id: uid,
                        account_id,
                        folder_id: 0, // filled in by the caller, which knows the ids
                        uid,
                        from_name: row.get(2)?,
                        from_addr: row.get(3)?,
                        reply_to: row.get(15)?,
                        to: row.get(10)?,
                        cc: row.get(11)?,
                        subject: row.get(4)?,
                        preview: row.get(14)?,
                        body: String::new(),
                        date: row.get(5)?,
                        timestamp: row.get(6)?,
                        unread: row.get(7)?,
                        starred: row.get(8)?,
                        keywords: split_keywords(row.get(16)?),
                        has_attachment: row.get(9)?,
                        message_id: row.get(12)?,
                        references: row.get(13)?,
                };
                m.scrub_nuls();
                Ok((row.get::<_, String>(0)?, m))
            })?;
            rows.collect()
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache messages_by_thread_ids failed: {e}");
            Vec::new()
        })
    }

    /// The next chunk of *replies* whose threading references are still the
    /// In-Reply-To-only ones an ENVELOPE fetch gives — a single id, or none.
    ///
    /// Replies only, and that is the whole economy of this: a message that opens
    /// a conversation has no References and never will, so asking the server for
    /// its header buys nothing. On a real 71k-message cache 68k rows hold a thin
    /// reference and only 3.9k of them are replies, so targeting them turns days
    /// of fetching into minutes.
    ///
    /// "None at all" cannot be told from a message that genuinely has no
    /// References, hence the `next_uid` watermark: each pass walks strictly
    /// downwards, so a message with nothing to find is visited once and never
    /// again.
    pub fn uids_needing_references(
        &self,
        account_id: u32,
        folder_path: &str,
        below_uid: u32,
        limit: usize,
    ) -> Vec<(u32, String)> {
        let run = || -> rusqlite::Result<Vec<(u32, String)>> {
            let mut stmt = self.conn.prepare(
                "SELECT uid, references_ FROM messages \
                 WHERE account_id = ?1 AND folder_path = ?2 AND uid < ?3 \
                   AND instr(trim(references_), ' ') = 0 \
                   AND (references_ <> '' OR subject LIKE 'Re:%' OR subject LIKE 'Fwd:%' \
                        OR subject LIKE 'Fw:%' OR subject LIKE 'Aw:%' OR subject LIKE 'Sv:%') \
                 ORDER BY uid DESC LIMIT ?4",
            )?;
            let rows = stmt.query_map(
                params![account_id, folder_path, below_uid, limit as i64],
                |row| Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?)),
            )?;
            rows.collect()
        };
        run().unwrap_or_default()
    }

    pub fn set_references(&self, account_id: u32, folder_path: &str, uid: u32, references: &str) {
        let _ = self.conn.execute(
            "UPDATE messages SET references_ = ?1 \
             WHERE account_id = ?2 AND folder_path = ?3 AND uid = ?4",
            params![references, account_id, folder_path, uid],
        );
    }

    /// How far down a folder's References repair has walked: the uid to look
    /// below next, and whether the pass has finished. A folder never touched
    /// starts at the top (`u32::MAX`), so the first chunk is its newest mail.
    pub fn refs_repair_state(&self, account_id: u32, folder_path: &str) -> (u32, bool) {
        self.conn
            .query_row(
                "SELECT next_uid, done FROM refs_repair \
                 WHERE account_id = ?1 AND folder_path = ?2",
                params![account_id, folder_path],
                |row| Ok((row.get::<_, u32>(0)?, row.get::<_, i64>(1)? != 0)),
            )
            .unwrap_or((u32::MAX, false))
    }

    pub fn set_refs_repair_state(
        &self,
        account_id: u32,
        folder_path: &str,
        next_uid: u32,
        done: bool,
    ) {
        let _ = self.conn.execute(
            "INSERT INTO refs_repair (account_id, folder_path, next_uid, done) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(account_id, folder_path) DO UPDATE SET next_uid = ?3, done = ?4",
            params![account_id, folder_path, next_uid, done as i64],
        );
    }

    pub fn load_body(&self, account_id: u32, folder_path: &str, uid: u32) -> Option<String> {
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

    pub fn save_body(&self, account_id: u32, folder_path: &str, uid: u32, body: &str) {
        if let Err(e) = self.conn.execute(
            "INSERT OR REPLACE INTO bodies (account_id, folder_path, uid, body) VALUES (?1, ?2, ?3, ?4)",
            params![account_id, folder_path, uid, body],
        ) {
            tracing::warn!("cache save_body failed: {e}");
        }
    }

    /// The cached sender-authentication verdict for a message, if one was stored
    /// when its body was fetched.
    pub fn load_sender_check(
        &self,
        account_id: u32,
        folder_path: &str,
        uid: u32,
    ) -> Option<crate::models::SenderCheck> {
        self.sender_check_of(account_id, folder_path, uid).or_else(|| {
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
                "SELECT trust, summary, findings, pgp FROM sender_checks \
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
                    })
                },
            )
            .ok()
    }

    pub fn save_sender_check(
        &self,
        account_id: u32,
        folder_path: &str,
        uid: u32,
        check: &crate::models::SenderCheck,
    ) {
        if let Err(e) = self.conn.execute(
            "INSERT OR REPLACE INTO sender_checks \
             (account_id, folder_path, uid, trust, summary, findings, pgp) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
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
                    .unwrap_or_default()
            ],
        ) {
            tracing::warn!("cache save_sender_check failed: {e}");
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
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
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

    /// One page of the attachments gallery, filtered and ordered by the
    /// database rather than in the UI: with two decades of archive indexed
    /// there are far more attachments than any window can hold, so the scope,
    /// the search and the sort all have to narrow the rows *before* the page is
    /// cut, or paging would show an arbitrary slice of the wrong set.
    ///
    /// Rows come from `attachment_meta` — everything known to exist — left
    /// joined to `attachments`, which holds the few whose bytes were actually
    /// downloaded. Bytes ride along only for a cached file under `data_cap`, so
    /// a page stays small however far back it reaches.
    pub fn gallery_page(&self, q: &GalleryQuery) -> Vec<crate::models::GalleryItem> {
        // Three leading parameters of our own, so the scope starts at ?4.
        let (where_sql, params) = q.where_clause(4);
        let sql = format!(
            "SELECT am.account_id, am.folder_path, am.uid, am.idx, am.name, am.size, \
                    COALESCE(m.from_name, ''), COALESCE(m.subject, ''), COALESCE(m.ts, 0), \
                    CASE WHEN a.data IS NOT NULL AND LENGTH(a.data) <= ?1 THEN a.data ELSE NULL END, \
                    a.uid IS NOT NULL \
             FROM attachment_meta am \
             LEFT JOIN messages m \
               ON m.account_id = am.account_id AND m.folder_path = am.folder_path AND m.uid = am.uid \
             LEFT JOIN attachments a \
               ON a.account_id = am.account_id AND a.folder_path = am.folder_path \
              AND a.uid = am.uid AND a.idx = am.idx \
             {where_sql} ORDER BY {} LIMIT ?2 OFFSET ?3",
            order_by(q.sort),
        );
        let run = || -> rusqlite::Result<Vec<crate::models::GalleryItem>> {
            let mut stmt = self.conn.prepare(&sql)?;
            let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&q.data_cap, &q.limit, &q.offset];
            bound.extend(params.iter().map(|p| p as &dyn rusqlite::ToSql));
            let rows = stmt.query_map(bound.as_slice(), |row| {
                Ok(crate::models::GalleryItem {
                    account_id: row.get(0)?,
                    folder_path: row.get(1)?,
                    uid: row.get(2)?,
                    name: row.get(4)?,
                    size: row.get::<_, i64>(5)?.max(0) as u64,
                    from_name: row.get(6)?,
                    subject: row.get(7)?,
                    timestamp: row.get(8)?,
                    data: row.get(9)?,
                    downloaded: row.get(10)?,
                })
            })?;
            rows.collect()
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache gallery_page failed: {e}");
            Vec::new()
        })
    }

    /// How many attachments the same query matches in total — what the footer
    /// counts up to, and how the UI knows whether another page exists.
    pub fn gallery_total(&self, q: &GalleryQuery) -> u32 {
        let (where_sql, params) = q.where_clause(1);
        let sql = format!(
            "SELECT COUNT(*) FROM attachment_meta am \
             LEFT JOIN messages m \
               ON m.account_id = am.account_id AND m.folder_path = am.folder_path AND m.uid = am.uid \
             {where_sql}"
        );
        let run = || -> rusqlite::Result<i64> {
            let bound: Vec<&dyn rusqlite::ToSql> =
                params.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
            self.conn.query_row(&sql, bound.as_slice(), |r| r.get(0))
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache gallery_total failed: {e}");
            0
        }) as u32
    }

    /// Record what a message's attachments are, without their bytes: one row
    /// per attachment plus a scan mark, so a message with none is never asked
    /// about again. Replaces any earlier answer for that message.
    pub fn save_attachment_meta(
        &self,
        account_id: u32,
        folder_path: &str,
        uid: u32,
        metas: &[crate::models::AttachmentMeta],
    ) {
        let _ = self.conn.execute(
            "DELETE FROM attachment_meta WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
            params![account_id, folder_path, uid],
        );
        for m in metas {
            if let Err(e) = self.conn.execute(
                "INSERT OR REPLACE INTO attachment_meta \
                 (account_id, folder_path, uid, idx, name, mime, size, section, ext, bucket, keywords) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    account_id,
                    folder_path,
                    uid,
                    m.idx,
                    &m.name,
                    &m.mime,
                    m.size as i64,
                    &m.section,
                    crate::models::ext_of(&m.name),
                    crate::models::type_bucket(&m.name),
                    crate::models::type_keywords(&m.name),
                ],
            ) {
                tracing::warn!("cache save_attachment_meta failed: {e}");
            }
        }
        let _ = self.conn.execute(
            "INSERT OR IGNORE INTO attachment_scan (account_id, folder_path, uid) VALUES (?1, ?2, ?3)",
            params![account_id, folder_path, uid],
        );
    }

    /// The next `limit` messages in a folder that carry an attachment and have
    /// not been scanned yet, newest first — the gallery's backfill work queue.
    pub fn unscanned_attachment_uids(
        &self,
        account_id: u32,
        folder_path: &str,
        limit: u32,
    ) -> Vec<u32> {
        let run = || -> rusqlite::Result<Vec<u32>> {
            let mut stmt = self.conn.prepare(
                "SELECT m.uid FROM messages m \
                 WHERE m.account_id = ?1 AND m.folder_path = ?2 AND m.has_attachment = 1 \
                   AND NOT EXISTS (SELECT 1 FROM attachment_scan s \
                       WHERE s.account_id = m.account_id AND s.folder_path = m.folder_path \
                         AND s.uid = m.uid) \
                 ORDER BY m.ts DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![account_id, folder_path, limit], |r| r.get(0))?;
            rows.collect()
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache unscanned_attachment_uids failed: {e}");
            Vec::new()
        })
    }

    /// How many attachment-carrying messages in a folder are still unscanned —
    /// what the gallery reports as work outstanding.
    pub fn unscanned_attachment_count(&self, account_id: u32, folder_path: &str) -> u32 {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM messages m \
                 WHERE m.account_id = ?1 AND m.folder_path = ?2 AND m.has_attachment = 1 \
                   AND NOT EXISTS (SELECT 1 FROM attachment_scan s \
                       WHERE s.account_id = m.account_id AND s.folder_path = m.folder_path \
                         AND s.uid = m.uid)",
                params![account_id, folder_path],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0) as u32
    }

    pub fn load_attachments(&self, account_id: u32, folder_path: &str, uid: u32) -> Vec<Attachment> {
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

    pub fn save_attachments(
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
    pub fn mark_attachments_checked(&self, account_id: u32, folder_path: &str, uid: u32) {
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
    pub fn attachments_checked(&self, account_id: u32, folder_path: &str, uid: u32) -> bool {
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

    pub fn set_unread(&self, account_id: u32, folder_path: &str, uid: u32, unread: bool) {
        let _ = self.conn.execute(
            "UPDATE messages SET unread = ?1 WHERE account_id = ?2 AND folder_path = ?3 AND uid = ?4",
            params![unread, account_id, folder_path, uid],
        );
    }

    /// Record recipients the user has sent to, so they autocomplete even before
    /// the Sent folder syncs. Each send bumps the address's frequency. The app
    /// records at the moment of sending, whatever route the message then
    /// takes (straight out, the Outbox, Send Later).
    pub fn record_addresses(&self, entries: &[(String, String)]) {
        for (name, email) in entries {
            let email = email.trim().to_lowercase();
            if email.is_empty() || !email.contains('@') {
                continue;
            }
            if let Err(e) = self.conn.execute(
                "INSERT INTO addresses(email, name, count) VALUES(?1, ?2, 1) \
                 ON CONFLICT(email) DO UPDATE SET count = count + 1, \
                   name = CASE WHEN excluded.name <> '' THEN excluded.name ELSE addresses.name END",
                params![email, name.trim()],
            ) {
                tracing::warn!("could not record recipient {email}: {e}");
            }
        }
    }

    // -----------------------------------------------------------------------
    // Outbox: messages that could not be sent yet
    // -----------------------------------------------------------------------

    /// Queue a message that failed to send. Returns its Outbox id.
    #[allow(clippy::too_many_arguments)]
    pub fn queue_outbox(
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

    /// Everything waiting for this account, oldest first (the order it is sent in).
    pub fn outbox_items(&self, account_id: u32) -> Vec<crate::models::OutboxItem> {
        let mut stmt = match self.conn.prepare(
            "SELECT id, account_id, from_addr, rcpts, recipients, subject, preview, raw, \
             sent_path, queued_at, attempts, last_error, send_at FROM outbox \
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

    /// Correct a message's attachment flag once its body has proved the answer,
    /// so the paperclip survives a restart rather than being re-guessed.
    pub fn set_has_attachment(&self, account_id: u32, folder_path: &str, uid: u32, has: bool) {
        let _ = self.conn.execute(
            "UPDATE messages SET has_attachment = ?4 \
             WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
            params![account_id, folder_path, uid, has],
        );
    }

    /// Drop a queued message — it went out, or the user discarded it.
    pub fn delete_outbox(&self, id: u32) {
        let _ = self.conn.execute("DELETE FROM outbox WHERE id = ?1", params![id]);
    }

    /// Record another failed attempt, so the list can say what went wrong.
    pub fn record_outbox_failure(&self, id: u32, error: &str) {
        let _ = self.conn.execute(
            "UPDATE outbox SET attempts = attempts + 1, last_error = ?2 WHERE id = ?1",
            params![id, error],
        );
    }

    /// Aggregate every address seen in stored mail (senders received +
    /// recipients sent/recorded), as (name, email, frequency), most-frequent first.
    pub fn address_history(&self) -> Vec<(String, String, u32)> {
        use std::collections::HashMap;
        let mut counts: HashMap<String, (String, u32)> = HashMap::new();
        fn bump(counts: &mut HashMap<String, (String, u32)>, name: &str, email: &str, n: u32) {
            let email = email.trim();
            if email.is_empty() || !email.contains('@') {
                return;
            }
            let key = email.to_lowercase();
            let entry = counts.entry(key).or_insert_with(|| (String::new(), 0));
            entry.1 += n;
            if entry.0.is_empty() && !name.trim().is_empty() && name.trim() != email {
                entry.0 = name.trim().to_string();
            }
            if entry.0.is_empty() {
                entry.0 = email.to_string();
            }
        }

        if let Ok(mut stmt) = self
            .conn
            .prepare("SELECT from_name, from_addr, recipients, cc FROM messages")
        {
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0).unwrap_or_default(),
                    r.get::<_, String>(1).unwrap_or_default(),
                    r.get::<_, String>(2).unwrap_or_default(),
                    r.get::<_, String>(3).unwrap_or_default(),
                ))
            });
            if let Ok(rows) = rows {
                for (from_name, from_addr, to, cc) in rows.flatten() {
                    bump(&mut counts, &from_name, &from_addr, 1);
                    for list in [to, cc] {
                        for addr in list.split(',') {
                            bump(&mut counts, addr.trim(), addr.trim(), 1);
                        }
                    }
                }
            }
        }

        if let Ok(mut stmt) = self.conn.prepare("SELECT name, email, count FROM addresses") {
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0).unwrap_or_default(),
                    r.get::<_, String>(1).unwrap_or_default(),
                    r.get::<_, u32>(2).unwrap_or(0),
                ))
            });
            if let Ok(rows) = rows {
                for (name, email, c) in rows.flatten() {
                    bump(&mut counts, &name, &email, c);
                }
            }
        }

        let mut out: Vec<(String, String, u32)> = counts
            .into_iter()
            .map(|(email, (name, n))| (name, email, n))
            .collect();
        out.sort_by(|a, b| b.2.cmp(&a.2));
        out
    }

    pub fn mark_folder_read(&self, account_id: u32, folder_path: &str) {
        let _ = self.conn.execute(
            "UPDATE messages SET unread = 0 WHERE account_id = ?1 AND folder_path = ?2",
            params![account_id, folder_path],
        );
    }

    pub fn set_starred(&self, account_id: u32, folder_path: &str, uid: u32, starred: bool) {
        let _ = self.conn.execute(
            "UPDATE messages SET starred = ?1 WHERE account_id = ?2 AND folder_path = ?3 AND uid = ?4",
            params![starred, account_id, folder_path, uid],
        );
    }

    /// Add or drop a keyword on a cached row (the server copy just changed).
    pub fn set_keyword(&self, account_id: u32, folder_path: &str, uid: u32, keyword: &str, add: bool) -> bool {
        let mut current = self.keywords_of(account_id, folder_path, uid);
        current.retain(|k| !k.eq_ignore_ascii_case(keyword));
        if add {
            current.push(keyword.to_string());
        }
        self.conn
            .execute(
                "UPDATE messages SET keywords = ?1 WHERE account_id = ?2 AND folder_path = ?3 AND uid = ?4",
                params![current.join(" "), account_id, folder_path, uid],
            )
            .is_ok_and(|n| n > 0)
    }

    /// The uids in one folder whose cached server-side keywords include
    /// `keyword` (the keyword re-sync of #166 diffs this against the
    /// server's own search).
    pub fn uids_with_keyword(&self, account_id: u32, folder_path: &str, keyword: &str) -> Vec<u32> {
        let needle = format!(" {} ", keyword.to_ascii_lowercase());
        let run = || -> rusqlite::Result<Vec<u32>> {
            let mut stmt = self.conn.prepare(
                "SELECT uid FROM messages \
                 WHERE account_id = ?1 AND folder_path = ?2 \
                   AND instr(' ' || lower(keywords) || ' ', ?3) > 0",
            )?;
            let rows = stmt.query_map(params![account_id, folder_path, needle], |r| r.get(0))?;
            rows.collect()
        };
        run().unwrap_or_default()
    }

    /// The server-side keywords cached for one message (local tags excluded).
    pub fn keywords_of(&self, account_id: u32, folder_path: &str, uid: u32) -> Vec<String> {
        self.conn
            .query_row(
                "SELECT keywords FROM messages WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
                params![account_id, folder_path, uid],
                |r| r.get::<_, String>(0),
            )
            .map(split_keywords)
            .unwrap_or_default()
    }

    /// Add or drop a tag kept in Hylki only (#71), for accounts whose server
    /// can't hold it: POP3, or IMAP without `\*` in PERMANENTFLAGS. Keyed by
    /// Message-ID so the tag follows the message between folders and
    /// survives a re-sync of the row.
    pub fn set_local_tag(&self, account_id: u32, message_id: &str, keyword: &str, add: bool) {
        if message_id.is_empty() {
            return;
        }
        let _ = if add {
            self.conn.execute(
                "INSERT OR IGNORE INTO local_tags (account_id, message_id, keyword) VALUES (?1, ?2, ?3)",
                params![account_id, message_id, keyword],
            )
        } else {
            self.conn.execute(
                "DELETE FROM local_tags WHERE account_id = ?1 AND message_id = ?2 AND lower(keyword) = lower(?3)",
                params![account_id, message_id, keyword],
            )
        };
    }

    /// Fold the account's locally-kept tags into freshly fetched summaries, so
    /// what the worker hands the app matches what a cache load would show.
    pub fn apply_local_tags(&self, account_id: u32, messages: &mut [Message]) {
        let run = || -> rusqlite::Result<Vec<(String, String)>> {
            let mut stmt = self
                .conn
                .prepare("SELECT message_id, keyword FROM local_tags WHERE account_id = ?1")?;
            let rows = stmt.query_map(params![account_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        };
        let local = run().unwrap_or_default();
        if local.is_empty() {
            return;
        }
        for m in messages.iter_mut().filter(|m| !m.message_id.is_empty()) {
            for (_, kw) in local.iter().filter(|(id, _)| *id == m.message_id) {
                if !m.has_keyword(kw) {
                    m.keywords.push(kw.clone());
                }
            }
        }
    }

    /// How many cached messages of the account carry `keyword` (server or
    /// local), for the tag finder's report where the server itself gives no
    /// count (Microsoft 365 categories).
    pub fn count_with_keyword(&self, account_id: u32, keyword: &str) -> usize {
        let needle = format!(" {} ", keyword.to_ascii_lowercase());
        let sql = format!(
            "SELECT count(*) FROM messages \
             WHERE account_id = ?1 AND instr(' ' || lower({KEYWORDS_COL}) || ' ', ?2) > 0"
        );
        self.conn
            .query_row(&sql, params![account_id, needle], |row| row.get::<_, i64>(0))
            .map(|n| n.max(0) as usize)
            .unwrap_or(0)
    }

    /// Every cached message of the account carrying `keyword` — on the server
    /// or locally — newest first, with the folder each sits in. Backs the
    /// sidebar's tag views (#71); the caller maps paths to folder ids and
    /// drops the copies Gmail keeps per label.
    pub fn messages_with_keyword(&self, account_id: u32, keyword: &str) -> Vec<(String, Message)> {
        let needle = format!(" {} ", keyword.to_ascii_lowercase());
        let sql = format!(
            "SELECT folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, \
                    has_attachment, recipients, cc, message_id, references_, preview, reply_to, {KEYWORDS_COL} \
             FROM messages \
             WHERE account_id = ?1 AND instr(' ' || lower({KEYWORDS_COL}) || ' ', ?2) > 0 \
             ORDER BY ts DESC LIMIT ?3"
        );
        let run = || -> rusqlite::Result<Vec<(String, Message)>> {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params![account_id, needle, TAG_VIEW_LIMIT], |row| {
                let uid: u32 = row.get(1)?;
                let mut m = Message {
                    id: uid,
                    account_id,
                    folder_id: 0, // filled in by the caller, which knows the ids
                    uid,
                    from_name: row.get(2)?,
                    from_addr: row.get(3)?,
                    reply_to: row.get(15)?,
                    to: row.get(10)?,
                    cc: row.get(11)?,
                    subject: row.get(4)?,
                    preview: row.get(14)?,
                    body: String::new(),
                    date: row.get(5)?,
                    timestamp: row.get(6)?,
                    unread: row.get(7)?,
                    starred: row.get(8)?,
                    keywords: split_keywords(row.get(16)?),
                    has_attachment: row.get(9)?,
                    message_id: row.get(12)?,
                    references: row.get(13)?,
                };
                m.scrub_nuls();
                Ok((row.get::<_, String>(0)?, m))
            })?;
            rows.collect()
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache messages_with_keyword failed: {e}");
            Vec::new()
        })
    }

    pub fn delete_message(&self, account_id: u32, folder_path: &str, uid: u32) {
        let _ = self.conn.execute(
            "DELETE FROM messages WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
            params![account_id, folder_path, uid],
        );
        let _ = self.conn.execute(
            "DELETE FROM bodies WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
            params![account_id, folder_path, uid],
        );
        let _ = self.conn.execute(
            "DELETE FROM attachments WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
            params![account_id, folder_path, uid],
        );
        let _ = self.conn.execute(
            "DELETE FROM attachments_checked WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
            params![account_id, folder_path, uid],
        );
    }
}

/// Compare folder lists ignoring the volatile id, but including unread counts so
/// a changed count re-emits the list and refreshes the sidebar badges.
pub fn folders_equal(a: &[Folder], b: &[Folder]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.path == y.path && x.name == y.name && x.kind == y.kind && x.unread == y.unread
        })
}

fn kind_to_i64(kind: FolderKind) -> i64 {
    match kind {
        FolderKind::Inbox => 0,
        FolderKind::Starred => 1,
        FolderKind::Sent => 2,
        FolderKind::Drafts => 3,
        FolderKind::Archive => 4,
        FolderKind::Junk => 5,
        FolderKind::Trash => 6,
        FolderKind::Custom => 7,
    }
}

fn kind_from_i64(v: i64) -> FolderKind {
    match v {
        0 => FolderKind::Inbox,
        1 => FolderKind::Starred,
        2 => FolderKind::Sent,
        3 => FolderKind::Drafts,
        4 => FolderKind::Archive,
        5 => FolderKind::Junk,
        6 => FolderKind::Trash,
        _ => FolderKind::Custom,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cache holds message bodies, attachment bytes and the address book, so
    /// it should be no more readable than `accounts.toml` is.
    ///
    /// Ignored by default: it sets `XDG_DATA_HOME`, which is process-global and
    /// would leak into tests running beside it. Run it on its own:
    ///
    /// ```text
    /// cargo test -- --ignored --test-threads=1 the_cache_is_not_world_readable
    /// ```
    #[test]
    #[ignore = "mutates process-global environment"]
    #[cfg(unix)]
    fn the_cache_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;

        let base = std::env::temp_dir().join(format!("hylki-cache-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var("XDG_DATA_HOME", &base);

        let dir = base.join("hylki");
        // Start from the permissions the old code left behind, to prove an
        // existing cache is tightened rather than only a freshly created one.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("cache.db"), b"").unwrap();
        std::fs::set_permissions(dir.join("cache.db"), std::fs::Permissions::from_mode(0o644))
            .unwrap();

        let cache = Cache::open().expect("cache opens");
        drop(cache);

        let mode = |p: &std::path::Path| {
            std::fs::metadata(p).unwrap().permissions().mode() & 0o777
        };
        assert_eq!(mode(&dir), 0o700, "cache directory");
        assert_eq!(mode(&dir.join("cache.db")), 0o600, "cache.db");
        for side in ["cache.db-wal", "cache.db-shm"] {
            let p = dir.join(side);
            if p.exists() {
                assert_eq!(mode(&p), 0o600, "{side}");
            }
        }

        std::env::remove_var("XDG_DATA_HOME");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn outbox_round_trips_a_queued_message() {
        let c = Cache::in_memory().unwrap();
        let rcpts = vec!["ada@example.com".to_string(), "bcc@example.com".to_string()];
        let id = c
            .queue_outbox(
                1,
                "me@example.com",
                &rcpts,
                "Ada Lovelace <ada@example.com>",
                "Notes",
                "the body preview",
                b"From: me\r\n\r\nbody",
                Some("Sent"),
                "connection refused",
                None,
            )
            .expect("queued");

        let items = c.outbox_items(1);
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.id, id);
        // The envelope has to survive verbatim — Bcc exists only here, so losing a
        // recipient silently drops someone from the message.
        assert_eq!(item.rcpts, rcpts);
        assert_eq!(item.from_addr, "me@example.com");
        assert_eq!(item.subject, "Notes");
        assert_eq!(item.preview, "the body preview");
        assert_eq!(item.raw, b"From: me\r\n\r\nbody");
        assert_eq!(item.sent_path.as_deref(), Some("Sent"));
        assert_eq!(item.attempts, 1);
        assert_eq!(item.last_error, "connection refused");
        assert!(item.queued_at > 0);

        // Another account's queue is its own.
        assert!(c.outbox_items(2).is_empty());

        c.record_outbox_failure(id, "no route to host");
        let item = c.outbox_items(1).remove(0);
        assert_eq!(item.attempts, 2);
        assert_eq!(item.last_error, "no route to host");

        c.delete_outbox(id);
        assert!(c.outbox_items(1).is_empty());
    }

    /// Send Later (#145): the scheduled time survives the round trip, and a
    /// message queued to go now has none.
    #[test]
    fn outbox_keeps_a_scheduled_time() {
        let c = Cache::in_memory().unwrap();
        let rcpts = vec!["ada@example.com".to_string()];
        let later = c
            .queue_outbox(1, "me@example.com", &rcpts, "ada", "Later", "", b"raw", None, "", Some(1_900_000_000))
            .expect("queued");
        let now = c
            .queue_outbox(1, "me@example.com", &rcpts, "ada", "Now", "", b"raw", None, "", None)
            .expect("queued");
        let items = c.outbox_items(1);
        let by = |id: u32| items.iter().find(|i| i.id == id).expect("listed");
        assert_eq!(by(later).send_at, Some(1_900_000_000));
        assert_eq!(by(now).send_at, None);
        assert!(by(later).as_message().preview.starts_with("Scheduled for "), "{}", by(later).as_message().preview);
    }

    #[test]
    fn outbox_keeps_the_order_messages_were_queued_in() {
        let c = Cache::in_memory().unwrap();
        for (subject, at) in [("second", 200), ("first", 100), ("third", 300)] {
            c.conn
                .execute(
                    "INSERT INTO outbox(account_id, from_addr, rcpts, recipients, subject, \
                     preview, raw, sent_path, queued_at, attempts, last_error) \
                     VALUES(1, 'me@example.com', 'a@b.com', '', ?1, '', x'00', NULL, ?2, 1, '')",
                    params![subject, at],
                )
                .unwrap();
        }
        let subjects: Vec<String> = c.outbox_items(1).into_iter().map(|i| i.subject).collect();
        assert_eq!(subjects, ["first", "second", "third"]);
    }

    fn add_msg(c: &Cache, folder: &str, uid: u32, from: &str, subject: &str, ts: i64) {
        c.conn.execute(
            "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment) \
             VALUES (1, ?1, ?2, ?3, '', ?4, '', ?5, 0, 0, 1)",
            params![folder, uid, from, subject, ts],
        ).unwrap();
    }
    fn add_att(c: &Cache, folder: &str, uid: u32, idx: u32, name: &str, data: &[u8]) {
        c.conn.execute(
            "INSERT INTO attachments (account_id, folder_path, uid, idx, name, data) VALUES (1, ?1, ?2, ?3, ?4, ?5)",
            params![folder, uid, idx, name, data],
        ).unwrap();
    }
    fn add_folder(c: &Cache, path: &str, kind: FolderKind) {
        c.conn.execute(
            "INSERT INTO folders (account_id, path, name, kind, unread, ord) VALUES (1, ?1, ?1, ?2, 0, 0)",
            params![path, kind_to_i64(kind)],
        ).unwrap();
    }

    /// Describe an attachment without downloading it, as the scan does.
    fn add_meta(c: &Cache, folder: &str, uid: u32, idx: u32, name: &str, size: u64) {
        c.save_attachment_meta(
            1,
            folder,
            uid,
            &[crate::models::AttachmentMeta {
                idx,
                name: name.to_string(),
                mime: String::new(),
                size,
                section: format!("{}", idx + 1),
            }],
        );
    }

    /// A query over everything, ordered newest first.
    fn all(sort: GallerySort, limit: u32, offset: u32) -> GalleryQuery<'static> {
        GalleryQuery {
            folders: &[],
            account_id: None,
            tokens: &[],
            bucket: 0,
            sort,
            limit,
            offset,
            data_cap: 10,
        }
    }

    /// The gallery lists what the scan found, whether or not the bytes were
    /// ever downloaded — the whole point of the metadata tier.
    #[test]
    fn the_gallery_lists_attachments_that_were_never_downloaded() {
        let c = Cache::in_memory().unwrap();
        add_folder(&c, "INBOX", FolderKind::Inbox);
        add_folder(&c, "Archive", FolderKind::Archive);
        add_msg(&c, "INBOX", 1, "Alice", "Hi", 100);
        add_msg(&c, "Archive", 2, "Bob", "Report", 200);
        add_meta(&c, "INBOX", 1, 0, "a.png", 4);
        add_meta(&c, "Archive", 2, 0, "old.pdf", 900_000);
        // Only the inbox one has ever been fetched.
        add_att(&c, "INBOX", 1, 0, "a.png", &[0u8; 4]);

        let items = c.gallery_page(&all(GallerySort::Newest, 50, 0));
        assert_eq!(items.len(), 2);
        // Newest message first: Archive/Bob (200) before Inbox/Alice (100).
        assert_eq!(items[0].name, "old.pdf");
        assert!(!items[0].downloaded, "never fetched");
        assert!(items[0].data.is_none());
        assert_eq!(items[0].size, 900_000, "size comes from the scan, not the blob");
        assert_eq!(items[1].name, "a.png");
        assert!(items[1].downloaded);
        assert_eq!(items[1].data.as_deref(), Some(&[0u8; 4][..]));
    }

    /// A cached file bigger than the page's cap is listed, and known to be
    /// downloaded, but its bytes stay behind — a page has to stay small however
    /// far back it reaches.
    #[test]
    fn a_cached_file_over_the_cap_is_listed_without_its_bytes() {
        let c = Cache::in_memory().unwrap();
        add_folder(&c, "INBOX", FolderKind::Inbox);
        add_msg(&c, "INBOX", 1, "Alice", "Hi", 100);
        add_meta(&c, "INBOX", 1, 0, "big.bin", 20);
        add_att(&c, "INBOX", 1, 0, "big.bin", &[0u8; 20]);

        let items = c.gallery_page(&all(GallerySort::Newest, 50, 0));
        assert_eq!(items.len(), 1);
        assert!(items[0].downloaded, "the bytes are in the cache");
        assert!(items[0].data.is_none(), "but over the cap, so not carried");
    }

    /// Paging must not drop or repeat a row. Every item has the same timestamp
    /// here, so only the tie-break keeps the order stable.
    #[test]
    fn paging_covers_every_row_exactly_once_even_when_the_sort_key_ties() {
        let c = Cache::in_memory().unwrap();
        add_folder(&c, "INBOX", FolderKind::Inbox);
        for uid in 1..=10 {
            add_msg(&c, "INBOX", uid, "X", "S", 500);
            add_meta(&c, "INBOX", uid, 0, "f.png", 2);
        }
        let mut seen: Vec<(u32, String)> = Vec::new();
        for page in 0..4 {
            for item in c.gallery_page(&all(GallerySort::Newest, 3, page * 3)) {
                seen.push((item.uid, item.name.clone()));
            }
        }
        assert_eq!(seen.len(), 10, "every row, once");
        let mut uids: Vec<u32> = seen.iter().map(|(u, _)| *u).collect();
        uids.sort_unstable();
        uids.dedup();
        assert_eq!(uids.len(), 10, "no row served twice");
        assert_eq!(c.gallery_total(&all(GallerySort::Newest, 3, 0)), 10);
    }

    /// The scope reaches the query, so a page is cut from the right set rather
    /// than filtered afterwards.
    #[test]
    fn the_folder_scope_narrows_the_query_itself() {
        let c = Cache::in_memory().unwrap();
        add_folder(&c, "INBOX", FolderKind::Inbox);
        add_folder(&c, "Sent", FolderKind::Sent);
        add_msg(&c, "INBOX", 1, "Alice", "Hi", 100);
        add_msg(&c, "Sent", 2, "Me", "Out", 200);
        add_meta(&c, "INBOX", 1, 0, "in.png", 2);
        add_meta(&c, "Sent", 2, 0, "out.png", 2);

        let inbox_only = [(1u32, "INBOX".to_string())];
        let q = GalleryQuery { folders: &inbox_only, ..all(GallerySort::Newest, 50, 0) };
        let items = c.gallery_page(&q);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "in.png");
        assert_eq!(c.gallery_total(&q), 1, "the count follows the same scope");
    }

    /// Search matches the filename, the sender, the subject and the type words
    /// stored beside each row — and every word has to match something.
    #[test]
    fn search_matches_across_the_row_and_its_message() {
        let c = Cache::in_memory().unwrap();
        add_folder(&c, "INBOX", FolderKind::Inbox);
        add_msg(&c, "INBOX", 1, "Dana Whitfield", "Quarter review", 100);
        add_meta(&c, "INBOX", 1, 0, "budget.xlsx", 2);
        add_msg(&c, "INBOX", 2, "Bob", "Holiday", 200);
        add_meta(&c, "INBOX", 2, 0, "beach.png", 2);

        let hit = |tokens: &[String]| {
            c.gallery_page(&GalleryQuery { tokens, ..all(GallerySort::Newest, 50, 0) }).len()
        };
        assert_eq!(hit(&["budget".to_string()]), 1, "filename");
        assert_eq!(hit(&["whitfield".to_string()]), 1, "sender");
        assert_eq!(hit(&["quarter".to_string()]), 1, "subject");
        assert_eq!(hit(&["spreadsheet".to_string()]), 1, "type keyword");
        assert_eq!(hit(&["image".to_string()]), 1, "type keyword, other row");
        assert_eq!(
            hit(&["dana".to_string(), "spreadsheet".to_string()]),
            1,
            "both words have to match"
        );
        assert_eq!(hit(&["dana".to_string(), "holiday".to_string()]), 0);
    }

    /// A search word is matched literally: SQL's own wildcards are not the
    /// user's to type by accident.
    #[test]
    fn a_search_word_cannot_smuggle_in_a_wildcard() {
        let c = Cache::in_memory().unwrap();
        add_folder(&c, "INBOX", FolderKind::Inbox);
        add_msg(&c, "INBOX", 1, "A", "S", 100);
        add_meta(&c, "INBOX", 1, 0, "report.pdf", 2);

        let wild = ["%".to_string()];
        let q = GalleryQuery { tokens: &wild, ..all(GallerySort::Newest, 50, 0) };
        assert_eq!(c.gallery_page(&q).len(), 0, "% matches a literal percent sign");
    }

    /// The type filter and the type sort both read the bucket stored with the
    /// row, so they agree with the UI's own dropdown.
    #[test]
    fn the_type_filter_uses_the_stored_bucket() {
        let c = Cache::in_memory().unwrap();
        add_folder(&c, "INBOX", FolderKind::Inbox);
        add_msg(&c, "INBOX", 1, "A", "S", 100);
        add_meta(&c, "INBOX", 1, 0, "shot.png", 2);
        add_msg(&c, "INBOX", 2, "B", "T", 200);
        add_meta(&c, "INBOX", 2, 0, "doc.pdf", 2);

        let images = GalleryQuery { bucket: 1, ..all(GallerySort::Newest, 50, 0) };
        let items = c.gallery_page(&images);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "shot.png");
        assert_eq!(c.gallery_total(&images), 1);
    }

    /// Each sort criterion orders by what it says it does.
    #[test]
    fn every_sort_criterion_orders_the_page() {
        let c = Cache::in_memory().unwrap();
        add_folder(&c, "INBOX", FolderKind::Inbox);
        add_msg(&c, "INBOX", 1, "Zoe", "S", 100);
        add_meta(&c, "INBOX", 1, 0, "apple.pdf", 900);
        add_msg(&c, "INBOX", 2, "Adam", "T", 200);
        add_meta(&c, "INBOX", 2, 0, "zebra.png", 50);

        let first = |sort| c.gallery_page(&all(sort, 50, 0))[0].name.clone();
        assert_eq!(first(GallerySort::Newest), "zebra.png");
        assert_eq!(first(GallerySort::Oldest), "apple.pdf");
        assert_eq!(first(GallerySort::Name), "apple.pdf");
        assert_eq!(first(GallerySort::NameDesc), "zebra.png");
        assert_eq!(first(GallerySort::Largest), "apple.pdf");
        assert_eq!(first(GallerySort::Smallest), "zebra.png");
        assert_eq!(first(GallerySort::Sender), "zebra.png", "Adam before Zoe");
        assert_eq!(first(GallerySort::SenderDesc), "apple.pdf");
        assert_eq!(first(GallerySort::Type), "apple.pdf", "pdf before png");
        assert_eq!(first(GallerySort::TypeDesc), "zebra.png");
    }

    /// Re-describing a message replaces what was known about it, so a message
    /// re-scanned after an edit does not accumulate ghost rows.
    #[test]
    fn rescanning_a_message_replaces_its_attachments() {
        let c = Cache::in_memory().unwrap();
        add_folder(&c, "INBOX", FolderKind::Inbox);
        add_msg(&c, "INBOX", 1, "A", "S", 100);
        add_meta(&c, "INBOX", 1, 0, "first.pdf", 2);
        add_meta(&c, "INBOX", 1, 0, "second.pdf", 2);
        let items = c.gallery_page(&all(GallerySort::Newest, 50, 0));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "second.pdf");
    }

    /// A batch the scan wrote off wholesale must be askable again: the v15
    /// re-queue drops the scan mark from any message recorded as holding
    /// nothing, so the scan that now isolates the one message at fault gets
    /// another look at its neighbours.
    #[test]
    fn messages_recorded_as_holding_nothing_can_be_re_queued() {
        let c = Cache::in_memory().unwrap();
        add_folder(&c, "INBOX", FolderKind::Inbox);
        add_msg(&c, "INBOX", 1, "A", "S", 100);
        add_msg(&c, "INBOX", 2, "B", "T", 200);
        add_meta(&c, "INBOX", 1, 0, "real.pdf", 2);
        c.save_attachment_meta(1, "INBOX", 2, &[]); // written off
        assert_eq!(c.unscanned_attachment_count(1, "INBOX"), 0);

        c.conn
            .execute(
                "DELETE FROM attachment_scan WHERE NOT EXISTS (\
                     SELECT 1 FROM attachment_meta am \
                     WHERE am.account_id = attachment_scan.account_id \
                       AND am.folder_path = attachment_scan.folder_path \
                       AND am.uid = attachment_scan.uid)",
                [],
            )
            .unwrap();

        assert_eq!(c.unscanned_attachment_uids(1, "INBOX", 10), vec![2], "only the empty one");
        assert_eq!(
            c.gallery_page(&all(GallerySort::Newest, 50, 0)).len(),
            1,
            "the described one is untouched"
        );
    }

    /// The scan's work queue: messages flagged as carrying an attachment that
    /// have not been described. A message with nothing in it is still marked,
    /// or it would be asked about on every pass forever.
    #[test]
    fn the_scan_queue_empties_even_for_messages_holding_nothing() {
        let c = Cache::in_memory().unwrap();
        add_folder(&c, "INBOX", FolderKind::Inbox);
        add_msg(&c, "INBOX", 1, "A", "S", 100);
        add_msg(&c, "INBOX", 2, "B", "T", 200);
        // A third with no paperclip: never in the queue.
        c.conn
            .execute(
                "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, \
                 subject, date, ts, unread, starred, has_attachment) \
                 VALUES (1, 'INBOX', 3, 'C', '', 'U', '', 300, 0, 0, 0)",
                [],
            )
            .unwrap();

        assert_eq!(c.unscanned_attachment_count(1, "INBOX"), 2);
        assert_eq!(c.unscanned_attachment_uids(1, "INBOX", 10), vec![2, 1], "newest first");

        c.save_attachment_meta(1, "INBOX", 2, &[]); // described, holds nothing
        assert_eq!(c.unscanned_attachment_count(1, "INBOX"), 1);
        add_meta(&c, "INBOX", 1, 0, "x.pdf", 2);
        assert_eq!(c.unscanned_attachment_count(1, "INBOX"), 0);
        assert!(c.unscanned_attachment_uids(1, "INBOX", 10).is_empty());
    }

    #[test]
    fn redecode_encoded_subjects_fixes_raw_encoded_words_in_place() {
        let c = Cache::in_memory().unwrap();
        add_folder(&c, "INBOX", FolderKind::Inbox);
        // A subject an older build stored raw after aborting on the over-long word.
        let raw = "=?utf-8?Q?92=2Dyear=2Dold=20artist=20Sheila=20Hicks?=";
        add_msg(&c, "INBOX", 1, "Popova", raw, 100);
        add_msg(&c, "INBOX", 2, "Plain", "Already fine", 200);

        Cache::redecode_encoded_subjects(&c.conn);

        let subj: String = c
            .conn
            .query_row(
                "SELECT subject FROM messages WHERE uid = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(subj, "92-year-old artist Sheila Hicks");
        let plain: String = c
            .conn
            .query_row("SELECT subject FROM messages WHERE uid = 2", [], |r| r.get(0))
            .unwrap();
        assert_eq!(plain, "Already fine");
    }

    /// Insert a message carrying threading headers.
    /// Like [`add_threaded`], but with a sender — for the checks that a shared
    /// Message-ID alone is not enough to call two rows the same mail.
    fn add_from(c: &Cache, folder: &str, uid: u32, ts: i64, msgid: &str, from: &str) {
        c.conn
            .execute(
                "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, message_id, references_) \
                 VALUES (1, ?1, ?2, 'X', ?5, 'S', '', ?3, 0, 0, 0, ?4, '')",
                params![folder, uid, ts, msgid, from],
            )
            .unwrap();
    }

    fn add_threaded(c: &Cache, folder: &str, uid: u32, ts: i64, msgid: &str, refs: &str) {
        c.conn
            .execute(
                "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, message_id, references_) \
                 VALUES (1, ?1, ?2, 'X', '', 'S', '', ?3, 0, 0, 0, ?4, ?5)",
                params![folder, uid, ts, msgid, refs],
            )
            .unwrap();
    }

    /// A conversation lookup must return the messages that actually reference the
    /// one being opened — nothing else.
    ///
    /// The bind indices for the `references_` comparisons were off by a slot, so
    /// the last one ran against the *account id*. Coerced to text, `instr` then
    /// matched every message whose References merely contained that digit, and
    /// one click dragged thousands of messages into the reader — each of them a
    /// body to fetch and re-render. That is worth a test.
    /// The repair asks the server only about replies. A message that opens a
    /// conversation has no References and never will, so fetching its header
    /// buys nothing — and on a real cache that distinction is the difference
    /// between 68k messages and 4k.
    #[test]
    fn only_replies_are_worth_repairing() {
        let c = Cache::in_memory().unwrap();
        // A reply whose References is the single id an ENVELOPE gives.
        c.conn.execute(
            "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, message_id, references_) \
             VALUES (1,'INBOX',10,'X','','Re: hello','',1,0,0,0,'a@x','parent@x')",
            [],
        ).unwrap();
        // A thread opener: no prefix, no references. Nothing to find.
        c.conn.execute(
            "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, message_id, references_) \
             VALUES (1,'INBOX',11,'X','','hello','',1,0,0,0,'b@x','')",
            [],
        ).unwrap();
        // Already complete: two ids, from a full header parse.
        c.conn.execute(
            "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, message_id, references_) \
             VALUES (1,'INBOX',12,'X','','Re: hello','',1,0,0,0,'c@x','root@x parent@x')",
            [],
        ).unwrap();

        let want = c.uids_needing_references(1, "INBOX", u32::MAX, 100);
        let uids: Vec<u32> = want.iter().map(|(u, _)| *u).collect();
        assert_eq!(uids, vec![10], "only the thin reply");
    }

    /// The watermark walks strictly down, so a message with nothing to find is
    /// asked about once rather than on every pass forever.
    #[test]
    fn the_repair_walks_downwards_and_finishes() {
        let c = Cache::in_memory().unwrap();
        for uid in [10u32, 20, 30] {
            c.conn.execute(
                "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, message_id, references_) \
                 VALUES (1,'INBOX',?1,'X','','Re: t','',1,0,0,0,'m@x','p@x')",
                params![uid],
            ).unwrap();
        }
        assert_eq!(c.refs_repair_state(1, "INBOX"), (u32::MAX, false), "starts at the top");
        let first = c.uids_needing_references(1, "INBOX", u32::MAX, 2);
        assert_eq!(first.iter().map(|(u, _)| *u).collect::<Vec<_>>(), vec![30, 20], "newest first");

        c.set_refs_repair_state(1, "INBOX", 20, false);
        let next = c.uids_needing_references(1, "INBOX", 20, 2);
        assert_eq!(next.iter().map(|(u, _)| *u).collect::<Vec<_>>(), vec![10], "strictly below");

        c.set_refs_repair_state(1, "INBOX", 0, true);
        assert!(c.refs_repair_state(1, "INBOX").1, "and it finishes");
    }

    #[test]
    fn a_conversation_holds_only_messages_that_reference_it() {
        let c = Cache::in_memory().unwrap();
        add_threaded(&c, "INBOX", 1, 500, "root@x", "");
        add_threaded(&c, "Sent", 2, 600, "reply@x", "root@x");
        // Unrelated, but its References contain the digit "1" — the account id.
        add_threaded(&c, "Archive", 3, 550, "other@x", "31337@elsewhere");

        let found = c.messages_by_thread_ids(1, &["root@x".to_string()]);
        let ids: Vec<&str> = found.iter().map(|(_, m)| m.message_id.as_str()).collect();
        assert_eq!(ids, vec!["reply@x", "root@x"], "only the real conversation");
    }

    /// Gmail stores one message under every label it carries, so INBOX, All Mail
    /// and Important hold three copies with three UIDs. A body read under one
    /// label has to answer for the others, or opening the conversation from the
    /// Inbox re-downloads mail already in hand — and its attachments come back
    /// as a "Load attachments" button rather than the files themselves.
    #[test]
    fn a_body_cached_under_one_gmail_label_answers_for_the_others() {
        let c = Cache::in_memory().unwrap();
        add_threaded(&c, "INBOX", 42, 500, "same@x", "");
        add_threaded(&c, "[Gmail]/All Mail", 900, 500, "same@x", "");
        c.save_body(1, "[Gmail]/All Mail", 900, "the real body");

        assert_eq!(c.load_body(1, "INBOX", 42).as_deref(), Some("the real body"));
        // The copy that actually holds it still answers directly.
        assert_eq!(
            c.load_body(1, "[Gmail]/All Mail", 900).as_deref(),
            Some("the real body")
        );
    }

    #[test]
    fn attachments_downloaded_under_one_label_answer_for_the_others() {
        let c = Cache::in_memory().unwrap();
        add_threaded(&c, "INBOX", 42, 500, "same@x", "");
        add_threaded(&c, "Paratype", 7, 500, "same@x", "");
        c.save_attachments(
            1,
            "Paratype",
            7,
            &[Attachment { name: "spec.pdf".into(), data: vec![1, 2, 3] }],
        );

        let found = c.load_attachments(1, "INBOX", 42);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "spec.pdf");
    }

    /// One message under two labels is one download, not two. `load_attachments`
    /// already answers for every label from whichever copy was fetched, so
    /// fetching the others only stores the same blobs again.
    #[test]
    fn attachments_fetched_under_one_label_count_as_fetched_for_the_others() {
        let c = Cache::in_memory().unwrap();
        add_threaded(&c, "INBOX", 42, 500, "same@x", "");
        add_threaded(&c, "Work", 7, 500, "same@x", "");
        c.mark_attachments_checked(1, "Work", 7);

        assert!(c.attachments_checked(1, "INBOX", 42));
    }

    /// The reach stops at the message: an unrelated mail is still unfetched.
    #[test]
    fn another_message_being_fetched_does_not_count_as_this_one() {
        let c = Cache::in_memory().unwrap();
        add_threaded(&c, "INBOX", 42, 500, "mine@x", "");
        add_threaded(&c, "Work", 7, 500, "someone-else@x", "");
        c.mark_attachments_checked(1, "Work", 7);

        assert!(!c.attachments_checked(1, "INBOX", 42));
    }

    /// Two different messages must never answer for each other — the fallback
    /// keys on Message-ID, and a message that has none has nothing to match.
    #[test]
    fn a_different_message_never_answers_for_this_one() {
        let c = Cache::in_memory().unwrap();
        add_threaded(&c, "INBOX", 42, 500, "mine@x", "");
        add_threaded(&c, "[Gmail]/All Mail", 900, 500, "someone-else@x", "");
        c.save_body(1, "[Gmail]/All Mail", 900, "not yours");
        assert_eq!(c.load_body(1, "INBOX", 42), None);

        add_threaded(&c, "INBOX", 43, 500, "", "");
        add_threaded(&c, "Archive", 44, 500, "", "");
        c.save_body(1, "Archive", 44, "unrelated");
        assert_eq!(
            c.load_body(1, "INBOX", 43),
            None,
            "an empty Message-ID matches nothing, not everything"
        );
    }

    /// Message-ID is written by the sender, and spam reuses one across unrelated
    /// mail. Serving one message's body for another would be worse than the
    /// cache miss the fallback exists to avoid, so sender and timestamp have to
    /// agree too.
    #[test]
    fn a_reused_message_id_from_a_different_sender_answers_for_nothing() {
        let c = Cache::in_memory().unwrap();
        add_from(&c, "INBOX", 42, 500, "dup@x", "me@real.example");
        add_from(&c, "Archive", 900, 500, "dup@x", "spammer@fake.example");
        c.save_body(1, "Archive", 900, "not the same mail");
        assert_eq!(c.load_body(1, "INBOX", 42), None);

        // Same sender, but a different message that happens to reuse the id.
        add_from(&c, "INBOX", 43, 500, "dup2@x", "me@real.example");
        add_from(&c, "Archive", 901, 900, "dup2@x", "me@real.example");
        c.save_body(1, "Archive", 901, "a different day's mail");
        assert_eq!(c.load_body(1, "INBOX", 43), None);
    }

    /// Re-adding an account re-downloads mail that is all older than the moment
    /// it was added. Threading reads what a message answers, not when it was
    /// sent, so every member belongs to the conversation regardless of age.
    #[test]
    fn a_conversation_holds_its_members_however_old_they_are() {
        let c = Cache::in_memory().unwrap();
        add_threaded(&c, "INBOX", 1, 500, "root@x", "");
        add_threaded(&c, "Sent", 2, 400, "old-reply@x", "root@x");
        add_threaded(&c, "Sent", 3, 600, "new-reply@x", "root@x");

        let found = c.messages_by_thread_ids(1, &["root@x".to_string()]);
        let mut ids: Vec<&str> = found.iter().map(|(_, m)| m.message_id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["new-reply@x", "old-reply@x", "root@x"], "all three, any age");
    }
}
