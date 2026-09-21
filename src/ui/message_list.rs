//! Middle pane: the scrollable list of messages in the selected folder,
//! with a search field and live filtering.

use adw::prelude::*;
use gtk::glib;
use relm4::factory::FactoryVecDeque;
use relm4::prelude::*;

use crate::models::Message;
use crate::ui::context_menu::{show_context_menu, show_context_menu_with_header, MenuEntry};
use crate::i18n::i18n;

/// Max rows rendered at once. GtkListBox isn't virtualized, so the full folder
/// index is kept in memory for search but only this many rows are built.
/// Raised from 200 (#236): the unified Inboxes merge every account newest
/// first, and a conversation's older messages fell past the window within
/// days; a wider window keeps more of a conversation on the same page, and
/// the rows past the first paint are built in idle-time chunks anyway.
const RENDER_CAP: usize = 500;

/// Rows built synchronously when the list is (re)built — enough to fill the
/// pane — before the rest of the page arrives in idle-time chunks. Building
/// a row is the expensive part of showing a folder (a ListBox isn't
/// virtualised and each row is a sizeable widget tree), so the first paint
/// waits for these alone.
const FIRST_ROWS: usize = 20;
const FILL_CHUNK: usize = 30;

/// An action chosen from a message's right-click context menu.
/// The action palette's state-carrying buttons, once built.
struct PaletteButtons {
    /// Absent for a draft: a draft is neither read nor unread.
    read: Option<gtk::Button>,
    star: gtk::Button,
    tag: gtk::Button,
}

impl MessageRow {
    /// Build the action palette's buttons into `inner`, on the palette's
    /// first open. Mirrors the reader toolbar's order (Reply, Reply All,
    /// Forward, Read, Flag; Archive, Delete, Spam), with Add to Contacts and
    /// View Source closing the line.
    fn build_palette(&self, inner: &gtk::Box, sender: &FactorySender<Self>) {
        let button = |icon: &str, tip: String| {
            let b = gtk::Button::from_icon_name(icon);
            b.set_tooltip_text(Some(tip.as_str()));
            b.add_css_class("flat");
            b
        };
        let action = |b: &gtk::Button, a: RowAction| {
            let s = sender.clone();
            b.connect_clicked(move |_| s.input(MessageRowInput::Action(a)));
        };
        let reply = button("co.hyprlab.Hylki-mail-reply-sender-symbolic", i18n("Reply"));
        action(&reply, RowAction::Reply);
        let reply_all = button("co.hyprlab.Hylki-mail-reply-all-symbolic", i18n("Reply All"));
        action(&reply_all, RowAction::ReplyAll);
        let forward = button("co.hyprlab.Hylki-mail-forward-symbolic", i18n("Forward"));
        action(&forward, RowAction::Forward);
        // A draft is neither read nor unread, so it gets no toggle.
        let read = (!self.in_drafts).then(|| {
            let b = button("co.hyprlab.Hylki-mail-read-symbolic", i18n("Mark as read"));
            action(&b, RowAction::ToggleRead);
            b
        });
        let star = button("co.hyprlab.Hylki-non-starred-symbolic", i18n("Star"));
        action(&star, RowAction::ToggleStar);
        let tag = button("co.hyprlab.Hylki-tag-outline-symbolic", i18n("Tags"));
        {
            let s = sender.clone();
            tag.connect_clicked(move |b| s.input(MessageRowInput::OpenTagMenu(b.clone())));
        }
        let moveto = button("co.hyprlab.Hylki-folder-symbolic", i18n("Move to…"));
        {
            let s = sender.clone();
            moveto.connect_clicked(move |b| s.input(MessageRowInput::OpenMoveMenu(b.clone())));
        }
        let archive = button("co.hyprlab.Hylki-mail-archive-symbolic", i18n("Archive"));
        action(&archive, RowAction::Archive);
        let delete = button("co.hyprlab.Hylki-user-trash-symbolic", i18n("Delete"));
        action(&delete, RowAction::Delete);
        let spam = if self.in_junk {
            let b = button("co.hyprlab.Hylki-mail-mark-notjunk-symbolic", i18n("Not spam"));
            action(&b, RowAction::NotSpam);
            b
        } else {
            let b = button("co.hyprlab.Hylki-mail-mark-junk-symbolic", i18n("Mark as spam"));
            action(&b, RowAction::Spam);
            b
        };
        let contact = button("co.hyprlab.Hylki-contact-new-symbolic", i18n("Add sender to Contacts"));
        action(&contact, RowAction::AddContact);
        let source = button("co.hyprlab.Hylki-code-symbolic", i18n("View Source"));
        action(&source, RowAction::ViewSource);
        for b in [Some(&reply), Some(&reply_all), Some(&forward), read.as_ref(), Some(&star), Some(&tag), Some(&moveto), Some(&archive), Some(&delete), Some(&spam), Some(&contact), Some(&source)].into_iter().flatten() {
            inner.append(b);
        }
        self.palette_buttons.replace(Some(PaletteButtons { read, star, tag }));
        self.palette_built.set(true);
    }

    /// Keep the built palette's state-carrying buttons in step with the
    /// message (what the view macro's `#[watch]` did while they were part
    /// of the declared view).
    fn sync_palette(&self) {
        let buttons = self.palette_buttons.borrow();
        let Some(b) = buttons.as_ref() else { return };
        // Action-showing icon (read envelope = "mark as read"), like the
        // menus and toolbar.
        if let Some(read) = &b.read {
            if self.msg.unread {
                read.set_icon_name("co.hyprlab.Hylki-mail-read-symbolic");
                read.set_tooltip_text(Some(i18n("Mark as read").as_str()));
            } else {
                read.set_icon_name("co.hyprlab.Hylki-mail-unread-symbolic");
                read.set_tooltip_text(Some(i18n("Mark as unread").as_str()));
            }
        }
        let starred = self.msg.starred || self.thread_starred;
        b.star.set_css_classes(if starred { &["flat", "star-active"] } else { &["flat"] });
        b.star.set_tooltip_text(Some(if starred { i18n("Remove star") } else { i18n("Star") }.as_str()));
        // Only once there is a tag to give.
        b.tag.set_visible(!self.tags.borrow().is_empty());
    }
}

#[derive(Debug, Clone, Copy)]
pub enum RowAction {
    Reply,
    ReplyAll,
    Forward,
    /// Open a copy of the message in the composer as a message of its own
    /// (#232) — same recipients, subject, body and files, nothing tying it
    /// to the original.
    EditAsNew,
    ToggleStar,
    ToggleRead,
    Spam,
    /// The reverse, for a message in Junk (#168): tell the server it is
    /// wanted and put it back in the Inbox.
    NotSpam,
    Archive,
    Delete,
    /// Put a message from Trash or Junk back in its account's Inbox (#138).
    MoveToInbox,
    ViewSource,
    AddContact,
}

/// Init for a row: the message, Gravatar flag, and optional account-ring class
/// (the account colour drawn as a ring around the avatar in the unified view).
pub struct RowInit {
    pub msg: Message,
    pub gravatar: bool,
    /// Whether the avatar is drawn at all (#29).
    pub avatars: bool,
    /// Build the avatar folded away and slide it in a moment later (Focus
    /// Mode has just given the avatars back).
    pub avatar_late: bool,
    /// Whether a sender's site icon may fill it (#30).
    pub sender_logos: bool,
    /// How many lines of the message's text the row shows (1–3).
    pub preview_lines: u32,
    /// Whether the subject line is drawn at all (Focus Mode can take it
    /// away, leaving the sender, the date and the row's marks).
    pub show_subject: bool,
    pub ring_class: Option<String>,
    /// Shared actions palette collapse delay in seconds — how long it stays open
    /// after the cursor leaves it (read live when scheduling).
    pub palette_collapse_secs: std::rc::Rc<std::cell::Cell<u64>>,
    /// Shared "open the palette on row hover" flag (read live on each hover).
    pub palette_hover: std::rc::Rc<std::cell::Cell<bool>>,
    /// The tags (#71), shared with every row: the chips a row shows are the
    /// message's keywords that name one of these.
    pub tags: std::rc::Rc<std::cell::RefCell<Vec<crate::config::Tag>>>,
    /// Whether the row carries the actions palette line at all (preference);
    /// off returns its reserved space to the row.
    pub show_palette: bool,
    /// The list shows Junk: the palette's spam button reads "Not Spam".
    pub in_junk: bool,
    /// The list shows Drafts: no read/unread toggle, a draft is neither.
    pub in_drafts: bool,
    /// Number of messages in this conversation (only set on a thread head; 1 for
    /// a standalone message).
    pub thread_count: usize,
    /// This row is a (newer/older) reply nested under a thread head — indent it.
    pub is_thread_child: bool,
    /// The LAST reply of its thread: the dotted rail stops at its node dot.
    pub is_last_child: bool,
    /// Whether this thread head is currently expanded.
    pub thread_expanded: bool,
    /// Whether conversations can expand in the list at all (preference) —
    /// off hides the chip's caret, since clicking can't open anything.
    pub thread_expandable: bool,
    /// The conversation key, set on a thread head so its chevron can toggle it.
    pub thread_key: Option<(u32, String)>,
    /// The newest member's display time (thread heads only): shown in place of
    /// the head's own — the row says when the conversation last moved.
    pub thread_date: Option<String>,
    /// The newest member's sender name/address and preview (thread heads
    /// only): the row surfaces the latest message, not the thread's opener —
    /// display only, the row's identity stays the head.
    pub thread_from: Option<(String, String)>,
    pub thread_preview: Option<String>,
    /// Any message in this conversation is unread (thread heads only) — keeps
    /// the head marked unread while unread replies are hidden beneath it.
    pub thread_unread: bool,
    /// Any message in this conversation is starred (thread heads only).
    pub thread_starred: bool,
    /// Every currently shown row as (account, folder, uid, id), in list order.
    /// Shared with the list so a drag can turn the ListBox's selected row
    /// *indices* into message ids and carry the whole selection (#23).
    pub drag_keys: DragKeys,
    pub thread_drag: ThreadDragKeys,
    /// Show who the message went to instead of who sent it — a Sent folder's
    /// rows all say "me" otherwise (#27).
    pub show_recipient: bool,
    /// Starting state for the row's own Revealer. A newly-inserted thread
    /// reply starts `false` and is flipped to `true` right after mounting, so
    /// it slides open instead of simply appearing; everything else starts
    /// (and stays) `true`.
    pub revealed: bool,
    /// Swap which side a swipe gesture archives/deletes on (preference,
    /// shared and read live so a change in Settings applies without a
    /// rebuild — false: left deletes, right archives; true: reversed).
    pub swipe_reversed: std::rc::Rc<std::cell::Cell<bool>>,
    /// Whether the swipe gesture is on at all (preference, shared and read
    /// live: the tracker is enabled or not on each render).
    pub swipe_enabled: std::rc::Rc<std::cell::Cell<bool>>,
    /// How sensitive a trackpad's two-finger swipe is (preference, shared and
    /// pushed into the row's swipe surface on each render).
    pub swipe_sensitivity: std::rc::Rc<std::cell::Cell<f64>>,
}

/// A full swipe (#swipe): also `AdwSwipeable`'s reported `distance`, the px
/// one full drag (progress ±1.0) spans.
const SWIPE_MAX: f64 = 120.0;

/// How far a thread member's node dot reaches left of the row's content box
/// (`.thread-node`: 8px wide, pulled 5px out by its negative margin, plus a
/// 2px masking ring), where it sits centred on the group's rail. The last
/// reply's rail stub reaches 2px the same way. The swipe surface's clip
/// leaves this much room on the left, or both come out cut in half.
const THREAD_NODE_REACH: f32 = 8.0;
/// Distance past which the indicator reads as "armed" (full colour) — purely
/// a visual cue; `AdwSwipeTracker` makes the real commit decision on
/// release, factoring in velocity too.
const SWIPE_ARM: f64 = 72.0;

/// The commit exit (#swipe): a released swipe that cleared `SWIPE_ARM` flies
/// the row off the side it was dragged to over this long, while the row's
/// Revealer closes its height over the same span — so the strip fills, the
/// message leaves, and the list shuts over the gap in one movement instead of
/// the row blinking out. Matches the Revealer's own transition duration.
const SWIPE_EXIT_MS: u32 = 200;
/// An action that leaves the row where it is (no Archive folder configured,
/// say) would strand it collapsed and off-screen, so the exit is put back
/// this long after the action if the row is still here.
const SWIPE_RESTORE_MS: u64 = 600;

/// The shown rows' (account, folder, uid, id) keys, in list order — rebuilt with
/// the list and read live when a drag starts.
pub type DragKeys = std::rc::Rc<std::cell::RefCell<Vec<(u32, u32, u32, u32)>>>;

/// Every member of each conversation, keyed by its head's (account, id) —
/// so a drag that starts on a conversation row carries the whole thread,
/// as its Delete does (#171). Published with `DragKeys`.
pub type ThreadDragKeys =
    std::rc::Rc<std::cell::RefCell<std::collections::HashMap<(u32, u32), Vec<(u32, u32, u32, u32)>>>>;

/// The message-list pane's floor: exactly what a conversation-member card
/// needs to show a row's full actions palette — the tightest real constraint
/// in the list. The sum of the card's insets (10px rail margin, 10px + 8px
/// card margins, 12px + 12px card padding), the avatar (38px), the unread
/// dot (8px), three 8px gaps, and the 234px actions-line reservation.
const LIST_MIN_WIDTH: i32 = 348;

/// What an expanded conversation needs beyond [`LIST_MIN_WIDTH`]: the member
/// cards' 10px rail indent plus their card margin/padding beyond a plain
/// pill's. The pane's floor grows by this while any thread is open, so the
/// cards' (and the head pill's) right inset is never clipped off the pane.
const THREAD_EXPANDED_EXTRA: i32 = 12;

/// A background face lookup's answer, correlated by sender address (a recycled
/// row compares before using it). The tiers are personal-first: the contact's
/// own photo, their Gravatar, then the icon their domain publishes (#30), with
/// the UI's coloured initials as the implicit last resort.
#[derive(Debug)]
pub enum FaceCmd {
    /// The avatar tiers (contact photo, Gravatar) answered. `logo` carries the
    /// logo tier's answer when it was consulted in the same trip: found bytes,
    /// or a definitive miss to remember.
    Avatar {
        email: String,
        generation: u64,
        mode: crate::avatar::FetchMode,
        outcome: crate::avatar::FetchOutcome,
        logo: Option<Option<Vec<u8>>>,
    },
    /// A logo-only lookup — the avatar tiers had already answered from cache.
    Logo { email: String, bytes: Option<Vec<u8>> },
}

/// Run the avatar tiers off the main thread, falling through to the domain icon
/// when they come up empty and `want_logo` says the switch is on. `generation`
/// and `mode` come from [`crate::avatar::lookup`] and ride along so the result
/// can be cached against the EDS state that was actually queried.
pub async fn find_face(
    email: String,
    generation: u64,
    mode: crate::avatar::FetchMode,
    want_logo: bool,
) -> FaceCmd {
    let lookup_email = email.clone();
    let result = tokio::task::spawn_blocking(move || {
        let outcome = crate::avatar::fetch(&lookup_email, mode);
        let logo = (want_logo && !matches!(outcome, crate::avatar::FetchOutcome::Found(_)))
            .then(|| crate::logo::fetch(&lookup_email));
        (outcome, logo)
    })
    .await;
    let (outcome, logo) = result.unwrap_or((crate::avatar::FetchOutcome::Retry, None));
    FaceCmd::Avatar { email, generation, mode, outcome, logo }
}

/// Fetch just the domain icon, off the main thread.
pub async fn find_logo(email: String) -> FaceCmd {
    let lookup_email = email.clone();
    let bytes = tokio::task::spawn_blocking(move || crate::logo::fetch(&lookup_email))
        .await
        .ok()
        .flatten();
    FaceCmd::Logo { email, bytes }
}

/// One message summary row.
pub struct MessageRow {
    msg: Message,
    /// Whether the action palette's buttons exist yet (built on first open).
    palette_built: std::cell::Cell<bool>,
    /// The palette buttons whose look follows the message: read/unread,
    /// star, and the tag button's presence.
    palette_buttons: std::cell::RefCell<Option<PaletteButtons>>,
    /// This row's live position, for telling the list which palette opened.
    index: DynamicIndex,
    /// The palette clip's current animation target width (interior-mutable:
    /// update_view only has &self). -0 sentinel not needed — starts closed.
    palette_target: std::cell::Cell<i32>,
    palette_anim: std::cell::RefCell<Option<adw::TimedAnimation>>,
    gravatar: bool,
    avatars: bool,
    /// The avatar's revealer state: down while Focus Mode slides it away
    /// (the row is rebuilt without it once it has gone), or up from folded
    /// when the avatars come back.
    avatar_shown: bool,
    sender_logos: bool,
    preview_lines: u32,
    show_subject: bool,
    avatar_texture: Option<gtk::gdk::Texture>,
    /// The initials circle drawn when no picture is known, kept per name
    /// so the view hands the avatar the same object on every refresh.
    initials_image: std::cell::RefCell<Option<(String, crate::ui::initials::InitialsPaintable)>>,
    ring_class: Option<String>,
    /// Whether the pointer is over this row (drives the chevron fade).
    row_hovered: bool,
    /// Whether the actions palette is slid open on this row.
    palette_open: bool,
    /// Pending auto-collapse timer (armed when the cursor isn't over the palette;
    /// cancelled while it is, so the palette stays open).
    collapse_timer: Option<gtk::glib::SourceId>,
    /// Shared collapse delay (seconds) after the cursor leaves the palette.
    palette_collapse_secs: std::rc::Rc<std::cell::Cell<u64>>,
    /// Shared "open the palette on row hover" flag (read live per hover).
    palette_hover: std::rc::Rc<std::cell::Cell<bool>>,
    /// The tags, shared with the list (#71).
    tags: std::rc::Rc<std::cell::RefCell<Vec<crate::config::Tag>>>,
    /// The keywords the chips were last built for, so post_view (which runs
    /// on every update) rebuilds them only when they changed.
    tags_rendered: std::cell::RefCell<Vec<String>>,
    /// Whether this row shows the actions palette line at all (preference).
    show_palette: bool,
    in_junk: bool,
    in_drafts: bool,
    /// Conversation size (only meaningful on a thread head).
    thread_count: usize,
    /// Nested reply under a thread head.
    is_thread_child: bool,
    is_last_child: bool,
    /// Whether this head's conversation is expanded.
    thread_expanded: bool,
    thread_expandable: bool,
    /// Sent-folder rows name the recipient, not the sender (#27).
    show_recipient: bool,
    /// Conversation key for the head's expand/collapse toggle.
    thread_key: Option<(u32, String)>,
    /// Newest member's display time (thread heads only), shown as the row date.
    thread_date: Option<String>,
    thread_from: Option<(String, String)>,
    thread_preview: Option<String>,
    /// Any message in this conversation is unread (heads only).
    thread_unread: bool,
    thread_starred: bool,
    /// Shared row keys, so a drag from this row can carry the whole selection.
    drag_keys: DragKeys,
    /// Shared conversation members, so a drag from a head row carries its thread.
    thread_drag: ThreadDragKeys,
    /// Drives the row's own Revealer — false only for the brief window a
    /// newly-expanded reply is sliding open, or a collapsing one is sliding
    /// shut before it's removed from the list.
    revealed: bool,
    /// Shared "swap swipe sides" preference, read live on each drag (#swipe).
    swipe_reversed: std::rc::Rc<std::cell::Cell<bool>>,
    /// Shared "swipe at all" preference (#92).
    swipe_enabled: std::rc::Rc<std::cell::Cell<bool>>,
    /// Shared trackpad swipe sensitivity, read live on each render.
    swipe_sensitivity: std::rc::Rc<std::cell::Cell<f64>>,
    /// Current swipe distance in px (negative = dragged left) — the source
    /// of truth while a gesture is live; reset to 0 the instant a release is
    /// resolved (post_view animates the strip back out of view).
    swipe_progress: f64,
    /// Which side last had a nonzero `swipe_progress` (-1 left, 1 right, 0
    /// never dragged) — kept once `swipe_progress` returns to 0 so the
    /// revealed side and action don't flip mid-shrink after a release.
    swipe_side: i8,
    /// A mouse-button or trackpad gesture is actively dragging this row.
    swipe_dragging: bool,
    /// The row is committing (#swipe): the release cleared the commit
    /// distance, so it is flying out to `swipe_side` while its Revealer
    /// closes. Cleared only if the action leaves the row in place.
    swipe_committing: bool,
    /// The exit animation has been started — `post_view` can run again
    /// before the row is removed, and it must not restart mid-flight.
    swipe_exit_started: std::cell::Cell<bool>,
    /// The row is mid-swipe: from the first drag until the snap-back
    /// animation lands — the `.swiping` class squares the pill off and
    /// drops its margins for that whole span, so the content and the strip
    /// under it read as one full-width surface rather than a rounded card
    /// sliding over a coloured band.
    swipe_active: bool,
    /// The row's `AdwSwipeTracker`, built once against its `SwipeSurface` in
    /// post_view — also doubles as the wiring guard, since the tracker has
    /// to stay alive for as long as the row does or it stops firing.
    swipe_tracker: std::cell::RefCell<Option<adw::SwipeTracker>>,
    /// In-flight snap-back-to-rest animation, if any.
    swipe_anim: std::cell::RefCell<Option<adw::TimedAnimation>>,
}

#[derive(Debug)]
pub enum MessageRowInput {
    /// Slide the avatar away or back (Focus Mode).
    SetAvatarShown(bool),
    /// Show this many lines of preview in place (Focus Mode; the rebuild
    /// that follows makes it permanent).
    SetPreviewLines(u32),
    SetRead(bool),
    SetStarred(bool),
    SetKeywords(Vec<String>),
    /// The palette's tag button: pop the tag menu on it.
    OpenTagMenu(gtk::Button),
    /// The palette's Move to… button: the list opens the folder picker under it.
    OpenMoveMenu(gtk::Button),
    SetHasAttachment(bool),
    /// The pointer entered/left the row — fade the chevron in/out.
    SetRowHover(bool),
    /// Another row's palette opened — fold this one if it is out.
    ClosePalette,
    /// Slide the actions palette open or shut.
    TogglePalette,
    /// The cursor moved onto the palette — keep it open (cancel auto-collapse).
    PaletteEnter,
    /// The cursor left the palette — arm the auto-collapse countdown.
    PaletteLeave,
    /// The auto-collapse countdown elapsed — slide the palette shut.
    CollapsePalette,
    Action(RowAction),
    /// The conversation chevron was clicked — expand/collapse the thread.
    ToggleThreadClicked,
    /// The thread's aggregate unread state changed (a hidden reply was read).
    SetThreadUnread(bool),
    SetThreadStarred(bool),
    /// Drive the row's own Revealer directly — used to slide a reply open
    /// right after it's inserted, or shut just before it's removed.
    SetRevealed(bool),
    /// The head's conversation was expanded/collapsed — updates in place
    /// (the head row survives a toggle) so the chevron actually rotates
    /// instead of mounting pre-set to its final angle.
    SetThreadExpanded(bool),
    /// A mouse-drag or trackpad swipe moved (#swipe): the distance dragged so
    /// far in px, negative = left.
    SwipeUpdate(f64),
    /// The swipe gesture was released. Whether it fires an action is decided
    /// on `swipe_progress` alone (see the handler) rather than trusting
    /// `AdwSwipeTracker`'s own velocity-aware snap-point choice — a quick
    /// flick short of the commit distance reads as a false positive when
    /// the intent was clearly to back out, not to commit fast.
    SwipeEnd,
    /// The post-release snap-back animation landed (or there was nothing
    /// to animate): the row can drop its `.swiping` geometry again.
    SwipeSettled,
    /// A swipe preference changed: post_view enables or disables the row's
    /// tracker accordingly, and re-reads the trackpad sensitivity.
    SwipePrefsChanged,
    /// The commit exit has landed: fire the action it committed to.
    SwipeCommitted(RowAction),
    /// The action left the row in place — put the exit back.
    SwipeRestore,
}

#[derive(Debug)]
pub enum MessageRowOutput {
    Action { action: RowAction, message: Box<Message> },
    /// Move to… pressed on the palette: open the picker at window point (x, y).
    MoveTo { message: Box<Message>, x: f64, y: f64 },
    /// A tag toggled from the palette's tag menu (#71).
    SetTag { message: Box<Message>, keyword: String, add: bool },
    ToggleThread((u32, String)),
    /// This row's palette just opened — the list closes every other one.
    PaletteOpened(usize),
}

/// The keys of every selected row in the ListBox this drag started from, in list
/// order. Empty when the row has no list parent yet or nothing is selected — the
/// caller then falls back to the dragged row alone.
/// Display names from a raw To header: "Ann <a@x>, b@y" -> "Ann, b@y".
fn recipient_names(to: &str) -> String {
    let mut names: Vec<String> = Vec::new();
    for part in to.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let name = match part.split_once('<') {
            Some((n, _)) if !n.trim().trim_matches('"').is_empty() => {
                n.trim().trim_matches('"').to_string()
            }
            Some((_, rest)) => rest.trim_end_matches('>').trim().to_string(),
            None => part.to_string(),
        };
        if !name.is_empty() {
            names.push(name);
        }
    }
    names.join(", ")
}

/// The first recipient's bare address from a raw To header, if any.
fn first_recipient_addr(to: &str) -> Option<String> {
    let first = to.split(',').map(str::trim).find(|p| !p.is_empty())?;
    let addr = match first.split_once('<') {
        Some((_, rest)) => rest.trim_end_matches('>').trim(),
        None => first,
    };
    (!addr.is_empty()).then(|| addr.to_string())
}

fn drag_selection(src: &gtk::DragSource, keys: &DragKeys) -> Vec<(u32, u32, u32, u32)> {
    let Some(list) = src.widget().and_then(|w| w.parent()).and_downcast::<gtk::ListBox>() else {
        return Vec::new();
    };
    let keys = keys.borrow();
    list.selected_rows()
        .iter()
        .filter_map(|r| keys.get(r.index() as usize).copied())
        .collect()
}

impl MessageRow {
    /// Rebuild the row's tag chips (#71): one pill per keyword that names a
    /// tag, in tag order, wearing the tag's colour class.
    fn render_tags(&self, tags_box: &gtk::Box) {
        while let Some(child) = tags_box.first_child() {
            tags_box.remove(&child);
        }
        for t in self.tags.borrow().iter().filter(|t| self.msg.has_keyword(&t.keyword)) {
            let chip = gtk::Label::new(Some(&t.name));
            chip.add_css_class("tag-chip");
            chip.add_css_class(&t.css_class());
            chip.set_valign(gtk::Align::Center);
            chip.set_ellipsize(gtk::pango::EllipsizeMode::End);
            chip.set_max_width_chars(14);
            tags_box.append(&chip);
        }
        *self.tags_rendered.borrow_mut() = self.msg.keywords.clone();
    }
}

/// The tag section of a message menu (#71): one entry per tag, its swatch
/// filled where the message carries it; choosing an entry toggles that tag
/// through `toggle(keyword, add)`.
pub fn tag_menu_entries(
    tags: &[crate::config::Tag],
    msg: &Message,
    toggle: impl Fn(String, bool) + Clone + 'static,
) -> Vec<MenuEntry> {
    tags.iter()
        .map(|t| {
            let on = msg.has_keyword(&t.keyword);
            let keyword = t.keyword.clone();
            let toggle = toggle.clone();
            MenuEntry::new(t.name.clone(), move || toggle(keyword.clone(), !on))
                .swatch(t.color.clone(), on)
        })
        .collect()
}

// A two-layer container implementing `AdwSwipeable`, so a real
// `AdwSwipeTracker` — the gesture engine behind `AdwFlap` and `AdwCarousel` —
// can drive the row's swipe-to-act gesture (#swipe): `background` is the
// fixed action strip, always allocated full-size and never moving;
// `foreground` is the row's real content, translated across it via a
// `GskTransform` on its allocation as the swipe drags it, Gmail-style.
glib::wrapper! {
    pub struct SwipeSurface(ObjectSubclass<swipe_surface_imp::SwipeSurface>)
        @extends gtk::Widget,
        @implements adw::Swipeable;
}

impl Default for SwipeSurface {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl SwipeSurface {
    /// The fixed action strip underneath — set first, so it ends up behind
    /// `foreground` in paint order.
    fn set_background(&self, child: &impl IsA<gtk::Widget>) {
        child.set_parent(self);
    }

