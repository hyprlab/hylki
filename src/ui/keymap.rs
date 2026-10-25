//! Keyboard shortcuts as data: which key does what in the main window, under
//! the single-key and Gmail-style maps, and the focus checks that decide
//! whether a key is the window's or the focused editor's.

use adw::prelude::*;

use crate::models::FolderKind;
use crate::ui::message_list::CheckFilter;

/// One single-key shortcut. Gmail-compatible where Gmail and the request agree
/// (issue #5); where they differ, the request wins — `a` archives here, rather
/// than replying to all as it does in Gmail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shortcut {
    NextMessage,
    PrevMessage,
    OpenMessage,
    BackToList,
    NextInThread,
    PrevInThread,
    ToggleSelect,
    Reply,
    ReplyAll,
    Forward,
    Archive,
    Delete,
    /// Shift+D: erase for good, skipping Trash (#471).
    DeleteForever,
    Spam,
    Star,
    ToggleRead,
    Compose,
    Search,
    Shortcuts,
    /// Add or remove the Nth tag (1-based, in Settings order) on the
    /// message being read (#157).
    Tag(u8),
    /// Take every tag off the message being read.
    ClearTags,
    /// Gmail's j/k (#392): the keyboard cursor moves, nothing opens.
    MoveCursor(i32),
    /// Gmail's o: open the row the cursor is on.
    OpenCursor,
    /// Gmail's Shift+I (read) and Shift+U (unread): the whole conversation
    /// when one is open.
    MarkRead(bool),
    /// Gmail's _: unread from the message being read onward.
    MarkUnreadFrom,
    /// Gmail's ; (expand, `false`) and : (collapse, `true`): every message
    /// of the open conversation.
    FoldAll(bool),
    /// Gmail's z.
    Undo,
    /// Gmail's v: the Move To picker.
    MoveTo,
    /// Gmail's b (z here): the Snooze menu (#527).
    Snooze,
    /// Gmail's l: the tag menu.
    Label,
    /// Gmail's g then a letter.
    GoTo(GoTarget),
    /// Gmail's * then a or n.
    SelectAll,
    SelectNone,
    /// Gmail's * then r, u, s or t.
    SelectWhere(CheckFilter),
    /// Gmail's ] and } (1) and [ and { (-1): archive the conversation and
    /// open the older or newer one.
    ArchiveAndStep(i32),
    /// Gmail's Shift+R, Shift+A, Shift+F and d.
    InWindow(WindowCompose),
    /// Gmail's Shift+N.
    Refresh,
}

impl Shortcut {
    /// Whether the key acts on the selection, so it needs one (#392).
    pub(crate) fn acts_on_selection(self) -> bool {
        matches!(
            self,
            Shortcut::Archive
                | Shortcut::ArchiveAndStep(_)
                | Shortcut::Delete
                | Shortcut::DeleteForever
                | Shortcut::Spam
                | Shortcut::Star
                | Shortcut::ToggleRead
                | Shortcut::MarkRead(_)
                | Shortcut::MarkUnreadFrom
                | Shortcut::MoveTo
                | Shortcut::Snooze
                | Shortcut::Label
                | Shortcut::Tag(_)
                | Shortcut::ClearTags
        )
    }
}

/// What Gmail's window keys write, in a composer window whatever Settings
/// says about inline composers (#392).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowCompose {
    Reply,
    ReplyAll,
    Forward,
    New,
}

/// Where Gmail's g sequences go (#392).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GoTarget {
    Kind(FolderKind),
    Contacts,
}

/// Which single keys are on, for the key hints in the menus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyMode {
    Off,
    Single,
    Gmail,
}

thread_local! {
    /// What Settings has on, kept by the app as it changes, so a menu built
    /// anywhere names the keys that work.
    static KEY_MODE: std::cell::Cell<KeyMode> = const { std::cell::Cell::new(KeyMode::Off) };
}

/// Note the single-key settings for the menus' hints. Gmail's keys need
/// single keys on as well.
pub(crate) fn set_key_mode(single: bool, gmail: bool) {
    let mode = match (single, gmail) {
        (false, _) => KeyMode::Off,
        (true, false) => KeyMode::Single,
        (true, true) => KeyMode::Gmail,
    };
    KEY_MODE.with(|m| m.set(mode));
}

