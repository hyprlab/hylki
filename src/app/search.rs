//! The search pool: every indexed folder's mail merged into one list for a
//! search across folders and accounts, one copy of each mail.

use std::collections::HashMap;

use relm4::prelude::*;

use crate::config;
use crate::models::{FolderKind, Message};
use crate::ui::message_list::{MessageListInput, SearchScope};

use super::conversation::same_mail;
use super::model::AppModel;
use super::msg::AppMsg;
use super::types::FolderMessages;

/// Flatten every folder's indexed messages into one pool for cross-folder search.
///
/// Gmail files one message under every label it carries, so an archived mail
/// sits in All Mail and its labels, and an inbox one in INBOX and All Mail
/// too (#472). Each mail is kept once, from the folder `rank` puts first.
fn build_search_pool<'a>(
    lists: impl IntoIterator<Item = &'a Vec<Message>>,
    rank: impl Fn(&Message) -> u8,
) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::new();
    let mut copies: HashMap<(u32, &str), Vec<usize>> = HashMap::new();
    for m in lists.into_iter().flatten() {
        if m.message_id.is_empty() {
            out.push(m.clone());
            continue;
        }
        let slots = copies
            .entry((m.account_id, m.message_id.as_str()))
            .or_default();
        match slots.iter().copied().find(|&i| same_mail(&out[i], m)) {
            Some(i) => {
                if rank(m) < rank(&out[i]) {
                    out[i] = m.clone();
                }
            }
            None => {
                slots.push(out.len());
                out.push(m.clone());
            }
        }
    }
    out
}

/// Which copy of a mail filed in several folders a search shows, lowest
/// first: the inbox's, then a folder of its own, then the folders that
/// gather everything (Gmail's All Mail and Starred), and Trash and Junk last,
/// which "All folders" leaves out.
fn pool_rank(kind: Option<FolderKind>) -> u8 {
    match kind {
        Some(FolderKind::Inbox) => 0,
        Some(FolderKind::Archive | FolderKind::Starred) => 2,
        Some(FolderKind::Trash | FolderKind::Junk) => 3,
        _ => 1,
    }
}

/// Where a search starts in a view just opened, as Settings → Message List
/// has it (#699). `one_account`: the view shows a single account's mail,
/// which "This Account" needs.
pub(super) fn default_scope(start: config::SearchStart, one_account: bool) -> SearchScope {
    use config::SearchStart;
    match start {
        SearchStart::ThisFolder => SearchScope::ThisFolder,
        SearchStart::AllFolders => SearchScope::AllFolders,
        SearchStart::Account if one_account => SearchScope::Account,
        // Across accounts, the nearest to every folder Trash and Junk
        // included.
        SearchStart::Account | SearchStart::Everywhere => SearchScope::Everywhere,
    }
}

impl AppModel {
    /// The open folder or view's search starts where the setting now says
    /// (#699). The Outbox, a smart view and a tag view keep their own.
    pub(super) fn apply_search_start(&self) {
        if self.showing_outbox
            || (!self.unified && (self.tag_view.is_some() || self.smart_view.is_some()))
        {
            return;
        }
        let scope = if self.unified {
            default_scope(self.list_search_start, self.unified_account.is_some())
        } else if self.selected.is_some() {
            default_scope(self.list_search_start, true)
        } else {
            return;
        };
        self.message_list.emit(MessageListInput::SetScope(scope));
    }

    /// The cross-folder search pool: the folders opened this run as they
    /// are now, and `from_disk` for the rest, answered with
    /// [`AppMsg::SearchPoolBuilt`]. Copying a mailbox's messages took the
    /// main loop for a good part of a second as a search began (#515), so
    /// the folders go to another thread shared, not copied.
    fn build_search_pool(
        &self,
        gen: u64,
        from_disk: Vec<Vec<Message>>,
        sender: &ComponentSender<Self>,
    ) {
        let kinds: HashMap<(u32, u32), FolderKind> = self
            .folders
            .iter()
            .flat_map(|(aid, fs)| fs.iter().map(move |f| ((*aid, f.id), f.kind)))
            .collect();
        let opened: Vec<FolderMessages> = self.message_cache.values().cloned().collect();
        let s = sender.clone();
        std::thread::spawn(move || {
            let t = std::time::Instant::now();
            let pool =
                build_search_pool(opened.iter().map(|f| &**f).chain(from_disk.iter()), |m| {
                    pool_rank(kinds.get(&(m.account_id, m.folder_id)).copied())
                });
            // Let go of the folders before the answer is in, or the next
            // change to one of them copies it.
            drop(opened);
            let text = pool
                .iter()
                .map(crate::ui::message_list::search_text_of)
                .collect();
            tracing::debug!(
                "search: pool of {} messages put together in {:?}",
                pool.len(),
                t.elapsed()
            );
            s.input(AppMsg::SearchPoolBuilt { gen, pool, text });
        });
    }

