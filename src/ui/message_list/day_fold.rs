//! Folding the list's date sections (#659): which ones the view keeps
//! folded, folding and opening them, and keeping the rows that stand in for
//! folded sections out of the selection.

use super::sort::SortOrder;
use super::types::MessageListOutput;
use super::MessageList;
use crate::models::Message;
use crate::ui::message_row::{fold_stub, is_fold_stub, DaySection, RowData, RowMeta};
use relm4::prelude::*;
use std::rc::Rc;

/// The sections named by `keys`, as the settings keep them.
pub(super) fn days_from_keys(keys: &[String]) -> Vec<DaySection> {
    keys.iter()
        .filter_map(|k| DaySection::from_key(k))
        .collect()
}

impl MessageList {
    /// A conversation in a folded date section (#659): no row of its own,
    /// only a count on the row standing in for the section, made by the
    /// first. The conversation maps still learn it, so the reader and a
    /// later opening of the section find it as before.
    pub(super) fn fold_group(
        &mut self,
        key: &(u32, String),
        mut msgs: Vec<Rc<Message>>,
        section: DaySection,
        passes: &impl Fn(&Message) -> bool,
        shown: &mut Vec<Rc<Message>>,
        rows: &mut Vec<Rc<RowData>>,
    ) {
        msgs.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.uid.cmp(&b.uid)));
        let unread = msgs.iter().any(|m| m.unread);
        let (_, members) = self.describe_group(key, &msgs, passes, false);
        drop(members);
        if self.threading {
            self.groups.insert(key.clone(), msgs.clone());
        }
        let stub = rows
            .last_mut()
            .filter(|r| r.section == section && r.meta.folded.is_some())
            .and_then(Rc::get_mut);
        match stub {
            Some(stub) => {
                let (count, unread_count) = stub.meta.folded.unwrap_or_default();
                stub.meta.folded = Some((count + 1, unread_count + usize::from(unread)));
            }
            None => {
                let m = Rc::new(fold_stub(&msgs[0], section));
                shown.push(m.clone());
                rows.push(Rc::new(RowData {
                    msg: m,
                    meta: RowMeta {
                        folded: Some((1, usize::from(unread))),
                        revealed: true,
                        ..Default::default()
                    },
                    section,
                }));
            }
        }
    }

    /// Whether the row at `pos` is a folded section's heading.
    pub(super) fn is_stub_at(&self, pos: usize) -> bool {
        self.shown.get(pos).is_some_and(|m| is_fold_stub(m))
    }

    /// The first row at or past `pos` in the direction of `step`, a folded
    /// heading stepped over; `None` past the end.
    pub(super) fn mail_row_from(&self, pos: usize, step: i32) -> Option<usize> {
        let mut p = pos;
        while self.is_stub_at(p) {
            p = p.checked_add_signed(step.signum() as isize)?;
        }
        (p < self.shown.len()).then_some(p)
    }

    /// The view on show changed: the sections it keeps folded.
    pub(super) fn on_set_folded_days(&mut self, keys: Vec<String>) {
        let days = days_from_keys(&keys);
        if days != self.folded_days {
            self.folded_days = days;
            self.queue_rebuild(true);
        }
    }

    /// Fold `section`, or open it, and tell the app so the view keeps it.
    /// The keyboard goes to the section's heading when it folds, and to its
    /// first row when it opens.
    pub(super) fn on_fold_day(
        &mut self,
        section: DaySection,
        fold: bool,
        sender: &ComponentSender<Self>,
    ) {
        if !self.set_day_folded(section, fold, sender) {
            return;
        }
        self.rebuild();
        let at = (0..self.shown.len()).find(|&p| {
            self.shared
                .model
                .row(p)
                .is_some_and(|r| r.section == section)
        });
        if let Some(at) = at {
            self.focus_row(at);
        }
    }

    /// Record `section` as folded or open, and say whether that changed.
    fn set_day_folded(
        &mut self,
        section: DaySection,
        fold: bool,
        sender: &ComponentSender<Self>,
    ) -> bool {
        if section == DaySection::None || self.folded_days.contains(&section) == fold {
            return false;
        }
        if fold {
            self.folded_days.push(section);
        } else {
            self.folded_days.retain(|s| *s != section);
        }
        let keys = self
            .folded_days
            .iter()
            .map(|s| s.key().to_string())
            .collect();
        let _ = sender.output(MessageListOutput::FoldedDays(keys));
        true
    }

    /// The folded section `key`'s message is hidden in, if it is: the one
    /// its conversation's row would go under, by the message that places
    /// the row in a date sort.
    fn folded_section_of(&self, key: (u32, u32)) -> Option<DaySection> {
        if !self.day_headings || self.folded_days.is_empty() {
            return None;
        }
        let m = self
            .active_source()
            .iter()
            .find(|m| (m.account_id, m.id) == key)?
            .clone();
        let members = self.msg_thread.get(&key).and_then(|k| self.groups.get(k));
        let ts = match members {
            Some(ms) if self.sort == SortOrder::DateOldest => {
                ms.iter().map(|m| m.timestamp).min()
            }
            Some(ms) => ms.iter().map(|m| m.timestamp).max(),
            None => None,
        }
        .unwrap_or(m.timestamp);
        let starts = crate::datefmt::day_starts(crate::datefmt::now());
        let section = DaySection::of(ts, &starts);
        self.folded_days.contains(&section).then_some(section)
    }

    /// Open whichever folded sections hide the messages `keys`, which are
    /// being opened from elsewhere, such as a notification. True when any
    /// was, and the rows are rebuilt.
    pub(super) fn unfold_for(&mut self, keys: &[(u32, u32)], sender: &ComponentSender<Self>) -> bool {
        let sections: Vec<DaySection> = keys
            .iter()
            .filter_map(|k| self.folded_section_of(*k))
            .collect();
        let mut changed = false;
        for section in sections {
            changed |= self.set_day_folded(section, false, sender);
        }
        if changed {
            self.rebuild();
        }
        changed
    }

    /// A folded heading in the selection is taken out of it: the arrow keys
    /// select as they move, and a heading is no message. The selection left
    /// is what it was before, so the reader stays on the message it shows.
    /// True when the selection was put right, which GTK reports again.
    pub(super) fn drop_stubs_from_selection(&mut self, positions: &[usize]) -> bool {
        // GTK clears the selection when an arrow key reaches a row it may
        // not select: the message open stays selected.
        if positions.is_empty()
            && !self.selected_ids.is_empty()
            && self.cursor_position().is_some_and(|p| self.is_stub_at(p))
        {
            self.select_current();
            return true;
        }
        if !positions.iter().any(|&p| self.is_stub_at(p)) {
            return false;
        }
        let rest: Vec<usize> = positions
            .iter()
            .copied()
            .filter(|&p| !self.is_stub_at(p))
            .collect();
        if rest.is_empty() {
            let keep: Vec<usize> = self
                .selected_ids
                .iter()
                .filter_map(|k| self.shown.iter().position(|m| (m.account_id, m.id) == *k))
                .collect();
            self.select_positions(&keep);
        } else {
            self.select_positions(&rest);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folded_sections_are_kept_by_key_and_unknown_keys_dropped() {
        let all = [
            DaySection::Today,
            DaySection::Yesterday,
            DaySection::LastWeek,
            DaySection::LastFortnight,
            DaySection::Older,
        ];
        let keys: Vec<String> = all.iter().map(|s| s.key().to_string()).collect();
        assert_eq!(days_from_keys(&keys), all);
        assert_eq!(
            days_from_keys(&["older".into(), "someday".into(), String::new()]),
            [DaySection::Older]
        );
    }
}
