//! The pop-out reader's messages in and out, and what it is opened with.

use crate::config::ReaderToolbar;
use crate::models::{Attachment, Message};
use crate::ui::attachment_drawer::AttachmentDrawerInput;
use crate::ui::keymap::Shortcut;
use crate::ui::message_list::RowAction;

/// Everything needed to open a popout reader for one message.
#[derive(Debug)]
pub(crate) struct MessageWindowInit {
    pub(crate) message: Message,
    /// The whole conversation (oldest first) when the popped-out message heads
    /// one; empty for a plain single message.
    pub(crate) thread: Vec<Message>,
    pub(crate) account_name: Option<String>,
    pub(crate) account_color: Option<String>,
    pub(crate) allow_remote: bool,
    /// The body is still being fetched — show a spinner.
    pub(crate) loading: bool,
    /// Each member's files, as far as they are in hand, by (account, id).
    pub(crate) attachments: Vec<((u32, u32), Vec<Attachment>)>,
    /// The members whose files are on their way.
    pub(crate) attachments_loading: Vec<(u32, u32)>,
    /// The attachment drawer is on (Settings, #213); off, the files show on
    /// the message's card instead, when that is on.
    pub(crate) drawer_enabled: bool,
    pub(crate) card_attachments: bool,
    /// The reader toolbar's layout, as the main window has it.
    pub(crate) toolbar: ReaderToolbar,
    /// The right group stays in the ⋯ menu at every width.
    pub(crate) toolbar_fold_right: bool,
    /// The message is in Junk (Spam reads Not Spam), or in Junk or Trash
    /// (Move to Inbox is offered).
    pub(crate) in_junk: bool,
    pub(crate) restorable: bool,
    /// Whether Snooze is offered (#527), and Unsnooze (`Some(true)`).
    pub(crate) snooze: Option<bool>,
    /// Message-content theme override (`None` follows the system).
    pub(crate) content_dark: Option<bool>,
    /// The reader's own fonts and colors over the senders' (#56).
    pub(crate) reader_style: crate::config::ReaderStyle,
    /// Reader View (the main window's header toggle), followed here too.
    pub(crate) reader_mode: bool,
    /// Message zoom in percent (Ctrl+ / Ctrl-), followed here too.
    pub(crate) zoom: u32,
    /// The zoom Settings starts at, for the chip.
    pub(crate) zoom_default: u32,
    /// Whether the Reader View switch is shown at all.
    pub(crate) reader_switch: bool,
    /// Whether the Light / Dark Mode switch is shown (PR #386).
    pub(crate) theme_switch: bool,
    /// What Reader View does when a message is opened.
    pub(crate) reader_default: crate::config::ReaderDefault,
    /// The tags (#71), for the cards' chips.
    pub(crate) tags: Vec<crate::config::Tag>,
    /// The OpenPGP chip says its verdict in words (#300).
    pub(crate) pgp_labels: bool,
    /// The find bar opens under the message (#392).
    pub(crate) find_bottom: bool,
    /// The main window's single-key and Gmail switches, shared so a change
    /// in Settings reaches an open window too (#392).
    pub(crate) single_key: std::rc::Rc<std::cell::Cell<bool>>,
    pub(crate) gmail_keys: std::rc::Rc<std::cell::Cell<bool>>,
}

