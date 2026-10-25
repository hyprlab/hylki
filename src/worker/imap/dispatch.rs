//! Serving one request: the cache first, then a live session, then the
//! handler for its kind, then the clean-up its outcome asks for.

use super::connect::connect_and_list;
use super::{After, Emit, ImapWorker};
use crate::i18n::i18n;
use crate::worker::compose::build::restore_msgid_case;
use crate::worker::lane::{related_from_cache, serve_cached_body, thread_summaries_with_members};
use crate::worker::outbox::{delete_queued, emit_outbox, schedule_send, send_failed};
use crate::worker::{offline_text, MailRequest, WorkerEvent};

impl<E: Emit> ImapWorker<E> {
    /// One request, start to finish. The handlers report how the connection
    /// fared; the selection reset and the reconnect after a lost connection
    /// happen here, and not at all for a request that was put back or
    /// answered without the server.
    pub(super) async fn handle(&mut self, req: MailRequest) {
        if matches!(req, MailRequest::Reconnect) {
            self.session = connect_and_list(
                self.account_id,
                &self.account,
                self.cache.as_ref(),
                &self.emit,
            )
            .await;
            self.sweep_due = true;
            return;
        }
        // Answered offline too: a move that could not run has still settled.
        if let MailRequest::Settle { path, uids } = req {
            (self.emit)(WorkerEvent::MovesSettled { path, uids });
            return;
        }

        let Some(pending_bodies) = self.serve_from_cache(&req) else {
            return;
        };
        let Some(req) = self.ensure_session(req).await else {
            return;
        };

        // A background resync keeps quiet and leaves IDLE/watching alone.
        let background = matches!(req, MailRequest::SyncFolder { .. });
        let invalidates_selection = matches!(
            &req,
            MailRequest::RenameFolder { .. }
                | MailRequest::DeleteFolder { .. }
                | MailRequest::DeleteAttachment { .. }
        );
        if invalidates_selection {
            self.forget_selection();
        }

        let after = self.dispatch(req, background, pending_bodies).await;
        if after == After::Skip {
            return;
        }
        if invalidates_selection {
            self.forget_selection();
        }
        // On a connection-shaped failure we drop the session to force a reconnect.
        if after == After::Lost {
            self.session = None;
        }
    }

    fn forget_selection(&mut self) {
        if let Some(sess) = self.session.as_mut() {
            sess.as_mut().set_selected_folder(None);
        }
    }

    /// Serve from cache first so mail appears instantly (and offline).
    /// `None` when the cache answered the whole request; otherwise the
    /// bodies of a `LoadBodies` batch that it could not answer, so the
    /// network handler fetches only those.
    fn serve_from_cache(&self, req: &MailRequest) -> Option<Vec<(u32, u32)>> {
        let account_id = self.account_id;
        let cache = &self.cache;
        let emit = &self.emit;
        let mut pending_bodies: Vec<(u32, u32)> = Vec::new();
        match req {
            MailRequest::LoadMessages { folder_id, path } => {
                if let Some(c) = cache.as_ref() {
                    let cached = c.load_messages(account_id, path, *folder_id);
                    if !cached.is_empty() {
                        emit(WorkerEvent::Messages {
                            folder_id: *folder_id,
                            messages: cached,
                        });
                    }
                }
            }
            MailRequest::LoadRelated { message_id, ids } => {
                let messages = cache
                    .as_ref()
                    .map(|c| related_from_cache(c, account_id, ids))
                    .unwrap_or_default();
                emit(WorkerEvent::Related {
                    message_id: *message_id,
                    messages,
                });
                return None; // cache-only, never hits the network
            }
            MailRequest::LoadThreadSummaries { groups } => {
                let summaries = cache
                    .as_ref()
                    .map(|c| thread_summaries_with_members(c, account_id, groups))
                    .unwrap_or_default();
                emit(WorkerEvent::ThreadSummaries { summaries });
                return None; // cache-only, never hits the network
            }
            MailRequest::LoadBody {
                message_id,
                path,
                uid,
            } => {
                if cache.as_ref().is_some_and(|c| {
                    serve_cached_body(c, account_id, path, *uid, *message_id, emit)
                }) {
                    return None; // already cached; no network needed
                }
            }
            MailRequest::LoadBodies { items, path } => {
                // Serve every member the cache already has, and leave the rest
                // for one fetch. A conversation reopened after its bodies were
                // cached costs no network at all.
                for (message_id, uid) in items {
                    if !cache.as_ref().is_some_and(|c| {
                        serve_cached_body(c, account_id, path, *uid, *message_id, emit)
                    }) {
                        pending_bodies.push((*message_id, *uid));
                    }
                }
                if pending_bodies.is_empty() {
                    return None; // whole conversation was cached; no network needed
                }
            }
            MailRequest::LoadAttachments {
                message_id,
                path,
                uid,
                download,
            } => {
                if let Some(c) = cache.as_ref() {
                    let items = c.load_attachments(account_id, path, *uid);
                    if !items.is_empty() {
                        emit(WorkerEvent::Attachments {
                            path: path.to_string(),
                            message_id: *message_id,
                            items,
                        });
                        return None; // already cached; no network needed
                    }
                }
                // Not cached and the user hasn't asked to download — tell the UI
                // so it can offer a "Load attachments" button instead of fetching.
                if !download {
                    emit(WorkerEvent::AttachmentsPending {
                        path: path.to_string(),
                        message_id: *message_id,
                    });
                    return None;
                }
            }
            _ => {}
        }
        Some(pending_bodies)
    }

