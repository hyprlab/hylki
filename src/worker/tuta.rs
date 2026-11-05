//! Tuta accounts, over the bundled `hylki-tuta` helper.
//!
//! Tuta keeps mail end-to-end encrypted on its servers and offers no IMAP,
//! SMTP or JMAP: only its own client library can sign in and decrypt. That
//! library runs in a separate helper program ([`crate::worker::helper`]),
//! one process per request, which signs in with the session kept in the
//! keyring (the password is never stored). Mail comes back decrypted and is
//! put together as MIME, so it reads through the same parser as every
//! other account's.
//!
//! Like Microsoft 365's, message uids are the pinned hash of the server's
//! ids (here the compound `listId/elementId`), and the uid → id map is
//! rebuilt from every listing. A listing has no Message-ID or References,
//! so mail threads by a synthetic `tuta-conv:` token (stripped before any
//! wire header in `build_email`) until a read body fills them in.
//!
//! Folders are made, renamed, moved and deleted on the server; drafts and
//! templates are Tuta drafts; tags are Tuta labels (see `labels`); mail
//! is erased for good from the Trash or Spam, as the server allows. Stars
//! are kept on this computer only, as on POP3: Tuta has none. Mail from
//! another account is imported the way Tuta's own app imports a mail file,
//! re-encrypted for this account by the helper. New mail is pushed over
//! Tuta's event bus while the helper's `watch` runs (see `push`), and
//! polled for while it does not.

use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};
use crate::worker::auth::resolve_keyring_passwords;
use crate::worker::index_queue::index_pause;
use crate::worker::helper::HelperError;
use crate::worker::lane::{related_from_cache, thread_summaries_with_members};
use crate::worker::offline::Offline;
use crate::worker::outbox::{delete_queued, emit_outbox};
use crate::worker::strip::strip_refused;
use crate::worker::{MailRequest, WorkerEvent};

mod folders;
mod handle_folders;
mod handle_read;
mod handle_send;
mod handle_write;
mod index;
mod labels;
mod messages;
mod mime;
mod offline;
mod ops;
mod push;
mod send;
mod wire;

pub(super) use wire::SPEC;

use index::{tuta_backfill_step, tuta_load_folder, TutaState};
use offline::tuta_offline_step;
use ops::{emit_folder, tuta_poll};
use push::{spawn_tuta_push, PushSignal};
use send::tuta_flush_outbox;

/// While push is live, the poll still runs this often, as the safety net
/// for a change the event bus did not report.
const PUSHED_POLL: Duration = Duration::from_secs(15 * 60);

/// A Tuta account's worker: the folder and uid maps, the background index,
/// and what it reports through.
struct TutaWorker<E> {
    account_id: u32,
    account: AccountConfig,
    rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: E,
    cache: Option<Cache>,
    state: TutaState,
    /// The download of the account's offline period (#536).
    offline: Offline,
    /// The push thread's signals (see `push`).
    push_rx: mpsc::UnboundedReceiver<PushSignal>,
    /// Its sender, until the thread is started with it.
    push_tx: Option<mpsc::UnboundedSender<PushSignal>>,
    /// Whether the helper's `watch` is connected now.
    push_live: bool,
    /// When the Inbox was last polled.
    polled: Instant,
    /// The folder the user last opened, refreshed along with the Inbox
    /// when a change was pushed without saying where.
    open: Option<String>,
}

/// Run a Tuta account until the UI drops its sender.
pub(super) async fn run_tuta(
    account_id: u32,
    account: AccountConfig,
    rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent),
) {
    TutaWorker::start(account_id, account, rx, emit)
        .await
        .run()
        .await;
}

/// The connection test: the stored session resumed with a one-header sync,
/// or, with none, a sign-in with the password whose new session is ended
/// again at once, so a test leaves no sign-in behind. An account that asks
/// for a second factor says so; signing in for good is the editor's.
pub(super) async fn test_tuta(account: &AccountConfig) -> Result<(), String> {
    use crate::worker::helper::{login, process_transport, HelperClient, Object};
    let account = account.clone();
    let session = match account.helper_session.as_str() {
        "" => crate::config::read_session(&account.email)
            .ok()
            .flatten()
            .unwrap_or_default(),
        held => held.to_string(),
    };
    let tested = index::run(move || {
        let transport = process_transport(SPEC);
        if !session.is_empty() {
            let client = HelperClient::new(SPEC, transport, &account.email, &session)?;
            return wire::sync(&client, None, 1, 0).map(|_| ());
        }
        if account.password.is_empty() {
            return Err(HelperError::new("not_signed_in"));
        }
        let fresh = login(&transport, SPEC, &account.email, &account.password, None)?;
        let client = HelperClient::new(SPEC, transport, &account.email, &fresh)?;
        if let Err(e) = client.request("revoke", Object::new()) {
            tracing::warn!(
                "could not end the test sign-in of {}: {}",
                account.email,
                e.code
            );
        }
        Ok(())
    })
    .await;
    tested.map_err(|e| e.message())
}