    /// The row's real content, on top — translated by [`Self::set_progress_px`]
    /// to reveal `background` underneath it.
    fn set_foreground(&self, child: &impl IsA<gtk::Widget>) {
        child.set_parent(self);
    }

    /// The live swipe distance, in `AdwSwipeTracker`'s own px convention
    /// (`AdwSwipeable::progress` reports it back verbatim). Queues a fresh
    /// allocation so the translation actually moves — setting this alone
    /// touches no property GTK would otherwise notice.
    fn set_progress_px(&self, px: f64) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        self.imp().progress_px.set(px);
        self.queue_allocate();
    }

    /// Read back by `post_view` as the "from" value when animating a
    /// released drag smoothly back to rest.
    fn progress_px(&self) -> f64 {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        self.imp().progress_px.get()
    }

    /// The trackpad sensitivity preference, pushed in by `post_view` so both
    /// `AdwSwipeable::distance` and the tracker's own callback see the same
    /// figure.
    fn set_sensitivity(&self, factor: f64) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        self.imp().sensitivity.set(factor.clamp(
            crate::config::SWIPE_SENSITIVITY_MIN,
            crate::config::SWIPE_SENSITIVITY_MAX,
        ));
    }

    fn sensitivity(&self) -> f64 {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        self.imp().sensitivity.get()
    }
}

mod swipe_surface_imp {
    use std::cell::Cell;

    use adw::subclass::prelude::*;
    use gtk::glib;
    use gtk::prelude::*;

    pub struct SwipeSurface {
        pub progress_px: Cell<f64>,
        /// Trackpad sensitivity (see `super::SWIPE_MAX`). Never 0 — that
        /// would divide by zero in `distance`/`progress` — so this can't
        /// simply be `#[derive(Default)]`.
        pub sensitivity: Cell<f64>,
    }

    impl Default for SwipeSurface {
        fn default() -> Self {
            Self {
                progress_px: Cell::new(0.0),
                sensitivity: Cell::new(1.0),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SwipeSurface {
        const NAME: &'static str = "HylkiSwipeSurface";
        type Type = super::SwipeSurface;
        type ParentType = gtk::Widget;
        type Interfaces = (adw::Swipeable,);
    }

    impl ObjectImpl for SwipeSurface {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl SwipeSurface {
        /// The action strip: always the first (bottom-most) child.
        fn background(&self) -> Option<gtk::Widget> {
            self.obj().first_child()
        }

        /// The row's real content: always the second (top-most) child.
        fn foreground(&self) -> Option<gtk::Widget> {
            self.background().and_then(|bg| bg.next_sibling())
        }

        /// `progress_px` flipped into the row model's "dragged left is
        /// negative" convention — shared by `size_allocate` and `snapshot`
        /// so they can't disagree about which side is revealed.
        fn visual_offset(&self) -> f32 {
            -self.progress_px.get() as f32
        }
    }

    impl WidgetImpl for SwipeSurface {
        // Height-for-width, like the content it wraps: the default for a
        // custom widget is constant-size, under which GTK measured the row's
        // height with no width at all — so a wrapping multi-line preview
        // came out clipped to under two lines (regression since 1.23.0).
        fn request_mode(&self) -> gtk::SizeRequestMode {
            self.foreground()
                .map(|c| c.request_mode())
                .unwrap_or(gtk::SizeRequestMode::ConstantSize)
        }

        // The background strip never dictates the row's size — only the
        // real content does; the strip is simply stretched to match it.
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            self.foreground()
                .map(|c| c.measure(orientation, for_size))
                .unwrap_or((0, 0, -1, -1))
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(bg) = self.background() {
                bg.allocate(width, height, baseline, None);
            }
            if let Some(fg) = self.foreground() {
                let offset = self.visual_offset();
                let transform = (offset != 0.0).then(|| {
                    gtk::gsk::Transform::new()
                        .translate(&gtk::graphene::Point::new(offset, 0.0))
                });
                fg.allocate(width, height, baseline, transform);
            }
        }

        // Only ever paints the exact gap the content has slid away from,
        // clipped from `offset` directly — a row has no background of its
        // own until hovered or selected, so an opaque foreground can't be
        // relied on to hide the strip the rest of the time. Also supplies
        // the clip the default snapshot lacks for `foreground`, so a drag
        // can't paint over the row above or below. The clip starts
        // THREAD_NODE_REACH left of the box: a thread member's node dot and
        // rail stub deliberately hang out there, onto the rail.
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            let reach = super::THREAD_NODE_REACH;
            snapshot.push_clip(&gtk::graphene::Rect::new(-reach, 0.0, w + reach, h));

            let offset = self.visual_offset();
            if let (Some(bg), true) = (self.background(), offset != 0.0) {
                // offset < 0: content slid left, uncovering a gap on the
                // RIGHT. offset > 0: slid right, gap on the LEFT.
                let gap = if offset < 0.0 {
                    gtk::graphene::Rect::new(w + offset, 0.0, -offset, h)
                } else {
                    gtk::graphene::Rect::new(0.0, 0.0, offset, h)
                };
                snapshot.push_clip(&gap);
                obj.snapshot_child(&bg, snapshot);
                snapshot.pop();
            }
            if let Some(fg) = self.foreground() {
                obj.snapshot_child(&fg, snapshot);
            }
            snapshot.pop();
        }
    }

    impl SwipeableImpl for SwipeSurface {
        // What one full swipe (progress ±1.0) costs the pointer: `SWIPE_MAX`
        // px at sensitivity 1.0, and deliberately *more* the higher the
        // trackpad sensitivity goes. `AdwSwipeTracker` divides a mouse or
        // touchscreen drag by this, and `wire_swipe_tracker` multiplies the
        // progress back by the same factor, so a drag still moves the row
        // exactly as far as the pointer went, whatever the preference says.
        // A trackpad's two-finger scroll never reaches here — libadwaita
        // scales that against a fixed 400px of its own — so the
        // multiplication is all that path feels, which is exactly the knob
        // this preference wants.
        fn distance(&self) -> f64 {
            super::SWIPE_MAX * self.sensitivity.get()
        }

        fn progress(&self) -> f64 {
            self.progress_px.get() / (super::SWIPE_MAX * self.sensitivity.get())
        }

        fn cancel_progress(&self) -> f64 {
            0.0
        }

        // Three stops: fully committed left, at rest, fully committed
        // right. `AdwSwipeTracker` picks whichever is nearest on release,
        // factoring in velocity — a fast flick short of the full distance
        // still commits, exactly like a real swipe-to-dismiss should.
        fn snap_points(&self) -> Vec<f64> {
            vec![-1.0, 0.0, 1.0]
        }

        fn swipe_area(
            &self,
            _navigation_direction: adw::NavigationDirection,
            _is_drag: bool,
        ) -> gtk::gdk::Rectangle {
            let w = self.obj();
            gtk::gdk::Rectangle::new(0, 0, w.width(), w.height())
        }
    }
}

/// The px a tracker `progress` reading moves the row: it undoes the
/// sensitivity `SwipeSurface::distance` folded in (leaving a mouse or
/// touchscreen drag exactly 1:1 with the pointer, whatever the preference
/// says), and caps the result at one full swipe so a long trackpad scroll
/// can't push the row on past the action strip.
fn swipe_progress_px(progress: f64, sensitivity: f64) -> f64 {
    (progress * SWIPE_MAX * sensitivity).clamp(-SWIPE_MAX, SWIPE_MAX)
}

/// Build the `AdwSwipeTracker` driving `surface`'s swipe-to-act gesture
/// (#swipe): mouse-drag and trackpad both arrive as the same signals.
/// Called once per row from `post_view`; the tracker must be kept alive by
/// the caller for as long as the row lives.
fn wire_swipe_tracker(surface: &SwipeSurface, sender: &FactorySender<MessageRow>) -> adw::SwipeTracker {
    use gtk::prelude::OrientableExt;

    let tracker = adw::SwipeTracker::new(surface);
    tracker.set_orientation(gtk::Orientation::Horizontal);
    tracker.set_allow_mouse_drag(true);

    // `AdwSwipeTracker`'s progress/snap-point convention runs opposite to
    // the row model's "dragged left is negative" one for a horizontal
    // tracker, hence the negation below — kept only where each value is
    // actually consumed, since `AdwSwipeable::progress` still has to answer
    // the tracker back in its own native convention (`swipe_surface_imp`).
    {
        let surface = surface.clone();
        let sender = sender.clone();
        tracker.connect_update_swipe(move |_, progress| {
            let raw_px = swipe_progress_px(progress, surface.sensitivity());
            surface.set_progress_px(raw_px);
            sender.input(MessageRowInput::SwipeUpdate(-raw_px));
        });
    }
    {
        let sender = sender.clone();
        // The release's own resolved snap point (`to`) isn't used — see
        // `MessageRowInput::SwipeEnd`.
        tracker.connect_end_swipe(move |_, _velocity, _to| {
            sender.input(MessageRowInput::SwipeEnd);
        });
    }
    tracker
}

#[relm4::factory(pub)]
impl FactoryComponent for MessageRow {
    type Init = RowInit;
    type Input = MessageRowInput;
    type Output = MessageRowOutput;
    type CommandOutput = FaceCmd;
    type ParentWidget = gtk::ListBox;

    view! {
        gtk::ListBoxRow {
            // Unread rows get a pale accent background (cleared once read);
            // thread replies are indented.
            #[watch]
            set_css_classes: &self.row_css(),

            // Track hover so the actions palette chevron can fade in/out.
            add_controller = gtk::EventControllerMotion {
                connect_enter[sender] => move |_, _, _| sender.input(MessageRowInput::SetRowHover(true)),
                connect_leave[sender] => move |_| sender.input(MessageRowInput::SetRowHover(false)),
            },

            // Drag a message onto a sidebar folder to move it there. The payload
            // carries one (account, source folder, UID, id) group per message —
            // the *whole* selection when this row is part of it, so dragging a
            // multi-selection moves every message, not just the row under the
            // pointer (#23).
            add_controller = gtk::DragSource {
                set_actions: gtk::gdk::DragAction::MOVE,
                // The row itself travels under the pointer, held where it was
                // grabbed — without an icon GTK draws the payload string.
                connect_drag_begin => move |src, drag| {
                    let scale = src.widget().map(|w| w.scale_factor()).unwrap_or(1);
                    if let Some(row) = src.widget() {
                        // `.dragging` fades the row while it is away.
                        row.add_css_class("dragging");
                    }
                    // A white envelope, cursor-sized, centred under the
                    // pointer — without an icon GTK draws the payload text.
                    // Set on the drag's own icon window as a widget, so it
                    // is drawn at logical size from a display-scale texture.
                    if let Some(envelope) = crate::app_icon::drag_envelope(scale) {
                        let icon = gtk::DragIcon::for_drag(drag);
                        icon.set_child(Some(&envelope));
                        let half = crate::app_icon::DRAG_ICON_SIZE / 2;
                        drag.set_hotspot(half, half);
                    }
                },
                connect_drag_end => move |src, _, _| {
                    if let Some(row) = src.widget() {
                        row.remove_css_class("dragging");
                    }
                },
                connect_drag_cancel => move |src, _, _| {
                    if let Some(row) = src.widget() {
                        row.remove_css_class("dragging");
                    }
                    false
                },
                connect_prepare[aid = self.msg.account_id, fid = self.msg.folder_id, uid = self.msg.uid, id = self.msg.id, keys = self.drag_keys.clone(), threads = self.thread_drag.clone()] => move |src, _, _| {
                    let mut items = drag_selection(src, &keys);
                    // Dragging a row outside the selection (or before the list has
                    // published its keys) moves just that row.
                    if !items.iter().any(|k| k.0 == aid && k.3 == id) {
                        items = vec![(aid, fid, uid, id)];
                    }
                    // A conversation row stands for its whole thread (#171):
                    // every member goes along, as with its Delete.
                    let threads = threads.borrow();
                    items = items
                        .into_iter()
                        .flat_map(|k| threads.get(&(k.0, k.3)).cloned().unwrap_or_else(|| vec![k]))
                        .collect();
                    let mut payload = String::from("vireo-move");
                    for (a, f, u, i) in items {
                        payload.push_str(&format!("\t{a}\t{f}\t{u}\t{i}"));
                    }
                    Some(gtk::gdk::ContentProvider::for_value(&payload.to_value()))
                },
            },

            // Thread replies animate open/shut by sliding, instead of the
            // list jumping to a new row count the instant a thread toggles
            // (see `revealed` / `MessageRowInput::SetRevealed`). (PR #79)
            #[wrap(Some)]
            set_child = &gtk::Revealer {
            set_transition_type: gtk::RevealerTransitionType::SlideDown,
            set_transition_duration: 200,
            #[watch]
            set_reveal_child: self.revealed,

            // Wrapped in a `SwipeSurface` (#swipe): `background` is the fixed
            // action strip, only ever revealed as `foreground` — the row's
            // real content Overlay, unchanged otherwise — physically slides
            // away from it under a mouse-drag or trackpad swipe.
            #[wrap(Some)]
            #[name = "swipe_surface"]
            set_child = &SwipeSurface {

            #[name = "swipe_background"]
            set_background = &gtk::Box {
                set_overflow: gtk::Overflow::Hidden,
                #[watch]
                set_css_classes: &self.swipe_indicator_classes(),

                gtk::Box {
                    set_halign: gtk::Align::Fill,
                    set_valign: gtk::Align::Center,
                    set_hexpand: true,
                    set_spacing: 6,
                    set_margin_start: 16,
                    set_margin_end: 16,
                    #[watch]
                    set_halign: self.swipe_halign(),

                    gtk::Image {
                        #[watch]
                        set_icon_name: Some(self.swipe_icon()),
                    },
                    gtk::Label {
                        #[watch]
                        set_label: &self.swipe_label(),
                    },
                },
            },

            #[name = "row_overlay"]
            set_foreground = &gtk::Overlay {
            // The node dot where this member meets the group's dotted rail
            // (thread children only): overlaid at the row's left edge and
            // pulled onto the rail itself by .thread-node's negative margin.
            // The last reply's rail: a real dotted border on a widget spanning
            // exactly the row's top half (homogeneous halves), so it ends at
            // the node dot and renders identically to the sibling rows' full
            // border rails — a gradient imitation drew square dots. Added
            // before the node dot so the dot draws over where they meet.
            add_overlay = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_halign: gtk::Align::Start,
                set_homogeneous: true,
                set_visible: self.is_last_child,

                gtk::Box {
                    add_css_class: "thread-rail-stub",
                },
                gtk::Box {},
            },

            add_overlay = &gtk::Box {
                add_css_class: "thread-node",
                set_halign: gtk::Align::Start,
                set_valign: gtk::Align::Center,
                set_visible: self.is_thread_child,
            },

            // The actions palette floats over the pill's bottom-left corner,
            // opening rightward from the ⋯ button. As an overlay it takes no
            // room in the row: the text sits centred in the pill whether the
            // palette preference is on or off (the reserved line used to read
            // as a lopsided empty band under the text — yioannides, #81).
            //
            // The outer holder has a FIXED width: overlay children are only
            // re-allocated lazily, so a container that grows with the slide
            // animation snaps in whenever the next relayout happens instead
            // of animating. Constant allocation outside, free growth inside.
            add_overlay = &gtk::Box {
                set_width_request: 320,
                set_halign: gtk::Align::Start,
                set_valign: gtk::Align::End,

                #[name = "actions_line"]
                gtk::Box {
                    set_spacing: 0,
                    set_halign: gtk::Align::Start,
                    set_valign: gtk::Align::End,
                    // With avatars on, the ⋯ centres under the avatar:
                    // circle centre (pill inset 6 + padding + 19) minus half
                    // the button. Thread children share the exact pill
                    // geometry and only add their card's 10px indent. Without
                    // circles it hugs the pill's edge. All measured from the
                    // row's edge, which now sits 6px outside the pill's.
                    set_margin_start: if self.avatars {
                        if self.is_thread_child { 32 } else { 22 }
                    } else if self.is_thread_child {
                        20
                    } else {
                        10
                    },
                    // With circles on, ride lower for a sliver of air between
                    // the circle's bottom edge and the ⋯ — but keep 1px clear
                    // of the pill's own bottom edge (the pill's 2px outer
                    // margin sits inside this overlay's bounds).
                    set_margin_bottom: if self.avatars { 3 } else { 4 },
                    // Open, the whole line — ⋯ and icons — sits on one card
                    // surface; the ⋯ is the card's left cap.
                    #[watch]
                    set_css_classes: if self.palette_open {
                        &["actions-line", "open"]
                    } else {
                        &["actions-line"]
                    },
                    // Preference: no palette at all — the line (and the space
                    // it reserves under the preview) goes away entirely.
                    set_visible: self.show_palette,

                    // Actions toggle (⋯). Clicking it opens/closes the palette but
                    // does NOT select or open the message (it's a button, so the
                    // click is consumed before the row's selection gesture).
                    gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-view-more-horizontal-symbolic",
                        // Hidden until the row is hovered (or the palette is open);
                        // the .revealed class fades it in via a CSS transition.
                        #[watch]
                        set_css_classes: &self.chevron_classes(),
                        set_tooltip_text: Some(i18n("Actions").as_str()),
                        set_valign: gtk::Align::Center,
                        connect_clicked => MessageRowInput::TogglePalette,
                    },

                    // Not a GtkRevealer: inside an Overlay's overlay child
                    // its slide never repainted frame-by-frame (the palette
                    // just popped in at the end). Instead the palette hangs
                    // as a clipped overlay over a spacer whose width is the
                    // one thing animated (post_view): the spacer is a hard
                    // cap — a Box's width_request is only a floor, and the
                    // palette at natural width would simply ignore it.
                    #[name = "palette_clip"]
                    gtk::Overlay {
                        #[wrap(Some)]
                        set_child = &gtk::Box {
                            #[name = "palette_spacer"]
                            gtk::Box {
                                set_width_request: 0,
                            },
                        },

                        #[name = "palette_inner"]
                        add_overlay = &gtk::Box {
                            add_css_class: "actions-palette",
                            set_halign: gtk::Align::Start,
                            set_valign: gtk::Align::Center,
                            set_spacing: 0,
                            // Hidden until the first open: rows that never
                            // process an update never run post_view, so the
                            // clip isn't armed yet — an unclipped palette
                            // would paint over every row (which it did).
                            set_visible: false,

                            // Keep the palette open while the cursor is over it.
                            add_controller = gtk::EventControllerMotion {
                                connect_enter[sender] => move |_, _, _| sender.input(MessageRowInput::PaletteEnter),
                                connect_leave[sender] => move |_| sender.input(MessageRowInput::PaletteLeave),
                            },

                            // The eleven action buttons are built on the
                            // palette's first open (`MessageRow::build_palette`):
                            // most rows never open theirs, and building them
                            // for every row was the larger part of a row's
                            // cost.
                        },
                    },

                },
            },

            #[wrap(Some)]
            set_child = &gtk::Box {
            set_orientation: gtk::Orientation::Horizontal,
            // Tighter than the row's left padding: the avatar sits well inside
            // the list's edge, and the unread dot's gutter is narrow enough that
            // the sender's name still reads as the start of the row.
            set_spacing: 8,
            set_css_classes: &self.content_css(),
            // A palette wider than the row it sits in is clipped here rather than
            // painted across the divider into the reader.
            set_overflow: gtk::Overflow::Hidden,

            // The circle sits in a revealer so Focus Mode can slide it away
            // (and back) before the rows are rebuilt without (or with) it.
            gtk::Revealer {
                // SlideRight: folding, the circle moves off past the row's
                // left edge (GTK names the transition for the reveal).
                set_transition_type: gtk::RevealerTransitionType::SlideRight,
                set_transition_duration: crate::ui::FOCUS_ANIM_MS,
                add_css_class: "focus-fade",
                // Hidden, not faded: the point of turning these off is to get the
                // width back, so the row must give up the slot entirely (#29).
                set_visible: self.avatars,
                #[watch]
                set_reveal_child: self.avatar_shown,
                #[watch]
                set_css_classes: if self.avatar_shown { &["focus-fade"] } else { &["focus-fade", "away"] },

                adw::Avatar {
                    set_size: 38,
                    set_valign: gtk::Align::Center,
                    set_show_initials: true,
                    // Account colour ring (unified view only).
                    set_css_classes: &self.ring_classes(),
                    #[watch]
                    set_text: Some(&self.face_name()),
                    #[watch]
                    set_custom_image: self.avatar_image().as_ref(),
                },
            },

            // Faded rather than hidden: a hidden widget gives up its slot in
            // the box, so read rows' text would sit left of unread rows' and
            // the column would jitter as mail is read — the slot is always
            // reserved and only the dot's ink changes (re-affirmed in #99:
            // collapsing it was tried and the shifting read worse). With
            // avatars on the dot centres beside the circle; without them it
            // leads the row on the sender name's line (the .no-avatar margin
            // in styles.css).
            gtk::Box {
                add_css_class: "unread-dot",
                set_valign: if self.avatars { gtk::Align::Center } else { gtk::Align::Start },
                #[watch]
                set_opacity: if self.msg.unread || self.thread_unread { 1.0 } else { 0.0 },
            },

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 2,
                set_hexpand: true,
                // Beside an avatar the text block centres on the circle; with
                // avatars off it anchors to the pill's top, under the corner
                // radius (#99) — the pill's own padding provides the inset.
                set_valign: if self.avatars { gtk::Align::Center } else { gtk::Align::Start },

                gtk::Box {
                    set_spacing: 6,
                    gtk::Label {
                        set_label: &self.name_line(),
                        set_halign: gtk::Align::Start,
                        set_hexpand: true,
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        #[watch]
                        set_css_classes: &self.sender_classes(),
                    },
                    gtk::Image {
                        set_icon_name: Some("co.hyprlab.Hylki-mail-attachment-symbolic"),
                        #[watch]
                        set_visible: self.msg.has_attachment,
                        add_css_class: "dim-icon",
                    },
                    gtk::Image {
                        set_icon_name: Some("co.hyprlab.Hylki-starred-symbolic"),
                        #[watch]
                        set_visible: self.msg.starred || self.thread_starred,
                        add_css_class: "star-icon",
                    },
                    gtk::Label {
                        set_label: self.thread_date.as_deref().unwrap_or(&self.msg.datetime_list()),
                        set_halign: gtk::Align::End,
                        // Ellipsized so it stops being the row's floor: it is the
                        // one item on this line with no give, and it held the list
                        // ~40px wider than the palette needs (#29).
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        add_css_class: "message-date",
                    },
                    // Conversation chip (thread heads only): the message count
                    // and the expand/collapse caret merged into one grey pill.
                    gtk::Button {
                        set_visible: self.thread_count > 1,
                        set_tooltip_text: Some(i18n("Show conversation").as_str()),
                        add_css_class: "flat",
                        add_css_class: "thread-chip",
                        set_valign: gtk::Align::Center,
                        connect_clicked[sender] => move |_| sender.input(MessageRowInput::ToggleThreadClicked),
                        gtk::Box {
                            set_spacing: 2,
                            // Centred in the pill: with the caret hidden
                            // (expansion off) the bare count must not sit
                            // against the chip's left edge. Alignment only —
                            // hexpand here propagates up and stretches the
                            // whole chip across the header line.
                            set_halign: gtk::Align::Center,
                            gtk::Label {
                                set_label: &self.thread_count.to_string(),
                            },
                            // One right-pointing caret; the "open" class
                            // rotates it 90° via a CSS transition (mirrors the
                            // sidebar's folder-tree expander) instead of
                            // swapping glyphs. (PR #79)
                            gtk::Image {
                                // No caret when expansion is off — the chip is
                                // just a count then, not a toggle.
                                set_visible: self.thread_expandable,
                                set_icon_name: Some("co.hyprlab.Hylki-pan-end-symbolic"),
                                #[watch]
                                set_css_classes: if self.thread_expanded {
                                    &["thread-toggle-icon", "open"]
                                } else {
                                    &["thread-toggle-icon"]
                                },
                            },
                        },
                    },
                },

                gtk::Box {
                    set_spacing: 6,
                    // Hidden outright by Focus Mode's subject part: the tag
                    // chips ride on this line, and a row of chips under a
                    // lone sender reads as a stray, so the line goes whole.
                    set_visible: self.show_subject,
                    gtk::Label {
                        set_label: &self.msg.subject,
                        set_halign: gtk::Align::Start,
                        set_hexpand: true,
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        #[watch]
                        set_css_classes: &self.subject_classes(),
                    },
                    // Tag chips (#71) at the subject's end, where a long
                    // subject gives way before the sender's name would.
                    // Built from the message's keywords in init_widgets and
                    // rebuilt by post_view when they change.
                    #[local_ref]
                    tags_box -> gtk::Box {
                        set_spacing: 4,
                        set_valign: gtk::Align::Center,
                    },
                },

                // The message's own text, at full width: nothing shares this line,
                // so it never reflows or gets covered.
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 4,
                    // 0 lines: previews are off, so the row gives them no space.
                    set_visible: self.preview_lines > 0,

                    // An encrypted message (#133) shows a lock where its text
                    // would be, in the preview's own dimmed colour: a symbolic
                    // icon takes the label's foreground, so it follows the
                    // light and dark themes with it.
                    gtk::Image {
                        set_icon_name: Some("co.hyprlab.Hylki-channel-secure-symbolic"),
                        set_pixel_size: 12,
                        set_valign: gtk::Align::Center,
                        set_visible: crate::models::preview_is_encrypted(
                            self.thread_preview.as_deref().unwrap_or(&self.msg.preview),
                        ),
                        add_css_class: "message-preview",
                    },

                gtk::Label {
                    set_label: &crate::models::preview_display(
                        self.thread_preview.as_deref().unwrap_or(&self.msg.preview),
                    ),
                    // Fill (not Start): the layout width then matches the
                    // allocation exactly, so the ellipsis lands right where the
                    // text is cut instead of stranded at a stale layout edge.
                    set_halign: gtk::Align::Fill,
                    set_hexpand: true,
                    set_xalign: 0.0,
                    // A single preview line never wraps: wrap + `lines` +
                    // ellipsize is the combination that detaches the "…" from
                    // the text; a plain ellipsized line keeps it attached and
                    // tracks the pane width continuously.
                    #[watch]
                    set_wrap: self.preview_lines > 1,
                    set_wrap_mode: gtk::pango::WrapMode::WordChar,
                    set_ellipsize: gtk::pango::EllipsizeMode::End,
                    // A ceiling, not a reservation: a short message keeps a short
                    // row, so the list stays scannable and only long messages use
                    // the extra lines.
                    #[watch]
                    set_lines: self.preview_lines.max(1) as i32,
                    add_css_class: "message-preview",
                },
                },

            },
            },
            },
            },
        }
        }
    }

    fn post_view() {
        if *self.tags_rendered.borrow() != self.msg.keywords {
            self.render_tags(&widgets.tags_box);
        }
        // Slide the palette open/shut by animating the spacer that gives the
        // clip Overlay its width — driven here (not a GtkRevealer) because
        // revealer transitions don't repaint inside an Overlay's overlay
        // child. Runs on every view update; only a change in target width
        // starts a new animation.
        widgets.palette_clip.set_clip_overlay(&widgets.palette_inner, true);
        if self.palette_open && !self.palette_built.get() {
            self.build_palette(&widgets.palette_inner, &sender);
        }
        self.sync_palette();
        if self.palette_open {
            widgets.palette_inner.set_visible(true);
        }
        let (inner_w, inner_h) = (
            widgets.palette_inner.measure(gtk::Orientation::Horizontal, -1).1,
            widgets.palette_inner.measure(gtk::Orientation::Vertical, -1).1,
        );
        widgets.palette_spacer.set_height_request(inner_h);
        let target = if self.palette_open { inner_w } else { 0 };
        if self.palette_target.get() != target {
            self.palette_target.set(target);
            let spacer = widgets.palette_spacer.clone();
            let from = spacer.width() as f64;
            let setter = {
                let spacer = spacer.clone();
                adw::CallbackAnimationTarget::new(move |v| spacer.set_width_request(v as i32))
            };
            // Bound to the line (mapped: it holds the visible toggle), NOT
            // the spacer — adw skips animations on unmapped widgets, and the
            // zero-width spacer counts as one, which made the slide jump
            // straight to its end value.
            let anim =
                adw::TimedAnimation::new(&widgets.actions_line, from, target as f64, 180, setter);
            anim.set_easing(adw::Easing::EaseOutCubic);
            if target == 0 {
                // Sliding shut: hide only once fully back in the button, so
                // the close still reads as a slide.
                let inner = widgets.palette_inner.downgrade();
                anim.connect_done(move |_| {
                    if let Some(inner) = inner.upgrade() {
                        inner.set_visible(false);
                    }
                });
            }
            if let Some(old) = self.palette_anim.borrow_mut().replace(anim) {
                old.pause();
            }
            if let Some(a) = self.palette_anim.borrow().as_ref() {
                a.play();
            }
        }

        // One-time gesture wiring (#swipe): no hook to attach an imperative
        // controller once from the declarative view, so it happens here on
        // the row's first render. The tracker doubles as the wiring guard —
        // kept alive in the model, since it stops firing once dropped.
        if self.swipe_tracker.borrow().is_none() {
            let tracker = wire_swipe_tracker(&widgets.swipe_surface, &sender);
            self.swipe_tracker.replace(Some(tracker));
        }
        widgets
            .swipe_surface
            .set_sensitivity(self.swipe_sensitivity.get());
        if let Some(t) = self.swipe_tracker.borrow().as_ref() {
            // A committing row takes no new gestures — it is on its way out.
            t.set_enabled(self.swipe_enabled.get() && !self.swipe_committing);
        }

        // A live drag already tracks 1:1 — `wire_swipe_tracker`'s
        // `update-swipe` handler sets the surface's progress directly, every
        // event. Once released, this animates the rest of the way smoothly
        // instead of letting `swipe_progress` resetting snap it.
        if self.swipe_dragging {
            if let Some(a) = self.swipe_anim.borrow_mut().take() {
                a.pause();
            }
        } else if self.swipe_committing && !self.swipe_exit_started.get() {
            // The exit: carry the content clear off the row's own side while
            // the Revealer (already told to close) shuts the height. Started
            // exactly once — post_view runs again on any later change, and a
            // restart mid-flight would stutter.
            self.swipe_exit_started.set(true);
            let span = (widgets.swipe_surface.width() as f64).max(SWIPE_MAX * 2.0);
            // Negated into `AdwSwipeTracker`'s convention, like the snap-back.
            let target = if self.swipe_side < 0 { span } else { -span };
            let surface = widgets.swipe_surface.clone();
            let setter = {
                let surface = surface.clone();
                adw::CallbackAnimationTarget::new(move |v| surface.set_progress_px(v))
            };
            let anim = adw::TimedAnimation::new(
                &widgets.row_overlay,
                surface.progress_px(),
                target,
                SWIPE_EXIT_MS,
                setter,
            );
            // Carries the drag's own momentum on rather than starting over.
            anim.set_easing(adw::Easing::EaseOutCubic);
            // The action fires as the exit lands, so the removal that follows
            // has nothing left to hide. Tied to the animation rather than a
            // timer: a row removed mid-flight takes the animation with it.
            // Sent fallibly all the same — libadwaita holds its own reference
            // to a playing animation, so `done` can still arrive after the
            // row is gone, and `input` would panic on a dead runtime.
            {
                let tx = sender.input_sender().clone();
                let action = self.swipe_action();
                anim.connect_done(move |_| {
                    let _ = tx.send(MessageRowInput::SwipeCommitted(action));
                });
            }
            if let Some(old) = self.swipe_anim.replace(Some(anim)) {
                old.pause();
            }
            if let Some(a) = self.swipe_anim.borrow().as_ref() {
                a.play();
            }
        } else if !self.swipe_committing {
            // Negated back to `AdwSwipeTracker`'s own convention, matching
            // `size_allocate`'s translation.
            let target = -self.swipe_progress;
            let current = widgets.swipe_surface.progress_px();
            if (current - target).abs() <= 0.5 {
                // Already at rest (a release right at 0): nothing to animate,
                // so settle the `.swiping` geometry straight away.
                if self.swipe_active && self.swipe_progress == 0.0 {
                    sender.input(MessageRowInput::SwipeSettled);
                }
            } else {
                let surface = widgets.swipe_surface.clone();
                let setter = {
                    let surface = surface.clone();
                    adw::CallbackAnimationTarget::new(move |v| surface.set_progress_px(v))
                };
                // Bound to the row overlay (always mapped), not the
                // surface — adw skips animations on unmapped widgets.
                let anim = adw::TimedAnimation::new(
                    &widgets.row_overlay,
                    current,
                    target,
                    180,
                    setter,
                );
                anim.set_easing(adw::Easing::EaseOutCubic);
                // The pill only rounds off and re-insets once the content
                // has fully slid back over the strip.
                if self.swipe_progress == 0.0 {
                    let tx = sender.input_sender().clone();
                    anim.connect_done(move |_| {
                        let _ = tx.send(MessageRowInput::SwipeSettled);
                    });
                }
                if let Some(old) = self.swipe_anim.replace(Some(anim)) {
                    old.pause();
                }
                if let Some(a) = self.swipe_anim.borrow().as_ref() {
                    a.play();
                }
            }
        }
    }

    fn init_model(init: Self::Init, index: &DynamicIndex, sender: FactorySender<Self>) -> Self {
        let RowInit {
            msg,
            gravatar,
            avatars,
            avatar_late,
            sender_logos,
            preview_lines,
            show_subject,
            ring_class,
            palette_collapse_secs,
            palette_hover,
            tags,
            show_palette,
            in_junk,
            in_drafts,
            thread_count,
            is_thread_child,
            is_last_child,
            thread_expanded,
            thread_expandable,
            thread_key,
            thread_date,
            thread_from,
            thread_preview,
            thread_unread,
            thread_starred,
            drag_keys,
            thread_drag,
            show_recipient,
            revealed,
            swipe_reversed,
            swipe_enabled,
            swipe_sensitivity,
        } = init;
        let mut model = Self {
            msg,
            palette_built: std::cell::Cell::new(false),
            palette_buttons: std::cell::RefCell::new(None),
            show_recipient,
            gravatar,
            avatars,
            avatar_shown: !avatar_late,
            sender_logos,
            preview_lines,
            show_subject,
            avatar_texture: None,
            initials_image: std::cell::RefCell::new(None),
            ring_class,
            row_hovered: false,
            palette_open: false,
            collapse_timer: None,
            palette_collapse_secs,
            palette_hover,
            tags,
            tags_rendered: std::cell::RefCell::new(Vec::new()),
            show_palette,
            in_junk,
            in_drafts,
            thread_count,
            is_thread_child,
            is_last_child,
            thread_expanded,
            thread_expandable,
            thread_key,
            thread_date,
            thread_from,
            thread_preview,
            thread_unread,
            thread_starred,
            drag_keys,
            thread_drag,
            revealed,
            index: index.clone(),
            palette_target: std::cell::Cell::new(0),
            palette_anim: std::cell::RefCell::new(None),
            swipe_reversed,
            swipe_enabled,
            swipe_sensitivity,
            swipe_progress: 0.0,
            swipe_side: 0,
            swipe_committing: false,
            swipe_exit_started: std::cell::Cell::new(false),
            swipe_dragging: false,
            swipe_active: false,
            swipe_tracker: std::cell::RefCell::new(None),
            swipe_anim: std::cell::RefCell::new(None),
        };

        // No point fetching anything for a circle that isn't drawn — and a
        // Gravatar lookup would send a hash of the sender's address for nothing.
        if model.avatars {
            model.load_face(&sender);
        }

        // A row that mounts already collapsed (a freshly-expanded reply)
        // flips to revealed on the next main-loop iteration, once it has been
        // measured at its natural size — animating it open instead of
        // starting from an already-final state.
        if !model.revealed {
            sender.input(MessageRowInput::SetRevealed(true));
        }
        // An avatar built folded away (Focus Mode just ended) slides in once
        // the row is on screen: a moment after mounting, not the next
        // iteration, so the revealer is mapped and animates.
        if avatar_late {
            let s = sender.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(60), move || {
                s.input(MessageRowInput::SetAvatarShown(true));
            });
        }

        model
    }

    fn init_widgets(
        &mut self,
        _index: &DynamicIndex,
        root: Self::Root,
        _returned_widget: &gtk::ListBoxRow,
        sender: FactorySender<Self>,
    ) -> Self::Widgets {
        // The chips are children added by hand (their number varies), so the
        // box is built here and handed to the view; post_view keeps it fresh.
        let tags_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        self.render_tags(&tags_box);
        let widgets = view_output!();
        widgets
    }

    fn update(&mut self, msg: Self::Input, sender: FactorySender<Self>) {
        match msg {
            MessageRowInput::SetRead(read) => self.msg.unread = !read,
            MessageRowInput::SetKeywords(keywords) => self.msg.keywords = keywords,
            MessageRowInput::OpenTagMenu(btn) => {
                let tags = self.tags.borrow().clone();
                let msg = self.msg.clone();
                let target = msg.clone();
                let entries = tag_menu_entries(&tags, &msg, move |keyword, add| {
                    let _ = sender.output(MessageRowOutput::SetTag {
                        message: Box::new(target.clone()),
                        keyword,
                        add,
                    });
                });
                show_context_menu(&btn, (btn.width() / 2) as f64, btn.height() as f64, vec![entries]);
            }
            MessageRowInput::OpenMoveMenu(btn) => {
                // Under the button's middle, in window coordinates: the
                // picker is anchored on the window by the app.
                let point = btn.root().and_then(|root| {
                    let root: gtk::Widget = root.upcast();
                    btn.compute_point(
                        &root,
                        &gtk::graphene::Point::new(btn.width() as f32 / 2.0, btn.height() as f32),
                    )
                });
                let (x, y) = point.map_or((0.0, 0.0), |p| (p.x() as f64, p.y() as f64));
                let _ = sender.output(MessageRowOutput::MoveTo {
                    message: Box::new(self.msg.clone()),
                    x,
                    y,
                });
            }
            MessageRowInput::SetStarred(starred) => self.msg.starred = starred,
            MessageRowInput::SetHasAttachment(has) => self.msg.has_attachment = has,
            MessageRowInput::SetRowHover(over) => {
                self.row_hovered = over;
                // Hover mode: the palette slides open by itself on the row,
                // and arms the usual collapse timeout on leave.
                if self.palette_hover.get() {
                    if over {
                        if !self.palette_open {
                            self.palette_open = true;
                            let _ = sender.output(MessageRowOutput::PaletteOpened(
                                self.index.current_index(),
                            ));
                        }
                        self.cancel_collapse();
                    } else if self.palette_open {
                        self.arm_collapse(&sender);
                    }
                }
            }
            MessageRowInput::TogglePalette => {
                if self.palette_open {
                    self.palette_open = false;
                    self.cancel_collapse();
                } else {
                    self.palette_open = true;
                    // Persist briefly; moving onto the palette cancels this.
                    self.arm_collapse(&sender);
                    // One palette at a time: the list folds the others.
                    let _ = sender
                        .output(MessageRowOutput::PaletteOpened(self.index.current_index()));
                }
            }
            MessageRowInput::SetAvatarShown(on) => self.avatar_shown = on,
            MessageRowInput::SetPreviewLines(lines) => self.preview_lines = lines.clamp(1, 3),
            MessageRowInput::ClosePalette => {
                if self.palette_open {
                    self.palette_open = false;
                    self.cancel_collapse();
                }
            }
            MessageRowInput::PaletteEnter => self.cancel_collapse(),
            MessageRowInput::PaletteLeave => {
                if self.palette_open {
                    self.arm_collapse(&sender);
                }
            }
            MessageRowInput::CollapsePalette => {
                self.collapse_timer = None;
                self.palette_open = false;
            }
            MessageRowInput::Action(action) => {
                let _ = sender.output(MessageRowOutput::Action {
                    action,
                    message: Box::new(self.msg.clone()),
                });
            }
            MessageRowInput::ToggleThreadClicked => {
                if let Some(key) = self.thread_key.clone() {
                    let _ = sender.output(MessageRowOutput::ToggleThread(key));
                }
            }
            MessageRowInput::SetThreadUnread(unread) => self.thread_unread = unread,
            MessageRowInput::SetThreadStarred(starred) => self.thread_starred = starred,
            MessageRowInput::SetRevealed(revealed) => self.revealed = revealed,
            MessageRowInput::SetThreadExpanded(expanded) => self.thread_expanded = expanded,
            MessageRowInput::SwipeUpdate(offset) => {
                // A row already flying out ignores further gesture events —
                // its exit owns the surface until the action lands.
                if self.swipe_committing {
                    return;
                }
                self.swipe_dragging = true;
                self.swipe_active = true;
                self.swipe_progress = offset.clamp(-SWIPE_MAX, SWIPE_MAX);
                if self.swipe_progress != 0.0 {
                    self.swipe_side = if self.swipe_progress < 0.0 { -1 } else { 1 };
                }
            }
            MessageRowInput::SwipePrefsChanged => {}
            MessageRowInput::SwipeSettled => {
                // Ignored if a new drag started before the old snap-back
                // finished — that drag owns the state now.
                if !self.swipe_dragging && self.swipe_progress == 0.0 {
                    self.swipe_active = false;
                }
            }
            MessageRowInput::SwipeEnd => {
                self.swipe_dragging = false;
                if self.swipe_progress.abs() >= SWIPE_ARM {
                    self.commit_swipe();
                } else {
                    self.swipe_progress = 0.0;
                }
            }
            MessageRowInput::SwipeCommitted(action) => {
                if self.swipe_committing {
                    sender.input(MessageRowInput::Action(action));
                    // Most actions take the row with them, so this timer
                    // usually fires into a component that is already gone —
                    // hence the fallible sender (`input` would panic).
                    let tx = sender.input_sender().clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(SWIPE_RESTORE_MS),
                        move || {
                            let _ = tx.send(MessageRowInput::SwipeRestore);
                        },
                    );
                }
            }
            MessageRowInput::SwipeRestore => {
                if self.swipe_committing {
                    // Still here: the action did not remove the row, so it
                    // slides back in and the Revealer reopens.
                    self.swipe_committing = false;
                    self.swipe_exit_started.set(false);
                    self.swipe_progress = 0.0;
                    self.revealed = true;
                }
            }
        }
    }

    fn update_cmd(&mut self, cmd: Self::CommandOutput, sender: FactorySender<Self>) {
        match cmd {
            FaceCmd::Avatar { email, generation, mode, outcome, logo } => {
                // Record what came back before deciding what to draw — the
                // caches are shared, so the sender's other rows benefit even
                // when this row has been recycled to a different message.
                let retry_stale = crate::avatar::cache_result(&email, generation, mode, outcome);
                match logo {
                    Some(Some(bytes)) => {
                        crate::logo::decode_and_cache(&email, &bytes);
                    }
                    Some(None) => crate::logo::remember_missing(&email),
                    None => {}
                }
                if !self.face_email().eq_ignore_ascii_case(&email) {
                    return;
                }
                match crate::avatar::lookup(&email, self.gravatar) {
                    crate::avatar::CacheLookup::Texture(texture) => {
                        self.avatar_texture = Some(texture);
                    }
                    crate::avatar::CacheLookup::Missing => self.load_logo(&email, &sender),
                    crate::avatar::CacheLookup::Fetch { generation, mode } => {
                        self.avatar_texture = None;
                        // Only chase a result the EDS generation invalidated;
                        // a transient Gravatar failure waits for a later render.
                        if retry_stale {
                            let want_logo =
                                self.sender_logos && !crate::logo::known_missing(&email);
                            sender.oneshot_command(find_face(email, generation, mode, want_logo));
                        }
                    }
                }
            }
            FaceCmd::Logo { email, bytes } => {
                let texture = match bytes {
                    Some(bytes) => crate::logo::decode_and_cache(&email, &bytes),
                    None => {
                        // Remember the miss, so the sender's other rows and the
                        // next sync don't ask the same domain again.
                        crate::logo::remember_missing(&email);
                        None
                    }
                };
                if self.face_email().eq_ignore_ascii_case(&email) && self.sender_logos {
                    self.avatar_texture = texture;
                }
            }
        }
    }

    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
        // Cancel a pending collapse timer so it can't fire into this (now dropped)
        // component's shut-down runtime when the row is removed during a rebuild.
        self.cancel_collapse();
    }
}

