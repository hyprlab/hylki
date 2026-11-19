//! Sending: the undo-send delay, handing a message to its worker, and the
//! answered marks shown before the server has them.

use std::collections::HashSet;

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::i18n;
use crate::models::Message;
use crate::ui::compose::ComposeInput;
use crate::ui::keymap::{focus_in_compose, focus_is_text};
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
        importance: Default::default(),
        receipt_report: None,
        merged: false,
        peer_keys: Vec::new(),
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
        crate::desktop::send_log::note_message(
            &account.email,
            &out,
            &if delay > 0 {
                format!("Send pressed; Undo on offer for {delay} s")
            } else {
                "Send pressed".to_string()
            },
        );
        let toast = adw::Toast::builder()
            .priority(adw::ToastPriority::High)
            .timeout(0)
            .build();
        self.journal_send(
            ticket,
            account_id,
            &account,
            &out,
            sent_path.as_deref(),
            sender,
        );
        let o = super::outgoing::Outgoing {
            ticket,
            account_id,
            account,
            message: out,
            sent_path,
            origin,
            held: delay > 0,
            journal: super::outgoing::Journal::Writing,
            // With nothing to undo, it goes as soon as its copy is written.
            due: delay == 0,
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
        }
        // With nothing to undo, there is no toast.
        self.outgoing.borrow_mut().push(o);
    }

    /// Write the message to the Outbox (#678) off the main thread; the
    /// answer comes back as `Journaled`. Until then its worker does not have
    /// it, so a flush of the Outbox and the worker never both send it.
    pub(super) fn journal_send(
        &self,
        ticket: u64,
        account_id: u32,
        account: &crate::config::AccountConfig,
        message: &OutgoingMessage,
        sent_path: Option<&str>,
        sender: &ComponentSender<Self>,
    ) {
        let (account, message, sent_path) = (
            account.clone(),
            message.clone(),
            sent_path.map(str::to_string),
        );
        let s = sender.clone();
        gtk::glib::spawn_future_local(async move {
            let id = gtk::gio::spawn_blocking(move || {
                crate::worker::send_journal::write(
                    account_id,
                    &account,
                    &message,
                    sent_path.as_deref(),
                )
            })
            .await
            .ok()
            .flatten();
            s.input(AppMsg::Journaled { ticket, id });
        });
    }

    /// A message's copy is in the Outbox, or could not be written. If its
    /// delay is over meanwhile, it goes to its worker now.
    fn journaled(&mut self, ticket: u64, id: Option<u32>) {
        let due = {
            let mut list = self.outgoing.borrow_mut();
            match list.iter_mut().find(|o| o.ticket == ticket) {
                Some(o) => {
                    match id {
                        Some(id) => {
                            o.journal = super::outgoing::Journal::Kept(id);
                            // The version it was edited from is gone: this
                            // copy is the one a send or a failure settles.
                            o.message.outbox_origin = Some(id);
                        }
                        None => o.journal = super::outgoing::Journal::NotKept,
                    }
                    Some(o.due)
                }
                None => None,
            }
        };
        match due {
            Some(true) => self.hand_to_worker(ticket),
            Some(false) => {}
            None => {
                if !self.merge_journaled(ticket, id) {
                    // Undone while it was written.
                    self.drop_journal(id);
                }
            }
        }
    }

    /// Take a message's copy out of the Outbox: Undo took it back.
    pub(super) fn drop_journal(&self, id: Option<u32>) {
        if let Some(id) = id {
            if let Some(c) = self.cache.as_ref() {
                c.delete_outbox(id);
            }
            crate::worker::send_journal::release(id);
        }
    }

    /// The message goes to its worker.
    fn hand_to_worker(&self, ticket: u64) {
        let request = {
            let list = self.outgoing.borrow();
            let Some(o) = list.iter().find(|o| o.ticket == ticket) else {
                return;
            };
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
        let written = {
            let mut list = self.outgoing.borrow_mut();
            let Some(o) = list.iter_mut().find(|o| o.ticket == ticket && o.held) else {
                return;
            };
            o.held = false;
            o.toast.dismiss();
            o.due = o.journal == super::outgoing::Journal::Writing;
            !o.due
        };
        if written {
            self.hand_to_worker(ticket);
        }
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
        let Some(mut o) = taken else { return };
        crate::desktop::send_log::note_message(
            &o.account.email,
            &o.message,
            "Undo Send: back in the composer",
        );
        o.toast.dismiss();
        // Its copy leaves the Outbox, now or once written (`journaled`
        // finds no message for it). The version it was edited from went
        // when the copy was written, so the composer is all there is.
        match o.journal {
            super::outgoing::Journal::Kept(id) => {
                self.drop_journal(Some(id));
                o.message.outbox_origin = None;
            }
            super::outgoing::Journal::Writing => o.message.outbox_origin = None,
            super::outgoing::Journal::NotKept => {}
        }
        self.unmark_answered(ticket);
        self.send_morph.finish();
        self.drop_provisional(&o.message.in_reply_to);
        let account_id = o.account_id;
        let prefill = unsent_prefill(*o.message, &self.email_of_config(account_id));
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

    /// Escape takes back the message waiting out its Undo Send delay, the
    /// latest when several wait (#607). In the capture phase, because the
    /// toast overlay binds Escape itself: it would close the notice first
    /// and leave the message to go. A text field, a composer and a dialog
    /// keep the key.
    pub(super) fn install_undo_send_key(
        &self,
        root: &adw::ApplicationWindow,
        sender: &ComponentSender<Self>,
    ) {
        use gtk::gdk::ModifierType as M;
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let outgoing = self.outgoing.clone();
        let window = root.clone();
        let s = sender.clone();
        keys.connect_key_pressed(move |_, keyval, _, state| {
            let held = outgoing
                .borrow()
                .iter()
                .filter(|o| o.held)
                .map(|o| o.ticket)
                .max();
            let Some(ticket) = held.filter(|_| {
                keyval == gtk::gdk::Key::Escape
                    && !state
                        .intersects(M::CONTROL_MASK | M::SHIFT_MASK | M::ALT_MASK | M::SUPER_MASK)
                    && window.visible_dialog().is_none()
                    && !focus_is_text(&window)
                    && !focus_in_compose(&window)
            }) else {
                return gtk::glib::Propagation::Proceed;
            };
            s.input(AppMsg::UndoSend(ticket));
            gtk::glib::Propagation::Stop
        });
        root.add_controller(keys);
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_send(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::SendMessage { from, out } => self.on_send_message(from, out, sender),

            AppMsg::SendMerged { messages, interval } => {
                self.send_merged(messages, interval, &sender)
            }

            AppMsg::MergeTick(id) => self.on_merge_tick(id, &sender),

            AppMsg::StopMerge(id) => self.stop_merge(id, &sender),

            AppMsg::DispatchSend(ticket) => self.dispatch_send(ticket),

            AppMsg::Journaled { ticket, id } => self.journaled(ticket, id),

            AppMsg::UndoSend(ticket) => self.undo_send(ticket, &sender),

            AppMsg::SendDone { ticket, sent } => {
                if self.merged_done(ticket, sent, &sender) {
                    return;
                }
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
                if let Some(o) = done {
                    if let super::outgoing::Journal::Kept(id) = o.journal {
                        crate::worker::send_journal::release(id);
                    }
                    if sent {
                        crate::desktop::send_log::note_message(
                            &o.account.email,
                            &o.message,
                            "sent",
                        );
                    }
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
                sent: 1,
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
        let sent_path = self.sent_copy_path_as(account_id, out.from_alias.as_deref());
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
