//! The Tuta handlers for sending, Send Later, and drafts and templates,
//! which Tuta keeps as drafts on the server.

use super::TutaWorker;
use crate::i18n::i18n;
use crate::models::DraftOrigin;
use crate::worker::compose::build::{build_draft, draft_message_id, restore_msgid_case};
use crate::worker::drafts::{draft_not_saved, saving_text};
use crate::worker::helper::send::{exact_forwards, SendOutcome};
use crate::worker::helper::HelperError;
use crate::worker::ids::hash_uid;
use crate::worker::outbox::{drop_superseded_outbox, schedule_send, send_failed, send_uncertain};
use crate::worker::tuta::index::{run, tuta_load_folder};
use crate::worker::tuta::ops::{emit_folder, tuta_ensure_folder, tuta_move_ids};
use crate::worker::tuta::send::{draft_parameters, tuta_list_sent, tuta_send_message};
use crate::worker::tuta::wire::{delete_draft, save_draft};
use crate::worker::{OutgoingMessage, WorkerEvent};

impl<E: Fn(WorkerEvent)> TutaWorker<E> {
    /// Send now. Tuta files the Sent copy itself, so `sent_path` is unused.
    pub(super) async fn on_send(&mut self, mut message: Box<OutgoingMessage>, ticket: u64) {
        let account_id = self.account_id;
        (self.emit)(WorkerEvent::Status(i18n("Sending…")));
        restore_msgid_case(self.cache.as_ref(), &mut message);
        let forwards = exact_forwards(self.cache.as_ref(), &message.forwards);
        let outcome = tuta_send_message(&self.account, &self.state, &message, forwards).await;
        (self.emit)(WorkerEvent::Status(String::new()));
        let cache = self.cache.as_ref();
        let emit = &self.emit;
        match outcome {
            SendOutcome::Sent => {
                drop_superseded_outbox(cache, account_id, &message, emit);
                self.drop_draft_origin(&message).await;
                let cache = self.cache.as_ref();
                tuta_list_sent(&self.account, account_id, cache, &mut self.state, &self.emit)
                    .await;
                (self.emit)(WorkerEvent::Sent);
                (self.emit)(WorkerEvent::SendDone { ticket, sent: true });
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

    /// Send Later (#145): into the Outbox until its time, and the draft it
    /// was written from off the server, as the send itself would take it.
    pub(super) async fn on_send_later(&mut self, mut message: Box<OutgoingMessage>, ticket: u64) {
        let at = message.send_at.unwrap_or_default();
        restore_msgid_case(self.cache.as_ref(), &mut message);
        schedule_send(
            self.cache.as_ref(),
            self.account_id,
            &self.account,
            &message,
            None,
            at,
            &self.emit,
        );
        (self.emit)(WorkerEvent::SendDone {
            ticket,
            sent: false,
        });
        self.drop_draft_origin(&message).await;
    }

    /// The draft of this account `message` was opened from, if any.
    fn own_origin(&self, message: &OutgoingMessage) -> Option<DraftOrigin> {
        message
            .draft_origin
            .clone()
            .filter(|o| o.account_id == self.account_id)
    }

    /// Delete the draft `message` was opened from, now that it has gone
    /// out or into the Outbox, and list its folder again.
    async fn drop_draft_origin(&mut self, message: &OutgoingMessage) {
        let account_id = self.account_id;
        let Some(origin) = self.own_origin(message) else {
            return;
        };
        if let Err(e) = self.delete_draft_at(&origin).await {
            tracing::warn!(
                "[account {account_id}] could not delete the draft a message was sent from: {}",
                e.code
            );
        }
        self.reload(&origin.path).await;
    }

    /// Delete the draft `origin` names.
    async fn delete_draft_at(&mut self, origin: &DraftOrigin) -> Result<(), HelperError> {
        let id = self.resolve(&origin.path, origin.uid).await?;
        let client = self.state.client(&self.account)?;
        run(move || delete_draft(&client, &id)).await?;
        self.state.uids.remove(&origin.uid);
        if let Some(c) = self.cache.as_ref() {
            c.delete_message(self.account_id, &origin.path, origin.uid);
        }
        Ok(())
    }

    /// Save a draft or a template (#340, #360) as a Tuta draft. The draft
    /// it was opened from is updated in place when it is in the folder the
    /// save goes to, and otherwise replaced. A template is a draft moved on
    /// to the Templates folder, made first if the account has none. The
    /// saved row is given the draft's Message-ID, which a Tuta listing
    /// lacks, so the app finds the copy to replace on the next save.
    pub(super) async fn on_save_draft(
        &mut self,
        message: Box<OutgoingMessage>,
        path: String,
        autosave: Option<u32>,
        template: bool,
    ) {
        if autosave.is_none() {
            (self.emit)(WorkerEvent::Status(saving_text(template)));
        }
        let mut message = OutgoingMessage {
            sign: false,
            encrypt: false,
            ..*message
        };
        restore_msgid_case(self.cache.as_ref(), &mut message);
        let saved = self.save_draft_in(&message, &path, template).await;
        (self.emit)(WorkerEvent::Status(String::new()));
        match saved {
            Ok(message_id) => (self.emit)(WorkerEvent::DraftSaved {
                autosave,
                message_id: Some(message_id),
            }),
            Err(why) => {
                tracing::warn!(
                    "[account {}] could not save a {}",
                    self.account_id,
                    if template { "template" } else { "draft" }
                );
                draft_not_saved(autosave, template, why, message, &self.emit)
            }
        }
    }

    /// [`on_save_draft`](Self::on_save_draft)'s work: the saved draft's
    /// Message-ID, or why not.
    async fn save_draft_in(
        &mut self,
        message: &OutgoingMessage,
        path: &str,
        template: bool,
    ) -> Result<String, String> {
        let account_id = self.account_id;
        let email = build_draft(&self.account, message).map_err(|e| e.to_string())?;
        let message_id = draft_message_id(&email);
        let forwards = exact_forwards(self.cache.as_ref(), &message.forwards);
        let parameters =
            draft_parameters(&email.formatted(), &forwards).map_err(|e| e.message())?;
        let drafts = self.state.path_of(crate::models::FolderKind::Drafts);
        if template {
            tuta_ensure_folder(
                &self.account,
                account_id,
                self.cache.as_ref(),
                &mut self.state,
                path,
                &self.emit,
            )
            .await;
        }
        let Some((folder_id, _)) = self.state.folders.get(path).cloned() else {
            return Err(HelperError::new("folder_not_found").message());
        };
        let origin = self.own_origin(message);
        let in_place = match origin.as_ref().filter(|o| o.path == path) {
            Some(o) => self.resolve(&o.path, o.uid).await.ok(),
            None => None,
        };
        let client = self.state.client(&self.account).map_err(|e| e.message())?;
        let draft_id = in_place.clone();
        let mut id = run(move || save_draft(&client, parameters, draft_id.as_deref()))
            .await
            .map_err(|e| e.message())?;
        self.state.uids.insert(hash_uid(&id), id.clone());
        // A new draft is made in Drafts: a template goes on from there.
        if template && in_place.is_none() && drafts.as_deref() != Some(path) {
            let from = drafts.unwrap_or_default();
            let (moved, failed) = tuta_move_ids(
                &self.account,
                account_id,
                &mut self.state,
                &from,
                &[hash_uid(&id)],
                path,
                self.cache.as_ref(),
                &self.emit,
            )
            .await;
            if let Some(e) = failed {
                return Err(e.message());
            }
            id = moved.into_iter().next().unwrap_or(id);
        }
        // The version this one replaces, when it was not updated in place.
        if let Some(o) = origin.filter(|_| in_place.is_none()) {
            if let Err(e) = self.delete_draft_at(&o).await {
                tracing::warn!(
                    "[account {account_id}] could not delete the draft a save replaced: {}",
                    e.code
                );
            }
            if o.path != path {
                self.reload(&o.path).await;
            }
        }
        self.list_saved(folder_id, path, hash_uid(&id), &message_id)
            .await;
        Ok(message_id)
    }

    /// List the folder a draft was saved in, with `message_id` on its row.
    async fn list_saved(&mut self, folder_id: u32, path: &str, uid: u32, message_id: &str) {
        let account_id = self.account_id;
        match tuta_load_folder(
            &self.account,
            account_id,
            folder_id,
            path,
            self.cache.as_ref(),
            &mut self.state,
            &self.emit,
        )
        .await
        {
            Ok(mut messages) => {
                if let Some(m) = messages.iter_mut().find(|m| m.uid == uid) {
                    m.message_id = message_id.to_string();
                    if let Some(c) = self.cache.as_ref() {
                        c.set_threading(account_id, path, uid, message_id, &m.references, "");
                    }
                }
                emit_folder(folder_id, messages, &self.state, &self.emit);
            }
            Err(e) => tracing::info!("[account {account_id}] could not list {path}: {}", e.code),
        }
    }
}