/// Fire `DayChanged` shortly after the next local midnight (re-armed each time).
fn schedule_midnight_refresh(sender: &ComponentSender<MessageList>) {
    use chrono::Timelike;
    let secs = 86_400u32
        .saturating_sub(chrono::Local::now().num_seconds_from_midnight())
        .saturating_add(2);
    let input = sender.input_sender().clone();
    gtk::glib::timeout_add_seconds_local(secs, move || {
        let _ = input.send(MessageListInput::DayChanged);
        gtk::glib::ControlFlow::Break
    });
}

impl MessageRow {
    /// Cancel any pending auto-collapse (e.g. the cursor is now over the palette).
    fn cancel_collapse(&mut self) {
        if let Some(id) = self.collapse_timer.take() {
            id.remove();
        }
    }

    /// (Re)start the auto-collapse countdown from the shared preference (min 1s).
    fn arm_collapse(&mut self, sender: &FactorySender<Self>) {
        self.cancel_collapse();
        let secs = self.palette_collapse_secs.get().max(1);
        // Fallible send: nothing removes this timer when the row is dropped
        // (a list rebuild while a palette is open), and `input` aborts the
        // process on a shut-down runtime rather than returning an error.
        let tx = sender.input_sender().clone();
        self.collapse_timer = Some(gtk::glib::timeout_add_seconds_local_once(
            secs as u32,
            move || {
                let _ = tx.send(MessageRowInput::CollapsePalette);
            },
        ));
    }

    /// Chevron classes: shown (`revealed`) while the row is hovered or the palette
    /// is open; a CSS opacity transition fades it in/out.
    fn chevron_classes(&self) -> Vec<&'static str> {
        let mut v = vec!["flat", "palette-toggle"];
        if self.row_hovered || self.palette_open {
            v.push("revealed");
        }
        v
    }

    fn ring_classes(&self) -> Vec<&str> {
        match &self.ring_class {
            Some(c) => vec![c.as_str()],
            None => Vec::new(),
        }
    }

    /// What a swipe to the left currently does (preference: Delete unless
    /// the sides are swapped).
    fn swipe_left_action(&self) -> RowAction {
        if self.swipe_reversed.get() {
            RowAction::Archive
        } else {
            RowAction::Delete
        }
    }

    /// What a swipe to the right currently does.
    fn swipe_right_action(&self) -> RowAction {
        if self.swipe_reversed.get() {
            RowAction::Delete
        } else {
            RowAction::Archive
        }
    }

    /// The action the indicator panel is currently showing — the last side
    /// it grew from, so it stays put while a release's snap-back shrinks it.
    fn swipe_action(&self) -> RowAction {
        if self.swipe_side < 0 {
            self.swipe_left_action()
        } else {
            self.swipe_right_action()
        }
    }

    /// The panel hugs the edge the swipe is dragging toward: right (End) for
    /// a left drag, left (Start) for a right drag — Gmail's own reveal side.
    fn swipe_halign(&self) -> gtk::Align {
        if self.swipe_side < 0 {
            gtk::Align::End
        } else {
            gtk::Align::Start
        }
    }

    fn swipe_icon(&self) -> &'static str {
        match self.swipe_action() {
            RowAction::Delete => "co.hyprlab.Hylki-user-trash-symbolic",
            _ => "co.hyprlab.Hylki-mail-archive-symbolic",
        }
    }

    fn swipe_label(&self) -> String {
        match self.swipe_action() {
            RowAction::Delete => i18n("Delete"),
            _ => i18n("Archive"),
        }
    }

    /// A released swipe that cleared the commit distance (#swipe): the row
    /// flies out the side it was dragged to while its Revealer closes over
    /// the same 200ms, and the action fires as the two land. The strip stays
    /// pinned at full commit for the whole exit, so the colour and icon it
    /// leaves under are the ones the release chose.
    fn commit_swipe(&mut self) {
        self.swipe_committing = true;
        self.swipe_active = true;
        self.swipe_progress = if self.swipe_side < 0 { -SWIPE_MAX } else { SWIPE_MAX };
        // post_view starts the exit and hangs the action off its landing.
        self.revealed = false;
    }

    /// The indicator panel's classes: coloured for whichever action is
    /// active, and "armed" once the drag has cleared the commit distance —
    /// full colour says a release now fires it, matching Gmail's own cue.
    fn swipe_indicator_classes(&self) -> Vec<&'static str> {
        let mut v = vec!["swipe-indicator"];
        v.push(match self.swipe_action() {
            RowAction::Delete => "swipe-delete",
            _ => "swipe-archive",
        });
        if self.swipe_progress.abs() >= SWIPE_ARM {
            v.push("armed");
        }
        v
    }

    /// The row's name line: the sender — or, in a Sent folder, who the message
    /// went to, since every sender there is you (#27).
    fn name_line(&self) -> String {
        if !self.show_recipient {
            // A thread head surfaces its NEWEST member's sender.
            if let Some((name, _)) = &self.thread_from {
                return name.clone();
            }
            return self.msg.from_name.clone();
        }
        let names = recipient_names(&self.msg.to);
        if names.is_empty() {
            self.msg.from_name.clone()
        } else {
            format!("To: {names}")
        }
    }

    /// What the avatar's initials (and face lookups) key on: the first
    /// recipient in a Sent folder, the sender everywhere else.
    fn face_name(&self) -> String {
        if self.show_recipient {
            let names = recipient_names(&self.msg.to);
            if let Some(first) = names.split(',').next().map(str::trim) {
                if !first.is_empty() {
                    return first.to_string();
                }
            }
        }
        if let Some((name, _)) = &self.thread_from {
            return name.clone();
        }
        self.msg.from_name.clone()
    }

    /// The address face lookups run against — the first recipient's in a Sent
    /// folder, so the circle shows who the mail went to.
    fn face_email(&self) -> String {
        if self.show_recipient {
            if let Some(addr) = first_recipient_addr(&self.msg.to) {
                return addr;
            }
        }
        if let Some((_, addr)) = &self.thread_from {
            return addr.clone();
        }
        self.msg.from_addr.clone()
    }

    /// What the avatar circle shows: the sender's picture when one is known,
    /// else their initials drawn ink-centred (see `ui::initials`), which
    /// replaces the avatar's own label. The same paintable is returned for
    /// the same name, so the avatar sees no change between refreshes.
    fn avatar_image(&self) -> Option<gtk::gdk::Paintable> {
        if let Some(tex) = &self.avatar_texture {
            return Some(tex.clone().upcast());
        }
        let mut slot = self.initials_image.borrow_mut();
        // A message from one of your own mailboxes wears that mailbox's emoji
        // on its colour (#189), the same face the sidebar circle shows. It
        // shares the slot with the initials, keyed by what it draws rather
        // than by a name, so either way the avatar is handed the same object
        // on every refresh.
        if let Some((emoji, color)) = crate::avatar::own_face(&self.face_email())
            .and_then(|face| face.emoji.map(|emoji| (emoji, face.color)))
        {
            let key = format!("{emoji}\u{1}{color}");
            if slot.as_ref().is_none_or(|(n, _)| *n != key) {
                let bg = gtk::gdk::RGBA::parse(&color).unwrap_or(gtk::gdk::RGBA::BLACK);
                let fg = gtk::gdk::RGBA::parse(crate::color::readable_text(&color))
                    .unwrap_or(gtk::gdk::RGBA::WHITE);
                let face = crate::ui::initials::InitialsPaintable::solid(&emoji, bg, fg, 0.55);
                *slot = Some((key, face));
            }
            return slot.as_ref().map(|(_, p)| p.clone().upcast());
        }
        let name = self.face_name();
        if slot.as_ref().is_none_or(|(n, _)| *n != name) {
            *slot = crate::ui::initials::InitialsPaintable::for_name(&name).map(|p| (name.clone(), p));
        }
        slot.as_ref().map(|(_, p)| p.clone().upcast())
    }

    /// Fill the circle: a cached face if one is known, otherwise go and look.
    /// The chain is your own mailbox's picture (#189) → contact photo →
    /// Gravatar → domain icon → initials, each tier consulted only while its
    /// switch is on.
    fn load_face(&mut self, sender: &FactorySender<Self>) {
        let email = self.face_email();
        if email.is_empty() {
            return;
        }
        // A mailbox of your own with a face of its own (#189): that is what
        // the circle shows, ahead of any contact photo or domain icon. Its
        // own Gravatar leads when the account asked for one (the app looks it
        // up once a session); otherwise the picture, then the emoji, which
        // `avatar_image` draws.
        if let Some(face) = crate::avatar::own_face(&email) {
            self.avatar_texture = face
                .gravatar
                .then(|| crate::avatar::own_gravatar(&email))
                .flatten()
                .or_else(|| {
                    face.picture.as_deref().and_then(crate::ui::initials::avatar_texture)
                });
            return;
        }
        match crate::avatar::lookup(&email, self.gravatar) {
            crate::avatar::CacheLookup::Texture(texture) => {
                self.avatar_texture = Some(texture);
            }
            // Contact and Gravatar are definitively absent — the logo tier is
            // all that's left before initials.
            crate::avatar::CacheLookup::Missing => self.load_logo(&email, sender),
            crate::avatar::CacheLookup::Fetch { generation, mode } => {
                let want_logo = self.sender_logos && !crate::logo::known_missing(&email);
                sender.oneshot_command(find_face(email, generation, mode, want_logo));
            }
        }
    }

    /// The logo tier: only consulted when enabled, so switching "sender logos"
    /// off hides already-cached logos immediately. A domain already asked about
    /// is not asked again — one request a session, not one a row.
    fn load_logo(&mut self, email: &str, sender: &FactorySender<Self>) {
        if !self.sender_logos {
            self.avatar_texture = None;
            return;
        }
        if let Some(tex) = crate::logo::cached(email) {
            self.avatar_texture = Some(tex);
            // A week-old stored icon still shows, but this new message from
            // the sender is the cue to look for a fresh one in the background.
            if crate::logo::wants_refresh(email) {
                sender.oneshot_command(find_logo(email.to_string()));
            }
            return;
        }
        self.avatar_texture = None;
        if crate::logo::known_missing(email) {
            return;
        }
        sender.oneshot_command(find_logo(email.to_string()));
    }

    /// Classes for the row's content box. Without the avatar the unread
    /// dot becomes the row's first element, and the wide inset that kept the
    /// circle clear of the list's edge would leave the dot lopsided — sitting
    /// twice as far from the edge as from the text beside it.
    fn content_css(&self) -> Vec<&'static str> {
        let mut v = if self.avatars {
            vec!["message-row"]
        } else {
            vec!["message-row", "no-avatar"]
        };
        // Without the palette line the pill gets extra breathing room instead
        // (see `.message-row.no-palette` in the stylesheet).
        if !self.show_palette {
            v.push("no-palette");
        }
        // Previews off leaves a two-line row: too short for the ⋯ to clear
        // the avatar's bottom edge — reserve just enough height that
        // they never collide (see `.message-row.palette-room`).
        if self.show_palette && self.avatars && self.preview_lines == 0 {
            v.push("palette-room");
        }
        v
    }

    /// Row classes: unread highlight plus a `thread-child` indent for replies.
    /// A thread head with unread messages anywhere in its conversation gets the
    /// heavier `thread-unread` highlight until every one of them is read.
    fn row_css(&self) -> Vec<&'static str> {
        let mut v = row_classes(&self.msg);
        if self.thread_unread {
            v.push("thread-unread");
        }
        if self.is_thread_child {
            v.push("thread-child");
        }
        if self.is_last_child {
            v.push("thread-last");
        }
        if self.swipe_active {
            v.push("swiping");
        }
        v
    }

    /// Unread for display purposes: the message itself, or (on a thread head)
    /// any message hidden in its conversation.
    fn display_unread(&self) -> bool {
        self.msg.unread || self.thread_unread
    }

    fn sender_classes(&self) -> Vec<&'static str> {
        if self.display_unread() {
            vec!["message-sender", "unread"]
        } else {
            vec!["message-sender"]
        }
    }

    fn subject_classes(&self) -> Vec<&'static str> {
        if self.display_unread() {
            vec!["message-subject", "unread"]
        } else {
            vec!["message-subject"]
        }
    }
}

