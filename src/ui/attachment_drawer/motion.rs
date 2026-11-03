//! How the drawer comes and goes and how tall it is: its list, collapse and
//! expand, the divider and the remembered height.

use super::{AttachmentDrawer, AttachmentDrawerInput, MIN_HEIGHT};
use crate::config;
use crate::models::Attachment;
use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;

impl AttachmentDrawer {
    /// Show a new list. Opening a message hands it over while the reading
    /// pane is frozen (`ReaderSwap`), so the drawer comes, goes or changes
    /// its files unseen, and its room is given or taken before the new
    /// message is laid out.
    pub(super) fn apply_items(&mut self, items: Vec<Attachment>, sender: &ComponentSender<Self>) {
        self.items = items;
        self.update_pill_visibility();
        self.rebuild(sender);
    }

    /// The grab pill shows whenever the drawer does — collapsed included,
    /// where a click or a drag brings the drawer back out. Only a message
    /// with no attachments (no drawer, no seam) hides it.
    fn update_pill_visibility(&self) {
        let shown = !self.items.is_empty();
        self.pill.set_visible(shown);
        // The pill's zone covers the reader's foot, where the find bar can
        // sit (#392): the class on the reader's overlay lets the bar make
        // room for it instead of losing clicks to it.
        if let Some(wrap) = self.pill.parent() {
            if shown {
                wrap.add_css_class("drawer-seam");
            } else {
                wrap.remove_css_class("drawer-seam");
            }
        }
    }

    /// Flip collapsed/expanded — a plain click on the grab pill — sliding the
    /// divider there like the split reply's entrance rather than jumping.
    ///
    /// Expanding shows the body at once so it rides the slide open; the
    /// collapse keeps the body visible for the ride down and hides it when
    /// the slide settles (`ToggleSettled`), else the thumbnails would vanish
    /// a beat before the panel. Either way the drawer may momentarily be
    /// squeezed below its minimum, so end-child shrink is allowed for the
    /// duration.
    pub(super) fn toggle_collapsed(&mut self, sender: &ComponentSender<Self>) {
        self.finish_running_toggle();
        let total = self.paned.height();
        if total <= 1 {
            // Not allocated (never shown): the old instant flip.
            self.collapsed = !self.collapsed;
            self.apply_position();
            config::save_drawer_collapsed(self.collapsed);
            return;
        }
        let closing = !self.collapsed;
        if !closing {
            self.collapsed = false;
        }
        config::save_drawer_collapsed(closing);
        let target_h = if closing {
            Self::header_height(&self.paned)
        } else {
            self.height
        };

        let paned = self.paned.clone();
        // The whole ride is programmatic — position-notify must not read it
        // as a user drag.
        self.adjusting.set(true);
        paned.set_shrink_end_child(true);
        let anim = adw::TimedAnimation::new(
            &paned,
            paned.position() as f64,
            (total - target_h).max(0) as f64,
            300,
            adw::CallbackAnimationTarget::new({
                let paned = paned.clone();
                move |v| paned.set_position(v as i32)
            }),
        );
        anim.set_easing(adw::Easing::EaseOutCubic);
        self.toggle_gen += 1;
        let gen = self.toggle_gen;
        let cell = self.toggle_anim.clone();
        let s = sender.clone();
        // NOTHING is restored here: un-shrinking the drawer while its body is
        // still visible makes the Paned jump the divider up to the body's
        // minimum, then the settle snaps it back — a visible flash (and a
        // window where the jump could be recorded as a height). The settle
        // handler restores state in the right order instead.
        anim.connect_done(move |_| {
            cell.borrow_mut().take();
            s.input_sender().emit(AttachmentDrawerInput::ToggleSettled {
                collapsed: closing,
                gen,
            });
        });
        *self.toggle_anim.borrow_mut() = Some((anim.clone(), closing));
        anim.play();
    }

    /// If an expand/collapse slide is running, jump it to its end and settle
    /// its target state synchronously. The `ToggleSettled` it queues on the
    /// way out is voided by bumping the generation.
    pub(super) fn finish_running_toggle(&mut self) {
        let running = self.toggle_anim.borrow_mut().take();
        let Some((anim, closing)) = running else {
            return;
        };
        self.toggle_gen += 1;
        anim.skip();
        if closing {
            self.settle_collapsed();
        }
        self.paned.set_shrink_end_child(false);
        self.adjusting.set(false);
    }

    /// Collapse without the slide, and keep it so.
    pub(super) fn fold(&mut self) {
        self.finish_running_toggle();
        if !self.collapsed {
            self.settle_collapsed();
            config::save_drawer_collapsed(true);
        }
    }

    /// The end state of a collapse: body hidden, divider locked, position
    /// pinned to exactly the header.
    fn settle_collapsed(&mut self) {
        self.collapsed = true;
        self.apply_position();
    }

    /// Natural height of the drawer's header row (the first child of the end
    /// pane) — the drawer's height when collapsed.
    fn header_height(paned: &gtk::Paned) -> i32 {
        // The collapsed drawer keeps everything above the scroller: the edge
        // strip and the header row.
        let Some(end) = paned.end_child() else {
            return 44;
        };
        // The body sits in the drawer's overlay (see init).
        let body = end
            .downcast_ref::<gtk::Overlay>()
            .and_then(|o| o.child())
            .unwrap_or(end);
        let mut sum = 0;
        let mut child = body.first_child();
        while let Some(c) = child {
            if c.is::<gtk::ScrolledWindow>() {
                break;
            }
            sum += c.measure(gtk::Orientation::Vertical, -1).1;
            child = c.next_sibling();
        }
        if sum > 0 {
            sum
        } else {
            44
        }
    }

