//! What the app hands the sidebar: its contents (a rebuild), unread counts
//! and the small states of the outbox, attachments, busy and offline rows.

use super::*;

impl Sidebar {
    /// `msg` is the `SetContents` the router hands over whole: its nineteen
    /// fields are the sidebar's contents and configuration.
    pub(super) fn on_set_contents(
        &mut self,
        msg: SidebarInput,
        widgets: &SidebarWidgets,
        sender: ComponentSender<Self>,
    ) {
        let SidebarInput::SetContents {
            mut sections,
            show_unified,
            unified_kinds,
            show_accounts,
            unified_chips,
            chevrons_left,
            account_click_folds,
            rail_dots,
            rail_fold,
            unified_unread,
            unified_folders,
            favorite_folders,
            tags,
            section_style,
            filtered_placement,
            tags_placement,
            favorites_placement,
            contacts_placement,
            smart_folders,
        } = msg
        else {
            // Only SetContents is routed here.
            return;
        };
        // Order each account's folders essential-first, then custom, so
        // the essential/custom split lines up with row indices (the main
        // list holds indices 0..E, the custom list E..), each part in
        // the user's own order where they set one.
        for s in &mut sections {
            s.folders = order_folders(
                std::mem::take(&mut s.folders),
                &s.folder_order,
                s.folder_sort,
                &s.hierarchy,
            );
        }
        // Seed the tree's collapsed nodes from the persisted state;
        // later chevron clicks flip the local copy.
        self.tree_collapsed = sections
            .iter()
            .map(|s| (s.account.id, s.tree_collapsed.iter().cloned().collect()))
            .collect();
        self.sections = sections;
        self.show_unified = show_unified;
        self.unified_chips = unified_chips;
        self.chevrons_left = chevrons_left;
        self.account_click_folds = account_click_folds;
        self.rail_dots = rail_dots;
        // "Fold up expanded items" is a view of the rail, not a
        // change to what is saved: the rebuild below draws the
        // ticked items folded while the sidebar is collapsed and
        // as they are once it expands (see `locked_row`).
        self.rail_fold = rail_fold;
        self.unified_unread = unified_unread;
        self.unified_folders = unified_folders;
        self.favorite_folders = favorite_folders;
        self.tags = tags;
        self.section_style = section_style;
        self.filtered_placement = filtered_placement;
        self.tags_placement = tags_placement;
        self.favorites_placement = favorites_placement;
        self.contacts_placement = contacts_placement;
        self.smart_folders = smart_folders;
        self.unified_kinds = unified_kinds;
        self.show_accounts = show_accounts;
        self.fill_filtered_rows();
        self.settle_selection();
        self.rebuild(widgets, &sender);
    }

    /// Every Filtered Folders section's rows: the unified one's (narrowed
    /// by the app to the rules that opted in) and each account's own
    /// (every rule's destination).
    fn fill_filtered_rows(&mut self) {
        self.filtered_rows.clear();
        self.filtered_rows
            .insert(Slot::Unified, self.unified_folders.clone());
        for s in &self.sections {
            let rows = s
                .filtered
                .iter()
                .map(|f| UnifiedFolderRef {
                    account_id: s.account.id,
                    folder: f.clone(),
                })
                .collect();
            self.filtered_rows.insert(Slot::Account(s.account.id), rows);
        }
    }

