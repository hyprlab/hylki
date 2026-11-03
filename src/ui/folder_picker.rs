//! The "Move To" popover (#164): an account's folders as a searchable list
//! anchored to the toolbar button, so mail can be filed without dragging it
//! to the sidebar. Rows follow the sidebar's order and indentation; a row
//! picked (clicked, or Enter on the first match) hands its account and path
//! back. The folders of the other accounts that can take mail follow, each
//! under its account's name (#265).

use gtk::prelude::*;

use crate::i18n::i18n;
use crate::models::Folder;

/// One account's folders in the picker.
pub(crate) struct PickerAccount {
    pub(crate) account_id: u32,
    /// Shown above the folders; `None` for the message's own account,
    /// which comes first without one.
    pub(crate) heading: Option<String>,
    pub(crate) folders: Vec<Folder>,
    /// The folder the mail already sits in, left out.
    pub(crate) exclude: Option<String>,
    /// How its folders nest, for the indent.
    pub(crate) hierarchy: crate::models::FolderHierarchy,
}

/// Open the picker below the point (`x`, `y`) of `parent`, listing each of
/// `accounts`' folders. `on_pick` gets the chosen folder's account and path
/// and whether the whole conversation goes with it: when the message is one
/// of a conversation of `conversation` messages (#171), a switch at the top
/// offers moving them all, on by default — the reading pane is showing the
/// conversation, and that is what a move from it most likely means.
pub(crate) fn show_folder_picker(
    parent: &impl IsA<gtk::Widget>,
    x: f64,
    y: f64,
    accounts: Vec<PickerAccount>,
    conversation: Option<usize>,
    on_pick: impl Fn(u32, String, bool) + 'static,
) {
    let popover = gtk::Popover::new();
    popover.set_has_arrow(false);
    popover.set_position(gtk::PositionType::Bottom);
    popover.add_css_class("menu");

    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.add_css_class("context-menu-list");
    column.set_margin_top(10);

    let whole = conversation
        .filter(|n| *n > 1)
        .map(|n| whole_conversation_switch(&column, n));

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(i18n("Search folders").as_str()));
    column.append(&search);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("navigation-sidebar");
    let table = fill_folder_list(&list, &accounts);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_propagate_natural_height(true);
    scroller.set_max_content_height(380);
    scroller.set_min_content_width(240);
    scroller.set_child(Some(&list));
    column.append(&scroller);

    let on_pick = std::rc::Rc::new(on_pick);
    let table = std::rc::Rc::new(table);
    {
        let popover = popover.clone();
        let on_pick = on_pick.clone();
        let whole = whole.clone();
        let table = table.clone();
        list.connect_row_activated(move |_, row| {
            if let Some(PickerRow::Folder { path, account, .. }) = row_entry(&table, row.index()) {
                let (path, account) = (path.clone(), *account);
                let all = whole.as_ref().is_some_and(|w| w.is_active());
                popover.popdown();
                on_pick(account, path, all);
            }
        });
    }
    {
        let list = list.clone();
        let table = table.clone();
        search.connect_search_changed(move |e| {
            let query = e.text().to_lowercase();
            // Headings come before their folders: settled once the next
            // heading (or the end) shows whether any of them matched.
            let mut heading: Option<(gtk::ListBoxRow, bool)> = None;
            let settle = |h: Option<(gtk::ListBoxRow, bool)>| {
                if let Some((row, any)) = h {
                    row.set_visible(any);
                }
            };
            let mut i = 0;
            while let Some(row) = list.row_at_index(i) {
                let entry = row_entry(&table, i);
                if matches!(entry, Some(PickerRow::Heading)) {
                    settle(heading.take());
                    heading = Some((row.clone(), false));
                } else {
                    let name = match entry {
                        Some(PickerRow::Folder { name, .. }) => Some(name),
                        _ => None,
                    };
                    let hit = query.is_empty() || name.is_some_and(|n| n.contains(&query));
                    row.set_visible(hit);
                    if hit {
                        if let Some((_, any)) = heading.as_mut() {
                            *any = true;
                        }
                    }
                }
                i += 1;
            }
            settle(heading.take());
        });
    }
    {
        // Enter files into the first folder still listed.
        let list = list.clone();
        search.connect_activate(move |_| {
            let mut i = 0;
            while let Some(row) = list.row_at_index(i) {
                if row.is_visible() && row.is_activatable() {
                    row.activate();
                    return;
                }
                i += 1;
            }
        });
    }

    popover.set_child(Some(&column));
    popover.set_parent(parent);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    popover.connect_closed(|p| p.unparent());
    popover.popup();
    search.grab_focus();

    // HYLKI_SHOWCASE_MOVE=1 captures the picker a second after it opens
    // (the window snapshot never includes a popover).
    if std::env::var("HYLKI_SHOWCASE_MOVE").is_ok() {
        if let Ok(path) = std::env::var("HYLKI_SHOWCASE") {
            let column = column.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1000), move || {
                crate::ui::showcase::showcase_capture(column.upcast_ref(), &path);
            });
        }
    }
}

