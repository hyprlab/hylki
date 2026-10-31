//! The split reply: a composer under the reader on a Paned.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::ui::compose::{Compose, ComposeInput, ComposePrefill};
use crate::ui::message_view::MessageViewInput;

use super::compose_host::ReaderCompose;
use super::model::AppModel;

/// The split reply's own height on its Paned: the divider position above
/// the reader, and what the position leaves of the pane beneath it, where
/// the position is the reader's height (#212).
pub(super) fn split_reply_height(split: &gtk::Paned, bottom: bool) -> i32 {
    if bottom {
        split.height() - split.position()
    } else {
        split.position()
    }
}

/// Let (or forbid) the split reply's child of `split` shrink below the
/// composer's minimum — needed for the slide, which runs the panel through
/// heights the composer could not otherwise be allocated.
pub(super) fn set_split_shrink(split: &gtk::Paned, bottom: bool, shrink: bool) {
    if bottom {
        split.set_shrink_end_child(shrink);
    } else {
        split.set_shrink_start_child(shrink);
    }
}

impl AppModel {
    /// Jumps a running slide of the split reply panel to its end.
    pub(super) fn settle_split_anim(&self) {
        // Taken out of the cell before it is skipped: `if let` holds the
        // temporary `borrow_mut` guard alive for its whole body, and
        // skipping emits `done`, whose handler reaches for this same cell.
        let prev_anim = self.split_close_anim.borrow_mut().take();
        if let Some(prev) = prev_anim {
            prev.skip();
        }
    }

    /// Put the reader's header bar back without its slide: the send morph's
    /// stage is drawing the pane meanwhile, and needs the header where it
    /// ends up, now.
    pub(super) fn reveal_reader_header_at_once(&self) {
        self.message_view
            .emit(MessageViewInput::SetUnderSplit(false));
        let Some(header) = self.reader_header.get() else {
            return;
        };
        header.set_opacity(1.0);
        let tv = header
            .ancestor(adw::ToolbarView::static_type())
            .and_downcast::<adw::ToolbarView>();
        if let Some(tv) = tv {
            if !tv.reveals_top_bars() {
                // libadwaita skips the slide of an unmapped toolbar view and
                // simply shows the bar; hidden and shown again in one turn,
                // nothing of it reaches the screen.
                tv.set_visible(false);
                tv.set_reveal_top_bars(true);
                tv.set_visible(true);
            }
        }
    }

    /// Slide the reader's header bar in or out of its toolbar view, fading
    /// its icons with it. The toolbar view animates the bar's height (its
    /// reveal-top-bars transition), so the content below slides rather than
    /// jumping by the bar's height; the opacity animation keeps the icons
    /// from appearing at full strength on the first frame of the slide.
    /// Timed to match the split reply's own 300ms slide, which it accompanies.
    pub(super) fn show_reader_header(&self, shown: bool) {
        // The header bar goes only while a split reply sits above the
        // reader; the subject block under it wants more air then.
        self.message_view
            .emit(MessageViewInput::SetUnderSplit(!shown));
        let Some(header) = self.reader_header.get() else {
            return;
        };
        let tv = header
            .ancestor(adw::ToolbarView::static_type())
            .and_downcast::<adw::ToolbarView>();
        if let Some(tv) = tv {
            if tv.reveals_top_bars() == shown {
                return;
            }
            tv.set_reveal_top_bars(shown);
        }
        let from = header.opacity();
        let to = if shown { 1.0 } else { 0.0 };
        let target = adw::CallbackAnimationTarget::new({
            let header = header.clone();
            move |v| header.set_opacity(v)
        });
        let anim = adw::TimedAnimation::new(header, from, to, 300, target);
        anim.set_easing(adw::Easing::EaseOutCubic);
        anim.play();
    }

    /// The split reply's grab handle: the shared grab pill, floated at the
    /// panel's edge that meets the reader and bounded like the panel's
    /// opening height — a usable panel, a visible reader. Above the reader
    /// that edge is the panel's bottom; beneath it (#212), its top, where the
    /// divider position is the reader's height rather than the panel's.
    fn build_split_grab_pill(&self, bottom: bool) -> gtk::Box {
        let paned = if bottom {
            &self.reader_split_lower
        } else {
            &self.reader_split
        };
        let pill = crate::ui::grab_pill::paned_grab_pill(paned, move |split, want| {
            if bottom {
                want.clamp(200, (split.height() - 220).max(200))
            } else {
                want.clamp(220, (split.height() - 200).max(220))
            }
        });
        pill.set_valign(if bottom {
            gtk::Align::Start
        } else {
            gtk::Align::End
        });
        // Bias the centred bar 6px away from the editor inside its hit zone
        // (a margin shifts the centring by half its size): more air between
        // the editor card and the bar, matching the attachment drawer's
        // spacing.
        if let Some(bar) = pill.first_child() {
            if bottom {
                bar.set_margin_bottom(12);
            } else {
                bar.set_margin_top(12);
            }
        }
        pill
    }

