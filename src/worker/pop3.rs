//! The POP3 backend: the whole mailbox is the inbox, downloaded in full and
//! kept in the cache; flags and folders live only on this machine.

use tokio::sync::mpsc;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};
use crate::models::{Folder, FolderKind, Message};
use crate::worker::auth::{load_or_migrate_password, load_smtp_password, sign_in_password};
use crate::worker::compose::build::restore_msgid_case;
use crate::worker::ids::hash_uid;
use crate::worker::imap::send::flush_outbox;
use crate::worker::outbox::{
    delete_queued, drop_superseded_outbox, emit_outbox, note_answered, send_failed,
};
use crate::worker::pop3::client::Pop3;
use crate::worker::render::attachments::{attachments_of, extract_attachments};
use crate::worker::render::body_cacheable;
use crate::worker::smtp::{send_smtp, smtp_refused};
use crate::worker::strip::strip_refused;
use crate::worker::{
    answered_copies, render_raw, summary_from_raw, MailRequest, OutgoingMessage, WorkerEvent,
};

pub(super) mod client;

/// Cap on how many messages a POP3 sync downloads in full (POP3 has no partial
/// fetch; mailboxes are usually small). Older messages aren't indexed.
const POP3_LIMIT: usize = 200;

/// The one folder a POP3 account has, and its id.
const INBOX: &str = "INBOX";
const INBOX_ID: u32 = 1;

/// The folder list for a POP3 account: just the inbox.
fn pop3_folders(account_id: u32) -> Vec<Folder> {
    vec![Folder {
        id: 1,
        account_id,
        name: "Inbox".to_string(),
        path: "INBOX".to_string(),
        kind: FolderKind::Inbox,
        unread: 0,
    }]
}

/// Run a POP3 account until the UI drops its sender.
pub(super) async fn run_pop3(
    account_id: u32,
    mut account: AccountConfig,
    mut rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent),
) {
    // Resolve credentials from the keyring (same as the IMAP path).
    load_or_migrate_password(&mut account);
    load_smtp_password(&mut account);

    let cache = Cache::open()
        .map_err(|e| tracing::warn!("cache unavailable: {e}"))
        .ok();

    emit(WorkerEvent::account(account_id, &account));
    let folders = pop3_folders(account_id);
    if let Some(c) = cache.as_ref() {
        c.save_folders(account_id, &folders);
    }
    emit(WorkerEvent::Folders(folders));

    let mut worker = Pop3Worker {
        account_id,
        account,
        cache,
        emit,
    };
    while let Some(req) = rx.recv().await {
        worker.handle(req).await;
    }
}

/// A POP3 account's worker: there is no session to keep, every request
/// that needs the server opens its own connection.
struct Pop3Worker<E> {
    account_id: u32,
    account: AccountConfig,
    cache: Option<Cache>,
    emit: E,
}