/// Order two messages by the chosen sort (ties fall back to date).
fn message_cmp(a: &Message, b: &Message, order: SortOrder) -> std::cmp::Ordering {
    let name_of = |m: &Message| {
        if m.from_name.trim().is_empty() {
            m.from_addr.to_lowercase()
        } else {
            m.from_name.to_lowercase()
        }
    };
    match order {
        SortOrder::DateNewest => b.timestamp.cmp(&a.timestamp),
        SortOrder::DateOldest => a.timestamp.cmp(&b.timestamp),
        SortOrder::Sender => name_of(a).cmp(&name_of(b)).then(b.timestamp.cmp(&a.timestamp)),
        SortOrder::Subject => normalize_subject(&a.subject)
            .cmp(&normalize_subject(&b.subject))
            .then(b.timestamp.cmp(&a.timestamp)),
        // `true` sorts after `false`, so compare b-vs-a to put unread/flagged first.
        SortOrder::UnreadFirst => b.unread.cmp(&a.unread).then(b.timestamp.cmp(&a.timestamp)),
        SortOrder::FlaggedFirst => b.starred.cmp(&a.starred).then(b.timestamp.cmp(&a.timestamp)),
    }
}

/// The conversation key a message belongs to: its owning account plus the
/// subject with reply/forward prefixes stripped. Messages with no subject get a
/// per-message key (by UID) so they never group together.
/// Lower-case the subject and strip any leading run of reply/forward prefixes
/// (`Re:`, `Fwd:`, …) so subject-sorting keeps a topic and its replies adjacent.
fn normalize_subject(subject: &str) -> String {
    const PREFIXES: &[&str] = &["re:", "fwd:", "fw:", "aw:", "sv:", "antw:", "wg:"];
    let mut s = subject.trim();
    loop {
        let lower = s.to_ascii_lowercase();
        let mut stripped = false;
        for p in PREFIXES {
            if lower.starts_with(p) {
                s = s[p.len()..].trim_start();
                stripped = true;
                break;
            }
        }
        if !stripped {
            break;
        }
    }
    s.trim().to_ascii_lowercase()
}

/// Which shown row stands for a reader key: the message's own row when the
/// list has one, and otherwise the row of the conversation it belongs to.
///
/// With expandable conversations off, a reply never gets a row of its own and
/// never will — the thread is only ever the one head row here. Without this
/// fallback the reader's selection would find nothing to select, and the
/// conversation would appear to deselect itself the moment one of its other
/// messages was clicked in the reading pane (#211). The head row stands for
/// the whole thread, so it is what stays lit however the user moves through
/// the cards.
///
/// A conversation reaches across folders, so some of its cards — the user's
/// own replies, pulled in from Sent — belong to no row in this folder at all
/// and are not in `msg_thread` either. `viewed` is the row the open
/// conversation was opened from, and `emitted` is that conversation as it was
/// handed to the reader: a card from it keeps that row lit.
fn row_for_reader_key(
    key: &(u32, u32),
    shown: &[Message],
    msg_thread: &std::collections::HashMap<(u32, u32), (u32, String)>,
    emitted: &[(u32, u32)],
    viewed: Option<(u32, u32)>,
) -> Option<usize> {
    let own_row = shown.iter().position(|m| (m.account_id, m.id) == *key);
    let thread_row = || {
        let tkey = msg_thread.get(key)?;
        shown
            .iter()
            .position(|m| msg_thread.get(&(m.account_id, m.id)) == Some(tkey))
    };
    let viewed_row = || {
        if !emitted.contains(key) {
            return None;
        }
        let viewed = viewed?;
        shown.iter().position(|m| (m.account_id, m.id) == viewed)
    };
    own_row.or_else(thread_row).or_else(viewed_row)
}

/// The conversation the reader is showing, for [`row_for_reader_key`]: what
/// this list `emitted` when the row was opened, plus whatever the app has
/// `merged` into it since from other folders — the user's own replies from
/// Sent, which reach the reader under ids this list never listed (#220).
fn reader_conversation(emitted: &[(u32, u32)], merged: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut all = emitted.to_vec();
    all.extend(merged.iter().filter(|k| !emitted.contains(k)).copied());
    all
}

/// Whether the message at `key` is the row its conversation collapses to:
/// the oldest member among those the rows were grouped from, or a message
/// grouped with nothing there (#236).
///
/// Judged over `msg_thread` and `thread_members`, which the rebuild fills
/// from the rendered window, and not over everything the list holds. The
/// two differ exactly when a conversation's older messages sit past the
/// window: the row on screen is then the oldest member *shown*, while the
/// conversation's true head is further down, unrendered. Asked against the
/// whole list, that row failed the head test, was taken for a reply picked
/// out of an opened-up thread, and was shown alone in the reader, with no
/// look in the cache for the rest. The unified Inboxes hit this constantly:
/// several inboxes merged, newest first, put a conversation's start past
/// the rendered window within days, while one folder rarely does.
fn heads_its_row(
    key: (u32, u32),
    msg_thread: &std::collections::HashMap<(u32, u32), (u32, String)>,
    thread_members: &std::collections::HashMap<(u32, String), Vec<(u32, u32)>>,
) -> bool {
    match msg_thread.get(&key) {
        None => true,
        Some(thread) => thread_members.get(thread).and_then(|m| m.first()) == Some(&key),
    }
}

/// Which of the page's conversations still need their real size looked up
/// (#222): the ones nobody has asked the cache about yet.
///
/// Every rebuild runs this, and rebuilds are cheap and frequent — a sync, a
/// scroll, each keystroke of a search. Asking is not cheap: it is a scan of the
/// account's message index. So a thread is asked about once and remembered.
/// That is also what stops the loop, since the answer arrives as a rebuild:
/// with every thread on the page already asked about, the next pass has nothing
/// to send and the list settles.
fn unasked_threads(
    listed: &[(u32, String, Vec<String>)],
    asked: &std::collections::HashSet<(u32, String)>,
) -> Vec<(u32, String, Vec<String>)> {
    listed
        .iter()
        .filter(|(aid, root, _)| !asked.contains(&(*aid, root.clone())))
        .cloned()
        .collect()
}

/// Group messages into conversations by their reply headers (Message-ID linked
/// via In-Reply-To / References), scoped per account. Returns each message's
/// thread key `(account_id, root)`. Messages with no reply relationship get a
/// unique key (a thread of one) — so unrelated messages that merely share a
/// subject are never threaded together.
///
/// Age plays no part: a message threads because its headers say what it answers,
/// and those are indexed with every message. Grouping runs over the rendered
/// window, so covering the whole mailbox costs no more than covering a day of
/// it; what a conversation costs to *open* is bounded separately, by
/// `THREAD_MEMBER_LIMIT`.
fn compute_thread_keys(
    msgs: &[Message],
    links: &[(u32, String, String)],
) -> std::collections::HashMap<(u32, u32), (u32, String)> {
    use std::collections::HashMap;

    // Union-find over message-id nodes (namespaced by account).
    let mut parent: HashMap<String, String> = HashMap::new();
    fn find(parent: &mut HashMap<String, String>, x: &str) -> String {
        let mut cur = x.to_string();
        while let Some(p) = parent.get(&cur) {
            if p == &cur {
                break;
            }
            cur = p.clone();
        }
        cur
    }
    fn union(parent: &mut HashMap<String, String>, a: &str, b: &str) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            parent.insert(ra, rb);
        }
    }
    // A message with its own Message-ID is a real node; one without gets a unique
    // node keyed by uid so it only links through its references (if any).
    let self_node = |m: &Message| -> String {
        if m.message_id.is_empty() {
            format!("{}\u{0}uid{}", m.account_id, m.uid)
        } else {
            format!("{}\u{0}{}", m.account_id, m.message_id)
        }
    };

    for m in msgs {
        let sn = self_node(m);
        parent.entry(sn.clone()).or_insert_with(|| sn.clone());
        for r in m.references.split_whitespace() {
            let rn = format!("{}\u{0}{}", m.account_id, r);
            parent.entry(rn.clone()).or_insert_with(|| rn.clone());
            union(&mut parent, &sn, &rn);
        }
    }

    // Messages from elsewhere in the account contribute their links but never
    // appear: a reply in the Inbox and the one before it are two answers to the
    // same message in Sent, and without that message nothing says so.
    for (aid, id, refs) in links {
        let sn = format!("{aid}\u{0}{id}");
        parent.entry(sn.clone()).or_insert_with(|| sn.clone());
        for r in refs.split_whitespace() {
            let rn = format!("{aid}\u{0}{r}");
            parent.entry(rn.clone()).or_insert_with(|| rn.clone());
            union(&mut parent, &sn, &rn);
        }
    }

    let mut out = HashMap::new();
    for m in msgs {
        let root = find(&mut parent, &self_node(m));
        out.insert((m.account_id, m.id), (m.account_id, root));
    }
    out
}

/// Style classes for a row: highlight unread messages with a pale accent.
fn row_classes(m: &Message) -> Vec<&'static str> {
    if m.unread {
        vec!["message-unread"]
    } else {
        Vec::new()
    }
}

pub struct MessageList {
    rows: FactoryVecDeque<MessageRow>,
    /// The list's own input sender, for work it schedules on the main loop
    /// (idle-time row filling, coalesced rebuilds).
    input: relm4::Sender<MessageListInput>,
    /// Rows of the current page not built yet (in `shown` order after the
    /// built ones); an idle callback builds them a chunk at a time.
    pending_rows: std::collections::VecDeque<RowInit>,
    fill_scheduled: bool,
    /// Row lists a folder switch left behind, torn down a chunk at a time
    /// at idle: destroying a page of rows costs about as much as building
    /// one, and it need not happen before the new page shows.
    retired: Vec<FactoryVecDeque<MessageRow>>,
    retire_scheduled: bool,
    /// A per-row signature of the last page built (thread count, expandable,
    /// child, last, expanded, unread, starred), so growing the page can tell
    /// that its existing rows are unchanged and only append.
    row_sigs: Vec<(usize, bool, bool, bool, bool, bool, bool)>,
    /// A rebuild asked for and not yet run: `Some(preserve_scroll)`. Several
    /// arrivals in one main-loop pass (a folder's cached copy, its synced
    /// copy, fresh thread links, a view switch's flag changes) collapse into
    /// one rebuild instead of one each.
    rebuild_queued: Option<bool>,
    /// A `SelectAndLoad` that arrived while a rebuild was queued: the rows it
    /// must find are not built yet, so it waits for that rebuild and runs
    /// after it (a notification click follows the folder's list into the
    /// channel in the same pass, and the list is only built on the idle).
    pending_select: Option<(u32, u32)>,
    /// All messages for the current folder (full searchable index).
    all: Vec<Message>,
    /// Every folder's messages (all accounts), supplied by the app while a search
    /// is active, so `AllFolders` scope can filter across the whole mailbox. Empty
    /// when not searching.
    search_pool: Vec<Message>,
    /// Which messages the search field filters over.
    scope: SearchScope,
    /// The search field widget, kept so a folder switch can clear its text.
    search_entry: Option<gtk::SearchEntry>,
    /// The search toolbar is hidden until asked for (#102).
    search_open: bool,
    /// When the search closed itself (empty entry losing focus): the button
    /// click that caused that blur arrives right after and must not reopen.
    search_closed_at: Option<std::time::Instant>,
    /// Currently displayed (post-filter, capped) messages, aligned with rows.
    shown: Vec<Message>,
    /// Total messages matching the current filter (may exceed what's rendered).
    total_matches: usize,
    query: String,
    gravatar: bool,
    /// Lines of preview text per row (1–3), from Preferences.
    preview_lines: u32,
    /// Whether rows draw their subject line (Focus Mode can take it away).
    show_subject: bool,
    /// Whether the coloured avatars are drawn (#29).
    avatars: bool,
    /// The next rebuild draws the avatars folded away and slides them in
    /// (Focus Mode has just given them back).
    reveal_avatars_late: bool,
    /// Whether a sender's site icon may fill one (#30).
    sender_logos: bool,
    /// Tint each row by its account (used in the unified inbox view).
    colorize: bool,
    /// account_id → avatar colour, for tinting rows.
    account_colors: std::collections::HashMap<u32, String>,
    /// Display-wide provider with each account's pale row-tint rule.
    color_provider: gtk::CssProvider,
    /// Actions palette collapse delay (seconds), shared with every row.
    palette_collapse_secs: std::rc::Rc<std::cell::Cell<u64>>,
    /// Shared with every row: open the palette on row hover.
    palette_hover: std::rc::Rc<std::cell::Cell<bool>>,
    /// The tags (#71), shared with every row for its chips and tag menu.
    tags: std::rc::Rc<std::cell::RefCell<Vec<crate::config::Tag>>>,
    /// Shared with every row: swap the swipe-gesture sides (#swipe).
    swipe_reversed: std::rc::Rc<std::cell::Cell<bool>>,
    /// Shared with every row: whether swiping is on at all (#92).
    swipe_enabled: std::rc::Rc<std::cell::Cell<bool>>,
    /// Shared with every row: how sensitive a trackpad two-finger swipe is.
    swipe_sensitivity: std::rc::Rc<std::cell::Cell<f64>>,
    /// The message currently being viewed, kept selected across list rebuilds.
    /// Keyed by (account_id, id) since UIDs collide across accounts in the
    /// unified "All Inboxes" view.
    selected_id: Option<(u32, u32)>,
    /// (account, message-id, references) for mail in the account's *other*
    /// folders. A conversation is often joined through messages that aren't on
    /// screen — every reply in an Inbox answers something in Sent — so those
    /// links are needed to see that the replies belong together.
    thread_links: Vec<(u32, String, String)>,
    /// How big each conversation really is, counted across the account's other
    /// folders and handed down by the app (#222). The list can only see its own
    /// folder, so a thread whose replies live in Sent would otherwise wear a
    /// badge that undercounts it. Keyed by thread key, as `rebuild` groups them.
    thread_counts: std::collections::HashMap<(u32, String), usize>,
    /// The conversations the last rebuild put on screen, as
    /// `(account, thread root, the Message-IDs it is threaded by)` — what the
    /// app needs to look their real sizes up.
    listed_threads: Vec<(u32, String, Vec<String>)>,
    /// Which conversations have already been asked about. A rebuild runs on
    /// every keystroke of a search and on every sync, and each ask is a scan of
    /// the account's index — so a thread is asked about once and remembered,
    /// not re-asked whenever its row is redrawn. Cleared per account by
    /// [`MessageListInput::ForgetThreadCounts`] when that account's mail moves.
    asked_threads: std::collections::HashSet<(u32, String)>,
    /// The shown rows' (account, folder, uid, id) keys, handed to every row so a
    /// drag can carry the whole selection (#23).
    drag_keys: DragKeys,
    thread_drag: ThreadDragKeys,
    /// Every selected message key, so the whole selection survives list rebuilds
    /// (background syncs) until the user clicks away.
    selected_ids: Vec<(u32, u32)>,
    /// The conversation the last `Selected` carried, as keys: after a
    /// rebuild, a selected head whose conversation now holds more is
    /// reported (`ThreadGrew`) so the reader shows the new reply at once.
    emitted_thread: Vec<(u32, u32)>,
    /// Selection changes still expected from a reader-driven selection, and what
    /// that selection is. GTK reports each `select_row`/`unselect_all` separately
    /// and a rebuild adds more, so a single flag would be consumed by the first
    /// and let a later one re-open the message; only a change that matches what
    /// the reader asked for is suppressed, and anything else ends it at once.
    from_reader: u8,
    reader_keys: Vec<(u32, u32)>,
    /// How many rows are currently selected (drives the bulk-action bar).
    selection_count: usize,
    /// Which way the user last moved through the list: +1 down, -1 up.
    /// Deleting the viewed message advances in this direction (like Apple
    /// Mail): triaging downward selects the message below the deleted one,
    /// and after moving up the list, deletion selects the one above instead.
    /// Updated only by the user's own selection movement — the programmatic
    /// post-delete advance keeps its index and never flips it.
    nav_direction: i32,
    /// Conversation keys the user has toggled away from the default state
    /// (expanded when the default is collapsed, and vice versa).
    expanded_threads: std::collections::HashSet<(u32, String)>,
    /// Whether conversations start expanded (user preference; collapsed default).
    default_expanded: bool,
    /// The open folder is Sent: rows name recipients instead of senders (#27).
    show_recipient: bool,
    /// The list shows Trash or Junk, where menus offer "Move to Inbox" (#138).
    restorable: bool,
    /// The list shows Junk: "Not Spam" stands where "Mark as Spam" would.
    in_junk: bool,
    /// The list shows Drafts (a folder or the unified row): drafts are
    /// neither read nor unread, so the toggles are not offered.
    in_drafts: bool,
    /// Rendered thread membership: message key → conversation key, rebuilt with
    /// the rows. Lets a read-state change on a hidden reply refresh its head.
    msg_thread: std::collections::HashMap<(u32, u32), (u32, String)>,
    /// Conversation key → member message keys (multi-message threads only).
    thread_members: std::collections::HashMap<(u32, String), Vec<(u32, u32)>>,
    /// Messages actually rendered (after the render limit), independent of how
    /// many rows are visible once threads are collapsed.
    rendered_count: usize,
    /// The rows on screen were built for a look that has changed (avatars,
    /// logos, preview lines, date style…): the next rebuild must build
    /// them again even though the messages are the same. Without this the
    /// page-growing shortcut in `rebuild` keeps them as they are.
    rows_stale: bool,
    /// How many messages to render — grows by `RENDER_CAP` each time the user
    /// scrolls to the bottom (infinite scroll). Reset on folder switch / search.
    render_limit: usize,
    /// Whether the folder's background index is fully loaded. When false, more
    /// rows may still stream in, so hitting the bottom shows a loading spinner.
    index_complete: bool,
    /// Whether a SetMessages has arrived since the last SetLoading — gates the
    /// empty-folder placeholder so it never flashes during a folder switch.
    loaded: bool,
    /// Threads whose replies are sliding shut. The rows stay in `shown` until
    /// the paired timer fires and drops them — otherwise they'd simply vanish
    /// rather than animate away. (PR #79)
    collapsing_threads: std::collections::HashMap<(u32, String), gtk::glib::SourceId>,
    /// The list's scroller, kept so expand/collapse can preserve scroll position.
    scroller: Option<gtk::ScrolledWindow>,
    /// Current sort order for the list.
    sort: SortOrder,
    /// Quick filter (#97): show only unread messages.
    unread_only: bool,
    /// Quick filter: show only starred messages.
    starred_only: bool,
    /// The last count string sent to the header bar, to emit only on change.
    last_count: String,
    /// Group messages into conversation threads (user preference).
    threading: bool,
    /// Whether a conversation row may expand into its member rows. Off: the
    /// row keeps its count chip and chevron, but never opens — the thread is
    /// read through the reader's cards instead.
    thread_expansion: bool,
    /// Whether rows carry the actions palette line at all (preference).
    list_palette: bool,
}

/// How the message list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    DateNewest,
    DateOldest,
    Sender,
    Subject,
    UnreadFirst,
    FlaggedFirst,
}

impl SortOrder {
    pub fn from_key(key: &str) -> Self {
        match key {
            "date_oldest" => SortOrder::DateOldest,
            "sender" => SortOrder::Sender,
            "subject" => SortOrder::Subject,
            "unread" => SortOrder::UnreadFirst,
            "flagged" => SortOrder::FlaggedFirst,
            _ => SortOrder::DateNewest,
        }
    }
}

/// Which messages the search field filters over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    /// Every folder of every account (the merged `search_pool`).
    AllFolders,
    /// Only the folder currently shown (the local `all` index).
    ThisFolder,
}

/// A bulk action applied to every selected message at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkAction {
    MarkRead,
    MarkUnread,
    Flag,
    /// Remove the star from every selected/threaded message (the bulk bar
    /// itself only offers Flag; conversations need the inverse too).
    Unflag,
    Archive,
    Spam,
    /// The reverse of `Spam`, for a selection in Junk (#168).
    NotSpam,
    Delete,
    /// Back to the Inbox, for a selection in Trash or Junk (#138).
    MoveToInbox,
}

#[derive(Debug)]
pub enum MessageListInput {
    /// The header's quick filter (#97): scope the list to unread mail.
    SetUnreadOnly(bool),
    /// The header's starred quick filter.
    SetStarredOnly(bool),
    SetMessages { messages: Vec<Message> },
    /// Merge more indexed messages into the current list (background backfill),
    /// preserving the current search query and view.
    AppendMessages { messages: Vec<Message> },
    SetLoading,
    SetThreading(bool),
    /// Whether conversation rows may expand into their members in the list
    /// (the row keeps its chip and chevron either way).
    SetThreadExpansion(bool),
    /// Whether rows carry the actions palette line at all.
    SetListPalette(bool),
    /// Resolve what deleting the current selection means: a lone selected row
    /// that heads a conversation stands for the whole thread (output
    /// `DeleteThread`); anything else is an ordinary `Bulk` delete.
    ResolveDelete,
    /// Reply headers from the account's other folders, so a conversation joined
    /// through a message that isn't on screen still groups.
    SetThreadLinks(Vec<(u32, String, String)>),
    /// True conversation sizes from the cache, keyed by thread key (#222).
    SetThreadCounts(Vec<((u32, String), usize)>),
    /// This account's mail changed, so what was counted may no longer be the
    /// conversation: drop its counts and let the next rebuild ask again (#222).
    ForgetThreadCounts(u32),
    /// Whether conversations start expanded (true) or collapsed (false).
    SetThreadsExpanded(bool),
    SetGravatar(bool),
    /// The GNOME Contacts photo index changed (EDS sync, or the first load
    /// finished) — refresh visible circles without losing the scroll position.
    ContactPhotosChanged,
    /// The open folder is (or stopped being) a Sent folder — rows name the
    /// recipient there instead of the sender (#27).
    SetShowRecipient(bool),
    /// Show or hide the coloured avatars (#29).
    SetAvatars(bool),
    /// The avatars and preview lines together, as the settings and Focus
    /// Mode leave them. `animate` (a Focus Mode toggle) slides the avatars
    /// away before the rows are rebuilt without them, or builds them folded
    /// and slides them in.
    SetLook { avatars: bool, preview_lines: u32, subject: bool, animate: bool },
    /// The Focus Mode slide finished: rebuild the rows as they now are.
    LookSettled,
    /// Fill them with senders' own site icons, or stop (#30).
    SetSenderLogos(bool),
    /// The date or clock preference changed: every row's date is built with the
    /// row, so they are built again (#32).
    RefreshDates,
    /// How many lines of preview text each row shows (1–3).
    SetPreviewLines(u32),
    SetColorize(bool),
    /// The local day rolled over — re-render rows so "Today" stays accurate.
    DayChanged,
    SetAccountColors(std::collections::HashMap<u32, String>),
    Search(String),
    /// Change the search scope (all folders vs. the current folder).
    SetScope(SearchScope),
    /// Replace the cross-folder search pool (all folders, all accounts). Sent by
    /// the app when a search begins; cleared to empty when it ends.
    SetSearchPool(Vec<Message>),
    /// The reader's selection: mirror whichever of these the list has rows for,
    /// so everything that acts on the list's selection acts on the messages the
    /// user pointed at. The reader is already showing them, so this must not
    /// re-open anything. Messages the list cannot represent — a reply read in
    /// from Sent, which belongs to another folder — keep the row of the
    /// conversation they were read in with (#211, #220). `conversation` is that
    /// conversation as the reader has it, which is wider than what this list
    /// handed over: the app pulls the user's own replies in from Sent after
    /// the fact, under ids the list never sees.
    SelectFromReader { keys: Vec<(u32, u32)>, conversation: Vec<(u32, u32)> },
    /// The set of selected rows changed (single click, Ctrl/Shift multi-select).
    SelectionChanged,
    /// A row was activated (double-click / Enter): pop it out into its own window.
    RowActivated(i32),
    /// Apply a bulk action to every selected message.
    Bulk(BulkAction),
    /// Deselect everything.
    ClearSelection,
    /// Move the selection by `delta` rows (single-key j/k and the arrow keys).
    MoveSelection(i32),
    /// Add or remove the focused row from the selection, without opening it.
    ToggleSelection,
    /// Put keyboard focus on the list (so the arrow keys work again),
    /// deliberately scrolling back to the selected row — the "back to list"
    /// shortcut.
    FocusList,
    /// Housekeeping focus restore (e.g. after a compose window closes) that
    /// doesn't scroll the viewport, unlike `FocusList`.
    ReclaimFocus,
    /// Put the cursor in the search field.
    FocusSearch,
    /// Close and clear the search toolbar (#102): Esc, empty focus-out, or
    /// the header button while open.
    CloseSearch,
    /// Showcase staging: open row N's actions palette (screenshot hook only —
    /// see HYLKI_SHOWCASE_PALETTE in app.rs).
    DebugOpenPalette(usize),
    /// Showcase only (HYLKI_SHOWCASE_SWIPE): drive row `index` through a full
    /// swipe and release, so the commit exit can be caught in stills — there
    /// is no way to inject a real gesture on this desktop.
    DebugSwipe { index: usize, left: bool },
    /// A row's palette opened; fold every other row's.
    PaletteOpened(usize),
    /// Expand/collapse a conversation thread.
    ToggleThread((u32, String)),
    /// A collapsing thread's replies have finished sliding shut — drop them
    /// from the list for real (see `start_collapse_thread`).
    FinishCollapseThread((u32, String)),
    /// Change the list sort order.
    SetSort(SortOrder),
    MarkRead(u32),
    SetRead { id: u32, read: bool },
    /// A hover-palette action for a specific message (forwarded to the app).
    RowAction { action: RowAction, message: Box<Message> },
    SetStarred { id: u32, starred: bool },
    /// A message's keywords changed (a tag put on or taken off, #71).
    SetKeywords { id: u32, keywords: Vec<String> },
    /// The tag definitions changed: rows rebuild their chips.
    SetTags(Vec<crate::config::Tag>),
    /// A row's tag menu toggled a tag — passed up to the app.
    SetTagFor { message: Box<Message>, keyword: String, add: bool },
    /// Update a message's attachment indicator (e.g. clearing a false paperclip).
    SetHasAttachment { id: u32, has: bool },
    Remove(u32),
    /// Remove many messages in a single batch (bulk archive/delete/spam), so the
    /// list updates in one render pass instead of one per message.
    RemoveMany(Vec<u32>),
    /// Secondary-click at (x, y) in the list: open the context menu.
    ContextMenu { x: f64, y: f64 },
    /// Set the actions palette auto-collapse delay (seconds).
    SetPaletteCollapse(u64),
    /// Open the actions palette on row hover, without the ⋯ click.
    SetPaletteHover(bool),
    /// A row's ⋯, in menu mode: the row's menu at the button's corner.
    /// Swap the swipe-gesture sides (#swipe).
    SetSwipeReversed(bool),
    /// Turn the swipe gesture on or off (#92).
    SetSwipeEnabled(bool),
    /// How far a trackpad two-finger swipe has to travel to fire the action.
    SetSwipeSensitivity(f64),
    /// The list shows Trash or Junk: menus offer "Move to Inbox" (#138).
    SetRestorable(bool),
    /// The list shows Junk: "Not Spam" replaces "Mark as Spam" (#168).
    SetInJunk(bool),
    /// The list shows Drafts: read/unread toggles are withheld.
    SetInDrafts(bool),
    /// Folder switch: reset infinite-scroll paging back to the first page and
    /// scroll to the top (a plain `SetMessages` now preserves paging for refreshes).
    ResetPaging,
    /// The list was scrolled to the bottom — render the next page of messages.
    LoadMore,
    /// Whether the current folder's background index is fully loaded.
    SetIndexComplete(bool),
    /// Build the next chunk of the current page's rows (scheduled at idle).
    FillRows,
    /// Run the rebuild queued by [`MessageList::queue_rebuild`].
    RunQueuedRebuild,
    /// Tear down the next chunk of a retired row list (scheduled at idle).
    RetireRows,
    /// Mark which message is being viewed so it stays highlighted across
    /// rebuilds; `None` clears the selection (e.g. on folder switch).
    SetSelected(Option<u32>),
    /// Select a message by `(account_id, id)` AND load it in the reader — used to
    /// advance after the viewed message is removed by a background sync.
    SelectAndLoad((u32, u32)),
    /// A row palette's Move to…: open the picker for that row (and, for a
    /// conversation head, its thread) at window point (x, y).
    RowMoveTo { message: Box<Message>, x: f64, y: f64 },
}

