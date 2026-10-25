//! The reader toolbar's layout: which buttons it shows, on which side,
//! and toolbar.toml.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::{config_file, read_toml, save_plain_toml};

// ---------------------------------------------------------------------------
// Reader toolbar layout: which buttons the reading pane's header shows, on
// which side, in what order (Settings → Appearance → Toolbar).
// ---------------------------------------------------------------------------

/// One button of the reader header. The left group stays on the bar at every
/// width; the right group folds into the ⋯ overflow menu when the pane is
/// narrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ToolbarItem {
    Reply,
    ReplyAll,
    Forward,
    Star,
    Archive,
    /// Put away until a set time (#527).
    Snooze,
    Delete,
    Spam,
    ReadUnread,
    Tags,
    MoveTo,
    Find,
    Print,
}

impl ToolbarItem {
    pub(crate) const ALL: [ToolbarItem; 13] = [
        ToolbarItem::Reply,
        ToolbarItem::ReplyAll,
        ToolbarItem::Forward,
        ToolbarItem::Star,
        ToolbarItem::Archive,
        ToolbarItem::Snooze,
        ToolbarItem::Delete,
        ToolbarItem::Spam,
        ToolbarItem::ReadUnread,
        ToolbarItem::Tags,
        ToolbarItem::MoveTo,
        ToolbarItem::Find,
        ToolbarItem::Print,
    ];

    /// The stable name written to `toolbar.toml`.
    pub(crate) fn key(self) -> &'static str {
        match self {
            ToolbarItem::Reply => "reply",
            ToolbarItem::ReplyAll => "reply_all",
            ToolbarItem::Forward => "forward",
            ToolbarItem::Star => "star",
            ToolbarItem::Archive => "archive",
            ToolbarItem::Snooze => "snooze",
            ToolbarItem::Delete => "delete",
            ToolbarItem::Spam => "spam",
            ToolbarItem::ReadUnread => "read_unread",
            ToolbarItem::Tags => "tags",
            ToolbarItem::MoveTo => "move_to",
            ToolbarItem::Find => "find",
            ToolbarItem::Print => "print",
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<ToolbarItem> {
        ToolbarItem::ALL.iter().copied().find(|i| i.key() == key)
    }

    /// The symbolic icon the button (and the settings chip) wears.
    pub(crate) fn icon(self) -> &'static str {
        match self {
            ToolbarItem::Reply => "mail-reply-sender-symbolic",
            ToolbarItem::ReplyAll => "mail-reply-all-symbolic",
            ToolbarItem::Forward => "mail-forward-symbolic",
            ToolbarItem::Star => "hylki-non-starred-symbolic",
            ToolbarItem::Archive => "mail-archive-symbolic",
            ToolbarItem::Snooze => "alarm-symbolic",
            ToolbarItem::Delete => "user-trash-symbolic",
            ToolbarItem::Spam => "mail-mark-junk-symbolic",
            ToolbarItem::ReadUnread => "mail-unread-symbolic",
            ToolbarItem::Tags => "tag-outline-symbolic",
            ToolbarItem::MoveTo => "folder-symbolic",
            ToolbarItem::Find => "loupe-with-arrow-symbolic",
            ToolbarItem::Print => "printer-symbolic",
        }
    }

    /// The untranslated label (callers pass it through `i18n`).
    pub(crate) fn label(self) -> &'static str {
        match self {
            ToolbarItem::Reply => crate::i18n::i18n_noop("Reply"),
            ToolbarItem::ReplyAll => crate::i18n::i18n_noop("Reply All"),
            ToolbarItem::Forward => crate::i18n::i18n_noop("Forward"),
            ToolbarItem::Star => crate::i18n::i18n_noop("Star"),
            ToolbarItem::Archive => crate::i18n::i18n_noop("Archive"),
            ToolbarItem::Snooze => crate::i18n::i18n_noop("Snooze"),
            ToolbarItem::Delete => crate::i18n::i18n_noop("Delete"),
            ToolbarItem::Spam => crate::i18n::i18n_noop("Spam"),
            ToolbarItem::ReadUnread => crate::i18n::i18n_noop("Read/Unread"),
            ToolbarItem::Tags => crate::i18n::i18n_noop("Tags"),
            ToolbarItem::MoveTo => crate::i18n::i18n_noop("Move To"),
            ToolbarItem::Find => crate::i18n::i18n_noop("Find"),
            ToolbarItem::Print => crate::i18n::i18n_noop("Print"),
        }
    }
}

