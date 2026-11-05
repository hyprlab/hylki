//! An assistant's search of a JMAP server: as for the user's own (#509), one
//! `Email/query` across the account, and what the index does not hold yet is
//! fetched and saved so the assistant can go on to name it.

use std::collections::HashMap;

use super::text_search::text_filter;
use super::JmapWorker;
use crate::worker::ids::hash_uid;
use crate::worker::jmap::messages::jmap_get_messages;
use crate::worker::jmap::wire::{args, call, jmap_call, jmap_session, CAP_CORE, CAP_MAIL};
use crate::worker::{blocking, ServerFound, ServerSearch, WorkerEvent, TEXT_SEARCH_LIMIT};

/// The most summaries asked for at once: servers limit one get, 500 often.
const GET_CHUNK: usize = 200;

impl<E: Fn(WorkerEvent)> JmapWorker<E> {
    pub(super) async fn on_ask_server(&mut self, ask: ServerSearch) {
        let account_id = self.account_id;
        let Some(s) = jmap_session(&self.account, &mut self.state, &|_| {}).await else {
            ask.refuse(account_id, "the server cannot be reached right now");
            return;
        };
        let sess = s.clone();
        let filter = text_filter(&ask.terms);
        let found = blocking(move || {
            let r = jmap_call(
                &sess,
                &[CAP_CORE, CAP_MAIL],
                vec![
                    call(
                        0,
                        "Email/query",
                        serde_json::json!({
                            "accountId": sess.account,
                            "filter": filter,
                            "sort": [{ "property": "receivedAt", "isAscending": false }],
                            "limit": TEXT_SEARCH_LIMIT,
                        }),
                    ),
                    call(
                        1,
                        "Email/get",
                        serde_json::json!({
                            "accountId": sess.account,
                            "#ids": { "resultOf": "0", "name": "Email/query", "path": "/ids" },
                            "properties": ["id", "blobId", "mailboxIds"],
                        }),
                    ),
                ],
            )?;
            Ok::<_, String>(args(&r, 1)["list"].as_array().cloned().unwrap_or_default())
        })
        .await;
        let found = match found {
            Ok(found) => found,
            Err(e) => {
                tracing::info!("[account {account_id}] mcp search: {e}");
                ask.refuse(account_id, "the server did not answer the search");
                return;
            }
        };
        // Each message under each of its mailboxes asked about, newest first.
        let mut by_path: HashMap<String, Vec<(u32, String)>> = HashMap::new();
        for e in &found {
            let Some(id) = e["id"].as_str() else { continue };
            let blob = e["blobId"].as_str().unwrap_or("").to_string();
            let uid = hash_uid(id);
            for (mbox, _) in e["mailboxIds"].as_object().into_iter().flatten() {
                if let Some((_, path)) = self.state.path_of(mbox) {
                    if ask.paths.contains(&path) {
                        self.state.uids.insert(uid, (id.to_string(), blob.clone()));
                        by_path.entry(path).or_default().push((uid, id.to_string()));
                    }
                }
            }
        }
        let mut part = ServerFound {
            account_id,
            done: true,
            ..Default::default()
        };
        for (path, mut uids) in by_path {
            part.found += uids.len();
            uids.truncate(ask.keep);
            part.hits
                .extend(uids.iter().map(|(uid, _)| (path.clone(), *uid)));
            if let Err(e) = self.index_missing(&s, &path, &uids).await {
                tracing::info!("[account {account_id}] mcp search: {path}: {e}");
                part.trouble = Some(format!("could not fetch what it found in {path}"));
            }
        }
        let _ = ask.answer.send(part);
    }