pub(crate) fn key_mode() -> KeyMode {
    KEY_MODE.with(|m| m.get())
}

/// The number key for the tag at `index` in Settings order (#157), for a
/// menu's hint: the first nine have one, while single keys are on.
pub(crate) fn tag_key(index: usize) -> Option<&'static str> {
    const DIGITS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];
    match key_mode() {
        KeyMode::Off => None,
        KeyMode::Single | KeyMode::Gmail => DIGITS.get(index).copied(),
    }
}

/// The shortcut for a key press, if any. `shift` distinguishes `r` from `R`.
pub(crate) fn shortcut_for(key: gtk::gdk::Key, shift: bool) -> Option<Shortcut> {
    use gtk::gdk::Key;
    let action = match key {
        Key::j | Key::Down => Shortcut::NextMessage,
        Key::k | Key::Up => Shortcut::PrevMessage,
        Key::l | Key::Right => Shortcut::OpenMessage,
        Key::h | Key::Left | Key::u => Shortcut::BackToList,
        Key::w => Shortcut::NextInThread,
        Key::b => Shortcut::PrevInThread,
        Key::x => Shortcut::ToggleSelect,
        Key::r if !shift => Shortcut::Reply,
        Key::R => Shortcut::ReplyAll,
        Key::f => Shortcut::Forward,
        Key::a => Shortcut::Archive,
        Key::d => Shortcut::Delete,
        Key::D => Shortcut::DeleteForever,
        Key::exclam => Shortcut::Spam,
        Key::s => Shortcut::Star,
        Key::m => Shortcut::ToggleRead,
        // b steps back through a conversation here: Gmail's key for it is
        // taken.
        Key::z => Shortcut::Snooze,
        Key::c => Shortcut::Compose,
        Key::slash => Shortcut::Search,
        Key::question => Shortcut::Shortcuts,
        // Tags by number, the way Evolution labels mail (#157). 0 clears.
        Key::_1 | Key::KP_1 => Shortcut::Tag(1),
        Key::_2 | Key::KP_2 => Shortcut::Tag(2),
        Key::_3 | Key::KP_3 => Shortcut::Tag(3),
        Key::_4 | Key::KP_4 => Shortcut::Tag(4),
        Key::_5 | Key::KP_5 => Shortcut::Tag(5),
        Key::_6 | Key::KP_6 => Shortcut::Tag(6),
        Key::_7 | Key::KP_7 => Shortcut::Tag(7),
        Key::_8 | Key::KP_8 => Shortcut::Tag(8),
        Key::_9 | Key::KP_9 => Shortcut::Tag(9),
        Key::_0 | Key::KP_0 => Shortcut::ClearTags,
        _ => return None,
    };
    Some(action)
}

/// Gmail's single keys (#392), in place of `shortcut_for` when Settings asks
/// for them: support.google.com/mail/answer/6594. j/k move a cursor without
/// opening, o or Enter opens, and the action keys are Gmail's. The digits
/// still toggle tags. `g` and `*` start a sequence (`gmail_sequence`).
pub(crate) fn gmail_shortcut_for(key: gtk::gdk::Key, shift: bool) -> Option<Shortcut> {
    use gtk::gdk::Key;
    let action = match key {
        Key::j => Shortcut::MoveCursor(1),
        Key::k => Shortcut::MoveCursor(-1),
        Key::o => Shortcut::OpenCursor,
        Key::u if !shift => Shortcut::BackToList,
        Key::n => Shortcut::NextInThread,
        Key::p => Shortcut::PrevInThread,
        Key::x => Shortcut::ToggleSelect,
        Key::s => Shortcut::Star,
        Key::e => Shortcut::Archive,
        Key::numbersign => Shortcut::Delete,
        Key::exclam => Shortcut::Spam,
        Key::r => Shortcut::Reply,
        Key::a => Shortcut::ReplyAll,
        Key::f => Shortcut::Forward,
        Key::c => Shortcut::Compose,
        Key::slash => Shortcut::Search,
        Key::question => Shortcut::Shortcuts,
        Key::U => Shortcut::MarkRead(false),
        Key::underscore => Shortcut::MarkUnreadFrom,
        Key::I => Shortcut::MarkRead(true),
        Key::semicolon => Shortcut::FoldAll(false),
        Key::colon => Shortcut::FoldAll(true),
        Key::z => Shortcut::Undo,
        Key::v => Shortcut::MoveTo,
        Key::b => Shortcut::Snooze,
        Key::l => Shortcut::Label,
        Key::bracketright | Key::braceright => Shortcut::ArchiveAndStep(1),
        Key::bracketleft | Key::braceleft => Shortcut::ArchiveAndStep(-1),
        Key::R => Shortcut::InWindow(WindowCompose::Reply),
        Key::A => Shortcut::InWindow(WindowCompose::ReplyAll),
        Key::F => Shortcut::InWindow(WindowCompose::Forward),
        Key::d => Shortcut::InWindow(WindowCompose::New),
        Key::N => Shortcut::Refresh,
        Key::Down => Shortcut::NextMessage,
        Key::Up => Shortcut::PrevMessage,
        _ => {
            return shortcut_for(key, shift)
                .filter(|s| matches!(s, Shortcut::Tag(_) | Shortcut::ClearTags))
        }
    };
    Some(action)
}