    /// The selection after new contents: a row that is gone falls back to
    /// the default view, and a folder that left a section stays the open
    /// folder, highlighted on its account's own row.
    fn settle_selection(&mut self) {
        // The open smart folder was deleted: fall back to the
        // default view.
        if let Sel::Smart(id) = &self.selected {
            if !self.smart_folders.iter().any(|(i, _)| i == id) {
                self.selected = Sel::None;
            }
        }
        // The open tag was removed (or its account): fall back to
        // the default view.
        if let Sel::Tag(acc, kw) = &self.selected {
            let gone = !self.tags.iter().any(|t| t.keyword.eq_ignore_ascii_case(kw))
                || acc.is_some_and(|a| !self.sections.iter().any(|s| s.account.id == a));
            if gone {
                self.selected = Sel::None;
            }
        }
        // A unified row switched off in Settings (or with nothing
        // left to list): its view stays, nothing shows selected.
        if let Sel::UnifiedKind(kind) = &self.selected {
            if !self.unified_kinds.has(*kind) {
                self.selected = Sel::None;
            }
        }
        if self.selected == Sel::UnifiedFiltered && self.unified_folders.is_empty() {
            self.selected = Sel::None;
        }
        if self.selected == Sel::UnifiedFavorites && !self.favorites_shown() {
            self.selected = Sel::None;
        }
        // All Inboxes gone (one account or none left in the unified
        // section, #267): the usual first view is picked again.
        if self.selected == Sel::Unified && !self.show_unified {
            self.selected = Sel::None;
        }
        // An inbox or folder picked under a unified row whose account
        // has just left the unified section is still the open
        // folder: carry the highlight to the account's own row.
        let left_unified = |acc: u32| {
            self.sections
                .iter()
                .find(|s| s.account.id == acc)
                .map(|s| !s.in_unified)
        };
        let moved = match &self.selected {
            Sel::UnifiedInbox(acc) => Some((*acc, FolderKind::Inbox)),
            Sel::UnifiedKindRow(kind, acc) => Some((*acc, *kind)),
            _ => None,
        };
        if let Some((acc, kind)) = moved.filter(|(acc, _)| left_unified(*acc) == Some(true)) {
            self.selected = self.own_folder_row(acc, kind);
        }
        if self.selected == Sel::UnifiedTags && !self.unified_tags_shown() {
            self.selected = Sel::None;
        }
        // Contacts placed off takes its selection with it; in the
        // footer, its one row is every scope.
        if matches!(self.selected, Sel::Contacts | Sel::ContactScope(_)) {
            if self.contacts_placement == crate::config::SectionPlacement::Off {
                self.selected = Sel::None;
            } else if self.contacts_in_footer() {
                self.selected = Sel::Contacts;
            }
        }
        // A folder picked in a switched-off row's account list is
        // still the open folder: carry the highlight to the
        // account's own row.
        if let Sel::UnifiedKindRow(kind, acc) = self.selected.clone() {
            if !self.unified_kinds.has(kind) {
                self.selected = self.own_folder_row(acc, kind);
            }
        }
        // A folder taken out of Favorites is still the open folder.
        if let Sel::Favorite(acc, path) = &self.selected {
            if !self
                .favorite_folders
                .iter()
                .any(|r| r.account_id == *acc && r.folder.path == *path)
            {
                self.selected = Sel::Folder(*acc, path.clone());
            }
        }
        // Likewise a folder that left an account's own Filtered
        // Folders section (its rule was removed).
        if let Sel::AccountFiltered(acc, path) = &self.selected {
            let listed = self
                .filtered_rows
                .get(&Slot::Account(*acc))
                .is_some_and(|rows| rows.iter().any(|r| r.folder.path == *path));
            if !listed {
                self.selected = Sel::Folder(*acc, path.clone());
            }
        }
        // A selected filtered folder that just left the section (its
        // rule opted out, or the section was switched off) is still
        // the open folder: carry the highlight to the account
        // section's own row for it.
        if let Sel::UnifiedFolder(acc, path) = &self.selected {
            let listed = self
                .unified_folders
                .iter()
                .any(|r| r.account_id == *acc && r.folder.path == *path);
            if !listed {
                self.selected = Sel::Folder(*acc, path.clone());
            }
        }
    }

    /// The row of account `acc`'s own folder of `kind`, or nothing when it
    /// has none.
    fn own_folder_row(&self, acc: u32, kind: FolderKind) -> Sel {
        let path = self
            .sections
            .iter()
            .find(|s| s.account.id == acc)
            .and_then(|s| s.folders.iter().find(|f| f.kind == kind))
            .map(|f| f.path.clone());
        path.map_or(Sel::None, |p| Sel::Folder(acc, p))
    }

    pub(super) fn on_set_contact_tree(
        &mut self,
        tree: crate::ui::contacts_page::ContactTree,
        sender: ComponentSender<Self>,
    ) {
        if self.contact_tree.as_ref() == Some(&tree) {
            return;
        }
        // The highlight follows a scope the contact page picked
        // itself (a book's New Contact opens that book).
        if matches!(self.selected, Sel::Contacts | Sel::ContactScope(_)) {
            self.selected = match &tree.scope {
                _ if self.contacts_in_footer() => Sel::Contacts,
                crate::ui::contacts_page::ContactScope::All => Sel::Contacts,
                scope => Sel::ContactScope(scope.clone()),
            };
            self.quiet.set(true);
            self.clear_other_selections(self.selected.clone());
            if self.selected == Sel::Contacts {
                self.select_contacts(None);
            }
            self.quiet.set(false);
        }
        self.contact_tree = Some(tree);
        self.fill_contact_tree(&sender);
    }

    pub(super) fn on_set_outbox_count(
        &mut self,
        count: u32,
        widgets: &SidebarWidgets,
        sender: ComponentSender<Self>,
    ) {
        if self.outbox_count == count {
            return;
        }
        let appearing = (self.outbox_count == 0) != (count == 0);
        self.outbox_count = count;
        // The row itself comes and goes with the count, so the sidebar
        // only needs rebuilding when it crosses zero; otherwise just
        // refresh the badge in place.
        if appearing {
            // Leaving the Outbox selected when its row disappears would
            // strand the view on an empty list.
            if count == 0 && self.selected == Sel::Outbox {
                self.selected = Sel::None;
            }
            self.rebuild(widgets, &sender);
        } else if let Some(list) = &self.outbox_list {
            if let Some(row) = list.row_at_index(0) {
                if let Some(badge) = row.child().and_downcast::<gtk::Box>() {
                    if let Some(label) = badge.last_child().and_downcast::<gtk::Label>() {
                        label.set_label(&count.to_string());
                    }
                }
            }
        }
    }

