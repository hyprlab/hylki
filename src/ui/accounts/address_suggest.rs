//! Address suggestions under a plain entry row, as the composer gives its
//! recipient fields: the address being typed (after the last comma) is
//! matched against Contacts and mail history, and the choice replaces it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use crate::contacts::Suggestion;

/// How many suggestions the popover lists.
const MAX_SUGGESTIONS: usize = 8;

/// Split a comma-separated field into what is already written, comma and
/// space included, and the address being typed after it.
fn split_typed(text: &str) -> (&str, &str) {
    match text.rfind(',') {
        Some(i) => (&text[..=i], text[i + 1..].trim_start()),
        None => ("", text.trim_start()),
    }
}

/// The field after `email` replaced the address being typed: the earlier
/// addresses stay, and a comma and space wait for the next one.
fn complete_text(text: &str, email: &str) -> String {
    let (done, _) = split_typed(text);
    let done = done.trim_end();
    if done.is_empty() {
        format!("{email}, ")
    } else {
        format!("{done} {email}, ")
    }
}

/// The suggestions to offer for `text`: those matching the address being
/// typed, not already in the field, groups left out (the field takes plain
/// addresses), best first.
fn matches_for(all: &[Suggestion], text: &str) -> Vec<Suggestion> {
    let (done, token) = split_typed(text);
    let token = token.trim();
    if token.is_empty() {
        return Vec::new();
    }
    let present: Vec<String> = done
        .split(',')
        .map(|a| a.trim().to_lowercase())
        .filter(|a| !a.is_empty())
        .collect();
    let mut out: Vec<Suggestion> = all
        .iter()
        .filter(|s| s.group.is_none() && s.matches(token))
        .filter(|s| !present.contains(&s.email.to_lowercase()))
        .cloned()
        .collect();
    crate::contacts::rank(&mut out, token);
    out.truncate(MAX_SUGGESTIONS);
    out
}

