//! The search bar and the quick filters.

use super::filters::{QuickFilter, QuickFilters};
use super::query::Query;
use super::search::{
    HitSet, PoolFolder, SearchHits, SearchScope, SearchText, ServerScope, TextHits,
};
use super::types::{MessageListInput, MessageListOutput};
use super::MessageList;
use crate::models::Message;
use adw::prelude::*;
use relm4::prelude::*;
use std::rc::Rc;

/// How long typing pauses before the servers are asked (#509): each ask
/// is a search of every folder on an IMAP server.
const SERVER_PAUSE: std::time::Duration = std::time::Duration::from_millis(700);

impl MessageList {
    pub(super) fn on_search(&mut self, q: String, sender: &ComponentSender<Self>) {
        let was_active = self.wants_pool();
        self.query = q;
        self.parsed = Query::parse(&self.query);
        let now_active = self.wants_pool();
        // On the empty↔non-empty edge, tell the app to supply or drop the
        // cross-folder pool.
        if was_active != now_active {
            let _ = sender.output(MessageListOutput::SearchActive(now_active));
        }
        if self.query.is_empty() {
            *self.search_text.borrow_mut() = Default::default();
            self.search_hits = None;
        }
        self.find_text(sender);
        // A large list is searched on another thread, and its rows
        // stay as they are until the answer is in: typing held the
        // main loop for as long as each search took (#515).
        if !self.start_search() {
            self.rebuild();
            self.scroll_top();
        }
    }

    /// Look for the query's words past the list's own text (#509): in the
    /// bodies on disk at once, and on the servers once typing pauses. The
    /// answers for words no longer asked are dropped.
    fn find_text(&mut self, sender: &ComponentSender<Self>) {
        if self
            .text_hits
            .as_ref()
            .is_some_and(|h| h.terms == self.parsed.terms)
        {
            return;
        }
        self.stop_server_search();
        let before = self.text_hits.take();
        self.text_hits = (!self.parsed.terms.is_empty())
            .then(|| Rc::new(TextHits::carried(before.as_deref(), &self.parsed.terms)));
        if self.parsed.terms.is_empty() {
            return;
        }
        let _ = sender.output(MessageListOutput::FindInBodies {
            terms: self.parsed.terms.clone(),
        });
        let round = self.server_round;
        let input = self.input.clone();
        gtk::glib::timeout_add_local_once(SERVER_PAUSE, move || {
            input.emit(MessageListInput::SearchServers { round });
        });
    }

    /// Forget a server search asked for or under way: its answers no
    /// longer count, and its timer does nothing when it fires.
    pub(super) fn stop_server_search(&mut self) {
        self.server_round += 1;
        self.server_searching = false;
    }

    /// Typing paused: ask the servers, if this is still the search.
    pub(super) fn on_search_servers(&mut self, round: u64, sender: &ComponentSender<Self>) {
        if round != self.server_round || self.parsed.terms.is_empty() {
            return;
        }
        let scope = match self.scope {
            SearchScope::ThisFolder if self.parsed.folder.is_empty() => {
                let mut here: Vec<(u32, u32)> = self
                    .all
                    .iter()
                    .map(|m| (m.account_id, m.folder_id))
                    .collect();
                here.sort_unstable();
                here.dedup();
                ServerScope::Here(here)
            }
            SearchScope::Everywhere => ServerScope::All { waste: true },
            _ => ServerScope::All {
                waste: !self.parsed.folder.is_empty()
                    || self.all.iter().any(|m| {
                        self.pool_folders
                            .get(&(m.account_id, m.folder_id))
                            .is_some_and(|f| f.waste)
                    }),
            },
        };
        self.server_searching = true;
        let _ = sender.output(MessageListOutput::FindOnServers {
            terms: self.parsed.terms.clone(),
            scope,
        });
    }

    /// What the index of the bodies on disk found, a set for each term.
    pub(super) fn on_body_hits(&mut self, terms: Vec<String>, hits: Vec<HitSet>) {
        let Some(found) = self.text_hits.as_mut().filter(|h| h.terms == terms) else {
            return;
        };
        tracing::debug!(
            "search: bodies on disk hold {:?} of {:?}",
            hits.iter().map(HitSet::len).collect::<Vec<_>>(),
            terms
        );
        // Mostly what stood in already, or nothing again: a rebuild of a
        // whole mailbox's results for no change is felt as typing lags.
        let none = HitSet::default();
        let same = (0..found.body.len().max(hits.len()))
            .all(|i| found.body.get(i).map_or(&none, |a| &**a) == hits.get(i).unwrap_or(&none));
        Rc::make_mut(found).body = hits.into_iter().map(Rc::new).collect();
        if !same {
            self.rebuild();
        }
    }

    /// What a server found for every term; `done` once every account has
    /// answered.
    pub(super) fn on_server_hits(&mut self, terms: Vec<String>, hits: HitSet, done: bool) {
        let Some(found) = self.text_hits.as_mut().filter(|h| h.terms == terms) else {
            return;
        };
        if done {
            self.server_searching = false;
        }
        let found = Rc::make_mut(found);
        if !found.server_own {
            // The first answer for these words: what stood in goes.
            let carried = !found.server.is_empty();
            found.server = hits;
            found.server_own = true;
            if carried || !found.server.is_empty() {
                self.rebuild();
            }
            return;
        }
        if hits.is_empty() {
            return;
        }
        found.server.extend(hits);
        self.rebuild();
    }

    pub(super) fn on_search_matched(
        &mut self,
        gen: u64,
        query: String,
        text: std::sync::Arc<Vec<Box<str>>>,
        hits: Vec<u64>,
    ) {
        if gen == self.search_gen.load(std::sync::atomic::Ordering::Relaxed) && query == self.query
        {
            self.search_hits = Some(SearchHits { query, text, hits });
            self.rebuild();
            self.scroll_top();
        }
    }

