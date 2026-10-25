//! Every message the root component handles: user actions, worker events,
//! child component outputs and showcase hooks.
//!
//! One flat enum, the usual relm4 shape. The variants are grouped by the
//! `on_<concern>` method `AppModel::route` (router.rs) hands them to, in the
//! router's order, each group under a comment naming the method and its file.

use std::collections::HashMap;

use relm4::prelude::*;

use crate::config::{self, AccountConfig};
use crate::models::{
    Account, Attachment, Folder, FolderKind, KeywordFinding, Message, ThreadSummary,
};
use crate::ui::attachments_gallery::GalleryRequest;
use crate::ui::message_list::{BulkAction, QuickFilter, RowAction};
use crate::ui::preferences::PrefOutput;
use crate::ui::sidebar::CtxAction;
use crate::worker::OutgoingMessage;

use super::attachments::AttachmentTarget;
use super::hand_off::{FileHandOff, HandOffTarget};
use super::types::UnifiedView;
use crate::ui::keymap::{Shortcut, WindowCompose};

#[derive(Debug)]
pub(crate) enum AppMsg {
    // ---- the busy-pointer gate (router.rs) ----
    /// A composer asked for, built now that the busy pointer is drawn.
    ComposeNow(Box<AppMsg>),

    // ---- on_outbox (outbox.rs) ----
    /// Show the Outbox (queued, unsent messages).
    ShowOutbox,
    /// A worker reported its queue: replaces that account's entries.
    OutboxItems {
        account_id: u32,
        items: Vec<crate::models::OutboxItem>,
    },
    /// Edit the queued message currently open in the reader.
    EditCurrentOutbox,
    /// Try to send the queued message currently open in the reader.
    SendCurrentOutbox,
    /// Send Later (#145): the half-minute clock that sends due queued mail.
    SendDueScheduled,
    /// Try to send everything waiting, across accounts.
    RetryAllOutbox,

    // ---- on_notifications (notifications.rs) ----
    /// A worker reported something noteworthy that isn't a failure.
    Notice(String),
    /// A new-mail desktop notification was clicked — open that message.
    OpenMessageFromNotification {
        account_id: u32,
        folder_id: u32,
        message_id: u32,
    },
    /// A notification action button (#38): mark the notified message read,
    /// without raising the window.
    NotificationMarkRead {
        account_id: u32,
        folder_id: u32,
        message_id: u32,
    },
    /// A notification's Archive button: archive the message, without
    /// raising the window.
    NotificationArchive {
        account_id: u32,
        folder_id: u32,
        message_id: u32,
    },
    /// A notification's Delete button: the message to Trash, as the Delete
    /// key does, without raising the window.
    NotificationDelete {
        account_id: u32,
        folder_id: u32,
        message_id: u32,
    },
    /// A notification's Spam button: mark the message as spam, without
    /// raising the window.
    NotificationSpam {
        account_id: u32,
        folder_id: u32,
        message_id: u32,
    },
    /// A notification's Reply button (#244): the message opens as a click on
    /// the notification opens it, and a reply follows once its body is in.
    NotificationReply {
        account_id: u32,
        folder_id: u32,
        message_id: u32,
    },
    /// A notification's Forward button (#244), as Reply.
    NotificationForward {
        account_id: u32,
        folder_id: u32,
        message_id: u32,
    },
    /// A worker's status line.
    Status { account_id: u32, text: String },
    /// A worker's error, for the log and the notification bar;
    /// `connectivity` marks one that clears itself once a later connect or
    /// sync succeeds.
    Error {
        account_id: u32,
        text: String,
        connectivity: bool,
    },
    /// An LDAP directory could not be asked (#307), said once per failure.
    DirectoryError(String),
    /// How many notices the notification bar holds.
    NotifyCount(usize),
    /// Show or hide the notification bar.
    ToggleNotifications,
    /// The notification bar was shown or hidden.
    StatusBarShown(bool),

    // ---- on_gallery (gallery.rs) ----
    /// Show the attachments gallery (sidebar "Attachments" row).
    ShowAttachments,
    /// Download an attachment the gallery knows of but has never fetched.
    GalleryFetch {
        account_id: u32,
        folder_path: String,
        uid: u32,
    },
    /// A chunk of a folder's attachments was described by the scan.
    AttachmentsScanned {
        account_id: u32,
        folder_path: String,
        added: u32,
        remaining: u32,
    },
    /// The gallery wants a page of attachments matching this query.
    GalleryLoad(Box<GalleryRequest>),
    /// Gallery "Go to Message" — open the attachment's source message.
    OpenAttachmentMessage {
        account_id: u32,
        folder_path: String,
        uid: u32,
    },

    // ---- on_navigation (navigation.rs) ----
    /// A unified row was chosen: All Inboxes, the unified section's Starred
    /// / Sent / Drafts row (every account's folder of that kind), or its
    /// Filtered Folders row (every rule's folder).
    UnifiedSelected(UnifiedView),
    /// An account's starred mail, for an account with no Starred folder
    /// (#482): the unified Starred view narrowed to it.
    AccountStarredSelected(u32),
    /// A folder row was chosen (the sidebar, the command palette): list it.
    FolderSelected {
        account_id: u32,
        folder_id: u32,
        name: String,
        path: String,
    },

    // ---- on_sidebar (sidebar.rs) ----
    /// An account section opened or folded in the sidebar.
    AccountFolded { id: u32, folded: bool },
    /// An account's custom folders were shown or hidden in the sidebar:
    /// remember it.
    ToggleCustomFolders(u32),
    /// A run of sibling folders was put in a new order in the sidebar.
    ReorderFolders { account_id: u32, paths: Vec<String> },
    /// A folder was dropped beside one at another level: move it there on
    /// the server, then put it just before or after `target`.
    PlaceFolder {
        account_id: u32,
        path: String,
        dest: String,
        target: String,
        after: bool,
    },
    /// An account's own folder order, chosen from its Folders heading.
    SetAccountFolderSort {
        account_id: u32,
        sort: Option<config::FolderSort>,
    },
    /// A sidebar folder-tree node was collapsed/expanded (#51) — persist it.
    FolderNodeCollapsed {
        account_id: u32,
        path: String,
        collapsed: bool,
    },
    /// The sidebar's All Inboxes / Filtered Folders / Tags sections were
    /// opened or folded — record it with the layout.
    SidebarSectionsOpen {
        all_inboxes: bool,
        filtered: bool,
        tags: bool,
        starred: bool,
        sent: bool,
        drafts: bool,
        archive: bool,
        junk: bool,
        trash: bool,
        favorites: bool,
        smart: bool,
        contacts: bool,
    },
    /// The accounts were dragged into a new order (their addresses, in
    /// order): the display order only, nothing reconnects.
    AccountsReordered(Vec<String>),
    /// An account's own Tags section was opened or folded: record it with
    /// the layout.
    ToggleAccountTags(u32),

