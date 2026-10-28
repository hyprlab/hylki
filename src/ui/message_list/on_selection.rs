//! The selection, the keyboard cursor and focus, and opening a row in the
//! reader.

use super::filters::CheckFilter;
use super::threading::{reader_conversation, row_for_reader_key};
use super::types::{MessageListInput, MessageListOutput};
use super::MessageList;
use crate::models::Message;
use adw::prelude::*;
use relm4::prelude::*;

impl MessageList {
    pub(super) fn on_select_from_reader(
        &mut self,
        keys: Vec<(u32, u32)>,
        conversation: Vec<(u32, u32)>,
        sender: &ComponentSender<Self>,
    ) {
        // An empty set means the reader cleared its card selection (a
        // click on the document's empty space). The list keeps the
        // viewed message highlighted rather than losing its anchor —
        // the focus-within CSS dims the highlight instead.
        let keys = if keys.is_empty() {
            self.selected_id.into_iter().collect()
        } else {
            keys
        };
        // The list shows only a thread's head while it is collapsed, so
        // selecting a reply has to open the thread first — otherwise
        // there is no row to select and only the head would ever answer.
        let hidden: Vec<(u32, String)> = keys
            .iter()
            .filter(|k| !self.shown.iter().any(|m| (m.account_id, m.id) == **k))
            .filter_map(|k| self.msg_thread.get(k).cloned())
            .collect();
        if !hidden.is_empty() && self.thread_expansion {
            for thread_key in hidden {
                // `expanded_threads` records the departure from the
                // default, so which way to move it depends on that.
                if self.default_expanded {
                    self.expanded_threads.remove(&thread_key);
                } else {
                    self.expanded_threads.insert(thread_key);
                }
            }
            self.rebuild();
        }
        // The conversation as the reader shows it: what this list
        // handed over plus what the app merged in from other folders
        // since (#220). Either way it was opened from the viewed row.
        let conversation = reader_conversation(&self.emitted_thread, &conversation);
        let mut positions: Vec<usize> = Vec::new();
        for key in &keys {
            let row = row_for_reader_key(
                key,
                &self.shown,
                &self.msg_thread,
                &conversation,
                self.selected_id,
            );
            // Several cards of a collapsed conversation are all its one row:
            // counted twice, the row's key never matched the selection GTK
            // reports, which was then taken for the user's and sent back to
            // the reader as one card, undoing a Ctrl or Shift click there.
            if let Some(row) = row.filter(|r| !positions.contains(r)) {
                positions.push(row);
            }
        }
        // What the reader asked for, as this list can represent it, so
        // the change GTK is about to report is recognised as ours
        // rather than the user's.
        self.reader_keys = self.keys_at(&positions);
        self.from_reader = 8;
        self.select_positions(&positions);
        sender.input(MessageListInput::SelectionChanged);
    }

    pub(super) fn on_selection_changed(&mut self, sender: &ComponentSender<Self>) {
        let positions = self.selected_positions();
        let keys = self.keys_at(&positions);
        self.selection_count = keys.len();
        // Set from the reader, which is already showing these messages:
        // mirror the selection but leave the reader alone. Reporting it
        // back would also drop whatever the reader has selected that this
        // list has no row for.
        if self.from_reader > 0 {
            self.from_reader -= 1;
            if keys == self.reader_keys {
                self.selected_ids = keys;
                return;
            }
            // Something else moved the selection — stop expecting ours.
            self.from_reader = 0;
        }
        // A selection the user made here; the reader outlines it, and
        // it wins over one still waiting for its row.
        if !keys.is_empty() {
            self.late_select = None;
        }
        let _ = sender.output(MessageListOutput::SelectionKeys(self.slots_at(&positions)));
        if self.checking {
            // Checked rows are not opened, one or many (#392).
            self.selected_id = None;
            let _ = sender.output(MessageListOutput::Checked(Some(keys.len())));
            self.selected_ids = keys;
            return;
        }
        match keys.as_slice() {
            [] => self.selected_id = None,
            // Exactly one selected → show it in the reader. Skip when
            // it's already the viewed row (e.g. programmatic restore
            // after a rebuild) so it isn't needlessly reloaded.
            [key] if self.selected_id != Some(*key) => {
                // Which way did the user move? Only a change with a
                // known previous row says anything — the post-delete
                // advance clears selected_id first, so it can never
                // flip the direction it is itself steering by.
                let pos =
                    |k: &(u32, u32)| self.shown.iter().position(|m| (m.account_id, m.id) == *k);
                if let (Some(old), Some(new)) = (self.selected_id.as_ref().and_then(pos), pos(key))
                {
                    if new != old {
                        self.nav_direction = if new > old { 1 } else { -1 };
                    }
                }
                self.selected_id = Some(*key);
                if let Some(m) = self
                    .shown
                    .iter()
                    .find(|m| (m.account_id, m.id) == *key)
                    .map(|m| Message::clone(m))
                {
                    let (thread, solo) = self.conversation_for(&m);
                    self.emitted_thread = thread.iter().map(|t| (t.account_id, t.id)).collect();
                    let ahead = self.rows_ahead(*key);
                    let _ = sender.output(MessageListOutput::Selected {
                        message: m,
                        thread,
                        solo,
                        ahead,
                    });
                }
            }
            // Multiple selected → keep the reader on the primary message;
            // the one row already shown stays as it is.
            _ => {}
        }
        self.selected_ids = keys;
    }

