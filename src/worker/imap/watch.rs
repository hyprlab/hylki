//! IDLE watchers on their own connections: the Inbox always, and a few
//! folders seen changing, each for a lease, so their unread chips stay live.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use crate::config::AccountConfig;
use crate::models::FolderKind;
use crate::worker::imap::connect::connect;
use crate::worker::imap::idle::idle_done;
use crate::worker::imap::sweep::selected_unseen;
use crate::worker::imap::wire::exam;
use crate::worker::imap::{
    ImapSession, IDLE_START_TIMEOUT, NETWORK_WAIT, WATCHER_LIMIT, WATCH_LEASE, WATCH_VERIFY,
};
use crate::worker::WorkerEvent;

/// A leased per-folder IDLE watcher: its task and the shared last-touch clock
/// that keeps it alive (see [`watch_folder`]).
pub(super) struct FolderWatch {
    handle: tokio::task::JoinHandle<()>,
    lease: Rc<Cell<std::time::Instant>>,
}

/// Whether a folder may hold a dynamic watcher slot. The Inbox is out because
/// it has a permanent watcher; Sent, Drafts and Templates because changes
/// there are the user's own doing; Trash and Junk because nobody needs
/// to-the-second unread counts on either — the periodic sweep keeps them
/// honest.
pub(in crate::worker) fn watchable(kind: FolderKind) -> bool {
    !matches!(
        kind,
        FolderKind::Inbox
            | FolderKind::Sent
            | FolderKind::Drafts
            | FolderKind::Templates
            | FolderKind::Snoozed
            | FolderKind::Trash
            | FolderKind::Junk
    )
}