impl<E: Fn(WorkerEvent)> TutaWorker<E> {
    /// Show the account and its cached folders, read the session from the
    /// keyring, then sync the folder tree and the Inbox.
    async fn start(
        account_id: u32,
        mut account: AccountConfig,
        rx: mpsc::UnboundedReceiver<MailRequest>,
        emit: E,
    ) -> Self {
        let cache = Cache::open()
            .map_err(|e| tracing::warn!("cache unavailable: {e}"))
            .ok();
        emit(WorkerEvent::account(account_id, &account));
        emit(WorkerEvent::Hierarchy(
            crate::models::FolderHierarchy::slashed(true),
        ));
        let cached_folders = cache
            .as_ref()
            .map(|c| c.load_folders(account_id))
            .unwrap_or_default();
        if !cached_folders.is_empty() {
            emit(WorkerEvent::Folders(cached_folders));
        }
        resolve_keyring_passwords(&mut account, &emit).await;
        let (push_tx, push_rx) = mpsc::unbounded_channel();
        let mut worker = TutaWorker {
            account_id,
            state: TutaState::new(&account),
            account,
            rx,
            emit,
            cache,
            offline: Offline::default(),
            push_rx,
            push_tx: Some(push_tx),
            push_live: false,
            polled: Instant::now(),
            open: None,
        };
        worker.sync_folders().await;
        worker.start_push();
        worker
    }

    /// Start the push thread, when the push preference allows it and the
    /// account has a sign-in to watch with. It lives as long as the worker.
    fn start_push(&mut self) {
        let wanted = self
            .account
            .push
            .unwrap_or_else(|| crate::config::load_privacy().push);
        if !wanted {
            return;
        }
        let Ok(client) = self.state.client(&self.account) else {
            return;
        };
        if let Some(tx) = self.push_tx.take() {
            spawn_tuta_push(client, None, tx);
        }
    }

    /// The folder tree and the Inbox, with a failure said.
    async fn sync_folders(&mut self) {
        let account_id = self.account_id;
        let emit = &self.emit;
        if let Err(e) = tuta_poll(
            &self.account,
            account_id,
            self.cache.as_ref(),
            &mut self.state,
            emit,
        )
        .await
        {
            tracing::warn!("[account {account_id}] could not list folders: {}", e.code);
            emit(WorkerEvent::net_error(i18n_f(
                "Could not list folders: {e}",
                &[("e", &e.message())],
            )));
        }
    }

    /// Serve requests until the UI hangs up; between them, follow push,
    /// poll the Inbox while push is down, and index the mailbox in the
    /// background.
    async fn run(mut self) {
        // The auto-fetch preference sets the poll's cadence when it's on;
        // otherwise a quiet couple of minutes, as for Microsoft 365.
        let poll_secs = match crate::config::load_privacy().fetch_interval_secs {
            0 => 120,
            s => s.max(60),
        };
        let mut poll = tokio::time::interval(Duration::from_secs(poll_secs));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        poll.tick().await; // consume the interval's immediate first tick

        loop {
            // The background index (#429) runs only while nothing else is
            // asked, a helper call a step; offline or metered it waits (#442).
            let net =
                crate::desktop::power::reaches_internet() && !crate::desktop::power::saving_data();
            let indexing = !self.state.backfill.is_empty() && net;
            let wait = self.state.backfill_wait;
            // Then the account's offline period (#536), once the index is in.
            let keeping = self.state.backfill.is_empty() && net && Offline::wanted(&self.account);
            let offline_wait = self.offline.wait();
            let account_id = self.account_id;
            let req = tokio::select! {
                biased;
                req = self.rx.recv() => match req {
                    Some(req) => req,
                    None => break,
                },
                Some(signal) = self.push_rx.recv() => {
                    self.on_pushed(signal).await;
                    continue;
                }
                _ = poll.tick() => {
                    let pushed = self.push_live && self.polled.elapsed() < PUSHED_POLL;
                    if !pushed && crate::desktop::power::reaches_internet() {
                        self.poll().await;
                    }
                    continue;
                }
                _ = index_pause(wait), if indexing => {
                    tuta_backfill_step(&self.account, account_id, self.cache.as_ref(), &mut self.state, &self.emit).await;
                    continue;
                }
                _ = index_pause(offline_wait), if keeping => {
                    tuta_offline_step(&self.account, account_id, self.cache.as_ref(), &self.state, &mut self.offline).await;
                    continue;
                }
            };
            self.handle(req).await;
        }
    }