    pub(super) fn on_move_selection(&mut self, delta: i32, sender: &ComponentSender<Self>) {
        if self.shown.is_empty() {
            return;
        }
        if delta != 0 {
            self.nav_direction = delta.signum();
        }
        // From the current row, or from the top/bottom when nothing is
        // selected yet, so the first keypress always lands somewhere.
        let current = self
            .selected_positions()
            .first()
            .map(|&p| p as i64)
            .unwrap_or(if delta > 0 {
                -1
            } else {
                self.shown.len() as i64
            });
        let next = (current + delta as i64).clamp(0, self.shown.len() as i64 - 1) as usize;
        self.shared.set_cursor(None);
        self.shared.selection.select_item(next as u32, true);
        self.stop_checking(sender);
        self.focus_row(next);
    }

    pub(super) fn on_set_gmail_keys(&mut self, on: bool, sender: &ComponentSender<Self>) {
        self.gmail_keys = on;
        if !on {
            self.stop_checking(sender);
        }
    }

    pub(super) fn on_move_cursor(&mut self, delta: i32) {
        if self.shown.is_empty() {
            return;
        }
        if delta != 0 {
            self.nav_direction = delta.signum();
        }
        // From the row the keyboard is on, else the open one, else
        // the top or the bottom.
        let from = self
            .cursor_position()
            .or_else(|| self.selected_positions().first().copied());
        let next = self.conversation_step(from, delta);
        self.focus_row(next);
        // Drawn by the row itself: the list's focus ring is off, and
        // the window's focus-visible flag is not reliably set by a key
        // a shortcut handler consumed.
        self.shared
            .set_cursor(self.shown.get(next).map(|m| (m.account_id, m.id)));
    }

    pub(super) fn on_open_cursor(&mut self, sender: &ComponentSender<Self>) {
        // Several checked are for an action, not for reading: which
        // one to open is anyone's guess (#392).
        if self.checking && self.selection_count > 1 {
            return;
        }
        let Some(pos) = self
            .cursor_position()
            .or_else(|| self.selected_positions().first().copied())
        else {
            return;
        };
        self.shared.set_cursor(None);
        let already_open = !self.checking && self.selected_positions() == [pos];
        self.shared.selection.select_item(pos as u32, true);
        self.stop_checking(sender);
        self.focus_row(pos);
        let _ = sender.output(MessageListOutput::OpenedByKey { already_open });
    }

    pub(super) fn on_pointer_pressed(&mut self, extend: bool, sender: &ComponentSender<Self>) {
        self.shared.set_cursor(None);
        if !extend {
            self.stop_checking(sender);
        }
    }

    pub(super) fn on_close_open(&mut self, sender: &ComponentSender<Self>) {
        // Only an open row closes: with rows checked, or none
        // selected, the keyboard just comes back to the list.
        let selected = self.selected_positions();
        let [pos] = selected[..] else {
            sender.input(MessageListInput::FocusList);
            return;
        };
        if self.checking {
            sender.input(MessageListInput::FocusList);
            return;
        }
        self.selected_id = None;
        self.selected_ids.clear();
        self.selection_count = 0;
        self.shared.selection.unselect_all();
        self.focus_row(pos);
        // The cursor stays on the row just read, so j, k and o carry
        // on from there.
        self.shared
            .set_cursor(self.shown.get(pos).map(|m| (m.account_id, m.id)));
        let _ = sender.output(MessageListOutput::SelectionCleared);
    }

