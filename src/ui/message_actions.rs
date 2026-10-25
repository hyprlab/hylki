//! The label and icon of each action on a message, as the reader's ⋯ menu
//! in the main window and the pop-out's both show them. One table, so the
//! two menus cannot drift apart. (The list's row menu and the card menu
//! word their entries differently and keep their own.)

use crate::i18n::i18n;
use crate::ui::message_row::RowAction;

/// Whose keys a menu's hints name: a message's own window has no Ctrl+R,
/// Ctrl+Shift+R or Ctrl+U.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeysOf {
    Main,
    Popout,
}

/// One entry of a message's ⋯ menu. The two-faced actions (star, spam,
/// read) have one variant per face; the caller picks by the message's
/// state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageAction {
    Reply,
    ReplyAll,
    Forward,
    Star,
    RemoveStar,
    Archive,
    Snooze,
    Delete,
    MarkSpam,
    NotSpam,
    MarkRead,
    MarkUnread,
    Tags,
    MoveToInbox,
    MoveTo,
    Find,
    PrintPreview,
}

impl MessageAction {
    /// The entry's text, translated.
    pub(crate) fn label(self) -> String {
        match self {
            Self::Reply => i18n("Reply"),
            Self::ReplyAll => i18n("Reply All"),
            Self::Forward => i18n("Forward"),
            Self::Star => i18n("Star"),
            Self::RemoveStar => i18n("Remove Star"),
            Self::Archive => i18n("Archive"),
            Self::Snooze => i18n("Snooze"),
            Self::Delete => i18n("Delete"),
            Self::MarkSpam => i18n("Mark as Spam"),
            Self::NotSpam => i18n("Not Spam"),
            Self::MarkRead => i18n("Mark as Read"),
            Self::MarkUnread => i18n("Mark as Unread"),
            Self::Tags => i18n("Tags"),
            Self::MoveToInbox => i18n("Move to Inbox"),
            Self::MoveTo => i18n("Move To…"),
            Self::Find => i18n("Find in Message"),
            Self::PrintPreview => i18n("Print Preview"),
        }
    }

    /// The entry's icon, without the `-symbolic` the menus add.
    pub(crate) fn icon(self) -> &'static str {
        match self {
            Self::Reply => "mail-reply-sender",
            Self::ReplyAll => "mail-reply-all",
            Self::Forward => "mail-forward",
            Self::Star => "starred",
            Self::RemoveStar => "hylki-non-starred",
            Self::Archive => "mail-archive",
            Self::Snooze => "alarm",
            Self::Delete => "user-trash",
            Self::MarkSpam => "mail-mark-junk",
            Self::NotSpam => "mail-mark-notjunk",
            Self::MarkRead => "hylki-mail-read",
            Self::MarkUnread => "mail-unread",
            Self::Tags => "tag-outline",
            Self::MoveToInbox => "mail-inbox",
            Self::MoveTo => "folder",
            Self::Find => "loupe-with-arrow",
            Self::PrintPreview => "printer",
        }
    }
}

