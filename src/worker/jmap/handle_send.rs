//! The JMAP handlers for sending and drafts.

use super::JmapWorker;
use crate::i18n::i18n;
use crate::worker::compose::build::{build_draft, draft_message_id, restore_msgid_case};
use crate::worker::drafts::{draft_not_saved, saving_text};
use crate::worker::jmap::folders::jmap_ensure_mailbox;
use crate::worker::jmap::index::jmap_load_folder;
use crate::worker::jmap::ops::{jmap_drop_draft_origin, jmap_import_draft, jmap_import_template};
use crate::worker::jmap::send::{
    jmap_flush_outbox, jmap_mark_answered, jmap_send_message, SUBMISSION_REFUSED,
};
use crate::worker::jmap::wire::{jmap_base_url, jmap_session};
use crate::worker::outbox::{drop_superseded_outbox, schedule_send, send_failed};
use crate::worker::{blocking, offline_text, OutgoingMessage, WorkerEvent};

impl<E: Fn(WorkerEvent)> JmapWorker<E> {
    /// Save a draft or a template, replacing the version it was edited from.
    pub(super) async fn on_save_draft(
        &mut self,
        message: Box<OutgoingMessage>,
        folder_id: u32,
        path: String,
        autosave: Option<u32>,
        template: bool,
    ) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        if autosave.is_none() {
            emit(WorkerEvent::Status(saving_text(template)));
        }
        let mut message = OutgoingMessage {
            sign: false,
            encrypt: false,
            ..*message
        };
        restore_msgid_case(cache.as_ref(), &mut message);
        let mut saved = None;
        let mut why = String::new();
        match build_draft(account, &message) {
            Ok(email) => {
                let message_id = draft_message_id(&email);
                let raw = email.formatted();
                if let Some(s) = jmap_session(account, state, emit).await {
                    // The first template of an account with no
                    // Templates folder makes one, as IMAP's APPEND does.
                    if template {
                        jmap_ensure_mailbox(&s, account_id, cache.as_ref(), state, emit, &path)
                            .await;
                    }
                    let mailbox_id = state.folders.get(&path).map(|(_, id)| id.clone());
                    // The listing just made numbers the new folder.
                    let folder_id = state
                        .folders
                        .get(&path)
                        .map(|(id, _)| *id)
                        .unwrap_or(folder_id);
                    match mailbox_id {
                        Some(mailbox_id) => {
                            let sess = s.clone();
                            let import = move || {
                                if template {
                                    jmap_import_template(&sess, &raw, &mailbox_id)
                                } else {
                                    jmap_import_draft(&sess, &raw, &mailbox_id)
                                }
                            };
                            match blocking(import).await {
                                Ok(_) => {
                                    if let Some(o) = message.draft_origin.clone() {
                                        jmap_drop_draft_origin(
                                            &s,
                                            account_id,
                                            &o,
                                            cache.as_ref(),
                                            state,
                                        )
                                        .await;
                                    }
                                    if let Ok(messages) = jmap_load_folder(
                                        &s,
                                        account_id,
                                        folder_id,
                                        &path,
                                        cache.as_ref(),
                                        state,
                                    )
                                    .await
                                    {
                                        emit(WorkerEvent::Messages {
                                            folder_id,
                                            messages,
                                        });
                                    }
                                    saved = Some(message_id);
                                }
                                Err(e) => why = e,
                            }
                        }
                        None => why = i18n("unknown folder"),
                    }
                } else {
                    why = i18n("Could not reach the server");
                }
            }
            Err(e) => why = e.to_string(),
        }
        emit(WorkerEvent::Status(String::new()));
        match saved {
            Some(id) => emit(WorkerEvent::DraftSaved {
                autosave,
                message_id: Some(id),
            }),
            None => draft_not_saved(autosave, template, why, message, emit),
        }
    }

    /// Send Later (#145): into the Outbox until its time.
    pub(super) async fn on_send_later(
        &mut self,
        mut message: Box<OutgoingMessage>,
        sent_path: Option<String>,
        ticket: u64,
    ) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        let at = message.send_at.unwrap_or_default();
        restore_msgid_case(cache.as_ref(), &mut message);
        schedule_send(
            cache.as_ref(),
            account_id,
            account,
            &message,
            sent_path.as_deref(),
            at,
            emit,
        );
        emit(WorkerEvent::SendDone {
            ticket,
            sent: false,
        });
        if let Some(o) = message.draft_origin.clone() {
            if let Some(s) = jmap_session(account, state, emit).await {
                jmap_drop_draft_origin(&s, account_id, &o, cache.as_ref(), state).await;
                if o.account_id == account_id {
                    if let Ok(messages) = jmap_load_folder(
                        &s,
                        account_id,
                        o.folder_id,
                        &o.path,
                        cache.as_ref(),
                        state,
                    )
                    .await
                    {
                        emit(WorkerEvent::Messages {
                            folder_id: o.folder_id,
                            messages,
                        });
                    }
                }
            }
        }
    }

    /// Send now through EmailSubmission.
    pub(super) async fn on_send(
        &mut self,
        mut message: Box<OutgoingMessage>,
        sent_path: Option<String>,
        ticket: u64,
    ) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        emit(WorkerEvent::Status(i18n("Sending…")));
        restore_msgid_case(cache.as_ref(), &mut message);
        let reachable = crate::desktop::power::reaches_url(&jmap_base_url(account));
        let sent = match jmap_session(account, state, emit).await {
            Some(s) if reachable => jmap_send_message(
                &s,
                account_id,
                account,
                &message,
                sent_path.as_deref(),
                cache.as_ref(),
                state,
                emit,
            )
            .await
            .map(|()| s),
            _ if !reachable => Err(offline_text()),
            _ => Err("could not reach the server".to_string()),
        };
        emit(WorkerEvent::Status(String::new()));
        match sent {
            Ok(s) => {
                jmap_mark_answered(
                    &s,
                    state,
                    cache.as_ref(),
                    account_id,
                    &message.in_reply_to,
                    &message.forwards,
                    emit,
                )
                .await;
                if let Some(o) = message.draft_origin.clone() {
                    if o.account_id == account_id {
                        jmap_drop_draft_origin(&s, account_id, &o, cache.as_ref(), state).await;
                        if let Ok(messages) = jmap_load_folder(
                            &s,
                            account_id,
                            o.folder_id,
                            &o.path,
                            cache.as_ref(),
                            state,
                        )
                        .await
                        {
                            emit(WorkerEvent::Messages {
                                folder_id: o.folder_id,
                                messages,
                            });
                        }
                    }
                }
                drop_superseded_outbox(cache.as_ref(), account_id, &message, emit);
                emit(WorkerEvent::Sent);
                emit(WorkerEvent::SendDone { ticket, sent: true });
            }
            Err(e) => {
                send_failed(
                    cache.as_ref(),
                    account_id,
                    account,
                    &message,
                    sent_path.as_deref(),
                    &e,
                    e.starts_with(SUBMISSION_REFUSED),
                    emit,
                );
                emit(WorkerEvent::SendDone {
                    ticket,
                    sent: false,
                });
            }
        }
    }

    /// Try the Outbox now, one message or all of them.
    pub(super) async fn on_flush_outbox(&mut self, id: Option<u32>) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &self.emit;
        if let Some(s) = jmap_session(account, state, emit).await {
            jmap_flush_outbox(&s, cache.as_ref(), account_id, account, id, state, emit).await;
        }
    }
}
