//! Find in the message (Ctrl+F): the bar and the page's search functions.

use super::types::MessageViewInput;
use super::MessageView;
use adw::prelude::*;
use relm4::prelude::*;

impl MessageView {
    pub(super) fn on_open_find(&mut self, sender: &ComponentSender<Self>) {
        // The toolbar button toggles: a second press collapses the bar.
        if self.find_open {
            sender.input_sender().emit(MessageViewInput::CloseFind);
            return;
        }
        if self
            .find_closed_at
            .take()
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(300))
        {
            return;
        }
        self.find_open = true;
        if let Some(entry) = self.find_entry.clone() {
            self.find_return = entry.root().and_then(|r| r.focus()).map(|w| w.downgrade());
            gtk::glib::idle_add_local_once(move || {
                entry.grab_focus();
            });
        }
    }

    pub(super) fn on_close_find(&mut self) {
        self.find_open = false;
        self.find_closed_at = Some(std::time::Instant::now());
        self.find_matches = None;
        if let Some(entry) = self.find_entry.clone() {
            entry.set_text("");
            self.return_focus_from_find(&entry);
        }
        if self.source_shown() {
            self.source_find_changed("");
        }
        self.eval_find("hylkiFindClear()");
    }

    /// Esc, Ctrl+F or the close button: the keyboard goes back to where it
    /// was before find opened, the message if that is gone. Closing because
    /// focus already moved on leaves it there.
    fn return_focus_from_find(&mut self, entry: &gtk::SearchEntry) {
        let back = self.find_return.take().and_then(|w| w.upgrade());
        let Some(bar) = entry.parent() else { return };
        let in_bar = entry
            .root()
            .and_then(|r| r.focus())
            .is_some_and(|f| f == bar || f.is_ancestor(&bar));
        if !in_bar {
            return;
        }
        let in_body = |w: &gtk::Widget| {
            self.cover
                .as_ref()
                .is_some_and(|c| w == c.upcast_ref::<gtk::Widget>() || w.is_ancestor(c))
        };
        match back {
            Some(w) if w.is_mapped() && !w.is_ancestor(&bar) && !in_body(&w) => {
                w.grab_focus();
            }
            _ => self.on_focus_body(),
        }
    }

    pub(super) fn on_set_find_bottom(&mut self, on: bool) {
        self.find_bottom = on;
        let Some((bar, top_anchor)) = &self.find_bar else {
            return;
        };
        let Some(pane) = bar.parent().and_downcast::<gtk::Box>() else {
            return;
        };
        let after = if on {
            pane.last_child()
        } else {
            Some(top_anchor.clone())
        };
        if after.as_ref() != Some(bar.upcast_ref()) {
            pane.reorder_child_after(bar, after.as_ref());
        }
    }

    pub(super) fn on_find_changed(&mut self, text: String) {
        if self.source_shown() {
            self.source_find_changed(&text);
            return;
        }
        let text = text.trim();
        if text.is_empty() {
            self.find_matches = None;
            self.eval_find("hylkiFindClear()");
            return;
        }
        // Into a JS single-quoted string: backslashes and quotes only.
        let quoted = text.replace('\\', "\\\\").replace('\'', "\\'");
        self.eval_find(&format!("hylkiFind('{quoted}')"));
    }
}

impl MessageView {
    /// Run one of the in-message find engine's calls (#103), logging failures.
    pub(super) fn eval_find(&self, js: &str) {
        self.run_js_or_warn(js, "find js");
    }
}
