//! Starting an IMAP account's worker: the passwords, what the cache already
//! knows (shown before any network), and the background queues.

use std::collections::{HashMap, HashSet, VecDeque};

use tokio::sync::mpsc;

use super::backfill::Backfill;
use super::connect::connect_and_list;
use super::fetch_mode::seed_fetch_mode;
use super::gmail::{gmail_labels_wanted, note_gmail_account};
use super::prefetch::gallery_folder;
use super::{Emit, ImapWorker};
use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::worker::auth::{load_or_migrate_password, load_smtp_password};
use crate::worker::{MailRequest, WorkerEvent};

impl<E: Emit> ImapWorker<E> {
    /// Resolve the passwords, show the account and its cached folders, and
    /// queue the background work the cache calls for. Connects at once only
    /// when there is no cached folder list to show.
    pub(super) async fn start(
        account_id: u32,
        mut account: AccountConfig,
        rx: mpsc::UnboundedReceiver<MailRequest>,
        emit: E,
    ) -> Self {
        resolve_passwords(&mut account, &emit).await;

        let cache = Cache::open()
            .map_err(|e| tracing::warn!("cache unavailable: {e}"))
            .ok();

        // Show the account + any cached folders immediately, before any network.
        emit(WorkerEvent::account(account_id, &account));

        let cached_folders = cache
            .as_ref()
            .map(|c| c.load_folders(account_id))
            .unwrap_or_default();
        let have_cached_folders = !cached_folders.is_empty();
        note_gmail_account(account_id, &account, &cached_folders);
        if have_cached_folders {
            emit(WorkerEvent::Folders(cached_folders));
        }

        // The worker stays alive even if connecting fails, so the UI can retry. With
        // cached folders we connect lazily (on the first request) so cached mail can
        // render without waiting on the network; with an empty cache we connect now
        // to bootstrap the folder list.
        let session = if have_cached_folders {
            None
        } else {
            connect_and_list(account_id, &account, cache.as_ref(), &emit).await
        };

        let push_enabled = account
            .push
            .unwrap_or_else(|| crate::config::load_privacy().push);
        let use_envelope = seed_fetch_mode(account_id, &account.email);
        let mut worker = ImapWorker {
            account_id,
            account,
            rx,
            emit,
            cache,
            session,
            prefetch: VecDeque::new(),
            body_prefetch: VecDeque::new(),
            body_emitted: HashSet::new(),
            backfill: VecDeque::new(),
            backfill_seen: HashSet::new(),
            offline: Default::default(),
            text_search: None,
            refs_repair: VecDeque::new(),
            label_fill: VecDeque::new(),
            backlog: VecDeque::new(),
            push_enabled,
            idle_folder: None,
            last_unread_sweep: None,
            last_auto_empty: None,
            last_list_clean: None,
            inbox_watch_spawned: !push_enabled,
            watchers: HashMap::new(),
            sweep_baseline: HashMap::new(),
            sweep_pending: Vec::new(),
            sweep_due: true,
            folder_refresh_due: false,
            pending_resync: false,
            use_envelope,
            outbox_flushed: false,
            local_tags_noticed: false,
        };
        worker.queue_known_folders();
        worker
    }

    /// Queue every known folder for background indexing so search covers the whole
    /// mailbox shortly after the first sync (like Apple Mail). The backfill skips
    /// UIDs already cached, so this is cheap on subsequent runs.
    fn queue_known_folders(&mut self) {
        let account_id = self.account_id;
        let cache = &self.cache;
        for f in cache
            .as_ref()
            .map(|c| c.load_folders(account_id))
            .unwrap_or_default()
        {
            if self.backfill_seen.insert(f.path.clone()) {
                // Messages indexed before Hylki fetched References thread by their
                // immediate parent alone — which for an incoming reply is usually
                // your own message, filed in Sent, so the link points outside the
                // folder and the reply starts a thread of its own.
                if cache
                    .as_ref()
                    .is_some_and(|c| !c.refs_repair_state(account_id, &f.path).1)
                {
                    self.refs_repair.push_back((f.id, f.path.clone()));
                }
                if gmail_labels_wanted(account_id)
                    && cache
                        .as_ref()
                        .is_some_and(|c| !c.label_fill_state(account_id, &f.path).1)
                {
                    self.label_fill.push_back((f.id, f.path.clone()));
                }
                self.backfill.push_back(Backfill {
                    folder_id: f.id,
                    gallery: gallery_folder(f.kind),
                    path: f.path,
                    remaining: None,
                    failures: 0,
                });
            }
        }
    }
}

/// Resolve the passwords off the main thread: migrate a legacy plaintext
/// password into the keyring (and strip it from disk), otherwise load it
/// from the keyring; then take what GNOME Online Accounts holds.
async fn resolve_passwords(account: &mut AccountConfig, emit: &impl Fn(WorkerEvent)) {
    load_or_migrate_password(account);
    load_smtp_password(account);
    // An account from GNOME Online Accounts takes its password from GOA at
    // every connect, so one changed in GNOME Settings reaches Hylki without
    // being typed again (#254). The keyring copy is kept for a GOA that
    // cannot answer yet; an imported account shows no password field, so
    // without GOA there would be no way to fix a missing one (#17).
    if !account.oauth {
        if let Some(goa_id) = account.goa_id.clone() {
            let (imap, smtp) =
                tokio::task::spawn_blocking(move || crate::services::goa::mail_passwords(&goa_id))
                    .await
                    .unwrap_or((None, None));
            if let Some(pw) = imap.filter(|pw| *pw != account.password) {
                let _ = crate::config::store_password(&account.email, &pw);
                account.password = pw;
            }
            if let (true, Some(pw)) = (account.smtp_separate, smtp) {
                if pw != account.smtp_password {
                    let _ = crate::config::store_smtp_password(&account.email, &pw);
                    account.smtp_password = pw;
                }
            }
            if account.password.is_empty() {
                emit(WorkerEvent::error(format!(
                    "GNOME Online Accounts has no password for {}. Open Settings → Online \
                         Accounts and sign in again.",
                    account.email
                )));
            }
        }
    }
}
