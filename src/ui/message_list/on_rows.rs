//! Acting on rows: bulk actions, the row's own actions and menus, and
//! patching a row's flags and tags in place.

use super::threading::thread_slot;
use super::types::{BulkAction, MessageListInput, MessageListOutput};
use super::{MessageList, RowAction};
use crate::models::Message;
use adw::prelude::*;
use relm4::prelude::*;

impl MessageList {
    pub(super) fn on_bulk(&mut self, action: BulkAction, sender: &ComponentSender<Self>) {
        // The bulk bar's spam button is one button: in Junk it means
        // the reverse.
        let action = if self.in_junk && action == BulkAction::Spam {
            BulkAction::NotSpam
        } else {
            action
        };
        let rows = self.selected_messages();
        // Likewise the star: it clears a selection that is starred
        // throughout, or it could never be taken off again (#313).
        let action =
            if action == BulkAction::Flag && !rows.is_empty() && rows.iter().all(|m| m.starred) {
                BulkAction::Unflag
            } else {
                action
            };
        // A conversation's row stands for all of it, as it does for a
        // lone Delete: marking several read reached only the first
        // message of each (#392). A star goes on the row's message.
        let messages = if action == BulkAction::Flag {
            rows
        } else {
            self.selected_conversations()
        };
        if !messages.is_empty() {
            let _ = sender.output(MessageListOutput::Bulk { action, messages });
        }
        // The selection stays, and with it the bulk bar: read and star
        // change rows in place, so a second action (or the same one
        // again, to undo it) can follow without selecting anew (#313).
        // Row-removing actions need it too: the RemoveMany that
        // follows reads it to know the viewed message is going away
        // and to advance the selection (and reader) in its place.
    }

    pub(super) fn on_move_selected_to(
        &mut self,
        account_id: u32,
        dest: String,
        sender: &ComponentSender<Self>,
    ) {
        let messages = self.selected_conversations();
        if !messages.is_empty() {
            let _ = sender.output(MessageListOutput::MoveMessages {
                account_id,
                dest,
                messages,
            });
        }
    }

    pub(super) fn on_bulk_toggle_read(&mut self, sender: &ComponentSender<Self>) {
        let action = if self.selection_any_unread() {
            BulkAction::MarkRead
        } else {
            BulkAction::MarkUnread
        };
        sender.input(MessageListInput::Bulk(action));
    }

    pub(super) fn on_bulk_tag_menu(&mut self, sender: &ComponentSender<Self>) {
        let btn = self.bulk_tag_btn.clone();
        let entries = self.bulk_tag_entries(sender);
        if !entries.is_empty() {
            // The menu takes the keyboard while it is open and does
            // not hand it back, which greys the selection and stops
            // the single-key shortcuts: give it back to the list.
            let before = btn.root().and_then(|r| r.focus());
            let popover = crate::ui::context_menu::show_context_menu_popover(
                &btn,
                (btn.width() / 2) as f64,
                btn.height() as f64,
                None,
                vec![entries],
            );
            popover.connect_closed(move |_| {
                if let Some(w) = &before {
                    w.grab_focus();
                }
            });
        }
    }

    pub(super) fn on_set_answered(&mut self, slot: (u32, u32, u32), forwarded: bool, on: bool) {
        self.update_message(
            |m| thread_slot(m) == slot,
            |m| {
                if forwarded {
                    m.forwarded = on
                } else {
                    m.answered = on
                }
            },
        );
        self.refresh_thread_head(slot);
    }

    pub(super) fn on_set_keywords(
        &mut self,
        slot: (u32, u32, u32),
        keywords: Vec<String>,
        labels: Vec<String>,
    ) {
        self.update_message(
            |m| thread_slot(m) == slot,
            |m| {
                m.keywords = keywords.clone();
                m.labels = labels.clone();
            },
        );
    }

    pub(super) fn on_set_tags(&mut self, tags: Vec<crate::config::Tag>) {
        if *self.shared.tags.borrow() != tags {
            *self.shared.tags.borrow_mut() = tags;
            // Chips follow the definitions.
            self.tags_gen += 1;
            self.sync_look();
        }
    }

    pub(super) fn on_set_has_attachment(
        &mut self,
        account_id: u32,
        folder_id: u32,
        uid: u32,
        has: bool,
    ) {
        self.update_message(
            |m| m.account_id == account_id && m.folder_id == folder_id && m.uid == uid,
            |m| m.has_attachment = has,
        );
    }

    pub(super) fn on_row_action(
        &mut self,
        action: RowAction,
        message: Box<Message>,
        sender: &ComponentSender<Self>,
    ) {
        // Starring a conversation row stars the conversation (#star):
        // every member together, toggled as a unit — individual
        // members keep their own stars via their own rows/cards.
        if matches!(action, RowAction::ToggleStar) {
            let members = self.thread_members(&message);
            let is_head = members
                .first()
                .is_some_and(|h| (h.account_id, h.id) == (message.account_id, message.id));
            if is_head && members.len() > 1 {
                // ANY starred member reads as a starred conversation
                // (matching the head's indicator), so the toggle can
                // always clear — all-starred semantics deadlocked the
                // moment one member was individually unstarred.
                let any = members.iter().any(|m| m.starred);
                let _ = sender.output(MessageListOutput::Bulk {
                    action: if any {
                        BulkAction::Unflag
                    } else {
                        BulkAction::Flag
                    },
                    messages: members,
                });
                return;
            }
        }
        let conversation = self.row_conversation(&message);
        let _ = sender.output(MessageListOutput::Action {
            action,
            message,
            conversation,
        });
    }

    pub(super) fn on_debug_swipe(&mut self, index: usize, left: bool) {
        if let Some(row) = self.shared.row_at(index) {
            row.debug_swipe(left);
        }
    }

    pub(super) fn on_debug_row_menu(&mut self, sender: &ComponentSender<Self>) {
        if let Some(m) = self.shown.first().map(|m| Message::clone(m)) {
            self.show_context_menu(&m, 120.0, 40.0, sender);
        }
    }

    pub(super) fn on_context_menu(
        &mut self,
        x: f64,
        y: f64,
        key: (u32, u32),
        sender: &ComponentSender<Self>,
    ) {
        let Some(pos) = self.shown.iter().position(|m| (m.account_id, m.id) == key) else {
            return;
        };
        let selected = self.selected_positions();
        if selected.len() > 1 && selected.contains(&pos) {
            // Right-clicked inside a multi-selection → bulk menu.
            self.show_bulk_menu(x, y, sender);
        } else {
            // Single-row menu acting on the clicked message. Crucially,
            // don't select it — selecting would load it in the reader,
            // and the user may just intend to move/archive/delete it.
            let msg = Message::clone(&self.shown[pos]);
            self.show_context_menu(&msg, x, y, sender);
        }
    }
}
