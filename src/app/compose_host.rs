//! Where composers live: their own windows, or the reader's split pane.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::ui::compose::{Compose, ComposeInput, ComposePrefill};
use crate::ui::message_list::MessageListInput;
use crate::ui::message_view::MessageViewInput;

use super::model::AppModel;
use super::msg::AppMsg;
use super::split_reply::{set_split_shrink, split_reply_height};
use super::AppModelWidgets;

/// A standalone compose window (New Message, compose-to, edit-draft, or a
/// popped-out reply) and its component. Both refs must stay alive: the window
/// holds the content, the controller holds the component root.
pub(super) struct ComposeHost {
    pub(super) id: u32,
    /// Held only to keep the component (and its widget tree) alive for the
    /// window's lifetime; dropped when the host is removed.
    #[allow(dead_code)]
    pub(super) controller: Controller<Compose>,
    pub(super) window: adw::Window,
}

/// The reader's inline reply/forward composer. `window` is `Some` only while the
/// pane has been promoted to a floating window (else it lives in the reader's
/// drop-down revealer).
pub(super) struct ReaderCompose {
    pub(super) id: u32,
    pub(super) controller: Controller<Compose>,
    pub(super) window: Option<adw::Window>,
    /// The message a split reply answers, by (account, id): its card in the
    /// reader is the one the reply was opened from.
    pub(super) answering: Option<(u32, u32)>,
    /// A draft opened by selecting its row, which the list keeps selected.
    pub(super) list_draft: bool,
}

impl AppModel {
    /// A new message with these recipients, from the account being looked at.
    pub(super) fn compose_addressed(
        &mut self,
        prefill: ComposePrefill,
        sender: &ComponentSender<Self>,
    ) {
        // The contacts view hosts its own compose slot (the composer
        // slides down over the contact card); from the gallery there is
        // no slot, so bring the mail panes back first — the composer
        // would otherwise open invisibly behind the stack.
        self.leave_gallery();
        let account = self
            .current
            .as_ref()
            .map(|m| m.account_id)
            .unwrap_or_else(|| self.active_account());
        let (account, prefill) = self.new_message_from(account, prefill);
        // Same preference as "New Message": slide down over the
        // reader, unless composing is set to open in a window.
        if self.compose_inline {
            self.open_inline_reply(account, prefill, None, sender);
        } else {
            self.open_compose(account, prefill, sender);
        }
    }

    /// Open a standalone compose window (New Message, compose-to, edit-draft).
    pub(super) fn open_compose(
        &mut self,
        account_id: u32,
        prefill: ComposePrefill,
        sender: &ComponentSender<Self>,
    ) {
        let (id, init) = self.build_compose_init(account_id, prefill, true, false);
        let controller = self.spawn_compose(init, sender);
        let window = self.compose_window_host(controller.widget(), id, sender);
        controller.emit(ComposeInput::FocusInitial);
        self.composers.push(ComposeHost {
            id,
            controller,
            window,
        });
    }

    /// The revealer the inline composer should slide down in: the reader
    /// pane's, or — while the contacts view is up — the contact card's.
    pub(super) fn compose_slot(&self) -> &gtk::Revealer {
        if self.showing_contacts {
            &self.contacts_compose_revealer
        } else {
            &self.reader_compose_revealer
        }
    }

    /// Hide and empty every inline-composer slot (only one ever holds it).
    pub(super) fn clear_compose_slots(&self) {
        for r in [
            &self.reader_compose_revealer,
            &self.contacts_compose_revealer,
        ] {
            r.set_reveal_child(false);
            // Without this the hidden, pane-covering revealer keeps
            // swallowing every click and scroll over the pane beneath.
            r.set_can_target(false);
            r.set_child(None::<&gtk::Widget>);
        }
        // In-flow (not overlay) slot: no click-swallowing to disarm. Settle
        // any running slide, then remember the height the user dragged the
        // panel to before it goes.
        self.settle_split_anim();
        if let Some((slot, split, bottom)) = self.live_split() {
            if slot.is_visible() {
                config::save_split_reply_height(split_reply_height(&split, bottom));
            }
        }
        // The reader gets its header back with the pane.
        self.show_reader_header(true);
        for slot in [&self.reader_split_top, &self.reader_split_bottom] {
            slot.set_reveal_child(false);
            // The composer sits inside the grab-pill Overlay wrapper:
            // unparent it from there explicitly, or hosting it elsewhere
            // (pop-out, drain) would find it still parented.
            if let Some(wrap) = slot.child().and_downcast::<gtk::Overlay>() {
                wrap.set_child(None::<&gtk::Widget>);
            }
            slot.set_child(None::<&gtk::Widget>);
            // Hiding the slot hides the Paned divider with it.
            slot.set_visible(false);
        }
        // The split's reply-target outline goes with the composer.
        self.message_view.emit(MessageViewInput::BlurCard);
    }