/// Which side of the reader header a button sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolbarSide {
    Left,
    Right,
}

/// How many buttons the left side of the reader toolbar holds at most. The
/// left group never folds, so it has to fit the narrowest reader with the
/// overflow button beside it. The right group has no limit: it folds into
/// the overflow menu before it can reach the left one.
pub(crate) const TOOLBAR_LEFT_MAX: usize = 7;

/// How far a saved layout has been brought up to the default's changes:
/// 1 moved Spam from the right group to the left, after Archive.
const TOOLBAR_REVISION: u32 = 1;

/// The buttons there were before toolbar.toml listed the ones it knew of:
/// a file without that list knew these, and any other is new to it.
const KNOWN_BEFORE_SNOOZE: [&str; 12] = [
    "reply",
    "reply_all",
    "forward",
    "star",
    "archive",
    "delete",
    "spam",
    "read_unread",
    "tags",
    "move_to",
    "find",
    "print",
];

/// The reader header's layout. A button absent from both lists is hidden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReaderToolbar {
    pub(crate) left: Vec<ToolbarItem>,
    pub(crate) right: Vec<ToolbarItem>,
}

impl Default for ReaderToolbar {
    fn default() -> Self {
        ReaderToolbar {
            left: vec![
                ToolbarItem::Reply,
                ToolbarItem::ReplyAll,
                ToolbarItem::Forward,
                ToolbarItem::Star,
                ToolbarItem::Archive,
                ToolbarItem::Spam,
                ToolbarItem::Delete,
            ],
            right: vec![
                ToolbarItem::Snooze,
                ToolbarItem::Tags,
                ToolbarItem::ReadUnread,
                ToolbarItem::MoveTo,
                ToolbarItem::Find,
                ToolbarItem::Print,
            ],
        }
    }
}

impl ReaderToolbar {
    /// The side an item sits on, or None when it is hidden.
    pub(crate) fn side(&self, item: ToolbarItem) -> Option<ToolbarSide> {
        if self.left.contains(&item) {
            Some(ToolbarSide::Left)
        } else if self.right.contains(&item) {
            Some(ToolbarSide::Right)
        } else {
            None
        }
    }

    /// The items on neither side, in canonical order.
    pub(crate) fn hidden(&self) -> Vec<ToolbarItem> {
        ToolbarItem::ALL
            .iter()
            .copied()
            .filter(|i| self.side(*i).is_none())
            .collect()
    }

    /// Drop repeats (an item may sit on one side only, once), and anything
    /// past a side's room.
    fn sanitize(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.left.retain(|i| seen.insert(*i));
        self.right.retain(|i| seen.insert(*i));
        self.left.truncate(TOOLBAR_LEFT_MAX);
    }

    /// Whether `side` has room for one more button that is not already on
    /// it: only the left side can be full.
    pub(crate) fn has_room(&self, side: Option<ToolbarSide>, item: ToolbarItem) -> bool {
        match side {
            Some(ToolbarSide::Left) => {
                self.left.contains(&item) || self.left.len() < TOOLBAR_LEFT_MAX
            }
            Some(ToolbarSide::Right) | None => true,
        }
    }

