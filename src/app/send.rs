//! Sending: the undo-send delay, handing a message to its worker, and the
//! answered marks shown before the server has them.

use std::collections::HashSet;

use relm4::prelude::*;

use crate::i18n::i18n;
use crate::models::Message;
use crate::ui::compose::ComposeInput;
use crate::ui::message_list::MessageListInput;
use crate::worker::{MailRequest, OutgoingMessage};

use super::model::AppModel;
use super::msg::AppMsg;
use super::prefill::unsent_prefill;
use super::theme::motion_wanted;

/// A message the app writes itself (an answer to an invitation, an
/// unsubscribe request): plain text, nothing attached, signed or quoted.
pub(super) fn machine_message(
    from_account_id: u32,
    from_alias: Option<String>,
    to: String,
    subject: String,
    body: String,
    in_reply_to: String,
) -> crate::worker::OutgoingMessage {
    crate::worker::OutgoingMessage {
        from_account_id,
        from_alias,
        to,
        cc: String::new(),
        bcc: String::new(),
        reply_to: String::new(),
        subject,
        body,
        html: String::new(),
        attachments: Vec::new(),
        in_reply_to,
        references: String::new(),
        forwards: String::new(),
        draft_origin: None,
        outbox_origin: None,
        sign: false,
        encrypt: false,
        send_at: None,
        calendar: None,
        request_receipt: false,
        receipt_report: None,
    }
}

impl AppModel {
    /// Sends a message the app wrote itself: no copy filed in Sent, no
    /// ticket to follow it by (a failure still lands in the Outbox).
    pub(super) fn send_unfiled(&self, out: crate::worker::OutgoingMessage) {
        self.send_to(
            out.from_account_id,
            MailRequest::Send {
                message: Box::new(out),
                sent_path: None,
                ticket: 0,
            },
        );
    }

    /// Send's first stop (#434): the message waits out the Undo Send delay
    /// with Undo on offer in a toast, listed in the Outbox, then goes to its
    /// worker.
    fn queue_send(
        &mut self,
        origin: super::outgoing::Origin,
        out: Box<OutgoingMessage>,
        sent_path: Option<String>,
        sender: &ComponentSender<Self>,
    ) {
        let account_id = out.from_account_id;
        let Some(account) = account_id
            .checked_sub(1)
            .and_then(|i| self.effective_config().get(i as usize).cloned())
        else {
            self.send_to(
                account_id,
                MailRequest::Send {
                    message: out,
                    sent_path,
                    ticket: 0,
                },
            );
            return;
        };
        self.send_ticket += 1;
        let ticket = self.send_ticket;
        self.mark_answered_ahead(ticket, &out);
        let delay = self.undo_send_secs;
        let toast = adw::Toast::builder()
            .priority(adw::ToastPriority::High)
            .timeout(0)
            .build();
        let o = super::outgoing::Outgoing {
            ticket,
            account_id,
            account,
            message: out,
            sent_path,
            queued_at: crate::datefmt::now(),
            origin,
            held: delay > 0,
            toast: toast.clone(),
        };
        toast.set_title(&o.sending_title());
        if delay > 0 {
            toast.set_button_label(Some(&i18n("Undo")));
            toast.set_custom_title(Some(&crate::ui::toast_countdown::undo_countdown(
                &o.sending_title(),
                delay,
            )));
            let s = sender.clone();
            toast.connect_button_clicked(move |_| s.input(AppMsg::UndoSend(ticket)));
            // The timer is never cancelled: Undo or Send now take the
            // message off the list, and this finds nothing to do.
            let s = sender.clone();
            gtk::glib::timeout_add_local_once(
                std::time::Duration::from_secs(delay.into()),
                move || s.input(AppMsg::DispatchSend(ticket)),
            );
            self.show_toast(&toast);
        } else {
            // With nothing to undo, there is no toast.
            self.send_to(
                account_id,
                MailRequest::Send {
                    message: o.message.clone(),
                    sent_path: o.sent_path.clone(),
                    ticket,
                },
            );
        }
        self.outgoing.borrow_mut().push(o);
        self.push_outbox();
    }

    /// Mark what a reply or forward answers as soon as it is sent, rather
    /// than once it has gone (#454): the messages the worker will mark, as
    /// the cache knows them. One already marked is left out, so taking the
    /// marks off again cannot clear what another send or client set.
    fn mark_answered_ahead(&mut self, ticket: u64, out: &OutgoingMessage) {
        let account_id = out.from_account_id;
        let copies = crate::worker::answered_copies(
            self.cache.as_ref(),
            account_id,
            &out.in_reply_to,
            &out.forwards,
        );
        let mut marked = Vec::new();
        for (path, uid, forwarded) in copies {
            let Some(fid) = self.folder_by_path(account_id, &path).map(|f| f.id) else {
                continue;
            };
            let (answered, was_forwarded) = self
                .cache
                .as_ref()
                .map(|c| c.answer_state(account_id, &path, uid))
                .unwrap_or_default();
            if if forwarded { was_forwarded } else { answered } {
                continue;
            }
            self.show_answered(account_id, fid, uid, forwarded, true);
            marked.push((account_id, fid, uid, forwarded));
        }
        if !marked.is_empty() {
            self.pending_answers.insert(ticket, marked);
        }
    }

