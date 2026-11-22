//! The addresses mail was sent to and received from, for the
//! composer's suggestions.

use rusqlite::params;

use super::Cache;

/// One address in the mail history.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct AddressSeen {
    pub(crate) name: String,
    /// Lower case.
    pub(crate) email: String,
    /// Messages it is on, sent or received.
    pub(crate) seen: u32,
    /// Messages the user sent to it.
    pub(crate) sent: u32,
}

/// The mail an address history is read from.
#[derive(Debug, Default, Clone)]
pub(crate) struct AddressScope {
    /// One account's mail, or every account's.
    pub(crate) account: Option<u32>,
    /// One folder of that account, by path.
    pub(crate) folder: Option<String>,
    /// Folders left out, by account and path: Trash and Junk, whose
    /// senders are mostly not people anyone wants as contacts.
    pub(crate) skip: std::collections::HashSet<(u32, String)>,
}

impl Cache {
    /// Record recipients the user has sent to, so they autocomplete even before
    /// the Sent folder syncs. Each send bumps the address's frequency. The app
    /// records at the moment of sending, whatever route the message then
    /// takes (straight out, the Outbox, Send Later).
    pub(crate) fn record_addresses(&self, entries: &[(String, String)]) {
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

    /// Aggregate every address seen in stored mail (senders received +
    /// recipients sent/recorded), most-seen first. `own` are the user's
    /// addresses, lower case: a message from one of them was sent, and its
    /// recipients count as written to.
    pub(crate) fn address_history(
        &self,
        own: &std::collections::HashSet<String>,
    ) -> Vec<AddressSeen> {
        self.address_history_in(own, &AddressScope::default())
    }

    /// [`Cache::address_history`] over the mail `scope` takes in (#677).
    pub(crate) fn address_history_in(
        &self,
        own: &std::collections::HashSet<String>,
        scope: &AddressScope,
    ) -> Vec<AddressSeen> {
        use std::collections::{HashMap, HashSet};
        let mut counts: HashMap<String, AddressSeen> = HashMap::new();
        fn entry<'a>(
            counts: &'a mut HashMap<String, AddressSeen>,
            name: &str,
            email: &str,
        ) -> Option<&'a mut AddressSeen> {
            let email = email.trim();
            if email.is_empty() || !email.contains('@') {
                return None;
            }
            let entry = counts
                .entry(email.to_lowercase())
                .or_insert_with(|| AddressSeen {
                    email: email.to_lowercase(),
                    ..Default::default()
                });
            if entry.name.is_empty() && !name.trim().is_empty() && name.trim() != email {
                entry.name = name.trim().to_string();
            }
            if entry.name.is_empty() {
                entry.name = email.to_string();
            }
            Some(entry)
        }

