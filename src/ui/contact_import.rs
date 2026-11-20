//! The Import Contacts window (#655): the address book the contacts go to,
//! for each CSV file, the contact field each of its columns fills, and a
//! switch for each Thunderbird book found (#681).
//! The columns start matched by their headers; a column no row has a
//! value in is not listed.
//!
//! A window of its own, as the alias editor is: a message dialog is too
//! narrow for a row per column.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;

use crate::contacts::import::{
    can_import, contacts_from, detect, targets, ImportFile, ImportRequest, ImportStart, Target,
};
use crate::i18n::{i18n, ni18n_f};
use crate::mail::merge::Table;

/// One CSV file's matching, as the window keeps it.
struct CsvForm {
    path: PathBuf,
    table: Table,
    fields: RefCell<Vec<Target>>,
    group: adw::PreferencesGroup,
}

impl CsvForm {
    /// Say under the file's name what the matching makes of it, or what it
    /// lacks.
    fn update_note(&self) {
        let fields = self.fields.borrow();
        if !can_import(&fields) {
            self.group.set_description(Some(&i18n(
                "Match a column to a name, an email address or a phone number.",
            )));
            return;
        }
        let found = contacts_from(&self.table, &fields);
        let count = |one: &str, many: &str, n: usize| {
            ni18n_f(one, many, n as u32, &[("n", &n.to_string())])
        };
        let mut parts = vec![count("{n} contact", "{n} contacts", found.cards.len())];
        if !found.groups.is_empty() {
            parts.push(count("{n} group", "{n} groups", found.groups.len()));
        }
        if found.skipped > 0 {
            parts.push(count(
                "{n} row with no name, address or phone is skipped",
                "{n} rows with no name, address or phone are skipped",
                found.skipped,
            ));
        }
        self.group.set_description(Some(&parts.join(" · ")));
    }
}

/// The first values of a column, to show what it holds.
fn sample(table: &Table, col: usize) -> String {
    let values: Vec<&str> = table
        .rows
        .iter()
        .map(|r| r[col].trim())
        .filter(|v| !v.is_empty())
        .take(2)
        .collect();
    let text = values.join(", ").replace(['\r', '\n'], " ");
    match text.char_indices().nth(60) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text,
    }
}

/// How many contacts and lists a Thunderbird book holds.
fn book_counts(contacts: usize, lists: usize) -> String {
    let count =
        |one: &str, many: &str, n: usize| ni18n_f(one, many, n as u32, &[("n", &n.to_string())]);
    let mut parts = vec![count("{n} contact", "{n} contacts", contacts)];
    if lists > 0 {
        parts.push(count("{n} group", "{n} groups", lists));
    }
    parts.join(" · ")
}

