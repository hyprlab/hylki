//! Filter rules (#47): the list on the Filters page and the rule editor.

use super::action_editor::{action_summary, ActionList};
use super::condition_editor::{form_scaffold, CondAdder, CondControls, CondRows};
use super::*;

impl AccountsWindow {
    /// Human labels for the filter enums, shared by rows and the dialog.
    pub(super) fn field_label(f: crate::config::FilterField) -> String {
        use crate::config::FilterField::*;
        // One i18n() call per label: the string extractor only sees literals
        // written directly inside the call.
        match f {
            FromAddress => i18n("From address"),
            FromName => i18n("From name"),
            Subject => i18n("Subject"),
            Recipients => i18n("To or Cc"),
            ReplyTo => i18n("Reply-To address"),
            Body => i18n("Message body"),
            FromInContacts => i18n("Sender is in Contacts"),
            FromNotInContacts => i18n("Sender is not in Contacts"),
        }
    }

    pub(super) fn match_label(m: crate::config::FilterMatch) -> String {
        use crate::config::FilterMatch::*;
        match m {
            Contains => i18n("contains"),
            Equals => i18n("is exactly"),
            StartsWith => i18n("starts with"),
            EndsWith => i18n("ends with"),
        }
    }

    /// One condition as the rule rows print it: `Subject contains “a, b”`.
    /// A body condition is always a "contains", whatever it stores (#191).
    fn condition_label(c: &crate::config::FilterCondition) -> String {
        if c.field.is_contact_check() {
            return Self::field_label(c.field);
        }
        let matcher = if c.field == crate::config::FilterField::Body {
            crate::config::FilterMatch::Contains
        } else {
            c.matcher
        };
        format!(
            "{} {} \u{201c}{}\u{201d}",
            Self::field_label(c.field),
            Self::match_label(matcher),
            c.value
        )
    }

    /// Re-render the Filters group's rule rows.
    pub(super) fn rebuild_filter_rows(&self, sender: &ComponentSender<Self>) {
        let Some(list) = &self.filters_list else {
            return;
        };
        while let Some(row) = list.first_child() {
            list.remove(&row);
        }
        list.set_visible(!self.filter_rules.is_empty());
        for (i, r) in self.filter_rules.iter().enumerate() {
            let row = adw::ActionRow::new();
            // Roomier than a stock row: the stacked switches and the two-line
            // title were pressed against the row edges.
            row.add_css_class("filter-rule-row");
            // Activating the row opens it in the filter dialog for editing,
            // as the alias rows do.
            row.set_activatable(true);
            let s = sender.clone();
            row.connect_activated(move |_| s.input_sender().emit(AccountsInput::EditFilter(i)));
            // Every condition, joined the way the rule combines them (#192).
            let joiner = if r.any { i18n(" or ") } else { i18n(" and ") };
            let conditions: Vec<String> =
                r.conditions().iter().map(Self::condition_label).collect();
            let conditions = conditions.join(&joiner);
            // A named rule (#197) is listed by its name, with the conditions
            // moved down beside what it does; an unnamed one reads exactly
            // as it always did.
            let named = !r.name.trim().is_empty();
            row.set_title(if named {
                r.name.trim()
            } else {
                conditions.as_str()
            });
            // "account → folder", "account, tagged Work", or both (#71).
            let mut subtitle = String::new();
            if named {
                subtitle.push_str(&conditions);
                subtitle.push('\n');
            }
            subtitle.push_str(&r.account_email);
            if !r.dest_path.is_empty() {
                let dest = self
                    .folders_by_email
                    .get(&r.account_email)
                    .and_then(|fs| fs.iter().find(|(p, _)| *p == r.dest_path))
                    .map(|(_, name)| name.clone())
                    .unwrap_or_else(|| r.dest_path.clone());
                subtitle.push_str(&format!(" \u{2192} {dest}"));
            }
            for part in action_summary(r) {
                subtitle.push_str(&format!(", {part}"));
            }
            if !r.tag.is_empty() {
                let name = self
                    .tags
                    .iter()
                    .find(|t| t.keyword.eq_ignore_ascii_case(&r.tag))
                    .map(|t| t.name.clone())
                    .unwrap_or_else(|| r.tag.clone());
                subtitle.push_str(&i18n_f(", tagged {tag}", &[("tag", &name)]));
            }
            row.set_subtitle(&subtitle);
            // A named rule's subtitle runs to two lines (the conditions,
            // then where the mail goes); an unnamed one keeps the one it had.
            row.set_subtitle_lines(if named { 0 } else { 1 });
            // The trash button removes; a chevron says the row opens the
            // rule's editor, as the account and cloud rows do. The rule's
            // "count unread" switch lives in the editor.
            let rm = gtk::Button::from_icon_name("user-trash-symbolic");
            rm.add_css_class("flat");
            rm.set_valign(gtk::Align::Center);
            rm.set_tooltip_text(Some(i18n("Remove filter").as_str()));
            let s = sender.clone();
            rm.connect_clicked(move |_| s.input_sender().emit(AccountsInput::RemoveFilter(i)));
            row.add_suffix(&rm);
            let next = gtk::Image::from_icon_name("go-next-symbolic");
            next.add_css_class("dim-label");
            row.add_suffix(&next);
            list.append(&row);
        }
    }