    pub(super) fn on_step_conversation(&mut self, delta: i32) {
        if self.shown.is_empty() {
            return;
        }
        if delta != 0 {
            self.nav_direction = delta.signum();
        }
        let from = self.selected_positions().first().copied();
        let next = self.conversation_step(from, delta);
        if from == Some(next) {
            return;
        }
        self.shared.set_cursor(None);
        // Selecting opens it; the keyboard stays where it is, in the
        // reader, so the next j or k goes on from there.
        self.shared.selection.select_item(next as u32, true);
        self.list_view
            .scroll_to(next as u32, gtk::ListScrollFlags::NONE, None);
        self.reveal_first_heading(next);
    }

    pub(super) fn on_select_all_shown(&mut self, sender: &ComponentSender<Self>) {
        if !self.shown.is_empty() {
            // Checked, as Gmail's * a checks them: nothing opens.
            self.checking = true;
            self.selected_id = None;
            self.shared.selection.select_all();
            sender.input(MessageListInput::SelectionChanged);
        }
    }

    pub(super) fn on_select_where(&mut self, filter: CheckFilter, sender: &ComponentSender<Self>) {
        let is_child = |p: usize| self.shared.model.row(p).is_some_and(|r| r.meta.is_child);
        let picked = gtk::Bitset::new_empty();
        for (pos, m) in self.shown.iter().enumerate() {
            if !is_child(pos) && filter.matches(&self.conversation_for(m).0) {
                picked.add(pos as u32);
            }
        }
        // Checked, as with * a; none matching leaves none checked.
        self.checking = true;
        self.selected_id = None;
        let all = gtk::Bitset::new_range(0, self.shown.len() as u32);
        self.shared.selection.set_selection(&picked, &all);
        sender.input(MessageListInput::SelectionChanged);
    }

    pub(super) fn on_set_nav_direction(&mut self, delta: i32) {
        if delta != 0 {
            self.nav_direction = delta.signum();
        }
    }

    pub(super) fn on_toggle_selection(&mut self, sender: &ComponentSender<Self>) {
        // The row the keyboard is on, or the first selected one.
        let Some(pos) = self
            .cursor_position()
            .or_else(|| self.selected_positions().first().copied())
        else {
            return;
        };
        let selection = &self.shared.selection;
        if self.gmail_keys && !self.checking {
            // Gmail's x checks a conversation without opening it
            // (#392): the one open closes, and the checks start
            // from this row.
            self.checking = true;
            self.selected_id = None;
            selection.select_item(pos as u32, true);
        } else if selection.is_selected(pos as u32) {
            selection.unselect_item(pos as u32);
        } else {
            selection.select_item(pos as u32, false);
        }
        if self.checking {
            // The keyboard stays on the row, for the next j, k or x.
            self.focus_row(pos);
            self.shared
                .set_cursor(self.shown.get(pos).map(|m| (m.account_id, m.id)));
            // Told even when GTK's selection did not change, as when
            // the row checked is the one that was open.
            sender.input(MessageListInput::SelectionChanged);
        }
    }

    pub(super) fn on_focus_list(&mut self) {
        // The "back to list" shortcut: deliberately returns to the
        // selected row (that's the point — resume j/k navigation
        // from what you were reading), so it's allowed to scroll.
        if !self.shown.is_empty() {
            let pos = self.selected_positions().first().copied().unwrap_or(0);
            self.shared.set_cursor(None);
            self.focus_row(pos);
        }
    }

    pub(super) fn on_reclaim_focus(&mut self) {
        // Housekeeping focus restore (e.g. after a compose window
        // closes) so keyboard shortcuts keep targeting the list —
        // unlike `FocusList`, this isn't a "go back to what I was
        // reading" action, so it shouldn't scroll the viewport away
        // from wherever the user was browsing.
        self.preserving_scroll(|this| {
            this.list_view.grab_focus();
        });
        self.hide_focus_ring();
    }

    pub(super) fn on_clear_selection(&mut self) {
        self.shared.selection.unselect_all();
        self.selected_id = None;
        self.selected_ids.clear();
        self.selection_count = 0;
    }

