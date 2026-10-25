//! The Graph handlers for sending and drafts.

use super::{name_labels, GraphWorker};
use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::i18n;
use crate::models::FolderKind;
use crate::worker::compose::build::{build_draft, draft_message_id, restore_msgid_case};
use crate::worker::drafts::{draft_not_saved, saving_text};
use crate::worker::graph::folders::graph_ensure_folder;
use crate::worker::graph::http::{graph_post_mime, graph_send_json, graph_token, GRAPH_BASE};
use crate::worker::graph::index::{graph_load_folder, GraphState};
use crate::worker::graph::ops::graph_drop_draft_origin;
use crate::worker::graph::send::{graph_mark_answered, graph_send_message};
use crate::worker::outbox::{drop_superseded_outbox, schedule_send, send_failed};
use crate::worker::{blocking, OutgoingMessage, WorkerEvent};

impl<E: Fn(WorkerEvent)> GraphWorker<E> {
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
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        if autosave.is_none() {
            emit(WorkerEvent::Status(saving_text(template)));
        }
        // A template (#360) is made where every new message is, in
        // Drafts, and moved on: so its folder is first made if the
        // account has none, and found by its Graph id.
        let mut folder_id = folder_id;
        let mut template_gid = None;
        if template {
            if let Some((id, gid)) =
                template_folder(account, account_id, cache.as_ref(), state, &path, emit).await
            {
                folder_id = id;
                template_gid = Some(gid);
            }
        }
        let mut message = OutgoingMessage {
            sign: false,
            encrypt: false,
            ..*message
        };
        restore_msgid_case(cache.as_ref(), &mut message);
        let mut why = String::new();
        let saved = match build_draft(account, &message) {
            Ok(email) => {
                let message_id = draft_message_id(&email);
                let raw = email.formatted();
                match graph_token(account, emit).await {
                    Some(token) => {
                        let r = post_draft(&token, raw, template_gid.as_deref(), template).await;
                        match r {
                            Ok(_) => {
                                // Replace the previous version of this draft.
                                if let Some(o) = &message.draft_origin {
                                    graph_drop_draft_origin(
                                        account,
                                        state,
                                        account_id,
                                        o,
                                        cache.as_ref(),
                                        emit,
                                    )
                                    .await;
                                }
                                if let Ok(messages) = graph_load_folder(
                                    &token,
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
                                Some(message_id)
                            }
                            Err(e) => {
                                why = e.to_string();
                                None
                            }
                        }
                    }
                    None => {
                        why = i18n("Could not reach the server");
                        None
                    }
                }
            }
            Err(e) => {
                why = e.to_string();
                None
            }
        };
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
    pub(super) async fn on_send_later(&mut self, mut message: Box<OutgoingMessage>, ticket: u64) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        let at = message.send_at.unwrap_or_default();
        restore_msgid_case(cache.as_ref(), &mut message);
        schedule_send(
            cache.as_ref(),
            account_id,
            account,
            &message,
            None,
            at,
            emit,
        );
        emit(WorkerEvent::SendDone {
            ticket,
            sent: false,
        });
        if let Some(o) = &message.draft_origin {
            graph_drop_draft_origin(account, state, account_id, o, cache.as_ref(), emit).await;
        }
    }

    /// Send now through sendMail, which files the Sent copy itself.
    pub(super) async fn on_send(&mut self, mut message: Box<OutgoingMessage>, ticket: u64) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let state = &mut self.state;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        emit(WorkerEvent::Status(i18n("Sending…")));
        restore_msgid_case(cache.as_ref(), &mut message);
        match graph_send_message(account, &message, emit).await {
            Ok(()) => {
                emit(WorkerEvent::Status(String::new()));
                graph_mark_answered(
                    account,
                    state,
                    cache.as_ref(),
                    account_id,
                    &message.in_reply_to,
                    &message.forwards,
                    emit,
                )
                .await;
                // If sending an edited draft, remove the obsolete draft.
                if let Some(o) = message.draft_origin.clone() {
                    if o.account_id == account_id {
                        graph_drop_draft_origin(
                            account,
                            state,
                            account_id,
                            &o,
                            cache.as_ref(),
                            emit,
                        )
                        .await;
                        if let Some(token) = graph_token(account, emit).await {
                            if let Ok(messages) = graph_load_folder(
                                &token,
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
                drop_superseded_outbox(cache.as_ref(), account_id, &message, emit);
                // The server files the copy in Sent Items itself; list
                // that folder now so the copy is in the cache and can
                // join the conversation it answers (#199).
                let sent = cache.as_ref().and_then(|c| {
                    c.load_folders(account_id)
                        .into_iter()
                        .find(|f| f.kind == FolderKind::Sent)
                });
                if let (Some(c), Some(sent)) = (cache.as_ref(), sent) {
                    if let Some(token) = graph_token(account, emit).await {
                        if let Ok(messages) = graph_load_folder(
                            &token,
                            account_id,
                            sent.id,
                            &sent.path,
                            Some(c),
                            state,
                        )
                        .await
                        {
                            emit(WorkerEvent::Messages {
                                folder_id: sent.id,
                                messages,
                            });
                        }
                    }
                }
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
        }
    }
}

/// The folder a template is kept in (#360), as (folder id, Graph id): made
/// first if the account has none, then found by name.
async fn template_folder(
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut GraphState,
    path: &str,
    emit: &impl Fn(WorkerEvent),
) -> Option<(u32, String)> {
    graph_ensure_folder(account, account_id, cache, state, path, emit).await;
    state.folders.get(path).map(|(id, gid)| (*id, gid.clone()))
}

/// Post a draft's MIME, which Graph files in Drafts, and move it on to
/// `dest` for a template; the message Graph answers with.
async fn post_draft(
    token: &str,
    raw: Vec<u8>,
    dest: Option<&str>,
    template: bool,
) -> Result<serde_json::Value, String> {
    let t = token.to_string();
    let url = format!("{GRAPH_BASE}/me/messages");
    let r = blocking(move || graph_post_mime(&t, &url, &raw)).await;
    match (r, dest) {
        (Ok(created), Some(dest)) => match created["id"].as_str() {
            Some(gid) => {
                let (t, url) = (
                    token.to_string(),
                    format!("{GRAPH_BASE}/me/messages/{gid}/move"),
                );
                let body = serde_json::json!({ "destinationId": dest });
                blocking(move || graph_send_json(&t, "POST", &url, &body)).await
            }
            None => Err("the new message has no id".to_string()),
        },
        (Ok(_), None) if template => Err(i18n("unknown folder")),
        (r, _) => r,
    }
}
