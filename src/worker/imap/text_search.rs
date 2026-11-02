//! Asking an IMAP server for a search's words (#509): `UID SEARCH TEXT`,
//! one folder a step on the idle ladder, so a message opened meanwhile
//! waits for one folder's search at most. Each folder's answer goes to the
//! list as it comes. On Gmail every message but the binned and the junk is
//! in All Mail, so that one folder stands for the rest.

use std::collections::VecDeque;

use super::ladder::Idle;
use super::wire::{search_uids, select_for_read};
use super::{Emit, ImapWorker};
use crate::cache::Cache;
use crate::models::FolderKind;
use crate::worker::{text_search_is_current, WorkerEvent, TEXT_SEARCH_LIMIT};

/// A search under way: the folders still to ask.
pub(super) struct TextSearch {
    gen: u64,
    terms: Vec<String>,
    paths: VecDeque<String>,
    /// Whether the folder at the front has been tried once already, over a
    /// connection that dropped.
    retried: bool,
}

/// `UID SEARCH` criteria for every term in the text of a message.
pub(super) fn text_query(terms: &[String]) -> String {
    let clean: Vec<String> = terms
        .iter()
        .map(|t| {
            t.chars()
                .filter(|c| !matches!(c, '\r' | '\n'))
                .collect::<String>()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
        })
        .collect();
    let charset = if clean.iter().all(|t| t.is_ascii()) {
        ""
    } else {
        "CHARSET UTF-8 "
    };
    let criteria: Vec<String> = clean.iter().map(|t| format!("TEXT \"{t}\"")).collect();
    format!("{charset}{}", criteria.join(" "))
}

/// The folders to search: on Gmail, All Mail for every label it holds,
/// with the binned and the junk when they were asked for.
fn folders_to_search(cache: Option<&Cache>, account_id: u32, paths: Vec<String>) -> Vec<String> {
    let Some(c) = cache else { return paths };
    let folders = c.load_folders(account_id);
    let all_mail = folders.iter().find(|f| {
        f.kind == FolderKind::Archive
            && (f.path.starts_with("[Gmail]/") || f.path.starts_with("[Google Mail]/"))
            && paths.contains(&f.path)
    });
    let Some(all_mail) = all_mail else {
        return paths;
    };
    let mut out = vec![all_mail.path.clone()];
    out.extend(
        folders
            .iter()
            .filter(|f| matches!(f.kind, FolderKind::Trash | FolderKind::Junk))
            .filter(|f| paths.contains(&f.path))
            .map(|f| f.path.clone()),
    );
    out
}

impl<E: Emit> ImapWorker<E> {
    /// Start a search, in place of any still running.
    pub(super) fn on_find_text(&mut self, gen: u64, terms: Vec<String>, paths: Vec<String>) {
        let paths = folders_to_search(self.cache.as_ref(), self.account_id, paths);
        if paths.is_empty() || terms.is_empty() {
            self.text_search = None;
            (self.emit)(WorkerEvent::TextFound {
                gen,
                hits: Vec::new(),
                done: true,
            });
            return;
        }
        self.text_search = Some(TextSearch {
            gen,
            terms,
            paths: paths.into(),
            retried: false,
        });
    }