    // ---- on_sidebar_peek (sidebar_peek.rs) ----
    /// The sidebar folded to its icon rail (true) or opened out again.
    SidebarCollapsed(bool),
    /// The narrow-window breakpoint applied/unapplied — collapse the sidebar to
    /// its icon rail (and restore it) without touching the user's preference.
    AutoRail(bool),
    /// The message-pane header's sidebar button: flip the sidebar between the
    /// expanded pane and the icon rail. Routed through the sidebar component so
    /// its CollapsedChanged output drives the same peek/pin/persist logic as
    /// ever (see `SidebarCollapsed`).
    ToggleSidebar,
    /// The floating sidebar overlay was dismissed from outside (a click on the
    /// dimmed content, an Escape) rather than via the sidebar's own button.
    SidebarPeekDismissed,
    /// The cursor entered the sidebar pane — open the hover peek (rail +
    /// preference permitting).
    SidebarHoverEnter,

    // ---- on_folder_menu (folder_menu.rs) ----
    /// A choice from a sidebar row's right-click menu.
    SidebarContext(CtxAction),
    /// Messages were dropped on a sidebar folder — move them there. `items` is
    /// the dragged selection as (account, folder, uid, id).
    DropMoveMessages {
        dest_account: u32,
        dest: String,
        items: Vec<(u32, u32, u32, u32)>,
    },
    /// Create a custom folder under an account (from the right-click menu).
    CreateFolder { account_id: u32, name: String },
    /// Move a folder under a new parent (sidebar drag-and-drop, #51).
    /// `dest` is the new parent's path, or "" for the account's top level.
    MoveFolder {
        account_id: u32,
        path: String,
        dest: String,
    },
    /// Rename a folder's leaf name in place (context menu).
    RenameFolderTo {
        account_id: u32,
        path: String,
        new_name: String,
    },
    /// Erase everything in a Trash or Junk folder (#152).
    EmptyFolder {
        account_id: u32,
        folder_id: u32,
        path: String,
    },
    /// Delete a custom folder (its contents are moved to Trash first).
    DeleteFolder { account_id: u32, path: String },

    // ---- on_search (search.rs) ----
    /// The indexes of the folders not opened this run, read from disk for
    /// the search `gen` started (#472).
    SearchPoolLoaded {
        gen: u64,
        lists: Vec<((u32, u32), Vec<Message>)>,
    },
    /// The pool [`AppModel::build_search_pool`] put together.
    SearchPoolBuilt {
        gen: u64,
        pool: Vec<Message>,
        text: Vec<Box<str>>,
    },
    /// The search field became active/inactive — supply or drop the cross-folder
    /// search pool (every folder's messages, so search can span the mailbox).
    SearchActive(bool),

    // ---- on_reader_select (reader_select.rs) ----
    /// Gmail's o or Enter opened a row (#392): the keyboard goes to the
    /// reader, now if the row was already the one open, else once it opens.
    OpenedByKey { already_open: bool },
    /// The message list has no selection to show (e.g. the last message was
    /// removed), so the reader should clear.
    ClearReader,
    /// `solo` marks a reply the user picked out of a conversation on screen:
    /// show that message alone and don't go looking for its siblings.
    MessageSelected {
        message: Message,
        thread: Vec<Message>,
        solo: bool,
    },
    /// The open conversation gained a member (a reply synced in while it
    /// was on screen): show it in place, without a re-selection.
    ThreadGrew {
        message: Box<Message>,
        thread: Vec<Message>,
    },
    /// The rest of an open message's conversation, found in other folders.
    Related {
        account_id: u32,
        message_id: u32,
        messages: Vec<Message>,
    },
    /// The list rebuilt its rows: which conversations are on screen, and the
    /// Message-IDs each is threaded by, so their real sizes can be looked up.
    ThreadsListed {
        groups: Vec<(u32, String, Vec<String>)>,
    },
    /// What the conversations on the list really look like, read across the
    /// account's folders: their true size (#222) and their newest message
    /// (#236). Tags are the list's own thread keys.
    ThreadSummaries {
        account_id: u32,
        summaries: Vec<(String, ThreadSummary)>,
    },
    /// Reveal (or toggle away) the message list's search bar (#102).
    OpenListSearch,
    /// Open the reader's in-message find bar (#103).
    OpenReaderFind,
    /// Turn one of the list header's quick filters on or off (#97, #441).
    SetQuickFilter(QuickFilter, bool),
    /// Turn every quick filter off.
    ClearQuickFilters,
    /// Delay-policy read marking (#100): fires a couple of seconds after a
    /// message opened; only applies if it is still the one on screen.
    DeferredMarkRead { message: Box<Message> },
    /// The list's selection changed; the reader outlines the matching cards.
    SelectionKeys(Vec<crate::ui::message_row::Slot>),
    /// Gmail's x checked rows, this many, or stopped checking (#392).
    ListChecked(Option<usize>),
    /// The reader's selection changed — mirror what the list can represent.
    SelectCards(Vec<(u32, u32)>),
    /// A conversation message was scrolled all the way through — mark it read on
    /// the server, in the caches and in the list.
    ThreadMessageSeen { account_id: u32, id: u32 },
    /// The conversation on screen has new bodies to show (coalesced).
    RenderThread,

    // ---- on_popout (popout.rs) ----
    /// Double-click: open the message in its own standalone window.
    OpenMessageWindow {
        message: Message,
        thread: Vec<Message>,
    },
    /// A popped-out message window was closed (remove it from the map).
    PopoutClosed((u32, u32)),
    /// Add a contact from a popout window's sender.
    AddContactFrom { name: String, email: String },
    /// A message window's Move To: the folder picker over that window, at
    /// its point (x, y). The window closes once a folder is picked.
    PopoutMoveTo {
        key: (u32, u32),
        message: Box<Message>,
        x: f64,
        y: f64,
    },
    /// A message window's single-key shortcut that needs the app (#392):
    /// the window's message and the conversation it shows.
    PopoutShortcut {
        action: Shortcut,
        message: Box<Message>,
        thread: Vec<Message>,
    },
    /// Show the source of the open message.
    ViewSource,

