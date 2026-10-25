//! What the UI and an account's worker say to each other: the requests the
//! UI sends ([`MailRequest`]), the events the worker answers with
//! ([`WorkerEvent`]), and the outgoing message a send carries.

use crate::config::AccountConfig;
use crate::models::{Account, Folder, KeywordFinding, MailboxQuota, Message, ThreadSummary};
use crate::worker::accent_for;

/// A request from the UI to the worker.
#[derive(Debug)]
pub(crate) enum MailRequest {
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
    MarkSpam {
        path: String,
        uid: u32,
        dest: String,
    },
    /// The opposite (#168): tag `$NotJunk`, clear `$Junk`, and move the
    /// message back to `dest` (the Inbox).
    MarkHam {
        path: String,
        uid: u32,
        dest: String,
    },
    /// [`MailRequest::MarkHam`] for a selection, in one server round; answers
    /// with [`WorkerEvent::BulkComplete`] like `MoveMessages`.
    MarkHamMany {
        path: String,
        uids: Vec<u32>,
        dest: String,
    },
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
    /// Work through a folder's attachment-carrying messages that have never
    /// been described, recording what each one holds without downloading it.
    ScanAttachments { folder_path: String },
    /// Assemble a conversation from the local cache across every folder of the
    /// account: the messages whose Message-ID or references match `ids`. Answers
    /// with [`WorkerEvent::Related`]; never touches the network.
    LoadRelated { message_id: u32, ids: Vec<String> },
    /// What each of these conversations looks like whole: how big it really
    /// is, counting the members in the account's other folders (#222), and
    /// which message moved it last, the replies in Sent included (#236). One
    /// `(tag, Message-IDs)` per thread on the list's page; answers with
    /// [`WorkerEvent::ThreadSummaries`] and, like [`MailRequest::LoadRelated`],
    /// never touches the network.
    LoadThreadSummaries { groups: Vec<(String, Vec<String>)> },
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
    UndoMove {
        path: String,
        dest: String,
        dest_folder_id: u32,
        message_ids: Vec<String>,
    },
    /// Bring snoozed mail back (#527): find `message_ids` in the Snoozed
    /// folder `path`, mark them unread, move them to `dest` and reload it.
    /// Answered with [`WorkerEvent::Unsnoozed`].
    Unsnooze {
        path: String,
        dest: String,
        dest_folder_id: u32,
        message_ids: Vec<String>,
    },
    /// Find a message by its (normalized) RFC Message-ID across every folder
    /// of the account, for a `mid:` link (#130) the cache could not resolve.
    /// Answered with [`WorkerEvent::Located`] either way.
    Locate { message_id: String },
    /// Move/rename a mailbox (drag-and-drop in the sidebar, #51). The server
    /// renames any child hierarchy along with it (RFC 3501 §6.3.5).
    RenameFolder { old_path: String, new_path: String },
    /// Delete a mailbox, first moving its contents to `trash` (if set).
    DeleteFolder { path: String, trash: Option<String> },
    /// The account's hidden folders changed (#239): list again without
    /// them, so they leave the sidebar and every sync at once.
    SetHiddenFolders { paths: Vec<String> },
    /// Send a new message over SMTP, optionally APPENDing a copy to `sent_path`.
    Send {
        message: Box<OutgoingMessage>,
        sent_path: Option<String>,
        /// Echoed back in [`WorkerEvent::SendDone`], so the app can tell
        /// which of its messages on their way out this was (#434). 0 for a
        /// send nobody is watching.
        ticket: u64,
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
        /// The composer saving it on its own, every so often (#340): its
        /// id comes back in [`WorkerEvent::DraftSaved`], and a failure is
        /// quiet, with the composer still open to try again.
        autosave: Option<u32>,
        /// Saved as a template (#360): `path` is the Templates folder, and
        /// the copy is filed as read mail rather than as a draft.
        template: bool,
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
    /// Sent right behind a request that takes mail out of `path` (a move,
    /// a purge, Spam/Not Spam) and answered with
    /// [`WorkerEvent::MovesSettled`] once the worker reaches it. The worker
    /// serves requests in order, so by then the one ahead has run, however
    /// it went; until then a folder list fetched earlier still shows the
    /// mail, and the app keeps it off screen (#255).
    Settle { path: String, uids: Vec<u32> },
    /// Moving mail to another account (#265), step one: the message's raw
    /// bytes, answered with [`WorkerEvent::RawExported`] carrying `token`.
    /// `for_reader` when the user is waiting on it: a draft being opened.
    ExportRaw {
        token: u64,
        path: String,
        uid: u32,
        for_reader: bool,
    },
    /// Step two, on the receiving account: add `raw` to `path` with its
    /// read and starred state, answered with [`WorkerEvent::RawImported`].
    ImportRaw {
        token: u64,
        path: String,
        raw: Vec<u8>,
        seen: bool,
        flagged: bool,
    },
    /// Take one attachment out of a message on the server (#289), found by
    /// its name and, among files sharing the name, its decoded size.
    /// IMAP and JMAP store a copy without it and delete the original, so
    /// the message comes back under a new UID; Microsoft 365 deletes the
    /// attachment in place. Answered with [`WorkerEvent::AttachmentDeleted`]
    /// or an error.
    DeleteAttachment {
        message_id: u32,
        path: String,
        uid: u32,
        name: String,
        size: u64,
    },
    /// The tag finder (Settings → Tags → Find Tags…): report every keyword
    /// in use across the account's folders as one
    /// [`WorkerEvent::KeywordsFound`] — always answered, even when empty, so
    /// the app can tell when every account has reported.
    FindKeywords,
    /// How much of the mailbox's storage is in use (#298), for the account's
    /// Settings page. Always answered with [`WorkerEvent::Quota`], `None` when
    /// the server gives no figure.
    Quota,
    /// Bring every folder's cached keywords in line with the server (#166):
    /// a tag set or cleared on another device reaches the tag views here
    /// without the folder it sits in being opened. `keywords` are the
    /// configured tags. Answered with [`WorkerEvent::KeywordsSynced`].
    RefreshKeywords { keywords: Vec<String> },
}

/// A message composed by the user, ready to send.
#[derive(Debug, Clone)]
pub(crate) struct OutgoingMessage {
    /// The account to send from.
    pub(crate) from_account_id: u32,
    /// Send-as alias (#34): the full From for the wire ("Name <alias@host>"),
    /// or `None` to send as the account itself. The mail leaves through the
    /// account's SMTP, unless the alias is configured with its own transport.
    pub(crate) from_alias: Option<String>,
    /// Comma-separated recipient addresses.
    pub(crate) to: String,
    pub(crate) cc: String,
    pub(crate) bcc: String,
    /// Reply-To addresses (comma-separated); empty for none (#58).
    pub(crate) reply_to: String,
    pub(crate) subject: String,
    /// Plain-text body (always present; the `text/plain` alternative).
    pub(crate) body: String,
    /// HTML body. When non-empty the mail is sent multipart/alternative with this
    /// as the `text/html` part and `body` as the plain fallback.
    pub(crate) html: String,
    /// File paths to attach.
    pub(crate) attachments: Vec<String>,
    /// The Message-ID this is a reply to, and the thread's id chain — both
    /// normalized (no angle brackets), as [`crate::models::Message`] stores
    /// them. Empty for a message that starts a conversation. Without these
    /// headers a reply is a new thread to every client that receives it,
    /// including Hylki.
    pub(crate) in_reply_to: String,
    pub(crate) references: String,
    /// The Message-ID of the message this forwards, normalized; empty for
    /// anything else. Marked forwarded once this has gone (#454).
    pub(crate) forwards: String,
    /// When editing an existing draft, the draft being replaced (removed from the
    /// Drafts folder after this message is saved or sent).
    pub(crate) draft_origin: Option<crate::models::DraftOrigin>,
    /// When this came out of the Outbox for editing, the queued row it replaces.
    /// Dropped once this version is sent or re-queued, so the queue never holds
    /// the message twice.
    pub(crate) outbox_origin: Option<u32>,
    /// Sign with the account's OpenPGP key (#133): PGP/MIME multipart/signed.
    pub(crate) sign: bool,
    /// Encrypt to every recipient's key (and the sender's): multipart/encrypted,
    /// signed inside when `sign` is set too.
    pub(crate) encrypt: bool,
    /// Send Later (#145): unix seconds to send at. `None` sends now. A time
    /// already past sends now too.
    pub(crate) send_at: Option<i64>,
    /// An answer to a meeting invitation (#223): the iCalendar document and
    /// its iTIP method, sent as a `text/calendar` alternative beside the
    /// plain text so a calendar reads the answer and a person reads the
    /// sentence.
    pub(crate) calendar: Option<CalendarPart>,
}

/// The calendar half of an outgoing message (#223).
#[derive(Debug, Clone)]
pub(crate) struct CalendarPart {
    /// The iCalendar document.
    pub(crate) ics: String,
    /// The iTIP method, which belongs on the part's own Content-Type as
    /// well as in the document: a recipient's calendar reads it there
    /// first, and Exchange ignores a part that does not carry it.
    pub(crate) method: String,
}

/// An event pushed from the worker back to the UI.
// Events cross the channel one at a time and are matched all over the app;
// boxing the large variant would change every place that builds or reads it.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum WorkerEvent {
    /// A batch of a folder's attachments was described (not downloaded).
    /// `remaining` is how many of its messages are still unscanned.
    AttachmentsScanned {
        folder_path: String,
        added: u32,
        remaining: u32,
    },
    Account(Account),
    /// The sending identities a JMAP server keeps for the account (#346):
    /// `(name, address)`, the address `*@domain` for one that sends as
    /// anyone at the domain.
    Identities(Vec<(String, String)>),
    Folders(Vec<Folder>),
    Messages {
        folder_id: u32,
        messages: Vec<Message>,
    },
    /// What the server found for the "Message body" filter conditions
    /// (#191) among a folder's just-listed mail: uid → the lowercased
    /// alternatives it contains. Sent just ahead of the `Messages` it
    /// describes, and only for an inbox with body rules.
    BodyHits {
        folder_id: u32,
        hits: std::collections::HashMap<u32, Vec<String>>,
    },
    /// Additional indexed message summaries for a folder, produced by the
    /// background backfill. Merged into the existing index without replacing it.
    MessagesAppend {
        folder_id: u32,
        messages: Vec<Message>,
    },
    /// The rest of a conversation, gathered from the cache across the account's
    /// folders in answer to [`MailRequest::LoadRelated`]. `message_id` echoes the
    /// message it was asked for, so a late answer to a message the user has
    /// already moved on from can be ignored.
    Related {
        message_id: u32,
        messages: Vec<Message>,
    },
    /// What the whole conversation looks like for the threads the list asked
    /// about, tagged as it tagged them (#222, #236). Only threads the cache
    /// found something for are listed; the rest keep the count, the sender and
    /// the preview the list worked out from its own page.
    ThreadSummaries {
        summaries: Vec<(String, ThreadSummary)>,
    },
    /// An undone move put these messages back in `folder_id`; sent after the
    /// folder's fresh [`WorkerEvent::Messages`] so the app can land the user
    /// on the restored message instead of wherever the reload left the list.
    Restored {
        folder_id: u32,
        message_ids: Vec<String>,
    },
    /// The answer to [`MailRequest::Unsnooze`]. `done` when the messages
    /// went back, or were no longer in the Snoozed folder to go; false when
    /// the server could not be asked, so the app tries again later.
    Unsnoozed {
        message_ids: Vec<String>,
        done: bool,
    },
    /// The answer to [`MailRequest::Locate`]: where the message with that
    /// Message-ID lives (`folder_path`, `uid`), or `None` when no folder of
    /// this account has it.
    Located {
        message_id: String,
        hit: Option<(String, u32)>,
    },
    /// The answer to [`MailRequest::FindKeywords`]: the keywords in use on
    /// this account, system flags left out.
    KeywordsFound(Vec<KeywordFinding>),
    /// The answer to [`MailRequest::Quota`].
    Quota(Option<MailboxQuota>),
    /// The answer to [`MailRequest::RefreshKeywords`]: the folders whose
    /// cached keywords changed (empty when nothing did).
    KeywordsSynced {
        paths: Vec<String>,
    },
    /// The background backfill for a folder finished — its whole index is now
    /// present, so the UI can stop expecting more rows to stream in.
    BackfillDone {
        folder_id: u32,
    },
    /// A folder's load has been all the way to the server and back, however
    /// it went. Sent after the `Messages` the server's answer produced (and
    /// after an error, which produces none), so a manual filter run (#198)
    /// knows the folder is done rather than stopping at the cache's answer,
    /// which arrives first and instantly.
    FolderSynced {
        folder_id: u32,
    },
    /// Server-side unread count for a folder (from STATUS/SEARCH, independent of
    /// the loaded window — accurate even for multi-thousand mailboxes).
    FolderUnread {
        folder_id: u32,
        unread: u32,
    },
    /// The same, from a per-folder IDLE watcher ([`watch_folder`]), which knows
    /// its folder only by path: folder ids are positional and shift when the
    /// list changes, but a path stays true for as long as the folder exists.
    /// The app resolves it against whatever list it currently holds.
    FolderUnreadByPath {
        path: String,
        unread: u32,
    },
    /// A `SetSeen` has been stored (or failed): the app stops holding its own
    /// read state for that message over what the server reports.
    SeenSettled {
        path: String,
        uid: u32,
    },
    /// A message was replied to or forwarded from here, and is marked so
    /// (#454).
    Answered {
        path: String,
        uid: u32,
        forwarded: bool,
    },
    /// A `SetKeyword` has been stored (or failed), the same for a tag.
    TagSettled {
        path: String,
        uid: u32,
        keyword: String,
    },
    /// The answer to [`MailRequest::Settle`]: whatever took these messages
    /// out of `path` has been done or has failed.
    MovesSettled {
        path: String,
        uids: Vec<u32>,
    },
    /// The answer to [`MailRequest::ExportRaw`].
    RawExported {
        token: u64,
        raw: Result<Vec<u8>, String>,
    },
    /// The answer to [`MailRequest::ImportRaw`].
    RawImported {
        token: u64,
        result: Result<(), String>,
    },
    /// The answer to [`MailRequest::DeleteAttachment`]: the message that was
    /// `uid` in `path` is now `new_uid` there, `None` when the copy could not
    /// be found again. The folder's list has been sent again first.
    AttachmentDeleted {
        message_id: u32,
        path: String,
        uid: u32,
        new_uid: Option<u32>,
        name: String,
        size: u64,
    },
    /// [`MailRequest::DeleteAttachment`] did not happen; an `Error` saying
    /// why goes with it. The file stays where it was.
    AttachmentNotDeleted {
        message_id: u32,
        name: String,
        size: u64,
    },
    /// A chunk of a folder's threading references was repaired, so what the list
    /// is showing may now group differently; or its Gmail labels were filled
    /// in (#374).
    RefsRepaired {
        folder_id: u32,
    },
    /// `path` is the folder the body was read from. A UID is unique only within
    /// its folder, so without it a background prefetch's body can be applied to a
    /// different message that happens to share the number.
    Body {
        message_id: u32,
        path: String,
        body: String,
    },
    /// A message the cache listed is no longer in `path` on the server; its
    /// cached row has been dropped.
    Gone {
        message_id: u32,
        path: String,
        uid: u32,
    },
    /// Whether the message's From: address survived its provider's SPF/DKIM/DMARC
    /// checks. Sent right after `Body`, from the same fetch. This and the
    /// attachment events below name the folder, like `Body`: a UID means a
    /// message only within its folder.
    SenderChecked {
        path: String,
        message_id: u32,
        check: crate::models::SenderCheck,
    },
    Source {
        text: String,
    },
    Attachments {
        path: String,
        message_id: u32,
        items: Vec<crate::models::Attachment>,
    },
    /// The message has attachments that aren't cached; the UI should offer to
    /// download them rather than fetching automatically.
    AttachmentsPending {
        path: String,
        message_id: u32,
    },
    /// A message flagged as having an attachment turned out to have none once its
    /// body was fetched (e.g. iCloud marketing mail whose only extra parts are
    /// inline `cid:` images). The UI should drop its paperclip.
    NoAttachments {
        path: String,
        message_id: u32,
    },
    /// The opposite: a message with no paperclip turned out to carry attachments
    /// after all (an inline PDF the structure didn't advertise — issue #9). The
    /// UI should show the paperclip and offer the files.
    HasAttachments {
        path: String,
        message_id: u32,
    },
    Sent,
    /// The worker is done with the `MailRequest::Send` carrying `ticket`:
    /// `sent` if it went out, otherwise it was queued in the Outbox or
    /// handed back with `Unsent` (#434).
    SendDone {
        ticket: u64,
        sent: bool,
    },
    /// A message that could be neither sent, queued nor saved as a draft, handed
    /// back so the app can open it in a composer again (#340): its composer
    /// closed when Send or Save was pressed, so this is the only copy left.
    Unsent(Box<OutgoingMessage>),
    /// Something worth telling the user that isn't a failure — a queued message
    /// going out on its own, say.
    Notice(String),
    /// The account's Outbox, whenever it changes (queued, retried, sent or
    /// discarded). Empty means nothing is waiting.
    Outbox {
        items: Vec<crate::models::OutboxItem>,
    },
    /// A draft was saved to the Drafts folder, under this Message-ID
    /// (`None`: an automatic save that did not go through). `autosave`
    /// echoes the request's.
    DraftSaved {
        autosave: Option<u32>,
        message_id: Option<String>,
    },
    /// A bulk MoveMessages request finished (success or failure) — drives the
    /// bulk-action spinner in the UI.
    BulkComplete,
    Status(String),
    /// `connectivity` marks connection/sync errors that should auto-clear once
    /// a later connect or sync succeeds.
    Error {
        text: String,
        connectivity: bool,
    },
}

impl WorkerEvent {
    /// A failure the user has to see, which stays until dismissed.
    pub(super) fn error(text: impl Into<String>) -> Self {
        WorkerEvent::Error {
            text: text.into(),
            connectivity: false,
        }
    }

    /// A connection or sync failure, which the next success clears.
    pub(in crate::worker) fn net_error(text: impl Into<String>) -> Self {
        WorkerEvent::Error {
            text: text.into(),
            connectivity: true,
        }
    }

    /// The account as the sidebar shows it: sent first, before any network.
    pub(super) fn account(account_id: u32, account: &AccountConfig) -> Self {
        WorkerEvent::Account(Account {
            id: account_id,
            name: account.name.clone(),
            email: account.email.clone(),
            label: account.display_label(),
            accent: accent_for(account_id).into(),
        })
    }
}