#[derive(Debug)]
pub enum MessageListOutput {
    /// A message was selected. `thread` holds the whole conversation (newest
    /// first) when the newest/head row was chosen, so the reader can show it as a
    /// scrollable conversation; otherwise it's just `[message]`.
    /// A message was opened. `thread` is the conversation to render (just
    /// `message` when it is not one). `solo` marks the deliberate case: the user
    /// picked one reply *inside* a conversation shown in the list, and wants
    /// only that message — so the reader must not go looking for its siblings.
    Selected { message: Message, thread: Vec<Message>, solo: bool },
    /// Every selected message, whenever that changes — the reader outlines the
    /// matching cards.
    SelectionKeys(Vec<(u32, u32)>),
    /// The header-bar count ("N" / "N of M") changed — app.rs shows it.
    CountChanged(String),
    /// A row was double-clicked: open it in its own window. `thread` is the
    /// whole conversation when the row heads one (same shape as `Selected`),
    /// so the window shows every card — otherwise just `[message]`.
    Activated { message: Message, thread: Vec<Message> },
    /// A context-menu or palette action chosen for a specific message.
    /// `conversation` holds the thread when the row stands for a collapsed
    /// conversation rather than for `message` alone, so a reply started there
    /// can answer the conversation instead of the head it is filed under
    /// (#210). Empty for an ordinary row, and for a head row shown alongside
    /// its expanded replies — there the row means only itself.
    Action { action: RowAction, message: Box<Message>, conversation: Vec<Message> },
    /// A tag toggled on a specific message (#71).
    SetTag { message: Box<Message>, keyword: String, add: bool },
    /// A bulk action chosen for every currently-selected message.
    Bulk { action: BulkAction, messages: Vec<Message> },
    /// "Move To…" from a row's menu: open the folder picker for `messages`
    /// at window point (`x`, `y`). With `offer_whole`, the first message is
    /// the clicked conversation row and the rest its members: the picker
    /// offers moving them all (#171), or just the first.
    MoveTo { messages: Vec<Message>, offer_whole: bool, x: f64, y: f64 },
    /// The selected conversation gained a member since it was opened (a
    /// reply synced in): the head and the whole conversation as it now is.
    ThreadGrew { message: Message, thread: Vec<Message> },
    /// The conversations now on screen and the Message-IDs each is threaded
    /// by, so the app can ask the cache how big they really are (#222). Sent
    /// only when the page's conversations actually change.
    ThreadsListed { groups: Vec<(u32, String, Vec<String>)> },
    /// Delete requested on a lone selected row that heads a whole conversation:
    /// every member of the thread, for the app to confirm and delete.
    DeleteThread { messages: Vec<Message> },
    /// The viewed message was removed and no row remains to advance to, so the
    /// reader should clear.
    SelectionCleared,
    /// The search field became active (non-empty) or inactive (empty), so the app
    /// can supply or drop the cross-folder search pool.
    SearchActive(bool),
}

#[relm4::component(pub)]
impl SimpleComponent for MessageList {
    type Init = ();
    type Input = MessageListInput;
    type Output = MessageListOutput;

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            add_css_class: "message-list-pane",

            gtk::Box {
                add_css_class: "list-toolbar",
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 8,

                // The folder name, count and sort control live in the pane's
                // header bar now (app.rs) — only search needs this toolbar,
                // and it stays hidden until asked for (#102).
                gtk::Revealer {
                    set_transition_type: gtk::RevealerTransitionType::SlideDown,
                    #[watch]
                    set_reveal_child: model.search_open,

                gtk::Box {
                    set_spacing: 6,
                    add_css_class: "list-search-row",

                    #[name = "search_entry"]
                    gtk::SearchEntry {
                        set_hexpand: true,
                        // Its own default minimum is wider than the rows need.
                        set_width_chars: 3,
                        #[watch]
                        set_placeholder_text: Some(model.search_placeholder().as_str()),
                        connect_search_changed[sender] => move |entry| {
                            sender.input(MessageListInput::Search(entry.text().to_string()));
                        },
                        // Esc closes (SearchEntry's own stop signal).
                        connect_stop_search[sender] => move |_| {
                            sender.input(MessageListInput::CloseSearch);
                        },
                        // Leaving an empty entry closes too.
                        add_controller = gtk::EventControllerFocus {
                            connect_leave[sender] => move |ctl| {
                                let empty = ctl
                                    .widget()
                                    .and_downcast_ref::<gtk::SearchEntry>()
                                    .is_some_and(|e| e.text().trim().is_empty());
                                if empty {
                                    sender.input(MessageListInput::CloseSearch);
                                }
                            },
                        },
                    },

                    // Scope picker: 0 = All folders (default), 1 = This folder.
                    #[name = "scope_dropdown"]
                    gtk::DropDown {
                        set_valign: gtk::Align::Center,
                        set_tooltip_text: Some(i18n("Choose which folders to search").as_str()),
                        set_model: Some(&gtk::StringList::new(&[i18n("All folders").as_str(), i18n("This folder").as_str()])),
                        set_selected: 0,
                        connect_selected_notify[sender] => move |dd| {
                            let scope = if dd.selected() == 0 {
                                SearchScope::AllFolders
                            } else {
                                SearchScope::ThisFolder
                            };
                            sender.input(MessageListInput::SetScope(scope));
                        },
                    },
                },
                },
            },

            // Bulk-action bar, revealed while more than one message is selected.
            gtk::Revealer {
                set_transition_type: gtk::RevealerTransitionType::SlideDown,
                #[watch]
                set_reveal_child: model.selection_count > 1,

                // A revealer sliding *down* still reserves its child's width while
                // collapsed, so this bar of seven buttons was setting the whole
                // pane's minimum width — 340px — however narrow the rows became
                // (#29). Scrolled, its minimum is nothing and its natural size is
                // unchanged, so it only clips when the list is genuinely too narrow
                // to hold it.
                gtk::ScrolledWindow {
                    set_vscrollbar_policy: gtk::PolicyType::Never,
                    set_hscrollbar_policy: gtk::PolicyType::External,
                    set_propagate_natural_width: true,
                    set_propagate_natural_height: true,

                gtk::Box {
                    add_css_class: "bulk-bar",
                    set_spacing: 2,

                    gtk::Label {
                        #[watch]
                        set_label: &format!("{} selected", model.selection_count),
                        set_hexpand: true,
                        set_halign: gtk::Align::Start,
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        add_css_class: "bulk-count",
                    },
                    // Drafts are neither read nor unread: both go when the
                    // list shows them.
                    gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-mail-read-symbolic",
                        set_tooltip_text: Some(i18n("Mark as Read").as_str()),
                        add_css_class: "flat",
                        #[watch]
                        set_visible: !model.in_drafts,
                        connect_clicked => MessageListInput::Bulk(BulkAction::MarkRead),
                    },
                    gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-mail-unread-symbolic",
                        set_tooltip_text: Some(i18n("Mark as Unread").as_str()),
                        add_css_class: "flat",
                        #[watch]
                        set_visible: !model.in_drafts,
                        connect_clicked => MessageListInput::Bulk(BulkAction::MarkUnread),
                    },
                    gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-starred-symbolic",
                        set_tooltip_text: Some(i18n("Flag").as_str()),
                        add_css_class: "flat",
                        connect_clicked => MessageListInput::Bulk(BulkAction::Flag),
                    },
                    gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-mail-archive-symbolic",
                        set_tooltip_text: Some(i18n("Archive").as_str()),
                        add_css_class: "flat",
                        connect_clicked => MessageListInput::Bulk(BulkAction::Archive),
                    },
                    gtk::Button {
                        #[watch]
                        set_icon_name: if model.in_junk {
                            "co.hyprlab.Hylki-mail-mark-notjunk-symbolic"
                        } else {
                            "co.hyprlab.Hylki-mail-mark-junk-symbolic"
                        },
                        #[watch]
                        set_tooltip_text: Some(if model.in_junk { i18n("Not Spam") } else { i18n("Mark as Spam") }.as_str()),
                        add_css_class: "flat",
                        // Mapped to NotSpam in Junk by the Bulk handler.
                        connect_clicked => MessageListInput::Bulk(BulkAction::Spam),
                    },
                    gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-user-trash-symbolic",
                        set_tooltip_text: Some(i18n("Delete").as_str()),
                        add_css_class: "flat",
                        connect_clicked => MessageListInput::Bulk(BulkAction::Delete),
                    },
                    gtk::Separator {
                        set_orientation: gtk::Orientation::Vertical,
                    },
                    gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-edit-clear-symbolic",
                        set_tooltip_text: Some(i18n("Clear selection").as_str()),
                        add_css_class: "flat",
                        connect_clicked => MessageListInput::ClearSelection,
                    },
                },
                },
            },

            gtk::Overlay {
                #[wrap(Some)]
                #[name = "scroller"]
                set_child = &gtk::ScrolledWindow {
                set_vexpand: true,
                // External, not Never: with Never the widest row's minimum (the
                // actions palette reservation plus the avatar column) propagates
                // all the way up and becomes part of the window's minimum width,
                // which pushed it past half of a 1920px screen — at which point
                // GNOME refuses to tile the window to the left/right edge. Rows
                // ellipsize, so a narrow pane clips gracefully instead.
                set_hscrollbar_policy: gtk::PolicyType::External,
                // The pane's own floor, now that rows no longer set one: room
                // for a row's full actions palette (avatar + dot + the reserved
                // actions line), so opening the palette never needs to clip —
                // the narrow-window breakpoint rails the sidebar in time to
                // afford this even in a half-screen tile. (Grows by the thread
                // indent while a conversation is expanded — see the rebuild.)
                set_size_request: (LIST_MIN_WIDTH, -1),

                // Reaching the bottom pulls in the next page (and, if the index is
                // still loading, shows the spinner below until more arrive).
                connect_edge_reached[sender] => move |_, pos| {
                    if pos == gtk::PositionType::Bottom {
                        sender.input(MessageListInput::LoadMore);
                    }
                },

                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,

                    // Wired by `wire_list` (shared with the lists that
                    // replace this one on a folder switch — see
                    // `discard_rows`).
                    #[local_ref]
                    row_box -> gtk::ListBox {},

                    // Bottom loading indicator while the rest of the folder streams in.
                    gtk::Box {
                        add_css_class: "list-loading",
                        set_halign: gtk::Align::Center,
                        set_spacing: 8,
                        set_margin_top: 10,
                        set_margin_bottom: 14,
                        #[watch]
                        set_visible: model.is_loading_more(),

                        gtk::Spinner {
                            set_spinning: true,
                            set_width_request: 18,
                            set_height_request: 18,
                        },
                        gtk::Label {
                            set_label: &i18n("Loading more…"),
                            add_css_class: "dim-label",
                        },
                    },

                    // Placeholder when the folder has loaded and holds nothing.
                    // Same full-size AdwStatusPage styling as the reader's
                    // "No message selected", so the two placeholders match.
                    adw::StatusPage {
                        set_icon_name: Some("co.hyprlab.Hylki-mail-inbox-symbolic"),
                        set_title: &i18n("No Messages"),
                        set_description: Some(i18n("There's nothing here right now.").as_str()),
                        set_vexpand: true,
                        #[watch]
                        set_visible: model.is_empty_state(),
                    },
                },
                },

            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let rows = Self::new_rows(sender.input_sender());

        let color_provider = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &color_provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        let mut model = MessageList {
            rows,
            input: sender.input_sender().clone(),
            pending_rows: std::collections::VecDeque::new(),
            fill_scheduled: false,
            retired: Vec::new(),
            retire_scheduled: false,
            row_sigs: Vec::new(),
            rebuild_queued: None,
            pending_select: None,
            all: Vec::new(),
            search_pool: Vec::new(),
            scope: SearchScope::AllFolders,
            search_entry: None,
            search_open: false,
            search_closed_at: None,
            shown: Vec::new(),
            total_matches: 0,
            render_limit: RENDER_CAP,
            index_complete: true,
            loaded: false,
            collapsing_threads: std::collections::HashMap::new(),
            query: String::new(),
            gravatar: false,
            avatars: true,
            reveal_avatars_late: false,
            rows_stale: false,
            sender_logos: false,
            preview_lines: 1,
            show_subject: true,
            colorize: false,
            account_colors: std::collections::HashMap::new(),
            color_provider,
            tags: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
            palette_collapse_secs: std::rc::Rc::new(std::cell::Cell::new(5)),
            palette_hover: std::rc::Rc::new(std::cell::Cell::new(
                crate::config::load_list_palette_hover(),
            )),
            swipe_reversed: std::rc::Rc::new(std::cell::Cell::new(
                crate::config::load_swipe_reversed(),
            )),
            swipe_enabled: std::rc::Rc::new(std::cell::Cell::new(
                crate::config::load_swipe_enabled(),
            )),
            swipe_sensitivity: std::rc::Rc::new(std::cell::Cell::new(
                crate::config::load_swipe_sensitivity(),
            )),
            thread_links: Vec::new(),
            thread_counts: std::collections::HashMap::new(),
            listed_threads: Vec::new(),
            asked_threads: std::collections::HashSet::new(),
            drag_keys: DragKeys::default(),
            thread_drag: ThreadDragKeys::default(),
            selected_id: None,
            selected_ids: Vec::new(),
            emitted_thread: Vec::new(),
            selection_count: 0,
            nav_direction: 1,
            from_reader: 0,
            reader_keys: Vec::new(),
            expanded_threads: std::collections::HashSet::new(),
            show_recipient: false,
            restorable: false,
            in_junk: false,
            in_drafts: false,
            default_expanded: false,
            msg_thread: std::collections::HashMap::new(),
            thread_members: std::collections::HashMap::new(),
            rendered_count: 0,
            scroller: None,
            sort: SortOrder::DateNewest,
            unread_only: false,
            starred_only: false,
            last_count: String::new(),
            threading: true,
            thread_expansion: true,
            list_palette: true,

        };

        let row_box = model.rows.widget();
        Self::wire_list(row_box, sender.input_sender());

        let widgets = view_output!();
        model.scroller = Some(widgets.scroller.clone());
        model.search_entry = Some(widgets.search_entry.clone());

        // The scope picker sizes itself to its widest entry ("All folders"), which
        // made it — not the messages — the narrowest the list could ever be (#29).
        // An ellipsizing label on the button lets it give way; the drop-down list
        // keeps its own factory, so the choices are still spelled out in full.
        let button_factory = gtk::SignalListItemFactory::new();
        button_factory.connect_setup(|_, item| {
            let label = gtk::Label::new(None);
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                item.set_child(Some(&label));
            }
        });
        button_factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let text = item
                .item()
                .and_downcast::<gtk::StringObject>()
                .map(|s| s.string().to_string())
                .unwrap_or_default();
            if let Some(label) = item.child().and_downcast::<gtk::Label>() {
                label.set_label(&text);
            }
        });
        widgets.scope_dropdown.set_factory(Some(&button_factory));

        schedule_midnight_refresh(&sender);

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            MessageListInput::SetMessages { messages } => {
                let t = std::time::Instant::now();
                let n = messages.len();
                self.all = messages;
                self.loaded = true;
                // Keep any active search query: this also fires for a background
                // re-sync of the folder you're viewing, which shouldn't drop your
                // search. Folder switches clear the query via `ResetPaging` first.
                self.queue_rebuild(true);
                tracing::debug!("list: set {n} messages in {:?}", t.elapsed());
            }
            MessageListInput::AppendMessages { messages } => {
                // Grow the searchable index in place. Dedup by (account, uid) since
                // UIDs collide across accounts in the unified inbox.
                let existing: std::collections::HashSet<(u32, u32)> =
                    self.all.iter().map(|m| (m.account_id, m.uid)).collect();
                let before = self.all.len();
                for m in messages {
                    if !existing.contains(&(m.account_id, m.uid)) {
                        self.all.push(m);
                    }
                }
                // Re-render when it could change what's visible: an active search, a
                // sort where older messages can surface at the top, or the user is
                // waiting at the bottom for more rows to fill the raised limit.
                let waiting_for_more = self.render_limit > self.rendered_count;
                if self.all.len() != before
                    && (!self.query.is_empty()
                        || self.sort != SortOrder::DateNewest
                        || waiting_for_more)
                {
                    self.queue_rebuild(true);
                }
            }
            MessageListInput::SetLoading => {
                self.all.clear();
                self.loaded = false;
                self.clear_search();
                self.render_limit = RENDER_CAP;
                // Queued: when the folder's list follows in the same pass
                // (served from cache), the old rows are torn down once, for
                // the new ones, not first for nothing.
                self.queue_rebuild(false);
            }
            MessageListInput::ResetPaging => {
                // Folder switch: drop any active search, back to the first page,
                // scrolled to the top.
                self.clear_search();
                self.emitted_thread.clear();
                self.render_limit = RENDER_CAP;
                if let Some(s) = &self.scroller {
                    s.vadjustment().set_value(0.0);
                }
            }
            MessageListInput::LoadMore => {
                // Show more if the index already has more, or if it's still loading
                // (the spinner covers the wait, and appended rows fill in).
                if self.rendered_count < self.total_matches || !self.index_complete {
                    self.render_limit = self.render_limit.saturating_add(RENDER_CAP);
                    self.rebuild_preserving_scroll();
                }
            }
            MessageListInput::SetIndexComplete(complete) => {
                self.index_complete = complete;
            }
            MessageListInput::FillRows => {
                self.fill_scheduled = false;
                self.fill_rows(FILL_CHUNK);
            }
            MessageListInput::RetireRows => {
                self.retire_scheduled = false;
                self.retire_rows();
            }
            MessageListInput::RunQueuedRebuild => {
                if let Some(preserve) = self.rebuild_queued.take() {
                    if preserve {
                        self.rebuild_preserving_scroll();
                    } else {
                        self.rebuild();
                    }
                }
                // The rows exist now: run the selection that waited for them.
                if let Some(key) = self.pending_select.take() {
                    let _ = self.input.send(MessageListInput::SelectAndLoad(key));
                }
                self.report_thread_growth(&sender);
            }
            MessageListInput::SetThreadLinks(links) => {
                if self.thread_links != links {
                    self.thread_links = links;
                    if self.threading {
                        self.queue_rebuild(true);
                    }
                }
            }
            MessageListInput::SetThreadCounts(counts) => {
                // Counts arrive a beat after the page paints (the cache is the
                // worker's, not ours), so this is a rebuild rather than part of
                // one. Only the badge numbers move; the rows themselves, and
                // which conversations are listed, do not — which is what keeps
                // this from asking for counts again and looping.
                let mut changed = false;
                for (key, n) in counts {
                    if self.thread_counts.get(&key) != Some(&n) {
                        self.thread_counts.insert(key, n);
                        changed = true;
                    }
                }
                if changed && self.threading {
                    self.queue_rebuild(true);
                }
            }
            MessageListInput::ForgetThreadCounts(account_id) => {
                let before = self.thread_counts.len();
                self.thread_counts.retain(|(aid, _), _| *aid != account_id);
                self.asked_threads.retain(|(aid, _)| *aid != account_id);
                // Only rebuild if a badge actually loses its number; the
                // re-ask itself rides on the rebuild the new mail causes.
                if self.thread_counts.len() != before && self.threading {
                    self.queue_rebuild(true);
                }
            }
            MessageListInput::SetThreading(on) => {
                if self.threading != on {
                    self.threading = on;
                    self.rebuild();
                }
            }
            MessageListInput::SetThreadExpansion(on) => {
                if self.thread_expansion != on {
                    self.thread_expansion = on;
                    // Turning expansion off folds every open thread (the
                    // `expanded` computation ignores the stored toggles while
                    // off); turning it on restores them.
                    self.rebuild();
                }
            }
            MessageListInput::SetListPalette(on) => {
                if self.list_palette != on {
                    self.list_palette = on;
                    self.rebuild();
                }
            }
            MessageListInput::ResolveDelete => {
                // A lone selected row that heads a multi-message conversation
                // stands for the whole thread: hand every member to the app
                // (which confirms before deleting). Anything else — multiple
                // rows, a reply row, a plain message — is an ordinary bulk
                // delete of exactly what is selected.
                let selected: Vec<Message> = self
                    .rows
                    .widget()
                    .selected_rows()
                    .iter()
                    .filter_map(|r| self.shown.get(r.index() as usize).cloned())
                    .collect();
                if let [m] = selected.as_slice() {
                    let key = (m.account_id, m.id);
                    if let Some(tkey) = self.msg_thread.get(&key) {
                        let members = self.thread_members.get(tkey).cloned().unwrap_or_default();
                        if members.len() > 1 && members.first() == Some(&key) {
                            let messages: Vec<Message> = members
                                .iter()
                                .filter_map(|mk| {
                                    self.active_source()
                                        .iter()
                                        .find(|x| (x.account_id, x.id) == *mk)
                                        .cloned()
                                })
                                .collect();
                            let _ = sender
                                .output(MessageListOutput::DeleteThread { messages });
                            return;
                        }
                    }
                }
                sender.input(MessageListInput::Bulk(BulkAction::Delete));
            }
            MessageListInput::SetThreadsExpanded(on) => {
                if self.default_expanded != on {
                    self.default_expanded = on;
                    // Per-thread toggles were exceptions to the old default;
                    // drop them so everything follows the new one.
                    self.expanded_threads.clear();
                    self.rebuild();
                }
            }
            MessageListInput::RefreshDates => self.rebuild_rows_preserving_scroll(),
            MessageListInput::SetSenderLogos(on) => {
                if self.sender_logos != on {
                    self.sender_logos = on;
                    // The circle is filled when the row is built.
                    self.rebuild_rows_preserving_scroll();
                }
            }
            MessageListInput::SetLook { avatars, preview_lines, subject, animate } => {
                let preview_lines = preview_lines.min(3);
                let avatars_changed = self.avatars != avatars;
                let lines_changed = self.preview_lines != preview_lines;
                let subject_changed = self.show_subject != subject;
                if !avatars_changed && !lines_changed && !subject_changed {
                    return;
                }
                // The subject line is built with the row, so it can only come
                // or go in a rebuild; the slide below ends in one anyway.
                self.show_subject = subject;
                if animate && avatars_changed && !avatars {
                    // Slide every circle away (and the preview to its new
                    // height in place); the rebuild that takes the slot
                    // back follows once they have gone, so the rows it
                    // draws are the ones on screen.
                    self.avatars = false;
                    self.preview_lines = preview_lines;
                    for i in 0..self.rows.len() {
                        self.rows.send(i, MessageRowInput::SetAvatarShown(false));
                        if lines_changed {
                            self.rows.send(i, MessageRowInput::SetPreviewLines(preview_lines));
                        }
                    }
                    let s = sender.clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(u64::from(crate::ui::FOCUS_ANIM_MS) + 40),
                        move || s.input(MessageListInput::LookSettled),
                    );
                } else {
                    // Circles coming back are built folded and slide in.
                    self.reveal_avatars_late = animate && avatars_changed && avatars;
                    self.avatars = avatars;
                    self.preview_lines = preview_lines;
                    self.rebuild_rows_preserving_scroll();
                    self.reveal_avatars_late = false;
                }
            }
            MessageListInput::LookSettled => self.rebuild_rows_preserving_scroll(),
            MessageListInput::SetAvatars(on) => {
                if self.avatars != on {
                    self.avatars = on;
                    // The circle is built with the row, so the rows have to be
                    // built again for the width to come back.
                    self.rebuild_rows_preserving_scroll();
                }
            }
            MessageListInput::SetGravatar(on) => {
                if self.gravatar != on {
                    self.gravatar = on;
                    self.rebuild_rows();
                }
            }
            MessageListInput::SetShowRecipient(on) => {
                if self.show_recipient != on {
                    self.show_recipient = on;
                    self.rows_stale = true;
                    self.queue_rebuild(true);
                }
            }
            MessageListInput::SetRestorable(on) => self.restorable = on,
            MessageListInput::SetInJunk(on) => self.in_junk = on,
            MessageListInput::SetInDrafts(on) => self.in_drafts = on,
            MessageListInput::ContactPhotosChanged => {
                // Pointless when the circles aren't drawn; rows check the
                // fresh index as they are rebuilt.
                if self.avatars {
                    self.rebuild_rows_preserving_scroll();
                }
            }
            MessageListInput::SetPreviewLines(lines) => {
                let lines = lines.min(3);
                if self.preview_lines != lines {
                    self.preview_lines = lines;
                    // Row height is set when the row is built, so the list has to
                    // be rebuilt rather than nudged.
                    self.rebuild_rows();
                }
            }
            MessageListInput::SetColorize(on) => {
                if self.colorize != on {
                    self.colorize = on;
                    self.rows_stale = true;
                    self.queue_rebuild(true);
                }
            }
            MessageListInput::DayChanged => {
                // Re-render so relative labels like "Today" reflect the new date.
                self.rebuild_rows();
                schedule_midnight_refresh(&sender);
            }
            MessageListInput::SetAccountColors(colors) => {
                self.account_colors = colors;
                self.refresh_tint_css();
                // Existing rows keep their classes; the rule update reaches them.
            }
            MessageListInput::Search(q) => {
                let was_active = self.searching();
                self.query = q;
                let now_active = self.searching();
                // On the empty↔non-empty edge, tell the app to supply or drop the
                // cross-folder pool.
                if was_active != now_active {
                    let _ = sender.output(MessageListOutput::SearchActive(now_active));
                }
                self.render_limit = RENDER_CAP;
                self.rebuild();
            }
            MessageListInput::SetScope(scope) => {
                if self.scope != scope {
                    self.scope = scope;
                    // Scope only affects the view while a query is present.
                    if self.searching() {
                        self.render_limit = RENDER_CAP;
                        self.rebuild();
                    }
                }
            }
            MessageListInput::SetSearchPool(pool) => {
                self.search_pool = pool;
                if self.searching() && self.scope == SearchScope::AllFolders {
                    self.render_limit = RENDER_CAP;
                    self.rebuild_preserving_scroll();
                }
            }
            MessageListInput::SelectFromReader { keys, conversation } => {
                // An empty set means the reader cleared its card selection (a
                // click on the document's empty space). The list keeps the
                // viewed message highlighted rather than losing its anchor —
                // the focus-within CSS dims the highlight instead.
                let keys = if keys.is_empty() {
                    self.selected_id.into_iter().collect()
                } else {
                    keys
                };
                // The list shows only a thread's head while it is collapsed, so
                // selecting a reply has to open the thread first — otherwise
                // there is no row to select and only the head would ever answer.
                let hidden: Vec<(u32, String)> = keys
                    .iter()
                    .filter(|k| !self.shown.iter().any(|m| (m.account_id, m.id) == **k))
                    .filter_map(|k| self.msg_thread.get(k).cloned())
                    .collect();
                if !hidden.is_empty() && self.thread_expansion {
                    for thread_key in hidden {
                        // `expanded_threads` records the departure from the
                        // default, so which way to move it depends on that.
                        if self.default_expanded {
                            self.expanded_threads.remove(&thread_key);
                        } else {
                            self.expanded_threads.insert(thread_key);
                        }
                    }
                    self.rebuild_preserving_scroll();
                }
                // The conversation as the reader shows it: what this list
                // handed over plus what the app merged in from other folders
                // since (#220). Either way it was opened from the viewed row.
                let conversation = reader_conversation(&self.emitted_thread, &conversation);
                let list = self.rows.widget();
                list.unselect_all();
                for key in &keys {
                    if let Some(idx) = row_for_reader_key(
                        key,
                        &self.shown,
                        &self.msg_thread,
                        &conversation,
                        self.selected_id,
                    ) {
                        if let Some(row) = list.row_at_index(idx as i32) {
                            list.select_row(Some(&row));
                        }
                    }
                }
                // What the reader asked for, as this list can represent it, so
                // the changes GTK is about to report are recognised as ours
                // rather than the user's.
                self.reader_keys = list
                    .selected_rows()
                    .iter()
                    .filter_map(|r| self.shown.get(r.index() as usize).map(|m| (m.account_id, m.id)))
                    .collect();
                self.from_reader = 8;
                sender.input(MessageListInput::SelectionChanged);
            }
            MessageListInput::SelectionChanged => {
                let keys: Vec<(u32, u32)> = self
                    .rows
                    .widget()
                    .selected_rows()
                    .iter()
                    .filter_map(|r| self.shown.get(r.index() as usize).map(|m| (m.account_id, m.id)))
                    .collect();
                self.selection_count = keys.len();
                // Set from the reader, which is already showing these messages:
                // mirror the selection but leave the reader alone. Reporting it
                // back would also drop whatever the reader has selected that this
                // list has no row for.
                if self.from_reader > 0 {
                    self.from_reader -= 1;
                    if keys == self.reader_keys {
                        self.selected_ids = keys;
                        return;
                    }
                    // Something else moved the selection — stop expecting ours.
                    self.from_reader = 0;
                }
                // A selection the user made here; the reader outlines it.
                let _ = sender.output(MessageListOutput::SelectionKeys(keys.clone()));
                match keys.as_slice() {
                    [] => self.selected_id = None,
                    [key] => {
                        // Exactly one selected → show it in the reader. Skip when
                        // it's already the viewed row (e.g. programmatic restore
                        // after a rebuild) so it isn't needlessly reloaded.
                        if self.selected_id != Some(*key) {
                            // Which way did the user move? Only a change with a
                            // known previous row says anything — the post-delete
                            // advance clears selected_id first, so it can never
                            // flip the direction it is itself steering by.
                            let pos =
                                |k: &(u32, u32)| self.shown.iter().position(|m| (m.account_id, m.id) == *k);
                            if let (Some(old), Some(new)) =
                                (self.selected_id.as_ref().and_then(&pos), pos(key))
                            {
                                if new != old {
                                    self.nav_direction = if new > old { 1 } else { -1 };
                                }
                            }
                            self.selected_id = Some(*key);
                            if let Some(m) = self
                                .shown
                                .iter()
                                .find(|m| (m.account_id, m.id) == *key)
                                .cloned()
                            {
                                let (thread, solo) = self.conversation_for(&m);
                                self.emitted_thread =
                                    thread.iter().map(|t| (t.account_id, t.id)).collect();
                                let _ = sender.output(MessageListOutput::Selected {
                                    message: m,
                                    thread,
                                    solo,
                                });
                            }
                        }
                    }
                    // Multiple selected → keep the reader on the primary message.
                    _ => {}
                }
                self.selected_ids = keys;
            }
            MessageListInput::Bulk(action) => {
                // The bulk bar's spam button is one button: in Junk it means
                // the reverse.
                let action = if self.in_junk && action == BulkAction::Spam { BulkAction::NotSpam } else { action };
                let messages: Vec<Message> = self
                    .rows
                    .widget()
                    .selected_rows()
                    .iter()
                    .filter_map(|r| self.shown.get(r.index() as usize).cloned())
                    .collect();
                if !messages.is_empty() {
                    let _ = sender.output(MessageListOutput::Bulk { action, messages });
                }
                // Row-removing actions keep the selection: the RemoveMany that
                // follows reads it to know the viewed message is going away and
                // to advance the selection (and reader) in its place — clearing
                // here left the reader stale on the deleted message. In-place
                // actions (read/flag) drop the selection as before, dismissing
                // the bulk bar.
                if matches!(
                    action,
                    BulkAction::MarkRead
                        | BulkAction::MarkUnread
                        | BulkAction::Flag
                        | BulkAction::Unflag
                )
                {
                    self.rows.widget().unselect_all();
                    self.selected_id = None;
                    self.selected_ids.clear();
                    self.selection_count = 0;
                }
            }
            MessageListInput::MoveSelection(delta) => {
                let list = self.rows.widget();
                if self.shown.is_empty() {
                    return;
                }
                if delta != 0 {
                    self.nav_direction = delta.signum();
                }
                // From the current row, or from the top/bottom when nothing is
                // selected yet, so the first keypress always lands somewhere.
                let current = list
                    .selected_rows()
                    .first()
                    .map(|r| r.index())
                    .unwrap_or(if delta > 0 { -1 } else { self.shown.len() as i32 });
                let next = (current + delta).clamp(0, self.shown.len() as i32 - 1);
                if let Some(row) = list.row_at_index(next) {
                    list.unselect_all();
                    list.select_row(Some(&row));
                    row.grab_focus();
                }
            }

            MessageListInput::ToggleSelection => {
                let list = self.rows.widget();
                let Some(row) = list.focus_child().and_downcast::<gtk::ListBoxRow>().or_else(|| {
                    list.selected_rows().first().cloned()
                }) else {
                    return;
                };
                if row.is_selected() {
                    list.unselect_row(&row);
                } else {
                    list.select_row(Some(&row));
                }
            }

            MessageListInput::SetUnreadOnly(on) => {
                if self.unread_only != on {
                    self.unread_only = on;
                    self.rebuild_preserving_scroll();
                }
            }

            MessageListInput::SetStarredOnly(on) => {
                if self.starred_only != on {
                    self.starred_only = on;
                    self.rebuild_preserving_scroll();
                }
            }

            MessageListInput::FocusList => {
                // The "back to list" shortcut: deliberately returns to the
                // selected row (that's the point — resume j/k navigation
                // from what you were reading), so it's allowed to scroll.
                let list = self.rows.widget();
                let row = list
                    .selected_rows()
                    .first()
                    .cloned()
                    .or_else(|| list.row_at_index(0));
                if let Some(row) = row {
                    row.grab_focus();
                }
            }
            MessageListInput::ReclaimFocus => {
                // Housekeeping focus restore (e.g. after a compose window
                // closes) so keyboard shortcuts keep targeting the list —
                // unlike `FocusList`, this isn't a "go back to what I was
                // reading" action, so it shouldn't scroll the viewport away
                // from wherever the user was browsing.
                self.preserving_scroll(|this| {
                    let list = this.rows.widget();
                    let row = list
                        .selected_rows()
                        .first()
                        .cloned()
                        .or_else(|| list.row_at_index(0));
                    if let Some(row) = row {
                        row.grab_focus();
                    }
                });
                self.hide_focus_ring();
            }

            MessageListInput::FocusSearch => {
                // Toggle (#102): the header button closes an open search too.
                if self.search_open {
                    sender.input(MessageListInput::CloseSearch);
                    return;
                }
                // Clicking the button blurs an empty entry, whose focus-leave
                // just closed the bar — that same click must not reopen it.
                if self
                    .search_closed_at
                    .take()
                    .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(300))
                {
                    return;
                }
                self.search_open = true;
                if let Some(entry) = &self.search_entry {
                    let entry = entry.clone();
                    // After the revealer maps it; focusing an unmapped entry
                    // is a no-op.
                    gtk::glib::idle_add_local_once(move || {
                        entry.grab_focus();
                    });
                }
            }

            MessageListInput::CloseSearch => {
                self.search_open = false;
                self.search_closed_at = Some(std::time::Instant::now());
                self.clear_search();
                self.rebuild_preserving_scroll();
            }

            MessageListInput::ClearSelection => {
                self.rows.widget().unselect_all();
                self.selected_id = None;
                self.selected_ids.clear();
                self.selection_count = 0;
            }
            MessageListInput::SetSort(order) => {
                if self.sort != order {
                    self.sort = order;
                    self.rebuild();
                }
            }
            MessageListInput::ToggleThread(key) => {
                // Expansion disabled: the chevron stays but does nothing — the
                // conversation is read through the reader's cards.
                if !self.thread_expansion {
                    return;
                }
                let was_expanded = self.expanded_threads.contains(&key) != self.default_expanded;
                if was_expanded {
                    // Slide the replies shut in place; the list only drops
                    // them once that animation has actually finished (see
                    // `start_collapse_thread`) — dropping them right away
                    // would just make them vanish instead of collapsing.
                    self.start_collapse_thread(key, &sender);
                } else {
                    if !self.expanded_threads.remove(&key) {
                        self.expanded_threads.insert(key.clone());
                    }
                    // Insert just this thread's replies rather than rebuilding
                    // the whole list — on a long list a full rebuild tears
                    // down and recreates every row (up to RENDER_CAP), which
                    // stutters right as the reveal animation is trying to run.
                    self.expand_thread(&key);
                }
            }
            MessageListInput::FinishCollapseThread(key) => {
                self.collapsing_threads.remove(&key);
                if !self.expanded_threads.remove(&key) {
                    self.expanded_threads.insert(key.clone());
                }
                // Same reasoning as `expand_thread`: drop just these rows.
                self.collapse_thread_rows(&key);
            }
            MessageListInput::RowActivated(index) => {
                if let Some(m) = self.shown.get(index as usize) {
                    let (thread, _solo) = self.conversation_for(m);
                    let _ = sender.output(MessageListOutput::Activated {
                        message: m.clone(),
                        thread,
                    });
                }
            }
            MessageListInput::MarkRead(id) => {
                if let Some(m) = self.all.iter_mut().find(|m| m.id == id) {
                    m.unread = false;
                }
                if let Some(idx) = self.shown.iter().position(|m| m.id == id) {
                    self.shown[idx].unread = false;
                    self.row_send(idx, MessageRowInput::SetRead(true));
                }
                self.refresh_thread_unread(id);
            }
            MessageListInput::SetRead { id, read } => {
                if let Some(m) = self.all.iter_mut().find(|m| m.id == id) {
                    m.unread = !read;
                }
                if let Some(idx) = self.shown.iter().position(|m| m.id == id) {
                    self.shown[idx].unread = !read;
                    self.row_send(idx, MessageRowInput::SetRead(read));
                }
                self.refresh_thread_unread(id);
            }
            MessageListInput::SetStarred { id, starred } => {
                if let Some(m) = self.all.iter_mut().find(|m| m.id == id) {
                    m.starred = starred;
                }
                if let Some(idx) = self.shown.iter().position(|m| m.id == id) {
                    self.shown[idx].starred = starred;
                    self.row_send(idx, MessageRowInput::SetStarred(starred));
                }
                self.refresh_thread_star(id);
            }
            MessageListInput::SetKeywords { id, keywords } => {
                if let Some(m) = self.all.iter_mut().find(|m| m.id == id) {
                    m.keywords = keywords.clone();
                }
                if let Some(idx) = self.shown.iter().position(|m| m.id == id) {
                    self.shown[idx].keywords = keywords.clone();
                    self.row_send(idx, MessageRowInput::SetKeywords(keywords));
                }
            }
            MessageListInput::SetTags(tags) => {
                if *self.tags.borrow() != tags {
                    *self.tags.borrow_mut() = tags;
                    // Chips and the palette's tag button follow the
                    // definitions; the rows are rebuilt to pick them up.
                    self.rebuild_preserving_scroll();
                }
            }
            MessageListInput::SetTagFor { message, keyword, add } => {
                let _ = sender.output(MessageListOutput::SetTag { message, keyword, add });
            }
            MessageListInput::SetHasAttachment { id, has } => {
                if let Some(m) = self.all.iter_mut().find(|m| m.id == id) {
                    m.has_attachment = has;
                }
                if let Some(idx) = self.shown.iter().position(|m| m.id == id) {
                    self.shown[idx].has_attachment = has;
                    self.row_send(idx, MessageRowInput::SetHasAttachment(has));
                }
            }
            MessageListInput::Remove(id) => {
                self.flush_rows();
                // Was the removed message the one shown in the reader? If so we'll
                // advance to whatever row slides into its place.
                let was_viewed = self.selected_id.map(|(_, i)| i) == Some(id);
                if was_viewed {
                    self.selected_id = None;
                }
                self.selected_ids.retain(|(_, i)| *i != id);
                self.all.retain(|m| m.id != id);
                let removed_idx = self.shown.iter().position(|m| m.id == id);
                // Was the row about to be destroyed the one holding keyboard
                // focus? If so, and it isn't the viewed row handled below,
                // we'll need to reclaim focus ourselves after it's gone —
                // otherwise GTK picks a fallback of its own (often the
                // selected row, wherever that is) and scrolls there.
                let had_focus = removed_idx
                    .and_then(|idx| self.rows.widget().row_at_index(idx as i32))
                    .is_some_and(|row| row.has_focus());
                if let Some(idx) = removed_idx {
                    // Removing a row can make GTK scroll the *selected* row
                    // back into view on its own, even though this row isn't
                    // it — pin the viewport so an unrelated deletion further
                    // down the list doesn't yank the user back to whatever
                    // they're reading.
                    self.preserving_scroll(|this| {
                        this.shown.remove(idx);
                        this.rows.guard().remove(idx);
                    });
                    self.publish_drag_keys();
                    // The surgical removal skips the rebuild that normally
                    // recomputes these — keep the header count honest.
                    self.total_matches = self.total_matches.saturating_sub(1);
                    self.rendered_count = self.shown.len();
                }

                if was_viewed {
                    match removed_idx {
                        // Advance in the direction the user was triaging: the
                        // row now at the removed slot when moving down the
                        // list, the one above it when moving up (Apple Mail's
                        // behaviour). Selecting it fires SelectionChanged,
                        // which loads it in the reader.
                        Some(idx) if !self.shown.is_empty() => {
                            let next = self.advance_index(idx);
                            self.select_and_focus(next);
                        }
                        // Nothing left to show → clear the reader.
                        _ => {
                            let _ = sender.output(MessageListOutput::SelectionCleared);
                        }
                    }
                } else if had_focus {
                    if let Some(idx) = removed_idx {
                        if !self.shown.is_empty() {
                            self.focus_only(self.advance_index(idx));
                        }
                    }
                }
            }
            MessageListInput::RemoveMany(ids) => {
                self.flush_rows();
                if ids.is_empty() {
                    return;
                }
                let set: std::collections::HashSet<u32> = ids.into_iter().collect();
                let was_viewed = self.selected_id.map(|(_, i)| i).is_some_and(|i| set.contains(&i));
                if was_viewed {
                    self.selected_id = None;
                }
                self.selected_ids.retain(|(_, i)| !set.contains(i));
                self.all.retain(|m| !set.contains(&m.id));
                // Where the first removed row sat, so we can re-select in its place.
                let first_removed = self.shown.iter().position(|m| set.contains(&m.id));
                // Did any row about to be destroyed hold keyboard focus? If
                // so, and it isn't the viewed row handled below, reclaim
                // focus ourselves afterward — otherwise GTK's own fallback
                // can land on the selected row and scroll the list there.
                let list = self.rows.widget();
                let had_focus = (0..self.shown.len()).any(|idx| {
                    set.contains(&self.shown[idx].id)
                        && list.row_at_index(idx as i32).is_some_and(|row| row.has_focus())
                });
                // Remove all matching rows in one guarded batch (a single widget
                // update) instead of one render cycle per message. Walk back-to-front
                // so indices stay valid. Pinned so GTK scrolling the selected row
                // back into view (see `preserving_scroll`) doesn't yank the user
                // away from browsing an unrelated part of the list.
                let shown_before = self.shown.len();
                self.preserving_scroll(|this| {
                    let mut guard = this.rows.guard();
                    let mut idx = this.shown.len();
                    while idx > 0 {
                        idx -= 1;
                        if set.contains(&this.shown[idx].id) {
                            this.shown.remove(idx);
                            guard.remove(idx);
                        }
                    }
                });
                self.publish_drag_keys();
                self.selection_count = self.selected_ids.len();
                // Keep the header count honest when no rebuild follows (the
                // backfill below recomputes these itself when it runs).
                self.total_matches =
                    self.total_matches.saturating_sub(shown_before - self.shown.len());
                self.rendered_count = self.shown.len();
                // Backfill the rendered window: a bulk removal can empty it
                // while `all` still holds messages beyond the render cap (a
                // folder larger than one page). Re-derive `shown` so what
                // remains appears immediately, instead of the list sitting
                // empty until the server finishes the move and pushes fresh
                // messages.
                if self.shown.len() < self.render_limit && self.all.len() > self.shown.len() {
                    self.rebuild_preserving_scroll();
                }
                if was_viewed {
                    match first_removed {
                        Some(idx) if !self.shown.is_empty() => {
                            let next = self.advance_index(idx);
                            self.select_and_focus(next);
                        }
                        _ => {
                            let _ = sender.output(MessageListOutput::SelectionCleared);
                        }
                    }
                } else if had_focus {
                    if let Some(idx) = first_removed {
                        if !self.shown.is_empty() {
                            self.focus_only(self.advance_index(idx));
                        }
                    }
                }
            }
            MessageListInput::RowAction { action, message } => {
                // Starring a conversation row stars the conversation (#star):
                // every member together, toggled as a unit — individual
                // members keep their own stars via their own rows/cards.
                if matches!(action, RowAction::ToggleStar) {
                    let members = self.thread_members(&message);
                    let is_head = members.first().is_some_and(|h| {
                        (h.account_id, h.id) == (message.account_id, message.id)
                    });
                    if is_head && members.len() > 1 {
                        // ANY starred member reads as a starred conversation
                        // (matching the head's indicator), so the toggle can
                        // always clear — all-starred semantics deadlocked the
                        // moment one member was individually unstarred.
                        let any = members.iter().any(|m| m.starred);
                        let _ = sender.output(MessageListOutput::Bulk {
                            action: if any { BulkAction::Unflag } else { BulkAction::Flag },
                            messages: members,
                        });
                        return;
                    }
                }
                let conversation = self.row_conversation(&message);
                let _ = sender.output(MessageListOutput::Action { action, message, conversation });
            }
            MessageListInput::SetPaletteCollapse(secs) => self.palette_collapse_secs.set(secs),
            MessageListInput::SetPaletteHover(on) => self.palette_hover.set(on),
            MessageListInput::SetSwipeReversed(on) => self.swipe_reversed.set(on),
            MessageListInput::SetSwipeEnabled(on) => {
                self.swipe_enabled.set(on);
                // Every mounted row re-renders and flips its tracker.
                for i in 0..self.rows.len() {
                    self.row_send(i, MessageRowInput::SwipePrefsChanged);
                }
            }
            MessageListInput::SetSwipeSensitivity(factor) => {
                self.swipe_sensitivity.set(factor);
                // Same nudge: post_view pushes the new figure into each
                // mounted row's swipe surface.
                for i in 0..self.rows.len() {
                    self.row_send(i, MessageRowInput::SwipePrefsChanged);
                }
            }
            MessageListInput::SetSelected(id) => {
                match id {
                    // Account-less id resolved against the shown list (the app
                    // only sends `None` today; `Some` kept for completeness).
                    Some(i) => {
                        if let Some(m) = self.shown.iter().find(|m| m.id == i) {
                            let key = (m.account_id, m.id);
                            self.selected_id = Some(key);
                            self.selected_ids = vec![key];
                            self.select_current();
                        }
                    }
                    None => {
                        self.selected_id = None;
                        self.selected_ids.clear();
                        self.selection_count = 0;
                        self.rows.widget().unselect_all();
                    }
                }
            }
            MessageListInput::DebugOpenPalette(idx) => {
                self.row_send(idx, MessageRowInput::TogglePalette);
            }
            MessageListInput::DebugSwipe { index, left } => {
                let px = if left { -SWIPE_MAX } else { SWIPE_MAX };
                self.row_send(index, MessageRowInput::SwipeUpdate(px));
                self.row_send(index, MessageRowInput::SwipeEnd);
            }
            MessageListInput::PaletteOpened(idx) => {
                for i in 0..self.shown.len() {
                    if i != idx {
                        self.row_send(i, MessageRowInput::ClosePalette);
                    }
                }
            }
            MessageListInput::SelectAndLoad(key) => {
                // Rows not built yet (the list landed in this same pass):
                // wait for the queued rebuild rather than find nothing.
                if self.rebuild_queued.is_some() {
                    self.pending_select = Some(key);
                    return;
                }
                // A reply inside a conversation has no row of its own: its
                // thread head does, and opening that shows the whole thread,
                // the reply included.
                let key = match self.shown.iter().any(|m| (m.account_id, m.id) == key) {
                    true => key,
                    false => match self.thread_head_for(key) {
                        Some(head) => head,
                        None => key,
                    },
                };
                if let Some(m) = self.shown.iter().find(|m| (m.account_id, m.id) == key).cloned() {
                    self.selected_id = Some(key);
                    self.selected_ids = vec![key];
                    // The list is in Multiple selection mode, so selecting the
                    // target only ADDS it — anything already selected (e.g. the
                    // row the last deletion advanced to) would stay lit and turn
                    // this into a two-row selection the reader ignores.
                    self.rows.widget().unselect_all();
                    self.select_current();
                    // These arrive from outside the list (a notification
                    // click, an undo) where the row can be far outside the
                    // viewport — bring it into view. In an idle so a rebuild
                    // queued just before this (and its own scroll restore)
                    // has laid the rows out first; the focus grab is what
                    // scrolls, and the selection pill is the indicator.
                    if let Some(idx) =
                        self.shown.iter().position(|m| (m.account_id, m.id) == key)
                    {
                        let list = self.rows.widget().clone();
                        gtk::glib::idle_add_local_once(move || {
                            if let Some(row) = list.row_at_index(idx as i32) {
                                row.grab_focus();
                            }
                            if let Some(win) =
                                list.root().and_then(|r| r.downcast::<gtk::Window>().ok())
                            {
                                win.set_focus_visible(false);
                            }
                        });
                    }
                    let (thread, solo) = self.conversation_for(&m);
                    self.emitted_thread = thread.iter().map(|t| (t.account_id, t.id)).collect();
                    let _ = sender.output(MessageListOutput::Selected { message: m, thread, solo });
                }
            }
            MessageListInput::RowMoveTo { message, x, y } => {
                let (messages, offer_whole) = self.move_to_messages(&message);
                let _ = sender.output(MessageListOutput::MoveTo { messages, offer_whole, x, y });
            }
            MessageListInput::ContextMenu { x, y } => {
                let list = self.rows.widget();
                // By the row's band first; failing that, by what is drawn
                // under the pointer, walked up to its row.
                let row = list.row_at_y(y as i32).or_else(|| {
                    list.pick(x, y, gtk::PickFlags::DEFAULT)
                        .and_then(|w| w.ancestor(gtk::ListBoxRow::static_type()))
                        .and_downcast::<gtk::ListBoxRow>()
                });
                if let Some(row) = row {
                    let selected = list.selected_rows();
                    let in_selection = selected.iter().any(|r| r.index() == row.index());
                    if selected.len() > 1 && in_selection {
                        // Right-clicked inside a multi-selection → bulk menu.
                        self.show_bulk_menu(x, y, &sender);
                    } else {
                        // Single-row menu acting on the clicked message. Crucially,
                        // don't select it — selecting would load it in the reader,
                        // and the user may just intend to move/archive/delete it.
                        if let Some(msg) = self.shown.get(row.index() as usize).cloned() {
                            self.show_context_menu(&msg, x, y, &sender);
                        }
                    }
                }
            }
        }
        // Whatever just happened, restate the header count if it moved — every
        // path that changes the visible rows funnels through here.
        let count = self.count_label();
        if count != self.last_count {
            self.last_count = count.clone();
            let _ = sender.output(MessageListOutput::CountChanged(count));
        }
        // And the conversations nobody has counted yet, so their real sizes
        // can be looked up (#222). Only the unasked ones: the answer arrives as
        // a rebuild, so asking again on the strength of it would never settle,
        // and a search re-filters the page on every keystroke.
        if self.threading {
            let fresh = unasked_threads(&self.listed_threads, &self.asked_threads);
            if !fresh.is_empty() {
                for (aid, root, _) in &fresh {
                    self.asked_threads.insert((*aid, root.clone()));
                }
                let _ = sender.output(MessageListOutput::ThreadsListed { groups: fresh });
            }
        }

    }
}

