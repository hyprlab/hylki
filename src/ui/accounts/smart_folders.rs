//! Smart folders (#417): saved searches, their list and their editor.

use super::condition_editor::{form_scaffold, CondAdder, CondControls, CondRows};
use super::*;

/// A smart-folder condition: the shared rows, and the tag a Tagged
/// condition looks for.
struct SmartCondRows {
    rows: CondRows,
    tag: adw::ComboRow,
}

impl AsRef<CondRows> for SmartCondRows {
    fn as_ref(&self) -> &CondRows {
        &self.rows
    }
}

impl AccountsWindow {
    /// The Smart Folders list (#417): a row per folder, saying where it
    /// looks, activating to edit, with a remove button.
    pub(super) fn rebuild_smart_rows(&self, sender: &ComponentSender<Self>) {
        let Some(list) = &self.smart_list else { return };
        while let Some(row) = list.first_child() {
            list.remove(&row);
        }
        list.set_visible(!self.smart_folders.is_empty());
        for (i, f) in self.smart_folders.iter().enumerate() {
            let row = adw::ActionRow::new();
            row.set_activatable(true);
            let s = sender.clone();
            row.connect_activated(move |_| {
                s.input_sender().emit(AccountsInput::EditSmartFolder(i))
            });
            row.set_title(&gtk::glib::markup_escape_text(&f.name));
            let account = if f.account_email.is_empty() {
                i18n("All accounts")
            } else {
                f.account_email.clone()
            };
            let folder = f
                .folder
                .map_or_else(|| i18n("All but Trash and Junk"), Self::smart_kind_label);
            row.set_subtitle(&gtk::glib::markup_escape_text(&format!(
                "{account} · {folder}"
            )));
            row.add_prefix(&gtk::Image::from_icon_name("folder-saved-search-symbolic"));
            let rm = gtk::Button::from_icon_name("user-trash-symbolic");
            rm.add_css_class("flat");
            rm.set_valign(gtk::Align::Center);
            rm.set_tooltip_text(Some(i18n("Remove smart folder").as_str()));
            let s = sender.clone();
            rm.connect_clicked(move |_| s.input_sender().emit(AccountsInput::RemoveSmartFolder(i)));
            row.add_suffix(&rm);
            list.append(&row);
        }
    }

    fn smart_kind_label(k: crate::config::SmartFolderKind) -> String {
        use crate::config::SmartFolderKind::*;
        match k {
            Inbox => i18n("Inbox"),
            Sent => i18n("Sent"),
            Drafts => i18n("Drafts"),
            Archive => i18n("Archive"),
            Junk => i18n("Junk"),
            Trash => i18n("Trash"),
        }
    }

    /// A Where choice: the field, or its negation for the fields the
    /// choice turns around (#639).
    fn smart_field_label((f, not): (crate::config::SmartField, bool)) -> String {
        use crate::config::SmartField::*;
        match (f, not) {
            (Starred, _) => i18n("Starred"),
            (NotStarred, _) => i18n("Not starred"),
            (Unread, _) => i18n("Unread"),
            (Read, _) => i18n("Read"),
            (Attachment, false) => i18n("Has attachments"),
            (Attachment, true) => i18n("No attachments"),
            (Tagged, false) => i18n("Tagged"),
            (Tagged, true) => i18n("Not tagged"),
            (Label, _) => i18n("Gmail label"),
            (Body, _) => i18n("Message preview"),
            (other, _) => other
                .filter_field()
                .map(Self::field_label)
                .unwrap_or_default(),
        }
    }

    /// The Match choices: each matcher, then its negation (#639).
    fn smart_match_choices() -> Vec<(crate::config::FilterMatch, bool)> {
        crate::config::FilterMatch::ALL
            .iter()
            .flat_map(|m| [(*m, false), (*m, true)])
            .collect()
    }

    fn smart_match_label((m, not): (crate::config::FilterMatch, bool)) -> String {
        use crate::config::FilterMatch::*;
        match (m, not) {
            (_, false) => Self::match_label(m),
            (Contains, true) => i18n("does not contain"),
            (Equals, true) => i18n("is not"),
            (StartsWith, true) => i18n("does not start with"),
            (EndsWith, true) => i18n("does not end with"),
        }
    }

