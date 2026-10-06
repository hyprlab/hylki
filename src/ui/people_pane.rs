//! The People pane: shown in place of the folder sidebar while the People
//! view is on, it lists everyone the user exchanges mail with, most recent
//! first, the way a messenger lists its chats. Picking a person fills the
//! message list with that conversation, whichever folder and account its
//! mail sits in; All People at the top shows every exchange.
//!
//! A mailbox easily has thousands of correspondents, and the list is read
//! again after every sync, so it is a `gtk::ListView`: the model is the
//! addresses in order, and only the rows on screen exist, filled from the
//! shared `Shown` when they are bound. A new order replaces the addresses;
//! a changed name or count refills just the rows on screen.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::i18n;
use crate::people::Person;
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use crate::ui::sidebar::{pin_icon_size, style_badge};

/// What the rows show, shared with the list's factory.
#[derive(Default)]
struct Shown {
    /// Everyone in the list, by address.
    people: HashMap<String, Person>,
    /// Everyone's unread mail, for the All People row.
    all_unread: u32,
    /// The rows on screen, by the address they show ("" for All People).
    bound: HashMap<String, gtk::Box>,
}

pub struct PeoplePane {
    /// "" (All People), then the addresses in list order.
    store: gtk::StringList,
    filter: gtk::CustomFilter,
    selection: gtk::SingleSelection,
    shown: Rc<RefCell<Shown>>,
    /// The addresses in list order (after All People).
    order: Vec<String>,
    /// The person shown (`None`: All People).
    selected: Option<String>,
    /// Set while the pane moves its own highlight or replaces its rows, so
    /// that is not taken for a pick.
    quiet: Rc<Cell<bool>>,
}

#[derive(Debug, Clone)]
pub enum PeoplePaneInput {
    /// The People list, most recent first.
    SetPeople(Vec<Person>),
    /// One person's unread count changed (mail read or marked unread).
    SetUnread { address: String, unread: u32 },
    /// Highlight a person (`None`: All People) without reporting it.
    Select(Option<String>),
    /// The highlight moved to a row: a person, or All People (`None`).
    Picked(Option<String>),
    /// The filter text changed.
    Refilter,
    /// Right-click on a person's row, at a point of it.
    Menu { address: String, row: gtk::Box, x: f64, y: f64 },
}

#[derive(Debug)]
pub enum PeoplePaneOutput {
    /// A person was picked (`None`: All People).
    Selected(Option<String>),
    /// Write to this address.
    Compose(String),
}

impl SimpleComponent for PeoplePane {
    type Init = ();
    type Input = PeoplePaneInput;
    type Output = PeoplePaneOutput;
    type Root = gtk::Box;
    type Widgets = ();

    fn init_root() -> Self::Root {
        gtk::Box::new(gtk::Orientation::Vertical, 0)
    }

    fn init(_: (), root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        root.add_css_class("people-pane");
        let entry = gtk::SearchEntry::new();
        entry.set_placeholder_text(Some(i18n("Filter People").as_str()));
        entry.set_margin_start(8);
        entry.set_margin_end(8);
        entry.set_margin_top(4);
        entry.set_margin_bottom(4);
        root.append(&entry);

        let shown: Rc<RefCell<Shown>> = Rc::default();
        let store = gtk::StringList::new(&[""]);
        let filter = {
            let shown = shown.clone();
            let entry = entry.downgrade();
            gtk::CustomFilter::new(move |obj| {
                let address = string_of(obj);
                let Some(entry) = entry.upgrade().filter(|_| !address.is_empty()) else {
                    return true;
                };
                let needle = entry.text().trim().to_lowercase();
                needle.is_empty()
                    || address.contains(&needle)
                    || shown
                        .borrow()
                        .people
                        .get(address.as_str())
                        .is_some_and(|p| p.name.to_lowercase().contains(&needle))
            })
        };
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let selection = gtk::SingleSelection::new(Some(filtered));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);

        let factory = gtk::SignalListItemFactory::new();
        {
            let s = sender.input_sender().clone();
            factory.connect_setup(move |_, item| {
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
                let row = row_widget();
                // The gesture belongs to the row and knows its item only
                // weakly, so a recycled row holds nothing it once showed.
                let click = gtk::GestureClick::new();
                click.set_button(3);
                let weak = item.downgrade();
                let s = s.clone();
                click.connect_pressed(move |gesture, _, x, y| {
                    let Some(item) = weak.upgrade() else { return };
                    let address = item.item().map(|o| string_of(&o)).unwrap_or_default();
                    let Some(row) = gesture.widget().and_downcast::<gtk::Box>() else { return };
                    if !address.is_empty() {
                        let _ = s.send(PeoplePaneInput::Menu { address, row, x, y });
                    }
                });
                row.add_controller(click);
                item.set_child(Some(&row));
            });
        }
        {
            let shown = shown.clone();
            factory.connect_bind(move |_, item| {
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
                let Some(row) = item.child().and_downcast::<gtk::Box>() else { return };
                let address = item.item().map(|o| string_of(&o)).unwrap_or_default();
                fill(&row, &address, &shown.borrow());
                shown.borrow_mut().bound.insert(address, row);
            });
        }
        {
            let shown = shown.clone();
            factory.connect_unbind(move |_, item| {
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
                let address = item.item().map(|o| string_of(&o)).unwrap_or_default();
                let mut shown = shown.borrow_mut();
                if shown.bound.get(&address).map(|r| r.upcast_ref::<gtk::Widget>()) == item.child().as_ref() {
                    shown.bound.remove(&address);
                }
            });
        }