    /// The Apply Now button while a filter run is under way (#198): a turning
    /// spinner beside a label saying so. A press meanwhile starts nothing.
    pub(super) fn set_filters_applying(&mut self, on: bool) {
        self.filters_applying = on;
        if let Some(spinner) = &self.apply_filters_spinner {
            spinner.set_visible(on);
            spinner.set_spinning(on);
        }
        if let Some(label) = &self.apply_filters_label {
            label.set_label(&if on {
                i18n("Applying…")
            } else {
                i18n("Apply Now")
            });
        }
    }

    /// The filter editor, a page of groups: the account; one group per
    /// condition (#192), each its own Where/Match/Text set with a remove
    /// button in its header; Add Condition and the all/any chooser; and
    /// "Then", what a match does. With `edit`, it opens on that existing
    /// rule (prefilled, "Save") and replaces it in place; otherwise it adds.
    fn open_filter_page(
        &self,
        nav: &adw::NavigationView,
        sender: &ComponentSender<Self>,
        edit: Option<(usize, crate::config::FilterRule)>,
        prefill: Option<crate::config::FilterRule>,
    ) {
        use crate::config::{FilterField, FilterMatch, FilterRule};
        let emails: Vec<String> = self.accounts.iter().map(|a| a.email.clone()).collect();
        if emails.is_empty() {
            return;
        }
        let (title, verb) = if edit.is_some() {
            (i18n("Edit Filter"), i18n("Save"))
        } else {
            (i18n("Add Filter"), i18n("Add Filter"))
        };

        let (page_box, scrolled) = form_scaffold();

        let account_group = adw::PreferencesGroup::new();
        // A name of your own for the rule (#197): what the filters list
        // calls it, in place of spelling its conditions out. Optional.
        let name_row = adw::EntryRow::new();
        name_row.set_title(&i18n("Name (optional)"));
        account_group.add(&name_row);
        let account_row = adw::ComboRow::new();
        account_row.set_title(&i18n("Account"));
        let email_refs: Vec<&str> = emails.iter().map(|s| s.as_str()).collect();
        account_row.set_model(Some(&gtk::StringList::new(&email_refs)));
        account_group.add(&account_row);

        // The conditions (#192): each a titled group of Where/Match/Text
        // rows with a remove button in its header (hidden while it is the
        // only one). Body conditions (#191) pin the matcher to "contains",
        // the only search a server offers, and say so.
        let conds: std::rc::Rc<std::cell::RefCell<Vec<CondRows>>> = Default::default();
        // Save stays dimmed until the rule has something to match and
        // something to do; every row that bears on that calls `changed`.
        // The check needs the Save button, so it is filled in last.
        type Check = std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn()>>>>;
        let revalidate: Check = Default::default();
        let changed = {
            let revalidate = revalidate.clone();
            move || {
                if let Some(check) = revalidate.borrow().as_ref() {
                    check();
                }
            }
        };
        let field_names: Vec<String> = FilterField::ALL
            .iter()
            .map(|f| Self::field_label(*f))
            .collect();
        let match_names: Vec<String> = FilterMatch::ALL
            .iter()
            .map(|m| Self::match_label(*m))
            .collect();
        let make_cond = {
            let field_names = field_names.clone();
            let match_names = match_names.clone();
            move |init: Option<&crate::config::FilterCondition>| -> CondRows {
                let rows = CondRows::new(&field_names, &match_names);
                let (field, matcher, value) = (&rows.field, &rows.matcher, &rows.value);
                if let Some(c) = init {
                    if let Some(i) = FilterField::ALL.iter().position(|f| *f == c.field) {
                        field.set_selected(i as u32);
                    }
                    if let Some(i) = FilterMatch::ALL.iter().position(|m| *m == c.matcher) {
                        matcher.set_selected(i as u32);
                    }
                    value.set_text(&c.value);
                }
                let explain = {
                    let matcher = matcher.clone();
                    let value = value.clone();
                    move |field: &adw::ComboRow| {
                        let chosen = FilterField::ALL.get(field.selected() as usize).copied();
                        let body = chosen == Some(FilterField::Body);
                        let contacts = chosen.is_some_and(|f| f.is_contact_check());
                        if body {
                            matcher.set_selected(0);
                        }
                        matcher.set_sensitive(!body && !contacts);
                        matcher.set_visible(!contacts);
                        value.set_visible(!contacts);
                        field.set_subtitle(&match chosen {
                            Some(FilterField::Body) => {
                                i18n("Searched on the server; nothing is downloaded.")
                            }
                            Some(FilterField::ReplyTo) => {
                                i18n("From address when no Reply-To is set")
                            }
                            Some(f) if f.is_contact_check() => {
                                i18n("Looks in the system address books and the Hylki Address Book.")
                            }
                            _ => String::new(),
                        });
                    }
                };
                explain(field);
                field.connect_selected_notify(explain);
                rows.add_rows();
                rows
            }
        };

        // Add Condition, and all or any (#192): the chooser is only shown
        // once there is a second condition.
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
                anchor: account_group.clone().upcast(),
                make: make_cond.clone(),
                relabel,
                form_save: self.form_save.clone(),
                nav: nav.clone(),
                changed: Some(changed.clone()),
            });
            move |init: Option<&crate::config::FilterCondition>| adder.add(init)
        };
        {
            let add_cond = add_cond.clone();
            add_row.connect_activated(move |_| add_cond(None));
        }

        // Then: what a match does (the action rows), and below them the
        // tag and the unread count.
        let own: Vec<(String, String)> = self
            .accounts
            .iter()
            .map(|a| (a.name.clone(), a.email.clone()))
            .collect();
        let actions = ActionList::new(std::rc::Rc::new(changed.clone()), own);
        // The Move rows offer the chosen account's folders.
        let folders = std::rc::Rc::new(self.folders_by_email.clone());
        let emails_rc = std::rc::Rc::new(emails.clone());
        let fill_dest = {
            let actions = actions.clone();
            let folders = folders.clone();
            let emails_rc = emails_rc.clone();
            move |idx: usize| {
                let empty = Vec::new();
                let list = emails_rc
                    .get(idx)
                    .and_then(|e| folders.get(e))
                    .unwrap_or(&empty);
                actions.set_destinations(list);
            }
        };
        fill_dest(0);
        {
            let fill_dest = fill_dest.clone();
            account_row.connect_selected_notify(move |row| fill_dest(row.selected() as usize));
        }

        let action_group = adw::PreferencesGroup::new();

        // Tag with (#71): none, or one of the tags.
        let tag_row = adw::ComboRow::new();
        tag_row.set_title(&i18n("Tag with"));
        let tags = self.tags.clone();
        {
            let none = i18n("None");
            let mut names: Vec<&str> = vec![none.as_str()];
            names.extend(tags.iter().map(|t| t.name.as_str()));
            tag_row.set_model(Some(&gtk::StringList::new(&names)));
        }
        {
            let changed = changed.clone();
            tag_row.connect_selected_notify(move |_| changed());
        }

        // Filed mail is still new mail: counted by default, so the unread
        // total stays true to what sits unread; a rule filing newsletters
        // can opt its folder out (#116).
        let count_row = adw::SwitchRow::new();
        count_row.set_title(&i18n("Count unread mail"));
        count_row.set_subtitle(&i18n(
            "Adds the folder’s unread mail to the tray icon’s count.",
        ));
        count_row.set_active(true);
        action_group.add(&tag_row);
        action_group.add(&count_row);

        // Editing: every field starts from the rule as it stands. A new
        // rule started from a message starts from its draft the same way,
        // and is added, not saved over another.
        let edit_index = edit.as_ref().map(|(i, _)| *i);
        let source = edit.as_ref().map(|(_, rule)| rule).or(prefill.as_ref());
        if let Some(rule) = source {
            name_row.set_text(&rule.name);
            if let Some(idx) = emails
                .iter()
                .position(|e| e.eq_ignore_ascii_case(&rule.account_email))
            {
                account_row.set_selected(idx as u32);
                fill_dest(idx);
            }
            combine_row.set_selected(if rule.any { 1 } else { 0 });
            actions.load(rule);
            if let Some(idx) = tags
                .iter()
                .position(|t| t.keyword.eq_ignore_ascii_case(&rule.tag))
            {
                tag_row.set_selected(idx as u32 + 1);
            }
            count_row.set_active(rule.count_unread);
        }

        page_box.append(&account_group);
        page_box.append(&add_group);
        page_box.append(&combine_group);
        page_box.append(&actions.group);
        page_box.append(&actions.add_group);
        page_box.append(&action_group);
        // The condition groups slot in above Add Condition: the rule's own
        // when editing, one blank one otherwise.
        match source {
            Some(rule) => {
                for c in rule.conditions() {
                    add_cond(Some(&c));
                }
            }
            None => add_cond(None),
        }

        showcase_open_row(&tag_row, &conds, &account_row);
        // HYLKI_SHOWCASE_ADD_CONDITION=<n> adds that many blank conditions
        // once the page is up, for a capture of the stacked groups.
        if let Some(Ok(n)) = std::env::var("HYLKI_SHOWCASE_ADD_CONDITION")
            .ok()
            .map(|v| v.parse::<usize>())
        {
            let add_cond = add_cond.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(1), move || {
                for _ in 0..n {
                    add_cond(None);
                }
            });
        }

        // The rule as the form stands, or None while it cannot be saved.
        let entries = vec![name_row.clone()];
        // When the rule being edited started forwarding, if it did.
        let forward_before: Option<(i64, Vec<String>)> = edit
            .as_ref()
            .filter(|(_, r)| !r.forward_to.is_empty())
            .map(|(_, r)| (r.forward_since, r.forward_to.clone()));
        let build = move || -> Option<FilterRule> {
            // Every condition with text; a blank extra one is dropped. The
            // matcher of a body condition is whatever the pinned row says.
            let conditions: Vec<crate::config::FilterCondition> = conds
                .borrow()
                .iter()
                .filter_map(|c| {
                    let field =
                        FilterField::ALL[c.field.selected() as usize % FilterField::ALL.len()];
                    let value = c.value.text().trim().to_string();
                    if value.is_empty() && !field.is_contact_check() {
                        return None;
                    }
                    Some(crate::config::FilterCondition {
                        field,
                        matcher: FilterMatch::ALL
                            [c.matcher.selected() as usize % FilterMatch::ALL.len()],
                        value,
                    })
                })
                .collect();
            let email = emails.get(account_row.selected() as usize)?;
            let acts = actions.read()?;
            let tag = match tag_row.selected() {
                0 => String::new(),
                i => tags
                    .get(i as usize - 1)
                    .map(|t| t.keyword.clone())
                    .unwrap_or_default(),
            };
            let mut rule = FilterRule {
                name: name_row.text().trim().to_string(),
                account_email: email.clone(),
                field: FilterField::FromAddress,
                matcher: FilterMatch::Contains,
                value: String::new(),
                more: Vec::new(),
                any: combine_row.selected() == 1,
                dest_path: acts.dest_path,
                tag,
                count_unread: count_row.is_active(),
                // Forwarding starts from the moment the Forward was added,
                // so the mail already in the folder is never sent on.
                forward_since: crate::config::forward_start(
                    forward_before
                        .as_ref()
                        .map(|(since, to)| (*since, to.as_slice())),
                    &acts.forward_to,
                    crate::datefmt::now(),
                ),
                forward_to: acts.forward_to,
                mark_read: acts.mark_read,
                delete: acts.delete,
            };
            // A rule needs something to match and something to do.
            if conditions.is_empty() || !rule.has_action() {
                return None;
            }
            rule.set_conditions(conditions);
            Some(rule)
        };
        let build = std::rc::Rc::new(build);

        let s = sender.clone();
        let save_btn = {
            let build = build.clone();
            self.push_form_content(
                nav,
                "filter",
                &title,
                &verb,
                scrolled.upcast_ref(),
                entries,
                move || {
                    let Some(rule) = build() else {
                        return false;
                    };
                    s.input_sender().emit(match edit_index {
                        Some(i) => AccountsInput::FilterEdited(i, rule),
                        None => AccountsInput::FilterAdded(rule),
                    });
                    true
                },
            )
        };
        // Grey, not a faded blue, while it cannot save.
        *revalidate.borrow_mut() = Some(Box::new(move || {
            let ready = build().is_some();
            save_btn.set_sensitive(ready);
            if ready {
                save_btn.add_css_class("suggested-action");
            } else {
                save_btn.remove_css_class("suggested-action");
            }
        }));
        changed();
    }
}

