//! The IMAP backend. One connection serves the requests in order, with the
//! reader's loads first; between requests it prefetches, indexes, sweeps
//! unread counts and sits in IDLE. More connections watch folders for new
//! mail.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use async_imap::Session;
use async_native_tls::TlsStream;
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::models::{Folder, FolderKind};
use crate::worker::imap::backfill::Backfill;
use crate::worker::imap::ladder::Idle;
use crate::worker::imap::send::flush_outbox;
use crate::worker::imap::stream::ImapIo;
use crate::worker::imap::watch::{watch_folder, FolderWatch};
use crate::worker::preempt::reorder_reader_loads;
use crate::worker::{MailRequest, WorkerEvent};

pub(super) mod backfill;
mod bodies;
pub(super) mod connect;
mod dispatch;
pub(super) mod fetch_mode;
mod folders;
pub(super) mod gmail;
mod handle_read;
mod handle_send;
mod handle_write;
mod idle;
pub(super) mod keywords;
mod ladder;
#[cfg(test)]
mod live_tests;
mod mutate;
mod prefetch;
mod preview_fetch;
mod retry;
pub(super) mod send;
mod setup;
mod snooze;
mod stream;
mod strip_attachment;
mod structure;
mod summary;
mod sweep;
mod sync;
pub(super) mod watch;
mod wire;

/// Number of most-recent messages to fetch attachment info (BODYSTRUCTURE) for;
/// older messages get an envelope-only index row and resolve attachments on open.
pub(in crate::worker) const PAGE_SIZE: u32 = 50;

/// Number of most-recent messages shown instantly when a folder is first opened
/// (never synced before). The rest of the folder is indexed in the background so
/// browsing is immediate and search fills in shortly after — like Apple Mail.
pub(in crate::worker) const FIRST_PAGE: u32 = 200;

/// The threading headers to pull alongside the ENVELOPE.
///
/// The IMAP ENVELOPE carries In-Reply-To but *not* References, and In-Reply-To
/// alone only links a reply to its immediate parent. When that parent isn't in
/// the folder being listed — the usual case, since your own replies are filed in
/// Sent — the chain breaks and each incoming message starts a thread of its own,
/// which is why conversations hardly ever grouped (#21). References carries the
/// whole ancestry, so one of its ids is almost always present locally.
///
/// The priority headers ride along for the list's Importance column (#334):
/// the ENVELOPE has none of them either.
pub(in crate::worker) const REFS_FETCH_ITEM: &str =
    " BODY.PEEK[HEADER.FIELDS (REFERENCES X-PRIORITY IMPORTANCE PRIORITY)]";

/// Background index backfill: how many messages to fetch per idle drain step.
/// Bigger = fewer round-trips; smaller = more responsive to interleaved requests.
pub(in crate::worker) const BACKFILL_CHUNK: usize = 1_000;

/// Pre-download attachments only for this many of the most recent messages;
/// older attachment messages download on demand (with a spinner) when opened.
pub(in crate::worker) const PREFETCH_LIMIT: usize = 25;

/// How many messages one attachment scan describes. One FETCH per chunk, so
/// bigger means fewer round trips; small enough that a chunk lands quickly and
/// the gallery fills in visibly while the user is looking at it.
const SCAN_CHUNK: u32 = 200;

/// Most FETCHes one scan request will spend, counting the extra ones a failing
/// batch costs as it is halved. Bounds the work a folder full of responses we
/// cannot parse can demand in one pass; whatever is left keeps its place in the
/// queue and is picked up by the next.
const SCAN_SPLIT_BUDGET: u32 = 64;

/// How many messages one body fetch asks for.
///
/// `BODY.PEEK[]` returns whole messages, attachments included, so a whole
/// conversation in one command — up to `THREAD_MEMBER_LIMIT` members — can be
/// tens of megabytes arriving as a single response: nothing on screen until all
/// of it lands, and nothing to show for it if the connection drops on the way.
/// Ten still cuts the round trips to a fifth of what asking one at a time cost,
/// while the reader fills in as each batch arrives.
const BODY_FETCH_BATCH: usize = 10;

