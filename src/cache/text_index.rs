//! The words of every body on disk, for search (#509). `body_text` is an
//! FTS5 index without a copy of the text, keyed by the `bodies` row it was
//! made from: a body saved writes its row, and a trigger drops the row when
//! the body goes, however it goes. Bodies saved by a build without the index
//! are indexed in the background ([`Cache::index_missing_bodies`]).

use rusqlite::{params, OptionalExtension};

use super::Cache;

/// The most text of one body the index keeps: the words a search is after
/// are seldom past the first pages, and a mailing-list digest is not worth
/// its megabytes.
const MAX_TEXT: usize = 256 * 1024;

/// A message whose body matched: account, folder path, uid and Message-ID.
pub(crate) type BodyHit = (u32, String, u32, String);

/// The text of a cached body, as the index wants it: its words, without the
/// markup, the style sheets or the scripts, entities decoded.
pub(super) fn index_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len().min(MAX_TEXT) / 2);
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        out.push(' ');
        rest = &rest[lt + 1..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '!' || *c == '-')
            .collect::<String>()
            .to_ascii_lowercase();
        let end = if name.starts_with("!--") {
            rest.find("-->").map(|i| i + 3)
        } else {
            rest.find('>').map(|i| i + 1)
        };
        let Some(end) = end else {
            rest = "";
            break;
        };
        rest = &rest[end..];
        // What a style or script element holds is not text.
        if matches!(name.as_str(), "style" | "script" | "head" | "title") {
            let close = format!("</{name}");
            match rest.to_ascii_lowercase().find(&close) {
                Some(i) => rest = &rest[i..],
                None => rest = "",
            }
        }
        if out.len() >= MAX_TEXT {
            break;
        }
    }
    out.push_str(rest);
    let mut text = crate::encoding::unescape_html(&out);
    if text.len() > MAX_TEXT {
        let mut cut = MAX_TEXT;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    text
}

/// One search term as an FTS5 query: a phrase as written, a word as the
/// start of a word, so "invo" finds "invoice" while it is being typed.
/// `None` when it has no letter or digit for the index to look up.
pub(super) fn fts_term(term: &str) -> Option<String> {
    if !term.chars().any(char::is_alphanumeric) {
        return None;
    }
    let quoted = format!("\"{}\"", term.replace('"', "\"\""));
    Some(if term.contains(char::is_whitespace) {
        quoted
    } else {
        quoted + "*"
    })
}

