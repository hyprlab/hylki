//! Building the rows: the rebuild (filter, sort, thread, date sections),
//! the window of rows shown, and patching one row in place. A hot path:
//! the algorithms here are tuned and kept as they are.

use super::layout::after_next_paint;
use super::sort::{correspondents, message_cmp, tag_sort_key, text_key_cmp, SortOrder};
use super::threading::{compute_thread_keys, latest_elsewhere, nested_members, thread_slot};
use super::types::{BulkAction, MessageListInput};
use super::views::{later_drop, Built};
use super::MessageList;
use crate::models::Message;
use crate::ui::message_row::{DaySection, RowData, RowMeta, RowShared};
use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;
use std::rc::Rc;

/// How many rows a large list puts on screen at once, and adds each time it
/// is scrolled near the end of them.
pub(super) const LIST_WINDOW: usize = 300;

/// How many rows a rebuild describes in full at most, past those the view
/// has bound: a list grown to the whole folder for its scrollbar leaves the
/// rest to be described as they come on screen.
const FULL_ROWS_AT_MOST: usize = 3000;

/// How many of a large list's newest messages go on screen while its
/// conversations are worked out (see `MessageList::threads_pending`).
const PREVIEW_ROWS: usize = 2000;

/// One conversation while the rows are built: its key and its messages.
type Group = ((u32, String), Vec<Rc<Message>>);

/// What a conversation's row says of its newest message: when it came, who
/// sent it, its preview and its time. The newer one in another folder
/// (`elsewhere`) when there is one, else this folder's newest, and nothing
/// for a row of one message.
type NewestLine = (
    Option<String>,
    Option<(String, String)>,
    Option<String>,
    Option<(i64, String)>,
);

fn newest_line(
    elsewhere: Option<&crate::models::ThreadLatest>,
    newest_here: &Message,
    count: usize,
) -> NewestLine {
    if let Some(l) = elsewhere {
        (
            Some(crate::models::datetime_list_at(l.timestamp, &l.date)),
            Some((l.from_name.clone(), l.from_addr.clone())),
            Some(l.preview.clone()),
            Some((l.timestamp, l.date.clone())),
        )
    } else if count > 1 {
        (
            Some(newest_here.datetime_list()),
            Some((newest_here.from_name.clone(), newest_here.from_addr.clone())),
            Some(newest_here.preview.clone()),
            Some((newest_here.timestamp, newest_here.date.clone())),
        )
    } else {
        (None, None, None, None)
    }
}

/// The conversations in the order of their first message in the sort; a
/// message `keys` has no thread for is a group of its own. The keys are
/// borrowed: a String per message was the larger part of grouping a long
/// list.
fn group_conversations(
    matches: Vec<Rc<Message>>,
    keys: &std::collections::HashMap<(u32, u32, u32), (u32, String)>,
) -> Vec<Group> {
    let mut grouped: Vec<Group> = Vec::with_capacity(matches.len());
    let mut at: std::collections::HashMap<&(u32, String), usize> =
        std::collections::HashMap::with_capacity(if keys.is_empty() { 0 } else { matches.len() });
    let mut alone: std::collections::HashMap<(u32, u32, u32), usize> =
        std::collections::HashMap::with_capacity(if keys.is_empty() { matches.len() } else { 0 });
    for m in matches {
        let slot = thread_slot(&m);
        let i = match keys.get(&slot) {
            Some(key) => *at.entry(key).or_insert_with(|| {
                grouped.push((key.clone(), Vec::new()));
                grouped.len() - 1
            }),
            None => *alone.entry(slot).or_insert_with(|| {
                grouped.push((
                    (m.account_id, format!("\u{0}uid{}/{}", m.folder_id, m.uid)),
                    Vec::new(),
                ));
                grouped.len() - 1
            }),
        };
        grouped[i].1.push(m);
    }
    grouped
}