/// Pre-download message *bodies* for this many of the most recent messages in a
/// synced folder, so new mail opens instantly with no network wait. Bodies are
/// small, so this stays cheap; older messages load on demand.
pub(in crate::worker) const PREFETCH_BODY_LIMIT: usize = 50;

/// Floor between two unread-count sweeps out of the IDLE loop, so a burst of
/// new mail (one `Refreshed` wake per delivery) doesn't STATUS the whole folder
/// tree over and over. Explicit [`MailRequest::RefreshUnread`]s are never
/// throttled — a refresh the user asked for always runs.
const UNREAD_SWEEP_MIN: Duration = Duration::from_secs(60);

/// How many leased per-folder IDLE watcher connections one account may hold at
/// once (see [`watch_folder`]). The Inbox's permanent watcher and the main
/// session sit outside this cap, so the ceiling is six connections per account
/// — comfortably under every major provider's per-user cap (Gmail 15, Outlook
/// 20, Dovecot's default 10). When the pool is full, fresh activity evicts the
/// stalest lease; folders without a watcher stay covered by the unread sweep.
pub(in crate::worker) const WATCHER_LIMIT: usize = 4;

/// How long one burst of activity keeps a folder's watcher alive. Every touch
/// — the sweep noticing the folder changed, the watcher itself seeing server
/// activity, the user opening the folder — restarts the clock; a folder left
/// quiet for the whole hour logs its watcher out and falls back to sweep
/// coverage until it changes again.
pub(in crate::worker) const WATCH_LEASE: Duration = Duration::from_secs(3600);

/// How long a watcher lets IDLE sit before verifying the count itself. IDLE
/// still wakes it instantly where servers announce changes, but a flag flipped
/// from another client is exactly what some servers (iCloud, observed
/// 2026-08-28) never announce to an idling session — so the watcher recounts
/// every minute regardless and reports only when the number moved. One UID
/// SEARCH per watcher per minute; the ceiling on a watched folder's staleness
/// whatever the server chooses to say.
pub(in crate::worker) const WATCH_VERIFY: Duration = Duration::from_secs(60);

/// How often a watcher waiting out a network outage looks again: no network
/// traffic, only a flag read, so it can be short (#442).
pub(in crate::worker) const NETWORK_WAIT: Duration = Duration::from_secs(5);

/// How long ending an IDLE (the DONE handshake) may take before the connection
/// is declared wedged and dropped (#91). Unbounded, a server that never answers
/// DONE — or a middlebox that silently killed the idle connection — swallowed
/// the folder click that interrupted the IDLE, forever.
pub(in crate::worker) const IDLE_DONE_TIMEOUT: Duration = Duration::from_secs(5);

/// Cap on the SELECT / IDLE-init round trips entering an IDLE, for the same
/// wedged-connection reason as [`IDLE_DONE_TIMEOUT`].
pub(in crate::worker) const IDLE_START_TIMEOUT: Duration = Duration::from_secs(10);

pub(in crate::worker) type ImapSession = Session<ImapIo<TlsStream<TcpStream>>>;

/// The callback a worker reports through (the UI's input sender, wrapped).
/// Cloned into the IDLE watchers, which run as tasks of their own.
pub(in crate::worker) trait Emit:
    Fn(WorkerEvent) + Clone + Send + 'static
{
}

impl<F: Fn(WorkerEvent) + Clone + Send + 'static> Emit for F {}

/// What a request handler leaves for the steps after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum After {
    /// Carry on: the selection is reset if the request invalidated it.
    Done,
    /// The connection failed: the selection is reset, then the session is
    /// dropped so the next request reconnects.
    Lost,
    /// Neither: the request was answered without the server, or put back
    /// to run after the reader's loads.
    Skip,
}