    /// Fetch and save the summaries of what the index does not hold.
    async fn index_missing(
        &self,
        s: &crate::worker::jmap::wire::JmapSession,
        path: &str,
        uids: &[(u32, String)],
    ) -> Result<(), String> {
        let Some(cache) = self.cache.as_ref() else {
            return Err("there is no index to save to".into());
        };
        let numbers: Vec<u32> = uids.iter().map(|(uid, _)| *uid).collect();
        let known: std::collections::HashSet<u32> = cache
            .indexed_message_ids(self.account_id, path, &numbers)
            .into_iter()
            .map(|(uid, _)| uid)
            .collect();
        let missing: Vec<String> = uids
            .iter()
            .filter(|(uid, _)| !known.contains(uid))
            .map(|(_, id)| id.clone())
            .collect();
        let account_id = self.account_id;
        for chunk in missing.chunks(GET_CHUNK) {
            let (sess, ids) = (s.clone(), chunk.to_vec());
            let got = blocking(move || jmap_get_messages(&sess, &ids, account_id, 0)).await?;
            let messages: Vec<_> = got.into_iter().map(|(m, _, _)| m).collect();
            cache.upsert_messages(account_id, path, &messages);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Cache;
    use crate::worker::jmap::folders::{jmap_create_mailbox, refresh_jmap_folders};
    use crate::worker::jmap::index::JmapState;
    use crate::worker::jmap::ops::{jmap_destroy_all, jmap_import_email};
    use tokio::sync::mpsc;

    /// With `JMAP_LIVE=url,user,pw`: mail found by its words is saved though
    /// the index never saw it, and only the folders asked about answer.
    #[test]
    #[ignore]
    fn live_a_search_brings_in_what_the_index_lacks() {
        let Ok(spec) = std::env::var("JMAP_LIVE") else {
            return;
        };
        let mut parts = spec.splitn(3, ',');
        let account = crate::config::AccountConfig {
            imap_host: parts.next().unwrap_or_default().into(),
            imap_port: 443,
            username: parts.next().unwrap_or_default().into(),
            password: parts.next().unwrap_or_default().into(),
            ..crate::worker::test_support::sample_account()
        };
        let (_tx, rx) = mpsc::unbounded_channel();
        let (push_tx, push_rx) = mpsc::unbounded_channel::<()>();
        let mut w = JmapWorker {
            account_id: 1,
            account: account.clone(),
            rx,
            emit: |_: WorkerEvent| {},
            cache: Some(Cache::in_memory().unwrap()),
            state: JmapState::new(&account),
            last_auto_empty: None,
            last_list_clean: None,
            push_tx,
            push_rx,
            push_enabled: false,
            push_started: false,
            offline: Default::default(),
        };
        let pid = std::process::id();
        let (asked, other) = (format!("Asked-{pid}"), format!("Other-{pid}"));
        let word = format!("zanzibar{pid}");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let s = jmap_session(&w.account, &mut w.state, &w.emit)
                .await
                .expect("session");
            let asked_id = jmap_create_mailbox(&s, &asked, None).expect("create");
            let other_id = jmap_create_mailbox(&s, &other, None).expect("create");
            let mut ids = Vec::new();
            for (n, mailbox) in [(1, &asked_id), (2, &asked_id), (3, &other_id)] {
                let raw = format!(
                    "From: Ann <ann@example.test>\r\nTo: me@example.test\r\nSubject: note {n}\r\n\
                     Message-ID: <hylki-mcp-{pid}-{n}@example.test>\r\n\r\nThe {word} file.\r\n"
                );
                let email = serde_json::json!({ "mailboxIds": { mailbox.clone(): true } });
                ids.push(jmap_import_email(&s, raw.as_bytes(), email).expect("import"));
            }
            refresh_jmap_folders(&s, 1, None, &mut w.state, &w.emit).await;

            // The server indexes text a moment after an import.
            let mut part = ServerFound::default();
            for _ in 0..20 {
                let (answer, mut answers) = mpsc::unbounded_channel();
                let ask = ServerSearch {
                    terms: vec![word.clone()],
                    paths: vec![asked.clone()],
                    keep: 10,
                    answer,
                };
                w.on_ask_server(ask).await;
                part = answers.recv().await.expect("an answer");
                if part.hits.len() == 2 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            assert!(part.done && part.trouble.is_none(), "{part:?}");
            assert_eq!(part.hits.len(), 2, "{part:?}");
            let cache = w.cache.as_ref().unwrap();
            let mut subjects: Vec<String> = part
                .hits
                .iter()
                .map(|(path, uid)| {
                    assert_eq!(path, &asked);
                    assert!(w.state.uids.contains_key(uid), "its id is known");
                    cache.message_at(1, path, *uid).expect("saved").subject
                })
                .collect();
            subjects.sort();
            assert_eq!(subjects, ["note 1", "note 2"]);

            jmap_destroy_all(&s, &ids).expect("drop mail");
            jmap_call(
                &s,
                &[CAP_CORE, CAP_MAIL],
                vec![call(
                    0,
                    "Mailbox/set",
                    serde_json::json!({ "accountId": s.account, "destroy": [asked_id, other_id] }),
                )],
            )
            .expect("drop mailboxes");
        });
    }
}
