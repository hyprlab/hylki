//! Views and paging: a folder's messages arriving, kept and prebuilt views,
//! and the window of rows growing as the list scrolls.

use super::rows::LIST_WINDOW;
use super::sort::SortOrder;
use super::threading::{source_threads_of, thread_inputs, SourceThreads};
use super::types::{MessageListInput, MessageListOutput};
use super::views::KEEP_AT_MOST;
use super::MessageList;
use crate::models::Message;
use relm4::prelude::*;
use std::rc::Rc;

impl MessageList {
    pub(super) fn on_set_view(
        &mut self,
        key: Option<u64>,
        fp: u64,
        restore: bool,
        sender: &ComponentSender<Self>,
    ) {
        let kept = if restore {
            self.take_view(key, fp)
        } else {
            None
        };
        self.trim_views();
        self.view_key = key;
        self.view_fp = fp;
        match kept {
            Some(snap) => {
                self.pending_restore = Some(snap);
                self.queue_rebuild(false);
            }
            None if restore => {
                let _ = sender.output(MessageListOutput::ViewMissed);
            }
            None => {}
        }
    }

    pub(super) fn on_prebuild(
        &mut self,
        key: u64,
        fp: u64,
        sort: (SortOrder, bool),
        messages: Vec<Message>,
    ) {
        if self.view_key == Some(key)
            || self.has_view(key, fp, sort)
            || messages.len() > KEEP_AT_MOST
        {
            return;
        }
        let all: Vec<Rc<Message>> = messages.into_iter().map(Rc::new).collect();
        self.prebuild_gen += 1;
        let gen = self.prebuild_gen;
        if !self.threading {
            self.prebuild_view(key, fp, sort, all, None);
            return;
        }
        let input = thread_inputs(&all);
        let links = self.thread_links.clone();
        let sender = self.input.clone();
        self.prebuild = Some((gen, key, fp, sort, all));
        std::thread::spawn(move || {
            let threads = source_threads_of(&input, &links, false);
            let _ = sender.send(MessageListInput::PrebuildThreads {
                gen,
                threads: Box::new(threads),
            });
        });
    }

    pub(super) fn on_prebuild_threads(&mut self, gen: u64, threads: SourceThreads) {
        if let Some((g, key, fp, sort, all)) = self.prebuild.take() {
            if g == gen {
                self.prebuild_view(key, fp, sort, all, Some(Rc::new(threads)));
            } else {
                self.prebuild = Some((g, key, fp, sort, all));
            }
        }
    }

    pub(super) fn on_set_messages(&mut self, messages: Vec<Message>) {
        self.pending_restore = None;
        let t = std::time::Instant::now();
        let n = messages.len();
        self.all = messages.into_iter().map(Rc::new).collect();
        self.drop_threads();
        self.loaded = true;
        // Keep any active search query: this also fires for a background
        // re-sync of the folder you're viewing, which shouldn't drop your
        // search. Folder switches clear the query via `ResetPaging` first.
        self.queue_rebuild(true);
        tracing::debug!("list: set {n} messages in {:?}", t.elapsed());
    }

    pub(super) fn on_append_messages(&mut self, messages: Vec<Message>) {
        self.pending_restore = None;
        // Grow the searchable index in place. Dedup by (account, uid) since
        // UIDs collide across accounts in the unified inbox.
        let existing: std::collections::HashSet<(u32, u32)> =
            self.all.iter().map(|m| (m.account_id, m.uid)).collect();
        let before = self.all.len();
        for m in messages {
            if !existing.contains(&(m.account_id, m.uid)) {
                self.all.push(Rc::new(m));
            }
        }
        // The list holds the whole folder, so what arrives joins it.
        if self.all.len() != before {
            self.drop_threads();
            self.queue_rebuild(true);
        }
    }

    pub(super) fn on_set_loading(&mut self, sender: &ComponentSender<Self>) {
        self.pending_restore = None;
        self.all.clear();
        self.drop_threads();
        self.loaded = false;
        self.clear_search(sender);
        // Queued: when the folder's list follows in the same pass
        // (served from cache), the old rows are torn down once, for
        // the new ones, not first for nothing.
        self.queue_rebuild(false);
    }

    pub(super) fn on_reset_paging(&mut self, sender: &ComponentSender<Self>) {
        // The view being left is kept as built, to come back to.
        self.keep_view();
        self.view_key = None;
        // Folder switch: drop any active search, scrolled to the top.
        self.switched = true;
        self.window = LIST_WINDOW;
        self.late_select = None;
        if std::mem::take(&mut self.checking) {
            let _ = sender.output(MessageListOutput::Checked(None));
        }
        self.clear_search(sender);
        self.emitted_thread.clear();
        // However the new view's list arrives, a rebuild follows, and
        // the top is scrolled to then, not over the old rows first.
        self.queue_rebuild(false);
    }

    pub(super) fn on_run_queued_rebuild(&mut self, sender: &ComponentSender<Self>) {
        self.switched = false;
        if let Some(preserve) = self.rebuild_queued.take() {
            match self.pending_restore.take() {
                Some(snap) => self.restore_view(snap),
                None => self.rebuild(),
            }
            if !preserve {
                self.scroll_top();
            }
        }
        self.recheck_bound_rows();
        // The rows exist now: run the selection that waited for them.
        if let Some(key) = self.pending_select.take() {
            let _ = self.input.send(MessageListInput::SelectAndLoad(key));
        } else if let Some((key, at)) = self.late_select {
            if at.elapsed() > std::time::Duration::from_secs(30) {
                self.late_select = None;
            } else if self.shown.iter().any(|m| (m.account_id, m.id) == key)
                || self.thread_head_for(key).is_some()
            {
                let _ = self.input.send(MessageListInput::SelectAndLoad(key));
            }
        }
        self.report_thread_growth(sender);
    }
}