impl MessageList {
    /// Change a message wherever the list holds it — the index, the search
    /// pool and its row — and refresh the row if it is on screen.
    pub(super) fn update_message(
        &mut self,
        is: impl Fn(&Message) -> bool,
        change: impl Fn(&mut Message),
    ) {
        let mut changed: Option<Rc<Message>> = None;
        for list in [&mut self.all, &mut self.search_pool] {
            if let Some(m) = list.iter_mut().find(|m| is(m)) {
                change(Rc::make_mut(m));
                changed.get_or_insert_with(|| m.clone());
            }
        }
        if let Some(idx) = self.shown.iter().position(|m| is(m)) {
            let m = match &changed {
                // The same message, as the index now has it.
                Some(m)
                    if (m.account_id, m.folder_id, m.id)
                        == (
                            self.shown[idx].account_id,
                            self.shown[idx].folder_id,
                            self.shown[idx].id,
                        ) =>
                {
                    m.clone()
                }
                _ => {
                    let mut m = Message::clone(&self.shown[idx]);
                    change(&mut m);
                    Rc::new(m)
                }
            };
            self.shown[idx] = m.clone();
            self.shared.model.update_row(idx, |d| d.msg = m);
        } else if let Some(row) = self.tail.iter_mut().find(|r| is(&r.msg)) {
            // Not on screen yet: what it says when it gets there.
            let mut data = RowData::clone(row);
            data.msg = match &changed {
                Some(m) => m.clone(),
                None => {
                    let mut m = Message::clone(&data.msg);
                    change(&mut m);
                    Rc::new(m)
                }
            };
            *row = Rc::new(data);
        }
    }

    /// Describe in full the head rows `rebuild_rows` left pending past the
    /// window, now that they come on screen.
    fn finish_rows(&mut self, rows: &mut [Rc<RowData>]) {
        if !rows.iter().any(|r| r.meta.pending.is_some()) {
            return;
        }
        let passes = self.filter();
        for row in rows.iter_mut() {
            let Some(key) = row.meta.pending.clone() else {
                continue;
            };
            let own = self
                .groups
                .get(&key)
                .cloned()
                .unwrap_or_else(|| vec![row.msg.clone()]);
            let (meta, _) = self.describe_group(&key, &own, &passes, true);
            let mut data = RowData::clone(row);
            data.meta = RowMeta {
                appear: data.meta.appear,
                ..meta
            };
            *row = Rc::new(data);
        }
    }

    /// Put up to `n` more rows on screen from the tail.
    pub(super) fn grow_window(&mut self, n: usize) {
        if self.tail.is_empty() {
            return;
        }
        let n = n.min(self.tail.len());
        let mut more: Vec<Rc<RowData>> = self.tail.drain(..n).collect();
        self.finish_rows(&mut more);
        self.shown.extend(more.iter().map(|r| r.msg.clone()));
        let at = self.shared.model.len();
        self.shared.model.splice(at, 0, more);
        self.window = self.shown.len();
    }

    /// Put the rest of the list on screen, as it is: describing tens of
    /// thousands of rows would hold the scrollbar up, and each is described
    /// when the view binds it.
    pub(super) fn show_whole_list(&mut self) {
        if self.tail.is_empty() {
            return;
        }
        let t = std::time::Instant::now();
        let more = std::mem::take(&mut self.tail);
        let n = more.len();
        self.shown.extend(more.iter().map(|r| r.msg.clone()));
        let at = self.shared.model.len();
        self.shared.model.splice(at, 0, more);
        self.window = self.shown.len();
        tracing::debug!(
            "list: {n} more rows on screen for the scrollbar in {:?}",
            t.elapsed()
        );
    }

    /// Describe in full the rows the view bound before they were.
    pub(super) fn finish_bound_rows(&mut self) {
        let wanted: std::collections::HashSet<_> =
            self.shared.take_unfinished().into_iter().collect();
        if wanted.is_empty() {
            return;
        }
        let found = self
            .shared
            .model
            .find(|r| r.meta.pending.is_some() && wanted.contains(&r.slot()));
        let t = std::time::Instant::now();
        let (at, mut rows): (Vec<usize>, Vec<Rc<RowData>>) = found.into_iter().unzip();
        self.finish_rows(&mut rows);
        let n = rows.len();
        for (pos, row) in at.into_iter().zip(rows) {
            self.shared.model.set_row(pos, row);
        }
        tracing::debug!(
            "list: {n} rows described as they came on screen in {:?}",
            t.elapsed()
        );
    }