impl MessageList {
    /// What the list holds in RAM, for the memory section of an export: the
    /// folder's index, the rows built from it, and the whole-mailbox search
    /// pool (empty unless a search is open), each as (messages, bytes).
    pub fn memory_stats(&self) -> [(usize, usize); 3] {
        use crate::memory_report::messages_bytes;
        [messages_bytes(&self.all), messages_bytes(&self.shown), messages_bytes(&self.search_pool)]
    }

    /// Build and pop up the right-click menu for `msg` at the click location.
    fn show_context_menu(
        &self,
        msg: &Message,
        x: f64,
        y: f64,
        sender: &ComponentSender<Self>,
    ) {
        // Each entry carries the same icon as the reader-toolbar button (or
        // row-palette button) for that action, tying the two together.
        let conversation = self.row_conversation(msg);
        let item = |action: RowAction, label: &str, icon: &str| -> MenuEntry {
            let s = sender.clone();
            let m = msg.clone();
            let conversation = conversation.clone();
            MenuEntry::new(label, move || {
                let _ = s.output(MessageListOutput::Action {
                    action,
                    message: Box::new(m.clone()),
                    conversation: conversation.clone(),
                });
            })
            .icon(icon)
        };

        // (computed early: the star entry needs it too)
        let members_for_star = self.thread_members(msg);
        let star_is_head = members_for_star
            .first()
            .is_some_and(|h| (h.account_id, h.id) == (msg.account_id, msg.id));
        let mut flag_section = vec![if star_is_head && members_for_star.len() > 1 {
            // A conversation row's star acts on the whole thread, like its
            // read toggle below.
            let any = members_for_star.iter().any(|m| m.starred);
            let s = sender.clone();
            let members = members_for_star.clone();
            MenuEntry::new(
                if any { i18n("Remove Stars") } else { i18n("Star Conversation") },
                move || {
                    let _ = s.output(MessageListOutput::Bulk {
                        action: if any { BulkAction::Unflag } else { BulkAction::Flag },
                        messages: members.clone(),
                    });
                },
            )
            .icon(if any {
                "co.hyprlab.Hylki-non-starred-symbolic"
            } else {
                "co.hyprlab.Hylki-starred-symbolic"
            })
        } else if msg.starred {
            item(RowAction::ToggleStar, &i18n("Remove Star"), "co.hyprlab.Hylki-non-starred-symbolic")
        } else {
            item(RowAction::ToggleStar, &i18n("Star"), "co.hyprlab.Hylki-starred-symbolic")
        }];

        // A conversation row acts on the whole thread: its read entry marks
        // every member, through the same bulk path as a multi-select, and the
        // singular toggle is dropped (the row isn't a singular message).
        // Expanded replies keep the singular toggle. Labels follow state, like
        // the singular one: any unread member reads as an unread conversation.
        let members = self.thread_members(msg);
        let is_thread_head =
            members.first().is_some_and(|h| (h.account_id, h.id) == (msg.account_id, msg.id));
        // Move To…: the clicked message, then (for a conversation row) the
        // rest of its members, at the click's point in the window so the
        // app can anchor the picker there.
        let move_entry = {
            let (messages, offer_whole) = self.move_to_messages(msg);
            let (wx, wy) = self.window_point(x, y);
            let s = sender.clone();
            MenuEntry::new(&i18n("Move To…"), move || {
                let _ = s.output(MessageListOutput::MoveTo {
                    messages: messages.clone(),
                    offer_whole,
                    x: wx,
                    y: wy,
                });
            })
            .icon("co.hyprlab.Hylki-folder-symbolic")
        };
        if self.in_drafts {
            // A draft is neither read nor unread: no toggle to offer.
        } else if is_thread_head {
            let any_unread = members.iter().any(|m| m.unread);
            let s = sender.clone();
            flag_section.push(
                MenuEntry::new(
                    if any_unread { i18n("Mark All as Read") } else { i18n("Mark All as Unread") },
                    move || {
                        let _ = s.output(MessageListOutput::Bulk {
                            action: if any_unread {
                                BulkAction::MarkRead
                            } else {
                                BulkAction::MarkUnread
                            },
                            messages: members.clone(),
                        });
                    },
                )
                .icon(if any_unread {
                    "co.hyprlab.Hylki-mail-read-symbolic"
                } else {
                    "co.hyprlab.Hylki-mail-unread-symbolic"
                }),
            );
        } else if msg.unread {
            flag_section
                .push(item(RowAction::ToggleRead, &i18n("Mark as Read"), "co.hyprlab.Hylki-mail-read-symbolic"));
        } else {
            flag_section.push(item(
                RowAction::ToggleRead,
                &i18n("Mark as Unread"),
                "co.hyprlab.Hylki-mail-unread-symbolic",
            ));
        }

        // Tags (#71): one toggle per tag, a filled swatch where the message
        // carries it, behind a "Tags" submenu so a long list never makes
        // this menu too tall. Absent until a tag exists.
        let tag_section = {
            let tags = self.tags.borrow().clone();
            let s = sender.clone();
            let m = msg.clone();
            let entries = tag_menu_entries(&tags, msg, move |keyword, add| {
                let _ = s.output(MessageListOutput::SetTag {
                    message: Box::new(m.clone()),
                    keyword,
                    add,
                });
            });
            if entries.is_empty() {
                Vec::new()
            } else {
                vec![MenuEntry::submenu(i18n("Tags"), vec![entries]).icon("co.hyprlab.Hylki-tag-outline-symbolic")]
            }
        };

        let sections = vec![
            vec![
                item(RowAction::Reply, &i18n("Reply"), "co.hyprlab.Hylki-mail-reply-sender-symbolic"),
                item(RowAction::ReplyAll, &i18n("Reply All"), "co.hyprlab.Hylki-mail-reply-all-symbolic"),
                item(RowAction::Forward, &i18n("Forward"), "co.hyprlab.Hylki-mail-forward-symbolic"),
                item(
                    RowAction::EditAsNew,
                    &i18n("Edit as New Message"),
                    "co.hyprlab.Hylki-document-edit-symbolic",
                ),
            ],
            flag_section,
            tag_section,
            {
                let mut section = Vec::new();
                // In Junk the way back is "Not Spam" (#168): the server is
                // told, and the message returns to the Inbox. In Trash it
                // is a plain move, with spam still on offer.
                if self.in_junk {
                    section.push(item(RowAction::NotSpam, &i18n("Not Spam"), "co.hyprlab.Hylki-mail-mark-notjunk-symbolic"));
                } else {
                    if self.restorable {
                        section.push(item(RowAction::MoveToInbox, &i18n("Move to Inbox"), "co.hyprlab.Hylki-mail-inbox-symbolic"));
                    }
                    section.push(item(RowAction::Spam, &i18n("Mark as Spam"), "co.hyprlab.Hylki-mail-mark-junk-symbolic"));
                }
                section.push(move_entry);
                section.push(item(RowAction::Archive, &i18n("Archive"), "co.hyprlab.Hylki-mail-archive-symbolic"));
                section.push(item(RowAction::Delete, &i18n("Delete"), "co.hyprlab.Hylki-user-trash-symbolic"));
                section
            },
            vec![item(
                RowAction::AddContact,
                &i18n("Add Sender to Contacts"),
                "co.hyprlab.Hylki-contact-new-symbolic",
            )],
            vec![item(RowAction::ViewSource, &i18n("View Source"), "co.hyprlab.Hylki-code-symbolic")],
        ];

        show_context_menu(self.rows.widget(), x, y, sections);
    }

