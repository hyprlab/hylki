//! Asking Microsoft 365 for a search's words (#509): one `$search` across
//! the mailbox, each result with the folder it is filed in.

use std::collections::HashMap;

use super::GraphWorker;
use crate::worker::graph::http::{graph_paged, graph_token};
use crate::worker::{blocking, WorkerEvent, TEXT_SEARCH_LIMIT};

/// The `$search` value for every term: a word as it is, a phrase in quotes,
/// all inside the quotes the parameter takes, encoded for the URL.
pub(super) fn search_param(terms: &[String]) -> String {
    let kql: Vec<String> = terms
        .iter()
        .map(|t| {
            let t = t.replace(['"', '\\'], "");
            if t.contains(char::is_whitespace) {
                format!("\\\"{t}\\\"")
            } else {
                t
            }
        })
        .collect();
    crate::encoding::percent::encode(&format!("\"{}\"", kql.join(" ")))
}

impl<E: Fn(crate::worker::WorkerEvent)> GraphWorker<E> {
    /// Search the mailbox, and say what it holds in `paths`.
    pub(super) async fn on_find_text(&mut self, gen: u64, terms: Vec<String>, paths: Vec<String>) {
        let account_id = self.account_id;
        let found = match graph_token(&self.account, &|_| {}).await {
            Some(token) => {
                let url = format!(
                    "{}/me/messages?$search={}&$select=id,parentFolderId&$top=250",
                    self.state.base,
                    search_param(&terms)
                );
                blocking(move || graph_paged(&token, &url, TEXT_SEARCH_LIMIT)).await
            }
            None => Err("no token".to_string()),
        };
        let found = found.unwrap_or_else(|e| {
            tracing::warn!("[account {account_id}] search: {e}");
            Vec::new()
        });
        let path_of: HashMap<&str, &str> = self
            .state
            .folders
            .iter()
            .map(|(path, (_, gid))| (gid.as_str(), path.as_str()))
            .collect();
        let mut by_path: HashMap<String, Vec<u32>> = HashMap::new();
        for m in &found {
            let (Some(id), Some(folder)) = (m["id"].as_str(), m["parentFolderId"].as_str()) else {
                continue;
            };
            if let Some(path) = path_of
                .get(folder)
                .filter(|p| paths.iter().any(|q| q == *p))
            {
                let uid = crate::worker::ids::hash_uid(id);
                self.state.uids.insert(uid, id.to_string());
                by_path.entry(path.to_string()).or_default().push(uid);
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

    #[test]
    fn words_stay_words_and_a_phrase_is_quoted() {
        let decoded = |s: String| {
            let mut out = Vec::new();
            let b = s.as_bytes();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' {
                    out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            String::from_utf8(out).unwrap()
        };
        assert_eq!(
            decoded(search_param(&["mario rossi".into(), "4471".into()])),
            r#""\"mario rossi\" 4471""#
        );
    }
}
