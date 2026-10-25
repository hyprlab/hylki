//! What every row shares: the look, the bound rows, the cursor, and the
//! factories that build and bind them (with spare rows made ahead).

use super::row::Row;
use super::{MessageModel, RowData, RowItem, ThreadDragKeys};
use crate::config::ListColumn;
use crate::ui::message_list::MessageListInput;
use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use relm4::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// How every row looks: the list's settings, read on each bind.
#[derive(Clone, Debug)]
pub(crate) struct RowLook {
    pub(crate) gravatar: bool,
    /// Whether the avatar is drawn at all (#29).
    pub(crate) avatars: bool,
    /// Whether a sender's site icon may fill it (#30).
    pub(crate) sender_logos: bool,
    /// One line per message, in columns, instead of the three-line card
    /// (#334).
    pub(crate) single_line: bool,
    /// Those columns, in order, the Due column left out of a list without
    /// Microsoft 365 mail in it.
    pub(crate) columns: Vec<ListColumn>,
    /// The widths columns were dragged to; the rest keep their own.
    pub(crate) widths: HashMap<ListColumn, i32>,
    /// The narrowest a single line goes before the list scrolls sideways
    /// (#443); -1 for none.
    pub(crate) min_width: i32,
    /// How many lines of the message's text a row shows (0–3).
    pub(crate) preview_lines: u32,
    /// Whether the subject line is drawn (Focus Mode can take it away).
    pub(crate) show_subject: bool,
    /// A card's pills go on a line of their own under its text (#374).
    pub(crate) pills_below: bool,
    /// The folder each result of a search across folders is in, by
    /// `(account_id, folder_id)`, worn as its first pill (#374). Empty
    /// otherwise.
    pub(crate) folder_pills: Rc<HashMap<(u32, u32), String>>,
    /// Whether rows carry the actions palette at all.
    pub(crate) show_palette: bool,
    /// The list shows Junk: the palette's spam button reads "Not Spam".
    pub(crate) in_junk: bool,
    /// The list shows Drafts: no read/unread toggle, a draft is neither.
    pub(crate) in_drafts: bool,
    /// Sent-folder rows name the recipient, not the sender (#27).
    pub(crate) show_recipient: bool,
    /// The accounts whose avatars wear their color as a ring (unified view).
    pub(crate) ringed: std::collections::HashSet<u32>,
    /// Whether conversations open out in the list at all.
    pub(crate) thread_expansion: bool,
    /// Bumped whenever the circles must be looked up again (contact photos
    /// changed, Gravatar or logos switched).
    pub(crate) face_gen: u64,
    /// Bumped whenever the tag definitions change.
    pub(crate) tags_gen: u64,
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
            min_width: -1,
            preview_lines: 1,
            show_subject: true,
            pills_below: false,
            folder_pills: Default::default(),
            show_palette: true,
            in_junk: false,
            in_drafts: false,
            show_recipient: false,
            ringed: Default::default(),
            thread_expansion: true,
            face_gen: 0,
            tags_gen: 0,
        }
    }
}

/// When snoozed mail comes back (#527), by (account, Message-ID), and the
/// Snoozed folders, by (account, folder id): a row shows the time only
/// there, not on a copy an undo has put back.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct SnoozedRows {
    pub(crate) times: HashMap<(u32, String), i64>,
    pub(crate) folders: std::collections::HashSet<(u32, u32)>,
}

impl SnoozedRows {
    /// When `msg` comes back, if it waits in a Snoozed folder.
    pub(crate) fn until(&self, msg: &crate::models::Message) -> Option<i64> {
        if self.times.is_empty() || !self.folders.contains(&(msg.account_id, msg.folder_id)) {
            return None;
        }
        self.times
            .get(&(msg.account_id, msg.message_id.clone()))
            .copied()
    }
}

