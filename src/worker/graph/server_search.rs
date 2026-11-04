//! An assistant's search of Microsoft 365: as for the user's own (#509), one
//! `$search` across the mailbox, and what the index does not hold yet is saved
//! so the assistant can go on to name it.

use std::collections::{HashMap, HashSet};

use super::text_search::search_param;
use super::GraphWorker;
use crate::models::Message;
use crate::worker::graph::http::{graph_paged, graph_token};
use crate::worker::graph::messages::{graph_message, GRAPH_MSG_SELECT};
use crate::worker::{blocking, ServerFound, ServerSearch, WorkerEvent, TEXT_SEARCH_LIMIT};

impl<E: Fn(WorkerEvent)> GraphWorker<E> {
    pub(super) async fn on_ask_server(&mut self, ask: ServerSearch) {
        let account_id = self.account_id;
        let Some(token) = graph_token(&self.account, &|_| {}).await else {
            ask.refuse(account_id, "the server cannot be reached right now");
            return;
        };
        // The select has to end with its expand, so the folder goes first.
        let select = GRAPH_MSG_SELECT.replacen("$select=", "$select=parentFolderId,", 1);
        let url = format!(
            "{}/me/messages?$search={}&$top=250&{select}",
            self.state.base,
            search_param(&ask.terms)
        );
        let found = blocking(move || graph_paged(&token, &url, TEXT_SEARCH_LIMIT)).await;
        let found = match found {
            Ok(found) => found,
            Err(e) => {
                tracing::info!("[account {account_id}] mcp search: {e}");
                ask.refuse(account_id, "the server did not answer the search");
                return;
            }
        };
        let part = file_found(
            &found,
            &ask,
            account_id,
            &self.state.folders,
            &mut self.state.uids,
            self.cache.as_ref(),
        );
        let _ = ask.answer.send(part);
    }
}

/// What a search found, kept to the folders asked about, and the newest of
/// each: saved where the index lacks it, and noted so its id is known.
fn file_found(
    found: &[serde_json::Value],
    ask: &ServerSearch,
    account_id: u32,
    folders: &HashMap<String, (u32, String)>,
    uids: &mut HashMap<u32, String>,
    cache: Option<&crate::cache::Cache>,
) -> ServerFound {
    let path_of: HashMap<&str, &str> = folders
        .iter()
        .map(|(path, (_, gid))| (gid.as_str(), path.as_str()))
        .collect();
    let mut by_path: HashMap<String, Vec<(Message, String)>> = HashMap::new();
    for v in found {
        let Some(path) = v["parentFolderId"]
            .as_str()
            .and_then(|folder| path_of.get(folder))
            .filter(|p| ask.paths.iter().any(|q| q == *p))
        else {
            continue;
        };
        if let Some(listed) = graph_message(v, account_id, 0) {
            by_path.entry(path.to_string()).or_default().push(listed);
        }
    }
    let mut part = ServerFound {
        account_id,
        done: true,
        ..Default::default()
    };
    for (path, mut listed) in by_path {
        part.found += listed.len();
        // A search here comes by relevance, and only the newest are kept.
        listed.sort_by_key(|(m, _)| std::cmp::Reverse(m.timestamp));
        listed.truncate(ask.keep);
        let numbers: Vec<u32> = listed.iter().map(|(m, _)| m.uid).collect();
        let known: HashSet<u32> = cache
            .map(|c| c.indexed_message_ids(account_id, &path, &numbers))
            .unwrap_or_default()
            .into_iter()
            .map(|(uid, _)| uid)
            .collect();
        let mut missing = Vec::new();
        for (m, gid) in listed {
            uids.insert(m.uid, gid);
            part.hits.push((path.clone(), m.uid));
            if !known.contains(&m.uid) {
                missing.push(m);
            }
        }
        if let Some(c) = cache {
            c.upsert_messages(account_id, &path, &missing);
        }
    }
    part
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Cache;

    fn listed(id: &str, folder: &str, subject: &str, at: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "parentFolderId": folder,
            "subject": subject,
            "receivedDateTime": at,
            "internetMessageId": format!("<{id}@example.org>"),
        })
    }

    #[test]
    fn what_is_found_is_saved_and_kept_to_the_folders_asked_about() {
        let cache = Cache::in_memory().unwrap();
        let folders = HashMap::from([
            ("Inbox".to_string(), (1, "gid-inbox".to_string())),
            ("Private".to_string(), (2, "gid-private".to_string())),
        ]);
        let (answer, _answers) = tokio::sync::mpsc::unbounded_channel();
        let ask = ServerSearch {
            terms: vec!["invoice".into()],
            paths: vec!["Inbox".into()],
            keep: 2,
            answer,
        };
        let found = [
            listed("a", "gid-inbox", "Oldest", "2020-01-01T00:00:00Z"),
            listed("b", "gid-inbox", "Newest", "2026-01-01T00:00:00Z"),
            listed("c", "gid-inbox", "Middle", "2023-01-01T00:00:00Z"),
            listed("d", "gid-private", "Kept back", "2026-06-01T00:00:00Z"),
            listed("e", "gid-unknown", "Nowhere", "2026-06-01T00:00:00Z"),
        ];
        let mut uids = HashMap::new();
        let part = file_found(&found, &ask, 1, &folders, &mut uids, Some(&cache));

        assert!(part.done);
        assert_eq!(part.found, 3);
        let subjects: Vec<String> = part
            .hits
            .iter()
            .map(|(path, uid)| cache.message_at(1, path, *uid).unwrap().subject)
            .collect();
        assert_eq!(subjects, ["Newest", "Middle"]);
        assert!(part.hits.iter().all(|(path, _)| path == "Inbox"));
        // Its id is known, for reading it next.
        assert_eq!(uids.len(), 2);
        assert_eq!(uids[&part.hits[0].1], "b");
    }
}
