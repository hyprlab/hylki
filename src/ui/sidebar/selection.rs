//! What is selected: keeping one selection across every list, restoring
//! it after a rebuild, and selecting rows on the app's behalf.

use super::tree::{is_main_row, nests_in_inbox};
use super::types::chevron_icon;
use super::types::UnifiedRow;
use super::*;

impl Sidebar {
    fn select_tag(&self, account: Option<u32>, keyword: &str) {
        let Some(idx) = self
            .tags
            .iter()
            .position(|t| t.keyword.eq_ignore_ascii_case(keyword))
        else {
            return;
        };
        let list = match account {
            None => self
                .kind_widgets
                .get(&UnifiedRow::Tags)
                .map(|w| w.list.clone()),
            Some(id) => self
                .tag_sections
                .get(&Slot::Account(id))
                .map(|w| w.list.clone()),
        };
        if let Some(list) = list {
            if let Some(row) = list.row_at_index(idx as i32) {
                list.select_row(Some(&row));
            }
        }
    }

    /// List the address books and groups under the Contacts row, the one
    /// the contact list shows picked.
    pub(super) fn fill_contact_tree(&mut self, sender: &ComponentSender<Self>) {
        let Some(list) = self
            .kind_widgets
            .get(&UnifiedRow::Contacts)
            .map(|w| w.list.clone())
        else {
            return;
        };
        let Some(tree) = self.contact_tree.as_ref() else {
            return;
        };
        self.quiet.set(true);
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        let filled = crate::ui::contact_tree::fill(
            &list,
            tree,
            self.collapsed,
            self.chevrons_left,
            &self.contacts_folded,
            sender.input_sender(),
        );
        if let Sel::ContactScope(scope) = &self.selected {
            if let Some((_, row)) = filled.rows.iter().find(|(s, _)| s == scope) {
                list.select_row(Some(row));
            }
        }
        self.quiet.set(false);
        *self.tree_rows.borrow_mut() = filled.rows;
        self.contact_layout = filled.layout;
        self.contact_chevrons = filled.chevrons;
    }

    /// Deselect every list except the one owning `keep` (whose own list keeps its
    /// selection). Used when a selection moves between sections.
    pub(super) fn clear_other_selections(&self, keep: Sel) {
        if keep != Sel::Unified {
            if let Some(l) = &self.unified_list {
                l.unselect_all();
            }
        }
        for (row, w) in &self.kind_widgets {
            let keep_header = match row {
                UnifiedRow::Kind(k) => keep == Sel::UnifiedKind(*k),
                UnifiedRow::Favorites => keep == Sel::UnifiedFavorites,
                UnifiedRow::Smart => false,
                UnifiedRow::Filtered => keep == Sel::UnifiedFiltered,
                UnifiedRow::Tags => keep == Sel::UnifiedTags,
                UnifiedRow::Contacts => keep == Sel::Contacts,
            };
            if !keep_header {
                w.header.unselect_all();
            }
            let keep_list = match row {
                UnifiedRow::Kind(k) => matches!(&keep, Sel::UnifiedKindRow(kk, _) if kk == k),
                UnifiedRow::Favorites => matches!(keep, Sel::Favorite(..)),
                UnifiedRow::Filtered => matches!(keep, Sel::UnifiedFolder(..)),
                UnifiedRow::Tags => matches!(keep, Sel::Tag(None, _)),
                UnifiedRow::Smart => matches!(keep, Sel::Smart(_)),
                UnifiedRow::Contacts => matches!(keep, Sel::ContactScope(_)),
            };
            if !keep_list {
                w.list.unselect_all();
            }
        }
        for (slot, w) in &self.tag_sections {
            if !matches!(&keep, Sel::Tag(acc, _) if *acc == slot.account()) {
                w.list.unselect_all();
            }
        }
        // One list holds both footer rows; single-selection makes them
        // mutually exclusive, so it only needs clearing when neither is kept.
        if keep != Sel::Attachments && keep != Sel::Contacts {
            if let Some(l) = &self.footer_list {
                l.unselect_all();
            }
        }
        if keep != Sel::Outbox {
            if let Some(l) = &self.outbox_list {
                l.unselect_all();
            }
        }
        if !matches!(keep, Sel::UnifiedInbox(_)) {
            if let Some(l) = &self.unified_inbox_list {
                l.unselect_all();
            }
        }
        // A folder selection lives in exactly one of the two lists (essential or
        // custom) of one account; unselect every other list, including the
        // sibling list of the same account.
        let keep_is_custom = if let Sel::Folder(kaid, kpath) = &keep {
            self.folder_kind(*kaid, kpath) == Some(FolderKind::Custom)
        } else {
            false
        };
        for (aid, lb) in &self.folder_lists {
            let keep_here = matches!(&keep, Sel::Folder(kaid, _) if kaid == aid) && !keep_is_custom;
            if !keep_here {
                lb.unselect_all();
            }
        }
        for (aid, lb) in &self.custom_folder_lists {
            let keep_here = matches!(&keep, Sel::Folder(kaid, _) if kaid == aid) && keep_is_custom;
            if !keep_here {
                lb.unselect_all();
            }
        }
    }