    /// Slide the covering composer away instead of blinking it out: the
    /// revealer plays its slide-up, and the child is removed only once the
    /// transition has had its time — removing it at once (what
    /// `clear_compose_slots` does) skips the slide entirely. The removal
    /// stands down if a new composer has taken the slot meanwhile. Pop-out
    /// reparenting keeps using the instant clear: it needs the widget freed
    /// immediately.
    fn animate_cover_close(&self) {
        for r in [
            &self.reader_compose_revealer,
            &self.contacts_compose_revealer,
        ] {
            let Some(child) = r.child() else { continue };
            r.set_can_target(false);
            if !r.reveals_child() {
                r.set_child(None::<&gtk::Widget>);
                continue;
            }
            r.set_reveal_child(false);
            let r2 = r.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(340), move || {
                if r2.child().as_ref() == Some(&child) && !r2.reveals_child() {
                    r2.set_child(None::<&gtk::Widget>);
                }
            });
        }
        self.message_view.emit(MessageViewInput::BlurCard);
    }

    /// Slide the split reply up and out the way it slid in, then tear the
    /// slot down. The Paned allocates the slot by divider position — not by
    /// the revealer's animated height — so the position is animated to zero
    /// in step with the revealer's own slide-up, with the slot briefly
    /// allowed to shrink below the composer's minimum. The teardown at the
    /// end is skipped if a new reply has taken the slot meanwhile.
    fn animate_split_close(&self) {
        let Some((slot, split, bottom)) = self.live_split() else {
            return;
        };
        let Some(wrap) = slot.child() else { return };
        // Settle any running slide first, so the height saved below is the
        // real one and not a mid-animation reading.
        self.settle_split_anim();
        if slot.is_visible() {
            config::save_split_reply_height(split_reply_height(&split, bottom));
        }
        // The reply-target outline goes with the composer, as on an
        // instant teardown.
        self.message_view.emit(MessageViewInput::BlurCard);

        set_split_shrink(&split, bottom, true);
        slot.set_reveal_child(false);
        // The reader's header comes back in step with the panel's exit: it
        // slides down while the panel slides up, its icons fading in, so the
        // reader below moves in one motion rather than jumping up as the
        // panel goes and back down as the header pops in after it. (Beneath
        // the reader the header never left; this is a no-op there.)
        self.show_reader_header(true);
        let from = split.position() as f64;
        // Out the way it came in: to nothing above the reader, to the
        // pane's full height beneath it.
        let to = if bottom { split.height() as f64 } else { 0.0 };
        let target = adw::CallbackAnimationTarget::new({
            let split = split.clone();
            move |v| split.set_position(v as i32)
        });
        let anim = adw::TimedAnimation::new(&split, from, to, 300, target);
        anim.set_easing(adw::Easing::EaseOutCubic);
        let cell = self.split_close_anim.clone();
        anim.connect_done(move |_| {
            cell.borrow_mut().take();
            if slot.child().as_ref() == Some(&wrap) {
                if let Some(w) = wrap.downcast_ref::<gtk::Overlay>() {
                    w.set_child(None::<&gtk::Widget>);
                }
                slot.set_child(None::<&gtk::Widget>);
                slot.set_visible(false);
            }
            // Only once the slot is empty (or a new panel owns it): with the
            // composer still in the slot, forbidding shrink first would
            // re-clamp the divider to the composer's minimum for a frame —
            // the bounce at the end of the slide.
            set_split_shrink(&split, bottom, false);
        });
        *self.split_close_anim.borrow_mut() = Some(anim.clone());
        anim.play();
    }

    /// Detach the reader's inline composer from the reader slot. If it was popped
    /// out to a window it lives on independently; if inline, ask it to save-if-
    /// dirty and let it drain closed.
    pub(super) fn release_reader_compose(&mut self) {
        let Some(r) = self.reader_compose.take() else {
            return;
        };
        self.compose_history = (None, None);
        self.refresh_undo_menu();
        match r.window {
            Some(window) => {
                self.composers.push(ComposeHost {
                    id: r.id,
                    controller: r.controller,
                    window,
                });
            }
            None => {
                // Either kind of inline composer slides out the way it
                // slid in.
                if self.live_split().is_some() {
                    self.animate_split_close();
                } else {
                    self.animate_cover_close();
                }
                self.reader_overflow_btn
                    .set_visible(self.reader_overflow_wanted());
                r.controller.emit(ComposeInput::SaveDraftIfDirty);
                self.draining_composers.push((r.id, r.controller));
            }
        }
    }

    /// Promote the reader's inline pane to a window, or collapse a window back
    /// inline — reparenting the live pane so the editor state survives the move.
    fn toggle_compose_window(&mut self, id: u32, sender: &ComponentSender<Self>) {
        let Some(mut r) = self.reader_compose.take() else {
            return;
        };
        if r.id != id {
            self.reader_compose = Some(r);
            return;
        }
        let widget = r.controller.widget().clone();
        match r.window.take() {
            None => {
                // inline → window: unparent from the revealer, then host in a window.
                self.clear_compose_slots();
                self.reader_overflow_btn
                    .set_visible(self.reader_overflow_wanted());
                let window = self.compose_window_host(&widget, id, sender);
                r.window = Some(window);
                r.controller.emit(ComposeInput::SetWindowed(true));
            }
            Some(window) => {
                // window → inline: unparent from the window, drop it back in place.
                window.set_content(None::<&gtk::Widget>);
                window.destroy();
                widget.set_vexpand(true);
                widget.set_hexpand(true);
                widget.add_css_class("inline-compose-surface");
                self.reader_overflow_btn.set_visible(false);
                let slot = self.compose_slot();
                slot.set_child(Some(&widget));
                slot.set_can_target(true);
                let s = slot.clone();
                gtk::glib::idle_add_local_once(move || s.set_reveal_child(true));
                // Back inline it covers the reader, header and all, so it
                // carries the decorations whatever slot it left from.
                r.controller.emit(ComposeInput::SetDecorations(true));
                r.controller.emit(ComposeInput::SetWindowed(false));
            }
        }
        r.controller.emit(ComposeInput::FocusInitial);
        self.reader_compose = Some(r);
    }

    /// Tear down a composer by id (from a Close output or a window's close-request).
    pub(super) fn close_compose(&mut self, id: u32) {
        tracing::debug!(target: "hylki::compose", "composer {id} closed");
        if let Some(pos) = self.composers.iter().position(|h| h.id == id) {
            let host = self.composers.remove(pos);
            host.window.set_content(None::<&gtk::Widget>);
            host.window.destroy();
            return;
        }
        if let Some(r) = self.reader_compose.take_if(|r| r.id == id) {
            match r.window {
                Some(window) => {
                    window.set_content(None::<&gtk::Widget>);
                    window.destroy();
                }
                None => {
                    // A draft opened from its row closed on its own (saved,
                    // sent, cancelled): the row lets go too, or it stays lit
                    // over a reader that says no draft is selected, and a
                    // click on it, being no change, opens nothing. Saving
                    // gives the draft a new row anyway.
                    if r.list_draft {
                        self.message_list.emit(MessageListInput::ClearSelection);
                    }
                    // A sent split reply turns into its card; cancel, and
                    // a send that cannot, slide it out rather than blink it.
                    match self.arriving.take().filter(|(cid, _, _)| *cid == id) {
                        Some((_, m, animated)) => {
                            if !animated {
                                tracing::info!(target: "hylki::morph", "no send morph: animations are off");
                            }
                            if !(animated
                                && self.begin_send_morph(
                                    m.clone(),
                                    r.controller.widget().upcast_ref(),
                                ))
                            {
                                // No morph, but the reply still never
                                // leaves the pane: the composer goes at once
                                // and the card is in the conversation.
                                let conversation =
                                    self.current.as_ref().map(|c| (c.account_id, c.id));
                                self.reveal_reader_header_at_once();
                                self.clear_compose_slots();
                                if let Some(conversation) = conversation {
                                    self.join_conversation(m, conversation, true);
                                }
                            }
                        }
                        None if self.live_split().is_some() => self.animate_split_close(),
                        None => self.animate_cover_close(),
                    }
                    self.reader_overflow_btn
                        .set_visible(self.reader_overflow_wanted());
                }
            }
            return;
        }
        self.draining_composers.retain(|(cid, _)| *cid != id);
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_compose_host(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::ComposeClosed(id) => {
                self.close_compose(id);
                self.message_list.emit(MessageListInput::ReclaimFocus);
            }

            AppMsg::ComposeCloseRequested(id) => {
                let controller = self
                    .composers
                    .iter()
                    .find(|h| h.id == id)
                    .map(|h| &h.controller)
                    .or(self
                        .reader_compose
                        .as_ref()
                        .filter(|r| r.id == id)
                        .map(|r| &r.controller));
                match controller {
                    Some(c) => c.emit(ComposeInput::Cancel),
                    None => self.close_compose(id),
                }
            }

            AppMsg::ComposeToggleWindow(id) => self.toggle_compose_window(id, &sender),
            _ => unreachable!("not routed to on_compose_host"),
        }
    }

    /// The inline compose/reply pane is an overlay over the WHOLE reader
    /// pane — its header bar included: revealed, it slides down from the
    /// top and covers everything (the composer's own header carries the
    /// window decorations meanwhile). While closed it must not eat the
    /// reader's clicks. Wrapping happens here because the view macro's
    /// deep nesting makes an in-place Overlay unwieldy.
    pub(super) fn init_compose_overlay(&self, widgets: &AppModelWidgets) {
        self.reader_compose_revealer.set_can_target(false);
        {
            let pane = widgets
                .reader_bin
                .child()
                .expect("the view puts the reader pane in reader_bin");
            widgets.reader_bin.set_child(None::<&gtk::Widget>);
            // Split-reply slot (#86) above the reader; the full-cover
            // revealer stays an overlay above the whole assembly. A Paned
            // rather than a Box so the composer's height is the divider
            // position — dragged by the user, immune to the editor's natural
            // height — instead of whatever the composer asks for.
            pane.set_vexpand(true);
            // The reader over the bottom slot (#212), the pair beneath the
            // top slot. Each slot is hidden while no split reply is open in
            // it, which hides its divider too.
            let lower = &self.reader_split_lower;
            lower.set_start_child(Some(&pane));
            lower.set_end_child(Some(&self.reader_split_bottom));
            lower.set_resize_start_child(true);
            lower.set_shrink_start_child(true);
            lower.set_resize_end_child(false);
            lower.set_shrink_end_child(false);
            lower.set_vexpand(true);
            self.reader_split_bottom.set_visible(false);
            let split = &self.reader_split;
            split.set_start_child(Some(&self.reader_split_top));
            split.set_end_child(Some(lower));
            // Window resizes go to the reader; the composer keeps its set
            // height and never shrinks below its minimum. The reader may
            // shrink — the divider clamp at open time is what keeps a slice
            // of it on screen.
            split.set_resize_start_child(false);
            split.set_shrink_start_child(false);
            split.set_resize_end_child(true);
            split.set_shrink_end_child(true);
            self.reader_split_top.set_visible(false);
            let overlay = gtk::Overlay::new();
            overlay.set_child(Some(split));
            overlay.add_overlay(&self.reader_compose_revealer);
            // Over everything: the send morph's stage covers the whole pane.
            overlay.add_overlay(&self.send_morph);
            widgets.reader_bin.set_child(Some(&overlay));
        }
    }
}