    /// Build and pop up the bulk-action menu for the current multi-selection.
    fn show_bulk_menu(&self, x: f64, y: f64, sender: &ComponentSender<Self>) {
        let item = |action: BulkAction, label: &str, icon: &str| -> MenuEntry {
            let s = sender.clone();
            MenuEntry::new(label, move || s.input(MessageListInput::Bulk(action))).icon(icon)
        };

        let sections = vec![
            {
                let mut section = Vec::new();
                // Drafts are neither read nor unread.
                if !self.in_drafts {
                    section.push(item(BulkAction::MarkRead, &i18n("Mark as Read"), "co.hyprlab.Hylki-mail-read-symbolic"));
                    section.push(item(BulkAction::MarkUnread, &i18n("Mark as Unread"), "co.hyprlab.Hylki-mail-unread-symbolic"));
                }
                section.push(item(BulkAction::Flag, &i18n("Flag"), "co.hyprlab.Hylki-starred-symbolic"));
                section
            },
            {
                let mut section = Vec::new();
                if self.in_junk {
                    section.push(item(BulkAction::NotSpam, &i18n("Not Spam"), "co.hyprlab.Hylki-mail-mark-notjunk-symbolic"));
                } else {
                    if self.restorable {
                        section.push(item(BulkAction::MoveToInbox, &i18n("Move to Inbox"), "co.hyprlab.Hylki-mail-inbox-symbolic"));
                    }
                    section.push(item(BulkAction::Spam, &i18n("Mark as Spam"), "co.hyprlab.Hylki-mail-mark-junk-symbolic"));
                }
                {
                    // Move To… for the whole selection.
                    let messages: Vec<Message> = self
                        .rows
                        .widget()
                        .selected_rows()
                        .iter()
                        .filter_map(|r| self.shown.get(r.index() as usize).cloned())
                        .collect();
                    let (wx, wy) = self.window_point(x, y);
                    let s = sender.clone();
                    section.push(
                        MenuEntry::new(&i18n("Move To…"), move || {
                            let _ = s.output(MessageListOutput::MoveTo {
                                messages: messages.clone(),
                                offer_whole: false,
                                x: wx,
                                y: wy,
                            });
                        })
                        .icon("co.hyprlab.Hylki-folder-symbolic"),
                    );
                }
                section.push(item(BulkAction::Archive, &i18n("Archive"), "co.hyprlab.Hylki-mail-archive-symbolic"));
                section.push(item(BulkAction::Delete, &i18n("Delete"), "co.hyprlab.Hylki-user-trash-symbolic"));
                section
            },
        ];

        show_context_menu_with_header(
            self.rows.widget(),
            x,
            y,
            Some(&format!("{} selected", self.selection_count)),
            sections,
        );
    }

    /// After a rebuild: a lone selected conversation head whose thread has
    /// gained members since it was opened (a reply just synced in) is
    /// reported, so the reader can show the new message without a
    /// re-selection. Members lost (deleted elsewhere) are left to the
    /// vanish handling.
    fn report_thread_growth(&mut self, sender: &ComponentSender<Self>) {
        if !self.threading || self.selected_ids.len() != 1 {
            return;
        }
        let Some(key) = self.selected_id else { return };
        let Some(m) = self.shown.iter().find(|m| (m.account_id, m.id) == key).cloned() else {
            return;
        };
        let (thread, solo) = self.conversation_for(&m);
        if solo || thread.len() <= 1 {
            return;
        }
        let keys: Vec<(u32, u32)> = thread.iter().map(|t| (t.account_id, t.id)).collect();
        let grew = keys.len() > self.emitted_thread.len()
            && self.emitted_thread.iter().all(|k| keys.contains(k));
        if grew {
            self.emitted_thread = keys;
            let _ = sender.output(MessageListOutput::ThreadGrew { message: m, thread });
        }
    }

    /// What a Move To… on `msg` offers: the message, then — when it heads a
    /// conversation — the rest of its members, with the whole-conversation
    /// switch (#171).
    fn move_to_messages(&self, msg: &Message) -> (Vec<Message>, bool) {
        let members = self.thread_members(msg);
        let is_head =
            members.first().is_some_and(|h| (h.account_id, h.id) == (msg.account_id, msg.id));
        let mut messages = vec![msg.clone()];
        let offer_whole = is_head && members.len() > 1;
        if offer_whole {
            messages.extend(
                members.iter().filter(|m| (m.account_id, m.id) != (msg.account_id, msg.id)).cloned(),
            );
        }
        (messages, offer_whole)
    }

    /// A point in the rows list, in the window's coordinates (the app
    /// anchors popovers on the window; falls back to the point as given).
    fn window_point(&self, x: f64, y: f64) -> (f64, f64) {
        let list = self.rows.widget();
        list.root()
            .and_then(|root| {
                let root: gtk::Widget = root.upcast();
                list.compute_point(&root, &gtk::graphene::Point::new(x as f32, y as f32))
            })
            .map_or((x, y), |p| (p.x() as f64, p.y() as f64))
    }

    /// Toolbar count: total matches, noting when more exist than are shown.
    fn count_label(&self) -> String {
        if self.total_matches > self.rendered_count {
            format!("{} of {}", self.rendered_count, self.total_matches)
        } else {
            format!("{}", self.total_matches)
        }
    }

    /// Whether the bottom loading spinner should show: the user has scrolled past
    /// what's loaded (`render_limit` exceeds the indexed count) and the folder's
    /// index is still streaming in.
    fn is_loading_more(&self) -> bool {
        self.render_limit > self.total_matches && !self.index_complete
    }

    /// Whether to show the empty-folder placeholder: the folder has loaded,
    /// nothing is in it, no search is filtering it, and no more rows are on
    /// their way (the loading indicator covers that state instead).
    fn is_empty_state(&self) -> bool {
        self.loaded && self.shown.is_empty() && self.query.is_empty() && self.index_complete
    }

    /// Runs `f` (typically some change to the rows in `self.rows`/`self.shown`)
    /// without letting the scroll position move — including a scroll GTK
    /// performs on its own to keep the *selected* row in view whenever the
    /// list's contents change, even if that row was never touched by `f`.
    /// Saves the current position, pins the adjustment there through every
    /// change `f` (or GTK) makes, and does a final restore once layout has
    /// had a tick to settle — a single restore right after `f` can lose the
    /// race against GTK's own scroll, which can land after this returns.
    fn preserving_scroll(&mut self, f: impl FnOnce(&mut Self)) {
        let Some(scroller) = self.scroller.clone() else {
            f(self);
            return;
        };
        let adj = scroller.vadjustment();
        let pos = adj.value();
        let hid = adj.connect_notify_local(Some("value"), move |adj, _| {
            if (adj.value() - pos).abs() > f64::EPSILON {
                adj.set_value(pos);
            }
        });
        f(self);
        let adj2 = adj.clone();
        gtk::glib::idle_add_local_once(move || {
            adj2.set_value(pos);
            adj2.disconnect(hid);
        });
    }

    /// A full rebuild that keeps the current scroll offset (a plain rebuild jumps
    /// to the top). Used when growing the list beneath the user, and for any
    /// background re-render (e.g. a folder re-sync) so it doesn't disturb
    /// someone browsing further down.
    fn rebuild_preserving_scroll(&mut self) {
        self.preserving_scroll(Self::rebuild);
    }

    /// Build every row again, scroll kept: for a change to how rows look
    /// rather than which messages they show (see `rows_stale`).
    fn rebuild_rows_preserving_scroll(&mut self) {
        self.rows_stale = true;
        self.rebuild_preserving_scroll();
    }

    /// Build every row again from the top (see `rows_stale`).
    fn rebuild_rows(&mut self) {
        self.rows_stale = true;
        self.rebuild();
    }

    /// A message's read state changed: recompute its conversation's aggregate
    /// unread flag and push it to the head row, so a collapsed thread's heavy
    /// highlight clears exactly when its last unread message is read.
    fn refresh_thread_unread(&mut self, id: u32) {
        let Some(key) = self
            .all
            .iter()
            .find(|m| m.id == id)
            .map(|m| (m.account_id, m.id))
        else {
            return;
        };
        let Some(tkey) = self.msg_thread.get(&key) else {
            return;
        };
        let Some(members) = self.thread_members.get(tkey).cloned() else {
            return;
        };
        let any_unread = members
            .iter()
            .any(|k| self.all.iter().any(|m| (m.account_id, m.id) == *k && m.unread));
        // The head is the first (and, when collapsed, only) member in `shown`.
        if let Some(idx) = self
            .shown
            .iter()
            .position(|m| members.contains(&(m.account_id, m.id)))
        {
            self.row_send(idx, MessageRowInput::SetThreadUnread(any_unread));
        }
    }

    /// Mirror of [`refresh_thread_unread`] for the star: the head shows a
    /// conversation as starred while any member is.
    fn refresh_thread_star(&mut self, id: u32) {
        let Some(key) = self
            .all
            .iter()
            .find(|m| m.id == id)
            .map(|m| (m.account_id, m.id))
        else {
            return;
        };
        let Some(tkey) = self.msg_thread.get(&key) else {
            return;
        };
        let Some(members) = self.thread_members.get(tkey).cloned() else {
            return;
        };
        let any = members
            .iter()
            .any(|k| self.all.iter().any(|m| (m.account_id, m.id) == *k && m.starred));
        if let Some(idx) = self
            .shown
            .iter()
            .position(|m| members.contains(&(m.account_id, m.id)))
        {
            self.row_send(idx, MessageRowInput::SetThreadStarred(any));
        }
    }