    pub(super) fn on_row_activated(&mut self, index: i32, sender: &ComponentSender<Self>) {
        // With Gmail's keys, Enter on the row the cursor reached opens
        // it here, as o does, checked or not (#392); a row already
        // open goes to a window.
        if self.gmail_keys && (self.checking || !self.shared.selection.is_selected(index as u32)) {
            sender.input(MessageListInput::OpenCursor);
            return;
        }
        if let Some(m) = self.shown.get(index as usize).map(|m| Message::clone(m)) {
            let (thread, _solo) = self.conversation_for(&m);
            let _ = sender.output(MessageListOutput::Activated { message: m, thread });
        }
    }

    pub(super) fn on_set_selected(&mut self, id: Option<u32>) {
        match id {
            // Account-less id resolved against the shown list (the app
            // only sends `None` today; `Some` kept for completeness).
            Some(i) => {
                if let Some(m) = self.shown.iter().find(|m| m.id == i) {
                    let key = (m.account_id, m.id);
                    self.selected_id = Some(key);
                    self.selected_ids = vec![key];
                    self.select_current();
                }
            }
            None => {
                self.selected_id = None;
                self.selected_ids.clear();
                self.selection_count = 0;
                self.shared.selection.unselect_all();
            }
        }
    }

    pub(super) fn on_select_and_load(&mut self, key: (u32, u32), sender: &ComponentSender<Self>) {
        // Rows not built yet (the list landed in this same pass):
        // wait for the queued rebuild rather than find nothing.
        if self.rebuild_queued.is_some() {
            self.pending_select = Some(key);
            return;
        }
        self.late_select = None;
        let asked = key;
        self.stop_checking(sender);
        // A row further down than the window reaches comes on screen.
        if let Some(at) = self
            .tail
            .iter()
            .position(|r| (r.msg.account_id, r.msg.id) == key)
        {
            self.grow_window(at + 1);
        }
        // A reply inside a conversation has no row of its own: its
        // thread head does, and opening that shows the whole thread,
        // the reply included.
        let key = match self.shown.iter().any(|m| (m.account_id, m.id) == key) {
            true => key,
            false => match self.thread_head_for(key) {
                Some(head) => head,
                None => key,
            },
        };
        if let Some(m) = self
            .shown
            .iter()
            .find(|m| (m.account_id, m.id) == key)
            .map(|m| Message::clone(m))
        {
            self.selected_id = Some(key);
            self.selected_ids = vec![key];
            // Exactly this row: anything already selected (e.g. the
            // row the last deletion advanced to) would otherwise stay
            // lit and turn this into a two-row selection the reader
            // ignores.
            self.select_current();
            // These arrive from outside the list (a notification
            // click, an undo) where the row can be far outside the
            // viewport — bring it into view, in an idle so a rebuild
            // queued just before this has run first.
            if let Some(idx) = self.shown.iter().position(|m| (m.account_id, m.id) == key) {
                let list = self.list_view.clone();
                gtk::glib::idle_add_local_once(move || {
                    if idx < list.model().map_or(0, |m| m.n_items() as usize) {
                        list.scroll_to(idx as u32, gtk::ListScrollFlags::FOCUS, None);
                        list.grab_focus();
                    }
                    if let Some(win) = list.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                        win.set_focus_visible(false);
                    }
                });
            }
            let (thread, solo) = self.conversation_for(&m);
            self.emitted_thread = thread.iter().map(|t| (t.account_id, t.id)).collect();
            let ahead = self.rows_ahead(key);
            let _ = sender.output(MessageListOutput::Selected {
                message: m,
                thread,
                solo,
                ahead,
            });
        } else {
            // Not listed yet: mail that has only just arrived, whose
            // notification was clicked before its folder's list was in.
            // Dropping the request left the reader empty (#332).
            self.late_select = Some((asked, std::time::Instant::now()));
        }
    }

    /// The rows likely opened next after `key`: three the way the user is
    /// moving through the list, one the other way (#474).
    fn rows_ahead(&self, key: (u32, u32)) -> Vec<Message> {
        let Some(at) = self.shown.iter().position(|m| (m.account_id, m.id) == key) else {
            return Vec::new();
        };
        let step = if self.nav_direction < 0 { -1 } else { 1 };
        [step, 2 * step, 3 * step, -step]
            .into_iter()
            .filter_map(|d| at.checked_add_signed(d))
            .filter_map(|i| self.shown.get(i))
            .map(|m| Message::clone(m))
            .collect()
    }
}
