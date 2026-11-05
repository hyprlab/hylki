//! Proton Mail accounts, over the bundled `hylki-proton` helper.
//!
//! Proton keeps mail encrypted to keys only its own libraries open, and
//! without Proton Mail Bridge offers no IMAP or SMTP. The helper
//! ([`crate::worker::helper`]) runs Proton's Go client library, one
//! process per request, and signs in with the session kept in the keyring
//! (the password is never stored). Mail comes back decrypted as finished
//! MIME, so it reads through the same parser as every other account's.
//!
//! Proton spends a refresh token every time it refreshes a session, and
//! the helper hands back the session as it stands after every call. That
//! session is the account's live one ([`crate::config::LiveSession`]): its
//! calls take turns, and each new one goes to the keyring by way of
//! [`WorkerEvent::HelperSession`].
//!
//! Message uids are the pinned hash of Proton's ids, which stay the same
//! across moves. A listing carries the Message-ID, so mail threads at once;
//! the References follow with the body. There is no push: a poll asks for
//! what changed since the last event, anywhere in the mailbox.
//!
//! What the helper cannot do is refused out loud rather than faked:
//! deleting for good, folder changes, drafts, and importing mail. Tags are
//! kept on this computer only, as on POP3.

use std::time::Duration;

use tokio::sync::mpsc;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};
use crate::worker::auth::resolve_keyring_passwords;
use crate::worker::index_queue::index_pause;
use crate::worker::lane::{related_from_cache, thread_summaries_with_members};
use crate::worker::offline::Offline;
use crate::worker::outbox::{delete_queued, emit_outbox};
use crate::worker::strip::strip_refused;
use crate::worker::{MailRequest, WorkerEvent};

mod folders;
mod handle_read;
mod handle_send;
mod handle_write;
mod index;
mod messages;
mod offline;
mod ops;
mod send;
mod wire;

pub(super) use wire::SPEC;

use handle_write::permanent_delete_text;
use index::{proton_backfill_step, ProtonState};
use offline::proton_offline_step;
use ops::proton_poll;
use send::proton_flush_outbox;

/// A Proton account's worker: the folder and uid maps, the event log, the
/// background index, and what it reports through.
struct ProtonWorker<E> {
    account_id: u32,
    account: AccountConfig,
    rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: E,
    cache: Option<Cache>,
    state: ProtonState,
    /// The session last handed to the app to keep, to tell a rotated one.
    kept: String,
    /// The download of the account's offline period (#536).
    offline: Offline,
}

/// Run a Proton account until the UI drops its sender.
pub(super) async fn run_proton(
    account_id: u32,
    account: AccountConfig,
    rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent),
) {
    ProtonWorker::start(account_id, account, rx, emit)
        .await
        .run()
        .await;
}

/// Hand a rotated session to the app to keep, when the live one is no
/// longer the one last handed over. The worker calls this after every
/// step that ran the helper, failed or not.
fn keep_rotated_session(
    state: &ProtonState,
    email: &str,
    kept: &mut String,
    emit: &impl Fn(WorkerEvent),
) {
    let latest = state.live.latest();
    if latest.is_empty() || latest == *kept {
        return;
    }
    *kept = latest;
    emit(WorkerEvent::HelperSession {
        email: email.to_string(),
    });
}

