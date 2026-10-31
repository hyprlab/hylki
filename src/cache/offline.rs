//! Mail kept offline (#536): which messages of an account's chosen period
//! are not on disk yet, and sparing a period when old downloads are removed.

use rusqlite::params_from_iter;
use rusqlite::types::Value;

use super::local_copies::COPIED;
use super::schema::DOWNLOADED_TABLES;
use super::Cache;

/// Whether `table` holds a row for message `m` or another copy of it: Gmail
/// files one message under every label, and one download serves them all.
/// The copies are found by Message-ID; left to itself, SQLite searched them
/// by account, a scan of the mailbox for every message, minutes on 100,000.
fn kept_in(table: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM {table} t WHERE t.account_id = m.account_id \
           AND t.folder_path = m.folder_path AND t.uid = m.uid) \
         OR (m.message_id != '' AND EXISTS (SELECT 1 FROM messages s \
           INDEXED BY messages_by_message_id JOIN {table} t \
           ON t.account_id = s.account_id AND t.folder_path = s.folder_path AND t.uid = s.uid \
           WHERE s.message_id = m.message_id AND s.account_id = m.account_id \
           AND s.from_addr = m.from_addr AND s.ts = m.ts)))"
    )
}

/// Of the copies of one message (Gmail and Proton file each in All Mail as
/// well), the one listed first is the one kept: the Inbox's, then a
/// folder's of its own, and only then a catch-all's, so a message copied to
/// On This Computer (#552) lands where it was filed.
const PLACE_ORDER: &str = "CASE WHEN upper(m.folder_path) = 'INBOX' THEN 0 \
     WHEN m.folder_path LIKE '%All Mail' OR m.folder_path LIKE '%Starred' \
     OR m.folder_path LIKE '%Important' THEN 2 ELSE 1 END";

/// A candidate row: folder path, uid, and the Message-ID, sender and date
/// that tell copies of one message apart from different messages.
type Candidate = (String, u32, String, String, i64);

impl Cache {
    /// Up to `limit` messages of an account dated `since` or later that are
    /// not kept yet, newest first, as (folder path, uid): no body on disk,
    /// or, with `attachments`, a paperclip whose files were never fetched.
    /// With `copy`, any message not copied to On This Computer yet (#552)
    /// too, whatever its date. Folders in `skip` are left out, and of
    /// several copies of one message only the first is listed.
    pub(crate) fn offline_missing(
        &self,
        account_id: u32,
        since: Option<i64>,
        attachments: bool,
        copy: bool,
        skip: &[String],
        limit: usize,
    ) -> Vec<(String, u32)> {
        if since.is_none() && !copy {
            return Vec::new();
        }
        let missing = if attachments {
            format!(
                "(NOT {} OR (m.has_attachment = 1 AND NOT {}))",
                kept_in("bodies"),
                kept_in("attachments_checked")
            )
        } else {
            format!("NOT {}", kept_in("bodies"))
        };
        let skipped = if skip.is_empty() {
            String::new()
        } else {
            format!(
                " AND m.folder_path NOT IN ({})",
                vec!["?"; skip.len()].join(", ")
            )
        };
        let wanted = match (since, copy) {
            (Some(_), true) => format!("((m.ts >= ? AND {missing}) OR NOT {COPIED})"),
            (Some(_), false) => format!("m.ts >= ? AND {missing}"),
            (None, _) => format!("NOT {COPIED}"),
        };
        // Only folders the account lists now: rows left under a folder
        // the server no longer has (or never had, from before rows followed
        // their accounts) were asked for again and again, and every refusal
        // held the rest back for minutes.
        let sql = format!(
            "SELECT m.folder_path, m.uid, m.message_id, m.from_addr, m.ts FROM messages m \
             WHERE m.account_id = ?{skipped} \
             AND m.folder_path IN (SELECT f.path FROM folders f WHERE f.account_id = m.account_id) \
             AND {wanted} \
             ORDER BY m.ts DESC, {PLACE_ORDER} LIMIT ?"
        );
        let mut args: Vec<Value> = vec![Value::from(i64::from(account_id))];
        args.extend(skip.iter().map(|p| Value::from(p.clone())));
        args.extend(since.map(Value::from));
        args.push(Value::from(limit as i64));
        let run = || -> rusqlite::Result<Vec<Candidate>> {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(args), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?;
            rows.collect()
        };
        let rows = run().unwrap_or_else(|e| {
            tracing::warn!("cache offline_missing failed: {e}");
            Vec::new()
        });
        let mut seen = std::collections::HashSet::new();
        rows.into_iter()
            .filter(|(_, _, id, from, ts)| {
                id.is_empty() || seen.insert((id.clone(), from.clone(), *ts))
            })
            .map(|(path, uid, ..)| (path, uid))
            .collect()
    }