/// A file's name, for a group title.
fn file_title(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Show the window over `parent`; `done` gets what to import, never called
/// when it is cancelled.
pub(crate) fn show(
    parent: Option<&gtk::Window>,
    start: ImportStart,
    done: impl Fn(ImportRequest) + 'static,
) {
    let window = adw::Window::builder()
        .modal(true)
        .default_width(560)
        .default_height(680)
        .title(i18n("Import Contacts"))
        .build();
    window.set_transient_for(parent);
    let cancel = gtk::Button::with_label(&i18n("Cancel"));
    let import = gtk::Button::with_label(&i18n("Import"));
    import.add_css_class("suggested-action");
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&import);
    let page = adw::PreferencesPage::new();

    let books = start.books;
    let book_group = adw::PreferencesGroup::new();
    let book_row = adw::ComboRow::new();
    book_row.set_title(&i18n("Import into"));
    let names: Vec<&str> = books.iter().map(|b| b.name.as_str()).collect();
    book_row.set_model(Some(&gtk::StringList::new(&names)));
    let at = start
        .book
        .and_then(|uid| books.iter().position(|b| b.uid == uid))
        .unwrap_or(0);
    book_row.set_selected(at as u32);
    // A book with no groups of its own keeps a row's labels on the card.
    let groupless = {
        let books = books.clone();
        move |row: &adw::ComboRow| {
            let none = books
                .get(row.selected() as usize)
                .is_some_and(|b| b.groups.is_none());
            row.set_subtitle(&if none {
                i18n("Keeps no groups: their names stay on each contact.")
            } else {
                String::new()
            });
        }
    };
    groupless(&book_row);
    book_row.connect_selected_notify(groupless);
    book_group.add(&book_row);
    page.add(&book_group);

    let choices = Rc::new(targets());
    let choice_names: Vec<String> = choices.iter().map(|t| t.name()).collect();
    let choice_refs: Vec<&str> = choice_names.iter().map(String::as_str).collect();
    let mut vcards: Vec<PathBuf> = Vec::new();
    let forms: Rc<RefCell<Vec<Rc<CsvForm>>>> = Rc::default();
    let has_vcards = Rc::new(Cell::new(false));
    // Each Thunderbird book, and whether its switch is on.
    let tb_books: Rc<RefCell<Vec<(PathBuf, bool)>>> = Rc::default();
    // Import needs a file or a book to import, and every CSV file matched.
    let check: Rc<dyn Fn()> = {
        let (forms, has_vcards, tb_books, import) = (
            forms.clone(),
            has_vcards.clone(),
            tb_books.clone(),
            import.downgrade(),
        );
        Rc::new(move || {
            let Some(import) = import.upgrade() else {
                return;
            };
            let forms = forms.borrow();
            let some_book = tb_books.borrow().iter().any(|(_, on)| *on);
            let ready = (!forms.is_empty() || has_vcards.get() || some_book)
                && forms.iter().all(|f| can_import(&f.fields.borrow()));
            import.set_sensitive(ready);
        })
    };
    let unreadable = adw::PreferencesGroup::new();
    unreadable.set_title(&i18n("Not imported"));
    let mut any_unreadable = false;

    for file in start.files {
        let table = match file.table {
            None => {
                vcards.push(file.path);
                continue;
            }
            Some(Err(e)) => {
                let row = adw::ActionRow::new();
                row.set_title(&file.name);
                row.set_subtitle(&e);
                unreadable.add(&row);
                any_unreadable = true;
                continue;
            }
            Some(Ok(table)) => table,
        };
        let fields = detect(&table);
        let group = adw::PreferencesGroup::new();
        group.set_title(&file.name);
        let form = Rc::new(CsvForm {
            path: file.path,
            fields: RefCell::new(fields.clone()),
            group: group.clone(),
            table,
        });
        for (col, header) in form.table.columns.iter().enumerate() {
            let value = sample(&form.table, col);
            if value.is_empty() {
                continue;
            }
            let row = adw::ComboRow::new();
            row.set_title(header);
            row.set_subtitle(&value);
            row.set_model(Some(&gtk::StringList::new(&choice_refs)));
            row.set_expression(Some(gtk::PropertyExpression::new(
                gtk::StringObject::static_type(),
                gtk::Expression::NONE,
                "string",
            )));
            row.set_enable_search(true);
            let at = choices.iter().position(|t| *t == fields[col]).unwrap_or(0);
            row.set_selected(at as u32);
            group.add(&row);
            let (form, choices, check) = (form.clone(), choices.clone(), check.clone());
            row.connect_selected_notify(move |row| {
                let picked = choices
                    .get(row.selected() as usize)
                    .copied()
                    .unwrap_or(Target::Ignore);
                form.fields.borrow_mut()[col] = picked;
                form.update_note();
                check();
            });
        }
        form.update_note();
        page.add(&group);
        forms.borrow_mut().push(form);
    }
    if !vcards.is_empty() {
        let group = adw::PreferencesGroup::new();
        group.set_title(&i18n("vCard files"));
        for path in &vcards {
            let row = adw::ActionRow::new();
            row.set_title(&file_title(path));
            group.add(&row);
        }
        page.add(&group);
    }
    let mut profiles: Vec<&str> = Vec::new();
    for b in &start.thunderbird {
        if !profiles.contains(&b.profile.as_str()) {
            profiles.push(&b.profile);
        }
    }
    for profile in &profiles {
        let group = adw::PreferencesGroup::new();
        group.set_title("Thunderbird");
        if profiles.len() > 1 {
            group.set_description(Some(profile));
        }
        for b in start.thunderbird.iter().filter(|b| b.profile == *profile) {
            let row = adw::SwitchRow::new();
            row.set_title(&b.name);
            row.set_subtitle(&book_counts(b.contacts, b.lists));
            // Collected Addresses is everyone ever written to.
            row.set_active(!b.collected);
            let at = {
                let mut books = tb_books.borrow_mut();
                books.push((b.path.clone(), row.is_active()));
                books.len() - 1
            };
            let (tb_books, check) = (tb_books.clone(), check.clone());
            row.connect_active_notify(move |row| {
                tb_books.borrow_mut()[at].1 = row.is_active();
                check();
            });
            group.add(&row);
        }
        page.add(&group);
    }
    if any_unreadable {
        page.add(&unreadable);
    }
    has_vcards.set(!vcards.is_empty());
    check();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
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
    import.connect_clicked(move |_| {
        let Some(book) = books.get(book_row.selected() as usize).cloned() else {
            return;
        };
        let mut files: Vec<ImportFile> = vcards.iter().cloned().map(ImportFile::Vcard).collect();
        files.extend(forms.borrow().iter().map(|f| ImportFile::Csv {
            path: f.path.clone(),
            fields: f.fields.borrow().clone(),
        }));
        files.extend(
            tb_books
                .borrow()
                .iter()
                .filter(|(_, on)| *on)
                .map(|(path, _)| ImportFile::Thunderbird(path.clone())),
        );
        done(ImportRequest { book, files });
        w.close();
    });
    window.present();
    // HYLKI_SHOWCASE_IMPORT=<png>[@<fraction>] (demo only) captures the
    // window two seconds after it opens, scrolled that far down.
    if let (Ok(spec), Some(_)) = (
        std::env::var("HYLKI_SHOWCASE_IMPORT"),
        std::env::var_os("HYLKI_DEMO"),
    ) {
        let (png, down) = match spec.rsplit_once('@') {
            Some((png, f)) => (png.to_string(), f.parse::<f64>().unwrap_or(0.0)),
            None => (spec, 0.0),
        };
        gtk::glib::timeout_add_seconds_local_once(2, move || {
            if let Some(sw) = page
                .first_child()
                .and_then(|c| c.downcast::<gtk::ScrolledWindow>().ok())
            {
                let adj = sw.vadjustment();
                adj.set_value((adj.upper() - adj.page_size()) * down);
            }
            let w = window.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                crate::ui::showcase::showcase_capture(w.upcast_ref(), &png);
            });
        });
    }
}
