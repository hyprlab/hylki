//! The selection and the keyboard: positions, the cursor, focus, and
//! stepping through conversations.

use super::layout::hold_top;
use super::threading::thread_slot;
use super::types::{MessageListInput, MessageListOutput, SelectionProbe};
use super::MessageList;
use crate::ui::message_row::Slot;
use adw::prelude::*;
use relm4::prelude::*;

impl MessageList {
    pub(crate) fn selection_probe(&self) -> SelectionProbe {
        SelectionProbe {
            selection: self.shared.selection.clone(),
            model: self.shared.model.clone(),
        }
    }

    /// The row the keyboard is on, if it is on one.
    pub(super) fn cursor_position(&self) -> Option<usize> {
        self.shared
            .bound_rows()
            .into_iter()
            .find(|r| r.has_focus_within())
            .and_then(|r| r.position())
    }

    /// The selected rows' positions, in list order.
    pub(super) fn selected_positions(&self) -> Vec<usize> {
        let set = self.shared.selection.selection();
        match gtk::BitsetIter::init_first(&set) {
            Some((iter, first)) => std::iter::once(first)
                .chain(iter)
                .map(|p| p as usize)
                .collect(),
            None => Vec::new(),
        }
    }

    pub(super) fn keys_at(&self, positions: &[usize]) -> Vec<(u32, u32)> {
        positions
            .iter()
            .filter(|&&p| !self.is_stub_at(p))
            .filter_map(|&p| self.shown.get(p).map(|m| (m.account_id, m.id)))
            .collect()
    }

    pub(super) fn slots_at(&self, positions: &[usize]) -> Vec<Slot> {
        positions
            .iter()
            .filter(|&&p| !self.is_stub_at(p))
            .filter_map(|&p| self.shown.get(p).map(|m| thread_slot(m)))
            .collect()
    }

    /// Select exactly these rows, telling GTK only if that changes anything.
    pub(super) fn select_positions(&self, positions: &[usize]) {
        let mut wanted: Vec<usize> = positions
            .iter()
            .copied()
            .filter(|&p| p < self.shown.len())
            .collect();
        wanted.sort_unstable();
        wanted.dedup();
        if wanted == self.selected_positions() {
            return;
        }
        let set = gtk::Bitset::new_empty();
        for p in &wanted {
            set.add(*p as u32);
        }
        let mask = gtk::Bitset::new_range(0, self.shown.len() as u32);
        self.shared.selection.set_selection(&set, &mask);
    }

    /// Select row `idx` and put the keyboard focus on it.
    ///
    /// Focus matters after a removal: GTK would otherwise pick a fallback of
    /// its own, which can be the top of the list — and moving focus scrolls
    /// the viewport with it, so the list appears to jump away from where the
    /// user was working (#19). Taking focus deliberately also means the
    /// single-key shortcuts carry on from the row that is now selected.
    pub(super) fn select_and_focus(&self, idx: usize) {
        if idx < self.shown.len() {
            self.shared.selection.select_item(idx as u32, false);
            self.focus_row(idx);
        }
    }

    /// Bring row `idx` into view and give it the keyboard. Scrolling with
    /// FOCUS only moves the list's own focus item; the keyboard follows it
    /// only when the list already has it.
    pub(super) fn focus_row(&self, idx: usize) {
        if idx < self.shown.len() {
            self.list_view
                .scroll_to(idx as u32, gtk::ListScrollFlags::FOCUS, None);
            self.reveal_first_heading(idx);
            self.list_view.grab_focus();
        }
    }

    /// A `scroll_to` the first row lines it up with the top, leaving the
    /// date heading over it out of sight (#435): the list goes back to its
    /// very top once laid out.
    pub(super) fn reveal_first_heading(&self, idx: usize) {
        if let (0, true, Some(s)) = (idx, self.day_headings, &self.scroller) {
            hold_top(&s.vadjustment(), &self.shared);
        }
    }