    /// Who took part in a conversation (#334), for its Correspondents
    /// column: `msgs` and the replies filed in other folders.
    fn people_of(&self, key: &(u32, String), msgs: &[Rc<Message>]) -> String {
        let summary = self
            .threading
            .then(|| self.thread_summaries.get(key))
            .flatten();
        let mut all: Vec<&Message> = msgs.iter().map(|m| &**m).collect();
        all.extend(summary.iter().flat_map(|s| s.members.iter()));
        correspondents(&all)
    }

    /// One conversation's row: what its head says about it, and its members
    /// head first, the parts filed in other folders included (#309). `own`
    /// is the folder's own members, oldest first. Records the conversation's
    /// membership for the whole-conversation actions as it goes.
    /// `full`: work out what the row shows too, not only the conversation
    /// maps; a row described without it is filled in by `grow_window`.
    fn describe_group(
        &mut self,
        key: &(u32, String),
        own: &[Rc<Message>],
        passes: &impl Fn(&Message) -> bool,
        full: bool,
    ) -> (RowMeta, Vec<Rc<Message>>) {
        let mut msgs = own.to_vec();
        let summary = self
            .threading
            .then(|| self.thread_summaries.get(key))
            .flatten();
        // The parts of the conversation filed elsewhere, which the row opens
        // out into along with this folder's own (#309). What you sent comes
        // with them only when "Show your own replies in the message list" is
        // on: off, an Inbox row opened out into nothing but your answers
        // from Sent (#388). Gmail's All Mail copy of them goes too.
        let own = (!self.thread_row_newest).then_some(&self.own_addresses);
        let extras: Vec<Rc<Message>> = match summary {
            Some(s) if self.thread_expansion => {
                nested_members(&s.members, &msgs, &self.listed_folders, passes)
                    .into_iter()
                    .filter(|m| !own.is_some_and(|own| own.contains(&m.from_addr.to_lowercase())))
                    .map(Rc::new)
                    .collect()
            }
            _ => Vec::new(),
        };
        let count = msgs.len() + extras.len();
        // What the badge says. `count` is what the row holds and goes on
        // steering it (whether it nests and what expands) but the number on
        // the chip is the size of the *conversation*, drafts included (#222).
        // Never less than what is on screen: a stale or partial answer from
        // the cache must not make the badge contradict the rows under it.
        let total = if self.threading {
            summary.map(|s| s.count).unwrap_or(0).max(count)
        } else {
            count
        };
        let expanded = count > 1
            && self.thread_expansion
            && (self.expanded_threads.contains(key) != self.default_expanded);
        if !full {
            self.join_members(key, &mut msgs, extras, count);
            let meta = RowMeta {
                count: total,
                expandable: count > 1,
                expanded,
                key: (count > 1).then(|| key.clone()),
                revealed: true,
                group: self.threading.then(|| key.clone()),
                pending: Some(key.clone()),
                ..Default::default()
            };
            return (meta, msgs);
        }
        // The newest member this folder holds, and the newer one the cache
        // found in another folder — the reply you sent, filed in Sent (#236).
        let newest_here = msgs
            .last()
            .expect("a group holds at least one message")
            .clone();
        let elsewhere = self
            .thread_row_newest
            .then(|| latest_elsewhere(summary, newest_here.timestamp))
            .flatten();
        let people = self
            .columns
            .contains(&crate::config::ListColumn::Correspondents)
            .then(|| self.people_of(key, &msgs));
        // `expanded_threads` stores toggles away from the default state. With
        // expansion disabled no thread ever opens in the list; the stored
        // toggles survive for when it is re-enabled.
        // The head stays marked unread while ANY message in its conversation
        // is unread, hidden replies included, but only this folder's own: an
        // archived message still unread is not new mail here.
        let any_unread = count > 1 && msgs.iter().any(|m| m.unread);
        // The head is the thread's *oldest* message, but its row speaks for
        // the conversation's NEWEST one: its sender, its preview and the time
        // it landed, rather than the opener re-shown every time a reply
        // arrives.
        //
        // Which message that is can depend on more than this folder. A mail
        // you answered is one row in the Inbox and the answer is in Sent, so
        // the row quotes the other side however recently you wrote back. With
        // "Show your own replies in the message list" turned on, the cache's
        // newest wins whenever it is later than anything on screen (#236);
        // off, which is how Hylki has always behaved, the folder has the last
        // word.
        let (latest, from, preview, latest_at) =
            newest_line(elsewhere.as_ref(), &newest_here, count);
        let any_starred = count > 1 && msgs.iter().any(|m| m.starred);
        let (answered, forwarded) = (
            count > 1 && newest_here.answered,
            count > 1 && newest_here.forwarded,
        );
        self.join_members(key, &mut msgs, extras, count);
        let meta = RowMeta {
            count: total,
            // The parts filed in other folders open out too (#309); a
            // conversation with nothing to show beyond its row, all of it in
            // Trash, say, wears a bare count and no caret.
            expandable: count > 1,
            expanded,
            key: (count > 1).then(|| key.clone()),
            from,
            preview,
            latest,
            latest_at,
            unread: any_unread,
            starred: any_starred,
            answered,
            forwarded,
            people,
            revealed: true,
            group: self.threading.then(|| key.clone()),
            ..Default::default()
        };
        (meta, msgs)
    }

