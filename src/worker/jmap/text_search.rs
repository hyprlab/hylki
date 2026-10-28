//! Asking a JMAP server for a search's words (#509): one `Email/query`
//! across the account with a `text` condition for each, then the mailboxes
//! of what it found, in one round trip.

use std::collections::HashMap;

use super::JmapWorker;
use crate::worker::ids::hash_uid;
use crate::worker::jmap::wire::{args, call, jmap_call, jmap_session, CAP_CORE, CAP_MAIL};
use crate::worker::{blocking, WorkerEvent, TEXT_SEARCH_LIMIT};

/// The `Email/query` filter for every term in a message's text.
pub(super) fn text_filter(terms: &[String]) -> serde_json::Value {
    let conditions: Vec<serde_json::Value> = terms
        .iter()
        .map(|t| serde_json::json!({ "text": t }))
        .collect();
    serde_json::json!({ "operator": "AND", "conditions": conditions })
}

impl<E: Fn(WorkerEvent)> JmapWorker<E> {
    /// Search the account, and say what it holds in `paths`.
    pub(super) async fn on_find_text(&mut self, gen: u64, terms: Vec<String>, paths: Vec<String>) {
        let account_id = self.account_id;
        let found = match jmap_session(&self.account, &mut self.state, &|_| {}).await {
            Some(s) => {
                let filter = text_filter(&terms);
                blocking(move || {
                    let r = jmap_call(
                        &s,
                        &[CAP_CORE, CAP_MAIL],
                        vec![
                            call(
                                0,
                                "Email/query",
                                serde_json::json!({
                                    "accountId": s.account,
                                    "filter": filter,
                                    "sort": [{ "property": "receivedAt", "isAscending": false }],
                                    "limit": TEXT_SEARCH_LIMIT,
                                }),
                            ),
                            call(
                                1,
                                "Email/get",
                                serde_json::json!({
                                    "accountId": s.account,
                                    "#ids": { "resultOf": "0", "name": "Email/query", "path": "/ids" },
                                    "properties": ["id", "mailboxIds"],
                                }),
                            ),
                        ],
                    )?;
                    Ok::<_, String>(args(&r, 1)["list"].as_array().cloned().unwrap_or_default())
                })
                .await
            }
            None => Err("no session".to_string()),
        };
        let found = found.unwrap_or_else(|e| {
            tracing::warn!("[account {account_id}] search: {e}");
            Vec::new()
        });
        // Each message under each of its mailboxes asked about.
        let mut by_path: HashMap<String, Vec<u32>> = HashMap::new();
        for e in &found {
            let Some(id) = e["id"].as_str() else { continue };
            for (mbox, _) in e["mailboxIds"].as_object().into_iter().flatten() {
                if let Some((_, path)) = self.state.path_of(mbox) {
                    if paths.contains(&path) {
                        by_path.entry(path).or_default().push(hash_uid(id));
                    }
                }
            }
        }
        let hits: Vec<(String, u32, String)> = by_path
            .into_iter()
            .flat_map(|(path, uids)| {
                self.cache
                    .as_ref()
                    .map(|c| c.indexed_message_ids(account_id, &path, &uids))
                    .unwrap_or_default()
                    .into_iter()
                    .map(move |(uid, id)| (path.clone(), uid, id))
            })
            .collect();
        tracing::debug!("[account {account_id}] search: {} hits", hits.len());
        (self.emit)(WorkerEvent::TextFound {
            gen,
            hits,
            done: true,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With `JMAP_LIVE=url,user,pw` (as for the JMAP `live` test): mail with
    /// the words in its body is found, and only it. Stalwart indexes text a
    /// moment after an import, so the query is asked again for a while.
    #[test]
    #[ignore]
    fn live_text_search_finds_body_words() {
        use crate::worker::jmap::folders::jmap_create_mailbox;
        use crate::worker::jmap::messages::jmap_query_ids;
        use crate::worker::jmap::ops::{jmap_destroy_all, jmap_import_email};
        use crate::worker::jmap::wire::jmap_connect;
        let Ok(spec) = std::env::var("JMAP_LIVE") else {
            return;
        };
        let mut parts = spec.splitn(3, ',');
        let acc = crate::config::AccountConfig {
            imap_host: parts.next().unwrap_or_default().into(),
            imap_port: 443,
            username: parts.next().unwrap_or_default().into(),
            password: parts.next().unwrap_or_default().into(),
            jmap_token: std::env::var_os("JMAP_LIVE_TOKEN").is_some(),
            ..crate::worker::test_support::sample_account()
        };
        let s = jmap_connect(&acc).expect("session");
        let pid = std::process::id();
        let made = jmap_create_mailbox(&s, &format!("Search-{pid}"), None).expect("create");
        let mut ids = Vec::new();
        for (n, body) in [
            (1, "The zanzibar invoice from Mario Rossi is attached."),
            (2, "Rossi said the zanzibar figures are late."),
            (3, "Nothing to see here."),
        ] {
            let raw = format!(
                "From: Ann <ann@example.test>\r\nTo: me@example.test\r\nSubject: note {n}\r\n\
                 Message-ID: <hylki-509-jmap-{pid}-{n}@example.test>\r\n\r\n{body}\r\n"
            );
            let email = serde_json::json!({ "mailboxIds": { made.clone(): true } });
            ids.push(jmap_import_email(&s, raw.as_bytes(), email).expect("import"));
        }
        let query = |terms: &[&str]| {
            let terms: Vec<String> = terms.iter().map(|t| t.to_string()).collect();
            let filter = serde_json::json!({
                "operator": "AND",
                "conditions": [text_filter(&terms), { "inMailbox": made }],
            });
            let mut found = jmap_query_ids(&s, filter, 50).expect("query");
            found.sort();
            found
        };
        let mut both = Vec::new();
        for _ in 0..20 {
            both = query(&["zanzibar"]);
            if both.len() == 2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let invoice = query(&["zanzibar", "invoice"]);
        jmap_destroy_all(&s, &ids).expect("drop mail");
        jmap_call(
            &s,
            &[CAP_CORE, CAP_MAIL],
            vec![call(
                0,
                "Mailbox/set",
                serde_json::json!({ "accountId": s.account, "destroy": [made] }),
            )],
        )
        .expect("drop mailbox");
        let mut expect_both = vec![ids[0].clone(), ids[1].clone()];
        expect_both.sort();
        assert_eq!(both, expect_both);
        assert_eq!(invoice, [ids[0].clone()]);
    }

    #[test]
    fn every_term_is_a_text_condition() {
        assert_eq!(
            text_filter(&["mario rossi".into(), "4471".into()]),
            serde_json::json!({
                "operator": "AND",
                "conditions": [{ "text": "mario rossi" }, { "text": "4471" }],
            })
        );
    }
}
