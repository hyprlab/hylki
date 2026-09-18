//! A wrapping row of chips whose positions animate: the drop zones of the
//! reader toolbar editor (Settings → Appearance → Toolbar). While a chip is dragged
//! over a zone, the zone opens a gap under the pointer and the other chips
//! slide out of the way; move along the row and the gap follows. The
//! dragged chip's own zone closes the hole it left the same way.
//!
//! A plain widget subclass: children are laid out left to right, wrapping
//! at the allocated width, and every child is allocated at an animated
//! position that eases towards its slot whenever the slots change.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{glib, subclass::prelude::*};

const SPACING: i32 = 6;
const ANIM_MS: u32 = 180;

/// One slot of the row: a child or the gap.
struct Slot {
    child: Option<gtk::Widget>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ChipFlow {
        /// Where each child currently sits (content coordinates), eased
        /// towards its slot.
        pub pos: RefCell<HashMap<gtk::Widget, (f64, f64)>>,
        /// The slot each child was last sent towards.
        pub target: RefCell<HashMap<gtk::Widget, (i32, i32)>>,
        pub anims: RefCell<HashMap<gtk::Widget, adw::TimedAnimation>>,
        /// The slot index a dragged chip would take (a gap opens there).
        pub gap: Cell<Option<usize>>,
        /// The largest chip seen by any zone (shared), to size an empty
        /// zone by.
        pub chip_size: RefCell<Option<Rc<Cell<(i32, i32)>>>>,
        /// The size of the chip being dragged right now (shared; zero when
        /// none): the gap opens at exactly that size.
        pub drag_size: RefCell<Option<Rc<Cell<(i32, i32)>>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ChipFlow {
        const NAME: &'static str = "HylkiChipFlow";
        type Type = super::ChipFlow;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for ChipFlow {
        fn dispose(&self) {
            for (_, a) in self.anims.borrow_mut().drain() {
                a.pause();
            }
            while let Some(c) = self.obj().first_child() {
                c.unparent();
            }
        }
    }

    impl WidgetImpl for ChipFlow {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let obj = self.obj();
            let chips = obj.chip_sizes();
            let gap = obj.gap_size(&chips);
            if orientation == gtk::Orientation::Horizontal {
                // Narrowest: one chip per row. Natural: the whole row.
                let widest = chips.iter().map(|(_, w, _)| *w).max().unwrap_or(0).max(gap.0);
                let mut nat = chips.iter().map(|(_, w, _)| w + SPACING).sum::<i32>();
                if self.gap.get().is_some() {
                    nat += gap.0 + SPACING;
                }
                (widest, (nat - SPACING).max(widest), -1, -1)
            } else {
                let width = if for_size < 0 { i32::MAX / 2 } else { for_size };
                let h = obj.layout_height(width, &chips);
                (h, h, -1, -1)
            }
        }

        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            let obj = self.obj();
            let chips = obj.chip_sizes();
            let slots = obj.slots(width, &chips);
            // Work out every move first, then start the animations with no
            // borrow held (their first tick may land before this returns).
            let mut moves: Vec<(gtk::Widget, (f64, f64), (i32, i32))> = Vec::new();
            {
                let mut pos = self.pos.borrow_mut();
                let mut target = self.target.borrow_mut();
                for s in &slots {
                    let Some(child) = &s.child else {
                        continue;
                    };
                    let t = (s.x, s.y);
                    if target.get(child) == Some(&t) {
                        continue;
                    }
                    target.insert(child.clone(), t);
                    match pos.get(child).copied() {
                        // A new chip appears in place; only moves animate.
                        None => {
                            pos.insert(child.clone(), (s.x as f64, s.y as f64));
                        }
                        Some(from) => moves.push((child.clone(), from, t)),
                    }
                }
            }
            for (child, from, to) in moves {
                obj.animate(child, from, to);
            }
            let pos = self.pos.borrow();
            for s in &slots {
                let Some(child) = &s.child else {
                    continue;
                };
                let (x, y) = pos.get(child).copied().unwrap_or((s.x as f64, s.y as f64));
                child.size_allocate(&gtk::Allocation::new(x.round() as i32, y.round() as i32, s.w, s.h), -1);
            }
        }
    }
}

