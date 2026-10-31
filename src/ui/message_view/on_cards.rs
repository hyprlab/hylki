//! The cards of a conversation: folding, selection, keyboard stepping,
//! focus, scroll places and live patches.

use super::snippets::tag_chips_html;
use super::types::{CardAttachment, MessageViewOutput, SelectMode};
use super::MessageView;
use adw::prelude::*;
use relm4::prelude::*;
use webkit6::prelude::WebViewExt;

impl MessageView {
    pub(super) fn on_folded(&mut self, account_id: u32, id: u32, folded: bool) {
        self.folds.insert((account_id, id), folded);
    }

    pub(super) fn on_fold_all(&mut self, folded: bool) {
        if !self.carded() {
            return;
        }
        for m in &self.thread {
            self.folds.insert((m.account_id, m.id), folded);
        }
        self.patch_folds();
    }

    pub(super) fn on_set_card_attachments(
        &mut self,
        map: std::collections::HashMap<(u32, u32), Vec<CardAttachment>>,
    ) {
        self.card_atts = map;
        let keys: Vec<(u32, u32)> = self.thread.iter().map(|m| (m.account_id, m.id)).collect();
        self.patch_card_atts(&keys);
    }

    pub(super) fn on_scroll_to_attachments(&mut self, account_id: u32, id: u32) {
        // The row when it has chips, else the card itself; centred,
        // with the row flashed so the eye lands on it.
        let js = format!(
            "(function(){{\
             var d=document.querySelector('.hylki-atts[data-key=\"{account_id}:{id}\"]');\
             var s=document.querySelector('.hylki-msg[data-key=\"{account_id}:{id}\"]');\
             var t=(d&&d.children.length)?d:s;if(!t)return;\
             t.scrollIntoView({{behavior:'smooth',block:'center'}});\
             if(d){{d.classList.remove('flash');void d.offsetWidth;d.classList.add('flash');\
             setTimeout(function(){{d.classList.remove('flash');}},1300);}}}})()"
        );
        self.run_js(&js);
    }

    pub(super) fn on_attachment_action(
        &mut self,
        account_id: u32,
        id: u32,
        index: usize,
        save: bool,
        sender: &ComponentSender<Self>,
    ) {
        let _ = sender.output(MessageViewOutput::AttachmentAction {
            account_id,
            id,
            index,
            save,
        });
    }

    pub(super) fn on_suppress_auto_read(&mut self, account_id: u32, id: u32) {
        self.no_autoread.insert((account_id, id));
        for m in self.thread.iter_mut() {
            if m.account_id == account_id && m.id == id {
                m.unread = true;
            }
        }
        // Put the card's dot back in place — reloading the document
        // for one mark made every card visibly resettle (the same
        // blip ClearDot avoids in the other direction).
        let js = format!(
            "(function(){{\
             var s=document.querySelector('.hylki-msg[data-key=\"{account_id}:{id}\"]');\
             if(s)s.classList.add('unread');\
             var h=document.querySelector('.hylki-msg-hdr[data-key=\"{account_id}:{id}\"]');\
             if(!h||h.querySelector('.hylki-dot'))return;\
             var d=document.createElement('span');d.className='hylki-dot';\
             d.setAttribute('data-key','{account_id}:{id}');\
             var f=h.querySelector('.hylki-from');\
             if(f)f.parentNode.insertBefore(d,f);else h.appendChild(d);}})()"
        );
        self.run_js_or_warn(&js, "unread-dot patch");
    }

    pub(super) fn on_clear_cards(&mut self, sender: &ComponentSender<Self>) {
        if !self.selected_cards.is_empty() {
            self.selected_cards.clear();
            self.apply_card_selection();
            let _ = sender.output(MessageViewOutput::SelectCards(Vec::new()));
        }
    }

    pub(super) fn on_card_clicked(
        &mut self,
        account_id: u32,
        id: u32,
        mode: SelectMode,
        sender: &ComponentSender<Self>,
    ) {
        if !self.carded() {
            return;
        }
        let key = (account_id, id);
        let order: Vec<(u32, u32)> = self.thread.iter().map(|m| (m.account_id, m.id)).collect();
        let mut keys = match mode {
            SelectMode::Plain => vec![key],
            SelectMode::Toggle => {
                let mut k = self.selected_cards.clone();
                if let Some(pos) = k.iter().position(|x| *x == key) {
                    k.remove(pos);
                } else {
                    k.push(key);
                }
                k
            }
            SelectMode::Range => {
                // From the first thing already selected to this one, in
                // the order the conversation is shown.
                let anchor = self
                    .selected_cards
                    .first()
                    .and_then(|a| order.iter().position(|x| x == a))
                    .unwrap_or_else(|| order.iter().position(|x| *x == key).unwrap_or(0));
                let here = order.iter().position(|x| *x == key).unwrap_or(anchor);
                let (lo, hi) = (anchor.min(here), anchor.max(here));
                order[lo..=hi].to_vec()
            }
        };
        keys.retain(|k| order.contains(k));
        self.selected_cards = keys.clone();
        self.apply_card_selection();
        let _ = sender.output(MessageViewOutput::SelectCards(keys));
    }

