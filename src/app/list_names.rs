//! Whether the list's rows name the sender or the recipients. A Sent folder
//! names the recipients, as every sender there is you (#27); any view can be
//! switched the other way and keeps that, the way it keeps its sort (#657).

use std::collections::BTreeMap;

use relm4::prelude::*;

use crate::config::SmartFolderKind;
use crate::models::FolderKind;
use crate::ui::message_list::MessageListInput;

use super::model::AppModel;
use super::types::UnifiedView;

/// Keep `recipients` as the choice of the view `name`, and say whether that
/// changed anything. A choice that matches the view's own default is not
/// kept, so a folder made Sent later names its recipients.
fn keep_names(
    own: &mut BTreeMap<String, bool>,
    default: bool,
    name: &str,
    recipients: bool,
) -> bool {
    if recipients == default {
        own.remove(name).is_some()
    } else {
        own.insert(name.to_string(), recipients) != Some(recipients)
    }
}

impl AppModel {
    /// Whether the open view names recipients when nothing was chosen in it:
    /// the Sent folders and the Outbox do, where every message is yours.
    fn names_recipients_by_default(&self) -> bool {
        if self.showing_outbox {
            return true;
        }
        if self.unified {
            return self.unified_view == UnifiedView::Kind(FolderKind::Sent);
        }
        if self.tag_view.is_some() {
            return false;
        }
        if self.smart_view.is_some() {
            return self
                .open_smart_folder()
                .is_some_and(|f| f.folder == Some(SmartFolderKind::Sent));
        }
        self.selected.as_ref().is_some_and(|s| {
            self.folder_kind(s.account_id, s.folder_id) == Some(FolderKind::Sent)
        })
    }

    /// Whether the open view's rows name the recipients.
    pub(super) fn view_names_recipients(&self) -> bool {
        self.list_view_name()
            .and_then(|name| self.list_recipients.get(&name).copied())
            .unwrap_or_else(|| self.names_recipients_by_default())
    }

    /// Give the list, and the header's radios, the open view's names.
    pub(super) fn apply_view_names(&self) {
        let recipients = self.view_names_recipients();
        self.message_list
            .emit(MessageListInput::SetShowRecipient(recipients));
        if let Some(lh) = self.list_header_widgets.get() {
            lh.show_names(recipients);
        }
    }

    /// Sender or Recipients was chosen in the open view.
    pub(super) fn keep_view_names(&mut self, recipients: bool) {
        if let Some(name) = self.list_view_name() {
            let default = self.names_recipients_by_default();
            if keep_names(&mut self.list_recipients, default, &name, recipients) {
                self.save_settings();
            }
        }
        self.message_list
            .emit(MessageListInput::SetShowRecipient(recipients));
        if let Some(lh) = self.list_header_widgets.get() {
            lh.show_names(recipients);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_keeps_recipients_chosen_against_its_default() {
        let mut own = BTreeMap::new();
        assert!(keep_names(&mut own, false, "a@x\tArchive/Sent 2019", true));
        assert_eq!(own.get("a@x\tArchive/Sent 2019"), Some(&true));
        assert!(!keep_names(&mut own, false, "a@x\tArchive/Sent 2019", true));
    }

    #[test]
    fn choosing_the_default_again_forgets_the_choice() {
        let mut own = BTreeMap::new();
        assert!(!keep_names(&mut own, true, "a@x\tSent", true));
        assert!(own.is_empty());
        assert!(keep_names(&mut own, true, "a@x\tSent", false));
        assert_eq!(own.get("a@x\tSent"), Some(&false));
        assert!(keep_names(&mut own, true, "a@x\tSent", true));
        assert!(own.is_empty());
    }
}