impl After {
    fn lost_if(lost: bool) -> Self {
        if lost {
            After::Lost
        } else {
            After::Done
        }
    }
}

/// The session a request handler works on: `dispatch` runs a handler only
/// after `ensure_session` found or made one.
fn ensured(session: &mut Option<ImapSession>) -> &mut ImapSession {
    session
        .as_mut()
        .expect("ensure_session gave the request a session")
}

/// One IMAP account's worker: the main connection, the work queued for when
/// it is idle, and the bookkeeping that spaces the chores out.
struct ImapWorker<E> {
    account_id: u32,
    account: AccountConfig,
    rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: E,
    /// `None` when the disk cache could not be opened.
    cache: Option<Cache>,
    /// The main connection; `None` makes the next request reconnect.
    session: Option<ImapSession>,
    /// Attachments queued for background pre-download (folder_path, uid).
    prefetch: VecDeque<(String, u32)>,
    /// Message bodies queued for background pre-download, so new mail opens
    /// with no network wait (folder_path, uid).
    body_prefetch: VecDeque<(String, u32)>,
    /// Bodies already pushed to the UI's in-memory cache this session, so
    /// they're not re-sent on every folder re-sync.
    body_emitted: HashSet<(String, u32)>,
    /// Folders queued for background index backfill (the rest of the mailbox
    /// past the fast first page).
    backfill: VecDeque<Backfill>,
    /// The folders already enqueued for backfill this session.
    backfill_seen: HashSet<String>,
    /// Folders queued for the one-time References repair (see
    /// [`run_one_refs_repair`]). Every folder gets one: a message indexed by
    /// an older build carries only its In-Reply-To, and threading reads
    /// References.
    refs_repair: VecDeque<(u32, String)>,
    /// Folders queued for the one-time Gmail label fill (see
    /// [`run_one_label_fill`]).
    label_fill: VecDeque<(u32, String)>,
    /// Requests already queued when one was taken, reordered so the reader's
    /// loads come before list syncs (see `reorder_reader_loads`).
    backlog: VecDeque<MailRequest>,
    /// IMAP IDLE push: watch the most recently loaded folder for new mail.
    /// The account's own setting wins over the global switch (#91).
    push_enabled: bool,
    idle_folder: Option<(u32, String)>,
    /// When the other folders' unread chips were last re-checked (`None` =
    /// not yet this session; the first idle pass counts the startup listing).
    last_unread_sweep: Option<Instant>,
    /// When the Junk / Trash auto-empty (#140) last ran for this connection.
    last_auto_empty: Option<Instant>,
    last_list_clean: Option<Instant>,
    /// Whether the Inbox's permanent IDLE watcher has been spawned. It waits
    /// for the main session to be up first (its connection must win the
    /// first slot on servers with a per-user cap) and needs the folder list
    /// in the cache. Push off = the user opted out of persistent connections
    /// entirely.
    inbox_watch_spawned: bool,
    /// The dynamic watcher pool: folders recently seen changing (or opened)
    /// each hold an IDLE connection for a [`WATCH_LEASE`], keyed by path.
    watchers: HashMap<String, FolderWatch>,
    /// Each folder's (unseen, UIDNEXT) from the previous sweep, for spotting
    /// the folders that changed between sweeps.
    sweep_baseline: HashMap<String, (u32, Option<u32>)>,
    /// The unread sweep's place (folders still to count). It runs from the
    /// idle ladder, after the new mail's body prefetch, never inline.
    sweep_pending: Vec<Folder>,
    /// Whether a sweep is owed: IDLE waking, or a sweep a request
    /// interrupted. Counted from the idle ladder, after queued reader
    /// requests.
    sweep_due: bool,
    folder_refresh_due: bool,
    /// Set after prefetching; triggers one re-sync (to catch mail that
    /// arrived while the connection was busy) before settling into the long
    /// IDLE.
    pending_resync: bool,
    /// Whether to use IMAP's structured ENVELOPE/BODYSTRUCTURE. Disabled for
    /// the session (falling back to raw-header parsing) if the server sends
    /// responses our IMAP parser can't handle (e.g. iCloud), and remembered
    /// across sessions once a way of asking has worked (#226).
    use_envelope: bool,
    /// Whether the Outbox has been retried since this connection came up. A
    /// queued message is almost always waiting on the network, so having a
    /// session again is the moment worth retrying, not a timer.
    outbox_flushed: bool,
    /// Whether the user has been told this account's server keeps no tags
    /// (one notice per session, not one per tagged message).
    local_tags_noticed: bool,
}

