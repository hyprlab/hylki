//! The page's menus and small dialogs: sort and filter, bulk choices,
//! confirmations, the "+" menu, the book picker's labels and file import.

use adw::prelude::*;
use relm4::prelude::*;

use crate::contacts::ContactDetails;
use crate::i18n::{i18n, i18n_f, i18n_noop};
use crate::ui::context_menu::{show_context_menu, MenuEntry};

use super::types::{ContactFilter, ContactSort, ContactsPage, ContactsPageInput};
use super::ContactsPageWidgets;

/// The sort and filter button's popover, laid out as the message list's
/// (#441): the orders in one column, the "Show only" filters in the other.
/// It stays open as rows are clicked, so an order and several filters can
/// be set in one visit.
pub(super) struct FilterMenu {
    button: gtk::MenuButton,
    sorts: Vec<(ContactSort, gtk::CheckButton)>,
    filters: Vec<(ContactFilter, gtk::CheckButton)>,
    clear: gtk::Button,
}

impl FilterMenu {
    pub(super) fn new(button: &gtk::MenuButton, input: &relm4::Sender<ContactsPageInput>) -> Self {
        let column = |title: &str| {
            let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
            col.add_css_class("sort-filter-column");
            let caption = gtk::Label::new(Some(title));
            caption.set_xalign(0.0);
            caption.add_css_class("dim-label");
            caption.add_css_class("caption-heading");
            col.append(&caption);
            col
        };
        let sort_col = column(&i18n("Sort by"));
        let mut sorts: Vec<(ContactSort, gtk::CheckButton)> = Vec::new();
        for (label, sort) in [
            (i18n_noop("First name"), ContactSort::FirstName),
            (i18n_noop("Last name"), ContactSort::LastName),
            (i18n_noop("Email"), ContactSort::Email),
        ] {
            let radio = gtk::CheckButton::with_label(&i18n(label));
            if let Some((_, first)) = sorts.first() {
                radio.set_group(Some(first));
            }
            radio.set_active(sort == ContactSort::FirstName);
            let s = input.clone();
            radio.connect_toggled(move |r| {
                if r.is_active() {
                    let _ = s.send(ContactsPageInput::SetSort(sort));
                }
            });
            sort_col.append(&radio);
            sorts.push((sort, radio));
        }
        let filter_col = column(&i18n("Show only"));
        let mut filters = Vec::new();
        for f in ContactFilter::ALL {
            let check = gtk::CheckButton::with_label(&f.label());
            let s = input.clone();
            check.connect_toggled(move |c| {
                let _ = s.send(ContactsPageInput::SetFilter(f, c.is_active()));
            });
            filter_col.append(&check);
            filters.push((f, check));
        }
        let clear = gtk::Button::with_label(&i18n("Clear Filters"));
        clear.add_css_class("flat");
        clear.set_halign(gtk::Align::Start);
        clear.set_sensitive(false);
        let s = input.clone();
        clear.connect_clicked(move |_| {
            let _ = s.send(ContactsPageInput::ClearFilters);
        });
        filter_col.append(&clear);

        let columns = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        columns.add_css_class("sort-filter-popover");
        columns.append(&sort_col);
        columns.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        columns.append(&filter_col);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&columns));
        button.set_popover(Some(&popover));
        FilterMenu {
            button: button.clone(),
            sorts,
            filters,
            clear,
        }
    }

    /// Show the order and the filters on the rows, and tint the button
    /// while any filter is on, so a narrowed list never passes for a short
    /// one.
    pub(super) fn sync(&self, sort: ContactSort, on: u8) {
        for (s, radio) in &self.sorts {
            if *s == sort && !radio.is_active() {
                radio.set_active(true);
            }
        }
        for (f, check) in &self.filters {
            let active = on & f.bit() != 0;
            if check.is_active() != active {
                check.set_active(active);
            }
        }
        self.clear.set_sensitive(on != 0);
        if on == 0 {
            self.button.remove_css_class("list-filter-active");
            self.button
                .set_tooltip_text(Some(&i18n("Sort and filter contacts")));
        } else {
            self.button.add_css_class("list-filter-active");
            let names: Vec<String> = ContactFilter::ALL
                .iter()
                .filter(|f| on & f.bit() != 0)
                .map(|f| f.label())
                .collect();
            self.button.set_tooltip_text(Some(&i18n_f(
                "Filtered: {names}",
                &[("names", &names.join(", "))],
            )));
        }
    }
}

/// The groups and books a selection of several can go to, as (index into
/// `contacts`, name) and (book UID, name).
pub(super) struct BulkChoices {
    pub(super) groups: Vec<(usize, String)>,
    pub(super) books: Vec<(String, String)>,
}

impl BulkChoices {
    pub(super) fn group_entries(&self, s: &relm4::Sender<ContactsPageInput>) -> Vec<MenuEntry> {
        self.groups
            .iter()
            .map(|(g, name)| {
                let (s, g) = (s.clone(), *g);
                MenuEntry::new(name.clone(), move || {
                    let _ = s.send(ContactsPageInput::BulkAddToGroup(g));
                })
                .icon("system-users-symbolic")
            })
            .collect()
    }

    pub(super) fn book_entries(&self, s: &relm4::Sender<ContactsPageInput>) -> Vec<MenuEntry> {
        self.books
            .iter()
            .map(|(uid, name)| {
                let (s, uid) = (s.clone(), uid.clone());
                MenuEntry::new(name.clone(), move || {
                    let _ = s.send(ContactsPageInput::BulkMoveRequest(uid.clone()));
                })
                .icon("x-office-address-book-symbolic")
            })
            .collect()
    }
}