    fn select_attachments(&self) {
        if let (Some(list), Some(row)) = (&self.footer_list, &self.attachments_row) {
            list.select_row(Some(row));
        }
    }

    /// The Contacts row (every contact), or a book or group under it.
    pub(super) fn select_contacts(&self, scope: Option<&crate::ui::contacts_page::ContactScope>) {
        if let (Some(list), Some(row)) = (&self.footer_list, &self.contacts_row) {
            list.select_row(Some(row));
            return;
        }
        let Some(w) = self.kind_widgets.get(&UnifiedRow::Contacts) else {
            return;
        };
        match scope {
            None => w.header.select_row(w.header.row_at_index(0).as_ref()),
            Some(scope) => {
                if let Some((_, row)) = self.tree_rows.borrow().iter().find(|(s, _)| s == scope) {
                    w.list.select_row(Some(row));
                }
            }
        }
    }

    fn select_outbox(&self) {
        if let Some(list) = &self.outbox_list {
            if let Some(row) = list.row_at_index(0) {
                list.select_row(Some(&row));
            }
        }
    }

    /// Highlight a unified row's header (row 0 of its own list).
    fn select_row_header(&self, row: UnifiedRow) {
        if let Some(w) = self.kind_widgets.get(&row) {
            w.header.select_row(w.header.row_at_index(0).as_ref());
        }
    }

    /// Re-apply the current selection after a rebuild; on first populate (no
    /// prior selection) default to the unified row if shown, else the first folder.
    pub(super) fn restore_selection(&mut self) {
        // With nothing selected the primary instance picks the opening view
        // (All Inboxes, else the first inbox) and must say so: that first
        // pick reaches the app through the list boxes' selection signals.
        // Everything else is a restore of what is already shown — silent.
        let announce = self.selected == Sel::None && !self.mirror;
        self.quiet.set(!announce);
        self.restore_selection_inner();
        self.quiet.set(false);
    }

    fn restore_selection_inner(&mut self) {
        // A view has been picked and has stuck: the launch view is done
        // with, whichever it was.
        if self.selected != Sel::None {
            self.start = None;
            self.reannounce = None;
        }
        let selected = self.selected.clone();
        if selected != Sel::None {
            return self.select_sel(selected);
        }
        // A view to pick again after a reconnect, once its account is back.
        if let Some((sel, email)) = self.reannounce.clone() {
            let sel = match email {
                Some(email) => match self
                    .sections
                    .iter()
                    .find(|s| s.account.email.eq_ignore_ascii_case(&email))
                {
                    Some(section) => sel.with_account(section.account.id),
                    // Not listed (yet, or removed): wait, and the timeout
                    // falls back to the usual first view.
                    None => return,
                },
                None => sel,
            };
            if !self.sel_ready(&sel) {
                return;
            }
            // The row's handler takes the pick up (a queued message). Until
            // it has, the pick stays pending and nothing else is picked: a
            // pass in between would queue the first view behind it. It is
            // announced whatever pass this is: the app is waiting for it.
            let quiet = self.quiet.replace(false);
            self.select_sel(sel);
            self.quiet.set(quiet);
            return;
        }
        // The launch view (#256) is picked again on every pass until
        // one sticks: the sections are rebuilt as each account's
        // folders arrive, and a pick only takes once its row's
        // selection has been handled. Until the account it names is
        // listed with its folders, nothing else is picked, or All
        // Inboxes would be on screen first and stay.
        let start = self.start.clone();
        if let Some(target) = start {
            match self.resolve_start(&target) {
                Some(Sel::UnifiedInbox(acc)) => return self.select_unified_inbox(acc),
                Some(Sel::Folder(acc, path)) => return self.select_folder(acc, &path),
                _ if !self.start_account_ready(&target) => return,
                // Listed, but with nowhere to land: the usual view.
                _ => self.start = None,
            }
        }
        if self.show_unified {
            self.select_unified();
        } else if let Some(acc) = self
            .sections
            .iter()
            .find(|s| !s.folders.is_empty())
            .map(|s| s.account.id)
            .filter(|_| self.show_accounts && !self.focus_hide_accounts)
        {
            self.select_folder_index(acc, 0);
        }
    }

