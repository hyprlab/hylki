//! The Import from Mail window (#677): everyone the mail has an address
//! for who is not a contact yet, most seen first, to search and tick. The
//! list is read again when another account is picked to look in.
//!
//! A window of its own, as Import Contacts is: a message dialog is too
//! narrow and too short for a list of people.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;

use crate::cache::AddressScope;
use crate::contacts::from_mail::{
    books_and_saved, cards_for, people_in_mail, FromMailStart, MailPerson, Place,
};
use crate::contacts::import::{ImportFile, ImportRequest};
use crate::contacts::Book;
use crate::i18n::{i18n, ni18n_f};

/// One row's check button, and the address it shows now.
type BoundRow = (gtk::glib::WeakRef<gtk::CheckButton>, Rc<RefCell<String>>);

/// What the window keeps while it is open.
struct State {
    people: RefCell<Vec<MailPerson>>,
    /// The addresses ticked, lower case.
    picked: RefCell<HashSet<String>>,
    /// The search, lower case.
    query: RefCell<String>,
    rows: RefCell<Vec<BoundRow>>,
    /// Bumped by each read, so an older one that ends late is dropped.
    read: Cell<u64>,
}

impl State {
    fn person(&self, item: Option<gtk::glib::Object>) -> Option<MailPerson> {
        let at: usize = item?
            .downcast::<gtk::StringObject>()
            .ok()?
            .string()
            .parse()
            .ok()?;
        self.people.borrow().get(at).cloned()
    }

    fn shows(&self, p: &MailPerson) -> bool {
        let q = self.query.borrow();
        q.is_empty() || p.email.contains(q.as_str()) || p.name.to_lowercase().contains(q.as_str())
    }

    /// Set every row's tick from `picked`.
    fn sync_rows(&self) {
        self.rows.borrow_mut().retain(|(check, email)| {
            let Some(check) = check.upgrade() else {
                return false;
            };
            check.set_active(self.picked.borrow().contains(&*email.borrow()));
            true
        });
    }
}

/// The Look in row: all mail, then each account with its folders under it.
/// The list shows a folder by its name, set in under its account; the row,
/// once one is picked, says which account it is in. Typing finds a place,
/// as there can be many.
fn look_in_row(places: &Rc<Vec<Place>>) -> adw::ComboRow {
    let row = adw::ComboRow::new();
    row.set_title(&i18n("Look in"));
    let mut labels = vec![i18n("All Mail")];
    labels.extend(places.iter().map(|p| p.label.clone()));
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    row.set_model(Some(&gtk::StringList::new(&refs)));
    row.set_expression(Some(gtk::PropertyExpression::new(
        gtk::StringObject::static_type(),
        gtk::Expression::NONE,
        "string",
    )));
    row.set_enable_search(true);
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(40);
        item.set_child(Some(&label));
    });
    let places = places.clone();
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(label) = item.child().and_downcast::<gtk::Label>() else {
            return;
        };
        let Some(text) = item.item().and_downcast::<gtk::StringObject>() else {
            return;
        };
        // The search filters the list, so the place is found by its label,
        // not by its position.
        let text = text.string();
        let place = places.iter().find(|p| p.label == text.as_str());
        label.set_label(place.map_or(text.as_str(), |p| p.name.as_str()));
        let folder = place.is_some_and(|p| p.folder.is_some());
        label.set_margin_start(if folder { 18 } else { 0 });
        if folder {
            label.remove_css_class("heading");
        } else {
            label.add_css_class("heading");
        }
    });
    row.set_list_factory(Some(&factory));
    row
}