glib::wrapper! {
    pub struct ChipFlow(ObjectSubclass<imp::ChipFlow>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ChipFlow {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl ChipFlow {
    pub fn new(chip_size: Rc<Cell<(i32, i32)>>, drag_size: Rc<Cell<(i32, i32)>>) -> Self {
        let this = Self::default();
        *this.imp().chip_size.borrow_mut() = Some(chip_size);
        *this.imp().drag_size.borrow_mut() = Some(drag_size);
        this
    }

    pub fn append(&self, child: &impl IsA<gtk::Widget>) {
        child.set_parent(self);
        self.queue_resize();
    }

    /// Drop every chip (and the gap). Positions start afresh: the next
    /// chips appear in place rather than sliding from wherever the old
    /// ones were.
    pub fn remove_all(&self) {
        let imp = self.imp();
        for (_, a) in imp.anims.borrow_mut().drain() {
            a.pause();
        }
        imp.pos.borrow_mut().clear();
        imp.target.borrow_mut().clear();
        imp.gap.set(None);
        while let Some(c) = self.first_child() {
            c.unparent();
        }
        self.queue_resize();
    }

    /// Open the gap at `index` (None closes it). The chips slide to make
    /// or take the room.
    pub fn set_gap(&self, index: Option<usize>) {
        let imp = self.imp();
        let index = index.map(|i| i.min(self.visible_children().len()));
        if imp.gap.get() != index {
            imp.gap.set(index);
            self.queue_resize();
        }
    }

    /// The slot a drop at (`x`, `y`) would take, judged against where the
    /// chips are heading (their slots with the current gap, not their
    /// mid-slide positions): every chip on a row above the point, or on
    /// its row with its middle left of it, comes before.
    pub fn insertion_index(&self, x: f64, y: f64) -> usize {
        let chips = self.chip_sizes();
        let slots = self.slots(self.width().max(1), &chips);
        let mut index = 0;
        for s in &slots {
            if s.child.is_none() {
                continue;
            }
            let above = (s.y + s.h) as f64 <= y;
            let same_row = (s.y as f64) <= y && y < (s.y + s.h) as f64;
            let left = (s.x as f64) + (s.w as f64) / 2.0 <= x;
            if above || (same_row && left) {
                index += 1;
            }
        }
        index
    }

    fn visible_children(&self) -> Vec<gtk::Widget> {
        let mut out = Vec::new();
        let mut child = self.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            if c.should_layout() {
                out.push(c);
            }
        }
        out
    }

    /// Each visible chip with its natural size. Keeps the shared chip
    /// size current for empty zones.
    fn chip_sizes(&self) -> Vec<(gtk::Widget, i32, i32)> {
        let out: Vec<_> = self
            .visible_children()
            .into_iter()
            .map(|c| {
                let w = c.measure(gtk::Orientation::Horizontal, -1).1;
                let h = c.measure(gtk::Orientation::Vertical, w).1;
                (c, w, h)
            })
            .collect();
        if let Some(shared) = self.imp().chip_size.borrow().as_ref() {
            let w = out.iter().map(|(_, w, _)| *w).max().unwrap_or(0);
            let h = out.iter().map(|(_, _, h)| *h).max().unwrap_or(0);
            if w > 0 && h > 0 {
                let cur = shared.get();
                shared.set((cur.0.max(w), cur.1.max(h)));
            }
        }
        out
    }

    /// The gap's size: the dragged chip's own while a drag is on, else the
    /// largest chip known (for an empty zone's height).
    fn gap_size(&self, chips: &[(gtk::Widget, i32, i32)]) -> (i32, i32) {
        let imp = self.imp();
        let dragging = imp.drag_size.borrow().as_ref().map(|s| s.get()).unwrap_or((0, 0));
        if dragging.0 > 0 && dragging.1 > 0 {
            return dragging;
        }
        let shared = imp.chip_size.borrow().as_ref().map(|s| s.get()).unwrap_or((0, 0));
        let w = chips.iter().map(|(_, w, _)| *w).max().unwrap_or(0).max(shared.0);
        let h = chips.iter().map(|(_, _, h)| *h).max().unwrap_or(0).max(shared.1);
        (if w > 0 { w } else { 72 }, if h > 0 { h } else { 48 })
    }

    /// Lay the chips (and the gap) out at `width`, wrapping.
    fn slots(&self, width: i32, chips: &[(gtk::Widget, i32, i32)]) -> Vec<Slot> {
        self.flow(width, chips).0
    }

    /// The rows' total height at `width`: one chip's worth even when empty,
    /// so there is still something to aim at.
    fn layout_height(&self, width: i32, chips: &[(gtk::Widget, i32, i32)]) -> i32 {
        self.flow(width, chips).1
    }

    fn flow(&self, width: i32, chips: &[(gtk::Widget, i32, i32)]) -> (Vec<Slot>, i32) {
        let gap = self.gap_size(chips);
        let row_h = chips.iter().map(|(_, _, h)| *h).max().unwrap_or(0).max(gap.1);
        let mut entries: Vec<(Option<gtk::Widget>, i32, i32)> =
            chips.iter().map(|(c, w, h)| (Some(c.clone()), *w, *h)).collect();
        if let Some(i) = self.imp().gap.get() {
            entries.insert(i.min(entries.len()), (None, gap.0, gap.1));
        }
        let mut out = Vec::with_capacity(entries.len());
        let (mut x, mut y) = (0, 0);
        for (child, w, h) in entries {
            if x > 0 && x + w > width {
                x = 0;
                y += row_h + SPACING;
            }
            // Chips sit centred in their row.
            out.push(Slot { child, x, y: y + (row_h - h) / 2, w, h });
            x += w + SPACING;
        }
        (out, y + row_h)
    }

    fn animate(&self, child: gtk::Widget, from: (f64, f64), to: (i32, i32)) {
        let imp = self.imp();
        if let Some(old) = imp.anims.borrow_mut().remove(&child) {
            // Stop it where it is; skip() would jump to the old slot.
            old.pause();
        }
        let weak = self.downgrade();
        let c = child.clone();
        let (tx, ty) = (to.0 as f64, to.1 as f64);
        let target = adw::CallbackAnimationTarget::new(move |v| {
            let Some(flow) = weak.upgrade() else {
                return;
            };
            flow.imp()
                .pos
                .borrow_mut()
                .insert(c.clone(), (from.0 + (tx - from.0) * v, from.1 + (ty - from.1) * v));
            flow.queue_allocate();
        });
        let anim = adw::TimedAnimation::new(self, 0.0, 1.0, ANIM_MS, target);
        anim.set_easing(adw::Easing::EaseOutCubic);
        imp.anims.borrow_mut().insert(child, anim.clone());
        anim.play();
    }
}