#[derive(Debug)]
pub(crate) enum MessageWindowInput {
    /// Print this message (Ctrl+P), the same as in the main window.
    Print,
    /// Preview it as it will print (Ctrl+Shift+P).
    PrintPreview,
    /// No-op (unreachable output mapping).
    Ignore,
    /// A message body arrived from the server — the popped-out message itself
    /// or, in a conversation window, any member of the thread.
    SetBody {
        account_id: u32,
        id: u32,
        body: String,
    },
    /// The sender-authentication verdict for this message.
    SetSenderCheck(Box<crate::models::SenderCheck>),
    /// The OpenPGP chip's words on or off (#300).
    SetPgpLabels(bool),
    /// The find bar under the message, not above it (#392).
    SetFindBottom(bool),
    /// The Settings switch for the "Check this sender" banner changed.
    SetSpoofBannerShown(bool),
    /// Reflect a star toggle that happened elsewhere (or came back from the app).
    SetStarred(bool),
    /// The message's keywords changed (#71).
    /// Keywords and Gmail labels (#374).
    SetKeywords(Vec<String>, Vec<String>),
    /// The tag definitions changed.
    SetTags(Vec<crate::config::Tag>),
    /// A member's downloaded attachments are now available.
    SetAttachments {
        account_id: u32,
        id: u32,
        items: Vec<Attachment>,
    },
    /// A member's attachments exist but were not on disk: fetch them.
    AttachmentsPending {
        account_id: u32,
        id: u32,
    },
    /// Reflect a read/unread change that happened elsewhere.
    SetUnread(bool),
    /// Settings changed the reader toolbar's layout.
    SetToolbar(ReaderToolbar),
    SetToolbarFold(bool),
    SetAttachmentDrawer(bool),
    SetCardAttachmentsShown(bool),
    /// The window crossed the width the whole toolbar needs.
    SetCollapsed(bool),
    /// Hand this to the attachment drawer (a deletion's progress, #289).
    Drawer(AttachmentDrawerInput),
    /// Show the lightbox over these previewable attachments.
    Lightbox {
        items: Vec<Attachment>,
        start: usize,
    },
    /// A card's attachment row: open or save that member's file at `index`.
    CardAttachment {
        account_id: u32,
        id: u32,
        index: usize,
        save: bool,
    },
    /// The drawer's Delete from Server.
    DeleteAttachment(Attachment),
    /// The drawer's Show in Message.
    ShowAttachment(Attachment),
    /// Update the message-content theme (`None` follows the system).
    SetContentTheme(Option<bool>),
    /// The reader's own fonts and colors changed (#56).
    SetReaderStyle(crate::config::ReaderStyle),
    SetReaderMode(bool),
    SetZoom(u32),
    SetZoomDefault(u32),
    /// A zoom step (or the chip's reset, 0) asked for in this window.
    Zoom(i8),
    SetReaderSwitchShown(bool),
    SetThemeSwitchShown(bool),
    SetReaderDefault(crate::config::ReaderDefault),
    /// This window's own Reader View switch was flipped. It changes this
    /// window alone: the main window and the saved choice are left as they
    /// were.
    ReaderMode(bool),
    /// What one of the user's own mailboxes shows changed (#189).
    FacesChanged,
    // ---- toolbar actions ----
    Reply,
    ReplyAll,
    Forward,
    ToggleStar,
    ToggleRead,
    Delete,
    Archive,
    /// Spam, or Not Spam in Junk.
    Spam,
    MoveToInbox,
    /// The folder picker, under the Move To button (or the ⋯).
    MoveTo,
    /// The folder picker at a window point (a card's Move to…).
    MoveToAt {
        message: Box<Message>,
        x: f64,
        y: f64,
    },
    TagMenu,
    /// The Snooze menu, under its button (or the ⋯), #527.
    SnoozeMenu,
    OverflowMenu,
    Find,
    /// A single-key shortcut pressed in this window (#392).
    Key(Shortcut),
    // ---- from the embedded reader ----
    /// Fetch a message's body again (its OpenPGP verdict changed, #133).
    ReloadBody(Box<Message>),
    /// A toast for the main window.
    Notice(String),
    AllowSender(String),
    /// An action chosen on one card — in a conversation window, possibly a
    /// member other than the popped-out message itself.
    CardAction {
        action: RowAction,
        message: Box<Message>,
    },
    /// A card's "Add sender to Contacts" — for that card's message.
    ContactFor(Box<Message>),
    /// An email address in a card header was clicked — compose to it.
    ComposeTo(String),
    /// "Add to Contacts" from an address's right-click menu.
    AddContactAddr(String),
    /// A right-click in the message: its menu, at this window's point (x, y).
    CardMenu {
        message: Box<Message>,
        x: f64,
        y: f64,
        hit: crate::ui::message_view::MenuHit,
    },
    /// A card's Unsubscribe button — handed to the app, which owns the
    /// request.
    Unsubscribe {
        message: Box<Message>,
        info: Box<crate::models::Unsubscribe>,
    },
    /// The lists left so far, for the cards' banners.
    SetUnsubscribed(std::collections::HashMap<String, i64>),
    /// Where a card's unsubscribe request stands.
    UnsubscribeState {
        account_id: u32,
        id: u32,
        state: Option<crate::ui::message_view::UnsubState>,
    },
    /// A card's invitation button — handed to the app, which owns the
    /// answer (#223).
    InviteAction {
        message: Box<Message>,
        invite: Box<crate::models::Invite>,
        action: crate::ui::message_view::InviteAction,
    },
    /// The addresses each account answers to, for the invitation banners.
    SetIdentities(std::collections::HashMap<u32, Vec<String>>),
    /// The invitations answered so far, for those banners.
    SetInviteAnswers(std::collections::HashMap<String, (String, i64)>),
}

#[derive(Debug)]
pub(crate) enum MessageWindowOutput {
    /// A card's Unsubscribe button: leave the list, by these handles.
    Unsubscribe {
        message: Box<Message>,
        info: Box<crate::models::Unsubscribe>,
    },
    /// A card's invitation button: answer the organiser, or hand the
    /// meeting to a calendar application (#223).
    InviteAction {
        message: Box<Message>,
        invite: Box<crate::models::Invite>,
        action: crate::ui::message_view::InviteAction,
    },
    /// A zoom step in this window: in (1), out (-1), or back to the
    /// default (0).
    Zoom(i8),
    /// A per-message action handled exactly like a list/context-menu action.
    Action {
        action: RowAction,
        message: Box<Message>,
    },
    /// Add this sender to Contacts.
    AddToContacts {
        name: String,
        email: String,
    },
    /// Download this message's attachments from the server.
    LoadAttachments(Box<Message>),
    /// The folder picker for this message, at window point (x, y).
    MoveTo {
        message: Box<Message>,
        x: f64,
        y: f64,
    },
    /// A shortcut that reaches past this message: to the rest of the
    /// conversation, or to the app (#392).
    Shortcut {
        action: Shortcut,
        message: Box<Message>,
        thread: Vec<Message>,
    },
    /// Put a tag on this message, or take it off (#71).
    SetTag {
        message: Box<Message>,
        keyword: String,
        add: bool,
    },
    /// Take this attachment out of the message on the server (#289).
    DeleteAttachment {
        message: Box<Message>,
        attachment: Box<Attachment>,
    },
    /// Persist a remote-content allowlist entry.
    /// Fetch a message's body again (its OpenPGP verdict changed, #133).
    ReloadBody(Box<Message>),
    /// A toast for the main window.
    Notice(String),
    AllowSender(String),
    /// An email address in a card header was clicked — compose to it.
    ComposeTo(String),
    /// A right-click in the message: the app shows its menu over this window.
    CardMenu {
        message: Box<Message>,
        x: f64,
        y: f64,
        hit: crate::ui::message_view::MenuHit,
    },
    /// The window was closed.
    Closed,
}