    /// Read the folders not opened this run from the disk cache, where the
    /// background index keeps every folder whole, and hand them to the
    /// search with [`AppMsg::SearchPoolLoaded`] (#472). Gmail's All Mail is
    /// the one archived mail lives in, and it is rarely opened.
    fn load_search_pool(&self, sender: &ComponentSender<Self>) {
        let gen = self.search_gen;
        if self.cache.is_none() {
            self.build_search_pool(gen, Vec::new(), sender);
            return;
        }
        let unopened: Vec<(u32, u32, String)> = self
            .folders
            .iter()
            .flat_map(|(aid, fs)| fs.iter().map(move |f| (*aid, f)))
            .filter(|(aid, f)| !self.message_cache.contains_key(&(*aid, f.id)))
            .map(|(aid, f)| (aid, f.id, f.path.clone()))
            .collect();
        if unopened.is_empty() {
            self.build_search_pool(gen, Vec::new(), sender);
            return;
        }
        let s = sender.clone();
        std::thread::spawn(move || {
            let Ok(cache) = crate::cache::Cache::open() else {
                s.input(AppMsg::SearchPoolLoaded {
                    gen,
                    lists: Vec::new(),
                });
                return;
            };
            let t = std::time::Instant::now();
            let lists: Vec<((u32, u32), Vec<Message>)> = unopened
                .into_iter()
                .map(|(aid, fid, path)| ((aid, fid), cache.load_messages(aid, &path, fid)))
                .filter(|(_, ms)| !ms.is_empty())
                .collect();
            tracing::debug!(
                "search: {} messages in {} unopened folders read in {:?}",
                lists.iter().map(|(_, ms)| ms.len()).sum::<usize>(),
                lists.len(),
                t.elapsed()
            );
            s.input(AppMsg::SearchPoolLoaded { gen, lists });
        });
    }

