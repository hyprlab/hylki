//! Rows leaving the list: removed messages, a conversation shrinking, and
//! where the selection goes next.

use super::types::{BulkAction, MessageListInput, MessageListOutput};
use super::MessageList;
use crate::models::Message;
use relm4::prelude::*;

impl MessageList {
    pub(super) fn on_resolve_delete(&mut self, sender: &ComponentSender<Self>) {
        // A lone selected row that heads a multi-message conversation
        // stands for the whole thread: hand every member to the app
        // (which confirms before deleting). Anything else — multiple
        // rows, a reply row, a plain message — is an ordinary bulk
        // delete of exactly what is selected.
        let selected = self.selected_messages();
        if let [m] = selected.as_slice() {
            let key = (m.account_id, m.id);
            if let Some(tkey) = self.msg_thread.get(&key) {
                let members = self.thread_members.get(tkey).cloned().unwrap_or_default();
                if members.len() > 1 && members.first() == Some(&key) {
                    let messages: Vec<Message> = members
                        .iter()
                        .filter_map(|mk| {
                            self.active_source()
                                .iter()
                                .find(|x| (x.account_id, x.id) == *mk)
                                .map(|m| Message::clone(m))
                        })
                        .collect();
                    let _ = sender.output(MessageListOutput::DeleteThread { messages });
                    return;
                }
            }
        }
        sender.input(MessageListInput::Bulk(BulkAction::Delete));
    }

    pub(super) fn on_remove(&mut self, id: u32, sender: &ComponentSender<Self>) {
        // Was the removed message the one shown in the reader? If so we'll
        // advance to whatever row slides into its place.
        let was_viewed = self.selected_id.map(|(_, i)| i) == Some(id);
        if was_viewed {
            self.selected_id = None;
        }
        self.selected_ids.retain(|(_, i)| *i != id);
        self.all.retain(|m| m.id != id);
        self.drop_threads();
        self.tail.retain(|r| r.msg.id != id);
        let removed_idx = self.shown.iter().position(|m| m.id == id);
        // Was the row about to go the one holding keyboard focus? If
        // so, and it isn't the viewed row handled below, focus is
        // put on its neighbour rather than left to GTK's fallback.
        let had_focus = removed_idx
            .and_then(|idx| self.shared.row_at(idx))
            .is_some_and(|row| row.has_focus_within());
        if let Some(idx) = removed_idx {
            self.shown.remove(idx);
            self.shared.model.splice(idx, 1, Vec::new());
            // No rebuild follows — keep the header count honest.
            self.total_matches = self.total_matches.saturating_sub(1);
        }
        if self.checking {
            self.selection_count = self.selected_ids.len();
            let _ = sender.output(MessageListOutput::SelectionKeys(
                self.slots_at(&self.selected_positions()),
            ));
            let _ = sender.output(MessageListOutput::Checked(Some(self.selection_count)));
        }

        if was_viewed {
            match removed_idx {
                // Advance in the direction the user was triaging: the
                // row now at the removed slot when moving down the
                // list, the one above it when moving up (Apple Mail's
                // behaviour). Selecting it fires SelectionChanged,
                // which loads it in the reader.
                Some(idx) if !self.shown.is_empty() => {
                    let next = self.advance_index(idx);
                    self.select_and_focus(next);
                }
                // Nothing left to show → clear the reader.
                _ => {
                    let _ = sender.output(MessageListOutput::SelectionCleared);
                }
            }
        } else if had_focus {
            if let Some(idx) = removed_idx {
                if !self.shown.is_empty() {
                    self.focus_only(self.advance_index(idx));
                }
            }
        }
    }

    pub(super) fn on_leave_conversation(&mut self, gone: (u32, u32), head: (u32, u32)) {
        // The cache's count of the conversation still holds the
        // message until the server has moved it, so the badge is
        // brought down here rather than asked for again. A reply
        // filed in another folder is no row of this list: the
        // conversation is found by its row.
        let group = [gone, head]
            .iter()
            .find_map(|k| self.msg_thread.get(k))
            .cloned()
            .or_else(|| {
                self.groups
                    .iter()
                    .find(|(_, ms)| ms.iter().any(|m| (m.account_id, m.id) == head))
                    .map(|(k, _)| k.clone())
            });
        // A message of this folder goes by another id there, so by
        // folder and UID; one filed elsewhere by the id the reader
        // gave it.
        let slot = self
            .all
            .iter()
            .find(|m| (m.account_id, m.id) == gone)
            .map(|m| (m.account_id, m.folder_id, m.uid));
        if let Some(s) = group.and_then(|k| self.thread_summaries.get_mut(&k)) {
            let before = s.members.len();
            s.members.retain(|m| {
                (m.account_id, m.id) != gone && Some((m.account_id, m.folder_id, m.uid)) != slot
            });
            if s.members.len() < before {
                s.count = s.count.saturating_sub(before - s.members.len());
                // The row may have been quoting it as the newest.
                s.latest = s.members.iter().max_by_key(|m| m.timestamp).map(|m| {
                    crate::models::ThreadLatest {
                        from_name: m.from_name.clone(),
                        from_addr: m.from_addr.clone(),
                        preview: m.preview.clone(),
                        timestamp: m.timestamp,
                        date: m.date.clone(),
                    }
                });
            }
        }
        self.all.retain(|m| (m.account_id, m.id) != gone);
        self.tail.retain(|r| (r.msg.account_id, r.msg.id) != gone);
        self.emitted_thread.retain(|k| *k != gone);
        self.drop_threads();
        // The row is selected under its new head before the rebuild
        // lights it, so the selection reads as the one already open
        // and the reader is not loaded again.
        for k in self.selected_ids.iter_mut().filter(|k| **k == gone) {
            *k = head;
        }
        self.selected_ids.dedup();
        if self.selected_id == Some(gone) {
            self.selected_id = Some(head);
        }
        self.queue_rebuild(true);
    }