    /// Select the row for `sel`. While the list holds no selection, the
    /// row's own handler takes it up and tells the app.
    fn select_sel(&self, sel: Sel) {
        match sel {
            Sel::Unified => self.select_unified(),
            Sel::Attachments => self.select_attachments(),
            Sel::Contacts => self.select_contacts(None),
            Sel::ContactScope(scope) => self.select_contacts(Some(&scope)),
            Sel::Outbox => self.select_outbox(),
            Sel::Folder(acc, path) => self.select_folder(acc, &path),
            Sel::UnifiedInbox(acc) => self.select_unified_inbox(acc),
            Sel::UnifiedFolder(acc, path) => self.select_filtered_row(Slot::Unified, acc, &path),
            Sel::AccountFiltered(acc, path) => {
                self.select_filtered_row(Slot::Account(acc), acc, &path)
            }
            Sel::Tag(acc, kw) => self.select_tag(acc, &kw),
            Sel::UnifiedKind(kind) => self.select_row_header(UnifiedRow::Kind(kind)),
            Sel::UnifiedFiltered => self.select_row_header(UnifiedRow::Filtered),
            Sel::UnifiedFavorites => self.select_row_header(UnifiedRow::Favorites),
            Sel::UnifiedTags => self.select_row_header(UnifiedRow::Tags),
            Sel::Smart(id) => {
                if let Some(w) = self.kind_widgets.get(&UnifiedRow::Smart) {
                    if let Some(idx) = self.smart_folders.iter().position(|(i, _)| *i == id) {
                        w.list.select_row(w.list.row_at_index(idx as i32).as_ref());
                    }
                }
            }
            Sel::Favorite(acc, path) => {
                if let Some(w) = self.kind_widgets.get(&UnifiedRow::Favorites) {
                    if let Some(idx) = self
                        .favorite_folders
                        .iter()
                        .position(|r| r.account_id == acc && r.folder.path == path)
                    {
                        w.list.select_row(w.list.row_at_index(idx as i32).as_ref());
                    }
                }
            }
            Sel::UnifiedKindRow(kind, acc) => {
                if let Some(w) = self.kind_widgets.get(&UnifiedRow::Kind(kind)) {
                    if let Some(idx) = w.rows.iter().position(|r| r.account_id == acc) {
                        w.list.select_row(w.list.row_at_index(idx as i32).as_ref());
                    }
                }
            }
            Sel::None => {}
        }
    }

    /// Whether the row for `sel` can be picked: its account (or, for the
    /// views across accounts, any account) has listed its folders.
    fn sel_ready(&self, sel: &Sel) -> bool {
        let account = sel.account();
        self.sections
            .iter()
            .any(|s| account.is_none_or(|acc| s.account.id == acc) && !s.folders.is_empty())
    }

    /// The row a launch view names, if it is on screen: an inbox in the
    /// Inboxes list when that is open (or the accounts' own sections are
    /// hidden), otherwise the folder in its account's section.
    fn resolve_start(&self, target: &StartTarget) -> Option<Sel> {
        let (email, path) = match target {
            StartTarget::Inbox(email) => (email, None),
            StartTarget::Folder(email, path) => (email, Some(path)),
        };
        let section = self
            .sections
            .iter()
            .find(|s| s.account.email.eq_ignore_ascii_case(email))?;
        let acc = section.account.id;
        let inbox = section
            .folders
            .iter()
            .find(|f| f.kind == FolderKind::Inbox)
            .map(|f| f.path.clone());
        // A folder since deleted or hidden: that account's inbox instead.
        let path = path
            .filter(|p| section.folders.iter().any(|f| &f.path == *p))
            .cloned()
            .or_else(|| inbox.clone())?;
        let accounts_shown = self.show_accounts && !self.focus_hide_accounts;
        let in_inboxes =
            self.show_unified && self.unified_inboxes.iter().any(|r| r.account_id == acc);
        if inbox.as_ref() == Some(&path) && in_inboxes && (self.unified_expanded || !accounts_shown)
        {
            return Some(Sel::UnifiedInbox(acc));
        }
        if accounts_shown && section.folders.iter().any(|f| f.path == path) {
            return Some(Sel::Folder(acc, path));
        }
        None
    }