    /// One quiet poll of the folder tree and the Inbox.
    async fn poll(&mut self) {
        let account_id = self.account_id;
        self.polled = Instant::now();
        if let Err(e) = tuta_poll(
            &self.account,
            account_id,
            self.cache.as_ref(),
            &mut self.state,
            &self.emit,
        )
        .await
        {
            tracing::info!("[account {account_id}] poll: {}", e.code);
        }
    }

    /// What the push thread said, with whatever else it said meanwhile: a
    /// burst of changes is one sync of each folder it named. A change that
    /// named no folder, and a reconnect, which may have missed some, sync
    /// the Inbox, the folder tree and the folder open in the list.
    async fn on_pushed(&mut self, first: PushSignal) {
        let mut signals = vec![first];
        while let Ok(signal) = self.push_rx.try_recv() {
            signals.push(signal);
        }
        let mut folders: Vec<String> = Vec::new();
        let mut everything = false;
        let mut tree = false;
        for signal in signals {
            match signal {
                // Connected: whatever changed while it was not was missed.
                PushSignal::Live => {
                    self.push_live = true;
                    everything = true;
                }
                PushSignal::Ended => self.push_live = false,
                PushSignal::Mail(folder) if folder.is_empty() => everything = true,
                PushSignal::Mail(folder) => {
                    if !folders.contains(&folder) {
                        folders.push(folder);
                    }
                }
                PushSignal::Folders => tree = true,
            }
        }
        if everything || tree {
            self.poll().await;
        }
        let inbox = self.state.path_of(crate::models::FolderKind::Inbox);
        let mut paths: Vec<String> = folders
            .iter()
            .filter_map(|tuta_id| {
                self.state
                    .folders
                    .iter()
                    .find(|(_, (_, id))| id == tuta_id)
                    .map(|(path, _)| path.clone())
            })
            .collect();
        if let Some(open) = self.open.clone().filter(|o| everything && !paths.contains(o)) {
            paths.push(open);
        }
        for path in paths {
            // The poll just listed the Inbox.
            if (everything || tree) && inbox.as_ref() == Some(&path) {
                continue;
            }
            self.refresh(&path).await;
        }
    }

