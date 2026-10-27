//! The JMAP path (issue #245).
//!
//! JMAP (RFC 8620/8621) is JSON over HTTPS: one `POST` carries a batch of
//! method calls, and a message's raw RFC 5322 bytes come from a blob
//! download URL, so they feed the exact same parsing pipeline as IMAP and
//! Graph. Compared with Graph this fits Hylki's model with less bending:
//! mailboxes carry their role (inbox, sent, drafts…), a message keeps its id
//! when it moves, keywords are the same `$seen` / `$flagged` / user keywords
//! IMAP has (so tags need no translation), and Message-ID, In-Reply-To and
//! References come with every listing, so threading uses the real headers.
//!
//! Message uids are the same stable string-hash the POP3 and Graph paths
//! use (JMAP ids are opaque strings); the uid → id map is rebuilt from every
//! folder listing. Sending goes through `EmailSubmission`, which the server
//! files in Sent itself, so a JMAP account needs no SMTP settings. New mail
//! arrives over the server's EventSource push channel when the account's
//! push preference allows, with a poll as the fallback.
//!
//! Proven against a local Stalwart container; Fastmail speaks the same
//! standard but has not been tried by hand.

use std::time::Duration;

use tokio::sync::mpsc;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::worker::auth::load_or_migrate_password;
use crate::worker::index_queue::index_pause;
use crate::worker::jmap::folders::refresh_jmap_folders;
use crate::worker::jmap::index::{jmap_backfill_step, JmapState, JMAP_BACKFILL_BACKOFF};
use crate::worker::jmap::push::{jmap_poll_inbox, spawn_jmap_push};
use crate::worker::jmap::send::emit_jmap_identities;
use crate::worker::jmap::wire::{jmap_base_url, jmap_session};
use crate::worker::lane::{related_from_cache, thread_summaries_with_members};
use crate::worker::{MailRequest, WorkerEvent};

mod chores;
mod folders;
mod handle_read;
mod handle_send;
mod handle_write;
mod index;
mod messages;
mod ops;
mod push;
mod send;
mod snooze;
mod wire;

use crate::worker::outbox::{delete_queued, emit_outbox};
pub(super) use wire::test_jmap;

/// A JMAP account's worker: the session and index state, the push channel,
/// and what it reports through.
struct JmapWorker<E> {
    account_id: u32,
    account: AccountConfig,
    rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: E,
    cache: Option<Cache>,
    state: JmapState,
    last_auto_empty: Option<std::time::Instant>,
    last_list_clean: Option<std::time::Instant>,
    /// The push thread's wake-ups: one `()` per change the server announced.
    push_tx: mpsc::UnboundedSender<()>,
    push_rx: mpsc::UnboundedReceiver<()>,
    push_enabled: bool,
    push_started: bool,
}

/// Run a JMAP account until the UI drops its sender.
pub(super) async fn run_jmap(
    account_id: u32,
    account: AccountConfig,
    rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent),
) {
    JmapWorker::start(account_id, account, rx, emit)
        .await
        .run()
        .await;
}

impl<E: Fn(WorkerEvent)> JmapWorker<E> {
    /// Read the password, show the account and its cached folders, then open
    /// the session, list the mailboxes and start push.
    async fn start(
        account_id: u32,
        mut account: AccountConfig,
        rx: mpsc::UnboundedReceiver<MailRequest>,
        emit: E,
    ) -> Self {
        // The password lives in the keyring, like an IMAP account's.
        load_or_migrate_password(&mut account);

        let cache = Cache::open()
            .map_err(|e| tracing::warn!("cache unavailable: {e}"))
            .ok();

        emit(WorkerEvent::account(account_id, &account));

        let cached_folders = cache
            .as_ref()
            .map(|c| c.load_folders(account_id))
            .unwrap_or_default();
        if !cached_folders.is_empty() {
            emit(WorkerEvent::Folders(cached_folders));
        }

        let state = JmapState::new(&account);
        let (push_tx, push_rx) = mpsc::unbounded_channel::<()>();
        let push_enabled = account
            .push
            .unwrap_or_else(|| crate::config::load_privacy().push);
        let mut worker = JmapWorker {
            account_id,
            account,
            rx,
            emit,
            cache,
            state,
            last_auto_empty: None,
            last_list_clean: None,
            push_tx,
            push_rx,
            push_enabled,
            push_started: false,
        };

        let w = &mut worker;
        if let Some(s) = jmap_session(&w.account, &mut w.state, &w.emit).await {
            refresh_jmap_folders(&s, account_id, w.cache.as_ref(), &mut w.state, &w.emit).await;
            emit_jmap_identities(&s, &w.emit);
            if w.push_enabled {
                spawn_jmap_push(s.clone(), w.push_tx.clone());
                w.push_started = true;
            }
        }
        worker
    }

