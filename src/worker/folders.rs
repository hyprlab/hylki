//! Folder roles, names and order, shared by every backend. Folder ids are
//! positional (1..n in [`folder_order`] then by lowercased path), so the
//! order here is persisted.

use async_imap::types::NameAttribute;

use crate::i18n::i18n;
use crate::models::{Folder, FolderKind, Message};
use crate::worker::WorkerEvent;

/// Whether a folder's sidebar chip counts every message rather than unread
/// ones. Drafts: a draft is never unread, and the chip is there to say how
/// many are waiting to be finished.
pub(crate) fn chip_counts_all(kind: FolderKind) -> bool {
    kind == FolderKind::Drafts
}

/// A [`WorkerEvent::SnoozedCount`] for each Snoozed folder of a listing
/// whose entries carry a total (#542).
pub(crate) fn snoozed_counts<T>(list: &[T], of: impl Fn(&T) -> (&Folder, u32)) -> Vec<WorkerEvent> {
    list.iter()
        .map(of)
        .filter(|(f, _)| f.kind == FolderKind::Snoozed)
        .map(|(f, count)| WorkerEvent::SnoozedCount {
            path: f.path.clone(),
            count,
        })
        .collect()
}

/// The chip number for a folder whose messages are all in hand.
pub(super) fn chip_count_of(kind: FolderKind, messages: &[Message]) -> u32 {
    if chip_counts_all(kind) {
        messages.len() as u32
    } else {
        messages.iter().filter(|m| m.unread).count() as u32
    }
}

/// Classify a folder, also reporting whether the kind came from an RFC 6154
/// SPECIAL-USE attribute (`true`) rather than name matching (`false`). This lets
/// `list_folders` prefer the real special-use folder when a server also exposes a
/// stray folder that merely *looks* like it fills the same role — e.g. Gmail's
/// real `[Gmail]/Trash` (\Trash) next to a plain top-level `Trash` label.
pub(super) fn classify_with_source(
    path: &str,
    delimiter: Option<&str>,
    attrs: &[NameAttribute],
) -> (FolderKind, bool) {
    // Prefer RFC 6154 SPECIAL-USE attributes; fall back to name matching.
    for a in attrs {
        match a {
            NameAttribute::Sent => return (FolderKind::Sent, true),
            NameAttribute::Drafts => return (FolderKind::Drafts, true),
            NameAttribute::Trash => return (FolderKind::Trash, true),
            NameAttribute::Junk => return (FolderKind::Junk, true),
            NameAttribute::Archive => return (FolderKind::Archive, true),
            NameAttribute::Flagged => return (FolderKind::Starred, true),
            _ => {}
        }
    }

    // INBOX is the one name RFC 3501 fixes, and only as the whole path: an
    // `Archiv/2020/Inbox` is an ordinary folder (#344).
    if path.eq_ignore_ascii_case("INBOX") {
        return (FolderKind::Inbox, false);
    }
    // The other roles go by name only where a server keeps them: at the top,
    // or one level down under INBOX (Dovecot, Courier) or a bracketed
    // container ([Gmail]). A `Sent` deeper in an archive is the user's own.
    let parts: Vec<&str> = match delimiter {
        Some(d) if !d.is_empty() => path.split(d).collect(),
        _ => path.split(['/', '.']).collect(),
    };
    let placed = match parts.as_slice() {
        [_] => true,
        [parent, _] => parent.eq_ignore_ascii_case("INBOX") || parent.starts_with('['),
        _ => false,
    };
    if !placed {
        return (FolderKind::Custom, false);
    }
    let leaf = parts.last().copied().unwrap_or(path).to_lowercase();
    let kind = match leaf.as_str() {
        "sent" | "sent items" | "sent mail" => FolderKind::Sent,
        "drafts" => FolderKind::Drafts,
        name if crate::models::is_templates_name(name) => FolderKind::Templates,
        name if crate::models::is_snoozed_name(name) => FolderKind::Snoozed,
        "trash" | "deleted" | "deleted items" | "bin" => FolderKind::Trash,
        "junk" | "spam" => FolderKind::Junk,
        "archive" | "all mail" => FolderKind::Archive,
        "starred" | "flagged" => FolderKind::Starred,
        _ => FolderKind::Custom,
    };
    (kind, false)
}

