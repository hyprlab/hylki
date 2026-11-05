//! Asking the mail servers. `search` looks only in what Hylki holds; a
//! server also knows the text of mail not downloaded, and mail the index has
//! not reached. Each server is asked for its words by the account's own
//! worker, which saves what it finds, so the ids handed out work like any
//! other. What is asked and what comes back keep to the folders the user
//! shares, and encrypted mail stays out of reach as it does everywhere: its
//! words are not there for a server to find, and reading it is still refused.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    failure, json, resolve_folder, shared_paths, summary, AppCall, AppMsg, Mail, Message,
    SummaryOut, Target, OFFSET_MAX, SEARCH_DEFAULT, SEARCH_MAX,
};
use crate::config::AssistantCap;
use crate::ui::message_list::Query;
use crate::worker::{ServerSearch, TEXT_SEARCH_LIMIT};

/// How long the servers are given: long enough for a big mailbox on a slow
/// one, and past it the answer says what is missing.
const ANSWER_WAIT: Duration = Duration::from_secs(60);
/// The longest query passed on: it goes over the network as it is.
const QUERY_MAX: usize = 1024;

#[derive(Deserialize, JsonSchema)]
pub(super) struct SearchServerArgs {
    /// What to look for. Each word must be found somewhere in the message, and "quotes" keep a
    /// phrase together; how whole words and parts of words match is up to the server. Also
    /// from:, to:, subject:, in:, has:attachment, is:unread, is:read, is:starred, but at least
    /// one word or from:, to: or subject: is needed.
    query: String,
    /// Only this account (its email address).
    account: Option<String>,
    /// Only this folder: a role or a path, as for list_messages.
    folder: Option<String>,
    /// How many results, newest first (default 20, at most 100).
    limit: Option<usize>,
    /// How many of the newest to skip, to go further back (default 0).
    offset: Option<usize>,
}

#[derive(Serialize)]
struct NotSearched {
    account: String,
    why: String,
}

#[derive(Serialize)]
struct ServerAnswer {
    messages: Vec<SummaryOut>,
    /// How many copies the servers found, kept or not: Gmail's labels and
    /// `limit` make it more than `messages`.
    found: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    not_searched: Vec<NotSearched>,
}

/// The words a server is asked for: its search covers the headers too, and
/// the operators on them are checked again here, as each has its own part.
fn words_to_ask(query: &Query) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for word in query
        .terms
        .iter()
        .chain(&query.from)
        .chain(&query.to)
        .chain(&query.subject)
    {
        if !words.contains(word) {
            words.push(word.clone());
        }
    }
    words
}

/// Whether anything but a plain word narrows the query, so that more has to
/// be looked at than are handed back.
fn narrows(query: &Query) -> bool {
    !(query.from.is_empty()
        && query.to.is_empty()
        && query.subject.is_empty()
        && query.folder.is_empty()
        && !query.attachment
        && query.unread.is_none()
        && !query.starred)
}

/// Whether a message found by its words passes what else the query says.
/// The words themselves are the server's word: they may be in the body.
fn passes_operators(query: &Query, path: &str, m: &Message) -> bool {
    let holds = |hay: &[&str], word: &str| hay.iter().any(|h| h.to_lowercase().contains(word));
    query
        .from
        .iter()
        .all(|w| holds(&[&m.from_name, &m.from_addr], w))
        && query.to.iter().all(|w| holds(&[&m.to, &m.cc, &m.bcc], w))
        && query.subject.iter().all(|w| holds(&[&m.subject], w))
        && query.folder.iter().all(|w| holds(&[path], w))
        && query.match_flags(m)
}

