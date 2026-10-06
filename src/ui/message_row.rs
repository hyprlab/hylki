//! One row of the message list. A `gtk::ListView` builds a screenful of
//! these and binds each to whichever message scrolls into view, so a row is
//! a plain widget tree filled synchronously on bind: a relm4 component's
//! update lands a frame later, and a recycled row would show the message it
//! held before for that frame.
//!
//! Also here: the list's model (`MessageModel`, which makes a row's item only
//! when the view asks for it), the swipe surface and the face lookups.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::ObjectSubclassIsExt;

use crate::config::ListColumn;
use crate::i18n::{i18n, i18n_f};
use crate::models::{Importance, Message};
use crate::ui::column_bin::ColumnBin;
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use crate::ui::message_list::MessageListInput;

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

/// A full swipe (#swipe): also `AdwSwipeable`'s reported `distance`, the px
/// one full drag (progress ±1.0) spans.
pub const SWIPE_MAX: f64 = 120.0;

/// How far a thread member's node dot reaches left of the row's content box
/// (`.thread-node`: 8px wide, pulled 5px out by its negative margin, plus a
/// 2px masking ring), where it sits centred on the group's rail. The last
/// reply's rail stub reaches 2px the same way. The swipe surface's clip
/// leaves this much room on the left, or both come out cut in half.
const THREAD_NODE_REACH: f32 = 8.0;
/// The single line's name columns, in pixels (#334): wide enough for most
/// names, and fixed so the columns after them line up.
const SENDER_COLUMN_PX: i32 = 160;
const PEOPLE_COLUMN_PX: i32 = 200;
const ACCOUNT_COLUMN_PX: i32 = 110;
/// The star's and the paperclip's size on a single line.
const LINE_ICON_PX: i32 = 12;

/// The width a single-line column takes until it is dragged to another
/// (#334): negative for a date, which is as wide as its text.
pub fn default_width(column: ListColumn) -> i32 {
    match column {
        ListColumn::Sender | ListColumn::Recipients => SENDER_COLUMN_PX,
        ListColumn::Correspondents => PEOPLE_COLUMN_PX,
        ListColumn::Account => ACCOUNT_COLUMN_PX,
        _ => -1,
    }
}

/// Whether a column's width can be dragged: the ones of text, but not the
/// subject, which takes the rest, or the tags, as wide as they are.
pub fn resizable(column: ListColumn) -> bool {
    matches!(
        column,
        ListColumn::Sender
            | ListColumn::Recipients
            | ListColumn::Correspondents
            | ListColumn::Account
            | ListColumn::Due
            | ListColumn::Date
    )
}

/// The width a column takes in `look`.
fn column_width(look: &RowLook, column: ListColumn) -> i32 {
    look.widths.get(&column).copied().unwrap_or_else(|| default_width(column))
}

/// Distance past which the indicator reads as "armed" (full color) — purely
/// a visual cue; `AdwSwipeTracker` makes the real commit decision on
/// release, factoring in velocity too.
pub const SWIPE_ARM: f64 = 72.0;

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
pub type ThreadDragKeys = HashMap<(u32, u32), Vec<(u32, u32, u32, u32)>>;

/// Where a message sits in the list: its account, folder and id. The id
/// alone is a UID, which only means something inside one folder (#317).
pub type Slot = (u32, u32, u32);

/// What a row says about its conversation, worked out by the list's rebuild.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowMeta {
    /// Size of the conversation (thread heads only; 1 or 0 otherwise).
    pub count: usize,
    /// Whether the chip can open anything: the folder holds more of it.
    pub expandable: bool,
    /// A reply nested under a thread head — indented on the rail.
    pub is_child: bool,
    /// The last reply of its thread: the dotted rail stops at its node.
    pub is_last: bool,
    /// The head's conversation is opened out in the list.
    pub expanded: bool,
    /// The conversation key, on a head that can toggle.
    pub key: Option<(u32, String)>,
    /// The newest member's sender, preview and time (thread heads only):
    /// the row speaks for the conversation's latest message.
    pub from: Option<(String, String)>,
    pub preview: Option<String>,
    pub latest: Option<String>,
    /// When that newest member arrived, and its date header, for a short
    /// date on a one-line row (#334).
    pub latest_at: Option<(i64, String)>,
    /// Any message of the conversation is unread / starred (heads only).
    pub unread: bool,
    pub starred: bool,
    /// Who took part in the conversation, for the Correspondents column
    /// (#334); worked out only while that column is shown.
    pub people: Option<String>,
    /// The row's height is open. False only while a reply slides shut
    /// before it is taken out of the list.
    pub revealed: bool,
    /// A reply just put in by opening its conversation: it slides open the
    /// first time it is shown instead of simply appearing.
    pub appear: bool,
    /// The group the row heads, for asking the cache how big it is (#222).
    pub group: Option<(u32, String)>,
}

/// One row's worth of the list: the message and what the row says about it.
#[derive(Clone, Debug, PartialEq)]
pub struct RowData {
    pub msg: Rc<Message>,
    pub meta: RowMeta,
}

impl RowData {
    pub fn slot(&self) -> Slot {
        (self.msg.account_id, self.msg.folder_id, self.msg.id)
    }
}

// ─── The model ─────────────────────────────────────────────────────────────

glib::wrapper! {
    /// The view's handle on one row of the list. Made only when the view
    /// asks for that position, and kept for as long as the view holds it.
    pub struct RowItem(ObjectSubclass<item_imp::RowItem>);
}

mod item_imp {
    use std::cell::RefCell;
    use std::rc::{Rc, Weak};

    use gtk::glib;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub struct RowItem {
        pub data: RefCell<Option<Rc<super::RowData>>>,
        /// The row widget showing this item, while it is bound.
        pub row: RefCell<Weak<super::Row>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RowItem {
        const NAME: &'static str = "HylkiRowItem";
        type Type = super::RowItem;
    }

    impl ObjectImpl for RowItem {}
}

impl RowItem {
    fn new(data: Rc<RowData>) -> Self {
        let item: RowItem = glib::Object::new();
        item.imp().data.replace(Some(data));
        item
    }

    pub fn data(&self) -> Rc<RowData> {
        self.imp().data.borrow().clone().expect("a row item always holds its data")
    }

    /// New data for the same message: the row showing it, if any, follows.
    fn set_data(&self, data: Rc<RowData>) {
        let old = self.imp().data.replace(Some(data.clone()));
        let row = self.imp().row.borrow().upgrade();
        if let Some(row) = row {
            row.refresh();
            // Thread links arriving can give a kept row's conversation a new
            // key. A row is not bound again for that, so its size is asked
            // for here, as binding would, or the badge never comes (#351).
            let moved = old.is_some_and(|o| o.meta.group != data.meta.group);
            if let (true, Some(group), Some(shared)) = (moved, data.meta.group.clone(), row.shared()) {
                shared.want(group);
            }
        }
    }
}

glib::wrapper! {
    /// Every row of the list, in order, as `RowData`. Items for the view are
    /// made on demand and shared while alive, keyed by the message's slot, so
    /// the same message keeps the same item (and the same bound row) across
    /// a rebuild.
    pub struct MessageModel(ObjectSubclass<model_imp::MessageModel>)
        @implements gtk::gio::ListModel;
}

mod model_imp {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use gtk::gio;
    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub struct MessageModel {
        pub rows: RefCell<Vec<Rc<super::RowData>>>,
        pub items: RefCell<HashMap<super::Slot, glib::WeakRef<super::RowItem>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageModel {
        const NAME: &'static str = "HylkiMessageModel";
        type Type = super::MessageModel;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for MessageModel {}

    impl ListModelImpl for MessageModel {
        fn item_type(&self) -> glib::Type {
            super::RowItem::static_type()
        }

        fn n_items(&self) -> u32 {
            self.rows.borrow().len() as u32
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            let data = self.rows.borrow().get(position as usize).cloned()?;
            let slot = data.slot();
            let live = self.items.borrow().get(&slot).and_then(|w| w.upgrade());
            if let Some(item) = live {
                if !Rc::ptr_eq(&item.data(), &data) {
                    item.set_data(data);
                }
                return Some(item.upcast());
            }
            let item = super::RowItem::new(data);
            self.items.borrow_mut().insert(slot, item.downgrade());
            Some(item.upcast())
        }
    }
}

/// One step of turning the rows on screen into a new list, in row order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RowEdit {
    /// The row stays, showing the next new row.
    Keep,
    /// New row `i` goes in here.
    Insert(usize),
    /// The row goes.
    Remove,
}

/// The edits that turn `old` into `new`, as far as `old` goes, and the
/// first of `new` left to add after them.
pub fn row_edits<K: std::hash::Hash + Eq + Copy>(old: &[K], new: &[K]) -> (Vec<RowEdit>, usize) {
    let at = |keys: &[K]| -> HashMap<K, usize> { keys.iter().enumerate().map(|(n, k)| (*k, n)).collect() };
    let (old_at, new_at) = (at(old), at(new));
    let mut edits = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < new.len() && j < old.len() {
        if new[i] == old[j] {
            edits.push(RowEdit::Keep);
            i += 1;
            j += 1;
            continue;
        }
        // How far ahead the new row sits among the old ones, and the old
        // row among the new: the nearer one is the one that moved, so a
        // conversation a reply lifts to the top costs two edits, not every
        // row it passed.
        let new_row_ahead = old_at.get(&new[i]).filter(|&&n| n > j).map(|n| n - j);
        let old_row_ahead = new_at.get(&old[j]).filter(|&&n| n > i).map(|n| n - i);
        let remove = match (new_row_ahead, old_row_ahead) {
            (_, None) => true,
            (None, Some(_)) => false,
            (Some(a), Some(b)) => a <= b,
        };
        if remove {
            edits.push(RowEdit::Remove);
            j += 1;
        } else {
            edits.push(RowEdit::Insert(i));
            i += 1;
        }
    }
    (edits, i)
}

impl Default for MessageModel {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl MessageModel {
    pub fn len(&self) -> usize {
        self.imp().rows.borrow().len()
    }

    pub fn row(&self, pos: usize) -> Option<Rc<RowData>> {
        self.imp().rows.borrow().get(pos).cloned()
    }

    /// Replace the list with `new`, telling the view only what changed:
    /// rows that show the same message keep their place (and their bound
    /// widget, refreshed with the new data), and the rest are put in or
    /// taken out in runs.
    pub fn replace(&self, new: Vec<Rc<RowData>>) {
        let old: Vec<Slot> = self.imp().rows.borrow().iter().map(|r| r.slot()).collect();
        let keys: Vec<Slot> = new.iter().map(|r| r.slot()).collect();
        let (edits, tail) = row_edits(&old, &keys);
        // A run of inserts and removals at `start`, applied as one change.
        let mut start = 0usize;
        let mut removed = 0usize;
        let mut added: Vec<Rc<RowData>> = Vec::new();
        let mut pos = 0usize;
        let mut i = 0usize;
        let flush = |start: usize, removed: &mut usize, added: &mut Vec<Rc<RowData>>| -> usize {
            let n = added.len();
            if *removed > 0 || n > 0 {
                self.imp()
                    .rows
                    .borrow_mut()
                    .splice(start..start + *removed, std::mem::take(added));
                self.items_changed(start as u32, *removed as u32, n as u32);
            }
            *removed = 0;
            n
        };
        let mut in_run = false;
        for edit in edits {
            match edit {
                RowEdit::Keep => {
                    if in_run {
                        pos = start + flush(start, &mut removed, &mut added);
                        in_run = false;
                    }
                    self.set_row(pos, new[i].clone());
                    pos += 1;
                    i += 1;
                }
                RowEdit::Insert(n) => {
                    if !in_run {
                        start = pos;
                        in_run = true;
                    }
                    added.push(new[n].clone());
                    i += 1;
                }
                RowEdit::Remove => {
                    if !in_run {
                        start = pos;
                        in_run = true;
                    }
                    removed += 1;
                }
            }
        }
        if in_run {
            pos = start + flush(start, &mut removed, &mut added);
        }
        // Whatever is left of the old list goes; the rest of the new joins.
        let left = self.len() - pos;
        added.extend(new[tail..].iter().cloned());
        removed = left;
        flush(pos, &mut removed, &mut added);
        self.imp().items.borrow_mut().retain(|_, w| w.upgrade().is_some());
    }

    /// New data for the row at `pos`, showing the same message.
    pub fn set_row(&self, pos: usize, data: Rc<RowData>) {
        let slot = data.slot();
        {
            let mut rows = self.imp().rows.borrow_mut();
            let Some(row) = rows.get_mut(pos) else { return };
            // A rebuild makes new data for every row; one that says the same
            // as before is not drawn again (#330).
            if Rc::ptr_eq(row, &data) || **row == *data {
                return;
            }
            *row = data.clone();
        }
        let live = self.imp().items.borrow().get(&slot).and_then(|w| w.upgrade());
        if let Some(item) = live {
            item.set_data(data);
        }
    }