pub(crate) fn folder_order(kind: FolderKind) -> u8 {
    match kind {
        FolderKind::Inbox => 0,
        FolderKind::Starred => 1,
        FolderKind::Drafts => 2,
        FolderKind::Templates => 3,
        FolderKind::Sent => 4,
        FolderKind::Archive => 5,
        FolderKind::Junk => 6,
        FolderKind::Trash => 7,
        // Last of the special folders: one made on the first snooze then
        // renumbers only the custom folders, as any new folder does.
        FolderKind::Snoozed => 8,
        FolderKind::Custom => 9,
    }
}

/// Show only the leaf segment of a hierarchical mailbox path, with INBOX
/// special-cased.
pub(super) fn display_name(path: &str, delimiter: Option<&str>) -> String {
    if path.eq_ignore_ascii_case("inbox") {
        return i18n("Inbox");
    }
    let leaf = match delimiter {
        Some(d) if !d.is_empty() => path.rsplit(d).next().unwrap_or(path),
        _ => path.rsplit(['/', '.']).next().unwrap_or(path),
    };
    // Split first, decode second: the delimiter is ASCII and modified UTF-7
    // never produces one inside an escaped run (that is why `,` stands in for
    // `/`), so the leaf is always a whole name (issue #1).
    crate::mail::mutf7::decode(leaf)
}