    // ---- on_attachments (attachments.rs) ----
    /// Download a specific message's attachments (from a popout window).
    LoadAttachmentsFor(Box<Message>),
    /// A message window's drawer: delete this file from the server (#289),
    /// asking over that window first.
    DeletePopoutAttachment {
        key: (u32, u32),
        message: Box<Message>,
        attachment: Box<Attachment>,
    },
    /// Show the full-window attachment lightbox (from the drawer or the
    /// toolbar popover's Preview) over these previewable items.
    ShowLightbox {
        items: Vec<Attachment>,
        start: usize,
    },
    /// A card's attachment chip (#213): open the file, or save it.
    CardAttachment {
        account_id: u32,
        id: u32,
        index: usize,
        save: bool,
    },
    /// The drawer's "Show in Message": scroll the reader to the message this
    /// attachment came with (#213).
    ShowAttachmentInMessage(Attachment),
    /// The drawer's "Delete from Server…" (#289): find the message the file
    /// came with, then ask.
    DeleteDrawerAttachment(Attachment),
    /// Ask before an attachment is taken out of its message on the server.
    AskDeleteAttachment(AttachmentTarget),
    /// The user said yes: have the account's worker do it.
    DeleteAttachment(AttachmentTarget),
    /// Send the request (held back by HYLKI_SHOWCASE_DELETE_HOLD, otherwise
    /// straight after [`AppMsg::DeleteAttachment`]).
    SendDeleteAttachment(AttachmentTarget),
    /// The worker could not delete it: show the file as it was.
    AttachmentNotDeleted {
        account_id: u32,
        message_id: u32,
        name: String,
        size: u64,
    },
    /// The worker did it: the message is `new_uid` now (the same UID on
    /// Microsoft 365, a new one elsewhere, `None` if it was not found again).
    AttachmentDeleted {
        account_id: u32,
        message_id: u32,
        path: String,
        uid: u32,
        new_uid: Option<u32>,
        name: String,
        size: u64,
    },

    // ---- on_showcase (showcase/messages.rs) ----
    /// HYLKI_SHOWCASE_POPOUT: open the open message in a window of its own.
    ShowcasePopout,
    /// Showcase only (HYLKI_SHOWCASE_FOLDER): switch to the first account's
    /// folder of this kind, so a capture can start from Drafts, Sent, etc.
    ShowcaseFolder {
        kind: FolderKind,
        account: Option<u32>,
    },
    /// Showcase only (HYLKI_SHOWCASE_NOTIFY): post the new-mail notification
    /// for the active account's newest Inbox message, focus or not, so the
    /// notification, its buttons and its sound can be tried without
    /// waiting for mail (#337).
    ShowcaseNotify,
    /// Showcase only (HYLKI_SHOWCASE_DELETE_ATTACHMENT): the drawer's
    /// Delete from Server for the file called `name`, asking first unless
    /// `confirmed`.
    ShowcaseDeleteAttachment { name: String, confirmed: bool },
    /// Showcase only: open a drop gap in the Settings toolbar editor.
    ShowcaseToolbarGap { zone: usize, index: usize },
    /// Showcase only: open the inline composer's format chooser, or its
    /// overflow menu.
    ShowcaseComposeMenu { format: bool },
    /// Showcase only: turn the inline composer's preview on.
    ShowcaseComposePreview,
    /// Pick entry `n` of the inline composer's From row (#237 capture).
    ShowcaseComposeFrom(u32),
    /// Forward one message of the open conversation by its id (#240), as
    /// its card's Forward button would.
    ShowcaseForward { account_id: u32, id: u32 },
    /// Showcase only (HYLKI_SHOWCASE_COMPOSE_UNDO): drive the inline
    /// composer's history through a scripted round of edits and undos.
    ShowcaseComposeUndo,
    /// Showcase only: open the window's burger menu, to read its Undo and
    /// Redo entries in the state they are actually seen in.
    ShowcaseBurger,
    /// Showcase only: put the inline composer into this composing format
    /// first, so the same round runs over a source-mode body.
    ShowcaseComposeFormat(config::ComposeFormat),
    /// Showcase only (HYLKI_SHOWCASE_COMPOSE_CLOSE): cancel the inline
    /// composer, to check its web process goes with it.
    ShowcaseComposeClose,
    /// Showcase only (HYLKI_SHOWCASE_COPY): the sent reply's copy is read
    /// back, made up from its stand-in card, as a server's would be.
    ShowcaseCopy,
    /// Showcase only (HYLKI_SHOWCASE_SEND): write `text` in the split
    /// reply's editor and send it, to watch the send morph.
    ShowcaseSend(String),
    /// Showcase only (HYLKI_SHOWCASE_ACCOUNT=new[:<provider>]): open a blank
    /// account editor, on that provider when one is named.
    ShowcaseNewAccount(Option<String>),
    /// Showcase only (HYLKI_SHOWCASE_EDITOR_DIRTY): change the open account
    /// editor, so leaving an edited one can be captured.
    ShowcaseDirtyEditor,
    /// Showcase: open filter rule `i` in the current Accounts panel.
    ShowcaseEditFilter(usize),

    // ---- on_actions (actions.rs) ----
    /// Star or unstar the open message, or every selected one (#313).
    ToggleStar,
    /// Archive the open message (its whole conversation when a row stands
    /// for one), or the selection.
    Archive,
    /// Delete the open message or conversation, or the selection.
    Delete,
    /// Shift+Delete: erase the selection from the server, skipping Trash,
    /// once the user confirms (#471).
    DeleteForever,
    /// Erase messages for good (confirmed "Delete permanently" in Trash).
    PurgeMessages(Vec<Message>),
    /// A conversation card's own action pill. Reply/Reply all/Forward open the
    /// reader's inline composer (like the toolbar); the rest act like RowAction.
    CardAction {
        action: RowAction,
        message: Box<Message>,
    },
    /// The reader toolbar's Mark as Read/Unread toggle for the open message.
    ToggleReadCurrent,
    /// A card's "Add sender to Contacts" button.
    CardContact(Box<Message>),
    /// A row's palette or context-menu action. `conversation` holds the whole
    /// thread when the row stands for a collapsed conversation rather than for
    /// `message` alone — a reply started there answers the conversation's
    /// newest message, not the head it is filed under (#210). Empty otherwise.
    RowAction {
        action: RowAction,
        message: Box<Message>,
        conversation: Vec<Message>,
    },
    /// A bulk action applied to every selected message.
    Bulk {
        action: BulkAction,
        messages: Vec<Message>,
    },
    /// A worker finished one bulk MoveMessages request; stops the busy
    /// indicator once all outstanding bulk moves are done.
    BulkComplete,
    /// Mark the open message, or the selection, as spam; in Junk the same
    /// action says it is not spam (#168).
    MarkSpam,
    /// Delete requested on a whole conversation (a lone selected thread-head
    /// row): confirm (per preference), then delete every member.
    DeleteThread(Vec<Message>),
    /// The thread-delete dialog was confirmed.
    DeleteThreadConfirmed(Vec<Message>),
    /// Delete every message in an account from this address, after leaving
    /// its list (#407).
    DeleteFromAddress { account_id: u32, addr: String },