impl AccountsWindow {
    pub(super) fn on_search_filters(&mut self, text: String, widgets: &AccountsWindowWidgets) {
        *self.filters_query.borrow_mut() = text.to_lowercase();
        widgets.filters.filters_list.invalidate_filter();
    }

    pub(super) fn on_add_filter(
        &mut self,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        self.open_filter_page(&widgets.nav, &sender, None, None);
    }

    pub(super) fn on_add_filter_from(
        &mut self,
        rule: crate::config::FilterRule,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        self.open_filter_page(&widgets.nav, &sender, None, Some(rule));
    }

    pub(super) fn on_apply_filters(&mut self, sender: ComponentSender<Self>) {
        if !self.filters_applying {
            self.set_filters_applying(true);
            let _ = sender.output(AccountsOutput::ApplyFilters);
        }
    }

    pub(super) fn on_edit_filter(
        &mut self,
        i: usize,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        if let Some(rule) = self.filter_rules.get(i).cloned() {
            self.open_filter_page(&widgets.nav, &sender, Some((i, rule)), None);
        }
    }

    pub(super) fn on_filter_edited(
        &mut self,
        i: usize,
        rule: crate::config::FilterRule,
        sender: ComponentSender<Self>,
    ) {
        if let Some(slot) = self.filter_rules.get_mut(i) {
            if *slot != rule {
                *slot = rule;
                self.rebuild_filter_rows(&sender);
                let _ = sender.output(AccountsOutput::SetFilters(self.filter_rules.clone()));
            }
        }
    }