    /// Change the row at `pos` in place.
    pub fn update_row(&self, pos: usize, f: impl FnOnce(&mut RowData)) {
        let Some(row) = self.row(pos) else { return };
        let mut data = (*row).clone();
        f(&mut data);
        self.set_row(pos, Rc::new(data));
    }

    pub fn splice(&self, pos: usize, removed: usize, added: Vec<Rc<RowData>>) {
        let n = added.len();
        self.imp().rows.borrow_mut().splice(pos..pos + removed, added);
        self.items_changed(pos as u32, removed as u32, n as u32);
    }
}

// ─── Shared state ──────────────────────────────────────────────────────────

/// How every row looks: the list's settings, read on each bind.
#[derive(Clone, Debug)]
pub struct RowLook {
    pub gravatar: bool,
    /// Whether the avatar is drawn at all (#29).
    pub avatars: bool,
    /// Whether a sender's site icon may fill it (#30).
    pub sender_logos: bool,
    /// One line per message, in columns, instead of the three-line card
    /// (#334).
    pub single_line: bool,
    /// Those columns, in order, the Due column left out of a list without
    /// Microsoft 365 mail in it.
    pub columns: Vec<ListColumn>,
    /// The widths columns were dragged to; the rest keep their own.
    pub widths: HashMap<ListColumn, i32>,
    /// How many lines of the message's text a row shows (0–3).
    pub preview_lines: u32,
    /// Whether the subject line is drawn (Focus Mode can take it away).
    pub show_subject: bool,
    /// Whether rows carry the actions palette at all.
    pub show_palette: bool,
    /// The list shows Junk: the palette's spam button reads "Not Spam".
    pub in_junk: bool,
    /// The list shows Drafts: no read/unread toggle, a draft is neither.
    pub in_drafts: bool,
    /// Sent-folder rows name the recipient, not the sender (#27).
    pub show_recipient: bool,
    /// The list is one person's mail (the People view), which the People
    /// pane already names: a card leads with the subject, without the name
    /// or the circle, and mail the user wrote says so. Holds the user's
    /// addresses.
    pub one_person: Option<std::rc::Rc<crate::people::Own>>,
    /// The accounts whose avatars wear their color as a ring (unified view).
    pub ringed: std::collections::HashSet<u32>,
    /// Whether conversations open out in the list at all.
    pub thread_expansion: bool,
    /// Bumped whenever the circles must be looked up again (contact photos
    /// changed, Gravatar or logos switched).
    pub face_gen: u64,
    /// Bumped whenever the tag definitions change.
    pub tags_gen: u64,
}

impl Default for RowLook {
    fn default() -> Self {
        RowLook {
            gravatar: false,
            avatars: true,
            sender_logos: false,
            single_line: false,
            columns: ListColumn::DEFAULT.to_vec(),
            widths: HashMap::new(),
            preview_lines: 1,
            show_subject: true,
            show_palette: true,
            in_junk: false,
            in_drafts: false,
            show_recipient: false,
            one_person: None,
            ringed: Default::default(),
            thread_expansion: true,
            face_gen: 0,
            tags_gen: 0,
        }
    }
}

/// What every row shares with the list.
pub struct RowShared {
    pub look: RefCell<RowLook>,
    /// The tags (#71): the chips a row shows are the message's keywords that
    /// name one of these.
    pub tags: RefCell<Vec<crate::config::Tag>>,
    /// Each account's name, for the Account column (#334).
    pub account_names: RefCell<HashMap<u32, String>>,
    /// How long an actions palette stays open after the pointer leaves it.
    pub palette_collapse_secs: Cell<u64>,
    /// Open the palette on row hover.
    pub palette_hover: Cell<bool>,
    /// Swap which side a swipe archives or deletes on.
    pub swipe_reversed: Cell<bool>,
    /// Whether the swipe gesture is on at all (#92).
    pub swipe_enabled: Cell<bool>,
    /// How sensitive a trackpad's two-finger swipe is.
    pub swipe_sensitivity: Cell<f64>,
    pub input: relm4::Sender<MessageListInput>,
    pub model: MessageModel,
    pub selection: gtk::MultiSelection,
    pub view: glib::WeakRef<gtk::ListView>,
    pub thread_drag: RefCell<ThreadDragKeys>,
    /// Every row widget the view has made, by its list item.
    rows: RefCell<HashMap<usize, Rc<Row>>>,
    /// The row whose palette is open: one at a time.
    open_palette: RefCell<Weak<Row>>,
    /// Conversations shown on rows whose size nobody has asked about yet.
    wanted: RefCell<Vec<(u32, String)>>,
    wanted_queued: Cell<bool>,
}

impl RowShared {
    pub fn new(input: relm4::Sender<MessageListInput>) -> Rc<Self> {
        let model = MessageModel::default();
        let selection = gtk::MultiSelection::new(Some(model.clone()));
        let privacy = crate::config::load_privacy();
        Rc::new(RowShared {
            look: RefCell::new(RowLook::default()),
            tags: RefCell::new(Vec::new()),
            account_names: RefCell::new(HashMap::new()),
            palette_collapse_secs: Cell::new(5),
            palette_hover: Cell::new(privacy.list_palette_hover),
            swipe_reversed: Cell::new(privacy.swipe_reversed),
            swipe_enabled: Cell::new(privacy.swipe_enabled),
            swipe_sensitivity: Cell::new(crate::config::load_swipe_sensitivity()),
            input,
            model,
            selection,
            view: glib::WeakRef::new(),
            thread_drag: RefCell::new(HashMap::new()),
            rows: RefCell::new(HashMap::new()),
            open_palette: RefCell::new(Weak::new()),
            wanted: RefCell::new(Vec::new()),
            wanted_queued: Cell::new(false),
        })
    }

    /// The rows showing a message right now.
    pub fn bound_rows(&self) -> Vec<Rc<Row>> {
        self.rows.borrow().values().filter(|r| r.st.borrow().item.is_some()).cloned().collect()
    }

    /// The row showing the list's position `pos`, if it is on screen.
    pub fn row_at(&self, pos: usize) -> Option<Rc<Row>> {
        self.bound_rows().into_iter().find(|r| r.position() == Some(pos))
    }

    /// Fill every row on screen again (the look changed).
    pub fn refresh_all(&self) {
        for row in self.bound_rows() {
            row.refresh();
        }
    }

    /// Ask the list, once the current pass is over, about the conversations
    /// rows have just started showing (#222).
    pub fn want(&self, group: (u32, String)) {
        self.wanted.borrow_mut().push(group);
        if !self.wanted_queued.replace(true) {
            let input = self.input.clone();
            glib::idle_add_local_once(move || {
                let _ = input.send(MessageListInput::AskThreads);
            });
        }
    }

    pub fn take_wanted(&self) -> Vec<(u32, String)> {
        self.wanted_queued.set(false);
        std::mem::take(&mut *self.wanted.borrow_mut())
    }

