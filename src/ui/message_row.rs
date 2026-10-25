//! One row of the message list. A `gtk::ListView` builds a screenful of
//! these and binds each to whichever message scrolls into view, so a row is
//! a plain widget tree filled synchronously on bind: a relm4 component's
//! update lands a frame later, and a recycled row would show the message it
//! held before for that frame.
//!
//! Also here: the list's model (`MessageModel`, which makes a row's item only
//! when the view asks for it), the swipe surface and the face lookups.

use std::collections::HashMap;
use std::rc::Rc;

use crate::config::ListColumn;
use crate::i18n::i18n;
use crate::models::Message;

mod build;
mod faces;
mod headings;
mod model;
mod palette;
mod refresh;
mod row;
mod shared;
mod swipe;

pub(crate) use faces::recipient_names;
pub(crate) use headings::{fill_headings, pill_label, tag_menu_entries};
pub(crate) use model::{MessageModel, RowItem};
pub(crate) use shared::{
    factory, header_factory, make_spare_rows, RowLook, RowShared, SnoozedRows,
};
pub(crate) use swipe::SwipeSurface;
// The row diff and the swipe math are tested from the list's tests.
#[cfg(test)]
pub(crate) use model::{row_edits, section_starts, swap_wanted, RowEdit};
#[cfg(test)]
pub(crate) use swipe::swipe_progress_px;

#[derive(Debug, Clone, Copy)]
pub(crate) enum RowAction {
    Reply,
    ReplyAll,
    Forward,
    /// Open a copy of the message in the composer as a message of its own
    /// (#232) — same recipients, subject, body and files, nothing tying it
    /// to the original.
    EditAsNew,
    /// Open a template (#360) in the composer to change it: saving puts it
    /// back in the Templates folder.
    EditTemplate,
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
    /// Save the message whole, as a .eml file (#398).
    SaveAsFile,
    AddContact,
    /// Start a new filter from the message's sender and subject.
    CreateFilter,
    /// A choice from the Snooze menu (#527).
    Snooze(crate::ui::later::SnoozeChoice),
}

/// A full swipe (#swipe): also `AdwSwipeable`'s reported `distance`, the px
/// one full drag (progress ±1.0) spans.
pub(super) const SWIPE_MAX: f64 = 120.0;

/// How far a thread member's node dot reaches past the start of the row's
/// content box (`.thread-node`: 8px wide, pulled 5px out by its negative
/// margin, plus a 2px masking ring), where it sits centred on the group's
/// rail. The last reply's rail stub reaches 2px the same way. The swipe
/// surface's clip leaves this much room on that side, or both come out cut
/// in half.
const THREAD_NODE_REACH: f32 = 8.0;
/// The single line's name columns, in pixels (#334): wide enough for most
/// names, and fixed so the columns after them line up.
const SENDER_COLUMN_PX: i32 = 160;
const PEOPLE_COLUMN_PX: i32 = 200;
const ACCOUNT_COLUMN_PX: i32 = 110;
/// The tags' column on one line (#443): fixed like the names, so a row
/// without tags puts the columns after it where a row with them does.
const TAGS_COLUMN_PX: i32 = 90;
/// The star's and the paperclip's size on a single line.
const LINE_ICON_PX: i32 = 12;
/// The replied column's width: its two arrows and the gap between them.
pub(super) const REPLIED_PX: i32 = 2 * LINE_ICON_PX + 2;

/// The width a single-line column takes until it is dragged to another
/// (#334): negative for a date, which is as wide as its text.
pub(super) fn default_width(column: ListColumn) -> i32 {
    match column {
        ListColumn::Sender | ListColumn::Recipients => SENDER_COLUMN_PX,
        ListColumn::Correspondents => PEOPLE_COLUMN_PX,
        ListColumn::Account => ACCOUNT_COLUMN_PX,
        ListColumn::Tags => TAGS_COLUMN_PX,
        _ => -1,
    }
}

/// Whether a column's width can be dragged: the ones of text and the tags,
/// but not the subject, which takes the rest.
pub(super) fn resizable(column: ListColumn) -> bool {
    matches!(
        column,
        ListColumn::Sender
            | ListColumn::Recipients
            | ListColumn::Correspondents
            | ListColumn::Account
            | ListColumn::Tags
            | ListColumn::Due
            | ListColumn::Date
    )
}

/// The width a column takes in `look`.
fn column_width(look: &RowLook, column: ListColumn) -> i32 {
    look.widths
        .get(&column)
        .copied()
        .unwrap_or_else(|| default_width(column))
}

