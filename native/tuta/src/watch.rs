//! `watch`: push notifications over Tuta's event bus, the one streaming
//! operation.
//!
//! Every other operation answers with exactly one line. `watch` instead
//! holds the event bus websocket the official client holds
//! (`app-kit/local-store/event/EventBusClient.ts`, here through
//! tutabridge's `EventBusClient`) and writes one JSON line per change until
//! its stdin closes, then exits without a further line. Each line carries
//! `version`:
//!
//! - `{"event":"ready"}` once the bus first connects;
//! - `{"event":"mail_changed","folder":"<mail set id>"}` when mail in that
//!   folder was added, removed or changed (read state, labels); `folder` is
//!   `""` when the folder could not be told, meaning "resync everything";
//! - `{"event":"folders_changed"}` when a folder or label was created,
//!   renamed, moved or deleted;
//! - `{"event":"reconnect"}` after the bus dropped and came back. Missed
//!   batches are replayed by the server, but a caller should resync anyway.
//! - a final `{"error":{"code":...}}` when the watch cannot go on (session
//!   revoked, client outdated), after which the helper exits.
//!
//! The request must be one newline-terminated line with stdin left open.
//! No per-operation timeout applies. Nothing about a mail beyond its folder
//! is written.
use super::{
    diag, id, new_sdk, resume, Operation, QuietStdout, Request, Result, Session, TUTA_URL, VERSION,
};
use crate::mailbox_ops::{is_label, load_mailbox};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashMap},
    io::{self, Read, Write},
    os::fd::AsFd,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{mpsc, watch};
use tutabridge_tuta::{
    event_bus::{
        EntityUpdateBatch, EventBusClient, EventBusError, EventBusMessage, Operation as BusOp,
        WsState,
    },
    mail::MailExtensions,
};
use tutasdk::{
    entities::{
        generated::{
            sys::User,
            tutanota::{Mail, MailSet, MailSetEntry},
        },
        Entity,
    },
    LoggedInSdk, TypeRef,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Watch {
    pub session: Session,
}

/// Just enough of a request to tell a watch from the rest without decoding
/// a large one twice.
#[derive(Deserialize)]
struct Peek {
    op: Option<String>,
}

/// Whether a newline-terminated first line asks for `watch`.
pub(crate) fn is_watch(line: &[u8]) -> bool {
    line.ends_with(b"\n")
        && serde_json::from_slice::<Peek>(line).is_ok_and(|p| p.op.as_deref() == Some("watch"))
}

fn type_id(type_ref: TypeRef) -> i64 {
    String::from(type_ref.type_id).parse().unwrap_or(-1)
}

/// What a batch means to a caller, in emission order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Change {
    Folders,
    Mail(String),
}

impl Change {
    fn line(&self) -> Value {
        match self {
            Change::Folders => json!({"version": VERSION, "event": "folders_changed"}),
            Change::Mail(folder) => {
                json!({"version": VERSION, "event": "mail_changed", "folder": folder})
            }
        }
    }
}

/// Mail set lookup by the list its entries live in, refreshed whenever a
/// mail set changes.
struct Folders {
    by_entries: HashMap<String, (String, bool)>,
    labels: BTreeSet<String>,
}

impl Folders {
    fn new(sets: &[MailSet]) -> Self {
        let mut by_entries = HashMap::new();
        let mut labels = BTreeSet::new();
        for set in sets {
            let Some(set_id) = set._id.as_ref().map(id) else {
                continue;
            };
            if is_label(set) {
                labels.insert(set_id.clone());
            }
            by_entries.insert(set.entries.to_string(), (set_id, is_label(set)));
        }
        Self { by_entries, labels }
    }
    async fn load(sdk: &LoggedInSdk) -> Result<Self> {
        Ok(Self::new(&load_mailbox(sdk).await?.sets))
    }
}

/// The folders (not labels) a mail is in.
fn mail_folders(sets: &[tutasdk::IdTupleGenerated], folders: &Folders) -> Vec<String> {
    sets.iter()
        .map(id)
        .filter(|set| !folders.labels.contains(set))
        .collect()
}

struct Types {
    mail: i64,
    mail_set: i64,
    entry: i64,
}

