//! The gallery's text: captions, folder names, column headers and the
//! footer's counts.

use gtk::glib;

use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::models::{GalleryItem, GallerySort};

pub(super) fn caption(item: &GalleryItem) -> String {
    let unknown = i18n("Unknown");
    let who = if item.from_name.trim().is_empty() {
        unknown.as_str()
    } else {
        item.from_name.trim()
    };
    let subject = item.subject.trim();
    let mut parts = vec![who.to_string()];
    if !subject.is_empty() {
        parts.push(subject.to_string());
    }
    parts.push(folder_label(item));
    let date = item.date_label();
    if !date.is_empty() {
        parts.push(date);
    }
    parts.push(item.human_size());
    parts.join(" · ")
}

/// The name of the folder an attachment's message is in: the one the
/// account's listing gives it, as the sidebar shows it. A folder no longer
/// listed shows its whole path, which no rule can split rightly for every
/// kind of account (#554).
pub(super) fn folder_label(item: &GalleryItem) -> String {
    if !item.folder_name.is_empty() {
        item.folder_name.clone()
    } else if item.folder_path.eq_ignore_ascii_case("inbox") {
        i18n("Inbox")
    } else {
        item.folder_path.clone()
    }
}

/// A column header's label, carrying the sort arrow when it is the active
/// column: `asc`/`desc` are the two criteria that column maps to.
pub(super) fn column_header(
    label: &str,
    current: GallerySort,
    asc: GallerySort,
    desc: GallerySort,
) -> String {
    if current == asc {
        format!("{label} \u{2191}")
    } else if current == desc {
        format!("{label} \u{2193}")
    } else {
        label.to_string()
    }
}

/// The footer's network status: a page arriving, or how much of the archive the
/// scan has still to describe. Empty when there is nothing happening.
pub(super) fn scan_text(loading_more: bool, scan_remaining: u32) -> String {
    if scan_remaining > 0 {
        // Deliberately counts messages, not attachments: until a message has
        // been described there is no telling how many files it holds.
        i18n_f(
            "Indexing {n} messages…",
            &[("n", &scan_remaining.to_string())],
        )
    } else if loading_more {
        i18n("Loading more…")
    } else {
        String::new()
    }
}

/// The footer's item count: what's shown of what's there.
pub(super) fn count_text(shown: usize, total: usize) -> String {
    if shown == total {
        ni18n_f(
            "{n} attachment",
            "{n} attachments",
            total as u32,
            &[("n", &total.to_string())],
        )
    } else {
        i18n_f(
            "{shown} of {total}",
            &[("shown", &shown.to_string()), ("total", &total.to_string())],
        )
    }
}

/// "Aug 26, 2026" for a table row, or nothing when the timestamp is unknown.
pub(super) fn date_text(timestamp: i64) -> String {
    glib::DateTime::from_unix_local(timestamp)
        .ok()
        .and_then(|d| d.format("%b %e, %Y").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(
        name: &str,
        from: &str,
        subject: &str,
        folder: &str,
        ts: i64,
        size: u64,
    ) -> GalleryItem {
        GalleryItem {
            account_id: 1,
            downloaded: true,
            folder_path: folder.into(),
            folder_name: match folder {
                "INBOX" => String::new(),
                f => f.rsplit('/').next().unwrap_or(f).to_string(),
            },
            uid: 1,
            name: name.into(),
            size,
            from_name: from.into(),
            subject: subject.into(),
            timestamp: ts,
            data: None,
        }
    }

    #[test]
    fn a_caption_names_sender_subject_folder_and_size() {
        assert_eq!(
            caption(&item("a.pdf", "  ", "  Hi ", "INBOX", 0, 2048)),
            "Unknown · Hi · Inbox · 2.0\u{a0}kB"
        );
        assert_eq!(
            caption(&item("a.pdf", " Ann ", "", "[Gmail]/Sent Mail", -5, 10)),
            format!("Ann · Sent Mail · {}", crate::models::human_size(10))
        );
    }

    #[test]
    fn folder_labels_are_the_listed_names() {
        let label = |path: &str, name: &str| {
            let mut it = item("a", "", "", path, 0, 0);
            it.folder_name = name.into();
            folder_label(&it)
        };
        assert_eq!(label("INBOX", ""), "Inbox");
        assert_eq!(label("inbox", ""), "Inbox");
        assert_eq!(label("Entw&APw-rfe", "Entwürfe"), "Entwürfe");
        // On a '/' server a dot is part of the name.
        assert_eq!(label("Archives/v1.2", "v1.2"), "v1.2");
        // No longer listed: the whole path, not a guess at its last level.
        assert_eq!(label("Archives.2024", ""), "Archives.2024");
    }

    #[test]
    fn the_footer_says_what_the_network_is_doing() {
        assert_eq!(scan_text(false, 0), "");
        assert_eq!(scan_text(true, 0), "Loading more…");
        assert_eq!(scan_text(true, 5), "Indexing 5 messages…");
        assert_eq!(scan_text(false, 1), "Indexing 1 messages…");
    }

    /// The table's date ignores the date-style setting, a known quirk.
    #[test]
    fn table_dates_use_a_fixed_pattern() {
        assert_eq!(date_text(1_787_745_600), "Aug 26, 2026");
    }

    #[test]
    fn column_headers_carry_the_sort_arrow() {
        use super::column_header;
        assert_eq!(
            column_header(
                "Name",
                GallerySort::Name,
                GallerySort::Name,
                GallerySort::NameDesc
            ),
            "Name ↑"
        );
        assert_eq!(
            column_header(
                "Name",
                GallerySort::NameDesc,
                GallerySort::Name,
                GallerySort::NameDesc
            ),
            "Name ↓"
        );
        assert_eq!(
            column_header(
                "Name",
                GallerySort::Newest,
                GallerySort::Name,
                GallerySort::NameDesc
            ),
            "Name"
        );
    }

    #[test]
    fn counts_read_naturally() {
        use super::count_text;
        assert_eq!(count_text(1, 1), "1 attachment");
        assert_eq!(count_text(42, 42), "42 attachments");
        assert_eq!(count_text(12, 42), "12 of 42");
    }
}
