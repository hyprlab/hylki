//! The date sections each view keeps folded (#659): a folder, a unified row,
//! a tag or a smart folder remembers the sections folded while it was open,
//! as it remembers its sort (#574).

use std::collections::BTreeMap;

use relm4::prelude::*;

use crate::ui::message_list::MessageListInput;

use super::model::AppModel;

/// Keep `days` as the sections folded in the view `name`, and say whether
/// that changed anything. A view with none folded is not kept.
fn keep_folds(own: &mut BTreeMap<String, Vec<String>>, name: &str, days: Vec<String>) -> bool {
    if days.is_empty() {
        own.remove(name).is_some()
    } else {
        own.insert(name.to_string(), days.clone()) != Some(days)
    }
}

impl AppModel {
    /// Give the list the sections the open view keeps folded.
    pub(super) fn apply_view_folds(&self) {
        let days = self
            .list_view_name()
            .and_then(|name| self.list_folded_days.get(&name).cloned())
            .unwrap_or_default();
        self.message_list
            .emit(MessageListInput::SetFoldedDays(days));
    }

    /// A section was folded or opened in the open view.
    pub(super) fn keep_view_folds(&mut self, days: Vec<String>) {
        if let Some(name) = self.list_view_name() {
            if keep_folds(&mut self.list_folded_days, &name, days) {
                self.save_settings();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_view_keeps_its_folded_sections_until_none_are() {
        let mut own = BTreeMap::new();
        assert!(keep_folds(&mut own, "a@x\tInbox", vec!["older".into()]));
        assert!(!keep_folds(&mut own, "a@x\tInbox", vec!["older".into()]));
        assert_eq!(own.get("a@x\tInbox"), Some(&vec!["older".to_string()]));
        assert!(keep_folds(&mut own, "a@x\tInbox", Vec::new()));
        assert!(own.is_empty());
        assert!(!keep_folds(&mut own, "a@x\tInbox", Vec::new()));
    }
}
