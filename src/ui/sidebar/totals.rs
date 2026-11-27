//! The message totals beside the unread chips (#720): which rows carry
//! one, and what a unified header's adds up.

use super::types::{InboxRef, UnifiedRow};
use super::widgets::add_total_column;
use super::*;

impl Sidebar {
    /// Note a folder row's total label for the in-place updates.
    pub(super) fn keep_total(&mut self, account_id: u32, folder_id: u32, label: gtk::Label) {
        self.folder_totals
            .entry((account_id, folder_id))
            .or_default()
            .push(label);
    }

    /// The total column on a row listing one folder, while totals are
    /// shown. Drafts' chip counts every draft already, and a row with no
    /// folder behind it (an account's starred mail kept as a flag) has
    /// nothing to count: theirs stays blank.
    pub(super) fn add_row_total(
        &mut self,
        row: &gtk::ListBoxRow,
        account_id: u32,
        folder: &Folder,
    ) {
        if !self.message_totals || self.collapsed {
            return;
        }
        if folder.path.is_empty() || crate::worker::chip_counts_all(folder.kind) {
            add_total_column(row, 0);
        } else if let Some(label) = add_total_column(row, folder.total) {
            self.keep_total(account_id, folder.id, label);
        }
    }

    /// A blank total column on a row with an unread chip but no folder
    /// (a tag, a smart folder), so its chip lines up with the folders'.
    pub(super) fn add_blank_total(&self, row: &gtk::ListBoxRow) {
        if self.message_totals && !self.collapsed {
            add_total_column(row, 0);
        }
    }

    /// The folders a unified header's total adds up: those listed beneath
    /// it that carry a total of their own.
    pub(super) fn header_total_keys(&self, row: UnifiedRow, rows: &[InboxRef]) -> Vec<(u32, u32)> {
        let counted = |f: &Folder| !crate::worker::chip_counts_all(f.kind);
        match row {
            UnifiedRow::Kind(kind) if !crate::worker::chip_counts_all(kind) => rows
                .iter()
                .filter(|r| !r.path.is_empty())
                .map(|r| (r.account_id, r.folder_id))
                .collect(),
            UnifiedRow::Favorites => self
                .favorite_folders
                .iter()
                .filter(|r| counted(&r.folder))
                .map(|r| (r.account_id, r.folder.id))
                .collect(),
            UnifiedRow::Filtered => self
                .unified_folders
                .iter()
                .filter(|r| counted(&r.folder))
                .map(|r| (r.account_id, r.folder.id))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The folders' totals added up, as the sidebar last had them.
    pub(super) fn total_of(&self, keys: &[(u32, u32)]) -> u32 {
        keys.iter()
            .filter_map(|(a, f)| {
                self.sections
                    .iter()
                    .find(|s| s.account.id == *a)?
                    .folders
                    .iter()
                    .find(|x| x.id == *f)
                    .map(|x| x.total)
            })
            .sum()
    }
}