/// Show the window over `parent`; `done` gets what to import, never called
/// when it is cancelled.
pub(crate) fn show(
    parent: Option<&gtk::Window>,
    start: FromMailStart,
    done: impl Fn(ImportRequest) + 'static,
) {
    let window = adw::Window::builder()
        .modal(true)
        .default_width(560)
        .default_height(680)
        .title(i18n("Import from Mail"))
        .build();
    window.set_transient_for(parent);
    let cancel = gtk::Button::with_label(&i18n("Cancel"));
    let import = gtk::Button::with_label(&i18n("Import"));
    import.add_css_class("suggested-action");
    import.set_sensitive(false);
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&import);

    let state = Rc::new(State {
        people: RefCell::default(),
        picked: RefCell::default(),
        query: RefCell::default(),
        rows: RefCell::default(),
        read: Cell::new(0),
    });

    // Filled once the books are read, which takes a moment: the window
    // opens at once and says it is reading.
    let books: Rc<RefCell<Vec<Book>>> = Rc::default();
    let book_row = adw::ComboRow::new();
    book_row.set_title(&i18n("Import into"));
    book_row.set_sensitive(false);
    let places = Rc::new(start.places);
    let scope_row = look_in_row(&places);
    let choices = gtk::ListBox::new();
    choices.add_css_class("boxed-list");
    choices.set_selection_mode(gtk::SelectionMode::None);
    choices.append(&book_row);
    choices.append(&scope_row);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(&i18n("Search names and addresses")));
    let picked_label = gtk::Label::new(None);
    picked_label.add_css_class("dim-label");
    picked_label.set_hexpand(true);
    picked_label.set_xalign(0.0);
    let all = gtk::Button::with_label(&i18n("Select All"));
    let none = gtk::Button::with_label(&i18n("Select None"));
    for b in [&all, &none] {
        b.add_css_class("flat");
    }
    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    tools.append(&picked_label);
    tools.append(&all);
    tools.append(&none);

    let model = gtk::StringList::new(&[]);
    let filter = gtk::CustomFilter::new({
        let state = state.clone();
        move |item| {
            state
                .person(Some(item.clone()))
                .is_some_and(|p| state.shows(&p))
        }
    });
    let filtered = gtk::FilterListModel::new(Some(model.clone()), Some(filter.clone()));

    // Say how many are ticked, and let Import go once one is.
    let count: Rc<dyn Fn()> = {
        let (state, label, import, books) = (
            state.clone(),
            picked_label.clone(),
            import.downgrade(),
            books.clone(),
        );
        Rc::new(move || {
            let n = state.picked.borrow().len();
            label.set_label(&ni18n_f(
                "{n} selected",
                "{n} selected",
                n as u32,
                &[("n", &n.to_string())],
            ));
            if let Some(import) = import.upgrade() {
                import.set_sensitive(n > 0 && !books.borrow().is_empty());
            }
        })
    };
    count();

    let factory = gtk::SignalListItemFactory::new();
    {
        let (state, count) = (state.clone(), count.clone());
        factory.connect_setup(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let name = gtk::Label::new(None);
            name.set_xalign(0.0);
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let email = gtk::Label::new(None);
            email.set_xalign(0.0);
            email.set_ellipsize(gtk::pango::EllipsizeMode::End);
            email.add_css_class("dim-label");
            email.add_css_class("caption");
            let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
            text.set_hexpand(true);
            text.append(&name);
            text.append(&email);
            let seen = gtk::Label::new(None);
            seen.add_css_class("dim-label");
            seen.add_css_class("numeric");
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            line.set_margin_top(6);
            line.set_margin_bottom(6);
            line.set_margin_start(6);
            line.append(&text);
            line.append(&seen);
            let check = gtk::CheckButton::new();
            check.set_child(Some(&line));
            let shown: Rc<RefCell<String>> = Rc::default();
            state
                .rows
                .borrow_mut()
                .push((check.downgrade(), shown.clone()));
            let (state, count) = (state.clone(), count.clone());
            check.connect_toggled(move |check| {
                let email = shown.borrow().clone();
                if email.is_empty() {
                    return;
                }
                let changed = if check.is_active() {
                    state.picked.borrow_mut().insert(email)
                } else {
                    state.picked.borrow_mut().remove(&email)
                };
                if changed {
                    count();
                }
            });
            item.set_child(Some(&check));
            item.set_activatable(false);
        });
    }
    {
        let state = state.clone();
        factory.connect_bind(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(p) = state.person(item.item()) else {
                return;
            };
            let Some(check) = item.child().and_downcast::<gtk::CheckButton>() else {
                return;
            };
            let Some(line) = check.child().and_downcast::<gtk::Box>() else {
                return;
            };
            let text = line.first_child().and_downcast::<gtk::Box>();
            let name = text
                .as_ref()
                .and_then(|t| t.first_child())
                .and_downcast::<gtk::Label>();
            let email = name
                .as_ref()
                .and_then(|n| n.next_sibling())
                .and_downcast::<gtk::Label>();
            let seen = line.last_child().and_downcast::<gtk::Label>();
            let (Some(name), Some(email), Some(seen)) = (name, email, seen) else {
                return;
            };
            let named = !p.name.is_empty();
            name.set_label(if named { &p.name } else { &p.email });
            email.set_label(&p.email);
            email.set_visible(named);
            seen.set_label(&ni18n_f(
                "{n} message",
                "{n} messages",
                p.seen,
                &[("n", &p.seen.to_string())],
            ));
            // The row's address first, so setting the tick records nothing.
            if let Some((_, shown)) = state
                .rows
                .borrow()
                .iter()
                .find(|(c, _)| c.upgrade().as_ref() == Some(&check))
            {
                *shown.borrow_mut() = String::new();
                check.set_active(state.picked.borrow().contains(&p.email));
                *shown.borrow_mut() = p.email.clone();
            }
        });
    }
    let list = gtk::ListView::new(
        Some(gtk::NoSelection::new(Some(filtered.clone()))),
        Some(factory),
    );
    list.add_css_class("navigation-sidebar");
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    let empty = adw::StatusPage::new();
    empty.add_css_class("compact");
    empty.set_vexpand(true);
    let spinner = adw::Spinner::new();
    spinner.set_size_request(32, 32);
    let reading_label = gtk::Label::new(Some(&i18n("Reading your mail…")));
    reading_label.add_css_class("dim-label");
    let reading = gtk::Box::new(gtk::Orientation::Vertical, 12);
    reading.set_valign(gtk::Align::Center);
    reading.set_vexpand(true);
    reading.append(&spinner);
    reading.append(&reading_label);
    let body = gtk::Stack::new();
    body.add_named(&reading, Some("reading"));
    body.add_named(&scroller, Some("list"));
    body.add_named(&empty, Some("empty"));

    // What the list area shows: the people, or why there are none.
    let show_list: Rc<dyn Fn()> = {
        let (body, empty, filtered, state) =
            (body.clone(), empty.clone(), filtered.clone(), state.clone());
        Rc::new(move || {
            if filtered.n_items() > 0 {
                body.set_visible_child_name("list");
                return;
            }
            if state.people.borrow().is_empty() {
                empty.set_icon_name(Some("avatar-default-symbolic"));
                empty.set_title(&i18n("No One New"));
                empty.set_description(Some(&i18n("Everyone in this mail is in Contacts already.")));
            } else {
                empty.set_icon_name(Some("edit-find-symbolic"));
                empty.set_title(&i18n("No Results Found"));
                empty.set_description(None);
            }
            body.set_visible_child_name("empty");
        })
    };

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&choices);
    content.append(&search);
    content.append(&tools);
    content.append(&body);
    let clamp = adw::Clamp::builder()
        .maximum_size(640)
        .child(&content)
        .build();

    // Read the people in the mail the scope row names, once the saved
    // contacts are known.
    let saved: Rc<RefCell<Option<HashSet<String>>>> = Rc::default();
    let (own, skip) = (Rc::new(start.own), Rc::new(start.skip));
    let reread: Rc<dyn Fn()> = {
        let (places, saved) = (places.clone(), saved.clone());
        let (state, model, scope_row, body, show_list, count) = (
            state.clone(),
            model.clone(),
            scope_row.clone(),
            body.clone(),
            show_list.clone(),
            count.clone(),
        );
        Rc::new(move || {
            let place = (scope_row.selected() as usize)
                .checked_sub(1)
                .and_then(|i| places.get(i));
            let scope = AddressScope {
                account: place.map(|p| p.account),
                folder: place.and_then(|p| p.folder.clone()),
                skip: (*skip).clone(),
            };
            let Some(saved) = saved.borrow().clone() else {
                return;
            };
            let own = (*own).clone();
            let gen = state.read.get() + 1;
            state.read.set(gen);
            body.set_visible_child_name("reading");
            let (state, model, show_list, count) = (
                state.clone(),
                model.clone(),
                show_list.clone(),
                count.clone(),
            );
            gtk::glib::spawn_future_local(async move {
                let Ok(people) =
                    gtk::gio::spawn_blocking(move || people_in_mail(&own, &saved, &scope)).await
                else {
                    return;
                };
                if state.read.get() != gen {
                    return;
                }
                // A tick on someone this mail does not hold would import
                // a person the list no longer shows.
                let here: HashSet<&str> = people.iter().map(|p| p.email.as_str()).collect();
                state
                    .picked
                    .borrow_mut()
                    .retain(|e| here.contains(e.as_str()));
                let n = people.len();
                *state.people.borrow_mut() = people;
                let items: Vec<String> = (0..n).map(|i| i.to_string()).collect();
                let refs: Vec<&str> = items.iter().map(String::as_str).collect();
                model.splice(0, model.n_items(), &refs);
                count();
                show_list();
            });
        })
    };
    {
        let reread = reread.clone();
        scope_row.connect_selected_notify(move |_| reread());
    }
    {
        let (state, filter, show_list) = (state.clone(), filter.clone(), show_list.clone());
        search.connect_search_changed(move |entry| {
            *state.query.borrow_mut() = entry.text().trim().to_lowercase();
            filter.changed(gtk::FilterChange::Different);
            show_list();
        });
    }
    // Select All and Select None act on the people the search shows.
    for (button, on) in [(&all, true), (&none, false)] {
        let (state, count) = (state.clone(), count.clone());
        button.connect_clicked(move |_| {
            {
                let people = state.people.borrow();
                let mut picked = state.picked.borrow_mut();
                for p in people.iter().filter(|p| state.shows(p)) {
                    if on {
                        picked.insert(p.email.clone());
                    } else {
                        picked.remove(&p.email);
                    }
                }
            }
            state.sync_rows();
            count();
        });
    }

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&clamp));
    window.set_content(Some(&toolbar));

    let w = window.clone();
    cancel.connect_clicked(move |_| w.close());
    let escape = gtk::ShortcutController::new();
    escape.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("Escape"),
        Some(gtk::CallbackAction::new(|widget, _| {
            if let Some(w) = widget.downcast_ref::<gtk::Window>() {
                w.close();
            }
            gtk::glib::Propagation::Stop
        })),
    ));
    window.add_controller(escape);
    let w = window.clone();
    let (books_read, book_choice) = (books.clone(), book_row.clone());
    import.connect_clicked(move |_| {
        let Some(book) = books.borrow().get(book_row.selected() as usize).cloned() else {
            return;
        };
        let picked: Vec<MailPerson> = {
            let chosen = state.picked.borrow();
            state
                .people
                .borrow()
                .iter()
                .filter(|p| chosen.contains(&p.email))
                .cloned()
                .collect()
        };
        if picked.is_empty() {
            return;
        }
        done(ImportRequest {
            book,
            files: vec![ImportFile::Cards(cards_for(&picked))],
        });
        w.close();
    });
    window.present();
    search.grab_focus();
    let (demo, first_book) = (start.demo, start.book);
    gtk::glib::spawn_future_local(async move {
        let Ok((read, contacts)) = gtk::gio::spawn_blocking(move || books_and_saved(demo)).await
        else {
            return;
        };
        let names: Vec<&str> = read.iter().map(|b| b.name.as_str()).collect();
        book_choice.set_model(Some(&gtk::StringList::new(&names)));
        let at = first_book
            .and_then(|uid| read.iter().position(|b| b.uid == uid))
            .unwrap_or(0);
        book_choice.set_selected(at as u32);
        book_choice.set_sensitive(true);
        *books_read.borrow_mut() = read;
        *saved.borrow_mut() = Some(contacts);
        reread();
    });
    // HYLKI_SHOWCASE_IMPORT=<png> (demo only) captures the window three
    // seconds after it opens; HYLKI_SHOWCASE_LOOK_IN=<text> first picks
    // the first place whose label starts with it, and "open" opens the
    // list of them.
    if let (Ok(png), Some(_)) = (
        std::env::var("HYLKI_SHOWCASE_IMPORT"),
        std::env::var_os("HYLKI_DEMO"),
    ) {
        match std::env::var("HYLKI_SHOWCASE_LOOK_IN").as_deref() {
            Ok("open") => {
                let row = scope_row.clone();
                gtk::glib::timeout_add_seconds_local_once(1, move || {
                    WidgetExt::activate(&row);
                });
            }
            Ok(text) => {
                if let Some(at) = places.iter().position(|p| p.label.starts_with(text)) {
                    scope_row.set_selected(at as u32 + 1);
                }
            }
            Err(_) => {}
        }
        gtk::glib::timeout_add_seconds_local_once(3, move || {
            crate::ui::showcase::showcase_capture(window.upcast_ref(), &png);
        });
    }
}