#[tool_router(router = server_search_tools, vis = "pub(super)")]
impl Mail {
    #[tool(
        description = "Search on the mail servers themselves, for text in mail Hylki has not \
        downloaded and mail older than it has reached. Slower than search, which should be \
        tried first: up to a minute. The search words go to the account's mail server. Returns \
        summaries, newest first, whose ids work like any other, and says which accounts could \
        not be searched.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn search_server(
        &self,
        Parameters(args): Parameters<SearchServerArgs>,
    ) -> CallToolResult {
        let text = args.query.trim();
        if text.len() > QUERY_MAX {
            return failure(format!("a query goes up to {QUERY_MAX} bytes"));
        }
        let query = Query::parse(text);
        let words = words_to_ask(&query);
        if words.is_empty() {
            return failure(
                "a server needs a word to look for, or from:, to: or subject:: the others only \
                 narrow what those find"
                    .into(),
            );
        }
        let accounts = match self.scope(args.account.as_deref(), AssistantCap::SearchServer) {
            Ok(accounts) => accounts,
            Err(e) => return failure(e),
        };
        let limit = args.limit.unwrap_or(SEARCH_DEFAULT).clamp(1, SEARCH_MAX);
        let offset = args.offset.unwrap_or(0).min(OFFSET_MAX);
        let keep = if narrows(&query) {
            TEXT_SEARCH_LIMIT
        } else {
            (limit + offset).min(TEXT_SEARCH_LIMIT)
        };

        // The folders each account may be asked about, whatever comes back
        // being held to them again below.
        let mut starred_only = false;
        let mut asked: HashMap<u32, (String, HashSet<String>)> = HashMap::new();
        {
            let cache = self.lock();
            for a in &accounts {
                let shared =
                    || -> HashSet<String> { shared_paths(&cache, a).into_iter().collect() };
                let paths = match args.folder.as_deref() {
                    None => shared(),
                    Some(folder) => match resolve_folder(&cache, a, folder) {
                        Some(Target::Path(path)) => HashSet::from([path]),
                        Some(Target::Starred) => {
                            starred_only = true;
                            shared()
                        }
                        None => continue,
                    },
                };
                if !paths.is_empty() {
                    asked.insert(a.id, (a.address.clone(), paths));
                }
            }
        }
        if asked.is_empty() {
            return failure(match args.folder.as_deref() {
                Some(folder) => format!("no folder \"{folder}\" is shared"),
                None => "no folder is shared".into(),
            });
        }

        let (answer, mut answers) = tokio::sync::mpsc::unbounded_channel();
        for (account_id, (_, paths)) in &asked {
            let ask = ServerSearch {
                terms: words.clone(),
                paths: paths.iter().cloned().collect(),
                keep,
                answer: answer.clone(),
            };
            let call = AppCall::AskServer {
                account_id: *account_id,
                ask,
            };
            if self.app.send(AppMsg::Assistant(call)).is_err() {
                return failure("Hylki is closing".into());
            }
        }
        drop(answer);

        let mut hits: Vec<(u32, String, u32)> = Vec::new();
        let mut found = 0;
        let mut answered: HashSet<u32> = HashSet::new();
        let mut trouble: Vec<(u32, String)> = Vec::new();
        let deadline = Instant::now() + ANSWER_WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Ok(Some(part)) = tokio::time::timeout(left, answers.recv()).await else {
                break;
            };
            found += part.found;
            let account_id = part.account_id;
            hits.extend(part.hits.into_iter().map(|(p, uid)| (account_id, p, uid)));
            if let Some(why) = part.trouble {
                trouble.push((account_id, why));
            }
            if part.done {
                answered.insert(account_id);
            }
        }

        let mut rows: Vec<(u32, String, Message)> = {
            let cache = self.lock();
            hits.into_iter()
                .filter(|(account_id, path, _)| {
                    asked
                        .get(account_id)
                        .is_some_and(|(_, paths)| paths.contains(path))
                })
                .filter_map(|(account_id, path, uid)| {
                    let m = cache.message_at(account_id, &path, uid)?;
                    Some((account_id, path, m))
                })
                .filter(|(_, path, m)| {
                    passes_operators(&query, path, m) && (!starred_only || m.starred)
                })
                .collect()
        };
        rows.sort_by_key(|(_, _, m)| std::cmp::Reverse(m.timestamp));
        let mut seen = HashSet::new();
        rows.retain(|(account_id, path, m)| {
            if m.message_id.is_empty() {
                seen.insert((*account_id, format!("{path}|{}", m.uid)))
            } else {
                seen.insert((*account_id, m.message_id.clone()))
            }
        });
        let messages: Vec<SummaryOut> = rows
            .iter()
            .skip(offset)
            .take(limit)
            .filter_map(|(account_id, path, m)| {
                let (address, _) = asked.get(account_id)?;
                Some(summary(address, path, m))
            })
            .collect();

        let mut not_searched: Vec<NotSearched> = trouble
            .into_iter()
            .filter_map(|(account_id, why)| {
                let (address, _) = asked.get(&account_id)?;
                Some(NotSearched {
                    account: address.clone(),
                    why,
                })
            })
            .collect();
        for (account_id, (address, _)) in &asked {
            if !answered.contains(account_id) {
                not_searched.push(NotSearched {
                    account: address.clone(),
                    why: "its server did not finish in time, so this may not be all: narrow the \
                          search with account or folder"
                        .into(),
                });
            }
        }
        // Said so that nothing is taken for an account having nothing.
        for a in self.shared_accounts(&self.policy()) {
            if !a.grant.allows(AssistantCap::SearchServer)
                && args
                    .account
                    .as_deref()
                    .is_none_or(|named| named.eq_ignore_ascii_case(&a.address))
            {
                not_searched.push(NotSearched {
                    account: a.address,
                    why: format!(
                        "the user has not allowed agents to {}",
                        AssistantCap::SearchServer.name()
                    ),
                });
            }
        }
        json(&ServerAnswer {
            messages,
            found,
            not_searched,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_is_asked_for_every_word_once() {
        let q = Query::parse("invoice from:ada subject:invoice is:unread");
        assert_eq!(words_to_ask(&q), ["invoice", "ada"]);
        assert!(words_to_ask(&Query::parse("is:unread has:attachment in:work")).is_empty());
    }

    #[test]
    fn what_a_server_found_still_has_to_pass_the_operators() {
        let m = Message {
            from_name: "Ada Lovelace".into(),
            from_addr: "ada@example.org".into(),
            to: "Bo <bo@example.org>".into(),
            subject: "Your Invoice".into(),
            unread: true,
            ..Message::blank()
        };
        let passes = |q: &str, path: &str| passes_operators(&Query::parse(q), path, &m);
        assert!(passes("anything", "INBOX"));
        assert!(passes(
            "from:lovelace to:bo subject:invoice is:unread",
            "INBOX"
        ));
        assert!(!passes("from:bo", "INBOX"));
        assert!(!passes("subject:lunch", "INBOX"));
        assert!(!passes("is:read", "INBOX"));
        assert!(!passes("is:starred", "INBOX"));
        assert!(!passes("has:attachment", "INBOX"));
        assert!(passes("in:work", "Work/Saved") && !passes("in:work", "INBOX"));
    }
}