    /// Put a group's members in row order and remember them. The row stays
    /// this folder's oldest message, whatever the other folders hold that
    /// is older (`extras`), so what is done to the row is done to mail in
    /// this folder; the rest follow in time order.
    fn join_members(
        &mut self,
        key: &(u32, String),
        msgs: &mut Vec<Rc<Message>>,
        extras: Vec<Rc<Message>>,
        count: usize,
    ) {
        if !extras.is_empty() {
            for m in &extras {
                self.nested.insert((m.account_id, m.id), m.clone());
            }
            let head = msgs.remove(0);
            msgs.extend(extras);
            msgs.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.uid.cmp(&b.uid)));
            msgs.insert(0, head);
        }
        if count > 1 {
            let members: Vec<(u32, u32)> = msgs.iter().map(|m| (m.account_id, m.id)).collect();
            for k in &members {
                self.msg_thread.insert(*k, key.clone());
            }
            self.thread_members.insert(key.clone(), members);
        }
    }

    /// Ask again about the conversations on screen whose account's mail
    /// changed. A row only asks when it is first bound, and the rows on
    /// screen already are.
    pub(super) fn recheck_bound_rows(&mut self) {
        let accounts = std::mem::take(&mut self.recheck_accounts);
        if accounts.is_empty() || !self.threading {
            return;
        }
        for row in self.shared.bound_rows() {
            let group = row.data().and_then(|d| d.meta.group.clone());
            if let Some(group) = group.filter(|(aid, _)| accounts.contains(aid)) {
                self.shared.want(group);
            }
        }
    }

    /// Fresh sizes for these conversations (#222): their head rows say so in
    /// place. A conversation opened out in the list may gain or lose rows,
    /// and that takes a rebuild.
    pub(super) fn apply_summaries(&mut self, keys: Vec<(u32, String)>) {
        let passes = self.filter();
        for key in keys {
            let Some(own) = self.groups.get(&key).cloned() else {
                continue;
            };
            let head = (own[0].account_id, own[0].id);
            let Some(pos) = self.shown.iter().position(|m| (m.account_id, m.id) == head) else {
                continue;
            };
            let Some(row) = self.shared.model.row(pos) else {
                continue;
            };
            if row.meta.expanded {
                self.queue_rebuild(true);
                return;
            }
            let (meta, _) = self.describe_group(&key, &own, &passes, true);
            if meta.expanded {
                self.queue_rebuild(true);
                return;
            }
            self.shared.model.update_row(pos, |d| d.meta = meta);
        }
    }

    /// Run `rebuild_rows`, keeping a list read from the very top at the top.
    /// The view holds its place by a row, so a new message put in above the
    /// first one landed out of sight, just above the view (#400). Scrolled
    /// down, the view keeps the row it has, as before.
    pub(super) fn rebuild(&mut self) {
        let at_top = self
            .scroller
            .as_ref()
            .is_some_and(|s| s.vadjustment().value() < 1.0);
        self.rebuild_rows();
        if at_top {
            self.scroll_top();
        }
    }

    fn rebuild_rows(&mut self) {
        let t_rebuild = std::time::Instant::now();
        let Built {
            rows,
            by_date,
            any_expanded,
        } = self.compute_rows(false);
        // Only what changed reaches the view: rows that show the same message
        // stay where they are, with whatever they were doing (#323).
        let t_rows = std::time::Instant::now();
        let stale = self.look_stale.take();
        if !self.shared.model.replace(rows, stale) && stale {
            self.shared.refresh_all();
        }
        self.show_headings(by_date);
        self.select_current();
        self.fit_expanded(any_expanded);
        tracing::debug!(
            "list: rebuild {} rows (+{} to come) of {} — sort+group {:?}, model {:?}",
            self.shown.len(),
            self.tail.len(),
            self.total_matches,
            t_rows.duration_since(t_rebuild),
            t_rows.elapsed()
        );
    }

    /// Sort, group and describe `all` into the rows of the window, leaving
    /// the rest in `tail` and the conversation maps filled. `offscreen`: for
    /// a view not on show (`prebuild_view`), which must not touch the look of
    /// the one that is.
    pub(super) fn compute_rows(&mut self, offscreen: bool) -> Built {
        let passes = self.filter();
        let source_len = self.active_source().len();
        let mut matches = self.source_matches();
        if self.searching() {
            tracing::debug!(
                "list: search matched {} of {} messages ({:?})",
                matches.len(),
                source_len,
                self.scope
            );
        }
        // A large list's conversations are worked out on a background
        // thread (`threads_pending`). Until they are in, its newest messages
        // go on screen at once, grouped among themselves; the whole list
        // follows when the answer comes.
        let preview = self.threading && self.threads_pending();
        self.listed_folders = matches
            .iter()
            .map(|m| (m.account_id, m.folder_id))
            .collect();
        let graph_in_view = self
            .listed_folders
            .iter()
            .any(|(a, _)| self.graph_accounts.contains(a));
        if !offscreen && graph_in_view != self.graph_in_view {
            self.graph_in_view = graph_in_view;
            self.sync_look();
        }
        let sort = self.sort;
        self.shared.by_date.set(matches!(
            sort,
            SortOrder::DateNewest | SortOrder::DateOldest
        ));
        self.sort_matches(&mut matches);
        self.total_matches = matches.len();
        if preview {
            matches.truncate(PREVIEW_ROWS);
        }

        // Group into conversations by reply headers (Message-ID / In-Reply-To /
        // References) across the whole list, preserving the sort's order of
        // threads. With threading off, every message is its own group.
        // A search or a quick filter keeps the conversations of the list it
        // narrows: working them out again from the matches took a large
        // mailbox's search most of its time at every keystroke (#515).
        let kept_threads;
        let own_keys;
        let empty = std::collections::HashMap::new();
        let keys = if !self.threading {
            &empty
        } else if !preview {
            kept_threads = self.source_threads();
            &kept_threads.keys
        } else {
            own_keys = compute_thread_keys(&matches, &self.thread_links);
            &own_keys
        };
        let mut grouped = group_conversations(matches, keys);
        if sort == SortOrder::Correspondents {
            grouped = self.sort_by_correspondents(grouped);
        }

        // A reply that is sliding open, or shut, keeps doing so.
        let appearing: std::collections::HashSet<(u32, u32, u32)> = (0..self.shared.model.len())
            .filter_map(|i| self.shared.model.row(i))
            .filter(|r| r.meta.appear)
            .map(|r| r.slot())
            .collect();

        // Under which date heading each conversation goes (#435): by the
        // message that put it where it is in the sort, the newest for
        // Newest First and the oldest for Oldest First, so a heading's rows
        // always run together.
        let by_date =
            self.group_by_date && matches!(sort, SortOrder::DateNewest | SortOrder::DateOldest);
        let day_starts = by_date.then(|| crate::datefmt::day_starts(crate::datefmt::now()));

        let (mut shown, mut rows, any_expanded) =
            self.flatten_groups(grouped, &passes, day_starts.as_ref(), &appearing);

        self.follow_renumbered_selection(&shown);
        // On screen, the window: as many rows as were before (so a rebuild
        // does not pull the list back from where it was scrolled), at least
        // the first window, and far enough to hold the selection.
        let selected_at = shown
            .iter()
            .rposition(|m| {
                self.selected_ids.contains(&(m.account_id, m.id))
                    || self.selected_id == Some((m.account_id, m.id))
            })
            .map_or(0, |i| i + 1);
        let limit = self
            .window
            .max(LIST_WINDOW)
            .max(selected_at)
            .min(rows.len());
        later_drop(std::mem::replace(&mut self.tail, rows.split_off(limit)));
        shown.truncate(limit);
        self.window = limit.max(LIST_WINDOW);
        self.shown = shown;
        Built {
            rows,
            by_date,
            any_expanded,
        }
    }

    /// Flatten the conversations back into display order, recording each
    /// row's thread metadata: the rows, the messages they show, and whether
    /// any conversation is open.
    fn flatten_groups(
        &mut self,
        grouped: Vec<Group>,
        passes: &impl Fn(&Message) -> bool,
        day_starts: Option<&[i64; 4]>,
        appearing: &std::collections::HashSet<(u32, u32, u32)>,
    ) -> (Vec<Rc<Message>>, Vec<Rc<RowData>>, bool) {
        let mut shown: Vec<Rc<Message>> = Vec::with_capacity(grouped.len());
        let mut rows: Vec<Rc<RowData>> = Vec::with_capacity(grouped.len());
        let mut thread_drag = crate::ui::message_row::ThreadDragKeys::new();
        // The last rebuild's maps go after the frame: for a large folder,
        // freeing them is tens of milliseconds of the switch.
        later_drop((
            std::mem::take(&mut self.msg_thread),
            std::mem::take(&mut self.thread_members),
            std::mem::take(&mut self.nested),
            std::mem::take(&mut self.groups),
        ));
        if self.threading {
            self.groups.reserve(grouped.len());
        }
        let mut any_expanded = false;
        let full_rows = self.window.max(LIST_WINDOW).min(FULL_ROWS_AT_MOST);
        // Rows on screen are described in full wherever they are, so a
        // rebuild never shows them half done.
        let bound = self.shared.bound_slots();
        let mut unseen: std::collections::HashSet<(u32, u32)> = self
            .selected_ids
            .iter()
            .copied()
            .chain(self.selected_id)
            .collect();
        for (key, mut msgs) in grouped {
            let key = &key;
            let section = day_starts
                .as_ref()
                .map_or(DaySection::None, |d| DaySection::of(msgs[0].timestamp, d));
            // A conversation reads like a transcript: the message that started it
            // is the row on screen, and its replies descend beneath it to the
            // newest. Where the *thread* sits among the other rows is still the
            // list's sort order — recent activity keeps it near the top — but
            // inside the thread, time only runs one way.
            msgs.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.uid.cmp(&b.uid)));
            // A drag that starts on the row carries the folder's own members
            // (#171).
            if msgs.len() > 1 {
                let head = &msgs[0];
                thread_drag.insert(
                    (head.account_id, head.id),
                    msgs.iter()
                        .map(|m| (m.account_id, m.folder_id, m.uid, m.id))
                        .collect(),
                );
            }
            // Past the window, and the selection, a row is described in
            // full only when it comes on screen.
            let full = rows.len() < full_rows
                || !unseen.is_empty()
                || (!bound.is_empty()
                    && msgs
                        .iter()
                        .any(|m| bound.contains(&(m.account_id, m.folder_id, m.id))));
            let (meta, members) = self.describe_group(key, &msgs, &passes, full);
            if !unseen.is_empty() {
                for m in &members {
                    unseen.remove(&(m.account_id, m.id));
                }
            }
            if self.threading {
                self.groups.insert(key.clone(), msgs);
            }
            let expanded = meta.expanded;
            let mut it = members.into_iter();
            let head = it.next().expect("a group holds at least one message");
            shown.push(head.clone());
            rows.push(Rc::new(RowData {
                msg: head,
                meta,
                section,
            }));
            if expanded {
                any_expanded = true;
                let closing = self.collapsing_threads.contains_key(key);
                let rest: Vec<Rc<Message>> = it.collect();
                let n = rest.len();
                for (j, child) in rest.into_iter().enumerate() {
                    let slot = thread_slot(&child);
                    shown.push(child.clone());
                    rows.push(Rc::new(RowData {
                        msg: child,
                        meta: RowMeta {
                            is_child: true,
                            is_last: j + 1 == n,
                            revealed: !closing,
                            appear: appearing.contains(&slot),
                            ..Default::default()
                        },
                        section,
                    }));
                }
            }
        }
        *self.shared.thread_drag.borrow_mut() = thread_drag;
        (shown, rows, any_expanded)
    }

    /// Sort the matches by the list's order: by tag name (each message's
    /// tag names worked out once, not at every comparison) or by
    /// `message_cmp`, reversed when the sort is.
    fn sort_matches(&self, matches: &mut Vec<Rc<Message>>) {
        let sort = self.sort;
        let reversed = self.sort_reversed;
        let flip = |order: std::cmp::Ordering| if reversed { order.reverse() } else { order };
        if sort == SortOrder::Tags {
            let tags = self.shared.tags.borrow();
            let mut keyed: Vec<(String, Rc<Message>)> = matches
                .drain(..)
                .map(|m| (tag_sort_key(&m, &tags), m))
                .collect();
            keyed.sort_by(|(ka, a), (kb, b)| {
                flip(text_key_cmp(ka, kb).then(b.timestamp.cmp(&a.timestamp)))
            });
            *matches = keyed.into_iter().map(|(_, m)| m).collect();
        } else {
            matches.sort_by(|a, b| flip(message_cmp(a, b, sort)));
        }
    }

    /// Correspondents sort by the whole conversation's writers, as the
    /// column names them, which no single message knows (#443). The
    /// conversations are in date order beneath, newest first, from the
    /// sort before grouping.
    fn sort_by_correspondents(&self, grouped: Vec<Group>) -> Vec<Group> {
        let reversed = self.sort_reversed;
        let flip = |order: std::cmp::Ordering| if reversed { order.reverse() } else { order };
        let people: Vec<String> = grouped
            .iter()
            .map(|(k, msgs)| self.people_of(k, msgs).to_lowercase())
            .collect();
        let mut by: Vec<usize> = (0..grouped.len()).collect();
        by.sort_by(|a, b| flip(text_key_cmp(&people[*a], &people[*b])));
        let mut slots: Vec<Option<Group>> = grouped.into_iter().map(Some).collect();
        by.into_iter().filter_map(|i| slots[i].take()).collect()
    }

    /// A message that was moved away and brought back comes home under a
    /// new UID, and a row's id is its UID. The selection would find no row
    /// to light, so the highlight blinked off and came back a moment later
    /// as the list caught up. Follow the selection by Message-ID over a
    /// renumbering instead (#200): `shown` is the new list, `self.shown`
    /// still the old one.
    fn follow_renumbered_selection(&mut self, shown: &[Rc<Message>]) {
        let lost = self
            .selected_ids
            .iter()
            .any(|k| !shown.iter().any(|m| (m.account_id, m.id) == *k));
        if !lost {
            return;
        }
        let renumbered: Vec<((u32, u32), (u32, u32))> = self
            .selected_ids
            .iter()
            .filter(|k| !shown.iter().any(|m| (m.account_id, m.id) == **k))
            .filter_map(|k| {
                let was = self
                    .shown
                    .iter()
                    .find(|m| (m.account_id, m.id) == *k)
                    .filter(|m| !m.message_id.is_empty())?;
                let now = shown
                    .iter()
                    .find(|m| m.account_id == was.account_id && m.message_id == was.message_id)?;
                Some((*k, (now.account_id, now.id)))
            })
            .collect();
        for (was, now) in renumbered {
            for k in self.selected_ids.iter_mut().filter(|k| **k == was) {
                *k = now;
            }
            if self.selected_id == Some(was) {
                self.selected_id = Some(now);
            }
        }
    }

    /// Everything the view needs wired: selection, activation and keys.
    pub(super) fn wire_list(
        list: &gtk::ListView,
        shared: &Rc<RowShared>,
        input: &relm4::Sender<MessageListInput>,
    ) {
        // Multiple selection: plain click selects one (shown in the reader),
        // Ctrl/Shift extend the selection for bulk actions; double click (or
        // Enter) pops a message out into its own window.
        list.add_css_class("message-list");
        list.set_single_click_activate(false);
        list.set_show_separators(false);
        let s = input.clone();
        shared.selection.connect_selection_changed(move |_, _, _| {
            let _ = s.send(MessageListInput::SelectionChanged);
        });
        let s = input.clone();
        list.connect_activate(move |_, pos| {
            let _ = s.send(MessageListInput::RowActivated(pos as i32));
        });
        // Delete / Backspace on a focused row deletes the selection (single or
        // multi). Scoped to the list, so typing in the search box is unaffected.
        // Shift+Delete erases it instead, as in Thunderbird (#471).
        // Right and Left open out and fold conversations, as in a tree
        // (#540).
        let key = gtk::EventControllerKey::new();
        let s = input.clone();
        let shared = shared.clone();
        key.connect_key_pressed(move |_, keyval, _, state| {
            let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
            if shift && matches!(keyval, gtk::gdk::Key::Delete | gtk::gdk::Key::KP_Delete) {
                let _ = s.send(MessageListInput::Bulk(BulkAction::DeleteForever));
                gtk::glib::Propagation::Stop
            } else if matches!(
                keyval,
                gtk::gdk::Key::Delete | gtk::gdk::Key::KP_Delete | gtk::gdk::Key::BackSpace
            ) {
                // ResolveDelete, not a straight Bulk: a lone thread-head row
                // stands for its whole conversation (confirmed by the app).
                let _ = s.send(MessageListInput::ResolveDelete);
                gtk::glib::Propagation::Stop
            } else if let Some(fold) = fold_key(&shared, keyval, state) {
                let _ = s.send(fold);
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            }
        });
        list.add_controller(key);
        // Watched in the capture phase and never claimed, so the row still
        // gets the click.
        let click = gtk::GestureClick::new();
        click.set_button(0);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let s = input.clone();
        click.connect_pressed(move |gesture, _, _, _| {
            let extend = gesture.current_button() != gtk::gdk::BUTTON_PRIMARY
                || gesture.current_event_state().intersects(
                    gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK,
                );
            let _ = s.send(MessageListInput::PointerPressed { extend });
        });
        list.add_controller(click);
    }

    /// Ask for a rebuild at the end of the current main-loop pass, folding
    /// any further requests before then into it. A request that must not
    /// keep the scroll offset (a folder switch) wins over ones that would.
    pub(super) fn queue_rebuild(&mut self, preserve_scroll: bool) {
        let first = self.rebuild_queued.is_none();
        let preserve = self
            .rebuild_queued
            .map_or(preserve_scroll, |p| p && preserve_scroll);
        self.rebuild_queued = Some(preserve);
        if !first {
            return;
        }
        let input = self.input.clone();
        let run = move || {
            glib::idle_add_local_full(glib::Priority::HIGH, move || {
                let _ = input.send(MessageListInput::RunQueuedRebuild);
                glib::ControlFlow::Break
            });
        };
        // Another view: the frame that moves the sidebar's highlight is
        // drawn first, and the new rows a frame later, so the click shows at
        // once rather than after the list is filled and laid out (#401).
        if self.switched || !preserve_scroll {
            if let Some(clock) = self.list_view.frame_clock() {
                after_next_paint(&clock, run);
                self.list_view.queue_draw();
                return;
            }
        }
        // Otherwise ahead of GTK's layout and paint, so the old rows are
        // never laid out one more time for nothing before they go.
        run();
    }
}