/// The changes one entity-update batch makes visible.
async fn changes(
    sdk: &Arc<LoggedInSdk>,
    folders: &mut Folders,
    types: &Types,
    batch: &EntityUpdateBatch,
) -> BTreeSet<Change> {
    let mut out = BTreeSet::new();
    let tutanota: Vec<_> = batch
        .updates
        .iter()
        .filter(|u| u.application == "tutanota")
        .collect();
    if tutanota.iter().any(|u| u.type_id == types.mail_set) {
        out.insert(Change::Folders);
        match Folders::load(sdk).await {
            Ok(fresh) => *folders = fresh,
            Err(code) => diag::note(&format!("watch: folder refresh failed: {code}")),
        }
    }
    for update in tutanota {
        if update.type_id == types.entry {
            match folders.by_entries.get(&update.instance_list_id) {
                // A label's entries change with the mail's own update,
                // which names its folders.
                Some((_, true)) => {}
                Some((folder, false)) => {
                    out.insert(Change::Mail(folder.clone()));
                }
                None => {
                    out.insert(Change::Mail(String::new()));
                }
            }
        } else if update.type_id == types.mail
            && matches!(update.operation, BusOp::Update | BusOp::Create)
        {
            match mail_sets(sdk, update).await {
                Some(sets) => {
                    out.extend(mail_folders(&sets, folders).into_iter().map(Change::Mail))
                }
                None => {
                    out.insert(Change::Mail(String::new()));
                }
            }
        }
    }
    out
}

/// The mail sets of a created or updated mail: from the inline instance
/// when the server sent one, else loaded.
async fn mail_sets(
    sdk: &Arc<LoggedInSdk>,
    update: &tutabridge_tuta::event_bus::EntityUpdateEvent,
) -> Option<Vec<tutasdk::IdTupleGenerated>> {
    if let Some(instance) = update.instance.as_deref() {
        match MailExtensions::new(sdk.clone())
            .decrypt_inline_and_parse::<Mail>(instance)
            .await
        {
            Ok(Some(mail)) => return Some(mail.sets),
            Ok(None) => {}
            Err(error) => diag::failed("watch: inline mail", &error),
        }
    }
    let mail_id = super::tuple(&format!(
        "{}/{}",
        update.instance_list_id, update.instance_id
    ))
    .ok()?;
    sdk.mail_facade()
        .get_crypto_entity_client()
        .load::<Mail, _>(&mail_id)
        .await
        .map_err(|error| diag::failed("watch: mail load", &error))
        .ok()
        .map(|mail| mail.sets)
}

/// Writes lines to the reply channel. A write failure means the reader is
/// gone, which ends the watch like a closed stdin.
struct Lines(Box<dyn Write + Send>);

impl Lines {
    fn emit(&mut self, line: &Value) -> bool {
        writeln!(self.0, "{line}")
            .and_then(|()| self.0.flush())
            .is_ok()
    }
}

/// The real stdout: the SDK prints progress to fd 1, which stays parked on
/// /dev/null for the whole watch; lines go to the saved descriptor.
fn reply_channel(quiet: Option<&QuietStdout>) -> Box<dyn Write + Send> {
    match quiet.and_then(|q| q.saved.as_fd().try_clone_to_owned().ok()) {
        Some(fd) => Box::new(std::fs::File::from(fd)),
        None => Box::new(io::stdout()),
    }
}

/// Fires once stdin reaches EOF (or fails): the caller's stop signal.
fn stdin_closed() -> watch::Receiver<bool> {
    let (tx, rx) = watch::channel(false);
    std::thread::spawn(move || {
        let mut buffer = [0u8; 256];
        let mut stdin = io::stdin();
        while matches!(stdin.read(&mut buffer), Ok(n) if n > 0) {}
        let _ = tx.send(true);
    });
    rx
}

/// The code a bus that gave up reports.
fn bus_error(error: &EventBusError) -> &'static str {
    match error {
        EventBusError::AuthenticationRejected(429) => "transport_failed",
        EventBusError::AuthenticationRejected(_) => "session_revoked",
        EventBusError::InvalidMessage(_) => "watch_failed",
        EventBusError::Stopped => "watch_failed",
    }
}

/// Whether a failed session check ends the watch: only a definite verdict
/// on the session or client does; an outage just waits for the bus.
fn fatal(code: &'static str) -> bool {
    matches!(
        code,
        "session_revoked" | "client_outdated" | "session_invalid"
    )
}

/// Serve a watch request (`line`, newline included). Writes the reply lines
/// itself; returns when stdin closes or the watch fails.
pub(crate) async fn serve(line: &[u8], quiet: Option<&QuietStdout>) {
    let mut lines = Lines(reply_channel(quiet));
    if let Err(code) = run(line, &mut lines).await {
        lines.emit(&json!({"version": VERSION, "error": {"code": code}}));
    }
}

