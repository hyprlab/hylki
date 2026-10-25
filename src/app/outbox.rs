//! The Outbox: queued and scheduled mail, shown as a folder of its own.

use relm4::prelude::*;

use crate::ui::compose::ComposePrefill;
use crate::ui::message_list::{MessageListInput, SearchScope};
use crate::ui::sidebar::SidebarInput;
use crate::worker::MailRequest;

use super::model::AppModel;
use super::msg::AppMsg;
use super::prefill::{first_recipient, stage_attachments, unsent_prefill};

impl AppModel {
    /// Push the merged queue to the Outbox view and the sidebar's badge. Oldest
    /// first, which is the order the workers send in.
    pub(super) fn push_outbox(&self) {
        let items = self.outbox_items();
        self.sidebars_emit(SidebarInput::SetOutboxCount(items.len() as u32));
        if self.showing_outbox {
            self.message_list.emit(MessageListInput::SetMessages {
                messages: items.iter().map(|i| i.as_message()).collect(),
            });
        }
    }

    /// Every account's queue merged, oldest first — the order they will send in.
    fn outbox_items(&self) -> Vec<crate::models::OutboxItem> {
        let mut items: Vec<crate::models::OutboxItem> = self
            .outbox_by_account
            .values()
            .flatten()
            .cloned()
            .chain(self.outgoing.borrow().iter().map(|o| o.as_outbox_item()))
            .collect();
        items.sort_by_key(|i| (i.queued_at, i.id));
        items
    }

    /// The queued message behind a list row, if the Outbox is what's shown.
    pub(super) fn outbox_item(
        &self,
        account_id: u32,
        id: u32,
    ) -> Option<crate::models::OutboxItem> {
        if !self.showing_outbox {
            return None;
        }
        if let Some(o) = self
            .outgoing
            .borrow()
            .iter()
            .find(|o| super::outgoing::row_id(o.ticket) == id)
        {
            return Some(o.as_outbox_item());
        }
        self.outbox_by_account
            .get(&account_id)?
            .iter()
            .find(|i| i.id == id)
            .cloned()
    }

    /// The ticket of the message on its way out behind an Outbox row, while
    /// Undo can still take it back.
    fn outgoing_ticket(&self, row: u32) -> Option<u64> {
        self.outgoing
            .borrow()
            .iter()
            .find(|o| o.held && super::outgoing::row_id(o.ticket) == row)
            .map(|o| o.ticket)
    }

    /// Delete on an Outbox row. A message still waiting out its delay goes
    /// back to its composer instead, where discarding it is a choice made
    /// with the message in view; one already with its worker is past
    /// stopping.
    pub(super) fn drop_outbox_row(
        &mut self,
        account_id: u32,
        id: u32,
        sender: &ComponentSender<Self>,
    ) {
        if self
            .outgoing
            .borrow()
            .iter()
            .any(|o| super::outgoing::row_id(o.ticket) == id)
        {
            if let Some(ticket) = self.outgoing_ticket(id) {
                self.undo_send(ticket, sender);
            }
            return;
        }
        self.send_to(account_id, MailRequest::DeleteOutbox { id });
    }

    /// Render a queued message in the reader. Its bytes are stored with it, so
    /// this is a local render — the same one the worker would produce.
    pub(super) fn show_outbox_message(&mut self, item: &crate::models::OutboxItem) {
        let mut message = item.as_message();
        // A message on its way out has no bytes yet (#434): it shows as
        // its card in Sent will.
        let outgoing = self
            .outgoing
            .borrow()
            .iter()
            .find(|o| super::outgoing::row_id(o.ticket) == item.id)
            .map(|o| {
                let body = crate::worker::sent_body_preview(&o.account, &o.message)
                    .unwrap_or_else(|| crate::worker::wrap_plain(&o.message.body));
                (body, o.message.attachments.is_empty())
            });
        if let Some((body, no_files)) = outgoing {
            message.body = crate::desktop::legacy::current_markup(body);
            message.date = item.status.clone();
            message.has_attachment = !no_files;
            self.attachments.clear();
            self.attachments_loading = false;
            self.sync_attachment_drawer();
            self.current = Some(message.clone());
            self.current_thread.clear();
            self.show_message(Some(message), false);
            return;
        }
        message.body =
            crate::desktop::legacy::current_markup(crate::worker::extract_body(&item.raw));
        message.date = crate::models::OutboxItem::waiting_label(item.queued_at);
        self.attachments = crate::worker::extract_attachments_of(&item.raw);
        self.attachments_loading = false;
        self.sync_attachment_drawer();
        self.current = Some(message.clone());
        self.current_thread.clear();
        self.show_message(Some(message), false);
    }

