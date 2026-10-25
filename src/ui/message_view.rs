//! Right pane: the reading view for a single message.
//!
//! Bodies are rendered in a sandboxed WebKit view: JavaScript is disabled and a
//! Content-Security-Policy blocks remote content. When remote content is
//! withheld, its URLs are also stripped from the HTML so nothing is requested;
//! the originals are only used once the user (or a trusted sender) allows them.
//! Link clicks open in the browser.

mod bridge;
mod card_drag;
mod cards;
mod chrome;
mod component;
mod dark;
mod document;
mod folds;
mod frame;
mod html;
mod init;
mod invite;
mod links;
mod look;
mod menu;
mod on_card_actions;
mod on_cards;
mod on_find;
mod on_prefs;
mod on_services;
mod on_show;
mod page_fade;
mod print;
mod remote;
mod render;
mod sender;
mod size_script;
mod snippets;
mod style;
mod translation;
mod types;
mod unsub_banner;
mod webview;
mod wrapper_css;

use self::chrome::ClassRules;
use self::page_fade::{PageFade, PendingLoad};
use crate::models::Message;

pub(crate) use frame::theme_grounds_for;
pub(crate) use menu::MenuHit;
pub(crate) use menu::{hit_menu_entries, strip_navigation_items};
pub(crate) use page_fade::{on_next_page_fade, PAGE_FADE_MS};
pub(crate) use types::{
    CardAttachment, InviteAction, MessageViewInput, MessageViewOutput, SelectMode, TrState,
    UnsubState,
};
pub(crate) use webview::{new_preview_webview, shared_web_context};

/// Each card's detected language, with the body length it was worked out
/// for.
type DetectedLanguages = std::collections::HashMap<(u32, u32), (usize, Option<String>)>;