/// The second key of a Gmail sequence: `g` goes to a folder, `*` selects.
pub(crate) fn gmail_sequence(first: char, key: gtk::gdk::Key) -> Option<Shortcut> {
    use gtk::gdk::Key;
    match (first, key) {
        ('g', Key::i) => Some(Shortcut::GoTo(GoTarget::Kind(FolderKind::Inbox))),
        ('g', Key::s) => Some(Shortcut::GoTo(GoTarget::Kind(FolderKind::Starred))),
        ('g', Key::t) => Some(Shortcut::GoTo(GoTarget::Kind(FolderKind::Sent))),
        ('g', Key::d) => Some(Shortcut::GoTo(GoTarget::Kind(FolderKind::Drafts))),
        ('g', Key::a) => Some(Shortcut::GoTo(GoTarget::Kind(FolderKind::Archive))),
        ('g', Key::c) => Some(Shortcut::GoTo(GoTarget::Contacts)),
        ('*', Key::a) => Some(Shortcut::SelectAll),
        ('*', Key::n) => Some(Shortcut::SelectNone),
        ('*', Key::r) => Some(Shortcut::SelectWhere(CheckFilter::Read)),
        ('*', Key::u) => Some(Shortcut::SelectWhere(CheckFilter::Unread)),
        ('*', Key::s) => Some(Shortcut::SelectWhere(CheckFilter::Starred)),
        ('*', Key::t) => Some(Shortcut::SelectWhere(CheckFilter::Unstarred)),
        _ => None,
    }
}

/// Keys the reader keeps while it has focus, because they scroll it or move
/// through its links. Letters and the rest go to the shortcuts, so a click
/// in a message no longer turns them off (#392).
pub(crate) fn reader_keeps(key: gtk::gdk::Key) -> bool {
    use gtk::gdk::Key;
    matches!(
        key,
        Key::Up
            | Key::Down
            | Key::Left
            | Key::Right
            | Key::Page_Up
            | Key::Page_Down
            | Key::Home
            | Key::End
            | Key::space
            | Key::Return
            | Key::KP_Enter
            | Key::Tab
            | Key::ISO_Left_Tab
            | Key::BackSpace
    )
}

/// The key as it would read with Caps Lock off. Caps Lock capitalises the
/// letters, so `j` arrived as `J` and no single key worked while it was on
/// (#392). Shift alone decides the case of a letter here.
pub(crate) fn without_caps_lock(
    key: gtk::gdk::Key,
    state: gtk::gdk::ModifierType,
) -> gtk::gdk::Key {
    if !state.contains(gtk::gdk::ModifierType::LOCK_MASK) || key.is_upper() == key.is_lower() {
        return key;
    }
    if state.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
        key.to_upper()
    } else {
        key.to_lower()
    }
}

/// Whether a keystroke should be left to the widget that has focus. Typing in a
/// search field, an address row or the composer must never archive mail, and the
/// message view is a web view that handles its own keys (find, scrolling).
pub(crate) fn focus_takes_keys(window: &impl IsA<gtk::Window>) -> bool {
    focus_matches(window, true)
}

