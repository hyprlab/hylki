//! Scheduled housekeeping shared by the IMAP, Graph and JMAP paths: which
//! folders the auto-empty (#140) and list clean-up (#407) touch, when they
//! are due, and their cut-off dates.

use std::time::Duration;

use crate::config::AccountConfig;
use crate::models::{Folder, FolderKind};

/// How often an account's Junk / Trash auto-empty (#140) may run: it rides
/// the unread sweep, but a server needs asking only a few times a day.
const AUTO_EMPTY_MIN: Duration = Duration::from_secs(6 * 60 * 60);

/// Auto-empty (#140): the roles the account wants emptied, with their ages.
pub(super) fn auto_empty_roles(account: &AccountConfig) -> Vec<(FolderKind, &'static str, u32)> {
    [
        (FolderKind::Junk, "junk", account.empty_junk_days),
        (FolderKind::Trash, "trash", account.empty_trash_days),
    ]
    .into_iter()
    .filter(|(_, _, days)| *days > 0)
    .collect()
}

/// The folder holding a role for this account: the manual assignment (#82)
/// when there is one, else the folder the listing detected.
pub(super) fn role_folder_path(
    account: &AccountConfig,
    folders: &[Folder],
    kind: FolderKind,
    role: &str,
) -> Option<String> {
    if let Some(p) = account.folder_roles.get(role) {
        if !p.is_empty() {
            return Some(p.clone());
        }
    }
    folders
        .iter()
        .find(|f| f.kind == kind)
        .map(|f| f.path.clone())
}

/// Whether the auto-empty is due: something is configured, and the last run
/// on this connection is older than [`AUTO_EMPTY_MIN`] (or never happened).
pub(super) fn auto_empty_due(
    account: &AccountConfig,
    last: &mut Option<std::time::Instant>,
) -> bool {
    if auto_empty_roles(account).is_empty() {
        return false;
    }
    if last.is_some_and(|t| t.elapsed() < AUTO_EMPTY_MIN) {
        return false;
    }
    *last = Some(std::time::Instant::now());
    true
}

/// The cut-off, `days` ago, in the form IMAP SEARCH wants ("1-Aug-2026").
/// SEARCH BEFORE compares internal dates by day, in the server's clock.
pub(super) fn imap_before_date(days: u32) -> String {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(days));
    cutoff.format("%-d-%b-%Y").to_string()
}

/// The same cut-off as an RFC 3339 instant for Graph's `$filter`.
pub(super) fn graph_before_date(days: u32) -> String {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(days));
    cutoff.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Whether moving old list mail (#407) is due, on the same terms as
/// [`auto_empty_due`]: set for the account, and not run in the last
/// [`AUTO_EMPTY_MIN`] on this connection.
pub(super) fn clean_lists_due(
    account: &AccountConfig,
    last: &mut Option<std::time::Instant>,
) -> bool {
    if account.clean_lists_days == 0 || last.is_some_and(|t| t.elapsed() < AUTO_EMPTY_MIN) {
        return false;
    }
    *last = Some(std::time::Instant::now());
    true
}

/// The candidates for the list clean-up (#407), as an IMAP search: older
/// than the cut-off and not starred. Which of them are list mail is read
/// from their headers: a `HEADER List-Unsubscribe ""` search would be one
/// round trip, but not every server answers it (Stalwart searches only the
/// headers it indexes, and finds nothing).
pub(super) fn clean_lists_query(days: u32) -> String {
    format!("UNFLAGGED BEFORE {}", imap_before_date(days))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::worker::test_support::sample_account;

    /// Auto-empty (#140): only the roles with an age are swept.
    #[test]
    fn auto_empty_roles_follow_the_ages() {
        let mut acc = sample_account();
        assert!(auto_empty_roles(&acc).is_empty());
        acc.empty_trash_days = 14;
        let roles = auto_empty_roles(&acc);
        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0].0, FolderKind::Trash);
        assert_eq!(roles[0].2, 14);
        acc.empty_junk_days = 7;
        assert_eq!(auto_empty_roles(&acc).len(), 2);
    }

    /// The manual role assignment (#82) names the folder first; otherwise
    /// the detected kind does.
    #[test]
    fn auto_empty_role_folder_prefers_the_assignment() {
        let mut acc = sample_account();
        let folders = vec![Folder {
            id: 7,
            account_id: 1,
            name: "Deleted Messages".into(),
            path: "Deleted Messages".into(),
            kind: FolderKind::Trash,
            unread: 0,
            childless: false,
        }];
        assert_eq!(
            role_folder_path(&acc, &folders, FolderKind::Trash, "trash").as_deref(),
            Some("Deleted Messages")
        );
        assert_eq!(
            role_folder_path(&acc, &folders, FolderKind::Junk, "junk"),
            None
        );
        acc.folder_roles.insert("trash".into(), "INBOX.Bin".into());
        assert_eq!(
            role_folder_path(&acc, &folders, FolderKind::Trash, "trash").as_deref(),
            Some("INBOX.Bin")
        );
    }

    /// The sweep runs once, then not again until the interval has passed.
    #[test]
    fn auto_empty_is_throttled() {
        let mut acc = sample_account();
        let mut last = None;
        assert!(!auto_empty_due(&acc, &mut last), "nothing configured");
        assert!(last.is_none());
        acc.empty_junk_days = 7;
        assert!(auto_empty_due(&acc, &mut last));
        assert!(!auto_empty_due(&acc, &mut last), "just ran");
        last = Some(std::time::Instant::now() - AUTO_EMPTY_MIN - Duration::from_secs(1));
        assert!(auto_empty_due(&acc, &mut last));
    }

    /// Moving list mail (#407) runs on the same terms: set, and not twice
    /// within the interval. Its search leaves starred mail out.
    #[test]
    fn list_clean_up_is_throttled_and_spares_starred_mail() {
        let mut acc = sample_account();
        let mut last = None;
        assert!(!clean_lists_due(&acc, &mut last), "not set");
        acc.clean_lists_days = 90;
        assert!(clean_lists_due(&acc, &mut last));
        assert!(!clean_lists_due(&acc, &mut last), "just ran");
        let q = clean_lists_query(90);
        assert!(q.starts_with("UNFLAGGED BEFORE "), "{q}");
    }

    /// IMAP wants "1-Aug-2026" (no zero padding); Graph an RFC 3339 instant.
    #[test]
    fn auto_empty_cutoffs_are_well_formed() {
        let imap = imap_before_date(0);
        let today = chrono::Utc::now().format("%-d-%b-%Y").to_string();
        assert_eq!(imap, today);
        assert!(!imap.starts_with('0'), "{imap}");
        let parts: Vec<&str> = imap.split('-').collect();
        assert_eq!(parts.len(), 3, "{imap}");
        assert_eq!(parts[1].len(), 3, "{imap}");
        let graph = graph_before_date(30);
        assert!(
            chrono::DateTime::parse_from_rfc3339(&graph).is_ok(),
            "{graph}"
        );
        assert!(graph < chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());
    }
}
