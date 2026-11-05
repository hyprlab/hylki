//! Which sort the list shows each view in: a folder, a unified row, a tag or
//! a smart folder keeps the sort chosen while it was open, and any other view
//! uses the one default (#574).

use relm4::prelude::*;
use std::collections::BTreeMap;

use crate::config::ListSort;
use crate::ui::message_list::{MessageListInput, SortOrder};

use super::model::AppModel;
use super::sidebar::folder_key;
use super::types::UnifiedView;

/// The sort of any view with none of its own, as `AppModel::list_sort`.
type Fallback = (&'static str, bool);

/// The sort of the view `name`: its own, or else the default.
fn sort_of(
    own: &BTreeMap<String, ListSort>,
    default: Fallback,
    name: Option<&str>,
) -> (SortOrder, bool) {
    match name.and_then(|n| own.get(n)) {
        Some(sort) => (SortOrder::from_key(&sort.order), sort.reversed),
        None => (SortOrder::from_key(default.0), default.1),
    }
}

/// Keep a sort chosen in the view `name`, and say whether that changed
/// anything. One that is the default is not kept: the view has no sort of its
/// own any more.
fn keep_sort(
    own: &mut BTreeMap<String, ListSort>,
    default: Fallback,
    name: &str,
    key: &str,
    reversed: bool,
) -> bool {
    if (key, reversed) == default {
        return own.remove(name).is_some();
    }
    let sort = ListSort {
        order: key.to_string(),
        reversed,
    };
    own.insert(name.to_string(), sort.clone()) != Some(sort)
}

impl AppModel {
    /// A unified row's name in `list_sorts`, with the account when the row
    /// is narrowed to one.
    fn unified_sort_name(&self, view: UnifiedView, account: Option<u32>) -> String {
        match account.and_then(|a| self.email_of(a)) {
            Some(email) => format!("unified\t{view:?}\t{email}"),
            None => format!("unified\t{view:?}"),
        }
    }

    /// The open view's name in `list_sorts`. A folder goes by its sidebar
    /// key, so a rename can carry its sort along.
    fn list_view_name(&self) -> Option<String> {
        if self.showing_outbox {
            return Some("outbox".to_string());
        }
        if self.unified {
            return Some(self.unified_sort_name(self.unified_view, self.unified_account));
        }
        if let Some((account, keyword)) = &self.tag_view {
            let email = account.and_then(|a| self.email_of(a)).unwrap_or_default();
            let keyword = keyword.as_deref().unwrap_or_default();
            return Some(format!("tag\t{email}\t{keyword}"));
        }
        if let Some(id) = &self.smart_view {
            return Some(format!("smart\t{id}"));
        }
        let open = self.selected.as_ref()?;
        Some(folder_key(&self.email_of(open.account_id)?, &open.path))
    }

    /// The open view's sort.
    pub(super) fn view_sort(&self) -> (SortOrder, bool) {
        let name = self.list_view_name();
        sort_of(&self.list_sorts, self.list_sort, name.as_deref())
    }

    /// The sort of a unified row that is not open, for building it ahead.
    pub(super) fn unified_sort(&self, view: UnifiedView) -> (SortOrder, bool) {
        let name = self.unified_sort_name(view, None);
        sort_of(&self.list_sorts, self.list_sort, Some(&name))
    }

    /// Give the list the open view's sort. Sent after `ResetPaging`, which
    /// keeps the view being left as built, under the sort it had.
    pub(super) fn apply_view_sort(&self) {
        let (order, reversed) = self.view_sort();
        self.message_list
            .emit(MessageListInput::ApplySort { order, reversed });
        self.show_view_sort();
    }

    pub(super) fn show_view_sort(&self) {
        if let Some(lh) = self.list_header_widgets.get() {
            lh.show_sort(self.view_sort().0.key(), !self.list_sorts.is_empty());
        }
    }

    /// The sort was chosen in the open view, by the menu or a heading.
    pub(super) fn keep_view_sort(&mut self, key: &'static str, reversed: bool) {
        if let Some(name) = self.list_view_name() {
            if keep_sort(&mut self.list_sorts, self.list_sort, &name, key, reversed) {
                self.save_settings();
            }
        }
        self.show_view_sort();
    }

    /// The open view's sort becomes the default, and no view keeps its own.
    pub(super) fn use_view_sort_for_all(&mut self) {
        let (order, reversed) = self.view_sort();
        self.list_sort = (order.key(), reversed);
        self.list_sorts.clear();
        self.save_settings();
        self.show_view_sort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEWEST: Fallback = ("date_newest", false);

    #[test]
    fn a_view_with_no_sort_of_its_own_uses_the_default() {
        let own = BTreeMap::new();
        assert_eq!(
            sort_of(&own, ("unread", true), Some("a@x\tInbox")),
            (SortOrder::UnreadFirst, true)
        );
        assert_eq!(sort_of(&own, NEWEST, None), (SortOrder::DateNewest, false));
    }

    #[test]
    fn a_folder_keeps_its_sort_and_the_others_stay_on_the_default() {
        let mut own = BTreeMap::new();
        assert!(keep_sort(&mut own, NEWEST, "a@x\tInbox", "unread", false));
        assert_eq!(
            sort_of(&own, NEWEST, Some("a@x\tInbox")),
            (SortOrder::UnreadFirst, false)
        );
        assert_eq!(
            sort_of(&own, NEWEST, Some("a@x\tTrash")),
            (SortOrder::DateNewest, false)
        );
    }

    #[test]
    fn choosing_the_same_sort_again_changes_nothing() {
        let mut own = BTreeMap::new();
        assert!(keep_sort(&mut own, NEWEST, "v", "sender", true));
        assert!(!keep_sort(&mut own, NEWEST, "v", "sender", true));
        assert!(keep_sort(&mut own, NEWEST, "v", "sender", false));
    }

    #[test]
    fn choosing_the_default_drops_the_folders_own() {
        let mut own = BTreeMap::new();
        assert!(!keep_sort(&mut own, NEWEST, "v", "date_newest", false));
        assert!(own.is_empty());
        keep_sort(&mut own, NEWEST, "v", "unread", false);
        assert!(keep_sort(&mut own, NEWEST, "v", "date_newest", false));
        assert!(own.is_empty());
        // Reversed is another sort, so it stays.
        assert!(keep_sort(&mut own, NEWEST, "v", "date_newest", true));
        assert_eq!(own.len(), 1);
    }
}