    /// The send was undone, or failed: its early marks come off.
    fn unmark_answered(&mut self, ticket: u64) {
        for (account_id, fid, uid, forwarded) in
            self.pending_answers.remove(&ticket).unwrap_or_default()
        {
            self.show_answered(account_id, fid, uid, forwarded, false);
        }
    }

    /// Put a replied or forwarded mark on a message, or take it off,
    /// wherever the app holds it.
    pub(super) fn show_answered(
        &mut self,
        account_id: u32,
        fid: u32,
        uid: u32,
        forwarded: bool,
        on: bool,
    ) {
        let same = |c: &Message| c.account_id == account_id && c.folder_id == fid && c.uid == uid;
        let mark = |c: &mut Message| {
            if forwarded {
                c.forwarded = on
            } else {
                c.answered = on
            }
        };
        let mut id = None;
        for msgs in [
            self.message_cache
                .get_mut(&(account_id, fid))
                .map(|c| &mut **c),
            self.unified_slices.get_mut(&(account_id, fid)),
        ]
        .into_iter()
        .flatten()
        {
            for c in msgs.iter_mut().filter(|c| same(c)) {
                mark(c);
                id = Some(c.id);
            }
        }
        self.current_thread
            .iter_mut()
            .filter(|c| same(c))
            .for_each(mark);
        if let Some(cur) = self.current.as_mut().filter(|c| same(c)) {
            mark(cur);
        }
        if let Some(id) = id {
            self.message_list.emit(MessageListInput::SetAnswered {
                slot: (account_id, fid, id),
                forwarded,
                on,
            });
        }
    }

    /// [`apply_pending_seen`] for the marks of a send still on its way: a
    /// list fetched before it went keeps them.
    pub(super) fn apply_pending_answers(
        &self,
        account_id: u32,
        folder_id: u32,
        mut messages: Vec<Message>,
    ) -> Vec<Message> {
        for &(a, f, uid, forwarded) in self.pending_answers.values().flatten() {
            if (a, f) != (account_id, folder_id) {
                continue;
            }
            for m in messages.iter_mut().filter(|m| m.uid == uid) {
                if forwarded {
                    m.forwarded = true;
                } else {
                    m.answered = true;
                }
            }
        }
        messages
    }

    /// The delay is over, or Send now was asked for: hand the message to its
    /// worker. Undo is no longer possible.
    pub(super) fn dispatch_send(&self, ticket: u64) {
        let request = {
            let mut list = self.outgoing.borrow_mut();
            let Some(o) = list.iter_mut().find(|o| o.ticket == ticket && o.held) else {
                return;
            };
            o.held = false;
            o.toast.dismiss();
            (o.account_id, o.message.clone(), o.sent_path.clone())
        };
        let (account_id, message, sent_path) = request;
        self.send_to(
            account_id,
            MailRequest::Send {
                message,
                sent_path,
                ticket,
            },
        );
        self.push_outbox();
    }

