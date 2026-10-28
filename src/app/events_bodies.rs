//! Worker events about one message: its body, source, attachments and
//! sender check.

use relm4::prelude::*;

use crate::models::{Attachment, Message};
use crate::ui::attachments_gallery::GalleryInput;
use crate::ui::message_view::MessageViewInput;
use crate::ui::message_window::MessageWindowInput;
use crate::worker::MailRequest;

use super::conversation::{body_key, is_event_target, BodyKey};
use super::model::AppModel;
use super::msg::AppMsg;
use super::prefill::reply_prefill;

impl AppModel {
    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_events_bodies(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::SenderChecked {
                account_id,
                path,
                message_id,
                check,
            } => {
                // Remember it: prefetch delivers the verdict long before the
                // message is opened, and opening it renders from the in-memory
                // body cache without asking the worker for anything.
                let key = self.event_key(account_id, &path, message_id);
                if let Some(k) = key {
                    self.sender_cache.insert(k, check.clone());
                }
                // Read receipts asked for on mail sent, and ones that came
                // back (#520).
                self.note_receipts(key, &check);
                // A filter's forward of a message with no preview waits for
                // this to know it is not encrypted.
                self.resume_auto_forwards(account_id, message_id, key, false);
                let is_target = |m: &Message| is_event_target(m, key, account_id, message_id);
                // Only the message actually on screen; a verdict that arrives
                // from a background prefetch must not relabel a different one.
                if self.current.as_ref().is_some_and(is_target) {
                    self.message_view
                        .emit(MessageViewInput::SetSenderCheck(check.clone()));
                }
                // Light the header seal on whichever on-screen card this
                // verdict belongs to (#88) — the open single message included
                // (it never fills current_thread). The card goes by the id
                // it has here, which for a member from another folder is not
                // its UID.
                // A read receipt (#520) is asked about only where the card
                // should ask, and one the setting sends goes as it is shown.
                let card = self
                    .current_thread
                    .iter()
                    .chain(self.current.iter())
                    .find(|m| is_target(m))
                    .cloned();
                if let Some(m) = card {
                    let check = self.reader_check(&m, &check, true);
                    self.message_view.emit(MessageViewInput::SenderCheckFor {
                        account_id,
                        id: m.id,
                        check,
                    });
                }
                if let Some(k) = key.filter(|k| self.popouts.values().any(|p| p.message == *k)) {
                    let check = self.reader_check_by_key(k, &check);
                    for p in self.popouts.values().filter(|p| p.message == k) {
                        p.controller
                            .emit(MessageWindowInput::SetSenderCheck(check.clone()));
                    }
                }
            }

            AppMsg::Body {
                account_id,
                message_id,
                path,
                body,
            } => self.on_body(account_id, message_id, path, body, sender),

            AppMsg::Source { text } => {
                // Source is only fetched on explicit request (toolbar or context
                // menu), so always show it — even for a message that isn't open.
                self.show_source_window(&text);
            }

            AppMsg::Attachments {
                account_id,
                path,
                message_id,
                items,
            } => self.on_attachments_arrived(account_id, path, message_id, items, sender),

            AppMsg::AttachmentsPending {
                account_id,
                path,
                message_id,
            } => {
                // Attachments exist but weren't on disk. Opening the message
                // was the request — fetch them now rather than asking for a
                // click (the old "load attachments" button). Every reader path
                // now downloads outright, so this is a safety net for any
                // cache-only probe that still answers "present, not fetched".
                let key = self.event_key(account_id, &path, message_id);
                let is_target = |m: &Message| is_event_target(m, key, account_id, message_id);
                let msg = self
                    .current
                    .as_ref()
                    .filter(|c| is_target(c))
                    .cloned()
                    .or_else(|| self.current_thread.iter().find(|m| is_target(m)).cloned());
                if let Some(m) = msg {
                    if let Some(path) = self.resolve_folder_path(&m) {
                        self.attachments_loading = true;
                        self.send_to(
                            account_id,
                            MailRequest::LoadAttachments {
                                message_id,
                                path,
                                uid: m.uid,
                                download: true,
                            },
                        );
                    }
                }
                for p in self.popouts.values() {
                    let member = p.members.iter().find(|(k, (a, id))| {
                        key.map_or(*a == account_id && *id == message_id, |key| *k == key)
                    });
                    if let Some((_, (a, id))) = member {
                        p.controller.emit(MessageWindowInput::AttachmentsPending {
                            account_id: *a,
                            id: *id,
                        });
                    }
                }
            }

            AppMsg::NoAttachments {
                account_id,
                path,
                message_id,
            } => {
                // Clear a false paperclip live: on this message, in its own
                // folder. A UID names a message only within its folder, so
                // the same number elsewhere is a different message.
                let key = self.event_key(account_id, &path, message_id);
                let is_target = |m: &Message| is_event_target(m, key, account_id, message_id);
                self.set_has_attachment(key, false);
                self.resume_auto_forwards(account_id, message_id, key, true);
                if let Some(c) = self.current.as_mut() {
                    if is_target(c) {
                        c.has_attachment = false;
                    }
                }
                // A copy waiting on files that do not exist: open it anyway,
                // rather than leave the menu entry looking dead.
                if let Some(mut pending) = self.pending_edit_as_new.take() {
                    if is_target(&pending) {
                        pending.has_attachment = false;
                        self.edit_as_new(pending, &sender);
                    } else {
                        self.pending_edit_as_new = Some(pending);
                    }
                }
                if let Some((mut pending, inline)) = self.pending_forward.take() {
                    if is_target(&pending) {
                        pending.has_attachment = false;
                        self.forward(pending, inline, &sender);
                    } else {
                        self.pending_forward = Some((pending, inline));
                    }
                }
            }

            AppMsg::HasAttachments {
                account_id,
                path,
                message_id,
            } => {
                // The mirror of NoAttachments: a message whose structure didn't
                // advertise its attachments (an inline PDF, say — issue #9) is
                // given its paperclip once the body has proved they are there.
                let key = self.event_key(account_id, &path, message_id);
                let is_target = |m: &Message| is_event_target(m, key, account_id, message_id);
                self.set_has_attachment(key, true);
                // If it is the message on screen, fetch the files too: the reader
                // only asks for them when the flag was already set, which by
                // definition it wasn't.
                let already = self
                    .current
                    .as_ref()
                    .is_some_and(|c| is_target(c) && c.has_attachment);
                let open = self.current.as_mut().filter(|c| is_target(c));
                if let (false, Some(current)) = (already, open) {
                    current.has_attachment = true;
                    let message = current.clone();
                    if let Some(cached) = self.attachment_cache.get(&body_key(&message)).cloned() {
                        self.attachments = cached;
                        self.sync_attachment_drawer();
                    } else if let Some(path) = self.resolve_folder_path(&message) {
                        self.attachments_loading = true;
                        self.send_to(
                            account_id,
                            MailRequest::LoadAttachments {
                                message_id,
                                path,
                                uid: message.uid,
                                download: true,
                            },
                        );
                    }
                }
            }
            _ => unreachable!("not routed to on_events_bodies"),
        }
    }

    /// These, like `Body`, name the folder: a UID means a message only
    /// within its folder.
    fn on_attachments_arrived(
        &mut self,
        account_id: u32,
        path: String,
        message_id: u32,
        items: Vec<Attachment>,
        sender: ComponentSender<Self>,
    ) {
        let key = self.event_key(account_id, &path, message_id);
        if let Some(k) = key {
            self.attachment_cache.insert(k, items.clone());
        }
        self.resume_auto_forwards(account_id, message_id, key, false);
        let is_target = |m: &Message| is_event_target(m, key, account_id, message_id);
        // A file the gallery asked for: hand the bytes straight over so
        // the thumbnail and the preview fill in without a reload, which
        // would throw away the user's place in the list.
        if self.showing_gallery {
            self.gallery.emit(GalleryInput::SetFetching(false));
            self.gallery.emit(GalleryInput::Fetched {
                account_id,
                path: path.clone(),
                uid: key.map_or(message_id, |k| k.2),
                items: items.clone(),
            });
        }
        if self.current.as_ref().is_some_and(is_target) {
            self.attachments_loading = false;
            self.attachments = items.clone();
            self.sync_attachment_drawer();
        }
        // With a conversation open the drawer spans the whole thread, so
        // any member's arrival re-merges the union (this supersedes the
        // single-message assignment above when both apply).
        if self.current_thread.len() > 1 && self.current_thread.iter().any(is_target) {
            self.attachments_loading = false;
            self.refresh_thread_attachments();
        }
        for p in self.popouts.values() {
            let member = p.members.iter().find(|(k, (a, id))| {
                key.map_or(*a == account_id && *id == message_id, |key| *k == key)
            });
            if let Some((_, (a, id))) = member {
                p.controller.emit(MessageWindowInput::SetAttachments {
                    account_id: *a,
                    id: *id,
                    items: items.clone(),
                });
            }
        }
        // These files were fetched to be carried into a copy of the
        // message; they are cached now, so the second pass stages them.
        if let Some(pending) = self.pending_edit_as_new.take() {
            if is_target(&pending) {
                self.edit_as_new(pending, &sender);
            } else {
                self.pending_edit_as_new = Some(pending);
            }
        }
        // The same for a forward that carries them (#240).
        if let Some((pending, inline)) = self.pending_forward.take() {
            if is_target(&pending) {
                self.forward(pending, inline, &sender);
            } else {
                self.pending_forward = Some((pending, inline));
            }
        }
    }

    /// A message body, fetched for the reader or by the prefetch.
    fn on_body(
        &mut self,
        account_id: u32,
        message_id: u32,
        path: String,
        body: String,
        sender: ComponentSender<Self>,
    ) {
        // Interior NULs abort glib string conversion (labels and the
        // WebView document alike); decoded mail bodies can carry them.
        let body = if body.contains('\0') {
            body.replace('\0', " ")
        } else {
            body
        };
        // A UID is unique only within its folder, and the background
        // prefetch pushes bodies from every folder it syncs, so the
        // body is filed by folder and UID. A folder the app does not
        // know yet has no key; the body is still on disk for when it
        // does.
        let key = self.event_key(account_id, &path, message_id);
        if let Some(k) = key {
            self.body_cache.insert(k, body.clone());
        }
        self.resume_auto_forwards(account_id, message_id, key, false);
        if self.hand_body_to_waiter(account_id, message_id, &body, &sender) {
            return;
        }
        self.show_arrived_body(key, account_id, message_id, &body, &sender);
        // Re-render any popped-out window showing this message — a
        // conversation window may hold it as any member of its thread,
        // so every popout gets the offer (non-members ignore it).
        self.popouts_emit(|| MessageWindowInput::SetBody {
            account_id,
            id: message_id,
            body: body.clone(),
        });
    }

    /// Hands an arrived body to whatever was waiting for it: a draft to
    /// open, a copy to edit as new, a forward, a reply from a notification
    /// button or one picked for handed-in files. True when one took it and
    /// the reader has nothing more to do with it.
    fn hand_body_to_waiter(
        &mut self,
        account_id: u32,
        message_id: u32,
        body: &str,
        sender: &ComponentSender<Self>,
    ) -> bool {
        // If this body was fetched to open a draft, open the editor now.
        // One waiting for its bytes ignores a body the prefetch sent.
        if let Some((pd, inline, extra)) = self
            .pending_draft
            .take_if(|_| self.pending_draft_token.is_none())
        {
            if pd.account_id == account_id && pd.id == message_id {
                self.compose_from_draft(pd, body.to_string(), inline, extra, sender);
                return true;
            }
            self.pending_draft = Some((pd, inline, extra));
        }
        // Likewise a copy being edited as a new message: the body is
        // in the cache now, so the second pass finds it there.
        if let Some(pending) = self.pending_edit_as_new.take() {
            if pending.account_id == account_id && pending.id == message_id {
                self.edit_as_new(pending, sender);
                return true;
            }
            self.pending_edit_as_new = Some(pending);
        }
        // And a forward waiting on the body it quotes.
        if let Some((pending, inline)) = self.pending_forward.take() {
            if pending.account_id == account_id && pending.id == message_id {
                self.forward(pending, inline, sender);
                return true;
            }
            self.pending_forward = Some((pending, inline));
        }
        // A reply from a notification's button (#244): the message
        // is opening in the reader too, so the body goes on to be
        // shown as usual.
        if let Some(mut m) = self.pending_notified_reply.take() {
            if m.account_id == account_id && m.id == message_id {
                m.body = body.to_string();
                self.open_reply(
                    m.account_id,
                    self.reply_context(&m, reply_prefill(&m, &self.reply_header)),
                    (m.account_id, m.id),
                    sender,
                );
            } else {
                self.pending_notified_reply = Some(m);
            }
        }
        // Likewise a reply picked for handed-in files.
        if let Some((mut m, extra)) = self.pending_reply.take() {
            if m.account_id == account_id && m.id == message_id {
                m.body = body.to_string();
                self.open_hand_off_reply(m, extra, sender);
                return true;
            }
            self.pending_reply = Some((m, extra));
        }
        false
    }

    /// The reader takes an arrived body: the lone message re-renders, a
    /// conversation queues one render for a burst of them.
    fn show_arrived_body(
        &mut self,
        key: Option<BodyKey>,
        account_id: u32,
        message_id: u32,
        body: &str,
        sender: &ComponentSender<Self>,
    ) {
        let is_target = |m: &Message| is_event_target(m, key, account_id, message_id);
        // Keep the primary's body up to date in either mode.
        if let Some(current) = self.current.as_mut() {
            if is_target(current) {
                current.body = body.to_string();
            }
        }
        if self.current_thread.len() > 1 {
            // Conversation mode: fill the matching message's body. The
            // whole conversation renders as one document holding every
            // body, so a burst of arrivals is coalesced into a single
            // re-render rather than one per body.
            let mut changed = false;
            for tm in self.current_thread.iter_mut() {
                if is_target(tm) {
                    tm.body = body.to_string();
                    changed = true;
                }
            }
            if changed {
                self.queue_thread_render(sender);
            }
        } else if self.current.as_ref().is_some_and(is_target) {
            let current = self.current.clone();
            self.show_message(current, false);
        }
    }
}
