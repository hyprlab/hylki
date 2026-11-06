//! A small searchable list dialog that picks one message: a draft to add
//! files to, a message to reply to, a template.

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::i18n;
use crate::models::Message;

/// A dialog listing messages to pick one from: a draft to continue or a
/// message to reply to for handed-in files, or a template to start from
/// (#360). `items` pairs each message with its account's name; a search box
/// narrows them by sender, subject or account. `new_instead` offers a way to
/// start a fresh message; without it the dialog only picks. `on_pick` gets
/// `hand_off` back with the choice, or `None` for a new message; Cancel drops
/// it.
#[allow(clippy::too_many_arguments)]
#[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
pub(super) fn show_message_picker<T: 'static>(
    parent: &gtk::Window,
    heading: &str,
    body: &str,
    empty_text: &str,
    pick_label: &str,
    items: Vec<(Message, String)>,
    hand_off: T,
    new_instead: bool,
    on_pick: impl Fn(T, Option<Message>) + 'static,
) {
    let dialog = adw::MessageDialog::new(Some(parent), Some(heading), Some(body));
    dialog.add_response("cancel", &i18n("Cancel"));
    if new_instead {
        dialog.add_response("new", &i18n("New Message Instead"));
    }
    dialog.add_response("pick", pick_label);
    dialog.set_response_appearance("pick", adw::ResponseAppearance::Suggested);
    dialog.set_response_enabled("pick", false);
    dialog.set_close_response("cancel");

    let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
    column.set_width_request(460);
    let items = std::rc::Rc::new(items);
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::Single);
    if items.is_empty() {
        let empty = gtk::Label::new(Some(empty_text));
        empty.add_css_class("dim-label");
        empty.set_margin_top(12);
        empty.set_margin_bottom(12);
        column.append(&empty);
    } else {
        fill_picker(&dialog, &column, &list, &items);
    }
    dialog.set_extra_child(Some(&column));

    let hand_off = std::rc::Rc::new(std::cell::RefCell::new(Some(hand_off)));
    dialog.connect_response(None, move |_, resp| {
        let Some(hand_off) = hand_off.borrow_mut().take() else {
            return;
        };
        match resp {
            "new" => on_pick(hand_off, None),
            "pick" => {
                let picked = list
                    .selected_row()
                    .and_then(|r| items.get(r.index().max(0) as usize))
                    .map(|(m, _)| m.clone());
                match picked {
                    Some(m) => on_pick(hand_off, Some(m)),
                    None => on_pick(hand_off, None),
                }
            }
            _ => tracing::info!("message picker: cancelled"),
        }
    });
    dialog.present();
}

/// The list, its search box and the dialog's wiring, for a picker with
/// messages to offer.
#[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
fn fill_picker(
    dialog: &adw::MessageDialog,
    column: &gtk::Box,
    list: &gtk::ListBox,
    items: &std::rc::Rc<Vec<(Message, String)>>,
) {
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(i18n("Search by sender, subject or account").as_str()));
    column.append(&search);
    for (m, account) in items.iter() {
        list.append(&picker_row(m, account));
    }
    // The search narrows by sender, subject or account, case-folded.
    let filter_items = items.clone();
    list.set_filter_func(move |row| matches_search(row, &filter_items));
    {
        let list = list.clone();
        search.connect_search_changed(move |_| list.invalidate_filter());
    }
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_min_content_height(120);
    scroller.set_max_content_height(360);
    scroller.set_propagate_natural_height(true);
    scroller.set_child(Some(list));
    column.append(&scroller);
    {
        let dialog = dialog.clone();
        list.connect_row_selected(move |_, row| dialog.set_response_enabled("pick", row.is_some()));
    }
    // The first row (the message on screen, or the newest draft) starts
    // selected, so Enter takes the likeliest answer.
    list.select_row(list.row_at_index(0).as_ref());
    {
        let dialog = dialog.clone();
        list.connect_row_activated(move |l, row| {
            l.select_row(Some(row));
            dialog.response("pick");
            dialog.set_visible(false);
        });
    }
}

/// One message's row: its subject, then who it is from (or to), its date
/// and its account.
fn picker_row(m: &Message, account: &str) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    let subject = if m.subject.trim().is_empty() {
        i18n("(no subject)")
    } else {
        m.subject.clone()
    };
    row.set_title(&gtk::glib::markup_escape_text(&subject));
    let who = if m.from_name.trim().is_empty() {
        m.from_addr.clone()
    } else {
        m.from_name.clone()
    };
    let who = if who.trim().is_empty() {
        m.to.clone()
    } else {
        who
    };
    let mut sub = vec![who, m.date.clone()];
    if !account.trim().is_empty() {
        sub.push(account.to_string());
    }
    row.set_subtitle(&gtk::glib::markup_escape_text(&sub.join("  ·  ")));
    row.set_activatable(true);
    row
}

/// Whether `row` matches the search box above the list: by sender,
/// subject or account, case-folded.
fn matches_search(row: &gtk::ListBoxRow, items: &[(Message, String)]) -> bool {
    let needle = row
        .parent()
        .and_then(|l| l.downcast::<gtk::ListBox>().ok())
        .and_then(|l| l.prev_sibling())
        .and_then(|w| w.prev_sibling().or(Some(w)))
        .and_then(|w| w.downcast::<gtk::SearchEntry>().ok())
        .map(|e| e.text().to_lowercase())
        .unwrap_or_default();
    if needle.trim().is_empty() {
        return true;
    }
    let Some((m, account)) = items.get(row.index().max(0) as usize) else {
        return true;
    };
    [&m.from_name, &m.from_addr, &m.subject, &m.to, account]
        .iter()
        .any(|f| f.to_lowercase().contains(needle.trim()))
}
