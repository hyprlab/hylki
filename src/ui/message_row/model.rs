//! The list's GObject model: `RowItem` ("HylkiRowItem") holds one row's
//! data, and `MessageModel` ("HylkiMessageModel") is the ListModel and
//! SectionModel the ListView shows, updated by diffing rows rather than
//! replacing them. A hot path: the diff and the swap thresholds are tuned.

use super::row::Row;
use super::{RowData, Slot};
use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use relm4::prelude::*;
use std::collections::HashMap;
use std::rc::Rc;

glib::wrapper! {
    /// The view's handle on one row of the list. Made only when the view
    /// asks for that position, and kept for as long as the view holds it.
    pub(crate) struct RowItem(ObjectSubclass<item_imp::RowItem>);
}

glib::wrapper! {
    /// Every row of the list, in order, as `RowData`. Items for the view are
    /// made on demand and shared while alive, keyed by the message's slot, so
    /// the same message keeps the same item (and the same bound row) across
    /// a rebuild. Its sections are the runs of rows under one date heading
    /// (#435).
    pub(crate) struct MessageModel(ObjectSubclass<model_imp::MessageModel>)
        @implements gtk::gio::ListModel, gtk::SectionModel;
}

mod item_imp {
    use std::cell::RefCell;
    use std::rc::{Rc, Weak};

    use gtk::glib;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub(crate) struct RowItem {
        pub(super) data: RefCell<Option<Rc<super::RowData>>>,
        /// The row widget showing this item, while it is bound.
        pub(crate) row: RefCell<Weak<super::Row>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RowItem {
        const NAME: &'static str = "HylkiRowItem";
        type Type = super::RowItem;
    }

    impl ObjectImpl for RowItem {}
}

impl RowItem {
    pub(super) fn new(data: Rc<RowData>) -> Self {
        let item: RowItem = glib::Object::new();
        item.imp().data.replace(Some(data));
        item
    }

    pub(crate) fn data(&self) -> Rc<RowData> {
        self.imp()
            .data
            .borrow()
            .clone()
            .expect("a row item always holds its data")
    }

    /// Data for another message: the row showing this item, if any, starts
    /// over as a new binding would (#401).
    pub(super) fn rebind(&self, data: Rc<RowData>) {
        self.imp().data.replace(Some(data));
        let row = self.imp().row.borrow().upgrade();
        if let Some(row) = row {
            row.rebind();
        }
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
            if let (true, Some(group), Some(shared)) =
                (moved, data.meta.group.clone(), row.shared())
            {
                shared.want(group);
            }
        }
    }
}

mod model_imp {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::rc::Rc;

    use gtk::gio;
    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub(crate) struct MessageModel {
        pub(crate) rows: RefCell<Vec<Rc<super::RowData>>>,
        pub(super) items: RefCell<HashMap<super::Slot, glib::WeakRef<super::RowItem>>>,
        /// Where each section starts, worked out again from `rows` the first
        /// time the view asks after they change: a handful of runs, so the
        /// view's question is a short search however long the list.
        sections: RefCell<Vec<u32>>,
        sections_stale: Cell<bool>,
    }