/// Give `row` the composer's address suggestions. `own` are the accounts'
/// (name, address) pairs, which the suggestions rank last.
pub(super) fn attach(row: &adw::EntryRow, own: Vec<(String, String)>) {
    let all: Rc<RefCell<Vec<Suggestion>>> = Rc::default();
    {
        let all = all.clone();
        gtk::glib::spawn_future_local(async move {
            if let Ok(list) =
                gtk::gio::spawn_blocking(move || crate::contacts::suggestions(&own)).await
            {
                *all.borrow_mut() = list;
            }
        });
    }
    let popover = gtk::Popover::new();
    popover.set_autohide(false);
    popover.set_has_arrow(false);
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_halign(gtk::Align::Start);
    popover.set_parent(row);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    // Focus stays in the entry, whose key controller drives the list.
    list.set_can_focus(false);
    list.add_css_class("autocomplete");
    popover.set_child(Some(&list));
    let shown: Rc<RefCell<Vec<Suggestion>>> = Rc::default();
    let selected = Rc::new(Cell::new(0usize));
    // Set while the row rewrites its own text, so that is not taken for typing.
    let busy = Rc::new(Cell::new(false));

    let accept = {
        let (row, popover, busy) = (row.clone(), popover.clone(), busy.clone());
        move |sug: &Suggestion| {
            busy.set(true);
            row.set_text(&complete_text(&row.text(), &sug.email));
            row.set_position(-1);
            busy.set(false);
            popover.popdown();
        }
    };

    {
        let (list, popover, shown, selected, busy, all) = (
            list.clone(),
            popover.clone(),
            shown.clone(),
            selected.clone(),
            busy.clone(),
            all.clone(),
        );
        let accept = accept.clone();
        row.connect_changed(move |row| {
            if busy.get() {
                return;
            }
            let matches = matches_for(&all.borrow(), &row.text());
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            if matches.is_empty() {
                popover.popdown();
                shown.borrow_mut().clear();
                return;
            }
            for (i, sug) in matches.iter().enumerate() {
                let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
                text.set_margin_start(6);
                text.set_margin_end(6);
                text.set_margin_top(3);
                text.set_margin_bottom(3);
                let title = gtk::Label::new(Some(&sug.name));
                title.set_halign(gtk::Align::Start);
                title.set_xalign(0.0);
                text.append(&title);
                if sug.name != sug.email {
                    let sub = gtk::Label::new(Some(&sug.email));
                    sub.set_halign(gtk::Align::Start);
                    sub.set_xalign(0.0);
                    sub.add_css_class("dim-label");
                    sub.add_css_class("caption");
                    text.append(&sub);
                }
                let lbr = gtk::ListBoxRow::new();
                lbr.set_can_focus(false);
                lbr.set_child(Some(&text));
                list.append(&lbr);
                let gesture = gtk::GestureClick::new();
                let (sug, accept) = (sug.clone(), accept.clone());
                gesture.connect_released(move |_, _, _, _| accept(&sug));
                lbr.add_controller(gesture);
                let _ = i;
            }
            selected.set(0);
            if let Some(first) = list.row_at_index(0) {
                list.select_row(Some(&first));
            }
            *shown.borrow_mut() = matches;
            popover.set_width_request(row.width().max(240));
            popover.popup();
        });
    }

    // Arrows move through the list, Return or Tab take the highlighted one,
    // Escape closes it; with nothing showing every key is the row's own.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let (list, popover, shown, selected) = (
            list.clone(),
            popover.clone(),
            shown.clone(),
            selected.clone(),
        );
        keys.connect_key_pressed(move |_, key, _, _| {
            use gtk::gdk::Key;
            if !popover.is_visible() || shown.borrow().is_empty() {
                return gtk::glib::Propagation::Proceed;
            }
            let last = shown.borrow().len() - 1;
            let step = |to: usize| {
                selected.set(to);
                if let Some(r) = list.row_at_index(to as i32) {
                    list.select_row(Some(&r));
                }
            };
            match key {
                Key::Down => step((selected.get() + 1).min(last)),
                Key::Up => step(selected.get().saturating_sub(1)),
                Key::Return | Key::KP_Enter | Key::Tab => {
                    let sug = shown.borrow().get(selected.get()).cloned();
                    match sug {
                        Some(sug) => accept(&sug),
                        None => return gtk::glib::Propagation::Proceed,
                    }
                }
                Key::Escape => popover.popdown(),
                _ => return gtk::glib::Propagation::Proceed,
            }
            gtk::glib::Propagation::Stop
        });
    }
    row.add_controller(keys);
    {
        let popover = popover.clone();
        row.connect_destroy(move |_| popover.unparent());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sug(name: &str, email: &str, score: u32) -> Suggestion {
        Suggestion {
            name: name.into(),
            email: email.into(),
            from_contacts: true,
            from_directory: false,
            score,
            sent: 0,
            own: false,
            group: None,
        }
    }

    #[test]
    fn the_typed_address_is_what_follows_the_last_comma() {
        assert_eq!(split_typed("ada"), ("", "ada"));
        assert_eq!(split_typed("a@x.org, bo"), ("a@x.org,", "bo"));
        assert_eq!(split_typed("a@x.org,"), ("a@x.org,", ""));
    }

    #[test]
    fn completing_keeps_earlier_addresses_and_waits_for_the_next() {
        assert_eq!(complete_text("ad", "ada@x.org"), "ada@x.org, ");
        assert_eq!(
            complete_text("a@x.org, bo", "bob@y.org"),
            "a@x.org, bob@y.org, "
        );
    }

    #[test]
    fn matches_skip_what_the_field_has_and_rank_by_use() {
        let all = vec![
            sug("Ada", "ada@x.org", 1),
            sug("Adam", "adam@x.org", 5),
            sug("Bob", "bob@y.org", 9),
        ];
        let got = matches_for(&all, "ada@x.org, ad");
        let emails: Vec<&str> = got.iter().map(|s| s.email.as_str()).collect();
        assert_eq!(emails, vec!["adam@x.org"]);
        let got = matches_for(&all, "ad");
        assert_eq!(got[0].email, "adam@x.org");
        assert!(matches_for(&all, "").is_empty());
        assert!(matches_for(&all, "a@x.org, ").is_empty());
    }
}