/// Give every [`watchable`] folder the sweep just saw change a watcher (or a
/// fresh lease on the one it has), staggering the connects of whatever
/// actually spawns — several logins in the same instant look like an attack to
/// providers that rate-limit authentication (iCloud does).
pub(super) fn watch_changed_folders(
    watchers: &mut std::collections::HashMap<String, FolderWatch>,
    account: &AccountConfig,
    changed: &[(FolderKind, String)],
    emit: &(impl Fn(WorkerEvent) + Clone + 'static),
) {
    // A sweep claiming more folders moved than the pool could ever hold says
    // nothing about which are hot — that shape is a bulk event (a client
    // reorganizing, a suspect baseline), and admitting them would churn every
    // standing watcher out for nothing. Let the next sweep pick the real ones.
    if changed.iter().filter(|(k, _)| watchable(*k)).count() > WATCHER_LIMIT {
        tracing::info!(
            "sweep: bulk change ({} folders), not adjusting watchers",
            changed.len()
        );
        return;
    }
    let mut stagger = 0;
    for (kind, path) in changed {
        if watchable(*kind) && watch_active_folder(watchers, account, path, stagger, emit) {
            stagger += 1;
        }
    }
}

/// Put `path` on the dynamic watch list: renew its lease if its watcher is
/// still running, otherwise spawn one — evicting the stalest lease when the
/// pool is at [`WATCHER_LIMIT`], so fresh activity always wins a slot. Returns
/// whether a new watcher was spawned (callers stagger connects with this).
/// Callers are responsible for the [`watchable`] check.
pub(super) fn watch_active_folder(
    watchers: &mut std::collections::HashMap<String, FolderWatch>,
    account: &AccountConfig,
    path: &str,
    stagger: usize,
    emit: &(impl Fn(WorkerEvent) + Clone + 'static),
) -> bool {
    if let Some(w) = watchers.get(path) {
        if !w.handle.is_finished() {
            w.lease.set(std::time::Instant::now());
            return false;
        }
    }
    // A metered connection keeps to the Inbox's own watcher; the sweep still
    // covers the rest (#442).
    if crate::desktop::power::saving_data() {
        return false;
    }
    // Lapsed watchers exited on their own; drop their leftover entries.
    watchers.retain(|_, w| !w.handle.is_finished());
    while watchers.len() >= WATCHER_LIMIT {
        // The candidate was touched just now, so it is by definition fresher
        // than the stalest sitting lease. Aborting drops that connection
        // without a LOGOUT; the server reaps it.
        let stalest = watchers
            .iter()
            .min_by_key(|(_, w)| w.lease.get())
            .map(|(p, _)| p.clone());
        let Some(stalest) = stalest else { break };
        if let Some(w) = watchers.remove(&stalest) {
            w.handle.abort();
            tracing::info!("unwatching {stalest}: slot needed for {path}");
        }
    }
    tracing::info!("watching {path} (recent activity)");
    let lease = Rc::new(Cell::new(std::time::Instant::now()));
    let handle = tokio::task::spawn_local(watch_folder(
        account.clone(),
        path.to_string(),
        Duration::from_secs(2 * stagger as u64),
        Some(lease.clone()),
        emit.clone(),
    ));
    watchers.insert(path.to_string(), FolderWatch { handle, lease });
    true
}

/// One extra IMAP connection that does nothing but sit in IDLE on a single
/// folder, so a change made anywhere else — rule-filed new mail, a message
/// read on another device — moves that folder's unread chip within seconds
/// instead of at the next sweep. This is how Apple Mail keeps folders live:
/// IDLE only reports on the selected mailbox, so watching N folders takes N
/// connections.
///
/// With a `lease` (the dynamic pool), the watcher lives on recency: every wake
/// the server reports renews the shared clock, as do the sweep and the user
/// opening the folder (via [`watch_active_folder`]); once the clock goes a
/// whole [`WATCH_LEASE`] untouched, the watcher logs out and the folder falls
/// back to sweep coverage until it changes again. Without one (the Inbox), it
/// watches for the life of the worker.
///
/// EXAMINE (never SELECT) keeps the watcher read-only, and it fetches no
/// message content — only a UID SEARCH for the unseen count. Failures are
/// quiet by design: a watcher is a freshness bonus on top of the sweep, so it
/// reconnects with backoff rather than surfacing errors, and retires for good
/// when a healthy session refuses the folder (deleted or renamed — spinning on
/// a mailbox that no longer exists helps no one).
pub(in crate::worker) async fn watch_folder(
    account: AccountConfig,
    path: String,
    start_delay: Duration,
    lease: Option<Rc<Cell<std::time::Instant>>>,
    emit: impl Fn(WorkerEvent),
) {
    let mut backoff = start_delay;
    loop {
        tokio::time::sleep(backoff).await;
        if lapsed(lease.as_ref()) {
            // The lease ran out while offline or backing off; don't reconnect
            // just to watch a folder nothing has touched for an hour.
            return;
        }
        // Offline: wait for the network, neither dialing nor growing the
        // backoff, so the watcher is back within moments of it (#442).
        while !crate::desktop::power::reaches(&account.imap_host) {
            tokio::time::sleep(NETWORK_WAIT).await;
            if lapsed(lease.as_ref()) {
                return;
            }
        }
        if lease.is_some() && crate::desktop::power::saving_data() {
            tracing::info!("unwatching {path}: the connection is metered");
            return;
        }
        let Ok(mut sess) = connect(&account).await else {
            // Offline, or the server's connection cap is spent (the main
            // session always connects first and is never contended by this).
            tracing::debug!("watcher for {path}: connect failed, backing off");
            backoff = if backoff < Duration::from_secs(60) {
                Duration::from_secs(60)
            } else {
                (backoff * 2).min(Duration::from_secs(900))
            };
            continue;
        };
        backoff = Duration::from_secs(60);
        match tokio::time::timeout(IDLE_START_TIMEOUT, exam(&mut sess, &path)).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => {
                tracing::info!("unwatching {path}: no longer selectable");
                return;
            }
            Err(_) => continue, // wedged straight away; reconnect
        }
        // Catch up on whatever happened while unwatched (startup, or the gap a
        // dropped connection left), then settle into the IDLE/verify cycle.
        let last_count = selected_unseen(&mut sess).await;
        match last_count {
            Some(unread) => emit(WorkerEvent::FolderUnreadByPath {
                path: path.clone(),
                unread,
            }),
            None => continue, // wedged straight out of the gate; reconnect
        }
        if watch_session(sess, &account, &path, lease.as_ref(), last_count, &emit).await
            == Watch::Retire
        {
            return;
        }
    }
}

/// How a watcher's session ended.
#[derive(PartialEq, Eq)]
enum Watch {
    /// The watcher is done: its lease lapsed or the connection is metered.
    Retire,
    /// The session is gone or wedged: connect again.
    Redial,
}

/// Whether a watcher's lease has run out; one without a lease never does.
fn lapsed(lease: Option<&Rc<Cell<std::time::Instant>>>) -> bool {
    lease.is_some_and(|l| l.get().elapsed() >= WATCH_LEASE)
}

/// The IDLE/verify cycle on one examined session: short IDLE rounds, a
/// recount after each, the lease renewed on activity, until the lease
/// lapses or the session is lost.
async fn watch_session(
    mut sess: ImapSession,
    account: &AccountConfig,
    path: &str,
    lease: Option<&Rc<Cell<std::time::Instant>>>,
    mut last_count: Option<u32>,
    emit: &impl Fn(WorkerEvent),
) -> Watch {
    loop {
        if lapsed(lease) {
            tracing::info!("unwatching {path}: quiet for the whole lease");
            let _ = sess.logout().await;
            return Watch::Retire;
        }
        if lease.is_some() && crate::desktop::power::saving_data() {
            tracing::info!("unwatching {path}: the connection is metered");
            let _ = sess.logout().await;
            return Watch::Retire;
        }
        if !crate::desktop::power::reaches(&account.imap_host) {
            return Watch::Redial; // the socket went with the network
        }
        // A short IDLE round: woken instantly where the server announces
        // changes, and capped at [`WATCH_VERIFY`] so the recount below
        // covers the servers that stay silent — never past the lease's
        // own deadline, so a lapsed watcher frees its connection promptly.
        let timeout = lease.map_or(WATCH_VERIFY.as_secs(), |l| {
            WATCH_LEASE
                .saturating_sub(l.get().elapsed())
                .as_secs()
                .clamp(1, WATCH_VERIFY.as_secs())
        });
        tracing::debug!(target: "hylki::imap", "> IDLE");
        let mut handle = sess.idle();
        match tokio::time::timeout(IDLE_START_TIMEOUT, handle.init()).await {
            Ok(Ok(())) => {}
            _ => return Watch::Redial, // error or wedged; drop the connection and redial
        }
        let woke = {
            handle.as_mut().set_idle(true);
            let (idle_fut, _stop) = handle.wait_with_timeout(Duration::from_secs(timeout));
            matches!(
                idle_fut.await,
                Ok(async_imap::extensions::idle::IdleResponse::NewData(_))
            )
        };
        match idle_done(handle).await {
            Some(s) => sess = s,
            None => return Watch::Redial,
        }
        // Recount whether pushed awake or on the verify tick; a changed
        // number (or any pushed wake) is activity that renews the lease,
        // and only a changed number is worth an event.
        match selected_unseen(&mut sess).await {
            Some(unread) => {
                let moved = last_count != Some(unread);
                if woke || moved {
                    if let Some(l) = lease {
                        l.set(std::time::Instant::now());
                    }
                }
                if moved {
                    tracing::debug!("watcher for {path}: unseen now {unread}");
                    last_count = Some(unread);
                    emit(WorkerEvent::FolderUnreadByPath {
                        path: path.to_string(),
                        unread,
                    });
                }
            }
            None => return Watch::Redial, // count unavailable = session gone; reconnect
        }
    }
}