    /// The split-reply slot in use, with the Paned that sizes it and whether
    /// it is the one beneath the reader; `None` while no split reply is open.
    pub(super) fn live_split(&self) -> Option<(gtk::Revealer, gtk::Paned, bool)> {
        if self.reader_split_top.child().is_some() {
            Some((
                self.reader_split_top.clone(),
                self.reader_split.clone(),
                false,
            ))
        } else if self.reader_split_bottom.child().is_some() {
            Some((
                self.reader_split_bottom.clone(),
                self.reader_split_lower.clone(),
                true,
            ))
        } else {
            None
        }
    }

    /// Whether the next split reply opens beneath the reader (#212): by the
    /// setting outright, or following the reading order — beneath when the
    /// conversation reads downward to its newest message, above when the
    /// newest is at the top.
    fn split_reply_at_bottom(&self) -> bool {
        match self.reply_position {
            config::ReplyPosition::Top => false,
            config::ReplyPosition::Bottom => true,
            config::ReplyPosition::Follow => !self.thread_newest_first,
        }
    }

    /// Where a reply or a forward opens: the reading pane, beside the
    /// message it answers, or a window of its own when Settings says so
    /// (#295).
    pub(super) fn open_reply(
        &mut self,
        account_id: u32,
        prefill: ComposePrefill,
        answering: (u32, u32),
        sender: &ComponentSender<Self>,
    ) {
        if self.reply_inline {
            self.open_inline_reply(account_id, prefill, Some(answering), sender);
        } else {
            self.open_compose(account_id, prefill, sender);
        }
    }

    /// Open (or replace) the reader's inline reply/forward drop-down pane.
    pub(super) fn open_inline_reply(
        &mut self,
        account_id: u32,
        prefill: ComposePrefill,
        focus: Option<(u32, u32)>,
        sender: &ComponentSender<Self>,
    ) {
        // No reading pane to open in, or one too short below the list to
        // write in (#475): a window of its own.
        if self.reading_pane != config::ReadingPane::Right {
            self.open_compose(account_id, prefill, sender);
            return;
        }
        let contextual = focus.is_some();
        // A sent reply still settling into the conversation lands at once:
        // the new composer opens over the pane as it really is.
        self.send_morph.finish();
        // Supersede any composer already in the reader slot first.
        self.release_reader_compose();
        let (id, mut init) = self.build_compose_init(account_id, prefill, false, true);
        // The split reply is compact: just the editor, fields behind pop-out.
        init.compact = contextual;
        // Beneath the reader (#212) the reader's header stays, so the
        // composer's must not carry a second set of window controls.
        let bottom = contextual && !self.showing_contacts && self.split_reply_at_bottom();
        init.decorations = !bottom;
        let controller = self.spawn_compose(init, sender);
        let widget = controller.widget();
        widget.set_hexpand(true);
        widget.add_css_class("inline-compose-surface");
        if contextual && !self.showing_contacts {
            self.open_split_panel(&controller, bottom, focus);
        } else {
            self.open_covering_composer(&controller);
        }
        self.reader_compose = Some(ReaderCompose {
            id,
            controller,
            window: None,
            answering: focus,
        });
        // A fresh composer has nothing to take back yet; the last one's
        // labels must not carry over into its menu.
        self.compose_history = (None, None);
        self.refresh_undo_menu();
    }