        let view = gtk::ListView::new(Some(selection.clone()), Some(factory));
        view.add_css_class("navigation-sidebar");
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_vexpand(true);
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroller.set_child(Some(&view));
        root.append(&scroller);

        let quiet = Rc::new(Cell::new(false));
        {
            let quiet = quiet.clone();
            let s = sender.input_sender().clone();
            selection.connect_selected_item_notify(move |sel| {
                if quiet.get() {
                    return;
                }
                if let Some(obj) = sel.selected_item() {
                    let address = string_of(&obj);
                    let _ = s.send(PeoplePaneInput::Picked((!address.is_empty()).then_some(address)));
                }
            });
        }
        {
            let s = sender.input_sender().clone();
            entry.connect_search_changed(move |_| {
                let _ = s.send(PeoplePaneInput::Refilter);
            });
        }

        let model = PeoplePane {
            store,
            filter,
            selection,
            shown,
            order: Vec::new(),
            selected: None,
            quiet,
        };
        model.highlight();
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, msg: PeoplePaneInput, sender: ComponentSender<Self>) {
        match msg {
            PeoplePaneInput::SetPeople(people) => self.set_people(people),
            PeoplePaneInput::SetUnread { address, unread } => {
                let mut shown = self.shown.borrow_mut();
                let Some(p) = shown.people.get_mut(&address) else { return };
                let before = std::mem::replace(&mut p.unread, unread);
                shown.all_unread = shown.all_unread - before + unread;
                for key in ["", address.as_str()] {
                    if let Some(row) = shown.bound.get(key) {
                        fill(row, key, &shown);
                    }
                }
            }
            PeoplePaneInput::Select(address) => {
                self.selected = address;
                self.highlight();
            }
            PeoplePaneInput::Picked(address) => {
                if address != self.selected {
                    self.selected = address.clone();
                    let _ = sender.output(PeoplePaneOutput::Selected(address));
                }
            }
            PeoplePaneInput::Refilter => {
                self.quiet.set(true);
                self.filter.changed(gtk::FilterChange::Different);
                self.quiet.set(false);
                self.highlight();
            }
            PeoplePaneInput::Menu { address, row, x, y } => {
                let s = sender.output_sender().clone();
                let to = address.clone();
                let clipboard = row.clipboard();
                show_context_menu(
                    &row,
                    x,
                    y,
                    vec![vec![
                        MenuEntry::new(i18n("Write To"), move || {
                            let _ = s.send(PeoplePaneOutput::Compose(to.clone()));
                        })
                        .icon("mail-message-new-symbolic"),
                        MenuEntry::new(i18n("Copy Address"), move || {
                            clipboard.set_text(&address);
                        })
                        .icon("edit-copy-symbolic"),
                    ]],
                );
            }
        }
    }
}

impl PeoplePane {
    fn set_people(&mut self, people: Vec<Person>) {
        let same_order = people.len() == self.order.len()
            && people.iter().zip(&self.order).all(|(p, a)| p.address == *a);
        {
            let mut shown = self.shown.borrow_mut();
            shown.all_unread = people.iter().map(|p| p.unread).sum();
            shown.people = people.iter().map(|p| (p.address.clone(), p.clone())).collect();
            if same_order {
                let shown = &*shown;
                for (address, row) in &shown.bound {
                    fill(row, address, shown);
                }
                return;
            }
        }
        // A new order: the rows on screen are bound afresh to it (which
        // reads `shown`, so it is not borrowed here).
        self.order = people.into_iter().map(|p| p.address).collect();
        let addresses: Vec<&str> = std::iter::once("").chain(self.order.iter().map(String::as_str)).collect();
        self.quiet.set(true);
        self.store.splice(0, self.store.n_items(), &addresses);
        self.quiet.set(false);
        self.highlight();
    }

    /// Move the highlight to the shown person, quietly.
    fn highlight(&self) {
        let want = self.selected.as_deref().unwrap_or("");
        let position = (0..self.selection.n_items())
            .find(|&i| self.selection.item(i).is_some_and(|o| string_of(&o) == want))
            .unwrap_or(gtk::INVALID_LIST_POSITION);
        self.quiet.set(true);
        self.selection.set_selected(position);
        self.quiet.set(false);
    }
}

/// The address a list item stands for ("" for All People).
fn string_of(obj: &impl IsA<gtk::glib::Object>) -> String {
    obj.upcast_ref::<gtk::glib::Object>()
        .downcast_ref::<gtk::StringObject>()
        .map(|s| s.string().to_string())
        .unwrap_or_default()
}

