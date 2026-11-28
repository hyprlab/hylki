//! The user picking a row: each list's selection handler, keeping one
//! selection across the sidebar and telling the app.

use super::*;

impl Sidebar {
    pub(super) fn on_unified_row_selected(&mut self, sender: ComponentSender<Self>) {
        if self.selected == Sel::Unified {
            return;
        }
        self.selected = Sel::Unified;
        self.clear_other_selections(Sel::Unified);
        let _ = sender.output(SidebarOutput::UnifiedSelected);
    }

    pub(super) fn on_attachments_row_selected(&mut self, sender: ComponentSender<Self>) {
        if self.selected == Sel::Attachments {
            return;
        }
        self.selected = Sel::Attachments;
        self.clear_other_selections(Sel::Attachments);
        let _ = sender.output(SidebarOutput::AttachmentsSelected);
    }

    pub(super) fn on_calendar_row_selected(&mut self, sender: ComponentSender<Self>) {
        if self.selected == Sel::Calendar {
            return;
        }
        self.selected = Sel::Calendar;
        self.clear_other_selections(Sel::Calendar);
        let _ = sender.output(SidebarOutput::CalendarSelected);
    }

    pub(super) fn on_contacts_row_clicked(&mut self, sender: ComponentSender<Self>) {
        if self.selected == Sel::Contacts {
            return;
        }
        self.selected = Sel::Contacts;
        self.clear_other_selections(Sel::Contacts);
        let _ = sender.output(SidebarOutput::ContactsClicked);
        // The row itself is every contact. The footer's opens
        // Contacts on the book its own picker last showed.
        if !self.contacts_in_footer() {
            let _ = sender.output(SidebarOutput::ContactScope(
                crate::ui::contacts_page::ContactScope::All,
            ));
        }
    }

    pub(super) fn on_contact_tree_selected(
        &mut self,
        scope: crate::ui::contacts_page::ContactScope,
        sender: ComponentSender<Self>,
    ) {
        let key = Sel::ContactScope(scope.clone());
        if self.selected == key {
            return;
        }
        self.selected = key.clone();
        self.clear_other_selections(key);
        let _ = sender.output(SidebarOutput::ContactScope(scope));
    }

    pub(super) fn on_outbox_row_selected(&mut self, sender: ComponentSender<Self>) {
        if self.selected == Sel::Outbox {
            return;
        }
        self.selected = Sel::Outbox;
        self.clear_other_selections(Sel::Outbox);
        let _ = sender.output(SidebarOutput::OutboxSelected);
    }

    pub(super) fn on_unified_inbox_row_selected(
        &mut self,
        index: i32,
        sender: ComponentSender<Self>,
    ) {
        if let Some(r) = self.unified_inboxes.get(index as usize).cloned() {
            let key = Sel::UnifiedInbox(r.account_id);
            if self.selected == key {
                return;
            }
            self.selected = key.clone();
            self.clear_other_selections(key);
            let _ = sender.output(SidebarOutput::FolderSelected {
                account_id: r.account_id,
                folder_id: r.folder_id,
                name: r.name,
                path: r.path,
            });
        }
    }

    pub(super) fn on_filtered_row_selected(
        &mut self,
        slot: Slot,
        index: i32,
        sender: ComponentSender<Self>,
    ) {
        let Some(r) = self
            .filtered_rows
            .get(&slot)
            .and_then(|rows| rows.get(index as usize))
            .cloned()
        else {
            return;
        };
        let key = match slot {
            Slot::Unified => Sel::UnifiedFolder(r.account_id, r.folder.path.clone()),
            Slot::Account(_) => Sel::AccountFiltered(r.account_id, r.folder.path.clone()),
        };
        if self.selected == key {
            return;
        }
        self.selected = key.clone();
        self.clear_other_selections(key);
        let _ = sender.output(SidebarOutput::FolderSelected {
            account_id: r.account_id,
            folder_id: r.folder.id,
            name: r.folder.name,
            path: r.folder.path,
        });
    }

    pub(super) fn on_tag_row_selected(
        &mut self,
        slot: Slot,
        index: i32,
        sender: ComponentSender<Self>,
    ) {
        if let Some(t) = self.tags.get(index as usize).cloned() {
            let account = slot.account();
            let key = Sel::Tag(account, t.keyword.clone());
            if self.selected == key {
                return;
            }
            self.selected = key.clone();
            self.clear_other_selections(key);
            let _ = sender.output(SidebarOutput::TagSelected {
                keyword: t.keyword,
                account,
            });
        }
    }