    // ---- on_snooze (snooze.rs) ----
    /// Snooze what the reader's actions act on until this time (#527).
    Snooze(i64),
    /// Bring it back now, out of the Snoozed folder.
    Unsnooze,
    /// The calendar, for a time of one's own.
    PickSnoozeTime,
    /// A Snooze menu's choice for these messages: a row's, a selection's,
    /// a pop-out's.
    SnoozeMessages {
        messages: Vec<Message>,
        choice: crate::ui::later::SnoozeChoice,
    },
    /// The Snooze menu, under the toolbar's button (the keyboard's way in).
    SnoozeMenu,
    /// The clock: bring back the mail whose time has come.
    WakeSnoozed,
    /// A worker's answer to `MailRequest::Unsnooze`.
    Unsnoozed {
        account_id: u32,
        message_ids: Vec<String>,
        done: bool,
    },

    // ---- on_workers (workers.rs) ----
    /// Check every account for new mail now.
    Refresh,
    /// The system resumed from sleep — worker IMAP sockets are stale, so
    /// reconnect every account and reload the visible folder.
    SystemResumed,
    /// The network came back after being down: the same reconnect as a wake.
    NetworkBack,
    /// The network went down: the Refresh button turns orange (#442).
    NetworkLost,
    /// The connection started or stopped being metered (#442).
    MeteredChanged,
    /// The dock may have dropped the launcher badge; send it again.
    LauncherCountLost,
    /// The main window became the active one.
    WindowInFront,

    // ---- on_compose_open (compose_open.rs) ----
    /// Write a new message.
    Compose,
    /// Reply to the message the reader shows.
    Reply,
    /// Reply to everyone on the message the reader shows.
    ReplyAll,
    /// Forward the message the reader shows.
    Forward,
    /// A reply, reply to all, forward or new message in a window of its
    /// own, whatever Settings says (Gmail's Shift+R, Shift+A, Shift+F, d).
    ComposeInWindow(WindowCompose),
    /// Copy the message the reader is on into a new one (#232).
    EditAsNewCurrent,
    /// Write a new message to this address (an address's menu, the
    /// Contacts page).
    ComposeTo(String),
    /// A new message to these recipients in Bcc: a contact group that hides
    /// its members' addresses.
    ComposeBcc(String),
    /// Open a composer from a `mailto:` URI.
    OpenMailto(String),
    /// The click on a "message ready" desktop alert: raise the window and
    /// the composer it announced.
    PresentComposers,

    // ---- on_contacts (contacts.rs) ----
    /// Add this address to Contacts, the name left for the user to fill in
    /// (an address's right-click menu).
    AddContactAddr(String),
    /// The address books an "Add to Contacts" can go to, gathered off the
    /// UI thread: the dialog for `name` and `email` opens with them.
    AddContactBooks {
        name: String,
        email: String,
        books: Vec<crate::contacts::Book>,
    },
    /// Adding a contact finished: created, merged into a contact, or why
    /// not.
    ContactAdded(Result<crate::contacts::AddOutcome, String>),
    /// The GNOME Contacts photo index changed (EDS sync, or the first load
    /// finished) — refresh the avatars that are on screen.
    ContactPhotosChanged,
    /// Contacts, asked for from a composer, which may be a window of its
    /// own: the main window comes forward to show it.
    ShowContactsPage,
    /// Show the Contacts page in the main window.
    OpenContacts,
    /// The Contacts page's books and groups, for the sidebars.
    ContactTree(crate::ui::contacts_page::ContactTree),
    /// Picked in the sidebar: what the contact list shows.
    ContactScope(crate::ui::contacts_page::ContactScope),
    /// The address books that sync with a server fetched what changed
    /// (#408): read the contacts again if they are showing.
    ContactBooksSynced,
    /// Read with the contacts: the books new contacts can go to, chosen one
    /// first, and how many contacts the Hylki book holds.
    ContactBooksLoaded {
        books: Vec<crate::contacts::Book>,
        local_count: usize,
    },
    /// Read the composer's recipient suggestions for the Contacts view's
    /// group form, off the main thread.
    ContactSuggestions,
    /// The background EDS read for the contacts view finished.
    ContactsLoaded(Vec<crate::contacts::ContactDetails>),
    /// Right-click on the sidebar's Contacts row: the external app.
    LaunchGnomeContacts,
    /// Contact editor writes (run on a background thread against EDS).
    SaveContact { book_uid: String, vcard: String },
    /// A new contact, into the book picked in the editor (`None`: the
    /// default one).
    CreateContact {
        book_uid: Option<String>,
        vcard: String,
    },
    /// A group, new (`original` is `None`) or changed.
    SaveGroup {
        book_uid: String,
        format: crate::contacts::GroupFormat,
        original: Option<String>,
        edit: crate::contacts::GroupEdit,
    },
    /// Contacts to delete, by book and UID.
    DeleteContacts(Vec<(String, String)>),
    /// A group to make in the book `to` and delete from `from`.
    MoveGroup {
        from: String,
        uid: String,
        to: String,
        format: crate::contacts::GroupFormat,
        edit: crate::contacts::GroupEdit,
    },
    /// Contacts dropped on a book or group in the sidebar.
    ContactDrop {
        keys: Vec<(String, String)>,
        on: crate::ui::contacts_page::DropOn,
        moving: bool,
    },
    /// A right-click choice on a book or group in the sidebar.
    ContactTreeAction(crate::ui::contacts_page::TreeAction),
    /// The accounts and books folded under the sidebar's Contacts row.
    ContactsFolded(Vec<String>),
    /// Name an address book in Hylki (`None`: its own name again).
    RenameBook { uid: String, name: Option<String> },
    /// Copy these cards into the book `to`.
    CopyContacts { to: String, cards: Vec<String> },
    /// Contacts to move to the book `to`: (book, UID, the card for `to`).
    MoveContacts {
        to: String,
        cards: Vec<(String, String, String)>,
    },
    /// A contact write finished (`Some` = the error to show).
    ContactWriteDone(Option<String>),
    /// `.vcf` files chosen in the Contacts page, to import into the Hylki book.
    ImportContacts(Vec<std::path::PathBuf>),
    /// The import finished.
    ContactsImported(Result<crate::contacts::local::ImportOutcome, String>),

