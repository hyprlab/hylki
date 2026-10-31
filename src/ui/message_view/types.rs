//! The reader's messages in and out, and the small types they carry.

use super::menu::MenuHit;
use crate::models::Message;

/// One attachment as a card lists it (#213): the name and size only. The
/// bytes stay with the app, which opens or saves the file on request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CardAttachment {
    pub(crate) name: String,
    pub(crate) size: u64,
}

#[derive(Debug)]
pub(crate) enum MessageViewInput {
    /// Whether the blocked-remote-content banner is shown. It doesn't change
    /// what is blocked — only what the reader says about it.
    SetBannerShown(bool),
    SetSpoofBannerShown(bool),
    /// The split reply opened above the reader (true) or went away (false).
    SetUnderSplit(bool),
    /// The "always show recipients" preference changed (re-render follows).
    SetAlwaysShowRecipients(bool),
    /// The "single messages as cards" preference changed (re-render follows).
    SetSingleMessageCard(bool),
    /// Which messages of a conversation open folded (#326).
    SetFoldMessages(crate::config::FoldMessages),
    /// What a folded card shows, from the message list's look (#326).
    SetFoldLook {
        preview_lines: u32,
    },
    /// A card's header folded or opened it (#326).
    Folded {
        account_id: u32,
        id: u32,
        folded: bool,
    },
    /// Expand All (false) or Collapse All (true) from the reading pane's
    /// menu (#326).
    FoldAll(bool),
    /// How many rows Gmail's x has checked (#392), and whether threading
    /// makes them conversations.
    SetChecked {
        count: usize,
        conversations: bool,
    },
    /// The view came on screen (#332).
    Mapped,
    /// The OpenPGP chip's words on or off (#300), applied to the open
    /// document at once.
    SetPgpLabels(bool),
    /// What each message on screen has attached (#213), patched into the
    /// cards live when the document is up.
    SetCardAttachments(std::collections::HashMap<(u32, u32), Vec<CardAttachment>>),
    /// Preference: cards list their own attachments (#213).
    SetCardAttachmentsShown(bool),
    /// Preference: the attachment drawer is in use (#213).
    SetAttachmentDrawer(bool),
    /// Scroll a card's attachment row into view and flash it (#213).
    ScrollToAttachments {
        account_id: u32,
        id: u32,
    },
    /// A card's attachment chip was clicked (open) or its save button (#213).
    AttachmentAction {
        account_id: u32,
        id: u32,
        index: usize,
        save: bool,
    },
    Show {
        /// The conversation, newest first. A single message for a normal open;
        /// several for a threaded conversation.
        thread: Vec<Message>,
        /// The sender is trusted, so remote content may auto-load.
        allow_remote: bool,
        /// Owning account's display name and color, for the header chip.
        account_name: Option<String>,
        account_color: Option<String>,
        /// The body is still being fetched — show a spinner.
        loading: bool,
        /// The message the user actually selected, which drives the header,
        /// avatar and sender check. Without it the newest message would, and a
        /// conversation can now open with a reply of your own on top (#21).
        /// Boxed: a `Message` inline here would double the size of every message
        /// this component sends.
        primary: Option<Box<Message>>,
        /// The app already had this conversation assembled, so the reader has
        /// nothing to wait for: it swaps the document in place rather than
        /// covering the view, which is what put a spinner on every return to a
        /// thread it had shown minutes before.
        instant: bool,
        /// Labels for conversation messages that live in another folder, keyed by
        /// (account, message id) — "Sent" beside a reply of yours read from the
        /// Inbox. Messages from the folder on screen aren't in here.
        folder_labels: std::collections::HashMap<(u32, u32), String>,
        /// The messages filed in a Sent folder, whose cards wear a Sent chip.
        sent: std::collections::HashSet<(u32, u32)>,
    },
    LoadRemoteOnce,
    AllowSenderAlways,
    /// A card's remote-content button: show what is blocked, or put the block
    /// back. The banner does the first half and can be switched off entirely.
    ToggleRemote {
        account_id: u32,
        id: u32,
    },
    /// The system/app light-dark preference changed; re-render to match.
    ThemeChanged,
    /// What one of the user's own mailboxes shows has changed — a Gravatar
    /// arrived, or the Settings switch moved (#189). Draw the cards again.
    FacesChanged,
    /// Print the message on screen (issue #16).
    Print,
    /// Render the message to a PDF and open it, so the layout can be checked
    /// before any paper is used.
    PrintPreview,
    /// Print `thread` (`message` naming the job) rather than what is on
    /// screen: the reading pane is off, and nothing is (#475).
    PrintMessages {
        message: Box<Message>,
        thread: Vec<Message>,
        allow_remote: bool,
        preview: bool,
    },
    /// Set the message-content theme: `None` follows the system, `Some(dark)`
    /// forces light/dark for email content only (not the app UI).
    SetContentTheme(Option<bool>),
    /// The reader's own fonts and colors over the senders' (#56).
    SetReaderStyle(crate::config::ReaderStyle),
    /// The card's "sender's formatting" toggle: show that one message as its
    /// sender formatted it, or back under the reader's style.
    ToggleSenderStyle {
        account_id: u32,
        id: u32,
    },
    /// Reader View on or off (the header's toggle): re-renders the whole
    /// conversation stripped to its content, or back as sent.
    SetReaderMode(bool),
    /// The header's Light / Dark Mode switch: on shows the message on screen
    /// in the scheme opposite the theme, off as the theme has it, for that
    /// message alone.
    SetMessageFlipped(bool),
    /// Message zoom in percent; the bodies on screen rescale in place.
    SetZoom(u32),
    /// The zoom Settings starts at, which the chip measures against.
    SetZoomDefault(u32),
    /// Settings: show the Reader View switch in the header at all.
    SetReaderSwitchShown(bool),
    /// Whether the header's Light / Dark Mode switch is shown (Settings).
    SetThemeSwitchShown(bool),
    /// Whether the header's account chip is shown (Settings).
    SetAccountChipShown(bool),
    /// Settings: what Reader View does when a conversation is opened. A
    /// per-message default applies to what is on screen right away.
    SetReaderDefault(crate::config::ReaderDefault),
    /// The popover's "Fetch the sender's key" (#133): the Autocrypt key in
    /// the message first, then WKD and the keyservers.
    PgpFetchKey {
        account_id: u32,
        id: u32,
    },
    /// The popover's "Trust this key": vouch for the signing key.
    PgpTrustKey {
        account_id: u32,
        id: u32,
    },
    /// The trust dialog was answered with Trust.
    PgpTrustConfirmed {
        account_id: u32,
        id: u32,
        fingerprint: String,
    },
    /// A key action finished: what to say, and whether to re-verify.
    PgpDone {
        account_id: u32,
        id: u32,
        result: Result<String, String>,
    },
    /// The WebView finished loading the current document — reveal it.
    Rendered,
    /// The sender-authentication verdict for the message now on screen.
    SetSenderCheck(Box<crate::models::SenderCheck>),
    /// A member's sender verdict, for its header seal (#88). Patched into the
    /// live document; queued until the document reports ready.
    SenderCheckFor {
        account_id: u32,
        id: u32,
        check: Box<crate::models::SenderCheck>,
    },
    /// The lists left so far (key → unix seconds), from the app; the cards'
    /// banners are patched to match.
    SetUnsubscribed(std::collections::HashMap<String, i64>),
    /// Sent mail's read receipts (#520), from the app; the cards'
    /// checkmarks are patched to match.
    SetReceiptMarks(crate::models::ReceiptMarks),
    /// A card's Unsubscribe button was clicked.
    Unsubscribe {
        account_id: u32,
        id: u32,
    },
    /// A card's read receipt banner (#520): Send Receipt, or Ignore.
    ReceiptAnswer {
        account_id: u32,
        id: u32,
        send: bool,
    },
    /// A card's Translate button or its translation banner (#327): start a
    /// translation, or switch between it and the original.
    Translate {
        account_id: u32,
        id: u32,
    },
    /// A translation came back, or failed.
    Translated {
        account_id: u32,
        id: u32,
        result: Result<crate::services::translate::Translated, String>,
    },
    /// The app reports where a card's unsubscribe request stands: under way,
    /// failed, or over (`None` — the banner goes back to its resting state,
    /// which says "unsubscribed" once `SetUnsubscribed` carries the list).
    UnsubscribeState {
        account_id: u32,
        id: u32,
        state: Option<UnsubState>,
    },
    /// The addresses each account answers to, so an invitation's attendee
    /// list can be searched for the reader (#223).
    SetIdentities(std::collections::HashMap<u32, Vec<String>>),
    /// The invitations answered so far (event key → PARTSTAT and when),
    /// from the app; the cards' banners are patched to match (#223).
    SetInviteAnswers(std::collections::HashMap<String, (String, i64)>),
    /// One of a card's invitation buttons was pressed (#223).
    InviteAction {
        account_id: u32,
        id: u32,
        action: InviteAction,
    },
    /// The header seal was clicked — show the verdict details anchored on the
    /// seal's own rect (x, y, w, h in document coordinates).
    SenderInfoAt {
        account_id: u32,
        id: u32,
        rect: (f64, f64, f64, f64),
        page_width: f64,
    },
    /// A conversation message header was double-clicked — open that message in
    /// its own window.
    OpenHeader {
        account_id: u32,
        id: u32,
    },
    /// The user marked this conversation message unread: keep the mark until the
    /// conversation is opened again, rather than clearing it the moment the
    /// message happens to be in view.
    SuppressAutoRead {
        account_id: u32,
        id: u32,
    },
    /// The page around the cards was clicked — nothing is selected any more.
    ClearCards,
    /// A card was clicked, with whatever modifier was held.
    CardClicked {
        account_id: u32,
        id: u32,
        mode: SelectMode,
    },
    /// Ctrl+A in the conversation — select every card.
    SelectAllCards,
    /// Step to the next or previous card of the conversation, opening a
    /// folded one, and make it the one actions apply to (#392).
    StepCard(i32),
    /// Keyboard focus into the message, so the arrow keys scroll it (#392).
    FocusBody,
    /// Ctrl+C reached the window rather than this view: copy the selection
    /// made in a message body, if there is one.
    CopySelection,
    /// An email address in a card header was clicked — compose to it.
    ComposeTo(String),
    /// "Add to Contacts" from an address's right-click menu.
    AddContact(String),
    /// Which messages the list has selected. Drawn as an accent outline on the
    /// matching cards, applied to the live document rather than by rendering it
    /// again — a re-render would lose the reader's scroll position.
    SetSelectedCards(Vec<(u32, u32)>),
    /// A message's frame measured this tall, so reopening it can lay out at that
    /// height instead of settling into it.
    FrameSized {
        account_id: u32,
        id: u32,
        height: u32,
    },
    /// One conversation message has been scrolled all the way through, so it
    /// has been read.
    MarkSeen {
        account_id: u32,
        id: u32,
    },
    /// Reply / Reply all / Forward chosen on one card's header, so the action
    /// applies to that message rather than to whichever the reader calls primary.
    CardAction {
        action: crate::ui::message_list::RowAction,
        account_id: u32,
        id: u32,
    },
    /// The card's "Add sender to Contacts" button.
    CardContact {
        account_id: u32,
        id: u32,
    },
    /// A right-click at webview point (x, y), on the card `key` names (the
    /// nearest one when it landed between cards; `None` when the page could
    /// not say): the app shows that message's menu, led by what `hit` offers.
    CardMenuAt {
        key: Option<(u32, u32)>,
        x: f64,
        y: f64,
        hit: MenuHit,
    },
    /// A card's Move to… button, at page point (x, y) in CSS pixels of a
    /// page `page_width` wide: the app opens the folder picker there.
    CardMoveAt {
        account_id: u32,
        id: u32,
        x: f64,
        y: f64,
        page_width: f64,
    },
    /// The card's ⋯, acting as a menu, was clicked at page point (x, y) in
    /// CSS pixels of a page `page_width` wide: the app shows that message's
    /// menu there.
    CardMenuFrom {
        account_id: u32,
        id: u32,
        x: f64,
        y: f64,
        page_width: f64,
    },
    /// Preferences: how card actions show — behind the ⋯ toggle, automatically
    /// on hover, or always.
    SetCardActionsMode {
        hover_toggle: bool,
        hover_auto: bool,
    },
    /// Preferences: seconds an opened card palette lingers after the pointer
    /// leaves. The cards' own timeout; the list keeps a separate one.
    SetPaletteCollapse(u64),
    /// Preferences: the card's ⋯ opens the card menu instead of sliding its
    /// actions palette out.
    SetCardPaletteMenu(bool),
    /// A conversation message was read: clear its card's unread dot in place,
    /// without reloading the document.
    ClearDot {
        account_id: u32,
        id: u32,
    },
    /// A message's star changed outside the card (list row, toolbar): sync
    /// the card's star button without a re-render.
    SetCardStar {
        account_id: u32,
        id: u32,
        starred: bool,
    },
    /// A message's keywords changed (#71): the card's chips are patched in
    /// place, like the star.
    SetCardKeywords {
        account_id: u32,
        id: u32,
        keywords: Vec<String>,
        labels: Vec<String>,
    },
    /// The tag definitions changed: re-render with the new names and colors.
    SetTags(Vec<crate::config::Tag>),
    /// Read-marking policy changed (#100).
    SetReadMark(crate::config::ReadMark),
    /// In-message find (#103): open/close the bar, run/step the search.
    OpenFind,
    CloseFind,
    /// The find bar under the message, not above it (#392).
    SetFindBottom(bool),
    /// A drag began on this card's header: carry it, or the selected cards
    /// it is one of, to a folder.
    DragCard((u32, u32)),
    FindChanged(String),
    FindNext,
    FindPrev,
    FindCounted {
        current: u32,
        total: u32,
    },
    /// A split reply opened for this message (#86): scroll its card to the
    /// top of the (now shorter) reader and give it the selection outline, so
    /// the message being answered is the one in view.
    FocusCard {
        account_id: u32,
        id: u32,
    },
    /// Drop the reply-target outline (the composer closed without sending).
    BlurCard,
    /// The wrapper document reported its scroll anchor (throttled): the card at
    /// the viewport top and the offset into it — kept so a re-render can put
    /// the reader back where they were.
    ScrollAnchor {
        account_id: u32,
        id: u32,
        offset: u32,
    },
    /// A sent reply's card is joining the conversation: the next document
    /// pins it in view, below the page's top gutter, and goes in without the
    /// page crossfade.
    PinArrival {
        account_id: u32,
        id: u32,
    },
    /// The list shows Drafts (or not): the empty pane's wording follows.
    SetDraftsView(bool),
    /// Which folders are Templates folders, as (account, folder) (#360).
    SetTemplateFolders(std::collections::HashSet<(u32, u32)>),
    /// A template banner's button: start a new message from the template,
    /// or edit it.
    TemplateAction {
        account_id: u32,
        id: u32,
        edit: bool,
    },
    /// Keep the pane where it is through the next render (a reply just
    /// arrived for the conversation on screen): when no scrolled-to place
    /// is recorded yet, the card at the top of the pane is pinned there,
    /// below the gutter, so a card inserted above it does not move what is
    /// on screen. A recorded place is kept as it is.
    HoldPlace {
        account_id: u32,
        id: u32,
    },
}