impl MessageAction {
    /// The keys for an action of the row and card menus, as [`Self::keys`].
    /// `unread` picks the face of the read toggle.
    pub(crate) fn row_keys(
        action: RowAction,
        unread: bool,
        window: KeysOf,
    ) -> Option<&'static str> {
        match (action, window) {
            (RowAction::ViewSource, KeysOf::Main) => Some("<Control>u"),
            _ => Self::of_row(action, unread)?.keys(window),
        }
    }

    fn of_row(action: RowAction, unread: bool) -> Option<Self> {
        Some(match action {
            RowAction::Reply => Self::Reply,
            RowAction::ReplyAll => Self::ReplyAll,
            RowAction::Forward => Self::Forward,
            RowAction::ToggleStar => Self::Star,
            RowAction::ToggleRead if unread => Self::MarkRead,
            RowAction::ToggleRead => Self::MarkUnread,
            RowAction::Spam => Self::MarkSpam,
            RowAction::NotSpam => Self::NotSpam,
            RowAction::Archive => Self::Archive,
            RowAction::Delete => Self::Delete,
            RowAction::MoveToInbox => Self::MoveToInbox,
            _ => return None,
        })
    }

    /// The keys that do the same in `window`, with the keys Settings has
    /// on, as a GTK accelerator for a menu to show beside the entry. While
    /// single keys are on, the single key is named over its Ctrl twin.
    pub(crate) fn keys(self, window: KeysOf) -> Option<&'static str> {
        use crate::ui::keymap::KeyMode;
        match self {
            Self::Delete => return Some("Delete"),
            Self::Find => return Some("<Control>f"),
            Self::PrintPreview => return Some("<Control><Shift>p"),
            _ => {}
        }
        let single = match crate::ui::keymap::key_mode() {
            KeyMode::Off => None,
            KeyMode::Single => match self {
                Self::Reply => Some("r"),
                Self::ReplyAll => Some("<Shift>r"),
                Self::Forward => Some("f"),
                Self::Star | Self::RemoveStar => Some("s"),
                Self::Archive => Some("a"),
                Self::Snooze => Some("z"),
                Self::MarkSpam | Self::NotSpam => Some("exclam"),
                Self::MarkRead | Self::MarkUnread => Some("m"),
                _ => None,
            },
            KeyMode::Gmail => match self {
                Self::Reply => Some("r"),
                Self::ReplyAll => Some("a"),
                Self::Forward => Some("f"),
                Self::Star | Self::RemoveStar => Some("s"),
                Self::Archive => Some("e"),
                Self::Snooze => Some("b"),
                Self::MarkSpam | Self::NotSpam => Some("exclam"),
                Self::MarkRead => Some("<Shift>i"),
                Self::MarkUnread => Some("<Shift>u"),
                Self::Tags => Some("l"),
                Self::MoveTo => Some("v"),
                _ => None,
            },
        };
        single.or(match (self, window) {
            (Self::Reply, KeysOf::Main) => Some("<Control>r"),
            (Self::ReplyAll, KeysOf::Main) => Some("<Control><Shift>r"),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::keymap::{gmail_shortcut_for, set_key_mode, shortcut_for, Shortcut};

    /// The single key a hint names, as the key handler receives it (GTK's
    /// own parser wants a display).
    fn pressed(accel: &str) -> (gtk::gdk::Key, bool) {
        let (shift, name) = match accel.strip_prefix("<Shift>") {
            Some(name) => (true, name),
            None => (false, accel),
        };
        assert!(!name.starts_with('<'), "{accel} is not a single key");
        let key = gtk::gdk::Key::from_name(name).unwrap();
        (if shift { key.to_upper() } else { key }, shift)
    }

    /// Every single key a menu names does that entry's action.
    #[test]
    fn hints_name_the_keys_that_act() {
        use MessageAction as A;
        let cases: [(A, Option<Shortcut>, Option<Shortcut>); 13] = [
            (A::Reply, Some(Shortcut::Reply), Some(Shortcut::Reply)),
            (
                A::ReplyAll,
                Some(Shortcut::ReplyAll),
                Some(Shortcut::ReplyAll),
            ),
            (A::Forward, Some(Shortcut::Forward), Some(Shortcut::Forward)),
            (A::Star, Some(Shortcut::Star), Some(Shortcut::Star)),
            (A::RemoveStar, Some(Shortcut::Star), Some(Shortcut::Star)),
            (A::Archive, Some(Shortcut::Archive), Some(Shortcut::Archive)),
            (A::Snooze, Some(Shortcut::Snooze), Some(Shortcut::Snooze)),
            (A::MarkSpam, Some(Shortcut::Spam), Some(Shortcut::Spam)),
            (
                A::MarkRead,
                Some(Shortcut::ToggleRead),
                Some(Shortcut::MarkRead(true)),
            ),
            (
                A::MarkUnread,
                Some(Shortcut::ToggleRead),
                Some(Shortcut::MarkRead(false)),
            ),
            (A::Tags, None, Some(Shortcut::Label)),
            (A::MoveTo, None, Some(Shortcut::MoveTo)),
            (A::MoveToInbox, None, None),
        ];
        for (gmail, map) in [
            (
                false,
                shortcut_for as fn(gtk::gdk::Key, bool) -> Option<Shortcut>,
            ),
            (true, gmail_shortcut_for),
        ] {
            set_key_mode(true, gmail);
            for (action, single, in_gmail) in cases {
                let want = if gmail { in_gmail } else { single };
                let got = action
                    .keys(KeysOf::Popout)
                    .map(pressed)
                    .and_then(|(key, shift)| map(key, shift));
                assert_eq!(got, want, "{action:?}, Gmail keys {gmail}");
            }
        }
        // Without single keys: only what works without them.
        set_key_mode(false, true);
        assert_eq!(A::Reply.keys(KeysOf::Main), Some("<Control>r"));
        assert_eq!(A::Reply.keys(KeysOf::Popout), None);
        assert_eq!(A::Archive.keys(KeysOf::Main), None);
        assert_eq!(A::Delete.keys(KeysOf::Main), Some("Delete"));
    }
}