    // ---- on_about (about.rs) ----
    /// Open the About window.
    OpenAbout,

    // ---- on_filters (filters.rs) ----
    /// Load remote content from this sender from now on (the reader's
    /// banner).
    AllowSender(String),
    /// An address added to the allowed senders in Settings; the same as
    /// `AllowSender`.
    AddSender(String),
    /// An address taken off the allowed senders.
    RemoveSender(String),
    /// Block a sender or domain, and sweep their mail already in view.
    AddBlacklist(String),
    /// Take a sender or domain off the block list.
    RemoveBlacklist(String),
    /// The filter rules changed in Settings (#47).
    SetFilters(Vec<config::FilterRule>),

    // ---- on_unsubscribe (unsubscribe.rs) ----
    /// A card's Unsubscribe button: confirm, then leave the list the message
    /// came from by the handles its headers offered.
    Unsubscribe {
        message: Box<Message>,
        info: Box<crate::models::Unsubscribe>,
    },
    /// Confirmed: send the request.
    UnsubscribeGo {
        message: Box<Message>,
        info: Box<crate::models::Unsubscribe>,
    },
    /// The one-click route failed (or was not offered): write to the list's
    /// `mailto:` handle instead, from the account the message arrived in.
    UnsubscribeByMail {
        message: Box<Message>,
        info: Box<crate::models::Unsubscribe>,
    },
    /// The request is over: `Ok(true)` left the list, `Ok(false)` only opened
    /// the list's web page, `Err` says what went wrong.
    UnsubscribeDone {
        message: Box<Message>,
        result: Result<bool, String>,
    },

    // ---- on_invites (invites.rs) ----
    /// A card's invitation button (#223): answer the organiser, or hand the
    /// meeting to whatever application opens calendar files.
    InviteAction {
        message: Box<Message>,
        invite: Box<crate::models::Invite>,
        action: crate::ui::message_view::InviteAction,
    },

