//! Smart folders and tag views: scans across an account's cached mail.

use rusqlite::params;

use crate::models::Message;

use super::{message_columns, message_from_row, Cache, KEYWORDS_COL, LABEL_SEP};

/// Most messages one tag view lists per account; the list pages within it.
const TAG_VIEW_LIMIT: i64 = 5000;

impl Cache {
    /// The account's cached messages a smart folder (#417) keeps, newest
    /// first with the folder each sits in: those in `include` (every folder
    /// when `None`) and not in `exclude`, past what `pre` rules out, for
    /// which `keep`, given the folder and the message, says yes. Rows are read one at a time and only the kept
    /// ones become messages, so a folder over a large index stays cheap to
    /// hold; reading stops at `limit` kept.
    pub(crate) fn smart_scan(
        &self,
        account_id: u32,
        include: Option<&[String]>,
        exclude: &[String],
        pre: crate::config::SmartPrefilter,
        limit: usize,
        mut keep: impl FnMut(&str, &Message) -> bool,
    ) -> Vec<(String, Message)> {
        use rusqlite::types::Value;
        let mut args: Vec<Value> = vec![Value::Integer(account_id.into())];
        let list = |paths: &[String], args: &mut Vec<Value>| -> String {
            let marks: Vec<String> = paths
                .iter()
                .map(|p| {
                    args.push(Value::Text(p.clone()));
                    format!("?{}", args.len())
                })
                .collect();
            marks.join(",")
        };
        let cols = message_columns();
        let mut sql = format!(
            "SELECT folder_path, {cols} \
             FROM messages WHERE account_id = ?1"
        );
        if let Some(paths) = include {
            if paths.is_empty() {
                return Vec::new();
            }
            sql.push_str(&format!(" AND folder_path IN ({})", list(paths, &mut args)));
        }
        if !exclude.is_empty() {
            sql.push_str(&format!(
                " AND folder_path NOT IN ({})",
                list(exclude, &mut args)
            ));
        }
        for (col, want) in [
            ("starred", pre.starred),
            ("unread", pre.unread),
            ("has_attachment", pre.attachment),
        ] {
            if let Some(want) = want {
                sql.push_str(&format!(" AND {col} = {}", i32::from(want)));
            }
        }
        sql.push_str(" ORDER BY ts DESC");
        let mut run = || -> rusqlite::Result<Vec<(String, Message)>> {
            let mut stmt = self.conn.prepare(&sql)?;
            let mut rows = stmt.query(rusqlite::params_from_iter(args.iter()))?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let m = message_from_row(row, 1, account_id, 0)?;
                let path: String = row.get(0)?;
                if keep(&path, &m) {
                    out.push((path, m));
                    if out.len() >= limit {
                        break;
                    }
                }
            }
            Ok(out)
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache smart_scan failed: {e}");
            Vec::new()
        })
    }

    /// Every cached message of the account carrying `keyword` — on the server
    /// or locally — newest first, with the folder each sits in. Backs the
    /// sidebar's tag views (#71); the caller maps paths to folder ids and
    /// drops the copies Gmail keeps per label. `name` is the tag's name: on
    /// Gmail a label named like the tag (or like its keyword) counts as the
    /// tag too (#374).
    pub(crate) fn messages_with_keyword(
        &self,
        account_id: u32,
        keyword: &str,
        name: &str,
    ) -> Vec<(String, Message)> {
        let (needle, by_keyword, by_name) = tag_needles(keyword, name);
        let cols = message_columns();
        let sql = format!(
            "SELECT folder_path, {cols} \
             FROM messages \
             WHERE account_id = ?1 AND (instr(' ' || lower({KEYWORDS_COL}) || ' ', ?2) > 0 \
                   OR (gmail_labels <> '' AND (instr(?4 || lower(gmail_labels) || ?4, ?5) > 0 \
                                               OR instr(?4 || lower(gmail_labels) || ?4, ?6) > 0))) \
             ORDER BY ts DESC LIMIT ?3"
        );
        let run = || -> rusqlite::Result<Vec<(String, Message)>> {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(
                params![
                    account_id,
                    needle,
                    TAG_VIEW_LIMIT,
                    LABEL_SEP.to_string(),
                    by_keyword,
                    by_name
                ],
                |row| {
                    let m = message_from_row(row, 1, account_id, 0)?;
                    Ok((row.get::<_, String>(0)?, m))
                },
            )?;
            rows.collect()
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache messages_with_keyword failed: {e}");
            Vec::new()
        })
    }

    /// The account's unread mail as the tag rows' chips need it (#717): one
    /// read of the unread rows answers every tag, where a query per tag
    /// would walk the whole index once each.
    pub(crate) fn unread_tag_marks(&self, account_id: u32) -> Vec<TagMarks> {
        let sql = format!(
            "SELECT folder_path, message_id, ' ' || lower({KEYWORDS_COL}) || ' ', \
                    CASE WHEN gmail_labels = '' THEN '' \
                         ELSE ?2 || lower(gmail_labels) || ?2 END \
             FROM messages WHERE account_id = ?1 AND unread = 1"
        );
        let run = || -> rusqlite::Result<Vec<TagMarks>> {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params![account_id, LABEL_SEP.to_string()], |row| {
                Ok(TagMarks {
                    path: row.get(0)?,
                    message_id: row.get(1)?,
                    keywords: row.get(2)?,
                    labels: row.get(3)?,
                })
            })?;
            rows.collect()
        };
        run().unwrap_or_else(|e| {
            tracing::warn!("cache unread_tag_marks failed: {e}");
            Vec::new()
        })
    }
}