    /// Every folder as a search across folders shows its results (#374):
    /// the name its pill says, and whether "All folders" leaves it out.
    pub(super) fn pool_folders(&self) -> HashMap<(u32, u32), crate::ui::message_list::PoolFolder> {
        self.folders
            .iter()
            .flat_map(|(aid, fs)| fs.iter().map(move |f| (*aid, f)))
            .map(|(aid, f)| {
                let folder = crate::ui::message_list::PoolFolder {
                    name: f.name.clone(),
                    waste: matches!(f.kind, FolderKind::Trash | FolderKind::Junk),
                };
                ((aid, f.id), folder)
            })
            .collect()
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_search(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::SearchPoolLoaded { gen, lists } => {
                if gen != self.search_gen {
                    return;
                }
                let lists = lists
                    .into_iter()
                    .map(|((account_id, folder_id), messages)| {
                        let messages = self.merge_local_tags(account_id, messages);
                        let messages = self.drop_pending_moves(account_id, folder_id, messages);
                        let messages = self.apply_pending_seen(account_id, folder_id, messages);
                        let messages = self.apply_pending_answers(account_id, folder_id, messages);
                        self.apply_pending_tags(account_id, folder_id, messages)
                    })
                    .collect();
                self.build_search_pool(gen, lists, &sender);
            }
            AppMsg::SearchPoolBuilt { gen, pool, text } => {
                if gen == self.search_gen {
                    self.message_list.emit(MessageListInput::SetSearchPool(
                        pool,
                        self.pool_folders(),
                        text,
                    ));
                }
            }
            AppMsg::SearchActive(active) => {
                self.search_gen += 1;
                if active {
                    // Every folder's indexed messages, so the search can span
                    // the whole mailbox: the folders opened this run and the
                    // rest from disk, put together off the main loop (#515).
                    self.load_search_pool(&sender);
                    // Results span accounts; tint rows by account (as in the unified
                    // inbox) so their origin is legible.
                    if self.accounts.len() > 1 {
                        self.message_list.emit(MessageListInput::SetColorize(true));
                    }
                } else {
                    self.message_list.emit(MessageListInput::SetSearchPool(
                        Vec::new(),
                        HashMap::new(),
                        Vec::new(),
                    ));
                    // Restore the tint state the underlying view wants.
                    self.message_list
                        .emit(MessageListInput::SetColorize(self.unified));
                }
            }
            _ => unreachable!("not routed to on_search"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::msg;

    #[test]
    fn search_pool_spans_every_folder_and_account() {
        let mut cache: HashMap<(u32, u32), Vec<Message>> = HashMap::new();
        cache.insert((1, 10), vec![msg(1, 10, 1), msg(1, 10, 2)]); // acct 1, inbox
        cache.insert((1, 11), vec![msg(1, 11, 3)]); // acct 1, archive
        cache.insert((2, 20), vec![msg(2, 20, 4), msg(2, 20, 5)]); // acct 2, inbox

        let pool = build_search_pool(cache.values(), |_| 0);
        assert_eq!(pool.len(), 5, "pool must include every folder's messages");

        let folders: std::collections::HashSet<(u32, u32)> =
            pool.iter().map(|m| (m.account_id, m.folder_id)).collect();
        assert_eq!(folders.len(), 3, "pool must span all three folders");
    }

    #[test]
    fn search_pool_is_empty_when_nothing_indexed() {
        assert!(
            build_search_pool(HashMap::<(u32, u32), Vec<Message>>::new().values(), |_| 0)
                .is_empty()
        );
    }

    /// Gmail's label copies of one mail are one result, from the folder that
    /// says most about it (#472).
    #[test]
    fn search_pool_keeps_one_copy_of_a_labelled_mail() {
        let copy = |folder_id: u32, uid: u32, mid: &str| {
            let mut m = msg(1, folder_id, uid);
            m.message_id = mid.into();
            m.from_addr = "ada@x".into();
            m.timestamp = 500;
            m
        };
        // INBOX = 1, All Mail = 2, a label = 3.
        let inbox = vec![copy(1, 10, "in@x")];
        let all_mail = vec![
            copy(2, 20, "in@x"),
            copy(2, 21, "archived@x"),
            copy(2, 22, "only@x"),
        ];
        let label = vec![copy(3, 30, "archived@x")];
        let rank = |m: &Message| match m.folder_id {
            1 => pool_rank(Some(FolderKind::Inbox)),
            2 => pool_rank(Some(FolderKind::Archive)),
            _ => pool_rank(Some(FolderKind::Custom)),
        };
        let mut pool = build_search_pool([&all_mail, &label, &inbox], rank);
        pool.sort_by_key(|m| m.uid);
        let kept: Vec<(u32, u32)> = pool.iter().map(|m| (m.folder_id, m.uid)).collect();
        assert_eq!(kept, vec![(1, 10), (2, 22), (3, 30)]);

        // The same Message-ID from someone else is another mail.
        let mut other = copy(3, 31, "only@x");
        other.from_addr = "spam@x".into();
        assert_eq!(build_search_pool([&all_mail, &vec![other]], rank).len(), 4);
    }

    #[test]
    fn a_search_starts_where_the_setting_says() {
        use config::SearchStart::*;
        assert_eq!(config::SearchStart::default(), Everywhere);
        for one in [true, false] {
            assert_eq!(default_scope(ThisFolder, one), SearchScope::ThisFolder);
            assert_eq!(default_scope(AllFolders, one), SearchScope::AllFolders);
            assert_eq!(default_scope(Everywhere, one), SearchScope::Everywhere);
        }
        assert_eq!(default_scope(Account, true), SearchScope::Account);
        // "This Account" has no meaning across accounts.
        assert_eq!(default_scope(Account, false), SearchScope::Everywhere);
    }
}