    /// List a folder's newest page again, quietly.
    async fn refresh(&mut self, path: &str) {
        let account_id = self.account_id;
        let Some((folder_id, _)) = self.state.folders.get(path).cloned() else {
            return;
        };
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
            Ok(messages) => emit_folder(folder_id, messages, &self.state, &self.emit),
            Err(e) => tracing::info!("[account {account_id}] pushed: {path}: {}", e.code),
        }
    }

    /// The handler for each kind of request.
    async fn handle(&mut self, req: MailRequest) {
        // A folder the user opened, rather than a background refresh.
        let user = matches!(req, MailRequest::LoadMessages { .. });
        let account_id = self.account_id;
        match req {
            // Tuta's servers hold only ciphertext and cannot search it; the
            // search of what is on this computer covers what is cached.
            MailRequest::AskServer(ask) => ask.refuse(
                account_id,
                i18n("Tuta's servers cannot search encrypted mail: search finds what is on this computer"),
            ),
            MailRequest::FindText { gen, .. } => (self.emit)(WorkerEvent::TextFound {
                gen,
                hits: Vec::new(),
                done: true,
            }),
            // No folders to search beyond what the cache held: "not found",
            // so the app can report the miss.
            MailRequest::Locate { message_id } => (self.emit)(WorkerEvent::Located {
                message_id,
                hit: None,
            }),
            // The helper reports no mailbox size or limit.
            MailRequest::Quota => (self.emit)(WorkerEvent::Quota(None)),
            // Tags are Tuta labels (see `labels`), which every sync lists
            // and every listing names on its rows: there is nothing more
            // to ask for.
            MailRequest::FindKeywords => self.on_find_keywords(),
            MailRequest::RefreshKeywords { .. } => {
                (self.emit)(WorkerEvent::KeywordsSynced { paths: Vec::new() })
            }
            // Read state comes with each listing; there is nothing else.
            MailRequest::RefreshFlags { .. } => {
                (self.emit)(WorkerEvent::FlagsSynced { paths: Vec::new() })
            }
            // Attachments come out of the decrypted message, so describing
            // one costs as much as fetching it: the gallery shows what has
            // been downloaded.
            MailRequest::ScanAttachments { folder_path } => {
                (self.emit)(WorkerEvent::AttachmentsScanned {
                    folder_path,
                    added: 0,
                    remaining: 0,
                })
            }
            MailRequest::LoadRelated { message_id, ids } => {
                let messages = self
                    .cache
                    .as_ref()
                    .map(|c| related_from_cache(c, account_id, &ids))
                    .unwrap_or_default();
                (self.emit)(WorkerEvent::Related {
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
                (self.emit)(WorkerEvent::ThreadSummaries { summaries });
            }
            MailRequest::LoadMessages { folder_id, path }
            | MailRequest::SyncFolder { folder_id, path } => {
                if user {
                    self.open = Some(path.clone());
                }
                self.on_load_messages(folder_id, path, user).await
            }
            MailRequest::LoadBody {
                message_id,
                path,
                uid,
            } => self.on_load_bodies(vec![(message_id, uid)], path).await,
            MailRequest::LoadBodies { items, path } => self.on_load_bodies(items, path).await,
            MailRequest::LoadSource { path, uid, .. } => match self.raw_of(&path, uid).await {
                Ok(raw) => (self.emit)(WorkerEvent::Source {
                    text: String::from_utf8_lossy(&raw).into_owned(),
                }),
                Err(e) => (self.emit)(WorkerEvent::net_error(i18n_f(
                    "Could not load source: {e}",
                    &[("e", &e)],
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
            // Tuta mail has no star or flag: it is kept on this computer
            // only, as on POP3, and the listing's merge keeps it.
            MailRequest::SetFlagged { path, uid, flagged } => {
                if let Some(c) = self.cache.as_ref() {
                    c.set_starred(account_id, &path, uid, flagged);
                }
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
            MailRequest::MoveMessage { path, uid, dest } => {
                self.move_to(path, vec![uid], dest, |e| {
                    i18n_f("Could not move message: {e}", &[("e", e)])
                })
                .await
            }
            MailRequest::MarkSpam { path, uid, dest } => self.on_mark_spam(path, uid, dest).await,
            MailRequest::MarkHam { path, uid, dest } => {
                self.move_to(path, vec![uid], dest, |e| {
                    i18n_f("Could not mark as not spam: {e}", &[("e", e)])
                })
                .await
            }
            MailRequest::MarkHamMany { path, uids, dest } => {
                let len = uids.len().to_string();
                self.move_to(path, uids, dest, |e| {
                    i18n_f(
                        "Could not mark {len} messages as not spam: {e}",
                        &[("len", &len), ("e", e)],
                    )
                })
                .await;
                (self.emit)(WorkerEvent::BulkComplete);
            }
            MailRequest::MoveMessages { path, uids, dest } => {
                self.move_to(path, uids, dest, |e| {
                    i18n_f("Could not move messages: {e}", &[("e", e)])
                })
                .await;
                (self.emit)(WorkerEvent::BulkComplete);
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
            MailRequest::DeleteFolder { path, .. } => self.on_delete_folder(path).await,
            MailRequest::SetHiddenFolders { paths } => {
                self.state.hidden = paths;
                self.sync_folders().await;
            }
            MailRequest::SaveDraft {
                message,
                path,
                autosave,
                template,
                ..
            } => self.on_save_draft(message, path, autosave, template).await,
            // Send Later (#145): into the Outbox until its time.
            MailRequest::Send {
                message, ticket, ..
            } if message.send_at.is_some_and(|t| t > crate::datefmt::now()) => {
                self.on_send_later(message, ticket).await
            }
            MailRequest::Send {
                message, ticket, ..
            } => self.on_send(message, ticket).await,
            MailRequest::LoadOutbox => emit_outbox(self.cache.as_ref(), account_id, &self.emit),
            MailRequest::DeleteOutbox { id } => {
                delete_queued(self.cache.as_ref(), account_id, id, &self.emit)
            }
            MailRequest::FlushOutbox { id } => {
                tuta_flush_outbox(
                    self.cache.as_ref(),
                    account_id,
                    &self.account,
                    id,
                    &mut self.state,
                    &self.emit,
                )
                .await
            }
            MailRequest::RefreshUnread => self.on_refresh_unread().await,
            MailRequest::Reconnect => self.sync_folders().await,
            // Only On This Computer imports mail (#552).
            MailRequest::ImportMail { .. } => {}
            MailRequest::Settle { path, uids } => {
                (self.emit)(WorkerEvent::MovesSettled { path, uids })
            }
            MailRequest::ExportRaw {
                token, path, uid, ..
            } => {
                let raw = self.raw_of(&path, uid).await;
                (self.emit)(WorkerEvent::RawExported { token, raw });
            }
            MailRequest::ImportRaw {
                token,
                path,
                raw,
                seen,
                flagged,
            } => self.on_import_raw(token, path, raw, seen, flagged).await,
            MailRequest::DeleteAttachment {
                message_id,
                name,
                size,
                ..
            } => strip_refused(
                &self.emit,
                i18n("Tuta cannot change a message on the server, so the attachment was not removed"),
                message_id,
                name,
                size,
            ),
        }
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod handler_tests;