    /// End Gmail's checking (#392): the row now selected, if it is one, opens.
    pub(super) fn stop_checking(&mut self, sender: &ComponentSender<Self>) {
        if std::mem::take(&mut self.checking) {
            let _ = sender.output(MessageListOutput::Checked(None));
            // Told even when GTK's selection stays as it was: a lone checked
            // row is selected already, and opening it changes nothing there.
            sender.input(MessageListInput::SelectionChanged);
        }
    }

    /// The row `delta` conversations away from `from`, clamped to the list:
    /// the replies of a conversation opened out in the list are stepped
    /// over, as Gmail's j and k move between conversations (#392), and so
    /// are folded date headings (#659). From no row, the first step lands on
    /// the top or the bottom one.
    pub(super) fn conversation_step(&self, from: Option<usize>, delta: i32) -> usize {
        let pos = self.conversation_step_any(from, delta);
        if !self.is_stub_at(pos) {
            return pos;
        }
        self.mail_row_from(pos, delta)
            .or_else(|| self.mail_row_from(pos, -delta))
            .unwrap_or(pos)
    }

    fn conversation_step_any(&self, from: Option<usize>, delta: i32) -> usize {
        let last = self.shown.len().saturating_sub(1);
        let is_child = |p: usize| {
            self.is_stub_at(p) || self.shared.model.row(p).is_some_and(|r| r.meta.is_child)
        };
        let Some(mut pos) = from.filter(|&p| p <= last) else {
            let mut p = if delta > 0 { 0 } else { last };
            while p > 0 && is_child(p) {
                p -= 1;
            }
            return p;
        };
        for _ in 0..delta.unsigned_abs() {
            let mut p = pos;
            loop {
                if delta > 0 {
                    if p == last {
                        break;
                    }
                    p += 1;
                } else {
                    if p == 0 {
                        break;
                    }
                    p -= 1;
                }
                if !is_child(p) {
                    break;
                }
            }
            if is_child(p) {
                // Only replies past here: stay on the conversation already
                // reached.
                break;
            }
            pos = p;
        }
        pos
    }

    /// Where deletion advances to, once the row at `idx` is gone: the row now
    /// occupying that slot when the user was moving down the list, the row
    /// above it when they were moving up. Clamped to the list either way.
    pub(super) fn advance_index(&self, idx: usize) -> usize {
        let next = if self.nav_direction < 0 {
            idx.saturating_sub(1)
        } else {
            idx
        };
        let next = next.min(self.shown.len().saturating_sub(1));
        // Not onto a folded date heading (#659): the next message past it.
        let step = if self.nav_direction < 0 { -1 } else { 1 };
        self.mail_row_from(next, step)
            .or_else(|| self.mail_row_from(next, -step))
            .unwrap_or(next)
    }

    /// Put keyboard focus on row `idx` without touching selection — the
    /// counterpart to `select_and_focus` for a row that wasn't the one being
    /// viewed. Used when a removed row held focus but wasn't the viewed
    /// message (e.g. deleted via its own row action while browsing further
    /// down the list).
    pub(super) fn focus_only(&self, idx: usize) {
        self.focus_row(idx);
        self.hide_focus_ring();
    }

    /// Drop the window's focus-visible flag after a *programmatic* focus grab:
    /// the reclaimed row keeps keyboard focus (arrow keys resume from it), but
    /// no accent focus ring appears around a row the user never navigated to.
    /// The next real key press turns the ring back on, as normal.
    pub(super) fn hide_focus_ring(&self) {
        if let Some(win) = self
            .list_view
            .root()
            .and_then(|r| r.downcast::<gtk::Window>().ok())
        {
            win.set_focus_visible(false);
        }
    }

    /// Re-apply the whole selection (the viewed message plus any multi-selected
    /// rows) so it persists across rebuilds — background syncs included — until
    /// the user clicks away.
    pub(super) fn select_current(&self) {
        let positions: Vec<usize> = self
            .selected_ids
            .iter()
            .filter_map(|key| self.shown.iter().position(|m| (m.account_id, m.id) == *key))
            .collect();
        self.select_positions(&positions);
    }
}