    /// Serve requests until the UI hangs up; between them, follow push,
    /// poll, and index the mailbox in the background.
    async fn run(mut self) {
        // The poll is the fallback for a server without push (or with it off):
        // the auto-fetch cadence when set, otherwise a quiet couple of minutes.
        let poll_secs = match crate::config::load_privacy().fetch_interval_secs {
            0 => 120,
            s => s.max(60),
        };
        let mut poll = tokio::time::interval(Duration::from_secs(poll_secs));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        poll.tick().await;

        loop {
            let account_id = self.account_id;
            let account = &self.account;
            let cache = &self.cache;
            let state = &mut self.state;
            let emit = &self.emit;
            // The background index (#429) runs only while nothing else is
            // asked, a single request a step.
            // Offline or on a metered connection it waits; the request that
            // follows the change wakes this loop to look again (#442).
            let indexing = !state.backfill.is_empty()
                && crate::desktop::power::reaches_url(&jmap_base_url(account))
                && !crate::desktop::power::saving_data();
            let wait = state.backfill_wait;
            let req = tokio::select! {
                biased;
                req = self.rx.recv() => match req {
                    Some(req) => req,
                    None => break,
                },
                _ = poll.tick() => {
                    if !crate::desktop::power::reaches_url(&jmap_base_url(account)) {
                        continue;
                    }
                    if let Some(s) = jmap_session(account, state, &|_| {}).await {
                        jmap_poll_inbox(&s, account_id, account, cache.as_ref(), state, emit).await;
                        if self.push_enabled && !self.push_started {
                            spawn_jmap_push(s.clone(), self.push_tx.clone());
                            self.push_started = true;
                        }
                    }
                    continue;
                }
                Some(()) = self.push_rx.recv() => {
                    // A burst of changes is one refresh.
                    while self.push_rx.try_recv().is_ok() {}
                    if let Some(s) = jmap_session(account, state, &|_| {}).await {
                        jmap_poll_inbox(&s, account_id, account, cache.as_ref(), state, emit).await;
                    }
                    continue;
                }
                _ = index_pause(wait), if indexing => {
                    match jmap_session(account, state, &|_| {}).await {
                        Some(s) => jmap_backfill_step(&s, account_id, cache.as_ref(), state, emit).await,
                        None => state.backfill_wait = Some(std::time::Instant::now() + JMAP_BACKFILL_BACKOFF),
                    }
                    continue;
                }
            };
            self.handle(req).await;
        }
    }