/// Distance past which the indicator reads as "armed" (full color) — purely
/// a visual cue; `AdwSwipeTracker` makes the real commit decision on
/// release, factoring in velocity too.
pub(super) const SWIPE_ARM: f64 = 72.0;

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

/// Every member of each conversation, keyed by its head's (account, id) —
/// so a drag that starts on a conversation row carries the whole thread,
/// as its Delete does (#171).
pub(super) type ThreadDragKeys = HashMap<(u32, u32), Vec<(u32, u32, u32, u32)>>;

/// Where a message sits in the list: its account, folder and id. The id
/// alone is a UID, which only means something inside one folder (#317).
pub(crate) type Slot = (u32, u32, u32);

/// What a row says about its conversation, worked out by the list's rebuild.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RowMeta {
    /// Size of the conversation (thread heads only; 1 or 0 otherwise).
    pub(super) count: usize,
    /// Whether the chip can open anything: the folder holds more of it.
    pub(super) expandable: bool,
    /// A reply nested under a thread head — indented on the rail.
    pub(super) is_child: bool,
    /// The last reply of its thread: the dotted rail stops at its node.
    pub(super) is_last: bool,
    /// The head's conversation is opened out in the list.
    pub(super) expanded: bool,
    /// The conversation key, on a head that can toggle.
    pub(super) key: Option<(u32, String)>,
    /// The newest member's sender, preview and time (thread heads only):
    /// the row speaks for the conversation's latest message.
    pub(super) from: Option<(String, String)>,
    pub(super) preview: Option<String>,
    pub(super) latest: Option<String>,
    /// When that newest member arrived, and its date header, for a short
    /// date on a one-line row (#334).
    pub(super) latest_at: Option<(i64, String)>,
    /// Any message of the conversation is unread / starred (heads only).
    pub(super) unread: bool,
    pub(super) starred: bool,
    /// The newest member here was replied to / forwarded (heads only): the
    /// row speaks for it, and the arrow says whether it has been answered.
    pub(super) answered: bool,
    pub(super) forwarded: bool,
    /// Who took part in the conversation, for the Correspondents column
    /// (#334); worked out only while that column is shown.
    pub(super) people: Option<String>,
    /// The row's height is open. False only while a reply slides shut
    /// before it is taken out of the list.
    pub(super) revealed: bool,
    /// A reply just put in by opening its conversation: it slides open the
    /// first time it is shown instead of simply appearing.
    pub(super) appear: bool,
    /// The group the row heads, for asking the cache how big it is (#222).
    pub(super) group: Option<(u32, String)>,
    /// A head row past the list's window, described only as far as the
    /// conversation maps need, by this key: the rest is worked out when it
    /// comes on screen (a large folder holds tens of thousands).
    pub(super) pending: Option<(u32, String)>,
}

/// The date heading a row sits under while the list is grouped by date
/// (#435). `None` everywhere when it is not: one section, and no heading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum DaySection {
    #[default]
    None,
    Today,
    Yesterday,
    LastWeek,
    LastFortnight,
    Older,
}

impl DaySection {
    /// The section `ts` falls in, given the local midnights that start
    /// today, yesterday, the last 7 days and the last 14 days
    /// (`crate::datefmt::day_starts`). Mail dated ahead of the clock counts
    /// as today's.
    pub(crate) fn of(ts: i64, starts: &[i64; 4]) -> Self {
        let [today, yesterday, week, fortnight] = *starts;
        if ts >= today {
            DaySection::Today
        } else if ts >= yesterday {
            DaySection::Yesterday
        } else if ts >= week {
            DaySection::LastWeek
        } else if ts >= fortnight {
            DaySection::LastFortnight
        } else {
            DaySection::Older
        }
    }

    pub(crate) fn label(self) -> String {
        match self {
            DaySection::None => String::new(),
            DaySection::Today => i18n("Today"),
            DaySection::Yesterday => i18n("Yesterday"),
            DaySection::LastWeek => i18n("Last 7 days"),
            DaySection::LastFortnight => i18n("Last 14 days"),
            DaySection::Older => i18n("Older"),
        }
    }
}

/// One row's worth of the list: the message and what the row says about it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RowData {
    pub(super) msg: Rc<Message>,
    pub(super) meta: RowMeta,
    /// The date heading it sits under (#435): a conversation's replies share
    /// their head's.
    pub(super) section: DaySection,
}

impl RowData {
    pub(crate) fn slot(&self) -> Slot {
        (self.msg.account_id, self.msg.folder_id, self.msg.id)
    }
}

#[cfg(test)]
mod characterization_tests;