    /// Everything past the cache needs a live session. One left from before
    /// the network went is dead, and a request on it would wait out the
    /// socket's timeout (#442). `None` when no session could be had: the
    /// request has then been answered as well as it can be offline.
    async fn ensure_session(&mut self, req: MailRequest) -> Option<MailRequest> {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let emit = &self.emit;
        let session = &mut self.session;
        if session.is_some() && !crate::desktop::power::reaches(&account.imap_host) {
            *session = None;
        }
        if session.is_some() {
            return Some(req);
        }
        *session = connect_and_list(account_id, account, cache.as_ref(), emit).await;
        if session.is_some() {
            return Some(req);
        }
        // Still offline; cached data (if any) was already sent. Close
        // the folder's index anyway (#218): "we tried and could not
        // reach the server" has to end the list's "Loading more…"
        // spinner the same way success does, or an unreachable account
        // is indistinguishable from one that is still streaming — and
        // in a unified view one such account pins the spinner for
        // every account's mail.
        // Snoozed mail waits for the next try (#527).
        if let MailRequest::Unsnooze { message_ids, .. } = &req {
            emit(WorkerEvent::Unsnoozed {
                message_ids: message_ids.clone(),
                done: false,
            });
        }
        if let MailRequest::LoadMessages { folder_id, .. }
        | MailRequest::SyncFolder { folder_id, .. } = &req
        {
            emit(WorkerEvent::BackfillDone {
                folder_id: *folder_id,
            });
        }
        // A message sent or scheduled with the server out of reach
        // waits in the Outbox; dropped here it was lost (#442).
        if let MailRequest::Send {
            mut message,
            sent_path,
            ticket,
        } = req
        {
            restore_msgid_case(cache.as_ref(), &mut message);
            match message.send_at.filter(|t| *t > crate::datefmt::now()) {
                Some(at) => schedule_send(
                    cache.as_ref(),
                    account_id,
                    account,
                    &message,
                    sent_path.as_deref(),
                    at,
                    emit,
                ),
                None => {
                    let e = if crate::desktop::power::reaches(&account.imap_host) {
                        i18n("could not reach the server")
                    } else {
                        offline_text()
                    };
                    send_failed(
                        cache.as_ref(),
                        account_id,
                        account,
                        &message,
                        sent_path.as_deref(),
                        &e,
                        false,
                        emit,
                    );
                }
            }
            emit(WorkerEvent::SendDone {
                ticket,
                sent: false,
            });
        }
        None
    }