/// Whether focus is in something being typed into. Narrower than
/// [`focus_takes_keys`]: Escape means "back out" while reading a message, but in
/// a search field it means "clear the search", which is the field's business.
pub(crate) fn focus_is_text(window: &impl IsA<gtk::Window>) -> bool {
    focus_matches(window, false)
}

/// Whether keyboard focus sits inside a composer — whose editor must keep
/// Ctrl+Z for its own text undo.
pub(crate) fn focus_in_compose(window: &adw::ApplicationWindow) -> bool {
    let mut w = gtk::prelude::GtkWindowExt::focus(window);
    while let Some(cur) = w {
        if cur.has_css_class("compose-pane") || cur.has_css_class("inline-compose-surface") {
            return true;
        }
        w = cur.parent();
    }
    false
}

fn focus_matches(window: &impl IsA<gtk::Window>, include_web_view: bool) -> bool {
    let Some(focus) = gtk::prelude::GtkWindowExt::focus(window) else {
        return false;
    };
    let mut node = Some(focus);
    while let Some(widget) = node {
        if widget.is::<gtk::Editable>() || widget.is::<gtk::TextView>() {
            return true;
        }
        if include_web_view && widget.is::<webkit6::WebView>() {
            return true;
        }
        node = widget.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_map_matches_gmail() {
        use gtk::gdk::Key;
        assert_eq!(
            gmail_shortcut_for(Key::j, false),
            Some(Shortcut::MoveCursor(1))
        );
        assert_eq!(
            gmail_shortcut_for(Key::k, false),
            Some(Shortcut::MoveCursor(-1))
        );
        assert_eq!(
            gmail_shortcut_for(Key::o, false),
            Some(Shortcut::OpenCursor)
        );
        assert_eq!(gmail_shortcut_for(Key::e, false), Some(Shortcut::Archive));
        assert_eq!(
            gmail_shortcut_for(Key::numbersign, true),
            Some(Shortcut::Delete)
        );
        assert_eq!(gmail_shortcut_for(Key::a, false), Some(Shortcut::ReplyAll));
        assert_eq!(
            gmail_shortcut_for(Key::U, true),
            Some(Shortcut::MarkRead(false))
        );
        assert_eq!(
            gmail_shortcut_for(Key::underscore, true),
            Some(Shortcut::MarkUnreadFrom)
        );
        assert_eq!(
            gmail_shortcut_for(Key::semicolon, false),
            Some(Shortcut::FoldAll(false))
        );
        assert_eq!(
            gmail_shortcut_for(Key::colon, true),
            Some(Shortcut::FoldAll(true))
        );
        assert_eq!(
            gmail_shortcut_for(Key::I, true),
            Some(Shortcut::MarkRead(true))
        );
        assert_eq!(
            gmail_shortcut_for(Key::n, false),
            Some(Shortcut::NextInThread)
        );
        assert_eq!(gmail_shortcut_for(Key::_3, false), Some(Shortcut::Tag(3)));
        assert_eq!(
            gmail_shortcut_for(Key::bracketright, false),
            Some(Shortcut::ArchiveAndStep(1))
        );
        assert_eq!(
            gmail_shortcut_for(Key::braceleft, true),
            Some(Shortcut::ArchiveAndStep(-1))
        );
        assert_eq!(
            gmail_shortcut_for(Key::R, true),
            Some(Shortcut::InWindow(WindowCompose::Reply))
        );
        assert_eq!(gmail_shortcut_for(Key::N, true), Some(Shortcut::Refresh));
        // d is Gmail's compose in a window, not Hylki's delete.
        assert_eq!(
            gmail_shortcut_for(Key::d, false),
            Some(Shortcut::InWindow(WindowCompose::New))
        );
        // Hylki's own keys that Gmail does not have stay off.
        assert_eq!(gmail_shortcut_for(Key::w, false), None);
        assert_eq!(gmail_shortcut_for(Key::h, false), None);
        assert_eq!(
            gmail_sequence('g', Key::i),
            Some(Shortcut::GoTo(GoTarget::Kind(FolderKind::Inbox)))
        );
        assert_eq!(
            gmail_sequence('g', Key::t),
            Some(Shortcut::GoTo(GoTarget::Kind(FolderKind::Sent)))
        );
        assert_eq!(gmail_sequence('*', Key::a), Some(Shortcut::SelectAll));
        assert_eq!(
            gmail_sequence('*', Key::u),
            Some(Shortcut::SelectWhere(CheckFilter::Unread))
        );
        assert_eq!(
            gmail_sequence('*', Key::t),
            Some(Shortcut::SelectWhere(CheckFilter::Unstarred))
        );
        assert_eq!(gmail_sequence('g', Key::q), None);
        // The reader keeps what scrolls it, and only that.
        assert!(reader_keeps(Key::space) && reader_keeps(Key::Down));
        assert!(!reader_keeps(Key::r) && !reader_keeps(Key::j));
    }

    #[test]
    fn caps_lock_leaves_the_keys_alone() {
        use gtk::gdk::{Key, ModifierType as M};
        assert_eq!(without_caps_lock(Key::J, M::LOCK_MASK), Key::j);
        assert_eq!(
            without_caps_lock(Key::r, M::LOCK_MASK | M::SHIFT_MASK),
            Key::R
        );
        assert_eq!(
            without_caps_lock(Key::R, M::LOCK_MASK | M::SHIFT_MASK),
            Key::R
        );
        assert_eq!(
            without_caps_lock(Key::numbersign, M::LOCK_MASK | M::SHIFT_MASK),
            Key::numbersign
        );
        assert_eq!(without_caps_lock(Key::_3, M::LOCK_MASK), Key::_3);
        // Without Caps Lock the key is as typed.
        assert_eq!(without_caps_lock(Key::J, M::empty()), Key::J);
    }

    #[test]
    fn single_key_map_matches_the_request() {
        use gtk::gdk::Key;
        // The keys issue #5 asked for, by name.
        assert_eq!(shortcut_for(Key::j, false), Some(Shortcut::NextMessage));
        assert_eq!(shortcut_for(Key::k, false), Some(Shortcut::PrevMessage));
        assert_eq!(shortcut_for(Key::l, false), Some(Shortcut::OpenMessage));
        assert_eq!(shortcut_for(Key::h, false), Some(Shortcut::BackToList));
        assert_eq!(shortcut_for(Key::r, false), Some(Shortcut::Reply));
        assert_eq!(shortcut_for(Key::a, false), Some(Shortcut::Archive));
        assert_eq!(shortcut_for(Key::x, false), Some(Shortcut::ToggleSelect));
        assert_eq!(shortcut_for(Key::d, false), Some(Shortcut::Delete));
        assert_eq!(shortcut_for(Key::D, true), Some(Shortcut::DeleteForever));
        assert_eq!(shortcut_for(Key::w, false), Some(Shortcut::NextInThread));
        assert_eq!(shortcut_for(Key::b, false), Some(Shortcut::PrevInThread));
        // Arrows do what h/j/k/l do.
        assert_eq!(shortcut_for(Key::Down, false), Some(Shortcut::NextMessage));
        assert_eq!(shortcut_for(Key::Up, false), Some(Shortcut::PrevMessage));
        assert_eq!(shortcut_for(Key::Right, false), Some(Shortcut::OpenMessage));
        assert_eq!(shortcut_for(Key::Left, false), Some(Shortcut::BackToList));
        // Shift distinguishes reply from reply-all.
        assert_eq!(shortcut_for(Key::R, true), Some(Shortcut::ReplyAll));
        // Digits toggle tags by position, on the keypad too; 0 clears (#157).
        assert_eq!(shortcut_for(Key::_1, false), Some(Shortcut::Tag(1)));
        assert_eq!(shortcut_for(Key::KP_5, false), Some(Shortcut::Tag(5)));
        assert_eq!(shortcut_for(Key::_9, false), Some(Shortcut::Tag(9)));
        assert_eq!(shortcut_for(Key::_0, false), Some(Shortcut::ClearTags));
        // z snoozes (#527): b, Gmail's key, steps through a conversation.
        assert_eq!(shortcut_for(Key::z, false), Some(Shortcut::Snooze));
        assert_eq!(gmail_shortcut_for(Key::b, false), Some(Shortcut::Snooze));
        // Anything unmapped is left to the widget with focus.
        assert_eq!(shortcut_for(Key::q, false), None);
        assert_eq!(shortcut_for(Key::Return, false), None);
        assert_eq!(shortcut_for(Key::space, false), None);
    }
}