    /// The keys of the selected rows, in list order, for a drag.
    fn selected_drag_keys(&self) -> Vec<(u32, u32, u32, u32)> {
        let set = self.selection.selection();
        let mut out = Vec::new();
        let rows = self.model.imp().rows.borrow();
        if let Some((iter, first)) = gtk::BitsetIter::init_first(&set) {
            for pos in std::iter::once(first).chain(iter) {
                if let Some(r) = rows.get(pos as usize) {
                    out.push((r.msg.account_id, r.msg.folder_id, r.msg.uid, r.msg.id));
                }
            }
        }
        out
    }
}

/// The view's row factory: makes rows, binds them to items and back.
pub fn factory(shared: &Rc<RowShared>) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let key = |obj: &glib::Object| obj.as_ptr() as usize;
    {
        let shared = Rc::downgrade(shared);
        factory.connect_setup(move |_, obj| {
            let (Some(shared), Some(item)) = (shared.upgrade(), obj.downcast_ref::<gtk::ListItem>()) else {
                return;
            };
            let row = Row::new(&shared, item);
            item.set_child(Some(&row.w.host));
            shared.rows.borrow_mut().insert(key(obj), row);
        });
    }
    {
        let shared = Rc::downgrade(shared);
        factory.connect_bind(move |_, obj| {
            let Some(shared) = shared.upgrade() else { return };
            let row = shared.rows.borrow().get(&key(obj)).cloned();
            let item = obj.downcast_ref::<gtk::ListItem>().and_then(|li| li.item()).and_downcast::<RowItem>();
            if let (Some(row), Some(item)) = (row, item) {
                row.bind(&item);
            }
        });
    }
    {
        let shared = Rc::downgrade(shared);
        factory.connect_unbind(move |_, obj| {
            let Some(shared) = shared.upgrade() else { return };
            let row = shared.rows.borrow().get(&key(obj)).cloned();
            if let Some(row) = row {
                row.unbind();
            }
        });
    }
    {
        let shared = Rc::downgrade(shared);
        factory.connect_teardown(move |_, obj| {
            let Some(shared) = shared.upgrade() else { return };
            let row = shared.rows.borrow_mut().remove(&key(obj));
            if let Some(row) = row {
                row.unbind();
            }
        });
    }
    factory
}

// ─── Faces ─────────────────────────────────────────────────────────────────

/// A background face lookup's answer, correlated by sender address (a recycled
/// row compares before using it). The tiers are personal-first: the contact's
/// own photo, their Gravatar, then the icon their domain publishes (#30), with
/// the UI's colored initials as the implicit last resort.
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

/// Display names from a raw To header: "Ann <a@x>, b@y" -> "Ann, b@y".
pub fn recipient_names(to: &str) -> String {
    let mut names: Vec<String> = Vec::new();
    for part in to.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let name = match part.split_once('<') {
            Some((n, _)) if !n.trim().trim_matches('"').is_empty() => n.trim().trim_matches('"').to_string(),
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

/// Fill `bar` with the single line's column headings (#334), laid out as a
/// row is so each sits over its column. `sorted` is the column the list is
/// sorted by and whether it runs downwards; a sortable heading sorts the
/// list by its column when clicked.
pub fn fill_headings(
    bar: &gtk::Box,
    look: &RowLook,
    sorted: Option<(ListColumn, bool)>,
    sortable: &dyn Fn(ListColumn) -> bool,
    click: Rc<dyn Fn(ListColumn)>,
    resize: Rc<dyn Fn(ListColumn, Option<i32>, bool)>,
) -> HashMap<ListColumn, ColumnBin> {
    let mut bins = HashMap::new();
    while let Some(child) = bar.first_child() {
        bar.remove(&child);
    }
    let mut classes = vec!["message-row", "single-line", "list-headings"];
    if !look.avatars {
        classes.push("no-avatar");
    }
    bar.set_css_classes(&classes);
    bar.set_spacing(8);
    // The avatar's and the unread dot's places.
    if look.avatars {
        let room = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        room.set_size_request(16, -1);
        bar.append(&room);
    }
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.set_size_request(10, -1);
    bar.append(&dot);
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.set_hexpand(true);
    bar.append(&line);

    let subject_at = look.columns.iter().position(|c| *c == ListColumn::Subject).unwrap_or(0);
    for (at, &column) in look.columns.iter().enumerate() {
        let arrow = sorted.filter(|(c, _)| *c == column).map(|(_, down)| if down { " \u{25BE}" } else { " \u{25B4}" });
        let text = |name: String| {
            let label = gtk::Label::new(Some(&format!("{name}{}", arrow.unwrap_or(""))));
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.add_css_class("list-heading");
            label
        };
        let icon = |name: &str, px: i32| {
            let image = gtk::Image::from_icon_name(name);
            image.set_pixel_size(px);
            image.add_css_class("list-heading");
            image.upcast::<gtk::Widget>()
        };
        let mut bin = None;
        let mut binned = |label: gtk::Label| {
            let b = ColumnBin::new(column_width(look, column));
            b.set_child(Some(&label));
            bin = Some(b.clone());
            b.upcast::<gtk::Widget>()
        };
        let cell: gtk::Widget = match column {
            ListColumn::Star => icon("starred-symbolic", LINE_ICON_PX),
            ListColumn::Attachment => icon("mail-attachment-symbolic", LINE_ICON_PX),
            ListColumn::Importance => icon("emblem-important-symbolic", -1),
            ListColumn::Sender => binned(text(if look.show_recipient { i18n("To") } else { i18n("From") })),
            ListColumn::Recipients => binned(text(i18n("To"))),
            ListColumn::Correspondents | ListColumn::Account => binned(text(i18n(column.label()))),
            ListColumn::Subject => {
                let b = ColumnBin::new(0);
                b.set_child(Some(&text(i18n(column.label()))));
                b.set_hexpand(true);
                b.upcast()
            }
            ListColumn::Tags => text(i18n(column.label())).upcast(),
            ListColumn::Due | ListColumn::Date => {
                let label = text(if column == ListColumn::Due { i18n("Due") } else { i18n(column.label()) });
                label.set_width_chars(9);
                label.set_xalign(1.0);
                binned(label)
            }
        };
        if let Some(bin) = &bin {
            bins.insert(column, bin.clone());
        }
        let cell = match bin {
            Some(bin) if resizable(column) => resize_handle(cell, &bin, column, at < subject_at, resize.clone()),
            _ => cell,
        };
        cell.set_tooltip_text(Some(&i18n(column.label())));
        if arrow.is_some() {
            cell.add_css_class("sorted");
        }
        if sortable(column) {
            cell.set_cursor_from_name(Some("pointer"));
            let click = click.clone();
            let gesture = gtk::GestureClick::new();
            gesture.connect_released(move |_, _, _, _| click(column));
            cell.add_controller(gesture);
        }
        line.append(&cell);
    }
    bins
}

/// A heading with a handle for dragging its column's width (#334), on the
/// edge it shares with the subject: the right edge of a column before it,
/// the left of one after it, so the edge follows the pointer. Each step of
/// a drag is handed to `resize`, the last marked done; a double click puts
/// the column back to its own width.
fn resize_handle(
    cell: gtk::Widget,
    bin: &ColumnBin,
    column: ListColumn,
    before_subject: bool,
    resize: Rc<dyn Fn(ListColumn, Option<i32>, bool)>,
) -> gtk::Widget {
    const MIN: i32 = 32;
    const MAX: i32 = 800;
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&cell));
    let handle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    handle.add_css_class("column-resize-handle");
    handle.set_size_request(8, -1);
    handle.set_halign(if before_subject { gtk::Align::End } else { gtk::Align::Start });
    handle.set_cursor_from_name(Some("col-resize"));
    handle.set_tooltip_text(Some(&i18n("Drag to resize, double-click for the usual width")));
    overlay.add_overlay(&handle);

    let drag = gtk::GestureDrag::new();
    let start = Rc::new(Cell::new(0));
    let last = Rc::new(Cell::new(0));
    // Where the press was, across the window. The drag's own offsets are
    // measured from the handle, which moves with the edge it drags, so each
    // step would count the last one's move again and the edge would shake.
    let pressed_at = Rc::new(Cell::new(0.0f32));
    // The pointer across the window, from a point on the handle as it is now.
    fn across(handle: &gtk::Widget, x: f64, y: f64) -> Option<f32> {
        let root = handle.root()?;
        handle.compute_point(&root, &gtk::graphene::Point::new(x as f32, y as f32)).map(|p| p.x())
    }
    {
        let (bin, start, last, pressed_at) = (bin.clone(), start.clone(), last.clone(), pressed_at.clone());
        drag.connect_drag_begin(move |g, x, y| {
            if let Some(at) = g.widget().and_then(|h| across(&h, x, y)) {
                pressed_at.set(at);
            }
            // Its own, so the heading under it does not take the press for
            // a click that sorts.
            g.set_state(gtk::EventSequenceState::Claimed);
            let asked = bin.asked_width();
            let now = if asked > 0 { asked } else { bin.width() };
            start.set(now);
            last.set(now);
        });
    }
    {
        let (start, last, resize) = (start.clone(), last.clone(), resize.clone());
        drag.connect_drag_update(move |g, dx, dy| {
            let Some((x, y)) = g.start_point() else { return };
            let Some(now) = g.widget().and_then(|h| across(&h, x + dx, y + dy)) else { return };
            let dx = (now - pressed_at.get()).round() as i32;
            let px = (start.get() + if before_subject { dx } else { -dx }).clamp(MIN, MAX);
            if px != last.get() {
                last.set(px);
                // The list sets the heading's width with the rows', fitted
                // to the pane the same way.
                resize(column, Some(px), false);
            }
        });
    }
    {
        let (start, last, resize) = (start.clone(), last.clone(), resize.clone());
        // A press that never moved (half of a double click) changes nothing.
        drag.connect_drag_end(move |_, _, _| {
            if last.get() != start.get() {
                resize(column, Some(last.get()), true);
            }
        });
    }
    let click = gtk::GestureClick::new();
    {
        let (bin, resize) = (bin.clone(), resize.clone());
        click.connect_pressed(move |g, n, _, _| {
            if n == 2 {
                g.set_state(gtk::EventSequenceState::Claimed);
                bin.set_width(default_width(column));
                resize(column, None, true);
            }
        });
    }
    // One group: the drag claiming the press must not deny the double
    // click on the same handle.
    click.group_with(&drag);
    handle.add_controller(drag);
    handle.add_controller(click);
    overlay.upcast()
}

/// A follow-up flag's due date for the Due column (#334): the day alone,
/// the year too when it is not this one. Empty when there is none.
fn due_label(due: i64) -> String {
    if due <= 0 {
        String::new()
    } else if crate::datefmt::year(due) == crate::datefmt::year(crate::datefmt::now()) {
        crate::datefmt::day_month(due)
    } else {
        crate::datefmt::day_month_year(due)
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
            MenuEntry::new(t.name.clone(), move || toggle(keyword.clone(), !on)).swatch(t.color.clone(), on)
        })
        .collect()
}

// ─── The swipe surface ─────────────────────────────────────────────────────

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
        self.imp().progress_px.set(px);
        self.queue_allocate();
    }

    /// Read back as the "from" value when animating a released drag
    /// smoothly back to rest.
    fn progress_px(&self) -> f64 {
        self.imp().progress_px.get()
    }

    /// The trackpad sensitivity preference, so both `AdwSwipeable::distance`
    /// and the tracker's own callback see the same figure.
    fn set_sensitivity(&self, factor: f64) {
        self.imp()
            .sensitivity
            .set(factor.clamp(crate::config::SWIPE_SENSITIVITY_MIN, crate::config::SWIPE_SENSITIVITY_MAX));
    }

    fn sensitivity(&self) -> f64 {
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
            Self { progress_px: Cell::new(0.0), sensitivity: Cell::new(1.0) }
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

        /// `progress_px` flipped into the row's "dragged left is negative"
        /// convention — shared by `size_allocate` and `snapshot` so they
        /// can't disagree about which side is revealed.
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
            self.foreground().map(|c| c.request_mode()).unwrap_or(gtk::SizeRequestMode::ConstantSize)
        }

        // The background strip never dictates the row's size — only the
        // real content does; the strip is simply stretched to match it.
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            self.foreground().map(|c| c.measure(orientation, for_size)).unwrap_or((0, 0, -1, -1))
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(bg) = self.background() {
                bg.allocate(width, height, baseline, None);
            }
            if let Some(fg) = self.foreground() {
                let offset = self.visual_offset();
                let transform = (offset != 0.0)
                    .then(|| gtk::gsk::Transform::new().translate(&gtk::graphene::Point::new(offset, 0.0)));
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
        // touchscreen drag by this, and `Row::wire_swipe` multiplies the
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

        fn swipe_area(&self, _navigation_direction: adw::NavigationDirection, _is_drag: bool) -> gtk::gdk::Rectangle {
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
pub fn swipe_progress_px(progress: f64, sensitivity: f64) -> f64 {
    (progress * SWIPE_MAX * sensitivity).clamp(-SWIPE_MAX, SWIPE_MAX)
}

// ─── The row ───────────────────────────────────────────────────────────────

/// The row's widgets, built once.
pub struct RowWidgets {
    pub host: gtk::Box,
    revealer: gtk::Revealer,
    surface: SwipeSurface,
    swipe_bg: gtk::Box,
    swipe_inner: gtk::Box,
    swipe_icon: gtk::Image,
    swipe_label: gtk::Label,
    overlay: gtk::Overlay,
    rail_stub: gtk::Box,
    node: gtk::Box,
    actions_line: gtk::Box,
    chevron: gtk::Button,
    palette_clip: gtk::Overlay,
    palette_spacer: gtk::Box,
    palette_inner: gtk::Box,
    content: gtk::Box,
    avatar_revealer: gtk::Revealer,
    avatar: adw::Avatar,
    dot: gtk::Box,
    text: gtk::Box,
    /// The three-line card's first line, which the single-line layout
    /// borrows its widgets from (#334).
    top: gtk::Box,
    /// The single-line layout's one line (#334).
    line: gtk::Box,
    /// The single-line layout's own columns, which a card has no place for,
    /// each in the bin that sizes its column.
    recipients: gtk::Label,
    recipients_col: ColumnBin,
    people: gtk::Label,
    people_col: ColumnBin,
    importance: gtk::Image,
    account: gtk::Label,
    account_col: ColumnBin,
    due: gtk::Label,
    due_col: ColumnBin,
    /// The bins the sender, the subject and the date move into on one line.
    name_col: ColumnBin,
    subject_col: ColumnBin,
    date_col: ColumnBin,
    name: gtk::Label,
    clip: gtk::Image,
    star: gtk::Image,
    date: gtk::Label,
    chip: gtk::Button,
    chip_count: gtk::Label,
    chip_caret: gtk::Image,
    subject_line: gtk::Box,
    subject: gtk::Label,
    tags_box: gtk::Box,
    preview_line: gtk::Box,
    lock: gtk::Image,
    preview: gtk::Label,
}

/// The action palette's state-carrying buttons, once built.
struct PaletteButtons {
    /// Absent for a draft: a draft is neither read nor unread.
    read: Option<gtk::Button>,
    star: gtk::Button,
    tag: gtk::Button,
    /// Built for Junk ("Not spam") and Drafts (no read toggle) or not.
    built_for: (bool, bool),
}

/// What a row remembers between binds, and while one message is bound.
#[derive(Default)]
struct RowState {
    item: Option<RowItem>,
    /// Bumped on every bind and unbind, so a timer or lookup started for
    /// the message the row showed before can tell.
    gen: u64,
    avatar_texture: Option<gtk::gdk::Texture>,
    /// The initials circle drawn when no picture is known, kept per name so
    /// the avatar is handed the same object on every refresh.
    initials: Option<(String, crate::ui::initials::InitialsPaintable)>,
    /// The address and look generation the circle was last looked up for.
    face_for: Option<(String, u64)>,
    /// The keywords and tag generation the chips were built for.
    tags_for: Option<(Vec<String>, u64)>,
    avatar_shown: bool,
    hovered: bool,
    /// The columns the widgets are laid out in on one line, or `None` for
    /// the card (#334).
    columns: Option<Vec<ListColumn>>,
    dragging: bool,
    palette_open: bool,
    palette: Option<PaletteButtons>,
    palette_target: i32,
    palette_anim: Option<adw::TimedAnimation>,
    collapse_timer: Option<glib::SourceId>,
    /// Current swipe distance in px (negative = dragged left) — the source
    /// of truth while a gesture is live; reset to 0 the instant a release is
    /// resolved (the strip then animates back out of view).
    swipe_progress: f64,
    /// Which side last had a nonzero `swipe_progress` (-1 left, 1 right),
    /// kept once it returns to 0 so the revealed side and action don't flip
    /// mid-shrink after a release.
    swipe_side: i8,
    /// A mouse-button or trackpad gesture is actively dragging this row.
    swipe_dragging: bool,
    /// The release cleared the commit distance: the row is flying out to
    /// `swipe_side` while its Revealer closes.
    swipe_committing: bool,
    swipe_exit_started: bool,
    /// From the first drag until the snap-back lands: the `.swiping` class
    /// squares the pill off for that whole span.
    swipe_active: bool,
    swipe_anim: Option<adw::TimedAnimation>,
}

pub struct Row {
    me: Weak<Row>,
    shared: Weak<RowShared>,
    pub w: RowWidgets,
    st: RefCell<RowState>,
    list_item: glib::WeakRef<gtk::ListItem>,
    /// Kept alive for as long as the row: the tracker stops firing once
    /// dropped.
    tracker: RefCell<Option<adw::SwipeTracker>>,
}

impl Row {
    fn new(shared: &Rc<RowShared>, list_item: &gtk::ListItem) -> Rc<Self> {
        let w = build_widgets();
        let row = Rc::new_cyclic(|me| Row {
            me: me.clone(),
            shared: Rc::downgrade(shared),
            w,
            st: RefCell::new(RowState { avatar_shown: true, ..Default::default() }),
            list_item: list_item.downgrade(),
            tracker: RefCell::new(None),
        });
        row.wire();
        row
    }

    fn shared(&self) -> Option<Rc<RowShared>> {
        self.shared.upgrade()
    }

    fn send(&self, msg: MessageListInput) {
        if let Some(shared) = self.shared() {
            let _ = shared.input.send(msg);
        }
    }

    pub fn data(&self) -> Option<Rc<RowData>> {
        self.st.borrow().item.as_ref().map(|i| i.data())
    }

    /// The list position this row shows, while bound.
    pub fn position(&self) -> Option<usize> {
        let item = self.list_item.upgrade()?;
        self.st.borrow().item.as_ref()?;
        let pos = item.position();
        (pos != gtk::INVALID_LIST_POSITION).then_some(pos as usize)
    }

    pub fn has_focus_within(&self) -> bool {
        let host = self.w.host.upcast_ref::<gtk::Widget>();
        host.root().and_then(|r| r.focus()).is_some_and(|f| f == *host || f.is_ancestor(host))
    }

    /// Hook the row's own controllers up, once.
    fn wire(&self) {
        let weak = self.me.clone();
        let on = move |f: fn(&Row)| {
            let weak = weak.clone();
            move || {
                if let Some(row) = weak.upgrade() {
                    f(&row);
                }
            }
        };

        // Hover: the ⋯ fades in, and in hover mode the palette opens.
        let motion = gtk::EventControllerMotion::new();
        {
            let enter = on(|r| r.set_hover(true));
            motion.connect_enter(move |_, _, _| enter());
            let leave = on(|r| r.set_hover(false));
            motion.connect_leave(move |_| leave());
        }
        self.w.host.add_controller(motion);

        // Right-click: the row's menu, in the capture phase so the press
        // reaches it before anything inside the row (the count chip, the
        // palette's buttons) can take it; claimed, so none of them acts on
        // it afterwards, and the row is not selected.
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_SECONDARY);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let weak = self.me.clone();
            click.connect_pressed(move |g, _, x, y| {
                g.set_state(gtk::EventSequenceState::Claimed);
                if let Some(row) = weak.upgrade() {
                    row.context_menu(x, y);
                }
            });
        }
        self.w.host.add_controller(click);

        // Drag a message onto a sidebar folder to move it there. The payload
        // carries one (account, source folder, UID, id) group per message —
        // the whole selection when this row is part of it (#23), and every
        // member of a conversation its row stands for (#171).
        let drag = gtk::DragSource::new();
        drag.set_actions(gtk::gdk::DragAction::MOVE);
        {
            let weak = self.me.clone();
            drag.connect_prepare(move |_, _, _| weak.upgrade().and_then(|row| row.drag_payload()));
        }
        {
            let weak = self.me.clone();
            drag.connect_drag_begin(move |src, drag| {
                if let Some(row) = weak.upgrade() {
                    row.st.borrow_mut().dragging = true;
                    row.sync_host_classes();
                }
                // A white envelope, cursor-sized, centred under the pointer —
                // without an icon GTK draws the payload text. Set on the
                // drag's own icon window as a widget, so it is drawn at
                // logical size from a display-scale texture.
                let scale = src.widget().map(|w| w.scale_factor()).unwrap_or(1);
                if let Some(envelope) = crate::app_icon::drag_envelope(scale) {
                    let icon = gtk::DragIcon::for_drag(drag);
                    icon.set_child(Some(&envelope));
                    let half = crate::app_icon::DRAG_ICON_SIZE / 2;
                    drag.set_hotspot(half, half);
                }
            });
        }
        {
            let end = on(|r| {
                r.st.borrow_mut().dragging = false;
                r.sync_host_classes();
            });
            drag.connect_drag_end(move |_, _, _| end());
        }
        {
            let cancel = on(|r| {
                r.st.borrow_mut().dragging = false;
                r.sync_host_classes();
            });
            drag.connect_drag_cancel(move |_, _, _| {
                cancel();
                false
            });
        }
        self.w.host.add_controller(drag);

        // The ⋯ and the palette.
        {
            let toggle = on(Row::toggle_palette);
            self.w.chevron.connect_clicked(move |_| toggle());
        }
        let palette_motion = gtk::EventControllerMotion::new();
        {
            let enter = on(Row::cancel_collapse);
            palette_motion.connect_enter(move |_, _, _| enter());
            let leave = on(|r| {
                if r.st.borrow().palette_open {
                    r.arm_collapse();
                }
            });
            palette_motion.connect_leave(move |_| leave());
        }
        self.w.palette_inner.add_controller(palette_motion);
        // The clip is set once: the palette hangs clipped over the spacer
        // whose width is the one thing animated.
        self.w.palette_clip.set_clip_overlay(&self.w.palette_inner, true);

        // The conversation chip.
        {
            let toggle = on(|r| {
                if let Some(key) = r.data().and_then(|d| d.meta.key.clone()) {
                    r.send(MessageListInput::ToggleThread(key));
                }
            });
            self.w.chip.connect_clicked(move |_| toggle());
        }

        self.wire_swipe();
    }

    /// The `AdwSwipeTracker` driving the swipe-to-act gesture (#swipe):
    /// mouse-drag and trackpad both arrive as the same signals.
    fn wire_swipe(&self) {
        let tracker = adw::SwipeTracker::new(&self.w.surface);
        tracker.set_orientation(gtk::Orientation::Horizontal);
        tracker.set_allow_mouse_drag(true);
        // `AdwSwipeTracker`'s progress/snap-point convention runs opposite to
        // the row's "dragged left is negative" one for a horizontal tracker,
        // hence the negation — kept only where each value is consumed, since
        // `AdwSwipeable::progress` still answers the tracker in its own.
        {
            let weak = self.me.clone();
            let surface = self.w.surface.clone();
            tracker.connect_update_swipe(move |_, progress| {
                let raw_px = swipe_progress_px(progress, surface.sensitivity());
                if let Some(row) = weak.upgrade() {
                    if row.st.borrow().swipe_committing {
                        return;
                    }
                    surface.set_progress_px(raw_px);
                    row.swipe_update(-raw_px);
                }
            });
        }
        {
            let weak = self.me.clone();
            // The release's own resolved snap point isn't used: whether it
            // fires is decided on the distance alone (see `swipe_end`).
            tracker.connect_end_swipe(move |_, _velocity, _to| {
                if let Some(row) = weak.upgrade() {
                    row.swipe_end();
                }
            });
        }
        self.tracker.replace(Some(tracker));
    }

    // ── Binding ──

    fn bind(self: &Rc<Self>, item: &RowItem) {
        let fresh = self.st.borrow().item.as_ref() != Some(item);
        if fresh {
            self.reset();
            {
                let mut st = self.st.borrow_mut();
                st.item = Some(item.clone());
                st.gen += 1;
            }
            item.imp().row.replace(Rc::downgrade(self));
        }
        self.refresh();
        if fresh {
            let data = item.data();
            if data.meta.appear {
                // A reply just put in by opening its conversation: shown
                // folded, then slid open once it has been measured.
                self.w.revealer.set_transition_duration(0);
                self.w.revealer.set_reveal_child(false);
                self.w.revealer.set_transition_duration(200);
                let weak = Rc::downgrade(self);
                let gen = self.st.borrow().gen;
                glib::idle_add_local_once(move || {
                    let Some(row) = weak.upgrade() else { return };
                    if row.st.borrow().gen != gen {
                        return;
                    }
                    if let (Some(shared), Some(pos)) = (row.shared(), row.position()) {
                        shared.model.update_row(pos, |d| d.meta.appear = false);
                    }
                    row.w.revealer.set_reveal_child(true);
                });
            }
            if let (Some(group), Some(shared)) = (data.meta.group.clone(), self.shared()) {
                shared.want(group);
            }
        }
    }

    fn unbind(&self) {
        self.reset();
        let item = {
            let mut st = self.st.borrow_mut();
            st.gen += 1;
            st.item.take()
        };
        // Only if the item still points here: when GTK binds it to its next
        // row before unbinding it from this one, clearing it would leave the
        // row on screen deaf to every later change of the message, a read
        // mark included (#333).
        if let Some(item) = item {
            let mine = item.imp().row.borrow().upgrade().is_none_or(|r| std::ptr::eq(Rc::as_ptr(&r), self));
            if mine {
                item.imp().row.replace(Weak::new());
            }
        }
    }

    /// Put away whatever the row was doing for the message it showed: a
    /// palette open, a swipe under way, a circle on loan from a lookup.
    fn reset(&self) {
        let (palette_anim, swipe_anim, timer) = {
            let mut st = self.st.borrow_mut();
            st.palette_open = false;
            st.palette_target = 0;
            st.hovered = false;
            st.dragging = false;
            st.avatar_shown = true;
            st.swipe_progress = 0.0;
            st.swipe_side = 0;
            st.swipe_dragging = false;
            st.swipe_committing = false;
            st.swipe_exit_started = false;
            st.swipe_active = false;
            st.avatar_texture = None;
            st.face_for = None;
            (st.palette_anim.take(), st.swipe_anim.take(), st.collapse_timer.take())
        };
        if let Some(a) = palette_anim {
            a.pause();
        }
        if let Some(a) = swipe_anim {
            a.pause();
        }
        if let Some(t) = timer {
            t.remove();
        }
        if let Some(shared) = self.shared() {
            let open = shared.open_palette.borrow().upgrade();
            if open.is_some_and(|r| std::ptr::eq(Rc::as_ptr(&r), self)) {
                shared.open_palette.replace(Weak::new());
            }
        }
        self.w.palette_spacer.set_width_request(0);
        self.w.palette_inner.set_visible(false);
        self.w.surface.set_progress_px(0.0);
        self.w.revealer.set_transition_duration(0);
        self.w.revealer.set_reveal_child(true);
        self.w.revealer.set_transition_duration(200);
        self.w.avatar_revealer.set_transition_duration(0);
        self.w.avatar_revealer.set_reveal_child(true);
        self.w.avatar_revealer.set_transition_duration(crate::ui::FOCUS_ANIM_MS);
    }

    /// Fill the widgets from the bound message and the list's look.
    pub fn refresh(&self) {
        let Some(shared) = self.shared() else { return };
        let Some(data) = self.data() else { return };
        let look = shared.look.borrow().clone();
        let msg = &data.msg;
        let meta = &data.meta;
        let w = &self.w;
        self.arrange(look.single_line.then_some(&look.columns[..]));
        let single = look.single_line;
        // Whether a widget's column is on: a card shows what it always has.
        let col = |c: ListColumn| !single || look.columns.contains(&c);

        self.sync_host_classes();
        {
            let st = self.st.borrow();
            w.revealer.set_reveal_child(meta.revealed && !st.swipe_committing && !meta.appear);
        }

        // The thread rail and node.
        w.rail_stub.set_visible(meta.is_last);
        w.node.set_visible(meta.is_child);

        // The actions line: centred under the avatar with circles on (circle
        // centre minus half the button, plus a reply card's 10px indent),
        // hugging the pill's edge without.
        w.actions_line.set_visible(look.show_palette);
        w.actions_line.set_margin_start(match (look.avatars, meta.is_child) {
            (true, true) => 32,
            (true, false) => 22,
            (false, true) => 20,
            (false, false) => 10,
        });
        w.actions_line.set_margin_bottom(if look.avatars { 3 } else { 4 });
        self.sync_palette_classes();

        // The content box.
        let mut content = vec!["message-row"];
        if !look.avatars {
            content.push("no-avatar");
        }
        if !look.show_palette {
            content.push("no-palette");
        }
        if single {
            content.push("single-line");
        } else if look.show_palette && look.avatars && look.preview_lines == 0 {
            content.push("palette-room");
        }
        w.content.set_css_classes(&content);

        // The circle.
        w.avatar_revealer.set_visible(look.avatars);
        let shown = self.st.borrow().avatar_shown;
        w.avatar_revealer.set_reveal_child(shown);
        w.avatar_revealer.set_css_classes(if shown { &["focus-fade"] } else { &["focus-fade", "away"] });
        let ring = format!("vireo-acct-ring-{}", msg.account_id);
        if look.ringed.contains(&msg.account_id) {
            w.avatar.set_css_classes(&[ring.as_str()]);
        } else {
            w.avatar.set_css_classes(&[]);
        }
        if look.avatars {
            let email = self.face_email(&data, &look);
            let want = Some((email.clone(), look.face_gen));
            if self.st.borrow().face_for != want {
                self.st.borrow_mut().face_for = want;
                self.st.borrow_mut().avatar_texture = None;
                self.load_face(&email, &look);
            }
        }
        w.avatar.set_text(Some(&self.face_name(&data, &look)));
        w.avatar.set_custom_image(self.avatar_image(&data, &look).as_ref());

        // Faded rather than hidden: the slot is always reserved and only
        // the dot's ink changes, so text never jitters as mail is read.
        let unread = msg.unread || meta.unread;
        w.dot.set_valign(if look.avatars || single { gtk::Align::Center } else { gtk::Align::Start });
        w.dot.set_opacity(if unread { 1.0 } else { 0.0 });
        w.text.set_valign(if look.avatars { gtk::Align::Center } else { gtk::Align::Start });

        w.name_col.set_visible(col(ListColumn::Sender));
        let one_person = look.one_person.is_some() && !single;
        let name = if one_person { msg.subject.clone() } else { self.name_line(&data, &look) };
        w.name.set_label(&name);
        w.name.set_css_classes(if unread { &["message-sender", "unread"] } else { &["message-sender"] });
        // On one line an icon keeps its column whether lit or not, so the
        // columns after it line up.
        w.clip.set_visible(if single { col(ListColumn::Attachment) } else { msg.has_attachment });
        w.clip.set_opacity(if msg.has_attachment { 1.0 } else { 0.0 });
        let starred = msg.starred || meta.starred;
        w.star.set_visible(if single { col(ListColumn::Star) } else { starred });
        w.star.set_opacity(if starred { 1.0 } else { 0.0 });
        if single {
            self.fill_columns(&data, &look, &shared, unread);
        }
        w.date.set_visible(col(ListColumn::Date));
        w.date_col.set_visible(col(ListColumn::Date));
        if single {
            for (c, bin) in [
                (ListColumn::Sender, &w.name_col),
                (ListColumn::Recipients, &w.recipients_col),
                (ListColumn::Correspondents, &w.people_col),
                (ListColumn::Account, &w.account_col),
                (ListColumn::Due, &w.due_col),
                (ListColumn::Date, &w.date_col),
            ] {
                bin.set_width(column_width(&look, c));
            }
        }
        match (&meta.latest, &meta.latest_at) {
            (_, Some((ts, date))) if single => w.date.set_label(&crate::models::date_short(*ts, date)),
            (Some(latest), _) if !single => w.date.set_label(latest),
            _ if single => w.date.set_label(&crate::models::date_short(msg.timestamp, &msg.date)),
            _ => w.date.set_label(&msg.datetime_list()),
        }
        w.chip.set_visible(meta.count > 1);
        w.chip_count.set_label(&meta.count.to_string());
        w.chip_caret.set_visible(look.thread_expansion && meta.expandable);
        w.chip_caret.set_css_classes(if meta.expanded {
            &["thread-toggle-icon", "open"]
        } else {
            &["thread-toggle-icon"]
        });

        w.subject.set_visible(!one_person);
        // One line: the subject, then its text dimmed after it, in one label,
        // so the subject is cut only once the text has gone (#334).
        if single {
            let esc = |t: &str| gtk::glib::markup_escape_text(t).to_string();
            let text = self.preview_text(&data, &look, meta.preview.as_deref().unwrap_or(&msg.preview));
            let markup = if look.preview_lines > 0 && !text.is_empty() {
                format!("{} <span weight=\"normal\" alpha=\"55%\">— {}</span>", esc(&msg.subject), esc(&text))
            } else {
                esc(&msg.subject)
            };
            w.subject.set_markup(&markup);
        } else {
            w.subject.set_use_markup(false);
            w.subject.set_label(&msg.subject);
        }
        w.subject.set_css_classes(if unread { &["message-subject", "unread"] } else { &["message-subject"] });
        w.tags_box.set_visible(col(ListColumn::Tags));
        self.render_tags(msg, &shared, &look);
        // With the subject moved up, the line is left for the tags.
        w.subject_line.set_visible(look.show_subject && (!one_person || w.tags_box.first_child().is_some()));

        let preview = meta.preview.as_deref().unwrap_or(&msg.preview);
        w.preview_line.set_visible(look.preview_lines > 0);
        w.lock.set_visible(crate::models::preview_is_encrypted(preview));
        let shown = self.preview_text(&data, &look, preview);
        if single {
            // Carried by the subject's label on one line.
            w.preview.set_visible(false);
        } else {
            w.preview.set_visible(true);
            w.preview.set_label(&shown);
            w.preview.set_wrap(look.preview_lines > 1);
            w.preview.set_lines(look.preview_lines.max(1) as i32);
        }

        w.surface.set_sensitivity(shared.swipe_sensitivity.get());
        if let Some(t) = self.tracker.borrow().as_ref() {
            // A committing row takes no new gestures — it is on its way out.
            t.set_enabled(shared.swipe_enabled.get() && !self.st.borrow().swipe_committing);
        }
        self.sync_palette_buttons();
        self.sync_swipe_strip();
    }

    /// The columns only a single line has (#334): each shown or not as the
    /// setting says, and filled from the message when it is.
    fn fill_columns(&self, data: &RowData, look: &RowLook, shared: &RowShared, unread: bool) {
        let w = &self.w;
        let msg = &data.msg;
        let on = |c: ListColumn| look.columns.contains(&c);
        let weight: &[&str] = if unread { &["message-sender", "unread"] } else { &["message-sender"] };

        w.recipients_col.set_visible(on(ListColumn::Recipients));
        if on(ListColumn::Recipients) {
            w.recipients.set_label(&recipient_names(&msg.to));
            w.recipients.set_css_classes(weight);
        }

        w.people_col.set_visible(on(ListColumn::Correspondents));
        if on(ListColumn::Correspondents) {
            let people = data.meta.people.clone().unwrap_or_else(|| msg.from_name.clone());
            w.people.set_tooltip_text(Some(&people));
            w.people.set_label(&people);
            w.people.set_css_classes(weight);
        }

        w.importance.set_visible(on(ListColumn::Importance));
        let (icon, class, tip) = match msg.importance {
            Importance::High => ("emblem-important-symbolic", "importance-high", Some(i18n("High importance"))),
            Importance::Low => ("hylki-importance-low-symbolic", "importance-low", Some(i18n("Low importance"))),
            Importance::Normal => ("emblem-important-symbolic", "importance-high", None),
        };
        w.importance.set_icon_name(Some(icon));
        w.importance.set_css_classes(&[class]);
        w.importance.set_opacity(if tip.is_some() { 1.0 } else { 0.0 });
        w.importance.set_tooltip_text(tip.as_deref());

        w.account_col.set_visible(on(ListColumn::Account));
        if on(ListColumn::Account) {
            let name = shared.account_names.borrow().get(&msg.account_id).cloned().unwrap_or_default();
            w.account.set_tooltip_text(Some(&name));
            w.account.set_label(&name);
        }

        w.due_col.set_visible(on(ListColumn::Due));
        if on(ListColumn::Due) {
            w.due.set_label(&due_label(msg.due));
            let overdue = msg.due > 0 && crate::datefmt::day_key(msg.due) < crate::datefmt::day_key(crate::datefmt::now());
            w.due.set_css_classes(if overdue { &["message-date", "overdue"] } else { &["message-date"] });
        }
    }

    /// The row's own classes, on the item widget the view wraps it in.
    fn sync_host_classes(&self) {
        let Some(data) = self.data() else { return };
        let st = self.st.borrow();
        let mut v = vec!["message-item"];
        if data.msg.unread {
            v.push("message-unread");
        }
        if data.meta.unread {
            v.push("thread-unread");
        }
        if data.meta.is_child {
            v.push("thread-child");
        }
        if data.meta.is_last {
            v.push("thread-last");
        }
        if st.swipe_active {
            v.push("swiping");
        }
        if st.dragging {
            v.push("dragging");
        }
        drop(st);
        self.w.host.set_css_classes(&v);
    }

    /// Rebuild the row's tag chips (#71) when the keywords or the tags
    /// changed: one pill per keyword that names a tag, in tag order.
    fn render_tags(&self, msg: &Message, shared: &RowShared, look: &RowLook) {
        let want = Some((msg.keywords.clone(), look.tags_gen));
        if self.st.borrow().tags_for == want {
            return;
        }
        self.st.borrow_mut().tags_for = want;
        let tags_box = &self.w.tags_box;
        while let Some(child) = tags_box.first_child() {
            tags_box.remove(&child);
        }
        for t in shared.tags.borrow().iter().filter(|t| msg.has_keyword(&t.keyword)) {
            let chip = gtk::Label::new(Some(&t.name));
            chip.add_css_class("tag-chip");
            chip.add_css_class(&t.css_class());
            chip.set_valign(gtk::Align::Center);
            chip.set_ellipsize(gtk::pango::EllipsizeMode::End);
            chip.set_max_width_chars(14);
            tags_box.append(&chip);
        }
    }

    // ── Names and faces ──

    /// The preview as shown. In one person's mail, where no names are
    /// shown, the user's own newest message starts with "You:".
    fn preview_text(&self, data: &RowData, look: &RowLook, preview: &str) -> String {
        let text = crate::models::preview_display(preview);
        let Some(own) = &look.one_person else { return text };
        let from = data.meta.from.as_ref().map_or(data.msg.from_addr.as_str(), |(_, addr)| addr.as_str());
        if own.contains(&from.trim().to_lowercase()) {
            i18n_f("You: {text}", &[("text", &text)])
        } else {
            text
        }
    }

    /// The row's name line: the sender — or, in a Sent folder, who the
    /// message went to, since every sender there is you (#27).
    fn name_line(&self, data: &RowData, look: &RowLook) -> String {
        if !look.show_recipient {
            // A thread head surfaces its NEWEST member's sender.
            if let Some((name, _)) = &data.meta.from {
                return name.clone();
            }
            return data.msg.from_name.clone();
        }
        let names = recipient_names(&data.msg.to);
        if names.is_empty() {
            data.msg.from_name.clone()
        } else {
            format!("To: {names}")
        }
    }

    /// What the avatar's initials (and face lookups) key on: the first
    /// recipient in a Sent folder, the sender everywhere else.
    fn face_name(&self, data: &RowData, look: &RowLook) -> String {
        if look.show_recipient {
            let names = recipient_names(&data.msg.to);
            if let Some(first) = names.split(',').next().map(str::trim) {
                if !first.is_empty() {
                    return first.to_string();
                }
            }
        }
        if let Some((name, _)) = &data.meta.from {
            return name.clone();
        }
        data.msg.from_name.clone()
    }

    /// The address face lookups run against — the first recipient's in a
    /// Sent folder, so the circle shows who the mail went to.
    fn face_email(&self, data: &RowData, look: &RowLook) -> String {
        if look.show_recipient {
            if let Some(addr) = first_recipient_addr(&data.msg.to) {
                return addr;
            }
        }
        if let Some((_, addr)) = &data.meta.from {
            return addr.clone();
        }
        data.msg.from_addr.clone()
    }

    /// What the avatar circle shows: the sender's picture when one is known,
    /// else their initials drawn ink-centred (see `ui::initials`). The same
    /// paintable is returned for the same name, so the avatar sees no change
    /// between refreshes.
    fn avatar_image(&self, data: &RowData, look: &RowLook) -> Option<gtk::gdk::Paintable> {
        let mut st = self.st.borrow_mut();
        if let Some(tex) = &st.avatar_texture {
            return Some(tex.clone().upcast());
        }
        // A message from one of your own mailboxes wears that mailbox's emoji
        // on its color (#189), the same face the sidebar circle shows. It
        // shares the slot with the initials, keyed by what it draws.
        if let Some((emoji, color)) = crate::avatar::own_face(&self.face_email(data, look))
            .and_then(|face| face.emoji.map(|emoji| (emoji, face.color)))
        {
            let key = format!("{emoji}\u{1}{color}");
            if st.initials.as_ref().is_none_or(|(n, _)| *n != key) {
                let bg = gtk::gdk::RGBA::parse(&color).unwrap_or(gtk::gdk::RGBA::BLACK);
                let fg = gtk::gdk::RGBA::parse(crate::color::readable_text(&color)).unwrap_or(gtk::gdk::RGBA::WHITE);
                let face = crate::ui::initials::InitialsPaintable::solid(&emoji, bg, fg, 0.55);
                st.initials = Some((key, face));
            }
            return st.initials.as_ref().map(|(_, p)| p.clone().upcast());
        }
        let name = self.face_name(data, look);
        if st.initials.as_ref().is_none_or(|(n, _)| *n != name) {
            st.initials = crate::ui::initials::InitialsPaintable::for_name(&name).map(|p| (name.clone(), p));
        }
        st.initials.as_ref().map(|(_, p)| p.clone().upcast())
    }

    /// Fill the circle: a cached face if one is known, otherwise go and
    /// look. The chain is your own mailbox's picture (#189) → contact photo
    /// → Gravatar → domain icon → initials, each tier consulted only while
    /// its switch is on.
    fn load_face(&self, email: &str, look: &RowLook) {
        if email.is_empty() {
            return;
        }
        if let Some(face) = crate::avatar::own_face(email) {
            let tex = face
                .gravatar
                .then(|| crate::avatar::own_gravatar(email))
                .flatten()
                .or_else(|| face.picture.as_deref().and_then(crate::ui::initials::avatar_texture));
            self.st.borrow_mut().avatar_texture = tex;
            return;
        }
        match crate::avatar::lookup(email, look.gravatar) {
            crate::avatar::CacheLookup::Texture(texture) => {
                self.st.borrow_mut().avatar_texture = Some(texture);
            }
            // Contact and Gravatar are definitively absent — the logo tier is
            // all that's left before initials.
            crate::avatar::CacheLookup::Missing => self.load_logo(email, look),
            crate::avatar::CacheLookup::Fetch { generation, mode } => {
                let want_logo = look.sender_logos && !crate::logo::known_missing(email);
                self.look_up(find_face(email.to_string(), generation, mode, want_logo));
            }
        }
    }

    /// The logo tier: only consulted when enabled, so switching "sender
    /// logos" off hides cached logos at once. A domain already asked about
    /// is not asked again — one request a session, not one a row.
    fn load_logo(&self, email: &str, look: &RowLook) {
        if !look.sender_logos {
            self.st.borrow_mut().avatar_texture = None;
            return;
        }
        if let Some(tex) = crate::logo::cached(email) {
            self.st.borrow_mut().avatar_texture = Some(tex);
            // A week-old stored icon still shows, but this new message from
            // the sender is the cue to look for a fresh one.
            if crate::logo::wants_refresh(email) {
                self.look_up(find_logo(email.to_string()));
            }
            return;
        }
        self.st.borrow_mut().avatar_texture = None;
        if !crate::logo::known_missing(email) {
            self.look_up(find_logo(email.to_string()));
        }
    }

    /// Run a face lookup off the main thread and bring its answer back to
    /// this row, which by then may show someone else.
    fn look_up(&self, fut: impl std::future::Future<Output = FaceCmd> + Send + 'static) {
        let weak = self.me.clone();
        let handle = relm4::spawn(fut);
        glib::MainContext::default().spawn_local(async move {
            if let Ok(cmd) = handle.await {
                if let Some(row) = weak.upgrade() {
                    row.face_answer(cmd);
                }
            }
        });
    }

    fn face_answer(&self, cmd: FaceCmd) {
        let Some(shared) = self.shared() else { return };
        let look = shared.look.borrow().clone();
        let current = self.data().map(|d| self.face_email(&d, &look));
        match cmd {
            FaceCmd::Avatar { email, generation, mode, outcome, logo } => {
                // Record what came back before deciding what to draw — the
                // caches are shared, so the sender's other rows benefit even
                // when this row now shows a different message.
                let retry_stale = crate::avatar::cache_result(&email, generation, mode, outcome);
                match logo {
                    Some(Some(bytes)) => {
                        crate::logo::decode_and_cache(&email, &bytes);
                    }
                    Some(None) => crate::logo::remember_missing(&email),
                    None => {}
                }
                if !current.is_some_and(|c| c.eq_ignore_ascii_case(&email)) {
                    return;
                }
                match crate::avatar::lookup(&email, look.gravatar) {
                    crate::avatar::CacheLookup::Texture(texture) => {
                        self.st.borrow_mut().avatar_texture = Some(texture);
                    }
                    crate::avatar::CacheLookup::Missing => self.load_logo(&email, &look),
                    crate::avatar::CacheLookup::Fetch { generation, mode } => {
                        self.st.borrow_mut().avatar_texture = None;
                        // Only chase a result the EDS generation invalidated;
                        // a transient Gravatar failure waits for a later bind.
                        if retry_stale {
                            let want_logo = look.sender_logos && !crate::logo::known_missing(&email);
                            self.look_up(find_face(email, generation, mode, want_logo));
                        }
                    }
                }
            }
            FaceCmd::Logo { email, bytes } => {
                let texture = match bytes {
                    Some(bytes) => crate::logo::decode_and_cache(&email, &bytes),
                    None => {
                        // Remember the miss, so the sender's other rows and
                        // the next sync don't ask the same domain again.
                        crate::logo::remember_missing(&email);
                        None
                    }
                };
                if !(current.is_some_and(|c| c.eq_ignore_ascii_case(&email)) && look.sender_logos) {
                    return;
                }
                self.st.borrow_mut().avatar_texture = texture;
            }
        }
        if let Some(data) = self.data() {
            self.w.avatar.set_custom_image(self.avatar_image(&data, &look).as_ref());
        }
    }

    // ── Focus Mode ──

    /// Slide the circle away (Focus Mode), before the list drops it.
    pub fn slide_avatar_away(&self) {
        self.st.borrow_mut().avatar_shown = false;
        self.w.avatar_revealer.set_reveal_child(false);
        self.w.avatar_revealer.set_css_classes(&["focus-fade", "away"]);
    }

    /// Fold the circle and slide it in a moment later (Focus Mode has given
    /// the avatars back), once the row is on screen and the revealer mapped.
    pub fn slide_avatar_in(self: &Rc<Self>) {
        self.st.borrow_mut().avatar_shown = false;
        self.w.avatar_revealer.set_transition_duration(0);
        self.w.avatar_revealer.set_reveal_child(false);
        self.w.avatar_revealer.set_transition_duration(crate::ui::FOCUS_ANIM_MS);
        let weak = Rc::downgrade(self);
        let gen = self.st.borrow().gen;
        glib::timeout_add_local_once(std::time::Duration::from_millis(60), move || {
            let Some(row) = weak.upgrade() else { return };
            if row.st.borrow().gen != gen {
                return;
            }
            row.st.borrow_mut().avatar_shown = true;
            row.w.avatar_revealer.set_reveal_child(true);
            row.w.avatar_revealer.set_css_classes(&["focus-fade"]);
        });
    }

    /// Show this many preview lines in place (Focus Mode).
    /// Lay the row's widgets out on one line or as the three-line card
    /// (#334). They are moved, not copied, and only when the layout
    /// changes: a row is recycled across messages but rarely across layouts.
    fn arrange(&self, columns: Option<&[ListColumn]>) {
        if self.st.borrow().columns.as_deref() == columns {
            return;
        }
        self.st.borrow_mut().columns = columns.map(<[ListColumn]>::to_vec);
        let w = &self.w;
        fn detach(widget: &gtk::Widget) {
            if let Some(from) = widget.parent().and_downcast::<gtk::Box>() {
                from.remove(widget);
            } else if let Some(bin) = widget.parent().and_downcast::<ColumnBin>() {
                bin.set_child(None::<&gtk::Widget>);
            }
        }
        fn into(to: &gtk::Box, widgets: &[&gtk::Widget]) {
            for widget in widgets {
                detach(widget);
                to.append(*widget);
            }
        }
        let name: &gtk::Widget = w.name.upcast_ref();
        let subject: &gtk::Widget = w.subject.upcast_ref();
        let star: &gtk::Widget = w.star.upcast_ref();
        let clip: &gtk::Widget = w.clip.upcast_ref();
        let date: &gtk::Widget = w.date.upcast_ref();
        let chip: &gtk::Widget = w.chip.upcast_ref();
        let tags: &gtk::Widget = w.tags_box.upcast_ref();
        let lock: &gtk::Widget = w.lock.upcast_ref();
        let preview: &gtk::Widget = w.preview.upcast_ref();
        if let Some(columns) = columns {
            // In the setting's order; a column that is off keeps its widget
            // wherever it was, hidden. The conversation chip rides at the
            // subject's end, where the subject gives way to it.
            for c in columns {
                match c {
                    ListColumn::Star => into(&w.line, &[star]),
                    ListColumn::Sender => {
                        detach(name);
                        w.name_col.set_child(Some(name));
                        into(&w.line, &[w.name_col.upcast_ref()]);
                    }
                    ListColumn::Recipients => into(&w.line, &[w.recipients_col.upcast_ref()]),
                    ListColumn::Correspondents => into(&w.line, &[w.people_col.upcast_ref()]),
                    ListColumn::Subject => {
                        detach(subject);
                        w.subject_col.set_child(Some(subject));
                        into(&w.line, &[lock, w.subject_col.upcast_ref(), chip]);
                    }
                    ListColumn::Tags => into(&w.line, &[tags]),
                    ListColumn::Attachment => into(&w.line, &[clip]),
                    ListColumn::Importance => into(&w.line, &[w.importance.upcast_ref()]),
                    ListColumn::Account => into(&w.line, &[w.account_col.upcast_ref()]),
                    ListColumn::Due => into(&w.line, &[w.due_col.upcast_ref()]),
                    ListColumn::Date => {
                        detach(date);
                        w.date_col.set_child(Some(date));
                        into(&w.line, &[w.date_col.upcast_ref()]);
                    }
                }
            }
            // Its bin sets the width (see `ColumnBin`); expanding, the
            // name would take the subject's room.
            w.name.set_hexpand(false);
            w.name.set_xalign(0.0);
            w.date.set_width_chars(9);
            w.date.set_xalign(1.0);
            w.avatar.set_size(16);
            // Smaller marks on one line, level with its smaller text.
            w.star.set_pixel_size(LINE_ICON_PX);
            w.clip.set_pixel_size(LINE_ICON_PX);
        } else {
            into(&w.top, &[name, clip, star, date, chip]);
            into(&w.subject_line, &[subject, tags]);
            into(&w.preview_line, &[lock, preview]);
            w.name.set_hexpand(true);
            w.date.set_width_chars(-1);
            w.date.set_xalign(0.5);
            w.avatar.set_size(38);
            w.star.set_pixel_size(-1);
            w.clip.set_pixel_size(-1);
        }
        w.text.set_visible(columns.is_none());
        w.line.set_visible(columns.is_some());
    }

    pub fn set_preview_lines(&self, lines: u32) {
        let lines = lines.clamp(1, 3);
        self.w.preview.set_wrap(lines > 1);
        self.w.preview.set_lines(lines as i32);
    }

    // ── The palette ──

    fn set_hover(&self, over: bool) {
        self.st.borrow_mut().hovered = over;
        self.sync_palette_classes();
        // Hover mode: the palette slides open by itself on the row, and arms
        // the usual collapse timeout on leave.
        let Some(shared) = self.shared() else { return };
        if !shared.palette_hover.get() || self.data().is_none() {
            return;
        }
        if over {
            if !self.st.borrow().palette_open {
                self.open_palette(true);
            }
            self.cancel_collapse();
        } else if self.st.borrow().palette_open {
            self.arm_collapse();
        }
    }

    pub fn toggle_palette(&self) {
        if self.data().is_none() {
            return;
        }
        if self.st.borrow().palette_open {
            self.open_palette(false);
            self.cancel_collapse();
        } else {
            self.open_palette(true);
            // Persist briefly; moving onto the palette cancels this.
            self.arm_collapse();
        }
    }

    fn open_palette(&self, open: bool) {
        if open {
            // One palette at a time.
            if let Some(shared) = self.shared() {
                let previous = shared.open_palette.borrow().upgrade();
                if let Some(prev) = previous {
                    if !std::ptr::eq(Rc::as_ptr(&prev), self) {
                        prev.open_palette(false);
                        prev.cancel_collapse();
                    }
                }
                shared.open_palette.replace(self.me.clone());
            }
            self.ensure_palette();
        }
        self.st.borrow_mut().palette_open = open;
        self.sync_palette_classes();
        self.slide_palette();
    }

    fn cancel_collapse(&self) {
        if let Some(id) = self.st.borrow_mut().collapse_timer.take() {
            id.remove();
        }
    }

    /// (Re)start the auto-collapse countdown from the preference (min 1s).
    fn arm_collapse(&self) {
        self.cancel_collapse();
        let Some(shared) = self.shared() else { return };
        let secs = shared.palette_collapse_secs.get().max(1);
        let weak = self.me.clone();
        let id = glib::timeout_add_seconds_local_once(secs as u32, move || {
            if let Some(row) = weak.upgrade() {
                row.st.borrow_mut().collapse_timer = None;
                row.open_palette(false);
            }
        });
        self.st.borrow_mut().collapse_timer = Some(id);
    }

    /// The ⋯ shows while the row is hovered or its palette open; open, the
    /// whole line sits on one card surface.
    fn sync_palette_classes(&self) {
        let st = self.st.borrow();
        let revealed = st.hovered || st.palette_open;
        let open = st.palette_open;
        drop(st);
        self.w
            .chevron
            .set_css_classes(if revealed { &["flat", "palette-toggle", "revealed"] } else { &["flat", "palette-toggle"] });
        self.w.actions_line.set_css_classes(if open { &["actions-line", "open"] } else { &["actions-line"] });
    }

    /// Build the palette's buttons on its first open: most rows never open
    /// theirs, and building them for every row was most of a row's cost.
    /// Mirrors the reader toolbar's order.
    fn ensure_palette(&self) {
        let Some(shared) = self.shared() else { return };
        let built_for = {
            let look = shared.look.borrow();
            (look.in_junk, look.in_drafts)
        };
        if self.st.borrow().palette.as_ref().is_some_and(|p| p.built_for == built_for) {
            return;
        }
        let inner = &self.w.palette_inner;
        while let Some(child) = inner.first_child() {
            inner.remove(&child);
        }
        let weak = self.me.clone();
        let button = |icon: &str, tip: String| {
            let b = gtk::Button::from_icon_name(icon);
            b.set_tooltip_text(Some(tip.as_str()));
            b.add_css_class("flat");
            b
        };
        let action = |b: &gtk::Button, a: RowAction| {
            let weak = weak.clone();
            b.connect_clicked(move |_| {
                if let Some(row) = weak.upgrade() {
                    row.act(a);
                }
            });
        };
        let (in_junk, in_drafts) = built_for;
        let reply = button("mail-reply-sender-symbolic", i18n("Reply"));
        action(&reply, RowAction::Reply);
        let reply_all = button("mail-reply-all-symbolic", i18n("Reply All"));
        action(&reply_all, RowAction::ReplyAll);
        let forward = button("mail-forward-symbolic", i18n("Forward"));
        action(&forward, RowAction::Forward);
        // A draft is neither read nor unread, so it gets no toggle.
        let read = (!in_drafts).then(|| {
            let b = button("hylki-mail-read-symbolic", i18n("Mark as read"));
            action(&b, RowAction::ToggleRead);
            b
        });
        let star = button("hylki-non-starred-symbolic", i18n("Star"));
        action(&star, RowAction::ToggleStar);
        let tag = button("tag-outline-symbolic", i18n("Tags"));
        {
            let weak = weak.clone();
            tag.connect_clicked(move |b| {
                if let Some(row) = weak.upgrade() {
                    row.tag_menu(b);
                }
            });
        }
        let moveto = button("folder-symbolic", i18n("Move to…"));
        {
            let weak = weak.clone();
            moveto.connect_clicked(move |b| {
                if let Some(row) = weak.upgrade() {
                    row.move_menu(b);
                }
            });
        }
        let archive = button("mail-archive-symbolic", i18n("Archive"));
        action(&archive, RowAction::Archive);
        let delete = button("user-trash-symbolic", i18n("Delete"));
        action(&delete, RowAction::Delete);
        let spam = if in_junk {
            let b = button("mail-mark-notjunk-symbolic", i18n("Not spam"));
            action(&b, RowAction::NotSpam);
            b
        } else {
            let b = button("mail-mark-junk-symbolic", i18n("Mark as spam"));
            action(&b, RowAction::Spam);
            b
        };
        let contact = button("contact-new-symbolic", i18n("Add sender to Contacts"));
        action(&contact, RowAction::AddContact);
        let source = button("code-symbolic", i18n("View Source"));
        action(&source, RowAction::ViewSource);
        for b in [
            Some(&reply),
            Some(&reply_all),
            Some(&forward),
            read.as_ref(),
            Some(&star),
            Some(&tag),
            Some(&moveto),
            Some(&archive),
            Some(&delete),
            Some(&spam),
            Some(&contact),
            Some(&source),
        ]
        .into_iter()
        .flatten()
        {
            inner.append(b);
        }
        self.st.borrow_mut().palette = Some(PaletteButtons { read, star, tag, built_for });
        self.sync_palette_buttons();
    }

    /// Keep the built palette's state-carrying buttons in step with the
    /// message.
    fn sync_palette_buttons(&self) {
        let Some(data) = self.data() else { return };
        let Some(shared) = self.shared() else { return };
        let st = self.st.borrow();
        let Some(b) = st.palette.as_ref() else { return };
        // Action-showing icon (read envelope = "mark as read"), like the
        // menus and toolbar.
        if let Some(read) = &b.read {
            if data.msg.unread {
                read.set_icon_name("hylki-mail-read-symbolic");
                read.set_tooltip_text(Some(i18n("Mark as read").as_str()));
            } else {
                read.set_icon_name("mail-unread-symbolic");
                read.set_tooltip_text(Some(i18n("Mark as unread").as_str()));
            }
        }
        let starred = data.msg.starred || data.meta.starred;
        b.star.set_css_classes(if starred { &["flat", "star-active"] } else { &["flat"] });
        b.star.set_tooltip_text(Some(if starred { i18n("Remove star") } else { i18n("Star") }.as_str()));
        // Only once there is a tag to give.
        b.tag.set_visible(!shared.tags.borrow().is_empty());
    }

    /// Slide the palette open or shut by animating the spacer that gives
    /// the clip its width — not a GtkRevealer, whose transitions don't
    /// repaint inside an Overlay's overlay child.
    fn slide_palette(&self) {
        let open = self.st.borrow().palette_open;
        let w = &self.w;
        if open {
            w.palette_inner.set_visible(true);
        }
        let (inner_w, inner_h) = (
            w.palette_inner.measure(gtk::Orientation::Horizontal, -1).1,
            w.palette_inner.measure(gtk::Orientation::Vertical, -1).1,
        );
        w.palette_spacer.set_height_request(inner_h);
        let target = if open { inner_w } else { 0 };
        if self.st.borrow().palette_target == target {
            return;
        }
        self.st.borrow_mut().palette_target = target;
        let spacer = w.palette_spacer.clone();
        let from = spacer.width() as f64;
        let setter = {
            let spacer = spacer.clone();
            adw::CallbackAnimationTarget::new(move |v| spacer.set_width_request(v as i32))
        };
        // Bound to the line (mapped: it holds the visible toggle), NOT the
        // spacer — adw skips animations on unmapped widgets, and the
        // zero-width spacer counts as one.
        let anim = adw::TimedAnimation::new(&w.actions_line, from, target as f64, 180, setter);
        anim.set_easing(adw::Easing::EaseOutCubic);
        if target == 0 {
            // Sliding shut: hide only once fully back in the button.
            let inner = w.palette_inner.downgrade();
            anim.connect_done(move |_| {
                if let Some(inner) = inner.upgrade() {
                    inner.set_visible(false);
                }
            });
        }
        let old = self.st.borrow_mut().palette_anim.replace(anim.clone());
        if let Some(old) = old {
            old.pause();
        }
        anim.play();
    }

    fn act(&self, action: RowAction) {
        if let Some(data) = self.data() {
            self.send(MessageListInput::RowAction { action, message: Box::new(Message::clone(&data.msg)) });
        }
    }

    fn tag_menu(&self, btn: &gtk::Button) {
        let (Some(data), Some(shared)) = (self.data(), self.shared()) else { return };
        let tags = shared.tags.borrow().clone();
        let target = Message::clone(&data.msg);
        let input = shared.input.clone();
        let entries = tag_menu_entries(&tags, &data.msg, move |keyword, add| {
            let _ = input.send(MessageListInput::SetTagFor { message: Box::new(target.clone()), keyword, add });
        });
        show_context_menu(btn, (btn.width() / 2) as f64, btn.height() as f64, vec![entries]);
    }

    fn move_menu(&self, btn: &gtk::Button) {
        let Some(data) = self.data() else { return };
        // Under the button's middle, in window coordinates: the picker is
        // anchored on the window by the app.
        let point = btn.root().and_then(|root| {
            let root: gtk::Widget = root.upcast();
            btn.compute_point(&root, &gtk::graphene::Point::new(btn.width() as f32 / 2.0, btn.height() as f32))
        });
        let (x, y) = point.map_or((0.0, 0.0), |p| (p.x() as f64, p.y() as f64));
        self.send(MessageListInput::RowMoveTo { message: Box::new(Message::clone(&data.msg)), x, y });
    }

    fn context_menu(&self, x: f64, y: f64) {
        let (Some(data), Some(shared)) = (self.data(), self.shared()) else { return };
        let Some(view) = shared.view.upgrade() else { return };
        let Some(point) = self.w.host.compute_point(&view, &gtk::graphene::Point::new(x as f32, y as f32)) else {
            return;
        };
        self.send(MessageListInput::ContextMenu {
            x: point.x() as f64,
            y: point.y() as f64,
            key: (data.msg.account_id, data.msg.id),
        });
    }

    fn drag_payload(&self) -> Option<gtk::gdk::ContentProvider> {
        let (data, shared) = (self.data()?, self.shared()?);
        let me = (data.msg.account_id, data.msg.folder_id, data.msg.uid, data.msg.id);
        let mut items = shared.selected_drag_keys();
        // Dragging a row outside the selection moves just that row.
        if !items.iter().any(|k| k.0 == me.0 && k.3 == me.3) {
            items = vec![me];
        }
        // A conversation row stands for its whole thread (#171).
        let threads = shared.thread_drag.borrow();
        let items: Vec<_> =
            items.into_iter().flat_map(|k| threads.get(&(k.0, k.3)).cloned().unwrap_or_else(|| vec![k])).collect();
        let mut payload = String::from("vireo-move");
        for (a, f, u, i) in items {
            payload.push_str(&format!("\t{a}\t{f}\t{u}\t{i}"));
        }
        Some(gtk::gdk::ContentProvider::for_value(&payload.to_value()))
    }

    // ── The swipe ──

    fn swipe_left_action(&self) -> RowAction {
        let reversed = self.shared().is_some_and(|s| s.swipe_reversed.get());
        if reversed {
            RowAction::Archive
        } else {
            RowAction::Delete
        }
    }

    fn swipe_right_action(&self) -> RowAction {
        let reversed = self.shared().is_some_and(|s| s.swipe_reversed.get());
        if reversed {
            RowAction::Delete
        } else {
            RowAction::Archive
        }
    }

    /// The action the strip is showing — the last side it grew from, so it
    /// stays put while a release's snap-back shrinks it.
    fn swipe_action(&self) -> RowAction {
        if self.st.borrow().swipe_side < 0 {
            self.swipe_left_action()
        } else {
            self.swipe_right_action()
        }
    }

    /// The strip's look: colored for whichever action is active and "armed"
    /// once the drag has cleared the commit distance; its icon and label hug
    /// the edge the swipe drags toward.
    fn sync_swipe_strip(&self) {
        let action = self.swipe_action();
        let (side, progress) = {
            let st = self.st.borrow();
            (st.swipe_side, st.swipe_progress)
        };
        let mut classes = vec!["swipe-indicator"];
        classes.push(match action {
            RowAction::Delete => "swipe-delete",
            _ => "swipe-archive",
        });
        if progress.abs() >= SWIPE_ARM {
            classes.push("armed");
        }
        self.w.swipe_bg.set_css_classes(&classes);
        self.w.swipe_inner.set_halign(if side < 0 { gtk::Align::End } else { gtk::Align::Start });
        match action {
            RowAction::Delete => {
                self.w.swipe_icon.set_icon_name(Some("user-trash-symbolic"));
                self.w.swipe_label.set_label(&i18n("Delete"));
            }
            _ => {
                self.w.swipe_icon.set_icon_name(Some("mail-archive-symbolic"));
                self.w.swipe_label.set_label(&i18n("Archive"));
            }
        }
    }

    fn swipe_update(&self, offset: f64) {
        {
            let mut st = self.st.borrow_mut();
            if st.swipe_committing {
                return;
            }
            st.swipe_dragging = true;
            st.swipe_active = true;
            st.swipe_progress = offset.clamp(-SWIPE_MAX, SWIPE_MAX);
            if st.swipe_progress != 0.0 {
                st.swipe_side = if st.swipe_progress < 0.0 { -1 } else { 1 };
            }
        }
        self.sync_host_classes();
        self.sync_swipe_strip();
        self.animate_swipe();
    }

    /// Whether it fires is decided on the distance alone rather than on
    /// `AdwSwipeTracker`'s velocity-aware snap point: a quick flick short of
    /// the commit distance reads as a false positive when the intent was
    /// clearly to back out.
    fn swipe_end(&self) {
        let commit = {
            let mut st = self.st.borrow_mut();
            st.swipe_dragging = false;
            st.swipe_progress.abs() >= SWIPE_ARM
        };
        if commit {
            // The row flies out the side it was dragged to while its
            // Revealer closes, and the action fires as the two land.
            let mut st = self.st.borrow_mut();
            st.swipe_committing = true;
            st.swipe_active = true;
            st.swipe_progress = if st.swipe_side < 0 { -SWIPE_MAX } else { SWIPE_MAX };
            drop(st);
            self.w.revealer.set_reveal_child(false);
            if let Some(t) = self.tracker.borrow().as_ref() {
                t.set_enabled(false);
            }
        } else {
            self.st.borrow_mut().swipe_progress = 0.0;
        }
        self.sync_swipe_strip();
        self.animate_swipe();
    }

    /// Move the surface to where the swipe state says it belongs: nothing
    /// while a drag tracks the pointer, the exit while committing, and back
    /// to rest otherwise.
    fn animate_swipe(&self) {
        let weak = self.me.clone();
        let (dragging, committing, exit_started, side, progress, active, gen) = {
            let st = self.st.borrow();
            (
                st.swipe_dragging,
                st.swipe_committing,
                st.swipe_exit_started,
                st.swipe_side,
                st.swipe_progress,
                st.swipe_active,
                st.gen,
            )
        };
        let surface = self.w.surface.clone();
        if dragging {
            if let Some(a) = self.st.borrow_mut().swipe_anim.take() {
                a.pause();
            }
            return;
        }
        let setter = {
            let surface = surface.clone();
            adw::CallbackAnimationTarget::new(move |v| surface.set_progress_px(v))
        };
        let anim = if committing {
            if exit_started {
                return;
            }
            self.st.borrow_mut().swipe_exit_started = true;
            // Carry the content clear off the row's own side while the
            // Revealer shuts the height. Negated into the tracker's
            // convention, like the snap-back.
            let span = (surface.width() as f64).max(SWIPE_MAX * 2.0);
            let target = if side < 0 { span } else { -span };
            let anim = adw::TimedAnimation::new(&self.w.overlay, surface.progress_px(), target, SWIPE_EXIT_MS, setter);
            let action = self.swipe_action();
            // The action fires as the exit lands, so the removal that
            // follows has nothing left to hide.
            anim.connect_done(move |_| {
                let Some(row) = weak.upgrade() else { return };
                if row.st.borrow().gen != gen || !row.st.borrow().swipe_committing {
                    return;
                }
                row.act(action);
                // The action left the row in place: put the exit back.
                let weak = Rc::downgrade(&row);
                glib::timeout_add_local_once(std::time::Duration::from_millis(SWIPE_RESTORE_MS), move || {
                    let Some(row) = weak.upgrade() else { return };
                    if row.st.borrow().gen != gen || !row.st.borrow().swipe_committing {
                        return;
                    }
                    {
                        let mut st = row.st.borrow_mut();
                        st.swipe_committing = false;
                        st.swipe_exit_started = false;
                        st.swipe_progress = 0.0;
                    }
                    row.refresh();
                    row.animate_swipe();
                });
            });
            anim
        } else {
            let target = -progress;
            let current = surface.progress_px();
            if (current - target).abs() <= 0.5 {
                // Already at rest: settle the `.swiping` geometry at once.
                if active && progress == 0.0 {
                    self.swipe_settled();
                }
                return;
            }
            // Bound to the row overlay (always mapped), not the surface —
            // adw skips animations on unmapped widgets.
            let anim = adw::TimedAnimation::new(&self.w.overlay, current, target, 180, setter);
            if progress == 0.0 {
                anim.connect_done(move |_| {
                    if let Some(row) = weak.upgrade() {
                        if row.st.borrow().gen == gen {
                            row.swipe_settled();
                        }
                    }
                });
            }
            anim
        };
        anim.set_easing(adw::Easing::EaseOutCubic);
        let old = self.st.borrow_mut().swipe_anim.replace(anim.clone());
        if let Some(old) = old {
            old.pause();
        }
        anim.play();
    }

    /// The snap-back landed: the row drops its `.swiping` geometry, unless
    /// a new drag started in the meantime.
    fn swipe_settled(&self) {
        let settle = {
            let st = self.st.borrow();
            !st.swipe_dragging && st.swipe_progress == 0.0
        };
        if settle {
            self.st.borrow_mut().swipe_active = false;
            self.sync_host_classes();
        }
    }

    /// Drive a full swipe and release (a screenshot hook).
    pub fn debug_swipe(&self, left: bool) {
        let px = if left { -SWIPE_MAX } else { SWIPE_MAX };
        self.swipe_update(px);
        self.swipe_end();
    }
}