/// An empty row: the All People icon or a person's avatar, the name and
/// the unread count.
fn row_widget() -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("folder-row");
    let icon = gtk::Image::from_icon_name("system-users-symbolic");
    pin_icon_size(&icon);
    icon.set_size_request(28, -1);
    icon.add_css_class("folder-icon");
    row.append(&icon);
    row.append(&adw::Avatar::new(28, None, true));
    // The name, and under it the address: the message list beside it
    // leaves names out, and two people can share one.
    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);
    let name = gtk::Label::new(None);
    name.set_halign(gtk::Align::Start);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.add_css_class("account-name");
    text.append(&name);
    let address = gtk::Label::new(None);
    address.set_halign(gtk::Align::Start);
    address.set_ellipsize(gtk::pango::EllipsizeMode::End);
    address.add_css_class("caption");
    address.add_css_class("dim-label");
    text.append(&address);
    row.append(&text);
    let badge = gtk::Label::new(None);
    style_badge(&badge, 5);
    row.append(&badge);
    row
}

/// Show a person (or All People, for "") in a row from [`row_widget`].
fn fill(row: &gtk::Box, address: &str, shown: &Shown) {
    let Some(icon) = row.first_child() else { return };
    let Some(avatar) = icon.next_sibling().and_downcast::<adw::Avatar>() else { return };
    let Some(text) = avatar.next_sibling() else { return };
    let Some(name) = text.first_child().and_downcast::<gtk::Label>() else { return };
    let Some(line) = name.next_sibling().and_downcast::<gtk::Label>() else { return };
    let Some(badge) = text.next_sibling().and_downcast::<gtk::Label>() else { return };
    let all = address.is_empty();
    icon.set_visible(all);
    avatar.set_visible(!all);
    // Without a name the address is the title, and not said twice.
    let (label, unread) = match shown.people.get(address) {
        _ if all => (i18n("All People"), shown.all_unread),
        Some(p) if !p.name.is_empty() => (p.name.clone(), p.unread),
        Some(p) => (address.to_string(), p.unread),
        None => (address.to_string(), 0),
    };
    if name.label() != label {
        name.set_label(&label);
    }
    let second = if all || label == address { "" } else { address };
    line.set_visible(!second.is_empty());
    if line.label() != second {
        line.set_label(second);
    }
    let initials = shown.people.get(address).map_or(label.as_str(), |p| p.display_name());
    if !all && avatar.text().as_deref() != Some(initials) {
        avatar.set_text(Some(initials));
    }
    row.set_tooltip_text((!all).then_some(address));
    badge.set_visible(unread > 0);
    let count = unread.to_string();
    if badge.label() != count {
        badge.set_label(&count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rss_mb() -> f64 {
        let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
        let pages: f64 = statm.split_whitespace().nth(1).and_then(|p| p.parse().ok()).unwrap_or(0.0);
        pages * 4096.0 / 1e6
    }

    fn person(i: usize) -> Person {
        Person {
            address: format!("person{i}@example.org"),
            name: format!("Person {i}"),
            latest: i as i64,
            unread: (i % 3) as u32,
            total: 1,
        }
    }

    /// How long the pane takes to list N people in a window, to take them
    /// again in a new order (as after a sync), and what they cost in
    /// memory. Needs a display (a headless Weston will do):
    /// `WAYLAND_DISPLAY=… cargo test --bin hylki
    /// people_pane::tests::rows_timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn rows_timing() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let ctx = gtk::glib::MainContext::default();
        // Until the pane lists `first` first.
        let settle = |pane: &relm4::Controller<PeoplePane>, first: &str| {
            let at = std::time::Instant::now();
            while pane.model().order.first().map(String::as_str) != Some(first) && at.elapsed().as_secs() < 60 {
                ctx.iteration(true);
            }
            for _ in 0..20 {
                ctx.iteration(false);
            }
        };
        for n in [500usize, 2000, 6000, 20000] {
            let before = rss_mb();
            let window = gtk::Window::new();
            window.set_default_size(300, 900);
            let pane = PeoplePane::builder().launch(()).detach();
            window.set_child(Some(pane.widget()));
            window.present();
            let people: Vec<Person> = (0..n).map(person).collect();
            let at = std::time::Instant::now();
            pane.emit(PeoplePaneInput::SetPeople(people.clone()));
            settle(&pane, &people[0].address);
            let listed = at.elapsed();
            let mut reordered = people;
            reordered.rotate_left(n / 2);
            let first = reordered[0].address.clone();
            let at = std::time::Instant::now();
            pane.emit(PeoplePaneInput::SetPeople(reordered));
            settle(&pane, &first);
            println!(
                "{n} people: listed in {listed:?}, reordered in {:?}, +{:.0} MB",
                at.elapsed(),
                rss_mb() - before
            );
            window.destroy();
            while ctx.iteration(false) {}
        }
    }
}