    /// Remove the rows at `slots`, and no others: a row elsewhere in a unified
    /// list with the same id stays, and so does its namesake until a sync
    /// settles it, since removal goes by id.
    pub(super) fn on_remove_exact(
        &mut self,
        slots: Vec<(u32, u32, u32)>,
        sender: &ComponentSender<Self>,
    ) {
        let slots: std::collections::HashSet<(u32, u32, u32)> = slots.into_iter().collect();
        let others: std::collections::HashSet<u32> = self
            .all
            .iter()
            .filter(|m| !slots.contains(&(m.account_id, m.folder_id, m.id)))
            .map(|m| m.id)
            .collect();
        let ids: Vec<u32> = self
            .all
            .iter()
            .filter(|m| {
                slots.contains(&(m.account_id, m.folder_id, m.id)) && !others.contains(&m.id)
            })
            .map(|m| m.id)
            .collect();
        self.on_remove_many(ids, sender);
    }

    pub(super) fn on_remove_many(&mut self, ids: Vec<u32>, sender: &ComponentSender<Self>) {
        if ids.is_empty() {
            return;
        }
        let set: std::collections::HashSet<u32> = ids.into_iter().collect();
        let was_viewed = self
            .selected_id
            .map(|(_, i)| i)
            .is_some_and(|i| set.contains(&i));
        if was_viewed {
            self.selected_id = None;
        }
        self.selected_ids.retain(|(_, i)| !set.contains(i));
        self.all.retain(|m| !set.contains(&m.id));
        self.drop_threads();
        self.tail.retain(|r| !set.contains(&r.msg.id));
        // Where the first removed row sat, so we can re-select in its place.
        let first_removed = self.shown.iter().position(|m| set.contains(&m.id));
        let had_focus = self
            .shared
            .bound_rows()
            .into_iter()
            .any(|r| r.has_focus_within() && r.data().is_some_and(|d| set.contains(&d.msg.id)));
        // Out in runs, back to front, so the view hears of each run
        // once and the positions still to come stay valid.
        let shown_before = self.shown.len();
        let mut idx = self.shown.len();
        while idx > 0 {
            idx -= 1;
            if !set.contains(&self.shown[idx].id) {
                continue;
            }
            let end = idx + 1;
            while idx > 0 && set.contains(&self.shown[idx - 1].id) {
                idx -= 1;
            }
            self.shown.drain(idx..end);
            self.shared.model.splice(idx, end - idx, Vec::new());
        }
        self.selection_count = self.selected_ids.len();
        if self.checking {
            let _ = sender.output(MessageListOutput::SelectionKeys(
                self.slots_at(&self.selected_positions()),
            ));
            let _ = sender.output(MessageListOutput::Checked(Some(self.selection_count)));
        }
        // Keep the header count honest; no rebuild follows.
        self.total_matches = self
            .total_matches
            .saturating_sub(shown_before - self.shown.len());
        if self.checking {
            // Checked rows deleted (#392): the keyboard goes to the
            // conversation above them, else the one below, outlined
            // but neither checked nor opened.
            let cursor_gone = self.shared.cursor().is_some_and(|(_, i)| set.contains(&i));
            if let Some(idx) =
                first_removed.filter(|_| (cursor_gone || had_focus) && !self.shown.is_empty())
            {
                let pos = if idx > 0 {
                    self.conversation_step(Some(idx), -1)
                } else {
                    0
                };
                self.focus_row(pos);
                self.shared
                    .set_cursor(self.shown.get(pos).map(|m| (m.account_id, m.id)));
            }
        } else if was_viewed {
            match first_removed {
                Some(idx) if !self.shown.is_empty() => {
                    let next = self.advance_index(idx);
                    self.select_and_focus(next);
                }
                _ => {
                    let _ = sender.output(MessageListOutput::SelectionCleared);
                }
            }
        } else if had_focus {
            if let Some(idx) = first_removed {
                if !self.shown.is_empty() {
                    self.focus_only(self.advance_index(idx));
                }
            }
        }
    }
}
