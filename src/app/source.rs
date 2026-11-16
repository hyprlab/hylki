//! View Source (#658): the raw message in the reader that shows it, the
//! main window's or a pop-out's, where Show Message goes back to it; in a
//! window of its own when no reader shows that message.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::i18n;
use crate::models::Message;
use crate::ui::message_view::MessageViewInput;
use crate::ui::message_window::MessageWindowInput;
use crate::ui::source_find::{count_label, source_view, SourceFind};
use crate::worker::MailRequest;

use super::model::AppModel;

/// Where a message's source goes once it is here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum SourceTarget {
    /// The main window's reader.
    Pane,
    /// The pop-out window with this key.
    Popout((u32, u32)),
    /// A Message Source window of its own.
    Window,
}

impl AppModel {
    /// View Source for `m`, or, when the main reader already shows its
    /// source, back to the message: Ctrl+U and the toolbar button toggle.
    pub(super) fn request_source(&mut self, m: &Message) {
        let key = (m.account_id, m.id);
        let target = self.source_target(key);
        if target == SourceTarget::Pane && self.message_view.model().source_key() == Some(key) {
            self.message_view.emit(MessageViewInput::CloseSource);
            return;
        }
        match target {
            SourceTarget::Pane => self.message_view.emit(MessageViewInput::SourceLoading(key)),
            SourceTarget::Popout(p) => {
                if let Some(p) = self.popouts.get(&p) {
                    p.controller.emit(MessageWindowInput::SourceLoading(key));
                }
            }
            SourceTarget::Window => {}
        }
        if let Some(raw) = self.file_raw(m) {
            let text = String::from_utf8_lossy(raw).into_owned();
            self.deliver_source(key, target, Some(text));
            return;
        }
        let Some(path) = self.resolve_folder_path(m) else {
            self.deliver_source(key, target, None);
            return;
        };
        self.sources_wanted.retain(|(k, _)| *k != key);
        self.sources_wanted.push((key, target));
        self.send_to(
            m.account_id,
            MailRequest::LoadSource {
                message_id: m.id,
                path,
                uid: m.uid,
            },
        );
    }

    /// A message's source arrived from its account, `None` when it could
    /// not be loaded.
    pub(super) fn on_source(&mut self, account_id: u32, message_id: u32, text: Option<String>) {
        let key = (account_id, message_id);
        let Some(i) = self.sources_wanted.iter().position(|(k, _)| *k == key) else {
            return;
        };
        let (_, target) = self.sources_wanted.remove(i);
        self.deliver_source(key, target, text);
    }

    /// The reader the action came from, if it shows the message: a pop-out
    /// in front, or the main window's. Neither, and it is a window of its
    /// own: the reading pane is off, or the row asked about is not open.
    fn source_target(&self, key: (u32, u32)) -> SourceTarget {
        let popout = self.popouts.iter().find(|(_, p)| {
            p.window.is_active() && p.members.iter().any(|(_, k)| *k == key)
        });
        if let Some((p, _)) = popout {
            return SourceTarget::Popout(*p);
        }
        let shown = |c: &Message| (c.account_id, c.id) == key;
        if self.reader_shown()
            && (self.current.as_ref().is_some_and(shown) || self.current_thread.iter().any(shown))
        {
            return SourceTarget::Pane;
        }
        SourceTarget::Window
    }

    fn deliver_source(&self, key: (u32, u32), target: SourceTarget, text: Option<String>) {
        match target {
            SourceTarget::Pane => {
                self.message_view.emit(MessageViewInput::ShowSource { key, text });
            }
            SourceTarget::Popout(p) => {
                if let Some(p) = self.popouts.get(&p) {
                    p.controller.emit(MessageWindowInput::ShowSource { key, text });
                }
            }
            SourceTarget::Window => {
                if let Some(text) = text {
                    self.show_source_window(text);
                }
            }
        }
    }

    /// Present a read-only window showing raw message source (monospace).
    fn show_source_window(&self, text: String) {
        let view = source_view();
        let scroller = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hexpand(true)
            .child(&view)
            .build();

        let find = FindBar::new(&view, text, self.reader_find_bottom);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        if self.reader_find_bottom {
            toolbar.add_bottom_bar(&find.revealer);
        } else {
            toolbar.add_top_bar(&find.revealer);
        }
        toolbar.set_content(Some(&scroller));

        let window = adw::Window::builder()
            .transient_for(&self.window)
            .title(i18n("Message Source"))
            .default_width(720)
            .default_height(620)
            .content(&toolbar)
            .build();
        find.attach_keys(&window);
        window.present();
        view.grab_focus();
    }
}

