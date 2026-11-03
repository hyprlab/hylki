//! Moving and tagging from menus and keys.

use adw::prelude::*;
use relm4::prelude::*;

use crate::models::FolderKind;
use crate::ui::message_list::{BulkAction, MessageListInput};

use super::model::AppModel;
use super::msg::AppMsg;
use super::undo::FlagKind;

impl AppModel {
    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_move_tag(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::SetTag {
                message,
                keyword,
                add,
            } => {
                let what = self.tag_label(&keyword, add);
                self.user_set_flags(what, &[*message], FlagKind::Tag(keyword), add);
            }

            AppMsg::SetTagMany {
                messages,
                keyword,
                add,
            } => {
                let what = self.tag_label(&keyword, add);
                self.user_set_flags(what, &messages, FlagKind::Tag(keyword), add);
            }

            AppMsg::MoveToInbox => {
                if self.bulk_on_cards(BulkAction::MoveToInbox, &sender) {
                    return;
                }
                if self.many_selected() {
                    self.message_list
                        .emit(MessageListInput::Bulk(BulkAction::MoveToInbox));
                    return;
                }
                if let Some(m) = self.reply_target() {
                    self.move_to(m, FolderKind::Inbox);
                }
            }

            AppMsg::ToggleTagCurrent(keyword) => {
                let targets = self.selected_targets();
                self.toggle_tag(keyword, &targets);
            }

            AppMsg::TagKey { n, slots } => {
                // Settings order, first nine. A number past the list does
                // nothing rather than something surprising.
                let Some(keyword) = self
                    .tags
                    .get(usize::from(n).saturating_sub(1))
                    .map(|t| t.keyword.clone())
                else {
                    return;
                };
                // Arrowing quickly, the key can arrive before the app has
                // heard the list moved: the rows selected at the key press are
                // the ones meant, or the tag lands on the row before and the
                // next press takes it back off (#416).
                let keys: Vec<(u32, u32)> = slots.iter().map(|&(a, _, id)| (a, id)).collect();
                let targets =
                    if !keys.is_empty() && keys != self.list_heard && keys != self.list_selection {
                        self.rows_at(&slots)
                    } else {
                        self.selected_targets()
                    };
                self.toggle_tag(keyword, &targets);
            }

            AppMsg::MoveToMenu => {
                // A multi-selection lists the folders of its first message's
                // account (the move reports anything from another account);
                // otherwise the target's. The picker leaves out the folder
                // the mail already sits in.
                let (account_id, exclude) = if self.many_selected() {
                    (self.list_selection[0].0, None)
                } else {
                    match self.reply_target() {
                        Some(m) => (m.account_id, self.resolve_folder_path(&m)),
                        None => return,
                    }
                };
                let accounts = self.picker_accounts(account_id, exclude);
                if accounts.is_empty() {
                    return;
                }
                // The reading pane shows a conversation (its row was opened,
                // not one reply of it): offer to move the whole of it.
                let conversation = (!self.many_selected() && self.current_thread.len() > 1)
                    .then_some(self.current_thread.len());
                // Anchored to the toolbar button, or to the overflow button
                // when the toolbar has folded into it.
                let btn = if self.reader_move_btn.is_mapped() {
                    self.reader_move_btn.clone()
                } else {
                    self.reader_overflow_btn.clone()
                };
                let s = sender.input_sender().clone();
                crate::ui::folder_picker::show_folder_picker(
                    &btn,
                    (btn.width() / 2) as f64,
                    btn.height() as f64,
                    accounts,
                    conversation,
                    move |account_id, dest, whole| {
                        let _ = s.send(AppMsg::MoveSelectionTo {
                            account_id,
                            dest,
                            whole,
                        });
                    },
                );
            }

            AppMsg::ListMoveTo {
                messages,
                offer_whole,
                x,
                y,
            } => {
                let Some(first) = messages.first() else {
                    return;
                };
                let account_id = first.account_id;
                // Leave out the folder the mail sits in, when it is one.
                let exclude = self.resolve_folder_path(first).filter(|p| {
                    messages
                        .iter()
                        .all(|m| self.resolve_folder_path(m).as_ref() == Some(p))
                });
                let accounts = self.picker_accounts(account_id, exclude);
                if accounts.is_empty() {
                    return;
                }
                let conversation = offer_whole.then_some(messages.len());
                let s = sender.input_sender().clone();
                let window = self.window.clone();
                crate::ui::folder_picker::show_folder_picker(
                    &window,
                    x,
                    y,
                    accounts,
                    conversation,
                    move |account_id, dest, whole| {
                        let picked = if offer_whole && !whole {
                            messages[..1].to_vec()
                        } else {
                            messages.clone()
                        };
                        let _ = s.send(AppMsg::MoveMessagesTo {
                            account_id,
                            dest,
                            messages: picked,
                        });
                    },
                );
            }

            AppMsg::ListCopyTo {
                messages,
                offer_whole,
                x,
                y,
            } => {
                let Some(first) = messages.first() else {
                    return;
                };
                let accounts = self.copy_picker_accounts(first.account_id, None);
                if accounts.is_empty() {
                    return;
                }
                let conversation = offer_whole.then_some(messages.len());
                let s = sender.input_sender().clone();
                let window = self.window.clone();
                crate::ui::folder_picker::show_folder_picker(
                    &window,
                    x,
                    y,
                    accounts,
                    conversation,
                    move |account_id, dest, whole| {
                        let picked = if offer_whole && !whole {
                            messages[..1].to_vec()
                        } else {
                            messages.clone()
                        };
                        let _ = s.send(AppMsg::CopyMessagesTo {
                            account_id,
                            dest,
                            messages: picked,
                        });
                    },
                );
            }

            AppMsg::CopyMessagesTo {
                account_id,
                dest,
                messages,
            } => self.copy_messages(messages, account_id, dest),

            AppMsg::MoveMessagesTo {
                account_id,
                dest,
                messages,
            } => {
                let items: Vec<(u32, u32, u32, u32)> = messages
                    .iter()
                    .map(|m| (m.account_id, m.folder_id, m.uid, m.id))
                    .collect();
                self.drop_move(account_id, dest, items);
            }

            AppMsg::MoveSelectionTo {
                account_id,
                dest,
                whole,
            } => {
                if whole && self.current_thread.len() > 1 {
                    // The whole conversation, the way a dragged selection
                    // moves: grouped by source folder, undoable, and any
                    // member from another account (a conversation merged
                    // across accounts in a unified view) reported.
                    let items: Vec<(u32, u32, u32, u32)> = self
                        .current_thread
                        .iter()
                        .map(|m| (m.account_id, m.folder_id, m.uid, m.id))
                        .collect();
                    self.drop_move(account_id, dest, items);
                } else if self.rows_selected() {
                    // The list knows the conversations its rows stand for.
                    self.message_list
                        .emit(MessageListInput::MoveSelectedTo { account_id, dest });
                } else if self.many_selected() {
                    // The same path a drag onto the sidebar takes: grouped by
                    // source folder, undoable, foreign accounts reported.
                    let items: Vec<(u32, u32, u32, u32)> = self
                        .list_selection
                        .iter()
                        .filter_map(|(aid, id)| {
                            self.find_cached_message(*aid, *id)
                                .map(|m| (m.account_id, m.folder_id, m.uid, m.id))
                        })
                        .collect();
                    self.drop_move(account_id, dest, items);
                } else if let Some(m) = self.reply_target() {
                    if m.account_id == account_id {
                        self.move_to_path(m, dest);
                    } else {
                        self.drop_move(
                            account_id,
                            dest,
                            vec![(m.account_id, m.folder_id, m.uid, m.id)],
                        );
                    }
                }
            }
            _ => unreachable!("not routed to on_move_tag"),
        }
    }
}