pub(crate) struct MessageView {
    /// Render a lone message as an inset card, same as a conversation's
    /// messages (#57, preference; off keeps the full-bleed view).
    single_message_card: bool,
    /// The OpenPGP chip says its verdict in words (#300); off keeps the
    /// icons alone.
    pgp_labels: bool,
    /// What each message on screen has attached (#213): names and sizes,
    /// handed over by the app as the files arrive. Listed beneath the card.
    card_atts: std::collections::HashMap<(u32, u32), Vec<CardAttachment>>,
    /// Preference: cards list their own attachments (#213).
    card_atts_shown: bool,
    /// Preference: the attachment drawer is in use. A lone message's files
    /// are listed on its card only when the drawer is not there to show them.
    drawer_on: bool,
    /// Always show the recipients line under the sender (preference) — the
    /// chip then only appears for multi-recipient mail, as a collapse toggle.
    always_show_recipients: bool,
    /// The newest message in the thread — drives the header, avatar and actions.
    current: Option<Message>,
    /// The whole conversation, newest first. One entry = a single message (shown
    /// exactly as before); more = a scrollable conversation in the body view.
    thread: Vec<Message>,
    /// Which of the conversation's messages came from another folder, and what to
    /// call that folder.
    folder_labels: std::collections::HashMap<(u32, u32), String>,
    /// The tags (#71): what a message's keywords are drawn as.
    tags: Vec<crate::config::Tag>,
    /// The keywords and Gmail labels the GTK header's chips were last built
    /// for (a lone full-bleed message; cards carry their chips in the
    /// document).
    header_tags_rendered: std::cell::RefCell<Option<(Vec<String>, Vec<String>)>>,
    /// Remote content was detected and is currently withheld — this drives the
    /// "blocked" banner, and nothing else.
    blocked: bool,
    /// Messages the user deliberately marked unread while this conversation was
    /// on screen. They keep their mark but get no scroll sentinel, so simply
    /// having them in view can't undo the thing the user just asked for. Cleared
    /// whenever a conversation is opened afresh.
    no_autoread: std::collections::HashSet<(u32, u32)>,
    /// The reader's own fonts and colors laid over every message (#56);
    /// `NONE` shows each message as its sender formatted it.
    reader_style: crate::config::ReaderStyle,
    /// Messages the user asked to see with the sender's own formatting
    /// despite `reader_style` (the card's "sender's formatting" toggle).
    /// Kept for the session, so a thread reopened keeps the choice.
    sender_style: std::collections::HashSet<(u32, u32)>,
    /// Reader View: every message on screen stripped to its content and set
    /// in the reader's own uniform sheet (see `crate::mail::reader`). The header's
    /// toggle; applies to the whole conversation at once.
    reader_mode: bool,
    /// Whether the header's Reader View switch is shown (Settings).
    reader_switch_shown: bool,
    /// Whether the header's Light / Dark Mode switch is shown (Settings).
    theme_switch_shown: bool,
    /// The scheme the user chose for a message with the header's Light /
    /// Dark Mode switch, dark (`true`) or light. The scheme itself is kept,
    /// not a flip of the theme, so a message turned light stays light when
    /// the theme changes after. Kept for the session, so a message reopened
    /// keeps the choice.
    theme_picks: std::collections::HashMap<(u32, u32), bool>,
    /// Message zoom in percent (Ctrl+ / Ctrl-): the bodies alone are
    /// scaled, in Reader View and out of it; the chrome keeps its size.
    zoom: u32,
    /// The zoom Settings starts every launch at. The header's chip only
    /// speaks up away from it: someone who chose 125% has nothing to be
    /// told at 125%.
    zoom_default: u32,
    /// What Reader View does when a conversation is opened afresh
    /// (Settings): keep the last choice, or start on or off.
    reader_default: crate::config::ReaderDefault,
    /// Read-marking policy (#100), stamped on the document for the
    /// viewport observer.
    read_mark: crate::config::ReadMark,
    /// Rows Gmail's x has checked in the list (#392), and whether they are
    /// conversations: the empty pane counts them.
    checked: (usize, bool),
    /// Whether the blocked-content banner is shown at all. It gates only the notice: `blocked`
    /// still governs what is withheld, so hiding it never loads anything.
    show_banner: bool,
    /// Whether a "Check this sender" verdict gets the red banner. A failed
    /// check gets it regardless: that one says the From: line is forged.
    show_spoof_banner: bool,
    /// Whether the user has actually permitted remote content for what is on
    /// screen (settings, "Load once", or "Always allow sender").
    ///
    /// Kept separate from `blocked` on purpose. `blocked` depends on a detector
    /// guessing whether a message references remote resources; this does not.
    /// Stripping and the CSP key off *this*, so a detector miss costs a banner
    /// rather than the protection itself.
    remote_allowed: bool,
    /// Owning account's display name (header chip).
    account_name: Option<String>,
    /// The owning account's color, which the header chip is painted in.
    account_color: Option<String>,
    /// The header chip's per-account colors.
    chip_rules: ClassRules,
    /// Paints the reader's spinner and its inter-document cover in the *message*
    /// theme rather than the app's. Reading a light message in a dark app used
    /// to mean a dark spinner giving way to a white page.
    cover_rules: ClassRules,
    /// The widgets those rules are applied to, by class.
    account_chip: Option<gtk::Label>,
    cover: Option<gtk::Stack>,
    /// Fingerprint of what the WebView is currently showing. Re-selecting a
    /// message that renders to the same document skips the load entirely —
    /// every load blanks the view for an instant, however briefly.
    shown_fingerprint: Option<u64>,
    /// Conversation card actions hide until the card is hovered (expanded via
    /// their ⋯ toggle). A preference, applied by stamping the document (see
    /// `document_html`).
    card_actions_hover: bool,
    /// With the toggle off: show the actions automatically while the card is
    /// hovered (vs always).
    card_actions_auto: bool,
    /// Seconds an opened card palette lingers after the pointer leaves — the
    /// cards' own "Message card actions palette timeout", not the list's.
    palette_collapse_secs: u64,
    /// The card's ⋯ opens the card menu, the same one a right-click shows,
    /// instead of sliding its actions palette out. A preference, applied by
    /// stamping the document (see `document_html`).
    card_palette_menu: bool,
    /// The open conversation has already auto-scrolled to its first unread
    /// message: later renders of the same thread (bodies streaming in, a theme
    /// flip) carry a no-scroll stamp so the reader's place is kept.
    did_autoscroll: bool,
    /// That auto-scroll was spent on a lone message, before the rest of its
    /// conversation (a sent original, an archived part) was found.
    autoscroll_lone: bool,
    /// The wrapper document's last reported scroll anchor: the topmost card at
    /// the viewport top and the offset into it. A re-render replaces the whole
    /// document (scroll resets to 0) and can reflow everything above — an
    /// element anchor survives that where a raw pixel offset lands short.
    saved_anchor: Option<(u32, u32, u32)>,
    /// The list beside the pane shows Drafts: the empty state invites
    /// editing a draft rather than reading a message.
    drafts_view: bool,
    /// Every account's Templates folder, as (account, folder) (#360): a
    /// message from one wears a banner offering to use it or edit it.
    template_folders: std::collections::HashSet<(u32, u32)>,
    /// The saved anchor was set by the app, not by a scroll: the render
    /// lands it below the page's top gutter (where an unscrolled pane has
    /// its first card), not flush at the top the way a place the user
    /// scrolled to is restored.
    anchor_gutter: bool,
    /// The next document goes in without the page crossfade: a sent reply's
    /// card is arriving, and the app's stage covers the pane while it does.
    skip_fade: bool,
    /// What each message's frame measured last time it was shown, so reopening a
    /// conversation lays out right away instead of settling into place.
    frame_heights: std::collections::HashMap<(u32, u32), u32>,
    /// This conversation came ready-made, so there is nothing to cover.
    instant: bool,
    /// The messages the list has selected, outlined in the reader.
    selected_cards: Vec<(u32, u32)>,
    /// Bumped by FocusCard/BlurCard so a blur cancels a focus whose 380ms
    /// post-slide scroll hasn't fired yet (reply cancelled immediately).
    focus_gen: std::rc::Rc<std::cell::Cell<u64>>,
    /// True while the body is being fetched (show a spinner instead).
    loading: bool,
    /// False from when a render starts until the WebView reports it finished
    /// loading — a themed cover hides the WebView's white inter-document gap.
    webview_ready: bool,
    /// In-message find (#103).
    find_open: bool,
    /// The split reply is open above this reader: its header bar is gone and
    /// the subject block meets the composer's grab strip instead, where the
    /// list-aligned 4px reads as cramped.
    under_split: bool,
    find_matches: Option<(u32, u32)>,
    find_entry: Option<gtk::SearchEntry>,
    /// Where the keyboard was when find opened, to hand it back on close:
    /// left to GTK, it fell to the message list, where the next Ctrl+F
    /// searched the list instead of the message (#392).
    find_return: Option<gtk::glib::WeakRef<gtk::Widget>>,
    /// The find bar opens under the message instead of above it (#392), and
    /// the bar and the alert it follows when it is at the top.
    find_bottom: bool,
    find_bar: Option<(gtk::Revealer, gtk::Widget)>,
    /// The primary button's press over the reader: a click that selects a
    /// card, or a drag that carries one to a folder.
    pointer: card_drag::Pointer,
    /// The page shown while a message loads, and whether it is holding the
    /// keyboard for the web view meanwhile (#392). A web view whose document
    /// is being replaced drops the keys it is given, so a shortcut pressed
    /// then never arrived; this page lets them through.
    loading_page: Option<gtk::Box>,
    focus_held: std::cell::Cell<bool>,
    /// When find last closed itself (empty entry blur) — the toolbar click
    /// that caused the blur must not instantly reopen it.
    find_closed_at: Option<std::time::Instant>,
    /// The web view the wrapper document (`conversation_document`) is shown
    /// in.
    webview: webkit6::WebView,
    /// Bumped per render: each load gets a unique base URI so WebKit treats it
    /// as a fresh document and re-fetches resources (reusing `about:blank` does
    /// not). An https base also lets https images load without mixed-content.
    seq: std::cell::Cell<u64>,
    /// The document waiting to replace the one on screen (`load_after_paint`).
    pending_load: std::rc::Rc<std::cell::RefCell<PendingLoad>>,
    /// The outgoing page's still, faded out over the incoming one.
    page_fade: PageFade,
    /// Forced dark flag for message content, or `None` to follow the system UI.
    /// This themes email content only, not the app chrome.
    content_dark: Option<bool>,
    /// Whether the current message's From: address survived its provider's
    /// authentication checks. `None` until the verdict arrives with the body.
    sender_check: Option<crate::models::SenderCheck>,
    /// Per-member verdicts for the conversation on screen, behind each card's
    /// header seal (#88). Keyed (account_id, id); cleared on Show.
    member_checks: std::collections::HashMap<(u32, u32), crate::models::SenderCheck>,
    /// The mailing lists left through the cards' Unsubscribe button, by
    /// [`crate::models::Unsubscribe::key`] with the time of the request, so a
    /// message from one says so instead of offering the button afresh.
    unsubscribed: std::collections::HashMap<String, i64>,
    /// Cards whose unsubscribe request is under way or has just failed.
    unsub_state: std::collections::HashMap<(u32, u32), UnsubState>,
    /// Which messages of a conversation open folded (#326).
    fold_messages: crate::config::FoldMessages,
    /// How many lines of preview a folded card shows under the sender, as
    /// the message list does (#326).
    fold_preview_lines: u32,
    /// A document was loaded while the view was not on screen (#332).
    painted_hidden: bool,
    /// The open scroll went to a document loaded while the view was hidden,
    /// which never painted (#332): the next document loaded on screen is owed
    /// it, unless the reader has a place to keep by then.
    scroll_owed: bool,
    /// The conversation's messages filed in Sent, for the folded bar.
    sent: std::collections::HashSet<(u32, u32)>,
    /// Cards opened or folded by hand: these win over the default for as
    /// long as they stay in the conversation on screen.
    folds: std::collections::HashMap<(u32, u32), bool>,
    /// Translations (#327), per card: under way, shown, set aside for the
    /// original, or failed.
    translations: std::collections::HashMap<(u32, u32), TrState>,
    /// Which language each card's message is in, for the Translate offer,
    /// worked out once per body (by its length) rather than on every paint.
    detected: std::cell::RefCell<DetectedLanguages>,
    /// The addresses each account answers to (its own and its aliases), so
    /// an invitation's attendee list can be searched for the reader (#223).
    identities: std::collections::HashMap<u32, Vec<String>>,
    /// The invitations already answered, by [`crate::models::Invite::key`]:
    /// the PARTSTAT sent and when, so a meeting opened again says so.
    invite_answers: std::collections::HashMap<String, (String, i64)>,
    /// Full URL of the link under the pointer, shown in a corner overlay so a
    /// link's real destination is visible before it is clicked.
    link_preview: gtk::Label,
}

#[cfg(test)]
mod tests;

impl Drop for MessageView {
    /// A pop-out's view goes with its window (the reader's own only ever
    /// drops at exit): see [`crate::desktop::memory_report::release_web_view`].
    fn drop(&mut self) {
        crate::desktop::memory_report::release_web_view(&self.webview);
    }
}