    /// Begin collapsing a thread: slide its currently-visible replies shut
    /// (they stay exactly where they are in `self.rows`/`self.shown` — only
    /// their own Revealer closes), then drop them once that animation has
    /// actually finished. Dropping right away — before the replies have
    /// shrunk — would just make them disappear outright. (PR #79)
    fn start_collapse_thread(&mut self, key: (u32, String), sender: &ComponentSender<Self>) {
        self.flush_rows();
        if let Some(members) = self.thread_members.get(&key).cloned() {
            // The head survives the toggle (only its replies are removed),
            // so its own chevron can rotate shut in place, in step with the
            // replies sliding closed beneath it.
            if let Some(&head_key) = members.first() {
                if let Some(idx) = self.shown.iter().position(|m| (m.account_id, m.id) == head_key)
                {
                    self.row_send(idx, MessageRowInput::SetThreadExpanded(false));
                }
            }
            // `members` is head-first (see `rebuild`); only the replies
            // beneath it animate closed.
            for child_key in members.iter().skip(1) {
                if let Some(idx) =
                    self.shown.iter().position(|m| (m.account_id, m.id) == *child_key)
                {
                    self.row_send(idx, MessageRowInput::SetRevealed(false));
                }
            }
        }
        if let Some(old) = self.collapsing_threads.remove(&key) {
            old.remove();
        }
        let s = sender.clone();
        let timer_key = key.clone();
        // Matches the Revealer's own transition duration, so the rows are
        // fully closed by the time they're actually dropped from the list.
        let timer =
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
                s.input(MessageListInput::FinishCollapseThread(timer_key));
            });
        self.collapsing_threads.insert(key, timer);
    }

    /// Insert a thread's replies right after its head, without touching any
    /// other row — the counterpart to `collapse_thread_rows`. Each new row
    /// mounts with `revealed: false` and animates open on its own (see
    /// `RowInit::revealed`). (PR #79)
    fn expand_thread(&mut self, key: &(u32, String)) {
        self.flush_rows();
        let Some(members) = self.thread_members.get(key).cloned() else { return };
        let Some(&head_key) = members.first() else { return };
        let Some(head_pos) = self.shown.iter().position(|m| (m.account_id, m.id) == head_key)
        else {
            return;
        };
        // `members` is already oldest-first (see `rebuild`); look the replies
        // up from the full index before borrowing `self.rows`/`self.shown`
        // mutably.
        let children: Vec<Message> = {
            let source = self.active_source();
            members
                .iter()
                .skip(1)
                .filter_map(|k| source.iter().find(|m| (m.account_id, m.id) == *k).cloned())
                .collect()
        };
        if children.is_empty() {
            return;
        }
        // The head survives the toggle (only its replies are inserted), so
        // its own chevron can rotate open in place.
        self.row_send(head_pos, MessageRowInput::SetThreadExpanded(true));
        {
            let mut guard = self.rows.guard();
            for (i, msg) in children.iter().enumerate() {
                let ring_class =
                    if self.colorize && self.account_colors.contains_key(&msg.account_id) {
                        Some(format!("vireo-acct-ring-{}", msg.account_id))
                    } else {
                        None
                    };
                guard.insert(
                    head_pos + 1 + i,
                    RowInit {
                        msg: msg.clone(),
                        gravatar: self.gravatar,
                        avatars: self.avatars,
                        avatar_late: false,
                        sender_logos: self.sender_logos,
                        preview_lines: self.preview_lines,
                        show_subject: self.show_subject,
                        ring_class,
                        palette_collapse_secs: self.palette_collapse_secs.clone(),
                        palette_hover: self.palette_hover.clone(),
                        tags: self.tags.clone(),
                        show_palette: self.list_palette,
                        in_junk: self.in_junk,
                        in_drafts: self.in_drafts,
                        thread_count: 0,
                        is_thread_child: true,
                        is_last_child: i == children.len() - 1,
                        thread_expanded: false,
                        thread_expandable: self.thread_expansion,
                        thread_key: None,
                        thread_date: None,
                        thread_from: None,
                        thread_preview: None,
                        thread_unread: false,
                        thread_starred: false,
                        drag_keys: self.drag_keys.clone(),
                        thread_drag: self.thread_drag.clone(),
                        show_recipient: self.show_recipient,
                        revealed: false,
                        swipe_reversed: self.swipe_reversed.clone(),
                        swipe_enabled: self.swipe_enabled.clone(),
                        swipe_sensitivity: self.swipe_sensitivity.clone(),
                    },
                );
            }
        }
        self.shown.splice(head_pos + 1..head_pos + 1, children);
        self.publish_drag_keys();
    }

    /// Drop a thread's reply rows (already slid shut by `start_collapse_thread`)
    /// without touching any other row — the counterpart to `expand_thread`.
    /// (PR #79)
    fn collapse_thread_rows(&mut self, key: &(u32, String)) {
        self.flush_rows();
        let Some(members) = self.thread_members.get(key).cloned() else { return };
        let mut indices: Vec<usize> = members
            .iter()
            .skip(1)
            .filter_map(|k| self.shown.iter().position(|m| (m.account_id, m.id) == *k))
            .collect();
        indices.sort_unstable();
        {
            let mut guard = self.rows.guard();
            // Remove back-to-front so earlier removals don't shift the
            // indices still to come.
            for &idx in indices.iter().rev() {
                guard.remove(idx);
            }
        }
        for &idx in indices.iter().rev() {
            self.shown.remove(idx);
        }
        self.publish_drag_keys();
    }

    /// Whether a search is currently active (the query is non-empty).
    fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// The message set the search filters over: the cross-folder pool while an
    /// `AllFolders` search is active (and the pool has arrived), otherwise the
    /// current folder's own index.
    fn active_source(&self) -> &[Message] {
        if self.searching()
            && self.scope == SearchScope::AllFolders
            && !self.search_pool.is_empty()
        {
            &self.search_pool
        } else {
            &self.all
        }
    }

    fn search_placeholder(&self) -> String {
        i18n(match self.scope {
            SearchScope::AllFolders => "Search all folders",
            SearchScope::ThisFolder => "Search this folder",
        })
    }

    /// Drop any active search: clear the query and the entry text so a folder
    /// switch doesn't leave a stale term filtering the new folder.
    fn clear_search(&mut self) {
        if self.query.is_empty() {
            return;
        }
        self.query.clear();
        if let Some(e) = &self.search_entry {
            e.set_text("");
        }
    }

    fn rebuild(&mut self) {
        let q = self.query.to_lowercase();
        // Filter and sort by reference, and clone only the page that is actually
        // rendered. A folder's index holds every message ever synced, while
        // `render_limit` is a few hundred — cloning the whole match set first put
        // a copy of the entire mailbox through the allocator on every keystroke
        // and on the cache-backed load at startup.
        let mut matches: Vec<&Message> = self
            .active_source()
            .iter()
            .filter(|m| !self.unread_only || m.unread)
            .filter(|m| !self.starred_only || m.starred)
            .filter(|m| {
                q.is_empty()
                    || m.subject.to_lowercase().contains(&q)
                    || m.from_name.to_lowercase().contains(&q)
                    || m.from_addr.to_lowercase().contains(&q)
                    || m.preview.to_lowercase().contains(&q)
            })
            .collect();
        let sort = self.sort;
        let t_rebuild = std::time::Instant::now();
        matches.sort_by(|a, b| message_cmp(a, b, sort));
        let total_matches = matches.len();
        // Render up to the current limit; the rest stay indexed (for search) until
        // the user scrolls further and `LoadMore` raises the limit.
        let capped: Vec<Message> = matches.into_iter().take(self.render_limit).cloned().collect();
        self.total_matches = total_matches;
        self.rendered_count = capped.len();

        // Group into conversations by reply headers (Message-ID / In-Reply-To /
        // References), preserving newest-first order. Each thread shows its newest
        // message as the head; expanding reveals the older replies beneath it.
        // With threading off, every message is its own group.
        let keys = if self.threading {
            compute_thread_keys(&capped, &self.thread_links)
        } else {
            std::collections::HashMap::new()
        };
        let key_for = |m: &Message| -> (u32, String) {
            keys.get(&(m.account_id, m.id))
                .cloned()
                .unwrap_or_else(|| (m.account_id, format!("\u{0}uid{}", m.uid)))
        };
        let mut order: Vec<(u32, String)> = Vec::new();
        let mut groups: std::collections::HashMap<(u32, String), Vec<Message>> =
            std::collections::HashMap::new();
        for m in capped {
            let key = key_for(&m);
            if let Some(v) = groups.get_mut(&key) {
                v.push(m);
            } else {
                order.push(key.clone());
                groups.insert(key, vec![m]);
            }
        }

        // Flatten back into display order, recording per-row thread metadata.
        struct RowMeta {
            count: usize,
            /// Whether this row's chip can actually open anything: true only
            /// when the folder holds more than one of the conversation (#222).
            expandable: bool,
            is_child: bool,
            is_last: bool,
            /// Newest member's (from_name, from_addr) and preview, surfaced on
            /// the head row (display only; identity stays the head's).
            from: Option<(String, String)>,
            preview: Option<String>,
            expanded: bool,
            key: Option<(u32, String)>,
            unread: bool,
            starred: bool,
            /// The newest member's display time (thread heads only): the head
            /// row says when the conversation last moved, not when it began.
            latest: Option<String>,
        }
        let mut shown: Vec<Message> = Vec::new();
        let mut metas: Vec<RowMeta> = Vec::new();
        self.msg_thread.clear();
        self.thread_members.clear();
        self.listed_threads.clear();
        for key in &order {
            let mut msgs = groups.remove(key).unwrap();
            // A conversation reads like a transcript: the message that started it
            // is the row on screen, and its replies descend beneath it to the
            // newest. Where the *thread* sits among the other rows is still the
            // list's sort order — recent activity keeps it near the top — but
            // inside the thread, time only runs one way.
            msgs.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.uid.cmp(&b.uid)));
            let count = msgs.len();
            // What the badge says. `count` is what this folder holds and goes on
            // steering the rows — which of them nest, what expands, whose unread
            // dot shows — but the number on the chip is the size of the
            // *conversation*, replies filed in Sent included (#222). Never less
            // than what is on screen: a stale or partial answer from the cache
            // must not make the badge contradict the rows under it.
            let total = if self.threading {
                self.thread_counts.get(key).copied().unwrap_or(0).max(count)
            } else {
                count
            };
            // Ask about anything that could be bigger than it looks. A thread of
            // one is worth asking about too — a mail you answered twice is a
            // conversation of three and shows no badge at all today.
            if self.threading {
                let ids = crate::models::thread_ids(&msgs);
                if !ids.is_empty() {
                    self.listed_threads.push((key.0, key.1.clone(), ids));
                }
            }
            // `expanded_threads` stores toggles away from the default state.
            // With expansion disabled no thread ever opens in the list; the
            // stored toggles survive for when it is re-enabled.
            let expanded = count > 1
                && self.thread_expansion
                && (self.expanded_threads.contains(key) != self.default_expanded);
            // The head stays marked unread while ANY message in its
            // conversation is unread — hidden replies included.
            let any_unread = count > 1 && msgs.iter().any(|m| m.unread);
            if count > 1 {
                let members: Vec<(u32, u32)> = msgs.iter().map(|m| (m.account_id, m.id)).collect();
                for k in &members {
                    self.msg_thread.insert(*k, key.clone());
                }
                self.thread_members.insert(key.clone(), members);
            }
            // The head is the thread's *oldest* message (see the sort above),
            // but its row should say when the conversation last moved — so the
            // newest member's time is carried alongside for display.
            let latest = (count > 1)
                .then(|| msgs.last().map(|m| m.datetime_list()))
                .flatten();
            // The row surfaces the conversation's NEWEST message — its
            // sender and preview — instead of re-showing the opener each
            // time a reply lands.
            let (latest_from, latest_preview) = if count > 1 {
                let m = msgs.last().unwrap();
                (
                    Some((m.from_name.clone(), m.from_addr.clone())),
                    Some(m.preview.clone()),
                )
            } else {
                (None, None)
            };
            let any_starred = count > 1 && msgs.iter().any(|m| m.starred);
            let mut it = msgs.into_iter();
            let head = it.next().unwrap();
            shown.push(head);
            metas.push(RowMeta {
                count: total,
                // Only this folder's copies can be nested under the head, so a
                // conversation whose extra members are all elsewhere wears a
                // bare count and no caret — there is nothing here to open.
                expandable: count > 1,
                is_child: false,
                is_last: false,
                from: latest_from,
                preview: latest_preview,
                expanded,
                key: if count > 1 { Some(key.clone()) } else { None },
                unread: any_unread,
                starred: any_starred,
                latest,
            });
            if expanded {
                let rest: Vec<Message> = it.collect();
                let n = rest.len();
                for (j, child) in rest.into_iter().enumerate() {
                    shown.push(child);
                    metas.push(RowMeta {
                        count: 0,
                        expandable: false,
                        is_child: true,
                        is_last: j + 1 == n,
                        from: None,
                        preview: None,
                        expanded: false,
                        key: None,
                        unread: false,
                        starred: false,
                        latest: None,
                    });
                }
            }
        }
        // Growing the page (LoadMore) leaves its existing rows as they are
        // when nothing about them changed — same messages in the same
        // order, same conversation shape — and only the new rows get built.
        // Anything else (a switch, new mail at the top, a thread that took
        // in a newly listed member) rebuilds from the top.
        let sigs: Vec<(usize, bool, bool, bool, bool, bool, bool)> = metas
            .iter()
            .map(|m| {
                (m.count, m.expandable, m.is_child, m.is_last, m.expanded, m.unread, m.starred)
            })
            .collect();
        let old_len = self.shown.len();
        let append_only = !std::mem::take(&mut self.rows_stale)
            && old_len > 0
            && self.pending_rows.is_empty()
            && self.rows.len() == old_len
            && shown.len() >= old_len
            && shown[..old_len]
                .iter()
                .zip(&self.shown)
                .all(|(a, b)| (a.account_id, a.id) == (b.account_id, b.id))
            && sigs[..old_len] == self.row_sigs[..];
        // A message that was moved away and brought back comes home under a
        // new UID, and a row's id is its UID. The selection would find no row
        // to light, so the highlight blinked off and came back a moment later
        // as the list caught up. Follow the selection by Message-ID over a
        // renumbering instead (#200).
        let lost = self
            .selected_ids
            .iter()
            .any(|k| !shown.iter().any(|m| (m.account_id, m.id) == *k));
        if lost {
            let renumbered: Vec<((u32, u32), (u32, u32))> = self
                .selected_ids
                .iter()
                .filter(|k| !shown.iter().any(|m| (m.account_id, m.id) == **k))
                .filter_map(|k| {
                    let was = self
                        .shown
                        .iter()
                        .find(|m| (m.account_id, m.id) == *k)
                        .filter(|m| !m.message_id.is_empty())?;
                    let now = shown
                        .iter()
                        .find(|m| m.account_id == was.account_id && m.message_id == was.message_id)?;
                    Some((*k, (now.account_id, now.id)))
                })
                .collect();
            for (was, now) in renumbered {
                for k in self.selected_ids.iter_mut().filter(|k| **k == was) {
                    *k = now;
                }
                if self.selected_id == Some(was) {
                    self.selected_id = Some(now);
                }
            }
        }
        self.shown = shown;
        self.row_sigs = sigs;
        // Republish the row keys before the rows are built, so a drag starting on
        // any of them can map selected row indices back to messages.
        self.publish_drag_keys();

        // Expanded conversations indent their member cards; give the pane the
        // extra floor that needs while any thread is open, so nothing is
        // clipped at the right edge (see THREAD_EXPANDED_EXTRA).
        if let Some(s) = &self.scroller {
            let any_expanded = metas.iter().any(|meta| meta.is_child);
            let floor =
                LIST_MIN_WIDTH + if any_expanded { THREAD_EXPANDED_EXTRA } else { 0 };
            s.set_size_request(floor, -1);
        }

        let t_rows = std::time::Instant::now();
        {
            // Build the rows' inits now, the widgets for the first few now
            // and the rest at idle: the pane fills at once and the page
            // completes a chunk at a time without holding the main loop.
            let mut inits: std::collections::VecDeque<RowInit> = std::collections::VecDeque::new();
            let skip = if append_only { old_len } else { 0 };
            for (m, meta) in self.shown.iter().zip(metas.into_iter()).skip(skip) {
                let ring_class = if self.colorize && self.account_colors.contains_key(&m.account_id) {
                    Some(format!("vireo-acct-ring-{}", m.account_id))
                } else {
                    None
                };
                inits.push_back(RowInit {
                    msg: m.clone(),
                    gravatar: self.gravatar,
                    avatars: self.avatars,
                    avatar_late: self.reveal_avatars_late,
                    sender_logos: self.sender_logos,
                    preview_lines: self.preview_lines,
                    show_subject: self.show_subject,
                    ring_class,
                    palette_collapse_secs: self.palette_collapse_secs.clone(),
                    palette_hover: self.palette_hover.clone(),
                    tags: self.tags.clone(),
                    show_palette: self.list_palette,
                    in_junk: self.in_junk,
                    in_drafts: self.in_drafts,
                    thread_count: meta.count,
                    is_thread_child: meta.is_child,
                    is_last_child: meta.is_last,
                    thread_expanded: meta.expanded,
                    thread_expandable: self.thread_expansion && meta.expandable,
                    thread_key: meta.key,
                    thread_date: meta.latest,
                    thread_from: meta.from,
                    thread_preview: meta.preview,
                    thread_unread: meta.unread,
                    thread_starred: meta.starred,
                    drag_keys: self.drag_keys.clone(),
                        thread_drag: self.thread_drag.clone(),
                    show_recipient: self.show_recipient,
                    // A full rebuild never needs a row to mount closed —
                    // that's only for `expand_thread`'s surgical insert.
                    revealed: true,
                    swipe_reversed: self.swipe_reversed.clone(),
                    swipe_enabled: self.swipe_enabled.clone(),
                    swipe_sensitivity: self.swipe_sensitivity.clone(),
                });
            }
            if !append_only {
                self.discard_rows();
            }
            self.pending_rows = inits;
            self.fill_rows(FIRST_ROWS);
        }

        tracing::debug!(
            "list: rebuild {} of {} rows — sort+group {:?}, first {} rows {:?}",
            self.rendered_count,
            total_matches,
            t_rows.duration_since(t_rebuild),
            self.rows.len(),
            t_rows.elapsed()
        );
    }

    /// The factory behind the rows, bound to a fresh list box.
    fn new_rows(input: &relm4::Sender<MessageListInput>) -> FactoryVecDeque<MessageRow> {
        FactoryVecDeque::builder()
            .launch(gtk::ListBox::new())
            .forward(input, |out| match out {
                MessageRowOutput::Action { action, message } => {
                    MessageListInput::RowAction { action, message }
                }
                MessageRowOutput::SetTag { message, keyword, add } => {
                    MessageListInput::SetTagFor { message, keyword, add }
                }
                MessageRowOutput::MoveTo { message, x, y } => {
                    MessageListInput::RowMoveTo { message, x, y }
                }
                MessageRowOutput::ToggleThread(key) => MessageListInput::ToggleThread(key),
                MessageRowOutput::PaletteOpened(idx) => MessageListInput::PaletteOpened(idx),
            })
    }

    /// Everything a row list needs wired: the first one from the view, and
    /// each one that replaces it on a folder switch.
    fn wire_list(list: &gtk::ListBox, input: &relm4::Sender<MessageListInput>) {
        // Multiple selection: plain click selects one (shown in the
        // reader), Ctrl/Shift extend the selection for bulk actions;
        // double click (or Enter) pops a message out into its own window.
        list.set_selection_mode(gtk::SelectionMode::Multiple);
        list.set_activate_on_single_click(false);
        list.add_css_class("message-listbox");
        let s = input.clone();
        list.connect_selected_rows_changed(move |_| {
            let _ = s.send(MessageListInput::SelectionChanged);
        });
        let s = input.clone();
        list.connect_row_activated(move |_, row| {
            let _ = s.send(MessageListInput::RowActivated(row.index()));
        });
        // Right-click a row to open its context menu. In the capture phase,
        // so the press reaches the list before any widget inside the row
        // (a conversation head's count chip, the hover palette's buttons)
        // can take it; claimed, so none of them acts on it afterwards.
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_SECONDARY);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let s = input.clone();
        click.connect_pressed(move |g, _, x, y| {
            g.set_state(gtk::EventSequenceState::Claimed);
            let _ = s.send(MessageListInput::ContextMenu { x, y });
        });
        list.add_controller(click);
        // Delete / Backspace on a focused row deletes the selection (single or
        // multi). Scoped to the list, so typing in the search box is unaffected.
        let key = gtk::EventControllerKey::new();
        let s = input.clone();
        key.connect_key_pressed(move |_, keyval, _, _| {
            if matches!(keyval, gtk::gdk::Key::Delete | gtk::gdk::Key::BackSpace) {
                // ResolveDelete, not a straight Bulk: a lone thread-head row
                // stands for its whole conversation (confirmed by the app).
                let _ = s.send(MessageListInput::ResolveDelete);
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            }
        });
        list.add_controller(key);
    }

    /// Drop the current page's rows for a new page. A few rows go at once;
    /// a full page is taken out of the pane whole — a fresh list box takes
    /// its place — and torn down at idle, a chunk at a time, so the switch
    /// never waits on the old rows' destruction.
    fn discard_rows(&mut self) {
        const RETIRE_MIN: usize = 40;
        if self.rows.len() <= RETIRE_MIN {
            self.rows.guard().clear();
            return;
        }
        let old_list = self.rows.widget().clone();
        let fresh = Self::new_rows(&self.input);
        Self::wire_list(fresh.widget(), &self.input);
        if let Some(parent) = old_list.parent().and_downcast::<gtk::Box>() {
            parent.insert_child_after(fresh.widget(), None::<&gtk::Widget>);
        }
        // Hidden now, unparented once its rows are gone: unparenting a full
        // list box is itself a slow step, and it can wait with the rest.
        old_list.set_visible(false);
        let old = std::mem::replace(&mut self.rows, fresh);
        self.retired.push(old);
        self.schedule_retire();
    }

    fn schedule_retire(&mut self) {
        if self.retire_scheduled {
            return;
        }
        self.retire_scheduled = true;
        let input = self.input.clone();
        glib::idle_add_local_once(move || {
            let _ = input.send(MessageListInput::RetireRows);
        });
    }

    /// Tear down a chunk of the oldest retired list, then come back for more.
    fn retire_rows(&mut self) {
        const RETIRE_CHUNK: usize = 25;
        let Some(old) = self.retired.first_mut() else { return };
        {
            let mut guard = old.guard();
            for _ in 0..RETIRE_CHUNK {
                if guard.pop_front().is_none() {
                    break;
                }
            }
        }
        if old.is_empty() {
            let list = old.widget().clone();
            if let Some(parent) = list.parent().and_downcast::<gtk::Box>() {
                parent.remove(&list);
            }
            self.retired.remove(0);
        }
        if !self.retired.is_empty() {
            self.schedule_retire();
        }
    }

    /// Build up to `n` of the pending rows, then schedule the next chunk if
    /// any remain. The highlight is restored after each chunk, since the
    /// viewed message's row may only just have been built.
    fn fill_rows(&mut self, n: usize) {
        if self.pending_rows.is_empty() {
            return;
        }
        {
            let mut guard = self.rows.guard();
            for _ in 0..n {
                let Some(mut init) = self.pending_rows.pop_front() else { break };
                // A change that landed while the row was pending (read,
                // starred, a tag) is in `shown`; the init was made earlier.
                if let Some(m) = self.shown.get(guard.len()) {
                    init.msg = m.clone();
                }
                guard.push_back(init);
            }
        }
        self.select_current();
        if !self.pending_rows.is_empty() && !self.fill_scheduled {
            self.fill_scheduled = true;
            let input = self.input.clone();
            glib::idle_add_local_once(move || {
                let _ = input.send(MessageListInput::FillRows);
            });
        }
    }

    /// Build every pending row now — before anything that addresses rows by
    /// index structurally (removing, inserting a thread's replies).
    fn flush_rows(&mut self) {
        let n = self.pending_rows.len();
        if n > 0 {
            self.fill_rows(n);
        }
    }

    /// Send to the row at `idx` if it is built; a row still pending takes
    /// the change from `shown` when it is built instead (the handlers keep
    /// `shown` current before sending).
    fn row_send(&self, idx: usize, msg: MessageRowInput) {
        if idx < self.rows.len() {
            self.rows.send(idx, msg);
        }
    }

    /// Ask for a rebuild at the end of the current main-loop pass, folding
    /// any further requests before then into it. A request that must not
    /// keep the scroll offset (a folder switch) wins over ones that would.
    fn queue_rebuild(&mut self, preserve_scroll: bool) {
        let first = self.rebuild_queued.is_none();
        let preserve = self.rebuild_queued.map_or(preserve_scroll, |p| p && preserve_scroll);
        self.rebuild_queued = Some(preserve);
        if first {
            // Ahead of GTK's layout and paint, so the old rows are never
            // laid out one more time for nothing before they go.
            let input = self.input.clone();
            glib::idle_add_local_full(glib::Priority::HIGH, move || {
                let _ = input.send(MessageListInput::RunQueuedRebuild);
                glib::ControlFlow::Break
            });
        }
    }

    /// Republish the shown rows' keys for drag-and-drop. Row indices shift
    /// whenever rows are added or removed, so this must follow every change to
    /// `shown` — a stale mapping would drag the wrong messages (#23).
    fn publish_drag_keys(&self) {
        *self.drag_keys.borrow_mut() = self
            .shown
            .iter()
            .map(|m| (m.account_id, m.folder_id, m.uid, m.id))
            .collect();
        // Each conversation head's members, for a drag that starts on it.
        let mut threads = std::collections::HashMap::new();
        if self.threading {
            let source = self.active_source();
            for m in &self.shown {
                let key = (m.account_id, m.id);
                let Some(tkey) = self.msg_thread.get(&key) else { continue };
                let Some(members) = self.thread_members.get(tkey) else { continue };
                if members.len() > 1 && members.first() == Some(&key) {
                    let items: Vec<(u32, u32, u32, u32)> = members
                        .iter()
                        .filter_map(|mk| source.iter().find(|x| (x.account_id, x.id) == *mk))
                        .map(|x| (x.account_id, x.folder_id, x.uid, x.id))
                        .collect();
                    threads.insert(key, items);
                }
            }
        }
        *self.thread_drag.borrow_mut() = threads;
    }

    /// The shown row (a thread head) whose conversation holds the message
    /// `key`, when `key` is in the index but has no row of its own.
    fn thread_head_for(&self, key: (u32, u32)) -> Option<(u32, u32)> {
        if !self.threading {
            return None;
        }
        let source = self.active_source();
        source.iter().find(|m| (m.account_id, m.id) == key)?;
        let keys = compute_thread_keys(source, &self.thread_links);
        let thread = keys.get(&key)?;
        self.shown
            .iter()
            .find(|m| keys.get(&(m.account_id, m.id)) == Some(thread))
            .map(|m| (m.account_id, m.id))
    }

    /// Every on-screen member of `m`'s conversation (oldest first) — from any
    /// member, head or reply. Empty when threading is off or `m` stands alone,
    /// so callers can treat non-empty as "this is a real thread".
    /// The conversation a row stands for, or empty when the row means only its
    /// own message. A row stands for its thread when it is the head of one and
    /// the thread is not currently opened out in the list — which, with
    /// expandable conversations off, it never is.
    fn row_conversation(&self, m: &Message) -> Vec<Message> {
        let members = self.thread_members(m);
        if members.is_empty()
            || !heads_its_row((m.account_id, m.id), &self.msg_thread, &self.thread_members)
        {
            return Vec::new();
        }
        let expanded = self.thread_expansion
            && self
                .msg_thread
                .get(&(m.account_id, m.id))
                .is_some_and(|key| {
                    self.expanded_threads.contains(key) != self.default_expanded
                });
        if expanded {
            Vec::new()
        } else {
            members
        }
    }

    fn thread_members(&self, m: &Message) -> Vec<Message> {
        if !self.threading {
            return Vec::new();
        }
        let source = self.active_source();
        let keys = compute_thread_keys(source, &self.thread_links);
        let Some(key) = keys.get(&(m.account_id, m.id)).cloned() else {
            return Vec::new();
        };
        let mut members: Vec<Message> = source
            .iter()
            .filter(|x| keys.get(&(x.account_id, x.id)) == Some(&key))
            .cloned()
            .collect();
        if members.len() <= 1 {
            return Vec::new();
        }
        members.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.uid.cmp(&b.uid)));
        members
    }

    /// The conversation to show for a selected message: when `m` is the oldest
    /// (head) of a multi-message thread, every message in it (oldest first);
    /// otherwise just `m` (so opening an individual reply shows only that one).
    fn conversation_for(&self, m: &Message) -> (Vec<Message>, bool) {
        if !self.threading {
            return (vec![m.clone()], false);
        }
        // Thread within whatever set is on screen (the search pool while searching,
        // otherwise the current folder) so the conversation matches the rows shown.
        let source = self.active_source();
        let keys = compute_thread_keys(source, &self.thread_links);
        let Some(key) = keys.get(&(m.account_id, m.id)).cloned() else {
            return (vec![m.clone()], false);
        };
        let mut members: Vec<Message> = source
            .iter()
            .filter(|x| keys.get(&(x.account_id, x.id)) == Some(&key))
            .cloned()
            .collect();
        if members.len() <= 1 {
            // Nothing else here to thread with. It may still have siblings in
            // another folder, so this is *not* solo — the reader may look.
            return (vec![m.clone()], false);
        }
        // Oldest first, matching the rows: the head is the message that opened
        // the conversation, and opening it shows the whole thread in order.
        members.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.uid.cmp(&b.uid)));
        // Whether `m` is the row, judged over the window the rows came from
        // (#236): a conversation whose start lies past the rendered window is
        // still one row, and that row stands for all of it.
        if heads_its_row((m.account_id, m.id), &self.msg_thread, &self.thread_members) {
            (members, false)
        } else {
            // A reply picked out of a conversation that is on screen: show it by
            // itself and leave it that way.
            (vec![m.clone()], true)
        }
    }

    /// Select row `idx` and put the keyboard focus on it.
    ///
    /// Focus matters after a removal: destroying the focused row leaves GTK to
    /// pick a fallback of its own, which can be the top of the list — and moving
    /// focus scrolls the viewport with it, so the list appears to jump away from
    /// where the user was working (#19). Taking focus deliberately also means the
    /// single-key shortcuts carry on from the row that is now selected.
    fn select_and_focus(&self, idx: usize) {
        let list = self.rows.widget();
        if let Some(row) = list.row_at_index(idx as i32) {
            list.select_row(Some(&row));
            row.grab_focus();
        }
    }

    /// Where deletion advances to, once the row at `idx` is gone: the row now
    /// occupying that slot when the user was moving down the list, the row
    /// above it when they were moving up. Clamped to the list either way.
    fn advance_index(&self, idx: usize) -> usize {
        let next = if self.nav_direction < 0 { idx.saturating_sub(1) } else { idx };
        next.min(self.shown.len().saturating_sub(1))
    }

    /// Put keyboard focus on row `idx` without touching selection — the
    /// counterpart to `select_and_focus` for a row that wasn't the one being
    /// viewed. Used when a removed row held focus but wasn't the viewed
    /// message (e.g. deleted via its own row action while browsing further
    /// down the list): without this, GTK's own fallback focus assignment
    /// scrolls the list away to wherever it lands instead of staying put.
    fn focus_only(&self, idx: usize) {
        let list = self.rows.widget();
        if let Some(row) = list.row_at_index(idx as i32) {
            row.grab_focus();
        }
        self.hide_focus_ring();
    }

    /// Drop the window's focus-visible flag after a *programmatic* focus grab:
    /// the reclaimed row keeps keyboard focus (arrow keys resume from it), but
    /// no accent focus ring appears around a row the user never navigated to.
    /// The next real key press turns the ring back on, as normal.
    fn hide_focus_ring(&self) {
        if let Some(win) = self
            .rows
            .widget()
            .root()
            .and_then(|r| r.downcast::<gtk::Window>().ok())
        {
            win.set_focus_visible(false);
        }
    }

    /// Re-apply the whole selection (the viewed message plus any multi-selected
    /// rows) so it persists across rebuilds — background syncs included — until
    /// the user clicks away. Called after a rebuild, when rows are freshly built
    /// and nothing is selected yet.
    fn select_current(&self) {
        let list = self.rows.widget();
        if self.selected_ids.is_empty() {
            list.unselect_all();
            return;
        }
        for key in &self.selected_ids {
            if let Some(idx) = self.shown.iter().position(|m| (m.account_id, m.id) == *key) {
                if let Some(row) = list.row_at_index(idx as i32) {
                    list.select_row(Some(&row));
                }
            }
        }
    }

    /// Update the display-wide CSS that rings each account's avatar with its
    /// colour (used in the unified "All Inboxes" view to identify the account).
    fn refresh_tint_css(&self) {
        let mut css = String::new();
        for (id, color) in &self.account_colors {
            css.push_str(&format!(
                ".vireo-acct-ring-{0} {{ border-radius: 9999px; box-shadow: 0 0 0 3px {1}; }}\n",
                id, color
            ));
        }
        self.color_provider.load_from_data(&css);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compute_thread_keys, heads_its_row, reader_conversation, row_for_reader_key,
        swipe_progress_px, unasked_threads, SWIPE_ARM, SWIPE_MAX,
    };
    use crate::models::Message;

    /// #236: the row a conversation collapses to is judged over the rendered
    /// window's grouping, so a conversation whose oldest message lies past
    /// the window still opens whole from its row.
    #[test]
    fn a_row_heads_its_conversation_within_the_rendered_window() {
        use std::collections::HashMap;
        let key = (1u32, "root@x".to_string());
        // The window holds uids 30 and 40 of the conversation; uid 10, its
        // real start, is past the window and so in neither map.
        let mut msg_thread = HashMap::new();
        msg_thread.insert((1, 30), key.clone());
        msg_thread.insert((1, 40), key.clone());
        let mut thread_members = HashMap::new();
        thread_members.insert(key, vec![(1, 30), (1, 40)]);

        assert!(heads_its_row((1, 30), &msg_thread, &thread_members), "the oldest shown is the row");
        assert!(!heads_its_row((1, 40), &msg_thread, &thread_members), "a child row is not");
        // A message grouped with nothing in the window is its own row, and the
        // reader is still free to look for the rest of it in the cache.
        assert!(heads_its_row((1, 10), &msg_thread, &thread_members));
        assert!(heads_its_row((2, 7), &msg_thread, &thread_members));
    }

    fn msg(id: u32, message_id: &str, references: &str) -> Message {
        Message {
            id,
            account_id: 1,
            folder_id: 1,
            uid: id,
            from_name: "X".into(),
            from_addr: "x@example.com".into(),
            reply_to: String::new(),
            to: String::new(),
            cc: String::new(),
            subject: "S".into(),
            preview: String::new(),
            body: String::new(),
            date: String::new(),
            timestamp: 1000,
            unread: false,
            starred: false,
            keywords: Vec::new(),
            has_attachment: false,
            message_id: message_id.into(),
            references: references.into(),
        }
    }

    /// Two replies in an Inbox each answer a different message in Sent, and
    /// reference nothing else — so within the Inbox they share no id at all.
    /// They are one conversation, and the messages that say so are the ones in
    /// Sent, which the folder on screen never shows.
    #[test]
    fn a_conversation_joined_through_another_folder_still_groups() {
        let shown = [
            msg(1, "reply-a@them", "sent-1@us"),
            msg(2, "reply-b@them", "sent-2@us"),
        ];
        // Without the Sent messages there is nothing to join them.
        let alone = compute_thread_keys(&shown, &[]);
        assert_ne!(
            alone.get(&(1, 1)),
            alone.get(&(1, 2)),
            "nothing on screen links these two"
        );

        // Sent 2 replied to reply-a, which replied to Sent 1: one conversation.
        let links = vec![
            (1u32, "sent-1@us".to_string(), String::new()),
            (1u32, "sent-2@us".to_string(), "sent-1@us reply-a@them".to_string()),
        ];
        let joined = compute_thread_keys(&shown, &links);
        assert_eq!(
            joined.get(&(1, 1)),
            joined.get(&(1, 2)),
            "the messages in Sent say they belong together"
        );
    }

    /// A re-added account re-downloads its whole mailbox, so every message in it
    /// is older than the moment the account was added. Threading reads the reply
    /// headers, which say the same thing whenever the mail was sent — the three
    /// messages here are the shape iCloud delivered: a root with no References,
    /// and two replies naming it.
    #[test]
    fn mail_older_than_the_account_still_threads() {
        let old = 1_787_565_140i64; // long before this list was ever built
        let mut root = msg(1, "root@dccma.com", "");
        root.timestamp = old;
        let mut first = msg(2, "r1@dccma.com", "root@dccma.com sent-1@me.com");
        first.timestamp = old + 48;
        let mut second = msg(3, "r2@dccma.com", "root@dccma.com sent-2@me.com");
        second.timestamp = old + 224;

        let shown = [root, first, second];
        let keys = compute_thread_keys(&shown, &[]);
        let root_key = keys.get(&(1, 1)).cloned().expect("the root is threaded");
        assert_eq!(keys.get(&(1, 2)), Some(&root_key), "first reply joins");
        assert_eq!(keys.get(&(1, 3)), Some(&root_key), "second reply joins");
    }

    fn listed(items: &[(u32, &str)]) -> Vec<(u32, String, Vec<String>)> {
        items.iter().map(|(a, r)| (*a, r.to_string(), vec![format!("{r}@x")])).collect()
    }

    /// The counts come back as a rebuild, and a rebuild is what decides to ask.
    /// If asking were driven by the page alone, that would be a loop; it is
    /// driven by what has not been asked yet, so the second pass is silent.
    #[test]
    fn a_page_is_only_asked_about_once() {
        let page = listed(&[(1, "a"), (1, "b")]);
        let mut asked = std::collections::HashSet::new();

        let first = unasked_threads(&page, &asked);
        assert_eq!(first.len(), 2, "nothing counted yet, so ask about both");
        for (aid, root, _) in &first {
            asked.insert((*aid, root.clone()));
        }
        assert!(unasked_threads(&page, &asked).is_empty(), "the answer must not start another round");
    }

    /// A search re-filters the page on every keystroke, and each rebuild would
    /// otherwise be a fresh scan of the message index.
    #[test]
    fn narrowing_the_page_asks_nothing_further() {
        let page = listed(&[(1, "a"), (1, "b"), (1, "c")]);
        let asked: std::collections::HashSet<(u32, String)> =
            page.iter().map(|(a, r, _)| (*a, r.clone())).collect();

        let narrowed = listed(&[(1, "b")]);
        assert!(unasked_threads(&narrowed, &asked).is_empty());
    }

    /// New mail brings threads nobody has counted; only those are asked about.
    #[test]
    fn only_the_new_conversations_are_asked_about() {
        let asked: std::collections::HashSet<(u32, String)> =
            [(1u32, "a".to_string())].into_iter().collect();
        let page = listed(&[(1, "a"), (1, "new")]);

        let fresh = unasked_threads(&page, &asked);
        assert_eq!(fresh.iter().map(|(_, r, _)| r.as_str()).collect::<Vec<_>>(), vec!["new"]);
    }

    /// Two accounts can root a thread at the same Message-ID (the unified
    /// inbox shows both), and they are different conversations in different
    /// caches — asking about one must not silence the other.
    #[test]
    fn the_same_thread_root_in_two_accounts_is_two_questions() {
        let asked: std::collections::HashSet<(u32, String)> =
            [(1u32, "shared".to_string())].into_iter().collect();
        let page = listed(&[(1, "shared"), (2, "shared")]);

        let fresh = unasked_threads(&page, &asked);
        assert_eq!(fresh.iter().map(|(a, _, _)| *a).collect::<Vec<_>>(), vec![2]);
    }

    /// Links are evidence, not glue: unrelated mail must not be pulled in.
    #[test]
    fn links_do_not_merge_unrelated_conversations() {
        let shown = [msg(1, "a@x", ""), msg(2, "b@x", "")];
        let links = vec![(1u32, "c@x".to_string(), "a@x".to_string())];
        let keys = compute_thread_keys(&shown, &links);
        assert_ne!(keys.get(&(1, 1)), keys.get(&(1, 2)), "still two conversations");
    }

    /// A long conversation groups whole. What it may drag in from *other*
    /// folders is bounded by the cache's own per-thread limit; what is already
    /// in the folder on screen is shown in full.
    #[test]
    fn a_long_conversation_groups_every_message() {
        let n = 60usize;
        let mut members: Vec<Message> = Vec::new();
        for i in 0..n {
            let mut m = msg(i as u32 + 1, &format!("m{i}@x"), "root@x");
            m.timestamp = 1000 + i as i64;
            members.push(m);
        }
        // All one conversation by their shared reference.
        let keys = compute_thread_keys(&members, &[]);
        let root = keys.get(&(1, 1)).cloned().expect("threaded");
        assert!(
            members.iter().all(|m| keys.get(&(1, m.id)) == Some(&root)),
            "one conversation"
        );
        assert_eq!(
            members.iter().filter(|m| keys.get(&(1, m.id)) == Some(&root)).count(),
            n,
            "every message belongs to it, however long the thread runs"
        );
    }

    /// libadwaita's own scale for a touchpad's two-finger scroll: it spends a
    /// fixed 400px of horizontal delta on a full swipe, whatever the widget's
    /// `distance` says, which is why the preference exists at all.
    const TOUCHPAD_BASE: f64 = 400.0;

    #[test]
    fn mouse_drag_tracks_the_pointer_at_every_sensitivity() {
        // `AdwSwipeTracker` divides a drag by `SwipeSurface::distance`, which
        // is SWIPE_MAX * sensitivity, so the row must come back out at the
        // pointer's own px however the preference is set.
        for sensitivity in [1.0, 3.5, 10.0] {
            for dragged in [10.0, 72.0, 120.0] {
                let progress = dragged / (SWIPE_MAX * sensitivity);
                assert!(
                    (swipe_progress_px(progress, sensitivity) - dragged).abs() < 0.001,
                    "{dragged}px drag at {sensitivity}"
                );
            }
        }
    }

    #[test]
    fn sensitivity_shortens_the_trackpad_swipe() {
        // How much two-finger scroll it takes to reach the commit distance.
        let travel = |sensitivity: f64| {
            (1..=2000)
                .map(|px| px as f64)
                .find(|px| {
                    swipe_progress_px(px / TOUCHPAD_BASE, sensitivity).abs() >= SWIPE_ARM
                })
                .expect("armed eventually")
        };
        // Untuned, a trackpad has to travel further than most can in one go —
        // the complaint behind the setting.
        assert_eq!(travel(1.0), 240.0);
        // The default puts it within a comfortable swipe, and raising it
        // further keeps shortening it.
        assert!(travel(3.5) < 70.0, "default is a short swipe");
        assert!(travel(10.0) < travel(3.5), "higher is always shorter");
    }

    #[test]
    fn a_long_swipe_stops_at_the_action_strip() {
        for sensitivity in [1.0, 10.0] {
            assert_eq!(swipe_progress_px(1.0, sensitivity), SWIPE_MAX);
            assert_eq!(swipe_progress_px(-1.0, sensitivity), -SWIPE_MAX);
        }
    }

    #[test]
    fn preview_lines_clamp_but_keep_zero() {
        // 0 is "off"; anything above 3 is a hand-edited file, not a setting.
        for (asked, expected) in [(0u32, 0u32), (1, 1), (3, 3), (9, 3)] {
            assert_eq!(asked.min(3), expected, "for {asked}");
        }
    }

    /// Clicking a reply's card keeps the conversation's row selected even when
    /// the list never shows that reply a row of its own (#211).
    #[test]
    fn a_hidden_reply_selects_its_conversation_row() {
        use std::collections::HashMap;
        // The list shows the head of a three-message conversation, and one
        // unrelated message below it.
        let shown = [msg(1, "head@them", ""), msg(9, "other@them", "")];
        let thread = (1u32, "head@them".to_string());
        let msg_thread: HashMap<(u32, u32), (u32, String)> = [
            ((1, 1), thread.clone()),
            ((1, 2), thread.clone()),
            ((1, 3), thread.clone()),
        ]
        .into_iter()
        .collect();

        // The conversation as this list handed it over, opened from its head
        // row. A message of the user's own, pulled in from Sent, joins it
        // only later and only on the app's side: this folder has no row for
        // it, no thread entry, and never listed it at all (#220).
        let emitted = [(1, 1), (1, 2), (1, 3)];
        let merged = [(1, 1), (1, 2), (1, 3), (1, 77)];
        let conversation = reader_conversation(&emitted, &merged);
        assert_eq!(conversation, [(1, 1), (1, 2), (1, 3), (1, 77)]);
        let viewed = Some((1, 1));
        let row = |key: (u32, u32)| {
            row_for_reader_key(&key, &shown, &msg_thread, &conversation, viewed)
        };

        // The head has a row of its own.
        assert_eq!(row((1, 1)), Some(0));
        // Its replies do not, and land on the head's row rather than nowhere.
        assert_eq!(row((1, 2)), Some(0));
        assert_eq!(row((1, 3)), Some(0));
        // Neither does the copy from Sent, which this folder never lists.
        assert_eq!(row((1, 77)), Some(0));
        // Going by what the list handed over alone, that copy would land
        // nowhere — the bug behind #220.
        assert_eq!(row_for_reader_key(&(1, 77), &shown, &msg_thread, &emitted, viewed), None);
        // A message in no conversation still matches only itself.
        assert_eq!(row((1, 9)), Some(1));
        // And one from neither is no row at all.
        assert_eq!(row((1, 42)), None);
    }
}