    /// The split reply (#86): the composer slides into the reader's Paned,
    /// above the reader or beneath it (`bottom`), and the card being
    /// answered (`focus`) goes to the top of what is left of the reader.
    fn open_split_panel(
        &mut self,
        controller: &Controller<Compose>,
        bottom: bool,
        focus: Option<(u32, u32)>,
    ) {
        // A reply/forward splits the pane instead of covering it (#86):
        // the composer slides down from the top and the message(s) stay
        // below, visible and interactive to refer to while writing. The
        // composer fills whatever the Paned divider grants it, and the
        // height it sets is final: pasting a novel into the body cannot
        // push the panel down.
        //
        // The grab handle is a floating pill at the panel's bottom edge
        // (the iOS home-indicator look) — the wrapper Overlay floats it
        // over the composer, and dragging it moves the Paned divider,
        // whose own separator is painted invisible.
        // A still-running close animation would fight the new panel for
        // the divider: jump it to its end (its teardown sees the slot's
        // new occupant and stands down).
        self.settle_split_anim();
        // Above the reader, the reader's own header would show a second
        // set of window decorations mid-window (the composer header at
        // the pane's top already carries them) and spend vertical space
        // the split needs: gone while the split hosts, back when it
        // goes. What remains of the reader starts at the remote-content
        // banner, or the subject block when there is none. Beneath the
        // reader the header is nowhere near the panel and stays.
        self.show_reader_header(bottom);
        let (slot, split) = if bottom {
            (&self.reader_split_bottom, self.reader_split_lower.clone())
        } else {
            (&self.reader_split_top, self.reader_split.clone())
        };
        let widget = controller.widget();
        widget.set_vexpand(true);
        let wrap = gtk::Overlay::new();
        wrap.set_child(Some(widget));
        wrap.add_overlay(&self.build_split_grab_pill(bottom));
        slot.set_child(Some(&wrap));
        slot.set_visible(true);
        slot.set_reveal_child(true);
        self.slide_split_open(&split, bottom, controller.sender().clone());
        if let Some((fa, fid)) = focus {
            // Put the card being answered at the top of the shortened
            // reader, wearing the selection outline.
            self.message_view.emit(MessageViewInput::FocusCard {
                account_id: fa,
                id: fid,
            });
        }
    }

    /// Slide the split's divider from nothing to the panel's opening height
    /// (the saved one, kept clear of the reader), then give the composer
    /// its focus.
    fn slide_split_open(
        &self,
        split: &gtk::Paned,
        bottom: bool,
        compose: relm4::Sender<ComposeInput>,
    ) {
        let split = split.clone();
        let pane_h = split.height();
        let pane_h = if pane_h > 0 { pane_h } else { 900 };
        let saved = config::load_split_reply_height();
        let h = if saved > 0 {
            saved
        } else {
            ((pane_h as f64 * 0.45) as i32).clamp(300, 560)
        };
        // Keep a usable slice of reader on screen whatever was saved —
        // and slide the panel in the way it slides out: the divider IS
        // the animation, driven from zero to the opening height (the
        // revealer's own transition can't run — it starts unmapped, and
        // adw skips animations on unmapped widgets — so the divider
        // does all the moving). Beneath the reader the divider position
        // is the reader's height, so the same slide runs from the pane's
        // full height down to what is left once the panel has its share.
        let target = h.clamp(220, (pane_h - 200).max(220));
        let (from, to) = if bottom {
            (pane_h as f64, (pane_h - target) as f64)
        } else {
            (0.0, target as f64)
        };
        set_split_shrink(&split, bottom, true);
        split.set_position(from as i32);
        let anim = adw::TimedAnimation::new(
            &split,
            from,
            to,
            300,
            adw::CallbackAnimationTarget::new({
                let split = split.clone();
                move |v| split.set_position(v as i32)
            }),
        );
        anim.set_easing(adw::Easing::EaseOutCubic);
        let cell = self.split_close_anim.clone();
        // Focus waits for the slide: the pane opens from nothing, and a
        // Paned child with no height is hidden, so focus given to it at
        // once fell to the Paned and the reply opened with no cursor in
        // it (#266).
        anim.connect_done({
            let cell = cell.clone();
            move |_| {
                cell.borrow_mut().take();
                set_split_shrink(&split, bottom, false);
                compose.emit(ComposeInput::FocusInitial);
            }
        });
        *cell.borrow_mut() = Some(anim.clone());
        anim.play();
    }

    /// A composer that covers the whole reader pane, sliding down over it.
    fn open_covering_composer(&self, controller: &Controller<Compose>) {
        // Composing clears the reader toolbar too (decorations excepted);
        // the ⋯ overflow is managed by hand, so hide it by hand.
        self.reader_overflow_btn.set_visible(false);
        // Fill the pane and paint an opaque surface: the composer covers
        // the pane completely, not a partial panel.
        let widget = controller.widget();
        widget.set_vexpand(true);
        let slot = self.compose_slot();
        slot.set_child(Some(widget));
        slot.set_can_target(true);
        // Revealed a frame later: starting the transition in the same
        // cycle the child was set finds it unmapped, and the slide is
        // skipped — the composer pops in instead of sliding down.
        let s = slot.clone();
        gtk::glib::idle_add_local_once(move || s.set_reveal_child(true));
        controller.emit(ComposeInput::FocusInitial);
    }
}