/// Run an IMAP account until the UI drops its sender.
pub(in crate::worker) async fn run_imap(
    account_id: u32,
    account: AccountConfig,
    rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Emit,
) {
    ImapWorker::start(account_id, account, rx, emit)
        .await
        .run()
        .await;
}

impl<E: Emit> ImapWorker<E> {
    /// Serve requests one at a time until the UI hangs up, doing the idle
    /// ladder's work in between, then log out.
    async fn run(mut self) {
        loop {
            self.flush_outbox_on_reconnect().await;
            self.spawn_inbox_watch();

            // Always prefer incoming requests. When idle: drain the attachment
            // prefetch queue (fast), then index a chunk of the background
            // backfill, then (if push is on) re-sync once to catch any mail that
            // arrived while busy, then sit in a long IMAP IDLE for instant
            // delivery. Without push, block for the next request.
            let req = match self
                .backlog
                .pop_front()
                .map_or_else(|| self.rx.try_recv(), Ok)
            {
                Ok(req) => req,
                Err(mpsc::error::TryRecvError::Disconnected) => break,
                Err(mpsc::error::TryRecvError::Empty) => match self.idle().await {
                    Idle::Request(req) => req,
                    Idle::Again => continue,
                    Idle::Stop => break,
                },
            };
            let req = reorder_reader_loads(req, &mut self.rx, &mut self.backlog);
            self.handle(req).await;
        }

        if let Some(mut session) = self.session.take() {
            let _ = session.logout().await;
        }
    }

    /// Back online with something queued: send it before anything else, so a
    /// message the user thinks they sent doesn't sit behind a mailbox sync.
    async fn flush_outbox_on_reconnect(&mut self) {
        if self.session.is_some() {
            if !self.outbox_flushed {
                self.outbox_flushed = true;
                flush_outbox(
                    self.cache.as_ref(),
                    self.account_id,
                    &self.account,
                    None,
                    &mut self.session,
                    &mut self.use_envelope,
                    &self.emit,
                    false,
                )
                .await;
            }
        } else {
            self.outbox_flushed = false;
        }
    }

    /// Start the Inbox's permanent watcher once the main session is up and
    /// the folder list is in the cache.
    fn spawn_inbox_watch(&mut self) {
        if self.inbox_watch_spawned || self.session.is_none() {
            return;
        }
        match self.cache.as_ref().map(|c| c.load_folders(self.account_id)) {
            // No cache = no folder list to watch from; stop checking.
            None => self.inbox_watch_spawned = true,
            Some(folders) if !folders.is_empty() => {
                self.inbox_watch_spawned = true;
                // The Inbox is watched permanently (no lease): it's where
                // unfiltered mail lands, so its chip staying live is the
                // baseline expectation whatever folder is being viewed.
                if let Some(inbox) = folders.iter().find(|f| f.kind == FolderKind::Inbox) {
                    tokio::task::spawn_local(watch_folder(
                        self.account.clone(),
                        inbox.path.clone(),
                        Duration::ZERO,
                        None,
                        self.emit.clone(),
                    ));
                }
                // The initial sweep is already owed. Seed its baseline in
                // the idle chain, after the startup reader requests.
            }
            // Connected but the listing hasn't landed in the cache yet
            // (fresh account, first moments): check again next pass.
            Some(_) => {}
        }
    }
}