    pub(super) fn on_remove_filter(&mut self, i: usize, sender: ComponentSender<Self>) {
        if i < self.filter_rules.len() {
            self.filter_rules.remove(i);
            self.rebuild_filter_rows(&sender);
            let _ = sender.output(AccountsOutput::SetFilters(self.filter_rules.clone()));
        }
    }

    pub(super) fn on_filter_added(
        &mut self,
        rule: crate::config::FilterRule,
        sender: ComponentSender<Self>,
    ) {
        self.filter_rules.push(rule);
        self.rebuild_filter_rows(&sender);
        let _ = sender.output(AccountsOutput::SetFilters(self.filter_rules.clone()));
    }
}

/// Showcase hook: HYLKI_SHOWCASE_OPEN_ROW=tag|where|match|account
/// activates that combo row once the page is up and logs whether its
/// popover opened and how many choices it holds, the way a click would
/// (the "Move to" list was found empty this way, 2026-09-14).
fn showcase_open_row(
    tag_row: &adw::ComboRow,
    conds: &std::rc::Rc<std::cell::RefCell<Vec<CondRows>>>,
    account_row: &adw::ComboRow,
) {
    if let Ok(which) = std::env::var("HYLKI_SHOWCASE_OPEN_ROW") {
        let row: adw::ComboRow = match which.as_str() {
            "tag" => tag_row.clone(),
            "where" => conds.borrow()[0].field.clone(),
            "match" => conds.borrow()[0].matcher.clone(),
            _ => account_row.clone(),
        };
        gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || {
            gtk::prelude::WidgetExt::activate(&row);
            gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(1), move || {
                fn find_popover(w: &gtk::Widget) -> Option<gtk::Popover> {
                    if let Some(p) = w.downcast_ref::<gtk::Popover>() {
                        return Some(p.clone());
                    }
                    let mut c = w.first_child();
                    while let Some(child) = c {
                        if let Some(p) = find_popover(&child) {
                            return Some(p);
                        }
                        c = child.next_sibling();
                    }
                    None
                }
                tracing::info!(
                    "showcase: {which} row choices={} popover visible={:?}",
                    row.model().map(|m| m.n_items()).unwrap_or(0),
                    find_popover(row.upcast_ref()).map(|p| p.is_visible()),
                );
            });
        });
    }
}