    pub(super) fn on_set_smart_unread(&mut self, counts: HashMap<String, u32>) {
        for (id, badge) in &self.smart_badges {
            let n = counts.get(id).copied().unwrap_or(0);
            badge.set_label(&n.to_string());
            badge.set_visible(n > 0);
        }
        self.smart_unread = counts;
        if let Some(w) = self.kind_widgets.get(&UnifiedRow::Smart) {
            if let Some(b) = &w.badge {
                let total = self.row_unread(UnifiedRow::Smart);
                b.set_label(&total.to_string());
                b.set_visible(total > 0 && !self.row_shown_open(UnifiedRow::Smart));
            }
        }
    }

    pub(super) fn on_set_unread(
        &mut self,
        folders: HashMap<(u32, u32), u32>,
        unified: u32,
        fresh: Option<std::collections::HashSet<(u32, u32)>>,
    ) {
        self.fresh = fresh;
        // Mirror the fresh counts into every row list too, so a
        // rebuild draws them right (the account folders are done
        // below).
        let fresh = |aid: u32, fid: u32, cur: u32| folders.get(&(aid, fid)).copied().unwrap_or(cur);
        for r in self
            .unified_folders
            .iter_mut()
            .chain(self.favorite_folders.iter_mut())
        {
            r.folder.unread = fresh(r.account_id, r.folder.id, r.folder.unread);
        }
        for rows in self.filtered_rows.values_mut() {
            for r in rows.iter_mut() {
                r.folder.unread = fresh(r.account_id, r.folder.id, r.folder.unread);
            }
        }
        for s in &mut self.sections {
            let aid = s.account.id;
            for f in &mut s.filtered {
                f.unread = fresh(aid, f.id, f.unread);
            }
        }
        for ((aid, fid), label) in self
            .folder_badges
            .iter()
            .chain(&self.unified_inbox_badges)
            .chain(self.filtered_badges.values().flatten())
            .chain(self.kind_widgets.values().flat_map(|w| &w.row_badges))
        {
            let n = folders.get(&(*aid, *fid)).copied().unwrap_or(0);
            label.set_text(&n.to_string());
            label.set_visible(n > 0);
        }
        if let Some(label) = &self.unified_badge {
            label.set_text(&unified.to_string());
            label.set_visible(
                unified > 0
                    && !self.row_shown_open(UnifiedRow::Kind(FolderKind::Inbox))
                    && self.unified_chips.all_inboxes,
            );
        }
        for (row, w) in &self.kind_widgets {
            let total: u32 = w
                .row_badges
                .keys()
                .map(|k| folders.get(k).copied().unwrap_or(0))
                .sum();
            if let Some(b) = &w.badge {
                b.set_text(&total.to_string());
                b.set_visible(total > 0 && !self.row_shown_open(*row) && self.chip_shown(*row));
            }
        }
        // Keep the avatar-circle badges in sync too. They only show while
        // the account is collapsed (toggled live in ToggleCollapseLocal),
        // so we just refresh the number and re-apply that visibility rule.
        for section in &self.sections {
            if let Some(label) = self.account_circle_badges.get(&section.account.id) {
                let n = section
                    .folders
                    .iter()
                    .find(|f| f.kind == FolderKind::Inbox)
                    .and_then(|f| folders.get(&(section.account.id, f.id)))
                    .copied()
                    .unwrap_or(0);
                label.set_text(&n.to_string());
                label.set_visible(self.account_shown_folded(section) && n > 0);
            }
        }
        self.unified_unread = unified;
        self.restyle_seen();
        // Persist the fresh counts into `sections` as well. Otherwise the
        // next rebuild_normal (e.g. toggling the sidebar collapse) recreates
        // every badge from the folder unread values captured at the last
        // SetContents, reverting in-place updates — so a read inbox's chip
        // reappears on collapse. Keep `sections` a faithful mirror.
        for section in &mut self.sections {
            for folder in &mut section.folders {
                folder.unread = folders
                    .get(&(section.account.id, folder.id))
                    .copied()
                    .unwrap_or(0);
            }
        }
    }

    pub(super) fn on_set_attachments_row(
        &mut self,
        show: bool,
        widgets: &SidebarWidgets,
        sender: ComponentSender<Self>,
    ) {
        self.show_attachments = show;
        // The row is about to disappear from under the selection; fall
        // back the same way an emptied selection does.
        if !show && self.selected == Sel::Attachments {
            self.selected = Sel::None;
        }
        self.rebuild(widgets, &sender);
    }

    pub(super) fn on_set_busy(&mut self, busy: bool) {
        self.busy = busy;
        if let Some(sp) = &self.sync_spinner {
            sp.set_spinning(busy);
        }
        if let Some(stack) = &self.sync_stack {
            stack.set_visible_child_name(if busy { "spinner" } else { "icon" });
        }
    }

    pub(super) fn on_set_offline(&mut self, offline: bool) {
        self.offline = offline;
        if let Some(button) = &self.sync_button {
            crate::ui::show_refresh_offline(button, offline);
        }
    }
}