/// Ask before doing something to contacts that cannot be taken back.
#[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
pub(super) fn confirm(
    widgets: &ContactsPageWidgets,
    heading: &str,
    body: &str,
    action: &str,
    destructive: bool,
    on_yes: impl Fn() + 'static,
) {
    let win = widgets.page_stack.root().and_downcast::<gtk::Window>();
    let dialog = adw::MessageDialog::new(win.as_ref(), Some(heading), Some(body));
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("yes", action);
    dialog.set_response_appearance(
        "yes",
        if destructive {
            adw::ResponseAppearance::Destructive
        } else {
            adw::ResponseAppearance::Suggested
        },
    );
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, resp| {
        if resp == "yes" {
            on_yes();
        }
    });
    dialog.present();
}

/// The header's "+": a contact or a group.
pub(super) fn new_menu(button: &gtk::Button, sender: &ComponentSender<ContactsPage>) {
    let (sc, sg) = (sender.input_sender().clone(), sender.input_sender().clone());
    show_context_menu(
        button,
        (button.width() / 2) as f64,
        button.height() as f64,
        vec![vec![
            MenuEntry::new(i18n("New Contact"), move || {
                let _ = sc.send(ContactsPageInput::NewContact);
            })
            .icon("contact-new-symbolic"),
            MenuEntry::new(i18n("New Group"), move || {
                let _ = sg.send(ContactsPageInput::NewGroup);
            })
            .icon("system-users-symbolic"),
        ]],
    );
}

/// The address book dropdown's labels; `ellipsize` for its own, which
/// the pane's width holds. Each label also gets its whole text as a
/// tooltip, so a name cut off on the row or squeezed in the popup (two
/// books of one CardDAV account begin with the same long prefix, and the
/// choice between them lives in the cut-off tail) still reads on a hover.
pub(crate) fn book_label_factory(ellipsize: bool) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        if ellipsize {
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            // The room the row's own label asks before giving way: the
            // default combo factory's, so the row keeps holding its width.
            label.set_max_width_chars(20);
            label.set_width_chars(1);
        }
        item.set_child(Some(&label));
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(label), Some(text)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<gtk::StringObject>(),
        ) else {
            return;
        };
        let name = text.string();
        label.set_label(&name);
        // A GtkListItem is not a widget, so the tooltip goes on its label:
        // hovering the row's value or a popup line reads the name whole.
        label.set_tooltip_text(Some(name.as_str()));
    });
    factory
}

/// Whether a contact matches a search, already lower case; an empty one
/// matches everyone, and "group" or "groups" every group.
pub(super) fn matches_query(c: &ContactDetails, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    (c.is_group() && names_groups(q))
        || c.name.to_lowercase().contains(q)
        || c.nickname.to_lowercase().contains(q)
        || c.org.to_lowercase().contains(q)
        || c.emails.iter().any(|e| e.value.to_lowercase().contains(q))
        || c.phones.iter().any(|p| p.value.to_lowercase().contains(q))
        || c.members
            .iter()
            .any(|m| m.name.to_lowercase().contains(q) || m.email.to_lowercase().contains(q))
}

/// Whether a search is the word for groups, in English or the app's
/// language, so groups can be found by what they are as well as by name.
pub(super) fn names_groups(q: &str) -> bool {
    let q = q.trim();
    ["group", "groups"].contains(&q)
        || [i18n("Group"), i18n("Groups")]
            .iter()
            .any(|w| w.to_lowercase() == q)
}

/// Ask for `.vcf` files to import into the Hylki book, and hand their paths
/// to `done` (never called for a cancelled dialog or no files).
pub(crate) fn choose_vcf_files(
    parent: Option<&gtk::Window>,
    done: impl Fn(Vec<std::path::PathBuf>) + 'static,
) {
    let dialog = gtk::FileDialog::builder()
        .title(i18n("Import Contacts"))
        .build();
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(&i18n("vCard files")));
    filter.add_pattern("*.vcf");
    filter.add_pattern("*.vcard");
    filter.add_mime_type("text/vcard");
    filter.add_mime_type("text/x-vcard");
    let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let any = gtk::FileFilter::new();
    any.set_name(Some(&i18n("All files")));
    any.add_pattern("*");
    filters.append(&any);
    dialog.set_filters(Some(&filters));
    dialog.open_multiple(parent, gtk::gio::Cancellable::NONE, move |res| {
        let Ok(files) = res else { return };
        let paths: Vec<std::path::PathBuf> = files
            .iter::<gtk::gio::File>()
            .flatten()
            .filter_map(|f| f.path())
            .collect();
        if !paths.is_empty() {
            done(paths);
        }
    });
}

/// The Cancel · title · Save bar over a form (dialog-header layout).
pub(super) fn editor_bar(detail: &gtk::Box, sender: &ComponentSender<ContactsPage>, heading: &str) {
    let bar = gtk::CenterBox::new();
    let cancel = gtk::Button::with_label(&i18n("Cancel"));
    let s = sender.input_sender().clone();
    cancel.connect_clicked(move |_| {
        let _ = s.send(ContactsPageInput::CancelEdit);
    });
    let save = gtk::Button::with_label(&i18n("Save"));
    save.add_css_class("suggested-action");
    let s = sender.input_sender().clone();
    save.connect_clicked(move |_| {
        let _ = s.send(ContactsPageInput::SaveEdit);
    });
    let heading = gtk::Label::new(Some(heading));
    heading.add_css_class("title-4");
    bar.set_start_widget(Some(&cancel));
    bar.set_center_widget(Some(&heading));
    bar.set_end_widget(Some(&save));
    detail.append(&bar);
}