    /// The smart folder editor (#417): a name, where it looks, and its
    /// conditions, each a group of its own as in the filter editor. With
    /// `edit` it opens on that folder and saves over it.
    pub(super) fn open_smart_page(
        &self,
        nav: &adw::NavigationView,
        sender: &ComponentSender<Self>,
        edit: Option<(usize, crate::config::SmartFolder)>,
    ) {
        use crate::config::{
            FilterMatch, SmartCondition, SmartField, SmartFolder, SmartFolderKind,
        };
        let (title, verb) = if edit.is_some() {
            (i18n("Edit Smart Folder"), i18n("Save"))
        } else {
            (i18n("Add Smart Folder"), i18n("Add Smart Folder"))
        };
        let emails: Vec<String> = self.accounts.iter().map(|a| a.email.clone()).collect();

        let (page_box, scrolled) = form_scaffold();

        let where_group = adw::PreferencesGroup::new();
        let name_row = adw::EntryRow::new();
        name_row.set_title(&i18n("Name"));
        where_group.add(&name_row);
        let account_row = adw::ComboRow::new();
        account_row.set_title(&i18n("Account"));
        {
            let all = i18n("All accounts");
            let mut names: Vec<&str> = vec![all.as_str()];
            names.extend(emails.iter().map(|e| e.as_str()));
            account_row.set_model(Some(&gtk::StringList::new(&names)));
        }
        account_row.set_subtitle(&i18n("Those in the unified section."));
        where_group.add(&account_row);
        let folder_row = adw::ComboRow::new();
        folder_row.set_title(&i18n("Folders"));
        {
            let mut names = vec![i18n("All but Trash and Junk")];
            names.extend(
                SmartFolderKind::ALL
                    .iter()
                    .map(|k| Self::smart_kind_label(*k)),
            );
            let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            folder_row.set_model(Some(&gtk::StringList::new(&refs)));
        }
        where_group.add(&folder_row);

        // The conditions: Where, then Match and Text for the fields that
        // look at text, or the tag for Tagged.
        let conds: std::rc::Rc<std::cell::RefCell<Vec<SmartCondRows>>> = Default::default();
        let field_names: Vec<String> = SmartField::CHOICES
            .iter()
            .map(|f| Self::smart_field_label(*f))
            .collect();
        let match_choices = Self::smart_match_choices();
        let match_names: Vec<String> = match_choices
            .iter()
            .map(|m| Self::smart_match_label(*m))
            .collect();
        let tags = self.tags.clone();
        let tag_names: Vec<String> = std::iter::once(i18n("Any tag"))
            .chain(tags.iter().map(|t| t.name.clone()))
            .collect();
        let make_cond = {
            let tags = tags.clone();
            let match_choices = match_choices.clone();
            move |init: Option<&SmartCondition>| -> SmartCondRows {
                let rows = CondRows::new(&field_names, &match_names);
                let (field, matcher, value) = (&rows.field, &rows.matcher, &rows.value);
                let tag = adw::ComboRow::new();
                tag.set_title(&i18n("Tag"));
                let refs: Vec<&str> = tag_names.iter().map(|s| s.as_str()).collect();
                tag.set_model(Some(&gtk::StringList::new(&refs)));
                if let Some(c) = init {
                    let by_choice = c.not && c.field.negated_by_choice();
                    if let Some(i) = SmartField::CHOICES
                        .iter()
                        .position(|f| *f == (c.field, by_choice))
                    {
                        field.set_selected(i as u32);
                    }
                    let by_match = c.not && c.field.takes_text();
                    if let Some(i) = match_choices
                        .iter()
                        .position(|m| *m == (c.matcher, by_match))
                    {
                        matcher.set_selected(i as u32);
                    }
                    if c.field == SmartField::Tagged {
                        if let Some(i) = tags
                            .iter()
                            .position(|t| t.keyword.eq_ignore_ascii_case(&c.value))
                        {
                            tag.set_selected(i as u32 + 1);
                        }
                    } else {
                        value.set_text(&c.value);
                    }
                }
                // The preview is only ever searched for words in it, so
                // it keeps to "contains" and "does not contain".
                let body_match = {
                    let field = field.clone();
                    move |matcher: &adw::ComboRow| {
                        let chosen = SmartField::CHOICES.get(field.selected() as usize);
                        if chosen.is_some_and(|(f, _)| *f == SmartField::Body)
                            && matcher.selected() > 1
                        {
                            matcher.set_selected(matcher.selected() % 2);
                        }
                    }
                };
                matcher.connect_selected_notify(body_match.clone());
                let explain = {
                    let (matcher, value, tag) = (matcher.clone(), value.clone(), tag.clone());
                    move |field: &adw::ComboRow| {
                        let chosen = SmartField::CHOICES
                            .get(field.selected() as usize)
                            .map(|(f, _)| *f);
                        let text = chosen.is_some_and(|f| f.takes_text());
                        body_match(&matcher);
                        matcher.set_visible(text);
                        value.set_visible(text);
                        tag.set_visible(chosen == Some(SmartField::Tagged));
                        field.set_subtitle(&match chosen {
                            Some(SmartField::Label) => i18n("With no text, any label matches."),
                            Some(SmartField::Body) => {
                                i18n("The preview each message has in the list")
                            }
                            Some(SmartField::ReplyTo) => {
                                i18n("From address when no Reply-To is set")
                            }
                            Some(SmartField::FromInContacts | SmartField::FromNotInContacts) => {
                                i18n("Looks in the system address books and the Hylki book")
                            }
                            _ => String::new(),
                        });
                    }
                };
                explain(field);
                field.connect_selected_notify(explain);
                rows.add_rows();
                rows.group.add(&tag);
                SmartCondRows { rows, tag }
            }
        };

        let controls = CondControls::new();
        let relabel = controls.relabeller(&conds);
        let CondControls {
            add_group,
            add_row,
            combine_row,
            combine_group,
        } = controls;
        let add_cond = {
            let adder = std::rc::Rc::new(CondAdder {
                conds: conds.clone(),
                page_box: page_box.clone(),
                anchor: where_group.clone().upcast(),
                make: make_cond,
                relabel,
                form_save: self.form_save.clone(),
                nav: nav.clone(),
                changed: None::<fn()>,
            });
            move |init: Option<&SmartCondition>| adder.add(init)
        };
        {
            let add_cond = add_cond.clone();
            add_row.connect_activated(move |_| add_cond(None));
        }

        let edit_index = edit.as_ref().map(|(i, _)| *i);
        let id = edit
            .as_ref()
            .map_or_else(crate::config::new_smart_folder_id, |(_, f)| f.id.clone());
        if let Some((_, f)) = &edit {
            name_row.set_text(&f.name);
            if let Some(i) = emails
                .iter()
                .position(|e| e.eq_ignore_ascii_case(&f.account_email))
            {
                account_row.set_selected(i as u32 + 1);
            }
            if let Some(i) = f
                .folder
                .and_then(|k| SmartFolderKind::ALL.iter().position(|x| *x == k))
            {
                folder_row.set_selected(i as u32 + 1);
            }
            combine_row.set_selected(u32::from(f.any));
        }

        page_box.append(&where_group);
        page_box.append(&add_group);
        page_box.append(&combine_group);
        match &edit {
            Some((_, f)) if !f.conditions.is_empty() => {
                for c in &f.conditions {
                    add_cond(Some(c));
                }
            }
            _ => add_cond(None),
        }

        let s = sender.clone();
        self.push_form_content(
            nav,
            "smart",
            &title,
            &verb,
            scrolled.upcast_ref(),
            vec![name_row.clone()],
            move || {
                let name = name_row.text().trim().to_string();
                // A text condition with no text says nothing and is dropped,
                // but for a Gmail label, where it means any label; the
                // others need none.
                let conditions: Vec<SmartCondition> = conds
                    .borrow()
                    .iter()
                    .filter_map(|c| {
                        let (field, field_not) = SmartField::CHOICES
                            [c.rows.field.selected() as usize % SmartField::CHOICES.len()];
                        let (matcher, match_not) =
                            match_choices[c.rows.matcher.selected() as usize % match_choices.len()];
                        let value = if field == SmartField::Tagged {
                            match c.tag.selected() {
                                0 => String::new(),
                                i => tags
                                    .get(i as usize - 1)
                                    .map(|t| t.keyword.clone())
                                    .unwrap_or_default(),
                            }
                        } else if field.takes_text() {
                            let v = c.rows.value.text().trim().to_string();
                            if v.is_empty() && field != SmartField::Label {
                                return None;
                            }
                            v
                        } else {
                            String::new()
                        };
                        let text = field.takes_text();
                        Some(SmartCondition {
                            field,
                            matcher: if text { matcher } else { FilterMatch::Contains },
                            value,
                            not: if text { match_not } else { field_not },
                        })
                    })
                    .collect();
                if name.is_empty() || conditions.is_empty() {
                    return false;
                }
                let account_email = match account_row.selected() {
                    0 => String::new(),
                    i => emails.get(i as usize - 1).cloned().unwrap_or_default(),
                };
                let folder = match folder_row.selected() {
                    0 => None,
                    i => SmartFolderKind::ALL.get(i as usize - 1).copied(),
                };
                let folder = SmartFolder {
                    id: id.clone(),
                    name,
                    account_email,
                    folder,
                    any: combine_row.selected() == 1,
                    conditions,
                };
                s.input_sender()
                    .emit(AccountsInput::SmartFolderSaved(edit_index, folder));
                true
            },
        );
    }
}

