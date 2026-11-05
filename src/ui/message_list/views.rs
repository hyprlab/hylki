//! Kept views: the last few folders' rows held ready, so returning to one
//! swaps them in instead of rebuilding, and views prebuilt before they are
//! opened.

use super::rows::LIST_WINDOW;
use super::sort::SortOrder;
use super::threading::{SourceThreads, THREADS_OFF_MAIN};
use super::MessageList;
use crate::models::Message;
use crate::ui::message_row::RowData;
use gtk::glib;
use relm4::prelude::*;
use std::rc::Rc;

/// What `compute_rows` hands the view: the window's rows, whether they go
/// under date headings, and whether a conversation is opened out.
pub(super) struct Built {
    pub(super) rows: Vec<Rc<RowData>>,
    pub(super) by_date: bool,
    pub(super) any_expanded: bool,
}

/// A list as a rebuild left it (see `MessageList::keep_view`).
pub(super) struct ViewSnapshot {
    key: u64,
    fp: u64,
    sig: u64,
    all: Vec<Rc<Message>>,
    threads: Option<std::rc::Rc<SourceThreads>>,
    rows: Vec<Rc<RowData>>,
    shown: Vec<Rc<Message>>,
    tail: Vec<Rc<RowData>>,
    groups: std::collections::HashMap<(u32, String), Vec<Rc<Message>>>,
    msg_thread: std::collections::HashMap<(u32, u32), (u32, String)>,
    thread_members: std::collections::HashMap<(u32, String), Vec<(u32, u32)>>,
    nested: std::collections::HashMap<(u32, u32), Rc<Message>>,
    listed_folders: std::collections::HashSet<(u32, u32)>,
    total_matches: usize,
    thread_drag: crate::ui::message_row::ThreadDragKeys,
    window: usize,
}

/// How many views the list keeps as built: the unified rows built ahead
/// after a start, and the views visited since. Each holds its messages,
/// which for a large Archive is tens of megabytes.
pub(crate) const KEPT_VIEWS: usize = 8;

/// The most messages a view may hold and still be kept as built, or built
/// ahead. A kept view holds its own copy of every message and its
/// conversation maps besides: an 81,000-message unified Archive came to
/// some 150 MB for a click made once a session, if at all (#504). Larger
/// views are built at each visit, the newest first.
pub(crate) const KEEP_AT_MOST: usize = 20_000;

/// Free `held` once the main loop is idle, not inside the pass that let go
/// of it.
pub(super) fn later_drop<T: 'static>(held: T) {
    let mut held = Some(held);
    glib::idle_add_local_once(move || drop(held.take()));
}

impl MessageList {
    /// What the list holds in RAM, for the memory section of an export: the
    /// folder's index, the rows listed from it, and the whole-mailbox search
    /// pool (empty unless a search is open), each as (messages, bytes). The
    /// rows share their messages with the index.
    pub(crate) fn memory_stats(&self) -> [(usize, usize); 3] {
        use crate::desktop::memory_report::messages_bytes;
        [
            messages_bytes(self.all.iter().map(|m| &**m)),
            (
                self.shown.len(),
                self.shown.len() * std::mem::size_of::<RowData>(),
            ),
            messages_bytes(self.search_pool.iter().map(|m| &**m)),
        ]
    }

    /// The views kept as built: how many, their messages and bytes (records
    /// not shared with the view on show), and what their conversation maps
    /// add on top.
    pub(crate) fn kept_view_stats(&self) -> (usize, (usize, usize), usize) {
        use crate::desktop::memory_report::message_bytes;
        let mut seen = std::collections::HashSet::new();
        for m in &self.all {
            seen.insert(Rc::as_ptr(m));
        }
        let (mut n, mut bytes, mut maps) = (0, 0, 0);
        let key = |s: &String| 8 + std::mem::size_of::<String>() + s.len();
        for v in &self.views {
            for m in &v.all {
                if seen.insert(Rc::as_ptr(m)) {
                    n += 1;
                    bytes += message_bytes(m);
                }
            }
            if let Some(t) = &v.threads {
                maps += t.keys.values().map(|(_, s)| 12 + key(s)).sum::<usize>();
                maps += t
                    .members
                    .iter()
                    .map(|((_, s), m)| key(s) + 24 + m.len() * 8)
                    .sum::<usize>();
            }
            maps += v
                .groups
                .iter()
                .map(|((_, s), m)| key(s) + 24 + m.len() * 8)
                .sum::<usize>();
            maps += v
                .msg_thread
                .values()
                .map(|(_, s)| 8 + key(s))
                .sum::<usize>();
            maps += v
                .thread_members
                .iter()
                .map(|((_, s), m)| key(s) + 24 + m.len() * 8)
                .sum::<usize>();
            maps += (v.rows.len() + v.tail.len()) * std::mem::size_of::<RowData>();
        }
        (self.views.len(), (n, bytes), maps)
    }