        // The same sent message is often in two folders (Gmail's Sent and
        // All Mail): it counts once.
        let mut sent_ids: HashSet<String> = HashSet::new();
        let mut sent_folder: HashMap<String, u32> = HashMap::new();
        if let Ok(mut stmt) = self.conn.prepare(
            "SELECT from_name, from_addr, recipients, cc, message_id, account_id, folder_path \
                 FROM messages",
        ) {
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0).unwrap_or_default(),
                    r.get::<_, String>(1).unwrap_or_default(),
                    r.get::<_, String>(2).unwrap_or_default(),
                    r.get::<_, String>(3).unwrap_or_default(),
                    r.get::<_, String>(4).unwrap_or_default(),
                    r.get::<_, u32>(5).unwrap_or_default(),
                    r.get::<_, String>(6).unwrap_or_default(),
                ))
            });
            if let Ok(rows) = rows {
                for (from_name, from_addr, to, cc, message_id, account, folder) in rows.flatten() {
                    if scope.account.is_some_and(|a| a != account)
                        || scope.folder.as_ref().is_some_and(|f| *f != folder)
                        || scope.skip.contains(&(account, folder))
                    {
                        continue;
                    }
                    if let Some(e) = entry(&mut counts, &from_name, &from_addr) {
                        e.seen += 1;
                    }
                    let sent = own.contains(&from_addr.trim().to_lowercase())
                        && (message_id.is_empty() || sent_ids.insert(message_id));
                    for list in [to, cc] {
                        for addr in list.split(',') {
                            if let Some(e) = entry(&mut counts, addr.trim(), addr.trim()) {
                                e.seen += 1;
                                if sent {
                                    *sent_folder.entry(e.email.clone()).or_default() += 1;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Recorded at each send, so mostly the same sends as the Sent folder
        // holds: the larger of the two, not their sum.
        let mut recorded: HashMap<String, u32> = HashMap::new();
        // Sends are recorded with no account, so only all mail takes them.
        let all_accounts = scope.account.is_none();
        if let Some(mut stmt) = self
            .conn
            .prepare("SELECT name, email, count FROM addresses")
            .ok()
            .filter(|_| all_accounts)
        {
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0).unwrap_or_default(),
                    r.get::<_, String>(1).unwrap_or_default(),
                    r.get::<_, u32>(2).unwrap_or(0),
                ))
            });
            if let Ok(rows) = rows {
                for (name, email, c) in rows.flatten() {
                    if let Some(e) = entry(&mut counts, &name, &email) {
                        e.seen += c;
                        recorded.insert(e.email.clone(), c);
                    }
                }
            }
        }

        let mut out: Vec<AddressSeen> = counts
            .into_values()
            .map(|mut a| {
                let folder = sent_folder.get(&a.email).copied().unwrap_or(0);
                a.sent = folder.max(recorded.get(&a.email).copied().unwrap_or(0));
                a
            })
            .collect();
        out.sort_by_key(|a| std::cmp::Reverse(a.seen));
        out
    }

    /// Every address in one account's mail, lower case: what the composer
    /// keeps of the history when that account suggests only its own (#410).
    pub(crate) fn account_addresses(&self, account_id: u32) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        let Ok(mut stmt) = self
            .conn
            .prepare("SELECT from_addr, recipients, cc FROM messages WHERE account_id = ?1")
        else {
            return out;
        };
        let rows = stmt.query_map(params![account_id], |r| {
            Ok((
                r.get::<_, String>(0).unwrap_or_default(),
                r.get::<_, String>(1).unwrap_or_default(),
                r.get::<_, String>(2).unwrap_or_default(),
            ))
        });
        for (from, to, cc) in rows.into_iter().flatten().flatten() {
            for addr in std::iter::once(from.as_str())
                .chain(to.split(','))
                .chain(cc.split(','))
            {
                let addr = addr.trim();
                if addr.contains('@') {
                    out.insert(addr.to_lowercase());
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::Cache;
    use rusqlite::params;

    fn add(c: &Cache, folder: &str, uid: u32, from: &str, to: &str, message_id: &str) {
        c.conn.execute(
            "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, recipients, cc, message_id, references_) \
             VALUES (1, ?1, ?2, '', ?3, '', '', 0, 0, 0, 0, ?4, '', ?5, '')",
            params![folder, uid, from, to, message_id],
        ).unwrap();
    }

    /// Mail from the user's own address counts as sent to its recipients,
    /// once per message whichever folders hold it; recorded sends count
    /// as the same sends, not more.
    #[test]
    fn sent_counts_the_messages_written_to_each_address() {
        let c = Cache::in_memory().unwrap();
        add(&c, "INBOX", 1, "news@shop.example", "me@example.org", "<a>");
        add(&c, "INBOX", 2, "news@shop.example", "me@example.org", "<b>");
        add(&c, "INBOX", 3, "news@shop.example", "me@example.org", "<c>");
        add(&c, "Sent", 4, "ME@example.org", "ann@example.org", "<d>");
        add(
            &c,
            "All Mail",
            5,
            "me@example.org",
            "ann@example.org",
            "<d>",
        );
        add(&c, "Sent", 6, "me@example.org", "bob@example.org", "<e>");
        c.record_addresses(&[("Bob".into(), "bob@example.org".into())]);
        c.record_addresses(&[("Bob".into(), "bob@example.org".into())]);
        let own = ["me@example.org".to_string()].into_iter().collect();
        let history = c.address_history(&own);
        let get = |email: &str| history.iter().find(|a| a.email == email).unwrap();
        assert_eq!(
            (get("news@shop.example").seen, get("news@shop.example").sent),
            (3, 0)
        );
        assert_eq!(get("ann@example.org").sent, 1);
        assert_eq!(get("bob@example.org").sent, 2);
    }

    /// A scope leaves out the folders it skips, and with one account the
    /// sends recorded with none; with a folder, it reads that folder alone.
    #[test]
    fn a_scope_reads_only_its_mail() {
        let c = Cache::in_memory().unwrap();
        add(&c, "INBOX", 1, "ann@example.org", "me@example.org", "<a>");
        add(&c, "Junk", 2, "spam@example.net", "me@example.org", "<b>");
        c.record_addresses(&[("Bob".into(), "bob@example.org".into())]);
        let own = ["me@example.org".to_string()].into_iter().collect();
        add(&c, "Work", 3, "cy@example.org", "me@example.org", "<c>");
        let mut scope = super::AddressScope {
            account: Some(1),
            folder: None,
            skip: [(1, "Junk".to_string())].into_iter().collect(),
        };
        let found = |scope: &super::AddressScope| -> Vec<String> {
            c.address_history_in(&own, scope)
                .into_iter()
                .map(|a| a.email)
                .collect()
        };
        let all = found(&scope);
        assert!(all.contains(&"ann@example.org".to_string()));
        assert!(all.contains(&"cy@example.org".to_string()));
        assert!(!all.contains(&"spam@example.net".to_string()));
        assert!(!all.contains(&"bob@example.org".to_string()));
        scope.folder = Some("Work".to_string());
        // The user's own address is a recipient there too; the importer
        // drops it, not the history.
        let mut work = found(&scope);
        work.sort();
        assert_eq!(work, vec!["cy@example.org", "me@example.org"]);
    }
}