    /// Whether the account a launch view names is listed with its folders.
    fn start_account_ready(&self, target: &StartTarget) -> bool {
        let (StartTarget::Inbox(email) | StartTarget::Folder(email, _)) = target;
        self.sections
            .iter()
            .any(|s| s.account.email.eq_ignore_ascii_case(email) && !s.folders.is_empty())
    }

    fn select_unified(&self) {
        if let Some(list) = &self.unified_list {
            if let Some(row) = list.row_at_index(0) {
                list.select_row(Some(&row));
            }
        }
    }

    fn select_unified_inbox(&self, account_id: u32) {
        if let Some(list) = &self.unified_inbox_list {
            if let Some(idx) = self
                .unified_inboxes
                .iter()
                .position(|r| r.account_id == account_id)
            {
                if let Some(row) = list.row_at_index(idx as i32) {
                    list.select_row(Some(&row));
                }
            }
        }
    }

    /// Highlight a filtered folder's row: in the unified row's list, or an
    /// account's own section.
    fn select_filtered_row(&self, slot: Slot, account_id: u32, path: &str) {
        let list = match slot {
            Slot::Unified => self
                .kind_widgets
                .get(&UnifiedRow::Filtered)
                .map(|w| w.list.clone()),
            Slot::Account(_) => None,
        };
        let Some(list) = list else { return };
        if let Some(idx) = self.filtered_rows.get(&slot).and_then(|rows| {
            rows.iter()
                .position(|r| r.account_id == account_id && r.folder.path == path)
        }) {
            if let Some(row) = list.row_at_index(idx as i32) {
                list.select_row(Some(&row));
            }
        }
    }

    /// Unfold an account's section if it is folded, the way its chevron
    /// would: the rail keeps its own fold state, the full sidebar's is
    /// reported (and persisted) as a toggle. Nothing happens to an account
    /// already open.
    pub(super) fn reveal_account(&mut self, id: u32, sender: &ComponentSender<Self>) {
        let Some(rev) = self.revealers.get(&id) else {
            return;
        };
        if rev.reveals_child() {
            return;
        }
        rev.set_reveal_child(true);
        if let Some(ch) = self.chevrons.get(&id) {
            ch.set_icon_name(Some(chevron_icon(true)));
        }
        if self.collapsed {
            self.rail_open_accounts.insert(id, true);
        } else if let Some(s) = self.sections.iter_mut().find(|s| s.account.id == id) {
            s.collapsed = false;
        }
        // The avatar badge stands in for the Inbox chip only while folded.
        if let Some(label) = self.account_circle_badges.get(&id) {
            label.set_visible(false);
        }
        if !self.collapsed {
            let _ = sender.output(SidebarOutput::AccountFolded { id, folded: false });
        }
    }

    fn select_folder(&self, account_id: u32, path: &str) {
        let idx = self
            .sections
            .iter()
            .find(|s| s.account.id == account_id)
            .and_then(|s| s.folders.iter().position(|f| f.path == path));
        if let Some(idx) = idx {
            self.select_folder_index(account_id, idx);
        }
    }

    fn select_folder_index(&self, account_id: u32, idx: usize) {
        // Essential folders live in the main list (rows 0..E); custom folders in
        // the collapsible list (rows E..). Route to the right one.
        let e = self.essential_count(account_id);
        let (list, row_idx) = if idx < e {
            (self.folder_lists.get(&account_id), idx)
        } else {
            (self.custom_folder_lists.get(&account_id), idx - e)
        };
        if let Some(list) = list {
            if let Some(row) = list.row_at_index(row_idx as i32) {
                list.select_row(Some(&row));
            }
        }
    }