    pub(super) fn on_set_scope(&mut self, scope: SearchScope, sender: &ComponentSender<Self>) {
        if let Some(picker) = &self.scope_picker {
            picker.sync(scope);
        }
        if self.scope != scope {
            let was_active = self.wants_pool();
            self.scope = scope;
            // "This folder" has no use for the pool, which can run to
            // every message of every account (#472).
            if was_active != self.wants_pool() {
                let _ = sender.output(MessageListOutput::SearchActive(!was_active));
            }
            // Scope only affects the view while a query is present.
            if self.searching() {
                self.sync_look();
                self.rebuild();
                self.scroll_top();
                // The servers are asked again, where the new scope looks;
                // what they found already still stands.
                if !self.parsed.terms.is_empty() {
                    self.server_round += 1;
                    self.on_search_servers(self.server_round, sender);
                }
            }
        }
    }

    pub(super) fn on_set_search_pool(
        &mut self,
        pool: Vec<Message>,
        folders: std::collections::HashMap<(u32, u32), PoolFolder>,
        text: Vec<Box<str>>,
    ) {
        self.search_pool = pool.into_iter().map(Rc::new).collect();
        self.search_text.borrow_mut()[1] =
            (text.len() == self.search_pool.len() && !text.is_empty()).then(|| SearchText {
                of: self.search_pool.iter().map(Rc::as_ptr).collect(),
                text: std::sync::Arc::new(text),
            });
        self.pool_folders = Rc::new(folders);
        self.drop_threads();
        self.sync_look();
        if self.searching() && self.scope != SearchScope::ThisFolder {
            self.rebuild();
        }
    }

    pub(super) fn on_set_quick_filters(
        &mut self,
        filters: QuickFilters,
        contacts: Option<std::sync::Arc<std::collections::HashSet<String>>>,
    ) {
        let contacts_changed = filters.has(QuickFilter::Contacts)
            && match (&self.contact_addresses, &contacts) {
                (Some(a), Some(b)) => !std::sync::Arc::ptr_eq(a, b),
                (a, b) => a.is_some() != b.is_some(),
            };
        self.contact_addresses = contacts;
        if self.quick != filters || contacts_changed {
            self.quick = filters;
            self.rebuild();
        }
    }

    pub(super) fn on_showcase_search(
        &mut self,
        text: String,
        scope: u32,
        sender: &ComponentSender<Self>,
    ) {
        self.search_open = true;
        if let Some(scope) = SearchScope::ALL.get(scope as usize) {
            sender.input(MessageListInput::SetScope(*scope));
        }
        if let Some(entry) = &self.search_entry {
            entry.set_text(&text);
        }
        // HYLKI_SHOWCASE_SCOPE_MENU=<png> opens the scope popover and
        // captures it.
        if let (Ok(path), Some(picker)) = (
            std::env::var("HYLKI_SHOWCASE_SCOPE_MENU"),
            &self.scope_picker,
        ) {
            picker.button.popup();
            if let Some(popover) = picker.button.popover() {
                gtk::glib::timeout_add_seconds_local_once(1, move || {
                    crate::ui::showcase::showcase_capture(popover.upcast_ref(), &path);
                });
            }
            // HYLKI_SHOWCASE_SCOPE_INFO=<png> opens the first scope's
            // info popover on top and captures that.
            if let (Ok(path), Some(info)) = (
                std::env::var("HYLKI_SHOWCASE_SCOPE_INFO"),
                picker.infos.first(),
            ) {
                let info = info.clone();
                gtk::glib::timeout_add_seconds_local_once(2, move || {
                    info.popup();
                    if let Some(popover) = info.popover() {
                        gtk::glib::timeout_add_seconds_local_once(1, move || {
                            crate::ui::showcase::showcase_capture(popover.upcast_ref(), &path);
                        });
                    }
                });
            }
        }
    }

    pub(super) fn on_focus_search(&mut self, sender: &ComponentSender<Self>) {
        // A field that is always there has nothing to open or close.
        if self.search_pinned {
            if let Some(entry) = &self.search_entry {
                entry.grab_focus();
            }
            return;
        }
        // Toggle (#102): the header button closes an open search too.
        if self.search_open {
            sender.input(MessageListInput::CloseSearch);
            return;
        }
        // Clicking the button blurs an empty entry, whose focus-leave
        // just closed the bar — that same click must not reopen it.
        if self
            .search_closed_at
            .take()
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(300))
        {
            return;
        }
        self.search_open = true;
        if let Some(entry) = &self.search_entry {
            let entry = entry.clone();
            // After the revealer maps it; focusing an unmapped entry
            // is a no-op.
            gtk::glib::idle_add_local_once(move || {
                entry.grab_focus();
            });
        }
    }

    pub(super) fn on_close_search(&mut self, sender: &ComponentSender<Self>) {
        if self.search_pinned {
            // Esc in the field hands the keyboard back to the list;
            // an empty field losing focus has gone somewhere already.
            let had_focus = self
                .search_entry
                .as_ref()
                .is_some_and(|e| e.has_focus() || e.focus_child().is_some());
            if !self.query.is_empty() {
                self.clear_search(sender);
                self.rebuild();
                self.scroll_top();
            }
            if had_focus {
                self.preserving_scroll(|this| {
                    this.list_view.grab_focus();
                });
            }
            return;
        }
        self.search_open = false;
        self.search_closed_at = Some(std::time::Instant::now());
        self.clear_search(sender);
        self.rebuild();
    }
}