/// What every row shares with the list.
pub(crate) struct RowShared {
    pub(crate) look: RefCell<RowLook>,
    /// The tags (#71): the chips a row shows are the message's keywords that
    /// name one of these.
    pub(crate) tags: RefCell<Vec<crate::config::Tag>>,
    /// Each account's name, for the Account column (#334).
    pub(crate) account_names: RefCell<HashMap<u32, String>>,
    /// When snoozed mail comes back (#527).
    pub(crate) snoozed: RefCell<SnoozedRows>,
    /// How long an actions palette stays open after the pointer leaves it.
    pub(crate) palette_collapse_secs: Cell<u64>,
    /// Open the palette on row hover.
    pub(crate) palette_hover: Cell<bool>,
    /// Swap which side a swipe archives or deletes on.
    pub(crate) swipe_reversed: Cell<bool>,
    /// Whether the swipe gesture is on at all (#92).
    pub(crate) swipe_enabled: Cell<bool>,
    /// How sensitive a trackpad's two-finger swipe is.
    pub(crate) swipe_sensitivity: Cell<f64>,
    pub(crate) input: relm4::Sender<MessageListInput>,
    pub(crate) model: MessageModel,
    pub(crate) selection: gtk::MultiSelection,
    pub(crate) view: glib::WeakRef<gtk::ListView>,
    pub(crate) thread_drag: RefCell<ThreadDragKeys>,
    /// Every row widget the view has made, by its list item.
    rows: RefCell<HashMap<usize, Rc<Row>>>,
    /// Rows the view let go of, kept for the next it asks for. A short list
    /// (an Inbox of ten) gives most of its rows back, and the next long one
    /// asked for them all again, at a few milliseconds each, inside the
    /// switch.
    spare: RefCell<Vec<Rc<Row>>>,
    /// The row Gmail's j/k cursor is on (#392), drawn with its own outline.
    pub(super) cursor: Cell<Option<(u32, u32)>>,
    /// The row whose palette is open: one at a time.
    pub(super) open_palette: RefCell<Weak<Row>>,
    /// Conversations shown on rows whose size nobody has asked about yet.
    wanted: RefCell<Vec<(u32, String)>>,
    wanted_queued: Cell<bool>,
    /// The date headings the view has made (#435).
    headers: RefCell<Vec<glib::WeakRef<gtk::ListHeader>>>,
}

impl RowShared {
    pub(crate) fn new(input: relm4::Sender<MessageListInput>) -> Rc<Self> {
        let model = MessageModel::default();
        let selection = gtk::MultiSelection::new(Some(model.clone()));
        let privacy = crate::config::load_privacy();
        Rc::new(RowShared {
            look: RefCell::new(RowLook::default()),
            tags: RefCell::new(Vec::new()),
            account_names: RefCell::new(HashMap::new()),
            snoozed: RefCell::new(SnoozedRows::default()),
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
            spare: RefCell::new(Vec::new()),
            cursor: Cell::new(None),
            open_palette: RefCell::new(Weak::new()),
            wanted: RefCell::new(Vec::new()),
            wanted_queued: Cell::new(false),
            headers: RefCell::new(Vec::new()),
        })
    }

    /// How tall the heading over the first row is drawn, once it is.
    pub(crate) fn first_heading_height(&self) -> Option<i32> {
        self.headers
            .borrow()
            .iter()
            .filter_map(|h| h.upgrade())
            .find(|h| h.start() == 0 && h.item().is_some())
            .and_then(|h| h.child())
            .and_then(|label| label.parent())
            .map(|widget| widget.measure(gtk::Orientation::Vertical, -1).1)
            .filter(|&h| h > 0)
    }

    /// Name every date heading on screen again. A heading is bound to the
    /// first row of its section, which can stay the first row while the
    /// section becomes another (the day turning over): the view does not
    /// bind it again for that.
    pub(crate) fn refresh_headers(&self) {
        self.headers.borrow_mut().retain(|h| h.upgrade().is_some());
        for header in self.headers.borrow().iter().filter_map(|h| h.upgrade()) {
            fill_header(&header);
        }
    }

    /// The rows showing a message right now.
    pub(crate) fn bound_rows(&self) -> Vec<Rc<Row>> {
        self.rows
            .borrow()
            .values()
            .filter(|r| r.st.borrow().item.is_some())
            .cloned()
            .collect()
    }

    /// The row showing the list's position `pos`, if it is on screen.
    pub(crate) fn row_at(&self, pos: usize) -> Option<Rc<Row>> {
        self.bound_rows()
            .into_iter()
            .find(|r| r.position() == Some(pos))
    }

    /// The row the keyboard cursor is on, if any.
    pub(crate) fn cursor(&self) -> Option<(u32, u32)> {
        self.cursor.get()
    }

    /// Put the keyboard cursor on the row showing `key`, or take it away.
    pub(crate) fn set_cursor(&self, key: Option<(u32, u32)>) {
        if self.cursor.replace(key) == key {
            return;
        }
        for row in self.bound_rows() {
            row.sync_host_classes();
        }
    }

    /// Fill every row on screen again (the look changed).
    pub(crate) fn refresh_all(&self) {
        for row in self.bound_rows() {
            row.refresh();
        }
    }

    /// Ask the list, once the current pass is over, about the conversations
    /// rows have just started showing (#222).
    pub(crate) fn want(&self, group: (u32, String)) {
        self.wanted.borrow_mut().push(group);
        if !self.wanted_queued.replace(true) {
            let input = self.input.clone();
            glib::idle_add_local_once(move || {
                let _ = input.send(MessageListInput::AskThreads);
            });
        }
    }

    pub(crate) fn take_wanted(&self) -> Vec<(u32, String)> {
        self.wanted_queued.set(false);
        std::mem::take(&mut *self.wanted.borrow_mut())
    }