/// The connection test: the stored session resumed with a one-header sync,
/// or, with none, a sign-in with the password whose new session is ended
/// again at once, so a test leaves no sign-in behind. An account that asks
/// for more (a code, a mailbox password, a CAPTCHA) says so; signing in for
/// good is the editor's.
pub(super) async fn test_proton(account: &AccountConfig) -> Result<(), String> {
    use crate::worker::helper::{
        login, process_transport, HelperClient, HelperError, HelperLogin, Object,
    };
    let account = account.clone();
    // A session the editor holds is not stored yet: saving the account
    // stores whatever it has become by then.
    let from_keyring = account.helper_session.is_empty();
    let session = if from_keyring {
        crate::config::read_session(&account.email)
            .ok()
            .flatten()
            .unwrap_or_default()
    } else {
        account.helper_session.clone()
    };
    let tested = index::run(move || {
        let transport = process_transport(SPEC);
        if !session.is_empty() || crate::config::latest_session(&account.email).is_some() {
            let client = HelperClient::shared(
                SPEC,
                transport,
                &account.email,
                crate::config::live_session(&account.email),
                &session,
            )?;
            let before = client.session();
            let probed = ops::probe(&client);
            if from_keyring && client.session() != before {
                crate::config::persist_live_session(&account.email);
            }
            return probed;
        }
        if account.password.is_empty() {
            return Err(HelperError::new("not_signed_in"));
        }
        let sign_in = HelperLogin {
            email: account.email.clone(),
            password: account.password.clone(),
            ..HelperLogin::default()
        };
        let fresh = login(&transport, SPEC, &sign_in)?;
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

impl<E: Fn(WorkerEvent)> ProtonWorker<E> {
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
        let mut worker = ProtonWorker {
            account_id,
            state: ProtonState::new(&account),
            kept: account.helper_session.clone(),
            account,
            rx,
            emit,
            cache,
            offline: Offline::default(),
        };
        worker.sync_folders().await;
        worker.keep_session();
        worker
    }

    /// The account's rotated session, to the app to keep.
    fn keep_session(&mut self) {
        keep_rotated_session(&self.state, &self.account.email, &mut self.kept, &self.emit);
    }

    /// The folder tree, the Inbox and what changed, with a failure said.
    async fn sync_folders(&mut self) {
        let account_id = self.account_id;
        let emit = &self.emit;
        if let Err(e) = proton_poll(
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

    /// Serve requests until the UI hangs up; between them, poll for changes
    /// and index the mailbox in the background.
    async fn run(mut self) {
        // No push: the helper offers no event channel, only the event log
        // a poll reads. The auto-fetch preference sets the cadence when it's
        // on; otherwise a quiet couple of minutes, as for Tuta.
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
                _ = poll.tick() => {
                    if crate::desktop::power::reaches_internet() {
                        if let Err(e) = proton_poll(&self.account, account_id, self.cache.as_ref(), &mut self.state, &self.emit).await {
                            tracing::info!("[account {account_id}] poll: {}", e.code);
                        }
                    }
                    self.keep_session();
                    continue;
                }
                _ = index_pause(wait), if indexing => {
                    proton_backfill_step(&self.account, account_id, self.cache.as_ref(), &mut self.state, &self.emit).await;
                    self.keep_session();
                    continue;
                }
                _ = index_pause(offline_wait), if keeping => {
                    proton_offline_step(&self.account, account_id, self.cache.as_ref(), &self.state, &mut self.offline).await;
                    self.keep_session();
                    continue;
                }
            };
            self.handle(req).await;
            self.keep_session();
        }
    }

    /// The handler for each kind of request.
    async fn handle(&mut self, req: MailRequest) {
        // A folder the user opened, rather than a background refresh.
        let user = matches!(req, MailRequest::LoadMessages { .. });
        let account_id = self.account_id;
        match req {
            // The helper has no search: the search of what is on this
            // computer covers what is cached.
            MailRequest::AskServer(ask) => ask.refuse(
                account_id,
                i18n("Hylki can't search on Proton's servers: search finds what is on this computer"),
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
            // Proton's labels are listed as folders, not keywords: every
            // tag is Hylki's own.
            MailRequest::FindKeywords => (self.emit)(WorkerEvent::KeywordsFound(Vec::new())),
            MailRequest::RefreshKeywords { .. } => {
                (self.emit)(WorkerEvent::KeywordsSynced { paths: Vec::new() })
            }
            // Read marks and stars come with each poll's changes.
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
            MailRequest::SetFlagged { path, uid, flagged } => {
                self.on_set_flagged(path, uid, flagged).await
            }
            // Proton's labels are folders here, so a tag is Hylki's own, by
            // Message-ID where the row has one so it follows the message.
            MailRequest::SetKeyword {
                path,
                uid,
                message_id,
                keyword,
                add,
            } => {
                if let Some(c) = self.cache.as_ref() {
                    if message_id.is_empty() {
                        c.set_keyword(account_id, &path, uid, &keyword, add);
                    } else {
                        c.set_local_tag(account_id, &message_id, &keyword, add);
                    }
                }
                (self.emit)(WorkerEvent::TagSettled { path, uid, keyword });
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
            MailRequest::EmptyFolder { .. } => {
                (self.emit)(WorkerEvent::error(permanent_delete_text()))
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
            // Snoozing is not offered on a Proton account (no folder can be
            // made to keep snoozed mail in); a stray request is spent.
            MailRequest::Unsnooze { message_ids, .. } => (self.emit)(WorkerEvent::Unsnoozed {
                message_ids,
                done: true,
            }),
            MailRequest::CreateFolder { .. }
            | MailRequest::RenameFolder { .. }
            | MailRequest::DeleteFolder { .. } => self.on_folder_change(),
            MailRequest::SetHiddenFolders { paths } => {
                self.state.hidden = paths;
                self.sync_folders().await;
            }
            MailRequest::SaveDraft {
                message,
                autosave,
                template,
                ..
            } => self.on_save_draft(message, autosave, template),
            // Send Later (#145): into the Outbox until its time.
            MailRequest::Send {
                message, ticket, ..
            } if message.send_at.is_some_and(|t| t > crate::datefmt::now()) => {
                self.on_send_later(message, ticket)
            }
            MailRequest::Send {
                message, ticket, ..
            } => self.on_send(message, ticket).await,
            MailRequest::LoadOutbox => emit_outbox(self.cache.as_ref(), account_id, &self.emit),
            MailRequest::DeleteOutbox { id } => {
                delete_queued(self.cache.as_ref(), account_id, id, &self.emit)
            }
            MailRequest::FlushOutbox { id } => {
                proton_flush_outbox(
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
            // The helper cannot store a message it did not send or receive;
            // a Proton account is never offered as a destination.
            MailRequest::ImportRaw { token, .. } => (self.emit)(WorkerEvent::RawImported {
                token,
                result: Err(i18n("A Proton account can't receive mail from another account")),
            }),
            MailRequest::DeleteAttachment {
                message_id,
                name,
                size,
                ..
            } => strip_refused(
                &self.emit,
                i18n("Proton cannot change a message on the server, so the attachment was not removed"),
                message_id,
                name,
                size,
            ),
        }
    }
}

#[cfg(test)]
mod tests;