    // ---- on_list_header (list_header.rs) ----
    /// The ⋯ of the message list header (Focus Mode): search, filters and
    /// sort as a menu.
    ListOverflowMenu,
    /// The list pane is this wide now (a drag of the divider, #334).
    ListPaneWidth(i32),
    /// The message list's visible-count text changed.
    ListCount(String),
    /// A column heading was dragged to a new width (#334): keep it.
    ListColumnWidths(std::collections::HashMap<config::ListColumn, i32>),
    /// A column heading sorted the list (#334): the sort menus follow.
    ListSortChanged(&'static str),

    // ---- on_identity (identity.rs) ----
    /// A Gravatar one of the accounts asked for came back (#189).
    OwnGravatarFetched {
        email: String,
        outcome: crate::services::avatar::FetchOutcome,
    },

    // ---- on_reader_toolbar (reader_toolbar.rs) ----
    /// The reader pane crossed the actions breakpoint (true = collapse the
    /// header's buttons into the overflow menu).
    SetReaderActionsCollapsed(bool),
    /// The reader pane crossed the second breakpoint (true = the header's
    /// buttons draw narrower, so the left group still fits).
    SetReaderCompact(bool),
    /// Measure what the folded row needs, now that it is drawn.
    MeasureReaderFold,
    /// The window controls were re-measured (decoration layout changed).
    ReaderControlsChanged(i32),
    /// The collapsed header's ⋯ button was clicked — pop its menu.
    ReaderOverflowMenu,
    /// A right-click on the reader header's empty space: the menu that
    /// leads to the toolbar editor.
    ReaderToolbarMenu { x: f64, y: f64 },
    /// Open Settings on the Appearance page (its Toolbar section).
    CustomizeToolbar,
    /// Settings, at Message List → Columns (#443).
    CustomizeColumns,

    // ---- on_tray (tray.rs) ----
    /// The welcome wizard's first page picked a language: save it and
    /// come back in it.
    WizardLanguage(String),
    /// "Restart Now" from the app-icon heads-up: quit into the restart
    /// helper once the desktop has taken the new launcher in.
    RestartApp,
    /// The settle time is up: hand off to the restart helper and quit.
    RestartNow,
    /// Quit was chosen from the tray icon's menu.
    QuitFromTray,
    /// "View all unread" was chosen from the tray icon's menu: go to All
    /// Inboxes, or without it to the first inbox holding unread mail.
    TrayViewUnread,
    /// Raise and focus the main window (welcome wizard hand-off).
    PresentWindow,

    // ---- on_shortcuts (shortcuts.rs) ----
    /// Show the keyboard-shortcut reference.
    ShowShortcuts,
    /// Ctrl+K: search what the app can do and where it can go (#399);
    /// Ctrl+Shift+K opens it on the actions only (`true`).
    CommandPalette(bool),
    /// Render it to a PDF and open that, to see what will come out.
    PrintPreview,
    /// Print the message in the reader (issue #16).
    PrintMessage,
    /// A single-key shortcut fired.
    Shortcut(Shortcut),
    /// The zoom keys in the main window: the inline composer's while it has
    /// the focus and its format zooms, else the message's.
    ZoomKey(i8),
    /// Ctrl+Shift+A or Ctrl+Shift+C in the main window: the inline
    /// composer's while it has the focus, else the accounts switch or the
    /// console.
    ComposerKey(gtk::gdk::Key),
    /// Message zoom (Ctrl+ / Ctrl- / Ctrl+0): a step up, a step down, or
    /// back to the default, for this session.
    ZoomMessage(i8),
    /// Ctrl+C while the reader's view does not hold the keyboard.
    CopyReaderSelection,

    // ---- on_prefs_reader (prefs_reader.rs) ----
    /// Settings → Reading: the zoom every launch starts at.
    SetZoomDefault(u32),
    /// Reader View on or off (the header's switch).
    SetReaderMode(bool),
    /// Show or hide the Reader View switch in the reader header.
    SetReaderSwitchShown(bool),
    /// Settings: show the Reader View switch in the reader header.
    /// Settings: show the Light / Dark Mode switch in the reader header.
    SetThemeSwitchShown(bool),
    /// Show or block remote content for one message, whatever the standing
    /// policy is — the reader menu's entry, and what the banner's Load does.
    SetRemoteContent {
        account_id: u32,
        id: u32,
        show: bool,
    },
    /// Fetch a message's body again: its OpenPGP verdict changed (#133).
    ReloadBody(Box<crate::models::Message>),

    // ---- on_undo (undo.rs) ----
    /// Ctrl+Z: undo the most recent action.
    Undo,
    /// Ctrl+Shift+Z / Ctrl+Y: put back what undo took away.
    Redo,
    /// An undone move landed and the folder has been reloaded: put the
    /// user on the restored messages (by Message-ID).
    UndoRestored {
        account_id: u32,
        folder_id: u32,
        message_ids: Vec<String>,
    },

    // ---- on_prefs_compose (prefs_compose.rs) ----
    /// The inline composer's own history changed (#200): what its Undo and
    /// Redo would take back now, or `None` for a direction with nothing in
    /// it. The window's entries follow it while it holds focus.
    ComposeHistory {
        id: u32,
        undo: Option<String>,
        redo: Option<String>,
    },

    // ---- on_hand_off (hand_off.rs) ----
    /// A hand-off's files have a destination (the dialog answered, or the
    /// preference decided); `remember` writes the choice to Settings.
    HandOffAction {
        hand_off: FileHandOff,
        action: config::FilesAction,
        remember: bool,
    },
    /// Show the draft picker with whatever Drafts lists have arrived.
    HandOffDraftPickNow,
    /// The draft picker answered (None: a new message instead).
    HandOffDraft {
        hand_off: FileHandOff,
        draft: Option<Message>,
    },
    /// The reply picker answered (None: a new message instead).
    HandOffReply {
        hand_off: FileHandOff,
        message: Option<Message>,
    },
    /// The size check answered: open the composer, attaching or uploading.
    HandOffOpen {
        hand_off: FileHandOff,
        target: HandOffTarget,
        cloud: bool,
        remember: bool,
    },
    /// Files handed in from outside the app (a file manager's "Open With
    /// Hylki", or the command line): open a fresh composer with them attached
    /// (Isaac's PR #96).
    OpenWithFiles(Vec<std::path::PathBuf>),
    /// Files let go on one of the main window's cards: a new message with
    /// them attached, in its text, or uploaded to the cloud.
    DropFilesAs(crate::ui::drop_zones::DropChoice, Vec<std::path::PathBuf>),
    /// Files dragged onto the main window from a file manager (#293).
    DropFiles(Vec<std::path::PathBuf>),

    // ---- on_drafts (drafts.rs) ----
    /// New Message from Template (#360): pick a template to start from,
    /// once every Templates folder has been listed.
    NewFromTemplate,
    /// Show the template picker with whatever Templates lists have arrived.
    TemplatePickNow,
    /// The template picker answered (None: a blank message instead).
    TemplatePicked(Option<Message>),
    /// A composer's Save Draft: store the message in its account's Drafts
    /// folder.
    SaveDraftMessage(Box<OutgoingMessage>),
    /// Save a message to the Templates folder (#360): a composer's Save as
    /// Template, or an edited template saved back.
    SaveTemplateMessage(Box<OutgoingMessage>),
    /// A composer saving its message on its own (#340).
    AutoSaveDraft {
        id: u32,
        message: Box<OutgoingMessage>,
    },
    /// A new message saved only automatically was discarded: the saved copy
    /// goes too, and the composer closes.
    DiscardAutosave {
        id: u32,
        origin: crate::models::DraftOrigin,
    },
    /// The composer's Delete Draft: trash the draft it was opened from and
    /// close that composer without saving.
    DeleteDraft {
        id: u32,
        origin: crate::models::DraftOrigin,
    },
    /// A draft is in the Drafts folder under `message_id` (`None`: an
    /// automatic save that did not go through). `autosave` is the composer
    /// that saved it on its own (#340).
    DraftSaved {
        account_id: u32,
        autosave: Option<u32>,
        message_id: Option<String>,
    },

    // ---- on_settings_window (settings_window.rs) ----
    /// Select a settings category by id (the showcase hook).
    ShowSettingsPage(String),
    /// Open the settings window on the user's preferred view (the menu entry).
    OpenSettings,
    /// The Settings window showed a category; remembered for reopening.
    SettingsPageShown(String),
    /// Open Settings.
    OpenPreferences,
    /// Settings was closed (the window only hides).
    ClosePreferences,
    /// Close the Settings window as the user would (the showcase's reopen
    /// timing).
    DebugCloseSettings,
    /// Build the Settings window ahead of its first open (see the handler).
    PrewarmSettings,
    /// The accounts editor subpage opened/closed in the settings window.
    SettingsEditorOpen(Option<&'static str>),
    /// A settings editor was asked whether it can be left: `ask` when it
    /// holds unsaved changes, otherwise it has already closed itself.
    SettingsLeaveEditor { page: String, ask: bool },
    /// A JMAP server's sending identities for an account (#346).
    ServerIdentities {
        account_id: u32,
        identities: Vec<(String, String)>,
    },
    /// Settings opened this account's page: the identities its JMAP server
    /// keeps, from the last connect (#346).
    WantIdentities(String),

    // ---- on_uri_queue (uri_queue.rs) ----
    /// A message file (.eml) handed in from a file manager or the command
    /// line, or picked with Open Message File (#398).
    OpenMessageFile(std::path::PathBuf),
    /// Choose a message file to read (#398).
    PickMessageFile,
    /// A `mid:` link (RFC 2392) from anywhere on the system (#130): open the
    /// message with that Message-ID.
    OpenMid(String),
    /// An account's answer to a server-side Message-ID search.
    MidLocated {
        account_id: u32,
        message_id: String,
        hit: Option<(String, u32)>,
    },

    // ---- on_tags (tags.rs) ----
    /// The tag finder: scan every account for keywords in use.
    FindTags,
    /// Settings opened this account's page: ask its server how much
    /// storage is in use (#298).
    WantQuota(String),
    /// The server's answer, for the account's Settings page.
    QuotaFound {
        account_id: u32,
        quota: Option<crate::models::MailboxQuota>,
    },
    /// One account's answer to the scan.
    KeywordsFound {
        account_id: u32,
        findings: Vec<KeywordFinding>,
    },
    /// The scan's safety net: report what has come in, if the scan `gen` is
    /// still the one running.
    TagScanTimeout(u32),
    /// The tags changed in Settings (#71).
    SetTags(Vec<config::Tag>),
    /// A tag row was chosen: its keyword (`None` for the unified Tags row,
    /// every tag at once), and the account it is scoped to (an account
    /// section's own Tags row) or `None` for every account.
    TagSelected {
        keyword: Option<String>,
        account: Option<u32>,
    },
    /// A keyword re-sync (#166) changed the cached keywords of these folders.
    KeywordsSynced { account_id: u32, paths: Vec<String> },
    /// A tag view's rows, read from the index off the main thread: the
    /// view they answer and, per message, its account and folder path.
    TagViewLoaded {
        key: (Option<u32>, Option<String>),
        rows: Vec<(u32, String, Message)>,
    },

    // ---- on_send (send.rs) ----
    /// A composer sent a message; `from` is that composer's id.
    SendMessage {
        from: u32,
        out: Box<OutgoingMessage>,
    },
    /// A message's Undo Send delay is over, or Send now was asked for it in
    /// the Outbox: it goes to its worker (#434).
    DispatchSend(u64),
    /// Undo on a message still waiting out its delay (#434).
    UndoSend(u64),
    /// The worker is done with a message on its way out (#434).
    SendDone { ticket: u64, sent: bool },
    /// An account's worker sent a message: refresh the folder its copy
    /// lands in when it is the open one.
    Sent { account_id: u32 },
    /// A message nothing could keep, back for a composer (#340).
    Unsent {
        account_id: u32,
        message: Box<crate::worker::OutgoingMessage>,
    },

    // ---- on_unified (unified.rs) ----
    /// Copy the unified view's folders into its slices (`unified_unfilled`).
    FillUnifiedSlices,
    /// Read the folders the unified rows list into memory, off the main
    /// thread, so the first visit after a start finds them there.
    Preload,
    /// Build the unified rows ahead for the message list, one at a time.
    PrebuildViews,
    /// Build the next view waiting in the prebuild queue (`PrebuildViews`).
    PrebuildNext,
    /// Folders not in memory yet, read whole from the index off the main
    /// thread: for the unified view being opened, or ahead of it (`Preload`).
    UnifiedFoldersLoaded {
        lists: Vec<((u32, u32), Vec<Message>)>,
    },
    /// Push the sidebar's unread counts, put off from a view switch.
    PushUnreadCounts,
    /// Merge the unified view for the list now: the list had not kept it.
    EmitUnifiedNow,
    /// Merge the unified view's slices now (see `queue_unified_emit`).
    EmitUnified,

    // ---- on_send_morph (send_morph.rs) ----
    /// Where the conversation's cards stood when a reply was sent has been
    /// measured: the pane can change under the send morph's stage.
    SendMorphMeasured,
    /// The pane could not be captured for the send morph.
    SendMorphFailed,

    // ---- on_compose_host (compose_host.rs) ----
    /// A composer (id) finished — tear down its host (window or inline revealer).
    ComposeClosed(u32),
    /// A composer window's close button: the composer decides, as for
    /// Cancel, whether to ask first (#290).
    ComposeCloseRequested(u32),
    /// Promote/demote the reader's inline composer (id) between inline and window.
    ComposeToggleWindow(u32),

    // ---- on_accounts (accounts.rs) ----
    /// Open Settings at its accounts panel.
    OpenAccounts,
    /// Open the accounts window straight to the "add account" form (empty state).
    AddFirstAccount,
    /// An account was saved in Settings; `original_email` is its address
    /// before the edit, none for a new account.
    AccountSaved {
        original_email: Option<String>,
        account: Box<AccountConfig>,
    },
    /// Show the keyring / Secret Service setup help. `problem: true` when a save
    /// actually failed to persist; `false` for the proactive one-time tip.
    ShowKeyringHelp { problem: bool },
    /// Reconnect every account: a saved edit changed a secret, which the
    /// keyring said after the save.
    ReconnectAccounts,
    /// The one-time notice that an earlier install's data was carried over.
    ShowCarryOverNotice(&'static crate::desktop::legacy::Predecessor),
    /// An account was switched on or off in Settings.
    AccountEnabledChanged { email: String, enabled: bool },
    /// The welcome wizard's privacy/personalize choices: fan out through the
    /// existing Set* handlers so every side effect stays in one place.
    ApplyWelcomePrefs(crate::ui::welcome::WelcomePrefs),
    /// Use a GNOME Online Accounts account in Hylki, or switch it back on.
    ImportGoaAccount(Box<AccountConfig>),
    /// An account was removed in Settings: its secrets and its config go.
    AccountRemoved { email: String },
    /// GNOME Online Accounts changed on the session bus. Carries the fresh live
    /// state, already fetched (debounced) on the watcher thread so the GTK main
    /// thread never does D-Bus I/O — re-reconcile against it.
    GoaChanged(crate::services::goa::GoaLiveState),
    /// A worker reported its account: add it, or update the one listed.
    SetAccount(Account),

    // ---- on_diagnostics (diagnostics.rs) ----
    /// Open the status bar straight into console mode (button / burger menu).
    OpenConsole,
    /// Write the exported log straight to `path`, no chooser: the
    /// HYLKI_SHOWCASE_MEMORY hook, for reading the memory section of a
    /// running instance.
    ExportLogTo(std::path::PathBuf),

    // ---- on_filter_run (filter_run.rs) ----
    /// Run the filter rules over mail that is already in these (account,
    /// folder) pairs (#198), rather than waiting for the next arrival. An
    /// empty list means every account's Inbox.
    ApplyFilters(Vec<(u32, u32)>),
    /// A manual filter run has waited long enough: report what it managed.
    FilterRunTimeout,
    /// The progress dialog of a manual filter run was dismissed: it goes on
    /// in the background and reports in the status bar instead (#198).
    FilterRunToBackground,
    /// A folder's load has been to the server and back (#198).
    FolderSynced { account_id: u32, folder_id: u32 },

    // ---- on_smart (smart.rs) ----
    /// A smart folder (#417) was picked, by id.
    SmartSelected(String),
    /// A smart folder's rows, read from the index off the main thread.
    SmartViewLoaded {
        id: String,
        rows: Vec<(u32, String, Message)>,
    },
    /// Count every smart folder's unread mail now (the debounce ran out).
    CountSmartFolders,
    /// The smart folders' unread counts, by id, for the sidebar.
    SmartCounts(HashMap<String, u32>),
    /// The smart folders as Settings left them.
    SetSmartFolders(Vec<config::SmartFolder>),

    // ---- on_unified_starred (unified_starred.rs) ----
    /// Read the starred mail of accounts with no Starred folder again (the
    /// debounce ran out).
    RefreshUnifiedStarred,
    /// The starred mail of the accounts with no Starred folder, read from
    /// the index for the unified Starred view (#482).
    UnifiedStarredLoaded {
        rows: HashMap<u32, Vec<(String, Message)>>,
    },

    // ---- on_move_tag (move_tag.rs) ----
    /// Put a tag on one message, or take it off (row menu, palette, card).
    SetTag {
        message: Box<Message>,
        keyword: String,
        add: bool,
    },
    /// Put a tag on several messages, or take it off (the list's bulk bar).
    SetTagMany {
        messages: Vec<Message>,
        keyword: String,
        add: bool,
    },
    /// Put the reader's target back in its Inbox (from Trash or Junk, #138).
    MoveToInbox,
    /// The reader toolbar's tag menu: toggle a tag on the reader's target.
    ToggleTagCurrent(String),
    /// A tag's number key, with the list rows selected when it was pressed
    /// (#416).
    TagKey {
        n: u8,
        slots: Vec<crate::ui::message_row::Slot>,
    },
    /// Open the Move To… folder picker (#164) for the reader's target, or
    /// the whole list selection.
    MoveToMenu,
    /// "Move To…" from the message list's right-click menu: open the
    /// picker at window point (`x`, `y`) for `messages` (with
    /// `offer_whole`, a conversation row and its members: the picker
    /// offers all of them, or the row's own message alone).
    ListMoveTo {
        messages: Vec<Message>,
        offer_whole: bool,
        x: f64,
        y: f64,
    },
    /// That picker's answer.
    MoveMessagesTo {
        account_id: u32,
        dest: String,
        messages: Vec<Message>,
    },
    /// The picker's answer: file the target or selection into `dest` —
    /// or, with `whole`, the open conversation entire (#171).
    MoveSelectionTo {
        account_id: u32,
        dest: String,
        whole: bool,
    },

    // ---- on_card_menu (card_menu.rs) ----
    /// Pop the tag menu on the reader toolbar's tag button.
    ReaderTagMenu,
    /// A reader's right-click: the message's menu at (x, y) in the main
    /// window, or in the pop-out opened for `popout`.
    CardMenu {
        message: Box<Message>,
        x: f64,
        y: f64,
        hit: crate::ui::message_view::MenuHit,
        popout: Option<(u32, u32)>,
    },

    // ---- on_prefs_data (prefs_data.rs) ----
    /// Second stage of ImportSettings: the chosen file, applied on a clean
    /// main-loop turn (working inside the chooser's completion callback froze
    /// the app when the confirmation dialog presented there).
    ImportSettingsFrom(std::path::PathBuf),

    // ---- on_events_folders (events_folders.rs) ----
    /// A worker listed an account's folders.
    SetFolders {
        account_id: u32,
        folders: Vec<Folder>,
    },
    /// A folder's unread count, from the server.
    FolderUnread {
        account_id: u32,
        folder_id: u32,
        unread: u32,
    },
    /// The worker stored (or failed to store) a read/unread change the app
    /// had applied ahead of it.
    SeenSettled {
        account_id: u32,
        path: String,
        uid: u32,
    },
    /// A message was replied to or forwarded, and the server told (#454).
    Answered {
        account_id: u32,
        path: String,
        uid: u32,
        forwarded: bool,
    },
    /// The server has stored a tag change on a message, or refused it: the
    /// list's provisional mark for it can go.
    TagSettled {
        account_id: u32,
        path: String,
        uid: u32,
        keyword: String,
    },
    /// The worker has run (or failed) the move that took these messages
    /// out of `path`: its lists can be believed about them again.
    MovesSettled {
        account_id: u32,
        path: String,
        uids: Vec<u32>,
    },
    /// From a per-folder IDLE watcher, which knows its folder only by path —
    /// folder ids are positional and may have shifted since it was spawned.
    FolderUnreadByPath {
        account_id: u32,
        path: String,
        unread: u32,
    },
    /// The message-body filter conditions the server confirmed for a folder's
    /// listed mail (#191): uid → the lowercased alternatives found in it.
    /// Arrives just ahead of the `Messages` it describes.
    BodyHits {
        account_id: u32,
        folder_id: u32,
        hits: std::collections::HashMap<u32, Vec<String>>,
    },

    // ---- on_transfer (transfer.rs) ----
    /// Moving mail to another account (#265): the source's copy of a
    /// message, and the receiving account's answer.
    RawExported {
        token: u64,
        raw: Result<Vec<u8>, String>,
    },
    /// A message copied to another account (the transfer `token`) landed,
    /// or why not.
    RawImported {
        token: u64,
        result: Result<(), String>,
    },

    // ---- on_events_messages (events_messages.rs) ----
    /// A worker sent a folder's messages.
    Messages {
        account_id: u32,
        folder_id: u32,
        messages: Vec<Message>,
    },
    /// Additional indexed summaries from the background backfill (search index).
    MessagesAppend {
        account_id: u32,
        folder_id: u32,
        messages: Vec<Message>,
    },
    /// A folder's background backfill finished — it's fully indexed now.
    BackfillDone { account_id: u32, folder_id: u32 },
    /// A message the cache listed is no longer in `path` on the server.
    MessageGone {
        account_id: u32,
        message_id: u32,
        path: String,
        uid: u32,
    },
    /// A folder's threading references were repaired — re-read it from the cache
    /// so what is on screen groups with what was found.
    RefsRepaired { account_id: u32, folder_id: u32 },

    // ---- on_events_bodies (events_bodies.rs) ----
    /// A message's sender-authentication verdict arrived with its body.
    SenderChecked {
        account_id: u32,
        path: String,
        message_id: u32,
        check: Box<crate::models::SenderCheck>,
    },
    /// `path` is the folder the body was read from — a UID only identifies a
    /// message within its own folder, so applying a body to a message means
    /// checking the folder too.
    Body {
        account_id: u32,
        message_id: u32,
        path: String,
        body: String,
    },
    /// A message's source arrived: show it.
    Source { text: String },
    /// These, like `Body`, name the folder: a UID means a message only
    /// within its folder.
    Attachments {
        account_id: u32,
        path: String,
        message_id: u32,
        items: Vec<Attachment>,
    },
    /// A message has attachments that are not on disk yet: fetch them.
    AttachmentsPending {
        account_id: u32,
        path: String,
        message_id: u32,
    },
    /// A flagged message turned out to have no real attachments — drop its paperclip.
    NoAttachments {
        account_id: u32,
        path: String,
        message_id: u32,
    },
    /// An unflagged message turned out to carry attachments — give it one.
    HasAttachments {
        account_id: u32,
        path: String,
        message_id: u32,
    },

    // ---- Settings: route_pref (router.rs) ----
    /// A setting changed in Preferences (or its equivalent elsewhere).
    Pref(PrefOutput),
}