    pub(super) fn on_copy_selection(&mut self) {
        // Copying from a body frame focuses the frame and then the
        // page, and a page focusing itself takes GTK focus: Ctrl+C
        // pressed in the message list left the keyboard in the
        // reader (#274). The script has run by the time its result
        // comes back, so hand focus back to where it was then.
        let before = self.webview.root().and_then(|r| r.focus());
        let webview = self.webview.clone();
        self.webview.evaluate_javascript(
            "window.__hylkiCopySel && window.__hylkiCopySel()",
            None,
            None,
            gtk::gio::Cancellable::NONE,
            move |_| {
                let Some(before) = before else { return };
                let now = webview.root().and_then(|r| r.focus());
                let taken = now.is_some_and(|f| {
                    f == *webview.upcast_ref::<gtk::Widget>() || f.is_ancestor(&webview)
                });
                if taken && before != *webview.upcast_ref::<gtk::Widget>() {
                    before.grab_focus();
                }
            },
        );
    }

    pub(super) fn on_select_all_cards(&mut self, sender: &ComponentSender<Self>) {
        if !self.carded() {
            return;
        }
        let keys: Vec<(u32, u32)> = self.thread.iter().map(|m| (m.account_id, m.id)).collect();
        if self.selected_cards != keys {
            self.selected_cards = keys.clone();
            self.apply_card_selection();
            let _ = sender.output(MessageViewOutput::SelectCards(keys));
        }
    }

    pub(super) fn on_step_card(&mut self, delta: i32, sender: &ComponentSender<Self>) {
        // A lone message has nothing to step through: the app moves
        // along the list instead.
        if self.thread.len() <= 1 {
            return;
        }
        let order: Vec<(u32, u32)> = self.thread.iter().map(|m| (m.account_id, m.id)).collect();
        let from = match self.selected_cards.as_slice() {
            [key] => order.iter().position(|k| k == key),
            _ => None,
        };
        let last = order.len() as i64 - 1;
        let next = match from {
            Some(i) => (i as i64 + i64::from(delta)).clamp(0, last) as usize,
            // No card picked yet: the first press lands where reading
            // starts, the first message unread or open, not one past
            // the conversation's first (#392).
            None => {
                let folded = self.folded_cards();
                self.thread
                    .iter()
                    .position(|m| m.unread || !folded.contains(&(m.account_id, m.id)))
                    .unwrap_or(if delta > 0 { 0 } else { last as usize })
            }
        };
        let key = order[next];
        if self.folded_cards().contains(&key) {
            self.folds.insert(key, false);
            self.patch_folds();
        }
        self.selected_cards = vec![key];
        self.apply_card_selection();
        let _ = sender.output(MessageViewOutput::SelectCards(vec![key]));
        let js = format!(
            "(function(){{var s=document.querySelector('.hylki-msg[data-key=\"{}:{}\"]');\
             if(s)s.scrollIntoView({{behavior:'smooth',block:'start'}});}})()",
            key.0, key.1
        );
        self.run_js(&js);
    }

    pub(super) fn on_focus_body(&mut self) {
        if self.current.is_some() {
            match &self.loading_page {
                Some(page) if self.body_page() == "loading" => {
                    page.grab_focus();
                    self.focus_held.set(true);
                }
                _ => {
                    self.webview.grab_focus();
                }
            }
        }
    }

    pub(super) fn on_set_selected_cards(&mut self, keys: Vec<(u32, u32)>) {
        // The list mirrors every selection change here — including the
        // plain single "this row is open" state that comes with merely
        // opening a message or thread, which must not outline anything.
        // A lone mirrored key is therefore dropped: the accent border
        // appears only for a deliberate selection — a card header
        // clicked in the reader (CardClicked, which never routes
        // through here), or a multi-selection made in the list.
        let keys = if keys.len() == 1 { Vec::new() } else { keys };
        if self.selected_cards != keys {
            self.selected_cards = keys;
            self.apply_card_selection();
        }
    }

    pub(super) fn on_frame_sized(&mut self, account_id: u32, id: u32, height: u32) {
        // A few hundred numbers at most, and only for messages actually
        // opened; the store is trimmed rather than allowed to creep.
        if self.frame_heights.len() > 512 {
            self.frame_heights.clear();
        }
        self.frame_heights.insert((account_id, id), height);
    }

    pub(super) fn on_mark_seen(
        &mut self,
        account_id: u32,
        id: u32,
        sender: &ComponentSender<Self>,
    ) {
        // A deliberate unread mark stands until the conversation is
        // reopened — the viewport observer must not undo it.
        if self.no_autoread.contains(&(account_id, id)) {
            return;
        }
        // A card read while open stays open: folding the read ones
        // is for when the conversation is drawn, not under the
        // reader's eyes the next time the folds are patched.
        let key = (account_id, id);
        if !self.folded_cards().contains(&key) {
            self.folds.entry(key).or_insert(false);
        }
        // Keep the local copy in step so a later re-render doesn't put
        // the mark back on a message already read.
        let mut found = false;
        for m in self.thread.iter_mut() {
            if m.account_id == account_id && m.id == id && m.unread {
                m.unread = false;
                found = true;
            }
        }
        if found {
            let _ = sender.output(MessageViewOutput::MarkSeen { account_id, id });
        }
    }