    pub(super) fn on_smart_row_selected(&mut self, index: i32, sender: ComponentSender<Self>) {
        let Some((id, _)) = self.smart_folders.get(index as usize).cloned() else {
            return;
        };
        let key = Sel::Smart(id.clone());
        if self.selected == key {
            return;
        }
        self.selected = key.clone();
        self.clear_other_selections(key);
        let _ = sender.output(SidebarOutput::SmartSelected(id));
    }

    pub(super) fn on_favorite_row_selected(&mut self, index: i32, sender: ComponentSender<Self>) {
        let Some(r) = self.favorite_folders.get(index as usize).cloned() else {
            return;
        };
        let key = Sel::Favorite(r.account_id, r.folder.path.clone());
        if self.selected == key {
            return;
        }
        self.selected = key.clone();
        self.clear_other_selections(key);
        let _ = sender.output(SidebarOutput::FolderSelected {
            account_id: r.account_id,
            folder_id: r.folder.id,
            name: r.folder.name,
            path: r.folder.path,
        });
    }

    pub(super) fn on_unified_kind_row_selected(
        &mut self,
        kind: FolderKind,
        sender: ComponentSender<Self>,
    ) {
        let key = Sel::UnifiedKind(kind);
        if self.selected == key {
            return;
        }
        self.selected = key.clone();
        self.clear_other_selections(key);
        let _ = sender.output(SidebarOutput::UnifiedKindSelected(kind));
    }

    pub(super) fn on_unified_favorites_selected(&mut self, sender: ComponentSender<Self>) {
        if self.selected == Sel::UnifiedFavorites {
            return;
        }
        self.selected = Sel::UnifiedFavorites;
        self.clear_other_selections(Sel::UnifiedFavorites);
        let _ = sender.output(SidebarOutput::UnifiedFavoritesSelected);
    }

    pub(super) fn on_unified_filtered_selected(&mut self, sender: ComponentSender<Self>) {
        if self.selected == Sel::UnifiedFiltered {
            return;
        }
        self.selected = Sel::UnifiedFiltered;
        self.clear_other_selections(Sel::UnifiedFiltered);
        let _ = sender.output(SidebarOutput::UnifiedFilteredSelected);
    }

    pub(super) fn on_unified_tags_selected(&mut self, sender: ComponentSender<Self>) {
        if self.selected == Sel::UnifiedTags {
            return;
        }
        self.selected = Sel::UnifiedTags;
        self.clear_other_selections(Sel::UnifiedTags);
        let _ = sender.output(SidebarOutput::UnifiedTagsSelected);
    }

    pub(super) fn on_kind_sub_row_selected(
        &mut self,
        kind: FolderKind,
        index: i32,
        sender: ComponentSender<Self>,
    ) {
        let Some(r) = self
            .kind_widgets
            .get(&UnifiedRow::Kind(kind))
            .and_then(|w| w.rows.get(index as usize))
            .cloned()
        else {
            return;
        };
        let key = Sel::UnifiedKindRow(kind, r.account_id);
        if self.selected == key {
            return;
        }
        self.selected = key.clone();
        self.clear_other_selections(key);
        if r.path.is_empty() {
            let _ = sender.output(SidebarOutput::AccountStarredSelected(r.account_id));
            return;
        }
        let _ = sender.output(SidebarOutput::FolderSelected {
            account_id: r.account_id,
            folder_id: r.folder_id,
            name: r.name,
            path: r.path,
        });
    }

    pub(super) fn on_folder_row_selected(
        &mut self,
        account_id: u32,
        index: i32,
        sender: ComponentSender<Self>,
    ) {
        let folder = self
            .sections
            .iter()
            .find(|s| s.account.id == account_id)
            .and_then(|s| s.folders.get(index as usize))
            .cloned();
        if let Some(folder) = folder {
            let key = Sel::Folder(account_id, folder.path.clone());
            if self.selected == key {
                return;
            }
            self.selected = key.clone();
            self.clear_other_selections(key);
            let _ = sender.output(SidebarOutput::FolderSelected {
                account_id,
                folder_id: folder.id,
                name: folder.name.clone(),
                path: folder.path.clone(),
            });
        }
    }
}