    /// The handler for each kind of request.
    async fn handle(&mut self, req: MailRequest) {
        // A folder the user opened, rather than a background refresh.
        let user = matches!(req, MailRequest::LoadMessages { .. });
        let account_id = self.account_id;
        let cache = &self.cache;
        let emit = &self.emit;
        match req {
            MailRequest::Locate { message_id } => self.on_locate(message_id).await,
            MailRequest::Quota => self.on_quota().await,
            MailRequest::FindKeywords => self.on_find_keywords().await,
            MailRequest::RefreshKeywords { .. } => self.on_refresh_keywords().await,
            // Not checked past the listing's newest page yet (#532).
            MailRequest::RefreshFlags { .. } => {
                emit(WorkerEvent::FlagsSynced { paths: Vec::new() })
            }
            // Cache-only, exactly like the other paths.
            MailRequest::ScanAttachments { folder_path } => {
                emit(WorkerEvent::AttachmentsScanned {
                    folder_path,
                    added: 0,
                    remaining: 0,
                });
            }
            MailRequest::LoadRelated { message_id, ids } => {
                let messages = cache
                    .as_ref()
                    .map(|c| related_from_cache(c, account_id, &ids))
                    .unwrap_or_default();
                emit(WorkerEvent::Related {
                    message_id,
                    messages,
                });
            }
            MailRequest::LoadThreadSummaries { groups } => {
                let summaries = cache
                    .as_ref()
                    .map(|c| thread_summaries_with_members(c, account_id, &groups))
                    .unwrap_or_default();
                emit(WorkerEvent::ThreadSummaries { summaries });
            }
            MailRequest::LoadMessages { folder_id, path }
            | MailRequest::SyncFolder { folder_id, path } => {
                self.on_load_messages(folder_id, path, user).await
            }
            MailRequest::LoadBody {
                message_id,
                path,
                uid,
            } => self.on_load_body(message_id, path, uid).await,
            MailRequest::LoadBodies { items, path } => self.on_load_bodies(items, path).await,
            MailRequest::LoadSource {
                message_id: _,
                path,
                uid,
            } => self.on_load_source(path, uid).await,
            MailRequest::LoadAttachments {
                message_id,
                path,
                uid,
                download,
            } => {
                self.on_load_attachments(message_id, path, uid, download)
                    .await
            }
            MailRequest::SetSeen { path, uid, seen } => self.on_set_seen(path, uid, seen).await,
            MailRequest::SetFlagged { path, uid, flagged } => {
                self.on_set_flagged(path, uid, flagged).await
            }
            MailRequest::SetKeyword {
                path,
                uid,
                keyword,
                add,
                ..
            } => self.on_set_keyword(path, uid, keyword, add).await,
            MailRequest::MarkAllRead { folder_id, path } => {
                self.on_mark_all_read(folder_id, path).await
            }
            MailRequest::MoveMessage { path, uid, dest } => {
                self.on_move_message(path, uid, dest).await
            }
            MailRequest::MarkSpam { path, uid, dest } => self.on_mark_spam(path, uid, dest).await,
            MailRequest::MarkHam { path, uid, dest } => self.on_mark_ham(path, uid, dest).await,
            MailRequest::MarkHamMany { path, uids, dest } => {
                self.on_mark_ham_many(path, uids, dest).await
            }
            MailRequest::MoveMessages { path, uids, dest } => {
                self.on_move_messages(path, uids, dest).await
            }
            MailRequest::PurgeMessages { path, uids } => self.on_purge_messages(path, uids).await,
            MailRequest::EmptyFolder { folder_id, path } => {
                self.on_empty_folder(folder_id, path).await
            }
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
            MailRequest::CreateFolder { path } => self.on_create_folder(path).await,
            MailRequest::RenameFolder { old_path, new_path } => {
                self.on_rename_folder(old_path, new_path).await
            }
            MailRequest::SetHiddenFolders { paths } => self.on_set_hidden_folders(paths).await,
            MailRequest::DeleteFolder { path, trash } => self.on_delete_folder(path, trash).await,
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
            MailRequest::LoadOutbox => emit_outbox(cache.as_ref(), account_id, emit),
            MailRequest::DeleteOutbox { id } => delete_queued(cache.as_ref(), account_id, id, emit),
            MailRequest::FlushOutbox { id } => self.on_flush_outbox(id).await,
            MailRequest::RefreshUnread => self.on_refresh_unread().await,
            MailRequest::Reconnect => self.on_reconnect().await,
            MailRequest::Settle { path, uids } => emit(WorkerEvent::MovesSettled { path, uids }),
            MailRequest::ExportRaw {
                token, path, uid, ..
            } => self.on_export_raw(token, path, uid).await,
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
            MailRequest::ImportRaw {
                token,
                path,
                raw,
                seen,
                flagged,
            } => self.on_import_raw(token, path, raw, seen, flagged).await,
        }
    }
}

#[cfg(test)]
mod snooze_tests;
#[cfg(test)]
mod tests;

/// Pins of current behaviour for JMAP helpers (Area B, step B0).
#[cfg(test)]
mod characterization_tests {
    use crate::models::FolderKind;
    use crate::worker::jmap::folders::role_kind;
    use crate::worker::jmap::wire::jmap_err;

    fn status(code: u16, body: &str) -> ureq::Error {
        ureq::Error::Status(code, ureq::Response::new(code, "x", body).unwrap())
    }

    #[test]
    fn jmap_errors_name_a_refused_sign_in_and_surface_problem_details() {
        assert_eq!(
            jmap_err(status(401, "whatever")),
            "the server refused the sign-in (401): check the username and password, or the API token"
        );
        assert_eq!(
            jmap_err(status(400, r#"{"title":"Bad","detail":"No such method"}"#)),
            "the server returned 400: No such method"
        );
        assert_eq!(
            jmap_err(status(400, r#"{"title":"Bad"}"#)),
            "the server returned 400: Bad"
        );
        assert_eq!(
            jmap_err(status(502, "gateway")),
            "the server returned 502: gateway"
        );
    }

    #[test]
    fn jmap_roles_map_to_folder_kinds() {
        for (role, kind) in [
            ("inbox", FolderKind::Inbox),
            ("sent", FolderKind::Sent),
            ("drafts", FolderKind::Drafts),
            ("trash", FolderKind::Trash),
            ("junk", FolderKind::Junk),
            ("archive", FolderKind::Archive),
            ("flagged", FolderKind::Custom),
            ("templates", FolderKind::Custom),
            ("Inbox", FolderKind::Custom),
        ] {
            assert_eq!(role_kind(role), kind, "{role}");
        }
    }
}
