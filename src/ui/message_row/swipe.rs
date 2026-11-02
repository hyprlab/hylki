//! Swiping a row (`SwipeSurface`, "HylkiSwipeSurface", an AdwSwipeable):
//! the surface, its progress, and the row's swipe actions and animation.

use super::row::Row;
use super::{RowAction, SWIPE_ARM, SWIPE_EXIT_MS, SWIPE_MAX, SWIPE_RESTORE_MS, THREAD_NODE_REACH};
use crate::i18n::i18n;
use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use relm4::prelude::*;
use std::rc::Rc;

// A two-layer container implementing `AdwSwipeable`, so a real
// `AdwSwipeTracker` — the gesture engine behind `AdwFlap` and `AdwCarousel` —
// can drive the row's swipe-to-act gesture (#swipe): `background` is the
// fixed action strip, always allocated full-size and never moving;
// `foreground` is the row's real content, translated across it via a
// `GskTransform` on its allocation as the swipe drags it, Gmail-style.
glib::wrapper! {
    pub(crate) struct SwipeSurface(ObjectSubclass<swipe_surface_imp::SwipeSurface>)
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
    pub(super) fn set_background(&self, child: &impl IsA<gtk::Widget>) {
        child.set_parent(self);
    }

    /// The row's real content, on top — translated by [`Self::set_progress_px`]
    /// to reveal `background` underneath it.
    pub(super) fn set_foreground(&self, child: &impl IsA<gtk::Widget>) {
        child.set_parent(self);
    }

    /// The live swipe distance, in `AdwSwipeTracker`'s own px convention
    /// (`AdwSwipeable::progress` reports it back verbatim). Queues a fresh
    /// allocation so the translation actually moves — setting this alone
    /// touches no property GTK would otherwise notice.
    pub(super) fn set_progress_px(&self, px: f64) {
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
    pub(super) fn set_sensitivity(&self, factor: f64) {
        self.imp().sensitivity.set(factor.clamp(
            crate::config::SWIPE_SENSITIVITY_MIN,
            crate::config::SWIPE_SENSITIVITY_MAX,
        ));
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

    pub(crate) struct SwipeSurface {
        pub(super) progress_px: Cell<f64>,
        /// Trackpad sensitivity (see `super::SWIPE_MAX`). Never 0 — that
        /// would divide by zero in `distance`/`progress` — so this can't
        /// simply be `#[derive(Default)]`.
        pub(super) sensitivity: Cell<f64>,
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
                    gtk::gsk::Transform::new().translate(&gtk::graphene::Point::new(offset, 0.0))
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
            // The rail runs down the start side: the right in a
            // right-to-left window (#366).
            let x = if obj.direction() == gtk::TextDirection::Rtl {
                0.0
            } else {
                -reach
            };
            snapshot.push_clip(&gtk::graphene::Rect::new(x, 0.0, w + reach, h));

            let offset = self.visual_offset();
            if let (Some(bg), true) = (self.background(), offset != 0.0) {
                // offset < 0: content slid left, uncovering a gap on the
                // RIGHT. offset > 0: slid right, gap on the LEFT. The gap
                // opens at the pill's edge, not the row's: the pill sits
                // inset in the row, and measuring from the row left a strip
                // of bare ground between a selected pill and the strip.
                let pill = self
                    .foreground()
                    .and_then(|fg| fg.downcast::<gtk::Overlay>().ok())
                    .and_then(|o| o.child())
                    .and_then(|c| c.compute_bounds(&*obj));
                let gap = if offset < 0.0 {
                    let x = pill.map_or(w + offset, |b| b.x() + b.width());
                    gtk::graphene::Rect::new(x, 0.0, w - x, h)
                } else {
                    let x = pill.map_or(offset, |b| b.x());
                    gtk::graphene::Rect::new(0.0, 0.0, x, h)
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
pub(crate) fn swipe_progress_px(progress: f64, sensitivity: f64) -> f64 {
    (progress * SWIPE_MAX * sensitivity).clamp(-SWIPE_MAX, SWIPE_MAX)
}

impl Row {
    /// The `AdwSwipeTracker` driving the swipe-to-act gesture (#swipe):
    /// mouse-drag and trackpad both arrive as the same signals.
    pub(super) fn wire_swipe(&self) {
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
    pub(super) fn sync_swipe_strip(&self) {
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
        self.w.swipe_inner.set_halign(if side < 0 {
            gtk::Align::End
        } else {
            gtk::Align::Start
        });
        match action {
            RowAction::Delete => {
                self.w.swipe_icon.set_icon_name(Some("user-trash-symbolic"));
                self.w.swipe_label.set_label(&i18n("Delete"));
            }
            _ => {
                self.w
                    .swipe_icon
                    .set_icon_name(Some("mail-archive-symbolic"));
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
            st.swipe_progress = if st.swipe_side < 0 {
                -SWIPE_MAX
            } else {
                SWIPE_MAX
            };
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
            let anim = adw::TimedAnimation::new(
                &self.w.overlay,
                surface.progress_px(),
                target,
                SWIPE_EXIT_MS,
                setter,
            );
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
                glib::timeout_add_local_once(
                    std::time::Duration::from_millis(SWIPE_RESTORE_MS),
                    move || {
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
                    },
                );
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
    pub(crate) fn debug_swipe(&self, left: bool) {
        let px = if left { -SWIPE_MAX } else { SWIPE_MAX };
        self.swipe_update(px);
        self.swipe_end();
    }
}
