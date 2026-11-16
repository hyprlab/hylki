//! Source mode (#658): the message's raw source in place of its body, with
//! the find bar searching the source, until Show Message goes back.

use super::types::MessageViewInput;
use super::MessageView;
use adw::prelude::*;
use relm4::prelude::*;

/// The source the reader shows, or is waiting for.
pub(super) struct SourceMode {
    /// (account, message) whose source it is.
    pub(super) key: (u32, u32),
    pub(super) loading: bool,
}

impl MessageView {
    /// The page of the body stack the source is on. Esc in it goes back to
    /// the message: the window's own Esc passes a text view by.
    pub(super) fn source_scroller(&self, sender: &ComponentSender<Self>) -> gtk::ScrolledWindow {
        let keys = gtk::EventControllerKey::new();
        let input = sender.input_sender().clone();
        keys.connect_key_pressed(move |_, key, _, mods| {
            let mods = mods & gtk::accelerator_get_default_mod_mask();
            if key == gtk::gdk::Key::Escape && mods.is_empty() {
                input.emit(MessageViewInput::CloseSource);
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        self.source_view.add_controller(keys);
        gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hexpand(true)
            .child(&self.source_view)
            .build()
    }

    /// The message whose source is shown or on its way, if any.
    pub(crate) fn source_key(&self) -> Option<(u32, u32)> {
        self.source.as_ref().map(|s| s.key)
    }

    /// Whether the source is on screen, so find searches it.
    pub(super) fn source_shown(&self) -> bool {
        self.source.as_ref().is_some_and(|s| !s.loading)
    }

    /// The source of `key` was asked for: the spinner until it is here.
    pub(super) fn on_source_loading(&mut self, key: (u32, u32)) {
        if self.find_open {
            self.on_close_find();
        }
        self.source_find.unload(&self.source_view);
        self.source = Some(SourceMode { key, loading: true });
    }

    /// The source arrived. One for a message no longer waited on (another
    /// message opened since) is dropped; `None`, it could not be loaded,
    /// and the message comes back.
    pub(super) fn on_show_source(&mut self, key: (u32, u32), text: Option<String>) {
        if self.source_key() != Some(key) {
            return;
        }
        let Some(text) = text else {
            self.on_close_source();
            return;
        };
        self.source_find.load(&self.source_view, text);
        self.source = Some(SourceMode { key, loading: false });
        // After the stack has switched to it: a hidden page takes no focus.
        let view = self.source_view.clone();
        gtk::glib::idle_add_local_once(move || {
            if view.is_mapped() {
                view.grab_focus();
            }
        });
    }

    /// Back to the message.
    pub(super) fn on_close_source(&mut self) {
        if self.source.is_none() {
            return;
        }
        if self.find_open {
            self.on_close_find();
        }
        let had_focus = self
            .source_view
            .root()
            .and_then(|r| r.focus())
            .is_some_and(|f| f == *self.source_view.upcast_ref::<gtk::Widget>());
        self.source = None;
        self.source_find.unload(&self.source_view);
        if had_focus {
            let webview = self.webview.clone();
            gtk::glib::idle_add_local_once(move || {
                webview.grab_focus();
            });
        }
    }

    /// Find in the source: the count the bar shows comes back at once.
    pub(super) fn source_find_changed(&mut self, text: &str) {
        self.find_matches = self.source_find.search(&self.source_view, text);
    }

    pub(super) fn source_find_step(&mut self, dir: i32) {
        if let Some(m) = self.source_find.step(&self.source_view, dir) {
            self.find_matches = Some(m);
        }
    }
}
