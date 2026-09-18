//! Background mail worker.
//!
//! IMAP is async and its sessions are stateful, while the relm4 UI is driven
//! synchronously on the GTK main thread. To bridge the two, [`spawn`] starts a
//! dedicated OS thread running a tokio runtime that owns the IMAP session. The
//! UI sends [`MailRequest`]s over an unbounded channel; the worker performs the
//! network I/O and pushes [`WorkerEvent`]s back via a caller-supplied callback
//! (in practice, the component's input sender). The mock path implements the
//! exact same protocol so the app behaves identically offline.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use async_imap::types::{Fetch, Flag, NameAttribute};
use async_imap::Session;
use async_native_tls::TlsStream;
use futures::TryStreamExt;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Address, AsyncSmtpTransport, AsyncTransport, Message as LettreMessage, Tokio1Executor};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::backend::{MailBackend, MockBackend};
use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::models::{Account, Folder, FolderKind, KeywordFinding, Message};
use crate::i18n::{i18n, i18n_f, ni18n_f};

/// Number of most-recent messages to fetch attachment info (BODYSTRUCTURE) for;
/// older messages get an envelope-only index row and resolve attachments on open.
const PAGE_SIZE: u32 = 50;

/// Number of most-recent messages shown instantly when a folder is first opened
/// (never synced before). The rest of the folder is indexed in the background so
/// browsing is immediate and search fills in shortly after — like Apple Mail.
const FIRST_PAGE: u32 = 200;

/// The threading headers to pull alongside the ENVELOPE.
///
/// The IMAP ENVELOPE carries In-Reply-To but *not* References, and In-Reply-To
/// alone only links a reply to its immediate parent. When that parent isn't in
/// the folder being listed — the usual case, since your own replies are filed in
/// Sent — the chain breaks and each incoming message starts a thread of its own,
/// which is why conversations hardly ever grouped (#21). References carries the
/// whole ancestry, so one of its ids is almost always present locally.
const REFS_FETCH_ITEM: &str = " BODY.PEEK[HEADER.FIELDS (REFERENCES)]";

/// Background index backfill: how many messages to fetch per idle drain step.
/// Bigger = fewer round-trips; smaller = more responsive to interleaved requests.
const BACKFILL_CHUNK: usize = 1_000;

/// Pre-download attachments only for this many of the most recent messages;
/// older attachment messages download on demand (with a spinner) when opened.
const PREFETCH_LIMIT: usize = 25;

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
const PREFETCH_BODY_LIMIT: usize = 50;

/// Messages the attachment prefetch has already tried this session, as
/// (account, folder path, uid). A message the prefetch cannot finish — OpenPGP
/// mail, which is left for an explicit open, or a read that fails to parse —
/// is never marked checked in the cache, and without this it was queued again
/// by the very resync the prefetch triggers, then fetched again, then queued
/// again: a full download of the same message several times a second, each
/// one followed by a folder listing and a rebuild of the message list, for as
/// long as it stayed among the newest rows. PGP/MIME mail always carries the
/// attachment flag (its encrypted part), so every PGP user with a recent
/// encrypted message sat in that loop.
static ATTACHMENT_PREFETCH_TRIED: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<(u32, String, u32)>>,
> = std::sync::LazyLock::new(Default::default);

fn attachment_prefetch_tried(account_id: u32, path: &str, uid: u32) -> bool {
    ATTACHMENT_PREFETCH_TRIED
        .lock()
        .map(|s| s.contains(&(account_id, path.to_string(), uid)))
        .unwrap_or(false)
}

fn note_attachment_prefetch_tried(account_id: u32, path: &str, uid: u32) {
    if let Ok(mut s) = ATTACHMENT_PREFETCH_TRIED.lock() {
        s.insert((account_id, path.to_string(), uid));
    }
}

/// What the BODY[TEXT] retry found for a row, as (account, folder id, uid) →
/// preview, kept for the session. The summary fetch comes back empty for
/// the same rows on every sync (iCloud's re-appended messages, file-first
/// messages), so without this the retry ran again each time — three rows
/// cost ~100 KB per sync in one user's log. An empty string is a message
/// with no text to show at all — an encrypted one, a photo — which is then
/// not asked about again either.
static PREVIEW_RETRIED: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<(u32, u32, u32), String>>,
> = std::sync::LazyLock::new(Default::default);

/// Floor between two unread-count sweeps out of the IDLE loop, so a burst of
/// new mail (one `Refreshed` wake per delivery) doesn't STATUS the whole folder
/// tree over and over. Explicit [`MailRequest::RefreshUnread`]s are never
/// throttled — a refresh the user asked for always runs.
const UNREAD_SWEEP_MIN: Duration = Duration::from_secs(60);
/// How often an account's Junk / Trash auto-empty (#140) may run: it rides
/// the unread sweep, but a server needs asking only a few times a day.
const AUTO_EMPTY_MIN: Duration = Duration::from_secs(6 * 60 * 60);

/// How many leased per-folder IDLE watcher connections one account may hold at
/// once (see [`watch_folder`]). The Inbox's permanent watcher and the main
/// session sit outside this cap, so the ceiling is six connections per account
/// — comfortably under every major provider's per-user cap (Gmail 15, Outlook
/// 20, Dovecot's default 10). When the pool is full, fresh activity evicts the
/// stalest lease; folders without a watcher stay covered by the unread sweep.
const WATCHER_LIMIT: usize = 4;

/// How long one burst of activity keeps a folder's watcher alive. Every touch
/// — the sweep noticing the folder changed, the watcher itself seeing server
/// activity, the user opening the folder — restarts the clock; a folder left
/// quiet for the whole hour logs its watcher out and falls back to sweep
/// coverage until it changes again.
const WATCH_LEASE: Duration = Duration::from_secs(3600);

/// How long a watcher lets IDLE sit before verifying the count itself. IDLE
/// still wakes it instantly where servers announce changes, but a flag flipped
/// from another client is exactly what some servers (iCloud, observed
/// 2026-08-28) never announce to an idling session — so the watcher recounts
/// every minute regardless and reports only when the number moved. One UID
/// SEARCH per watcher per minute; the ceiling on a watched folder's staleness
/// whatever the server chooses to say.
const WATCH_VERIFY: Duration = Duration::from_secs(60);

/// How long ending an IDLE (the DONE handshake) may take before the connection
/// is declared wedged and dropped (#91). Unbounded, a server that never answers
/// DONE — or a middlebox that silently killed the idle connection — swallowed
/// the folder click that interrupted the IDLE, forever.
const IDLE_DONE_TIMEOUT: Duration = Duration::from_secs(5);

/// Cap on the SELECT / IDLE-init round trips entering an IDLE, for the same
/// wedged-connection reason as [`IDLE_DONE_TIMEOUT`].
const IDLE_START_TIMEOUT: Duration = Duration::from_secs(10);

/// A request from the UI to the worker.
#[derive(Debug)]
pub enum MailRequest {
    /// Load the message summaries for a folder.
    LoadMessages { folder_id: u32, path: String },
    /// Resync a folder's message list in the background: the same fetch as
    /// [`LoadMessages`](Self::LoadMessages), but quiet (no status), and the
    /// folder is not adopted as the IDLE folder or put on the watch list.
    /// Sent when a watcher or sweep sees an inbox's unread count move while
    /// another folder is open, so its list (the tray menu, the new-mail
    /// notification) follows the count instead of waiting for the inbox
    /// to be opened.
    SyncFolder { folder_id: u32, path: String },
    /// Load cached attachments across the account's folders, for the gallery.
    /// Load the full body of a single message.
    LoadBody {
        message_id: u32,
        path: String,
        uid: u32,
    },
    /// Load several messages' bodies from one folder in a single IMAP fetch.
    ///
    /// Opening a conversation needs every member's body at once. Asked for one
    /// at a time that is one round trip each — on a ten-message thread, ten
    /// sequential waits on the server, and (because each arrival re-renders the
    /// reader) ten redraws. `uid_fetch` takes a whole UID set, so the same work
    /// is one round trip. Members cached on disk are served without touching the
    /// network at all, and only the rest go into the set.
    LoadBodies {
        /// `(message_id, uid)` for each member, primary first.
        items: Vec<(u32, u32)>,
        path: String,
    },
    /// Load the raw RFC 822 source of a single message.
    LoadSource {
        message_id: u32,
        path: String,
        uid: u32,
    },
    /// Load the attachments of a single message. When `download` is false, only
    /// serve from cache (otherwise reply `AttachmentsPending`) — never hits the
    /// network. The user explicitly opts in to downloading older attachments.
    LoadAttachments {
        message_id: u32,
        path: String,
        uid: u32,
        download: bool,
    },
    /// Mark a message as spam: tag `$Junk` (so the server filter can learn) and
    /// move it to the Junk folder.
    MarkSpam { path: String, uid: u32, dest: String },
    /// The opposite (#168): tag `$NotJunk`, clear `$Junk`, and move the
    /// message back to `dest` (the Inbox).
    MarkHam { path: String, uid: u32, dest: String },
    /// [`MailRequest::MarkHam`] for a selection, in one server round; answers
    /// with [`WorkerEvent::BulkComplete`] like `MoveMessages`.
    MarkHamMany { path: String, uids: Vec<u32>, dest: String },
    /// Add or remove the `\Seen` flag.
    SetSeen { path: String, uid: u32, seen: bool },
    /// Mark every message in a folder as read (`\Seen`).
    MarkAllRead { folder_id: u32, path: String },
    /// Add or remove the `\Flagged` flag.
    SetFlagged {
        path: String,
        uid: u32,
        flagged: bool,
    },
    /// Put a tag's keyword on a message, or take it off (#71). IMAP stores it
    /// as a user flag where the mailbox allows custom keywords, Microsoft 365
    /// as a category; POP3 and refusing servers keep it in the cache against
    /// `message_id` instead.
    SetKeyword {
        path: String,
        uid: u32,
        message_id: String,
        keyword: String,
        add: bool,
    },
    /// Move a message to another mailbox (archive / trash).
    MoveMessage {
        path: String,
        uid: u32,
        dest: String,
    },
    /// Move many messages from one mailbox to another in a single UID MOVE (bulk
    /// archive / delete / spam). Far faster and more reliable than one request per
    /// message on large mailboxes.
    MoveMessages {
        path: String,
        uids: Vec<u32>,
        dest: String,
    },
    /// Assemble a conversation from the local cache across every folder of the
    /// account: the messages whose Message-ID or references match `ids`. Answers
    /// with [`WorkerEvent::Related`]; never touches the network.
    /// Work through a folder's attachment-carrying messages that have never
    /// been described, recording what each one holds without downloading it.
    ScanAttachments { folder_path: String },
    LoadRelated { message_id: u32, ids: Vec<String> },
    /// Permanently erase messages from `path` (flag `\Deleted` + EXPUNGE), used
    /// when "delete" is asked for in Trash, where there is nowhere left to move to.
    PurgeMessages { path: String, uids: Vec<u32> },
    /// Erase every message in a folder (Empty Trash / Empty Junk, #152).
    EmptyFolder { folder_id: u32, path: String },
    /// Create a new mailbox (folder) at `path`.
    CreateFolder { path: String },
    /// Undo a move: find the messages (by Message-ID header — their UIDs
    /// changed in transit) in `path`, where a move just put them, and move
    /// them back to `dest`, then reload that folder so they reappear.
    UndoMove { path: String, dest: String, dest_folder_id: u32, message_ids: Vec<String> },
    /// Find a message by its (normalized) RFC Message-ID across every folder
    /// of the account, for a `mid:` link (#130) the cache could not resolve.
    /// Answered with [`WorkerEvent::Located`] either way.
    Locate { message_id: String },
    /// Move/rename a mailbox (drag-and-drop in the sidebar, #51). The server
    /// renames any child hierarchy along with it (RFC 3501 §6.3.5).
    RenameFolder { old_path: String, new_path: String },
    /// Delete a mailbox, first moving its contents to `trash` (if set).
    DeleteFolder { path: String, trash: Option<String> },
    /// Send a new message over SMTP, optionally APPENDing a copy to `sent_path`.
    Send {
        message: Box<OutgoingMessage>,
        sent_path: Option<String>,
    },
    /// Try the Outbox again now (a queued message, or every one of them).
    FlushOutbox { id: Option<u32> },
    /// Load the Outbox for display.
    LoadOutbox,
    /// Discard a queued message without sending it.
    DeleteOutbox { id: u32 },
    /// Save a message to the Drafts folder (`folder_id`/`path`) without sending.
    SaveDraft {
        message: Box<OutgoingMessage>,
        folder_id: u32,
        path: String,
    },
    /// Re-ask the server for every folder's unread count and answer with one
    /// [`WorkerEvent::FolderUnread`] per folder. Cheap (STATUS only — no message
    /// content), so the auto-fetch tick and the manual refresh broadcast it:
    /// only the visible folder and the IDLE-watched inbox ever re-sync on their
    /// own, and without this the other folders' sidebar chips go stale until
    /// each one is selected.
    RefreshUnread,
    /// Force a fresh connection and re-list folders (e.g. after a failure).
    Reconnect,
    /// The tag finder (Settings → Tags → Find Tags…): report every keyword
    /// in use across the account's folders as one
    /// [`WorkerEvent::KeywordsFound`] — always answered, even when empty, so
    /// the app can tell when every account has reported.
    FindKeywords,
    /// Bring every folder's cached keywords in line with the server (#166):
    /// a tag set or cleared on another device reaches the tag views here
    /// without the folder it sits in being opened. `keywords` are the
    /// configured tags. Answered with [`WorkerEvent::KeywordsSynced`].
    RefreshKeywords { keywords: Vec<String> },
}

/// A message composed by the user, ready to send.
#[derive(Debug, Clone)]
pub struct OutgoingMessage {
    /// The account to send from.
    pub from_account_id: u32,
    /// Send-as alias (#34): the full From for the wire ("Name <alias@host>"),
    /// or `None` to send as the account itself. The mail leaves through the
    /// account's SMTP, unless the alias is configured with its own transport.
    pub from_alias: Option<String>,
    /// Comma-separated recipient addresses.
    pub to: String,
    pub cc: String,
    pub bcc: String,
    /// Reply-To addresses (comma-separated); empty for none (#58).
    pub reply_to: String,
    pub subject: String,
    /// Plain-text body (always present; the `text/plain` alternative).
    pub body: String,
    /// HTML body. When non-empty the mail is sent multipart/alternative with this
    /// as the `text/html` part and `body` as the plain fallback.
    pub html: String,
    /// File paths to attach.
    pub attachments: Vec<String>,
    /// The Message-ID this is a reply to, and the thread's id chain — both
    /// normalized (no angle brackets), as [`crate::models::Message`] stores
    /// them. Empty for a message that starts a conversation. Without these
    /// headers a reply is a new thread to every client that receives it,
    /// including Vireo.
    pub in_reply_to: String,
    pub references: String,
    /// When editing an existing draft, the draft being replaced (removed from the
    /// Drafts folder after this message is saved or sent).
    pub draft_origin: Option<crate::models::DraftOrigin>,
    /// When this came out of the Outbox for editing, the queued row it replaces.
    /// Dropped once this version is sent or re-queued, so the queue never holds
    /// the message twice.
    pub outbox_origin: Option<u32>,
    /// Sign with the account's OpenPGP key (#133): PGP/MIME multipart/signed.
    pub sign: bool,
    /// Encrypt to every recipient's key (and the sender's): multipart/encrypted,
    /// signed inside when `sign` is set too.
    pub encrypt: bool,
    /// Send Later (#145): unix seconds to send at. `None` sends now. A time
    /// already past sends now too.
    pub send_at: Option<i64>,
}

/// An event pushed from the worker back to the UI.
#[derive(Debug)]
pub enum WorkerEvent {
    /// A batch of a folder's attachments was described (not downloaded).
    /// `remaining` is how many of its messages are still unscanned.
    AttachmentsScanned { folder_path: String, added: u32, remaining: u32 },
    Account(Account),
    Folders(Vec<Folder>),
    Messages { folder_id: u32, messages: Vec<Message> },
    /// What the server found for the "Message body" filter conditions
    /// (#191) among a folder's just-listed mail: uid → the lowercased
    /// alternatives it contains. Sent just ahead of the `Messages` it
    /// describes, and only for an inbox with body rules.
    BodyHits { folder_id: u32, hits: std::collections::HashMap<u32, Vec<String>> },
    /// Additional indexed message summaries for a folder, produced by the
    /// background backfill. Merged into the existing index without replacing it.
    MessagesAppend { folder_id: u32, messages: Vec<Message> },
    /// The rest of a conversation, gathered from the cache across the account's
    /// folders in answer to [`MailRequest::LoadRelated`]. `message_id` echoes the
    /// message it was asked for, so a late answer to a message the user has
    /// already moved on from can be ignored.
    Related { message_id: u32, messages: Vec<Message> },
    /// An undone move put these messages back in `folder_id`; sent after the
    /// folder's fresh [`WorkerEvent::Messages`] so the app can land the user
    /// on the restored message instead of wherever the reload left the list.
    Restored { folder_id: u32, message_ids: Vec<String> },
    /// The answer to [`MailRequest::Locate`]: where the message with that
    /// Message-ID lives (`folder_path`, `uid`), or `None` when no folder of
    /// this account has it.
    Located { message_id: String, hit: Option<(String, u32)> },
    /// The answer to [`MailRequest::FindKeywords`]: the keywords in use on
    /// this account, system flags left out.
    KeywordsFound(Vec<KeywordFinding>),
    /// The answer to [`MailRequest::RefreshKeywords`]: the folders whose
    /// cached keywords changed (empty when nothing did).
    KeywordsSynced { paths: Vec<String> },
    /// Cached attachments for an inbox, for the attachments gallery.
    /// The background backfill for a folder finished — its whole index is now
    /// present, so the UI can stop expecting more rows to stream in.
    BackfillDone { folder_id: u32 },
    /// A folder's load has been all the way to the server and back, however
    /// it went. Sent after the `Messages` the server's answer produced (and
    /// after an error, which produces none), so a manual filter run (#198)
    /// knows the folder is done rather than stopping at the cache's answer,
    /// which arrives first and instantly.
    FolderSynced { folder_id: u32 },
    /// Server-side unread count for a folder (from STATUS/SEARCH, independent of
    /// the loaded window — accurate even for multi-thousand mailboxes).
    FolderUnread { folder_id: u32, unread: u32 },
    /// The same, from a per-folder IDLE watcher ([`watch_folder`]), which knows
    /// its folder only by path: folder ids are positional and shift when the
    /// list changes, but a path stays true for as long as the folder exists.
    /// The app resolves it against whatever list it currently holds.
    FolderUnreadByPath { path: String, unread: u32 },
    /// A `SetSeen` has been stored (or failed): the app stops holding its own
    /// read state for that message over what the server reports.
    SeenSettled { path: String, uid: u32 },
    /// `path` is the folder the body was read from. A UID is unique only within
    /// its folder, so without it a background prefetch's body can be applied to a
    /// different message that happens to share the number.
    /// A chunk of a folder's threading references was repaired, so what the list
    /// is showing may now group differently.
    RefsRepaired { folder_id: u32 },
    Body { message_id: u32, path: String, body: String },
    /// Whether the message's From: address survived its provider's SPF/DKIM/DMARC
    /// checks. Sent right after `Body`, from the same fetch.
    SenderChecked { message_id: u32, check: crate::models::SenderCheck },
    Source { text: String },
    Attachments { message_id: u32, items: Vec<crate::models::Attachment> },
    /// The message has attachments that aren't cached; the UI should offer to
    /// download them rather than fetching automatically.
    AttachmentsPending { message_id: u32 },
    /// A message flagged as having an attachment turned out to have none once its
    /// body was fetched (e.g. iCloud marketing mail whose only extra parts are
    /// inline `cid:` images). The UI should drop its paperclip.
    NoAttachments { message_id: u32 },
    /// The opposite: a message with no paperclip turned out to carry attachments
    /// after all (an inline PDF the structure didn't advertise — issue #9). The
    /// UI should show the paperclip and offer the files.
    HasAttachments { message_id: u32 },
    Sent,
    /// Something worth telling the user that isn't a failure — a queued message
    /// going out on its own, say.
    Notice(String),
    /// The account's Outbox, whenever it changes (queued, retried, sent or
    /// discarded). Empty means nothing is waiting.
    Outbox { items: Vec<crate::models::OutboxItem> },
    /// A draft was saved to the Drafts folder.
    DraftSaved,
    /// A bulk MoveMessages request finished (success or failure) — drives the
    /// bulk-action spinner in the UI.
    BulkComplete,
    Status(String),
    /// `connectivity` marks connection/sync errors that should auto-clear once
    /// a later connect or sync succeeds.
    Error { text: String, connectivity: bool },
}

type ImapSession = Session<TlsStream<TcpStream>>;

/// A distinct accent colour per account (cycles through a small palette).
pub(crate) fn accent_for(account_id: u32) -> &'static str {
    const PALETTE: [&str; 6] = [
        "#3584e4", "#2ec27e", "#e5a50a", "#e66100", "#9141ac", "#c01c28",
    ];
    PALETTE[(account_id.saturating_sub(1) as usize) % PALETTE.len()]
}

/// Start a worker thread for one account (`Some`) or the offline sample data
/// (`None`). Returns the sender used to issue requests. `account_id` stamps all
/// emitted folders/messages and keys the cache.
pub fn spawn(
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
            .name(format!("vireo-cache-{account_id}"))
            .spawn(move || cache_lane(account_id, protocol, lane_rx, tx, emit))
            .expect("failed to spawn cache lane thread");
    }

    std::thread::Builder::new()
        .name(format!("vireo-mail-{account_id}"))
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    emit(WorkerEvent::Error {
                        text: i18n_f("runtime error: {e}", &[("e", &(e).to_string())]),
                        connectivity: false,
                    });
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

/// The fast lane in front of an account's worker: everything the disk cache
/// can answer is answered here, at once, and only what needs the network goes
/// on to the worker.
///
/// The worker handles one request at a time, and a folder sync or a
/// prefetch can hold it for a good while. Without this lane a click on a
/// message whose body was cached long ago would still queue behind that
/// work — at startup, the whole initial sync — and the reader would sit on
/// its spinner over mail that is already on disk. Cached mail must open
/// instantly whatever the worker is doing, so the lane runs on its own
/// thread with its own cache connection (WAL mode; readers never block).
///
/// Order is kept for whatever is forwarded: the lane is a single loop, so
/// two requests that both reach the worker arrive in the order the UI sent
/// them. The worker keeps its own cache-first arms, which cost a miss each
/// for a forwarded request and still serve its prefetch paths.
///
/// The offline sample backend (`protocol == None`) has no cache; everything
/// passes straight through. POP3 keeps its whole conversation in the inbox
/// and answers `LoadRelated` itself.
fn cache_lane(
    account_id: u32,
    protocol: Option<crate::config::Protocol>,
    mut rx: mpsc::UnboundedReceiver<MailRequest>,
    tx: mpsc::UnboundedSender<MailRequest>,
    emit: impl Fn(WorkerEvent),
) {
    let cache = protocol.and_then(|_| {
        Cache::open().map_err(|e| tracing::warn!("cache lane unavailable: {e}")).ok()
    });
    while let Some(req) = rx.blocking_recv() {
        let Some(c) = cache.as_ref() else {
            if tx.send(req).is_err() {
                break;
            }
            continue;
        };
        let forward = match req {
            MailRequest::LoadBody { message_id, path, uid } => {
                if serve_cached_body(c, account_id, &path, uid, message_id, &emit) {
                    None
                } else {
                    Some(MailRequest::LoadBody { message_id, path, uid })
                }
            }
            MailRequest::LoadBodies { items, path } => {
                let items: Vec<(u32, u32)> = items
                    .into_iter()
                    .filter(|(message_id, uid)| {
                        !serve_cached_body(c, account_id, &path, *uid, *message_id, &emit)
                    })
                    .collect();
                if items.is_empty() {
                    None
                } else {
                    Some(MailRequest::LoadBodies { items, path })
                }
            }
            MailRequest::LoadAttachments { message_id, path, uid, download } => {
                let items = c.load_attachments(account_id, &path, uid);
                if !items.is_empty() {
                    emit(WorkerEvent::Attachments { message_id, items });
                    None
                } else if !download {
                    emit(WorkerEvent::AttachmentsPending { message_id });
                    None
                } else {
                    Some(MailRequest::LoadAttachments { message_id, path, uid, download })
                }
            }
            MailRequest::LoadRelated { message_id, ids }
                if protocol != Some(crate::config::Protocol::Pop3) =>
            {
                emit(WorkerEvent::Related {
                    message_id,
                    messages: related_from_cache(c, account_id, &ids),
                });
                None
            }
            other => Some(other),
        };
        if let Some(req) = forward {
            if tx.send(req).is_err() {
                break;
            }
        }
    }
}

/// Emit a message's cached body (and its cached sender check, if any).
/// Returns false when the cache has no body for it.
fn serve_cached_body(
    cache: &Cache,
    account_id: u32,
    path: &str,
    uid: u32,
    message_id: u32,
    emit: &impl Fn(WorkerEvent),
) -> bool {
    let Some(body) = cache.load_body(account_id, path, uid) else {
        return false;
    };
    emit(WorkerEvent::Body { message_id, path: path.to_string(), body });
    if let Some(check) = cache.load_sender_check(account_id, path, uid) {
        emit(WorkerEvent::SenderChecked { message_id, check });
    }
    true
}

/// Assemble a conversation from the cache across every folder of the account:
/// the messages whose Message-ID or references match `ids`.
///
/// Folder ids are positional over the same ordered folder list the UI was
/// given, so a cached row's path maps back to the id the app knows it by —
/// which is what lets the reader fetch a related message's body and say which
/// folder it came from. Deleting or binning a message is a decision about it,
/// so Trash and Junk copies are left out: a conversation shouldn't quietly put
/// them back on screen.
fn related_from_cache(cache: &Cache, account_id: u32, ids: &[String]) -> Vec<Message> {
    let folders = cache.load_folders(account_id);
    cache
        .messages_by_thread_ids(account_id, ids)
        .into_iter()
        .filter_map(|(path, mut m)| {
            let f = folders.iter().find(|f| f.path == path)?;
            if matches!(f.kind, FolderKind::Trash | FolderKind::Junk) {
                return None;
            }
            m.folder_id = f.id;
            Some(m)
        })
        .collect()
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
        Some(account) => run_imap(account_id, account, rx, emit).await,
        None => run_mock(account_id, rx, emit).await,
    }
}

/// A request that puts mail in front of the reader: what the user is
/// waiting on right now.
fn is_reader_load(req: &MailRequest) -> bool {
    matches!(
        req,
        MailRequest::LoadBody { .. }
            | MailRequest::LoadBodies { .. }
            | MailRequest::LoadSource { .. }
            | MailRequest::LoadAttachments { download: true, .. }
    )
}

/// A folder list fetch: work the reader's loads may go ahead of.
fn is_list_load(req: &MailRequest) -> bool {
    matches!(req, MailRequest::LoadMessages { .. } | MailRequest::SyncFolder { .. })
}

/// Take everything else already queued behind `first` and move the reader's
/// loads ahead of the list syncs they sit behind, keeping every other order.
///
/// The worker serves one request at a time, so a message opened right after
/// a view that loads its folder (a notification click into Inboxes, which
/// asks every account for its inbox, then opens the mail) waited for the
/// whole list fetch — and the IDLE hand-off before it — before its body was
/// even asked for. The body is what the user is looking at; the list can
/// follow. A reader load only overtakes list fetches: it never passes a
/// move, a flag change or a reconnect, so what it reads is what the
/// requests before it left.
fn reorder_reader_loads(
    first: MailRequest,
    rx: &mut mpsc::UnboundedReceiver<MailRequest>,
    backlog: &mut std::collections::VecDeque<MailRequest>,
) -> MailRequest {
    let mut queue: Vec<MailRequest> = Vec::with_capacity(backlog.len() + 2);
    queue.push(first);
    queue.extend(backlog.drain(..));
    while let Ok(req) = rx.try_recv() {
        queue.push(req);
    }
    if queue.len() > 1 {
        let mut ordered: Vec<MailRequest> = Vec::with_capacity(queue.len());
        for req in queue {
            if is_reader_load(&req) {
                // Slot in ahead of the run of list fetches at the tail.
                let mut at = ordered.len();
                while at > 0 && is_list_load(&ordered[at - 1]) {
                    at -= 1;
                }
                ordered.insert(at, req);
            } else {
                ordered.push(req);
            }
        }
        queue = ordered;
    }
    let mut it = queue.into_iter();
    let first = it.next().expect("queue holds at least the request taken");
    backlog.extend(it);
    first
}

// ---------------------------------------------------------------------------
// IMAP path
// ---------------------------------------------------------------------------

async fn run_imap(
    account_id: u32,
    mut account: AccountConfig,
    mut rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent) + Clone + Send + 'static,
) {
    // Resolve the password off the main thread: migrate a legacy plaintext
    // password into the keyring (and strip it from disk), otherwise load it from
    // the keyring.
    if account.password.is_empty() {
        if let Some(pw) = crate::config::load_password(&account.email) {
            account.password = pw;
        }
    } else {
        let _ = crate::config::store_password(&account.email, &account.password);
        crate::config::strip_passwords_on_disk();
    }
    // Resolve the separate SMTP password from the keyring when in use.
    if account.smtp_separate && account.smtp_password.is_empty() {
        if let Some(pw) = crate::config::load_smtp_password(&account.email) {
            account.smtp_password = pw;
        }
    }
    // Still nothing, and the account came from GNOME Online Accounts? Ask GOA.
    // The import may have read it before GOA could unlock the keyring, and since
    // an imported account no longer exposes its password field there would
    // otherwise be no way to fix it (issue #17). Storing what comes back means a
    // later run works even if GOA is slow to start.
    if account.password.is_empty() && !account.oauth {
        if let Some(goa_id) = account.goa_id.clone() {
            let (imap, smtp) =
                tokio::task::spawn_blocking(move || crate::goa::mail_passwords(&goa_id))
                    .await
                    .unwrap_or((None, None));
            if let Some(pw) = imap {
                let _ = crate::config::store_password(&account.email, &pw);
                account.password = pw;
            }
            if let (true, Some(pw)) = (account.smtp_separate, smtp) {
                let _ = crate::config::store_smtp_password(&account.email, &pw);
                account.smtp_password = pw;
            }
            if account.password.is_empty() {
                emit(WorkerEvent::Error {
                    text: format!(
                        "GNOME Online Accounts has no password for {}. Open Settings → Online \
                         Accounts and sign in again.",
                        account.email
                    ),
                    connectivity: false,
                });
            }
        }
    }

    let cache = Cache::open()
        .map_err(|e| tracing::warn!("cache unavailable: {e}"))
        .ok();

    // Show the account + any cached folders immediately, before any network.
    emit(WorkerEvent::Account(Account {
        id: account_id,
        name: account.name.clone(),
        email: account.email.clone(),
        label: account.display_label(),
        accent: accent_for(account_id).into(),
    }));

    let cached_folders = cache
        .as_ref()
        .map(|c| c.load_folders(account_id))
        .unwrap_or_default();
    let have_cached_folders = !cached_folders.is_empty();
    if have_cached_folders {
        emit(WorkerEvent::Folders(cached_folders));
    }

    // The worker stays alive even if connecting fails, so the UI can retry. With
    // cached folders we connect lazily (on the first request) so cached mail can
    // render without waiting on the network; with an empty cache we connect now
    // to bootstrap the folder list.
    let mut session = if have_cached_folders {
        None
    } else {
        connect_and_list(account_id, &account, cache.as_ref(), &emit).await
    };

    // Attachments queued for background pre-download (folder_path, uid).
    let mut prefetch: std::collections::VecDeque<(String, u32)> = std::collections::VecDeque::new();
    // Message bodies queued for background pre-download, so new mail opens with no
    // network wait (folder_path, uid).
    let mut body_prefetch: std::collections::VecDeque<(String, u32)> =
        std::collections::VecDeque::new();
    // Bodies already pushed to the UI's in-memory cache this session, so they're
    // not re-sent on every folder re-sync.
    let mut body_emitted: std::collections::HashSet<(String, u32)> =
        std::collections::HashSet::new();
    // Folders queued for background index backfill (the rest of the mailbox past
    // the fast first page), and the set already enqueued this session.
    let mut backfill: std::collections::VecDeque<Backfill> = std::collections::VecDeque::new();
    let mut backfill_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Folders queued for the one-time References repair (see
    // [`run_one_refs_repair`]). Every folder gets one: a message indexed by an
    // older build carries only its In-Reply-To, and threading reads References.
    let mut refs_repair: std::collections::VecDeque<(u32, String)> =
        std::collections::VecDeque::new();
    // Requests already queued when one was taken, reordered so the reader's
    // loads come before list syncs (see `reorder_reader_loads`).
    let mut backlog: std::collections::VecDeque<MailRequest> = std::collections::VecDeque::new();
    // IMAP IDLE push: watch the most recently loaded folder for new mail.
    // The account's own setting wins over the global switch (#91).
    let push_enabled = account.push.unwrap_or_else(crate::config::load_push);
    let mut idle_folder: Option<(u32, String)> = None;
    // When the other folders' unread chips were last re-checked (None = not
    // yet this session; connect_and_list's full listing covers startup itself).
    let mut last_unread_sweep: Option<std::time::Instant> = None;
    // When the Junk / Trash auto-empty (#140) last ran for this connection.
    let mut last_auto_empty: Option<std::time::Instant> = None;
    // Whether the Inbox's permanent IDLE watcher has been spawned. It waits for
    // the main session to be up first — its connection must win the first slot
    // on servers with a per-user cap — and needs the folder list in the cache.
    // Push off = the user opted out of persistent connections entirely.
    let mut inbox_watch_spawned = !push_enabled;
    // The dynamic watcher pool: folders recently seen changing (or opened) each
    // hold an IDLE connection for a [`WATCH_LEASE`], keyed by path.
    let mut watchers: std::collections::HashMap<String, FolderWatch> =
        std::collections::HashMap::new();
    // Each folder's (unseen, UIDNEXT) from the previous sweep, for spotting the
    // folders that changed between sweeps.
    let mut sweep_baseline: std::collections::HashMap<String, (u32, Option<u32>)> =
        std::collections::HashMap::new();
    // The unread sweep's place (folders still to count) and whether one is
    // owed: IDLE waking, or a sweep a request interrupted. It runs from the
    // idle chain below, after the new mail's body prefetch, never inline.
    let mut sweep_pending: Vec<crate::models::Folder> = Vec::new();
    let mut sweep_due = false;
    // Set after prefetching; triggers one re-sync (to catch mail that arrived
    // while the connection was busy) before settling into the long IDLE.
    let mut pending_resync = false;
    // Whether to use IMAP's structured ENVELOPE/BODYSTRUCTURE. Disabled for the
    // session (falling back to raw-header parsing) if the server sends responses
    // our IMAP parser can't handle (e.g. iCloud).
    let mut use_envelope = true;
    // Whether the Outbox has been retried since this connection came up. A queued
    // message is almost always waiting on the network, so having a session again
    // is the moment worth retrying — not a timer.
    let mut outbox_flushed = false;

    // Queue every known folder for background indexing so search covers the whole
    // mailbox shortly after the first sync (like Apple Mail). The backfill skips
    // UIDs already cached, so this is cheap on subsequent runs.
    for f in cache
        .as_ref()
        .map(|c| c.load_folders(account_id))
        .unwrap_or_default()
    {
        if backfill_seen.insert(f.path.clone()) {
            // Messages indexed before Vireo fetched References thread by their
            // immediate parent alone — which for an incoming reply is usually
            // your own message, filed in Sent, so the link points outside the
            // folder and the reply starts a thread of its own.
            if cache
                .as_ref()
                .is_some_and(|c| !c.refs_repair_state(account_id, &f.path).1)
            {
                refs_repair.push_back((f.id, f.path.clone()));
            }
            backfill.push_back(Backfill {
                folder_id: f.id,
                gallery: gallery_folder(f.kind),
                path: f.path,
                remaining: None,
            });
        }
    }

    // Whether the user has been told this account's server keeps no tags
    // (one notice per session, not one per tagged message).
    let mut local_tags_noticed = false;
    loop {
        // Back online with something queued: send it before anything else, so a
        // message the user thinks they sent doesn't sit behind a mailbox sync.
        if session.is_some() {
            if !outbox_flushed {
                outbox_flushed = true;
                flush_outbox(
                    cache.as_ref(), account_id, &account, None, &mut session, &mut use_envelope,
                    &emit, false,
                )
                .await;
            }
        } else {
            outbox_flushed = false;
        }

        if !inbox_watch_spawned && session.is_some() {
            match cache.as_ref().map(|c| c.load_folders(account_id)) {
                // No cache = no folder list to watch from; stop checking.
                None => inbox_watch_spawned = true,
                Some(folders) if !folders.is_empty() => {
                    inbox_watch_spawned = true;
                    // The Inbox is watched permanently (no lease): it's where
                    // unfiltered mail lands, so its chip staying live is the
                    // baseline expectation whatever folder is being viewed.
                    if let Some(inbox) = folders.iter().find(|f| f.kind == FolderKind::Inbox) {
                        tokio::task::spawn_local(watch_folder(
                            account.clone(),
                            inbox.path.clone(),
                            Duration::ZERO,
                            None,
                            emit.clone(),
                        ));
                    }
                    // Seed the sweep baseline now rather than at the first
                    // timer tick, so the very first change a sweep sees —
                    // maybe the user's own manual refresh minutes from now —
                    // already has something to differ from and earns its
                    // folder a watcher. Also the first accurate (searched,
                    // not STATUSed) chip pass for servers where STATUS lies.
                    if let Some(sess) = session.as_mut() {
                        let (changed, complete) = refresh_unread_counts(
                            account_id,
                            sess,
                            cache.as_ref(),
                            idle_folder.as_ref().map(|(_, p)| p.as_str()),
                            &mut sweep_baseline,
                            &mut sweep_pending,
                            &rx,
                            &emit,
                        )
                        .await;
                        sweep_due = !complete;
                        last_unread_sweep = Some(std::time::Instant::now());
                        watch_changed_folders(&mut watchers, &account, &changed, &emit);
                        auto_empty_imap(account_id, &account, sess, cache.as_ref(), &mut last_auto_empty, &emit)
                            .await;
                    }
                }
                // Connected but the listing hasn't landed in the cache yet
                // (fresh account, first moments): check again next pass.
                Some(_) => {}
            }
        }

        // Always prefer incoming requests. When idle: drain the attachment prefetch
        // queue (fast), then index a chunk of the background backfill, then — if
        // push is on — re-sync once to catch any mail that arrived while busy, then
        // sit in a long IMAP IDLE for instant delivery. Without push, block for the
        // next request.
        let req = match backlog.pop_front().map_or_else(|| rx.try_recv(), Ok) {
            Ok(req) => req,
            Err(mpsc::error::TryRecvError::Disconnected) => break,
            Err(mpsc::error::TryRecvError::Empty) => {
                if !body_prefetch.is_empty() {
                    // Highest priority: get new mail's body cached so opening it is
                    // instant (no network wait).
                    run_one_body_prefetch(
                        &mut body_prefetch,
                        &mut session,
                        &account,
                        account_id,
                        cache.as_ref(),
                        &mut body_emitted,
                        &emit,
                    )
                    .await;
                    continue;
                } else if sweep_due && session.is_some() {
                    // The unread chips' re-count, owed since IDLE last woke
                    // (or left half done by a request): folder by folder,
                    // yielding to whatever arrives, its place kept.
                    let sess = session.as_mut().unwrap();
                    let (changed, complete) = refresh_unread_counts(
                        account_id,
                        sess,
                        cache.as_ref(),
                        idle_folder.as_ref().map(|(_, p)| p.as_str()),
                        &mut sweep_baseline,
                        &mut sweep_pending,
                        &rx,
                        &emit,
                    )
                    .await;
                    watch_changed_folders(&mut watchers, &account, &changed, &emit);
                    if complete {
                        sweep_due = false;
                        last_unread_sweep = Some(std::time::Instant::now());
                    }
                    continue;
                } else if !prefetch.is_empty() {
                    run_one_prefetch(
                        &mut prefetch,
                        &mut session,
                        &account,
                        account_id,
                        cache.as_ref(),
                        &emit,
                    )
                    .await;
                    pending_resync = true;
                    continue;
                } else if !refs_repair.is_empty() && session.is_some() && backfill.is_empty() {
                    // Lower priority than indexing new mail: repair the threading
                    // references of already-indexed replies, a chunk at a time.
                    run_one_refs_repair(
                        &mut refs_repair,
                        &mut session,
                        account_id,
                        cache.as_ref(),
                        &emit,
                    )
                    .await;
                    continue;
                } else if !backfill.is_empty() {
                    // Index the rest of the mailbox in the background. Connect first
                    // for cached-folder accounts (which connect lazily); if still
                    // offline, wait for a request instead of spinning.
                    if session.is_none() {
                        // A request beats the handshake. The first thing the UI
                        // asks for at startup is the visible folder, which the
                        // cache can answer without a network round trip — awaiting
                        // the connect here left the message list empty until the
                        // IMAP handshake finished (or timed out). Dropping the
                        // connect future only cancels this attempt; the next idle
                        // pass starts a fresh one.
                        tokio::select! {
                            biased;
                            req = rx.recv() => match req {
                                Some(req) => req,
                                None => break,
                            },
                            connected = connect_and_list(
                                account_id,
                                &account,
                                cache.as_ref(),
                                &emit,
                            ) => {
                                if connected.is_some() {
                                    session = connected;
                                    continue;
                                }
                                // Still offline: wait for a request rather than
                                // spinning on back-to-back reconnect attempts.
                                match rx.recv().await {
                                    Some(req) => req,
                                    None => break,
                                }
                            }
                        }
                    } else {
                        run_one_backfill(
                            &mut backfill,
                            &mut session,
                            &account,
                            account_id,
                            cache.as_ref(),
                            &mut prefetch,
                            &mut use_envelope,
                            &emit,
                        )
                        .await;
                        continue;
                    }
                } else if push_enabled && session.is_some() && idle_folder.is_some() {
                    let (fid, fpath) = idle_folder.clone().unwrap();
                    // Catch mail delivered while the connection was busy prefetching.
                    if pending_resync {
                        pending_resync = false;
                        if let Ok(messages) = load_messages_retry(
                            account_id,
                            &mut session,
                            &account,
                            fid,
                            &fpath,
                            &mut use_envelope,
                            cache.as_ref(),
                        )
                        .await
                        {
                            if let Some(c) = cache.as_ref() {
                                c.upsert_messages(account_id, &fpath, &messages);
                            }
                            queue_body_prefetch(
                                &mut body_prefetch,
                                &fpath,
                                &messages,
                                &body_emitted,
                            );
                            queue_attachment_prefetch(
                                &mut prefetch,
                                &fpath,
                                &messages,
                                cache.as_ref(),
                                account_id,
                            );
                            emit_body_hits(
                                &mut session, &account, account_id, fid, &fpath, &messages,
                                cache.as_ref(), &emit,
                            )
                            .await;
                            emit(WorkerEvent::Messages { folder_id: fid, messages });
                        }
                        continue;
                    }
                    match idle_wait(
                        &mut session,
                        &account,
                        account_id,
                        fid,
                        &fpath,
                        &mut rx,
                        cache.as_ref(),
                        &mut use_envelope,
                        &mut body_prefetch,
                        &mut prefetch,
                        &body_emitted,
                        &emit,
                        1740,
                    )
                    .await
                    {
                        IdleOutcome::Request(req) => req,
                        IdleOutcome::Refreshed | IdleOutcome::Quiet => {
                            // IDLE only watches one folder; mail filed by
                            // server-side rules lands in the others without ever
                            // waking it. Re-check their unread chips whenever
                            // IDLE surfaces — on new mail, and on the quiet
                            // timeout, which is the only clock a push-only
                            // (manual-fetch) account has.
                            // Owed, not run here: the idle chain runs it
                            // after the new mail's body is prefetched, and
                            // any request that arrives meanwhile goes first.
                            if last_unread_sweep
                                .map_or(true, |t| t.elapsed() >= UNREAD_SWEEP_MIN)
                            {
                                sweep_due = true;
                            }
                            continue;
                        }
                        IdleOutcome::Closed => break,
                    }
                } else {
                    match rx.recv().await {
                        Some(req) => req,
                        None => break,
                    }
                }
            }
        };
        let req = reorder_reader_loads(req, &mut rx, &mut backlog);

        if matches!(req, MailRequest::Reconnect) {
            session = connect_and_list(account_id, &account, cache.as_ref(), &emit).await;
            continue;
        }

        // Bodies of a `LoadBodies` batch that the disk cache couldn't answer, so
        // the network arm below fetches only those.
        let mut pending_bodies: Vec<(u32, u32)> = Vec::new();

        // Serve from cache first so mail appears instantly (and offline).
        match &req {
            MailRequest::LoadMessages { folder_id, path } => {
                if let Some(c) = cache.as_ref() {
                    let cached = c.load_messages(account_id, path, *folder_id);
                    if !cached.is_empty() {
                        emit(WorkerEvent::Messages {
                            folder_id: *folder_id,
                            messages: cached,
                        });
                    }
                }
            }
            MailRequest::LoadRelated { message_id, ids } => {
                let messages = cache
                    .as_ref()
                    .map(|c| related_from_cache(c, account_id, ids))
                    .unwrap_or_default();
                emit(WorkerEvent::Related { message_id: *message_id, messages });
                continue; // cache-only, never hits the network
            }
            MailRequest::LoadBody {
                message_id,
                path,
                uid,
            } => {
                if cache
                    .as_ref()
                    .is_some_and(|c| serve_cached_body(c, account_id, path, *uid, *message_id, &emit))
                {
                    continue; // already cached; no network needed
                }
            }
            MailRequest::LoadBodies { items, path } => {
                // Serve every member the cache already has, and leave the rest
                // for one fetch. A conversation reopened after its bodies were
                // cached costs no network at all.
                for (message_id, uid) in items {
                    if !cache.as_ref().is_some_and(|c| {
                        serve_cached_body(c, account_id, path, *uid, *message_id, &emit)
                    }) {
                        pending_bodies.push((*message_id, *uid));
                    }
                }
                if pending_bodies.is_empty() {
                    continue; // whole conversation was cached; no network needed
                }
            }
            MailRequest::LoadAttachments {
                message_id,
                path,
                uid,
                download,
            } => {
                if let Some(c) = cache.as_ref() {
                    let items = c.load_attachments(account_id, path, *uid);
                    if !items.is_empty() {
                        emit(WorkerEvent::Attachments {
                            message_id: *message_id,
                            items,
                        });
                        continue; // already cached; no network needed
                    }
                }
                // Not cached and the user hasn't asked to download — tell the UI
                // so it can offer a "Load attachments" button instead of fetching.
                if !download {
                    emit(WorkerEvent::AttachmentsPending { message_id: *message_id });
                    continue;
                }
            }
            _ => {}
        }

        // Everything below needs a live session.
        if session.is_none() {
            session = connect_and_list(account_id, &account, cache.as_ref(), &emit).await;
            if session.is_none() {
                // Still offline; cached data (if any) was already sent. Close
                // the folder's index anyway (#218): "we tried and could not
                // reach the server" has to end the list's "Loading more…"
                // spinner the same way success does, or an unreachable account
                // is indistinguishable from one that is still streaming — and
                // in a unified view one such account pins the spinner for
                // every account's mail.
                if let MailRequest::LoadMessages { folder_id, .. }
                | MailRequest::SyncFolder { folder_id, .. } = &req
                {
                    emit(WorkerEvent::BackfillDone { folder_id: *folder_id });
                }
                continue;
            }
        }
        // On a connection-shaped failure we drop the session to force a reconnect.
        let mut lost = false;
        // A background resync keeps quiet and leaves IDLE/watching alone.
        let background = matches!(req, MailRequest::SyncFolder { .. });

        match req {
            // Served from cache before this network match; never reached here.
            MailRequest::LoadRelated { .. } => {}
            MailRequest::LoadMessages { folder_id, path }
            | MailRequest::SyncFolder { folder_id, path } => {
                if !background {
                    emit(WorkerEvent::Status(i18n("Syncing…")));
                }
                // Fast first page (or a recent-window refresh over the cached
                // index); the background backfill indexes the rest of the folder.
                // Reads retry once across a reconnect, so an idle-dropped session
                // recovers transparently instead of surfacing an EOF.
                match load_messages_retry(
                    account_id,
                    &mut session,
                    &account,
                    folder_id,
                    &path,
                    &mut use_envelope,
                    cache.as_ref(),
                )
                .await
                {
                    Ok(messages) => {
                        if let Some(c) = cache.as_ref() {
                            // Upsert (not replace) so the background-indexed tail
                            // isn't wiped by a fast first-page load.
                            c.upsert_messages(account_id, &path, &messages);
                        }
                        // Ensure this folder gets fully indexed (if not already
                        // queued this session).
                        if backfill_seen.insert(path.clone()) {
                            backfill.push_back(Backfill {
                                folder_id,
                                gallery: folder_is_gallery(cache.as_ref(), account_id, &path),
                                path: path.clone(),
                                remaining: None,
                            });
                        }
                        // Pre-download recent bodies so opening them is instant.
                        queue_body_prefetch(
                            &mut body_prefetch,
                            &path,
                            &messages,
                            &body_emitted,
                        );
                        // Queue background attachment pre-downloads for recent
                        // messages that have them — older ones download on demand.
                        queue_attachment_prefetch(
                            &mut prefetch,
                            &path,
                            &messages,
                            cache.as_ref(),
                            account_id,
                        );
                        if !background {
                            idle_folder = Some((folder_id, path.clone()));
                            // Opening a folder is "use": keep it push-watched
                            // for the next lease hour, so changes still land
                            // instantly after the user moves on to another
                            // folder.
                            let kind = cache.as_ref().and_then(|c| {
                                c.load_folders(account_id)
                                    .into_iter()
                                    .find(|f| f.path == path)
                                    .map(|f| f.kind)
                            });
                            if kind.is_some_and(watchable) {
                                watch_active_folder(&mut watchers, &account, &path, 0, &emit);
                            }
                        }
                        emit_body_hits(
                            &mut session, &account, account_id, folder_id, &path, &messages,
                            cache.as_ref(), &emit,
                        )
                        .await;
                        // A load that came back with less than a full first
                        // page *is* the whole folder — an uncached folder asks
                        // for the newest FIRST_PAGE of however many the server
                        // holds, and a cached one is reconciled against the
                        // server's complete UID set — so its index is already
                        // complete and the background backfill has nothing to
                        // add. Saying so now (#218) instead of waiting for that
                        // job's turn matters because the job sits behind every
                        // other folder in the mailbox, and behind the body
                        // prefetch, unread sweep and attachment prefetch ahead
                        // of it in the idle ladder: a small inbox could wait
                        // hours for a "Loading more…" spinner that had nothing
                        // left to load. Larger folders are unaffected — the
                        // list stops showing the spinner once it holds a full
                        // page anyway.
                        let complete = messages.len() < FIRST_PAGE as usize;
                        emit(WorkerEvent::Messages { folder_id, messages });
                        if complete {
                            emit(WorkerEvent::BackfillDone { folder_id });
                        }
                        // Refresh the true unread count (catches new mail and
                        // reads from other clients beyond the loaded window).
                        if let Some(sess) = session.as_mut() {
                            let kind = cached_folder_kind(cache.as_ref(), account_id, &path);
                            if let Some(unread) = selected_chip_count(sess, kind).await {
                                emit(WorkerEvent::FolderUnread { folder_id, unread });
                            }
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not load {path}: {e}", &[("path", &(path).to_string()), ("e", &(e).to_string())]),
                            connectivity: true,
                        });
                        // The load failed, so nothing more is on its way for
                        // this folder: end its index (#218) rather than leave
                        // the list spinning on a backfill that will not run.
                        emit(WorkerEvent::BackfillDone { folder_id });
                    }
                }
                // However it went, this folder has been to the server (#198).
                emit(WorkerEvent::FolderSynced { folder_id });
                emit(WorkerEvent::Status(prefetch_status(prefetch.len())));
            }

            MailRequest::LoadBody {
                message_id,
                path,
                uid,
            } => match load_body_retry(&mut session, &account, &path, uid).await {
                Ok((body, check, has_attachments)) => {
                    if let Some(c) = cache.as_ref() {
                        if body_cacheable(&check) {
                            c.save_body(account_id, &path, uid, &body);
                        }
                        c.save_sender_check(account_id, &path, uid, &check);
                        c.set_has_attachment(account_id, &path, uid, has_attachments);
                    }
                    emit(WorkerEvent::Body { message_id, path: path.clone(), body });
                    emit(WorkerEvent::SenderChecked { message_id, check });
                    emit(if has_attachments {
                        WorkerEvent::HasAttachments { message_id }
                    } else {
                        WorkerEvent::NoAttachments { message_id }
                    });
                }
                Err(e) => {
                    emit(WorkerEvent::Error {
                        text: i18n_f("Could not load message: {e}", &[("e", &(e).to_string())]),
                        connectivity: true,
                    });
                }
            },

            MailRequest::LoadBodies { path, .. } => {
                for group in pending_bodies.chunks(BODY_FETCH_BATCH) {
                    // A failed batch leaves no session behind (the retry only
                    // hands one back on success), so the rest of the conversation
                    // needs one before it can go on.
                    if session.is_none() {
                        session = connect(&account).await.ok();
                    }
                    if session.is_none() {
                        break; // still offline; what's left loads on reopen
                    }
                    let uids: Vec<u32> = group.iter().map(|(_, uid)| *uid).collect();
                    match load_bodies_retry(&mut session, &account, &path, &uids).await {
                        Ok(mut fetched) => {
                            for (message_id, uid) in group {
                                let Some((body, check, has_attachments)) = fetched.remove(uid)
                                else {
                                    // The server answered the set but not this
                                    // UID — the message is gone from the folder.
                                    // Say so in place, rather than leaving one
                                    // member of the conversation blank forever.
                                    emit(WorkerEvent::Body {
                                        message_id: *message_id,
                                        path: path.clone(),
                                        body: "(empty message)".to_string(),
                                    });
                                    continue;
                                };
                                if let Some(c) = cache.as_ref() {
                                    if body_cacheable(&check) {
                                        c.save_body(account_id, &path, *uid, &body);
                                    }
                                    c.save_sender_check(account_id, &path, *uid, &check);
                                    c.set_has_attachment(account_id, &path, *uid, has_attachments);
                                }
                                emit(WorkerEvent::Body {
                                    message_id: *message_id,
                                    path: path.clone(),
                                    body,
                                });
                                emit(WorkerEvent::SenderChecked {
                                    message_id: *message_id,
                                    check,
                                });
                                emit(if has_attachments {
                                    WorkerEvent::HasAttachments { message_id: *message_id }
                                } else {
                                    WorkerEvent::NoAttachments { message_id: *message_id }
                                });
                            }
                        }
                        Err(e) => {
                            emit(WorkerEvent::Error {
                                text: i18n_f("Could not load conversation: {e}", &[("e", &(e).to_string())]),
                                connectivity: true,
                            });
                        }
                    }
                }
            }

            MailRequest::LoadSource { path, uid, .. } => match load_source_retry(
                &mut session,
                &account,
                &path,
                uid,
            )
            .await
            {
                Ok(text) => emit(WorkerEvent::Source { text }),
                Err(e) => {
                    emit(WorkerEvent::Error {
                        text: i18n_f("Could not load source: {e}", &[("e", &(e).to_string())]),
                        connectivity: true,
                    });
                }
            },

            MailRequest::LoadAttachments {
                message_id,
                path,
                uid,
                download: _,
            } => match load_raw_retry(&mut session, &account, &path, uid).await {
                Ok(raw) => {
                    let (items, cacheable) = attachments_of(&raw);
                    if let Some(c) = cache.as_ref() {
                        if cacheable {
                            c.save_attachments(account_id, &path, uid, &items);
                            c.mark_attachments_checked(account_id, &path, uid);
                        }
                    }
                    emit(WorkerEvent::Attachments { message_id, items });
                }
                Err(e) => {
                    emit(WorkerEvent::Error {
                        text: i18n_f("Could not load attachments: {e}", &[("e", &(e).to_string())]),
                        connectivity: true,
                    });
                }
            },

            MailRequest::SetSeen { path, uid, seen } => {
                let sess = session.as_mut().unwrap();
                if let Err(e) = store_flag(sess, &path, uid, "\\Seen", seen).await {
                    emit(WorkerEvent::Error {
                        text: i18n_f("Could not update message: {e}", &[("e", &(e).to_string())]),
                        connectivity: false,
                    });
                    lost = true;
                } else if let Some(c) = cache.as_ref() {
                    c.set_unread(account_id, &path, uid, !seen);
                }
                emit(WorkerEvent::SeenSettled { path, uid });
            }

            MailRequest::SetFlagged {
                path,
                uid,
                flagged,
            } => {
                let sess = session.as_mut().unwrap();
                if let Err(e) = store_flag(sess, &path, uid, "\\Flagged", flagged).await {
                    emit(WorkerEvent::Error {
                        text: i18n_f("Could not flag message: {e}", &[("e", &(e).to_string())]),
                        connectivity: false,
                    });
                    lost = true;
                } else if let Some(c) = cache.as_ref() {
                    c.set_starred(account_id, &path, uid, flagged);
                }
            }

            MailRequest::SetKeyword { path, uid, message_id, keyword, add } => {
                let sess = session.as_mut().unwrap();
                match store_keyword(sess, &path, uid, &keyword, add).await {
                    Ok(true) => {
                        if let Some(c) = cache.as_ref() {
                            c.set_keyword(account_id, &path, uid, &keyword, add);
                        }
                    }
                    Ok(false) => {
                        // The mailbox takes no custom keywords: keep the tag
                        // in Vireo, and say so once per session.
                        if let Some(c) = cache.as_ref() {
                            if message_id.is_empty() {
                                c.set_keyword(account_id, &path, uid, &keyword, add);
                            } else {
                                c.set_local_tag(account_id, &message_id, &keyword, add);
                            }
                        }
                        if !local_tags_noticed {
                            local_tags_noticed = true;
                            emit(WorkerEvent::Notice(i18n(
                                "This server does not store tags, so Vireo keeps them on this computer only",
                            )));
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not tag message: {e}", &[("e", &(e).to_string())]),
                            connectivity: false,
                        });
                        lost = true;
                    }
                }
            }

            MailRequest::MarkAllRead { folder_id, path } => {
                let sess = session.as_mut().unwrap();
                match mark_all_read(sess, &path).await {
                    Ok(()) => {
                        if let Some(c) = cache.as_ref() {
                            c.mark_folder_read(account_id, &path);
                        }
                        emit(WorkerEvent::FolderUnread { folder_id, unread: 0 });
                    }
                    Err(e) => {
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not mark folder read: {e}", &[("e", &(e).to_string())]),
                            connectivity: false,
                        });
                        lost = true;
                    }
                }
            }

            MailRequest::RefreshUnread => {
                let sess = session.as_mut().unwrap();
                let (changed, complete) = refresh_unread_counts(
                    account_id,
                    sess,
                    cache.as_ref(),
                    idle_folder.as_ref().map(|(_, p)| p.as_str()),
                    &mut sweep_baseline,
                    &mut sweep_pending,
                    &rx,
                    &emit,
                )
                .await;
                sweep_due = !complete;
                last_unread_sweep = Some(std::time::Instant::now());
                watch_changed_folders(&mut watchers, &account, &changed, &emit);
                auto_empty_imap(account_id, &account, sess, cache.as_ref(), &mut last_auto_empty, &emit)
                    .await;
            }

            MailRequest::MarkSpam { path, uid, dest } => {
                let sess = session.as_mut().unwrap();
                match mark_spam(sess, &path, uid, &dest).await {
                    Ok(created) => {
                        if let Some(c) = cache.as_ref() {
                            c.delete_message(account_id, &path, uid);
                        }
                        if created {
                            refresh_folders(account_id, &account, sess, cache.as_ref(), &emit).await;
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not mark as spam: {e}", &[("e", &(e).to_string())]),
                            connectivity: false,
                        });
                        lost = true;
                    }
                }
            }

            MailRequest::MarkHam { path, uid, dest } => {
                let sess = session.as_mut().unwrap();
                match mark_ham(sess, &path, &[uid], &dest).await {
                    Ok(created) => {
                        if let Some(c) = cache.as_ref() {
                            c.delete_message(account_id, &path, uid);
                        }
                        if created {
                            refresh_folders(account_id, &account, sess, cache.as_ref(), &emit).await;
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not mark as not spam: {e}", &[("e", &(e).to_string())]),
                            connectivity: false,
                        });
                        lost = true;
                    }
                }
            }

            MailRequest::MarkHamMany { path, uids, dest } => {
                let sess = session.as_mut().unwrap();
                match mark_ham(sess, &path, &uids, &dest).await {
                    Ok(created) => {
                        if let Some(c) = cache.as_ref() {
                            for uid in &uids {
                                c.delete_message(account_id, &path, *uid);
                            }
                        }
                        if created {
                            refresh_folders(account_id, &account, sess, cache.as_ref(), &emit).await;
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not mark {len} messages as not spam: {e}", &[("len", &(uids.len()).to_string()), ("e", &(e).to_string())]),
                            connectivity: false,
                        });
                        lost = true;
                    }
                }
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::MoveMessage { path, uid, dest } => {
                let sess = session.as_mut().unwrap();
                match move_message(sess, &path, uid, &dest).await {
                    Ok(created) => {
                        if let Some(c) = cache.as_ref() {
                            c.delete_message(account_id, &path, uid);
                        }
                        if created {
                            refresh_folders(account_id, &account, sess, cache.as_ref(), &emit).await;
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not move message: {e}", &[("e", &(e).to_string())]),
                            connectivity: false,
                        });
                        lost = true;
                    }
                }
            }

            MailRequest::MoveMessages { path, uids, dest } => {
                let sess = session.as_mut().unwrap();
                match move_messages(sess, &path, &uids, &dest).await {
                    Ok(created) => {
                        if let Some(c) = cache.as_ref() {
                            for uid in &uids {
                                c.delete_message(account_id, &path, *uid);
                            }
                        }
                        if created {
                            refresh_folders(account_id, &account, sess, cache.as_ref(), &emit).await;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "move failed: {path:?} -> {dest:?} set={} ({e})",
                            uid_set(&uids)
                        );
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not move {len} messages: {e}", &[("len", &(uids.len()).to_string()), ("e", &(e).to_string())]),
                            connectivity: false,
                        });
                        lost = true;
                    }
                }
                // Always signal completion so the UI's bulk spinner clears.
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::Locate { message_id } => {
                // A folder at a time: SELECT, then a HEADER search. Servers
                // whose header index lags (see UndoMove below) are the reason
                // the cache is asked first; this is the fallback for a message
                // Vireo has never listed.
                let sess = session.as_mut().unwrap();
                let mut hit: Option<(String, u32)> = None;
                match list_folders(account_id, sess, &account.folder_roles).await {
                    Ok(folders) => {
                        for f in folders {
                            if sel(sess, &f.path).await.is_err() {
                                continue;
                            }
                            let Ok(set) = search_uids(sess, format!("HEADER Message-ID \"{message_id}\""))
                                .await
                            else {
                                continue;
                            };
                            if let Some(uid) = set.into_iter().max() {
                                hit = Some((f.path.clone(), uid));
                                break;
                            }
                        }
                    }
                    Err(e) => tracing::warn!("[account {account_id}] locate: listing folders failed: {e}"),
                }
                emit(WorkerEvent::Located { message_id: message_id.clone(), hit });
            }

            MailRequest::ScanAttachments { folder_path } => {
                // Ask the server what a batch of messages holds, without
                // downloading any of it. BODYSTRUCTURE is a few hundred bytes
                // per message, so a whole archive costs single-digit megabytes
                // — the difference between a gallery that reaches back two
                // decades and one that shows only what has been opened.
                let Some(c) = cache.as_ref() else { continue };
                let uids = c.unscanned_attachment_uids(account_id, folder_path.as_str(), SCAN_CHUNK);
                if uids.is_empty() {
                    emit(WorkerEvent::AttachmentsScanned {
                        folder_path: folder_path.clone(),
                        added: 0,
                        remaining: 0,
                    });
                    continue;
                }
                let sess = session.as_mut().unwrap();
                if sel(sess, folder_path.as_str()).await.is_err() {
                    continue;
                }

                // One bad response must not cost the whole batch. A batch that
                // will not parse is halved and retried, down to the single
                // message responsible: its neighbours are described normally
                // and only it is written off. Smaller responses also parse more
                // often, so the split usually resolves the failure outright.
                let mut added = 0u32;
                let mut pending: Vec<Vec<u32>> = vec![uids];
                let mut budget = SCAN_SPLIT_BUDGET;
                while let Some(batch) = pending.pop() {
                    if budget == 0 {
                        break; // the rest keeps its place in the queue
                    }
                    budget -= 1;
                    let set = uid_set(&batch);
                    // BODYSTRUCTURE on its own, not beside ENVELOPE: the
                    // servers that make us fall back to raw headers (iCloud)
                    // usually choke on the ENVELOPE half — an unescaped quote
                    // in a Message-ID — so asking for the structure alone
                    // often parses where the combined fetch did not.
                    let fetched: Result<Vec<Fetch>, _> =
                        match fetch_uids(sess, &set, "BODYSTRUCTURE").await {
                            Ok(stream) => stream.try_collect().await,
                            Err(e) => Err(e),
                        };
                    match fetched {
                        Ok(fetches) => {
                            // Every UID asked about is answered, including the
                            // ones holding nothing: an empty list is still an
                            // answer, and recording it stops the message being
                            // asked about on every pass forever.
                            let mut seen: std::collections::HashSet<u32> =
                                std::collections::HashSet::new();
                            for f in &fetches {
                                let Some(uid) = f.uid else { continue };
                                seen.insert(uid);
                                let metas = f
                                    .bodystructure()
                                    .map(structure_attachments)
                                    .unwrap_or_default();
                                added += metas.len() as u32;
                                c.save_attachment_meta(account_id, folder_path.as_str(), uid, &metas);
                            }
                            for uid in batch.iter().filter(|u| !seen.contains(u)) {
                                c.save_attachment_meta(account_id, folder_path.as_str(), *uid, &[]);
                            }
                        }
                        // A dropped connection says nothing about these
                        // messages. Leave them unscanned and let the next pass
                        // have them, rather than recording an answer the server
                        // never gave.
                        Err(async_imap::error::Error::ConnectionLost) => {
                            pending.clear();
                            session.take();
                            break;
                        }
                        Err(e) if batch.len() > 1 => {
                            let mid = batch.len() / 2;
                            pending.push(batch[mid..].to_vec());
                            pending.push(batch[..mid].to_vec());
                            tracing::debug!(
                                "attachment scan of {folder_path}: splitting {} after {e}",
                                batch.len()
                            );
                        }
                        Err(e) => {
                            // One message whose structure our parser will not
                            // take. Record it as holding nothing rather than
                            // retrying it forever; opening it still fetches the
                            // message in full and lists what it really holds.
                            tracing::warn!(
                                "attachment scan of {folder_path} uid {}: {e}",
                                batch[0]
                            );
                            c.save_attachment_meta(account_id, folder_path.as_str(), batch[0], &[]);
                        }
                    }
                }

                emit(WorkerEvent::AttachmentsScanned {
                    folder_path: folder_path.clone(),
                    added,
                    remaining: c.unscanned_attachment_count(account_id, folder_path.as_str()),
                });
            }
            MailRequest::FindKeywords => {
                // Every folder, read-only: EXAMINE answers with the mailbox's
                // FLAGS line, which lists the keywords in use there, and a
                // KEYWORD search per candidate says how many messages carry
                // it (some servers list every keyword ever defined, so an
                // unused one is dropped here).
                let sess = session.as_mut().unwrap();
                let mut found: Vec<KeywordFinding> = Vec::new();
                match list_folders(account_id, sess, &account.folder_roles).await {
                    Ok(folders) => {
                        for f in &folders {
                            let Ok(mb) = exam(sess, &f.path).await else { continue };
                            let candidates: Vec<String> = mb
                                .flags
                                .iter()
                                .filter_map(|fl| match fl {
                                    Flag::Custom(k) if !is_system_keyword(k) => Some(k.to_string()),
                                    _ => None,
                                })
                                .collect();
                            for kw in candidates {
                                let n = match search_uids(sess, format!("KEYWORD {kw}")).await {
                                    Ok(set) => set.len(),
                                    Err(_) => 0,
                                };
                                if n == 0 {
                                    continue;
                                }
                                match found.iter_mut().find(|x| x.keyword.eq_ignore_ascii_case(&kw)) {
                                    Some(x) => {
                                        x.count += n;
                                        if !x.folders.contains(&f.name) {
                                            x.folders.push(f.name.clone());
                                        }
                                    }
                                    None => found.push(KeywordFinding {
                                        keyword: kw,
                                        name: None,
                                        color: None,
                                        count: n,
                                        folders: vec![f.name.clone()],
                                    }),
                                }
                            }
                        }
                    }
                    Err(e) => tracing::warn!("[account {account_id}] find tags: listing folders failed: {e}"),
                }
                emit(WorkerEvent::KeywordsFound(found));
            }

            MailRequest::RefreshKeywords { keywords } => {
                let sess = session.as_mut().unwrap();
                let paths = refresh_keywords(account_id, sess, cache.as_ref(), &keywords).await;
                emit(WorkerEvent::KeywordsSynced { paths });
            }

            MailRequest::UndoMove { path, dest, dest_folder_id, message_ids } => {
                // Find the moved messages where the move landed them. UIDs
                // changed in transit; Message-IDs did not.
                let mut uids: Vec<u32> = Vec::new();
                let mut failed: Option<String> = None;
                {
                    let sess = session.as_mut().unwrap();
                    let mut exists = 0u32;
                    match sel(sess, &path).await {
                        Err(e) => failed = Some(e.to_string()),
                        Ok(mb) => {
                            exists = mb.exists;
                            for id in &message_ids {
                                match search_uids(sess, format!("HEADER Message-ID \"{id}\"")).await {
                                    Ok(set) => uids.extend(set),
                                    Err(e) => {
                                        failed = Some(e.to_string());
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    uids.sort_unstable();
                    uids.dedup();
                    // Some servers (iCloud among them) can't be trusted with
                    // HEADER searches: the index lags behind a just-landed
                    // move, and our stored ids are lowercased while the
                    // server's match may be case-sensitive. FETCH has neither
                    // problem — before declaring the messages gone, read the
                    // newest headers (a move lands at the top of the UID
                    // space) and match Message-IDs locally.
                    if failed.is_none() && uids.is_empty() && exists > 0 {
                        let wanted: std::collections::HashSet<&str> =
                            message_ids.iter().map(String::as_str).collect();
                        let lo = exists.saturating_sub(199).max(1);
                        let scanned: Result<Vec<Fetch>, _> = async {
                            sess.fetch(
                                format!("{lo}:{exists}"),
                                "(UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])",
                            )
                            .await?
                            .try_collect()
                            .await
                        }
                        .await;
                        match scanned {
                            Ok(fetches) => uids.extend(fetches.iter().filter_map(|f| {
                                f.uid.filter(|_| wanted.contains(message_id_of(f).as_str()))
                            })),
                            Err(e) => failed = Some(e.to_string()),
                        }
                    }
                    if failed.is_none() && !uids.is_empty() {
                        if let Err(e) = move_messages(sess, &path, &uids, &dest).await {
                            failed = Some(e.to_string());
                        } else if let Some(c) = cache.as_ref() {
                            for uid in &uids {
                                c.delete_message(account_id, &path, *uid);
                            }
                        }
                    }
                }
                if let Some(e) = failed {
                    emit(WorkerEvent::Error {
                        text: i18n_f("Undo failed: {e}", &[("e", &(e).to_string())]),
                        connectivity: false,
                    });
                } else if uids.is_empty() {
                    // Not an error the reader needs interrupting for: the
                    // step is simply spent (#200).
                    tracing::info!("undo: the messages are no longer where that move put them");
                } else {
                    // Reload the restored folder so the messages reappear in
                    // the list (the app can't restore them optimistically —
                    // it never learns their new UIDs).
                    if let Ok(messages) = load_messages_retry(
                        account_id, &mut session, &account, dest_folder_id, &dest,
                        &mut use_envelope, cache.as_ref(),
                    )
                    .await
                    {
                        if let Some(c) = cache.as_ref() {
                            c.upsert_messages(account_id, &dest, &messages);
                        }
                        emit(WorkerEvent::Messages { folder_id: dest_folder_id, messages });
                        emit(WorkerEvent::Restored {
                            folder_id: dest_folder_id,
                            message_ids: message_ids.clone(),
                        });
                    }
                }
                // Undoing something is not news: the messages reappearing in
                // the list is the whole report (#200).
                // Always signal completion — the app spins the refresh
                // indicator while the undo's server work is in flight.
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::PurgeMessages { path, uids } => {
                let sess = session.as_mut().unwrap();
                match purge_messages(sess, &path, &uids).await {
                    Ok(()) => {
                        if let Some(c) = cache.as_ref() {
                            for uid in &uids {
                                c.delete_message(account_id, &path, *uid);
                            }
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not delete permanently: {e}", &[("e", &(e).to_string())]),
                            connectivity: false,
                        });
                        lost = true;
                    }
                }
                // Always signal completion so the UI's bulk spinner clears.
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::EmptyFolder { folder_id, path } => {
                let sess = session.as_mut().unwrap();
                let found = match sel(sess, &path).await {
                    Ok(_) => search_uids(sess, "ALL").await,
                    Err(e) => Err(e),
                };
                let result = match found {
                    Ok(set) => {
                        let uids: Vec<u32> = set.into_iter().collect();
                        purge_messages(sess, &path, &uids).await.map(|()| uids)
                    }
                    Err(e) => Err(e),
                };
                match result {
                    Ok(uids) => {
                        if let Some(c) = cache.as_ref() {
                            for uid in &uids {
                                c.delete_message(account_id, &path, *uid);
                            }
                        }
                        tracing::info!("emptied {path}: {} message(s) erased", uids.len());
                        emit(WorkerEvent::Messages { folder_id, messages: Vec::new() });
                        emit(WorkerEvent::FolderUnread { folder_id, unread: 0 });
                    }
                    Err(e) => {
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not empty the folder: {e}", &[("e", &e.to_string())]),
                            connectivity: false,
                        });
                        lost = true;
                    }
                }
            }

            MailRequest::CreateFolder { path } => {
                let sess = session.as_mut().unwrap();
                match create_folder(sess, &path).await {
                    Ok(()) => refresh_folders(account_id, &account, sess, cache.as_ref(), &emit).await,
                    Err(e) => {
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not create folder: {e}", &[("e", &(e).to_string())]),
                            connectivity: false,
                        });
                        lost = true;
                    }
                }
            }

            MailRequest::RenameFolder { old_path, new_path } => {
                let sess = session.as_mut().unwrap();
                match rename_folder(sess, &old_path, &new_path).await {
                    Ok(()) => refresh_folders(account_id, &account, sess, cache.as_ref(), &emit).await,
                    Err(e) => {
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not move folder: {e}", &[("e", &(e).to_string())]),
                            connectivity: false,
                        });
                        lost = true;
                    }
                }
            }

            MailRequest::DeleteFolder { path, trash } => {
                let sess = session.as_mut().unwrap();
                match delete_folder(sess, &path, trash.as_deref()).await {
                    Ok(()) => refresh_folders(account_id, &account, sess, cache.as_ref(), &emit).await,
                    Err(e) => {
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not delete folder: {e}", &[("e", &(e).to_string())]),
                            connectivity: false,
                        });
                        lost = true;
                    }
                }
            }

            // Send Later (#145): into the Outbox until its time.
            MailRequest::Send { message, sent_path }
                if message.send_at.is_some_and(|t| t > crate::datefmt::now()) =>
            {
                let at = message.send_at.unwrap_or_default();
                schedule_send(cache.as_ref(), account_id, &account, &message, sent_path.as_deref(), at, &emit);
                // The draft it was opened from is superseded by the queued copy.
                if let Some(o) = message.draft_origin.clone() {
                    if o.account_id == account_id {
                        if let Some(sess) = session.as_mut() {
                            let _ = delete_draft(sess, &o.path, o.uid).await;
                        }
                        if let Some(c) = cache.as_ref() {
                            c.delete_message(account_id, &o.path, o.uid);
                        }
                        if let Ok(messages) = load_messages_retry(
                            account_id, &mut session, &account, o.folder_id, &o.path,
                            &mut use_envelope, cache.as_ref(),
                        )
                        .await
                        {
                            emit(WorkerEvent::Messages { folder_id: o.folder_id, messages });
                        }
                    }
                }
            }

            MailRequest::Send { message, sent_path } => {
                emit(WorkerEvent::Status(i18n("Sending…")));
                match send_smtp(&account, &message).await {
                    Ok(raw) => {
                        emit(WorkerEvent::Status(String::new()));
                        // Save a copy to the Sent folder; sending still counts as
                        // success even if this part fails.
                        if let Some(path) = sent_path {
                            let sess = session.as_mut().unwrap();
                            if let Err(e) = append_to_sent(sess, &path, &raw).await {
                                emit(WorkerEvent::Error {
                                    text: i18n_f("Message sent, but saving to Sent failed: {e}", &[("e", &(e).to_string())]),
                                    connectivity: false,
                                });
                            } else {
                                index_sent_copy(
                                    account_id, &mut session, &account, &path,
                                    &mut use_envelope, cache.as_ref(), &emit,
                                )
                                .await;
                            }
                        }
                        // If sending an edited draft (from this account), remove the
                        // now-obsolete draft and refresh the Drafts folder.
                        if let Some(o) = message.draft_origin.clone() {
                            if o.account_id == account_id {
                                {
                                    let sess = session.as_mut().unwrap();
                                    let _ = delete_draft(sess, &o.path, o.uid).await;
                                }
                                if let Some(c) = cache.as_ref() {
                                    c.delete_message(account_id, &o.path, o.uid);
                                }
                                if let Ok(messages) = load_messages_retry(
                                    account_id, &mut session, &account, o.folder_id, &o.path,
                                    &mut use_envelope, cache.as_ref(),
                                )
                                .await
                                {
                                    if let Some(c) = cache.as_ref() {
                                        c.upsert_messages(account_id, &o.path, &messages);
                                    }
                                    emit(WorkerEvent::Messages { folder_id: o.folder_id, messages });
                                }
                            }
                        }
                        // This version replaces the queued one it was edited from.
                        if let (Some(queued), Some(c)) = (message.outbox_origin, cache.as_ref()) {
                            c.delete_outbox(queued);
                            emit_outbox(cache.as_ref(), account_id, &emit);
                        }
                        emit(WorkerEvent::Sent);
                    }
                    Err(e) => {
                        emit(WorkerEvent::Status(String::new()));
                        // Hold the message rather than losing it: the composer is
                        // already closed by the time this arrives, so anything not
                        // queued here is gone (issue #15). Being offline is the
                        // usual reason a send fails, which is exactly when saving
                        // to the server's Drafts folder would fail too.
                        let queued = queue_failed_send(
                            cache.as_ref(),
                            account_id,
                            &account,
                            &message,
                            sent_path.as_deref(),
                            &e.to_string(),
                        );
                        // Queue first, drop the superseded row second: a crash in
                        // between leaves the message queued twice, which is
                        // recoverable, rather than not at all.
                        if let (true, Some(old), Some(c)) =
                            (queued, message.outbox_origin, cache.as_ref())
                        {
                            c.delete_outbox(old);
                        }
                        emit(WorkerEvent::Error {
                            text: if queued {
                                i18n_f("Send failed: {e}. The message is in the Outbox and will be sent when the connection is back.", &[("e", &e.to_string())])
                            } else {
                                i18n_f("Send failed: {e}", &[("e", &e.to_string())])
                            },
                            connectivity: false,
                        });
                        emit_outbox(cache.as_ref(), account_id, &emit);
                    }
                }
            }

            MailRequest::LoadOutbox => emit_outbox(cache.as_ref(), account_id, &emit),

            MailRequest::DeleteOutbox { id } => {
                if let Some(c) = cache.as_ref() {
                    c.delete_outbox(id);
                }
                emit_outbox(cache.as_ref(), account_id, &emit);
            }

            MailRequest::FlushOutbox { id } => {
                flush_outbox(
                    cache.as_ref(),
                    account_id,
                    &account,
                    id,
                    &mut session,
                    &mut use_envelope,
                    &emit,
                    true,
                )
                .await;
            }

            MailRequest::SaveDraft { message, folder_id, path } => {
                emit(WorkerEvent::Status(i18n("Saving draft…")));
                // A draft is kept as written: signing and encrypting happen
                // at send time (#133).
                let message = OutgoingMessage { sign: false, encrypt: false, ..*message };
                match build_draft(&account, &message) {
                    Ok(email) => {
                        let raw = email.formatted();
                        let append_res = {
                            let sess = session.as_mut().unwrap();
                            let r = append_draft(sess, &path, &raw).await;
                            // Replace the previous version of this draft (same account).
                            if r.is_ok() {
                                if let Some(o) = &message.draft_origin {
                                    if o.account_id == account_id {
                                        let _ = delete_draft(sess, &o.path, o.uid).await;
                                    }
                                }
                            }
                            r
                        };
                        match append_res {
                            Ok(()) => {
                                if let Some(o) = &message.draft_origin {
                                    if o.account_id == account_id {
                                        if let Some(c) = cache.as_ref() {
                                            c.delete_message(account_id, &o.path, o.uid);
                                        }
                                    }
                                }
                                // Reload Drafts so the saved draft appears.
                                if let Ok(messages) = load_messages_retry(
                                    account_id, &mut session, &account, folder_id, &path,
                                    &mut use_envelope, cache.as_ref(),
                                )
                                .await
                                {
                                    if let Some(c) = cache.as_ref() {
                                        c.upsert_messages(account_id, &path, &messages);
                                    }
                                    emit(WorkerEvent::Messages { folder_id, messages });
                                }
                                // Surface a newly-created Drafts folder in the sidebar.
                                if let Some(sess) = session.as_mut() {
                                    refresh_folders(account_id, &account, sess, cache.as_ref(), &emit).await;
                                }
                                // Saved as a draft instead of sent: the queued
                                // copy it was edited from is now superseded.
                                if let (Some(queued), Some(c)) =
                                    (message.outbox_origin, cache.as_ref())
                                {
                                    c.delete_outbox(queued);
                                    emit_outbox(cache.as_ref(), account_id, &emit);
                                }
                                emit(WorkerEvent::Status(String::new()));
                                emit(WorkerEvent::DraftSaved);
                            }
                            Err(e) => {
                                emit(WorkerEvent::Status(String::new()));
                                emit(WorkerEvent::Error {
                                    text: i18n_f("Could not save draft: {e}", &[("e", &(e).to_string())]),
                                    connectivity: false,
                                });
                                lost = true;
                            }
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::Status(String::new()));
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not save draft: {e}", &[("e", &(e).to_string())]),
                            connectivity: false,
                        });
                    }
                }
            }

            MailRequest::Reconnect => unreachable!("handled above"),
        }

        if lost {
            session = None;
        }
    }

    if let Some(mut session) = session {
        let _ = session.logout().await;
    }
}

/// Connect, announce the account, and list folders, updating the cache. Emits a
/// fresh `Folders` event only when the listing differs from the cache (so an
/// unchanged list doesn't trigger a redundant UI rebuild). Returns `None` (after
/// emitting an error) if the connection could not be established.
async fn connect_and_list(
    account_id: u32,
    account: &AccountConfig,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) -> Option<ImapSession> {
    emit(WorkerEvent::Status(i18n_f("Connecting to {host}…", &[("host", &account.imap_host)])));

    let result = match connect(account).await {
        Ok(mut session) => {
            emit(WorkerEvent::Account(Account {
                id: account_id,
                name: account.name.clone(),
                email: account.email.clone(),
                label: account.display_label(),
                accent: accent_for(account_id).into(),
            }));
            match list_folders(account_id, &mut session, &account.folder_roles).await {
                // An empty LIST can't be right — INBOX always exists (RFC
                // 3501). Keep whatever the cache has instead of wiping it.
                Ok(folders) if folders.is_empty() => {}
                Ok(folders) => {
                    let changed = cache
                        .map(|c| !crate::cache::folders_equal(&c.load_folders(account_id), &folders))
                        .unwrap_or(true);
                    if let Some(c) = cache {
                        c.save_folders(account_id, &folders);
                    }
                    if changed {
                        emit(WorkerEvent::Folders(folders));
                    }
                }
                Err(e) => emit(WorkerEvent::Error {
                    text: i18n_f("Could not list folders: {e}", &[("e", &(e).to_string())]),
                    connectivity: true,
                }),
            }
            Some(session)
        }
        Err(e) => {
            emit(WorkerEvent::Error {
                text: i18n_f("Connection failed: {e}", &[("e", &(e).to_string())]),
                connectivity: true,
            });
            None
        }
    };

    emit(WorkerEvent::Status(String::new()));
    result
}

/// Run `load_messages`, retrying once over a fresh login if the first attempt
/// fails (typically a server-dropped idle connection: "unexpected EOF"). On
/// success the (possibly new) session is stored back; on failure it is dropped.
async fn load_messages_retry(
    account_id: u32,
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    folder_id: u32,
    path: &str,
    use_envelope: &mut bool,
    cache: Option<&Cache>,
) -> Result<Vec<Message>, async_imap::error::Error> {
    let mut s = session.take().expect("session ensured before call");
    let first = load_messages(account_id, &mut s, folder_id, path, *use_envelope, cache).await;

    // A non-empty success is trustworthy — keep the session and return it.
    if matches!(&first, Ok(msgs) if !msgs.is_empty()) {
        *session = Some(s);
        return first;
    }

    // Otherwise the result is an error (stale connection → EOF, or a BODYSTRUCTURE
    // our parser rejected) or an empty mailbox (which a stale session can return
    // without erroring). Re-verify on a fresh login. An *unverified* empty result
    // is treated as a failure so it can never wipe cached mail.
    match connect(account).await {
        Ok(fresh) => {
            s = fresh;
            // If the first attempt errored while parsing the structured ENVELOPE/
            // BODYSTRUCTURE, the server likely sends non-compliant responses (e.g.
            // iCloud). Fall back to raw-header parsing for the rest of the session.
            if first.is_err() && *use_envelope {
                *use_envelope = false;
            }
            let second =
                load_messages(account_id, &mut s, folder_id, path, *use_envelope, cache).await;
            if second.is_ok() {
                *session = Some(s);
            }
            second
        }
        Err(_) => match first {
            Ok(_) => Err(async_imap::error::Error::ConnectionLost),
            Err(e) => Err(e),
        },
    }
}

/// Like [`load_messages_retry`], but for a single body.
async fn load_body_retry(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    path: &str,
    uid: u32,
) -> Result<(String, crate::models::SenderCheck, bool), async_imap::error::Error> {
    let mut s = session.take().expect("session ensured before call");
    let mut res = load_body(&mut s, path, uid).await;
    if res.is_err() {
        if let Ok(fresh) = connect(account).await {
            s = fresh;
            res = load_body(&mut s, path, uid).await;
        }
    }
    if res.is_ok() {
        *session = Some(s);
    }
    res
}

/// [`load_bodies`] with [`load_body_retry`]'s one reconnect-and-retry.
async fn load_bodies_retry(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    path: &str,
    uids: &[u32],
) -> Result<
    std::collections::HashMap<u32, (String, crate::models::SenderCheck, bool)>,
    async_imap::error::Error,
> {
    let mut s = session.take().expect("session ensured before call");
    let mut res = load_bodies(&mut s, path, uids).await;
    if res.is_err() {
        if let Ok(fresh) = connect(account).await {
            s = fresh;
            res = load_bodies(&mut s, path, uids).await;
        }
    }
    if res.is_ok() {
        *session = Some(s);
    }
    res
}

async fn load_source_retry(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    path: &str,
    uid: u32,
) -> Result<String, async_imap::error::Error> {
    let mut s = session.take().expect("session ensured before call");
    let mut res = load_source(&mut s, path, uid).await;
    if res.is_err() {
        if let Ok(fresh) = connect(account).await {
            s = fresh;
            res = load_source(&mut s, path, uid).await;
        }
    }
    if res.is_ok() {
        *session = Some(s);
    }
    res
}

/// Fetch the raw RFC 822 source (headers + body) of a message, undecoded.
async fn load_source(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
) -> Result<String, async_imap::error::Error> {
    sel(session, path).await?;

    let fetches: Vec<Fetch> = fetch_uids(session, uid.to_string(), "(BODY.PEEK[])")
        .await?
        .try_collect()
        .await?;

    let raw = fetches
        .iter()
        .find_map(|f| f.body())
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_else(|| "(empty message)".to_string());

    Ok(raw)
}

/// Result of an IDLE wait: a request to handle, a folder that was refreshed
/// (new mail arrived), or the channel closing.
enum IdleOutcome {
    Request(MailRequest),
    Refreshed,
    Quiet,
    Closed,
}

/// Download and cache one queued attachment (body + attachments), if the session
/// is live. Connects on demand; clears the queue if offline.
async fn run_one_prefetch(
    prefetch: &mut std::collections::VecDeque<(String, u32)>,
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    let Some((path, uid)) = prefetch.front().cloned() else {
        return;
    };
    if session.is_none() {
        *session = connect_and_list(account_id, account, cache, emit).await;
    }
    if session.is_some() {
        prefetch.pop_front();
        // Tried is tried, whatever comes of it: the resync this pass ends
        // with must not queue the same message again.
        note_attachment_prefetch_tried(account_id, &path, uid);
        let already = cache
            .map(|c| c.attachments_checked(account_id, &path, uid))
            .unwrap_or(false);
        if !already {
            if let Ok(raw) = load_raw_retry(session, account, &path, uid).await {
                // OpenPGP mail is left for an explicit open (#133): nothing
                // of it is cached, and decrypting here would raise the
                // passphrase prompt out of nowhere.
                if crate::pgp::detect(&raw).is_some() {
                    emit(WorkerEvent::Status(prefetch_status(prefetch.len())));
                    return;
                }
                let (_, check, _) = render_raw(&raw);
                let (attachments, _) = attachments_of(&raw);
                emit(WorkerEvent::SenderChecked { message_id: uid, check: check.clone() });
                if let Some(c) = cache {
                    c.save_body(account_id, &path, uid, &render_raw(&raw).0);
                    c.save_sender_check(account_id, &path, uid, &check);
                    c.save_attachments(account_id, &path, uid, &attachments);
                    // Mark as fetched so it's never re-downloaded to re-check,
                    // even if it had no attachments after all.
                    c.mark_attachments_checked(account_id, &path, uid);
                }
                // Correct a false paperclip live: a message flagged as having an
                // attachment but with none once fetched (iCloud multipart/mixed
                // wrapping only inline images).
                if attachments.is_empty() {
                    emit(WorkerEvent::NoAttachments { message_id: uid });
                }
            }
        }
        emit(WorkerEvent::Status(prefetch_status(prefetch.len())));
    } else {
        prefetch.clear();
        emit(WorkerEvent::Status(String::new()));
    }
}

/// Queue the newest messages for background body prefetch, so opening new mail is
/// instant. The prefetch both caches the body on disk *and* pushes it to the UI's
/// in-memory cache (see [`run_one_body_prefetch`]), so a click needs no round-trip.
/// Skips messages already pushed this session. Called after every folder sync.
fn queue_body_prefetch(
    queue: &mut std::collections::VecDeque<(String, u32)>,
    path: &str,
    messages: &[Message],
    emitted: &std::collections::HashSet<(String, u32)>,
) {
    let mut recent: Vec<&Message> = messages.iter().collect();
    recent.sort_by(|a, b| b.uid.cmp(&a.uid)); // newest first
    for m in recent.into_iter().take(PREFETCH_BODY_LIMIT) {
        let key = (path.to_string(), m.uid);
        if emitted.contains(&key) {
            continue;
        }
        if queue.iter().any(|(p, u)| p == path && *u == m.uid) {
            continue;
        }
        queue.push_back(key);
    }
}

/// Queue the newest messages that have attachments (not already cached) for
/// background attachment pre-download, so new mail's attachments are ready too.
/// Whether a folder's attachments feed the gallery (so its mail is worth
/// prefetching): everything except Trash, Junk and Drafts.
fn gallery_folder(kind: crate::models::FolderKind) -> bool {
    use crate::models::FolderKind::*;
    !matches!(kind, Trash | Junk | Drafts)
}

/// Gallery eligibility of a folder by path, looked up from the cached folder list.
/// Unknown folders default to eligible.
fn folder_is_gallery(cache: Option<&Cache>, account_id: u32, path: &str) -> bool {
    cache
        .map(|c| {
            c.load_folders(account_id)
                .iter()
                .find(|f| f.path == path)
                .map(|f| gallery_folder(f.kind))
                .unwrap_or(true)
        })
        .unwrap_or(true)
}

fn queue_attachment_prefetch(
    queue: &mut std::collections::VecDeque<(String, u32)>,
    path: &str,
    messages: &[Message],
    cache: Option<&Cache>,
    account_id: u32,
) {
    let mut recent: Vec<&Message> = messages.iter().collect();
    recent.sort_by(|a, b| b.uid.cmp(&a.uid)); // newest first
    for m in recent.into_iter().take(PREFETCH_LIMIT) {
        if !m.has_attachment {
            continue;
        }
        // Skip messages we've already fetched attachments for — including ones
        // that turned out to have none (so false "has attachment" flags, e.g.
        // iCloud's multipart/mixed, aren't re-downloaded on every sync) — and
        // ones this session already tried and could not finish (see
        // [`ATTACHMENT_PREFETCH_TRIED`]).
        let checked = cache
            .map(|c| c.attachments_checked(account_id, path, m.uid))
            .unwrap_or(false);
        let tried = attachment_prefetch_tried(account_id, path, m.uid);
        let queued = queue.iter().any(|(p, u)| p == path && *u == m.uid);
        if !checked && !tried && !queued {
            queue.push_back((path.to_string(), m.uid));
        }
    }
}

/// Prefetch one queued message body: serve it from the disk cache if present,
/// otherwise fetch and cache it. Either way, push it to the UI so its in-memory
/// cache is warm and clicking the message renders instantly with no round-trip.
async fn run_one_body_prefetch(
    queue: &mut std::collections::VecDeque<(String, u32)>,
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    emitted: &mut std::collections::HashSet<(String, u32)>,
    emit: &impl Fn(WorkerEvent),
) {
    let Some((path, uid)) = queue.front().cloned() else {
        return;
    };
    // A cached body needs no connection — serve it straight to the UI, with the
    // verdict stored alongside it. The two must always travel together: the UI
    // renders from whichever arrives, and a body without its verdict leaves the
    // sender badge blank with no second chance to fill it.
    if let Some(body) = cache.and_then(|c| c.load_body(account_id, &path, uid)) {
        queue.pop_front();
        emit(WorkerEvent::Body { message_id: uid, path: path.clone(), body });
        if let Some(check) = cache.and_then(|c| c.load_sender_check(account_id, &path, uid)) {
            emit(WorkerEvent::SenderChecked { message_id: uid, check });
        }
        emitted.insert((path, uid));
        return;
    }
    if session.is_none() {
        *session = connect_and_list(account_id, account, cache, emit).await;
    }
    if session.is_none() {
        queue.clear();
        return;
    }
    queue.pop_front();
    let Ok(raw) = load_raw_retry(session, account, &path, uid).await else {
        // One failed read is enough for this session: left out of `emitted`,
        // the message would be queued again by every resync and fetched
        // again each time.
        emitted.insert((path, uid));
        return;
    };
    {
        // OpenPGP mail (#133) is left for an explicit open: nothing of it is
        // cached or pushed ahead, and rendering it here would decrypt in the
        // background — a passphrase prompt out of nowhere.
        if crate::pgp::detect(&raw).is_some() {
            emitted.insert((path, uid));
            return;
        }
        let (body, check, has_attachments) = render_raw(&raw);
        if let Some(c) = cache {
            c.save_body(account_id, &path, uid, &body);
            c.save_sender_check(account_id, &path, uid, &check);
            c.set_has_attachment(account_id, &path, uid, has_attachments);
        }
        emit(WorkerEvent::Body { message_id: uid, path: path.clone(), body });
        emit(WorkerEvent::SenderChecked { message_id: uid, check });
        // The background prefetch reads the whole message anyway, so a wrong
        // paperclip corrects itself before the message is ever opened.
        if has_attachments {
            emit(WorkerEvent::HasAttachments { message_id: uid });
        }
        emitted.insert((path, uid));
    }
}

/// Block for the next request (used as the IDLE fallback).
async fn recv_one(rx: &mut mpsc::UnboundedReceiver<MailRequest>) -> IdleOutcome {
    match rx.recv().await {
        Some(req) => IdleOutcome::Request(req),
        None => IdleOutcome::Closed,
    }
}

/// Enter IMAP IDLE on `path` for up to `timeout_secs` and wait for new mail or
/// an incoming request. Returns `Refreshed` (after re-syncing) only when the
/// server actually reports new data; `Quiet` on timeout (so the caller can do a
/// prefetch pass and re-IDLE); `Request` to be handled normally; `Closed` when
/// the channel ends. Any IDLE error falls back to a plain receive.
#[allow(clippy::too_many_arguments)]
/// End an IDLE and recover the session, giving the DONE handshake
/// [`IDLE_DONE_TIMEOUT`] to complete. `None` on timeout or error — the future
/// (and with it the wedged connection) is simply dropped, and the caller's
/// next request reconnects (#91: an unbounded await here swallowed the very
/// request that interrupted the IDLE).
async fn idle_done(
    handle: async_imap::extensions::idle::Handle<TlsStream<TcpStream>>,
) -> Option<ImapSession> {
    match tokio::time::timeout(IDLE_DONE_TIMEOUT, handle.done()).await {
        Ok(res) => res.ok(),
        Err(_) => {
            tracing::info!("IDLE DONE went unanswered; dropping the connection");
            None
        }
    }
}

async fn idle_wait(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    account_id: u32,
    folder_id: u32,
    path: &str,
    rx: &mut mpsc::UnboundedReceiver<MailRequest>,
    cache: Option<&Cache>,
    use_envelope: &mut bool,
    body_prefetch: &mut std::collections::VecDeque<(String, u32)>,
    att_prefetch: &mut std::collections::VecDeque<(String, u32)>,
    body_emitted: &std::collections::HashSet<(String, u32)>,
    emit: &impl Fn(WorkerEvent),
    timeout_secs: u64,
) -> IdleOutcome {
    let Some(mut sess) = session.take() else {
        return recv_one(rx).await;
    };
    // Stale or wedged connection — drop it so the next request reconnects.
    // The timeout matters as much as the error: a dead-but-open connection
    // (silently dropped by a NAT during a long IDLE) answers nothing at all.
    match tokio::time::timeout(IDLE_START_TIMEOUT, sel(&mut sess, path)).await {
        Ok(Ok(_)) => {}
        _ => return recv_one(rx).await,
    }

    tracing::debug!(target: "vireo::imap", "> IDLE");
    let mut handle = sess.idle();
    match tokio::time::timeout(IDLE_START_TIMEOUT, handle.init()).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => {
            *session = idle_done(handle).await;
            return recv_one(rx).await;
        }
        // No answer: the handle (and its connection) is dropped, not reused.
        Err(_) => return recv_one(rx).await,
    }

    enum Wake {
        Idle(async_imap::error::Result<async_imap::extensions::idle::IdleResponse>),
        Request(Option<MailRequest>),
    }
    let wake = {
        let (idle_fut, stop) = handle.wait_with_timeout(Duration::from_secs(timeout_secs));
        tokio::select! {
            r = idle_fut => Wake::Idle(r),
            req = rx.recv() => { drop(stop); Wake::Request(req) }
        }
    };
    *session = idle_done(handle).await;

    match wake {
        Wake::Request(Some(req)) => IdleOutcome::Request(req),
        Wake::Request(None) => IdleOutcome::Closed,
        // Only re-sync on actual new data; a plain timeout is Quiet.
        Wake::Idle(Ok(async_imap::extensions::idle::IdleResponse::NewData(_))) => {
            if session.is_some() {
                if let Ok(messages) = load_messages_retry(
                    account_id, session, account, folder_id, path, use_envelope, cache,
                )
                .await
                {
                    if let Some(c) = cache {
                        c.upsert_messages(account_id, path, &messages);
                    }
                    // Pre-download the new mail's body (and any attachments) so
                    // opening it is instant.
                    queue_body_prefetch(body_prefetch, path, &messages, body_emitted);
                    queue_attachment_prefetch(att_prefetch, path, &messages, cache, account_id);
                    emit_body_hits(session, account, account_id, folder_id, path, &messages, cache, emit)
                        .await;
                    emit(WorkerEvent::Messages { folder_id, messages });
                    // Refresh the true unread count too. IDLE only re-synced the
                    // message list; without this the sidebar chip never moves when
                    // new mail lands in a background (unfocused) inbox — it would
                    // take an explicit reload / "All Inboxes" refresh to appear.
                    if let Some(sess) = session.as_mut() {
                        let kind = cached_folder_kind(cache, account_id, path);
                        if let Some(unread) = selected_chip_count(sess, kind).await {
                            emit(WorkerEvent::FolderUnread { folder_id, unread });
                        }
                    }
                }
            }
            IdleOutcome::Refreshed
        }
        Wake::Idle(_) => IdleOutcome::Quiet,
    }
}

/// A leased per-folder IDLE watcher: its task and the shared last-touch clock
/// that keeps it alive (see [`watch_folder`]).
struct FolderWatch {
    handle: tokio::task::JoinHandle<()>,
    lease: Rc<Cell<std::time::Instant>>,
}

/// Whether a folder may hold a dynamic watcher slot. The Inbox is out because
/// it has a permanent watcher; Sent and Drafts because changes there are the
/// user's own doing; Trash and Junk because nobody needs to-the-second unread
/// counts on either — the periodic sweep keeps them honest.
fn watchable(kind: FolderKind) -> bool {
    !matches!(
        kind,
        FolderKind::Inbox
            | FolderKind::Sent
            | FolderKind::Drafts
            | FolderKind::Trash
            | FolderKind::Junk
    )
}

/// Give every [`watchable`] folder the sweep just saw change a watcher (or a
/// fresh lease on the one it has), staggering the connects of whatever
/// actually spawns — several logins in the same instant look like an attack to
/// providers that rate-limit authentication (iCloud does).
fn watch_changed_folders(
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
        tracing::info!("sweep: bulk change ({} folders), not adjusting watchers", changed.len());
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
fn watch_active_folder(
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
async fn watch_folder(
    account: AccountConfig,
    path: String,
    start_delay: Duration,
    lease: Option<Rc<Cell<std::time::Instant>>>,
    emit: impl Fn(WorkerEvent),
) {
    let lapsed =
        |lease: &Option<Rc<Cell<std::time::Instant>>>| {
            lease.as_ref().is_some_and(|l| l.get().elapsed() >= WATCH_LEASE)
        };
    let mut backoff = start_delay;
    loop {
        tokio::time::sleep(backoff).await;
        if lapsed(&lease) {
            // The lease ran out while offline or backing off; don't reconnect
            // just to watch a folder nothing has touched for an hour.
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
        let mut last_count = selected_unseen(&mut sess).await;
        match last_count {
            Some(unread) => emit(WorkerEvent::FolderUnreadByPath { path: path.clone(), unread }),
            None => continue, // wedged straight out of the gate; reconnect
        }
        loop {
            if lapsed(&lease) {
                tracing::info!("unwatching {path}: quiet for the whole lease");
                let _ = sess.logout().await;
                return;
            }
            // A short IDLE round: woken instantly where the server announces
            // changes, and capped at [`WATCH_VERIFY`] so the recount below
            // covers the servers that stay silent — never past the lease's
            // own deadline, so a lapsed watcher frees its connection promptly.
            let timeout = lease.as_ref().map_or(WATCH_VERIFY.as_secs(), |l| {
                WATCH_LEASE
                    .saturating_sub(l.get().elapsed())
                    .as_secs()
                    .clamp(1, WATCH_VERIFY.as_secs())
            });
            tracing::debug!(target: "vireo::imap", "> IDLE");
            let mut handle = sess.idle();
            match tokio::time::timeout(IDLE_START_TIMEOUT, handle.init()).await {
                Ok(Ok(())) => {}
                _ => break, // error or wedged; drop the connection and redial
            }
            let woke = {
                let (idle_fut, _stop) = handle.wait_with_timeout(Duration::from_secs(timeout));
                matches!(
                    idle_fut.await,
                    Ok(async_imap::extensions::idle::IdleResponse::NewData(_))
                )
            };
            match idle_done(handle).await {
                Some(s) => sess = s,
                None => break,
            }
            // Recount whether pushed awake or on the verify tick; a changed
            // number (or any pushed wake) is activity that renews the lease,
            // and only a changed number is worth an event.
            match selected_unseen(&mut sess).await {
                Some(unread) => {
                    let moved = last_count != Some(unread);
                    if woke || moved {
                        if let Some(l) = &lease {
                            l.set(std::time::Instant::now());
                        }
                    }
                    if moved {
                        tracing::debug!("watcher for {path}: unseen now {unread}");
                        last_count = Some(unread);
                        emit(WorkerEvent::FolderUnreadByPath { path: path.clone(), unread });
                    }
                }
                None => break, // count unavailable = session gone; reconnect
            }
        }
    }
}

async fn load_raw_retry(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    path: &str,
    uid: u32,
) -> Result<Vec<u8>, async_imap::error::Error> {
    let mut s = session.take().expect("session ensured before call");
    let mut res = load_raw(&mut s, path, uid).await;
    if res.is_err() {
        if let Ok(fresh) = connect(account).await {
            s = fresh;
            res = load_raw(&mut s, path, uid).await;
        }
    }
    if res.is_ok() {
        *session = Some(s);
    }
    res
}

/// Fetch the raw RFC 822 bytes of a message (binary-safe, for attachments).
async fn load_raw(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
) -> Result<Vec<u8>, async_imap::error::Error> {
    sel(session, path).await?;
    let fetches: Vec<Fetch> = fetch_uids(session, uid.to_string(), "(BODY.PEEK[])")
        .await?
        .try_collect()
        .await?;
    Ok(fetches
        .iter()
        .find_map(|f| f.body())
        .map(|b| b.to_vec())
        .unwrap_or_default())
}

/// Decoded size at or above which a part carrying a Content-ID counts as an
/// attachment as well as being rendered in the body.
///
/// A `cid:` part is referenced from the HTML and drawn in place, so listing it
/// would give newsletters a paperclip for their logo, spacer and social icons —
/// the false-attachment noise fixed in 1.4.1. But when someone emails you a
/// photo, Gmail and Apple Mail send exactly the same shape, and that picture has
/// to be saveable. Size is the only honest discriminator: decoration is small,
/// content is not. 64 KiB sits well above logos and icons and well below any
/// photo worth keeping.
const INLINE_ATTACHMENT_MIN: usize = 64 * 1024;

/// The attachments of a raw message. Public so the Outbox can list a queued
/// message's files without a fetch.
pub fn extract_attachments_of(raw: &[u8]) -> Vec<crate::models::Attachment> {
    extract_attachments(raw)
}

/// What a raw message renders to: body HTML, the sender check (carrying the
/// OpenPGP verdict, #133, when the message had any), and whether it has
/// attachments. An encrypted message is decrypted through the user's GnuPG
/// here, which may raise the agent's passphrase prompt — so this runs only
/// for an explicit open, never for the background prefetch.
fn render_raw(raw: &[u8]) -> (String, crate::models::SenderCheck, bool) {
    let mut check = crate::verify::check_sender(raw);
    match crate::pgp::unwrap_message(raw, &crate::pgp::Gpg::system()) {
        Some(u) => {
            let body = match &u.inner {
                Some(inner) => extract_body(inner),
                None => wrap_plain(&pgp_notice(&u.status)),
            };
            let has = u.inner.as_deref().is_some_and(|i| !extract_attachments(i).is_empty());
            check.pgp = Some(u.status);
            (body, check, has)
        }
        None => (extract_body(raw), check, !extract_attachments(raw).is_empty()),
    }
}

/// The reader's stand-in for a message that could not be decrypted.
fn pgp_notice(status: &crate::models::PgpStatus) -> String {
    let mut text = status.summary();
    for n in &status.notes {
        text.push_str("\n");
        text.push_str(n);
    }
    text
}

/// Whether a rendered body may go into the cache: never for anything
/// OpenPGP. An encrypted message's plaintext lives only in the reader, and
/// a failed decryption is retried at the next open. A signed one is fetched
/// and verified afresh each time too, so importing or trusting the sender's
/// key changes the verdict at the next open — a cached verdict would be the
/// keyring as it stood the first time, for ever.
fn body_cacheable(check: &crate::models::SenderCheck) -> bool {
    check.pgp.is_none()
}

/// The attachments of a raw message, through any OpenPGP wrapping, and
/// whether they may be cached (never for OpenPGP mail, like its body).
fn attachments_of(raw: &[u8]) -> (Vec<crate::models::Attachment>, bool) {
    match crate::pgp::detect(raw) {
        None => (extract_attachments(raw), true),
        Some(_) => {
            let items = crate::pgp::unwrap_message(raw, &crate::pgp::Gpg::system())
                .and_then(|u| u.inner)
                .map(|inner| extract_attachments(&inner))
                .unwrap_or_default();
            (items, false)
        }
    }
}

/// Parse attachment parts (name, mime, decoded bytes) out of a raw message.
fn extract_attachments(raw: &[u8]) -> Vec<crate::models::Attachment> {
    use mail_parser::{MessageParser, MimeHeaders};
    let Some(parsed) = MessageParser::default().parse(raw) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, part) in parsed.attachments().enumerate() {
        // Decoration referenced from the body (a `cid:` logo) is rendered in
        // place, not listed — unless it's big enough to be real content.
        //
        // A part the sender marked `Content-Disposition: attachment` is never
        // decoration, however small. Gmail's composer stamps a Content-ID
        // (`<f_…>`) on every file it uploads, so the size test alone silently
        // dropped real attachments under 64 KiB: a message with two small PDFs
        // and one large font file listed only the font. `structure_has_attachment`
        // already tests disposition first, which is why such mail showed a
        // paperclip and then an incomplete list — the two disagreed.
        let declared_attachment = part
            .content_disposition()
            .is_some_and(|d| d.is_attachment());
        if !declared_attachment
            && part.content_id().is_some()
            && part.contents().len() < INLINE_ATTACHMENT_MIN
        {
            continue;
        }
        let name = attachment_name_of(part, raw)
            .unwrap_or_else(|| format!("attachment-{}", i + 1));
        out.push(crate::models::Attachment {
            name,
            data: part.contents().to_vec(),
        });
    }
    out
}

/// An attachment part's filename, repaired if the sender split it across adjacent
/// RFC 2047 encoded-words.
///
/// RFC 2047 §6.2 joins adjacent encoded-words separated only by linear whitespace
/// and discards that whitespace — it is there so a long word can be folded, not as
/// part of the text. `mail_parser` keeps the separator inside parameter values, so
/// a file announced as
///
/// ```text
/// Content-Disposition: attachment; filename="=?utf-8?q?docu?= =?utf-8?q?ment.pdf?="
/// ```
///
/// arrives named `docu ment.pdf`. That is not a cosmetic problem: with no `.pdf`
/// suffix [`guess_mime`] can't type the file, so it gets no PDF thumbnail and
/// opens in nothing. A header folded between the encoded-words leaves a tab
/// instead of a space, with the same result.
///
/// Splice such pairs in the raw header and let `mail_parser` decode the repaired
/// version, so the charset handling stays in one place.
fn attachment_name_of(part: &mail_parser::MessagePart<'_>, raw: &[u8]) -> Option<String> {
    use mail_parser::{HeaderName, MessageParser, MimeHeaders};

    let names = |h: &mail_parser::Header<'_>| {
        matches!(
            h.name,
            HeaderName::ContentDisposition | HeaderName::ContentType
        )
    };
    let plain = || part.attachment_name().map(|s| s.to_string());

    let mut repaired = Vec::new();
    let mut spliced = false;
    for h in part.headers.iter().filter(|h| names(h)) {
        let Some(value) = raw.get(h.offset_start..h.offset_end) else {
            return plain();
        };
        let joined = splice_encoded_words(value);
        spliced |= joined != value;
        repaired.extend_from_slice(h.name().as_bytes());
        repaired.push(b':');
        repaired.extend_from_slice(&joined);
        if !repaired.ends_with(b"\n") {
            repaired.extend_from_slice(b"\r\n");
        }
    }
    // Nothing to repair: the common case, and no second parse.
    if !spliced {
        return plain();
    }
    repaired.extend_from_slice(b"\r\n");
    MessageParser::default()
        .parse(&repaired)
        .and_then(|m| m.attachment_name().map(|s| s.to_string()))
        .or_else(plain)
}

/// Join adjacent RFC 2047 encoded-words, discarding the linear whitespace between
/// them. Bytes are safe to scan directly: `?=`, `=?` and the whitespace are all
/// ASCII, which never occurs inside a multi-byte UTF-8 sequence.
fn splice_encoded_words(value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len());
    let mut i = 0;
    while i < value.len() {
        if value[i..].starts_with(b"?=") {
            let mut j = i + 2;
            while matches!(value.get(j), Some(b' ' | b'\t' | b'\r' | b'\n')) {
                j += 1;
            }
            if j > i + 2 && value[j..].starts_with(b"=?") {
                out.extend_from_slice(b"?=");
                i = j;
                continue;
            }
        }
        out.push(value[i]);
        i += 1;
    }
    out
}

/// Status text for the attachment pre-download queue (empty = idle).
fn prefetch_status(remaining: usize) -> String {
    if remaining == 0 {
        String::new()
    } else {
        ni18n_f("Downloading attachments… {n} remaining", "Downloading attachments… {n} remaining", remaining as u32, &[("n", &remaining.to_string())])
    }
}

/// Guess a MIME type from a filename extension (best-effort).
fn guess_mime(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        "zip" => "application/zip",
        "doc" | "docx" => "application/msword",
        "xls" | "xlsx" => "application/vnd.ms-excel",
        _ => "application/octet-stream",
    }
}

type SmtpError = Box<dyn std::error::Error + Send + Sync>;

// ---------------------------------------------------------------------------
// Outbox
// ---------------------------------------------------------------------------

/// Push the account's queue to the UI (empty list = nothing waiting).
fn emit_outbox(cache: Option<&Cache>, account_id: u32, emit: &impl Fn(WorkerEvent)) {
    let items = cache.map(|c| c.outbox_items(account_id)).unwrap_or_default();
    emit(WorkerEvent::Outbox { items });
}

/// Store a message that could not be sent. Returns whether it was kept — with no
/// cache there is nowhere to put it, and the caller must not claim otherwise.
fn queue_failed_send(
    cache: Option<&Cache>,
    account_id: u32,
    account: &AccountConfig,
    msg: &OutgoingMessage,
    sent_path: Option<&str>,
    error: &str,
) -> bool {
    queue_outbox_message(cache, account_id, account, msg, sent_path, error, None)
}

/// Send Later (#145): park the message in the Outbox until `at`. The bytes are
/// built now, like a failed send's, so the attachments are read while they
/// exist; the app's clock sends it when the time comes, or the next launch
/// does if Vireo was not running then. Replaces the queued row it was edited
/// from, and drops the draft it was opened from (the caller handles the
/// server-side copy).
fn schedule_send(
    cache: Option<&Cache>,
    account_id: u32,
    account: &AccountConfig,
    msg: &OutgoingMessage,
    sent_path: Option<&str>,
    at: i64,
    emit: &impl Fn(WorkerEvent),
) {
    let queued = queue_outbox_message(cache, account_id, account, msg, sent_path, "", Some(at));
    if queued {
        if let (Some(old), Some(c)) = (msg.outbox_origin, cache) {
            c.delete_outbox(old);
        }
        emit(WorkerEvent::Notice(i18n_f(
            "“{subject}” will be sent {when}",
            &[("subject", &msg.subject), ("when", &crate::datefmt::date_time(at))],
        )));
    } else {
        emit(WorkerEvent::Error {
            text: i18n("Could not schedule the message: there is no local store to keep it in."),
            connectivity: false,
        });
    }
    emit_outbox(cache, account_id, emit);
}

fn queue_outbox_message(
    cache: Option<&Cache>,
    account_id: u32,
    account: &AccountConfig,
    msg: &OutgoingMessage,
    sent_path: Option<&str>,
    error: &str,
    send_at: Option<i64>,
) -> bool {
    let Some(cache) = cache else {
        return false;
    };
    // Build once, here: the composed attachments are read from disk now, while
    // their files certainly exist. A retry sends these bytes verbatim.
    let email = match build_email(account, msg) {
        Ok(email) => email,
        Err(e) => {
            tracing::warn!("could not build the failed message for the outbox: {e}");
            return false;
        }
    };
    let envelope = email.envelope().clone();
    let raw = email.formatted();
    let recipients = [&msg.to, &msg.cc]
        .iter()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let preview: String = msg.body.split_whitespace().collect::<Vec<_>>().join(" ");
    let preview: String = preview.chars().take(160).collect();
    cache
        .queue_outbox(
            account_id,
            envelope.from().map(|f| f.to_string()).unwrap_or_default().as_str(),
            &envelope.to().iter().map(|a| a.to_string()).collect::<Vec<_>>(),
            &recipients,
            &msg.subject,
            &preview,
            &raw,
            sent_path,
            error,
            send_at,
        )
        .is_some()
}

/// Try the queue: one message when `id` is given, otherwise all of them, oldest
/// first. A message that goes out is copied to Sent and dropped from the queue;
/// one that fails again keeps its place with the new reason recorded. `loud`
/// reports failures to the UI — background sweeps stay quiet, since the user did
/// not ask for anything and already knows the message is waiting.
/// Where a sent copy belongs *now*, rather than where it was headed when the
/// message was queued (#199).
///
/// A message can sit in the Outbox for days — Send Later puts it there on
/// purpose — and the account's choice can move on while it waits. Reading the
/// stored path back meant a message scheduled last week landed in the folder
/// that was configured last week. The queued path stays as the fallback for
/// when the folder list can't be read, so a send never loses its copy over
/// this.
fn current_sent_copy_path(
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    queued: Option<&str>,
) -> Option<String> {
    let folders = cache.map(|c| c.load_folders(account_id)).unwrap_or_default();
    pick_queued_sent_copy(account, &folders, queued)
}

/// [`current_sent_copy_path`] once the folder list is in hand.
fn pick_queued_sent_copy(
    account: &AccountConfig,
    folders: &[Folder],
    queued: Option<&str>,
) -> Option<String> {
    // Set since the message was queued (or all along): the server keeps its
    // own copy and ours would be a duplicate.
    if account.server_saves_sent {
        return None;
    }
    if folders.is_empty() {
        return queued.map(str::to_string);
    }
    account
        .sent_copy_path
        .as_deref()
        // Renamed or removed since it was chosen: fall through to Sent rather
        // than appending into a name the server would have to recreate.
        .filter(|p| folders.iter().any(|f| f.path == *p))
        .map(str::to_string)
        .or_else(|| {
            folders
                .iter()
                .find(|f| f.kind == FolderKind::Sent)
                .map(|f| f.path.clone())
        })
}

async fn flush_outbox(
    cache: Option<&Cache>,
    account_id: u32,
    account: &AccountConfig,
    id: Option<u32>,
    session: &mut Option<ImapSession>,
    use_envelope: &mut bool,
    emit: &impl Fn(WorkerEvent),
    loud: bool,
) {
    let Some(cache) = cache else { return };
    // A scheduled message waits for its time; asking for it by id (Send Now,
    // or the app's clock) sends it regardless.
    let now = crate::datefmt::now();
    let items: Vec<crate::models::OutboxItem> = cache
        .outbox_items(account_id)
        .into_iter()
        .filter(|item| match id {
            Some(wanted) => wanted == item.id,
            None => item.send_at.is_none_or(|t| t <= now),
        })
        .collect();
    if items.is_empty() {
        return;
    }

    if loud {
        emit(WorkerEvent::Status(i18n("Sending…")));
    }
    let mut sent_any = false;
    let mut sent = 0usize;
    for item in items {
        let envelope = match outbox_envelope(&item) {
            Some(envelope) => envelope,
            None => {
                // Unsendable as stored: keep it, but say so rather than retrying
                // it forever in silence.
                cache.record_outbox_failure(item.id, "the stored recipients are not valid addresses");
                continue;
            }
        };
        match send_raw_smtp(account, &envelope, &item.raw).await {
            Ok(()) => {
                sent_any = true;
                sent += 1;
                cache.delete_outbox(item.id);
                let dest = current_sent_copy_path(
                    account,
                    account_id,
                    Some(cache),
                    item.sent_path.as_deref(),
                );
                if let (Some(path), Some(sess)) = (dest.as_deref(), session.as_mut()) {
                    if let Err(e) = append_to_sent(sess, path, &item.raw).await {
                        tracing::warn!("outbox: sent, but saving the copy failed: {e}");
                    } else {
                        index_sent_copy(
                            account_id, session, account, path, use_envelope, Some(cache), emit,
                        )
                        .await;
                    }
                }
            }
            Err(e) => {
                cache.record_outbox_failure(item.id, &e.to_string());
                if loud {
                    emit(WorkerEvent::Error {
                        text: i18n_f("Still could not send “{subject}”: {e}", &[("subject", &item.subject.to_string()), ("e", &e.to_string())]),
                        connectivity: false,
                    });
                }
                // A failure now will almost certainly repeat for the rest of the
                // queue (the connection is down), so stop rather than hammering.
                break;
            }
        }
    }
    if loud {
        emit(WorkerEvent::Status(String::new()));
    }
    if sent_any {
        // A background flush is silent up to here, but the user was last told the
        // message had *failed* to send. They have to learn that it since went.
        if !loud {
            emit(WorkerEvent::Notice(match sent {
                1 => "A message waiting in the Outbox has been sent".to_string(),
                n => format!("{n} messages waiting in the Outbox have been sent"),
            }));
        }
        emit(WorkerEvent::Sent);
    }
    emit_outbox(Some(cache), account_id, emit);
}

/// Render an address header back into the form the composer's fields use —
/// `Ada Lovelace <ada@example.com>, bob@example.com` — keeping display names so
/// editing a queued message doesn't reduce everyone to a bare address.
fn addr_list(header: Option<&mail_parser::Address>) -> String {
    header
        .map(|a| {
            a.iter()
                .filter_map(|addr| {
                    let email = addr.address()?.trim();
                    if email.is_empty() {
                        return None;
                    }
                    Some(match addr.name().map(str::trim).filter(|n| !n.is_empty()) {
                        Some(name) => format!("{name} <{email}>"),
                        None => email.to_string(),
                    })
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// A queued message taken apart for editing: the fields a composer needs, plus
/// its attachments as bytes.
pub struct EditableMessage {
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    /// The best body to edit: the HTML alternative when there is one, otherwise
    /// the plain text escaped into HTML (the composer edits HTML).
    pub body_html: String,
    pub attachments: Vec<crate::models::Attachment>,
}

/// Take a queued message apart so it can be edited and sent again.
///
/// The stored bytes are the source of truth, but they are not quite the whole
/// message: lettre strips `Bcc` from what goes on the wire, so those recipients
/// survive only in the stored envelope. Anything in the envelope that isn't in
/// To or Cc is therefore a Bcc recipient, and is restored as one.
pub fn editable_from_raw(raw: &[u8], envelope_rcpts: &[String]) -> EditableMessage {
    use mail_parser::MessageParser;

    let parsed = MessageParser::default().parse(raw);
    let to = parsed.as_ref().map(|p| addr_list(p.to())).unwrap_or_default();
    let cc = parsed.as_ref().map(|p| addr_list(p.cc())).unwrap_or_default();
    let subject = parsed
        .as_ref()
        .and_then(|p| p.subject())
        .unwrap_or_default()
        .to_string();

    // Whoever is in the envelope but named in neither header was a Bcc.
    let named: Vec<String> = parse_recipients(&to)
        .into_iter()
        .chain(parse_recipients(&cc))
        .map(|(_, addr)| addr.to_lowercase())
        .collect();
    let bcc = envelope_rcpts
        .iter()
        .filter(|addr| !named.contains(&addr.to_lowercase()))
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");

    let html = parsed
        .as_ref()
        .and_then(|p| p.body_html(0).map(|b| b.to_string()))
        .unwrap_or_default();
    let body_html = if html.trim().is_empty() {
        let text = parsed
            .as_ref()
            .and_then(|p| p.body_text(0).map(|b| b.to_string()))
            .unwrap_or_default();
        // The composer edits HTML, so plain text has to be escaped and its line
        // breaks preserved — otherwise the whole body collapses into one line.
        text.lines()
            .map(|line| format!("<div>{}</div>", escape_html(line)))
            .collect::<Vec<_>>()
            .join("")
    } else {
        html
    };

    EditableMessage {
        to,
        cc,
        bcc,
        subject,
        body_html,
        attachments: extract_attachments(raw),
    }
}

/// Rebuild the SMTP envelope stored alongside a queued message.
fn outbox_envelope(item: &crate::models::OutboxItem) -> Option<lettre::address::Envelope> {
    let from = item.from_addr.parse::<Address>().ok();
    let to: Vec<Address> = item
        .rcpts
        .iter()
        .filter_map(|a| a.parse::<Address>().ok())
        .collect();
    if to.len() != item.rcpts.len() || to.is_empty() {
        return None;
    }
    lettre::address::Envelope::new(from, to).ok()
}

/// Parse a recipient field ("Name <a@b>, c@d") into (name, email) pairs.
pub fn parse_recipients(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match (part.rfind('<'), part.rfind('>')) {
            (Some(lt), Some(gt)) if lt < gt => {
                let email = part[lt + 1..gt].trim().to_string();
                let name = part[..lt].trim().trim_matches('"').trim().to_string();
                out.push((name, email));
            }
            _ => out.push((String::new(), part.to_string())),
        }
    }
    out
}

/// Build a `Name <addr>` mailbox from its parts. Never format the two into one
/// string and parse that back: an RFC 5322 display name has to be quoted unless
/// it is a bare atom, so "Alfonso Lizárraga", "Martin, Jason" or a name that is
/// itself an address ("a@b.com <a@b.com>", which is what an import with no
/// separate display name produces) all fail to parse and the send is rejected.
/// `Mailbox` quotes and encodes the name itself when the header is written.
fn mailbox(name: &str, addr: &str) -> Result<Mailbox, SmtpError> {
    let address: Address = addr
        .parse()
        .map_err(|e| format!("invalid email address {addr:?}: {e}"))?;
    let name = name.trim();
    // A "display name" that just repeats the address is noise in the header.
    if name.is_empty() || name.eq_ignore_ascii_case(addr.trim()) {
        Ok(Mailbox::new(None, address))
    } else {
        Ok(Mailbox::new(Some(name.to_string()), address))
    }
}

/// Build the RFC 822 email (headers + MIME body) from a composed message, for
/// sending: the SMTP envelope is derived from To/Cc/Bcc, so a message with no
/// recipient is refused here as well as in the composer.
fn build_email(account: &AccountConfig, msg: &OutgoingMessage) -> Result<LettreMessage, SmtpError> {
    build_message(account, msg, false)
}

/// Build the same bytes for the Drafts folder. A draft is often saved before
/// any address is typed (the composer saves on the way out whenever the body
/// was edited), and lettre refuses to build a message whose envelope has no
/// destination — "missing destination address, invalid envelope". The
/// envelope is only ever used by SMTP, and a draft is appended as raw bytes,
/// so a recipient-less draft gets an explicit envelope naming the sender,
/// which never reaches the formatted headers.
fn build_draft(account: &AccountConfig, msg: &OutgoingMessage) -> Result<LettreMessage, SmtpError> {
    build_message(account, msg, true)
}

fn build_message(
    account: &AccountConfig,
    msg: &OutgoingMessage,
    draft: bool,
) -> Result<LettreMessage, SmtpError> {
    // A send-as alias replaces the From header (and, if the alias has its own
    // SMTP, the transport — see `send_raw_smtp`); the Sent copy and everything
    // else about the send stays the account's (#34).
    let (from, from_addr) = match msg.from_alias.as_deref() {
        Some(alias) => match parse_recipients(alias).into_iter().next() {
            Some((name, addr)) => (mailbox(&name, &addr)?, addr),
            None => (mailbox(&account.name, &account.email)?, account.email.clone()),
        },
        None => (mailbox(&account.name, &account.email)?, account.email.clone()),
    };
    let sender_address = from.email.clone();
    let mut builder = LettreMessage::builder().from(from);
    let mut recipients = 0;
    for (name, addr) in parse_recipients(&msg.to) {
        builder = builder.to(mailbox(&name, &addr)?);
        recipients += 1;
    }
    for (name, addr) in parse_recipients(&msg.cc) {
        builder = builder.cc(mailbox(&name, &addr)?);
        recipients += 1;
    }
    for (name, addr) in parse_recipients(&msg.bcc) {
        builder = builder.bcc(mailbox(&name, &addr)?);
        recipients += 1;
    }
    if draft && recipients == 0 {
        let envelope = lettre::address::Envelope::new(
            Some(sender_address.clone()),
            vec![sender_address],
        )?;
        builder = builder.envelope(envelope);
    }
    // Our own Message-ID, in the account's own domain. Without one the SMTP
    // server assigns it on the way out — so the copy filed in Sent has no id at
    // all, and nothing that replies to it can ever be linked back. Setting it
    // here means the copy we keep and the copy that arrives share the same id.
    let mut builder = builder
        .subject(msg.subject.clone())
        .message_id(Some(new_message_id(&from_addr)));
    // Threading headers, re-wrapped in the angle brackets the wire format wants
    // (they are stored stripped). References carries the whole chain so a client
    // can place the reply even if it never saw the immediate parent.
    if !msg.in_reply_to.trim().is_empty() {
        builder = builder.header(lettre::message::header::InReplyTo::from(
            bracketed(&msg.in_reply_to),
        ));
    }
    // Reply-To (#58): answers go where the sender asked, not to From.
    for addr in msg.reply_to.split(',') {
        if let Ok(mbox) = addr.trim().parse() {
            builder = builder.reply_to(mbox);
        }
    }
    // Graph accounts thread by a synthetic "graph-conv:<id>" token (there is no
    // cheap way to read the real References header over the API) — that token is
    // internal glue and must never reach a wire header.
    let wire_references = msg
        .references
        .split_whitespace()
        .filter(|t| !t.starts_with("graph-conv:"))
        .collect::<Vec<_>>()
        .join(" ");
    if !wire_references.trim().is_empty() {
        builder = builder.header(lettre::message::header::References::from(
            bracketed(&wire_references),
        ));
    }

    use lettre::message::{header::ContentType, Attachment, MultiPart, SinglePart};
    let has_html = !msg.html.trim().is_empty();
    // The message's readable half: alternative(plain, html) — wrapped in
    // related() carrying an inline part per image the composer embedded as a
    // `data:` URI (#113). Inline parts go out `Content-Disposition: inline`
    // with a Content-ID the HTML references by `cid:`, so they render in
    // place and stay out of the recipient's attachment list — the same
    // distinction the reader's own `extract_attachments` draws.
    let content = has_html.then(|| {
        let (html, images) = extract_data_images(&msg.html);
        let alt = MultiPart::alternative_plain_html(msg.body.clone(), html);
        if images.is_empty() {
            return alt;
        }
        let mut related = MultiPart::related().multipart(alt);
        for img in images {
            let ct = ContentType::parse(&img.mime)
                .unwrap_or_else(|_| ContentType::parse("application/octet-stream").unwrap());
            let part = match img.name {
                Some(name) => Attachment::new_inline_with_name(img.cid, name),
                None => Attachment::new_inline(img.cid),
            };
            related = related.singlepart(part.body(img.data, ct));
        }
        related
    });
    // OpenPGP (#133): the body as one MIME entity, signed and/or encrypted
    // into the PGP/MIME shape the reader recognises.
    if msg.sign || msg.encrypt {
        let entity = if msg.attachments.is_empty() {
            match content {
                Some(content) => PgpEntity::Multi(content),
                None => PgpEntity::Single(SinglePart::plain(msg.body.clone())),
            }
        } else {
            PgpEntity::Multi(attachments_multipart(content, msg)?)
        };
        let wrapped = pgp_wrap(account, msg, &from_addr, entity)?;
        return Ok(builder.multipart(wrapped)?);
    }
    let email = if msg.attachments.is_empty() {
        match content {
            Some(content) => builder.multipart(content)?,
            None => builder.body(msg.body.clone())?,
        }
    } else {
        builder.multipart(attachments_multipart(content, msg)?)?
    };
    Ok(email)
}

/// The text part (plain, or alternative plain+html) followed by the
/// attachments, as one mixed multipart.
fn attachments_multipart(
    content: Option<lettre::message::MultiPart>,
    msg: &OutgoingMessage,
) -> Result<lettre::message::MultiPart, SmtpError> {
    use lettre::message::{header::ContentType, Attachment, MultiPart, SinglePart};
    let mut multipart = match content {
        Some(content) => MultiPart::mixed().multipart(content),
        None => MultiPart::mixed().singlepart(SinglePart::plain(msg.body.clone())),
    };
    for path in &msg.attachments {
        // Name the file in the error: "No such file or directory" on its own
        // gives no clue which attachment went missing, and under Flatpak the
        // portal's /run/user/.../doc/ paths do expire.
        let bytes = std::fs::read(path)
            .map_err(|e| format!("could not read the attachment {path}: {e}"))?;
        let name = std::path::Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "attachment".to_string());
        let ct = ContentType::parse(guess_mime(&name))
            .unwrap_or(ContentType::TEXT_PLAIN);
        multipart = multipart.singlepart(Attachment::new(name).body(bytes, ct));
    }
    Ok(multipart)
}

/// A message body before OpenPGP wrapping: one MIME entity either way.
enum PgpEntity {
    Multi(lettre::message::MultiPart),
    Single(lettre::message::SinglePart),
}

impl PgpEntity {
    /// The entity's exact bytes, headers included: what a signature covers
    /// and what encryption hides. lettre formats a part the same way every
    /// time (its boundary is fixed at creation), so the signed bytes are
    /// the bytes that go on the wire.
    fn formatted(&self) -> Vec<u8> {
        match self {
            PgpEntity::Multi(m) => m.formatted(),
            PgpEntity::Single(s) => s.formatted(),
        }
    }
}

/// The user's key for an outgoing message: the account's chosen key, else
/// the key whose address matches the From, else the account's own address.
fn signing_key(account: &AccountConfig, from_addr: &str) -> Option<crate::pgp::KeyInfo> {
    let gpg = crate::pgp::Gpg::system();
    account
        .pgp_key
        .as_deref()
        .and_then(|f| crate::pgp::secret_key_by_fingerprint(&gpg, f))
        .or_else(|| crate::pgp::secret_key_for(&gpg, from_addr))
        .or_else(|| crate::pgp::secret_key_for(&gpg, &account.email))
}

/// Sign and/or encrypt an outgoing entity into PGP/MIME (RFC 3156).
fn pgp_wrap(
    account: &AccountConfig,
    msg: &OutgoingMessage,
    from_addr: &str,
    entity: PgpEntity,
) -> Result<lettre::message::MultiPart, SmtpError> {
    use lettre::message::{header::ContentType, MultiPart, SinglePart};
    let gpg = crate::pgp::Gpg::system();
    let key = signing_key(account, from_addr);
    let inner = entity.formatted();
    let no_key = || {
        i18n_f(
            "no OpenPGP key of your own for {addr}; generate one under Settings, OpenPGP",
            &[("addr", from_addr)],
        )
    };
    if msg.encrypt {
        let mut recipients: Vec<String> = [&msg.to, &msg.cc, &msg.bcc]
            .iter()
            .flat_map(|field| parse_recipients(field).into_iter().map(|(_, addr)| addr.to_ascii_lowercase()))
            .collect();
        recipients.sort();
        recipients.dedup();
        for r in &recipients {
            if crate::pgp::public_key_for(&gpg, r).is_none() {
                return Err(i18n_f("no OpenPGP key for {addr}", &[("addr", r)]).into());
            }
        }
        // The sender's own copy: to the account's key when it has one (by
        // fingerprint, so a chosen key with another address still reads its
        // Sent mail), else to whatever key the From address has.
        match &key {
            Some(k) => recipients.push(k.fingerprint.clone()),
            None => {
                if crate::pgp::public_key_for(&gpg, from_addr).is_none() {
                    return Err(no_key().into());
                }
                recipients.push(from_addr.to_ascii_lowercase());
            }
        }
        let signer = if msg.sign { Some(key.ok_or_else(no_key)?.fingerprint) } else { None };
        let ciphertext = crate::pgp::encrypt(&gpg, &inner, &recipients, signer.as_deref())?;
        let control = SinglePart::builder()
            .header(ContentType::parse("application/pgp-encrypted").expect("static content type"))
            .body(String::from("Version: 1\r\n"));
        let payload = SinglePart::builder()
            .header(ContentType::parse("application/octet-stream; name=\"encrypted.asc\"").expect("static content type"))
            .body(ciphertext);
        return Ok(MultiPart::encrypted("application/pgp-encrypted".into())
            .singlepart(control)
            .singlepart(payload));
    }
    let key = key.ok_or_else(no_key)?;
    // lettre writes a part as its formatted bytes, CRLF-terminated, and the
    // boundary delimiter straight after; RFC 3156 counts that CRLF as part
    // of the boundary, so the signature covers everything before it.
    let signed_bytes = inner.strip_suffix(b"\r\n").unwrap_or(&inner);
    let (signature, micalg) = crate::pgp::sign_detached(&gpg, signed_bytes, &key.fingerprint)?;
    let signed = MultiPart::signed("application/pgp-signature".into(), micalg);
    let signed = match entity {
        PgpEntity::Multi(m) => signed.multipart(m),
        PgpEntity::Single(s) => signed.singlepart(s),
    };
    let sig_part = SinglePart::builder()
        .header(ContentType::parse("application/pgp-signature; name=\"signature.asc\"").expect("static content type"))
        .body(signature);
    Ok(signed.singlepart(sig_part))
}

/// One image the composer embedded, lifted out of the outgoing HTML: its
/// generated Content-ID, MIME type, and decoded bytes.
struct InlineImage {
    cid: String,
    mime: String,
    data: Vec<u8>,
    /// The file's own name, when the composer knew one — carried on the
    /// `<img>` as `alt`. Pasted image *bytes* never have one.
    name: Option<String>,
}

/// Lift every quoted `data:image/...;base64,...` URI out of outgoing HTML
/// into an [`InlineImage`], replacing the URI with a `cid:` reference (#113).
/// Anything that is not a well-formed base64 image data URI in an attribute
/// value is left exactly as written.
fn extract_data_images(html: &str) -> (String, Vec<InlineImage>) {
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut parts: Vec<InlineImage> = Vec::new();
    let mut i = 0usize;
    // Content-IDs are per-message; the timestamp keeps them globally unique
    // the way Message-IDs are, since some clients index by Content-ID alone.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    while let Some(off) = html[i..].find("data:image/") {
        let start = i + off;
        // Only splice an attribute value: the URI must sit right after a
        // quote and run to the matching one.
        let quote = match start.checked_sub(1).map(|p| bytes[p]) {
            Some(q @ (b'"' | b'\'')) => Some(q as char),
            _ => None,
        };
        let len = quote.and_then(|q| html[start..].find(q));
        let lifted = len.and_then(|len| {
            let uri = &html[start..start + len];
            let (meta, payload) = uri.split_once(',')?;
            let mime = meta.strip_prefix("data:")?.strip_suffix(";base64")?;
            let data = crate::oauth::base64_decode(payload)?;
            Some((len, mime.to_string(), data))
        });
        match lifted {
            Some((len, mime, data)) => {
                let cid = format!("{}.{}@vireo.inline", stamp, parts.len() + 1);
                let name = alt_of_enclosing_tag(html, start, start + len);
                out.push_str(&html[i..start]);
                out.push_str("cid:");
                out.push_str(&cid);
                parts.push(InlineImage { cid, mime, data, name });
                i = start + len;
            }
            None => {
                out.push_str(&html[i..start + "data:image/".len()]);
                i = start + "data:image/".len();
            }
        }
    }
    out.push_str(&html[i..]);
    (out, parts)
}

/// The `alt` of the tag a lifted `data:` URI sat in — the filename the
/// composer put there for a picture that came from a file (see the editor's
/// insert script). The tag is delimited by the `<` before the URI and the `>`
/// after it; anything unterminated, or a value that is not a plain filename,
/// yields `None` and the part simply goes out unnamed as before.
fn alt_of_enclosing_tag(html: &str, uri_start: usize, uri_end: usize) -> Option<String> {
    let open = html[..uri_start].rfind('<')?;
    let close = uri_end + html[uri_end..].find('>')?;
    let tag = &html[open..close];
    let at = tag.find("alt=")?;
    let rest = &tag[at + "alt=".len()..];
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let value = &rest[quote.len_utf8()..];
    let value = &value[..value.find(quote)?];
    let unescaped = value
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    // Only ever a bare filename: `alt` is attacker-reachable through a quoted
    // reply, and it ends up in a Content-Disposition header.
    let name = std::path::Path::new(&unescaped).file_name()?.to_str()?.to_string();
    let clean = !name.is_empty()
        && name.len() <= 200
        && !name.chars().any(|c| c.is_control() || c == '"' || c == '\\');
    clean.then_some(name)
}

/// A Message-ID for outgoing mail: unique, and in the sender's own domain so it
/// looks like what it is rather than like this machine.
fn new_message_id(from: &str) -> String {
    let domain = from.rsplit('@').next().filter(|d| !d.is_empty()).unwrap_or("localhost");
    let unique = crate::rng::nonce(18).unwrap_or_else(|_| {
        // Entropy is only unavailable in extremis; the clock still separates
        // one message from the next.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos().to_string())
            .unwrap_or_default()
    });
    format!("<vireo-{unique}@{domain}>")
}

/// Wrap each stored (bare) message id back in angle brackets for a header value.
fn bracketed(ids: &str) -> String {
    ids.split_whitespace()
        .map(|id| {
            if id.starts_with('<') {
                id.to_string()
            } else {
                format!("<{id}>")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// TLS settings for an SMTP connection, matching [`tls_connector`]: a local
/// bridge's self-signed certificate is accepted, every other host is verified.
fn smtp_tls_parameters(
    host: &str,
) -> Result<lettre::transport::smtp::client::TlsParameters, lettre::transport::smtp::Error> {
    use lettre::transport::smtp::client::TlsParameters;
    if is_loopback_host(host) {
        TlsParameters::builder(host.to_string())
            .dangerous_accept_invalid_certs(true)
            .dangerous_accept_invalid_hostnames(true)
            .build()
    } else {
        TlsParameters::new(host.to_string())
    }
}

/// The account's send-as alias matching `addr` (case-insensitive), when that
/// alias carries its own SMTP transport (#34). Plain aliases — and the
/// account's own address — resolve to `None`: the account's transport.
fn alias_with_own_smtp<'a>(
    account: &'a AccountConfig,
    addr: &str,
) -> Option<&'a crate::config::AliasConfig> {
    account
        .aliases
        .iter()
        .find(|a| a.has_own_smtp() && a.address().eq_ignore_ascii_case(addr.trim()))
}

/// A TLS-configured transport builder for one SMTP endpoint. Port 465 is
/// implicit TLS; everything else (587, etc.) uses STARTTLS. A loopback bridge
/// signs its own certificate (see `is_loopback_host`), so the relay builders'
/// verification would reject it — TLS stays required, only the certificate
/// checks are relaxed.
fn smtp_transport_builder(
    host: &str,
    port: u16,
) -> Result<lettre::transport::smtp::AsyncSmtpTransportBuilder, SmtpError> {
    let implicit_tls = port == 465;
    let builder = if is_loopback_host(host) {
        let tls = smtp_tls_parameters(host)?;
        let mode = if implicit_tls {
            lettre::transport::smtp::client::Tls::Wrapper(tls)
        } else {
            lettre::transport::smtp::client::Tls::Required(tls)
        };
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host).tls(mode)
    } else if implicit_tls {
        AsyncSmtpTransport::<Tokio1Executor>::relay(host)?
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)?
    };
    Ok(builder.port(port))
}

/// An SMTP transport, configured but not yet connected: the account's own, or —
/// when `alias` is given — the alias's separate server and credentials (#34).
async fn smtp_transport(
    account: &AccountConfig,
    alias: Option<&crate::config::AliasConfig>,
) -> Result<AsyncSmtpTransport<Tokio1Executor>, SmtpError> {
    if let Some(alias) = alias {
        // An alias's own transport is always password-authenticated, never the
        // account's OAuth: it is a different provider's server, which knows
        // nothing of the account's tokens. The password lives in the keyring
        // and is fetched here (the in-memory config never carries it).
        let password = if alias.smtp_password.is_empty() {
            crate::config::load_alias_smtp_password(&account.email, &alias.address())
                .ok_or_else(|| -> SmtpError {
                    format!(
                        "no SMTP password stored for the alias {} — re-enter it in \
                         Accounts",
                        alias.address()
                    )
                    .into()
                })?
        } else {
            alias.smtp_password.clone()
        };
        return Ok(smtp_transport_builder(alias.smtp_host.trim(), alias.smtp_port)?
            .credentials(Credentials::new(alias.smtp_username.clone(), password))
            .build());
    }
    let host = smtp_host(account);
    let mut builder = smtp_transport_builder(&host, account.smtp_port)?;
    if account.oauth {
        // XOAUTH2: the "password" is a fresh OAuth token from GOA.
        let token = fetch_oauth_token(account).await.ok_or_else(|| -> SmtpError {
            "could not get an OAuth token from GNOME Online Accounts".into()
        })?;
        let user = oauth_user(account);
        builder = builder
            .credentials(Credentials::new(user, token))
            .authentication(vec![lettre::transport::smtp::authentication::Mechanism::Xoauth2]);
    } else {
        // Use the separate SMTP credentials when configured, else the IMAP ones.
        let creds = if account.smtp_separate {
            Credentials::new(account.smtp_username.clone(), account.smtp_password.clone())
        } else {
            Credentials::new(account.username.clone(), account.password.clone())
        };
        builder = builder.credentials(creds);
    }
    Ok(builder.build())
}

/// Send an already-built message: the bytes that go on the wire, plus the
/// envelope they are addressed with. Retrying an Outbox message goes through
/// here, so what is retried is byte-for-byte what was composed.
async fn send_raw_smtp(
    account: &AccountConfig,
    envelope: &lettre::address::Envelope,
    raw: &[u8],
) -> Result<(), SmtpError> {
    // The envelope sender names the identity this leaves as. When it is an
    // alias with its own SMTP, the mail goes out through that transport (#34);
    // Outbox retries come through here too, so a queued alias send retries on
    // the alias's server.
    let alias = envelope
        .from()
        .and_then(|f| alias_with_own_smtp(account, f.as_ref()));
    let mailer = smtp_transport(account, alias).await?;
    // The SMTP leg for the console: size and recipient count, then the verdict.
    tracing::debug!(
        target: "vireo::smtp",
        "> send {} bytes to {} recipient(s) via {}",
        raw.len(),
        envelope.to().len(),
        alias.map(|a| a.smtp_host.as_str()).unwrap_or(&smtp_host(account)),
    );
    let r = mailer.send_raw(envelope, raw).await;
    match &r {
        Ok(_) => tracing::debug!(target: "vireo::smtp", "< OK (send)"),
        Err(e) => tracing::warn!(target: "vireo::smtp", "< {e} (send)"),
    }
    r?;
    Ok(())
}

async fn send_smtp(account: &AccountConfig, msg: &OutgoingMessage) -> Result<Vec<u8>, SmtpError> {
    let email = build_email(account, msg)?;
    let raw = email.formatted();
    send_raw_smtp(account, email.envelope(), &raw).await?;
    Ok(raw)
}

/// The SASL identity for XOAUTH2 (the IMAP/SMTP username, else the email).
fn oauth_user(account: &AccountConfig) -> String {
    if account.username.trim().is_empty() {
        account.email.clone()
    } else {
        account.username.clone()
    }
}

/// Fetch a fresh OAuth access token for this account — from GNOME Online Accounts
/// (imported) or by refreshing a natively-added account's stored refresh token.
async fn fetch_oauth_token(account: &AccountConfig) -> Option<String> {
    if let Some(goa_id) = account.goa_id.clone() {
        return tokio::task::spawn_blocking(move || crate::goa::oauth_token(&goa_id))
            .await
            .ok()
            .flatten();
    }
    // Natively-added OAuth account: refresh with the keyring-stored refresh token.
    // The client credentials saved with the account MUST be used — a refresh token
    // is bound to the OAuth client that issued it, so switching clients requires
    // re-adding the account (a fresh sign-in), not swapping creds here.
    let settings = account.oauth_settings.clone()?;
    let refresh = crate::config::load_oauth_refresh(&account.email)?;
    tokio::task::spawn_blocking(move || crate::oauth::refresh_access_token(&settings, &refresh).ok())
        .await
        .ok()
        .flatten()
}

/// XOAUTH2 SASL authenticator for async-imap.
struct XOAuth2 {
    user: String,
    token: String,
    step: u8,
}

impl async_imap::Authenticator for XOAuth2 {
    type Response = Vec<u8>;
    fn process(&mut self, challenge: &[u8]) -> Self::Response {
        self.step += 1;
        if self.step == 1 {
            // Initial SASL response.
            format!("user={}\x01auth=Bearer {}\x01\x01", self.user, self.token).into_bytes()
        } else {
            // The server rejected auth and sent an error challenge; XOAUTH2 requires
            // an empty response so the server then sends the tagged error (rather
            // than the exchange deadlocking).
            let _ = challenge;
            Vec::new()
        }
    }
}

async fn append_to_sent(
    session: &mut ImapSession,
    path: &str,
    raw: &[u8],
) -> Result<(), async_imap::error::Error> {
    // Mark the saved copy as already read.
    append_msg(session, path, Some("(\\Seen)"), raw).await
}

/// The copy of a message just sent was APPENDed to `path`: list that folder
/// again so the copy is in the cache now. A conversation is assembled from
/// the cache (`related_from_cache`), so until the copy is indexed the reply
/// cannot join the thread it answers, and the next visit to the Sent folder
/// was the only thing that indexed it (#199). The folder's listing goes out
/// too, so a list showing that folder (copies filed into the Inbox) is
/// current as well.
async fn index_sent_copy(
    account_id: u32,
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    path: &str,
    use_envelope: &mut bool,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    let Some(c) = cache else { return };
    let Some(folder_id) = c.load_folders(account_id).iter().find(|f| f.path == path).map(|f| f.id)
    else {
        return;
    };
    match load_messages_retry(account_id, session, account, folder_id, path, use_envelope, cache).await
    {
        Ok(messages) => {
            c.upsert_messages(account_id, path, &messages);
            emit(WorkerEvent::Messages { folder_id, messages });
        }
        Err(e) => tracing::warn!("could not list {path} after saving the sent copy: {e}"),
    }
}

/// APPEND a draft to the Drafts folder (flagged `\Draft \Seen`), creating the
/// mailbox first if the server doesn't have one yet.
async fn append_draft(
    session: &mut ImapSession,
    path: &str,
    raw: &[u8],
) -> Result<(), async_imap::error::Error> {
    if append_msg(session, path, Some("(\\Draft \\Seen)"), raw)
        .await
        .is_ok()
    {
        return Ok(());
    }
    // Folder likely doesn't exist — create it and retry.
    let _ = create_box(session, path).await;
    let _ = subscribe_box(session, path).await;
    append_msg(session, path, Some("(\\Draft \\Seen)"), raw).await
}

/// Delete a superseded draft (the previous version being replaced or sent):
/// flag it `\Deleted` and expunge it from the Drafts folder.
async fn delete_draft(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
) -> Result<(), async_imap::error::Error> {
    sel(session, path).await?;
    let _: Result<Vec<Fetch>, _> = store_uids(session, uid.to_string(), "+FLAGS (\\Deleted)")
        .await?
        .try_collect()
        .await;
    let _: Vec<u32> = expunge_all(session).await?.try_collect().await?;
    Ok(())
}

/// Permanently erase messages from a mailbox: flag them `\Deleted`, then expunge.
///
/// `UID EXPUNGE` (RFC 4315) removes only the UIDs we asked for, leaving alone
/// anything another client flagged `\Deleted` in the meantime. Servers without
/// UIDPLUS reject it, so fall back to a plain `EXPUNGE`.
async fn purge_messages(
    session: &mut ImapSession,
    path: &str,
    uids: &[u32],
) -> Result<(), async_imap::error::Error> {
    if uids.is_empty() {
        return Ok(());
    }
    let set = uid_set(uids);
    sel(session, path).await?;
    flag_deleted_and_expunge(session, &set).await
}

/// Flag a UID set `\Deleted` and expunge it: `UID EXPUNGE` where the server
/// has UIDPLUS, a plain `EXPUNGE` otherwise.
async fn flag_deleted_and_expunge(
    session: &mut ImapSession,
    set: &str,
) -> Result<(), async_imap::error::Error> {
    let _: Vec<Fetch> = store_uids(session, set, "+FLAGS (\\Deleted)")
        .await?
        .try_collect()
        .await?;
    // A server without UIDPLUS answers `BAD`, which surfaces while draining the
    // response stream rather than from the call itself — so judge it on the
    // collected result, not with `?`.
    let uid_expunged = match expunge_uids(session, set).await {
        Ok(stream) => stream.try_collect::<Vec<u32>>().await.is_ok(),
        Err(_) => false,
    };
    if !uid_expunged {
        let _: Vec<u32> = expunge_all(session).await?.try_collect().await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The IMAP conversation, for the console (#132 and every "it failed on my
// server" report): each command Vireo sends is logged as `> …` and the
// server's verdict as `< OK` or `< <error>`, under the `vireo::imap`
// target — commands and results only, never a message body or a password.
// The console keeps it whether or not console mode is on, so "Export log"
// carries it after the fact; stderr sees only the failures.
fn wire(cmd: &str) {
    tracing::debug!(target: "vireo::imap", "> {cmd}");
}

fn wired<T>(cmd: &str, r: &Result<T, async_imap::error::Error>) {
    match r {
        Ok(_) => tracing::debug!(target: "vireo::imap", "< OK ({})", cmd_head(cmd)),
        Err(e) => tracing::warn!(target: "vireo::imap", "< {e} ({cmd})"),
    }
}

/// The command's verb and first argument, for the terse OK line.
fn cmd_head(cmd: &str) -> String {
    cmd.split_whitespace().take(3).collect::<Vec<_>>().join(" ")
}

/// Keywords that are a server's or another client's bookkeeping, not tags a
/// person set: left out of the tag finder's report. System flags (`\Seen`…)
/// never reach here as keywords, but a leading backslash is rejected too.
pub fn is_system_keyword(keyword: &str) -> bool {
    let k = keyword.to_ascii_lowercase();
    if k.starts_with('\\') {
        return true;
    }
    const SYSTEM: &[&str] = &[
        "$junk", "$notjunk", "junk", "nonjunk", "notjunk", "$autojunk",
        "$forwarded", "$mdnsent", "$phishing", "$submitted", "$submitpending",
        "$hasattachment", "$hasnoattachment", "$signed", "$encrypted", "$recent",
        "$isnotification", "$ismailinglist", "$istrusted", "$notification",
    ];
    SYSTEM.contains(&k.as_str())
        // Apple Mail's flag colours and Fastmail's internal annotations.
        || k.starts_with("$mailflagbit")
        || k.starts_with("$x-me-")
}

/// A Microsoft 365 category colour (`preset0`…`preset24`) as the nearest
/// `#rrggbb`; `None` for "none" or anything unknown.
fn graph_preset_color(preset: &str) -> Option<String> {
    let hex = match preset.to_ascii_lowercase().as_str() {
        "preset0" => "#c01c28",  // red
        "preset1" => "#e66100",  // orange
        "preset2" => "#865e3c",  // brown
        "preset3" => "#f5c211",  // yellow
        "preset4" => "#2ec27e",  // green
        "preset5" => "#0e9aa7",  // teal
        "preset6" => "#6b8e23",  // olive
        "preset7" => "#1c71d8",  // blue
        "preset8" => "#813d9c",  // purple
        "preset9" => "#b5127a",  // cranberry
        "preset10" => "#5d7c99", // steel
        "preset11" => "#3f5567", // dark steel
        "preset12" => "#77767b", // gray
        "preset13" => "#5e5c64", // dark gray
        "preset14" => "#241f31", // black
        "preset15" => "#a51d2d", // dark red
        "preset16" => "#c64600", // dark orange
        "preset17" => "#63452c", // dark brown
        "preset18" => "#e5a50a", // dark yellow
        "preset19" => "#26a269", // dark green
        "preset20" => "#0b7c85", // dark teal
        "preset21" => "#55701c", // dark olive
        "preset22" => "#1a5fb4", // dark blue
        "preset23" => "#613583", // dark purple
        "preset24" => "#8f0e5d", // dark cranberry
        _ => return None,
    };
    Some(hex.to_string())
}

async fn sel(session: &mut ImapSession, path: &str) -> Result<async_imap::types::Mailbox, async_imap::error::Error> {
    let cmd = format!("SELECT {}", quote_mailbox(path));
    wire(&cmd);
    let r = session.select(path).await;
    wired(&cmd, &r);
    r
}

async fn exam(session: &mut ImapSession, path: &str) -> Result<async_imap::types::Mailbox, async_imap::error::Error> {
    let cmd = format!("EXAMINE {}", quote_mailbox(path));
    wire(&cmd);
    let r = session.examine(path).await;
    wired(&cmd, &r);
    r
}

async fn fetch_uids<'a, S1: AsRef<str>, S2: AsRef<str>>(
    session: &'a mut ImapSession,
    set: S1,
    items: S2,
) -> Result<impl futures::Stream<Item = Result<Fetch, async_imap::error::Error>> + 'a + Send + Unpin, async_imap::error::Error> {
    let (set, items) = (set.as_ref().to_string(), items.as_ref().to_string());
    let cmd = format!("UID FETCH {set} {items}");
    wire(&cmd);
    let r = session.uid_fetch(set, items).await;
    wired(&cmd, &r);
    r
}

async fn store_uids<'a, S1: AsRef<str>, S2: AsRef<str>>(
    session: &'a mut ImapSession,
    set: S1,
    query: S2,
) -> Result<impl futures::Stream<Item = Result<Fetch, async_imap::error::Error>> + 'a + Send + Unpin, async_imap::error::Error> {
    let (set, query) = (set.as_ref().to_string(), query.as_ref().to_string());
    let cmd = format!("UID STORE {set} {query}");
    wire(&cmd);
    let r = session.uid_store(set, query).await;
    wired(&cmd, &r);
    r
}

async fn copy_uids(session: &mut ImapSession, set: &str, dest: &str) -> Result<(), async_imap::error::Error> {
    let cmd = format!("UID COPY {set} {}", quote_mailbox(dest));
    wire(&cmd);
    let r = session.uid_copy(set, quote_mailbox(dest)).await;
    wired(&cmd, &r);
    r
}

async fn mv_uids(session: &mut ImapSession, set: &str, dest: &str) -> Result<(), async_imap::error::Error> {
    let cmd = format!("UID MOVE {set} {}", quote_mailbox(dest));
    wire(&cmd);
    let r = session.uid_mv(set, dest).await;
    wired(&cmd, &r);
    r
}

async fn expunge_uids<'a, S: AsRef<str>>(
    session: &'a mut ImapSession,
    set: S,
) -> Result<impl futures::Stream<Item = Result<u32, async_imap::error::Error>> + 'a + Send, async_imap::error::Error> {
    let set = set.as_ref().to_string();
    let cmd = format!("UID EXPUNGE {set}");
    wire(&cmd);
    let r = session.uid_expunge(set).await;
    wired(&cmd, &r);
    r
}

async fn expunge_all<'a>(
    session: &'a mut ImapSession,
) -> Result<impl futures::Stream<Item = Result<u32, async_imap::error::Error>> + 'a + Send, async_imap::error::Error> {
    let cmd = "EXPUNGE";
    wire(cmd);
    let r = session.expunge().await;
    wired(cmd, &r);
    r
}

async fn search_uids<S: AsRef<str>>(session: &mut ImapSession, query: S) -> Result<std::collections::HashSet<u32>, async_imap::error::Error> {
    let cmd = format!("UID SEARCH {}", query.as_ref());
    wire(&cmd);
    let r = session.uid_search(query.as_ref()).await;
    wired(&cmd, &r);
    r
}

async fn create_box(session: &mut ImapSession, path: &str) -> Result<(), async_imap::error::Error> {
    let cmd = format!("CREATE {}", quote_mailbox(path));
    wire(&cmd);
    let r = session.create(path).await;
    wired(&cmd, &r);
    r
}

async fn subscribe_box(session: &mut ImapSession, path: &str) -> Result<(), async_imap::error::Error> {
    let cmd = format!("SUBSCRIBE {}", quote_mailbox(path));
    wire(&cmd);
    let r = session.subscribe(path).await;
    wired(&cmd, &r);
    r
}

async fn unsubscribe_box(session: &mut ImapSession, path: &str) -> Result<(), async_imap::error::Error> {
    let cmd = format!("UNSUBSCRIBE {}", quote_mailbox(path));
    wire(&cmd);
    let r = session.unsubscribe(path).await;
    wired(&cmd, &r);
    r
}

async fn rename_box(session: &mut ImapSession, from: &str, to: &str) -> Result<(), async_imap::error::Error> {
    let cmd = format!("RENAME {} {}", quote_mailbox(from), quote_mailbox(to));
    wire(&cmd);
    let r = session.rename(from, to).await;
    wired(&cmd, &r);
    r
}

async fn delete_box(session: &mut ImapSession, path: &str) -> Result<(), async_imap::error::Error> {
    let cmd = format!("DELETE {}", quote_mailbox(path));
    wire(&cmd);
    let r = session.delete(path).await;
    wired(&cmd, &r);
    r
}

async fn box_status(session: &mut ImapSession, path: &str, items: &str) -> Result<async_imap::types::Mailbox, async_imap::error::Error> {
    let cmd = format!("STATUS {} {items}", quote_mailbox(path));
    wire(&cmd);
    let r = session.status(path, items).await;
    wired(&cmd, &r);
    r
}

async fn append_msg(
    session: &mut ImapSession,
    path: &str,
    flags: Option<&str>,
    raw: &[u8],
) -> Result<(), async_imap::error::Error> {
    let cmd = format!("APPEND {} {} ({} bytes)", quote_mailbox(path), flags.unwrap_or(""), raw.len());
    wire(&cmd);
    let r = session.append(path, flags, None, raw).await;
    wired(&cmd, &r);
    r
}

/// `UID MOVE` where the server offers it (RFC 6851), else what every client
/// did before there was a MOVE: `UID COPY`, flag the originals `\Deleted`,
/// expunge (issue #128 — a server without MOVE answers `UID MOVE` with
/// "command not permitted with UID", and every move, deleting included,
/// failed on it). The capability is asked for each time: it is one short
/// round trip, and a session's answer can change after login on some
/// servers, so it is not worth caching wrong.
async fn uid_move(
    session: &mut ImapSession,
    set: &str,
    dest: &str,
) -> Result<(), async_imap::error::Error> {
    let can_move = match session.capabilities().await {
        Ok(caps) => caps.has_str("MOVE"),
        // If even CAPABILITY fails, let MOVE produce the real error.
        Err(_) => true,
    };
    tracing::debug!(target: "vireo::imap", "server {} MOVE", if can_move { "offers" } else { "lacks" });
    if can_move {
        return mv_uids(session, set, dest).await;
    }
    // `uid_copy` sends the mailbox name as given (unlike `uid_mv`, which
    // quotes it), so a name with a space — iCloud's "Deleted Messages", the
    // one server this path actually serves — went out bare and every move
    // and delete there answered "BAD Parse Error" (1.22.0 to 1.23.0).
    // `copy_uids` quotes it.
    copy_uids(session, set, dest).await?;
    flag_deleted_and_expunge(session, set).await
}

/// A mailbox name as an IMAP quoted string: backslashes and quotes escaped,
/// the whole wrapped in quotes (RFC 3501 §4.3).
fn quote_mailbox(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    out.push('"');
    for c in name.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// SMTP host: the configured value, or derived from the IMAP host.
fn smtp_host(account: &AccountConfig) -> String {
    let configured = account.smtp_host.trim();
    if !configured.is_empty() {
        configured.to_string()
    } else if let Some(rest) = account.imap_host.strip_prefix("imap") {
        format!("smtp{rest}")
    } else {
        account.imap_host.clone()
    }
}

async fn store_flag(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
    flag: &str,
    add: bool,
) -> Result<(), async_imap::error::Error> {
    sel(session, path).await?;
    let op = if add { "+FLAGS" } else { "-FLAGS" };
    let query = format!("{op} ({flag})");
    // Drain the resulting FETCH stream so the command completes.
    let _: Vec<Fetch> = store_uids(session, uid.to_string(), query)
        .await?
        .try_collect()
        .await?;
    Ok(())
}

/// Add or remove a user-defined keyword (a tag, #71). Answers whether the
/// server took it: `Ok(false)` means the mailbox's PERMANENTFLAGS lists no
/// `\*` (and not this keyword either), so a STORE would be refused or
/// forgotten at the end of the session — the caller keeps the tag locally.
/// A server that sends no PERMANENTFLAGS at all keeps every flag (RFC 3501).
async fn store_keyword(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
    keyword: &str,
    add: bool,
) -> Result<bool, async_imap::error::Error> {
    let mailbox = sel(session, path).await?;
    let allowed = mailbox.permanent_flags.is_empty()
        || mailbox.permanent_flags.iter().any(|f| match f {
            Flag::MayCreate => true,
            Flag::Custom(k) => k.eq_ignore_ascii_case(keyword),
            _ => false,
        });
    if !allowed {
        return Ok(false);
    }
    let op = if add { "+FLAGS" } else { "-FLAGS" };
    let query = format!("{op} ({keyword})");
    let _: Vec<Fetch> = store_uids(session, uid.to_string(), query)
        .await?
        .try_collect()
        .await?;
    Ok(true)
}

/// Move a message to `dest`, creating (and subscribing to) the destination
/// mailbox first if it doesn't exist yet. Returns whether a folder was created,
/// so the caller can refresh the folder list.
async fn move_or_create(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
    dest: &str,
) -> Result<bool, async_imap::error::Error> {
    sel(session, path).await?;
    if uid_move(session, &uid.to_string(), dest).await.is_ok() {
        return Ok(false);
    }
    // The move failed — most likely the destination mailbox doesn't exist (the
    // account has no Archive/Junk/… folder yet). Create it, subscribe, and retry;
    // if it still fails, surface that error.
    let created = create_box(session, dest).await.is_ok();
    let _ = subscribe_box(session, dest).await;
    sel(session, path).await?;
    uid_move(session, &uid.to_string(), dest).await?;
    Ok(created)
}

async fn move_message(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
    dest: &str,
) -> Result<bool, async_imap::error::Error> {
    move_or_create(session, path, uid, dest).await
}

/// Move many messages from `path` to `dest` with as few IMAP commands as
/// possible: one SELECT, then a UID MOVE per chunk of the UID set (chunked so a
/// huge selection never overflows the server's command-length limit). Creates and
/// subscribes to `dest` on demand. Returns whether the destination was created.
async fn move_messages(
    session: &mut ImapSession,
    path: &str,
    uids: &[u32],
    dest: &str,
) -> Result<bool, async_imap::error::Error> {
    sel(session, path).await?;
    let mut created = false;
    let mut ensured_dest = false;
    for chunk in uids.chunks(300) {
        let set = uid_set(chunk);
        if uid_move(session, &set, dest).await.is_ok() {
            continue;
        }
        // First failure is most likely a missing destination mailbox — create,
        // subscribe, re-select the source, and retry this chunk (then the rest).
        if !ensured_dest {
            created = create_box(session, dest).await.is_ok();
            let _ = subscribe_box(session, dest).await;
            ensured_dest = true;
            sel(session, path).await?;
        }
        uid_move(session, &set, dest).await?;
    }
    Ok(created)
}

/// Create (and subscribe to) a new mailbox.
async fn create_folder(
    session: &mut ImapSession,
    path: &str,
) -> Result<(), async_imap::error::Error> {
    create_box(session, path).await?;
    let _ = subscribe_box(session, path).await;
    Ok(())
}

/// RENAME a mailbox — the server carries any inferior hierarchy along
/// (RFC 3501 §6.3.5) — and follow the subscription to the new name.
async fn rename_folder(
    session: &mut ImapSession,
    old_path: &str,
    new_path: &str,
) -> Result<(), async_imap::error::Error> {
    rename_box(session, old_path, new_path).await?;
    let _ = unsubscribe_box(session, old_path).await;
    let _ = subscribe_box(session, new_path).await;
    Ok(())
}

/// Delete a mailbox after moving all of its messages to `trash` (creating the
/// trash mailbox if needed). With no trash target the contents are discarded.
async fn delete_folder(
    session: &mut ImapSession,
    path: &str,
    trash: Option<&str>,
) -> Result<(), async_imap::error::Error> {
    let mailbox = sel(session, path).await?;
    if mailbox.exists > 0 {
        if let Some(trash) = trash {
            if !trash.eq_ignore_ascii_case(path) {
                // Move everything to Trash; create it first if the move fails.
                if uid_move(session, "1:*", trash).await.is_err() {
                    let _ = create_box(session, trash).await;
                    let _ = subscribe_box(session, trash).await;
                    sel(session, path).await?;
                    uid_move(session, "1:*", trash).await?;
                }
            }
        }
    }
    // A mailbox can't be deleted while selected — close it first.
    let _ = session.close().await;
    delete_box(session, path).await?;
    Ok(())
}

/// Mark every message in a folder as read (`\Seen`) in one STORE.
async fn mark_all_read(
    session: &mut ImapSession,
    path: &str,
) -> Result<(), async_imap::error::Error> {
    let mailbox = sel(session, path).await?;
    if mailbox.exists == 0 {
        return Ok(());
    }
    let _: Vec<Fetch> = store_uids(session, "1:*", "+FLAGS (\\Seen)")
        .await?
        .try_collect()
        .await?;
    Ok(())
}

/// Mark a message as spam following common conventions: set the `$Junk` keyword
/// and clear `$NotJunk` (so server-side filters / other clients can learn), then
/// move it to the Junk folder. Keyword stores are best-effort — some servers
/// reject custom keywords — but the move is authoritative.
async fn mark_spam(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
    dest: &str,
) -> Result<bool, async_imap::error::Error> {
    sel(session, path).await?;
    for query in ["+FLAGS ($Junk)", "-FLAGS ($NotJunk)"] {
        if let Ok(stream) = store_uids(session, uid.to_string(), query).await {
            let _: Result<Vec<Fetch>, _> = stream.try_collect().await;
        }
    }
    move_or_create(session, path, uid, dest).await
}

/// The reverse of [`mark_spam`] (#168): set `$NotJunk` and clear `$Junk` on
/// every uid (best-effort, as above), then move them back to `dest`.
async fn mark_ham(
    session: &mut ImapSession,
    path: &str,
    uids: &[u32],
    dest: &str,
) -> Result<bool, async_imap::error::Error> {
    sel(session, path).await?;
    let set = uid_set(uids);
    for query in ["+FLAGS ($NotJunk)", "-FLAGS ($Junk)"] {
        if let Ok(stream) = store_uids(session, set.clone(), query).await {
            let _: Result<Vec<Fetch>, _> = stream.try_collect().await;
        }
    }
    move_messages(session, path, uids, dest).await
}

/// Whether a host is this machine. Local mail bridges — Proton Bridge, hydroxide,
/// DavMail — terminate TLS with a certificate generated on the machine itself at
/// install time: no CA has signed it and it is issued for an address rather than
/// a name, so it fails both checks. Verification is relaxed for loopback
/// addresses only, where anyone able to intercept the connection is already
/// running code as the user, and never for a host reached over a network.
fn is_loopback_host(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// A TLS connector for a mail server: it tolerates a local bridge’s self-signed
/// certificate and verifies everything else normally.
fn tls_connector(host: &str) -> async_native_tls::TlsConnector {
    let tls = async_native_tls::TlsConnector::new();
    if is_loopback_host(host) {
        tls.danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
    } else {
        tls
    }
}

/// Whether the IMAP connection opens in plaintext and upgrades with STARTTLS
/// instead of negotiating TLS from the first byte. 993 is always implicit TLS
/// and 143 is the conventional STARTTLS port; a local bridge listens on a port
/// of its own (Proton Bridge defaults to 1143) and speaks STARTTLS there.
fn imap_uses_starttls(account: &AccountConfig) -> bool {
    account.imap_port != 993
        && (account.imap_port == 143 || is_loopback_host(&account.imap_host))
}

/// Hard ceiling on one IMAP connection attempt — TCP, TLS, and authentication
/// together. A server (or middlebox) that accepts the socket and then stalls
/// would otherwise hang the worker with no error ever surfacing.
const IMAP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

async fn connect(account: &AccountConfig) -> Result<ImapSession, Box<dyn std::error::Error>> {
    match tokio::time::timeout(IMAP_CONNECT_TIMEOUT, connect_inner(account)).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "connecting to {} timed out after {} seconds",
            account.imap_host,
            IMAP_CONNECT_TIMEOUT.as_secs()
        )
        .into()),
    }
}

async fn connect_inner(account: &AccountConfig) -> Result<ImapSession, Box<dyn std::error::Error>> {
    let tcp = TcpStream::connect((account.imap_host.as_str(), account.imap_port)).await?;
    let tls = tls_connector(&account.imap_host);
    let client = if imap_uses_starttls(account) {
        let mut plain = async_imap::Client::new(tcp);
        // Consume the plaintext greeting before issuing STARTTLS. Nothing secret
        // has been sent at this point; the credentials go out below, after the
        // socket has been upgraded.
        let _ = plain.read_response().await;
        plain.run_command_and_check_ok("STARTTLS", None).await?;
        let stream = tls.connect(account.imap_host.as_str(), plain.into_inner()).await?;
        // A server that accepted STARTTLS does not send a second greeting.
        async_imap::Client::new(stream)
    } else {
        let stream = tls.connect(account.imap_host.as_str(), tcp).await?;
        let mut client = async_imap::Client::new(stream);
        // Consume the server greeting before issuing commands. LOGIN tolerates an
        // unread greeting, but the AUTHENTICATE handshake reads it as the command
        // reply and deadlocks — so read it explicitly here.
        let _ = client.read_response().await;
        client
    };
    // The sign-in for the console: mechanism and user, never the secret.
    let session = if account.oauth {
        // XOAUTH2 with a fresh access token (from GOA or a native refresh token).
        tracing::debug!(target: "vireo::imap", "> AUTHENTICATE XOAUTH2 {}", oauth_user(account));
        let token = match fetch_oauth_token(account).await {
            Some(t) => t,
            None => {
                tracing::warn!(target: "vireo::imap", "< no OAuth token (AUTHENTICATE XOAUTH2)");
                return Err("could not get an OAuth token".into());
            }
        };
        let auth = XOAuth2 { user: oauth_user(account), token, step: 0 };
        let r = client.authenticate("XOAUTH2", auth).await.map_err(|(e, _client)| e);
        match &r {
            Ok(_) => tracing::debug!(target: "vireo::imap", "< OK (AUTHENTICATE XOAUTH2)"),
            Err(e) => tracing::warn!(target: "vireo::imap", "< {e} (AUTHENTICATE XOAUTH2)"),
        }
        r?
    } else {
        tracing::debug!(target: "vireo::imap", "> LOGIN {} ****", account.username);
        let r = client.login(&account.username, &account.password).await.map_err(|(e, _client)| e);
        match &r {
            Ok(_) => tracing::debug!(target: "vireo::imap", "< OK (LOGIN)"),
            Err(e) => tracing::warn!(target: "vireo::imap", "< {e} (LOGIN)"),
        }
        r?
    };
    Ok(session)
}

/// Outcome of a credential/connection test for the Accounts window.
#[derive(Debug)]
pub struct ConnTest {
    /// Incoming server (IMAP or POP3, per the account's protocol).
    pub incoming: Result<(), String>,
    pub smtp: Result<(), String>,
}

/// Test that the account's servers accept the given credentials (no mail sent).
pub async fn test_connection(account: &AccountConfig) -> ConnTest {
    let incoming = if account.protocol == crate::config::Protocol::Pop3 {
        test_pop3(account).await
    } else {
        test_imap(account).await
    };
    ConnTest {
        incoming,
        smtp: test_smtp(account).await,
    }
}

async fn test_pop3(account: &AccountConfig) -> Result<(), String> {
    let mut pop = Pop3::connect(account).await?;
    pop.login(&account.username, &account.password).await?;
    pop.quit().await;
    Ok(())
}

/// Blocking wrapper around [`test_connection`] that spins up its own Tokio
/// runtime — call it from `spawn_blocking` so the IMAP/SMTP sockets have an I/O
/// reactor regardless of the caller's runtime.
pub fn test_connection_blocking(account: AccountConfig) -> ConnTest {
    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt.block_on(test_connection(&account)),
        Err(e) => ConnTest {
            incoming: Err(e.to_string()),
            smtp: Err(e.to_string()),
        },
    }
}

async fn test_imap(account: &AccountConfig) -> Result<(), String> {
    // Stringify the (non-Send) error before any further await so the returned
    // future stays Send (required by relm4's command runner).
    let mut session = connect(account).await.map_err(|e| e.to_string())?;
    let _ = session.logout().await;
    Ok(())
}

/// Connect to the SMTP server and authenticate (then quit) — verifies the send
/// credentials without delivering anything.
async fn test_smtp(account: &AccountConfig) -> Result<(), String> {
    use lettre::transport::smtp::authentication::Mechanism;

    let host = smtp_host(account);
    // Authenticate the same way the send path does: XOAUTH2 with a fresh token
    // for OAuth accounts, otherwise the password (SMTP-specific if configured).
    let (creds, mechanisms): (Credentials, &[Mechanism]) = if account.oauth {
        let token = fetch_oauth_token(account)
            .await
            .ok_or_else(|| "could not get an OAuth token".to_string())?;
        (Credentials::new(oauth_user(account), token), &[Mechanism::Xoauth2])
    } else if account.smtp_separate {
        (
            Credentials::new(account.smtp_username.clone(), account.smtp_password.clone()),
            &[Mechanism::Plain, Mechanism::Login],
        )
    } else {
        (
            Credentials::new(account.username.clone(), account.password.clone()),
            &[Mechanism::Plain, Mechanism::Login],
        )
    };
    smtp_auth_check(&host, account.smtp_port, &creds, mechanisms).await
}

/// Test a send-as alias's own SMTP server and credentials (#34): connect,
/// authenticate, quit. Used by the alias editor's Test button; an alias without
/// its own transport has nothing of its own to test.
pub async fn test_alias_smtp(
    account_email: &str,
    alias: &crate::config::AliasConfig,
) -> Result<(), String> {
    use lettre::transport::smtp::authentication::Mechanism;

    let password = if alias.smtp_password.is_empty() {
        crate::config::load_alias_smtp_password(account_email, &alias.address())
            .unwrap_or_default()
    } else {
        alias.smtp_password.clone()
    };
    let creds = Credentials::new(alias.smtp_username.clone(), password);
    smtp_auth_check(
        alias.smtp_host.trim(),
        alias.smtp_port,
        &creds,
        &[Mechanism::Plain, Mechanism::Login],
    )
    .await
}

/// Blocking wrapper around [`test_alias_smtp`] — call from `spawn_blocking`,
/// like [`test_connection_blocking`].
pub fn test_alias_smtp_blocking(
    account_email: String,
    alias: crate::config::AliasConfig,
) -> Result<(), String> {
    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt.block_on(test_alias_smtp(&account_email, &alias)),
        Err(e) => Err(e.to_string()),
    }
}

/// Shared SMTP credential check: connect (implicit TLS on 465, STARTTLS
/// otherwise), authenticate, quit.
async fn smtp_auth_check(
    host: &str,
    port: u16,
    creds: &Credentials,
    mechanisms: &[lettre::transport::smtp::authentication::Mechanism],
) -> Result<(), String> {
    use lettre::transport::smtp::client::AsyncSmtpConnection;
    use lettre::transport::smtp::extension::ClientId;

    let hello = ClientId::default();
    let tls = smtp_tls_parameters(host).map_err(|e| e.to_string())?;
    // A (host, port) pair resolves bare IPv6 addresses correctly; a "host:port"
    // string would mis-parse their colons.
    let addr = (host, port);
    let timeout = Some(std::time::Duration::from_secs(20));

    // Port 465 is implicit TLS; everything else uses STARTTLS.
    let mut conn = if port == 465 {
        AsyncSmtpConnection::connect_tokio1(addr, timeout, &hello, Some(tls), None)
            .await
            .map_err(|e| e.to_string())?
    } else {
        let mut conn = AsyncSmtpConnection::connect_tokio1(addr, timeout, &hello, None, None)
            .await
            .map_err(|e| e.to_string())?;
        conn.starttls(tls, &hello).await.map_err(|e| e.to_string())?;
        conn
    };
    let result = conn.auth(mechanisms, creds).await.map(|_| ()).map_err(|e| e.to_string());
    let _ = conn.quit().await;
    result
}

async fn list_folders(
    account_id: u32,
    session: &mut ImapSession,
    roles: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<Folder>, async_imap::error::Error> {
    let names: Vec<async_imap::types::Name> = session
        .list(Some(""), Some("*"))
        .await?
        .try_collect()
        .await?;

    let mut folders = Vec::new();
    let mut special_use = Vec::new(); // parallel: kind came from a SPECIAL-USE attr
    for name in names.iter() {
        // Skip containers that cannot hold messages.
        if name.attributes().contains(&NameAttribute::NoSelect) {
            continue;
        }
        let path = name.name().to_string();
        let (kind, by_special_use) = classify_with_source(&path, name.attributes());
        special_use.push(by_special_use);
        folders.push(Folder {
            id: 0, // assigned by order below
            account_id,
            name: display_name(&path, name.delimiter()),
            path,
            kind,
            unread: 0,
        });
    }

    // When a role has a real SPECIAL-USE folder, demote any name-matched impostors
    // of that role to Custom — otherwise a stray folder like a plain "Trash" label
    // can shadow the server's actual [Gmail]/Trash and mail "moved" there never
    // really leaves (it just gains a label).
    for role in [
        FolderKind::Sent,
        FolderKind::Drafts,
        FolderKind::Trash,
        FolderKind::Junk,
        FolderKind::Archive,
        FolderKind::Starred,
    ] {
        let has_special = folders
            .iter()
            .zip(&special_use)
            .any(|(f, su)| f.kind == role && *su);
        if has_special {
            for (f, su) in folders.iter_mut().zip(&special_use) {
                if f.kind == role && !*su {
                    f.kind = FolderKind::Custom;
                }
            }
        }
    }

    // Essential folders first in their fixed order; custom folders sorted by
    // their full hierarchical path (case-insensitively), so sub-folders sit
    // grouped under their parents in a stable, server-independent order rather
    // than in whatever order the server's LIST happened to return them.
    folders.sort_by(|a, b| {
        folder_order(a.kind)
            .cmp(&folder_order(b.kind))
            .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
    });
    for (i, f) in folders.iter_mut().enumerate() {
        f.id = i as u32 + 1;
    }
    // Manual Special Folders assignments (#82) ride over the detection
    // before anything is counted: a folder assigned as Drafts must count
    // every draft, and the cached kind drives every later re-count (the
    // sweep, the on-open refresh) too. Ids and order stay as numbered —
    // the app re-sorts its own copy.
    crate::models::assign_folder_roles(roles, &mut folders);

    // Ask the server for each folder's true unread count. STATUS is cheap and
    // downloads no message content, so this stays fast even for huge mailboxes.
    for f in folders.iter_mut() {
        if let Ok(mb) = box_status(session, &f.path, "(UNSEEN MESSAGES)").await {
            f.unread = if chip_counts_all(f.kind) { mb.exists } else { mb.unseen.unwrap_or(0) };
        }
    }

    // STATUS is unreliable on some servers (notably iCloud), which leaves stale
    // inbox chips in the sidebar until the folder is opened. The inbox is the only
    // folder whose unread count is shown, so refine just that one up front with the
    // accurate EXAMINE + SEARCH UNSEEN (read-only; leaves the mailbox unselected
    // for the main loop to re-select as needed).
    if let Some(inbox) = folders.iter_mut().find(|f| f.kind == FolderKind::Inbox) {
        if exam(session, &inbox.path).await.is_ok() {
            if let Some(n) = selected_unseen(session).await {
                inbox.unread = n;
            }
        }
    }
    Ok(folders)
}

/// Re-list folders (e.g. after auto-creating one) and push them to the UI.
/// Auto-empty (#140): the roles the account wants emptied, with their ages.
fn auto_empty_roles(account: &AccountConfig) -> Vec<(FolderKind, &'static str, u32)> {
    [
        (FolderKind::Junk, "junk", account.empty_junk_days),
        (FolderKind::Trash, "trash", account.empty_trash_days),
    ]
    .into_iter()
    .filter(|(_, _, days)| *days > 0)
    .collect()
}

/// The folder holding a role for this account: the manual assignment (#82)
/// when there is one, else the folder the listing detected.
fn role_folder_path(account: &AccountConfig, folders: &[Folder], kind: FolderKind, role: &str) -> Option<String> {
    if let Some(p) = account.folder_roles.get(role) {
        if !p.is_empty() {
            return Some(p.clone());
        }
    }
    folders.iter().find(|f| f.kind == kind).map(|f| f.path.clone())
}

/// Whether the auto-empty is due: something is configured, and the last run
/// on this connection is older than [`AUTO_EMPTY_MIN`] (or never happened).
fn auto_empty_due(account: &AccountConfig, last: &mut Option<std::time::Instant>) -> bool {
    if auto_empty_roles(account).is_empty() {
        return false;
    }
    if last.is_some_and(|t| t.elapsed() < AUTO_EMPTY_MIN) {
        return false;
    }
    *last = Some(std::time::Instant::now());
    true
}

/// The cut-off, `days` ago, in the form IMAP SEARCH wants ("1-Aug-2026").
/// SEARCH BEFORE compares internal dates by day, in the server's clock.
fn imap_before_date(days: u32) -> String {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(days));
    cutoff.format("%-d-%b-%Y").to_string()
}

/// The same cut-off as an RFC 3339 instant for Graph's `$filter`.
fn graph_before_date(days: u32) -> String {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(days));
    cutoff.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Auto-empty (#140) for an IMAP account: delete for good whatever in the
/// Junk / Trash folder is older than the account's chosen age. Rides the
/// unread sweep, at most every [`AUTO_EMPTY_MIN`]. Age is the message's
/// internal date, the day it reached the server, which is how the other
/// clients count too. A failure is logged and tried again next time; the
/// sweep never surfaces an error for a chore nobody is waiting on.
async fn auto_empty_imap(
    account_id: u32,
    account: &AccountConfig,
    session: &mut ImapSession,
    cache: Option<&Cache>,
    last: &mut Option<std::time::Instant>,
    emit: &impl Fn(WorkerEvent),
) {
    if !auto_empty_due(account, last) {
        return;
    }
    let folders = cache.map(|c| c.load_folders(account_id)).unwrap_or_default();
    let mut purged_any = false;
    for (kind, role, days) in auto_empty_roles(account) {
        let Some(path) = role_folder_path(account, &folders, kind, role) else {
            tracing::info!("auto-empty: account {account_id} has no {role} folder");
            continue;
        };
        let before = imap_before_date(days);
        let found = match sel(session, &path).await {
            Ok(_) => search_uids(session, format!("BEFORE {before}")).await,
            Err(e) => Err(e),
        };
        let uids: Vec<u32> = match found {
            Ok(set) => set.into_iter().collect(),
            Err(e) => {
                tracing::warn!("auto-empty: could not search {path} on account {account_id}: {e}");
                continue;
            }
        };
        if uids.is_empty() {
            continue;
        }
        match purge_messages(session, &path, &uids).await {
            Ok(()) => {
                if let Some(c) = cache {
                    for uid in &uids {
                        c.delete_message(account_id, &path, *uid);
                    }
                }
                tracing::info!(
                    "auto-empty: deleted {} message(s) older than {days} days from {path} on account {account_id}",
                    uids.len()
                );
                purged_any = true;
            }
            Err(e) => {
                tracing::warn!("auto-empty: could not delete from {path} on account {account_id}: {e}");
            }
        }
    }
    if purged_any {
        refresh_folders(account_id, account, session, cache, emit).await;
    }
}

/// Auto-empty (#140) for a Microsoft 365 account, by the well-known folder
/// names and `receivedDateTime`.
async fn auto_empty_graph(
    token: &str,
    account_id: u32,
    account: &AccountConfig,
    cache: Option<&Cache>,
    state: &mut GraphState,
    last: &mut Option<std::time::Instant>,
    emit: &impl Fn(WorkerEvent),
) {
    if !auto_empty_due(account, last) {
        return;
    }
    let folders = cache.map(|c| c.load_folders(account_id)).unwrap_or_default();
    let mut purged_any = false;
    for (kind, role, days) in auto_empty_roles(account) {
        let well_known = match kind {
            FolderKind::Junk => "junkemail",
            _ => "deleteditems",
        };
        let path = role_folder_path(account, &folders, kind, role);
        let url = format!(
            "{GRAPH_BASE}/me/mailFolders/{well_known}/messages\
             ?$filter=receivedDateTime%20le%20{}&$select=id&$top=100",
            graph_before_date(days)
        );
        let t = token.to_string();
        let items = tokio::task::spawn_blocking(move || graph_paged(&t, &url, GRAPH_INDEX_CAP))
            .await
            .unwrap_or_else(|_| Err("task failed".into()));
        let ids: Vec<String> = match items {
            Ok(items) => items.iter().filter_map(|v| v["id"].as_str().map(str::to_string)).collect(),
            Err(e) => {
                tracing::warn!("auto-empty: could not list {well_known} on account {account_id}: {e}");
                continue;
            }
        };
        if ids.is_empty() {
            continue;
        }
        let mut deleted = 0usize;
        for gid in ids {
            let t = token.to_string();
            let url = format!("{GRAPH_BASE}/me/messages/{gid}");
            let ok = tokio::task::spawn_blocking(move || graph_delete_req(&t, &url))
                .await
                .unwrap_or_else(|_| Err("task failed".into()))
                .is_ok();
            if ok {
                deleted += 1;
                let uid = hash_uid(&gid);
                state.uids.remove(&uid);
                if let (Some(c), Some(path)) = (cache, path.as_deref()) {
                    c.delete_message(account_id, path, uid);
                }
            }
        }
        tracing::info!(
            "auto-empty: deleted {deleted} message(s) older than {days} days from {well_known} on account {account_id}"
        );
        purged_any |= deleted > 0;
    }
    if purged_any {
        graph_refresh_unread(token, account_id, cache, state, emit).await;
    }
}

async fn refresh_folders(
    account_id: u32,
    account: &AccountConfig,
    session: &mut ImapSession,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    if let Ok(folders) = list_folders(account_id, session, &account.folder_roles).await {
        // A mailbox always has at least INBOX (RFC 3501): an empty LIST is a
        // wedged or throttled session answering nonsense, not the truth.
        // Trusting one once wiped an account's whole folder list — cached,
        // so it stayed wiped across restarts (seen on iCloud after a burst
        // of RENAMEs).
        if folders.is_empty() {
            return;
        }
        if let Some(c) = cache {
            c.save_folders(account_id, &folders);
        }
        emit(WorkerEvent::Folders(folders));
    }
}

/// Sweep every known folder for its true unread count and emit a
/// [`WorkerEvent::FolderUnread`] for each — the per-folder event is the one
/// path allowed to assert a genuine zero (the merged Folders path deliberately
/// won't, see the app's SetFolders). A folder that can't be read emits
/// nothing, so a wedged answer can't wipe a good chip.
///
/// EXAMINE + UID SEARCH, not STATUS: STATUS answers from a cache that lags
/// minutes behind on some servers (iCloud, see the inbox refinement in
/// [`list_folders`]) — stale numbers left chips unmoved and, worse, blinded
/// the change detection below. The read-only EXAMINE walk costs a second
/// round trip per folder and leaves the session's selection wherever it ends;
/// every other network path re-selects what it needs first.
///
/// `selected` (the mailbox the main loop is working) is skipped: its count is
/// already refreshed by every load and IDLE pass, and examining it out from
/// under the loop would be rude.
///
/// Doubles as the activity detector for the dynamic watchers: each folder's
/// (unseen, UIDNEXT) pair is compared against `baseline` — the same pair from
/// the previous sweep — and the folders that moved are returned so the caller
/// can put them on the watch list. UIDNEXT (from the EXAMINE response)
/// catches deliveries of already-read mail that the unseen count alone would
/// miss. A folder with no baseline yet is recorded, not reported: everything
/// that happened while Vireo was closed would otherwise look like fresh
/// activity.
/// The keyword half of a refresh with a tag view open (#166): every folder
/// is examined and, for each configured tag, the server's KEYWORD search is
/// set against the cached keywords. Tags set or cleared from another client
/// land in the index here — a folder's own sync only sees them once that
/// folder is opened. Returns the folders whose rows changed.
async fn refresh_keywords(
    account_id: u32,
    session: &mut ImapSession,
    cache: Option<&Cache>,
    keywords: &[String],
) -> Vec<String> {
    let mut changed = Vec::new();
    let Some(cache) = cache else { return changed };
    if keywords.is_empty() {
        return changed;
    }
    for f in cache.load_folders(account_id) {
        if exam(session, &f.path).await.is_err() {
            continue;
        }
        let mut touched = false;
        for kw in keywords {
            let Ok(server) = search_uids(session, format!("KEYWORD {kw}")).await else { continue };
            let cached: std::collections::HashSet<u32> =
                cache.uids_with_keyword(account_id, &f.path, kw).into_iter().collect();
            // A uid the index does not hold is new mail: the write hits
            // nothing, and the folder's own sync brings it with its flags.
            for uid in server.difference(&cached) {
                touched |= cache.set_keyword(account_id, &f.path, *uid, kw, true);
            }
            for uid in cached.difference(&server) {
                touched |= cache.set_keyword(account_id, &f.path, *uid, kw, false);
            }
        }
        if touched {
            changed.push(f.path.clone());
        }
    }
    if !changed.is_empty() {
        tracing::info!("keywords: changed {changed:?}");
    }
    changed
}

/// Re-count every folder's unread chip (EXAMINE + SEARCH each), one folder
/// at a time — and only while nothing is waiting on the worker. Each folder
/// costs two round trips, so a dozen folders on a slow link hold the
/// session for seconds; a message the user just opened would wait behind
/// the whole walk (a notification click did exactly that). `pending` is the
/// walk's place: empty starts a fresh pass over the cached folder list, and
/// what is left when a request interrupts is resumed by the next call.
/// Returns the folders whose counts moved and whether the pass finished.
async fn refresh_unread_counts(
    account_id: u32,
    session: &mut ImapSession,
    cache: Option<&Cache>,
    selected: Option<&str>,
    baseline: &mut std::collections::HashMap<String, (u32, Option<u32>)>,
    pending: &mut Vec<crate::models::Folder>,
    rx: &mpsc::UnboundedReceiver<MailRequest>,
    emit: &impl Fn(WorkerEvent),
) -> (Vec<(FolderKind, String)>, bool) {
    let mut changed = Vec::new();
    if pending.is_empty() {
        *pending = cache.map(|c| c.load_folders(account_id)).unwrap_or_default();
        pending.reverse(); // popped from the back, so walked in list order
    }
    while let Some(f) = pending.last().cloned() {
        if !rx.is_empty() {
            tracing::debug!("sweep: paused with {} folders left", pending.len());
            return (changed, false);
        }
        pending.pop();
        if Some(f.path.as_str()) == selected {
            continue;
        }
        let Ok(mb) = exam(session, &f.path).await else {
            tracing::debug!("sweep: cannot examine {}", f.path);
            continue;
        };
        let Some(unread) = selected_chip_count(session, Some(f.kind)).await else {
            continue;
        };
        emit(WorkerEvent::FolderUnread { folder_id: f.id, unread });
        let now = (unread, mb.uid_next);
        match baseline.insert(f.path.clone(), now) {
            Some(prev) if prev != now => {
                tracing::debug!("sweep: {} moved {:?} -> {:?}", f.path, prev, now);
                changed.push((f.kind, f.path.clone()));
            }
            _ => {}
        }
    }
    if !changed.is_empty() {
        tracing::info!(
            "sweep: changed {:?}",
            changed.iter().map(|(_, p)| p.as_str()).collect::<Vec<_>>()
        );
    }
    (changed, true)
}

/// Whether a folder's sidebar chip counts every message rather than unread
/// ones. Drafts: a draft is never unread, and the chip is there to say how
/// many are waiting to be finished.
pub fn chip_counts_all(kind: FolderKind) -> bool {
    kind == FolderKind::Drafts
}

/// The chip number for a folder whose messages are all in hand.
fn chip_count_of(kind: FolderKind, messages: &[Message]) -> u32 {
    if chip_counts_all(kind) {
        messages.len() as u32
    } else {
        messages.iter().filter(|m| m.unread).count() as u32
    }
}

/// The chip number for the currently-selected mailbox: unseen, or every
/// message for a kind that counts them all. `None` for an unknown kind
/// counts unseen.
async fn selected_chip_count(session: &mut ImapSession, kind: Option<FolderKind>) -> Option<u32> {
    if kind.is_some_and(chip_counts_all) {
        search_uids(session, "ALL").await.ok().map(|uids| uids.len() as u32)
    } else {
        selected_unseen(session).await
    }
}

/// A folder's kind as the cache last saw it, by path.
/// Answer the "Message body" filter conditions (#191) for an inbox sync on
/// the server: one `UID SEARCH … BODY` per alternative over the listed
/// uids, so the rules learn what the server finds without a body coming
/// down. The answer goes out as [`WorkerEvent::BodyHits`] just ahead of the
/// messages. Nothing is sent for other folders, without body rules, or for
/// an empty list; a search the server refuses is logged and skipped, and
/// that alternative then works off the list preview alone.
async fn emit_body_hits(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    account_id: u32,
    folder_id: u32,
    path: &str,
    messages: &[Message],
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    if messages.is_empty() || !is_inbox_path(cache, account_id, path) {
        return;
    }
    let needles = crate::config::filter_body_needles(&account.email);
    if needles.is_empty() {
        return;
    }
    let Some(sess) = session.as_mut() else { return };
    let uids: Vec<u32> = messages.iter().map(|m| m.uid).collect();
    let listed: std::collections::HashSet<u32> = uids.iter().copied().collect();
    let set = uid_set(&uids);
    let mut hits: std::collections::HashMap<u32, Vec<String>> = Default::default();
    for needle in needles {
        let query = body_search_query(&set, &needle);
        match search_uids(sess, &query).await {
            Ok(found) => {
                for uid in found.into_iter().filter(|u| listed.contains(u)) {
                    hits.entry(uid).or_default().push(needle.clone());
                }
            }
            Err(e) => tracing::warn!("filter: body search for {needle:?} failed: {e}"),
        }
    }
    tracing::info!("filter: body search over {} messages hit {}", uids.len(), hits.len());
    emit(WorkerEvent::BodyHits { folder_id, hits });
}

/// Whether `path` is the account's inbox: by the cached folder list, or by
/// name when there is no cache to ask.
fn is_inbox_path(cache: Option<&Cache>, account_id: u32, path: &str) -> bool {
    match cached_folder_kind(cache, account_id, path) {
        Some(kind) => kind == FolderKind::Inbox,
        None => path.eq_ignore_ascii_case("INBOX"),
    }
}

/// The `UID SEARCH` keys that find `needle` in the bodies of `set`. A
/// needle beyond ASCII needs the CHARSET key in front (RFC 3501 §6.4.4),
/// which must lead the keys; quotes and backslashes are escaped as in any
/// quoted string, and line breaks can never be in a quoted string at all.
fn body_search_query(set: &str, needle: &str) -> String {
    let clean: String = needle.chars().filter(|c| !matches!(c, '\r' | '\n')).collect();
    let quoted = clean.replace('\\', "\\\\").replace('"', "\\\"");
    let charset = if clean.is_ascii() { "" } else { "CHARSET UTF-8 " };
    format!("{charset}UID {set} BODY \"{quoted}\"")
}

/// Microsoft Graph's side of [`emit_body_hits`]: one `$search="body:…"`
/// listing per alternative over the inbox, intersected with the listed
/// messages (Graph ids hash to the uids the app knows).
async fn emit_graph_body_hits(
    token: &str,
    account: &AccountConfig,
    folder_id: u32,
    path: &str,
    messages: &[Message],
    state: &GraphState,
    emit: &impl Fn(WorkerEvent),
) {
    if messages.is_empty() || state.inbox.as_ref().map(|(_, p)| p.as_str()) != Some(path) {
        return;
    }
    let needles = crate::config::filter_body_needles(&account.email);
    if needles.is_empty() {
        return;
    }
    let Some((_, gid)) = state.folders.get(path).cloned() else { return };
    let listed: std::collections::HashSet<u32> = messages.iter().map(|m| m.uid).collect();
    let mut hits: std::collections::HashMap<u32, Vec<String>> = Default::default();
    for needle in needles {
        // KQL: the term in quotes; a quote inside it would end the term.
        let term = format!("\"body:{}\"", needle.replace('"', " "));
        let url = format!(
            "{GRAPH_BASE}/me/mailFolders/{gid}/messages?$search={}&$select=id&$top=250",
            url_query_encode(&term)
        );
        let t = token.to_string();
        let found = tokio::task::spawn_blocking(move || graph_paged(&t, &url, 250))
            .await
            .unwrap_or_else(|_| Err("task failed".into()));
        match found {
            Ok(items) => {
                for uid in items
                    .iter()
                    .filter_map(|v| v["id"].as_str())
                    .map(hash_uid)
                    .filter(|u| listed.contains(u))
                {
                    hits.entry(uid).or_default().push(needle.clone());
                }
            }
            Err(e) => tracing::warn!("filter: Graph body search for {needle:?} failed: {e}"),
        }
    }
    tracing::info!("filter: Graph body search over {} messages hit {}", listed.len(), hits.len());
    emit(WorkerEvent::BodyHits { folder_id, hits });
}

/// Percent-encode a URL query value (everything but the unreserved set).
fn url_query_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn cached_folder_kind(cache: Option<&Cache>, account_id: u32, path: &str) -> Option<FolderKind> {
    cache?.load_folders(account_id).into_iter().find(|f| f.path == path).map(|f| f.kind)
}

/// Count unseen messages in the currently-selected mailbox via SEARCH (safe on
/// the selected folder, unlike STATUS on some servers). Downloads only ids.
async fn selected_unseen(session: &mut ImapSession) -> Option<u32> {
    search_uids(session, "UNSEEN")
        .await
        .ok()
        .map(|uids| uids.len() as u32)
}

/// Load a folder's message index for immediate display.
///
/// Never-synced folder → fetch a fast [`FIRST_PAGE`] of the newest messages so
/// browsing is instant; the background backfill indexes the rest. Already-cached
/// folder → fetch just the recent window and merge it over the existing (possibly
/// whole-mailbox) index, picking up new mail and flag changes without re-pulling
/// thousands of envelopes.
async fn load_messages(
    account_id: u32,
    session: &mut ImapSession,
    folder_id: u32,
    path: &str,
    use_envelope: bool,
    cache: Option<&Cache>,
) -> Result<Vec<Message>, async_imap::error::Error> {
    let mailbox = sel(session, path).await?;
    let total = mailbox.exists;
    if total == 0 {
        // Folder emptied on the server — drop any cached copies so they don't linger.
        if let Some(c) = cache {
            for uid in c.cached_uids(account_id, path) {
                c.delete_message(account_id, path, uid);
            }
        }
        return Ok(Vec::new());
    }

    let cached = cache
        .map(|c| c.load_messages(account_id, path, folder_id))
        .unwrap_or_default();
    if cached.is_empty() {
        let mut messages =
            fetch_window(account_id, session, folder_id, total, FIRST_PAGE, use_envelope).await?;
        reconcile_attachment_flags(cache, account_id, path, &mut messages);
        Ok(messages)
    } else {
        let recent =
            fetch_window(account_id, session, folder_id, total, PAGE_SIZE, use_envelope).await?;
        let mut merged = merge_index(cached, recent);
        redecode_garbled_previews(session, &mut merged).await;
        // Reconcile deletions/moves made on the server or another device: drop any
        // message whose UID the server no longer lists (a plain merge would keep it
        // forever), and prune it from the cache so it doesn't come back. The full
        // UID set is a cheap server-side search even for large mailboxes.
        let server: std::collections::HashSet<u32> = search_uids(session, "ALL").await?;
        if let Some(c) = cache {
            for m in merged.iter().filter(|m| !server.contains(&m.uid)) {
                c.delete_message(account_id, path, m.uid);
            }
        }
        merged.retain(|m| server.contains(&m.uid));
        reconcile_attachment_flags(cache, account_id, path, &mut merged);
        Ok(merged)
    }
}

/// Clear the "has attachment" flag on freshly-fetched summaries whose bodies we
/// already downloaded and found to contain no real attachments. Server summary
/// flags (especially iCloud's header-only `multipart/mixed` guess) over-report
/// attachments for HTML mail whose only extra parts are inline `cid:` images.
fn reconcile_attachment_flags(
    cache: Option<&Cache>,
    account_id: u32,
    path: &str,
    messages: &mut [Message],
) {
    let Some(c) = cache else { return };
    if messages.iter().all(|m| !m.has_attachment) {
        return;
    }
    let attachmentless = c.attachmentless_uids(account_id, path);
    if attachmentless.is_empty() {
        return;
    }
    for m in messages.iter_mut() {
        if m.has_attachment && attachmentless.contains(&m.uid) {
            m.has_attachment = false;
        }
    }
}

/// Fetch the most-recent `count` messages' summaries (newest first). Includes
/// BODYSTRUCTURE so the attachment indicator is known for every indexed message
/// — still no bodies/attachments are downloaded.
async fn fetch_window(
    account_id: u32,
    session: &mut ImapSession,
    folder_id: u32,
    total: u32,
    count: u32,
    use_envelope: bool,
) -> Result<Vec<Message>, async_imap::error::Error> {
    let start = total.saturating_sub(count - 1).max(1);
    let range = format!("{start}:{total}");

    // Normally we fetch the structured ENVELOPE + BODYSTRUCTURE (compact, and the
    // latter gives the attachment indicator). But some servers (notably iCloud)
    // emit RFC-noncompliant ENVELOPE/BODYSTRUCTURE (e.g. NIL transfer-encodings,
    // unescaped quotes in the Message-ID) that our IMAP parser rejects. For those
    // the caller retries with `use_envelope = false`, and we instead pull the raw
    // header block — opaque to the IMAP parser — and parse it with mail-parser.
    // `BODY.PEEK[1]<0.2048>` rides along for the list preview, with `BODY.PEEK[1.MIME]`
    // — the part's own headers — so the bytes are read in the charset they
    // declare rather than assumed UTF-8 (#159). Section 1 is the
    // first body part of a multipart message and the whole body of a single-part
    // one, so one query covers both without a second round trip per message.
    // When that first part is itself a multipart — `mixed(alternative(text, html),
    // attachment)`, which is what ProtonMail sends — what comes back is the nested
    // MIME rather than any text, and [`preview_from_part`] descends into it.
    //
    // With previews switched off it is left out altogether: the setting exists
    // partly to avoid downloading a slice of every message, so honouring it only
    // in the UI would miss the point. Read per sync, so turning it back on takes
    // effect without a restart.
    let want_preview = crate::config::load_preview_lines() > 0;
    let preview_part = if want_preview { preview_fetch_items() } else { String::new() };
    let mut messages: Vec<Message> = if use_envelope {
        let query = format!(
            "(UID ENVELOPE FLAGS BODYSTRUCTURE INTERNALDATE{REFS_FETCH_ITEM}{preview_part})"
        );
        let fetches: Vec<Fetch> = session.fetch(&range, query).await?.try_collect().await?;
        fetches
            .iter()
            .map(|f| {
                let mut m = build_summary(account_id, f, folder_id);
                m.preview = preview_of(f);
                m
            })
            .collect()
    } else {
        let query = format!("(UID FLAGS BODY.PEEK[HEADER] INTERNALDATE{preview_part})");
        let fetches: Vec<Fetch> = session.fetch(&range, query).await?.try_collect().await?;
        fetches
            .iter()
            .map(|f| {
                let mut m = summary_from_headers(account_id, f, folder_id);
                m.preview = preview_of(f);
                m
            })
            .collect()
    };
    // Fill in previews the summary fetch came back without (see
    // [`retry_missing_previews`] for who those are). Bounded so body-less
    // messages don't grow the fetch each sync.
    if want_preview {
        let missing: Vec<u32> = messages
            .iter()
            .filter(|m| m.preview.is_empty())
            .take(PREVIEW_REDECODE_ROWS)
            .map(|m| m.uid)
            .collect();
        retry_missing_previews(session, &mut messages, missing).await;
    }
    messages.reverse(); // IMAP returns oldest-first; show newest at the top.
    Ok(messages)
}

/// The preview snippet from a fetch that asked for `BODY.PEEK[1]` together with
/// `BODY.PEEK[1.MIME]`, the part's own headers, which say what the bytes are.
fn preview_of(fetch: &Fetch) -> String {
    use async_imap::imap_proto::types::SectionPath;
    let ctype = section_content_type(fetch);
    // An image or a document as the first part — photo mail, a forwarded
    // file — has no text to show. An empty preview here lets the BODY[TEXT]
    // retry look further into the message for the text part, instead of the
    // row wearing the file's first bytes.
    if ctype.as_deref().is_some_and(|t| {
        !(t.starts_with("text/") || t.starts_with("multipart/") || t.starts_with("message/"))
    }) {
        return String::new();
    }
    let charset = ctype.as_deref().and_then(charset_param);
    let encoding = section_encoding(fetch);
    let bytes = fetch.section(&SectionPath::Part(vec![1], None)).unwrap_or_default();
    let p = preview_from_part(bytes, charset.as_deref(), encoding.as_deref());
    // A preheader is built to defeat exactly this read: a marketing mail can
    // spend thousands of bytes on invisible padding before its first real
    // sentence, the way an HTML part spends them on <head>. When the slice ran
    // to its limit and still yielded almost nothing to show, report nothing
    // rather than a scrap, and let the deeper repair read — bounded, and rare
    // — go and find the text.
    if bytes.len() >= PREVIEW_FETCH_BYTES && p.chars().count() < PREVIEW_MIN_CHARS {
        return String::new();
    }
    // GTK labels abort on interior NULs, and decoded message text can carry
    // them.
    if p.contains('\0') { p.replace('\0', " ") } else { p }
}

/// The lowercased Content-Type section 1 declares, read from the
/// `BODY[1.MIME]` header block that rides along with the preview fetch (#159).
/// `None` when the server sent no MIME headers.
fn section_content_type(fetch: &Fetch) -> Option<String> {
    use async_imap::imap_proto::types::{MessageSection, SectionPath};
    let mime = fetch.section(&SectionPath::Part(vec![1], Some(MessageSection::Mime)))?;
    mime_header(&String::from_utf8_lossy(mime), "content-type")
}

/// The charset section 1 declares, when its headers name one — a multipart
/// part 1 doesn't, its text parts carry their own.
fn section_charset(fetch: &Fetch) -> Option<String> {
    section_content_type(fetch).as_deref().and_then(charset_param)
}

/// The transfer encoding section 1 declares, from the same `BODY[1.MIME]`
/// block the charset comes from. Reading it beats guessing: a part can be
/// quoted-printable and still be spelled entirely in base64's alphabet.
fn section_encoding(fetch: &Fetch) -> Option<String> {
    use async_imap::imap_proto::types::{MessageSection, SectionPath};
    let mime = fetch.section(&SectionPath::Part(vec![1], Some(MessageSection::Mime)))?;
    mime_header(&String::from_utf8_lossy(mime), "content-transfer-encoding")
}

/// How much of a message's first body part to fetch for the list preview. Enough
/// for a couple of lines of text after decoding, small enough that syncing a
/// large mailbox doesn't turn into downloading it.
const PREVIEW_FETCH_BYTES: usize = 2048;

/// Longest preview stored per message.
const PREVIEW_CHARS: usize = 240;

/// Below this many characters, a preview read from a full slice has not found
/// the message's text yet — it is still in the preheader padding or the
/// `<head>` — and the repair read is worth the round trip.
const PREVIEW_MIN_CHARS: usize = 30;

/// The bigger slice for the preview *repair* fetch (the BODY[TEXT] retry for
/// messages whose summary fetch produced no preview). An HTML-only message can
/// spend several KB on `<head>` boilerplate before its first visible text —
/// which is exactly how a message ends up on the repair list — so the retry
/// reads deep enough to find it. Only a handful of messages ever qualify.
const PREVIEW_REPAIR_BYTES: usize = 32768;

/// How many cached rows with a garbled preview one sync re-reads.
const PREVIEW_REDECODE_ROWS: usize = 12;

/// The fetch items that read a message's first part for the list preview: a
/// slice of its bytes plus its MIME headers, for the charset.
fn preview_fetch_items() -> String {
    format!(" BODY.PEEK[1]<0.{PREVIEW_FETCH_BYTES}> BODY.PEEK[1.MIME]")
}

/// Re-read the previews of cached rows that still show replacement characters
/// (#159). Rows indexed before the preview fetch learned the part's charset
/// keep their garbled snippet otherwise, since only the recent window is read
/// again on each sync. A few rows per sync.
async fn redecode_garbled_previews(session: &mut ImapSession, messages: &mut [Message]) {
    if crate::config::load_preview_lines() == 0 {
        return;
    }
    let uids: Vec<u32> = messages
        .iter()
        .filter(|m| m.preview.contains('\u{fffd}'))
        .take(PREVIEW_REDECODE_ROWS)
        .map(|m| m.uid)
        .collect();
    if uids.is_empty() {
        return;
    }
    tracing::info!("previews: re-reading garbled rows uids={uids:?}");
    let fetched: Result<Vec<Fetch>, _> = async {
        fetch_uids(session, uid_set(&uids), format!("(UID{})", preview_fetch_items()))
            .await?
            .try_collect()
            .await
    }
    .await;
    let fetches = match fetched {
        Err(e) => {
            tracing::warn!("previews: garbled-row re-read failed: {e}");
            return;
        }
        Ok(f) => f,
    };
    let mut still_missing = Vec::new();
    for f in &fetches {
        let Some(m) = f.uid.and_then(|uid| messages.iter_mut().find(|m| m.uid == uid)) else {
            continue;
        };
        // Whatever this read produced — including nothing, when the part
        // turned out to be a file — beats the garbage it replaces, and an
        // empty row is not asked about again next sync.
        m.preview = preview_of(f);
        if m.preview.is_empty() {
            still_missing.push(m.uid);
        }
    }
    retry_missing_previews(session, messages, still_missing).await;
}

/// Fill in previews for `uids`, rows whose first-part read came back empty.
///
/// iCloud answers BODY[1] with nothing on a message it re-appended (a move-back
/// from an undo, for one) — and keeps doing so on every later sync, so the row
/// would wear a blank summary for good — and a message whose first part is a
/// file has its text further in. The TEXT section survives (the full-body
/// fetch the reader uses works fine), so re-ask with that and run the same
/// MIME-prefix extraction. Callers bound the list: genuinely body-less messages
/// stay empty and shouldn't grow the fetch each sync.
async fn retry_missing_previews(session: &mut ImapSession, messages: &mut [Message], uids: Vec<u32>) {
    // Rows the retry already answered this session are served from that
    // answer (see [`PREVIEW_RETRIED`]): the retry runs on every sync of the
    // folder, and it would otherwise cost 32 KB per row per sync for as
    // long as the row stayed among those checked.
    fn key(messages: &[Message], uid: u32) -> Option<(u32, u32, u32)> {
        messages.iter().find(|m| m.uid == uid).map(|m| (m.account_id, m.folder_id, uid))
    }
    let uids: Vec<u32> = {
        let retried = PREVIEW_RETRIED.lock().ok();
        uids.into_iter()
            .filter(|&uid| {
                let Some(k) = key(messages, uid) else { return false };
                match retried.as_ref().and_then(|r| r.get(&k)) {
                    None => true,
                    Some(found) => {
                        if let Some(m) = messages.iter_mut().find(|m| m.uid == uid) {
                            m.preview = found.clone();
                        }
                        false
                    }
                }
            })
            .collect()
    };
    if uids.is_empty() {
        return;
    }
    retry_missing_previews_now(session, messages, &uids).await;
    let answers: Vec<((u32, u32, u32), String)> = uids
        .iter()
        .filter_map(|&uid| {
            let k = key(messages, uid)?;
            let preview = messages.iter().find(|m| m.uid == uid)?.preview.clone();
            Some((k, preview))
        })
        .collect();
    if let Ok(mut retried) = PREVIEW_RETRIED.lock() {
        retried.extend(answers);
    }
}

/// The BODY[TEXT] retry itself, for `uids` (non-empty).
async fn retry_missing_previews_now(session: &mut ImapSession, messages: &mut [Message], uids: &[u32]) {
    tracing::info!("previews: retrying via BODY[TEXT] uids={uids:?}");
    let refetched: Result<Vec<Fetch>, _> = async {
        fetch_uids(
            session,
            uid_set(&uids),
            format!("(UID BODY.PEEK[TEXT]<0.{PREVIEW_REPAIR_BYTES}> BODY.PEEK[1.MIME])"),
        )
        .await?
        .try_collect()
        .await
    }
    .await;
    match refetched {
        Err(e) => tracing::warn!("previews: BODY[TEXT] retry failed: {e}"),
        Ok(fetches) => {
            use async_imap::imap_proto::types::{MessageSection, SectionPath};
            for f in &fetches {
                // For a multipart, `text_in_multipart` reads each part's own
                // headers and these go unused; for a single-part message,
                // section 1 is the body and they describe it exactly.
                let charset = section_charset(f);
                let encoding = section_encoding(f);
                let p = f
                    .section(&SectionPath::Full(MessageSection::Text))
                    .map(|b| preview_from_part(b, charset.as_deref(), encoding.as_deref()))
                    .unwrap_or_default()
                    .replace('\0', " ");
                if p.is_empty() {
                    continue;
                }
                if let Some(m) = f.uid.and_then(|uid| messages.iter_mut().find(|m| m.uid == uid)) {
                    m.preview = p;
                }
            }
        }
    }
}

/// Turn the first bytes of a message's body part into a one-line snippet for the
/// message list.
///
/// What arrives is raw MIME, cut off mid-stream: still transfer-encoded, perhaps
/// HTML, and (being a prefix) possibly ending mid-character or mid-tag. The
/// transfer encoding is declared in headers this fetch doesn't include, so it is
/// inferred from the bytes themselves — wrongly guessing plain text for base64
/// would show gibberish in the list, which is worse than showing nothing. The
/// charset is the one the part's MIME headers declared, when the caller has
/// them.
fn preview_from_part(bytes: &[u8], charset: Option<&str>, encoding: Option<&str>) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    // Section 1 can be a whole nested multipart rather than a body: descend to the
    // text inside it, which also carries the encoding and charset in its own
    // headers instead of leaving them to be guessed.
    if let Some(inner) = text_in_multipart(bytes, 0) {
        return finish_preview(inner);
    }
    // What the part says it is, when it says: the headers ride along with the
    // preview fetch, and a declaration beats any amount of inspection.
    let declared = encoding.map(|e| e.trim().to_ascii_lowercase());
    if let Some(e) = declared.as_deref().filter(|e| {
        e.starts_with("base64")
            || e.starts_with("quoted-printable")
            || matches!(*e, "7bit" | "8bit" | "binary")
    }) {
        return finish_preview(decode_mime_body(bytes, e, charset));
    }
    let decoded = if looks_like_base64(bytes) {
        decode_base64_prefix(bytes)
    } else if bytes.contains(&b'=') {
        decode_quoted_printable(bytes)
    } else {
        bytes.to_vec()
    };

    finish_preview(decode_text(&decoded, charset))
}

/// Text from a body's bytes, in the charset its headers declared (#159).
///
/// Everything here used to be read as UTF-8, so a bank's `iso-8859-2` mail
/// showed a replacement character for every accented letter in the list while
/// the reader — which hands the whole message to mail-parser — showed it fine.
/// mail-parser's charset table is used here too; UTF-8 and ASCII are read
/// directly.
fn decode_text(bytes: &[u8], charset: Option<&str>) -> String {
    let charset = charset
        .map(|c| c.trim().trim_matches('"').to_ascii_lowercase())
        .filter(|c| !c.is_empty());
    let declared_unicode =
        matches!(charset.as_deref(), Some("utf-8" | "utf8" | "us-ascii" | "ascii"));
    if let Some(cs) = charset.as_deref().filter(|_| !declared_unicode) {
        if let Some(decode) = mail_parser::decoders::charsets::map::charset_decoder(cs.as_bytes()) {
            return decode(bytes);
        }
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        // A fetch of the first N bytes can end mid-character: keep the whole
        // characters and drop the fragment rather than show it as noise.
        Err(e) if e.error_len().is_none() => {
            String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned()
        }
        // Declared UTF-8 that isn't: replacement characters mark the damage.
        Err(_) if declared_unicode => String::from_utf8_lossy(bytes).into_owned(),
        // 8-bit text with no (usable) charset: read it as Windows-1252, as
        // browsers and mail clients do, which at least shows Western European
        // accents rather than a row of replacement characters.
        Err(_) => mail_parser::decoders::charsets::single_byte::decoder_cp1252(bytes),
    }
}

/// The `charset=` parameter of a (lowercased) Content-Type value.
fn charset_param(ctype: &str) -> Option<String> {
    ctype
        .split(';')
        .skip(1)
        .find_map(|p| {
            let (k, v) = p.split_once('=')?;
            (k.trim() == "charset").then(|| v.trim().trim_matches('"').to_string())
        })
        .filter(|c| !c.is_empty())
}

/// Decode as much of a (possibly truncated) base64 body as forms whole groups.
fn decode_base64_prefix(body: &[u8]) -> Vec<u8> {
    let clean: String = body
        .iter()
        .filter(|b| !b.is_ascii_whitespace())
        .map(|&b| b as char)
        .collect();
    // Only whole 4-character groups can be decoded; the tail of a truncated
    // fetch is dropped rather than turned into noise.
    let usable = &clean[..clean.len() - clean.len() % 4];
    crate::oauth::base64_decode(usable).unwrap_or_default()
}

/// Tidy decoded body text into the single line the list shows.
fn finish_preview(decoded: String) -> String {
    // An HTML part becomes readable text; a plain part passes through.
    let plain = if decoded.contains('<') && decoded.contains('>') {
        crate::app::message_text(&decoded)
    } else {
        decoded
    };

    // Quoted replies say nothing about *this* message, so skip those lines.
    let body: String = plain
        .lines()
        .filter(|line| !line.trim_start().starts_with('>'))
        .collect::<Vec<_>>()
        .join("\n");
    let body = strip_link_noise(&body);
    // Marketing "preheader padding" — long chains of &nbsp; + zero-width
    // characters (zwnj, word-joiner, combining grapheme joiner, soft hyphen…)
    // meant to push the real body out of inbox previews. The zero-width
    // characters are NOT Unicode whitespace, so they would survive the
    // collapse below as a huge blank gap ending in the ellipsis. Drop them;
    // the non-breaking spaces between them then collapse away normally.
    let body: String = body
        .chars()
        .filter(|c| {
            !matches!(
                c,
                '\u{200b}'..='\u{200d}' // zero-width space/nbsp/joiners
                    | '\u{2060}'        // word joiner
                    | '\u{feff}'        // BOM / zero-width no-break space
                    | '\u{00ad}'        // soft hyphen
                    | '\u{034f}'        // combining grapheme joiner
                    | '\u{2800}'        // Braille blank, another padder
            )
        })
        .collect();
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(p) = pgp_preview(&collapsed) {
        return p;
    }
    collapsed.chars().take(PREVIEW_CHARS).collect()
}

/// What the list says for an OpenPGP-encrypted message (#133), whose first
/// text is otherwise the PGP/MIME version stub ("Version: 1") or the
/// armour itself — neither of which says anything a reader wants to see.
fn pgp_preview(collapsed: &str) -> Option<String> {
    let t = collapsed.trim();
    let stub = t.to_ascii_lowercase();
    let stub = stub.trim_start_matches("version:").trim();
    if stub == "1" || t.starts_with("-----BEGIN PGP MESSAGE-----") {
        return Some(crate::models::ENCRYPTED_PREVIEW.to_string());
    }
    None
}

/// Drop the link furniture that plain-text alternatives are built from, so the
/// preview starts where the message does.
///
/// A converter renders a link as `text ( url )`, so a mail whose first element is
/// a linked logo — every marketing template — begins with a bare tracking URL.
/// Vireo showed that URL as the preview where Apple Mail shows the greeting. The
/// parenthesised URLs go, and any URL-only lines left at the front go with them;
/// if that is the whole message, it stays as it was, since a link is better than
/// an empty row.
fn strip_link_noise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        let after = rest[open + 1..].trim_start();
        let is_url = after.starts_with("http://") || after.starts_with("https://");
        let close = rest[open..].find(')').map(|i| open + i);
        match (is_url, close) {
            // `( https://… )` — a rendered link, and nothing else in the brackets.
            (true, Some(close)) if !rest[open..close].trim_end_matches(')').contains('\n') => {
                out.push_str(&rest[..open]);
                rest = &rest[close + 1..];
            }
            _ => {
                out.push_str(&rest[..=open]);
                rest = &rest[open + 1..];
            }
        }
    }
    out.push_str(rest);

    // Whatever is left at the top that is only a link.
    let trimmed: String = out
        .lines()
        .skip_while(|line| {
            let l = line.trim();
            l.is_empty() || (is_url(l) && l.split_whitespace().count() == 1)
        })
        .collect::<Vec<_>>()
        .join("\n");

    if trimmed.trim().is_empty() {
        text.to_string()
    } else {
        trimmed
    }
}

/// Whether a word is a bare URL.
fn is_url(word: &str) -> bool {
    word.starts_with("http://") || word.starts_with("https://")
}

/// The text of the first readable part of a MIME multipart, or `None` if this
/// isn't one.
///
/// `text/plain` is preferred and `text/html` taken only if there is no plain
/// alternative, which matches what the reader shows. Nesting is followed a couple
/// of levels deep — enough for `mixed(alternative(…))` — and no further, since a
/// preview is not worth unbounded recursion through a hostile message.
fn text_in_multipart(part: &[u8], depth: usize) -> Option<String> {
    if depth > 3 {
        return None;
    }
    // The boundary opens the part — or follows a short preamble ("This is a
    // multi-part message in MIME format.") when what arrived is a whole body
    // rather than a part. A boundary delimiter, not a message that merely
    // opens with a dash: two dashes, more after them, no spaces.
    let boundary = part
        .split(|&b| b == b'\n')
        .take(10)
        .map(|line| {
            let end = line.iter().rposition(|b| !b.is_ascii_whitespace()).map_or(0, |i| i + 1);
            &line[..end]
        })
        .find(|line| line.starts_with(b"--") && line.len() >= 3 && !line.contains(&b' '))?;
    let mut html: Option<String> = None;
    for chunk in split_on(part, boundary).skip(1) {
        let start = chunk.iter().position(|b| !matches!(b, b'\r' | b'\n')).unwrap_or(chunk.len());
        let chunk = &chunk[start..];
        if chunk.starts_with(b"--") {
            break; // closing delimiter: no more parts
        }
        let Some((headers, body)) = split_mime_headers(chunk) else {
            continue;
        };
        let headers = String::from_utf8_lossy(headers);
        let ctype = mime_header(&headers, "content-type").unwrap_or_default();
        let encoding = mime_header(&headers, "content-transfer-encoding").unwrap_or_default();
        let charset = charset_param(&ctype);
        if ctype.starts_with("multipart/") {
            if let Some(inner) = text_in_multipart(body, depth + 1) {
                return Some(inner);
            }
        } else if ctype.starts_with("text/plain") {
            return Some(decode_mime_body(body, &encoding, charset.as_deref()));
        } else if ctype.starts_with("text/html") && html.is_none() {
            html = Some(decode_mime_body(body, &encoding, charset.as_deref()));
        }
    }
    html
}

/// The pieces of `hay` between occurrences of `sep`, like `str::split`.
fn split_on<'a>(hay: &'a [u8], sep: &'a [u8]) -> impl Iterator<Item = &'a [u8]> {
    let mut rest = Some(hay);
    std::iter::from_fn(move || {
        let cur = rest?;
        match cur.windows(sep.len()).position(|w| w == sep) {
            Some(i) => {
                rest = Some(&cur[i + sep.len()..]);
                Some(&cur[..i])
            }
            None => {
                rest = None;
                Some(cur)
            }
        }
    })
}

/// Split a MIME part into its header block and its body.
fn split_mime_headers(part: &[u8]) -> Option<(&[u8], &[u8])> {
    if let Some(i) = part.windows(4).position(|w| w == b"\r\n\r\n") {
        return Some((&part[..i], &part[i + 4..]));
    }
    part.windows(2)
        .position(|w| w == b"\n\n")
        .map(|i| (&part[..i], &part[i + 2..]))
}

/// One header's value, lowercased, from a MIME part's header block.
fn mime_header(headers: &str, name: &str) -> Option<String> {
    // Unfold first: a parameter on a continuation line ("Content-Type:
    // text/html;\r\n\tcharset=iso-8859-2") belongs to the header above it.
    let unfolded = headers.replace("\r\n", "\n").replace("\n ", " ").replace("\n\t", " ");
    unfolded
        .lines()
        .find(|l| {
            // Compare as bytes: a header block reaches us through
            // `from_utf8_lossy`, so a line can carry a multibyte character
            // that `name.len()` falls inside — slicing there would panic and
            // take the account's mail thread with it. A name is ASCII, so a
            // byte-wise match also proves `name.len()` is a char boundary,
            // which is what makes the slice below safe.
            l.len() > name.len()
                && l.as_bytes()[..name.len()].eq_ignore_ascii_case(name.as_bytes())
                && l[name.len()..].trim_start().starts_with(':')
        })
        .map(|l| l[name.len()..].trim_start()[1..].trim().to_ascii_lowercase())
}

/// Decode a part's body according to the encoding and charset it declares.
fn decode_mime_body(body: &[u8], encoding: &str, charset: Option<&str>) -> String {
    let bytes = if encoding.starts_with("base64") {
        decode_base64_prefix(body)
    } else if encoding.starts_with("quoted-printable") {
        decode_quoted_printable(body)
    } else {
        body.to_vec()
    };
    decode_text(&bytes, charset)
}

/// Whether a chunk looks like base64 rather than text: base64's alphabet only,
/// and long enough that a short plain word can't be mistaken for it.
///
/// An `=` is padding, and padding only ever closes a base64 stream — so one
/// anywhere else says this is not base64 however well the rest fits. That is
/// what quoted-printable looks like from here: `=E2=80=87=CD=8F` repeated is
/// nothing but letters, digits and `=`, and reading a marketing preheader's
/// worth of it as base64 turned a Cloudflare newsletter's preview into a row
/// of replacement characters. A truncated fetch of real base64 simply has no
/// padding in it at all.
fn looks_like_base64(bytes: &[u8]) -> bool {
    let mut significant = 0;
    let mut padding = 0;
    for &c in bytes {
        if c.is_ascii_whitespace() {
            continue;
        }
        if !(c.is_ascii_alphanumeric() || c == b'+' || c == b'/' || c == b'=') {
            return false;
        }
        if c == b'=' {
            padding += 1;
        } else if padding > 0 {
            // Alphabet after padding: the `=` was not closing anything.
            return false;
        }
        significant += 1;
    }
    significant >= 40 && padding <= 2
}

/// Decode quoted-printable to the bytes it stands for, leaving anything
/// malformed as written (a truncated fetch can end mid-escape).
fn decode_quoted_printable(src: &[u8]) -> Vec<u8> {
    // The fetch behind a preview is a slice of a part, so it ends wherever the
    // byte count ran out — routinely in the middle of an escape. Left alone
    // that stub reaches the reader as literal text ("… =E"), so drop it: a
    // trailing "=" (which in quoted-printable is a soft line break anyway) and
    // a trailing "=" plus one hex digit. Anything else is malformed rather
    // than cut short, and is left as written.
    let src = match src.len().checked_sub(2).map(|at| &src[at..]) {
        Some(&[b'=', h]) if h.is_ascii_hexdigit() => &src[..src.len() - 2],
        _ if src.last() == Some(&b'=') => &src[..src.len() - 1],
        _ => src,
    };
    let mut buf = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'=' {
            // "=\r\n" is a soft line break: the line continues.
            if src.get(i + 1) == Some(&b'\r') && src.get(i + 2) == Some(&b'\n') {
                i += 3;
                continue;
            }
            if src.get(i + 1) == Some(&b'\n') {
                i += 2;
                continue;
            }
            let hex = src.get(i + 1..i + 3).and_then(|h| {
                std::str::from_utf8(h).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
            });
            match hex {
                Some(b) => {
                    buf.push(b);
                    i += 3;
                    continue;
                }
                None => {
                    buf.push(b'=');
                    i += 1;
                    continue;
                }
            }
        }
        buf.push(src[i]);
        i += 1;
    }
    buf
}

/// Build a message summary from a raw header block (mail-parser), for servers
/// whose structured ENVELOPE our IMAP parser can't handle.
fn summary_from_headers(account_id: u32, fetch: &Fetch, folder_id: u32) -> Message {
    use mail_parser::MessageParser;

    let uid = fetch.uid.unwrap_or(0);
    let flags: Vec<Flag> = fetch.flags().collect();
    let unread = !flags.iter().any(|f| matches!(f, Flag::Seen));
    let starred = flags.iter().any(|f| matches!(f, Flag::Flagged));
    let keywords = custom_flags(&flags);

    let raw = fetch.header().unwrap_or(&[]);
    let parsed = MessageParser::default().parse(raw);

    let mp_first = |a: Option<&mail_parser::Address>| -> (String, String) {
        a.and_then(|a| a.first())
            .map(|addr| {
                let email = addr.address().unwrap_or_default().to_string();
                let name = addr.name().map(|s| s.to_string()).filter(|s| !s.is_empty());
                (name.unwrap_or_else(|| email.clone()), email)
            })
            .unwrap_or_else(|| ("Unknown".to_string(), String::new()))
    };
    let mp_list = |a: Option<&mail_parser::Address>| -> String {
        a.map(|a| {
            a.iter()
                .filter_map(|addr| addr.address().map(|s| s.to_string()))
                .filter(|e| !e.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
    };

    let (from_name, from_addr) = parsed
        .as_ref()
        .map(|p| mp_first(p.from()))
        .unwrap_or_else(|| ("Unknown".to_string(), String::new()));
    let subject = parsed
        .as_ref()
        .and_then(|p| p.subject())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(no subject)".to_string());
    let (date, timestamp) = parsed
        .as_ref()
        .and_then(|p| p.date())
        .map(|d| {
            let ts = d.to_timestamp();
            (format_timestamp(ts), ts)
        })
        .filter(|(_, ts)| *ts > 0)
        .unwrap_or_else(|| internal_date_summary(fetch));
    let to = parsed.as_ref().map(|p| mp_list(p.to())).unwrap_or_default();
    let cc = parsed.as_ref().map(|p| mp_list(p.cc())).unwrap_or_default();
    let reply_to = parsed.as_ref().map(|p| mp_list(p.reply_to())).unwrap_or_default();
    let reply_to = if reply_to.eq_ignore_ascii_case(&from_addr) { String::new() } else { reply_to };

    // Best-effort attachment hint from the top-level Content-Type (BODYSTRUCTURE
    // isn't available on this path). multipart/mixed is the usual attachment case.
    let has_attachment = parsed
        .as_ref()
        .and_then(|p| p.header("Content-Type"))
        .and_then(|h| h.as_content_type())
        .map(|ct| {
            ct.ctype().eq_ignore_ascii_case("multipart")
                && ct
                    .subtype()
                    .is_some_and(|s| s.eq_ignore_ascii_case("mixed"))
        })
        .unwrap_or(false);

    let (message_id, references) = mp_thread_ids(parsed.as_ref());

    let mut msg = Message {
        id: uid,
        account_id,
        folder_id,
        uid,
        from_name,
        from_addr,
        reply_to,
        to,
        cc,
        subject,
        preview: String::new(),
        body: String::new(),
        date,
        timestamp,
        unread,
        starred,
        keywords,
        has_attachment,
        message_id,
        references,
    };
    msg.scrub_nuls();
    msg
}

/// Extract (message_id, references) from a parsed message for threading. References
/// combines In-Reply-To and References, normalized (no angle brackets, lowercased).
fn mp_thread_ids(parsed: Option<&mail_parser::Message>) -> (String, String) {
    use mail_parser::HeaderValue;
    let norm = |s: &str| {
        s.trim().trim_start_matches('<').trim_end_matches('>').trim().to_ascii_lowercase()
    };
    let collect = |hv: &HeaderValue| -> Vec<String> {
        match hv {
            HeaderValue::Text(t) => vec![norm(t)],
            HeaderValue::TextList(v) => v.iter().map(|t| norm(t)).collect(),
            _ => Vec::new(),
        }
    };
    let Some(p) = parsed else {
        return (String::new(), String::new());
    };
    let message_id = p.message_id().map(norm).unwrap_or_default();
    let mut refs: Vec<String> = Vec::new();
    for id in collect(p.in_reply_to()).into_iter().chain(collect(p.references())) {
        if !id.is_empty() && !refs.contains(&id) {
            refs.push(id);
        }
    }
    (message_id, refs.join(" "))
}

/// Overlay a freshly-fetched recent window onto the cached index: recent rows
/// replace their cached versions (updated flags / new mail), the rest are kept.
/// No size cap — the whole folder is searchable once the background backfill has
/// indexed it.
fn merge_index(cached: Vec<Message>, recent: Vec<Message>) -> Vec<Message> {
    let mut map: std::collections::HashMap<u32, Message> =
        cached.into_iter().map(|m| (m.uid, m)).collect();
    for m in recent {
        map.insert(m.uid, m);
    }
    let mut out: Vec<Message> = map.into_values().collect();
    out.sort_by(|a, b| b.uid.cmp(&a.uid)); // newest first
    out
}

/// How many messages one References-repair step asks the server about. Each row
/// costs only its References header, so this can be larger than an envelope
/// chunk without making the connection unresponsive to real requests.
const REFS_REPAIR_CHUNK: usize = 500;

/// One step of the one-time References repair for the folder at the head of the
/// queue (#21, #42).
///
/// Messages indexed before Vireo asked for References hold only In-Reply-To,
/// which names the immediate parent — and for an incoming reply that parent is
/// usually your own message in Sent, so the link points outside the folder and
/// the reply starts a thread of its own. This asks the server for the one header
/// that fixes it, for the replies that can still be improved, newest first.
///
/// The watermark makes it resumable and run exactly once per folder; a repaired
/// row re-threads the next time its folder is read from cache.
async fn run_one_refs_repair(
    queue: &mut std::collections::VecDeque<(u32, String)>,
    session: &mut Option<ImapSession>,
    account_id: u32,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    let Some((folder_id, path)) = queue.pop_front() else {
        return;
    };
    let Some(c) = cache else { return }; // nothing to repair without a cache
    let Some(mut s) = session.take() else {
        queue.push_front((folder_id, path));
        return;
    };

    let (below, _) = c.refs_repair_state(account_id, &path);
    let chunk = c.uids_needing_references(account_id, &path, below, REFS_REPAIR_CHUNK);
    if chunk.is_empty() {
        c.set_refs_repair_state(account_id, &path, 0, true);
        *session = Some(s); // folder done — don't requeue
        return;
    }

    let set = uid_set(&chunk.iter().map(|(uid, _)| *uid).collect::<Vec<_>>());
    let fetched: Result<Vec<Fetch>, _> = async {
        sel(&mut s, &path).await?;
        fetch_uids(&mut s, &set, format!("(UID{REFS_FETCH_ITEM})")).await?.try_collect().await
    }
    .await;
    let Ok(fetches) = fetched else {
        // Leave the watermark alone and try this folder again later.
        queue.push_back((folder_id, path));
        *session = Some(s);
        return;
    };

    let found: std::collections::HashMap<u32, String> = fetches
        .iter()
        .filter_map(|f| f.uid.map(|uid| (uid, references_of(f))))
        .collect();
    let mut repaired = 0usize;
    for (uid, existing) in &chunk {
        if let Some(refs) = found.get(uid) {
            let merged = merge_msgids(refs, existing);
            if &merged != existing {
                c.set_references(account_id, &path, *uid, &merged);
                repaired += 1;
            }
        }
    }
    // Walk on from the oldest uid this chunk covered.
    let lowest = chunk.iter().map(|(uid, _)| *uid).min().unwrap_or(0);
    c.set_refs_repair_state(account_id, &path, lowest, false);
    queue.push_back((folder_id, path));
    *session = Some(s);
    if repaired > 0 {
        // The folder's cached rows have changed underneath whatever is on
        // screen; re-emitting lets the list re-thread with what was found.
        emit(WorkerEvent::RefsRepaired { folder_id });
    }
}

/// A background job to index the rest of a folder (everything past the fast first
/// page) so search covers the whole mailbox. `remaining` is the still-to-fetch
/// UIDs (newest first), computed lazily on the first drain.
struct Backfill {
    folder_id: u32,
    path: String,
    remaining: Option<Vec<u32>>,
    /// Whether this folder feeds the attachments gallery (not Trash/Junk/Drafts);
    /// if so, its backfilled messages' attachments are prefetched too.
    gallery: bool,
}

/// Determine which UIDs still need indexing: everything on the server not already
/// cached. Also reconciles deletions (cached UIDs the server no longer has).
async fn backfill_worklist(
    session: &mut ImapSession,
    account_id: u32,
    path: &str,
    cache: Option<&Cache>,
) -> Result<Vec<u32>, async_imap::error::Error> {
    sel(session, path).await?;
    let server: std::collections::HashSet<u32> = search_uids(session, "ALL").await?;
    let cached = cache
        .map(|c| c.cached_uids(account_id, path))
        .unwrap_or_default();
    if let Some(c) = cache {
        for uid in cached.iter() {
            if !server.contains(uid) {
                c.delete_message(account_id, path, *uid);
            }
        }
    }
    let mut remaining: Vec<u32> = server.difference(&cached).copied().collect();
    remaining.sort_unstable_by(|a, b| b.cmp(a)); // newest first
    Ok(remaining)
}

/// Fetch message summaries for a specific set of UIDs (used by the backfill).
async fn fetch_summaries_by_uid(
    account_id: u32,
    session: &mut ImapSession,
    folder_id: u32,
    path: &str,
    uids: &[u32],
    use_envelope: bool,
) -> Result<Vec<Message>, async_imap::error::Error> {
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    sel(session, path).await?;
    let set = uid_set(uids);
    let items: String = if use_envelope {
        format!("(UID ENVELOPE FLAGS BODYSTRUCTURE INTERNALDATE{REFS_FETCH_ITEM})")
    } else {
        "(UID FLAGS BODY.PEEK[HEADER] INTERNALDATE)".to_string()
    };
    let fetches: Vec<Fetch> = fetch_uids(session, set, items).await?.try_collect().await?;
    Ok(fetches
        .iter()
        .map(|f| {
            if use_envelope {
                build_summary(account_id, f, folder_id)
            } else {
                summary_from_headers(account_id, f, folder_id)
            }
        })
        .collect())
}

/// Advance one backfill job by a chunk. Fetches the next `BACKFILL_CHUNK` UIDs,
/// upserts them into the cache, and emits them as an append to the UI's index.
/// Requeues the job (at the back) if more remain. Reconnects and, if needed,
/// disables ENVELOPE parsing (iCloud) on a parse failure.
#[allow(clippy::too_many_arguments)]
async fn run_one_backfill(
    queue: &mut std::collections::VecDeque<Backfill>,
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    prefetch: &mut std::collections::VecDeque<(String, u32)>,
    use_envelope: &mut bool,
    emit: &impl Fn(WorkerEvent),
) {
    let Some(mut job) = queue.pop_front() else {
        return;
    };
    let Some(mut s) = session.take() else {
        queue.push_front(job);
        return;
    };

    // Compute the worklist on first touch (with one reconnect on failure).
    if job.remaining.is_none() {
        match backfill_worklist(&mut s, account_id, &job.path, cache).await {
            Ok(rem) => job.remaining = Some(rem),
            Err(_) => match connect(account).await {
                Ok(fresh) => {
                    s = fresh;
                    match backfill_worklist(&mut s, account_id, &job.path, cache).await {
                        Ok(rem) => job.remaining = Some(rem),
                        Err(_) => {
                            *session = Some(s);
                            queue.push_back(job);
                            return;
                        }
                    }
                }
                Err(_) => {
                    // Stay offline; retry this job on the next connect.
                    queue.push_back(job);
                    return;
                }
            },
        }
    }

    let rem = job.remaining.as_mut().unwrap();
    if rem.is_empty() {
        emit(WorkerEvent::BackfillDone { folder_id: job.folder_id });
        *session = Some(s); // done — don't requeue
        return;
    }
    let take = rem.len().min(BACKFILL_CHUNK);
    let chunk: Vec<u32> = rem.drain(..take).collect();

    match fetch_summaries_by_uid(account_id, &mut s, job.folder_id, &job.path, &chunk, *use_envelope)
        .await
    {
        Ok(msgs) => {
            if let Some(c) = cache {
                c.upsert_messages(account_id, &job.path, &msgs);
            }
            // Gallery folders: queue this chunk's attachments for background
            // download so they appear in the attachments gallery.
            if job.gallery {
                queue_attachment_prefetch(prefetch, &job.path, &msgs, cache, account_id);
            }
            emit(WorkerEvent::MessagesAppend {
                folder_id: job.folder_id,
                messages: msgs,
            });
            *session = Some(s);
        }
        Err(_) => {
            // Put the chunk back and reconnect; a parse error means the server's
            // structured responses are unusable (iCloud) — fall back to headers.
            for uid in chunk.into_iter().rev() {
                rem.insert(0, uid);
            }
            if *use_envelope {
                *use_envelope = false;
            }
            if let Ok(fresh) = connect(account).await {
                *session = Some(fresh);
            }
            // else: session stays None; the main loop reconnects on next request.
        }
    }

    if job.remaining.as_ref().is_some_and(|r| !r.is_empty()) {
        queue.push_back(job);
    } else {
        emit(WorkerEvent::BackfillDone { folder_id: job.folder_id });
    }
}

/// Returns the rendered body, the sender verdict, and whether the message
/// actually carries attachments — all three from the one fetch.
async fn load_body(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
) -> Result<(String, crate::models::SenderCheck, bool), async_imap::error::Error> {
    sel(session, path).await?;

    // Fetch the whole message (PEEK so \Seen isn't set) and extract the body with
    // mail-parser. We deliberately avoid a BODYSTRUCTURE-based "text part only"
    // fast path: some servers (iCloud) return structures our IMAP parser rejects,
    // which would fail the fetch and corrupt the session.
    let fetches: Vec<Fetch> = fetch_uids(session, uid.to_string(), "(BODY.PEEK[])")
        .await?
        .try_collect()
        .await?;
    // The whole message is in hand, so the sender check rides along for free
    // rather than costing a second fetch.
    let raw = fetches.iter().find_map(|f| f.body());
    // The paperclip is guessed from BODYSTRUCTURE (or, on servers whose structure
    // we can't parse, from the top-level Content-Type), and both guesses miss
    // shapes like Apple Mail's inline PDF nested under an alternative (issue #9).
    // The whole message is in hand here, so the guess can be replaced with fact
    // — at no extra network cost.
    Ok(raw
        .map(render_raw)
        .unwrap_or_else(|| ("(empty message)".to_string(), Default::default(), false)))
}

/// Fetch several messages' bodies from one folder in a single `uid_fetch`.
///
/// The per-message extraction is [`load_body`]'s, applied to each response in
/// the set: body, sender check and the attachment fact all come off the raw
/// message that is already in hand, at no extra network cost. A UID the server
/// doesn't answer is simply absent from the map — the caller decides what to
/// show in its place.
async fn load_bodies(
    session: &mut ImapSession,
    path: &str,
    uids: &[u32],
) -> Result<
    std::collections::HashMap<u32, (String, crate::models::SenderCheck, bool)>,
    async_imap::error::Error,
> {
    sel(session, path).await?;
    let set = uid_set(uids);
    let fetches: Vec<Fetch> = fetch_uids(session, set, "(BODY.PEEK[])")
        .await?
        .try_collect()
        .await?;
    let mut out = std::collections::HashMap::new();
    let mut last_uid: Option<u32> = None;
    for f in &fetches {
        if let Some(u) = f.uid {
            last_uid = Some(u);
        }
        // A message's response can arrive split across items — [`load_body`]
        // scans every one of them for the body rather than trusting the first,
        // and this has to be as forgiving. An item with no body is not an empty
        // message, just not the part carrying one, so it is skipped: leaving the
        // UID out of the map means the caller shows "(empty message)" without
        // writing that over a real body in the cache.
        let (Some(uid), Some(raw)) = (last_uid, f.body()) else {
            continue;
        };
        out.entry(uid).or_insert_with(|| render_raw(raw));
    }
    Ok(out)
}

/// Fetch BODYSTRUCTURE and return the IMAP section number of the preferred text
/// part (HTML over plain). `None` for non-multipart messages (just fetch whole).
#[allow(dead_code)]
async fn body_section(
    session: &mut ImapSession,
    uid: u32,
) -> Result<Option<String>, async_imap::error::Error> {
    let fetches: Vec<Fetch> = fetch_uids(session, uid.to_string(), "BODYSTRUCTURE")
        .await?
        .try_collect()
        .await?;
    Ok(fetches
        .iter()
        .find_map(|f| f.bodystructure())
        .and_then(find_text_section))
}

/// Fetch a single MIME part's headers + body and decode it into display HTML.
#[allow(dead_code)]
async fn fetch_part_body(
    session: &mut ImapSession,
    uid: u32,
    section: &str,
) -> Result<Option<String>, async_imap::error::Error> {
    let mime: Vec<Fetch> = fetch_uids(session, uid.to_string(), format!("(BODY.PEEK[{section}.MIME])"))
        .await?
        .try_collect()
        .await?;
    let headers = mime.iter().find_map(|f| f.body()).map(|b| b.to_vec());

    let part: Vec<Fetch> = fetch_uids(session, uid.to_string(), format!("(BODY.PEEK[{section}])"))
        .await?
        .try_collect()
        .await?;
    let body = part.iter().find_map(|f| f.body()).map(|b| b.to_vec());

    match (headers, body) {
        (Some(mut msg), Some(b)) => {
            // Reassemble "headers\r\n\r\nbody" so mail-parser decodes it (charset,
            // quoted-printable / base64) using the part's own MIME headers. Trim
            // any trailing newlines first so there's exactly one blank-line
            // separator whether or not the server already included it.
            while msg.ends_with(b"\r\n") {
                msg.truncate(msg.len() - 2);
            }
            while msg.ends_with(b"\n") {
                msg.truncate(msg.len() - 1);
            }
            msg.extend_from_slice(b"\r\n\r\n");
            msg.extend_from_slice(&b);
            Ok(Some(extract_body(&msg)))
        }
        _ => Ok(None),
    }
}

/// Find the IMAP section number of the best text part in a BODYSTRUCTURE.
/// Returns `None` for non-multipart messages (small — fetch the whole thing).
#[allow(dead_code)]
fn find_text_section(bs: &async_imap::imap_proto::types::BodyStructure) -> Option<String> {
    use async_imap::imap_proto::types::BodyStructure as Bs;

    fn walk(
        bs: &Bs,
        prefix: &str,
        html: &mut Option<String>,
        plain: &mut Option<String>,
    ) {
        match bs {
            Bs::Multipart { bodies, .. } => {
                for (i, child) in bodies.iter().enumerate() {
                    let section = if prefix.is_empty() {
                        format!("{}", i + 1)
                    } else {
                        format!("{prefix}.{}", i + 1)
                    };
                    walk(child, &section, html, plain);
                }
            }
            Bs::Text { common, .. } => {
                let is_attachment = common
                    .disposition
                    .as_ref()
                    .is_some_and(|d| d.ty.eq_ignore_ascii_case("attachment"));
                if !is_attachment {
                    if common.ty.subtype.eq_ignore_ascii_case("html") && html.is_none() {
                        *html = Some(prefix.to_string());
                    } else if common.ty.subtype.eq_ignore_ascii_case("plain") && plain.is_none() {
                        *plain = Some(prefix.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    if !matches!(bs, Bs::Multipart { .. }) {
        return None;
    }
    let (mut html, mut plain) = (None, None);
    walk(bs, "", &mut html, &mut plain);
    html.or(plain)
}

fn build_summary(account_id: u32, fetch: &Fetch, folder_id: u32) -> Message {
    let uid = fetch.uid.unwrap_or(0);
    let flags: Vec<Flag> = fetch.flags().collect();
    let unread = !flags.iter().any(|f| matches!(f, Flag::Seen));
    let starred = flags.iter().any(|f| matches!(f, Flag::Flagged));
    let keywords = custom_flags(&flags);

    let env = fetch.envelope();
    let (from_name, from_addr) = env
        .and_then(|e| e.from.as_ref())
        .and_then(|v| v.first())
        .map(address_parts)
        .unwrap_or_else(|| ("Unknown".to_string(), String::new()));
    let subject = env
        .and_then(|e| e.subject.as_deref())
        .map(decode_header)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(no subject)".to_string());
    let (date, timestamp) = env
        .and_then(|e| e.date.as_deref())
        .map(format_date)
        .filter(|(_, ts)| *ts > 0)
        .unwrap_or_else(|| internal_date_summary(fetch));
    let to = address_list(env.and_then(|e| e.to.as_ref()));
    let cc = address_list(env.and_then(|e| e.cc.as_ref()));
    // Where the sender asked replies to go. Servers echo From here when the
    // header is absent; storing that echo would be noise, so keep it only
    // when it actually differs.
    let reply_to = address_list(env.and_then(|e| e.reply_to.as_ref()));
    let reply_to = if reply_to.eq_ignore_ascii_case(&from_addr) { String::new() } else { reply_to };

    let has_attachment = fetch
        .bodystructure()
        .map(structure_has_attachment)
        .unwrap_or(false);

    // Message-ID + References + In-Reply-To drive accurate threading. References
    // isn't in the ENVELOPE, so it rides along as its own header fetch
    // ([`REFS_FETCH_ITEM`]) — without it only the immediate parent is known, and
    // threads break wherever that parent lives in another folder.
    let message_id = env
        .and_then(|e| e.message_id.as_deref())
        .map(normalize_msgid)
        .unwrap_or_default();
    let in_reply_to = env
        .and_then(|e| e.in_reply_to.as_deref())
        .map(normalize_msgids)
        .unwrap_or_default();
    let references = merge_msgids(&references_of(fetch), &in_reply_to);

    let mut msg = Message {
        id: uid,
        account_id,
        folder_id,
        uid,
        from_name,
        from_addr,
        reply_to,
        to,
        cc,
        subject,
        preview: String::new(),
        body: String::new(),
        date,
        timestamp,
        unread,
        starred,
        keywords,
        has_attachment,
        message_id,
        references,
    };
    msg.scrub_nuls();
    msg
}

/// A UID list as an IMAP sequence set: sorted, without repeats, and with
/// runs collapsed to ranges (`3,4,5,9` → `3:5,9`). Every command that takes
/// a set goes through here: iCloud (and Exchange) answer an unsorted set —
/// which a selection taken in list order, newest first, always was — with
/// "Parse Error", so a bulk delete or move there failed outright.
fn uid_set(uids: &[u32]) -> String {
    let mut sorted: Vec<u32> = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out = String::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        if !out.is_empty() {
            out.push(',');
        }
        if end > start {
            out.push_str(&format!("{start}:{end}"));
        } else {
            out.push_str(&start.to_string());
        }
        i += 1;
    }
    out
}

/// The user-defined keywords among a message's flags, verbatim.
fn custom_flags(flags: &[Flag]) -> Vec<String> {
    flags
        .iter()
        .filter_map(|f| match f {
            Flag::Custom(k) => Some(k.to_string()),
            _ => None,
        })
        .collect()
}

/// Normalize a single Message-ID: strip angle brackets/whitespace, lowercase.
fn normalize_msgid(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    s.trim().trim_start_matches('<').trim_end_matches('>').trim().to_ascii_lowercase()
}

/// Normalize a whitespace-separated list of Message-IDs into a canonical string.
fn normalize_msgids(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    s.split_whitespace()
        .map(|tok| tok.trim_start_matches('<').trim_end_matches('>').trim().to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The `References:` value from a fetch that asked for [`REFS_FETCH_ITEM`],
/// normalized into the same space-separated form as the ENVELOPE ids.
///
/// What comes back is a small header block — the one requested header, folded
/// across lines like any other, terminated by a blank line — or nothing at all
/// when the message has no References.
fn references_of(fetch: &Fetch) -> String {
    use async_imap::imap_proto::types::{MessageSection, SectionPath};
    let Some(raw) = fetch.section(&SectionPath::Full(MessageSection::Header)) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(raw);
    let Some((name, value)) = text.split_once(':') else {
        return String::new();
    };
    if !name.trim().eq_ignore_ascii_case("references") {
        return String::new();
    }
    normalize_msgids(value.as_bytes())
}

/// The `Message-ID:` value from a fetch that asked for
/// `HEADER.FIELDS (MESSAGE-ID)`, normalized like the ENVELOPE ids so it
/// compares equal to the ids the app stores.
fn message_id_of(fetch: &Fetch) -> String {
    use async_imap::imap_proto::types::{MessageSection, SectionPath};
    let Some(raw) = fetch.section(&SectionPath::Full(MessageSection::Header)) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(raw);
    let Some((name, value)) = text.split_once(':') else {
        return String::new();
    };
    if !name.trim().eq_ignore_ascii_case("message-id") {
        return String::new();
    }
    normalize_msgid(value.as_bytes())
}

/// Combine two normalized Message-ID lists, keeping the first list's order and
/// dropping duplicates. References already contains In-Reply-To on a well-formed
/// message; on a malformed one either may hold the only usable link.
fn merge_msgids(a: &str, b: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for id in a.split_whitespace().chain(b.split_whitespace()) {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out.join(" ")
}

/// Whether an IMAP BODYSTRUCTURE contains a part marked as an attachment.
///
/// `Content-Disposition: attachment` is the obvious case, but Apple Mail sends
/// iPhone photos as *inline* parts of a multipart/mixed, so disposition alone
/// misses them. Any non-text part therefore counts, except a small one carrying a
/// Content-ID — see [`INLINE_ATTACHMENT_MIN`].
fn structure_has_attachment(bs: &async_imap::imap_proto::types::BodyStructure) -> bool {
    use async_imap::imap_proto::types::{BodyStructure as Bs, ContentEncoding};

    let is_attachment = |common: &async_imap::imap_proto::types::BodyContentCommon| {
        common
            .disposition
            .as_ref()
            .is_some_and(|d| d.ty.eq_ignore_ascii_case("attachment"))
    };
    // BODYSTRUCTURE reports the *encoded* size; base64 inflates by 4/3.
    let decoded_size = |other: &async_imap::imap_proto::types::BodyContentSinglePart| {
        let octets = other.octets as usize;
        match other.transfer_encoding {
            ContentEncoding::Base64 => octets / 4 * 3,
            _ => octets,
        }
    };

    match bs {
        Bs::Multipart { bodies, .. } => bodies.iter().any(structure_has_attachment),
        Bs::Text { common, .. } => is_attachment(common),
        Bs::Basic { common, other, .. } | Bs::Message { common, other, .. } => {
            is_attachment(common)
                || other.id.is_none()
                || decoded_size(other) >= INLINE_ATTACHMENT_MIN
        }
    }
}

/// Every attachment in a BODYSTRUCTURE, with what it is called, how big it is
/// and which IMAP section holds it — all without downloading a byte of it. This
/// is what lets the gallery reach back through an archive: the server describes
/// the files, and only opening one actually fetches it.
///
/// What counts as an attachment is [`structure_has_attachment`]'s rule, kept
/// deliberately identical so a message the list gives a paperclip to is a
/// message the gallery lists parts for.
fn structure_attachments(
    bs: &async_imap::imap_proto::types::BodyStructure,
) -> Vec<crate::models::AttachmentMeta> {
    use async_imap::imap_proto::types::{BodyStructure as Bs, ContentEncoding};

    /// A parameter's value, matched case-insensitively as RFC 2045 requires.
    fn param(params: &async_imap::imap_proto::types::BodyParams, key: &str) -> Option<String> {
        params.as_ref()?.iter().find_map(|(k, v)| {
            k.eq_ignore_ascii_case(key).then(|| decode_header(v.as_bytes()))
        })
    }

    fn walk(
        bs: &Bs,
        section: &str,
        out: &mut Vec<crate::models::AttachmentMeta>,
    ) {
        match bs {
            Bs::Multipart { bodies, .. } => {
                for (i, child) in bodies.iter().enumerate() {
                    let child_section = if section.is_empty() {
                        format!("{}", i + 1)
                    } else {
                        format!("{section}.{}", i + 1)
                    };
                    walk(child, &child_section, out);
                }
            }
            Bs::Text { common, other, .. }
            | Bs::Basic { common, other, .. }
            | Bs::Message { common, other, .. } => {
                let declared = common
                    .disposition
                    .as_ref()
                    .is_some_and(|d| d.ty.eq_ignore_ascii_case("attachment"));
                // BODYSTRUCTURE reports the encoded size; base64 inflates by 4/3.
                let size = match other.transfer_encoding {
                    ContentEncoding::Base64 => other.octets as u64 / 4 * 3,
                    _ => other.octets as u64,
                };
                let is_text = matches!(bs, Bs::Text { .. });
                // A text part is only ever an attachment when it says so; for
                // the rest, an inline part with a Content-ID is decoration
                // unless it is big enough to be content (see the constant).
                let counts = if is_text {
                    declared
                } else {
                    declared || other.id.is_none() || size as usize >= INLINE_ATTACHMENT_MIN
                };
                if !counts {
                    return;
                }
                let name = common
                    .disposition
                    .as_ref()
                    .and_then(|d| param(&d.params, "filename"))
                    .or_else(|| param(&common.ty.params, "name"))
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| {
                        // Unnamed, as Apple Mail's inline photos arrive: give it
                        // the extension its type implies so the gallery can sort
                        // and filter it like any other file.
                        let ext = mime_extension(&common.ty.ty, &common.ty.subtype);
                        format!("attachment-{}{ext}", out.len() + 1)
                    });
                out.push(crate::models::AttachmentMeta {
                    idx: out.len() as u32,
                    name,
                    mime: format!(
                        "{}/{}",
                        common.ty.ty.to_lowercase(),
                        common.ty.subtype.to_lowercase()
                    ),
                    size,
                    section: section.to_string(),
                });
            }
        }
    }

    let mut out = Vec::new();
    // A message that is not multipart is one part, section 1.
    walk(bs, if matches!(bs, Bs::Multipart { .. }) { "" } else { "1" }, &mut out);
    out
}

/// A filename extension for a MIME type, for parts that arrive unnamed. Only
/// the types that actually turn up without a filename are worth listing; the
/// rest get none, and show as a generic file.
fn mime_extension(ty: &str, subtype: &str) -> &'static str {
    if !ty.eq_ignore_ascii_case("image") {
        return "";
    }
    match subtype.to_ascii_lowercase().as_str() {
        "jpeg" | "jpg" => ".jpg",
        "png" => ".png",
        "gif" => ".gif",
        "heic" => ".heic",
        "heif" => ".heif",
        "webp" => ".webp",
        "tiff" => ".tif",
        _ => "",
    }
}

// ---------------------------------------------------------------------------
// POP3 path
// ---------------------------------------------------------------------------

/// Cap on how many messages a POP3 sync downloads in full (POP3 has no partial
/// fetch; mailboxes are usually small). Older messages aren't indexed.
const POP3_LIMIT: usize = 200;

/// A minimal async POP3 client over TLS (implicit on 995, STLS otherwise).
/// The POP3 conversation for the console, like the IMAP one: the command
/// (a PASS line with its secret dropped) and the server's first reply line.
fn pop3_wire(cmd: &str) {
    let shown = if cmd.starts_with("PASS ") { "PASS ****" } else { cmd };
    tracing::debug!(target: "vireo::pop3", "> {shown}");
}

fn pop3_wired(cmd: &str, reply: &[u8]) {
    let line = String::from_utf8_lossy(reply);
    let line = line.trim();
    let shown = if cmd.starts_with("PASS ") { "PASS" } else { cmd };
    if reply.starts_with(b"+OK") {
        tracing::debug!(target: "vireo::pop3", "< {} ({shown})", line.chars().take(80).collect::<String>());
    } else {
        tracing::warn!(target: "vireo::pop3", "< {line} ({shown})");
    }
}

struct Pop3 {
    stream: tokio::io::BufReader<async_native_tls::TlsStream<TcpStream>>,
}

impl Pop3 {
    async fn connect(account: &AccountConfig) -> Result<Self, String> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let host = account.imap_host.as_str();
        let port = account.imap_port;
        let tcp = TcpStream::connect((host, port))
            .await
            .map_err(|e| e.to_string())?;
        let tls = tls_connector(host);

        let stream = if port == 995 {
            tls.connect(host, tcp).await.map_err(|e| e.to_string())?
        } else {
            // STARTTLS: greet + STLS on the plaintext socket, then upgrade.
            let mut plain = BufReader::new(tcp);
            let mut line = Vec::new();
            plain.read_until(b'\n', &mut line).await.map_err(|e| e.to_string())?;
            plain.write_all(b"STLS\r\n").await.map_err(|e| e.to_string())?;
            plain.flush().await.map_err(|e| e.to_string())?;
            line.clear();
            plain.read_until(b'\n', &mut line).await.map_err(|e| e.to_string())?;
            if !line.starts_with(b"+OK") {
                return Err("server refused STLS".to_string());
            }
            tls.connect(host, plain.into_inner())
                .await
                .map_err(|e| e.to_string())?
        };

        let mut pop = Pop3 { stream: BufReader::new(stream) };
        if port == 995 {
            pop.read_reply().await?; // greeting
        }
        Ok(pop)
    }

    async fn read_reply(&mut self) -> Result<Vec<u8>, String> {
        use tokio::io::AsyncBufReadExt;
        let mut line = Vec::new();
        self.stream
            .read_until(b'\n', &mut line)
            .await
            .map_err(|e| e.to_string())?;
        if line.is_empty() {
            return Err("connection closed".to_string());
        }
        Ok(line)
    }

    async fn send(&mut self, cmd: &str) -> Result<(), String> {
        use tokio::io::AsyncWriteExt;
        self.stream.write_all(cmd.as_bytes()).await.map_err(|e| e.to_string())?;
        self.stream.write_all(b"\r\n").await.map_err(|e| e.to_string())?;
        self.stream.flush().await.map_err(|e| e.to_string())
    }

    /// Single-line command: returns Err with the server text on `-ERR`.
    async fn command(&mut self, cmd: &str) -> Result<(), String> {
        pop3_wire(cmd);
        self.send(cmd).await?;
        let reply = self.read_reply().await?;
        pop3_wired(cmd, &reply);
        if reply.starts_with(b"+OK") {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&reply).trim().to_string())
        }
    }

    /// Multi-line command: returns the dot-unstuffed body bytes after `+OK`.
    async fn multiline(&mut self, cmd: &str) -> Result<Vec<u8>, String> {
        pop3_wire(cmd);
        self.send(cmd).await?;
        let first = self.read_reply().await?;
        pop3_wired(cmd, &first);
        if !first.starts_with(b"+OK") {
            return Err(String::from_utf8_lossy(&first).trim().to_string());
        }
        let mut out = Vec::new();
        loop {
            let line = self.read_reply().await?;
            let trimmed = strip_crlf(&line);
            if trimmed == b"." {
                break;
            }
            // Dot-stuffing: a leading '.' is doubled on the wire.
            let content = if trimmed.starts_with(b"..") { &trimmed[1..] } else { trimmed };
            out.extend_from_slice(content);
            out.extend_from_slice(b"\r\n");
        }
        Ok(out)
    }

    async fn login(&mut self, user: &str, pass: &str) -> Result<(), String> {
        self.command(&format!("USER {user}")).await?;
        self.command(&format!("PASS {pass}")).await
    }

    /// Returns (message number, server UID) pairs.
    async fn uidl(&mut self) -> Result<Vec<(u32, String)>, String> {
        let body = self.multiline("UIDL").await?;
        let text = String::from_utf8_lossy(&body);
        let mut out = Vec::new();
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            if let (Some(n), Some(uid)) = (parts.next(), parts.next()) {
                if let Ok(num) = n.parse::<u32>() {
                    out.push((num, uid.to_string()));
                }
            }
        }
        Ok(out)
    }

    async fn retr(&mut self, num: u32) -> Result<Vec<u8>, String> {
        self.multiline(&format!("RETR {num}")).await
    }

    async fn quit(&mut self) {
        let _ = self.command("QUIT").await;
    }
}

fn strip_crlf(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && (line[end - 1] == b'\n' || line[end - 1] == b'\r') {
        end -= 1;
    }
    &line[..end]
}

/// Stable u32 id derived from a POP3 server UID string (which is a string, but
/// the rest of the app keys messages by u32).
fn hash_uid(uid: &str) -> u32 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    uid.hash(&mut h);
    (h.finish() & 0x7fff_ffff) as u32
}

/// Format a unix timestamp the same way [`format_date`] labels mail dates.
fn label_from_timestamp(ts: i64) -> String {
    let now = crate::datefmt::now();
    if crate::datefmt::day_key(ts) == crate::datefmt::day_key(now) {
        crate::datefmt::time(ts)
    } else if crate::datefmt::year(ts) == crate::datefmt::year(now) {
        crate::datefmt::day_month(ts)
    } else {
        crate::datefmt::day_month_year(ts)
    }
}

/// Build a message summary from a full RFC 822 message (POP3 has no ENVELOPE).
fn summary_from_raw(account_id: u32, folder_id: u32, uid: u32, raw: &[u8]) -> Message {
    use mail_parser::MessageParser;
    let parsed = MessageParser::default().parse(raw);
    let addr = parsed
        .as_ref()
        .and_then(|p| p.from())
        .and_then(|a| a.first());
    let from_addr = addr
        .and_then(|a| a.address())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let from_name = addr
        .and_then(|a| a.name())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            if from_addr.is_empty() {
                "Unknown".to_string()
            } else {
                from_addr.clone()
            }
        });
    let subject = parsed
        .as_ref()
        .and_then(|p| p.subject())
        .filter(|s| !s.is_empty())
        .unwrap_or("(no subject)")
        .to_string();
    let timestamp = parsed
        .as_ref()
        .and_then(|p| p.date())
        .map(|d| d.to_timestamp())
        .unwrap_or(0);
    let date = label_from_timestamp(timestamp);
    let addr_list = |list: Option<&mail_parser::Address>| -> String {
        list.map(|a| {
            a.iter()
                .filter_map(|x| x.address())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
    };
    let to = addr_list(parsed.as_ref().and_then(|p| p.to()));
    let cc = addr_list(parsed.as_ref().and_then(|p| p.cc()));
    let reply_to = addr_list(parsed.as_ref().and_then(|p| p.reply_to()));
    let reply_to = if reply_to.eq_ignore_ascii_case(&from_addr) { String::new() } else { reply_to };
    let has_attachment = parsed.as_ref().map(|p| p.attachment_count() > 0).unwrap_or(false);
    let (message_id, references) = mp_thread_ids(parsed.as_ref());

    let mut msg = Message {
        id: uid,
        account_id,
        folder_id,
        uid,
        from_name,
        from_addr,
        reply_to,
        to,
        cc,
        subject,
        preview: String::new(),
        body: String::new(),
        date,
        timestamp,
        unread: true,
        starred: false,
        keywords: Vec::new(),
        has_attachment,
        message_id,
        references,
    };
    msg.scrub_nuls();
    msg
}

/// The folder list for a POP3 account: just the inbox.
fn pop3_folders(account_id: u32) -> Vec<Folder> {
    vec![Folder {
        id: 1,
        account_id,
        name: "Inbox".to_string(),
        path: "INBOX".to_string(),
        kind: FolderKind::Inbox,
        unread: 0,
    }]
}

async fn run_pop3(
    account_id: u32,
    mut account: AccountConfig,
    mut rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent),
) {
    // Resolve credentials from the keyring (same as the IMAP path).
    if account.password.is_empty() {
        if let Some(pw) = crate::config::load_password(&account.email) {
            account.password = pw;
        }
    } else {
        let _ = crate::config::store_password(&account.email, &account.password);
        crate::config::strip_passwords_on_disk();
    }
    if account.smtp_separate && account.smtp_password.is_empty() {
        if let Some(pw) = crate::config::load_smtp_password(&account.email) {
            account.smtp_password = pw;
        }
    }

    let cache = Cache::open().map_err(|e| tracing::warn!("cache unavailable: {e}")).ok();
    const INBOX: &str = "INBOX";
    let inbox_id = 1u32;

    emit(WorkerEvent::Account(Account {
        id: account_id,
        name: account.name.clone(),
        email: account.email.clone(),
        label: account.display_label(),
        accent: accent_for(account_id).into(),
    }));
    let folders = pop3_folders(account_id);
    if let Some(c) = cache.as_ref() {
        c.save_folders(account_id, &folders);
    }
    emit(WorkerEvent::Folders(folders));

    while let Some(req) = rx.recv().await {
        match req {
            MailRequest::Locate { message_id } => {
                // No folders to search here beyond what the cache already
                // held: answer "not found" so the app can report the miss.
                emit(WorkerEvent::Located { message_id: message_id.clone(), hit: None });
            }
            // POP3 downloads every message in full, so an attachment is either
            // already in the cache or does not exist: there is nothing a scan
            // could add.
            MailRequest::ScanAttachments { folder_path } => {
                emit(WorkerEvent::AttachmentsScanned { folder_path, added: 0, remaining: 0 });
            }
            // POP3 has no server-side keywords: nothing to find.
            MailRequest::FindKeywords => emit(WorkerEvent::KeywordsFound(Vec::new())),
            MailRequest::RefreshKeywords { .. } => emit(WorkerEvent::KeywordsSynced { paths: Vec::new() }),
            // POP3 keeps everything in the inbox, so a conversation never spans
            // folders and the reader already has all of it.
            MailRequest::LoadRelated { message_id, .. } => {
                emit(WorkerEvent::Related { message_id, messages: Vec::new() });
            }
            MailRequest::LoadMessages { folder_id, path }
            | MailRequest::SyncFolder { folder_id, path } => {
                if path != INBOX {
                    emit(WorkerEvent::Messages { folder_id, messages: Vec::new() });
                    emit(WorkerEvent::BackfillDone { folder_id });
                    continue;
                }
                // Serve cache first for instant display.
                if let Some(c) = cache.as_ref() {
                    let cached = c.load_messages(account_id, INBOX, inbox_id);
                    if !cached.is_empty() {
                        emit(WorkerEvent::Messages { folder_id, messages: cached });
                    }
                }
                emit(WorkerEvent::Status(i18n("Syncing…")));
                match pop3_sync(account_id, &account, inbox_id, cache.as_ref()).await {
                    Ok(messages) => {
                        let unread = messages.iter().filter(|m| m.unread).count() as u32;
                        emit(WorkerEvent::Messages { folder_id, messages });
                        emit(WorkerEvent::FolderUnread { folder_id: inbox_id, unread });
                        // POP3 syncs the whole mailbox in one pass; no backfill
                        // follows, so the index is complete (see the Graph path).
                        emit(WorkerEvent::BackfillDone { folder_id });
                    }
                    Err(e) => {
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not fetch mail: {e}", &[("e", &(e).to_string())]),
                            connectivity: true,
                        });
                        emit(WorkerEvent::BackfillDone { folder_id });
                    }
                }
                emit(WorkerEvent::Status(String::new()));
            }

            MailRequest::LoadBody { message_id, path: _, uid } => {
                if let Some(body) = cache.as_ref().and_then(|c| c.load_body(account_id, INBOX, uid)) {
                    emit(WorkerEvent::Body { message_id, path: INBOX.to_string(), body });
                    if let Some(check) = cache.as_ref().and_then(|c| c.load_sender_check(account_id, INBOX, uid)) {
                        emit(WorkerEvent::SenderChecked { message_id, check });
                    }
                    continue;
                }
                match pop3_fetch_raw(&account, uid).await {
                    Ok(raw) => {
                        let (body, check, _) = render_raw(&raw);
                        if let Some(c) = cache.as_ref() {
                            if body_cacheable(&check) {
                                c.save_body(account_id, INBOX, uid, &body);
                            }
                            c.save_sender_check(account_id, INBOX, uid, &check);
                        }
                        emit(WorkerEvent::Body { message_id, path: INBOX.to_string(), body });
                        emit(WorkerEvent::SenderChecked { message_id, check });
                    }
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Could not load message: {e}", &[("e", &(e).to_string())]),
                        connectivity: true,
                    }),
                }
            }

            // POP3 has no set fetch — RETR is one message at a time — so a batch
            // is the same work in a loop, saving only the per-message request hop.
            MailRequest::LoadBodies { items, path: _ } => {
                for (message_id, uid) in items {
                    if let Some(body) =
                        cache.as_ref().and_then(|c| c.load_body(account_id, INBOX, uid))
                    {
                        emit(WorkerEvent::Body { message_id, path: INBOX.to_string(), body });
                        if let Some(check) = cache.as_ref().and_then(|c| c.load_sender_check(account_id, INBOX, uid)) {
                            emit(WorkerEvent::SenderChecked { message_id, check });
                        }
                        continue;
                    }
                    match pop3_fetch_raw(&account, uid).await {
                        Ok(raw) => {
                            let (body, check, _) = render_raw(&raw);
                            if let Some(c) = cache.as_ref() {
                                if body_cacheable(&check) {
                                    c.save_body(account_id, INBOX, uid, &body);
                                }
                                c.save_sender_check(account_id, INBOX, uid, &check);
                            }
                            emit(WorkerEvent::Body { message_id, path: INBOX.to_string(), body });
                            emit(WorkerEvent::SenderChecked { message_id, check });
                        }
                        Err(e) => emit(WorkerEvent::Error {
                            text: i18n_f("Could not load message: {e}", &[("e", &(e).to_string())]),
                            connectivity: true,
                        }),
                    }
                }
            }

            MailRequest::LoadSource { message_id: _, path: _, uid } => {
                match pop3_fetch_raw(&account, uid).await {
                    Ok(raw) => emit(WorkerEvent::Source {
                        text: String::from_utf8_lossy(&raw).into_owned(),
                    }),
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Could not load source: {e}", &[("e", &(e).to_string())]),
                        connectivity: true,
                    }),
                }
            }

            MailRequest::LoadAttachments { message_id, path: _, uid, download } => {
                if let Some(c) = cache.as_ref() {
                    let items = c.load_attachments(account_id, INBOX, uid);
                    if !items.is_empty() {
                        emit(WorkerEvent::Attachments { message_id, items });
                        continue;
                    }
                }
                if !download {
                    emit(WorkerEvent::AttachmentsPending { message_id });
                    continue;
                }
                match pop3_fetch_raw(&account, uid).await {
                    Ok(raw) => {
                        let items = extract_attachments(&raw);
                        if let Some(c) = cache.as_ref() {
                            c.save_attachments(account_id, INBOX, uid, &items);
                        }
                        emit(WorkerEvent::Attachments { message_id, items });
                    }
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Could not load attachments: {e}", &[("e", &(e).to_string())]),
                        connectivity: true,
                    }),
                }
            }

            MailRequest::SetSeen { uid, seen, .. } => {
                if let Some(c) = cache.as_ref() {
                    c.set_unread(account_id, INBOX, uid, !seen);
                }
                emit(WorkerEvent::SeenSettled { path: INBOX.to_string(), uid });
            }
            MailRequest::SetFlagged { uid, flagged, .. } => {
                if let Some(c) = cache.as_ref() {
                    c.set_starred(account_id, INBOX, uid, flagged);
                }
            }
            // POP3 has no flags at all: every tag is Vireo's own.
            MailRequest::SetKeyword { uid, message_id, keyword, add, .. } => {
                if let Some(c) = cache.as_ref() {
                    if message_id.is_empty() {
                        c.set_keyword(account_id, INBOX, uid, &keyword, add);
                    } else {
                        c.set_local_tag(account_id, &message_id, &keyword, add);
                    }
                }
            }
            MailRequest::MarkAllRead { folder_id, .. } => {
                if let Some(c) = cache.as_ref() {
                    c.mark_folder_read(account_id, INBOX);
                }
                emit(WorkerEvent::FolderUnread { folder_id, unread: 0 });
            }

            // POP3 has no folders to move between: deleting removes from the
            // server. (Archive/spam have no destination folder, so the UI never
            // reaches here for those.)
            // Not-spam has nothing to do here either (no Junk folder to leave).
            MailRequest::MarkHam { .. } => {}
            MailRequest::MarkHamMany { .. } => emit(WorkerEvent::BulkComplete),
            MailRequest::MoveMessage { uid, .. } | MailRequest::MarkSpam { uid, .. } => {
                match pop3_delete(&account, uid).await {
                    Ok(()) => {
                        if let Some(c) = cache.as_ref() {
                            c.delete_message(account_id, INBOX, uid);
                        }
                    }
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Could not delete message: {e}", &[("e", &(e).to_string())]),
                        connectivity: false,
                    }),
                }
            }
            // POP3 shows only its Inbox; there is no Trash or Junk to empty.
            MailRequest::EmptyFolder { .. } => {}
            MailRequest::MoveMessages { uids, .. } | MailRequest::PurgeMessages { uids, .. } => {
                for uid in uids {
                    if pop3_delete(&account, uid).await.is_ok() {
                        if let Some(c) = cache.as_ref() {
                            c.delete_message(account_id, INBOX, uid);
                        }
                    }
                }
                emit(WorkerEvent::BulkComplete);
            }

            // POP3 has no folders beyond the inbox.
            MailRequest::CreateFolder { .. }
            | MailRequest::RenameFolder { .. }
            | MailRequest::UndoMove { .. }
            | MailRequest::DeleteFolder { .. }
            | MailRequest::SaveDraft { .. } => {
                emit(WorkerEvent::Error {
                    text: i18n("POP3 accounts don't support folders"),
                    connectivity: false,
                });
            }

            MailRequest::Send { message, .. } => match send_smtp(&account, &message).await {
                Ok(_) => {
                    if let (Some(queued), Some(c)) = (message.outbox_origin, cache.as_ref()) {
                        c.delete_outbox(queued);
                        emit_outbox(cache.as_ref(), account_id, &emit);
                    }
                    emit(WorkerEvent::Sent);
                }
                Err(e) => {
                    // POP3 has no Sent folder to copy to, but the message is held
                    // exactly as it is for IMAP accounts.
                    let queued = queue_failed_send(
                        cache.as_ref(),
                        account_id,
                        &account,
                        &message,
                        None,
                        &e.to_string(),
                    );
                    if let (true, Some(old), Some(c)) =
                        (queued, message.outbox_origin, cache.as_ref())
                    {
                        c.delete_outbox(old);
                    }
                    emit(WorkerEvent::Error {
                        text: if queued {
                            i18n_f("Send failed: {e}. The message is in the Outbox and will be sent when the connection is back.", &[("e", &e.to_string())])
                        } else {
                            i18n_f("Send failed: {e}", &[("e", &e.to_string())])
                        },
                        connectivity: false,
                    });
                    emit_outbox(cache.as_ref(), account_id, &emit);
                }
            },

            MailRequest::LoadOutbox => emit_outbox(cache.as_ref(), account_id, &emit),

            MailRequest::DeleteOutbox { id } => {
                if let Some(c) = cache.as_ref() {
                    c.delete_outbox(id);
                }
                emit_outbox(cache.as_ref(), account_id, &emit);
            }

            MailRequest::FlushOutbox { id } => {
                let mut no_session = None;
                flush_outbox(
                    cache.as_ref(),
                    account_id,
                    &account,
                    id,
                    &mut no_session,
                    &mut true,
                    &emit,
                    true,
                )
                .await;
            }

            // POP3 has only the inbox, whose count every sync refreshes.
            MailRequest::RefreshUnread => {}

            MailRequest::Reconnect => {
                emit(WorkerEvent::Folders(pop3_folders(account_id)));
            }
        }
    }
}

/// Connect, download summaries/bodies for new messages, return the inbox list
/// (newest first), merging server UIDs with the local cache (for read state).
async fn pop3_sync(
    account_id: u32,
    account: &AccountConfig,
    inbox_id: u32,
    cache: Option<&Cache>,
) -> Result<Vec<Message>, String> {
    const INBOX: &str = "INBOX";
    let mut pop = Pop3::connect(account).await?;
    pop.login(&account.username, &account.password).await?;
    let mut entries = pop.uidl().await?;
    // Newest first; bound how many we index.
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    entries.truncate(POP3_LIMIT);

    let cached: std::collections::HashMap<u32, Message> = cache
        .map(|c| c.load_messages(account_id, INBOX, inbox_id))
        .unwrap_or_default()
        .into_iter()
        .map(|m| (m.uid, m))
        .collect();

    let mut messages = Vec::with_capacity(entries.len());
    for (num, uid_str) in &entries {
        let uid = hash_uid(uid_str);
        if let Some(existing) = cached.get(&uid) {
            messages.push(existing.clone()); // keep read/star state; already downloaded
            continue;
        }
        // New message: download in full, cache its body + attachments.
        let raw = pop.retr(*num).await?;
        let msg = summary_from_raw(account_id, inbox_id, uid, &raw);
        // OpenPGP mail (#133) is neither rendered nor cached here: it is
        // decrypted or verified, in the reader only, when opened.
        if let Some(c) = cache.filter(|_| crate::pgp::detect(&raw).is_none()) {
            let (body, check, _) = render_raw(&raw);
            c.save_body(account_id, INBOX, uid, &body);
            c.save_sender_check(account_id, INBOX, uid, &check);
            let (items, _) = attachments_of(&raw);
            if !items.is_empty() {
                c.save_attachments(account_id, INBOX, uid, &items);
            }
        }
        messages.push(msg);
    }
    pop.quit().await;

    messages.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    if let Some(c) = cache {
        c.save_messages(account_id, INBOX, &messages);
    }
    Ok(messages)
}

/// Fetch one message's raw bytes by its hashed UID (reconnects + maps UID→num).
async fn pop3_fetch_raw(account: &AccountConfig, uid: u32) -> Result<Vec<u8>, String> {
    let mut pop = Pop3::connect(account).await?;
    pop.login(&account.username, &account.password).await?;
    let num = pop
        .uidl()
        .await?
        .into_iter()
        .find(|(_, u)| hash_uid(u) == uid)
        .map(|(n, _)| n)
        .ok_or_else(|| "message no longer on server".to_string())?;
    let raw = pop.retr(num).await?;
    pop.quit().await;
    Ok(raw)
}

/// Delete a message from the POP3 server (DELE, committed on QUIT).
async fn pop3_delete(account: &AccountConfig, uid: u32) -> Result<(), String> {
    let mut pop = Pop3::connect(account).await?;
    pop.login(&account.username, &account.password).await?;
    let num = pop
        .uidl()
        .await?
        .into_iter()
        .find(|(_, u)| hash_uid(u) == uid)
        .map(|(n, _)| n);
    if let Some(num) = num {
        pop.command(&format!("DELE {num}")).await?;
    }
    pop.quit().await; // commits the deletion
    Ok(())
}

// ---------------------------------------------------------------------------
// Mock path (offline fallback)
// ---------------------------------------------------------------------------

/// The files a demo message "carries": a 1x1 PNG and a short text note.
fn demo_attachment_files() -> Vec<crate::models::Attachment> {
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    vec![
        crate::models::Attachment { name: "palette-hover.png".into(), data: PNG_1X1.to_vec() },
        crate::models::Attachment {
            name: "notes.txt".into(),
            data: b"Hover palette: reserved-space fade vs. layout shift.\n".to_vec(),
        },
    ]
}

async fn run_mock(
    account_id: u32,
    mut rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent) + Clone + 'static,
) {
    let backend = MockBackend::new();

    if let Some(account) = backend.accounts().into_iter().find(|a| a.id == account_id) {
        emit(WorkerEvent::Account(account));
    }
    emit(WorkerEvent::Folders(backend.folders(account_id)));
    // VIREO_DEMO_ARRIVAL=<secs>: a reply lands in account 1's Inbox
    // conversation after that long, as a sync would bring it — for
    // watching an open conversation take in a new message.
    if account_id == 1 {
        if let Some(secs) =
            std::env::var("VIREO_DEMO_ARRIVAL").ok().and_then(|v| v.parse::<u64>().ok())
        {
            let emit = emit.clone();
            let mut messages = backend.messages(1);
            tokio::task::spawn_local(async move {
                tokio::time::sleep(Duration::from_secs(secs)).await;
                messages.insert(0, crate::backend::demo_arrival());
                emit(WorkerEvent::Messages { folder_id: 1, messages });
            });
        }
    }

    while let Some(req) = rx.recv().await {
        match req {
            MailRequest::Locate { message_id } => {
                // No folders to search here beyond what the cache already
                // held: answer "not found" so the app can report the miss.
                emit(WorkerEvent::Located { message_id: message_id.clone(), hit: None });
            }
            // Demo mode seeds its in-memory cache up front (see
            // `backend::seed_demo_cache`), so every attachment is already
            // described and there is nothing to ask a server about.
            MailRequest::ScanAttachments { folder_path } => {
                emit(WorkerEvent::AttachmentsScanned { folder_path, added: 0, remaining: 0 });
            }
            // What a scan of a lived-in mailbox turns up: Thunderbird's
            // built-ins and a few of the user's own, plus one the demo's tags
            // already know (dropped by the app before the report).
            MailRequest::FindKeywords => {
                let f = |keyword: &str, count: usize, folders: &[&str]| KeywordFinding {
                    keyword: keyword.to_string(),
                    name: None,
                    color: None,
                    count,
                    folders: folders.iter().map(|s| s.to_string()).collect(),
                };
                emit(WorkerEvent::KeywordsFound(vec![
                    f("$label1", 14, &["Inbox", "Archive"]),
                    f("$label4", 6, &["Inbox"]),
                    f("Receipts", 23, &["Inbox", "Archive"]),
                    f("travel_plans", 4, &["Inbox"]),
                    f("Work", 9, &["Inbox"]),
                ]));
            }
            MailRequest::LoadRelated { message_id, .. } => {
                emit(WorkerEvent::Related { message_id, messages: Vec::new() });
            }
            MailRequest::LoadMessages { folder_id, .. }
            | MailRequest::SyncFolder { folder_id, .. } => {
                emit(WorkerEvent::Messages {
                    folder_id,
                    messages: backend.messages(folder_id),
                });
                // VIREO_DEMO_SYNC_DELAY=<secs> holds the "been to the server"
                // signal back, so the progress a long sync shows (the manual
                // filter run's dialog, #198) can be watched here.
                match std::env::var("VIREO_DEMO_SYNC_DELAY")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                {
                    Some(secs) => {
                        let emit = emit.clone();
                        tokio::task::spawn_local(async move {
                            tokio::time::sleep(Duration::from_secs(secs)).await;
                            emit(WorkerEvent::FolderSynced { folder_id });
                        });
                    }
                    None => emit(WorkerEvent::FolderSynced { folder_id }),
                }
            }
            MailRequest::LoadBody { message_id, ref path, .. } => {
                let body = backend.message(message_id).map(|m| m.body).unwrap_or_default();
                emit(WorkerEvent::Body { message_id, path: path.clone(), body });
            }
            MailRequest::LoadBodies { ref items, ref path } => {
                for (message_id, _) in items {
                    let body = backend.message(*message_id).map(|m| m.body).unwrap_or_default();
                    emit(WorkerEvent::Body {
                        message_id: *message_id,
                        path: path.clone(),
                        body,
                    });
                }
            }
            MailRequest::LoadSource { message_id, .. } => {
                let text = backend.message(message_id).map(|m| m.body).unwrap_or_default();
                emit(WorkerEvent::Source { text });
            }
            MailRequest::LoadAttachments { message_id, .. } => {
                // A demo message flagged as carrying attachments gets two
                // small files, so the reader's per-card rows and the drawer
                // have something to show (#213).
                let items = if backend.message(message_id).is_some_and(|m| m.has_attachment) {
                    demo_attachment_files()
                } else {
                    Vec::new()
                };
                emit(WorkerEvent::Attachments { message_id, items });
            }
            // Mutations are no-ops offline; the UI updates optimistically.
            MailRequest::SetSeen { .. }
            | MailRequest::SetFlagged { .. }
            | MailRequest::SetKeyword { .. }
            | MailRequest::MarkAllRead { .. }
            | MailRequest::MarkSpam { .. }
            | MailRequest::MarkHam { .. }
            | MailRequest::MoveMessage { .. }
            | MailRequest::UndoMove { .. }
            | MailRequest::CreateFolder { .. }
            | MailRequest::RenameFolder { .. }
            | MailRequest::DeleteFolder { .. }
            | MailRequest::FlushOutbox { .. }
            | MailRequest::DeleteOutbox { .. }
            | MailRequest::RefreshUnread
            | MailRequest::RefreshKeywords { .. }
            | MailRequest::Reconnect => {}
            // The demo's folders are fixed, but an emptied one reads as empty.
            MailRequest::EmptyFolder { folder_id, .. } => {
                emit(WorkerEvent::Messages { folder_id, messages: Vec::new() });
                emit(WorkerEvent::FolderUnread { folder_id, unread: 0 });
            }
            // The demo backend sends nothing, so its Outbox is always empty.
            MailRequest::LoadOutbox => emit(WorkerEvent::Outbox { items: Vec::new() }),
            // Signal completion so the demo's bulk spinner clears.
            MailRequest::MoveMessages { .. } | MailRequest::MarkHamMany { .. } | MailRequest::PurgeMessages { .. } => {
                emit(WorkerEvent::BulkComplete)
            }
            MailRequest::SaveDraft { .. } => emit(WorkerEvent::DraftSaved),
            // Pretend the send succeeded so the compose flow is demoable offline.
            MailRequest::Send { .. } => emit(WorkerEvent::Sent),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Classify a folder, also reporting whether the kind came from an RFC 6154
/// SPECIAL-USE attribute (`true`) rather than name matching (`false`). This lets
/// `list_folders` prefer the real special-use folder when a server also exposes a
/// stray folder that merely *looks* like it fills the same role — e.g. Gmail's
/// real `[Gmail]/Trash` (\Trash) next to a plain top-level `Trash` label.
fn classify_with_source(path: &str, attrs: &[NameAttribute]) -> (FolderKind, bool) {
    // Prefer RFC 6154 SPECIAL-USE attributes; fall back to name matching.
    for a in attrs {
        match a {
            NameAttribute::Sent => return (FolderKind::Sent, true),
            NameAttribute::Drafts => return (FolderKind::Drafts, true),
            NameAttribute::Trash => return (FolderKind::Trash, true),
            NameAttribute::Junk => return (FolderKind::Junk, true),
            NameAttribute::Archive => return (FolderKind::Archive, true),
            NameAttribute::Flagged => return (FolderKind::Starred, true),
            _ => {}
        }
    }

    let leaf = path.rsplit(['/', '.']).next().unwrap_or(path).to_lowercase();
    let kind = match leaf.as_str() {
        "inbox" => FolderKind::Inbox,
        "sent" | "sent items" | "sent mail" => FolderKind::Sent,
        "drafts" => FolderKind::Drafts,
        "trash" | "deleted" | "deleted items" | "bin" => FolderKind::Trash,
        "junk" | "spam" => FolderKind::Junk,
        "archive" | "all mail" => FolderKind::Archive,
        "starred" | "flagged" => FolderKind::Starred,
        _ => FolderKind::Custom,
    };
    (kind, false)
}

pub(crate) fn folder_order(kind: FolderKind) -> u8 {
    match kind {
        FolderKind::Inbox => 0,
        FolderKind::Starred => 1,
        FolderKind::Drafts => 2,
        FolderKind::Sent => 3,
        FolderKind::Archive => 4,
        FolderKind::Junk => 5,
        FolderKind::Trash => 6,
        FolderKind::Custom => 7,
    }
}

/// Show only the leaf segment of a hierarchical mailbox path, with INBOX
/// special-cased.
fn display_name(path: &str, delimiter: Option<&str>) -> String {
    if path.eq_ignore_ascii_case("inbox") {
        return i18n("Inbox");
    }
    let leaf = match delimiter {
        Some(d) if !d.is_empty() => path.rsplit(d).next().unwrap_or(path),
        _ => path.rsplit(['/', '.']).next().unwrap_or(path),
    };
    // Split first, decode second: the delimiter is ASCII and modified UTF-7
    // never produces one inside an escaped run (that is why `,` stands in for
    // `/`), so the leaf is always a whole name (issue #1).
    crate::mutf7::decode(leaf)
}

/// Join an envelope address list into "a@x.com, b@y.com" (emails only).
fn address_list(addrs: Option<&Vec<async_imap::imap_proto::types::Address>>) -> String {
    addrs
        .map(|v| {
            v.iter()
                .map(|a| address_parts(a).1)
                .filter(|e| !e.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn address_parts(addr: &async_imap::imap_proto::types::Address) -> (String, String) {
    let mailbox = addr.mailbox.as_deref().map(bytes_to_string).unwrap_or_default();
    let host = addr.host.as_deref().map(bytes_to_string).unwrap_or_default();
    let email = if host.is_empty() {
        mailbox.clone()
    } else {
        format!("{mailbox}@{host}")
    };
    let name = addr
        .name
        .as_deref()
        .map(decode_header)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| email.clone());
    (name, email)
}

fn bytes_to_string(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Decode a header value that may be RFC 2047 encoded ("=?UTF-8?…?=").
///
/// Some senders (notably Mailchimp) emit a single encoded-word far longer than
/// RFC 2047's 75-character limit — e.g. The Marginalian's newsletter subjects.
/// The decoder aborts on over-long words by default, which would leave the raw
/// `=?utf-8?Q?…?=` gibberish in the UI; we ask it to decode them anyway, as
/// Apple Mail and Thunderbird do.
pub(crate) fn decode_header(raw: &[u8]) -> String {
    use rfc2047_decoder::{Decoder, RecoverStrategy};
    let decode = |bytes: &[u8]| -> Option<String> {
        Decoder::new()
            .too_long_encoded_word_strategy(RecoverStrategy::Decode)
            .decode(bytes)
            .ok()
    };
    let first = decode(raw).unwrap_or_else(|| String::from_utf8_lossy(raw).into_owned());
    if !first.contains("=?") {
        return first;
    }
    // Still carrying raw encoded words — some senders put literal spaces
    // inside them ("…_DPD ?="), which the decoder refuses. Repair and retry.
    repair_spaces_in_encoded_words(&first)
        .and_then(|fixed| decode(fixed.as_bytes()))
        .filter(|s| !s.contains("=?"))
        .unwrap_or(first)
}

/// Map illegal raw spaces inside RFC 2047 encoded words to their legal
/// spelling (`_` for Q encoding, dropped for B), so the word decodes instead
/// of showing as `=?UTF-8?Q?…?=` gibberish. `None` when nothing needed it.
fn repair_spaces_in_encoded_words(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    let mut repaired = false;
    while let Some(start) = rest.find("=?") {
        let (head, tail) = rest.split_at(start);
        out.push_str(head);
        let Some(end) = tail.find("?=") else {
            out.push_str(tail);
            rest = "";
            break;
        };
        let word = &tail[..end + 2];
        let parts: Vec<&str> = word.split('?').collect();
        // "=?charset?enc?text?=" splits to ["=", cs, enc, text…, "="].
        if parts.len() >= 5 && word.contains(' ') {
            let charset = parts[1];
            let enc = parts[2];
            let text = word[..word.len() - 2].splitn(4, '?').nth(3).unwrap_or("");
            let fixed = if enc.eq_ignore_ascii_case("q") {
                text.replace(' ', "_")
            } else {
                text.replace(' ', "")
            };
            if fixed != text {
                repaired = true;
            }
            out.push_str("=?");
            out.push_str(charset);
            out.push('?');
            out.push_str(enc);
            out.push('?');
            out.push_str(&fixed);
            out.push_str("?=");
        } else {
            out.push_str(word);
        }
        rest = &tail[end + 2..];
    }
    out.push_str(rest);
    repaired.then_some(out)
}

/// Compact date label from a unix timestamp (same style as [`format_date`]).
fn format_timestamp(ts: i64) -> String {
    if ts <= 0 {
        return String::new();
    }
    label_from_timestamp(ts)
}

/// Fall back to the server's `INTERNALDATE` (the delivery date) when a message
/// carries no parseable `Date:` header. Some senders omit `Date:` entirely,
/// which would otherwise leave the row with a blank date label and a zero sort
/// timestamp (sinking it to the bottom of the list). Returns `("", 0)` if the
/// server didn't supply an INTERNALDATE either.
fn internal_date_summary(fetch: &Fetch) -> (String, i64) {
    match fetch.internal_date() {
        Some(dt) => {
            let ts = dt.timestamp();
            (format_timestamp(ts), ts)
        }
        None => (String::new(), 0),
    }
}

/// Parse an RFC 2822 date into a compact label and a sortable unix timestamp.
fn format_date(raw: &[u8]) -> (String, i64) {
    let s = String::from_utf8_lossy(raw);
    let s = s.trim();
    let Ok(dt) = chrono::DateTime::parse_from_rfc2822(s) else {
        return (s.to_string(), 0);
    };
    (label_from_timestamp(dt.timestamp()), dt.timestamp())
}

/// Extract a renderable HTML body from a raw RFC 822 message. A lone HTML part is
/// used directly; anything else is composed part by part.
///
/// A message can carry more than one display body. Apple Mail (iPhone) sends photo
/// mail as a multipart/mixed that interleaves text parts with the images, so taking
/// only the first body — as `body_html(0)` does — renders the message blank. Walk
/// every display part in order instead, embedding inline images as `data:` URIs:
/// the reader's CSP permits those even while remote content is blocked, and the
/// bytes arrived with the message, so nothing is fetched from the network.
///
/// HTML bodies that reference their images as `cid:` (Gmail's `multipart/related`
/// photo mail, most newsletters) get the same treatment: nothing in the reader can
/// resolve a `cid:` URL, so each reference is rewritten to the `data:` URI of the
/// part it names — otherwise the image renders as its broken-image alt text.
/// Render a raw message to the HTML the reader displays. Public so the Outbox
/// can show a queued message without a round trip to the server — the bytes are
/// already on disk.
pub fn extract_body(raw: &[u8]) -> String {
    use mail_parser::{MessageParser, MimeHeaders, PartType};

    /// Total decoded bytes of inline images embedded into one message body.
    const INLINE_IMAGE_BUDGET: usize = 16 * 1024 * 1024;

    let Some(parsed) = MessageParser::default().parse(raw) else {
        return wrap_plain(&String::from_utf8_lossy(raw));
    };

    let bodies: Vec<_> = parsed.html_bodies().collect();
    let mut budget = INLINE_IMAGE_BUDGET;
    // Content-IDs rendered in place by `inline_cid_images`, so the loop below
    // doesn't also append them as standalone images.
    let mut embedded: Vec<String> = Vec::new();

    // A single HTML part is already a complete document — pass it through with only
    // its `cid:` references resolved, so the sender's own layout and styling survive.
    if let [only] = bodies.as_slice() {
        if let PartType::Html(html) = &only.body {
            return inline_cid_images(html, &parsed.parts, &mut budget, &mut embedded);
        }
    }

    // Resolve every HTML body first: an image part can appear *before* the body
    // that references it, and the loop below needs to know it was consumed.
    let rendered: Vec<Option<String>> = bodies
        .iter()
        .map(|part| match &part.body {
            PartType::Html(html) => Some(inline_cid_images(
                html,
                &parsed.parts,
                &mut budget,
                &mut embedded,
            )),
            _ => None,
        })
        .collect();

    let mut inner = String::new();
    for (part, html) in bodies.iter().zip(&rendered) {
        if let Some(html) = html {
            inner.push_str(html);
            continue;
        }
        match &part.body {
            PartType::Text(text) if !text.trim().is_empty() => {
                inner.push_str("<div class=\"vireo-plain\">");
                inner.push_str(&linkify(text));
                inner.push_str("</div>");
            }
            PartType::Binary(bytes) | PartType::InlineBinary(bytes) => {
                // Already rendered where the HTML placed it — don't repeat it.
                if part
                    .content_id()
                    .is_some_and(|id| embedded.iter().any(|e| e == id))
                {
                    continue;
                }
                // Embedding inflates by ~4/3 and the result is cached on disk, so
                // stop inlining past a budget. Oversized images are still listed
                // as attachments and can be opened from there.
                if let Some(mime) = image_mime(part) {
                    if let Some(left) = budget.checked_sub(bytes.len()) {
                        budget = left;
                        inner.push_str(&format!(
                            "<img class=\"vireo-inline\" src=\"data:{mime};base64,{}\">",
                            crate::oauth::base64_encode(bytes)
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    if inner.trim().is_empty() {
        return match parsed.body_text(0) {
            Some(text) if !text.trim().is_empty() => wrap_plain(&text),
            _ => wrap_plain("(no readable content)"),
        };
    }
    wrap_fragment(&inner)
}

/// Rewrite `cid:` resource references in `html` to `data:` URIs built from the
/// message's own parts.
///
/// A `cid:` URL names another MIME part of the same message (RFC 2392). WebKit has
/// no handler for the scheme, so an untouched `<img src="cid:…">` renders as its
/// alt text — for Gmail photo mail (`multipart/related`) that means the filename
/// shows where the picture should be. The bytes are already in hand, so swapping in
/// a `data:` URI costs no network access and needs no change to the reader's CSP.
///
/// Only image parts are resolved (the reader can't display anything else inline),
/// each is charged against `budget` once however many times it's referenced, and
/// every Content-ID actually embedded is recorded in `embedded`. References that
/// can't be resolved are left as they are: an unresolved `cid:` is no worse than
/// before, and a rewrite that guessed wrong would be.
fn inline_cid_images(
    html: &str,
    parts: &[mail_parser::MessagePart<'_>],
    budget: &mut usize,
    embedded: &mut Vec<String>,
) -> String {
    use mail_parser::{MimeHeaders, PartType};

    let bytes = html.as_bytes();
    if !bytes.windows(4).any(|w| w.eq_ignore_ascii_case(b"cid:")) {
        return html.to_string();
    }

    // A Content-ID matches case-insensitively and with any angle brackets stripped;
    // senders aren't consistent about either between the header and the reference.
    let normalize = |id: &str| {
        id.trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .to_ascii_lowercase()
    };
    // cid -> data URI, or `None` for one we've already failed to resolve. Built on
    // first reference so a cid used twice is embedded (and charged) only once.
    let mut resolved: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();

    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    let mut copied = 0;
    while i + 4 <= bytes.len() {
        if !bytes[i..i + 4].eq_ignore_ascii_case(b"cid:") {
            i += 1;
            continue;
        }
        // Only rewrite in resource position (`src="cid:…"`, `src=cid:…`,
        // `url(cid:…)`) so prose that happens to mention a cid is left alone, and
        // never in an `href` — a link is handed to the external browser on click,
        // where a megabyte-long `data:` URL is worse than a dead `cid:` one.
        if i == 0 || !matches!(bytes[i - 1], b'"' | b'\'' | b'=' | b'(') || is_href(bytes, i) {
            i += 1;
            continue;
        }
        // The reference runs to the closing quote/bracket/whitespace. Every
        // terminator is ASCII, so these stay on `char` boundaries.
        let start = i + 4;
        let end = bytes[start..]
            .iter()
            .position(|b| matches!(b, b'"' | b'\'' | b'>' | b')' | b' ' | b'\t' | b'\r' | b'\n'))
            .map_or(html.len(), |n| start + n);
        let key = normalize(&percent_decode(&html[start..end]));
        if key.is_empty() {
            i = end.max(i + 1);
            continue;
        }

        let uri = resolved.entry(key.clone()).or_insert_with(|| {
            let part = parts
                .iter()
                .find(|p| p.content_id().is_some_and(|id| normalize(id) == key))?;
            let data = match &part.body {
                PartType::Binary(data) | PartType::InlineBinary(data) => data,
                _ => return None,
            };
            let mime = image_mime(part)?;
            *budget = budget.checked_sub(data.len())?;
            embedded.push(part.content_id()?.to_string());
            Some(format!(
                "data:{mime};base64,{}",
                crate::oauth::base64_encode(data)
            ))
        });

        match uri {
            Some(uri) => {
                out.push_str(&html[copied..i]);
                out.push_str(uri);
                copied = end;
                i = end;
            }
            // Unresolvable: leave the reference verbatim.
            None => i = end.max(i + 1),
        }
    }
    out.push_str(&html[copied..]);
    out
}

/// Whether the value starting at `at` is the target of an `href=` attribute,
/// looking back past the quote and any whitespace around the `=`.
fn is_href(bytes: &[u8], at: usize) -> bool {
    let mut j = at;
    while j > 0 && matches!(bytes[j - 1], b'"' | b'\'' | b'=' | b' ' | b'\t' | b'\r' | b'\n') {
        j -= 1;
    }
    j >= 4 && bytes[j - 4..j].eq_ignore_ascii_case(b"href")
}

/// Decode `%XX` escapes in a URI reference, leaving anything else untouched.
/// `cid:` values are percent-encoded when they contain URI-reserved characters
/// (Gmail's ids embed an `@`, which some senders write as `%40`).
fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (i + 2 < bytes.len())
            .then(|| std::str::from_utf8(&bytes[i + 1..i + 3]).ok())
            .flatten()
            .filter(|_| bytes[i] == b'%')
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The `image/<subtype>` MIME type of a part, if it is an image we can inline.
/// The subtype is validated so it can't break out of the `data:` URI.
fn image_mime(part: &mail_parser::MessagePart) -> Option<String> {
    use mail_parser::MimeHeaders;
    let ty = part.content_type()?;
    if !ty.ctype().eq_ignore_ascii_case("image") {
        return None;
    }
    let subtype = ty.subtype()?;
    let safe = subtype
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    safe.then(|| format!("image/{}", subtype.to_ascii_lowercase()))
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Escape a string for use inside a double-quoted HTML attribute (e.g. `href`).
fn escape_attr(text: &str) -> String {
    escape_html(text).replace('"', "&quot;")
}

/// HTML-escape plain text and turn bare URLs into clickable links. Runs on raw
/// (unescaped) text: non-URL spans are escaped as usual; each URL becomes an
/// `<a href>` whose scheme is always http(s) (a bare `www.` host is prefixed with
/// `https://`), so no `javascript:`-style link can be forged. Links open in the
/// external browser via the reader's navigation policy.
fn linkify(text: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    while let Some((start, end, href)) = next_url(text, i) {
        out.push_str(&escape_html(&text[i..start]));
        out.push_str(&format!(
            "<a href=\"{}\">{}</a>",
            escape_attr(&href),
            escape_html(&text[start..end])
        ));
        i = end;
    }
    out.push_str(&escape_html(&text[i..]));
    out
}

/// Find the next bare URL at or after `from`, returning `(start, end, href)`.
fn next_url(text: &str, from: usize) -> Option<(usize, usize, String)> {
    let mut i = from;
    while i < text.len() {
        if !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &text[i..];
        let (prefix, add_https) = if rest.starts_with("https://") {
            ("https://", false)
        } else if rest.starts_with("http://") {
            ("http://", false)
        } else if rest.starts_with("www.") {
            ("www.", true)
        } else {
            i += 1;
            continue;
        };
        // Only match at a boundary — the start, after whitespace, or after an
        // opening bracket/quote — so "shttp://", "awww." and "hi@www.x" (an email)
        // aren't linked.
        let boundary = text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '<' | '[' | '{' | '"' | '\'' | '|'));
        let end = consume_url(text, i);
        // Reject a scheme with nothing (usable) after it.
        if boundary && end > i + prefix.len() {
            let url = &text[i..end];
            let href = if add_https { format!("https://{url}") } else { url.to_string() };
            return Some((i, end, href));
        }
        i += prefix.len();
    }
    None
}

/// The end index of the URL that begins at `start`: consume non-terminator chars,
/// then trim trailing sentence punctuation (keeping a `)` that balances a `(`).
fn consume_url(text: &str, start: usize) -> usize {
    let mut end = start;
    for (off, ch) in text[start..].char_indices() {
        if ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '`' | '\'' | '\\') {
            break;
        }
        end = start + off + ch.len_utf8();
    }
    let url = &text[start..end];
    let mut trimmed = url;
    loop {
        match trimmed.chars().last() {
            Some(')') => {
                if trimmed.matches(')').count() > trimmed.matches('(').count() {
                    trimmed = &trimmed[..trimmed.len() - 1];
                } else {
                    break; // balanced — part of the URL (e.g. a Wikipedia link)
                }
            }
            Some('.' | ',' | ';' | ':' | '!' | '?' | ']' | '}') => {
                trimmed = &trimmed[..trimmed.len() - 1];
            }
            _ => break,
        }
    }
    start + trimmed.len()
}

/// Wrap plain text in a minimal, readable HTML document. Colours are left to
/// the `color-scheme` the reader injects, so the message follows the
/// light/dark theme — and no padding is baked in: the reader injects the
/// default inset at render time, so it stays tunable without invalidating
/// every cached body (bodies are stored rendered; see cache SCHEMA_VERSION).
fn wrap_plain(text: &str) -> String {
    let escaped = linkify(text);
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><style>\
         body{{margin:0;font:14px/1.5 system-ui,sans-serif;\
         white-space:pre-wrap;word-wrap:break-word}}\
         </style></head><body>{escaped}</body></html>"
    )
}

/// Wrap composed body fragments (text blocks and inline images) in a document.
/// Padding is the reader's injected default, as in [`wrap_plain`].
fn wrap_fragment(inner: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><style>\
         body{{margin:0;font:14px/1.5 system-ui,sans-serif}}\
         .vireo-plain{{white-space:pre-wrap;word-wrap:break-word}}\
         .vireo-inline{{display:block;max-width:100%;height:auto;\
         margin:12px 0;border-radius:6px}}\
         </style></head><body>{inner}</body></html>"
    )
}

// ---------------------------------------------------------------------------
// Microsoft Graph path (issue #36)
// ---------------------------------------------------------------------------
//
// Microsoft 365 accounts imported from GNOME Online Accounts authenticate with
// a GOA token scoped to the Graph API — it cannot log in to IMAP or SMTP at
// all. So these accounts speak Graph (REST) end to end: folders, summaries,
// raw MIME bodies (`/$value`, which feeds the exact same parsing pipeline as
// IMAP), flags, moves, drafts, and `sendMail`. Message uids are the same
// stable string-hash the POP3 path uses (Graph ids are strings); the
// uid → Graph-id map is rebuilt from every folder listing. Threading uses a
// synthetic `graph-conv:<conversationId>` reference token (stripped before any
// wire header in `build_email`) because the real References header isn't
// available from list queries.

const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";
/// Summaries listed per folder (matches the POP3 path's indexing appetite).
const GRAPH_INDEX_CAP: usize = 300;

/// One Graph mail folder, flattened out of the tree.
struct GraphFolder {
    graph_id: String,
    folder: Folder,
}

fn graph_auth(token: &str) -> String {
    format!("Bearer {token}")
}

/// Read a ureq error into something a user can act on (status + body snippet).
fn graph_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            // Graph errors are JSON with a nested message; surface just that.
            let msg = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                .unwrap_or_else(|| body.chars().take(200).collect());
            format!("Microsoft Graph returned {code}: {msg}")
        }
        other => other.to_string(),
    }
}

/// The Microsoft Graph conversation for the console: method and URL (the
/// token travels in a header and is never logged), then the verdict.
fn graph_wire(method: &str, url: &str) {
    tracing::debug!(target: "vireo::graph", "> {method} {}", url.strip_prefix(GRAPH_BASE).unwrap_or(url));
}

fn graph_wired<T>(method: &str, url: &str, r: &Result<T, String>) {
    let path = url.strip_prefix(GRAPH_BASE).unwrap_or(url);
    match r {
        Ok(_) => tracing::debug!(target: "vireo::graph", "< OK ({method} {path})"),
        Err(e) => tracing::warn!(target: "vireo::graph", "< {e} ({method} {path})"),
    }
}

fn graph_get_json(token: &str, url: &str) -> Result<serde_json::Value, String> {
    graph_wire("GET", url);
    let r = ureq::get(url)
        .set("Authorization", &graph_auth(token))
        .call()
        .map_err(graph_err)
        .and_then(|resp| resp.into_json().map_err(|e| e.to_string()));
    graph_wired("GET", url, &r);
    r
}

fn graph_get_bytes(token: &str, url: &str) -> Result<Vec<u8>, String> {
    graph_wire("GET", url);
    let resp = ureq::get(url)
        .set("Authorization", &graph_auth(token))
        .call()
        .map_err(graph_err);
    graph_wired("GET", url, &resp);
    let resp = resp?;
    let mut out = Vec::new();
    use std::io::Read;
    // Raw MIME can be large; cap well above any sane message (64 MB).
    resp.into_reader()
        .take(64 * 1024 * 1024)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// POST/PATCH a JSON body; an empty 2xx response comes back as `Null`.
fn graph_send_json(
    token: &str,
    method: &str,
    url: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    graph_wire(method, url);
    let resp = ureq::request(method, url)
        .set("Authorization", &graph_auth(token))
        .send_json(body.clone())
        .map_err(graph_err);
    graph_wired(method, url, &resp);
    let text = resp?.into_string().map_err(|e| e.to_string())?;
    if text.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
}

/// POST raw MIME (base64, `text/plain` content type — Graph's MIME format) to
/// `sendMail` or a create-message endpoint.
fn graph_post_mime(token: &str, url: &str, raw: &[u8]) -> Result<serde_json::Value, String> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
    graph_wire("POST(mime)", url);
    let resp = ureq::post(url)
        .set("Authorization", &graph_auth(token))
        .set("Content-Type", "text/plain")
        .send_string(&b64)
        .map_err(graph_err);
    graph_wired("POST(mime)", url, &resp);
    let text = resp?.into_string().map_err(|e| e.to_string())?;
    if text.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
}

fn graph_delete_req(token: &str, url: &str) -> Result<(), String> {
    graph_wire("DELETE", url);
    let r = ureq::delete(url)
        .set("Authorization", &graph_auth(token))
        .call()
        .map_err(graph_err)
        .map(|_| ());
    graph_wired("DELETE", url, &r);
    r
}

/// Follow `@odata.nextLink` pagination, collecting `value` arrays up to `cap`.
fn graph_paged(token: &str, first_url: &str, cap: usize) -> Result<Vec<serde_json::Value>, String> {
    let mut out = Vec::new();
    let mut url = first_url.to_string();
    loop {
        let page = graph_get_json(token, &url)?;
        if let Some(items) = page["value"].as_array() {
            out.extend(items.iter().cloned());
        }
        if out.len() >= cap {
            out.truncate(cap);
            return Ok(out);
        }
        match page["@odata.nextLink"].as_str() {
            Some(next) => url = next.to_string(),
            None => return Ok(out),
        }
    }
}

/// List the account's mail folders (tree flattened, well-known roles mapped),
/// sorted and id-numbered exactly like the IMAP path's folder list.
fn graph_list_folders(
    token: &str,
    account_id: u32,
    assignments: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<GraphFolder>, String> {
    // Well-known folders name the roles; everything else is Custom. A role
    // folder some account type lacks (e.g. archive) just 404s — skip it.
    let mut roles: std::collections::HashMap<String, FolderKind> = Default::default();
    for (wk, kind) in [
        ("inbox", FolderKind::Inbox),
        ("sentitems", FolderKind::Sent),
        ("drafts", FolderKind::Drafts),
        ("deleteditems", FolderKind::Trash),
        ("junkemail", FolderKind::Junk),
        ("archive", FolderKind::Archive),
    ] {
        if let Ok(v) = graph_get_json(token, &format!("{GRAPH_BASE}/me/mailFolders/{wk}?$select=id"))
        {
            if let Some(id) = v["id"].as_str() {
                roles.insert(id.to_string(), kind);
            }
        }
    }

    const SELECT: &str =
        "$select=id,displayName,childFolderCount,unreadItemCount,totalItemCount";
    let roots = graph_paged(
        token,
        &format!("{GRAPH_BASE}/me/mailFolders?$top=100&{SELECT}"),
        200,
    )?;

    // Flatten the tree breadth-first; paths join with '/' like the sidebar's
    // hierarchy expects. Depth and total are capped defensively.
    let mut out: Vec<GraphFolder> = Vec::new();
    // Graph folder id → (every message, unread): the chip picks one once the
    // kinds are final (a manual Drafts assignment counts every draft).
    let mut counts: std::collections::HashMap<String, (u32, u32)> = Default::default();
    let mut queue: Vec<(serde_json::Value, String, u8)> =
        roots.into_iter().map(|v| (v, String::new(), 0u8)).collect();
    while let Some((v, prefix, depth)) = queue.pop() {
        let Some(gid) = v["id"].as_str() else { continue };
        let name = v["displayName"].as_str().unwrap_or("?").to_string();
        let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        let kind = roles.get(gid).copied().unwrap_or(FolderKind::Custom);
        if v["childFolderCount"].as_i64().unwrap_or(0) > 0 && depth < 4 && out.len() < 400 {
            if let Ok(children) = graph_paged(
                token,
                &format!("{GRAPH_BASE}/me/mailFolders/{gid}/childFolders?$top=100&{SELECT}"),
                200,
            ) {
                queue.extend(children.into_iter().map(|c| (c, path.clone(), depth + 1)));
            }
        }
        let count = |field: &str| v[field].as_i64().unwrap_or(0).max(0) as u32;
        counts.insert(gid.to_string(), (count("totalItemCount"), count("unreadItemCount")));
        out.push(GraphFolder {
            graph_id: gid.to_string(),
            folder: Folder {
                id: 0, // assigned by order below
                account_id,
                name,
                path,
                kind,
                unread: 0, // filled in below, once the kinds are final
            },
        });
    }

    out.sort_by(|a, b| {
        folder_order(a.folder.kind)
            .cmp(&folder_order(b.folder.kind))
            .then_with(|| a.folder.path.to_lowercase().cmp(&b.folder.path.to_lowercase()))
    });
    for (i, f) in out.iter_mut().enumerate() {
        f.folder.id = i as u32 + 1;
    }
    // Manual Special Folders assignments (#82) over the well-known roles,
    // before the chips are counted — as the IMAP listing does.
    let mut folders: Vec<Folder> = out.iter().map(|f| f.folder.clone()).collect();
    crate::models::assign_folder_roles(assignments, &mut folders);
    for (gf, f) in out.iter_mut().zip(folders) {
        gf.folder.kind = f.kind;
        let (total, unseen) = counts.get(&gf.graph_id).copied().unwrap_or_default();
        gf.folder.unread = if chip_counts_all(f.kind) { total } else { unseen };
    }
    Ok(out)
}

/// The comma-separated addresses of a Graph recipient array.
fn graph_addrs(v: &serde_json::Value) -> String {
    v.as_array()
        .map(|list| {
            list.iter()
                .filter_map(|r| r["emailAddress"]["address"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// Map one Graph message summary to a [`Message`]. Returns the Graph id too so
/// the caller can index it.
fn graph_message(v: &serde_json::Value, account_id: u32, folder_id: u32) -> Option<(Message, String)> {
    let gid = v["id"].as_str()?.to_string();
    let uid = hash_uid(&gid);
    let ts = v["receivedDateTime"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp())
        .unwrap_or(0);
    let preview: String = v["bodyPreview"]
        .as_str()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect();
    let preview = pgp_preview(&preview).unwrap_or(preview);
    let from_addr = v["from"]["emailAddress"]["address"].as_str().unwrap_or("").to_string();
    let reply_to = graph_addrs(&v["replyTo"]);
    let reply_to =
        if reply_to.eq_ignore_ascii_case(&from_addr) { String::new() } else { reply_to };
    let msg = Message {
        id: uid,
        account_id,
        folder_id,
        uid,
        from_name: v["from"]["emailAddress"]["name"].as_str().unwrap_or("").to_string(),
        from_addr,
        reply_to,
        to: graph_addrs(&v["toRecipients"]),
        cc: graph_addrs(&v["ccRecipients"]),
        subject: v["subject"].as_str().unwrap_or("").to_string(),
        preview,
        body: String::new(),
        date: if ts > 0 { label_from_timestamp(ts) } else { String::new() },
        timestamp: ts,
        unread: !v["isRead"].as_bool().unwrap_or(true),
        starred: v["flag"]["flagStatus"].as_str() == Some("flagged"),
        keywords: v["categories"]
            .as_array()
            .map(|cs| cs.iter().filter_map(|c| c.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
        has_attachment: v["hasAttachments"].as_bool().unwrap_or(false),
        message_id: normalize_msgid(v["internetMessageId"].as_str().unwrap_or("").as_bytes()),
        references: v["conversationId"]
            .as_str()
            .map(|c| format!("graph-conv:{}", c.to_ascii_lowercase()))
            .unwrap_or_default(),
    };
    Some((msg, gid))
}

const GRAPH_MSG_SELECT: &str = "$select=id,internetMessageId,conversationId,subject,bodyPreview,\
                                from,replyTo,toRecipients,ccRecipients,receivedDateTime,isRead,\
                                flag,hasAttachments,categories";

/// List a folder's newest summaries (newest first).
fn graph_list_messages(
    token: &str,
    folder_graph_id: &str,
    account_id: u32,
    folder_id: u32,
) -> Result<Vec<(Message, String)>, String> {
    let url = format!(
        "{GRAPH_BASE}/me/mailFolders/{folder_graph_id}/messages\
         ?$top=100&$orderby=receivedDateTime%20desc&{GRAPH_MSG_SELECT}"
    );
    let items = graph_paged(token, &url, GRAPH_INDEX_CAP)?;
    Ok(items.iter().filter_map(|v| graph_message(v, account_id, folder_id)).collect())
}

/// Per-account state the Graph loop threads through its handlers.
struct GraphState {
    /// Vireo folder path → (folder id, Graph folder id), from the last listing.
    folders: std::collections::HashMap<String, (u32, String)>,
    /// Message uid (hashed Graph id) → Graph message id.
    uids: std::collections::HashMap<u32, String>,
    /// The Drafts folder's (folder id, path), for draft reloads after a send.
    drafts: Option<(u32, String)>,
    /// The Inbox's (folder id, path), for the new-mail poll.
    inbox: Option<(u32, String)>,
    /// The account's manual Special Folders assignments (#82), applied to
    /// every listing.
    roles: std::collections::BTreeMap<String, String>,
}

impl GraphState {
    /// The chip number for a folder just loaded: every draft in Drafts,
    /// unread mail elsewhere.
    fn chip_count(&self, folder_id: u32, messages: &[Message]) -> u32 {
        let kind = match &self.drafts {
            Some((id, _)) if *id == folder_id => FolderKind::Drafts,
            _ => FolderKind::Custom,
        };
        chip_count_of(kind, messages)
    }

    fn adopt_folders(&mut self, list: &[GraphFolder]) {
        self.folders = list
            .iter()
            .map(|f| (f.folder.path.clone(), (f.folder.id, f.graph_id.clone())))
            .collect();
        self.drafts = list
            .iter()
            .find(|f| f.folder.kind == FolderKind::Drafts)
            .map(|f| (f.folder.id, f.folder.path.clone()));
        self.inbox = list
            .iter()
            .find(|f| f.folder.kind == FolderKind::Inbox)
            .map(|f| (f.folder.id, f.folder.path.clone()));
    }
}

async fn run_graph(
    account_id: u32,
    account: AccountConfig,
    mut rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent),
) {
    let cache = Cache::open().map_err(|e| tracing::warn!("cache unavailable: {e}")).ok();

    emit(WorkerEvent::Account(Account {
        id: account_id,
        name: account.name.clone(),
        email: account.email.clone(),
        label: account.display_label(),
        accent: accent_for(account_id).into(),
    }));

    // Cached folders immediately, then the live list.
    let cached_folders = cache.as_ref().map(|c| c.load_folders(account_id)).unwrap_or_default();
    if !cached_folders.is_empty() {
        emit(WorkerEvent::Folders(cached_folders));
    }

    // When the Junk / Trash auto-empty (#140) last ran for this worker.
    let mut last_auto_empty: Option<std::time::Instant> = None;
    let mut state = GraphState {
        folders: Default::default(),
        uids: Default::default(),
        drafts: None,
        inbox: None,
        roles: account.folder_roles.clone(),
    };

    // Fetch a token and the folder list. A GOA token failure here is the one
    // users actually hit (signed out in GNOME Settings), so say that.
    if let Some(token) = graph_token(&account, &emit).await {
        refresh_graph_folders(&token, account_id, cache.as_ref(), &mut state, &emit).await;
    }

    // Graph has no push channel (nothing like IMAP IDLE is available to this
    // token), so new mail arrives on a poll. The auto-fetch preference sets the
    // cadence when it's on; otherwise a quiet couple of minutes.
    let poll_secs = match crate::config::load_fetch_interval() {
        0 => 120,
        s => s.max(60),
    };
    let mut poll = tokio::time::interval(Duration::from_secs(poll_secs));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    poll.tick().await; // consume the interval's immediate first tick

    loop {
        let req = tokio::select! {
            req = rx.recv() => match req {
                Some(req) => req,
                None => break,
            },
            _ = poll.tick() => {
                graph_poll_inbox(account_id, &account, cache.as_ref(), &mut state, &emit).await;
                continue;
            }
        };
        match req {
            MailRequest::Locate { message_id } => {
                // No folders to search here beyond what the cache already
                // held: answer "not found" so the app can report the miss.
                emit(WorkerEvent::Located { message_id: message_id.clone(), hit: None });
            }
            MailRequest::FindKeywords => {
                // Categories are defined once per mailbox, with a name and a
                // colour: the master list is the whole answer. Counts come
                // from the cache, the only place Vireo has them.
                let mut found: Vec<KeywordFinding> = Vec::new();
                if let Some(token) = graph_token(&account, &emit).await {
                    let t = token.clone();
                    let cats = tokio::task::spawn_blocking(move || {
                        graph_get_json(&t, &format!("{GRAPH_BASE}/me/outlook/masterCategories"))
                    })
                    .await
                    .unwrap_or_else(|_| Err("task failed".into()));
                    match cats {
                        Ok(v) => {
                            for c in v["value"].as_array().into_iter().flatten() {
                                let Some(name) = c["displayName"].as_str() else { continue };
                                let count = cache
                                    .as_ref()
                                    .map(|c| c.count_with_keyword(account_id, name))
                                    .unwrap_or(0);
                                found.push(KeywordFinding {
                                    keyword: name.to_string(),
                                    name: Some(name.to_string()),
                                    color: graph_preset_color(c["color"].as_str().unwrap_or("")),
                                    count,
                                    folders: Vec::new(),
                                });
                            }
                        }
                        Err(e) => tracing::warn!("[account {account_id}] find tags: categories failed: {e}"),
                    }
                }
                emit(WorkerEvent::KeywordsFound(found));
            }
            MailRequest::RefreshKeywords { .. } => {
                // Categories arrive with each folder's listing, so the
                // listing is the sync: every folder is re-read, which
                // rewrites its cached keywords (#166).
                let mut paths = Vec::new();
                if let Some(token) = graph_token(&account, &emit).await {
                    let folders: Vec<(String, u32)> =
                        state.folders.iter().map(|(p, (id, _))| (p.clone(), *id)).collect();
                    for (path, folder_id) in folders {
                        if graph_load_folder(&token, account_id, folder_id, &path, cache.as_ref(), &mut state)
                            .await
                            .is_ok()
                        {
                            paths.push(path);
                        }
                    }
                }
                emit(WorkerEvent::KeywordsSynced { paths });
            }

            // Cache-only, exactly like the IMAP path: assemble the conversation
            // from every folder's cached summaries.
            // Graph has no equivalent of BODYSTRUCTURE in the shape this path
            // uses: attachments come out of the raw MIME it downloads, so
            // describing one costs the same as fetching it. Listing them
            // through /messages/{id}/attachments would work, but it is a
            // different call on a backend there is no account here to test
            // against — so a Microsoft account's gallery still shows what has
            // been downloaded rather than reaching back through the archive.
            MailRequest::ScanAttachments { folder_path } => {
                emit(WorkerEvent::AttachmentsScanned { folder_path, added: 0, remaining: 0 });
            }
            MailRequest::LoadRelated { message_id, ids } => {
                let messages = cache
                    .as_ref()
                    .map(|c| related_from_cache(c, account_id, &ids))
                    .unwrap_or_default();
                emit(WorkerEvent::Related { message_id, messages });
            }

            MailRequest::LoadMessages { folder_id, path }
            | MailRequest::SyncFolder { folder_id, path } => {
                if let Some(c) = cache.as_ref() {
                    let cached = c.load_messages(account_id, &path, folder_id);
                    if !cached.is_empty() {
                        emit(WorkerEvent::Messages { folder_id, messages: cached });
                    }
                }
                emit(WorkerEvent::Status(i18n("Syncing…")));
                let Some(token) = graph_token(&account, &emit).await else {
                    // No token, so no mail is coming: end the folder's index
                    // (#218) rather than leave the list spinning.
                    emit(WorkerEvent::BackfillDone { folder_id });
                    emit(WorkerEvent::Status(String::new()));
                    continue;
                };
                match graph_load_folder(
                    &token, account_id, folder_id, &path, cache.as_ref(), &mut state,
                )
                .await
                {
                    Ok(messages) => {
                        let unread = state.chip_count(folder_id, &messages);
                        emit_graph_body_hits(&token, &account, folder_id, &path, &messages, &state, &emit)
                            .await;
                        emit(WorkerEvent::Messages { folder_id, messages });
                        emit(WorkerEvent::FolderUnread { folder_id, unread });
                        // Graph loads the whole folder in one pass — there is no
                        // background backfill, so the index is complete now.
                        // Without this the list's "Loading more…" tail spinner
                        // never clears on folders with less than a page of mail
                        // (an emptied inbox most visibly).
                        emit(WorkerEvent::BackfillDone { folder_id });
                    }
                    Err(e) => {
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not fetch mail: {e}", &[("e", &(e).to_string())]),
                            connectivity: true,
                        });
                        emit(WorkerEvent::BackfillDone { folder_id });
                    }
                }
                emit(WorkerEvent::Status(String::new()));
            }

            MailRequest::LoadBody { message_id, path, uid } => {
                if let Some(body) = cache.as_ref().and_then(|c| c.load_body(account_id, &path, uid))
                {
                    let check = cache.as_ref().and_then(|c| c.load_sender_check(account_id, &path, uid));
                    emit(WorkerEvent::Body { message_id, path, body });
                    if let Some(check) = check {
                        emit(WorkerEvent::SenderChecked { message_id, check });
                    }
                    continue;
                }
                match graph_fetch_raw(&account, &mut state, &path, uid, &emit).await {
                    Ok(raw) => {
                        let (body, check, _) = render_raw(&raw);
                        if let Some(c) = cache.as_ref() {
                            if body_cacheable(&check) {
                                c.save_body(account_id, &path, uid, &body);
                            }
                            c.save_sender_check(account_id, &path, uid, &check);
                        }
                        emit(WorkerEvent::Body { message_id, path, body });
                        emit(WorkerEvent::SenderChecked { message_id, check });
                    }
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Could not load message: {e}", &[("e", &(e).to_string())]),
                        connectivity: true,
                    }),
                }
            }

            MailRequest::LoadBodies { items, path } => {
                for (message_id, uid) in items {
                    if let Some(body) =
                        cache.as_ref().and_then(|c| c.load_body(account_id, &path, uid))
                    {
                        emit(WorkerEvent::Body { message_id, path: path.clone(), body });
                        if let Some(check) = cache.as_ref().and_then(|c| c.load_sender_check(account_id, &path, uid)) {
                            emit(WorkerEvent::SenderChecked { message_id, check });
                        }
                        continue;
                    }
                    match graph_fetch_raw(&account, &mut state, &path, uid, &emit).await {
                        Ok(raw) => {
                            let (body, check, _) = render_raw(&raw);
                            if let Some(c) = cache.as_ref() {
                                if body_cacheable(&check) {
                                    c.save_body(account_id, &path, uid, &body);
                                }
                                c.save_sender_check(account_id, &path, uid, &check);
                            }
                            emit(WorkerEvent::Body { message_id, path: path.clone(), body });
                            emit(WorkerEvent::SenderChecked { message_id, check });
                        }
                        Err(e) => emit(WorkerEvent::Error {
                            text: i18n_f("Could not load message: {e}", &[("e", &(e).to_string())]),
                            connectivity: true,
                        }),
                    }
                }
            }

            MailRequest::LoadSource { message_id: _, path, uid } => {
                match graph_fetch_raw(&account, &mut state, &path, uid, &emit).await {
                    Ok(raw) => emit(WorkerEvent::Source {
                        text: String::from_utf8_lossy(&raw).into_owned(),
                    }),
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Could not load source: {e}", &[("e", &(e).to_string())]),
                        connectivity: true,
                    }),
                }
            }

            MailRequest::LoadAttachments { message_id, path, uid, download } => {
                if let Some(c) = cache.as_ref() {
                    let items = c.load_attachments(account_id, &path, uid);
                    if !items.is_empty() {
                        emit(WorkerEvent::Attachments { message_id, items });
                        continue;
                    }
                }
                if !download {
                    emit(WorkerEvent::AttachmentsPending { message_id });
                    continue;
                }
                match graph_fetch_raw(&account, &mut state, &path, uid, &emit).await {
                    Ok(raw) => {
                        let items = extract_attachments(&raw);
                        if let Some(c) = cache.as_ref() {
                            c.save_attachments(account_id, &path, uid, &items);
                        }
                        emit(WorkerEvent::Attachments { message_id, items });
                    }
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Could not load attachments: {e}", &[("e", &(e).to_string())]),
                        connectivity: true,
                    }),
                }
            }

            MailRequest::SetSeen { path, uid, seen } => {
                if let Some(c) = cache.as_ref() {
                    c.set_unread(account_id, &path, uid, !seen);
                }
                graph_patch_message(
                    &account,
                    &mut state,
                    &path,
                    uid,
                    serde_json::json!({ "isRead": seen }),
                    &emit,
                )
                .await;
                emit(WorkerEvent::SeenSettled { path, uid });
            }

            MailRequest::SetFlagged { path, uid, flagged } => {
                if let Some(c) = cache.as_ref() {
                    c.set_starred(account_id, &path, uid, flagged);
                }
                let status = if flagged { "flagged" } else { "notFlagged" };
                graph_patch_message(
                    &account,
                    &mut state,
                    &path,
                    uid,
                    serde_json::json!({ "flag": { "flagStatus": status } }),
                    &emit,
                )
                .await;
            }

            MailRequest::SetKeyword { path, uid, keyword, add, .. } => {
                // Categories travel as the whole list, so the cached row is
                // the base: patched first, then sent as it now stands.
                let categories = match cache.as_ref() {
                    Some(c) => {
                        c.set_keyword(account_id, &path, uid, &keyword, add);
                        c.keywords_of(account_id, &path, uid)
                    }
                    None if add => vec![keyword.clone()],
                    None => Vec::new(),
                };
                graph_patch_message(
                    &account,
                    &mut state,
                    &path,
                    uid,
                    serde_json::json!({ "categories": categories }),
                    &emit,
                )
                .await;
            }

            MailRequest::MarkAllRead { folder_id, path } => {
                // The server side is one PATCH per message; run it over the
                // cached unread rows (the listing window), then settle the cache.
                let unread_uids: Vec<u32> = cache
                    .as_ref()
                    .map(|c| {
                        c.load_messages(account_id, &path, folder_id)
                            .into_iter()
                            .filter(|m| m.unread)
                            .map(|m| m.uid)
                            .collect()
                    })
                    .unwrap_or_default();
                for uid in unread_uids {
                    graph_patch_message(
                        &account,
                        &mut state,
                        &path,
                        uid,
                        serde_json::json!({ "isRead": true }),
                        &emit,
                    )
                    .await;
                }
                if let Some(c) = cache.as_ref() {
                    c.mark_folder_read(account_id, &path);
                }
                emit(WorkerEvent::FolderUnread { folder_id, unread: 0 });
            }

            MailRequest::MoveMessage { path, uid, dest } => {
                if let Err(e) =
                    graph_move_uids(&account, account_id, &mut state, &path, &[uid], &dest, cache.as_ref())
                        .await
                {
                    emit(WorkerEvent::Error {
                        text: i18n_f("Could not move message: {e}", &[("e", &(e).to_string())]),
                        connectivity: false,
                    });
                }
            }

            MailRequest::MarkSpam { path, uid, dest } => {
                if let Err(e) =
                    graph_move_uids(&account, account_id, &mut state, &path, &[uid], &dest, cache.as_ref())
                        .await
                {
                    emit(WorkerEvent::Error {
                        text: i18n_f("Could not mark as spam: {e}", &[("e", &(e).to_string())]),
                        connectivity: false,
                    });
                }
            }

            // Microsoft 365 learns from the move itself: back to the Inbox.
            MailRequest::MarkHam { path, uid, dest } => {
                if let Err(e) =
                    graph_move_uids(&account, account_id, &mut state, &path, &[uid], &dest, cache.as_ref())
                        .await
                {
                    emit(WorkerEvent::Error {
                        text: i18n_f("Could not mark as not spam: {e}", &[("e", &(e).to_string())]),
                        connectivity: false,
                    });
                }
            }
            MailRequest::MarkHamMany { path, uids, dest } => {
                if let Err(e) =
                    graph_move_uids(&account, account_id, &mut state, &path, &uids, &dest, cache.as_ref())
                        .await
                {
                    emit(WorkerEvent::Error {
                        text: i18n_f("Could not mark {len} messages as not spam: {e}", &[("len", &(uids.len()).to_string()), ("e", &(e).to_string())]),
                        connectivity: false,
                    });
                }
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::MoveMessages { path, uids, dest } => {
                if let Err(e) =
                    graph_move_uids(&account, account_id, &mut state, &path, &uids, &dest, cache.as_ref())
                        .await
                {
                    emit(WorkerEvent::Error {
                        text: i18n_f("Could not move messages: {e}", &[("e", &(e).to_string())]),
                        connectivity: false,
                    });
                }
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::PurgeMessages { path, uids } => {
                graph_purge_uids(&account, account_id, &mut state, &path, uids, cache.as_ref(), &emit).await;
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::EmptyFolder { folder_id, path } => {
                // No bulk endpoint for an arbitrary folder: list it, then
                // delete each message the way a purge does.
                let Some(token) = graph_token(&account, &emit).await else { continue };
                match graph_load_folder(&token, account_id, folder_id, &path, cache.as_ref(), &mut state).await {
                    Ok(messages) => {
                        let uids: Vec<u32> = messages.iter().map(|m| m.uid).collect();
                        let n = uids.len();
                        graph_purge_uids(&account, account_id, &mut state, &path, uids, cache.as_ref(), &emit).await;
                        tracing::info!("emptied {path}: {n} message(s) erased");
                        emit(WorkerEvent::Messages { folder_id, messages: Vec::new() });
                        emit(WorkerEvent::FolderUnread { folder_id, unread: 0 });
                    }
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Could not empty the folder: {e}", &[("e", &e.to_string())]),
                        connectivity: false,
                    }),
                }
            }

            MailRequest::UndoMove { path, dest, dest_folder_id, message_ids } => {
                match graph_undo_move(&account, account_id, &mut state, &path, &dest, &message_ids, cache.as_ref())
                    .await
                {
                    Ok(0) => {
                        tracing::info!("undo: the messages are no longer where that move put them");
                    }
                    Ok(_) => {
                        // Reload the restored folder so the messages reappear.
                        if let Some(token) = graph_token(&account, &emit).await {
                            if let Ok(messages) = graph_load_folder(
                                &token,
                                account_id,
                                dest_folder_id,
                                &dest,
                                cache.as_ref(),
                                &mut state,
                            )
                            .await
                            {
                                let unread = state.chip_count(dest_folder_id, &messages);
                                emit(WorkerEvent::Messages {
                                    folder_id: dest_folder_id,
                                    messages,
                                });
                                emit(WorkerEvent::FolderUnread {
                                    folder_id: dest_folder_id,
                                    unread,
                                });
                            }
                        }
                    }
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Undo failed: {e}", &[("e", &(e).to_string())]),
                        connectivity: false,
                    }),
                }
            }

            MailRequest::CreateFolder { path } => {
                let Some(token) = graph_token(&account, &emit).await else { continue };
                // "A/B" nests under A (resolved from the last listing);
                // otherwise a top-level folder.
                let (url, name) = match path.rsplit_once('/') {
                    Some((parent, leaf)) => match state.folders.get(parent) {
                        Some((_, pgid)) => (
                            format!("{GRAPH_BASE}/me/mailFolders/{pgid}/childFolders"),
                            leaf.to_string(),
                        ),
                        None => (format!("{GRAPH_BASE}/me/mailFolders"), path.clone()),
                    },
                    None => (format!("{GRAPH_BASE}/me/mailFolders"), path.clone()),
                };
                let t = token.clone();
                let body = serde_json::json!({ "displayName": name });
                let r = tokio::task::spawn_blocking(move || graph_send_json(&t, "POST", &url, &body))
                    .await
                    .unwrap_or_else(|_| Err("task failed".into()));
                match r {
                    Ok(_) => {
                        refresh_graph_folders(&token, account_id, cache.as_ref(), &mut state, &emit)
                            .await;
                    }
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Could not create folder: {e}", &[("e", &(e).to_string())]),
                        connectivity: false,
                    }),
                }
            }

            MailRequest::RenameFolder { old_path, new_path } => {
                let Some(token) = graph_token(&account, &emit).await else { continue };
                let Some((_, gid)) = state.folders.get(&old_path).cloned() else {
                    emit(WorkerEvent::Error {
                        text: i18n("Could not rename folder: unknown folder"),
                        connectivity: false,
                    });
                    continue;
                };
                // Graph renames by displayName; moving between parents would be
                // a different call — the sidebar only renames leaves here.
                let leaf = new_path.rsplit('/').next().unwrap_or(&new_path).to_string();
                let t = token.clone();
                let url = format!("{GRAPH_BASE}/me/mailFolders/{gid}");
                let body = serde_json::json!({ "displayName": leaf });
                let r =
                    tokio::task::spawn_blocking(move || graph_send_json(&t, "PATCH", &url, &body))
                        .await
                        .unwrap_or_else(|_| Err("task failed".into()));
                match r {
                    Ok(_) => {
                        refresh_graph_folders(&token, account_id, cache.as_ref(), &mut state, &emit)
                            .await;
                    }
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Could not rename folder: {e}", &[("e", &(e).to_string())]),
                        connectivity: false,
                    }),
                }
            }

            MailRequest::DeleteFolder { path, trash: _ } => {
                // Graph's folder delete moves the folder (contents included) to
                // Deleted Items itself; no separate content move needed.
                let Some(token) = graph_token(&account, &emit).await else { continue };
                let Some((_, gid)) = state.folders.get(&path).cloned() else {
                    emit(WorkerEvent::Error {
                        text: i18n("Could not delete folder: unknown folder"),
                        connectivity: false,
                    });
                    continue;
                };
                let t = token.clone();
                let url = format!("{GRAPH_BASE}/me/mailFolders/{gid}");
                let r = tokio::task::spawn_blocking(move || graph_delete_req(&t, &url))
                    .await
                    .unwrap_or_else(|_| Err("task failed".into()));
                match r {
                    Ok(()) => {
                        refresh_graph_folders(&token, account_id, cache.as_ref(), &mut state, &emit)
                            .await;
                    }
                    Err(e) => emit(WorkerEvent::Error {
                        text: i18n_f("Could not delete folder: {e}", &[("e", &(e).to_string())]),
                        connectivity: false,
                    }),
                }
            }

            MailRequest::SaveDraft { message, folder_id, path } => {
                emit(WorkerEvent::Status(i18n("Saving draft…")));
                let message = OutgoingMessage { sign: false, encrypt: false, ..*message };
                let saved = match build_draft(&account, &message) {
                    Ok(email) => {
                        let raw = email.formatted();
                        match graph_token(&account, &emit).await {
                            Some(token) => {
                                let t = token.clone();
                                let url = format!("{GRAPH_BASE}/me/messages");
                                let r = tokio::task::spawn_blocking(move || {
                                    graph_post_mime(&t, &url, &raw)
                                })
                                .await
                                .unwrap_or_else(|_| Err("task failed".into()));
                                match r {
                                    Ok(_) => {
                                        // Replace the previous version of this draft.
                                        if let Some(o) = &message.draft_origin {
                                            if o.account_id == account_id {
                                                if let Some((tok, gid)) = graph_resolve(
                                                    &account, &mut state, &o.path, o.uid, &emit,
                                                )
                                                .await
                                                {
                                                    let url =
                                                        format!("{GRAPH_BASE}/me/messages/{gid}");
                                                    let _ = tokio::task::spawn_blocking(move || {
                                                        graph_delete_req(&tok, &url)
                                                    })
                                                    .await;
                                                }
                                                if let Some(c) = cache.as_ref() {
                                                    c.delete_message(account_id, &o.path, o.uid);
                                                }
                                            }
                                        }
                                        if let Ok(messages) = graph_load_folder(
                                            &token,
                                            account_id,
                                            folder_id,
                                            &path,
                                            cache.as_ref(),
                                            &mut state,
                                        )
                                        .await
                                        {
                                            emit(WorkerEvent::Messages { folder_id, messages });
                                        }
                                        true
                                    }
                                    Err(e) => {
                                        emit(WorkerEvent::Error {
                                            text: i18n_f("Could not save draft: {e}", &[("e", &(e).to_string())]),
                                            connectivity: false,
                                        });
                                        false
                                    }
                                }
                            }
                            None => false,
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::Error {
                            text: i18n_f("Could not save draft: {e}", &[("e", &(e).to_string())]),
                            connectivity: false,
                        });
                        false
                    }
                };
                emit(WorkerEvent::Status(String::new()));
                if saved {
                    emit(WorkerEvent::DraftSaved);
                }
            }

            // `sent_path` is unused: Graph's sendMail files the Sent copy itself.
            // Send Later (#145): into the Outbox until its time.
            MailRequest::Send { message, sent_path: _ }
                if message.send_at.is_some_and(|t| t > crate::datefmt::now()) =>
            {
                let at = message.send_at.unwrap_or_default();
                schedule_send(cache.as_ref(), account_id, &account, &message, None, at, &emit);
                if let Some(o) = message.draft_origin.clone() {
                    if o.account_id == account_id {
                        if let Some((tok, gid)) =
                            graph_resolve(&account, &mut state, &o.path, o.uid, &emit).await
                        {
                            let url = format!("{GRAPH_BASE}/me/messages/{gid}");
                            let _ = tokio::task::spawn_blocking(move || graph_delete_req(&tok, &url)).await;
                        }
                        if let Some(c) = cache.as_ref() {
                            c.delete_message(account_id, &o.path, o.uid);
                        }
                    }
                }
            }

            MailRequest::Send { message, sent_path: _ } => {
                emit(WorkerEvent::Status(i18n("Sending…")));
                match graph_send_message(&account, &message, &emit).await {
                    Ok(()) => {
                        emit(WorkerEvent::Status(String::new()));
                        // If sending an edited draft, remove the obsolete draft.
                        if let Some(o) = message.draft_origin.clone() {
                            if o.account_id == account_id {
                                if let Some((tok, gid)) =
                                    graph_resolve(&account, &mut state, &o.path, o.uid, &emit).await
                                {
                                    let url = format!("{GRAPH_BASE}/me/messages/{gid}");
                                    let _ = tokio::task::spawn_blocking(move || {
                                        graph_delete_req(&tok, &url)
                                    })
                                    .await;
                                }
                                if let Some(c) = cache.as_ref() {
                                    c.delete_message(account_id, &o.path, o.uid);
                                }
                                if let Some(token) = graph_token(&account, &emit).await {
                                    if let Ok(messages) = graph_load_folder(
                                        &token,
                                        account_id,
                                        o.folder_id,
                                        &o.path,
                                        cache.as_ref(),
                                        &mut state,
                                    )
                                    .await
                                    {
                                        emit(WorkerEvent::Messages {
                                            folder_id: o.folder_id,
                                            messages,
                                        });
                                    }
                                }
                            }
                        }
                        if let (Some(queued), Some(c)) = (message.outbox_origin, cache.as_ref()) {
                            c.delete_outbox(queued);
                            emit_outbox(cache.as_ref(), account_id, &emit);
                        }
                        // The server files the copy in Sent Items itself; list
                        // that folder now so the copy is in the cache and can
                        // join the conversation it answers (#199).
                        let sent = cache
                            .as_ref()
                            .and_then(|c| c.load_folders(account_id).into_iter().find(|f| f.kind == FolderKind::Sent));
                        if let (Some(c), Some(sent)) = (cache.as_ref(), sent) {
                            if let Some(token) = graph_token(&account, &emit).await {
                                if let Ok(messages) = graph_load_folder(
                                    &token, account_id, sent.id, &sent.path, cache.as_ref(), &mut state,
                                )
                                .await
                                {
                                    c.upsert_messages(account_id, &sent.path, &messages);
                                    emit(WorkerEvent::Messages { folder_id: sent.id, messages });
                                }
                            }
                        }
                        emit(WorkerEvent::Sent);
                    }
                    Err(e) => {
                        emit(WorkerEvent::Status(String::new()));
                        let queued = queue_failed_send(
                            cache.as_ref(),
                            account_id,
                            &account,
                            &message,
                            None,
                            &e,
                        );
                        if let (true, Some(old), Some(c)) =
                            (queued, message.outbox_origin, cache.as_ref())
                        {
                            c.delete_outbox(old);
                        }
                        emit(WorkerEvent::Error {
                            text: if queued {
                                i18n_f("Send failed: {e}. The message is in the Outbox and will be sent when the connection is back.", &[("e", &e.to_string())])
                            } else {
                                i18n_f("Send failed: {e}", &[("e", &e.to_string())])
                            },
                            connectivity: false,
                        });
                        emit_outbox(cache.as_ref(), account_id, &emit);
                    }
                }
            }

            MailRequest::LoadOutbox => emit_outbox(cache.as_ref(), account_id, &emit),

            MailRequest::DeleteOutbox { id } => {
                if let Some(c) = cache.as_ref() {
                    c.delete_outbox(id);
                }
                emit_outbox(cache.as_ref(), account_id, &emit);
            }

            MailRequest::FlushOutbox { id } => {
                graph_flush_outbox(cache.as_ref(), account_id, &account, id, &emit).await;
            }

            MailRequest::RefreshUnread => {
                // Quiet token fetch: this rides the auto-fetch tick, and a
                // signed-out account already errors on interactive actions.
                let quiet = |_: WorkerEvent| {};
                if let Some(token) = graph_token(&account, &quiet).await {
                    graph_refresh_unread(&token, account_id, cache.as_ref(), &mut state, &emit)
                        .await;
                    auto_empty_graph(&token, account_id, &account, cache.as_ref(), &mut state, &mut last_auto_empty, &emit)
                        .await;
                }
            }

            MailRequest::Reconnect => {
                if let Some(token) = graph_token(&account, &emit).await {
                    refresh_graph_folders(&token, account_id, cache.as_ref(), &mut state, &emit)
                        .await;
                }
            }
        }
    }
}

/// One poll tick: refresh the Inbox and emit it. The app diffs the arriving
/// list against its cache, so this drives both the visible refresh and the
/// desktop notification for genuinely new mail. Token failures stay quiet here
/// — a signed-out account already errors on every interactive action.
async fn graph_poll_inbox(
    account_id: u32,
    account: &AccountConfig,
    cache: Option<&Cache>,
    state: &mut GraphState,
    emit: &impl Fn(WorkerEvent),
) {
    let quiet = |_: WorkerEvent| {};
    let Some(token) = graph_token(account, &quiet).await else { return };
    if state.inbox.is_none() {
        // The startup folder listing may have failed (offline launch).
        refresh_graph_folders(&token, account_id, cache, state, emit).await;
    }
    let Some((folder_id, path)) = state.inbox.clone() else { return };
    if let Ok(messages) =
        graph_load_folder(&token, account_id, folder_id, &path, cache, state).await
    {
        let unread = messages.iter().filter(|m| m.unread).count() as u32;
        emit_graph_body_hits(&token, account, folder_id, &path, &messages, state, emit).await;
        emit(WorkerEvent::Messages { folder_id, messages });
        emit(WorkerEvent::FolderUnread { folder_id, unread });
    }
    // The poll only re-syncs the inbox; mail filed by server-side rules lands
    // in other folders without passing through it. Re-list for every folder's
    // unreadItemCount so their chips keep pace too.
    graph_refresh_unread(&token, account_id, cache, state, emit).await;
}

/// Re-list the folders for their server-side unread counts and push them, but
/// stay silent on failure — this runs on the background poll, where a transient
/// network error is not worth a banner (unlike [`refresh_graph_folders`]).
///
/// Emits the merged folder list first (so a renamed/new folder gets fresh ids),
/// then one [`WorkerEvent::FolderUnread`] per folder: the per-folder event is
/// the path allowed to assert a genuine zero, which the app's SetFolders merge
/// deliberately ignores.
async fn graph_refresh_unread(
    token: &str,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut GraphState,
    emit: &impl Fn(WorkerEvent),
) {
    let t = token.to_string();
    let roles = state.roles.clone();
    let Ok(list) = tokio::task::spawn_blocking(move || graph_list_folders(&t, account_id, &roles))
        .await
        .unwrap_or_else(|_| Err("task failed".into()))
    else {
        return;
    };
    if list.is_empty() {
        return;
    }
    state.adopt_folders(&list);
    let folders: Vec<Folder> = list.into_iter().map(|f| f.folder).collect();
    if let Some(c) = cache {
        c.save_folders(account_id, &folders);
    }
    let counts: Vec<(u32, u32)> = folders.iter().map(|f| (f.id, f.unread)).collect();
    emit(WorkerEvent::Folders(folders));
    for (folder_id, unread) in counts {
        emit(WorkerEvent::FolderUnread { folder_id, unread });
    }
}

/// Delete messages for good, one Graph request each; the cache and the uid
/// map forget every one that went.
async fn graph_purge_uids(
    account: &AccountConfig,
    account_id: u32,
    state: &mut GraphState,
    path: &str,
    uids: Vec<u32>,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    for uid in uids {
        let deleted = match graph_resolve(account, state, path, uid, emit).await {
            Some((token, gid)) => {
                let url = format!("{GRAPH_BASE}/me/messages/{gid}");
                tokio::task::spawn_blocking(move || graph_delete_req(&token, &url))
                    .await
                    .unwrap_or_else(|_| Err("task failed".into()))
                    .is_ok()
            }
            None => false,
        };
        if deleted {
            state.uids.remove(&uid);
            if let Some(c) = cache {
                c.delete_message(account_id, path, uid);
            }
        }
    }
}

/// A fresh GOA token, or a user-actionable error.
async fn graph_token(account: &AccountConfig, emit: &impl Fn(WorkerEvent)) -> Option<String> {
    match fetch_oauth_token(account).await {
        Some(t) => Some(t),
        None => {
            emit(WorkerEvent::Error {
                text: format!(
                    "GNOME Online Accounts could not provide a sign-in token for {}. Open \
                     Settings → Online Accounts and sign in again.",
                    account.email
                ),
                connectivity: true,
            });
            None
        }
    }
}

/// Re-list the folders, emit them, remember the path → Graph-id map.
async fn refresh_graph_folders(
    token: &str,
    account_id: u32,
    cache: Option<&Cache>,
    state: &mut GraphState,
    emit: &impl Fn(WorkerEvent),
) {
    let t = token.to_string();
    let roles = state.roles.clone();
    let r = tokio::task::spawn_blocking(move || graph_list_folders(&t, account_id, &roles))
        .await
        .unwrap_or_else(|_| Err("task failed".into()));
    match r {
        Ok(list) => {
            state.adopt_folders(&list);
            let folders: Vec<Folder> = list.into_iter().map(|f| f.folder).collect();
            if let Some(c) = cache {
                c.save_folders(account_id, &folders);
            }
            emit(WorkerEvent::Folders(folders));
        }
        Err(e) => emit(WorkerEvent::Error {
            text: i18n_f("Could not list folders: {e}", &[("e", &(e).to_string())]),
            connectivity: true,
        }),
    }
}

/// List a folder's summaries, refresh the uid map and the cache.
async fn graph_load_folder(
    token: &str,
    account_id: u32,
    folder_id: u32,
    path: &str,
    cache: Option<&Cache>,
    state: &mut GraphState,
) -> Result<Vec<Message>, String> {
    // An unknown path usually means the folder list hasn't been fetched yet
    // (or the folder is new) — refresh it once before giving up.
    if !state.folders.contains_key(path) {
        let t = token.to_string();
        let roles = state.roles.clone();
        if let Ok(list) =
            tokio::task::spawn_blocking(move || graph_list_folders(&t, account_id, &roles))
                .await
                .unwrap_or_else(|_| Err("task failed".into()))
        {
            state.adopt_folders(&list);
        }
    }
    let (_, gid) = state
        .folders
        .get(path)
        .cloned()
        .ok_or_else(|| format!("unknown folder {path}"))?;

    let t = token.to_string();
    let listed = tokio::task::spawn_blocking(move || {
        graph_list_messages(&t, &gid, account_id, folder_id)
    })
    .await
    .unwrap_or_else(|_| Err("task failed".into()))?;

    let mut messages = Vec::with_capacity(listed.len());
    for (m, gid) in listed {
        state.uids.insert(m.uid, gid);
        messages.push(m);
    }
    if let Some(c) = cache {
        c.save_messages(account_id, path, &messages);
    }
    Ok(messages)
}

/// Resolve a message uid to (token, Graph id), re-listing the folder once if
/// the uid isn't in the map (fresh start from cache, or a moved message).
async fn graph_resolve(
    account: &AccountConfig,
    state: &mut GraphState,
    path: &str,
    uid: u32,
    emit: &impl Fn(WorkerEvent),
) -> Option<(String, String)> {
    let token = graph_token(account, emit).await?;
    if let Some(gid) = state.uids.get(&uid) {
        return Some((token, gid.clone()));
    }
    // Not indexed yet: list the folder (fills the uid map) and try again. The
    // account/folder ids only label the discarded summaries, so zeros are fine.
    let _ = graph_load_folder(&token, 0, 0, path, None, state).await;
    state.uids.get(&uid).map(|gid| (token, gid.clone()))
}

/// Fetch a message's raw RFC 822 bytes.
async fn graph_fetch_raw(
    account: &AccountConfig,
    state: &mut GraphState,
    path: &str,
    uid: u32,
    emit: &impl Fn(WorkerEvent),
) -> Result<Vec<u8>, String> {
    let (token, gid) = graph_resolve(account, state, path, uid, emit)
        .await
        .ok_or_else(|| "message not found".to_string())?;
    let url = format!("{GRAPH_BASE}/me/messages/{gid}/$value");
    tokio::task::spawn_blocking(move || graph_get_bytes(&token, &url))
        .await
        .unwrap_or_else(|_| Err("task failed".into()))
}

/// PATCH one message (read state, flag). Errors are logged, not surfaced — the
/// optimistic UI state already changed and a stale flag is not worth a dialog.
async fn graph_patch_message(
    account: &AccountConfig,
    state: &mut GraphState,
    path: &str,
    uid: u32,
    body: serde_json::Value,
    emit: &impl Fn(WorkerEvent),
) {
    let Some((token, gid)) = graph_resolve(account, state, path, uid, emit).await else {
        return;
    };
    let url = format!("{GRAPH_BASE}/me/messages/{gid}");
    let r = tokio::task::spawn_blocking(move || graph_send_json(&token, "PATCH", &url, &body))
        .await
        .unwrap_or_else(|_| Err("task failed".into()));
    if let Err(e) = r {
        tracing::warn!("graph: could not update message flags: {e}");
    }
}

/// Move messages to another folder. The Graph id changes in transit; the moved
/// entries leave the uid map and the destination re-indexes on its next load.
async fn graph_move_uids(
    account: &AccountConfig,
    account_id: u32,
    state: &mut GraphState,
    path: &str,
    uids: &[u32],
    dest: &str,
    cache: Option<&Cache>,
) -> Result<(), String> {
    let quiet = |_e: WorkerEvent| {};
    let token = graph_token(account, &quiet)
        .await
        .ok_or_else(|| "no sign-in token from GNOME Online Accounts".to_string())?;
    let dest_gid = match state.folders.get(dest) {
        Some((_, gid)) => gid.clone(),
        None => return Err(format!("unknown folder {dest}")),
    };
    for &uid in uids {
        let Some(gid) = state.uids.get(&uid).cloned() else { continue };
        let t = token.clone();
        let url = format!("{GRAPH_BASE}/me/messages/{gid}/move");
        let body = serde_json::json!({ "destinationId": dest_gid });
        tokio::task::spawn_blocking(move || graph_send_json(&t, "POST", &url, &body))
            .await
            .unwrap_or_else(|_| Err("task failed".into()))?;
        state.uids.remove(&uid);
        if let Some(c) = cache {
            c.delete_message(account_id, path, uid);
        }
    }
    Ok(())
}

/// Undo a move: the messages are in `path` (where the move put them) with new
/// Graph ids — find them by Internet Message-ID and move them back to `dest`.
async fn graph_undo_move(
    account: &AccountConfig,
    account_id: u32,
    state: &mut GraphState,
    path: &str,
    dest: &str,
    message_ids: &[String],
    cache: Option<&Cache>,
) -> Result<usize, String> {
    let quiet = |_e: WorkerEvent| {};
    let token = graph_token(account, &quiet)
        .await
        .ok_or_else(|| "no sign-in token from GNOME Online Accounts".to_string())?;
    let (folder_id, gid) = state
        .folders
        .get(path)
        .cloned()
        .ok_or_else(|| format!("unknown folder {path}"))?;
    let wanted: std::collections::HashSet<&str> =
        message_ids.iter().map(|s| s.as_str()).collect();

    let t = token.clone();
    let listed = tokio::task::spawn_blocking(move || {
        graph_list_messages(&t, &gid, account_id, folder_id)
    })
    .await
    .unwrap_or_else(|_| Err("task failed".into()))?;

    let mut uids = Vec::new();
    for (m, mgid) in listed {
        if wanted.contains(m.message_id.as_str()) {
            state.uids.insert(m.uid, mgid);
            uids.push(m.uid);
        }
    }
    if uids.is_empty() {
        return Ok(0);
    }
    let n = uids.len();
    graph_move_uids(account, account_id, state, path, &uids, dest, cache).await?;
    Ok(n)
}

/// Send over Graph: `sendMail` takes the same raw MIME `build_email` produces
/// and files the Sent copy itself.
async fn graph_send_message(
    account: &AccountConfig,
    message: &OutgoingMessage,
    emit: &impl Fn(WorkerEvent),
) -> Result<(), String> {
    let email = build_email(account, message).map_err(|e| e.to_string())?;
    let raw = email.formatted();
    let token = graph_token(account, emit)
        .await
        .ok_or_else(|| "no sign-in token from GNOME Online Accounts".to_string())?;
    let url = format!("{GRAPH_BASE}/me/sendMail");
    tokio::task::spawn_blocking(move || graph_post_mime(&token, &url, &raw))
        .await
        .unwrap_or_else(|_| Err("task failed".into()))?;
    Ok(())
}

/// The Outbox retry loop for Graph accounts: same queue and bookkeeping as
/// [`flush_outbox`], with `sendMail` as the transport (Sent copy automatic).
async fn graph_flush_outbox(
    cache: Option<&Cache>,
    account_id: u32,
    account: &AccountConfig,
    id: Option<u32>,
    emit: &impl Fn(WorkerEvent),
) {
    let Some(cache) = cache else { return };
    let now = crate::datefmt::now();
    let items: Vec<crate::models::OutboxItem> = cache
        .outbox_items(account_id)
        .into_iter()
        .filter(|item| match id {
            Some(wanted) => wanted == item.id,
            None => item.send_at.is_none_or(|t| t <= now),
        })
        .collect();
    if items.is_empty() {
        return;
    }
    emit(WorkerEvent::Status(i18n("Sending…")));
    let Some(token) = graph_token(account, emit).await else {
        emit(WorkerEvent::Status(String::new()));
        return;
    };
    let mut sent_any = false;
    for item in items {
        let t = token.clone();
        let raw = item.raw.clone();
        let url = format!("{GRAPH_BASE}/me/sendMail");
        let r = tokio::task::spawn_blocking(move || graph_post_mime(&t, &url, &raw))
            .await
            .unwrap_or_else(|_| Err("task failed".into()));
        match r {
            Ok(_) => {
                sent_any = true;
                cache.delete_outbox(item.id);
            }
            Err(e) => {
                cache.record_outbox_failure(item.id, &e);
                emit(WorkerEvent::Error {
                    text: i18n_f("Still could not send “{subject}”: {e}", &[("subject", &item.subject.to_string()), ("e", &e.to_string())]),
                    connectivity: false,
                });
                break;
            }
        }
    }
    emit(WorkerEvent::Status(String::new()));
    if sent_any {
        emit(WorkerEvent::Sent);
    }
    emit_outbox(Some(cache), account_id, emit);
}

#[cfg(test)]
mod tests {

    use super::*;

    fn flagged_message(uid: u32) -> Message {
        Message {
            id: uid,
            account_id: 7,
            folder_id: 1,
            uid,
            from_name: String::new(),
            from_addr: String::new(),
            reply_to: String::new(),
            to: String::new(),
            cc: String::new(),
            subject: String::new(),
            preview: String::new(),
            body: String::new(),
            date: String::new(),
            timestamp: 0,
            unread: true,
            starred: false,
            keywords: Vec::new(),
            has_attachment: true,
            message_id: String::new(),
            references: String::new(),
        }
    }

    /// The loop behind the 2.6 GB report: a message the attachment prefetch
    /// popped but could not finish (OpenPGP, or a read that would not parse)
    /// was queued again by the resync the prefetch triggers, and fetched
    /// again, forever. One attempt per session is all it gets now.
    #[test]
    fn attachment_prefetch_never_requeues_a_message_it_already_tried() {
        let msgs = vec![flagged_message(2383), flagged_message(2372)];
        let mut queue = std::collections::VecDeque::new();
        queue_attachment_prefetch(&mut queue, "INBOX", &msgs, None, 7);
        assert_eq!(queue.len(), 2, "both flagged rows are queued the first time");

        // The prefetch pops 2383 and notes the attempt, as run_one_prefetch does
        // before anything can go wrong with the read.
        queue.pop_front();
        note_attachment_prefetch_tried(7, "INBOX", 2383);
        assert!(attachment_prefetch_tried(7, "INBOX", 2383));

        // The resync that follows sees the same rows: 2383 must stay out.
        queue_attachment_prefetch(&mut queue, "INBOX", &msgs, None, 7);
        assert_eq!(queue.iter().map(|(_, u)| *u).collect::<Vec<_>>(), vec![2372]);

        // Another account's row with the same uid and path is its own attempt.
        assert!(!attachment_prefetch_tried(8, "INBOX", 2383));
    }

    fn sample_account() -> AccountConfig {
        AccountConfig {
            folder_roles: Default::default(),
            sent_copy_path: None,
            server_saves_sent: false,
            empty_junk_days: 0,
            empty_trash_days: 0,
            pgp_key: None,
            push: None,
            name: String::new(),
            email: "me@example.com".into(),
            protocol: crate::config::Protocol::Imap,
            imap_host: "imap.example.com".into(),
            imap_port: 993,
            smtp_host: String::new(),
            smtp_port: 587,
            username: "me@example.com".into(),
            password: String::new(),
            smtp_separate: false,
            smtp_username: String::new(),
            smtp_password: String::new(),
            color: None,
            emoji: None,
            avatar: None,
            gravatar: false,
            signature: None,
            signature_html: false,
            label: None,
            aliases: Vec::new(),
            enabled: true,
            goa_id: None,
            goa_mail_disabled: false,
            goa_enabled_before_mail_disabled: true,
            oauth: false,
            oauth_settings: None,
            oauth_refresh: String::new(),
        }
    }

    fn sample_outgoing() -> OutgoingMessage {
        OutgoingMessage {
            from_account_id: 1,
            from_alias: None,
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            reply_to: String::new(),
            subject: "Subject".into(),
            body: "Body".into(),
            html: String::new(),
            attachments: Vec::new(),
            in_reply_to: String::new(),
            references: String::new(),
            draft_origin: None,
            outbox_origin: None,
            sign: false,
            encrypt: false,
            send_at: None,
        }
    }

    /// A draft saved before any address is typed: the bytes build without a
    /// To header, and the send path still refuses the same message.
    #[test]
    fn recipient_less_draft_builds_but_send_does_not() {
        let account = sample_account();
        let mut msg = sample_outgoing();
        msg.body = "half-written".into();
        let raw = build_draft(&account, &msg).expect("draft builds").formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(!text.contains("\r\nTo:"), "no To header: {text}");
        assert!(text.contains("half-written"), "{text}");
        let err = build_email(&account, &msg).expect_err("send needs a recipient").to_string();
        assert!(err.contains("missing destination"), "{err}");
        // With a recipient the draft is built the ordinary way.
        msg.to = "Peer <peer@vireo.invalid>".into();
        let raw = build_draft(&account, &msg).expect("builds").formatted();
        assert!(String::from_utf8_lossy(&raw).contains("peer@vireo.invalid"));
    }

    /// OpenPGP sending (#133), end to end against a real gpg in a throwaway
    /// keyring: a signed message comes out as PGP/MIME multipart/signed whose
    /// signature the reader's own path verifies over lettre's exact bytes; an
    /// encrypted one as multipart/encrypted that decrypts back to the body.
    /// Skipped where gpg is missing.
    #[test]
    fn build_email_signs_and_encrypts_with_pgp_mime() {
        if !crate::pgp::available() {
            eprintln!("gpg not installed; skipping");
            return;
        }
        let home = std::env::temp_dir().join(format!("vireo-gpg-send-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        // `Gpg::system()` (what build_email uses) honours this override.
        std::env::set_var("VIREO_GNUPGHOME", &home);
        let gpg = crate::pgp::Gpg::system();
        let mut account = sample_account();
        account.email = "sender@vireo.invalid".into();
        let fpr = crate::pgp::generate_key(&gpg, "Sender", "sender@vireo.invalid", "never", "").expect("own key");
        crate::pgp::generate_key(&gpg, "Peer", "peer@vireo.invalid", "never", "").expect("peer key");

        let mut msg = sample_outgoing();
        msg.to = "Peer <peer@vireo.invalid>".into();
        msg.body = "signed body text".into();
        msg.sign = true;
        let raw = build_email(&account, &msg).expect("builds").formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("multipart/signed"), "{text}");
        assert!(text.contains("micalg=\"pgp-sha"), "{text}");
        let u = crate::pgp::unwrap_message(&raw, &gpg).expect("recognised");
        assert!(matches!(u.status.signature, crate::models::PgpSignature::Good { .. }), "{:?}", u.status);
        assert!(!u.status.encrypted);
        let inner = String::from_utf8_lossy(&u.inner.unwrap()).to_string();
        assert!(inner.contains("signed body text"), "{inner}");

        // With an attachment the signed entity is the whole mixed multipart.
        let path = home.join("note.txt");
        std::fs::write(&path, b"attached").unwrap();
        msg.attachments = vec![path.to_string_lossy().to_string()];
        let raw = build_email(&account, &msg).expect("builds").formatted();
        let u = crate::pgp::unwrap_message(&raw, &gpg).expect("recognised");
        assert!(matches!(u.status.signature, crate::models::PgpSignature::Good { .. }), "{:?}", u.status);
        assert!(!extract_attachments(&u.inner.unwrap()).is_empty());

        // Encrypted and signed: readable by the peer and by the sender.
        msg.attachments.clear();
        msg.encrypt = true;
        msg.body = "secret body text".into();
        let raw = build_email(&account, &msg).expect("builds").formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("multipart/encrypted"), "{text}");
        assert!(!text.contains("secret body text"), "plaintext must not appear: {text}");
        let u = crate::pgp::unwrap_message(&raw, &gpg).expect("recognised");
        assert!(u.status.encrypted && u.status.decrypted, "{:?}", u.status);
        assert!(matches!(u.status.signature, crate::models::PgpSignature::Good { .. }), "{:?}", u.status);
        let inner = String::from_utf8_lossy(&u.inner.unwrap()).to_string();
        assert!(inner.contains("secret body text"), "{inner}");

        // A recipient without a key is refused by name; a From without a key too.
        msg.cc = "nobody@vireo.invalid".into();
        let err = build_email(&account, &msg).expect_err("no key for nobody").to_string();
        assert!(err.contains("nobody@vireo.invalid"), "{err}");
        msg.cc.clear();
        account.email = "keyless@vireo.invalid".into();
        let err = build_email(&account, &msg).expect_err("no own key").to_string();
        assert!(err.contains("keyless@vireo.invalid"), "{err}");
        // The account's chosen key overrides the address match.
        account.pgp_key = Some(fpr);
        build_email(&account, &msg).expect("chosen key signs");

        // A draft of the same message stays plain.
        let plain = OutgoingMessage { sign: false, encrypt: false, ..msg.clone() };
        let raw = build_email(&account, &plain).expect("builds").formatted();
        assert!(String::from_utf8_lossy(&raw).contains("secret body text"));

        std::env::remove_var("VIREO_GNUPGHOME");
        let _ = std::process::Command::new("gpgconf").env("GNUPGHOME", &home).args(["--kill", "all"]).status();
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A reply with no In-Reply-To/References is a new conversation to every
    /// client that receives it — including Vireo's own threading.
    #[test]
    fn reply_to_header_reaches_the_wire() {
        let mut msg = sample_outgoing();
        msg.to = "someone@example.com".into();
        msg.reply_to = "list@example.org, Other <other@example.net>".into();
        let email = build_email(&sample_account(), &msg).expect("builds");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(raw.contains("Reply-To:"), "Reply-To must be present: {raw}");
        assert!(raw.contains("list@example.org"), "{raw}");
        assert!(raw.contains("other@example.net"), "{raw}");
    }

    #[test]
    fn a_reply_carries_its_threading_headers() {
        let mut msg = sample_outgoing();
        msg.to = "someone@example.com".into();
        msg.in_reply_to = "parent@example.com".into();
        msg.references = "root@example.com parent@example.com".into();
        let email = build_email(&sample_account(), &msg).expect("builds");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(
            raw.contains("In-Reply-To: <parent@example.com>"),
            "In-Reply-To must be present and bracketed: {raw}"
        );
        assert!(
            raw.contains("References: <root@example.com> <parent@example.com>"),
            "References must carry the whole chain, bracketed: {raw}"
        );
    }

    /// A message that starts a conversation must not carry empty headers.
    /// Without a Message-ID of our own the server assigns one on the way out, so
    /// the copy filed in Sent has none — and nothing that replies to it can be
    /// linked back to it. It must also be in the sender's domain, not this
    /// machine's hostname.
    #[test]
    fn outgoing_mail_carries_a_message_id_in_the_senders_domain() {
        let account = AccountConfig { email: "me@example.com".into(), ..sample_account() };
        let msg = OutgoingMessage { to: "you@example.org".into(), ..sample_outgoing() };
        let raw = String::from_utf8_lossy(
            &build_email(&account, &msg).expect("builds").formatted(),
        )
        .to_string();
        let line = raw
            .lines()
            .find(|l| l.starts_with("Message-ID:"))
            .expect(&format!("a Message-ID is set: {raw}"));
        assert!(line.contains("@example.com>"), "in the sender's domain: {line}");
    }

    #[test]
    fn a_new_message_carries_no_threading_headers() {
        let msg = OutgoingMessage { to: "someone@example.com".into(), ..sample_outgoing() };
        let email = build_email(&sample_account(), &msg).expect("builds");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(!raw.contains("In-Reply-To"), "{raw}");
        assert!(!raw.contains("References"), "{raw}");
    }

    #[test]
    fn mailbox_quotes_display_names_that_are_not_atoms() {
        // Each of these fails if the mailbox is built by formatting
        // "Name <addr>" and parsing the result back.
        for name in ["Alfonso Lizárraga", "Martin, Jason", "Dr. X", "a@b.com"] {
            let mb = mailbox(name, "a@b.com").expect("mailbox builds");
            assert_eq!(mb.email.to_string(), "a@b.com");
        }
    }

    #[test]
    fn mailbox_drops_a_name_that_merely_repeats_the_address() {
        // An import with no separate display name sets both to the address;
        // "a@b.com <a@b.com>" is noise, so the header carries the address alone.
        let mb = mailbox("A@B.com", "a@b.com").expect("mailbox builds");
        assert!(mb.name.is_none());
        assert_eq!(mb.to_string(), "a@b.com");
    }

    fn queued(from: &str, rcpts: &[&str]) -> crate::models::OutboxItem {
        crate::models::OutboxItem {
            id: 1,
            account_id: 1,
            from_addr: from.into(),
            rcpts: rcpts.iter().map(|s| s.to_string()).collect(),
            recipients: String::new(),
            subject: "Notes".into(),
            preview: String::new(),
            raw: b"From: me\r\n\r\nbody".to_vec(),
            sent_path: None,
            queued_at: 0,
            attempts: 1,
            last_error: String::new(),
            send_at: None,
        }
    }

    #[test]
    fn a_queued_message_can_be_taken_apart_for_editing() {
        // Round trip: compose → build → (failed send) → edit. What comes back has
        // to be what went in, including the Bcc that lettre strips from the bytes.
        let account = AccountConfig {
            name: "Me".into(),
            email: "me@example.com".into(),
            ..sample_account()
        };
        let msg = OutgoingMessage {
            to: "Ada Lovelace <ada@example.com>".into(),
            cc: "carol@example.com".into(),
            bcc: "hidden@example.com".into(),
            subject: "Quarterly numbers".into(),
            body: "First line\nSecond line".into(),
            ..sample_outgoing()
        };
        let email = build_email(&account, &msg).expect("builds");
        let rcpts: Vec<String> =
            email.envelope().to().iter().map(|a| a.to_string()).collect();
        let raw = email.formatted();

        // lettre drops Bcc from the wire format, so the envelope is the only
        // record of it — which is exactly why it is stored alongside.
        assert!(!String::from_utf8_lossy(&raw).contains("hidden@example.com"));

        let editable = editable_from_raw(&raw, &rcpts);
        assert_eq!(editable.to, "Ada Lovelace <ada@example.com>");
        assert_eq!(editable.cc, "carol@example.com");
        assert_eq!(editable.bcc, "hidden@example.com");
        assert_eq!(editable.subject, "Quarterly numbers");
        // A plain-text body is escaped into HTML for the editor, keeping its lines.
        assert!(editable.body_html.contains("First line"), "{}", editable.body_html);
        assert!(editable.body_html.contains("Second line"), "{}", editable.body_html);
        assert!(editable.attachments.is_empty());
    }

    #[test]
    fn editing_a_queued_message_keeps_its_attachments_and_html() {
        let dir = std::env::temp_dir().join(format!("vireo-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("report.pdf");
        std::fs::write(&file, [0x25, 0x50, 0x44, 0x46, 0x00, 0xff]).expect("write");

        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            subject: "With a file".into(),
            body: "plain fallback".into(),
            html: "<p>rich <b>body</b></p>".into(),
            attachments: vec![file.to_string_lossy().to_string()],
            ..sample_outgoing()
        };
        let email = build_email(&sample_account(), &msg).expect("builds");
        let rcpts: Vec<String> =
            email.envelope().to().iter().map(|a| a.to_string()).collect();
        let editable = editable_from_raw(&email.formatted(), &rcpts);

        // The HTML alternative is what the composer edits, not the plain fallback.
        assert!(editable.body_html.contains("rich <b>body</b>"), "{}", editable.body_html);
        assert_eq!(editable.attachments.len(), 1);
        assert_eq!(editable.attachments[0].name, "report.pdf");
        assert_eq!(editable.attachments[0].data, [0x25, 0x50, 0x44, 0x46, 0x00, 0xff]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preview_skips_the_link_a_marketing_mail_opens_with() {
        // Cloudways' text/plain alternative, verbatim in shape: the header logo's
        // link renders as a bare URL above the greeting, and that URL was the
        // whole preview.
        let part = concat!(
            "--751d69df691580924654d5924db69f0ae9507550b8b16fd8b2d83daf1d3f\r\n",
            "Content-Transfer-Encoding: quoted-printable\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "( https://www.cloudways.com/en?utm_campaign=3DAPI+Keys+being+replaced+by+Ac=\r\n",
            "cess+Tokens&utm_medium=3Demail_action )\r\n",
            "\r\n",
            "Hi Camp crystal clear,\r\n",
            "\r\n",
            "We have a quick update on how you connect your AI agent to Cloudways.\r\n",
            "--751d69df691580924654d5924db69f0ae9507550b8b16fd8b2d83daf1d3f--\r\n",
        );
        let preview = preview_from_part(part.as_bytes(), None, None);
        assert!(preview.starts_with("Hi Camp crystal clear,"), "{preview}");
        assert!(!preview.contains("utm_campaign"), "{preview}");
    }

    #[test]
    fn preview_drops_rendered_links_but_keeps_their_text() {
        assert_eq!(
            preview_from_part(b"Generate your Access Token ( https://example.com/api?a=1 )Got questions?", None, None),
            "Generate your Access Token Got questions?"
        );
        // Brackets that are not a link are left exactly as written.
        assert_eq!(
            preview_from_part(b"Lunch (the usual place) at noon", None, None),
            "Lunch (the usual place) at noon"
        );
        // A message that is nothing but a link still shows it: better than a
        // blank row.
        assert_eq!(
            preview_from_part(b"https://example.com/only", None, None),
            "https://example.com/only"
        );
    }

    #[test]
    fn preview_descends_into_a_nested_multipart() {
        // ProtonMail sends mixed(alternative(plain, html), attachment), so
        // `BODY[1]` is the whole alternative container rather than any text — and
        // the preview showed its boundary line ("--b2=_cipkIEq1…") instead of the
        // message. The bytes below are that fetch, verbatim in shape.
        let part = concat!(
            "--b2=_cipkIEq1WkCIMIbpGDVyYc52x8YElrqa8uRU7GKJ8\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "SGVsbG8sIEkgcmVjZW50bHkgaW5zdGFsbGVkIFZpcmVvIGFmdGVyIHJlYWRpbmcgYWJvdXQgaXQg\r\n",
            "b24gdGhlIG9tZyF1YnVudHUgd2Vic2l0ZS4=\r\n",
            "\r\n",
            "--b2=_cipkIEq1WkCIMIbpGDVyYc52x8YElrqa8uRU7GKJ8\r\n",
            "Content-Type: text/html; charset=utf-8\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "PGRpdj5IZWxsbzwvZGl2Pg==\r\n",
            "--b2=_cipkIEq1WkCIMIbpGDVyYc52x8YElrqa8uRU7GKJ8--\r\n",
        );
        assert_eq!(
            preview_from_part(part.as_bytes(), None, None),
            "Hello, I recently installed Vireo after reading about it on the omg!ubuntu website."
        );
        // Nothing of the MIME machinery reaches the list.
        assert!(!preview_from_part(part.as_bytes(), None, None).contains("--b2="));
    }

    #[test]
    fn preview_prefers_plain_text_but_settles_for_html() {
        let html_only = concat!(
            "--x\r\n",
            "Content-Type: text/html; charset=utf-8\r\n",
            "\r\n",
            "<div>Only markup here</div>\r\n",
            "--x--\r\n",
        );
        assert_eq!(preview_from_part(html_only.as_bytes(), None, None), "Only markup here");
        // Two levels of nesting — mixed(alternative(...)) — are followed.
        let nested = concat!(
            "--outer\r\n",
            "Content-Type: multipart/alternative; boundary=\"inner\"\r\n",
            "\r\n",
            "--inner\r\n",
            "Content-Type: text/plain\r\n",
            "\r\n",
            "Buried but readable\r\n",
            "--inner--\r\n",
            "--outer--\r\n",
        );
        assert_eq!(preview_from_part(nested.as_bytes(), None, None), "Buried but readable");
    }

    #[test]
    fn a_message_that_merely_starts_with_dashes_is_not_a_multipart() {
        // A signature separator, or a line of dashes, must still read as text.
        assert_eq!(
            preview_from_part(b"-- \r\nRegards,\r\nSteve", None, None),
            "-- Regards, Steve"
        );
        assert_eq!(preview_from_part(b"--\r\nsigned off", None, None), "-- signed off");
    }

    /// A PGP/MIME message's first part is the "Version: 1" stub, and an
    /// inline one begins with the armour; the list says what it is instead.
    #[test]
    fn preview_names_an_encrypted_message() {
        let marker = crate::models::ENCRYPTED_PREVIEW;
        assert_eq!(preview_from_part(b"Version: 1\r\n", None, None), marker);
        assert_eq!(preview_from_part(b"version: 1", None, None), marker);
        assert_eq!(
            preview_from_part(b"-----BEGIN PGP MESSAGE-----\r\n\r\nhQEMA3\r\n-----END PGP MESSAGE-----\r\n", None, None),
            marker
        );
        assert!(crate::models::preview_is_encrypted(marker));
        assert_eq!(crate::models::preview_display(marker), "Encrypted message");
        assert_eq!(crate::models::preview_display("Hello"), "Hello");
        assert_eq!(pgp_preview("Version 1 of the plan is attached"), None);
        assert_eq!(preview_from_part(b"Hello there", None, None), "Hello there");
    }

    #[test]
    fn preview_reads_plain_text() {
        let text = b"Here are the figures we discussed.\r\n\r\nLet me know if the Q3 line looks wrong.\r\n";
        assert_eq!(
            preview_from_part(text, None, None),
            "Here are the figures we discussed. Let me know if the Q3 line looks wrong."
        );
        assert_eq!(preview_from_part(b"", None, None), "");
    }

    #[test]
    fn a_queued_message_takes_the_sent_copy_folder_it_finds_on_the_way_out() {
        use crate::models::{Folder, FolderKind};
        let folder = |id: u32, path: &str, kind: FolderKind| Folder {
            id,
            account_id: 1,
            name: path.to_string(),
            path: path.to_string(),
            kind,
            unread: 0,
        };
        let folders = vec![
            folder(1, "INBOX", FolderKind::Inbox),
            folder(2, "Sent", FolderKind::Sent),
        ];
        let account = |chosen: Option<&str>| AccountConfig {
            sent_copy_path: chosen.map(str::to_string),
            ..sample_account()
        };
        // The account says Inbox now; the message was queued for Sent. It goes
        // where the account says today, not where it said last week.
        assert_eq!(
            super::pick_queued_sent_copy(&account(Some("INBOX")), &folders, Some("Sent")).as_deref(),
            Some("INBOX"),
        );
        // Turned back off since queuing: back to the Sent folder.
        assert_eq!(
            super::pick_queued_sent_copy(&account(None), &folders, Some("INBOX")).as_deref(),
            Some("Sent"),
        );
        // Chosen folder gone from the server: Sent rather than a dead name.
        assert_eq!(
            super::pick_queued_sent_copy(&account(Some("Gone")), &folders, Some("Gone")).as_deref(),
            Some("Sent"),
        );
        // No folder list to read (offline, no cache): the queued path stands,
        // so a send never loses its copy over this.
        assert_eq!(
            super::pick_queued_sent_copy(&account(Some("INBOX")), &[], Some("Sent")).as_deref(),
            Some("Sent"),
        );
        // The server files its own: nothing of ours goes anywhere, whatever
        // was queued and whatever folder was picked.
        let server_saves = AccountConfig {
            sent_copy_path: Some("INBOX".into()),
            server_saves_sent: true,
            ..sample_account()
        };
        assert_eq!(super::pick_queued_sent_copy(&server_saves, &folders, Some("Sent")), None);
        assert_eq!(super::pick_queued_sent_copy(&server_saves, &[], Some("Sent")), None);
    }

    #[test]
    fn a_quoted_printable_preheader_is_not_mistaken_for_base64() {
        // Cloudflare's newsletter opens its text/plain part with a long run of
        // invisible padding — U+2007 U+034F over and over — so that the preview
        // a mail client shows is the sender's chosen line rather than whatever
        // the message starts with. Quoted-printable, every byte of it spelled
        // with letters, digits and `=`: base64's alphabet exactly. Guessing
        // read it as base64 and produced a row of replacement characters.
        let mut part = b"AI Bot Controls ".to_vec();
        for _ in 0..40 {
            part.extend_from_slice(b"=E2=80=87=CD=8F ");
        }
        part.extend_from_slice(b"Updates to managing AI crawlers on your account");

        assert!(!super::looks_like_base64(&part), "padding runs are not base64");

        // Told what it is, which is what the part's own MIME headers say.
        let told = super::preview_from_part(&part, Some("UTF-8"), Some("quoted-printable"));
        assert!(!told.contains('\u{fffd}'), "no replacement characters: {told}");
        assert!(told.starts_with("AI Bot Controls"), "{told}");
        assert!(told.contains("Updates to managing AI crawlers"), "{told}");

        // And the same left to the guess, now that the guess knows better.
        let guessed = super::preview_from_part(&part, Some("UTF-8"), None);
        assert_eq!(guessed, told);

        // Real base64 still reads as base64, truncated (no padding) or whole.
        let whole = b"VGhlIHF1aWNrIGJyb3duIGZveCBqdW1wcyBvdmVyIHRoZSBsYXp5IGRvZywgdHdpY2Uu";
        assert!(super::looks_like_base64(whole));
        assert!(super::looks_like_base64(b"VGhlIHF1aWNrIGJyb3duIGZveCBqdW1wcyBvdmVyIHRoZSBsYXp5IGRvZz0="));

        // The slice a preview reads ends on a byte count, routinely mid-escape.
        // The stub is dropped rather than shown as text.
        let cut = b"AI Bot Controls =E2=80=87=CD=8F =E";
        let out = super::preview_from_part(cut, Some("UTF-8"), Some("quoted-printable"));
        assert!(!out.contains('='), "no half-decoded escape left behind: {out:?}");
        assert_eq!(out.trim(), "AI Bot Controls");
        let cut = b"AI Bot Controls =E2=80=87=CD=8F =";
        let out = super::preview_from_part(cut, Some("UTF-8"), Some("quoted-printable"));
        assert!(!out.contains('='), "nor a bare one: {out:?}");
    }

    #[test]
    fn a_preview_of_nothing_but_preheader_padding_reaches_for_the_real_text() {
        // What the deeper BODY[TEXT] repair read sees: the whole multipart,
        // each part carrying its own headers. The text is thousands of bytes
        // past the padding, which is the point of the padding.
        let mut plain = b"AI Bot Controls ".to_vec();
        for _ in 0..250 {
            plain.extend_from_slice(b"=E2=80=87=CD=8F ");
        }
        plain.extend_from_slice(b"Updates to managing AI crawlers on your account.");
        let mut body = Vec::new();
        body.extend_from_slice(b"--BOUND\r\n");
        body.extend_from_slice(b"Content-Type: text/plain; charset=UTF-8\r\n");
        body.extend_from_slice(b"Content-Transfer-Encoding: quoted-printable\r\n\r\n");
        body.extend_from_slice(&plain);
        body.extend_from_slice(b"\r\n--BOUND--\r\n");

        let preview = super::preview_from_part(&body, None, None);
        assert!(
            preview.contains("Updates to managing AI crawlers"),
            "the padding is stepped over, not shown: {preview:?}",
        );
        assert!(!preview.contains('\u{fffd}'), "{preview:?}");
        assert!(!preview.contains('\u{034f}'), "joiners are dropped: {preview:?}");
    }

    #[test]
    fn preview_decodes_the_transfer_encoding_it_was_not_told_about() {
        // The fetch asks for body bytes only, so the encoding has to be inferred.
        // Quoted-printable, including a soft line break mid-sentence:
        let qp = b"Caf=C3=A9 meeting at 3pm =\r\nsharp, bring the numbers";
        assert_eq!(preview_from_part(qp, None, None), "Café meeting at 3pm sharp, bring the numbers");

        // Base64 — shown raw this would be gibberish in the list.
        let encoded = crate::oauth::base64_encode(
            b"Base64 bodies are common from newsletters and phones alike.",
        );
        assert_eq!(
            preview_from_part(encoded.as_bytes(), None, None),
            "Base64 bodies are common from newsletters and phones alike."
        );
    }

    #[test]
    fn preview_survives_being_cut_off_mid_stream() {
        // 2KB of a base64 part almost never lands on a group boundary; the
        // incomplete tail is dropped rather than decoded into noise.
        let full = crate::oauth::base64_encode(&b"The quick brown fox jumps over the lazy dog. ".repeat(4));
        let truncated = &full[..full.len() - 3];
        let preview = preview_from_part(truncated.as_bytes(), None, None);
        assert!(preview.starts_with("The quick brown fox"), "{preview}");
        // An escape cut in half is dropped, not shown: the slice ends on a
        // byte count, so its last escape is routinely half there. A lone
        // trailing "=" is a soft line break in quoted-printable and goes the
        // same way.
        assert_eq!(preview_from_part(b"Total: 50=", None, None), "Total: 50");
        assert_eq!(preview_from_part(b"Total: 50=4", None, None), "Total: 50");
        // Malformed rather than cut short: "=Z" is no escape at all, and
        // whatever it is, it isn't ours to remove.
        assert_eq!(preview_from_part(b"Total: 50=Z", None, None), "Total: 50=Z");
    }

    #[test]
    fn preview_reads_html_and_skips_quoted_replies() {
        let html = b"<html><body><p>Meeting moved to <b>Tuesday</b>.</p></body></html>";
        assert_eq!(preview_from_part(html, None, None), "Meeting moved to Tuesday.");
        let reply = b"Sounds good to me.\r\n\r\n> On Monday, Ada wrote:\r\n> the original text\r\n";
        assert_eq!(preview_from_part(reply, None, None), "Sounds good to me.");
    }

    #[test]
    fn preview_is_capped() {
        let long = "word ".repeat(200);
        assert_eq!(preview_from_part(long.as_bytes(), None, None).chars().count(), PREVIEW_CHARS);
    }

    #[test]
    fn short_text_is_not_mistaken_for_base64() {
        // "Meeting" is all base64 characters, but far too short to be a body.
        assert_eq!(preview_from_part(b"Meeting", None, None), "Meeting");
    }

    #[test]
    fn preview_reads_the_charset_the_part_declares() {
        // #159: a Hungarian bank's mail, quoted-printable in iso-8859-2. Read as
        // UTF-8, every accented letter was a replacement character in the list
        // while the reader showed the message fine.
        let qp = b"Tisztelt =DCgyfel=FCnk! K=E9rj=FCk, =F5rizze meg.";
        assert_eq!(
            preview_from_part(qp, Some("iso-8859-2"), None),
            "Tisztelt Ügyfelünk! Kérjük, őrizze meg."
        );
        // The 8-bit bytes as they are, no transfer encoding; label case and
        // quotes as servers send them.
        assert_eq!(preview_from_part(b"\xD5rizze meg", Some("\"ISO-8859-2\""), None), "Őrizze meg");
        assert_eq!(preview_from_part(b"\xF5rizze", Some("windows-1250"), None), "őrizze");
        // Base64 is decoded to bytes first, then read in the charset.
        let b64 = crate::oauth::base64_encode(b"Caf\xE9 meeting at three, bring the numbers please.");
        assert_eq!(
            preview_from_part(b64.as_bytes(), Some("iso-8859-1"), None),
            "Café meeting at three, bring the numbers please."
        );
        // UTF-8 still reads as UTF-8 whatever it is called.
        assert_eq!(preview_from_part("Café".as_bytes(), Some("UTF-8"), None), "Café");
        assert_eq!(preview_from_part("Café".as_bytes(), Some("utf8"), None), "Café");
    }

    #[test]
    fn preview_parts_of_a_multipart_carry_their_own_charset() {
        let nested = b"--b1\r\nContent-Type: text/plain; charset=\"iso-8859-2\"\r\n\
            Content-Transfer-Encoding: quoted-printable\r\n\r\n=D5rizze meg\r\n--b1--\r\n";
        assert_eq!(preview_from_part(nested, None, None), "Őrizze meg");
        // A charset on a folded continuation line still counts.
        let folded = b"--b1\r\nContent-Type: text/html;\r\n\tcharset=iso-8859-2\r\n\r\n\
            <p>\xD5rizze meg</p>\r\n--b1--\r\n";
        assert_eq!(preview_from_part(folded, None, None), "Őrizze meg");
    }

    #[test]
    fn preview_finds_the_parts_behind_a_mime_preamble() {
        // A whole body (the BODY[TEXT] retry) can open with the preamble
        // rather than the boundary.
        let body = b"This is a multi-part message in MIME format.\r\n\r\n--b1\r\n\
            Content-Type: image/png\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--b1\r\n\
            Content-Type: text/plain\r\n\r\nThe photo from Sunday\r\n--b1--\r\n";
        assert_eq!(preview_from_part(body, None, None), "The photo from Sunday");
        // But plain text that merely mentions a dashed line is still text.
        assert_eq!(preview_from_part(b"Hello\r\n--foo\r\nbar", None, None), "Hello --foo bar");
    }

    #[test]
    fn preview_copes_with_bytes_that_are_not_utf8() {
        // Undeclared 8-bit text reads as Windows-1252, the way browsers do.
        assert_eq!(preview_from_part(b"Caf\xE9 au lait", None, None), "Café au lait");
        // A slice that ends mid-character drops the fragment, not the row.
        let cut = &"Café".as_bytes()[..4];
        assert_eq!(preview_from_part(cut, None, None), "Caf");
        assert_eq!(preview_from_part(cut, Some("utf-8"), None), "Caf");
        // Declared UTF-8 that isn't: the damage shows as replacement characters.
        assert_eq!(preview_from_part(b"a\xFFb", Some("utf-8"), None), "a\u{fffd}b");
    }

    #[test]
    fn charset_param_is_read_from_a_content_type() {
        assert_eq!(charset_param("text/html; charset=\"iso-8859-2\""), Some("iso-8859-2".into()));
        assert_eq!(charset_param("text/plain; format=flowed; charset=utf-8"), Some("utf-8".into()));
        assert_eq!(charset_param("multipart/alternative; boundary=b1"), None);
        assert_eq!(
            mime_header("Content-Type: text/plain;\r\n\tcharset=us-ascii\r\nX: y", "content-type")
                .as_deref()
                .and_then(charset_param),
            Some("us-ascii".into())
        );
    }

    /// A part header carrying a non-ASCII character where a header name's
    /// length falls mid-character used to panic the account's mail thread, so
    /// its mail never arrived and the spinner turned for good.
    #[test]
    fn a_header_line_with_a_multibyte_character_is_not_sliced_apart() {
        // "content-transfer-encoding" is 25 bytes, and byte 25 of this line
        // sits inside the two-byte character that starts at byte 24.
        let headers = "Content-Description: abc\u{f3}de\r\nContent-Transfer-Encoding: base64\r\n";
        assert_eq!(mime_header(headers, "content-transfer-encoding").as_deref(), Some("base64"));
        assert_eq!(mime_header(headers, "content-type"), None);
    }

    /// The same line reached through the preview path that reads it: a part of
    /// a multipart, headers decoded leniently from raw bytes.
    #[test]
    fn a_preview_survives_a_multibyte_part_header() {
        let part = concat!(
            "--b1\r\n",
            "Content-Description: abc\u{f3}de\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n\r\n",
            "Hello.\r\n",
            "--b1--\r\n",
        );
        assert_eq!(preview_from_part(part.as_bytes(), None, None).trim(), "Hello.");
    }

    /// Issue #9's message: an Apple Mail PDF marked `inline` with a filename and
    /// no Content-ID, inside a top-level multipart/mixed.
    const INLINE_PDF: &str = concat!(
        "Content-Type: multipart/mixed; boundary=\"Apple-Mail=_A9BB\"\r\n\r\n",
        "--Apple-Mail=_A9BB\r\nContent-Type: text/plain\r\n\r\nSee attached.\r\n",
        "--Apple-Mail=_A9BB\r\n",
        "Content-Disposition: inline;\r\n\tfilename=\"Report.pdf\"\r\n",
        "Content-Type: application/pdf;\r\n\tname=\"Report.pdf\"\r\n",
        "Content-Transfer-Encoding: base64\r\n\r\n",
        "JVBERi0xLjQNCiWio4+T\r\n",
        "--Apple-Mail=_A9BB--\r\n",
    );

    /// The same file, but nested under a multipart/alternative — the shape a
    /// forwarded Apple Mail message takes, where the top-level Content-Type says
    /// nothing about attachments.
    const NESTED_INLINE_PDF: &str = concat!(
        "Content-Type: multipart/alternative; boundary=outer\r\n\r\n",
        "--outer\r\nContent-Type: text/plain\r\n\r\nPlease see attached.\r\n",
        "--outer\r\nContent-Type: multipart/mixed; boundary=inner\r\n\r\n",
        "--inner\r\nContent-Type: text/html\r\n\r\n<p>Please see attached.</p>\r\n",
        "--inner\r\nContent-Type: application/pdf; name=\"Report.pdf\"\r\n",
        "Content-Disposition: inline; filename=\"Report.pdf\"\r\n",
        "Content-Transfer-Encoding: base64\r\n\r\n",
        "JVBERi0xLjQNCiWio4+T\r\n",
        "--inner--\r\n--outer--\r\n",
    );

    #[test]
    fn an_inline_pdf_is_listed_as_an_attachment() {
        // Both shapes: the file is `inline` with no Content-ID, which is what the
        // reporter's Apple Mail message looked like.
        for (name, raw) in [("top level", INLINE_PDF), ("nested", NESTED_INLINE_PDF)] {
            let found = extract_attachments(raw.as_bytes());
            assert_eq!(found.len(), 1, "{name}");
            assert_eq!(found[0].name, "Report.pdf", "{name}");
            assert!(found[0].data.starts_with(b"%PDF-1.4"), "{name}");
        }
    }

    #[test]
    fn a_body_load_reports_the_attachments_the_summary_missed() {
        // The paperclip is guessed before the body is fetched, and the guess
        // misses the nested shape: the top-level type is multipart/alternative,
        // which says nothing. Fetching the body is what settles it — the same
        // check `load_body` runs before emitting HasAttachments.
        let has_attachments = |raw: &str| !extract_attachments(raw.as_bytes()).is_empty();
        assert!(has_attachments(NESTED_INLINE_PDF));
        assert!(has_attachments(INLINE_PDF));

        // The header-only guess, for comparison: right about the first, wrong
        // about the second — which is why the body has the last word.
        let top_level_says_mixed = |raw: &str| {
            mail_parser::MessageParser::default()
                .parse(raw.as_bytes())
                .and_then(|p| {
                    p.header("Content-Type").and_then(|h| h.as_content_type()).map(|ct| {
                        ct.ctype().eq_ignore_ascii_case("multipart")
                            && ct.subtype().is_some_and(|s| s.eq_ignore_ascii_case("mixed"))
                    })
                })
                .unwrap_or(false)
        };
        assert!(top_level_says_mixed(INLINE_PDF));
        assert!(!top_level_says_mixed(NESTED_INLINE_PDF));
    }

    #[test]
    fn decoration_still_does_not_earn_a_paperclip() {
        // The correction must not undo 1.4.1's fix: a newsletter whose only extra
        // parts are small inline `cid:` images has no attachments.
        assert!(extract_attachments(CID_NEWSLETTER.as_bytes()).is_empty());
    }

    #[test]
    fn folder_names_are_decoded_for_display() {
        // Issue #1: Gmail's Chinese labels arrived as modified UTF-7 and were
        // shown verbatim in the sidebar.
        assert_eq!(display_name("&XfJSoGYfaAc-", Some("/")), "已加星标");
        // Only the leaf is shown, and the parent may be encoded too.
        assert_eq!(display_name("&U,BTFw-/&g0l6Pw-", Some("/")), "草稿");
        assert_eq!(display_name("INBOX.&U,BTFw-", Some(".")), "台北");
        // Ordinary names are untouched, and INBOX keeps its special casing.
        assert_eq!(display_name("INBOX", Some("/")), "Inbox");
        assert_eq!(display_name("[Gmail]/All Mail", Some("/")), "All Mail");
        assert_eq!(display_name("Tom & Jerry", Some("/")), "Tom & Jerry");
    }

    #[test]
    fn a_queued_message_keeps_every_recipient_on_retry() {
        // Bcc exists only in the envelope, so a retry that rebuilds it from the
        // headers would silently drop those recipients.
        let item = queued("me@example.com", &["ada@example.com", "bcc@example.com"]);
        let envelope = outbox_envelope(&item).expect("envelope rebuilds");
        assert_eq!(envelope.from().map(|f| f.to_string()).as_deref(), Some("me@example.com"));
        assert_eq!(
            envelope.to().iter().map(|a| a.to_string()).collect::<Vec<_>>(),
            ["ada@example.com", "bcc@example.com"]
        );
    }

    #[test]
    fn a_queued_message_with_an_unusable_address_is_not_retried() {
        // Better to leave it in the Outbox saying so than to retry forever.
        assert!(outbox_envelope(&queued("me@example.com", &["not-an-address"])).is_none());
        assert!(outbox_envelope(&queued("me@example.com", &[])).is_none());
        // A missing sender is legal (SMTP's null reverse-path), so it is not a
        // reason to refuse.
        assert!(outbox_envelope(&queued("", &["ada@example.com"])).is_some());
    }

    #[test]
    fn an_alias_replaces_from_but_not_the_transport() {
        let account = sample_account();
        let mut msg = sample_outgoing();
        msg.to = "someone@example.com".into();
        msg.from_alias = Some("Ann Work <ann@work.example>".into());
        let email = build_email(&account, &msg).expect("builds");
        let wire = String::from_utf8(email.formatted()).expect("utf8");
        let from_line = wire.lines().find(|l| l.starts_with("From:")).expect("From header");
        assert!(from_line.contains("ann@work.example"), "alias address on the wire: {from_line}");
        assert!(!from_line.contains(&account.email), "account address replaced: {from_line}");
        // The Message-ID follows the alias's domain, so replies thread back.
        let id_line = wire.lines().find(|l| l.starts_with("Message-ID:")).expect("id");
        assert!(id_line.contains("@work.example"), "id in alias domain: {id_line}");
    }

    #[test]
    fn only_an_alias_with_its_own_smtp_switches_transport() {
        use crate::config::AliasConfig;
        let account = AccountConfig {
            email: "me@example.com".into(),
            aliases: vec![
                AliasConfig { identity: "Plain <plain@fwd.example>".into(), ..Default::default() },
                AliasConfig {
                    identity: "Ann Work <ann@work.example>".into(),
                    smtp_host: "smtp.work.example".into(),
                    smtp_port: 465,
                    smtp_username: "ann@work.example".into(),
                    ..Default::default()
                },
            ],
            ..sample_account()
        };
        // A plain alias, and the account's own address, keep the account transport.
        assert!(alias_with_own_smtp(&account, "plain@fwd.example").is_none());
        assert!(alias_with_own_smtp(&account, "me@example.com").is_none());
        // The per-SMTP alias resolves — case-insensitively, as addresses are.
        let hit = alias_with_own_smtp(&account, "Ann@Work.example").expect("matches");
        assert_eq!(hit.smtp_host, "smtp.work.example");
        // An address that is nobody's alias resolves to the account transport.
        assert!(alias_with_own_smtp(&account, "stranger@else.example").is_none());
    }

    #[test]
    fn build_email_carries_attachments() {
        // Issue #15: "Send failed: Invalid input" when sending with attachments.
        // "Invalid input" is lettre's AddressError, raised only by parsing a
        // "Name <addr>" string — which 1.8.x did for From/To and 1.9.0 replaced
        // with mailboxes built from parts. This pins the attachment path itself,
        // including a name that would have needed quoting and a UTF-8 filename.
        let dir = std::env::temp_dir().join(format!("vireo-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("Meeting notes, final.pdf");
        // Binary, so the encoder has to base64 it — as it would a real PDF.
        std::fs::write(&file, [0x25, 0x50, 0x44, 0x46, 0x2d, 0x31, 0x2e, 0x34, 0x00, 0x80, 0xff])
            .expect("write attachment");

        let account = AccountConfig {
            name: "Powers, Benny".into(),
            email: "benny@example.com".into(),
            ..sample_account()
        };
        let msg = OutgoingMessage {
            to: "Ada Lovelace <ada@example.com>".into(),
            subject: "Notes".into(),
            attachments: vec![file.to_string_lossy().to_string()],
            ..sample_outgoing()
        };

        let email = build_email(&account, &msg).expect("email builds with an attachment");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(raw.contains("multipart/mixed"), "{raw}");
        assert!(raw.contains("application/pdf"), "{raw}");
        // The filename has a comma and a space, so it has to be quoted or encoded.
        assert!(raw.contains("Meeting notes, final.pdf") || raw.contains("Meeting%20notes"), "{raw}");
        // Base64 of the bytes written above.
        assert!(raw.contains("base64"), "{raw}");
        assert!(raw.contains("JVBERi0xLjQAgP8="), "{raw}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A picture pasted into the composer rides the HTML as a data: URI; the
    /// wire message must carry it as an inline cid: part inside
    /// multipart/related — rendered in place, absent from the recipient's
    /// attachment list — with no data: URI left behind.
    #[test]
    fn pasted_images_go_out_as_inline_cid_parts() {
        let png = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x01, 0x02];
        let uri = format!("data:image/png;base64,{}", crate::oauth::base64_encode(&png));
        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            body: "see picture".into(),
            html: format!("<p>see</p><img src=\"{uri}\" style=\"max-width:100%\"><p>picture</p>"),
            ..sample_outgoing()
        };
        let email = build_email(&sample_account(), &msg).expect("email builds");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(raw.contains("multipart/related"), "{raw}");
        assert!(raw.contains("Content-ID"), "{raw}");
        assert!(raw.contains("cid:"), "{raw}");
        assert!(raw.contains("Content-Disposition: inline"), "{raw}");
        assert!(raw.contains("image/png"), "{raw}");
        assert!(!raw.contains("data:image"), "{raw}");
    }

    /// A picture dropped in from a file manager keeps the name it had; one
    /// pasted as raw bytes has none to keep.
    #[test]
    fn an_images_own_filename_reaches_the_inline_part() {
        let png = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00];
        let uri = format!("data:image/png;base64,{}", crate::oauth::base64_encode(&png));
        let raw_for = |img: String| {
            let msg = OutgoingMessage {
                to: "ada@example.com".into(),
                body: "see picture".into(),
                html: format!("<p>see</p>{img}"),
                ..sample_outgoing()
            };
            let email = build_email(&sample_account(), &msg).expect("email builds");
            String::from_utf8_lossy(&email.formatted()).to_string()
        };
        let named = raw_for(format!("<img src=\"{uri}\" alt=\"holiday snap.png\">"));
        assert!(named.contains("holiday snap.png"), "{named}");
        let anonymous = raw_for(format!("<img src=\"{uri}\">"));
        assert!(anonymous.contains("Content-Disposition: inline"), "{anonymous}");
        assert!(!anonymous.contains("filename"), "{anonymous}");
    }

    /// `alt` arrives from outside on any quoted reply and lands in a header,
    /// so a quote, a backslash or a control character never reaches one, and
    /// a value that is no filename at all names nothing.
    #[test]
    fn an_alt_that_is_not_a_plain_filename_names_nothing() {
        let html = "<img src='data:image/png;base64,AAAA' alt='%s'>";
        for bad in ["a\"b", "a\\b", "a\nb", "", ".", ".."] {
            let doc = html.replace("%s", bad);
            let (_, imgs) = extract_data_images(&doc);
            assert_eq!(imgs.len(), 1, "{doc}");
            assert_eq!(imgs[0].name.as_deref(), None, "{doc}");
        }
        // A path is not rejected but reduced: only its last segment can name
        // the part, so a traversal cannot escape into the header.
        let (_, ok) = extract_data_images(
            "<img src='data:image/png;base64,AAAA' alt='../../etc/report.png'>",
        );
        assert_eq!(ok[0].name.as_deref(), Some("report.png"));
    }

    /// HTML without an embedded image keeps the old, plain alternative shape.
    #[test]
    fn html_without_images_stays_a_plain_alternative() {
        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            body: "hi".into(),
            html: "<p>hi</p>".into(),
            ..sample_outgoing()
        };
        let email = build_email(&sample_account(), &msg).expect("email builds");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(raw.contains("multipart/alternative"), "{raw}");
        assert!(!raw.contains("multipart/related"), "{raw}");
    }

    /// The lift leaves anything that is not a well-formed quoted base64 image
    /// data URI exactly as written.
    #[test]
    fn extract_data_images_leaves_odd_uris_alone() {
        let html = "<p>data:image/png is a scheme</p><img src=\"data:image/png;base65,zzz\">";
        let (out, imgs) = extract_data_images(html);
        assert_eq!(out, html);
        assert!(imgs.is_empty());

        // Single-quoted attributes lift too.
        let (out, imgs) =
            extract_data_images("<img src='data:image/jpeg;base64,AAAA'>");
        assert!(out.contains("src='cid:"), "{out}");
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].mime, "image/jpeg");
        assert_eq!(imgs[0].data, vec![0, 0, 0]);
    }

    #[test]
    fn build_email_reports_a_missing_attachment_by_name() {
        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            attachments: vec!["/nonexistent/report.pdf".into()],
            ..sample_outgoing()
        };
        let err = build_email(&sample_account(), &msg).expect_err("missing file fails");
        assert!(err.to_string().contains("report.pdf"), "{err}");
    }

    #[test]
    fn build_email_accepts_named_recipients() {
        let account = AccountConfig {
            name: "Jason Martin".into(),
            email: "me@example.com".into(),
            ..sample_account()
        };
        let msg = OutgoingMessage {
            to: "Alfonso Lizárraga <alfonso@example.com>, plain@example.com".into(),
            ..sample_outgoing()
        };
        let email = build_email(&account, &msg).expect("email builds");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(raw.contains("alfonso@example.com"), "{raw}");
        assert!(raw.contains("plain@example.com"), "{raw}");
    }

    #[test]
    fn only_this_machine_counts_as_a_local_bridge() {
        for host in ["127.0.0.1", "localhost", "LocalHost", "::1", "[::1]", "127.1.2.3"] {
            assert!(is_loopback_host(host), "{host} should be loopback");
        }
        // A remote host must keep full certificate verification, however it is
        // named — including anything merely containing "localhost".
        for host in ["imap.gmail.com", "127.0.0.1.example.com", "localhost.evil.com", "10.0.1.14"] {
            assert!(!is_loopback_host(host), "{host} should not be loopback");
        }
    }

    #[test]
    fn starttls_is_used_for_143_and_local_bridges_only() {
        let starttls = |host: &str, port: u16| {
            imap_uses_starttls(&AccountConfig {
                imap_host: host.into(),
                imap_port: port,
                ..sample_account()
            })
        };
        // Proton Bridge's default endpoint, and a bridge on a changed port.
        assert!(starttls("127.0.0.1", 1143));
        assert!(starttls("localhost", 2143));
        // The conventional STARTTLS port, anywhere.
        assert!(starttls("imap.example.com", 143));
        // 993 is implicit TLS even on a bridge, and a remote host on any other
        // port keeps the implicit-TLS behaviour it had before.
        assert!(!starttls("127.0.0.1", 993));
        assert!(!starttls("imap.gmail.com", 993));
        assert!(!starttls("imap.example.com", 1993));
    }

    #[test]
    fn decode_header_repairs_interior_spaces() {
        // DPD's complaints department sends its display name with a literal
        // trailing space INSIDE the encoded word — illegal, and the decoder
        // used to give up and show the raw `=?UTF-8?Q?…?=` (beta 1.18.0b
        // feedback from p-mitana).
        let raw = b"=?UTF-8?Q?Dzia=C5=82_Reklamacji_DPD ?=";
        assert_eq!(decode_header(raw).trim_end(), "Dzia\u{142} Reklamacji DPD");
    }

    #[test]
    fn decode_header_handles_over_long_encoded_word() {
        // The Marginalian (Mailchimp) sends the entire subject as one Q-encoded
        // word ~250 chars long — far past RFC 2047's 75-char limit. Earlier
        // builds aborted and left the raw `=?utf-8?Q?…?=` in the UI; we now
        // decode it as Apple Mail and Thunderbird do.
        let raw = b"=?utf-8?Q?92=2Dyear=2Dold=20artist=20Sheila=20Hicks=20on=20the=20key=20to=20creative=20vitality=2C=20how=20to=20manage=20heartbreak=20like=20Frida=20Kahlo=2C=20the=20elusive=20science=20of=20the=20present=20moment?=";
        assert_eq!(
            decode_header(raw),
            "92-year-old artist Sheila Hicks on the key to creative vitality, \
             how to manage heartbreak like Frida Kahlo, the elusive science of \
             the present moment"
        );
    }

    #[test]
    fn decode_header_leaves_plain_text_untouched() {
        assert_eq!(decode_header(b"Just a normal subject"), "Just a normal subject");
    }

    #[test]
    fn references_and_in_reply_to_merge_without_duplicates() {
        // A well-formed reply: In-Reply-To repeats the last id of References, and
        // the ancestry it adds is what lets a thread survive a parent kept in
        // another folder (#21).
        assert_eq!(
            merge_msgids("a@x b@y", "b@y"),
            "a@x b@y",
            "the repeated parent should not be listed twice"
        );
        // Either side may be the only one present.
        assert_eq!(merge_msgids("", "b@y"), "b@y");
        assert_eq!(merge_msgids("a@x", ""), "a@x");
        assert_eq!(merge_msgids("", ""), "");
        // A malformed reply whose In-Reply-To names an ancestor References omits.
        assert_eq!(merge_msgids("a@x", "c@z"), "a@x c@z");
    }

    #[test]
    fn header_references_are_normalized_like_envelope_ids() {
        // Folded exactly as a server sends it, angle brackets and mixed case.
        assert_eq!(
            normalize_msgids(b"<A@x>\r\n <b@Y>\r\n"),
            "a@x b@y",
            "ids should be unwrapped, lowercased and space-joined"
        );
    }

    #[test]
    fn linkify_wraps_http_and_https() {
        assert_eq!(
            linkify("see http://example.com now"),
            "see <a href=\"http://example.com\">http://example.com</a> now"
        );
        assert_eq!(
            linkify("https://a.b/c?d=1"),
            "<a href=\"https://a.b/c?d=1\">https://a.b/c?d=1</a>"
        );
    }

    #[test]
    fn linkify_prefixes_bare_www_with_https() {
        assert_eq!(
            linkify("go to www.example.com today"),
            "go to <a href=\"https://www.example.com\">www.example.com</a> today"
        );
    }

    #[test]
    fn linkify_escapes_and_does_not_double_link() {
        // The surrounding <> are escaped; the URL inside is linked once.
        assert_eq!(
            linkify("<http://x.com>"),
            "&lt;<a href=\"http://x.com\">http://x.com</a>&gt;"
        );
        // A query '&' is escaped in both href and text.
        assert_eq!(
            linkify("http://x.com/?a=1&b=2"),
            "<a href=\"http://x.com/?a=1&amp;b=2\">http://x.com/?a=1&amp;b=2</a>"
        );
    }

    #[test]
    fn linkify_trims_trailing_punctuation_but_keeps_balanced_parens() {
        assert_eq!(
            linkify("visit http://x.com."),
            "visit <a href=\"http://x.com\">http://x.com</a>."
        );
        assert_eq!(
            linkify("(see http://x.com)"),
            "(see <a href=\"http://x.com\">http://x.com</a>)"
        );
        // A balanced ')' belongs to the URL (e.g. a Wikipedia article).
        assert_eq!(
            linkify("http://en.wikipedia.org/wiki/Foo_(bar)"),
            "<a href=\"http://en.wikipedia.org/wiki/Foo_(bar)\">http://en.wikipedia.org/wiki/Foo_(bar)</a>"
        );
    }

    #[test]
    fn linkify_ignores_scheme_inside_a_word() {
        // No word boundary before "http", so it's not a link (just escaped text).
        assert_eq!(linkify("shttp://x.com"), "shttp://x.com");
        assert!(!linkify("email hi@www.x").contains("<a "));
    }

    #[test]
    fn linkify_never_forges_a_dangerous_scheme() {
        // "javascript:" isn't one of our recognized prefixes, so it stays plain text.
        let out = linkify("javascript:alert(1)");
        assert!(!out.contains("<a "), "got: {out}");
    }

    /// Auto-empty (#140): only the roles with an age are swept.
    #[test]
    fn auto_empty_roles_follow_the_ages() {
        let mut acc = sample_account();
        assert!(auto_empty_roles(&acc).is_empty());
        acc.empty_trash_days = 14;
        let roles = auto_empty_roles(&acc);
        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0].0, FolderKind::Trash);
        assert_eq!(roles[0].2, 14);
        acc.empty_junk_days = 7;
        assert_eq!(auto_empty_roles(&acc).len(), 2);
    }

    /// The manual role assignment (#82) names the folder first; otherwise
    /// the detected kind does.
    #[test]
    fn auto_empty_role_folder_prefers_the_assignment() {
        let mut acc = sample_account();
        let folders = vec![Folder {
            id: 7,
            account_id: 1,
            name: "Deleted Messages".into(),
            path: "Deleted Messages".into(),
            kind: FolderKind::Trash,
            unread: 0,
        }];
        assert_eq!(
            role_folder_path(&acc, &folders, FolderKind::Trash, "trash").as_deref(),
            Some("Deleted Messages")
        );
        assert_eq!(role_folder_path(&acc, &folders, FolderKind::Junk, "junk"), None);
        acc.folder_roles.insert("trash".into(), "INBOX.Bin".into());
        assert_eq!(
            role_folder_path(&acc, &folders, FolderKind::Trash, "trash").as_deref(),
            Some("INBOX.Bin")
        );
    }

    /// The sweep runs once, then not again until the interval has passed.
    #[test]
    fn auto_empty_is_throttled() {
        let mut acc = sample_account();
        let mut last = None;
        assert!(!auto_empty_due(&acc, &mut last), "nothing configured");
        assert!(last.is_none());
        acc.empty_junk_days = 7;
        assert!(auto_empty_due(&acc, &mut last));
        assert!(!auto_empty_due(&acc, &mut last), "just ran");
        last = Some(std::time::Instant::now() - AUTO_EMPTY_MIN - Duration::from_secs(1));
        assert!(auto_empty_due(&acc, &mut last));
    }

    /// IMAP wants "1-Aug-2026" (no zero padding); Graph an RFC 3339 instant.
    #[test]
    fn auto_empty_cutoffs_are_well_formed() {
        let imap = imap_before_date(0);
        let today = chrono::Utc::now().format("%-d-%b-%Y").to_string();
        assert_eq!(imap, today);
        assert!(!imap.starts_with('0'), "{imap}");
        let parts: Vec<&str> = imap.split('-').collect();
        assert_eq!(parts.len(), 3, "{imap}");
        assert_eq!(parts[1].len(), 3, "{imap}");
        let graph = graph_before_date(30);
        assert!(chrono::DateTime::parse_from_rfc3339(&graph).is_ok(), "{graph}");
        assert!(graph < chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());
    }

    #[test]
    fn extract_body_linkifies_plain_text_mail() {
        let raw = b"Content-Type: text/plain\r\n\r\nRead http://example.com/x for more.";
        let body = extract_body(raw);
        assert!(
            body.contains("<a href=\"http://example.com/x\">http://example.com/x</a>"),
            "body was: {body}"
        );
    }

    /// An iPhone photo mail: multipart/mixed interleaving an empty text part, an
    /// *inline* JPEG, and the signature. Exactly the shape Apple Mail produces.
    const IPHONE_PHOTO: &str = concat!(
        "Content-Type: multipart/mixed; boundary=Apple-Mail-32B9517E\r\n",
        "Content-Transfer-Encoding: 7bit\r\n",
        "From: Alex Doe <alex@example.com>\r\n",
        "Subject: Panda\r\n",
        "X-Mailer: iPhone Mail (23F84)\r\n",
        "\r\n",
        "--Apple-Mail-32B9517E\r\n",
        "Content-Type: text/plain;\r\n\tcharset=us-ascii\r\n",
        "Content-Transfer-Encoding: 7bit\r\n\r\n\r\n\r\n",
        "--Apple-Mail-32B9517E\r\n",
        "Content-Type: image/jpeg;\r\n\tname=C21AA3E8.jpeg;\r\n",
        "\tx-apple-part-url=4F5E768A\r\n",
        "Content-Disposition: inline;\r\n\tfilename=C21AA3E8.jpeg\r\n",
        "Content-Transfer-Encoding: base64\r\n\r\n",
        "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBk=\r\n",
        "\r\n",
        "--Apple-Mail-32B9517E\r\n",
        "Content-Type: text/plain;\r\n\tcharset=us-ascii\r\n",
        "Content-Transfer-Encoding: 7bit\r\n\r\n",
        "\r\n\r\nAlex Doe\r\nSent from my iPhone\r\n",
        "--Apple-Mail-32B9517E--\r\n",
    );

    /// A newsletter: HTML body plus a `cid:` logo referenced from it.
    const CID_NEWSLETTER: &str = concat!(
        "Content-Type: multipart/related; boundary=R\r\n",
        "Subject: News\r\n\r\n",
        "--R\r\n",
        "Content-Type: text/html; charset=utf-8\r\n\r\n",
        "<p>hi <img src=\"cid:logo\"></p>\r\n",
        "--R\r\n",
        "Content-Type: image/png; name=logo.png\r\n",
        "Content-ID: <logo>\r\n",
        "Content-Disposition: inline; filename=logo.png\r\n",
        "Content-Transfer-Encoding: base64\r\n\r\n",
        "iVBORw0KGgo=\r\n",
        "--R--\r\n",
    );

    #[test]
    fn iphone_photo_body_keeps_text_and_inlines_the_image() {
        let body = extract_body(IPHONE_PHOTO.as_bytes());
        // The signature lives in the *third* part; taking only the first left the
        // message blank.
        assert!(body.contains("Sent from my iPhone"), "body was: {body}");
        // The photo is embedded, not dropped or fetched from the network.
        assert!(body.contains("src=\"data:image/jpeg;base64,/9j/4AAQ"), "body was: {body}");
    }

    #[test]
    fn iphone_photo_is_listed_as_an_attachment() {
        let found = extract_attachments(IPHONE_PHOTO.as_bytes());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "C21AA3E8.jpeg");
        assert!(!found[0].data.is_empty());
    }

    /// Gmail photo mail: multipart/related wrapping a plain+HTML alternative and
    /// the picture the HTML references by Content-ID.
    const GMAIL_RELATED_PHOTO: &str = concat!(
        "Content-Type: multipart/related; boundary=\"OUT\"\r\n",
        "Subject: Share A Day NYC\r\n\r\n",
        "--OUT\r\n",
        "Content-Type: multipart/alternative; boundary=\"IN\"\r\n\r\n",
        "--IN\r\n",
        "Content-Type: text/plain; charset=\"UTF-8\"\r\n\r\n",
        "[image: SAD_POSTCARD_FRONT.JPEG]\r\n",
        "--IN\r\n",
        "Content-Type: text/html; charset=\"UTF-8\"\r\n\r\n",
        "<div dir=\"ltr\"><img src=\"cid:ii_mslv0bge0\" ",
        "alt=\"SAD_POSTCARD_FRONT.JPEG\" width=\"420\" height=\"542\"><br></div>\r\n",
        "--IN--\r\n",
        "--OUT\r\n",
        "Content-Type: image/jpeg; name=\"SAD_POSTCARD_FRONT.JPEG\"\r\n",
        "Content-Disposition: inline; filename=\"SAD_POSTCARD_FRONT.JPEG\"\r\n",
        "Content-Transfer-Encoding: base64\r\n",
        "Content-ID: <ii_mslv0bge0>\r\n\r\n",
        "/9j/4AAQSkZJRg==\r\n",
        "--OUT--\r\n",
    );

    #[test]
    fn cid_resources_are_not_listed_as_attachments() {
        assert!(extract_attachments(CID_NEWSLETTER.as_bytes()).is_empty());
    }

    /// A `multipart/related` whose `cid:` image decodes to `bytes` bytes.
    fn cid_mail_with_image_of(bytes: usize) -> String {
        // base64 of `bytes` zeroes: 4 chars per 3 bytes, rounded up.
        let payload = crate::oauth::base64_encode(&vec![0u8; bytes]);
        concat!(
            "Content-Type: multipart/related; boundary=R\r\n",
            "Subject: Photo\r\n\r\n",
            "--R\r\n",
            "Content-Type: text/html; charset=utf-8\r\n\r\n",
            "<p><img src=\"cid:pic\"></p>\r\n",
            "--R\r\n",
            "Content-Type: image/jpeg; name=\"pic.jpg\"\r\n",
            "Content-ID: <pic>\r\n",
            "Content-Disposition: inline; filename=\"pic.jpg\"\r\n",
            "Content-Transfer-Encoding: base64\r\n\r\n",
        )
        .to_string()
            + &payload
            + "\r\n--R--\r\n"
    }

    #[test]
    fn large_cid_image_is_listed_as_an_attachment() {
        // A photo someone emailed you arrives in the same shape as a newsletter
        // logo; it has to be saveable, so size decides.
        let found = extract_attachments(cid_mail_with_image_of(INLINE_ATTACHMENT_MIN).as_bytes());
        assert_eq!(found.len(), 1, "found: {found:?}");
        assert_eq!(found[0].name, "pic.jpg");
        assert_eq!(found[0].data.len(), INLINE_ATTACHMENT_MIN);
    }

    #[test]
    fn small_cid_image_is_still_only_decoration() {
        let raw = cid_mail_with_image_of(INLINE_ATTACHMENT_MIN - 1);
        assert!(extract_attachments(raw.as_bytes()).is_empty());
    }

    /// A message in the shape Gmail's composer sends: every uploaded file is
    /// marked `Content-Disposition: attachment` *and* stamped with an `<f_…>`
    /// Content-ID, whatever its size.
    fn gmail_mail_with_files(files: &[(&str, &str, usize)]) -> String {
        let mut raw = String::from("Content-Type: multipart/mixed; boundary=M\r\n");
        raw.push_str("Subject: files\r\n\r\n");
        raw.push_str("--M\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n");
        raw.push_str("Body text.\r\n");
        for (i, (name, ctype, bytes)) in files.iter().enumerate() {
            raw.push_str("--M\r\n");
            raw.push_str(&format!("Content-Type: {ctype}; name=\"{name}\"\r\n"));
            raw.push_str(&format!(
                "Content-Disposition: attachment; filename=\"{name}\"\r\n"
            ));
            raw.push_str("Content-Transfer-Encoding: base64\r\n");
            raw.push_str(&format!("Content-ID: <f_a1b2c3d4{i}>\r\n\r\n"));
            raw.push_str(&crate::oauth::base64_encode(&vec![0u8; *bytes]));
            raw.push_str("\r\n");
        }
        raw.push_str("--M--\r\n");
        raw
    }

    /// Two small files and one large one are sent together; only the large one
    /// came back, because the small parts carried a Content-ID. A part the sender
    /// declared an attachment is never decoration, however small.
    #[test]
    fn small_declared_attachments_survive_their_content_id() {
        let raw = gmail_mail_with_files(&[
            ("first.pdf", "application/pdf", 16_384),
            ("second.pdf", "application/pdf", 40_960),
            ("large.bin", "application/octet-stream", INLINE_ATTACHMENT_MIN + 1),
        ]);
        let found = extract_attachments(raw.as_bytes());
        let names: Vec<&str> = found.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            ["first.pdf", "second.pdf", "large.bin"],
            "found: {names:?}"
        );
        assert_eq!(found[0].data.len(), 16_384);
        assert_eq!(found[1].data.len(), 40_960);
    }

    /// One attachment, its Content-Disposition supplied verbatim.
    fn mail_with_disposition(disposition: &str) -> String {
        format!(
            concat!(
                "Content-Type: multipart/mixed; boundary=B\r\n",
                "Subject: file\r\n\r\n",
                "--B\r\n",
                "Content-Type: application/octet-stream\r\n",
                "Content-Transfer-Encoding: base64\r\n",
                "Content-Disposition: {disposition}\r\n\r\n",
                "AAAAAA==\r\n",
                "--B--\r\n",
            ),
            disposition = disposition
        )
    }

    /// A sender split one filename across two encoded-words with a space between
    /// them. RFC 2047 §6.2 joins those and drops the space; keeping it yields
    /// `docu ment.pdf` — a PDF with no `.pdf` suffix, so nothing can open it.
    #[test]
    fn a_filename_split_across_encoded_words_is_rejoined() {
        let raw =
            mail_with_disposition("attachment; filename=\"=?utf-8?q?docu?= =?utf-8?q?ment.pdf?=\"");
        let found = extract_attachments(raw.as_bytes());
        assert_eq!(found.len(), 1, "found: {found:?}");
        assert_eq!(found[0].name, "document.pdf");
    }

    /// The same split, folded across two lines: the separator arrives as a tab.
    #[test]
    fn a_filename_folded_between_encoded_words_is_rejoined() {
        let raw = mail_with_disposition(
            "attachment;\r\n\tfilename=\"=?utf-8?q?docu?=\r\n\t=?utf-8?q?ment.pdf?=\"",
        );
        let found = extract_attachments(raw.as_bytes());
        assert_eq!(found.len(), 1, "found: {found:?}");
        assert_eq!(found[0].name, "document.pdf");
    }

    /// `=20` is a space in the name itself, not a separator between two words.
    #[test]
    fn a_space_inside_one_encoded_word_is_kept() {
        let raw = mail_with_disposition("attachment; filename=\"=?utf-8?q?my=20file.pdf?=\"");
        let found = extract_attachments(raw.as_bytes());
        assert_eq!(found.len(), 1, "found: {found:?}");
        assert_eq!(found[0].name, "my file.pdf");
    }

    /// A plain filename with a space in it is nobody's business but the sender's.
    #[test]
    fn an_unencoded_filename_with_a_space_is_untouched() {
        let raw = mail_with_disposition("attachment; filename=\"my file.pdf\"");
        let found = extract_attachments(raw.as_bytes());
        assert_eq!(found.len(), 1, "found: {found:?}");
        assert_eq!(found[0].name, "my file.pdf");
    }

    #[test]
    fn large_cid_image_counts_toward_the_paperclip() {
        // BODYSTRUCTURE reports the base64 size: 4 characters per 3 bytes, padded.
        let octets = INLINE_ATTACHMENT_MIN.div_ceil(3) * 4;
        let raw = format!(
            concat!(
                "* 1 FETCH (BODYSTRUCTURE (",
                "(\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 20 1 NIL NIL NIL NIL)",
                "(\"IMAGE\" \"JPEG\" (\"NAME\" \"pic.jpg\") \"<pic>\" NIL \"BASE64\" {} NIL ",
                "(\"INLINE\" (\"FILENAME\" \"pic.jpg\")) NIL NIL)",
                " \"RELATED\" (\"BOUNDARY\" \"r\") NIL NIL NIL))\r\n",
            ),
            octets
        );
        assert!(structure_has_attachment(&bodystructure(&raw)));
    }

    #[test]
    fn cid_image_is_embedded_in_the_html_body() {
        let body = extract_body(CID_NEWSLETTER.as_bytes());
        // Nothing can fetch a `cid:` URL, so the reference must be gone.
        assert!(!body.contains("cid:logo"), "body was: {body}");
        assert!(
            body.contains("src=\"data:image/png;base64,iVBORw0KGgo=\""),
            "body was: {body}"
        );
    }

    #[test]
    fn gmail_related_photo_renders_the_picture_not_its_filename() {
        let body = extract_body(GMAIL_RELATED_PHOTO.as_bytes());
        assert!(!body.contains("cid:ii_mslv0bge0"), "body was: {body}");
        assert!(
            body.contains("src=\"data:image/jpeg;base64,/9j/4AAQSkZJRg==\""),
            "body was: {body}"
        );
        // The rest of the sender's markup survives the rewrite.
        assert!(body.contains("width=\"420\""), "body was: {body}");
        // The image is rendered in place, so it isn't appended a second time.
        assert_eq!(body.matches("/9j/4AAQSkZJRg==").count(), 1, "body was: {body}");
    }

    #[test]
    fn body_search_keys_quote_and_declare_charset() {
        // #191: ASCII needles go as a plain quoted string over the listed
        // uids; quotes and backslashes are escaped; anything beyond ASCII
        // puts CHARSET UTF-8 at the head of the keys; line breaks vanish.
        assert_eq!(body_search_query("1:5,9", "unsubscribe"), r#"UID 1:5,9 BODY "unsubscribe""#);
        assert_eq!(body_search_query("1", r#"say "hi" \now"#), r#"UID 1 BODY "say \"hi\" \\now""#);
        assert_eq!(body_search_query("1", "café"), r#"CHARSET UTF-8 UID 1 BODY "café""#);
        assert_eq!(body_search_query("1", "a\r\nb"), r#"UID 1 BODY "ab""#);
    }

    #[test]
    fn url_query_values_are_percent_encoded() {
        assert_eq!(url_query_encode("\"body:opt out\""), "%22body%3Aopt%20out%22");
        assert_eq!(url_query_encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(url_query_encode("é"), "%C3%A9");
    }

    #[test]
    fn percent_encoded_cid_reference_still_resolves() {
        let raw = concat!(
            "Content-Type: multipart/related; boundary=R\r\n\r\n",
            "--R\r\n",
            "Content-Type: text/html\r\n\r\n",
            "<img src=\"cid:part1%40mail\">\r\n",
            "--R\r\n",
            "Content-Type: image/png\r\n",
            "Content-ID: <part1@mail>\r\n",
            "Content-Transfer-Encoding: base64\r\n\r\n",
            "iVBORw0KGgo=\r\n",
            "--R--\r\n",
        );
        let body = extract_body(raw.as_bytes());
        assert!(body.contains("data:image/png;base64,"), "body was: {body}");
    }

    #[test]
    fn cid_link_target_is_not_rewritten() {
        let raw = concat!(
            "Content-Type: multipart/related; boundary=R\r\n\r\n",
            "--R\r\n",
            "Content-Type: text/html\r\n\r\n",
            "<a href=\"cid:logo\"><img src=\"cid:logo\"></a>\r\n",
            "--R\r\n",
            "Content-Type: image/png\r\n",
            "Content-ID: <logo>\r\n",
            "Content-Transfer-Encoding: base64\r\n\r\n",
            "iVBORw0KGgo=\r\n",
            "--R--\r\n",
        );
        let body = extract_body(raw.as_bytes());
        assert!(body.contains("href=\"cid:logo\""), "body was: {body}");
        assert!(body.contains("src=\"data:image/png;"), "body was: {body}");
    }

    #[test]
    fn unresolvable_cid_reference_is_left_untouched() {
        let raw = b"Content-Type: text/html\r\n\r\n<p>see cid:nope</p><img src=\"cid:gone\">";
        let body = extract_body(raw);
        // A cid with no matching part stays as it was — and a bare mention in prose
        // is never treated as a resource reference.
        assert_eq!(body, "<p>see cid:nope</p><img src=\"cid:gone\">");
    }

    #[test]
    fn lone_html_part_passes_through_untouched() {
        let raw = b"Content-Type: text/html\r\n\r\n<p>hello</p>";
        assert_eq!(extract_body(raw), "<p>hello</p>");
    }

    #[test]
    fn plain_text_only_message_still_renders() {
        let raw = b"Content-Type: text/plain\r\n\r\nhello <there>";
        let body = extract_body(raw);
        assert!(body.contains("hello &lt;there&gt;"), "body was: {body}");
    }

    /// Parse a real server BODYSTRUCTURE response into the structure our IMAP
    /// summary path sees.
    fn bodystructure(raw: &str) -> async_imap::imap_proto::types::BodyStructure<'_> {
        use async_imap::imap_proto::{parser::parse_response, AttributeValue, Response};
        let (_, resp) = parse_response(raw.as_bytes()).expect("parses");
        match resp {
            Response::Fetch(_, attrs) => attrs
                .into_iter()
                .find_map(|a| match a {
                    AttributeValue::BodyStructure(bs) => Some(bs),
                    _ => None,
                })
                .expect("has BODYSTRUCTURE"),
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn inline_iphone_photo_counts_as_an_attachment() {
        // Apple marks the JPEG `INLINE` with no Content-ID; requiring a disposition
        // of `attachment` missed it, so no paperclip and no download.
        let bs = bodystructure(concat!(
            "* 1 FETCH (BODYSTRUCTURE (",
            "(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"us-ascii\") NIL NIL \"7BIT\" 4 2 NIL NIL NIL NIL)",
            "(\"IMAGE\" \"JPEG\" (\"NAME\" \"a.jpeg\") NIL NIL \"BASE64\" 100 NIL ",
            "(\"INLINE\" (\"FILENAME\" \"a.jpeg\")) NIL NIL)",
            "(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"us-ascii\") NIL NIL \"7BIT\" 40 4 NIL NIL NIL NIL)",
            " \"MIXED\" (\"BOUNDARY\" \"b\") NIL NIL NIL))\r\n",
        ));
        assert!(structure_has_attachment(&bs));
    }

    #[test]
    fn cid_logo_does_not_count_as_an_attachment() {
        // Same inline disposition, but a Content-ID: it's rendered in the body.
        let bs = bodystructure(concat!(
            "* 1 FETCH (BODYSTRUCTURE (",
            "(\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 20 1 NIL NIL NIL NIL)",
            "(\"IMAGE\" \"PNG\" (\"NAME\" \"logo.png\") \"<logo>\" NIL \"BASE64\" 100 NIL ",
            "(\"INLINE\" (\"FILENAME\" \"logo.png\")) NIL NIL)",
            " \"RELATED\" (\"BOUNDARY\" \"r\") NIL NIL NIL))\r\n",
        ));
        assert!(!structure_has_attachment(&bs));
    }

    #[test]
    fn plain_alternative_has_no_attachment() {
        let bs = bodystructure(concat!(
            "* 1 FETCH (BODYSTRUCTURE (",
            "(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 10 1 NIL NIL NIL NIL)",
            "(\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 20 1 NIL NIL NIL NIL)",
            " \"ALTERNATIVE\" (\"BOUNDARY\" \"a\") NIL NIL NIL))\r\n",
        ));
        assert!(!structure_has_attachment(&bs));
    }

    #[test]
    fn image_mime_rejects_a_hostile_subtype() {
        // Guards the `data:` URI against a subtype that would break out of it.
        let raw = b"Content-Type: image/\"onerror=alert(1)\r\n\r\nx";
        let parsed = mail_parser::MessageParser::default().parse(raw.as_slice()).unwrap();
        let part = parsed.html_bodies().next().unwrap();
        assert!(image_mime(part).is_none());
    }
}

#[cfg(test)]
mod uid_set_tests {
    use super::{quote_mailbox, uid_set};

    #[test]
    fn mailbox_names_are_quoted_for_copy() {
        assert_eq!(quote_mailbox("Deleted Messages"), "\"Deleted Messages\"");
        assert_eq!(quote_mailbox("INBOX"), "\"INBOX\"");
        assert_eq!(quote_mailbox("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[test]
    fn sets_are_sorted_deduplicated_and_ranged() {
        assert_eq!(uid_set(&[1005, 1003, 999, 1004]), "999,1003:1005");
        assert_eq!(uid_set(&[7, 7, 7]), "7");
        assert_eq!(uid_set(&[3, 4, 5, 9]), "3:5,9");
        assert_eq!(uid_set(&[]), "");
    }
}

#[cfg(test)]
mod attachment_scan_tests {
    use super::{mime_extension, structure_attachments, INLINE_ATTACHMENT_MIN};
    use async_imap::imap_proto::types::{
        BodyContentCommon, BodyContentSinglePart, BodyStructure, ContentDisposition, ContentEncoding,
        ContentType,
    };
    use std::borrow::Cow;

    fn ctype(ty: &'static str, subtype: &'static str, name: Option<&'static str>) -> ContentType<'static> {
        ContentType {
            ty: Cow::Borrowed(ty),
            subtype: Cow::Borrowed(subtype),
            params: name.map(|n| vec![(Cow::Borrowed("name"), Cow::Borrowed(n))]),
        }
    }

    fn disposition(ty: &'static str, filename: Option<&'static str>) -> ContentDisposition<'static> {
        ContentDisposition {
            ty: Cow::Borrowed(ty),
            params: filename.map(|f| vec![(Cow::Borrowed("filename"), Cow::Borrowed(f))]),
        }
    }

    fn single(
        ty: &'static str,
        subtype: &'static str,
        octets: u32,
        encoding: ContentEncoding<'static>,
        disp: Option<ContentDisposition<'static>>,
        content_id: Option<&'static str>,
        name: Option<&'static str>,
    ) -> BodyStructure<'static> {
        BodyStructure::Basic {
            common: BodyContentCommon {
                ty: ctype(ty, subtype, name),
                disposition: disp,
                language: None,
                location: None,
            },
            other: BodyContentSinglePart {
                id: content_id.map(Cow::Borrowed),
                md5: None,
                description: None,
                transfer_encoding: encoding,
                octets,
            },
            extension: None,
        }
    }

    fn text(subtype: &'static str, octets: u32) -> BodyStructure<'static> {
        BodyStructure::Text {
            common: BodyContentCommon {
                ty: ctype("text", subtype, None),
                disposition: None,
                language: None,
                location: None,
            },
            other: BodyContentSinglePart {
                id: None,
                md5: None,
                description: None,
                transfer_encoding: ContentEncoding::SevenBit,
                octets,
            },
            lines: 10,
            extension: None,
        }
    }

    fn multipart(bodies: Vec<BodyStructure<'static>>) -> BodyStructure<'static> {
        BodyStructure::Multipart {
            common: BodyContentCommon {
                ty: ctype("multipart", "mixed", None),
                disposition: None,
                language: None,
                location: None,
            },
            bodies,
            extension: None,
        }
    }

    /// The ordinary case: a message with a body and one attached file. The body
    /// is not an attachment; the file is, named from its disposition, and its
    /// section is what a FETCH would ask for.
    #[test]
    fn a_body_and_one_attachment_yields_just_the_attachment() {
        let bs = multipart(vec![
            text("plain", 400),
            single(
                "application",
                "pdf",
                4000,
                ContentEncoding::Base64,
                Some(disposition("attachment", Some("report.pdf"))),
                None,
                None,
            ),
        ]);
        let found = structure_attachments(&bs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "report.pdf");
        assert_eq!(found[0].mime, "application/pdf");
        assert_eq!(found[0].section, "2", "the second part of the multipart");
        assert_eq!(found[0].size, 3000, "base64 inflates by 4/3, so decode it back");
    }

    /// A newsletter's logo: inline, small, with a Content-ID. Decoration, not a
    /// file anyone wants in their gallery.
    #[test]
    fn a_small_inline_image_with_a_content_id_is_decoration() {
        let bs = multipart(vec![
            text("html", 900),
            single("image", "png", 2000, ContentEncoding::Base64, None, Some("logo@x"), None),
        ]);
        assert!(structure_attachments(&bs).is_empty());
    }

    /// The same shape, but big enough to be a photograph someone sent: Apple
    /// Mail attaches pictures exactly this way, and they have to be saveable.
    #[test]
    fn a_large_inline_image_is_content_however_it_is_labelled() {
        let octets = (INLINE_ATTACHMENT_MIN as u32 + 1) * 4 / 3 + 8;
        let bs = multipart(vec![
            text("plain", 40),
            single("image", "jpeg", octets, ContentEncoding::Base64, None, Some("img@x"), None),
        ]);
        let found = structure_attachments(&bs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "attachment-1.jpg", "unnamed, so named by its type");
    }

    /// Nested multiparts number their sections through: this is what an
    /// alternative body beside an attachment looks like.
    #[test]
    fn nested_parts_are_numbered_through() {
        let bs = multipart(vec![
            multipart(vec![text("plain", 100), text("html", 200)]),
            single(
                "application",
                "zip",
                900,
                ContentEncoding::Base64,
                Some(disposition("attachment", Some("bundle.zip"))),
                None,
                None,
            ),
        ]);
        let found = structure_attachments(&bs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].section, "2");
    }

    /// A text part is only an attachment when it says so — otherwise every
    /// message's own body would turn up in the gallery.
    #[test]
    fn a_text_part_counts_only_when_it_declares_itself() {
        let plain = multipart(vec![text("plain", 500)]);
        assert!(structure_attachments(&plain).is_empty());

        let attached = multipart(vec![BodyStructure::Text {
            common: BodyContentCommon {
                ty: ctype("text", "csv", None),
                disposition: Some(disposition("attachment", Some("rows.csv"))),
                language: None,
                location: None,
            },
            other: BodyContentSinglePart {
                id: None,
                md5: None,
                description: None,
                transfer_encoding: ContentEncoding::SevenBit,
                octets: 700,
            },
            lines: 9,
            extension: None,
        }]);
        let found = structure_attachments(&attached);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "rows.csv");
        assert_eq!(found[0].size, 700, "not base64, so the octets stand");
    }

    /// The filename falls back to the content type's own `name` parameter,
    /// which is where older clients put it.
    #[test]
    fn a_name_parameter_is_used_when_there_is_no_filename() {
        let bs = multipart(vec![
            text("plain", 10),
            single(
                "application",
                "msword",
                600,
                ContentEncoding::SevenBit,
                Some(disposition("attachment", None)),
                None,
                Some("minutes.doc"),
            ),
        ]);
        assert_eq!(structure_attachments(&bs)[0].name, "minutes.doc");
    }

    /// An RFC 2047 filename is decoded, not shown as the encoded word.
    #[test]
    fn an_encoded_filename_is_decoded() {
        let bs = multipart(vec![
            text("plain", 10),
            single(
                "application",
                "pdf",
                600,
                ContentEncoding::SevenBit,
                Some(disposition("attachment", Some("=?utf-8?Q?rapport=20final?="))),
                None,
                None,
            ),
        ]);
        assert_eq!(structure_attachments(&bs)[0].name, "rapport final");
    }

    /// A message that is not multipart at all is one part, section 1.
    #[test]
    fn a_single_part_message_is_section_one() {
        let bs = single(
            "application",
            "pdf",
            800,
            ContentEncoding::SevenBit,
            Some(disposition("attachment", Some("lone.pdf"))),
            None,
            None,
        );
        let found = structure_attachments(&bs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].section, "1");
    }

    #[test]
    fn only_image_types_get_a_guessed_extension() {
        assert_eq!(mime_extension("image", "jpeg"), ".jpg");
        assert_eq!(mime_extension("IMAGE", "PNG"), ".png");
        assert_eq!(mime_extension("application", "pdf"), "");
        assert_eq!(mime_extension("image", "svg+xml"), "");
    }
}
