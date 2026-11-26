//! The "Then" part of the filter editor: one row per action a rule takes
//! (Forward, Mark as Read, Move, Delete), added and removed on the page.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::config::{parse_forward_addresses, FilterRule};
use crate::i18n::{i18n, i18n_f};

/// The kinds of action a rule can hold, in the order the menu lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ActionKind {
    Forward,
    MarkRead,
    Move,
    Delete,
}

impl ActionKind {
    const ALL: [ActionKind; 4] = [
        ActionKind::Forward,
        ActionKind::MarkRead,
        ActionKind::Move,
        ActionKind::Delete,
    ];

    /// The menu entry for this kind.
    fn label(self) -> String {
        match self {
            ActionKind::Forward => i18n("Forward to"),
            ActionKind::MarkRead => i18n("Mark as Read"),
            ActionKind::Move => i18n("Move to"),
            ActionKind::Delete => i18n("Delete"),
        }
    }
}

/// What the actions of a rule add to its line in the filters list, after the
/// destination folder: `forwards to a@x.org`, `marked as read`, `deleted`.
pub(super) fn action_summary(rule: &FilterRule) -> Vec<String> {
    let mut parts = Vec::new();
    if !rule.forward_to.is_empty() {
        parts.push(i18n_f(
            "forwards to {to}",
            &[("to", &rule.forward_to.join(", "))],
        ));
    }
    if rule.mark_read {
        parts.push(i18n("marked as read"));
    }
    if rule.delete {
        parts.push(i18n("deleted for good"));
    }
    parts
}

/// One action's rows.
struct ActionRow {
    kind: adw::ComboRow,
    forward: adw::EntryRow,
    dest: adw::ComboRow,
}

impl ActionRow {
    fn selected(&self) -> ActionKind {
        ActionKind::ALL[self.kind.selected() as usize % ActionKind::ALL.len()]
    }
}

/// The folders the Move rows offer: the chosen account's, as display names
/// and server paths side by side.
#[derive(Default)]
struct Destinations {
    names: Vec<String>,
    paths: Vec<String>,
}

/// The actions group of the filter editor and the rows in it.
#[derive(Clone)]
pub(super) struct ActionList {
    /// Titled "Then"; holds the action rows.
    pub(super) group: adw::PreferencesGroup,
    /// The Add Action row, in a group of its own below.
    pub(super) add_group: adw::PreferencesGroup,
    rows: Rc<RefCell<Vec<ActionRow>>>,
    dests: Rc<RefCell<Destinations>>,
    changed: Rc<dyn Fn()>,
    own: Vec<(String, String)>,
}

impl ActionList {
    /// An empty list; `changed` is called whenever a row is added, removed
    /// or edited.
    /// `own` are the accounts' (name, address) pairs, for the address
    /// suggestions of the Forward rows.
    pub(super) fn new(changed: Rc<dyn Fn()>, own: Vec<(String, String)>) -> Self {
        let group = adw::PreferencesGroup::new();
        group.set_title(&i18n("Then"));
        let add_group = adw::PreferencesGroup::new();
        let add_row = adw::ActionRow::new();
        add_row.set_title(&i18n("Add Action"));
        add_row.set_activatable(true);
        add_row.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
        add_group.add(&add_row);
        let list = ActionList {
            group,
            add_group,
            rows: Default::default(),
            dests: Default::default(),
            changed,
            own,
        };
        let adder = list.clone();
        add_row.connect_activated(move |_| adder.add(None));
        list
    }

    /// Offer the folders of the chosen account to every Move row.
    pub(super) fn set_destinations(&self, folders: &[(String, String)]) {
        {
            let mut d = self.dests.borrow_mut();
            d.names = folders.iter().map(|(_, n)| n.clone()).collect();
            d.paths = folders.iter().map(|(p, _)| p.clone()).collect();
        }
        for row in self.rows.borrow().iter() {
            self.fill_dest(&row.dest, "");
        }
    }

    /// Put the folder list in a Move row, keeping `keep` (a path) selected.
    fn fill_dest(&self, dest: &adw::ComboRow, keep: &str) {
        let d = self.dests.borrow();
        let names: Vec<&str> = d.names.iter().map(String::as_str).collect();
        dest.set_model(Some(&gtk::StringList::new(&names)));
        if let Some(i) = d.paths.iter().position(|p| p == keep) {
            dest.set_selected(i as u32);
        }
    }

    /// Fill the list from `rule`: one row per action it takes.
    pub(super) fn load(&self, rule: &FilterRule) {
        if !rule.forward_to.is_empty() {
            self.add(Some((ActionKind::Forward, rule.forward_to.join(", "))));
        }
        if rule.mark_read {
            self.add(Some((ActionKind::MarkRead, String::new())));
        }
        if !rule.dest_path.is_empty() {
            self.add(Some((ActionKind::Move, rule.dest_path.clone())));
        }
        if rule.delete {
            self.add(Some((ActionKind::Delete, String::new())));
        }
    }