/// What a tag is looked for by: its keyword between spaces, and on Gmail a
/// label named like the keyword or like the tag (#374), between label
/// separators. Lower case, as the queries compare.
fn tag_needles(keyword: &str, name: &str) -> (String, String, String) {
    let label = |s: &str| {
        format!(
            "{sep}{}{sep}",
            crate::mail::mutf7::encode(s).to_lowercase(),
            sep = LABEL_SEP
        )
    };
    (
        format!(" {} ", keyword.to_ascii_lowercase()),
        label(keyword),
        label(name),
    )
}

/// One unread message's tags, read by [`Cache::unread_tag_marks`].
pub(crate) struct TagMarks {
    pub(crate) path: String,
    pub(crate) message_id: String,
    /// Server and local keywords, lower case, each between spaces.
    keywords: String,
    /// Gmail labels, lower case, each between separators; empty for none.
    labels: String,
}

impl TagMarks {
    /// Whether the message carries the tag, as [`Cache::messages_with_keyword`]
    /// judges it.
    pub(crate) fn carries(&self, keyword: &str, name: &str) -> bool {
        let (needle, by_keyword, by_name) = tag_needles(keyword, name);
        self.keywords.contains(&needle)
            || (!self.labels.is_empty()
                && (self.labels.contains(&by_keyword) || self.labels.contains(&by_name)))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::add_msg;
    use super::*;

    /// The tag rows' chips (#717) count what the tag views list: keywords on
    /// the server or kept locally, and Gmail labels named like the tag, on
    /// unread mail only.
    #[test]
    fn unread_tag_marks_match_what_a_tag_view_lists() {
        let c = Cache::in_memory().unwrap();
        let set = |uid: u32, unread: bool, kw: &str, labels: &str, id: &str| {
            c.conn
                .execute(
                    "UPDATE messages SET unread = ?2, keywords = ?3, gmail_labels = ?4, \
                     message_id = ?5 WHERE uid = ?1",
                    params![uid, unread, kw, labels, id],
                )
                .unwrap();
        };
        for uid in 1..=5 {
            add_msg(&c, "INBOX", uid, "Ada", "Hello", i64::from(uid));
        }
        set(1, true, "Work $Seen", "", "a@x");
        set(2, false, "work", "", "b@x");
        set(
            3,
            true,
            "",
            &format!("My Label{LABEL_SEP}Work/Projects"),
            "c@x",
        );
        set(4, true, "", "", "d@x");
        set(5, true, "Workshop", "", "e@x");
        c.set_local_tag(1, "d@x", "work", true);

        let marks = c.unread_tag_marks(1);
        let tagged = |kw: &str, name: &str| -> Vec<&str> {
            let mut ids: Vec<&str> = marks
                .iter()
                .filter(|m| m.carries(kw, name))
                .map(|m| m.message_id.as_str())
                .collect();
            ids.sort_unstable();
            ids
        };
        assert_eq!(
            tagged("Work", "Work"),
            ["a@x", "d@x"],
            "read mail and Workshop stay out"
        );
        assert_eq!(tagged("x", "my label"), ["c@x"]);
        assert_eq!(
            tagged("x", "Work"),
            Vec::<&str>::new(),
            "a nested label is its own name"
        );
        // The same messages, read or not, as the view finds them.
        let mut viewed: Vec<u32> = c
            .messages_with_keyword(1, "Work", "Work")
            .into_iter()
            .map(|(_, m)| m.uid)
            .collect();
        viewed.sort_unstable();
        assert_eq!(viewed, [1, 2, 4]);
    }
}