/// A row of the folder list: an account's heading, or a folder with its
/// lowercased name (for the search), its path and its account.
enum PickerRow {
    Heading,
    Folder {
        name: String,
        path: String,
        account: u32,
    },
}

/// The entry for the row at `index` (a `ListBoxRow` index, never negative
/// for a row in the list).
fn row_entry(table: &[PickerRow], index: i32) -> Option<&PickerRow> {
    usize::try_from(index).ok().and_then(|i| table.get(i))
}

/// The "Whole conversation" switch at the top of the picker, for a
/// conversation of `n` messages; on to start with.
fn whole_conversation_switch(column: &gtk::Box, n: usize) -> gtk::Switch {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.set_margin_start(6);
    row.set_margin_end(6);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.set_hexpand(true);
    let title = gtk::Label::new(Some(i18n("Whole conversation").as_str()));
    title.set_xalign(0.0);
    text.append(&title);
    let count = gtk::Label::new(Some(
        crate::i18n::ni18n_f(
            "{n} message",
            "{n} messages",
            n as u32,
            &[("n", &n.to_string())],
        )
        .as_str(),
    ));
    count.set_xalign(0.0);
    count.add_css_class("dim-label");
    count.add_css_class("caption");
    text.append(&count);
    row.append(&text);
    let switch = gtk::Switch::new();
    switch.set_active(true);
    switch.set_valign(gtk::Align::Center);
    row.append(&switch);
    column.append(&row);
    column.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    switch
}

/// The accounts' headings and folders as rows of `list`, and what each row
/// stands for, at the row's index: rows are only ever appended, in this
/// order.
fn fill_folder_list(list: &gtk::ListBox, accounts: &[PickerAccount]) -> Vec<PickerRow> {
    let mut table: Vec<PickerRow> = Vec::new();
    for account in accounts {
        if let Some(heading) = &account.heading {
            let label = gtk::Label::new(Some(heading));
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.add_css_class("caption-heading");
            label.add_css_class("dim-label");
            label.set_margin_top(8);
            label.set_margin_start(4);
            let lbr = gtk::ListBoxRow::new();
            lbr.set_child(Some(&label));
            lbr.set_activatable(false);
            lbr.set_selectable(false);
            // The search keeps a heading while any of its folders match.
            table.push(PickerRow::Heading);
            list.append(&lbr);
        }
        let refs: Vec<&Folder> = account.folders.iter().collect();
        for f in &account.folders {
            if account.exclude.as_deref() == Some(f.path.as_str()) {
                continue;
            }
            let depth = crate::ui::sidebar::folder_depth(f, &refs, &account.hierarchy);
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            row.set_margin_start(4 + 16 * depth as i32);
            let icon = gtk::Image::from_icon_name(f.kind.icon());
            icon.set_pixel_size(16);
            row.append(&icon);
            let label = gtk::Label::new(Some(&f.name));
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            row.append(&label);
            let lbr = gtk::ListBoxRow::new();
            lbr.set_child(Some(&row));
            // The search filter reads the name back from here.
            table.push(PickerRow::Folder {
                name: f.name.to_lowercase(),
                path: f.path.clone(),
                account: account.account_id,
            });
            list.append(&lbr);
        }
    }
    table
}

#[cfg(test)]
mod tests {
    use super::{row_entry, PickerRow};

    #[test]
    fn rows_are_looked_up_by_their_index() {
        let table = [
            PickerRow::Heading,
            PickerRow::Folder {
                name: "inbox".into(),
                path: "INBOX".into(),
                account: 2,
            },
        ];
        assert!(matches!(row_entry(&table, 0), Some(PickerRow::Heading)));
        assert!(matches!(
            row_entry(&table, 1),
            Some(PickerRow::Folder { account: 2, .. })
        ));
        assert!(row_entry(&table, 2).is_none());
        assert!(row_entry(&table, -1).is_none());
    }
}
