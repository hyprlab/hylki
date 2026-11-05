//! New mail pushed: the helper's streaming `watch` op, which holds Tuta's
//! event bus open, kept running on a thread of its own. Each change it
//! reports wakes the worker for a sync of what changed; while it is down
//! the worker polls. It is started again after a pause that doubles with
//! each failure in a row, and given up for good on an answer that a retry
//! cannot change: the sign-in is gone, or the helper does not know `watch`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::mpsc;

use crate::worker::helper::{locate_helper, HelperClient, Object};
use crate::worker::tuta::wire::SPEC;

/// What the push thread tells the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PushSignal {
    /// The helper is connected: changes from here on are pushed, and what
    /// happened before may have been missed.
    Live,
    /// Mail changed in the folder with this Tuta id; empty when the helper
    /// could not say which.
    Mail(String),
    /// The folder tree changed.
    Folders,
    /// The helper stopped: nothing is pushed until it is `Live` again.
    Ended,
}

/// The first pause before `watch` is started again, and the longest.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(5 * 60);

/// A run that lasted this long was working: the next failure starts the
/// pauses over.
const HEALTHY_RUN: Duration = Duration::from_secs(5 * 60);

/// How often the thread looks up from a quiet helper to see whether the
/// worker has gone.
const LOOK_UP: Duration = Duration::from_secs(1);

/// The codes after which no later `watch` can do better: the sign-in is
/// gone or broken, or the helper is missing or does not know the op.
fn is_final(code: &str) -> bool {
    matches!(
        code,
        "session_revoked"
            | "session_invalid"
            | "not_signed_in"
            | "client_outdated"
            | "helper_missing"
            | "unsupported_version"
            | "invalid_request"
            | "unsupported_operation"
    )
}

/// What one line of `watch` means for the worker; `None` for an event it
/// has no use for, which a newer helper may add.
pub(super) fn signal_of(line: &Object) -> Option<PushSignal> {
    match line.get("event").and_then(Value::as_str)? {
        "ready" => Some(PushSignal::Live),
        "mail_changed" => Some(PushSignal::Mail(
            line.get("folder")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        )),
        "folders_changed" => Some(PushSignal::Folders),
        // The event bus came back after a drop: what changed meanwhile
        // was not pushed.
        "reconnect" => Some(PushSignal::Mail(String::new())),
        _ => None,
    }
}

/// How one run of `watch` ended.
enum RunEnd {
    /// The worker has gone.
    WorkerGone,
    /// The helper stopped, with this code.
    Stopped(String),
}

/// One run of `watch` until it stops or the worker goes.
fn watch_once(
    client: &HelperClient,
    executable: &std::path::Path,
    tx: &mpsc::UnboundedSender<PushSignal>,
) -> RunEnd {
    let mut watch = match client.watch(executable, "watch") {
        Ok(watch) => watch,
        Err(e) => return RunEnd::Stopped(e.code),
    };
    loop {
        if tx.is_closed() {
            return RunEnd::WorkerGone;
        }
        match watch.next(LOOK_UP) {
            Ok(None) => {}
            Ok(Some(line)) => {
                if let Some(signal) = signal_of(&line) {
                    if tx.send(signal).is_err() {
                        return RunEnd::WorkerGone;
                    }
                }
            }
            Err(e) => return RunEnd::Stopped(e.code),
        }
    }
}

/// Sleep `pause`, waking early when the worker has gone: whether it has.
fn rest(pause: Duration, tx: &mpsc::UnboundedSender<PushSignal>) -> bool {
    let until = Instant::now() + pause;
    while Instant::now() < until {
        if tx.is_closed() {
            return true;
        }
        std::thread::sleep(LOOK_UP.min(until.saturating_duration_since(Instant::now())));
    }
    tx.is_closed()
}

/// Keep `watch` running for `client`'s account on a thread of its own,
/// with the helper at `executable` (the installed one when `None`), until
/// the worker drops its receiver or a run ends with a final code.
pub(super) fn spawn_tuta_push(
    client: HelperClient,
    executable: Option<PathBuf>,
    tx: mpsc::UnboundedSender<PushSignal>,
) {
    let spawned = std::thread::Builder::new()
        .name("tuta-push".into())
        .spawn(move || push_loop(&client, executable, &tx));
    if spawned.is_err() {
        tracing::warn!("tuta: could not start the push thread");
    }
}

fn push_loop(
    client: &HelperClient,
    executable: Option<PathBuf>,
    tx: &mpsc::UnboundedSender<PushSignal>,
) {
    let mut pause = RETRY_MIN;
    loop {
        // Offline: wait for the network rather than start into nothing
        // (#442).
        if !crate::desktop::power::reaches_internet() {
            if rest(RETRY_MIN, tx) {
                return;
            }
            continue;
        }
        let started = Instant::now();
        let ended = match executable.clone().map_or_else(|| locate_helper(SPEC), Ok) {
            Ok(executable) => watch_once(client, &executable, tx),
            Err(e) => RunEnd::Stopped(e.code),
        };
        let code = match ended {
            RunEnd::WorkerGone => return,
            RunEnd::Stopped(code) => code,
        };
        if tx.send(PushSignal::Ended).is_err() {
            return;
        }
        if is_final(&code) {
            tracing::info!("tuta: push stopped for good ({code}); polling instead");
            return;
        }
        if started.elapsed() >= HEALTHY_RUN {
            pause = RETRY_MIN;
        }
        tracing::info!("tuta: push stopped ({code}); again in {}s", pause.as_secs());
        if rest(pause, tx) {
            return;
        }
        pause = (pause * 2).min(RETRY_MAX);
    }
}