    /// How many of an account's messages are not copied to On This
    /// Computer yet (#552), each message once however many folders file
    /// it, in the folders [`Cache::offline_missing`] would take them from.
    pub(crate) fn local_copy_pending(&self, account_id: u32, skip: &[String]) -> usize {
        let skipped = if skip.is_empty() {
            String::new()
        } else {
            format!(
                " AND m.folder_path NOT IN ({})",
                vec!["?"; skip.len()].join(", ")
            )
        };
        let sql = format!(
            "SELECT COUNT(DISTINCT CASE WHEN m.message_id = '' \
             THEN m.folder_path || char(10) || m.uid ELSE m.message_id END) \
             FROM messages m WHERE m.account_id = ?{skipped} \
             AND m.folder_path IN (SELECT f.path FROM folders f WHERE f.account_id = m.account_id) \
             AND NOT {COPIED}"
        );
        let mut args: Vec<Value> = vec![Value::from(i64::from(account_id))];
        args.extend(skip.iter().map(|p| Value::from(p.clone())));
        self.conn
            .query_row(&sql, params_from_iter(args), |r| r.get::<_, i64>(0))
            .map_or(0, |n| n.max(0) as usize)
    }

    /// [`Cache::remove_downloaded`] before `before` (a Unix time), but an
    /// account in `keep` keeps its downloads back to its own date: a period
    /// kept offline is not removed for being old, only to be downloaded
    /// again. Returns how many bodies went.
    pub(crate) fn remove_downloaded_sparing(
        &self,
        before: i64,
        keep: &[(u32, i64)],
    ) -> rusqlite::Result<usize> {
        // Each account's own cut-off, the older of the two dates.
        let mut cutoff = String::from("CASE m.account_id");
        let mut args: Vec<Value> = Vec::new();
        for (account_id, since) in keep {
            cutoff.push_str(" WHEN ? THEN ?");
            args.push(Value::from(i64::from(*account_id)));
            args.push(Value::from((*since).min(before)));
        }
        cutoff.push_str(" ELSE ? END");
        args.push(Value::from(before));
        let tx = self.conn.unchecked_transaction()?;
        let mut removed = 0;
        for table in DOWNLOADED_TABLES {
            let n = tx.execute(
                &format!(
                    "DELETE FROM {table} WHERE EXISTS (SELECT 1 FROM messages m \
                     WHERE m.account_id = {table}.account_id AND m.folder_path = {table}.folder_path \
                     AND m.uid = {table}.uid AND m.ts < {cutoff})"
                ),
                params_from_iter(args.iter()),
            )?;
            if table == "bodies" {
                removed = n;
            }
        }
        tx.commit()?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::add_msg;
    use super::*;

    /// List these folders for account 1, as its last folder listing did.
    fn listed(c: &Cache, paths: &[&str]) {
        let folders: Vec<crate::models::Folder> = paths
            .iter()
            .enumerate()
            .map(|(i, p)| crate::models::Folder {
                id: i as u32 + 1,
                account_id: 1,
                name: p.to_string(),
                path: p.to_string(),
                kind: crate::models::FolderKind::Custom,
                unread: 0,
                childless: false,
            })
            .collect();
        c.save_folders(1, &folders);
    }

    fn set_id(c: &Cache, folder: &str, uid: u32, id: &str) {
        c.conn
            .execute(
                "UPDATE messages SET message_id = ?1 WHERE folder_path = ?2 AND uid = ?3",
                rusqlite::params![id, folder, uid],
            )
            .unwrap();
    }

    #[test]
    fn lists_what_the_period_still_lacks() {
        let c = Cache::in_memory().unwrap();
        listed(&c, &["INBOX", "Trash"]);
        add_msg(&c, "INBOX", 1, "a", "old", 100);
        add_msg(&c, "INBOX", 2, "a", "kept", 300);
        add_msg(&c, "INBOX", 3, "a", "new", 400);
        add_msg(&c, "Trash", 4, "a", "binned", 500);
        c.save_body(1, "INBOX", 2, "<p>kept</p>");
        let skip = vec!["Trash".to_string()];
        assert_eq!(
            c.offline_missing(1, Some(200), false, false, &skip, 10),
            vec![("INBOX".to_string(), 3)]
        );
        assert_eq!(
            c.offline_missing(1, Some(i64::MIN), false, false, &skip, 10)
                .len(),
            2
        );
        // The test rows all carry a paperclip: with attachments asked for,
        // the kept body without its files is listed again.
        assert_eq!(
            c.offline_missing(1, Some(200), true, false, &skip, 10)
                .len(),
            2
        );
        c.mark_attachments_checked(1, "INBOX", 2);
        assert_eq!(
            c.offline_missing(1, Some(200), true, false, &skip, 10)
                .len(),
            1
        );
    }

    #[test]
    fn one_download_serves_every_copy() {
        let c = Cache::in_memory().unwrap();
        listed(&c, &["INBOX", "[Gmail]/All Mail"]);
        add_msg(&c, "INBOX", 1, "a", "s", 400);
        add_msg(&c, "[Gmail]/All Mail", 9, "a", "s", 400);
        set_id(&c, "INBOX", 1, "<x@y>");
        set_id(&c, "[Gmail]/All Mail", 9, "<x@y>");
        assert_eq!(
            c.offline_missing(1, Some(0), false, false, &[], 10).len(),
            1
        );
        c.save_body(1, "[Gmail]/All Mail", 9, "<p>s</p>");
        assert!(c
            .offline_missing(1, Some(0), false, false, &[], 10)
            .is_empty());
    }

    /// Copying to On This Computer (#552) lists every message not copied
    /// yet, whatever its date or body, and a copy by Message-ID covers the
    /// message wherever it moved since.
    #[test]
    fn copying_lists_what_is_not_copied_yet() {
        let c = Cache::in_memory().unwrap();
        listed(&c, &["INBOX", "Work", "Archive"]);
        add_msg(&c, "INBOX", 1, "a", "old", 100);
        add_msg(&c, "INBOX", 2, "a", "new", 400);
        add_msg(&c, "Work", 3, "a", "no id", 300);
        set_id(&c, "INBOX", 1, "m1@x");
        set_id(&c, "INBOX", 2, "m2@x");
        c.save_body(1, "INBOX", 2, "<p>new</p>");
        assert!(c.offline_missing(1, None, false, false, &[], 10).is_empty());
        assert_eq!(c.offline_missing(1, None, false, true, &[], 10).len(), 3);
        c.note_local_copy(1, &("m2@x".into(), "INBOX".into(), 2));
        c.note_local_copy(1, &(String::new(), "Work".into(), 3));
        assert_eq!(
            c.offline_missing(1, None, false, true, &[], 10),
            vec![("INBOX".to_string(), 1)]
        );
        // With a period too: its missing bodies, and what is not copied.
        assert_eq!(
            c.offline_missing(1, Some(350), false, true, &[], 10).len(),
            1
        );
        // Moved: the same Message-ID elsewhere is copied already.
        c.delete_message(1, "INBOX", 2);
        add_msg(&c, "Archive", 8, "a", "new", 400);
        set_id(&c, "Archive", 8, "m2@x");
        assert!(c.is_local_copy(1, &("m2@x".into(), "Archive".into(), 8)));
        assert_eq!(c.offline_missing(1, None, false, true, &[], 10).len(), 1);
    }

    #[test]
    fn what_is_left_to_copy_is_counted_once_per_message() {
        let c = Cache::in_memory().unwrap();
        listed(&c, &["INBOX", "All Mail", "Trash"]);
        add_msg(&c, "INBOX", 1, "a", "s", 400);
        add_msg(&c, "All Mail", 2, "a", "s", 400);
        add_msg(&c, "All Mail", 3, "a", "t", 300);
        add_msg(&c, "Trash", 4, "a", "u", 200);
        add_msg(&c, "Gone", 5, "a", "v", 100);
        set_id(&c, "INBOX", 1, "s@x");
        set_id(&c, "All Mail", 2, "s@x");
        let skip = vec!["Trash".to_string()];
        assert_eq!(c.local_copy_pending(1, &skip), 2);
        c.note_local_copy(1, &("s@x".into(), "INBOX".into(), 1));
        assert_eq!(c.local_copy_pending(1, &skip), 1);
    }

    /// A message filed twice is fetched from its Inbox, not from All Mail.
    #[test]
    fn the_inbox_copy_comes_before_all_mail() {
        let c = Cache::in_memory().unwrap();
        listed(&c, &["All Mail", "INBOX", "Work"]);
        for (folder, uid) in [("All Mail", 1), ("INBOX", 2), ("All Mail", 3), ("Work", 4)] {
            add_msg(&c, folder, uid, "a", "s", 400);
        }
        set_id(&c, "All Mail", 1, "in@x");
        set_id(&c, "INBOX", 2, "in@x");
        set_id(&c, "All Mail", 3, "work@x");
        set_id(&c, "Work", 4, "work@x");
        let mut got = c.offline_missing(1, None, false, true, &[], 10);
        got.sort();
        assert_eq!(got, [("INBOX".to_string(), 2), ("Work".to_string(), 4)]);
    }

    /// Rows under a folder the account does not list (gone from the
    /// server, or left from before rows followed their accounts) are never
    /// asked for.
    #[test]
    fn only_listed_folders_are_downloaded() {
        let c = Cache::in_memory().unwrap();
        listed(&c, &["INBOX"]);
        add_msg(&c, "INBOX", 1, "a", "here", 100);
        add_msg(&c, "[Gmail]/All Mail", 2, "a", "ghost", 500);
        assert_eq!(
            c.offline_missing(1, Some(0), false, true, &[], 10),
            vec![("INBOX".to_string(), 1)]
        );
    }

    #[test]
    fn removal_spares_a_period_kept_offline() {
        let c = Cache::in_memory().unwrap();
        add_msg(&c, "INBOX", 1, "a", "old", 100);
        add_msg(&c, "INBOX", 2, "a", "older", 50);
        c.save_body(1, "INBOX", 1, "<p>old</p>");
        c.save_body(1, "INBOX", 2, "<p>older</p>");
        // Account 1 keeps everything since 80: only the message from 50 goes.
        assert_eq!(c.remove_downloaded_sparing(200, &[(1, 80)]).unwrap(), 1);
        assert!(c.load_body(1, "INBOX", 1).is_some());
        // Kept all along: nothing goes.
        assert_eq!(
            c.remove_downloaded_sparing(200, &[(1, i64::MIN)]).unwrap(),
            0
        );
        // Another account's period spares nothing here.
        assert_eq!(
            c.remove_downloaded_sparing(200, &[(2, i64::MIN)]).unwrap(),
            1
        );
    }
}