async fn run(line: &[u8], lines: &mut Lines) -> Result<()> {
    let request: Request = serde_json::from_slice(line).map_err(|_| "invalid_request")?;
    if request.version != VERSION {
        return Err("unsupported_version");
    }
    let Operation::Watch(Watch { session }) = request.operation else {
        return Err("invalid_request");
    };
    let (access_token, user_id) = (session.access_token.clone(), session.user_id.clone());
    let mut stop = stdin_closed();
    let sdk = new_sdk()?;
    let logged_in = resume(&sdk, session).await?;
    let mut folders = Folders::load(&logged_in).await?;
    let model_version = |type_ref: TypeRef| {
        logged_in
            .type_model_provider
            .resolve_client_type_ref(&type_ref)
            .and_then(|model| u32::try_from(model.version).ok())
            .ok_or("watch_failed")
    };
    let types = Types {
        mail: type_id(Mail::type_ref()),
        mail_set: type_id(MailSet::type_ref()),
        entry: type_id(MailSetEntry::type_ref()),
    };
    let bus = Arc::new(EventBusClient::new(
        TUTA_URL.to_string(),
        model_version(User::type_ref())?,
        model_version(Mail::type_ref())?,
        tutasdk::CLIENT_VERSION.to_string(),
        String::new(),
    ));
    let mut state = bus.state();
    let last_batch_ids = bus.last_batch_ids();
    let (tx, mut rx) = mpsc::channel(64);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let runner = {
        let bus = bus.clone();
        tokio::spawn(async move { bus.run(access_token, user_id, tx, shutdown_rx).await })
    };
    let mut connected_once = false;
    let outcome = loop {
        tokio::select! {
            _ = stop.changed() => break Ok(()),
            changed = state.changed() => {
                if changed.is_err() {
                    continue;
                }
                let now = *state.borrow_and_update();
                match now {
                    WsState::Connected => {
                        let event = if connected_once { "reconnect" } else { "ready" };
                        connected_once = true;
                        if !lines.emit(&json!({"version": VERSION, "event": event})) {
                            break Ok(());
                        }
                    }
                    // The bus retries an expired session forever; a REST
                    // call tells a dead session from a dead network.
                    WsState::Reconnecting => {
                        if let Err(code) = load_mailbox(&logged_in).await {
                            if fatal(code) {
                                break Err(code);
                            }
                        }
                    }
                    WsState::Connecting | WsState::Stopped => {}
                }
            }
            message = rx.recv() => match message {
                Some(EventBusMessage::EntityUpdate(batch)) => {
                    let found = changes(&logged_in, &mut folders, &types, &batch).await;
                    if !found.iter().all(|change| lines.emit(&change.line())) {
                        break Ok(());
                    }
                    // Processed: a reconnect resumes after this batch.
                    if let Ok(mut ids) = last_batch_ids.lock() {
                        ids.insert(batch.group_id.clone(), batch.batch_id.clone());
                    }
                }
                Some(_) => {}
                // The bus gave up; its result, read below, says why.
                None => break Err("watch_failed"),
            },
        }
    };
    let _ = shutdown_tx.send(true);
    let finished = tokio::time::timeout(Duration::from_secs(5), runner).await;
    match (outcome, finished) {
        (Err("watch_failed"), Ok(Ok(Err(error)))) => {
            diag::failed("watch: event bus", &error);
            Err(bus_error(&error))
        }
        (outcome, _) => outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_newline_terminated_watch_line_streams() {
        assert!(is_watch(
            b"{\"version\":1,\"op\":\"watch\",\"session\":{}}\n"
        ));
        assert!(!is_watch(
            b"{\"version\":1,\"op\":\"watch\",\"session\":{}}"
        ));
        assert!(!is_watch(b"{\"version\":1,\"op\":\"sync\"}\n"));
        assert!(!is_watch(b"{\"version\":1,\n"));
        assert!(!is_watch(b"\n"));
    }

    #[test]
    fn change_lines_carry_version_and_folder() {
        assert_eq!(
            Change::Mail("a/b".into()).line(),
            json!({"version": 1, "event": "mail_changed", "folder": "a/b"})
        );
        assert_eq!(
            Change::Folders.line(),
            json!({"version": 1, "event": "folders_changed"})
        );
        // Folder changes sort first, so a caller refreshes its tree before
        // syncing the folders named after it.
        let mut set = BTreeSet::new();
        set.insert(Change::Mail("z/z".into()));
        set.insert(Change::Folders);
        assert_eq!(set.into_iter().next(), Some(Change::Folders));
    }

    #[test]
    fn bus_failures_map_to_fixed_codes() {
        assert_eq!(
            bus_error(&EventBusError::AuthenticationRejected(401)),
            "session_revoked"
        );
        assert_eq!(
            bus_error(&EventBusError::AuthenticationRejected(472)),
            "session_revoked"
        );
        assert_eq!(
            bus_error(&EventBusError::AuthenticationRejected(429)),
            "transport_failed"
        );
        assert!(fatal("session_revoked") && !fatal("transport_failed"));
    }

    #[test]
    fn type_ids_match_the_model() {
        assert_eq!(type_id(Mail::type_ref()), 97);
        assert_eq!(type_id(MailSet::type_ref()), 429);
        assert_eq!(type_id(MailSetEntry::type_ref()), 1450);
    }
}
