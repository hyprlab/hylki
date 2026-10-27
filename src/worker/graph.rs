//! Microsoft Graph path (issue #36)
//!
//! Microsoft 365 accounts imported from GNOME Online Accounts authenticate with
//! a GOA token scoped to the Graph API — it cannot log in to IMAP or SMTP at
//! all. So these accounts speak Graph (REST) end to end: folders, summaries,
//! raw MIME bodies (`/$value`, which feeds the exact same parsing pipeline as
//! IMAP), flags, moves, drafts, and `sendMail`. Message uids are the same
//! stable string-hash the POP3 path uses (Graph ids are strings); the
//! uid → Graph-id map is rebuilt from every folder listing. Threading uses a
//! synthetic `graph-conv:<conversationId>` reference token (stripped before any
//! wire header in `build_email`) because the real References header isn't
//! available from list queries.

use std::time::Duration;

use tokio::sync::mpsc;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};
use crate::worker::graph::folders::{graph_poll_inbox, refresh_graph_folders};
use crate::worker::graph::http::graph_token;
use crate::worker::graph::index::{graph_backfill_step, GraphState};
use crate::worker::graph::ops::graph_fetch_raw;
use crate::worker::graph::send::graph_flush_outbox;
use crate::worker::index_queue::index_pause;
use crate::worker::lane::{related_from_cache, thread_summaries_with_members};
use crate::worker::{MailRequest, WorkerEvent};

mod chores;
mod folders;
mod handle_read;
mod handle_send;
mod handle_write;
mod http;
mod index;
mod messages;
mod ops;
mod send;
mod snooze;
use crate::worker::outbox::{delete_queued, emit_outbox};

/// A label whose name the message leaves out is named from the
/// organization's list (#411) on its way to the reader, whether the
/// check was just made or comes from the cache, which keeps the header's.
fn name_labels(account: &AccountConfig, emit: &impl Fn(WorkerEvent), mut ev: WorkerEvent) {
    if let WorkerEvent::SenderChecked { check, .. } = &mut ev {
        if let Some(label) = check.label.as_mut() {
            crate::services::graph_labels::fill(account, label);
        }
    }
    emit(ev)
}

/// A Microsoft account's worker: the Graph state (folders, uid map, the
/// background index) and what it reports through.
struct GraphWorker<E> {
    account_id: u32,
    account: AccountConfig,
    rx: mpsc::UnboundedReceiver<MailRequest>,
    /// The UI's callback; events go out through [`name_labels`].
    emit: E,
    cache: Option<Cache>,
    state: GraphState,
    /// When the Junk / Trash auto-empty (#140) last ran for this worker.
    last_auto_empty: Option<std::time::Instant>,
}

/// Run a Microsoft account until the UI drops its sender.
pub(super) async fn run_graph(
    account_id: u32,
    account: AccountConfig,
    rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent),
) {
    GraphWorker::start(account_id, account, rx, emit)
        .await
        .run()
        .await;
}

impl<E: Fn(WorkerEvent)> GraphWorker<E> {
    /// Show the account and its cached folders, then fetch a token and the
    /// live folder list.
    async fn start(
        account_id: u32,
        account: AccountConfig,
        rx: mpsc::UnboundedReceiver<MailRequest>,
        emit: E,
    ) -> Self {
        let cache = Cache::open()
            .map_err(|e| tracing::warn!("cache unavailable: {e}"))
            .ok();
        let mut worker = GraphWorker {
            account_id,
            state: GraphState::new(&account),
            account,
            rx,
            emit,
            cache,
            last_auto_empty: None,
        };
        let account_id = worker.account_id;
        let account = &worker.account;
        let cache = &worker.cache;
        let emit = &|ev| name_labels(&worker.account, &worker.emit, ev);
        emit(WorkerEvent::account(account_id, account));

        // Cached folders immediately, then the live list.
        let cached_folders = cache
            .as_ref()
            .map(|c| c.load_folders(account_id))
            .unwrap_or_default();
        if !cached_folders.is_empty() {
            emit(WorkerEvent::Folders(cached_folders));
        }

        // Fetch a token and the folder list. A GOA token failure here is the one
        // users actually hit (signed out in GNOME Settings), so say that.
        if let Some(token) = graph_token(account, emit).await {
            refresh_graph_folders(&token, account_id, cache.as_ref(), &mut worker.state, emit)
                .await;
        }
        worker
    }