    /// Take `item` out of wherever it is and put it at `index` of `side`
    /// (clamped); `side` None hides it. A full side is left as it is.
    pub(crate) fn place(&mut self, item: ToolbarItem, side: Option<ToolbarSide>, index: usize) {
        if !self.has_room(side, item) {
            return;
        }
        self.left.retain(|i| *i != item);
        self.right.retain(|i| *i != item);
        let list = match side {
            Some(ToolbarSide::Left) => &mut self.left,
            Some(ToolbarSide::Right) => &mut self.right,
            None => return,
        };
        let index = index.min(list.len());
        list.insert(index, item);
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub(super) struct ToolbarFile {
    #[serde(default)]
    pub(super) left: Vec<String>,
    #[serde(default)]
    pub(super) right: Vec<String>,
    /// Every button the Hylki that wrote the file had, shown or not, so a
    /// newer one's buttons are told from the ones put away (#527).
    #[serde(default)]
    pub(super) known: Vec<String>,
    /// The `TOOLBAR_REVISION` the file was written at; older files get
    /// the default's later changes once, on load.
    #[serde(default)]
    pub(super) revision: u32,
}

impl ToolbarFile {
    pub(super) fn of(layout: &ReaderToolbar) -> ToolbarFile {
        ToolbarFile {
            left: layout.left.iter().map(|i| i.key().to_string()).collect(),
            right: layout.right.iter().map(|i| i.key().to_string()).collect(),
            known: ToolbarItem::ALL
                .iter()
                .map(|i| i.key().to_string())
                .collect(),
            revision: TOOLBAR_REVISION,
        }
    }

    /// The layout the file describes. A button the writer did not know of
    /// goes where the default layout has it, if that side has room.
    fn layout(&self) -> ReaderToolbar {
        let keys = |list: &[String]| -> Vec<ToolbarItem> {
            list.iter()
                .filter_map(|k| ToolbarItem::from_key(k))
                .collect()
        };
        let mut layout = ReaderToolbar {
            left: keys(&self.left),
            right: keys(&self.right),
        };
        layout.sanitize();
        let knew = |key: &str| {
            if self.known.is_empty() {
                KNOWN_BEFORE_SNOOZE.contains(&key)
            } else {
                self.known.iter().any(|k| k == key)
            }
        };
        let default = ReaderToolbar::default();
        for item in ToolbarItem::ALL {
            if knew(item.key()) || layout.side(item).is_some() {
                continue;
            }
            let Some(side) = default.side(item) else {
                continue;
            };
            let list = match side {
                ToolbarSide::Left => &default.left,
                ToolbarSide::Right => &default.right,
            };
            let index = list.iter().position(|i| *i == item).unwrap_or(0);
            layout.place(item, Some(side), index);
        }
        if self.revision < 1 {
            spam_beside_archive(&mut layout);
        }
        layout
    }
}

/// Spam moved to the default left group, between Archive and Delete. A
/// layout saved before gets it too, but only while its left group is the
/// old default one: a left group of someone's own choosing is left alone.
fn spam_beside_archive(layout: &mut ReaderToolbar) {
    use ToolbarItem as T;
    const OLD_LEFT: [ToolbarItem; 6] = [
        T::Reply,
        T::ReplyAll,
        T::Forward,
        T::Star,
        T::Archive,
        T::Delete,
    ];
    if layout.left != OLD_LEFT || layout.side(T::Spam) != Some(ToolbarSide::Right) {
        return;
    }
    layout.place(T::Spam, Some(ToolbarSide::Left), 5);
}

fn toolbar_path() -> Option<PathBuf> {
    config_file("toolbar.toml")
}

/// The saved reader toolbar layout, or the default when there is none.
/// Unknown names (a newer Hylki's buttons) are skipped, not fatal.
pub(crate) fn load_reader_toolbar() -> ReaderToolbar {
    match read_toml::<ToolbarFile>(toolbar_path()) {
        Some(file) => file.layout(),
        None => ReaderToolbar::default(),
    }
}

pub(crate) fn save_reader_toolbar(layout: &ReaderToolbar) {
    save_plain_toml(toolbar_path(), &ToolbarFile::of(layout));
}

#[cfg(test)]
mod toolbar_tests {
    use super::*;

    #[test]
    fn place_moves_between_and_within_sides() {
        let mut t = ReaderToolbar::default();
        // The default left group is full: a move onto it changes nothing.
        t.place(ToolbarItem::Print, Some(ToolbarSide::Left), 0);
        assert_eq!(t.left[0], ToolbarItem::Reply);
        assert_eq!(t.right.last(), Some(&ToolbarItem::Print));
        // Make room, then right → left, at the front.
        t.place(ToolbarItem::Star, None, 0);
        t.place(ToolbarItem::Print, Some(ToolbarSide::Left), 0);
        assert_eq!(t.left[0], ToolbarItem::Print);
        assert!(!t.right.contains(&ToolbarItem::Print));
        // Within the left group: Reply (now index 1) to the end.
        t.place(ToolbarItem::Reply, Some(ToolbarSide::Left), 99);
        assert_eq!(t.left.last(), Some(&ToolbarItem::Reply));
        assert_eq!(t.left.len(), TOOLBAR_LEFT_MAX);
        // Hidden: on neither side, listed under hidden().
        t.place(ToolbarItem::Spam, None, 0);
        assert_eq!(t.side(ToolbarItem::Spam), None);
        assert_eq!(t.hidden(), vec![ToolbarItem::Star, ToolbarItem::Spam]);
        // Back from hidden into the right group's middle.
        t.place(ToolbarItem::Spam, Some(ToolbarSide::Right), 1);
        assert_eq!(t.right[1], ToolbarItem::Spam);
        assert_eq!(t.hidden(), vec![ToolbarItem::Star]);
        // The right group takes every button there is.
        for item in ToolbarItem::ALL {
            t.place(item, Some(ToolbarSide::Right), 99);
        }
        assert!(t.left.is_empty());
        assert_eq!(t.right.len(), ToolbarItem::ALL.len());
    }

    #[test]
    fn a_button_new_to_the_file_joins_its_default_side_once() {
        // Written before Snooze (#527): it arrives where the default has it.
        let old: ToolbarFile =
            toml::from_str("left = [\"reply\", \"delete\"]\nright = [\"tags\", \"print\"]\n")
                .unwrap();
        let layout = old.layout();
        assert_eq!(layout.right[0], ToolbarItem::Snooze);
        assert_eq!(layout.left, vec![ToolbarItem::Reply, ToolbarItem::Delete]);
        // Put away and saved: it stays away.
        let mut hidden = layout.clone();
        hidden.place(ToolbarItem::Snooze, None, 0);
        let text = toml::to_string(&ToolbarFile::of(&hidden)).unwrap();
        let back: ToolbarFile = toml::from_str(&text).unwrap();
        assert_eq!(back.layout().side(ToolbarItem::Snooze), None);
        // A button the old file knew of and left out stays out.
        assert_eq!(old.layout().side(ToolbarItem::Find), None);
    }

    #[test]
    fn spam_joins_only_an_untouched_left_group_once() {
        let old_right = "right = [\"snooze\", \"tags\", \"read_unread\", \"spam\", \"move_to\", \"find\", \"print\"]\n";
        // The old default left group: Spam goes between Archive and Delete.
        let old: ToolbarFile = toml::from_str(&format!(
            "left = [\"reply\", \"reply_all\", \"forward\", \"star\", \"archive\", \"delete\"]\n{old_right}"
        ))
        .unwrap();
        assert_eq!(old.layout(), ReaderToolbar::default());
        // A left group of the user's own (reordered, or other buttons):
        // Spam stays on the right.
        for left in [
            "[\"reply\", \"reply_all\", \"forward\", \"star\", \"delete\", \"archive\"]",
            "[\"reply\", \"forward\", \"star\", \"archive\", \"delete\"]",
        ] {
            let custom: ToolbarFile =
                toml::from_str(&format!("left = {left}\n{old_right}")).unwrap();
            assert_eq!(
                custom.layout().side(ToolbarItem::Spam),
                Some(ToolbarSide::Right)
            );
        }
        // Put away with the default left group: it stays away.
        let hidden: ToolbarFile = toml::from_str(
            "left = [\"reply\", \"reply_all\", \"forward\", \"star\", \"archive\", \"delete\"]\nright = [\"print\"]\n",
        )
        .unwrap();
        assert_eq!(hidden.layout().side(ToolbarItem::Spam), None);
        // Saved since, back on the right: it stays there.
        let mut moved = ReaderToolbar::default();
        moved.place(ToolbarItem::Spam, Some(ToolbarSide::Right), 0);
        let text = toml::to_string(&ToolbarFile::of(&moved)).unwrap();
        let back: ToolbarFile = toml::from_str(&text).unwrap();
        assert_eq!(back.layout(), moved);
    }

    #[test]
    fn file_round_trip_skips_unknown_names() {
        let file: ToolbarFile =
            toml::from_str("left = [\"reply\", \"bogus\", \"delete\"]\nright = [\"print\"]\n")
                .unwrap();
        let layout = file.layout();
        assert_eq!(layout.left, vec![ToolbarItem::Reply, ToolbarItem::Delete]);
        assert_eq!(layout.right, vec![ToolbarItem::Snooze, ToolbarItem::Print]);
        for item in ToolbarItem::ALL {
            assert_eq!(ToolbarItem::from_key(item.key()), Some(item));
        }
    }
}