    /// One step: search the next folder and say what it holds.
    pub(super) async fn search_one_folder(&mut self) -> Idle {
        let Some(job) = self.text_search.as_mut() else {
            return Idle::Again;
        };
        if !text_search_is_current(job.gen) {
            self.text_search = None;
            return Idle::Again;
        }
        let Some(path) = job.paths.front().cloned() else {
            self.text_search = None;
            return Idle::Again;
        };
        let Some(session) = self.session.as_mut() else {
            return Idle::Again;
        };
        let query = text_query(&job.terms);
        let result = match select_for_read(session, &path).await {
            Ok(()) => search_uids(session, &query).await,
            Err(e) => Err(e),
        };
        let uids = match result {
            Ok(found) => {
                let mut uids: Vec<u32> = found.into_iter().collect();
                uids.sort_unstable_by(|a, b| b.cmp(a));
                uids.truncate(TEXT_SEARCH_LIMIT);
                uids
            }
            // The server answered: this folder cannot be searched so.
            Err(
                e @ (async_imap::error::Error::No(_)
                | async_imap::error::Error::Bad(_)
                | async_imap::error::Error::Parse(_)),
            ) => {
                tracing::warn!("search: {path} refused the text search: {e}");
                Vec::new()
            }
            // The connection went: once more over a new one.
            Err(e) => {
                tracing::info!("search: the connection dropped searching {path}: {e}");
                self.session = None;
                if !job.retried {
                    job.retried = true;
                    return Idle::Again;
                }
                Vec::new()
            }
        };
        job.paths.pop_front();
        job.retried = false;
        let (gen, done) = (job.gen, job.paths.is_empty());
        let hits: Vec<(String, u32, String)> = self
            .cache
            .as_ref()
            .map(|c| c.indexed_message_ids(self.account_id, &path, &uids))
            .unwrap_or_default()
            .into_iter()
            .map(|(uid, id)| (path.clone(), uid, id))
            .collect();
        tracing::debug!("search: {path} holds {} of the words' mail", hits.len());
        (self.emit)(WorkerEvent::TextFound { gen, hits, done });
        if done {
            self.text_search = None;
        }
        Idle::Again
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The uids of `folder` whose text holds every one of `terms`.
    async fn found(
        sess: &mut crate::worker::imap::ImapSession,
        folder: &str,
        terms: &[&str],
    ) -> Vec<u32> {
        let terms: Vec<String> = terms.iter().map(|t| t.to_string()).collect();
        select_for_read(sess, folder).await.expect("select");
        search_uids(sess, &text_query(&terms))
            .await
            .expect("search")
            .into_iter()
            .collect()
    }

    /// With `IMAP_LIVE=host,port,user,password` (as for the IMAP live tests):
    /// words found in different messages' bodies, every one of them asked
    /// for, and the uids named by their Message-ID from the index. Whether
    /// a phrase must be in order is the server's to say: Stalwart matches
    /// its words.
    #[test]
    #[ignore]
    fn live_text_search_finds_body_words() {
        use crate::worker::imap::connect::connect;
        use crate::worker::imap::sync::load_messages;
        use crate::worker::imap::wire::{append_dated, create_box, delete_box};
        let Ok(spec) = std::env::var("IMAP_LIVE") else {
            return;
        };
        let p: Vec<&str> = spec.splitn(4, ',').collect();
        let account = crate::config::AccountConfig {
            imap_host: p[0].into(),
            imap_port: p[1].parse().expect("port"),
            username: p[2].into(),
            password: p[3].into(),
            security: Some(crate::config::ServerSecurity {
                imap_starttls: false,
                imap_accept_invalid_certs: true,
                ..Default::default()
            }),
            ..crate::worker::test_support::sample_account()
        };
        let pid = std::process::id();
        let folder = format!("Search-{pid}");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut sess = connect(&account).await.expect("connect");
            create_box(&mut sess, &folder).await.expect("create");
            for (n, body) in [
                (1, "The invoice from Mario Rossi is attached."),
                (2, "Rossi said the quarterly figures are late."),
                (3, "Nothing to see here."),
            ] {
                let raw = format!(
                    "From: Ann <ann@example.test>\r\nTo: me@example.test\r\nSubject: note {n}\r\n\
                     Message-ID: <hylki-509-{pid}-{n}@example.test>\r\n\r\n{body}\r\n"
                );
                append_dated(&mut sess, &folder, None, None, raw.as_bytes())
                    .await
                    .expect("append");
            }
            let cache = Cache::in_memory().expect("cache");
            let listed = load_messages(1, &mut sess, 1, &folder, true, None)
                .await
                .expect("list");
            cache.save_messages(1, &folder, &listed);
            let subject = |uid: u32| {
                listed
                    .iter()
                    .find(|m| m.uid == uid)
                    .map(|m| m.subject.clone())
                    .unwrap_or_default()
            };
            let subjects = |uids: Vec<u32>| {
                let mut s: Vec<String> = uids.into_iter().map(subject).collect();
                s.sort();
                s
            };
            let f = &folder;
            let rossi = found(&mut sess, f, &["rossi"]).await;
            assert_eq!(subjects(rossi.clone()), ["note 1", "note 2"]);
            let both = found(&mut sess, f, &["rossi", "invoice"]).await;
            assert_eq!(subjects(both), ["note 1"]);
            let phrase = found(&mut sess, f, &["mario rossi"]).await;
            assert_eq!(subjects(phrase), ["note 1"]);
            let named = cache.indexed_message_ids(1, &folder, &rossi);
            assert_eq!(named.len(), 2);
            assert!(named.iter().all(|(_, id)| id.contains("hylki-509")));
            delete_box(&mut sess, &folder).await.expect("clean up");
        });
    }

    #[test]
    fn every_term_is_asked_for_in_the_text() {
        assert_eq!(
            text_query(&["mario rossi".into(), "4471".into()]),
            "TEXT \"mario rossi\" TEXT \"4471\""
        );
        assert_eq!(
            text_query(&["say \"hi\"".into()]),
            "TEXT \"say \\\"hi\\\"\""
        );
        assert_eq!(
            text_query(&["müller".into()]),
            "CHARSET UTF-8 TEXT \"müller\""
        );
    }
}