    /// Open a queued Outbox message in the composer.
    ///
    /// The queued copy stays put until the edited version is handed back to the
    /// worker: an editor that is closed again must not have destroyed the only
    /// copy of the message. Attachments are written to a private temp directory,
    /// because the composer attaches files by path and the originals are long
    /// gone (under Flatpak the portal's paths expire).
    fn compose_from_outbox(&mut self, account_id: u32, id: u32, sender: &ComponentSender<Self>) {
        let Some(item) = self
            .outbox_by_account
            .get(&account_id)
            .and_then(|items| items.iter().find(|i| i.id == id))
            .cloned()
        else {
            return;
        };
        let editable = crate::worker::editable_from_raw(&item.raw, &item.rcpts);

        let attachments = stage_attachments(
            &format!("hylki-outbox-{account_id}-{id}"),
            &editable.attachments,
        );

        let prefill = ComposePrefill {
            to: editable.to,
            cc: editable.cc,
            bcc: editable.bcc,
            subject: editable.subject,
            body_html: editable.body_html,
            attachments,
            // Re-editing a queued message: it keeps whatever thread it was
            // already part of (carried in the raw MIME it was built from).
            in_reply_to: String::new(),
            references: String::new(),
            forwards: item.forwards.clone(),
            draft_origin: None,
            encrypt: false,
            outbox_origin: Some(id),
            reply_addressed_to: String::new(),
            from_address: first_recipient(&editable.from)
                .map(|(_, addr)| addr)
                .unwrap_or_default(),
            custom_from: editable.from,
            send_at: item.send_at,
            cloud_uploads: Vec::new(),
            inline_files: Vec::new(),
            block_remote_images: false,
            resumed: false,
            no_signature: false,
            template: false,
            original_plain: false,
        };
        // The Outbox stays the folder on screen: its list is still what's listed,
        // so its toolbar has to stay too. Leaving it would strand the user in a
        // reader offering Reply and Forward for a message that hasn't been sent.
        self.open_compose(account_id, prefill, sender);
    }

    /// Open a message the worker could not send, queue or save (#340). Its
    /// composer closed when Send was pressed, so without this the message is
    /// gone.
    pub(super) fn compose_unsent(
        &mut self,
        account_id: u32,
        m: crate::worker::OutgoingMessage,
        sender: &ComponentSender<Self>,
    ) {
        self.open_compose(account_id, unsent_prefill(m), sender);
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_outbox(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::ShowOutbox => {
                self.close_sidebar_peek();
                self.mirror_selection(crate::ui::sidebar::Sel::Outbox);
                // Treated as a folder: same list, same reader, nothing swapped
                // out from under the user. `selected` stays None so no sync or
                // server request is ever aimed at it.
                self.showing_outbox = true;
                self.leave_gallery();
                self.showing_contacts = false;
                self.unified = false;
                self.tag_view = None;
                self.smart_view = None;
                self.selected = None;
                self.current = None;
                self.current_thread.clear();
                self.attachments.clear();
                self.attachments_loading = false;
                self.sync_attachment_drawer();
                self.message_list.emit(MessageListInput::SetSelected(None));
                self.message_list.emit(MessageListInput::ResetPaging);
                self.message_list
                    .emit(MessageListInput::SetScope(SearchScope::ThisFolder));
                self.message_list
                    .emit(MessageListInput::SetColorize(self.accounts.len() > 1));
                self.show_message(None, false);
                self.push_outbox();
            }

            AppMsg::OutboxItems { account_id, items } => {
                self.outbox_by_account.insert(account_id, items);
                self.push_outbox();
            }

            AppMsg::EditCurrentOutbox => {
                if let Some(m) = self.current.clone() {
                    match self.outgoing_ticket(m.id) {
                        Some(ticket) => self.undo_send(ticket, &sender),
                        None => self.compose_from_outbox(m.account_id, m.id, &sender),
                    }
                }
            }

            AppMsg::SendCurrentOutbox => {
                if let Some(m) = self.current.clone() {
                    let in_flight = self
                        .outgoing
                        .borrow()
                        .iter()
                        .any(|o| super::outgoing::row_id(o.ticket) == m.id);
                    match self.outgoing_ticket(m.id) {
                        Some(ticket) => self.dispatch_send(ticket),
                        None if in_flight => {}
                        None => {
                            self.send_to(m.account_id, MailRequest::FlushOutbox { id: Some(m.id) })
                        }
                    }
                }
            }

            AppMsg::SendDueScheduled => {
                // Its one try would be spent on no network; the time is
                // still past when the network is back (#442).
                if !crate::desktop::power::reaches_internet() {
                    return;
                }
                let now = crate::datefmt::now();
                let due: Vec<(u32, u32)> = self
                    .outbox_by_account
                    .iter()
                    .flat_map(|(account_id, items)| {
                        items
                            .iter()
                            .filter(|i| i.send_at.is_some_and(|t| t <= now) && i.attempts <= 1)
                            .map(move |i| (*account_id, i.id))
                    })
                    .collect();
                for (account_id, id) in due {
                    self.send_to(account_id, MailRequest::FlushOutbox { id: Some(id) });
                }
            }

            AppMsg::RetryAllOutbox => {
                let held: Vec<u64> = self
                    .outgoing
                    .borrow()
                    .iter()
                    .filter(|o| o.held)
                    .map(|o| o.ticket)
                    .collect();
                for ticket in held {
                    self.dispatch_send(ticket);
                }
                let accounts: Vec<u32> = self
                    .outbox_by_account
                    .iter()
                    .filter(|(_, items)| !items.is_empty())
                    .map(|(id, _)| *id)
                    .collect();
                for account_id in accounts {
                    self.send_to(account_id, MailRequest::FlushOutbox { id: None });
                }
            }
            _ => unreachable!("not routed to on_outbox"),
        }
    }

    /// Send Later (#145): the app's clock. Every half minute, any queued
    /// message whose time has come is flushed by id, which sends it even
    /// though the ordinary flush leaves scheduled mail alone. One try per
    /// scheduling: a failure shows in the Outbox with Send Now to retry,
    /// rather than a loud attempt every tick.
    pub(super) fn start_outbox_clocks(&self, sender: &ComponentSender<Self>) {
        {
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local(30, move || {
                s.input(AppMsg::SendDueScheduled);
                // Snoozed mail (#527) keeps the same clock.
                s.input(AppMsg::WakeSnoozed);
                gtk::glib::ControlFlow::Continue
            });
        }
        {
            // Settings, built hidden a moment after startup (see PrewarmSettings).
            let s = sender.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
                s.input(AppMsg::PrewarmSettings);
            });
        }
    }
}
