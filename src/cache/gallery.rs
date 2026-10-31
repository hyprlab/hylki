//! The attachments gallery's queries and the scan that describes
//! attachments without downloading them.

use rusqlite::params;

use crate::models::GallerySort;

use super::Cache;

/// What the attachments gallery is asking for: the scope, the search and the
/// ordering, all of which have to reach the database — the archive holds far
/// more attachments than the UI can hold in memory, so filtering after the fact
/// would page through the wrong set.
pub(crate) struct GalleryQuery<'a> {
    /// `(account id, folder path)` pairs whose attachments are in scope. Empty
    /// means "every folder", which is what a gallery opened before the folder
    /// list arrives should show.
    pub(crate) folders: &'a [(u32, String)],
    /// Narrow to one account, as the footer's account dropdown does.
    pub(crate) account_id: Option<u32>,
    /// Search words; a row has to match every one of them somewhere.
    pub(crate) tokens: &'a [String],
    /// A [`crate::models::type_bucket`] value, or 0 for every type.
    pub(crate) bucket: u32,
    pub(crate) sort: GallerySort,
    pub(crate) limit: u32,
    pub(crate) offset: u32,
    /// Largest cached file whose bytes ride along with the page.
    pub(crate) data_cap: i64,
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
    pub(crate) fn gallery_page(&self, q: &GalleryQuery) -> Vec<crate::models::GalleryItem> {
        // Three leading parameters of our own, so the scope starts at ?4.
        let (where_sql, params) = q.where_clause(4);
        let sql = format!(
            "SELECT am.account_id, am.folder_path, am.uid, am.idx, am.name, am.size, \
                    COALESCE(m.from_name, ''), COALESCE(m.subject, ''), COALESCE(m.ts, 0), \
                    CASE WHEN a.data IS NOT NULL AND LENGTH(a.data) <= ?1 THEN a.data ELSE NULL END, \
                    a.uid IS NOT NULL, COALESCE(f.name, '') \
             FROM attachment_meta am \
             LEFT JOIN folders f \
               ON f.account_id = am.account_id AND f.path = am.folder_path \
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
                    folder_name: row.get(11)?,
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
    pub(crate) fn gallery_total(&self, q: &GalleryQuery) -> u32 {
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
    pub(crate) fn save_attachment_meta(
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
    pub(crate) fn unscanned_attachment_uids(
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
    pub(crate) fn unscanned_attachment_count(&self, account_id: u32, folder_path: &str) -> u32 {
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
}