    /// The selected rows, in list order, for a drag's cards.
    pub(super) fn selected_rows(&self) -> Vec<Rc<RowData>> {
        let set = self.selection.selection();
        let rows = self.model.imp().rows.borrow();
        let Some((iter, first)) = gtk::BitsetIter::init_first(&set) else {
            return Vec::new();
        };
        std::iter::once(first)
            .chain(iter)
            .filter_map(|pos| rows.get(pos as usize).cloned())
            .collect()
    }

    /// The keys of the selected rows, in list order, for a drag.
    pub(super) fn selected_drag_keys(&self) -> Vec<(u32, u32, u32, u32)> {
        self.selected_rows()
            .iter()
            .map(|r| (r.msg.account_id, r.msg.folder_id, r.msg.uid, r.msg.id))
            .collect()
    }
}

/// How many rows the view's spares hold at most. A long list keeps about
/// two hundred rows made, far more than a window shows: GTK's list view
/// fills rows well beyond the visible ones.
const SPARE_ROWS: usize = 220;

/// How many rows are made ahead, while the app is idle, so the first long
/// list does not make them inside its switch.
const SPARE_AHEAD: usize = 210;

/// Make spare rows ahead, a few per idle pass.
pub(crate) fn make_spare_rows(shared: &Rc<RowShared>) {
    let weak = Rc::downgrade(shared);
    glib::idle_add_local(move || {
        let Some(shared) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let made = shared.rows.borrow().len() + shared.spare.borrow().len();
        if made >= SPARE_AHEAD {
            return glib::ControlFlow::Break;
        }
        for _ in 0..4 {
            let row = Row::new(&shared);
            shared.spare.borrow_mut().push(row);
        }
        glib::ControlFlow::Continue
    });
}

/// The view's row factory: makes rows, binds them to items and back.
pub(crate) fn factory(shared: &Rc<RowShared>) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let key = |obj: &glib::Object| obj.as_ptr() as usize;
    {
        let shared = Rc::downgrade(shared);
        factory.connect_setup(move |_, obj| {
            let (Some(shared), Some(item)) =
                (shared.upgrade(), obj.downcast_ref::<gtk::ListItem>())
            else {
                return;
            };
            let spare = shared.spare.borrow_mut().pop();
            let row = spare.unwrap_or_else(|| Row::new(&shared));
            row.list_item.replace(item.downgrade());
            item.set_child(Some(&row.w.host));
            shared.rows.borrow_mut().insert(key(obj), row);
        });
    }
    {
        let shared = Rc::downgrade(shared);
        factory.connect_bind(move |_, obj| {
            let Some(shared) = shared.upgrade() else {
                return;
            };
            let row = shared.rows.borrow().get(&key(obj)).cloned();
            let item = obj
                .downcast_ref::<gtk::ListItem>()
                .and_then(|li| li.item())
                .and_downcast::<RowItem>();
            if let (Some(row), Some(item)) = (row, item) {
                row.bind(&item);
            }
        });
    }
    {
        let shared = Rc::downgrade(shared);
        factory.connect_unbind(move |_, obj| {
            let Some(shared) = shared.upgrade() else {
                return;
            };
            let row = shared.rows.borrow().get(&key(obj)).cloned();
            if let Some(row) = row {
                row.unbind();
            }
        });
    }
    {
        let shared = Rc::downgrade(shared);
        factory.connect_teardown(move |_, obj| {
            let Some(shared) = shared.upgrade() else {
                return;
            };
            let row = shared.rows.borrow_mut().remove(&key(obj));
            if let Some(row) = row {
                row.unbind();
                if let Some(item) = obj.downcast_ref::<gtk::ListItem>() {
                    item.set_child(None::<&gtk::Widget>);
                }
                row.list_item.replace(glib::WeakRef::new());
                let mut spare = shared.spare.borrow_mut();
                if spare.len() < SPARE_ROWS {
                    spare.push(row);
                }
            }
        });
    }
    factory
}

/// The view's factory for the date headings (#435): a dim heading over each
/// section, not a row, so it is never selected or focused.
pub(crate) fn header_factory(shared: &Rc<RowShared>) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let weak = Rc::downgrade(shared);
    factory.connect_setup(move |_, obj| {
        let Some(header) = obj.downcast_ref::<gtk::ListHeader>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.add_css_class("heading");
        label.add_css_class("dim-label");
        label.add_css_class("list-day-heading");
        header.set_child(Some(&label));
        if let Some(shared) = weak.upgrade() {
            shared.headers.borrow_mut().push(header.downgrade());
        }
    });
    factory.connect_bind(|_, obj| {
        if let Some(header) = obj.downcast_ref::<gtk::ListHeader>() {
            fill_header(header);
        }
    });
    factory
}

/// A heading names the section of the row it stands over.
fn fill_header(header: &gtk::ListHeader) {
    let Some(label) = header.child().and_downcast::<gtk::Label>() else {
        return;
    };
    let text = header
        .item()
        .and_downcast::<RowItem>()
        .map(|i| i.data().section.label())
        .unwrap_or_default();
    if label.label() != text {
        label.set_label(&text);
    }
}
