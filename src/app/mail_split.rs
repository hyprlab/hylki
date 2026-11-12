//! The split between the list and the reader (#28, #475, #648).
//!
//! The list keeps the size the user dragged it to, or with Settings →
//! Message List → Resize the list with the window, the share of the window
//! that size was. A window too small for it squeezes the list for the
//! moment only: what is remembered is the drag, never the squeeze, so the
//! list grows back with the window instead of staying squeezed.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use relm4::ComponentSender;

use super::model::AppModel;
use super::msg::AppMsg;
use crate::config;

/// A drag sends a position for every pixel: one write once it settles.
const SAVE_AFTER: Duration = Duration::from_millis(600);

/// The split, and the sizes the user chose for it.
#[derive(Debug)]
pub(super) struct MailSplit {
    paned: OnceCell<gtk::Paned>,
    /// The list keeps its share of the window, not its size.
    follows: Cell<bool>,
    /// The list's width beside the reader and its height above it.
    width: Cell<i32>,
    height: Cell<i32>,
    /// The share of the split each was, or 0 until worked out.
    width_share: Cell<f64>,
    height_share: Cell<f64>,
    /// The paned's own drag gestures: a move while one is active is a drag.
    drags: RefCell<Vec<gtk::GestureDrag>>,
    /// A key moved the divider (F6 or F8, then the arrows).
    keyed: Cell<bool>,
    /// This module is moving the divider: not the user.
    placing: Cell<bool>,
    pending_save: RefCell<Option<glib::SourceId>>,
}

impl MailSplit {
    pub(super) fn new(follows: bool) -> Rc<Self> {
        let (width_share, height_share) = config::load_list_pane_shares();
        Rc::new(Self {
            paned: OnceCell::new(),
            follows: Cell::new(follows),
            width: Cell::new(config::load_list_pane_width()),
            height: Cell::new(config::load_list_pane_height()),
            width_share: Cell::new(width_share),
            height_share: Cell::new(height_share),
            drags: RefCell::new(Vec::new()),
            keyed: Cell::new(false),
            placing: Cell::new(false),
            pending_save: RefCell::new(None),
        })
    }

    pub(super) fn follows(&self) -> bool {
        self.follows.get()
    }

    /// Take charge of the split, built by the window's view.
    pub(super) fn wire(self: &Rc<Self>, paned: gtk::Paned, sender: &ComponentSender<AppModel>) {
        let controllers = paned.observe_controllers();
        let drags = (0..controllers.n_items())
            .filter_map(|i| controllers.item(i)?.downcast::<gtk::GestureDrag>().ok())
            .collect();
        *self.drags.borrow_mut() = drags;

        self.place(&paned, self.wanted(&paned));

        let me = self.clone();
        let sender = sender.clone();
        paned.connect_position_notify(move |p| me.moved(p, &sender));

        // The floors bound the divider, and they move with the window's
        // size: each time, put the list back where it belongs.
        for bound in ["min-position", "max-position"] {
            let me = self.clone();
            paned.connect_notify_local(Some(bound), move |p, _| me.settle(p));
        }

        let me = self.clone();
        paned.connect_move_handle(move |_, _| {
            me.keyed.set(true);
            let me = me.clone();
            glib::idle_add_local_once(move || me.keyed.set(false));
            false
        });

        let _ = self.paned.set(paned);
    }

    /// Turn the split for the reader beside the list or below it.
    pub(super) fn set_orientation(&self, orientation: gtk::Orientation) {
        let Some(paned) = self.paned.get() else {
            return;
        };
        if paned.orientation() != orientation {
            paned.set_orientation(orientation);
            // The other length is not allocated yet: start from the size,
            // and the new bounds settle it from there.
            let below = orientation == gtk::Orientation::Vertical;
            self.place(paned, self.size(below));
        }
    }

    /// Turned on, the list keeps the share of the window it has now.
    pub(super) fn set_follows(&self, on: bool) {
        self.follows.set(on);
        let Some(paned) = self.paned.get() else {
            return;
        };
        let len = Self::length(paned);
        if on && len > 0 {
            let below = Self::below(paned);
            self.share(below)
                .set(f64::from(paned.position()) / f64::from(len));
            self.save();
        }
        self.settle(paned);
    }

    fn below(paned: &gtk::Paned) -> bool {
        paned.orientation() == gtk::Orientation::Vertical
    }

    /// The paned's length across the divider.
    fn length(paned: &gtk::Paned) -> i32 {
        if Self::below(paned) {
            paned.height()
        } else {
            paned.width()
        }
    }

    fn size(&self, below: bool) -> i32 {
        if below {
            self.height.get()
        } else {
            self.width.get()
        }
    }

    fn share(&self, below: bool) -> &Cell<f64> {
        if below {
            &self.height_share
        } else {
            &self.width_share
        }
    }

    /// Where the divider belongs at the paned's present length.
    fn wanted(&self, paned: &gtk::Paned) -> i32 {
        let below = Self::below(paned);
        let size = self.size(below);
        let len = Self::length(paned);
        if !self.follows.get() || len <= 0 {
            return size;
        }
        // No share until the list is dragged with the reader on this side,
        // or the setting is turned on: until then, the size.
        match self.share(below).get() {
            share if share > 0.0 => (share * f64::from(len)).round() as i32,
            _ => size,
        }
    }

    /// Put the divider where it belongs, as far as the panes' floors allow.
    fn settle(&self, paned: &gtk::Paned) {
        let want = self.wanted(paned);
        let (lo, hi) = (paned.min_position(), paned.max_position());
        let want = if lo <= hi { want.clamp(lo, hi) } else { want };
        if paned.position() != want {
            self.place(paned, want);
        }
    }

    fn place(&self, paned: &gtk::Paned, position: i32) {
        self.placing.set(true);
        paned.set_position(position);
        self.placing.set(false);
    }

    fn moved(self: &Rc<Self>, paned: &gtk::Paned, sender: &ComponentSender<AppModel>) {
        let pos = paned.position();
        let below = Self::below(paned);
        // Automatic layout follows the width as it changes, not once a
        // drag has settled (#334).
        if !below {
            sender.input(AppMsg::ListPaneWidth(pos));
        }
        let by_user = self.keyed.get() || self.drags.borrow().iter().any(|g| g.is_active());
        if self.placing.get() || !by_user {
            return;
        }
        if below {
            self.height.set(pos);
        } else {
            self.width.set(pos);
        }
        let len = Self::length(paned);
        if len > 0 {
            self.share(below).set(f64::from(pos) / f64::from(len));
        }
        self.save_soon();
    }

    fn save_soon(self: &Rc<Self>) {
        if let Some(id) = self.pending_save.borrow_mut().take() {
            id.remove();
        }
        let me = self.clone();
        let id = glib::timeout_add_local_once(SAVE_AFTER, move || {
            me.pending_save.borrow_mut().take();
            me.save();
        });
        *self.pending_save.borrow_mut() = Some(id);
    }

    fn save(&self) {
        config::save_list_split(
            self.width.get(),
            self.height.get(),
            self.width_share.get(),
            self.height_share.get(),
        );
    }
}