    /// Move the Paned divider so the drawer body is the desired height: the
    /// header alone when collapsed, else `self.height`. Guarded so the resulting
    /// position-notify isn't taken for a user drag.
    fn apply_position(&self) {
        let total = self.paned.height();
        if total <= 1 {
            return; // not allocated yet
        }
        let drawer_h = if self.collapsed {
            Self::header_height(&self.paned)
        } else {
            self.height
        };
        self.adjusting.set(true);
        self.paned.set_position((total - drawer_h).max(0));
        self.adjusting.set(false);
    }

    /// Make the Paned's own separator inert (there is no lock API on
    /// GtkPaned, so this walks its internal children and untargets the child
    /// that is neither pane). Called once at init and left off for good: the
    /// separator's enlarged input region otherwise steals presses near the
    /// seam and wears its own cursor. The pill zone and the drawer's edge
    /// strip do all the clicking and dragging.
    pub(super) fn set_divider_draggable(&self, on: bool) {
        let start = self.paned.start_child();
        let end = self.paned.end_child();
        let mut child = self.paned.first_child();
        while let Some(c) = child {
            if Some(&c) != start.as_ref() && Some(&c) != end.as_ref() {
                c.set_can_target(on);
            }
            child = c.next_sibling();
        }
    }

    /// Persist the dragged height, debounced past the end of the drag.
    fn schedule_height_save(&self) {
        let height = self.height;
        let slot = self.height_save.clone();
        if let Some(prev) = slot.borrow_mut().take() {
            prev.remove();
        }
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(400), {
            let slot = slot.clone();
            move || {
                slot.borrow_mut().take();
                config::save_drawer_height(height);
            }
        });
        *slot.borrow_mut() = Some(id);
    }

    /// Apply the remembered split once the Paned has been allocated. The drawer
    /// has just become visible, so defer one loop iteration for layout to settle.
    fn schedule_initial_position(&self) {
        let paned = self.paned.clone();
        let adjusting = self.adjusting.clone();
        let positioned = self.positioned.clone();
        let height = self.height;
        let collapsed = self.collapsed;
        glib::idle_add_local_once(move || {
            if positioned.get() {
                return;
            }
            let total = paned.height();
            if total <= 1 {
                return; // still unallocated; a later SetItems will retry
            }
            let drawer_h = if collapsed {
                Self::header_height(&paned)
            } else {
                height
            };
            adjusting.set(true);
            paned.set_position((total - drawer_h).max(0));
            adjusting.set(false);
            positioned.set(true);
        });
    }
}

impl AttachmentDrawer {
    /// A new list of files, the empty one held back a moment (see `SetItems`).
    pub(super) fn set_items(&mut self, items: Vec<Attachment>, sender: &ComponentSender<Self>) {
        self.empty_gen += 1;
        // Opening a message clears the list and then gives the new
        // one, as two messages. Taken as they come, the drawer would
        // fade out and back in between two messages that both have
        // files. The empty list waits until the messages already
        // queued are handled (still before the next frame is drawn,
        // so a drawer that really goes still goes with its message).
        if items.is_empty() && self.drawer_body.as_ref().is_some_and(|b| b.is_visible()) {
            let gen = self.empty_gen;
            let s = sender.clone();
            glib::idle_add_local_full(glib::Priority::HIGH_IDLE, move || {
                s.input_sender()
                    .emit(AttachmentDrawerInput::SettleEmpty(gen));
                glib::ControlFlow::Break
            });
            return;
        }
        let became_visible = self.items.is_empty() && !items.is_empty();
        self.apply_items(items, sender);
        // Apply the remembered split the first time the drawer appears
        // (the Paned is allocated only once both children are visible).
        if became_visible && !self.positioned.get() {
            self.schedule_initial_position();
        }
    }

    /// The divider moved: keep the new height.
    pub(super) fn position_changed(&mut self) {
        if self.adjusting.get() || !self.positioned.get() {
            return;
        }
        // Collapsed = not resizable: a drag on the (invisible) divider
        // snaps straight back to the header-only split. Expanding via
        // the chevron is what re-enables resizing.
        if self.collapsed {
            self.apply_position();
            return;
        }
        let drawer_h = self.paned.height() - self.paned.position();
        if drawer_h >= MIN_HEIGHT {
            self.height = drawer_h;
            self.schedule_height_save();
        }
    }

    /// The grab pill dragged to a new divider position.
    pub(super) fn pill_drag(&mut self, want: i32) {
        self.pill_dragged = true;
        if self.collapsed {
            // A real drag pulls the drawer straight out of the
            // collapse and follows the pointer from the seam; where
            // it is released is the drawer's new height (recorded by
            // position-notify like any other resize).
            self.collapsed = false;
            config::save_drawer_collapsed(self.collapsed);
        }
        // The pill moves the divider live, exactly like a drag on the
        // (invisible) separator. The height is recorded here directly
        // (as well as by position-notify) so a drag is remembered
        // whatever else is in flight when the divider settles.
        self.paned.set_position(want.max(0));
        let drawer_h = self.paned.height() - want.max(0);
        if drawer_h >= MIN_HEIGHT {
            self.height = drawer_h;
            self.schedule_height_save();
        }
    }

    /// The expand or collapse slide finished.
    pub(super) fn toggle_settled(&mut self, collapsed: bool, gen: u64) {
        // A skipped slide's settle arrives late; its work was already
        // done synchronously when it was superseded.
        if gen != self.toggle_gen {
            return;
        }
        // Order matters: flip the state first (the view update right
        // after this hides the body, shrinking the drawer's minimum),
        // THEN allow the end child its minimum again — the other way
        // round the Paned jumps the divider to fit the still-visible
        // body before the settle pulls it back.
        if collapsed {
            self.settle_collapsed();
        }
        self.paned.set_shrink_end_child(false);
        self.adjusting.set(false);
    }
}