/// How a click on a conversation card changes the selection, mirroring what the
/// message list does with the same modifiers.
#[derive(Debug, Clone, Copy)]
pub(crate) enum SelectMode {
    /// Plain click: this message alone.
    Plain,
    /// Ctrl: add or remove this one, leaving the rest.
    Toggle,
    /// Shift: everything between the last selection and this one.
    Range,
}

#[derive(Debug)]
pub(crate) enum MessageViewOutput {
    /// Add this sender address to the remote-content allowlist.
    AllowSender(String),
    /// Open a conversation message in its own window (header double-clicked).
    OpenWindow(Box<Message>),
    /// The reader's selection changed. It owns this: a conversation can hold
    /// messages the list has no row for — a reply of yours read in from Sent —
    /// and those must still be selectable. The list mirrors what it can.
    SelectCards(Vec<(u32, u32)>),
    /// A conversation message has been read (scrolled through). The reader has
    /// already dropped its mark; the app makes it stick.
    MarkSeen { account_id: u32, id: u32 },
    /// An action chosen on one message's card in a conversation.
    CardAction {
        action: crate::ui::message_list::RowAction,
        message: Box<Message>,
    },
    /// Remote content shown or blocked for one message, from the banner's
    /// Load or the card's own toggle. Reported so the choice is remembered
    /// and a repaint doesn't undo it.
    SetRemote {
        account_id: u32,
        id: u32,
        show: bool,
    },
    /// A card's "Add sender to Contacts" button — add this message's sender.
    ContactSender(Box<Message>),
    /// A card's attachment chip (#213): open the file at `index` of that
    /// message's attachments, or save it.
    AttachmentAction {
        account_id: u32,
        id: u32,
        index: usize,
        save: bool,
    },
    /// A right-click on a card: the app shows the message's full menu (the
    /// list row's) at window point (x, y), with `hit`'s entries first.
    CardMenu {
        message: Box<Message>,
        x: f64,
        y: f64,
        hit: MenuHit,
    },
    /// A card's Move to… button: the folder picker for that message, at
    /// window point (x, y).
    CardMoveTo {
        message: Box<Message>,
        x: f64,
        y: f64,
    },
    /// An email address in a card header was clicked — open a composer to it.
    ComposeTo(String),
    /// "Add to Contacts" picked on an address's right-click menu.
    AddContactAddr(String),
    /// Fetch this message's body again (its OpenPGP verdict changed, #133).
    ReloadBody(Box<Message>),
    /// Something to tell the user in a toast.
    Notice(String),
    /// The subject block's Reader View toggle was flipped; the app saves the
    /// preference and pushes it back (`SetReaderMode`) to every reader.
    ReaderMode(bool),
    /// Message zoom one step in (1) or out (-1), from Ctrl + the scroll
    /// wheel over the body, or back to the default from Settings (0), from
    /// a click on the zoom chip.
    Zoom(i8),
    /// A card's Unsubscribe button: leave the list this message came from,
    /// by the handles its headers offered.
    Unsubscribe {
        message: Box<Message>,
        info: Box<crate::models::Unsubscribe>,
    },
    /// A card's read receipt banner (#520): send the receipt, or ignore
    /// the request.
    ReceiptAnswer { message: Box<Message>, send: bool },
    /// A card's invitation button (#223): answer the organiser, or hand the
    /// meeting to a calendar application.
    InviteAction {
        message: Box<Message>,
        invite: Box<crate::models::Invite>,
        action: InviteAction,
    },
}

/// Where a card's unsubscribe request stands while it is not at rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UnsubState {
    /// The request is on its way.
    Working,
    /// It failed, and this is why (plain words for the banner).
    Failed(String),
}

/// Where a card's translation stands (#327).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TrState {
    /// The service has been asked.
    Working,
    /// The card shows the translation.
    Shown(crate::services::translate::Translated),
    /// Translated, but the card is back on the original.
    Original(crate::services::translate::Translated),
    /// It failed, and this is why.
    Failed(String),
}

/// What a card's invitation banner can be asked to do (#223).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InviteAction {
    /// One of the three answers.
    Answer(crate::models::Rsvp),
    /// Hand the event to whatever application opens calendar files.
    AddToCalendar,
}
