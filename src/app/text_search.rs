//! Finding a search's words past the list's own text (#509): in the index
//! of the bodies on disk, on another thread, and on each account's server.
//! What is found goes back to the list by (account, folder id, uid) and by
//! Message-ID, which also finds the copy a search across folders shows.

use std::collections::{HashMap, HashSet};

use relm4::prelude::*;

use crate::models::FolderKind;
use crate::ui::message_list::{HitSet, MessageListInput, ServerScope};
use crate::worker::MailRequest;

use super::model::AppModel;
use super::msg::AppMsg;

/// The most messages the bodies on disk answer one term with: a word of
/// two letters can be in most of them.
const BODY_HITS: usize = 50_000;

/// A server search under way: its number, its words, and the accounts
/// still to answer.
#[derive(Debug, Default)]
pub(super) struct ServerSearch {
    gen: u64,
    terms: Vec<String>,
    waiting: HashSet<u32>,
}

/// At start (#509): the names a search finds recipients by, and the words
/// of the bodies saved before the index, a batch at a time on another
/// thread once the first syncs are under way.
pub(super) fn prepare_text_search() {
    if std::env::var_os("HYLKI_DEMO").is_some() {
        return;
    }
    crate::ui::message_list::load_address_names();
    gtk::glib::timeout_add_seconds_local_once(30, || {
        std::thread::spawn(|| {
            let Ok(cache) = crate::cache::Cache::open() else {
                return;
            };
            let t = std::time::Instant::now();
            let mut after = 0;
            loop {
                match cache.index_missing_bodies(after, 100) {
                    Ok(Some(last)) => after = last,
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!("search: could not index the bodies on disk: {e}");
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            tracing::debug!("search: bodies on disk indexed in {:?}", t.elapsed());
        });
    });
}

impl AppModel {
    /// Every folder's id, by account and path.
    fn folder_ids(&self) -> HashMap<(u32, String), u32> {
        self.folders
            .iter()
            .flat_map(|(aid, fs)| fs.iter().map(move |f| ((*aid, f.path.clone()), f.id)))
            .collect()
    }

    /// The folders each account is asked to search.
    fn server_paths(&self, scope: &ServerScope) -> Vec<(u32, Vec<String>)> {
        self.folders
            .iter()
            .map(|(aid, fs)| {
                let paths = fs
                    .iter()
                    .filter(|f| match scope {
                        ServerScope::Here(here) => here.contains(&(*aid, f.id)),
                        ServerScope::All { waste } => {
                            *waste || !matches!(f.kind, FolderKind::Trash | FolderKind::Junk)
                        }
                    })
                    .map(|f| f.path.clone())
                    .collect();
                (*aid, paths)
            })
            .filter(|(_, paths): &(u32, Vec<String>)| !paths.is_empty())
            .collect()
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_text_search(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::FindInBodies { terms } => {
                let ids = self.folder_ids();
                let s = sender.clone();
                std::thread::spawn(move || {
                    let Ok(cache) = crate::cache::Cache::open() else {
                        return;
                    };
                    let t = std::time::Instant::now();
                    let hits: Vec<HitSet> = terms
                        .iter()
                        .map(|term| {
                            let mut set = HitSet::default();
                            // One letter is in nearly every body.
                            if term.chars().count() < 2 {
                                return set;
                            }
                            for (aid, path, uid, id) in cache.find_body_text(term, BODY_HITS) {
                                let folder = ids.get(&(aid, path)).copied().unwrap_or(u32::MAX);
                                set.insert(aid, folder, uid, &id);
                            }
                            set
                        })
                        .collect();
                    tracing::debug!("search: bodies on disk searched in {:?}", t.elapsed());
                    s.input(AppMsg::BodiesFound { terms, hits });
                });
            }
            AppMsg::BodiesFound { terms, hits } => {
                self.message_list
                    .emit(MessageListInput::BodyHits { terms, hits });
            }
            AppMsg::FindOnServers { terms, scope } => {
                self.server_search.gen += 1;
                let gen = self.server_search.gen;
                crate::worker::begin_text_search(gen);
                let asked: Vec<(u32, Vec<String>)> = self
                    .server_paths(&scope)
                    .into_iter()
                    .filter(|(aid, _)| self.workers.contains_key(aid))
                    .collect();
                self.server_search.terms = terms.clone();
                self.server_search.waiting = asked.iter().map(|(aid, _)| *aid).collect();
                if asked.is_empty() {
                    self.message_list.emit(MessageListInput::ServerHits {
                        terms,
                        hits: HitSet::default(),
                        done: true,
                    });
                    return;
                }
                for (aid, paths) in asked {
                    self.send_to(
                        aid,
                        MailRequest::FindText {
                            gen,
                            terms: terms.clone(),
                            paths,
                        },
                    );
                }
            }
            AppMsg::TextFound {
                account_id,
                gen,
                hits,
                done,
            } => {
                if gen != self.server_search.gen {
                    return;
                }
                if done {
                    self.server_search.waiting.remove(&account_id);
                }
                let ids = self.folder_ids();
                let mut set = HitSet::default();
                for (path, uid, id) in hits {
                    let folder = ids.get(&(account_id, path)).copied().unwrap_or(u32::MAX);
                    set.insert(account_id, folder, uid, &id);
                }
                self.message_list.emit(MessageListInput::ServerHits {
                    terms: self.server_search.terms.clone(),
                    hits: set,
                    done: self.server_search.waiting.is_empty(),
                });
            }
            _ => unreachable!("not routed to on_text_search"),
        }
    }
}