impl AccountsWindow {
    pub(super) fn on_edit_smart_folder(
        &mut self,
        i: usize,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        if let Some(f) = self.smart_folders.get(i).cloned() {
            self.open_smart_page(&widgets.nav, &sender, Some((i, f)));
        }
    }

    pub(super) fn on_remove_smart_folder(&mut self, i: usize, sender: ComponentSender<Self>) {
        if i < self.smart_folders.len() {
            self.smart_folders.remove(i);
            self.rebuild_smart_rows(&sender);
            let _ = sender.output(AccountsOutput::SetSmartFolders(self.smart_folders.clone()));
        }
    }

    pub(super) fn on_smart_folder_saved(
        &mut self,
        at: Option<usize>,
        folder: crate::config::SmartFolder,
        sender: ComponentSender<Self>,
    ) {
        match at.filter(|i| *i < self.smart_folders.len()) {
            Some(i) => self.smart_folders[i] = folder,
            None => self.smart_folders.push(folder),
        }
        self.rebuild_smart_rows(&sender);
        let _ = sender.output(AccountsOutput::SetSmartFolders(self.smart_folders.clone()));
    }

    pub(super) fn on_set_smart_folders(
        &mut self,
        folders: Vec<crate::config::SmartFolder>,
        sender: ComponentSender<Self>,
    ) {
        if folders != self.smart_folders {
            self.smart_folders = folders;
            self.rebuild_smart_rows(&sender);
        }
    }
}