/// The row's widget tree. The host is what the view wraps in its item
/// widget; everything else hangs off it.
fn build_widgets() -> RowWidgets {
    let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    host.add_css_class("message-item");

    // Replies slide open and shut instead of the list jumping to a new row
    // count the instant a conversation toggles. (PR #79)
    let revealer = gtk::Revealer::new();
    revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    revealer.set_transition_duration(200);
    revealer.set_reveal_child(true);
    host.append(&revealer);

    // The swipe surface: the fixed action strip underneath, the row's real
    // content on top, slid away from it under a drag (#swipe).
    let surface = SwipeSurface::default();
    revealer.set_child(Some(&surface));

    let swipe_bg = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    swipe_bg.set_overflow(gtk::Overflow::Hidden);
    let swipe_inner = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    swipe_inner.set_valign(gtk::Align::Center);
    swipe_inner.set_hexpand(true);
    swipe_inner.set_margin_start(16);
    swipe_inner.set_margin_end(16);
    let swipe_icon = gtk::Image::new();
    let swipe_label = gtk::Label::new(None);
    swipe_inner.append(&swipe_icon);
    swipe_inner.append(&swipe_label);
    swipe_bg.append(&swipe_inner);
    surface.set_background(&swipe_bg);

    let overlay = gtk::Overlay::new();
    surface.set_foreground(&overlay);

    // The content: avatar, unread dot and the three lines of text.
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.add_css_class("message-row");
    // A palette wider than the row is clipped here rather than painted
    // across the divider into the reader.
    content.set_overflow(gtk::Overflow::Hidden);
    overlay.set_child(Some(&content));

    // The circle sits in a revealer so Focus Mode can slide it away (and
    // back). SlideRight: folding, the circle moves off past the left edge.
    let avatar_revealer = gtk::Revealer::new();
    avatar_revealer.set_transition_type(gtk::RevealerTransitionType::SlideRight);
    avatar_revealer.set_transition_duration(crate::ui::FOCUS_ANIM_MS);
    avatar_revealer.add_css_class("focus-fade");
    avatar_revealer.set_reveal_child(true);
    let avatar = adw::Avatar::new(38, None, true);
    avatar.set_valign(gtk::Align::Center);
    avatar_revealer.set_child(Some(&avatar));
    content.append(&avatar_revealer);

    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("unread-dot");
    content.append(&dot);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    content.append(&text);

    let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let name = gtk::Label::new(None);
    name.set_halign(gtk::Align::Start);
    name.set_hexpand(true);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.add_css_class("message-sender");
    top.append(&name);
    let clip = gtk::Image::from_icon_name("mail-attachment-symbolic");
    clip.add_css_class("dim-icon");
    top.append(&clip);
    let star = gtk::Image::from_icon_name("starred-symbolic");
    star.add_css_class("star-icon");
    top.append(&star);
    let date = gtk::Label::new(None);
    date.set_halign(gtk::Align::End);
    // Ellipsized so it stops being the row's floor: it is the one item on
    // this line with no give (#29).
    date.set_ellipsize(gtk::pango::EllipsizeMode::End);
    date.add_css_class("message-date");
    top.append(&date);
    // The conversation chip (thread heads only): the count and the
    // expand/collapse caret merged into one grey pill.
    let chip = gtk::Button::new();
    chip.set_tooltip_text(Some(i18n("Show conversation").as_str()));
    chip.add_css_class("flat");
    chip.add_css_class("thread-chip");
    chip.set_valign(gtk::Align::Center);
    let chip_box = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    // Centred: with the caret hidden the bare count must not sit against
    // the chip's left edge.
    chip_box.set_halign(gtk::Align::Center);
    let chip_count = gtk::Label::new(None);
    chip_box.append(&chip_count);
    // One caret; the "open" class rotates it via a CSS transition. (PR #79)
    let chip_caret = gtk::Image::from_icon_name("pan-end-symbolic");
    chip_caret.add_css_class("thread-toggle-icon");
    chip_box.append(&chip_caret);
    chip.set_child(Some(&chip_box));
    top.append(&chip);
    text.append(&top);

    // The subject and the tag chips at its end, where a long subject gives
    // way before the sender's name would. Hidden whole by Focus Mode.
    let subject_line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let subject = gtk::Label::new(None);
    subject.set_halign(gtk::Align::Start);
    subject.set_hexpand(true);
    subject.set_ellipsize(gtk::pango::EllipsizeMode::End);
    subject.add_css_class("message-subject");
    subject_line.append(&subject);
    let tags_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tags_box.set_valign(gtk::Align::Center);
    subject_line.append(&tags_box);
    text.append(&subject_line);

    // The message's own text, at full width.
    let preview_line = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    // An encrypted message (#133) shows a lock where its text would be, in
    // the preview's own dimmed color.
    let lock = gtk::Image::from_icon_name("channel-secure-symbolic");
    lock.set_pixel_size(12);
    lock.set_valign(gtk::Align::Center);
    lock.add_css_class("message-preview");
    preview_line.append(&lock);
    let preview = gtk::Label::new(None);
    // Fill (not Start): the layout width then matches the allocation, so
    // the ellipsis lands right where the text is cut.
    preview.set_halign(gtk::Align::Fill);
    preview.set_hexpand(true);
    preview.set_xalign(0.0);
    preview.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
    preview.add_css_class("message-preview");
    preview_line.append(&preview);
    text.append(&preview_line);

    // The single-line layout's row (#334): empty and hidden until a row is
    // laid out that way, when the widgets above move into it.
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.set_hexpand(true);
    line.set_visible(false);
    content.append(&line);
    // The columns a card has no place for, which live on that line, each in
    // a bin that keeps it the same width on every row.
    let column_label = |px: i32| {
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let bin = ColumnBin::new(px);
        bin.set_child(Some(&label));
        bin.set_visible(false);
        line.append(&bin);
        (label, bin)
    };
    let (recipients, recipients_col) = column_label(SENDER_COLUMN_PX);
    let (people, people_col) = column_label(PEOPLE_COLUMN_PX);
    let (account, account_col) = column_label(ACCOUNT_COLUMN_PX);
    let name_col = ColumnBin::new(SENDER_COLUMN_PX);
    let subject_col = ColumnBin::new(0);
    let date_col = ColumnBin::new(-1);
    // The subject takes whatever room the other columns leave.
    subject_col.set_hexpand(true);
    account.add_css_class("message-account");
    let due = gtk::Label::new(None);
    due.set_width_chars(9);
    due.set_xalign(1.0);
    due.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let due_col = ColumnBin::new(-1);
    due_col.set_child(Some(&due));
    due_col.set_visible(false);
    line.append(&due_col);
    let importance = gtk::Image::from_icon_name("emblem-important-symbolic");
    importance.set_visible(false);
    line.append(&importance);

    // The last reply's rail: a real dotted border on a widget spanning
    // exactly the row's top half, so it ends at the node dot. Added before
    // the node so the dot draws over where they meet.
    let rail_stub = gtk::Box::new(gtk::Orientation::Vertical, 0);
    rail_stub.set_halign(gtk::Align::Start);
    rail_stub.set_homogeneous(true);
    let stub = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    stub.add_css_class("thread-rail-stub");
    rail_stub.append(&stub);
    rail_stub.append(&gtk::Box::new(gtk::Orientation::Horizontal, 0));
    overlay.add_overlay(&rail_stub);

    // The node dot where a reply meets the conversation's rail.
    let node = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    node.add_css_class("thread-node");
    node.set_halign(gtk::Align::Start);
    node.set_valign(gtk::Align::Center);
    overlay.add_overlay(&node);

    // The actions palette floats over the pill's bottom-left corner, opening
    // rightward from the ⋯ (#81). The holder has a FIXED width: overlay
    // children are re-allocated lazily, so one that grew with the slide
    // would snap instead of animating.
    let holder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    holder.set_width_request(320);
    holder.set_halign(gtk::Align::Start);
    holder.set_valign(gtk::Align::End);
    let actions_line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    actions_line.set_halign(gtk::Align::Start);
    actions_line.set_valign(gtk::Align::End);
    actions_line.add_css_class("actions-line");
    // The ⋯ opens and closes the palette without selecting the message: a
    // button's click is consumed before the row's selection gesture.
    let chevron = gtk::Button::from_icon_name("view-more-horizontal-symbolic");
    chevron.add_css_class("flat");
    chevron.add_css_class("palette-toggle");
    chevron.set_tooltip_text(Some(i18n("Actions").as_str()));
    chevron.set_valign(gtk::Align::Center);
    actions_line.append(&chevron);
    // The palette hangs clipped over a spacer whose width is the one thing
    // animated: a Box's width request is only a floor, so the spacer is the
    // hard cap.
    let palette_clip = gtk::Overlay::new();
    let spacer_holder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let palette_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    palette_spacer.set_width_request(0);
    spacer_holder.append(&palette_spacer);
    palette_clip.set_child(Some(&spacer_holder));
    let palette_inner = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    palette_inner.add_css_class("actions-palette");
    palette_inner.set_halign(gtk::Align::Start);
    palette_inner.set_valign(gtk::Align::Center);
    palette_inner.set_visible(false);
    palette_clip.add_overlay(&palette_inner);
    actions_line.append(&palette_clip);
    holder.append(&actions_line);
    overlay.add_overlay(&holder);

    RowWidgets {
        host,
        revealer,
        surface,
        swipe_bg,
        swipe_inner,
        swipe_icon,
        swipe_label,
        overlay,
        rail_stub,
        node,
        actions_line,
        chevron,
        palette_clip,
        palette_spacer,
        palette_inner,
        content,
        recipients,
        recipients_col,
        people,
        people_col,
        importance,
        account,
        account_col,
        due,
        due_col,
        name_col,
        subject_col,
        date_col,
        avatar_revealer,
        avatar,
        dot,
        text,
        top,
        line,
        name,
        clip,
        star,
        date,
        chip,
        chip_count,
        chip_caret,
        subject_line,
        subject,
        tags_box,
        preview_line,
        lock,
        preview,
    }
}
