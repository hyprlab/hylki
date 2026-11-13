//! The pop-out's longer `update` arms: a member's body and flags arriving,
//! its attachments, and the actions that close or move it.

use super::types::MessageWindowOutput;
use super::MessageWindow;
use crate::models::{Attachment, Message};
use adw::prelude::*;
use relm4::prelude::*;

use crate::config::ToolbarItem;
use crate::ui::attachment_drawer::for_previewer;
use crate::ui::attachment_io::save_attachment_as;
use crate::ui::lightbox::LightboxInput;
use crate::ui::message_list::RowAction;
use crate::ui::message_view::{ChipAction, MessageViewInput};

impl MessageWindow {
    pub(super) fn on_invite_action(
        &mut self,
        message: Box<Message>,
        invite: Box<crate::models::Invite>,
        action: crate::ui::message_view::InviteAction,
        sender: &ComponentSender<Self>,
    ) {
        let _ = sender.output(MessageWindowOutput::InviteAction {
            message,
            invite,
            action,
        });
    }

    pub(super) fn on_unsubscribe_state(
        &mut self,
        account_id: u32,
        id: u32,
        state: Option<crate::ui::message_view::UnsubState>,
    ) {
        self.view.emit(MessageViewInput::UnsubscribeState {
            account_id,
            id,
            state,
        });
    }

    pub(super) fn on_set_sender_check(
        &mut self,
        check: Box<crate::models::SenderCheck>,
        root: &adw::Window,
    ) {
        // Decrypted, the window is named by the subject inside (#493).
        if let Some(subject) = check.pgp.as_ref().and_then(|p| p.subject.as_deref()) {
            root.set_title(Some(subject));
        }
        // Light the popout's header seal too (#88).
        self.view.emit(MessageViewInput::SenderCheckFor {
            account_id: self.msg.account_id,
            id: self.msg.id,
            check: check.clone(),
        });
        self.view.emit(MessageViewInput::SetSenderCheck(check));
    }

    pub(super) fn on_set_body(&mut self, account_id: u32, id: u32, body: String) {
        let mine = self.msg.account_id == account_id && self.msg.id == id;
        let member = self
            .thread
            .iter_mut()
            .find(|t| t.account_id == account_id && t.id == id);
        if !mine && member.is_none() {
            return;
        }
        if let Some(t) = member {
            t.body = body.clone();
        }
        if mine {
            self.msg.body = body;
            self.loading = false;
        }
        self.render_body();
    }

    pub(super) fn on_set_keywords(&mut self, keywords: Vec<String>, labels: Vec<String>) {
        self.msg.keywords = keywords.clone();
        self.msg.labels = labels.clone();
        for m in self.thread.iter_mut() {
            if m.account_id == self.msg.account_id && m.id == self.msg.id {
                m.keywords = keywords.clone();
                m.labels = labels.clone();
            }
        }
        self.view.emit(MessageViewInput::SetCardKeywords {
            account_id: self.msg.account_id,
            id: self.msg.id,
            keywords,
            labels,
        });
    }

    pub(super) fn on_set_attachments(&mut self, account_id: u32, id: u32, items: Vec<Attachment>) {
        let key = (account_id, id);
        self.attachments.retain(|(k, _)| *k != key);
        self.attachments.push((key, items));
        self.attachments_loading.remove(&key);
        self.sync_attachments();
        self.sync_toolbar();
    }

    pub(super) fn on_attachments_pending(
        &mut self,
        account_id: u32,
        id: u32,
        sender: &ComponentSender<Self>,
    ) {
        // Opening the message was the request: fetch them, as the
        // main reader does, rather than wait for a click.
        let Some(m) = self.member(account_id, id).cloned() else {
            return;
        };
        self.attachments_loading.insert((account_id, id));
        let _ = sender.output(MessageWindowOutput::LoadAttachments(Box::new(m)));
        self.sync_toolbar();
    }

    pub(super) fn on_card_attachment(
        &mut self,
        account_id: u32,
        id: u32,
        index: usize,
        action: ChipAction,
        root: &adw::Window,
    ) {
        let Some(files) = self
            .attachments
            .iter()
            .find(|(k, _)| *k == (account_id, id))
            .map(|(_, f)| f)
        else {
            return;
        };
        let Some(att) = files.get(index).cloned() else {
            return;
        };
        let preview = match action {
            ChipAction::Preview(time) if for_previewer(&att) => Some(time),
            _ => None,
        };
        if action == ChipAction::Save {
            // A failed write is dropped.
            save_attachment_as(&att.name, att.data, Some(root.upcast_ref()), |_| {});
        } else if let Some(time) = preview {
            // Space on a chip (#625).
            crate::desktop::previewer::show(
                files.clone(),
                index,
                Some(root.upcast_ref()),
                time,
                None,
            );
        } else if crate::ui::attachment_drawer::previewable(&att) {
            // The lightbox pages through this message's previewable
            // files, starting at the one clicked.
            let previewable = |a: &&Attachment| crate::ui::attachment_drawer::previewable(a);
            let items: Vec<Attachment> = files.iter().filter(previewable).cloned().collect();
            let start = files[..=index]
                .iter()
                .filter(previewable)
                .count()
                .saturating_sub(1);
            self.lightbox.emit(LightboxInput::Show { items, start });
        } else {
            crate::ui::attachment_io::open_bytes(&att.name, &att.data, Some(root.upcast_ref()));
        }
    }

    pub(super) fn on_delete_attachment(&mut self, att: Attachment, sender: &ComponentSender<Self>) {
        let Some(message) = self.owner(&att).cloned() else {
            return;
        };
        let _ = sender.output(MessageWindowOutput::DeleteAttachment {
            message: Box::new(message),
            attachment: Box::new(att),
        });
    }

    pub(super) fn on_spam(&mut self, sender: &ComponentSender<Self>, root: &adw::Window) {
        let action = if self.in_junk {
            RowAction::NotSpam
        } else {
            RowAction::Spam
        };
        self.emit_action(action, sender);
        root.close();
    }

    pub(super) fn on_move_to(&mut self, sender: &ComponentSender<Self>, root: &adw::Window) {
        // Under the Move To button, or under the ⋯ that stands in
        // for it while the toolbar is folded.
        let button = self
            .button(ToolbarItem::MoveTo)
            .filter(|b| b.is_mapped())
            .unwrap_or(&self.bar.overflow);
        let Some(point) = button.compute_point(
            root,
            &gtk::graphene::Point::new(button.width() as f32 / 2.0, button.height() as f32),
        ) else {
            return;
        };
        let _ = sender.output(MessageWindowOutput::MoveTo {
            message: Box::new(self.msg.clone()),
            x: point.x().into(),
            y: point.y().into(),
        });
    }
}