    /// Serve requests until the UI hangs up; between them, poll the Inbox
    /// and index the mailbox in the background.
    async fn run(mut self) {
        // Graph has no push channel (nothing like IMAP IDLE is available to this
        // token), so new mail arrives on a poll. The auto-fetch preference sets the
        // cadence when it's on; otherwise a quiet couple of minutes.
        let poll_secs = match crate::config::load_privacy().fetch_interval_secs {
            0 => 120,
            s => s.max(60),
        };
        let mut poll = tokio::time::interval(Duration::from_secs(poll_secs));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        poll.tick().await; // consume the interval's immediate first tick

        loop {
            // The background index (#429) only runs while nothing else is
            // asked: a request, then the poll, come first, and each step is a
            // single request so neither waits long behind one.
            // Offline or on a metered connection it waits; the request that
            // follows the change wakes this loop to look again (#442).
            let indexing = !self.state.backfill.is_empty()
                && crate::desktop::power::reaches_internet()
                && !crate::desktop::power::saving_data();
            let wait = self.state.backfill_wait;
            let account_id = self.account_id;
            let emit = &|ev| name_labels(&self.account, &self.emit, ev);
            let req = tokio::select! {
                biased;
                req = self.rx.recv() => match req {
                    Some(req) => req,
                    None => break,
                },
                _ = poll.tick() => {
                    if !crate::desktop::power::reaches_internet() {
                        continue;
                    }
                    graph_poll_inbox(account_id, &self.account, self.cache.as_ref(), &mut self.state, emit).await;
                    continue;
                }
                _ = index_pause(wait), if indexing => {
                    graph_backfill_step(&self.account, account_id, self.cache.as_ref(), &mut self.state, emit).await;
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
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
        match req {
            MailRequest::Locate { message_id } => {
                // No folders to search here beyond what the cache already
                // held: answer "not found" so the app can report the miss.
                emit(WorkerEvent::Located {
                    message_id: message_id.clone(),
                    hit: None,
                });
            }
            // Graph reports no mailbox size or limit to the user it signs in.
            MailRequest::Quota => emit(WorkerEvent::Quota(None)),
            MailRequest::FindKeywords => self.on_find_keywords().await,
            MailRequest::RefreshKeywords { .. } => self.on_refresh_keywords().await,
            // Not checked past the listing's newest page yet (#532).
            MailRequest::RefreshFlags { .. } => {
                emit(WorkerEvent::FlagsSynced { paths: Vec::new() })
            }
            // Graph has no equivalent of BODYSTRUCTURE in the shape this path
            // uses: attachments come out of the raw MIME it downloads, so
            // describing one costs the same as fetching it. Listing them
            // through /messages/{id}/attachments would work, but it is a
            // different call on a backend there is no account here to test
            // against — so a Microsoft account's gallery still shows what has
            // been downloaded rather than reaching back through the archive.
            MailRequest::ScanAttachments { folder_path } => {
                emit(WorkerEvent::AttachmentsScanned {
                    folder_path,
                    added: 0,
                    remaining: 0,
                });
            }
            // Cache-only, exactly like the IMAP path: assemble the conversation
            // from every folder's cached summaries.
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
            } => match graph_fetch_raw(&self.account, &mut self.state, &path, uid, emit).await {
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
            MailRequest::DeleteFolder { path, trash: _ } => self.on_delete_folder(path).await,
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
            // `sent_path` is unused: Graph's sendMail files the Sent copy itself.
            // Send Later (#145): into the Outbox until its time.
            MailRequest::Send {
                message,
                sent_path: _,
                ticket,
            } if message.send_at.is_some_and(|t| t > crate::datefmt::now()) => {
                self.on_send_later(message, ticket).await
            }
            MailRequest::Send {
                message,
                sent_path: _,
                ticket,
            } => self.on_send(message, ticket).await,
            MailRequest::LoadOutbox => emit_outbox(cache.as_ref(), account_id, emit),
            MailRequest::DeleteOutbox { id } => delete_queued(cache.as_ref(), account_id, id, emit),
            MailRequest::FlushOutbox { id } => {
                graph_flush_outbox(
                    cache.as_ref(),
                    account_id,
                    &self.account,
                    id,
                    &mut self.state,
                    emit,
                )
                .await;
            }
            MailRequest::RefreshUnread => self.on_refresh_unread().await,
            MailRequest::Reconnect => {
                if let Some(token) = graph_token(&self.account, emit).await {
                    refresh_graph_folders(
                        &token,
                        account_id,
                        cache.as_ref(),
                        &mut self.state,
                        emit,
                    )
                    .await;
                }
            }
            MailRequest::Settle { path, uids } => emit(WorkerEvent::MovesSettled { path, uids }),
            MailRequest::ExportRaw {
                token, path, uid, ..
            } => {
                let raw = graph_fetch_raw(&self.account, &mut self.state, &path, uid, emit).await;
                emit(WorkerEvent::RawExported { token, raw });
            }
            // Graph files a message posted to a folder as a draft; it is never
            // offered as a destination.
            MailRequest::ImportRaw { token, .. } => emit(WorkerEvent::RawImported {
                token,
                result: Err(i18n(
                    "A Microsoft account can't receive mail from another account",
                )),
            }),
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
        }
    }
}

#[cfg(test)]
mod tests;

/// Pins of current behaviour for the Graph error helpers (Area B, step B0).
#[cfg(test)]
mod characterization_tests {
    use super::*;

    use crate::worker::graph::http::{graph_err, graph_throttle_wait};

    fn status(code: u16, body: &str) -> ureq::Error {
        ureq::Error::Status(code, ureq::Response::new(code, "x", body).unwrap())
    }

    #[test]
    fn graph_errors_surface_the_nested_message_or_the_body() {
        assert_eq!(
            graph_err(status(
                403,
                r#"{"error":{"code":"ErrorAccessDenied","message":"Access is denied."}}"#
            )),
            "Microsoft Graph returned 403: Access is denied."
        );
        let long = "x".repeat(300);
        assert_eq!(
            graph_err(status(500, &long)),
            format!("Microsoft Graph returned 500: {}", "x".repeat(200))
        );
    }

    #[test]
    fn only_429_and_503_are_waited_on_five_seconds_by_default() {
        assert_eq!(
            graph_throttle_wait(&status(429, "")),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            graph_throttle_wait(&status(503, "")),
            Some(Duration::from_secs(5))
        );
        assert_eq!(graph_throttle_wait(&status(500, "")), None);
        assert_eq!(graph_throttle_wait(&status(404, "")), None);
    }
}