/// Put a folder listing in the order Hylki keeps and number it 1..n:
/// the essential folders first in [`folder_order`], the custom ones by
/// their full path (case-insensitively), so sub-folders sit under their
/// parents whatever order the server listed them in. Folder ids are
/// positional and stored, so this order must not change.
pub(super) fn number_folders<T>(
    items: &mut [T],
    folder: impl Fn(&T) -> &Folder,
    folder_mut: impl Fn(&mut T) -> &mut Folder,
) {
    items.sort_by(|a, b| {
        let (a, b) = (folder(a), folder(b));
        folder_order(a.kind)
            .cmp(&folder_order(b.kind))
            .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
    });
    for (i, f) in items.iter_mut().enumerate() {
        folder_mut(f).id = i as u32 + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::worker::imap::watch::watchable;

    fn folder(path: &str, kind: FolderKind) -> Folder {
        Folder {
            id: 0,
            account_id: 1,
            name: path.to_string(),
            path: path.to_string(),
            kind,
            unread: 0,
        }
    }

    /// The ids are stored: roles first in their fixed order, then the
    /// rest by lowercased path, numbered from 1.
    #[test]
    fn folders_are_numbered_roles_first_then_by_path() {
        let mut list = vec![
            folder("Work/b", FolderKind::Custom),
            folder("Trash", FolderKind::Trash),
            folder("archive2", FolderKind::Custom),
            folder("INBOX", FolderKind::Inbox),
            folder("Work", FolderKind::Custom),
            folder("Sent", FolderKind::Sent),
            folder("Drafts", FolderKind::Drafts),
            folder("Work/A", FolderKind::Custom),
        ];
        number_folders(&mut list, |f| f, |f| f);
        let got: Vec<(u32, &str)> = list.iter().map(|f| (f.id, f.path.as_str())).collect();
        assert_eq!(
            got,
            [
                (1, "INBOX"),
                (2, "Drafts"),
                (3, "Sent"),
                (4, "Trash"),
                (5, "archive2"),
                (6, "Work"),
                (7, "Work/A"),
                (8, "Work/b"),
            ]
        );
    }

    #[test]
    fn only_the_top_level_inbox_is_the_inbox() {
        // #344: an archive keeps per-year Inbox and Sent folders.
        let kind = |p: &str, d: &str| classify_with_source(p, Some(d), &[]).0;
        assert_eq!(kind("INBOX", "/"), FolderKind::Inbox);
        assert_eq!(kind("Inbox", "/"), FolderKind::Inbox);
        assert_eq!(kind("Archiv/2020/Inbox", "/"), FolderKind::Custom);
        assert_eq!(kind("Archiv/2020/Sent", "/"), FolderKind::Custom);
        assert_eq!(kind("INBOX.Archiv.Inbox", "."), FolderKind::Custom);
        // Where servers keep their roles, names still count.
        assert_eq!(kind("Sent", "/"), FolderKind::Sent);
        assert_eq!(kind("INBOX.Sent", "."), FolderKind::Sent);
        assert_eq!(kind("INBOX.Trash", "."), FolderKind::Trash);
        assert_eq!(kind("[Gmail]/Spam", "/"), FolderKind::Junk);
        assert_eq!(kind("Projects/Drafts", "/"), FolderKind::Custom);
        // A dot in a name is not a level when the server's delimiter is /.
        assert_eq!(kind("v1.0/Sent", "/"), FolderKind::Custom);
    }

    #[test]
    fn a_templates_folder_is_found_by_name() {
        // #360: no SPECIAL-USE attribute marks one.
        let kind = |p: &str, d: &str| classify_with_source(p, Some(d), &[]).0;
        assert_eq!(kind("Templates", "/"), FolderKind::Templates);
        assert_eq!(kind("INBOX.Templates", "."), FolderKind::Templates);
        assert_eq!(kind("template", "/"), FolderKind::Templates);
        assert_eq!(kind("Projects/Templates", "/"), FolderKind::Custom);
        // Listed right after Drafts, ahead of Sent.
        assert!(folder_order(FolderKind::Drafts) < folder_order(FolderKind::Templates));
        assert!(folder_order(FolderKind::Templates) < folder_order(FolderKind::Sent));
        assert!(folder_order(FolderKind::Templates) < folder_order(FolderKind::Custom));
        // Nothing waits on it, and no conversation reaches into it.
        assert!(!watchable(FolderKind::Templates));
        assert!(!chip_counts_all(FolderKind::Templates));
    }

    #[test]
    fn a_snoozed_folder_is_found_by_name() {
        // #527: made by the first snooze, found again by its name.
        let kind = |p: &str, d: &str| classify_with_source(p, Some(d), &[]).0;
        assert_eq!(kind("Snoozed", "/"), FolderKind::Snoozed);
        assert_eq!(kind("INBOX.Snoozed", "."), FolderKind::Snoozed);
        assert_eq!(kind("Projects/Snoozed", "/"), FolderKind::Custom);
        assert!(folder_order(FolderKind::Trash) < folder_order(FolderKind::Snoozed));
        assert!(folder_order(FolderKind::Snoozed) < folder_order(FolderKind::Custom));
        assert!(!watchable(FolderKind::Snoozed));
        assert!(!chip_counts_all(FolderKind::Snoozed));
    }

    #[test]
    fn only_snoozed_folders_report_what_they_hold() {
        // #542: the unified Snoozed row follows these, so a listing's other
        // folders send none.
        let folder = |path: &str, kind| Folder {
            id: 1,
            account_id: 1,
            name: path.into(),
            path: path.into(),
            kind,
            unread: 0,
        };
        let list = vec![
            (folder("INBOX", FolderKind::Inbox), 40),
            (folder("Snoozed", FolderKind::Snoozed), 3),
        ];
        let events = snoozed_counts(&list, |(f, n)| (f, *n));
        assert!(matches!(
            events.as_slice(),
            [WorkerEvent::SnoozedCount { path, count: 3 }] if path == "Snoozed"
        ));
    }

    #[test]
    fn folder_names_are_decoded_for_display() {
        // Issue #1: Gmail's Chinese labels arrived as modified UTF-7 and were
        // shown verbatim in the sidebar.
        assert_eq!(display_name("&XfJSoGYfaAc-", Some("/")), "已加星标");
        // Only the leaf is shown, and the parent may be encoded too.
        assert_eq!(display_name("&U,BTFw-/&g0l6Pw-", Some("/")), "草稿");
        assert_eq!(display_name("INBOX.&U,BTFw-", Some(".")), "台北");
        // Ordinary names are untouched, and INBOX keeps its special casing.
        assert_eq!(display_name("INBOX", Some("/")), "Inbox");
        assert_eq!(display_name("[Gmail]/All Mail", Some("/")), "All Mail");
        assert_eq!(display_name("Tom & Jerry", Some("/")), "Tom & Jerry");
    }

    #[test]
    fn special_use_attributes_win_over_names_and_placement() {
        let cases = [
            (NameAttribute::Sent, FolderKind::Sent),
            (NameAttribute::Drafts, FolderKind::Drafts),
            (NameAttribute::Trash, FolderKind::Trash),
            (NameAttribute::Junk, FolderKind::Junk),
            (NameAttribute::Archive, FolderKind::Archive),
            (NameAttribute::Flagged, FolderKind::Starred),
        ];
        for (attr, kind) in cases {
            // Even deep in a hierarchy and under a name that says otherwise.
            assert_eq!(
                classify_with_source("a/b/c/Inbox", Some("/"), &[attr]),
                (kind, true)
            );
        }
        // `\All` and the other attributes are not mapped: the name decides.
        assert_eq!(
            classify_with_source("[Gmail]/All Mail", Some("/"), &[NameAttribute::All]),
            (FolderKind::Archive, false)
        );
        assert_eq!(
            classify_with_source("Lists", Some("/"), &[NameAttribute::All]),
            (FolderKind::Custom, false)
        );
        // The first mapped attribute decides.
        assert_eq!(
            classify_with_source(
                "X",
                Some("/"),
                &[
                    NameAttribute::Marked,
                    NameAttribute::Junk,
                    NameAttribute::Sent
                ]
            ),
            (FolderKind::Junk, true)
        );
    }

    #[test]
    fn folder_names_map_to_roles_where_servers_keep_them() {
        let kind = |p: &str, d: Option<&str>| classify_with_source(p, d, &[]).0;
        for (name, want) in [
            ("Sent Items", FolderKind::Sent),
            ("sent mail", FolderKind::Sent),
            ("Deleted", FolderKind::Trash),
            ("Deleted Items", FolderKind::Trash),
            ("Bin", FolderKind::Trash),
            ("Spam", FolderKind::Junk),
            ("All Mail", FolderKind::Archive),
            ("Archive", FolderKind::Archive),
            ("Flagged", FolderKind::Starred),
            ("Starred", FolderKind::Starred),
            ("Outbox", FolderKind::Custom),
        ] {
            assert_eq!(kind(name, Some("/")), want, "{name}");
        }
        // No delimiter (or an empty one): both `/` and `.` split levels.
        assert_eq!(kind("INBOX.Sent", None), FolderKind::Sent);
        assert_eq!(kind("INBOX/Sent", Some("")), FolderKind::Sent);
        assert_eq!(kind("a.b.Sent", None), FolderKind::Custom);
        // A bracketed container, not only Gmail's, places a role.
        assert_eq!(kind("[Mail]/Trash", Some("/")), FolderKind::Trash);
        assert_eq!(kind("Mail/Trash", Some("/")), FolderKind::Custom);
    }

    #[test]
    fn folder_order_is_the_persisted_numbering_order() {
        let order = [
            FolderKind::Inbox,
            FolderKind::Starred,
            FolderKind::Drafts,
            FolderKind::Templates,
            FolderKind::Sent,
            FolderKind::Archive,
            FolderKind::Junk,
            FolderKind::Trash,
            FolderKind::Snoozed,
            FolderKind::Custom,
        ];
        for (i, kind) in order.into_iter().enumerate() {
            assert_eq!(folder_order(kind), i as u8);
        }
    }

    #[test]
    fn display_name_without_a_delimiter_splits_on_slash_and_dot() {
        assert_eq!(display_name("inbox", None), "Inbox");
        assert_eq!(display_name("a.b/c", None), "c");
        assert_eq!(display_name("a.b", Some("/")), "a.b");
        assert_eq!(display_name("a/b.c", Some("")), "c");
    }
}