    /// Undo Send: the message comes back to the composer it was written in,
    /// as it was. A split reply reopens beside its card if that card is
    /// still on screen; otherwise it gets a window.
    pub(super) fn undo_send(&mut self, ticket: u64, sender: &ComponentSender<Self>) {
        let taken = {
            let mut list = self.outgoing.borrow_mut();
            list.iter()
                .position(|o| o.ticket == ticket && o.held)
                .map(|i| list.remove(i))
        };
        let Some(o) = taken else { return };
        o.toast.dismiss();
        self.unmark_answered(ticket);
        self.push_outbox();
        self.send_morph.finish();
        self.drop_provisional(&o.message.in_reply_to);
        if self.showing_outbox
            && self
                .current
                .as_ref()
                .is_some_and(|m| m.id == super::outgoing::row_id(ticket))
        {
            self.current = None;
            self.show_message(None, false);
        }
        let account_id = o.account_id;
        let prefill = unsent_prefill(*o.message);
        let free = self.reader_compose.is_none() && !self.showing_contacts && !self.showing_outbox;
        match o.origin {
            super::outgoing::Origin::Split(answering)
                if free
                    && self
                        .current
                        .iter()
                        .chain(self.current_thread.iter())
                        .any(|m| (m.account_id, m.id) == answering) =>
            {
                self.open_inline_reply(account_id, prefill, Some(answering), sender);
            }
            super::outgoing::Origin::Pane if free => {
                self.open_inline_reply(account_id, prefill, None, sender)
            }
            _ => self.open_compose(account_id, prefill, sender),
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_send(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::SendMessage { from, out } => self.on_send_message(from, out, sender),

            AppMsg::DispatchSend(ticket) => self.dispatch_send(ticket),

            AppMsg::UndoSend(ticket) => self.undo_send(ticket, &sender),

            AppMsg::SendDone { ticket, sent } => {
                self.finish_auto_forward(ticket, sent);
                // A failure has said so already, in the status bar, and the
                // message is in the Outbox or back in a composer. Its
                // original is answered once it goes, not before (#454).
                if sent {
                    self.pending_answers.remove(&ticket);
                } else {
                    self.unmark_answered(ticket);
                }
                let done = {
                    let mut list = self.outgoing.borrow_mut();
                    list.iter()
                        .position(|o| o.ticket == ticket)
                        .map(|i| list.remove(i))
                };
                if done.is_some() {
                    self.push_outbox();
                }
            }

            AppMsg::Sent { account_id } => {
                // No success notification: only send failures are surfaced
                // (via WorkerEvent::Error). Just refresh the folder the copy
                // landed in, if that folder is the open one, which is
                // wherever the account files its copies (#199), not always
                // Sent.
                if let Some(sel) = self.selected.clone() {
                    let copied_to = self.sent_copy_path(account_id);
                    let viewing_sent = sel.account_id == account_id
                        && copied_to.is_some_and(|path| path == sel.path);
                    if viewing_sent {
                        self.send_to(
                            account_id,
                            MailRequest::LoadMessages {
                                folder_id: sel.folder_id,
                                path: sel.path,
                            },
                        );
                    }
                }
                // The reply just sent answers the conversation on screen, and
                // its copy is in the cache by now (the worker lists the copy's
                // folder before it says Sent): ask for the rest of the thread
                // again so the reply joins it here, not after the next visit
                // to the Sent folder (#199).
                self.reload_related(account_id);
            }

            AppMsg::Unsent {
                account_id,
                message,
            } => {
                if self.take_refused_auto_forward(account_id, &message) {
                    return;
                }
                self.drop_provisional(&message.in_reply_to);
                self.compose_unsent(account_id, *message, &sender);
            }
            _ => unreachable!("not routed to on_send"),
        }
    }

    /// A composer sent a message; `from` is that composer's id.
    fn on_send_message(
        &mut self,
        from: u32,
        out: Box<OutgoingMessage>,
        sender: ComponentSender<Self>,
    ) {
        // A reply sent from the split pane becomes its own card in
        // the conversation, instead of sliding away (send_morph.rs):
        // its card is built now, for the composer's close to carry
        // the editor to.
        self.arriving = None;
        if self.reply_joins(from, &out) {
            match self.provisional_message(&out) {
                Some(m) => self.arriving = Some((from, m, motion_wanted())),
                None => {
                    tracing::info!(target: "hylki::morph", "no send morph: the reply's card could not be built")
                }
            }
        }
        let account_id = out.from_account_id;
        // Remember who this went to, now, whatever route it takes
        // from here (straight out, the Outbox, Send Later): the next
        // composer suggests them, and so do the ones already open.
        let recipients: Vec<(String, String)> = [&out.to, &out.cc, &out.bcc]
            .into_iter()
            .flat_map(|f| crate::worker::parse_recipients(f))
            .collect();
        if let Some(c) = self.cache.as_ref() {
            c.record_addresses(&recipients);
        }
        let own: HashSet<String> = self.config.iter().map(|c| c.email.to_lowercase()).collect();
        let fresh: Vec<crate::contacts::Suggestion> = recipients
            .into_iter()
            .filter(|(_, e)| e.contains('@'))
            .map(|(name, email)| crate::contacts::Suggestion {
                name: if name.is_empty() { email.clone() } else { name },
                own: own.contains(&email.to_lowercase()),
                email,
                score: 1,
                ..Default::default()
            })
            .collect();
        if !fresh.is_empty() {
            for host in &self.composers {
                host.controller.emit(ComposeInput::AddSuggestions {
                    account_id,
                    new: fresh.clone(),
                });
            }
            if let Some(rc) = &self.reader_compose {
                rc.controller.emit(ComposeInput::AddSuggestions {
                    account_id,
                    new: fresh.clone(),
                });
            }
        }
        let sent_path = self.sent_copy_path(account_id);
        if out.send_at.is_some() {
            // Send Later has its own way back: the message waits in
            // the Outbox, where it can be edited or dropped.
            self.send_to(
                account_id,
                MailRequest::Send {
                    message: out,
                    sent_path,
                    ticket: 0,
                },
            );
        } else {
            let origin = match self.reader_compose.as_ref().filter(|r| r.id == from) {
                Some(r) if r.window.is_none() => r.answering.map_or(
                    super::outgoing::Origin::Pane,
                    super::outgoing::Origin::Split,
                ),
                _ => super::outgoing::Origin::Window,
            };
            self.queue_send(origin, out, sent_path, &sender);
        }
    }
}
