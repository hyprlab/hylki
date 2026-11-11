//! The handlers for sending: Send Later, sending now (with the Sent copy
//! and the draft it replaces), the Outbox, and saving drafts.

use super::{ensured, After, Emit, ImapWorker};
use crate::i18n::{i18n, i18n_f};
use crate::worker::compose::build::{build_draft, draft_message_id, restore_msgid_case};
use crate::worker::drafts::{draft_not_saved, saving_text};
use crate::worker::imap::folders::refresh_folders;
use crate::worker::imap::retry::load_messages_retry;
use crate::worker::imap::send::{
    append_draft, delete_draft, file_sent_copy, flush_outbox, index_sent_copy, mark_answered,
};
use crate::worker::outbox::{drop_superseded_outbox, schedule_send, send_failed};
use crate::worker::smtp::{send_smtp, smtp_refused};
use crate::worker::{reply_copy_path, OutgoingMessage, WorkerEvent};

impl<E: Emit> ImapWorker<E> {
    /// Send Later (#145): into the Outbox until its time.
    pub(super) async fn on_send_later(
        &mut self,
        mut message: Box<OutgoingMessage>,
        sent_path: Option<String>,
        ticket: u64,
    ) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let use_envelope = &mut self.use_envelope;
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
        // The draft it was opened from is superseded by the queued copy.
        if let Some(o) = message.draft_origin.clone() {
            if o.account_id == account_id {
                if let Some(sess) = session.as_mut() {
                    let _ = delete_draft(sess, &o.path, o.uid).await;
                }
                if let Some(c) = cache.as_ref() {
                    c.delete_message(account_id, &o.path, o.uid);
                }
                if let Ok(messages) = load_messages_retry(
                    account_id,
                    session,
                    account,
                    o.folder_id,
                    &o.path,
                    use_envelope,
                    cache.as_ref(),
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
        After::Done
    }

    /// Send now: SMTP, then the Sent copy, the answered mark and the draft it replaces.
    pub(super) async fn on_send(
        &mut self,
        mut message: Box<OutgoingMessage>,
        sent_path: Option<String>,
        ticket: u64,
    ) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let use_envelope = &mut self.use_envelope;
        emit(WorkerEvent::Status(i18n("Sending…")));
        restore_msgid_case(cache.as_ref(), &mut message);
        match send_smtp(account, &message).await {
            Ok(raw) => {
                emit(WorkerEvent::Status(String::new()));
                mark_answered(
                    session,
                    cache.as_ref(),
                    account_id,
                    &message.in_reply_to,
                    &message.forwards,
                    emit,
                )
                .await;
                // Save a copy to the Sent folder, or beside the
                // original for a reply (#468); sending still counts
                // as success even if this part fails.
                let reply = reply_copy_path(
                    account,
                    account_id,
                    cache.as_ref(),
                    &message.in_reply_to,
                    message.from_alias.as_deref(),
                );
                let sess = ensured(session);
                match file_sent_copy(sess, reply.as_deref(), sent_path.as_deref(), &raw).await {
                    Ok(Some(path)) => {
                        index_sent_copy(
                            account_id,
                            session,
                            account,
                            &path,
                            use_envelope,
                            cache.as_ref(),
                            emit,
                        )
                        .await;
                    }
                    Ok(None) => {}
                    Err(e) => emit(WorkerEvent::error(i18n_f(
                        "Message sent, but saving to Sent failed: {e}",
                        &[("e", &(e).to_string())],
                    ))),
                }
                // If sending an edited draft (from this account), remove the
                // now-obsolete draft and refresh the Drafts folder.
                if let Some(o) = message.draft_origin.clone() {
                    if o.account_id == account_id {
                        {
                            // Not `ensured`: index_sent_copy above can leave
                            // the session `None`, and this then panics (a
                            // suspected bug kept as it is, B1 in the notes).
                            let sess = session.as_mut().unwrap();
                            let _ = delete_draft(sess, &o.path, o.uid).await;
                        }
                        if let Some(c) = cache.as_ref() {
                            c.delete_message(account_id, &o.path, o.uid);
                        }
                        if let Ok(messages) = load_messages_retry(
                            account_id,
                            session,
                            account,
                            o.folder_id,
                            &o.path,
                            use_envelope,
                            cache.as_ref(),
                        )
                        .await
                        {
                            if let Some(c) = cache.as_ref() {
                                c.upsert_messages(account_id, &o.path, &messages);
                            }
                            emit(WorkerEvent::Messages {
                                folder_id: o.folder_id,
                                messages,
                            });
                        }
                    }
                }
                // This version replaces the queued one it was edited from.
                drop_superseded_outbox(cache.as_ref(), account_id, &message, emit);
                emit(WorkerEvent::Sent);
                emit(WorkerEvent::SendDone { ticket, sent: true });
            }
            Err(e) => {
                emit(WorkerEvent::Status(String::new()));
                send_failed(
                    cache.as_ref(),
                    account_id,
                    account,
                    &message,
                    sent_path.as_deref(),
                    &e.to_string(),
                    smtp_refused(&e),
                    emit,
                );
                emit(WorkerEvent::SendDone {
                    ticket,
                    sent: false,
                });
            }
        }
        After::Done
    }

    /// Try the Outbox now, one message or all of them.
    pub(super) async fn on_flush_outbox(&mut self, id: Option<u32>) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let use_envelope = &mut self.use_envelope;
        flush_outbox(
            cache.as_ref(),
            account_id,
            account,
            id,
            session,
            use_envelope,
            emit,
            true,
        )
        .await;
        After::Done
    }