    /// The handler for each kind of request. Order matters only for `Send`,
    /// whose Send Later arm comes first.
    async fn dispatch(
        &mut self,
        req: MailRequest,
        background: bool,
        pending_bodies: Vec<(u32, u32)>,
    ) -> After {
        match req {
            // Served from cache before this network match; never reached here.
            MailRequest::LoadRelated { .. } | MailRequest::LoadThreadSummaries { .. } => {
                After::Done
            }
            MailRequest::LoadMessages { folder_id, path }
            | MailRequest::SyncFolder { folder_id, path } => {
                self.on_load_messages(folder_id, path, background).await
            }
            MailRequest::LoadBody {
                message_id,
                path,
                uid,
            } => self.on_load_body(message_id, path, uid).await,
            MailRequest::LoadBodies { path, .. } => self.on_load_bodies(path, pending_bodies).await,
            MailRequest::LoadSource { path, uid, .. } => self.on_load_source(path, uid).await,
            MailRequest::LoadAttachments {
                message_id,
                path,
                uid,
                download: _,
            } => self.on_load_attachments(message_id, path, uid).await,
            MailRequest::SetSeen { path, uid, seen } => self.on_set_seen(path, uid, seen).await,
            MailRequest::SetFlagged { path, uid, flagged } => {
                self.on_set_flagged(path, uid, flagged).await
            }
            MailRequest::SetKeyword {
                path,
                uid,
                message_id,
                keyword,
                add,
            } => {
                self.on_set_keyword(path, uid, message_id, keyword, add)
                    .await
            }
            MailRequest::MarkAllRead { folder_id, path } => {
                self.on_mark_all_read(folder_id, path).await
            }
            MailRequest::RefreshUnread => {
                // reorder_reader_loads has moved queued requests into backlog,
                // so rx alone cannot tell a sweep that the reader is waiting.
                // Defer both the listing and counts to the idle chain, which
                // runs only after backlog and rx have drained. Repeated refresh
                // ticks coalesce instead of queuing another mailbox-wide walk.
                self.folder_refresh_due = true;
                self.sweep_due = true;
                After::Done
            }
            MailRequest::MarkSpam { path, uid, dest } => self.on_mark_spam(path, uid, dest).await,
            MailRequest::MarkHam { path, uid, dest } => self.on_mark_ham(path, uid, dest).await,
            MailRequest::MarkHamMany { path, uids, dest } => {
                self.on_mark_ham_many(path, uids, dest).await
            }
            MailRequest::ExportRaw {
                token, path, uid, ..
            } => self.on_export_raw(token, path, uid).await,
            MailRequest::ImportRaw {
                token,
                path,
                raw,
                seen,
                flagged,
            } => self.on_import_raw(token, path, raw, seen, flagged).await,
            MailRequest::DeleteAttachment {
                message_id,
                path,
                uid,
                name,
                size,
            } => {
                self.on_delete_attachment(message_id, path, uid, name, size)
                    .await
            }
            MailRequest::MoveMessage { path, uid, dest } => {
                self.on_move_message(path, uid, dest).await
            }
            MailRequest::MoveMessages { path, uids, dest } => {
                self.on_move_messages(path, uids, dest).await
            }
            MailRequest::Locate { message_id } => self.on_locate(message_id).await,
            MailRequest::ScanAttachments { folder_path } => {
                self.on_scan_attachments(folder_path).await
            }
            MailRequest::FindKeywords => self.on_find_keywords().await,
            MailRequest::Quota => self.on_quota().await,
            MailRequest::RefreshKeywords { keywords } => self.on_refresh_keywords(keywords).await,
            MailRequest::UndoMove {
                path,
                dest,
                dest_folder_id,
                message_ids,
            } => {
                self.on_undo_move(path, dest, dest_folder_id, message_ids)
                    .await
            }
            MailRequest::Unsnooze {
                path,
                dest,
                dest_folder_id,
                message_ids,
            } => {
                self.on_unsnooze(path, dest, dest_folder_id, message_ids)
                    .await
            }
            MailRequest::PurgeMessages { path, uids } => self.on_purge_messages(path, uids).await,
            MailRequest::EmptyFolder { folder_id, path } => {
                self.on_empty_folder(folder_id, path).await
            }
            MailRequest::CreateFolder { path } => self.on_create_folder(path).await,
            MailRequest::RenameFolder { old_path, new_path } => {
                self.on_rename_folder(old_path, new_path).await
            }
            MailRequest::SetHiddenFolders { paths } => self.on_set_hidden_folders(paths).await,
            MailRequest::DeleteFolder { path, trash } => self.on_delete_folder(path, trash).await,
            // Send Later (#145): into the Outbox until its time.
            MailRequest::Send {
                message,
                sent_path,
                ticket,
            } if message.send_at.is_some_and(|t| t > crate::datefmt::now()) => {
                self.on_send_later(message, sent_path, ticket).await
            }
            MailRequest::Send {
                message,
                sent_path,
                ticket,
            } => self.on_send(message, sent_path, ticket).await,
            MailRequest::LoadOutbox => {
                emit_outbox(self.cache.as_ref(), self.account_id, &self.emit);
                After::Done
            }
            MailRequest::DeleteOutbox { id } => {
                delete_queued(self.cache.as_ref(), self.account_id, id, &self.emit);
                After::Done
            }
            MailRequest::FlushOutbox { id } => self.on_flush_outbox(id).await,
            MailRequest::SaveDraft {
                message,
                folder_id,
                path,
                autosave,
                template,
            } => {
                self.on_save_draft(message, folder_id, path, autosave, template)
                    .await
            }
            MailRequest::Reconnect | MailRequest::Settle { .. } => unreachable!("handled above"),
        }
    }
}
