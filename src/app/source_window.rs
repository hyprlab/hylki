//! View Source: the raw message in a window of its own, with Ctrl+F find
//! (#658) that looks and behaves like the reader's.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::i18n::{i18n, i18n_f};

use super::model::AppModel;

/// Past this many matches only the current one is marked: a one-letter
/// search through a few megabytes of base64 would otherwise tag hundreds of
/// thousands of ranges and stall the window. The count stays exact.
const MARK_LIMIT: usize = 10_000;

impl AppModel {
    /// Present a read-only window showing raw message source (monospace).
    pub(super) fn show_source_window(&self, text: &str) {
        let buffer = gtk::TextBuffer::new(None);
        buffer.set_text(text);
        // The reader's find colors, so a match looks the same in both.
        buffer.create_tag(
            Some("find-match"),
            &[
                ("background-rgba", &rgba(255, 198, 0, 0.5)),
                ("foreground-rgba", &rgba(0, 0, 0, 1.0)),
            ],
        );
        buffer.create_tag(
            Some("find-current"),
            &[
                ("background-rgba", &rgba(255, 198, 0, 1.0)),
                ("foreground-rgba", &rgba(0, 0, 0, 1.0)),
            ],
        );
        let view = gtk::TextView::with_buffer(&buffer);
        view.set_editable(false);
        view.set_monospace(true);
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_left_margin(12);
        view.set_right_margin(12);
        view.set_top_margin(8);
        view.set_bottom_margin(8);

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

fn rgba(r: u8, g: u8, b: u8, a: f32) -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::new(f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0, a)
}

/// Where each match of `query` starts in `text`, in characters (what a
/// `TextIter` counts), with its length in characters.
///
/// Case folds for ASCII only, which keeps byte positions identical between
/// the text and its folded copy. A raw message is ASCII almost throughout:
/// headers are encoded words and bodies mostly quoted-printable or base64.
fn find_matches(text: &str, query: &str) -> Vec<(i32, i32)> {
    let needle = query.to_ascii_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let hay = text.to_ascii_lowercase();
    let len = i32::try_from(needle.chars().count()).unwrap_or(i32::MAX);
    let mut out = Vec::new();
    // Walk the text once, counting characters up to each match.
    let (mut byte, mut chars) = (0usize, 0i32);
    for (at, _) in hay.match_indices(needle.as_str()) {
        chars += i32::try_from(hay[byte..at].chars().count()).unwrap_or(i32::MAX);
        byte = at;
        out.push((chars, len));
    }
    out
}

/// The bar's state, shared by its signal handlers.
struct Find {
    view: gtk::TextView,
    text: String,
    matches: Vec<(i32, i32)>,
    current: usize,
    /// Where the keyboard was when find opened, to hand it back on close.
    focus_back: Option<gtk::glib::WeakRef<gtk::Widget>>,
}

#[derive(Clone)]
struct FindBar {
    revealer: gtk::Revealer,
    entry: gtk::SearchEntry,
    count: gtk::Label,
    state: Rc<RefCell<Find>>,
}

impl FindBar {
    fn new(view: &gtk::TextView, text: &str, bottom: bool) -> Self {
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

        let bar = Self {
            revealer,
            entry: entry.clone(),
            count,
            state: Rc::new(RefCell::new(Find {
                view: view.clone(),
                text: text.to_owned(),
                matches: Vec::new(),
                current: 0,
                focus_back: None,
            })),
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
        self.state.borrow_mut().focus_back = back.map(|w| w.downgrade());
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
        // Clearing the entry clears the marks through `search`.
        self.entry.set_text("");
        self.search("");
        let back = self.state.borrow_mut().focus_back.take();
        let view = self.state.borrow().view.clone();
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
                    view.grab_focus();
                }
            }
        }
    }

    fn search(&self, query: &str) {
        let query = query.trim();
        let mut s = self.state.borrow_mut();
        let buffer = s.view.buffer();
        let (start, end) = buffer.bounds();
        buffer.remove_tag_by_name("find-match", &start, &end);
        buffer.remove_tag_by_name("find-current", &start, &end);
        s.matches = find_matches(&s.text, query);
        s.current = 0;
        for &(at, len) in s.matches.iter().take(MARK_LIMIT) {
            let a = buffer.iter_at_offset(at);
            let b = buffer.iter_at_offset(at + len);
            buffer.apply_tag_by_name("find-match", &a, &b);
        }
        drop(s);
        if query.is_empty() {
            self.count.set_label("");
        } else {
            self.show_current();
        }
    }

    fn step(&self, dir: i32) {
        {
            let mut s = self.state.borrow_mut();
            let n = s.matches.len();
            if n == 0 {
                return;
            }
            let buffer = s.view.buffer();
            let (start, end) = buffer.bounds();
            buffer.remove_tag_by_name("find-current", &start, &end);
            s.current = if dir < 0 {
                (s.current + n - 1) % n
            } else {
                (s.current + 1) % n
            };
        }
        self.show_current();
    }

    /// Mark the current match, scroll it into view and update the count.
    fn show_current(&self) {
        let s = self.state.borrow();
        let total = s.matches.len();
        if total == 0 {
            self.count.set_label(&i18n("No matches"));
            return;
        }
        let (at, len) = s.matches[s.current];
        let buffer = s.view.buffer();
        let a = buffer.iter_at_offset(at);
        let b = buffer.iter_at_offset(at + len);
        buffer.apply_tag_by_name("find-current", &a, &b);
        // A mark, not the iter: it scrolls once the lines are measured,
        // which a long source may not be yet.
        let mark = match buffer.mark("find-current") {
            Some(m) => {
                buffer.move_mark(&m, &a);
                m
            }
            None => buffer.create_mark(Some("find-current"), &a, true),
        };
        s.view.scroll_to_mark(&mark, 0.0, true, 0.0, 0.3);
        self.count.set_label(&i18n_f(
            "{cur} of {total}",
            &[
                ("cur", &(s.current + 1).to_string()),
                ("total", &total.to_string()),
            ],
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::find_matches;

    #[test]
    fn matches_ignore_ascii_case_and_count_characters() {
        let text = "Subject: Café\r\nX-Spam: café CAFÉ cafe";
        // "é" is two bytes but one character: offsets are characters.
        assert_eq!(find_matches(text, "caf"), vec![(9, 3), (23, 3), (28, 3), (33, 3)]);
        assert_eq!(find_matches(text, "café"), vec![(9, 4), (23, 4)]);
        assert!(find_matches(text, "").is_empty());
        assert!(find_matches(text, "nowhere").is_empty());
    }
}
