//! Message actions from the toolbar, the cards and the list rows: star,
//! archive, delete, spam and bulk actions.

use relm4::prelude::*;

use crate::config;
use crate::models::{FolderKind, Message};
use crate::ui::message_list::{BulkAction, MessageListInput, RowAction};
use crate::ui::notifications::NotifyInput;
use crate::worker::MailRequest;

use super::flags::{read_label, star_label};
use super::hand_off::HandOffFiles;
use super::model::AppModel;
use super::msg::AppMsg;
use super::prefill::{reply_all_prefill, reply_prefill};
use super::undo::FlagKind;
use super::uri_queue::eml_file_name;

impl AppModel {
    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_actions(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::ToggleStar => {
                // Several selected: star them all, or clear them all once
                // every one is starred (#313).
                let many = self.multi_selection();
                if !many.is_empty() {
                    let star = !many.iter().all(|m| m.starred);
                    self.user_set_flags(star_label(star), &many, FlagKind::Star, star);
                    return;
                }
                if let Some(m) = self.reply_target() {
                    if self.thread_star_target(&m) {
                        // The conversation is the target: any member starred
                        // reads as a starred thread, and the toggle clears or
                        // sets the whole set (same semantics as the list row).
                        let any = self.current_thread.iter().any(|t| t.starred);
                        let members = self.current_thread.clone();
                        self.user_set_flags(star_label(!any), &members, FlagKind::Star, !any);
                    } else {
                        let starred = !m.starred;
                        self.user_set_flags(star_label(starred), &[m], FlagKind::Star, starred);
                    }
                }
            }

            AppMsg::Archive => {
                if self.bulk_on_cards(BulkAction::Archive, &sender) {
                    return;
                }
                if self.many_selected() {
                    self.message_list
                        .emit(MessageListInput::Bulk(BulkAction::Archive));
                    return;
                }
                // An open conversation's row stands for all of it, as it does
                // for Delete; a picked card is that message alone (#392).
                if self.acts_on_conversation(false) {
                    let conversation = self.conversation_to_archive();
                    if conversation.len() > 1 {
                        self.apply_bulk_move(BulkAction::Archive, conversation);
                        return;
                    }
                }
                if let Some(m) = self.reply_target() {
                    self.move_to(m, FolderKind::Archive);
                }
            }
            AppMsg::Delete => {
                // More than one message selected: the trash button deletes the
                // whole selection, exactly as the bulk bar's Delete would. One
                // row checked with Gmail's x is a lone row, whose conversation
                // asks first as an open one does (#392).
                if self.bulk_on_cards(BulkAction::Delete, &sender) {
                    return;
                }
                if self.many_selected() {
                    self.message_list.emit(if self.list_selection.len() == 1 {
                        MessageListInput::ResolveDelete
                    } else {
                        MessageListInput::Bulk(BulkAction::Delete)
                    });
                    return;
                }
                // A lone list-row selection over an open conversation may be
                // its thread head, which stands for the whole thread — the
                // list resolves that (DeleteThread) or falls back to a plain
                // bulk delete. A picked card is always just that message.
                if !self.selection_from_cards
                    && self.list_selection.len() == 1
                    && self.current_thread.len() > 1
                {
                    self.message_list.emit(MessageListInput::ResolveDelete);
                    return;
                }
                if let Some(m) = self.reply_target() {
                    // In the Outbox, deleting means giving up on sending it —
                    // there is no server-side copy to move to Trash.
                    if self.outbox_item(m.account_id, m.id).is_some() {
                        self.drop_outbox_row(m.account_id, m.id, &sender);
                        self.current = None;
                        self.show_message(None, false);
                    } else {
                        self.delete_messages(vec![m], &sender);
                    }
                }
            }
            AppMsg::DeleteForever => {
                // The list resolves its rows as it does for Delete, a
                // conversation's row bringing all of it, and hands them back
                // as a bulk action that asks first.
                if self.bulk_on_cards(BulkAction::DeleteForever, &sender) {
                    return;
                }
                if self.many_selected()
                    || (!self.selection_from_cards
                        && self.list_selection.len() == 1
                        && self.current_thread.len() > 1)
                {
                    self.message_list
                        .emit(MessageListInput::Bulk(BulkAction::DeleteForever));
                    return;
                }
                if let Some(m) = self.reply_target() {
                    if self.outbox_item(m.account_id, m.id).is_some() {
                        self.drop_outbox_row(m.account_id, m.id, &sender);
                        self.current = None;
                        self.show_message(None, false);
                    } else {
                        self.confirm_purge(vec![m], &sender);
                    }
                }
            }

            AppMsg::PurgeMessages(messages) => {
                self.purge_messages(messages);
            }

            AppMsg::CardAction { action, message } => self.on_card_action(action, message, sender),

            AppMsg::ToggleReadCurrent => {
                // Rows selected in the list: the list marks every message
                // of the conversations they stand for (#392).
                if self.rows_selected() {
                    self.message_list.emit(MessageListInput::BulkToggleRead);
                    return;
                }
                // Several selected: any unread one makes them all read,
                // otherwise they all go back to unread (#313).
                let many = self.multi_selection();
                if !many.is_empty() {
                    let read = many.iter().any(|m| m.unread);
                    self.user_set_flags(read_label(read), &many, FlagKind::Read, read);
                } else if let Some(m) = self.reply_target() {
                    let read = m.unread;
                    self.user_set_flags(read_label(read), &[m], FlagKind::Read, read);
                }
            }

            AppMsg::CardContact(message) => {
                self.show_add_contact_dialog(&message.from_name, &message.from_addr, &sender);
            }

            AppMsg::RowAction {
                action,
                message,
                conversation,
            } => self.on_row_action(action, message, conversation, sender),

            AppMsg::Bulk { action, messages } => self.on_bulk(action, messages, sender),

            AppMsg::BulkComplete => {
                self.bulk_pending = self.bulk_pending.saturating_sub(1);
                self.update_busy_indicator();
                if self.bulk_pending == 0 && self.busy.is_empty() {
                    self.notifications
                        .emit(NotifyInput::SetStatus(String::new()));
                }
            }

            AppMsg::MarkSpam => {
                // In Junk the same button, shortcut and menu entry mean the
                // reverse (#168); for a selection the list makes that swap.
                if self.bulk_on_cards(BulkAction::Spam, &sender) {
                    return;
                }
                if self.many_selected() {
                    self.message_list
                        .emit(MessageListInput::Bulk(BulkAction::Spam));
                    return;
                }
                if let Some(m) = self.reply_target() {
                    if self.in_junk(&m) {
                        self.mark_ham_msg(m);
                    } else {
                        self.mark_spam_msg(m);
                    }
                }
            }

            AppMsg::DeleteThread(messages) => {
                if messages.is_empty() {
                    return;
                }
                if self.confirm_thread_delete {
                    self.confirm_delete_thread(messages, &sender);
                } else {
                    self.delete_messages(messages, &sender);
                }
            }

            AppMsg::DeleteThreadConfirmed(messages) => {
                self.delete_messages(messages, &sender);
            }

            AppMsg::DeleteFromAddress { account_id, addr } => {
                let messages = self.mail_from_address(account_id, &addr);
                tracing::info!(
                    "unsubscribe: deleting {} message(s) from {addr}",
                    messages.len()
                );
                if !messages.is_empty() {
                    self.delete_messages(messages, &sender);
                }
            }
            _ => unreachable!("not routed to on_actions"),
        }
    }

    /// A conversation card's own action pill. Reply/Reply all/Forward open the
    /// reader's inline composer (like the toolbar); the rest act like RowAction.
    fn on_card_action(
        &mut self,
        action: RowAction,
        message: Box<Message>,
        sender: ComponentSender<Self>,
    ) {
        // A reply started from a card belongs in the main window's
        // inline composer, exactly like the toolbar's buttons — not in
        // a separate compose window.
        let m = self.with_cached_body(*message);
        tracing::debug!(
            target: "hylki::reply",
            "card {:?}: {}:{} from={}",
            action, m.account_id, m.id, m.from_addr,
        );
        match action {
            RowAction::Reply => {
                self.open_reply(
                    m.account_id,
                    self.reply_context(&m, reply_prefill(&m, &self.reply_header)),
                    (m.account_id, m.id),
                    &sender,
                );
            }
            RowAction::ReplyAll => {
                let own = self.own_identities(m.account_id);
                self.open_reply(
                    m.account_id,
                    self.reply_context(&m, reply_all_prefill(&m, &own, &self.reply_header)),
                    (m.account_id, m.id),
                    &sender,
                );
            }
            RowAction::Forward => {
                self.forward(m, true, &sender);
            }
            // A template's banner (#360): edited where a new message
            // is written.
            RowAction::EditTemplate => {
                let inline = self.compose_inline;
                self.open_draft(m, inline, HandOffFiles::default(), &sender);
            }
            // Cards only carry the three above; anything else falls
            // through to the ordinary row behaviour.
            other => sender.input(AppMsg::RowAction {
                action: other,
                message: Box::new(m),
                // A card is always its own message.
                conversation: Vec::new(),
            }),
        }
    }

    /// A row's palette or context-menu action. `conversation` holds the whole
    /// thread when the row stands for a collapsed conversation rather than for
    /// `message` alone — a reply started there answers the conversation's
    /// newest message, not the head it is filed under (#210). Empty otherwise.
    fn on_row_action(
        &mut self,
        action: RowAction,
        message: Box<Message>,
        conversation: Vec<Message>,
        sender: ComponentSender<Self>,
    ) {
        let m = *message;
        if matches!(
            action,
            RowAction::Reply | RowAction::ReplyAll | RowAction::Forward
        ) {
            tracing::debug!(
                target: "hylki::reply",
                "row {:?}: {}:{} from={} conversation={:?}",
                action, m.account_id, m.id, m.from_addr,
                conversation.iter().map(|c| c.id).collect::<Vec<_>>(),
            );
        }
        if self.outbox_item(m.account_id, m.id).is_some() {
            // Nothing else in the palette applies to an unsent message,
            // and every other action would aim an IMAP command at a UID
            // that doesn't exist on any server.
            if matches!(action, RowAction::Delete) {
                self.drop_outbox_row(m.account_id, m.id, &sender);
            }
            return;
        }
        match action {
            // A row that stands for a whole conversation answers the
            // conversation's newest message, exactly as the reader's
            // own reply does (#210) — the row is filed under the
            // thread's head, which is its oldest message.
            RowAction::Reply => {
                let m = self.newest_to_answer(&conversation, m);
                let m = self.with_cached_body(m);
                self.open_compose(
                    m.account_id,
                    self.reply_context(&m, reply_prefill(&m, &self.reply_header)),
                    &sender,
                );
            }
            RowAction::ReplyAll => {
                let m = self.newest_to_answer(&conversation, m);
                let m = self.with_cached_body(m);
                let own = self.own_identities(m.account_id);
                self.open_compose(
                    m.account_id,
                    self.reply_context(&m, reply_all_prefill(&m, &own, &self.reply_header)),
                    &sender,
                );
            }
            RowAction::Forward => {
                let m = self.newest_to_answer(&conversation, m);
                self.forward(m, false, &sender);
            }
            // A conversation row stands for its newest message here
            // too: that is the one the row is showing.
            RowAction::EditAsNew => {
                let m = self.newest_to_answer(&conversation, m);
                self.edit_as_new(m, &sender);
            }
            // Edited where a new message is written (#360), from a
            // row's menu as from the banner.
            RowAction::EditTemplate => {
                let inline = self.compose_inline;
                self.open_draft(m, inline, HandOffFiles::default(), &sender);
            }
            RowAction::ToggleStar => {
                let starred = !m.starred;
                self.user_set_flags(star_label(starred), &[m], FlagKind::Star, starred);
            }
            RowAction::ToggleRead => {
                let read = m.unread;
                self.user_set_flags(read_label(read), &[m], FlagKind::Read, read);
            }
            RowAction::Spam => self.mark_spam_msg(m),
            RowAction::NotSpam => self.mark_ham_msg(m),
            RowAction::Archive => self.move_to(m, FolderKind::Archive),
            RowAction::MoveToInbox => self.move_to(m, FolderKind::Inbox),
            RowAction::Delete => self.delete_messages(vec![m], &sender),
            RowAction::ViewSource => self.request_source(&m),
            RowAction::SaveAsFile => self.save_as_file(&m),
            RowAction::AddContact => {
                self.show_add_contact_dialog(&m.from_name, &m.from_addr, &sender);
            }
            RowAction::CreateFilter => self.create_filter_from(&m, &sender),
            // A conversation's row snoozes the conversation, as far as it
            // is in this folder: a reply in Sent stays where it is.
            RowAction::Snooze(choice) => {
                let mut messages: Vec<Message> = conversation
                    .into_iter()
                    .filter(|c| c.account_id == m.account_id && c.folder_id == m.folder_id)
                    .collect();
                if !messages.iter().any(|c| c.id == m.id) {
                    messages.push(m);
                }
                self.snooze_choice(messages, choice, &sender);
            }
        }
    }

    /// View Source: ask the server for the message as it keeps it.
    fn request_source(&self, m: &Message) {
        if let Some(path) = self.resolve_folder_path(m) {
            self.send_to(
                m.account_id,
                MailRequest::LoadSource {
                    message_id: m.id,
                    path,
                    uid: m.uid,
                },
            );
        }
    }

    /// Save as File: the whole message as the server keeps it, asked for
    /// first; the save dialog opens once it is here.
    fn save_as_file(&mut self, m: &Message) {
        if let Some(path) = self.resolve_folder_path(m) {
            let token = self.next_transfer;
            self.next_transfer += 1;
            self.pending_saves.insert(token, eml_file_name(&m.subject));
            self.send_to(
                m.account_id,
                MailRequest::ExportRaw {
                    token,
                    path,
                    uid: m.uid,
                    for_reader: true,
                },
            );
        }
    }

    /// Create Filter: the filter editor, opened on a rule drafted from the
    /// message's sender and subject.
    fn create_filter_from(&mut self, m: &Message, sender: &ComponentSender<Self>) {
        let Some(email) = self.email_of(m.account_id) else {
            return;
        };
        let rule = config::FilterRule::draft_from_message(&email, &m.from_addr, &m.subject);
        self.open_settings_at(
            sender,
            "filters",
            crate::ui::accounts::AccountsInput::AddFilterFrom(rule),
        );
    }

    /// Several cards of the open conversation picked in the reader: the
    /// action is theirs alone. The list's row they share stands for the
    /// whole conversation, so handing it the action deleted all of it.
    pub(super) fn bulk_on_cards(
        &mut self,
        action: BulkAction,
        sender: &ComponentSender<Self>,
    ) -> bool {
        if !self.many_selected() || !self.selection_from_cards {
            return false;
        }
        let messages = self.multi_selection();
        if messages.is_empty() {
            return false;
        }
        // In Junk, Spam means the reverse, as it does for the list (#168).
        let action = if action == BulkAction::Spam && messages.iter().all(|m| self.in_junk(m)) {
            BulkAction::NotSpam
        } else {
            action
        };
        self.on_bulk(action, messages, sender.clone());
        true
    }

    /// A bulk action applied to every selected message.
    fn on_bulk(
        &mut self,
        action: BulkAction,
        messages: Vec<Message>,
        sender: ComponentSender<Self>,
    ) {
        if self.showing_outbox {
            if matches!(action, BulkAction::Delete | BulkAction::DeleteForever) {
                for m in &messages {
                    self.drop_outbox_row(m.account_id, m.id, &sender);
                }
            }
            self.message_list.emit(MessageListInput::ClearSelection);
            return;
        }
        match action {
            // Flag/read changes update rows in place (no removal).
            BulkAction::MarkRead => {
                self.user_set_flags(read_label(true), &messages, FlagKind::Read, true)
            }
            BulkAction::MarkUnread => {
                self.user_set_flags(read_label(false), &messages, FlagKind::Read, false)
            }
            BulkAction::Flag => {
                self.user_set_flags(star_label(true), &messages, FlagKind::Star, true)
            }
            BulkAction::Unflag => {
                self.user_set_flags(star_label(false), &messages, FlagKind::Star, false)
            }
            BulkAction::DeleteForever => self.confirm_purge(messages, &sender),
            BulkAction::Snooze | BulkAction::Unsnooze => self.apply_snooze(action, messages),
            // Archive/Delete/Spam remove every selected row. Doing that one
            // at a time blocks the UI thread (a render cycle per message) and
            // trips GTK's "app is not responding" dialog for large selections.
            // Batch it; for big selections show a spinner and defer the apply
            // one tick so the spinner paints before the blocking work runs.
            BulkAction::Archive
            | BulkAction::Spam
            | BulkAction::NotSpam
            | BulkAction::Delete
            | BulkAction::MoveToInbox => {
                // Deleting in Trash means erasing, not moving — split the
                // selection so each half takes the right path (and the
                // erasures get their confirmation).
                if matches!(action, BulkAction::Delete) && messages.iter().any(|m| self.in_trash(m))
                {
                    self.delete_messages(messages, &sender);
                    return;
                }
                // The apply is one batched widget update, so no overlay
                // needed at any size: rows vanish at once, the server
                // work runs invisibly in the workers, and the refresh
                // spinner + status bar carry the progress story.
                self.apply_bulk_move(action, messages);
            }
        }
    }
}