    /// Save a draft or a template, replacing the version it was edited from.
    pub(super) async fn on_save_draft(
        &mut self,
        message: Box<OutgoingMessage>,
        folder_id: u32,
        path: String,
        autosave: Option<u32>,
        template: bool,
    ) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let use_envelope = &mut self.use_envelope;
        let mut lost = false;
        if autosave.is_none() {
            emit(WorkerEvent::Status(saving_text(template)));
        }
        // A draft is kept as written: signing and encrypting happen
        // at send time (#133).
        let mut message = OutgoingMessage {
            sign: false,
            encrypt: false,
            ..*message
        };
        restore_msgid_case(cache.as_ref(), &mut message);
        match build_draft(account, &message) {
            Ok(email) => {
                let message_id = draft_message_id(&email);
                let raw = email.formatted();
                let append_res = {
                    let sess = ensured(session);
                    let r = append_draft(sess, &path, &raw, template).await;
                    // Replace the previous version of this draft (same account).
                    if r.is_ok() {
                        if let Some(o) = &message.draft_origin {
                            if o.account_id == account_id {
                                let _ = delete_draft(sess, &o.path, o.uid).await;
                            }
                        }
                    }
                    r
                };
                match append_res {
                    Ok(()) => {
                        if let Some(o) = &message.draft_origin {
                            if o.account_id == account_id {
                                if let Some(c) = cache.as_ref() {
                                    c.delete_message(account_id, &o.path, o.uid);
                                }
                            }
                        }
                        // Reload Drafts so the saved draft appears.
                        if let Ok(messages) = load_messages_retry(
                            account_id,
                            session,
                            account,
                            folder_id,
                            &path,
                            use_envelope,
                            cache.as_ref(),
                        )
                        .await
                        {
                            if let Some(c) = cache.as_ref() {
                                c.upsert_messages(account_id, &path, &messages);
                            }
                            emit(WorkerEvent::Messages {
                                folder_id,
                                messages,
                            });
                        }
                        // Surface a newly-created Drafts folder in the sidebar.
                        if let Some(sess) = session.as_mut() {
                            refresh_folders(account_id, account, sess, cache.as_ref(), emit).await;
                        }
                        // Saved as a draft instead of sent: the queued
                        // copy it was edited from is now superseded.
                        // A template is a copy, and supersedes nothing.
                        if !template {
                            drop_superseded_outbox(cache.as_ref(), account_id, &message, emit);
                        }
                        emit(WorkerEvent::Status(String::new()));
                        emit(WorkerEvent::DraftSaved {
                            autosave,
                            message_id: Some(message_id),
                            template,
                        });
                    }
                    Err(e) => {
                        emit(WorkerEvent::Status(String::new()));
                        draft_not_saved(autosave, template, e.to_string(), message.clone(), emit);
                        lost = true;
                    }
                }
            }
            Err(e) => {
                emit(WorkerEvent::Status(String::new()));
                draft_not_saved(autosave, template, e.to_string(), message, emit);
            }
        }
        After::lost_if(lost)
    }
}
