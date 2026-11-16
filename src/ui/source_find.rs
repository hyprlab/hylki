//! A message's raw source on a text view, and find in it (#658): for the
//! reader's source mode and the Message Source window both.

use gtk::prelude::*;

use crate::i18n::{i18n, i18n_f};

/// Past this many matches only the current one is marked: a one-letter
/// search through a few megabytes of base64 would otherwise tag hundreds of
/// thousands of ranges and stall the view. The count stays exact.
const MARK_LIMIT: usize = 10_000;

/// A read-only monospace view for a message's source, with the reader's find
/// colors as tags so a match looks the same in both.
pub(crate) fn source_view() -> gtk::TextView {
    let buffer = gtk::TextBuffer::new(None);
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
    view
}

fn rgba(r: u8, g: u8, b: u8, a: f32) -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::new(
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
        a,
    )
}

/// What the find bar's count says for `(current, total)`.
pub(crate) fn count_label(matches: Option<(u32, u32)>) -> String {
    match matches {
        Some((_, 0)) => i18n("No matches"),
        Some((cur, total)) => i18n_f(
            "{cur} of {total}",
            &[("cur", &cur.to_string()), ("total", &total.to_string())],
        ),
        None => String::new(),
    }
}

/// The source on a view and the search through it.
#[derive(Default)]
pub(crate) struct SourceFind {
    text: String,
    matches: Vec<(i32, i32)>,
    current: usize,
}

impl SourceFind {
    /// Put `text` on `view`, forgetting any search before it.
    pub(crate) fn load(&mut self, view: &gtk::TextView, text: String) {
        view.buffer().set_text(&text);
        self.text = text;
        self.matches.clear();
        self.current = 0;
    }

    /// Empty the view, so a source that is no longer shown holds no memory.
    pub(crate) fn unload(&mut self, view: &gtk::TextView) {
        self.load(view, String::new());
    }

    /// Mark every match of `query` and go to the first. The count for the
    /// bar, `(current, total)` with `current` from 1, or `None` for an empty
    /// query.
    pub(crate) fn search(&mut self, view: &gtk::TextView, query: &str) -> Option<(u32, u32)> {
        let query = query.trim();
        let buffer = view.buffer();
        let (start, end) = buffer.bounds();
        buffer.remove_tag_by_name("find-match", &start, &end);
        buffer.remove_tag_by_name("find-current", &start, &end);
        self.matches = find_matches(&self.text, query);
        self.current = 0;
        if query.is_empty() {
            return None;
        }
        for &(at, len) in self.matches.iter().take(MARK_LIMIT) {
            let a = buffer.iter_at_offset(at);
            let b = buffer.iter_at_offset(at + len);
            buffer.apply_tag_by_name("find-match", &a, &b);
        }
        Some(self.show_current(view))
    }

    /// The next match (`dir` 1) or the one before (-1), round the ends.
    pub(crate) fn step(&mut self, view: &gtk::TextView, dir: i32) -> Option<(u32, u32)> {
        let n = self.matches.len();
        if n == 0 {
            return None;
        }
        let buffer = view.buffer();
        let (start, end) = buffer.bounds();
        buffer.remove_tag_by_name("find-current", &start, &end);
        self.current = if dir < 0 {
            (self.current + n - 1) % n
        } else {
            (self.current + 1) % n
        };
        Some(self.show_current(view))
    }

    /// Mark the current match and scroll it into view.
    fn show_current(&self, view: &gtk::TextView) -> (u32, u32) {
        let total = u32::try_from(self.matches.len()).unwrap_or(u32::MAX);
        let Some(&(at, len)) = self.matches.get(self.current) else {
            return (0, 0);
        };
        let buffer = view.buffer();
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
        view.scroll_to_mark(&mark, 0.0, true, 0.0, 0.3);
        let current = u32::try_from(self.current + 1).unwrap_or(u32::MAX);
        (current, total)
    }
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

#[cfg(test)]
mod tests {
    use super::find_matches;

    #[test]
    fn matches_ignore_ascii_case_and_count_characters() {
        let text = "Subject: Café\r\nX-Spam: café CAFÉ cafe";
        // "é" is two bytes but one character: offsets are characters.
        assert_eq!(
            find_matches(text, "caf"),
            vec![(9, 3), (23, 3), (28, 3), (33, 3)]
        );
        assert_eq!(find_matches(text, "café"), vec![(9, 4), (23, 4)]);
        assert!(find_matches(text, "").is_empty());
        assert!(find_matches(text, "nowhere").is_empty());
    }
}