/// What Right or Left does on the row the keyboard is on (#540): Right opens
/// a folded conversation out in the list, Left folds an open one, from its
/// head or any of its replies. `None` leaves the key to the window's own
/// shortcuts: the row stands alone, is already as asked, or conversations
/// do not open out in the list.
fn fold_key(
    shared: &RowShared,
    keyval: gtk::gdk::Key,
    state: gtk::gdk::ModifierType,
) -> Option<MessageListInput> {
    use gtk::gdk::{Key, ModifierType};
    if state
        .intersects(ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK | ModifierType::ALT_MASK)
    {
        return None;
    }
    let forward = match keyval {
        Key::Right | Key::KP_Right => true,
        Key::Left | Key::KP_Left => false,
        _ => return None,
    };
    if !shared.look.borrow().thread_expansion {
        return None;
    }
    let view = shared.view.upgrade()?;
    // Right to left the tree opens the other way, as GTK's own do.
    let open = forward != (view.direction() == gtk::TextDirection::Rtl);
    let pos = shared
        .bound_rows()
        .into_iter()
        .find(|r| r.has_focus_within())
        .and_then(|r| r.position())?;
    let mut head = pos;
    while head > 0 && shared.model.row(head).is_some_and(|r| r.meta.is_child) {
        head -= 1;
    }
    let row = shared.model.row(head)?;
    let key = row.meta.key.clone()?;
    if row.meta.expanded == open {
        return None;
    }
    Some(MessageListInput::FoldByKey {
        key,
        head: (head != pos).then_some(head),
    })
}