    /// Number of essential (non-custom) folders for an account — the boundary
    /// between the main and custom folder lists in `section.folders`.
    fn essential_count(&self, account_id: u32) -> usize {
        self.sections
            .iter()
            .find(|s| s.account.id == account_id)
            .map(|s| {
                let nest = nests_in_inbox(&s.folders, &s.hierarchy);
                s.folders
                    .iter()
                    .filter(|f| is_main_row(f, nest, &s.hierarchy))
                    .count()
            })
            .unwrap_or(0)
    }

    /// The kind of the folder at `path` in `account_id`, if known.
    fn folder_kind(&self, account_id: u32, path: &str) -> Option<FolderKind> {
        self.sections
            .iter()
            .find(|s| s.account.id == account_id)
            .and_then(|s| s.folders.iter().find(|f| f.path == path))
            .map(|f| f.kind)
    }
}

impl Sidebar {
    pub(super) fn on_drop_start(&mut self) {
        let waited = self.start.take().is_some() | self.reannounce.take().is_some();
        if waited && self.selected == Sel::None {
            self.restore_selection();
        }
    }

    pub(super) fn on_reannounce(&mut self, sender: ComponentSender<Self>) {
        if self.mirror || self.selected == Sel::None {
            return;
        }
        let sel = std::mem::replace(&mut self.selected, Sel::None);
        let email = sel.account().and_then(|acc| {
            self.sections
                .iter()
                .find(|s| s.account.id == acc)
                .map(|s| s.account.email.clone())
        });
        self.reannounce = Some((sel, email));
        self.quiet.set(true);
        self.clear_other_selections(Sel::None);
        self.quiet.set(false);
        // An account that never comes back online must not leave
        // the list without a view.
        let s = sender.input_sender().clone();
        gtk::glib::timeout_add_seconds_local_once(8, move || {
            let _ = s.send(SidebarInput::DropStart);
        });
    }

    pub(super) fn on_mirror_selection(&mut self, sel: Sel) {
        if self.selected != sel {
            self.selected = sel.clone();
            self.quiet.set(true);
            self.clear_other_selections(sel);
            self.quiet.set(false);
            self.restore_selection();
        }
    }

    pub(super) fn on_select_unified_row(&mut self, sender: ComponentSender<Self>) {
        if self.selected == Sel::Unified {
            return;
        }
        self.selected = Sel::Unified;
        self.quiet.set(true);
        self.clear_other_selections(Sel::Unified);
        if let Some(l) = &self.unified_list {
            l.select_row(l.row_at_index(0).as_ref());
        }
        self.quiet.set(false);
        let _ = sender.output(SidebarOutput::UnifiedSelected);
    }

    pub(super) fn on_select_folder_row(
        &mut self,
        account_id: u32,
        path: String,
        sender: ComponentSender<Self>,
    ) {
        // A click on a per-account inbox under "All Inboxes" echoes
        // back here as its plain folder path; that sub-row already
        // shows this exact folder, so keep its highlight instead of
        // jumping to the account section's own Inbox row.
        if self.selected == Sel::UnifiedInbox(account_id)
            && self
                .unified_inboxes
                .iter()
                .any(|r| r.account_id == account_id && r.path == path)
        {
            return;
        }
        // Likewise a click on a filtered folder in either kind of
        // Filtered Folders section, on a favorite, or on an account's
        // folder in a unified Starred / Sent / Drafts row's list.
        if self.selected == Sel::UnifiedFolder(account_id, path.clone())
            || self.selected == Sel::AccountFiltered(account_id, path.clone())
            || self.selected == Sel::Favorite(account_id, path.clone())
        {
            return;
        }
        if let Sel::UnifiedKindRow(kind, acc) = &self.selected {
            if *acc == account_id
                && self
                    .kind_widgets
                    .get(&UnifiedRow::Kind(*kind))
                    .is_some_and(|w| {
                        w.rows
                            .iter()
                            .any(|r| r.account_id == account_id && r.path == path)
                    })
            {
                return;
            }
        }
        let key = Sel::Folder(account_id, path.clone());
        if self.selected != key {
            self.selected = key.clone();
            self.quiet.set(true);
            self.clear_other_selections(key);
            self.select_folder(account_id, &path);
            self.quiet.set(false);
        }
        // A highlight inside a folded account shows nothing (#170:
        // a notification click seemed to land nowhere); unfold it.
        self.reveal_account(account_id, &sender);
    }
}