impl Cache {
    /// Index the text of a body just saved, as `rowid` of `bodies`.
    pub(super) fn index_body(&self, rowid: i64, body: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO body_text (rowid, text) VALUES (?1, ?2)",
            params![rowid, index_text(body)],
        )?;
        Ok(())
    }

    /// The `bodies` row of a message, if its body is on disk.
    pub(super) fn body_rowid(&self, account_id: u32, folder_path: &str, uid: u32) -> Option<i64> {
        self.conn
            .query_row(
                "SELECT rowid FROM bodies WHERE account_id = ?1 AND folder_path = ?2 AND uid = ?3",
                params![account_id, folder_path, uid],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// Index up to `batch` bodies past rowid `after` that have no row in
    /// the index yet: ones saved by an earlier build. Returns the last
    /// rowid indexed, to go on from, or `None` once there are no more.
    pub(crate) fn index_missing_bodies(
        &self,
        after: i64,
        batch: usize,
    ) -> rusqlite::Result<Option<i64>> {
        let missing: Vec<(i64, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT b.rowid, b.body FROM bodies b WHERE b.rowid > ?1 \
                 AND NOT EXISTS (SELECT 1 FROM body_text t WHERE t.rowid = b.rowid) \
                 ORDER BY b.rowid LIMIT ?2",
            )?;
            let rows =
                stmt.query_map(params![after, batch as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let Some(last) = missing.last().map(|(rowid, _)| *rowid) else {
            return Ok(None);
        };
        let texts: Vec<(i64, String)> = missing
            .into_iter()
            .map(|(rowid, body)| (rowid, index_text(&body)))
            .collect();
        let tx = self.conn.unchecked_transaction()?;
        for (rowid, text) in &texts {
            tx.execute(
                "INSERT INTO body_text (rowid, text) VALUES (?1, ?2)",
                params![rowid, text],
            )?;
        }
        tx.commit()?;
        Ok(Some(last))
    }

    /// The messages whose body on disk holds `term` (see [`fts_term`]),
    /// `limit` at most. Empty for a term the index cannot look up.
    pub(crate) fn find_body_text(&self, term: &str, limit: usize) -> Vec<BodyHit> {
        let Some(query) = fts_term(term) else {
            return Vec::new();
        };
        let run = || -> rusqlite::Result<Vec<BodyHit>> {
            let mut stmt = self.conn.prepare(
                "SELECT b.account_id, b.folder_path, b.uid, COALESCE(m.message_id, '') \
                 FROM body_text t JOIN bodies b ON b.rowid = t.rowid \
                 LEFT JOIN messages m ON m.account_id = b.account_id \
                   AND m.folder_path = b.folder_path AND m.uid = b.uid \
                 WHERE body_text MATCH ?1 LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![query, limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            rows.collect()
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache find_body_text failed: {e}");
            Vec::new()
        })
    }
}

impl Cache {
    /// Of `uids` in a folder, the ones the index holds, with their
    /// Message-IDs: a server's search names mail by uid alone.
    pub(crate) fn indexed_message_ids(
        &self,
        account_id: u32,
        folder_path: &str,
        uids: &[u32],
    ) -> Vec<(u32, String)> {
        let mut out = Vec::with_capacity(uids.len());
        for chunk in uids.chunks(500) {
            let marks = vec!["?"; chunk.len()].join(", ");
            let sql = format!(
                "SELECT uid, message_id FROM messages \
                 WHERE account_id = ? AND folder_path = ? AND uid IN ({marks})"
            );
            let mut args: Vec<rusqlite::types::Value> =
                vec![i64::from(account_id).into(), folder_path.to_string().into()];
            args.extend(chunk.iter().map(|u| i64::from(*u).into()));
            let run = || -> rusqlite::Result<Vec<(u32, String)>> {
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
                rows.collect()
            };
            match run() {
                Ok(rows) => out.extend(rows),
                Err(e) => tracing::warn!("cache indexed_message_ids failed: {e}"),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_index_reads_the_words_not_the_markup() {
        let html = "<html><head><style>p { color: red }</style></head>\
                    <body><p>Invoice&nbsp;4471 for <b>Mario</b>&amp;co</p>\
                    <!-- a <p>comment</p> --><script>var x = 1;</script>end</body></html>";
        let text = index_text(html);
        let words: Vec<&str> = text.split_whitespace().collect();
        assert_eq!(words, ["Invoice", "4471", "for", "Mario", "&co", "end"]);
    }

    #[test]
    fn a_word_is_the_start_of_a_word_and_a_phrase_is_as_written() {
        assert_eq!(fts_term("invo").as_deref(), Some("\"invo\"*"));
        assert_eq!(fts_term("mario rossi").as_deref(), Some("\"mario rossi\""));
        assert_eq!(
            fts_term("say \"hi\"").as_deref(),
            Some("\"say \"\"hi\"\"\"")
        );
        assert_eq!(fts_term("--"), None);
    }

    #[test]
    fn bodies_are_found_by_their_words_until_they_go() {
        let c = Cache::in_memory().unwrap();
        super::super::test_util::add_msg(&c, "INBOX", 1, "a", "s", 100);
        c.save_body(1, "INBOX", 1, "<p>The Rossi invoice, Mário</p>");
        c.save_body(1, "INBOX", 2, "<p>Nothing to see</p>");
        let found = |t: &str| {
            c.find_body_text(t, 10)
                .into_iter()
                .map(|(_, _, uid, _)| uid)
                .collect::<Vec<_>>()
        };
        assert_eq!(found("invo"), [1]);
        assert_eq!(found("mario"), [1], "accents are folded");
        assert_eq!(found("rossi invoice"), [1]);
        assert!(found("invoice rossi").is_empty(), "a phrase is in order");
        // Saved again: the old words go with the old body.
        c.save_body(1, "INBOX", 1, "<p>Paid</p>");
        assert!(found("invoice").is_empty());
        assert_eq!(found("paid"), [1]);
        c.remove_downloaded(None).unwrap();
        assert!(found("paid").is_empty());
    }

    #[test]
    fn bodies_from_before_the_index_are_indexed_later() {
        let c = Cache::in_memory().unwrap();
        c.conn
            .execute(
                "INSERT INTO bodies (account_id, folder_path, uid, body) VALUES (1, 'INBOX', 5, '<p>old words</p>')",
                [],
            )
            .unwrap();
        assert!(c.find_body_text("old", 10).is_empty());
        let last = c.index_missing_bodies(0, 100).unwrap();
        assert!(last.is_some());
        assert_eq!(c.index_missing_bodies(0, 100).unwrap(), None);
        assert_eq!(c.find_body_text("old", 10).len(), 1);
    }
}
