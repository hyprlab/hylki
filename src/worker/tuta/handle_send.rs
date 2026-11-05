//! The Tuta handlers for sending, Send Later and drafts.

use super::TutaWorker;
use crate::i18n::i18n;
use crate::worker::compose::build::restore_msgid_case;
use crate::worker::drafts::draft_not_saved;
use crate::worker::outbox::{drop_superseded_outbox, schedule_send, send_failed, send_uncertain};
use crate::worker::tuta::send::{tuta_list_sent, tuta_send_message, SendOutcome};
use crate::worker::{OutgoingMessage, WorkerEvent};

impl<E: Fn(WorkerEvent)> TutaWorker<E> {
    /// Send now. Tuta files the Sent copy itself, so `sent_path` is unused.
    pub(super) async fn on_send(&mut self, mut message: Box<OutgoingMessage>, ticket: u64) {
        let account_id = self.account_id;
        let emit = &self.emit;
        emit(WorkerEvent::Status(i18n("Sending…")));
        restore_msgid_case(self.cache.as_ref(), &mut message);
        let outcome = tuta_send_message(&self.account, &self.state, &message).await;
        emit(WorkerEvent::Status(String::new()));
        let cache = self.cache.as_ref();
        match outcome {
            SendOutcome::Sent => {
                // The draft it was opened from stays in Tuta's Drafts: the
                // helper cannot delete, and no draft was saved from Hylki.
                drop_superseded_outbox(cache, account_id, &message, emit);
                tuta_list_sent(&self.account, account_id, cache, &mut self.state, emit).await;
                emit(WorkerEvent::Sent);
                emit(WorkerEvent::SendDone { ticket, sent: true });
            }
            SendOutcome::Failed(e) => {
                send_failed(
                    cache,
                    account_id,
                    &self.account,
                    &message,
                    None,
                    &e,
                    false,
                    emit,
                );
                emit(WorkerEvent::SendDone {
                    ticket,
                    sent: false,
                });
            }
            SendOutcome::Refused(e) => {
                send_failed(
                    cache,
                    account_id,
                    &self.account,
                    &message,
                    None,
                    &e,
                    true,
                    emit,
                );
                emit(WorkerEvent::SendDone {
                    ticket,
                    sent: false,
                });
            }
            SendOutcome::Uncertain => {
                send_uncertain(cache, account_id, &self.account, &message, emit);
                emit(WorkerEvent::SendDone {
                    ticket,
                    sent: false,
                });
            }
        }
    }

    /// Send Later (#145): into the Outbox until its time.
    pub(super) fn on_send_later(&mut self, mut message: Box<OutgoingMessage>, ticket: u64) {
        let emit = &self.emit;
        let at = message.send_at.unwrap_or_default();
        restore_msgid_case(self.cache.as_ref(), &mut message);
        schedule_send(
            self.cache.as_ref(),
            self.account_id,
            &self.account,
            &message,
            None,
            at,
            emit,
        );
        emit(WorkerEvent::SendDone {
            ticket,
            sent: false,
        });
    }

    /// Drafts and templates: the helper cannot store a draft on the server
    /// yet, so the message is handed back (an automatic save just tries
    /// again later), never dropped.
    pub(super) fn on_save_draft(
        &mut self,
        message: Box<OutgoingMessage>,
        autosave: Option<u32>,
        template: bool,
    ) {
        draft_not_saved(
            autosave,
            template,
            i18n("Tuta accounts can't save drafts on the server from Hylki yet"),
            *message,
            &self.emit,
        );
    }
}