    pub(super) fn on_clear_dot(&mut self, account_id: u32, id: u32) {
        for m in self.thread.iter_mut() {
            if m.account_id == account_id && m.id == id {
                m.unread = false;
            }
        }
        let js = format!(
            "(function(){{\
             var d=document.querySelector('.hylki-dot[data-key=\"{account_id}:{id}\"]');\
             if(d)d.remove();\
             var s=document.querySelector('.hylki-msg[data-key=\"{account_id}:{id}\"]');\
             if(s)s.classList.remove('unread');}})()"
        );
        self.run_js_or_warn(&js, "clear-dot patch");
    }

    pub(super) fn on_set_card_keywords(
        &mut self,
        account_id: u32,
        id: u32,
        keywords: Vec<String>,
        labels: Vec<String>,
    ) {
        for m in self.thread.iter_mut() {
            if m.account_id == account_id && m.id == id {
                m.keywords = keywords.clone();
                m.labels = labels.clone();
            }
        }
        if let Some(c) = self.current.as_mut() {
            if c.account_id == account_id && c.id == id {
                c.keywords = keywords.clone();
                c.labels = labels.clone();
            }
        }
        let chips = tag_chips_html(&self.tags, &keywords, &labels);
        let js = format!(
            "(function(){{\
             var t=document.querySelector('.hylki-tags[data-key=\"{account_id}:{id}\"]');\
             if(t)t.innerHTML={chips};}})()",
            chips = serde_json::to_string(&chips).unwrap_or_else(|_| "''".into()),
        );
        self.run_js_or_warn(&js, "card tag patch");
    }

    pub(super) fn on_set_card_star(&mut self, account_id: u32, id: u32, starred: bool) {
        for m in self.thread.iter_mut() {
            if m.account_id == account_id && m.id == id {
                m.starred = starred;
            }
        }
        let js = format!(
            "(function(){{\
             var b=document.querySelector('.hylki-act[data-act=\"star\"][data-key=\"{account_id}:{id}\"]');\
             if(b)b.classList.{0}('on');\
             var s=document.querySelector('.hylki-msg[data-key=\"{account_id}:{id}\"]');\
             if(s)s.classList.{0}('starred');}})()",
            if starred { "add" } else { "remove" },
        );
        self.run_js_or_warn(&js, "card star patch");
    }

    pub(super) fn on_focus_card(&mut self, account_id: u32, id: u32) {
        let webview = self.webview.clone();
        let gen = self.focus_gen.clone();
        gen.set(gen.get() + 1);
        let my_gen = gen.get();
        // Let the split's 300ms slide finish first — scrolling while
        // the viewport is still shrinking lands somewhere else.
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(380), move || {
            if gen.get() != my_gen {
                // A BlurCard beat us here: the reply was cancelled
                // before the slide finished. Adding the outline
                // now would strand it.
                return;
            }
            let js = format!(
                    "(function(){{\
                     if(!document.body.classList.contains('hylki-conv'))return;\
                     var s=document.querySelector('.hylki-msg[data-key=\"{account_id}:{id}\"]');\
                     if(!s)return;\
                     document.querySelectorAll('.hylki-msg.selected').forEach(function(e){{e.classList.remove('selected');}});\
                     s.classList.add('selected');\
                     s.style.scrollMarginTop='14px';\
                     s.scrollIntoView({{behavior:'smooth',block:'start'}});}})()"
                );
            webview.evaluate_javascript(&js, None, None, None::<&gtk::gio::Cancellable>, |r| {
                if let Err(e) = r {
                    tracing::warn!("focus-card scroll failed: {e}");
                }
            });
        });
    }

    pub(super) fn on_blur_card(&mut self) {
        self.focus_gen.set(self.focus_gen.get() + 1);
        self.webview.evaluate_javascript(
            "(function(){document.querySelectorAll('.hylki-msg.selected')\
             .forEach(function(e){e.classList.remove('selected');});})()",
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            |r| {
                if let Err(e) = r {
                    tracing::warn!("card blur failed: {e}");
                }
            },
        );
        // Cards the list still has selected keep their outline.
        self.apply_card_selection();
    }

    pub(super) fn on_scroll_anchor(&mut self, account_id: u32, id: u32, offset: u32) {
        self.saved_anchor = Some((account_id, id, offset));
        self.anchor_gutter = false;
    }

    pub(super) fn on_pin_arrival(&mut self, account_id: u32, id: u32) {
        self.saved_anchor = Some((account_id, id, 0));
        self.anchor_gutter = true;
        // The anchor rides only on a document that keeps its place.
        self.did_autoscroll = true;
        self.skip_fade = true;
    }

    pub(super) fn on_hold_place(&mut self, account_id: u32, id: u32) {
        if self.saved_anchor.is_none() {
            self.saved_anchor = Some((account_id, id, 0));
            self.anchor_gutter = true;
        }
    }
}
