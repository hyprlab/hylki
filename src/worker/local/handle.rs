//! What the On This Computer account answers to each request, and the ones
//! that only read: bodies, sources, attachments, searches.

use std::collections::BTreeMap;

use super::LocalWorker;
use crate::i18n::{i18n, i18n_f};
use crate::models::KeywordFinding;
use crate::worker::lane::{related_from_cache, thread_summaries_with_members};
use crate::worker::outbox::{delete_queued, emit_outbox, send_failed};
use crate::worker::render::attachments::extract_attachments;
use crate::worker::render::body_cacheable;
use crate::worker::{render_raw, MailRequest, OutgoingMessage, WorkerEvent};

impl<E: Fn(WorkerEvent)> LocalWorker<E> {
    pub(super) async fn handle(&mut self, req: MailRequest) {
        let account_id = self.account_id;
        let emit = &self.emit;
        match req {
            MailRequest::LoadMessages { folder_id, path }
            | MailRequest::SyncFolder { folder_id, path } => self.on_load_messages(folder_id, path),
            MailRequest::LoadBody {
                message_id,
                path,
                uid,
            } => self.on_load_body(message_id, &path, uid),
            MailRequest::LoadBodies { items, path } => {
                for (message_id, uid) in items {
                    self.on_load_body(message_id, &path, uid);
                }
            }
            MailRequest::LoadSource { path, uid, .. } => match self.store.read(&path, uid) {
                Ok(raw) => emit(WorkerEvent::Source {
                    text: String::from_utf8_lossy(&raw).into_owned(),
                }),
                Err(e) => emit(WorkerEvent::error(i18n_f(
                    "Could not load source: {e}",
                    &[("e", &e.to_string())],
                ))),
            },
            // The file is right here: no download to wait for, whatever
            // the setting says.
            MailRequest::LoadAttachments {
                message_id,
                path,
                uid,
                ..
            } => self.on_load_attachments(message_id, path, uid),
            MailRequest::ExportRaw {
                token, path, uid, ..
            } => {
                let raw = self.store.read(&path, uid).map_err(|e| e.to_string());
                emit(WorkerEvent::RawExported { token, raw });
            }
            MailRequest::Locate { message_id } => {
                let hit = self
                    .cache
                    .as_ref()
                    .map(|c| c.locate_by_message_id(&message_id))
                    .unwrap_or_default()
                    .into_iter()
                    .find(|(a, _, _)| *a == account_id)
                    .map(|(_, path, uid)| (path, uid));
                emit(WorkerEvent::Located { message_id, hit });
            }
            // There is no server to search: every message is rendered into
            // the cache, whose index the app searches itself.
            MailRequest::FindText { gen, .. } => emit(WorkerEvent::TextFound {
                gen,
                hits: Vec::new(),
                done: true,
            }),
            MailRequest::ScanAttachments { folder_path } => emit(WorkerEvent::AttachmentsScanned {
                folder_path,
                added: 0,
                remaining: 0,
            }),
            MailRequest::FindKeywords => self.on_find_keywords(),
            // The file names are the flags, and a folder reads them again
            // each time it opens.
            MailRequest::RefreshKeywords { .. } => {
                emit(WorkerEvent::KeywordsSynced { paths: Vec::new() })
            }
            MailRequest::RefreshFlags { .. } => {
                emit(WorkerEvent::FlagsSynced { paths: Vec::new() })
            }
            MailRequest::Quota => emit(WorkerEvent::Quota(None)),
            MailRequest::LoadRelated { message_id, ids } => {
                let messages = self
                    .cache
                    .as_ref()
                    .map(|c| related_from_cache(c, account_id, &ids))
                    .unwrap_or_default();
                emit(WorkerEvent::Related {
                    message_id,
                    messages,
                });
            }
            MailRequest::LoadThreadSummaries { groups } => {
                let summaries = self
                    .cache
                    .as_ref()
                    .map(|c| thread_summaries_with_members(c, account_id, &groups))
                    .unwrap_or_default();
                emit(WorkerEvent::ThreadSummaries { summaries });
            }

            MailRequest::SetSeen { path, uid, seen } => self.on_set_seen(path, uid, seen),
            MailRequest::SetFlagged { path, uid, flagged } => {
                self.on_set_flagged(path, uid, flagged)
            }
            MailRequest::SetKeyword {
                path,
                uid,
                message_id,
                keyword,
                add,
            } => self.on_set_keyword(path, uid, message_id, keyword, add),
            MailRequest::MarkAllRead { folder_id, path } => self.on_mark_all_read(folder_id, path),
            MailRequest::MoveMessage { path, uid, dest }
            | MailRequest::MarkSpam { path, uid, dest }
            | MailRequest::MarkHam { path, uid, dest } => {
                self.on_move(&path, &[uid], &dest);
            }
            MailRequest::MoveMessages { path, uids, dest }
            | MailRequest::MarkHamMany { path, uids, dest } => {
                self.on_move(&path, &uids, &dest);
                (self.emit)(WorkerEvent::BulkComplete);
            }
            MailRequest::PurgeMessages { path, uids } => {
                self.on_purge(&path, &uids);
                (self.emit)(WorkerEvent::BulkComplete);
            }
            MailRequest::EmptyFolder { folder_id, path } => self.on_empty_folder(folder_id, path),
            MailRequest::UndoMove {
                path,
                dest,
                dest_folder_id,
                message_ids,
            } => {
                if let Err(e) = self.move_back(&path, &dest, dest_folder_id, &message_ids, "") {
                    (self.emit)(WorkerEvent::error(i18n_f("Undo failed: {e}", &[("e", &e)])));
                }
                (self.emit)(WorkerEvent::BulkComplete);
            }
            // Snoozed mail comes back unread (#527).
            MailRequest::Unsnooze {
                path,
                dest,
                dest_folder_id,
                message_ids,
            } => {
                let done = match self.move_back(&path, &dest, dest_folder_id, &message_ids, "S") {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::warn!("snooze: could not bring {message_ids:?} back: {e}");
                        false
                    }
                };
                (self.emit)(WorkerEvent::Unsnoozed { message_ids, done });
            }
            MailRequest::Settle { path, uids } => emit(WorkerEvent::MovesSettled { path, uids }),
            MailRequest::CreateFolder { path } => self.on_create_folder(path),
            MailRequest::RenameFolder { old_path, new_path } => {
                self.on_rename_folder(old_path, new_path)
            }
            MailRequest::DeleteFolder { path, trash } => self.on_delete_folder(path, trash),
            MailRequest::SetHiddenFolders { paths } => {
                self.account.hidden_folders = paths;
                self.refresh_folders();
            }
            MailRequest::ImportRaw {
                token,
                path,
                raw,
                seen,
                flagged,
            } => self.on_import_raw(token, path, raw, seen, flagged),
            MailRequest::ImportMail { sources, mbox } => self.on_import(sources, mbox),
            MailRequest::DeleteAttachment {
                message_id,
                path,
                uid,
                name,
                size,
            } => self.on_delete_attachment(message_id, path, uid, name, size),

            // Nothing is sent from this account; the composer offers it no
            // From address. A message that gets here all the same goes back
            // to its composer, with word why.
            MailRequest::Send {
                message, ticket, ..
            } => self.refuse_send(&message, ticket),
            MailRequest::SaveDraft { .. } => emit(WorkerEvent::error(i18n(
                "Mail on this computer is not sent, and has no drafts",
            ))),
            MailRequest::LoadOutbox | MailRequest::FlushOutbox { .. } => {
                emit_outbox(self.cache.as_ref(), account_id, emit)
            }
            MailRequest::DeleteOutbox { id } => {
                delete_queued(self.cache.as_ref(), account_id, id, emit)
            }
            MailRequest::RefreshUnread => self.on_refresh_unread(),
            MailRequest::Reconnect => self.refresh_folders(),
        }
    }

    /// One message's body, rendered from its file and cached.
    fn on_load_body(&mut self, message_id: u32, path: &str, uid: u32) {
        let account_id = self.account_id;
        let raw = match self.store.read(path, uid) {
            Ok(raw) => raw,
            Err(e) => {
                (self.emit)(WorkerEvent::error(i18n_f(
                    "Could not load message: {e}",
                    &[("e", &e.to_string())],
                )));
                return;
            }
        };
        let (body, check, _) = render_raw(&raw);
        if let Some(c) = self.cache.as_ref() {
            if body_cacheable(&check) {
                c.save_body(account_id, path, uid, &body);
            }
            c.save_sender_check(account_id, path, uid, &check);
        }
        (self.emit)(WorkerEvent::Body {
            message_id,
            path: path.to_string(),
            body,
        });
        (self.emit)(WorkerEvent::SenderChecked {
            path: path.to_string(),
            message_id,
            check,
        });
    }

    /// A message's attachments, from the cache or the file.
    fn on_load_attachments(&mut self, message_id: u32, path: String, uid: u32) {
        let account_id = self.account_id;
        if let Some(items) = self
            .cache
            .as_ref()
            .map(|c| c.load_attachments(account_id, &path, uid))
            .filter(|items| !items.is_empty())
        {
            (self.emit)(WorkerEvent::Attachments {
                path,
                message_id,
                items,
            });
            return;
        }
        match self.store.read(&path, uid) {
            Ok(raw) => {
                let items = extract_attachments(&raw);
                if let Some(c) = self.cache.as_ref() {
                    c.save_attachments(account_id, &path, uid, &items);
                }
                (self.emit)(WorkerEvent::Attachments {
                    path,
                    message_id,
                    items,
                });
            }
            Err(e) => (self.emit)(WorkerEvent::error(i18n_f(
                "Could not load attachments: {e}",
                &[("e", &e.to_string())],
            ))),
        }
    }

    /// The tags in use, counted from the cached summaries.
    fn on_find_keywords(&self) {
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        if let Some(c) = self.cache.as_ref() {
            for f in &self.folders {
                for m in c.load_messages(self.account_id, &f.path, f.id) {
                    for k in m.keywords {
                        *seen.entry(k).or_default() += 1;
                    }
                }
            }
        }
        let found = seen
            .into_iter()
            .map(|(keyword, count)| KeywordFinding {
                keyword,
                name: None,
                color: None,
                count,
                folders: Vec::new(),
            })
            .collect();
        (self.emit)(WorkerEvent::KeywordsFound(found));
    }

    /// A message to send that reached this account: back to its composer.
    fn refuse_send(&self, message: &OutgoingMessage, ticket: u64) {
        send_failed(
            self.cache.as_ref(),
            self.account_id,
            &self.account,
            message,
            None,
            &i18n("Mail is not sent from On This Computer: choose another From address"),
            true,
            &self.emit,
        );
        (self.emit)(WorkerEvent::SendDone {
            ticket,
            sent: false,
        });
    }
}