/// The window's find bar, built like the reader's.
#[derive(Clone)]
struct FindBar {
    revealer: gtk::Revealer,
    entry: gtk::SearchEntry,
    count: gtk::Label,
    view: gtk::TextView,
    find: Rc<RefCell<SourceFind>>,
    /// Where the keyboard was when find opened, to hand it back on close.
    focus_back: Rc<RefCell<Option<gtk::glib::WeakRef<gtk::Widget>>>>,
}

impl FindBar {
    fn new(view: &gtk::TextView, text: String, bottom: bool) -> Self {
        let entry = gtk::SearchEntry::builder()
            .hexpand(true)
            .placeholder_text(i18n("Find in message"))
            .build();
        let count = gtk::Label::new(None);
        count.add_css_class("dim-label");
        let button = |icon: &str, tip: String| {
            let b = gtk::Button::from_icon_name(icon);
            b.set_tooltip_text(Some(&tip));
            b.add_css_class("flat");
            b
        };
        let prev = button("pan-up-symbolic", i18n("Previous match"));
        let next = button("pan-down-symbolic", i18n("Next match"));
        let close = button("window-close-symbolic", i18n("Close find"));

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.add_css_class("reader-find");
        if bottom {
            row.add_css_class("bottom");
        }
        row.append(&entry);
        row.append(&count);
        row.append(&prev);
        row.append(&next);
        row.append(&close);

        let revealer = gtk::Revealer::builder()
            .transition_type(if bottom {
                gtk::RevealerTransitionType::SlideUp
            } else {
                gtk::RevealerTransitionType::SlideDown
            })
            .child(&row)
            .build();

        let mut find = SourceFind::default();
        find.load(view, text);
        let bar = Self {
            revealer,
            entry: entry.clone(),
            count,
            view: view.clone(),
            find: Rc::new(RefCell::new(find)),
            focus_back: Rc::default(),
        };

        let b = bar.clone();
        entry.connect_search_changed(move |e| b.search(&e.text()));
        let b = bar.clone();
        entry.connect_activate(move |_| b.step(1));
        // Ctrl+G and Ctrl+Shift+G, which a search entry binds itself.
        let b = bar.clone();
        entry.connect_next_match(move |_| b.step(1));
        let b = bar.clone();
        entry.connect_previous_match(move |_| b.step(-1));
        let b = bar.clone();
        entry.connect_stop_search(move |_| b.close());
        let b = bar.clone();
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(move |_| {
            let b2 = b.clone();
            crate::ui::close_if_focus_left_row(&b.entry, move || b2.close());
        });
        entry.add_controller(focus);
        let b = bar.clone();
        prev.connect_clicked(move |_| b.step(-1));
        let b = bar.clone();
        next.connect_clicked(move |_| b.step(1));
        let b = bar.clone();
        close.connect_clicked(move |_| b.close());
        bar
    }

    /// Ctrl+F toggles the bar and Esc closes it, from anywhere in the window.
    /// Capture phase, so the text view never sees either key first.
    fn attach_keys(&self, window: &adw::Window) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let b = self.clone();
        keys.connect_key_pressed(move |_, key, _, mods| {
            let mods = mods & gtk::accelerator_get_default_mod_mask();
            let find = matches!(key, gtk::gdk::Key::f | gtk::gdk::Key::F)
                && mods == gtk::gdk::ModifierType::CONTROL_MASK;
            if find {
                if b.revealer.reveals_child() {
                    b.close();
                } else {
                    b.open();
                }
                return gtk::glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::Escape && mods.is_empty() && b.revealer.reveals_child() {
                b.close();
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        window.add_controller(keys);
    }

    fn open(&self) {
        let back = self.entry.root().and_then(|r| r.focus());
        *self.focus_back.borrow_mut() = back.map(|w| w.downgrade());
        self.revealer.set_reveal_child(true);
        let entry = self.entry.clone();
        gtk::glib::idle_add_local_once(move || {
            entry.grab_focus();
        });
    }

    fn close(&self) {
        if !self.revealer.reveals_child() {
            return;
        }
        self.revealer.set_reveal_child(false);
        self.entry.set_text("");
        self.search("");
        let back = self.focus_back.borrow_mut().take();
        let in_bar = self
            .entry
            .root()
            .and_then(|r| r.focus())
            .is_some_and(|f| f.is_ancestor(&self.revealer));
        if in_bar {
            match back.and_then(|w| w.upgrade()) {
                Some(w) if w.is_mapped() && !w.is_ancestor(&self.revealer) => {
                    w.grab_focus();
                }
                _ => {
                    self.view.grab_focus();
                }
            }
        }
    }

    fn search(&self, query: &str) {
        let count = self.find.borrow_mut().search(&self.view, query);
        self.count.set_label(&count_label(count));
    }

    fn step(&self, dir: i32) {
        if let Some(count) = self.find.borrow_mut().step(&self.view, dir) {
            self.count.set_label(&count_label(Some(count)));
        }
    }
}
