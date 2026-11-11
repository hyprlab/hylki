//! The members of a whole page of conversations in one pass (#296).

use std::collections::HashMap;

use crate::models::Message;

use super::{message_columns, message_from_row, Cache};

/// Most Message-IDs one conversation's members are matched by, as for a
/// single conversation opened in the reader.
const GROUP_ID_LIMIT: usize = 24;

/// Most messages one conversation pulls in, as for one opened.
const GROUP_MEMBER_LIMIT: usize = 100;

/// Message-IDs bound in one `IN (…)` query, well under SQLite's limit.
const IDS_PER_QUERY: usize = 500;

impl Cache {
    /// [`Cache::messages_by_thread_ids`] for every one of `groups` at once:
    /// for each, the account's cached messages that are one of its ids or
    /// name one in their References, with the folder each sits in, newest
    /// first.
    ///
    /// The References half of the match is a scan of the account. Asked a
    /// conversation at a time, a page of 205 conversations scanned an
    /// 87,000-message Gmail cache 205 times, and each page's badges took up
    /// to 26 seconds (#296). One scan now answers the page.
    pub(crate) fn messages_by_thread_groups(
        &self,
        account_id: u32,
        groups: &[&[String]],
    ) -> Vec<Vec<(String, Message)>> {
        let mut owners: HashMap<&str, Vec<usize>> = HashMap::new();
        for (gi, ids) in groups.iter().enumerate() {
            for id in ids.iter().filter(|i| !i.is_empty()).take(GROUP_ID_LIMIT) {
                let owned = owners.entry(id.as_str()).or_default();
                if owned.last() != Some(&gi) {
                    owned.push(gi);
                }
            }
        }
        let mut out: Vec<Vec<(String, Message)>> = vec![Vec::new(); groups.len()];
        if owners.is_empty() {
            return out;
        }
        let run = |out: &mut Vec<Vec<(String, Message)>>| -> rusqlite::Result<()> {
            let members = self.group_member_rowids(account_id, &owners)?;
            if members.is_empty() {
                return Ok(());
            }
            let rowids: Vec<String> = members.keys().map(i64::to_string).collect();
            let sql = format!(
                "SELECT rowid, folder_path, {cols} FROM messages \
                 WHERE rowid IN ({list}) ORDER BY ts DESC",
                cols = message_columns(),
                list = rowids.join(","),
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let rowid: i64 = row.get(0)?;
                let Some(gis) = members.get(&rowid) else {
                    continue;
                };
                let open: Vec<usize> = gis
                    .iter()
                    .copied()
                    .filter(|gi| out[*gi].len() < GROUP_MEMBER_LIMIT)
                    .collect();
                if open.is_empty() {
                    continue;
                }
                let path: String = row.get(1)?;
                let m = message_from_row(row, 2, account_id, 0)?;
                for gi in open {
                    out[gi].push((path.clone(), m.clone()));
                }
            }
            Ok(())
        };
        if let Err(e) = run(&mut out) {
            tracing::warn!("cache messages_by_thread_groups failed: {e}");
            return vec![Vec::new(); groups.len()];
        }
        out
    }

    /// Each matching row's rowid, with the groups it belongs to.
    fn group_member_rowids(
        &self,
        account_id: u32,
        owners: &HashMap<&str, Vec<usize>>,
    ) -> rusqlite::Result<HashMap<i64, Vec<usize>>> {
        let mut members: HashMap<i64, Vec<usize>> = HashMap::new();
        let mut join = |rowid: i64, gis: &[usize]| {
            let into = members.entry(rowid).or_default();
            for gi in gis {
                if !into.contains(gi) {
                    into.push(*gi);
                }
            }
        };
        let ids: Vec<&str> = owners.keys().copied().collect();
        for chunk in ids.chunks(IDS_PER_QUERY) {
            let slots = (0..chunk.len())
                .map(|i| format!("?{}", i + 2))
                .collect::<Vec<_>>()
                .join(", ");
            let mut stmt = self.conn.prepare(&format!(
                "SELECT rowid, message_id FROM messages \
                 WHERE account_id = ?1 AND message_id IN ({slots})"
            ))?;
            let mut binds: Vec<&dyn rusqlite::ToSql> = vec![&account_id];
            for id in chunk {
                binds.push(id as &dyn rusqlite::ToSql);
            }
            let mut rows = stmt.query(binds.as_slice())?;
            while let Some(row) = rows.next()? {
                let id = row.get_ref(1)?.as_str().unwrap_or("");
                if let Some(gis) = owners.get(id) {
                    join(row.get(0)?, gis);
                }
            }
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT rowid, references_ FROM messages WHERE account_id = ?1 AND references_ <> ''",
        )?;
        let mut rows = stmt.query([account_id])?;
        while let Some(row) = rows.next()? {
            let refs = row.get_ref(1)?.as_str().unwrap_or("");
            for r in refs.split_whitespace() {
                if let Some(gis) = owners.get(r) {
                    join(row.get(0)?, gis);
                }
            }
        }
        Ok(members)
    }
}