    /// Add a row: blank (Forward), or set to a kind with its value (the
    /// addresses of a Forward, the path of a Move).
    fn add(&self, init: Option<(ActionKind, String)>) {
        let kind = adw::ComboRow::new();
        kind.set_title(&i18n("Action"));
        let labels: Vec<String> = ActionKind::ALL.iter().map(|k| k.label()).collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        kind.set_model(Some(&gtk::StringList::new(&refs)));
        let trash = gtk::Button::from_icon_name("user-trash-symbolic");
        trash.add_css_class("flat");
        trash.set_valign(gtk::Align::Center);
        trash.set_tooltip_text(Some(i18n("Remove action").as_str()));
        kind.add_suffix(&trash);
        let forward = adw::EntryRow::new();
        forward.set_title(&i18n("Addresses, separated by commas"));
        super::address_suggest::attach(&forward, self.own.clone());
        let dest = adw::ComboRow::new();
        dest.set_title(&i18n("Folder"));
        let (init_kind, value) = init.unwrap_or((ActionKind::Forward, String::new()));
        self.fill_dest(
            &dest,
            if init_kind == ActionKind::Move {
                &value
            } else {
                ""
            },
        );
        if init_kind == ActionKind::Forward {
            forward.set_text(&value);
        }
        kind.set_selected(
            ActionKind::ALL
                .iter()
                .position(|k| *k == init_kind)
                .unwrap_or(0) as u32,
        );
        let row = ActionRow {
            kind: kind.clone(),
            forward: forward.clone(),
            dest: dest.clone(),
        };
        let show = {
            let (forward, dest) = (forward.clone(), dest.clone());
            move |kind: &adw::ComboRow| {
                let chosen = ActionKind::ALL[kind.selected() as usize % ActionKind::ALL.len()];
                forward.set_visible(chosen == ActionKind::Forward);
                dest.set_visible(chosen == ActionKind::Move);
                kind.set_subtitle(&if chosen == ActionKind::Delete {
                    i18n("Permanently deletes the message.")
                } else {
                    String::new()
                });
            }
        };
        show(&kind);
        {
            let changed = self.changed.clone();
            kind.connect_selected_notify(move |k| {
                show(k);
                changed();
            });
        }
        {
            let changed = self.changed.clone();
            forward.connect_changed(move |_| changed());
        }
        {
            let changed = self.changed.clone();
            dest.connect_selected_notify(move |_| changed());
        }
        self.group.add(&kind);
        self.group.add(&forward);
        self.group.add(&dest);
        {
            let list = self.clone();
            trash.connect_clicked(move |_| {
                list.group.remove(&kind);
                list.group.remove(&forward);
                list.group.remove(&dest);
                list.rows.borrow_mut().retain(|r| r.kind != kind);
                (list.changed)();
            });
        }
        self.rows.borrow_mut().push(row);
        (self.changed)();
    }

    /// The actions as the rows stand, or `None` while they cannot be
    /// saved: a kind used twice, Move together with Delete, a Move with no
    /// folder, a Forward with no address or with one that is not an
    /// address (that row is marked).
    pub(super) fn read(&self) -> Option<Actions> {
        let mut out = Actions::default();
        let mut seen: Vec<ActionKind> = Vec::new();
        let mut valid = true;
        let dests = self.dests.borrow();
        for row in self.rows.borrow().iter() {
            let kind = row.selected();
            if seen.contains(&kind) {
                valid = false;
            }
            seen.push(kind);
            match kind {
                ActionKind::Forward => match parse_forward_addresses(&row.forward.text()) {
                    Ok(to) if !to.is_empty() => {
                        row.forward.remove_css_class("error");
                        out.forward_to = to;
                    }
                    Ok(_) => valid = false,
                    Err(_) => {
                        row.forward.add_css_class("error");
                        valid = false;
                    }
                },
                ActionKind::MarkRead => out.mark_read = true,
                ActionKind::Move => match dests.paths.get(row.dest.selected() as usize) {
                    Some(path) => out.dest_path = path.clone(),
                    None => valid = false,
                },
                ActionKind::Delete => out.delete = true,
            }
        }
        if out.delete && !out.dest_path.is_empty() {
            valid = false;
        }
        valid.then_some(out)
    }
}

/// What the rows of an [`ActionList`] add up to.
#[derive(Default, Debug, PartialEq, Eq)]
pub(super) struct Actions {
    pub(super) forward_to: Vec<String>,
    pub(super) mark_read: bool,
    pub(super) dest_path: String,
    pub(super) delete: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FilterField, FilterMatch};

    fn rule() -> FilterRule {
        let mut r = FilterRule::draft_from_message("a@b.c", "x@y.z", "");
        r.field = FilterField::FromAddress;
        r.matcher = FilterMatch::Equals;
        r.dest_path = String::new();
        r
    }

    #[test]
    fn summary_lists_every_action() {
        let mut r = rule();
        assert!(action_summary(&r).is_empty());
        r.forward_to = vec!["a@x.org".into(), "b@x.org".into()];
        r.mark_read = true;
        r.delete = true;
        assert_eq!(
            action_summary(&r),
            vec![
                "forwards to a@x.org, b@x.org".to_string(),
                "marked as read".to_string(),
                "deleted for good".to_string()
            ]
        );
    }
}