    /// What a rebuild reads besides the messages, as one number: a kept view
    /// is shown again only under the settings it was built with.
    fn rows_signature(&self) -> u64 {
        self.signature_under((self.sort, self.sort_reversed))
    }

    fn signature_under(&self, (sort, reversed): (SortOrder, bool)) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let mut expanded: Vec<&(u32, String)> = self.expanded_threads.iter().collect();
        expanded.sort();
        let mut own: Vec<&String> = self.own_addresses.iter().collect();
        own.sort();
        let mut graph: Vec<&u32> = self.graph_accounts.iter().collect();
        graph.sort();
        format!(
            "{:?}{}{}{}{}{}{:?}{:?}{}{:?}{:?}{:?}{}",
            sort,
            reversed,
            self.threading,
            self.thread_expansion,
            self.default_expanded,
            self.group_by_date,
            self.columns,
            self.quick,
            self.thread_row_newest,
            expanded,
            own,
            graph,
            self.contact_addresses.as_ref().map_or(0, |c| c.len()),
        )
        .hash(&mut h);
        self.thread_links.hash(&mut h);
        crate::datefmt::day_key(crate::datefmt::now()).hash(&mut h);
        h.finish()
    }

    /// Keep the view on show as built, when it is one the app named and
    /// nothing but its own list is on screen: no search, no conversations
    /// still being worked out.
    pub(super) fn keep_view(&mut self) {
        let Some(key) = self.view_key else { return };
        if !self.loaded
            || !self.query.is_empty()
            || self.all.is_empty()
            || self.all.len() > KEEP_AT_MOST
        {
            return;
        }
        let threads = self.source_threads.borrow().clone();
        if self.threading
            && self.all.len() >= THREADS_OFF_MAIN
            && threads.as_ref().is_none_or(|t| t.pool)
        {
            return;
        }
        let snap = ViewSnapshot {
            key,
            fp: self.view_fp,
            sig: self.rows_signature(),
            // Copies, not taken: the rows stay on screen until the next view's
            // replace them, a frame on, and an empty `shown` meanwhile drew
            // "No Messages" over them.
            all: self.all.clone(),
            threads,
            rows: self.shared.model.rows(),
            shown: self.shown.clone(),
            tail: std::mem::take(&mut self.tail),
            groups: std::mem::take(&mut self.groups),
            msg_thread: std::mem::take(&mut self.msg_thread),
            thread_members: std::mem::take(&mut self.thread_members),
            nested: std::mem::take(&mut self.nested),
            listed_folders: std::mem::take(&mut self.listed_folders),
            total_matches: self.total_matches,
            thread_drag: std::mem::take(&mut *self.shared.thread_drag.borrow_mut()),
            window: self.window,
        };
        self.views.retain(|v| v.key != key);
        self.views.push_front(snap);
    }

    /// Keep no more than `KEPT_VIEWS`, the oldest going first. Not in
    /// `keep_view`: the view being left is kept before the one being opened
    /// is taken out, and must not push that one out.
    pub(super) fn trim_views(&mut self) {
        while self.views.len() > KEPT_VIEWS {
            later_drop(self.views.pop_back());
        }
    }

    /// Whether the view `key` is kept with this content, under the
    /// settings the list has now and the sort it opens with: the app shows
    /// it with `SetView { restore }` rather than sending its messages. The
    /// sort is passed as the view's own, as the list has not been told it
    /// when the app asks.
    pub(crate) fn has_view(&self, key: u64, fp: u64, sort: (SortOrder, bool)) -> bool {
        let sig = self.signature_under(sort);
        self.views
            .iter()
            .any(|v| v.key == key && v.fp == fp && v.sig == sig)
    }

    /// Why `has_view` said no, for the log.
    pub(crate) fn view_miss(&self, key: u64, fp: u64, sort: (SortOrder, bool)) -> &'static str {
        match self.views.iter().find(|v| v.key == key) {
            None => "never kept",
            Some(v) if v.fp != fp => "its mail changed",
            Some(v) if v.sig != self.signature_under(sort) => "the list's settings changed",
            Some(_) => "kept",
        }
    }

    /// The view the app named as on show, if any.
    pub(crate) fn view_key(&self) -> Option<u64> {
        self.view_key
    }

    pub(super) fn take_view(&mut self, key: Option<u64>, fp: u64) -> Option<ViewSnapshot> {
        let key = key?;
        let sig = self.rows_signature();
        let at = self
            .views
            .iter()
            .position(|v| v.key == key && v.fp == fp && v.sig == sig)?;
        self.views.remove(at)
    }

    /// Show a kept view as it was built: what `rebuild_rows` would make of
    /// the same messages under the same settings, without the work.
    pub(super) fn restore_view(&mut self, snap: ViewSnapshot) {
        let t = std::time::Instant::now();
        later_drop((
            std::mem::replace(&mut self.all, snap.all),
            std::mem::replace(&mut self.groups, snap.groups),
            std::mem::replace(&mut self.msg_thread, snap.msg_thread),
            std::mem::replace(&mut self.thread_members, snap.thread_members),
            std::mem::replace(&mut self.nested, snap.nested),
            std::mem::replace(&mut self.tail, snap.tail),
        ));
        self.drop_threads();
        *self.source_threads.borrow_mut() = snap.threads;
        self.loaded = true;
        self.listed_folders = snap.listed_folders;
        let graph_in_view = self
            .listed_folders
            .iter()
            .any(|(a, _)| self.graph_accounts.contains(a));
        if graph_in_view != self.graph_in_view {
            self.graph_in_view = graph_in_view;
            self.sync_look();
        }
        self.total_matches = snap.total_matches;
        *self.shared.thread_drag.borrow_mut() = snap.thread_drag;
        self.shown = snap.shown;
        self.window = snap.window;
        let stale = self.look_stale.take();
        if !self.shared.model.replace(snap.rows, stale) && stale {
            self.shared.refresh_all();
        }
        let by_date = self.group_by_date
            && matches!(self.sort, SortOrder::DateNewest | SortOrder::DateOldest);
        self.show_headings(by_date);
        self.select_current();
        let any_expanded = self
            .shared
            .model
            .rows()
            .iter()
            .chain(&self.tail)
            .any(|r| r.meta.expanded);
        self.fit_expanded(any_expanded);
        tracing::debug!(
            "list: kept view shown, {} rows of {}, in {:?}",
            self.shown.len(),
            self.total_matches,
            t.elapsed()
        );
    }

    /// Build the view `key` from `all` and keep it, leaving the view on show
    /// as it is: its state steps aside for the build and comes back.
    pub(super) fn prebuild_view(
        &mut self,
        key: u64,
        fp: u64,
        sort: (SortOrder, bool),
        all: Vec<Rc<Message>>,
        threads: Option<Rc<SourceThreads>>,
    ) {
        // A search on show would filter the build; the visit builds it then.
        if !self.query.is_empty() {
            return;
        }
        let t = std::time::Instant::now();
        let all = std::mem::replace(&mut self.all, all);
        let source_threads =
            std::mem::replace(&mut *self.source_threads.borrow_mut(), threads.clone());
        let shown = std::mem::take(&mut self.shown);
        let tail = std::mem::take(&mut self.tail);
        let groups = std::mem::take(&mut self.groups);
        let msg_thread = std::mem::take(&mut self.msg_thread);
        let thread_members = std::mem::take(&mut self.thread_members);
        let nested = std::mem::take(&mut self.nested);
        let listed_folders = std::mem::take(&mut self.listed_folders);
        let total_matches = self.total_matches;
        let thread_drag = std::mem::take(&mut *self.shared.thread_drag.borrow_mut());
        let window = std::mem::replace(&mut self.window, LIST_WINDOW);
        let selected_ids = std::mem::take(&mut self.selected_ids);
        let selected_id = self.selected_id.take();
        let sort_on_show = (self.sort, self.sort_reversed);
        (self.sort, self.sort_reversed) = sort;

        let built = self.compute_rows(true);
        let snap = ViewSnapshot {
            key,
            fp,
            sig: self.rows_signature(),
            all: std::mem::replace(&mut self.all, all),
            threads,
            rows: built.rows,
            shown: std::mem::replace(&mut self.shown, shown),
            tail: std::mem::replace(&mut self.tail, tail),
            groups: std::mem::replace(&mut self.groups, groups),
            msg_thread: std::mem::replace(&mut self.msg_thread, msg_thread),
            thread_members: std::mem::replace(&mut self.thread_members, thread_members),
            nested: std::mem::replace(&mut self.nested, nested),
            listed_folders: std::mem::replace(&mut self.listed_folders, listed_folders),
            total_matches: std::mem::replace(&mut self.total_matches, total_matches),
            thread_drag: std::mem::replace(&mut *self.shared.thread_drag.borrow_mut(), thread_drag),
            window: std::mem::replace(&mut self.window, window),
        };
        (self.sort, self.sort_reversed) = sort_on_show;
        *self.source_threads.borrow_mut() = source_threads;
        self.selected_ids = selected_ids;
        self.selected_id = selected_id;
        tracing::debug!(
            "list: built ahead, {} rows of {}, in {:?}",
            snap.shown.len(),
            snap.total_matches,
            t.elapsed()
        );
        self.views.retain(|v| v.key != key);
        self.views.push_back(snap);
    }
}