impl<E: Fn(WorkerEvent)> Pop3Worker<E> {
    /// The answer to each kind of request; most have nothing to do on POP3.
    async fn handle(&mut self, req: MailRequest) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let emit = &self.emit;
        match req {
            MailRequest::Locate { message_id } => {
                // No folders to search here beyond what the cache already
                // held: answer "not found" so the app can report the miss.
                emit(WorkerEvent::Located {
                    message_id: message_id.clone(),
                    hit: None,
                });
            }
            // POP3 downloads every message in full, so an attachment is either
            // already in the cache or does not exist: there is nothing a scan
            // could add.
            MailRequest::ScanAttachments { folder_path } => {
                emit(WorkerEvent::AttachmentsScanned {
                    folder_path,
                    added: 0,
                    remaining: 0,
                });
            }
            // POP3 has no server-side keywords: nothing to find.
            MailRequest::FindKeywords => emit(WorkerEvent::KeywordsFound(Vec::new())),
            // POP3 has no way to say how much room is left.
            MailRequest::Quota => emit(WorkerEvent::Quota(None)),
            MailRequest::RefreshKeywords { .. } => {
                emit(WorkerEvent::KeywordsSynced { paths: Vec::new() })
            }
            // POP3 keeps everything in the inbox, so a conversation never spans
            // folders and the reader already has all of it.
            MailRequest::LoadRelated { message_id, .. } => {
                emit(WorkerEvent::Related {
                    message_id,
                    messages: Vec::new(),
                });
            }
            // Same reason: everything is in the inbox, so the list's own count
            // is already the whole conversation (#222).
            MailRequest::LoadThreadSummaries { .. } => {
                emit(WorkerEvent::ThreadSummaries {
                    summaries: Vec::new(),
                });
            }
            MailRequest::LoadMessages { folder_id, path }
            | MailRequest::SyncFolder { folder_id, path } => {
                self.on_load_messages(folder_id, path).await
            }

            MailRequest::LoadBody {
                message_id,
                path: _,
                uid,
            } => self.load_body(message_id, uid).await,

            // POP3 has no set fetch — RETR is one message at a time — so a batch
            // is the same work in a loop, saving only the per-message request hop.
            MailRequest::LoadBodies { items, path: _ } => {
                for (message_id, uid) in items {
                    self.load_body(message_id, uid).await;
                }
            }

            MailRequest::LoadSource {
                message_id: _,
                path: _,
                uid,
            } => match pop3_fetch_raw(account, uid).await {
                Ok(raw) => emit(WorkerEvent::Source {
                    text: String::from_utf8_lossy(&raw).into_owned(),
                }),
                Err(e) => emit(WorkerEvent::net_error(i18n_f(
                    "Could not load source: {e}",
                    &[("e", &(e).to_string())],
                ))),
            },

            MailRequest::LoadAttachments {
                message_id,
                path: _,
                uid,
                download,
            } => self.on_load_attachments(message_id, uid, download).await,

            MailRequest::SetSeen { uid, seen, .. } => {
                if let Some(c) = cache.as_ref() {
                    c.set_unread(account_id, INBOX, uid, !seen);
                }
                emit(WorkerEvent::SeenSettled {
                    path: INBOX.to_string(),
                    uid,
                });
            }
            MailRequest::SetFlagged { uid, flagged, .. } => {
                if let Some(c) = cache.as_ref() {
                    c.set_starred(account_id, INBOX, uid, flagged);
                }
            }
            // POP3 has no flags at all: every tag is Hylki's own.
            MailRequest::SetKeyword {
                uid,
                message_id,
                keyword,
                add,
                ..
            } => {
                if let Some(c) = cache.as_ref() {
                    if message_id.is_empty() {
                        c.set_keyword(account_id, INBOX, uid, &keyword, add);
                    } else {
                        c.set_local_tag(account_id, &message_id, &keyword, add);
                    }
                }
            }
            MailRequest::MarkAllRead { folder_id, .. } => {
                if let Some(c) = cache.as_ref() {
                    c.mark_folder_read(account_id, INBOX);
                }
                emit(WorkerEvent::FolderUnread {
                    folder_id,
                    unread: 0,
                });
            }

            // POP3 has no folders to move between: deleting removes from the
            // server. (Archive/spam have no destination folder, so the UI never
            // reaches here for those.)
            // Not-spam has nothing to do here either (no Junk folder to leave).
            MailRequest::MarkHam { .. } => {}
            MailRequest::MarkHamMany { .. } => emit(WorkerEvent::BulkComplete),
            MailRequest::MoveMessage { uid, .. } | MailRequest::MarkSpam { uid, .. } => {
                match pop3_delete(account, uid).await {
                    Ok(()) => {
                        if let Some(c) = cache.as_ref() {
                            c.delete_message(account_id, INBOX, uid);
                        }
                    }
                    Err(e) => emit(WorkerEvent::error(i18n_f(
                        "Could not delete message: {e}",
                        &[("e", &(e).to_string())],
                    ))),
                }
            }
            // POP3 shows only its Inbox; there is no Trash or Junk to empty.
            MailRequest::EmptyFolder { .. } => {}
            MailRequest::MoveMessages { uids, .. } | MailRequest::PurgeMessages { uids, .. } => {
                for uid in uids {
                    if pop3_delete(account, uid).await.is_ok() {
                        if let Some(c) = cache.as_ref() {
                            c.delete_message(account_id, INBOX, uid);
                        }
                    }
                }
                emit(WorkerEvent::BulkComplete);
            }

            // A POP3 move deletes the message on the server: there is
            // nothing to bring back. Answered all the same, so the app's
            // busy indicator stops.
            MailRequest::UndoMove { .. } => {
                emit(WorkerEvent::error(i18n(
                    "POP3 accounts don't support folders",
                )));
                emit(WorkerEvent::BulkComplete);
            }

            // Nothing is snoozed on POP3, which has no folder to keep it in
            // (#527); a stray request is spent, not retried.
            MailRequest::Unsnooze { message_ids, .. } => emit(WorkerEvent::Unsnoozed {
                message_ids,
                done: true,
            }),

            // POP3 has no folders beyond the inbox.
            MailRequest::CreateFolder { .. }
            | MailRequest::RenameFolder { .. }
            | MailRequest::DeleteFolder { .. }
            | MailRequest::SetHiddenFolders { .. }
            | MailRequest::SaveDraft { .. } => {
                emit(WorkerEvent::error(i18n(
                    "POP3 accounts don't support folders",
                )));
            }

            MailRequest::Send {
                message, ticket, ..
            } => self.on_send(message, ticket).await,

            MailRequest::LoadOutbox => emit_outbox(cache.as_ref(), account_id, emit),

            MailRequest::DeleteOutbox { id } => delete_queued(cache.as_ref(), account_id, id, emit),

            MailRequest::FlushOutbox { id } => self.on_flush_outbox(id).await,

            // POP3 has only the inbox, whose count every sync refreshes.
            MailRequest::RefreshUnread => {}

            MailRequest::Reconnect => {
                emit(WorkerEvent::Folders(pop3_folders(account_id)));
            }

            MailRequest::Settle { path, uids } => emit(WorkerEvent::MovesSettled { path, uids }),

            MailRequest::ExportRaw { token, uid, .. } => {
                let raw = pop3_fetch_raw(account, uid).await;
                emit(WorkerEvent::RawExported { token, raw });
            }

            // POP3 has nowhere to put mail: it is never offered as a destination.
            MailRequest::ImportRaw { token, .. } => emit(WorkerEvent::RawImported {
                token,
                result: Err(i18n(
                    "A POP3 account can't receive mail from another account",
                )),
            }),
            MailRequest::DeleteAttachment {
                message_id,
                name,
                size,
                ..
            } => strip_refused(
                emit,
                i18n(
                    "POP3 cannot change a message on the server, so the attachment was not removed",
                ),
                message_id,
                name,
                size,
            ),
        }
    }

    /// The inbox list: the cache at once, then the whole mailbox from the
    /// server. Other folders do not exist on POP3.
    async fn on_load_messages(&mut self, folder_id: u32, path: String) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let emit = &self.emit;
        if path != INBOX {
            emit(WorkerEvent::Messages {
                folder_id,
                messages: Vec::new(),
            });
            emit(WorkerEvent::BackfillDone { folder_id });
            return;
        }
        // Serve cache first for instant display.
        if let Some(c) = cache.as_ref() {
            let cached = c.load_messages(account_id, INBOX, INBOX_ID);
            if !cached.is_empty() {
                emit(WorkerEvent::Messages {
                    folder_id,
                    messages: cached,
                });
            }
        }
        emit(WorkerEvent::Status(i18n("Syncing…")));
        match pop3_sync(account_id, account, INBOX_ID, cache.as_ref()).await {
            Ok(messages) => {
                let unread = messages.iter().filter(|m| m.unread).count() as u32;
                emit(WorkerEvent::Messages {
                    folder_id,
                    messages,
                });
                emit(WorkerEvent::FolderUnread {
                    folder_id: INBOX_ID,
                    unread,
                });
                // POP3 syncs the whole mailbox in one pass; no backfill
                // follows, so the index is complete (see the Graph path).
                emit(WorkerEvent::BackfillDone { folder_id });
            }
            Err(e) => {
                emit(WorkerEvent::net_error(i18n_f(
                    "Could not fetch mail: {e}",
                    &[("e", &(e).to_string())],
                )));
                emit(WorkerEvent::BackfillDone { folder_id });
            }
        }
        emit(WorkerEvent::Status(String::new()));
    }

    /// One message's body: from the cache, or downloaded in full, rendered
    /// and cached.
    async fn load_body(&mut self, message_id: u32, uid: u32) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let emit = &self.emit;
        if let Some(body) = cache
            .as_ref()
            .and_then(|c| c.load_body(account_id, INBOX, uid))
        {
            emit(WorkerEvent::Body {
                message_id,
                path: INBOX.to_string(),
                body,
            });
            if let Some(check) = cache
                .as_ref()
                .and_then(|c| c.load_sender_check(account_id, INBOX, uid))
            {
                emit(WorkerEvent::SenderChecked {
                    path: INBOX.to_string(),
                    message_id,
                    check,
                });
            }
            return;
        }
        match pop3_fetch_raw(account, uid).await {
            Ok(raw) => {
                let (body, check, _) = render_raw(&raw);
                if let Some(c) = cache.as_ref() {
                    if body_cacheable(&check) {
                        c.save_body(account_id, INBOX, uid, &body);
                    }
                    c.save_sender_check(account_id, INBOX, uid, &check);
                }
                emit(WorkerEvent::Body {
                    message_id,
                    path: INBOX.to_string(),
                    body,
                });
                emit(WorkerEvent::SenderChecked {
                    path: INBOX.to_string(),
                    message_id,
                    check,
                });
            }
            Err(e) => emit(WorkerEvent::net_error(i18n_f(
                "Could not load message: {e}",
                &[("e", &(e).to_string())],
            ))),
        }
    }

    /// A message's attachments, from the cache or, when the user asked, the
    /// server.
    async fn on_load_attachments(&mut self, message_id: u32, uid: u32, download: bool) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let emit = &self.emit;
        if let Some(c) = cache.as_ref() {
            let items = c.load_attachments(account_id, INBOX, uid);
            if !items.is_empty() {
                emit(WorkerEvent::Attachments {
                    path: INBOX.to_string(),
                    message_id,
                    items,
                });
                return;
            }
        }
        if !download {
            emit(WorkerEvent::AttachmentsPending {
                path: INBOX.to_string(),
                message_id,
            });
            return;
        }
        match pop3_fetch_raw(account, uid).await {
            Ok(raw) => {
                let items = extract_attachments(&raw);
                if let Some(c) = cache.as_ref() {
                    c.save_attachments(account_id, INBOX, uid, &items);
                }
                emit(WorkerEvent::Attachments {
                    path: INBOX.to_string(),
                    message_id,
                    items,
                });
            }
            Err(e) => emit(WorkerEvent::net_error(i18n_f(
                "Could not load attachments: {e}",
                &[("e", &(e).to_string())],
            ))),
        }
    }

    /// Send through SMTP; POP3 has no Sent folder, and an answered mark
    /// is kept in the cache only.
    async fn on_send(&mut self, mut message: Box<OutgoingMessage>, ticket: u64) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let emit = &self.emit;
        restore_msgid_case(cache.as_ref(), &mut message);
        match send_smtp(account, &message).await {
            Ok(_) => {
                // Kept in the cache only: POP3 has no flags.
                for (path, uid, forwarded) in answered_copies(
                    cache.as_ref(),
                    account_id,
                    &message.in_reply_to,
                    &message.forwards,
                ) {
                    note_answered(cache.as_ref(), account_id, path, uid, forwarded, emit);
                }
                drop_superseded_outbox(cache.as_ref(), account_id, &message, emit);
                emit(WorkerEvent::Sent);
                emit(WorkerEvent::SendDone { ticket, sent: true });
            }
            Err(e) => {
                // POP3 has no Sent folder to copy to, but the message is held
                // exactly as it is for IMAP accounts.
                send_failed(
                    cache.as_ref(),
                    account_id,
                    account,
                    &message,
                    None,
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
    }

    /// Try the Outbox now, one message or all of them.
    async fn on_flush_outbox(&mut self, id: Option<u32>) {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let emit = &self.emit;
        let mut no_session = None;
        flush_outbox(
            cache.as_ref(),
            account_id,
            account,
            id,
            &mut no_session,
            &mut true,
            emit,
            true,
        )
        .await;
    }
}

/// Connect, download summaries/bodies for new messages, return the inbox list
/// (newest first), merging server UIDs with the local cache (for read state).
async fn pop3_sync(
    account_id: u32,
    account: &AccountConfig,
    inbox_id: u32,
    cache: Option<&Cache>,
) -> Result<Vec<Message>, String> {
    let mut pop = Pop3::connect(account).await?;
    pop.login(&account.username, &sign_in_password(account, false).await?)
        .await?;
    let mut entries = pop.uidl().await?;
    // Newest first; bound how many we index.
    entries.sort_by_key(|e| std::cmp::Reverse(e.0));
    entries.truncate(POP3_LIMIT);

    let cached: std::collections::HashMap<u32, Message> = cache
        .map(|c| c.load_messages(account_id, INBOX, inbox_id))
        .unwrap_or_default()
        .into_iter()
        .map(|m| (m.uid, m))
        .collect();

    let mut messages = Vec::with_capacity(entries.len());
    for (num, uid_str) in &entries {
        let uid = hash_uid(uid_str);
        if let Some(existing) = cached.get(&uid) {
            messages.push(existing.clone()); // keep read/star state; already downloaded
            continue;
        }
        // New message: download in full, cache its body + attachments.
        let raw = pop.retr(*num).await?;
        let msg = summary_from_raw(account_id, inbox_id, uid, &raw);
        // OpenPGP mail (#133) is neither rendered nor cached here: it is
        // decrypted or verified, in the reader only, when opened.
        if let Some(c) = cache.filter(|_| crate::mail::pgp::detect(&raw).is_none()) {
            let (body, check, _) = render_raw(&raw);
            c.save_body(account_id, INBOX, uid, &body);
            c.save_sender_check(account_id, INBOX, uid, &check);
            let (items, _) = attachments_of(&raw);
            if !items.is_empty() {
                c.save_attachments(account_id, INBOX, uid, &items);
            }
        }
        messages.push(msg);
    }
    pop.quit().await;

    messages.sort_by_key(|m| std::cmp::Reverse(m.timestamp));
    if let Some(c) = cache {
        c.save_messages(account_id, INBOX, &messages);
    }
    Ok(messages)
}

/// Fetch one message's raw bytes by its hashed UID (reconnects + maps UID→num).
async fn pop3_fetch_raw(account: &AccountConfig, uid: u32) -> Result<Vec<u8>, String> {
    let mut pop = Pop3::connect(account).await?;
    pop.login(&account.username, &sign_in_password(account, false).await?)
        .await?;
    let num = pop
        .uidl()
        .await?
        .into_iter()
        .find(|(_, u)| hash_uid(u) == uid)
        .map(|(n, _)| n)
        .ok_or_else(|| "message no longer on server".to_string())?;
    let raw = pop.retr(num).await?;
    pop.quit().await;
    Ok(raw)
}

/// Delete a message from the POP3 server (DELE, committed on QUIT).
async fn pop3_delete(account: &AccountConfig, uid: u32) -> Result<(), String> {
    let mut pop = Pop3::connect(account).await?;
    pop.login(&account.username, &sign_in_password(account, false).await?)
        .await?;
    let num = pop
        .uidl()
        .await?
        .into_iter()
        .find(|(_, u)| hash_uid(u) == uid)
        .map(|(n, _)| n);
    if let Some(num) = num {
        pop.command(&format!("DELE {num}")).await?;
    }
    pop.quit().await; // commits the deletion
    Ok(())
}
