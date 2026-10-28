//! Background mail worker.
//!
//! IMAP is async and its sessions are stateful, while the relm4 UI is driven
//! synchronously on the GTK main thread. To bridge the two, [`spawn`] starts a
//! dedicated OS thread running a tokio runtime that owns the IMAP session. The
//! UI sends [`MailRequest`]s over an unbounded channel; the worker performs the
//! network I/O and pushes [`WorkerEvent`]s back via a caller-supplied callback
//! (in practice, the component's input sender). The mock path implements the
//! exact same protocol so the app behaves identically offline.

// The worker's steps take the account's id, settings, cache, session or
// protocol state and event sink as arguments of their own, each only what it
// uses; a context struct bundling them would be a new interface for every
// backend, not a restructure.
#![allow(clippy::too_many_arguments)]

use tokio::sync::mpsc;

use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};

mod api;
mod auth;
mod chores;
mod compose;
mod conn_test;
mod drafts;
mod folders;
mod graph;
mod headers;
mod ids;
mod imap;
mod index_queue;
mod jmap;
mod lane;
mod mock;
mod offline;
mod outbox;
mod pop3;
mod preempt;
mod preview;
mod reconcile;
mod render;
mod smtp;
mod strip;
mod summary;
#[cfg(test)]
mod test_support;
mod tls;

pub(crate) use api::{
    begin_text_search, text_search_is_current, CalendarPart, MailRequest, OutgoingMessage,
    WorkerEvent, TEXT_SEARCH_LIMIT,
};
pub(crate) use auth::{forget_access_token, oauth_token_blocking};
pub(crate) use compose::build::sent_body_preview;
pub(crate) use compose::editable::editable_from_raw;
pub(crate) use compose::recipients::{
    clean_recipient_field, format_recipient, invalid_recipient, parse_recipients,
};
pub(crate) use conn_test::{test_alias_smtp_blocking, test_connection_blocking, ConnTest};
pub(crate) use folders::folder_order;
pub(crate) use folders::{chip_counts_all, snoozed_counts};
use graph::run_graph;
pub(crate) use headers::{clean_display_name, decode_header, unescape_quoted_str};
pub(crate) use imap::fetch_mode::readable_error;
pub(crate) use imap::gmail::has_gmail_labels;
pub(crate) use imap::keywords::is_system_keyword;
use imap::run_imap;
use lane::cache_lane;
use mock::run_mock;
pub(crate) use outbox::{answered_copies, park_for_next_launch, reply_copy_path};
use pop3::run_pop3;
pub(crate) use render::attachments::extract_attachments_of;
pub(crate) use render::body::extract_body;
pub(crate) use render::body::wrap_plain;
pub(crate) use render::render_raw;
pub(crate) use summary::summary_from_raw;
pub(crate) use tls::is_hostname_mismatch;

/// What a connection that was never tried, because the network is down,
/// reports (#442).
fn offline_text() -> String {
    i18n("no network connection")
}

/// Run blocking work (an HTTP request, a cache query) off the async
/// threads; a task that panicked reads as an error.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|_| Err("task failed".into()))
}

/// A distinct accent color per account (cycles through a small palette).
pub(crate) fn accent_for(account_id: u32) -> &'static str {
    const PALETTE: [&str; 6] = [
        "#3584e4", "#2ec27e", "#e5a50a", "#e66100", "#9141ac", "#c01c28",
    ];
    PALETTE[(account_id.saturating_sub(1) as usize) % PALETTE.len()]
}

/// Start a worker thread for one account (`Some`) or the offline sample data
/// (`None`). Returns the sender used to issue requests. `account_id` stamps all
/// emitted folders/messages and keys the cache.
pub(crate) fn spawn(
    account_id: u32,
    account: Option<AccountConfig>,
    emit: impl Fn(WorkerEvent) + Clone + Send + 'static,
) -> mpsc::UnboundedSender<MailRequest> {
    let (tx, rx) = mpsc::unbounded_channel();

    // The UI talks to the cache lane, which answers what the disk cache can
    // and hands the rest to the worker (`tx`), so a cached message never
    // waits behind a sync the worker is busy with. See [`cache_lane`].
    let (ui_tx, lane_rx) = mpsc::unbounded_channel();
    {
        let protocol = account.as_ref().map(|a| a.protocol);
        let emit = emit.clone();
        std::thread::Builder::new()
            .name(format!("hylki-cache-{account_id}"))
            .spawn(move || cache_lane(account_id, protocol, lane_rx, tx, emit))
            .expect("failed to spawn cache lane thread");
    }

    std::thread::Builder::new()
        .name(format!("hylki-mail-{account_id}"))
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    emit(WorkerEvent::error(i18n_f(
                        "runtime error: {e}",
                        &[("e", &(e).to_string())],
                    )));
                    return;
                }
            };
            // A LocalSet so the per-folder IDLE watchers ([`watch_folder`]) can
            // be spawned as (non-Send) local tasks sharing this thread; they
            // are cancelled with it when the worker winds down.
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, run(account_id, account, rx, emit));
        })
        .expect("failed to spawn mail worker thread");

    ui_tx
}

async fn run(
    account_id: u32,
    account: Option<AccountConfig>,
    rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent) + Clone + Send + 'static,
) {
    match account {
        Some(account) if account.protocol == crate::config::Protocol::Pop3 => {
            run_pop3(account_id, account, rx, emit).await
        }
        Some(account) if account.protocol == crate::config::Protocol::Graph => {
            run_graph(account_id, account, rx, emit).await
        }
        Some(account) if account.protocol == crate::config::Protocol::Jmap => {
            jmap::run_jmap(account_id, account, rx, emit).await
        }
        Some(account) => run_imap(account_id, account, rx, emit).await,
        None => run_mock(account_id, rx, emit).await,
    }
}