    impl MessageModel {
        /// `rows` changed: the sections are worked out again when next asked.
        pub(crate) fn touch(&self) {
            self.sections_stale.set(true);
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageModel {
        const NAME: &'static str = "HylkiMessageModel";
        type Type = super::MessageModel;
        type Interfaces = (gio::ListModel, gtk::SectionModel);
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

    impl SectionModelImpl for MessageModel {
        fn section(&self, position: u32) -> (u32, u32) {
            let rows = self.rows.borrow();
            let n = rows.len() as u32;
            if position >= n {
                return (n, u32::MAX);
            }
            if self.sections_stale.replace(false) || self.sections.borrow().is_empty() {
                *self.sections.borrow_mut() = super::section_starts(&rows);
            }
            let starts = self.sections.borrow();
            let i = starts.partition_point(|&s| s <= position);
            (starts[i - 1], starts.get(i).copied().unwrap_or(n))
        }
    }
}

/// The first row of each run of rows under one heading: 0 first.
pub(crate) fn section_starts(rows: &[Rc<RowData>]) -> Vec<u32> {
    let mut starts = vec![0];
    for (i, pair) in rows.windows(2).enumerate() {
        if pair[0].section != pair[1].section {
            starts.push(i as u32 + 1);
        }
    }
    starts
}

/// One step of turning the rows on screen into a new list, in row order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum RowEdit {
    /// The row stays, showing the next new row.
    Keep,
    /// New row `i` goes in here.
    Insert(usize),
    /// The row goes.
    Remove,
}

/// The edits that turn `old` into `new`, as far as `old` goes, and the
/// first of `new` left to add after them.
pub(crate) fn row_edits<K: std::hash::Hash + Eq + Copy>(
    old: &[K],
    new: &[K],
) -> (Vec<RowEdit>, usize) {
    let at = |keys: &[K]| -> HashMap<K, usize> {
        keys.iter().enumerate().map(|(n, k)| (*k, n)).collect()
    };
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

/// How many changes past which the view is better told of one swap: each
/// change costs it a pass over its rows, and the row widgets for anything
/// put in.
const SWAP_RUNS: usize = 8;

/// Whether the view is better told of one swap than of `edits`: nothing is
/// kept, or what is kept is in scattered pieces.
pub(crate) fn swap_wanted(edits: &[RowEdit], old_len: usize, tail: usize, new_len: usize) -> bool {
    old_len > 0
        && new_len > 0
        && (!edits.contains(&RowEdit::Keep)
            || change_runs(edits, old_len, tail, new_len) > SWAP_RUNS)
}

/// The separate changes `edits` come to, as `MessageModel::replace` tells
/// the view of them: each run of puts and takes between kept rows, then what
/// is left of either list at the end.
fn change_runs(edits: &[RowEdit], old_len: usize, tail: usize, new_len: usize) -> usize {
    let mut runs = 0;
    let mut in_run = false;
    let mut taken = 0;
    for edit in edits {
        match edit {
            RowEdit::Keep => {
                in_run = false;
                taken += 1;
            }
            RowEdit::Insert(_) => {
                runs += usize::from(!in_run);
                in_run = true;
            }
            RowEdit::Remove => {
                runs += usize::from(!in_run);
                in_run = true;
                taken += 1;
            }
        }
    }
    if taken < old_len || tail < new_len {
        runs += 1;
    }
    runs
}

impl Default for MessageModel {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl MessageModel {
    pub(crate) fn len(&self) -> usize {
        self.imp().rows.borrow().len()
    }

    /// Every row, in order.
    pub(crate) fn rows(&self) -> Vec<Rc<RowData>> {
        self.imp().rows.borrow().clone()
    }

    pub(crate) fn row(&self, pos: usize) -> Option<Rc<RowData>> {
        self.imp().rows.borrow().get(pos).cloned()
    }

    /// Where the rows `f` picks are, with them.
    pub(crate) fn find(&self, f: impl Fn(&RowData) -> bool) -> Vec<(usize, Rc<RowData>)> {
        self.imp()
            .rows
            .borrow()
            .iter()
            .enumerate()
            .filter(|(_, r)| f(r))
            .map(|(i, r)| (i, r.clone()))
            .collect()
    }

    /// Replace the list with `new`, telling the view only what changed:
    /// rows that show the same message keep their place (and their bound
    /// widget, refreshed with the new data), and the rest are put in or
    /// taken out in runs.
    /// `stale`: the list's look changed since the rows on screen were
    /// filled, so a swap fills again the ones it keeps. Returns whether it
    /// was a swap, which leaves nothing on screen stale.
    pub(crate) fn replace(&self, new: Vec<Rc<RowData>>, stale: bool) -> bool {
        let old: Vec<Slot> = self.imp().rows.borrow().iter().map(|r| r.slot()).collect();
        let keys: Vec<Slot> = new.iter().map(|r| r.slot()).collect();
        let (edits, tail) = row_edits(&old, &keys);
        if swap_wanted(&edits, old.len(), tail, new.len()) {
            self.swap(new, stale);
            return true;
        }
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
                self.imp().touch();
                self.items_changed(start as u32, *removed as u32, n as u32);
            }
            *removed = 0;
            n
        };
        let mut in_run = false;
        let mut moved_section = false;
        for edit in edits {
            match edit {
                RowEdit::Keep => {
                    if in_run {
                        pos = start + flush(start, &mut removed, &mut added);
                        in_run = false;
                    }
                    moved_section |= self.put_row(pos, new[i].clone());
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
        self.imp()
            .items
            .borrow_mut()
            .retain(|_, w| w.upgrade().is_some());
        if moved_section {
            self.resection();
        }
        false
    }

    /// New data for the row at `pos`, showing the same message.
    pub(crate) fn set_row(&self, pos: usize, data: Rc<RowData>) {
        if self.put_row(pos, data) {
            self.resection();
        }
    }

    /// `set_row`, leaving the view to be told of a row that moved to another
    /// heading: what it returns.
    fn put_row(&self, pos: usize, data: Rc<RowData>) -> bool {
        let slot = data.slot();
        let moved;
        {
            let mut rows = self.imp().rows.borrow_mut();
            let Some(row) = rows.get_mut(pos) else {
                return false;
            };
            // A rebuild makes new data for every row; one that says the same
            // as before is not drawn again (#330).
            if Rc::ptr_eq(row, &data) || **row == *data {
                return false;
            }
            moved = row.section != data.section;
            *row = data.clone();
        }
        if moved {
            self.imp().touch();
        }
        let live = self
            .imp()
            .items
            .borrow()
            .get(&slot)
            .and_then(|w| w.upgrade());
        if let Some(item) = live {
            item.set_data(data);
        }
        moved
    }

    /// Rows changed heading in place, which no `items-changed` says: the
    /// view asks for every section again. Rare (the day turning over, the
    /// setting, another sort), and the view only looks again at the
    /// headings it holds.
    fn resection(&self) {
        let n = self.len() as u32;
        if n > 0 {
            self.sections_changed(0, n);
        }
    }

    /// Put `new` in place of the rows by position, for a list that is
    /// mostly or wholly another (a folder switch). The view keeps the items
    /// it holds, each now standing for the new row at its place, so it keeps
    /// its row widgets too: new items would have it destroy every one and
    /// build and style them again, a third of a second at a folder's size
    /// (#401).
    fn swap(&self, new: Vec<Rc<RowData>>, stale: bool) {
        let imp = self.imp();
        let old = imp.rows.replace(new.clone());
        imp.touch();
        let moved_section = old
            .iter()
            .zip(&new)
            .any(|(was, now)| was.section != now.section);
        let held = imp.items.take();
        let mut moved = Vec::new();
        {
            let mut items = imp.items.borrow_mut();
            for (was, now) in old.iter().zip(&new) {
                if let Some(item) = held.get(&was.slot()).and_then(|w| w.upgrade()) {
                    items.insert(now.slot(), item.downgrade());
                    moved.push((item, now.clone()));
                }
            }
        }
        // Outside the borrows: a row being filled reads the model.
        for (item, data) in moved {
            let was = item.data();
            if was.slot() != data.slot() {
                item.rebind(data);
            } else if stale || *was != *data {
                item.set_data(data);
            } else {
                item.imp().data.replace(Some(data));
            }
        }
        let kept = old.len().min(new.len()) as u32;
        if old.len() > new.len() {
            self.items_changed(kept, old.len() as u32 - kept, 0);
        } else if new.len() > old.len() {
            self.items_changed(kept, 0, new.len() as u32 - kept);
        }
        if moved_section {
            self.resection();
        }
    }

    /// Change the row at `pos` in place.
    pub(crate) fn update_row(&self, pos: usize, f: impl FnOnce(&mut RowData)) {
        let Some(row) = self.row(pos) else { return };
        let mut data = (*row).clone();
        f(&mut data);
        self.set_row(pos, Rc::new(data));
    }

    pub(crate) fn splice(&self, pos: usize, removed: usize, added: Vec<Rc<RowData>>) {
        let n = added.len();
        self.imp()
            .rows
            .borrow_mut()
            .splice(pos..pos + removed, added);
        self.imp().touch();
        self.items_changed(pos as u32, removed as u32, n as u32);
    }
}
