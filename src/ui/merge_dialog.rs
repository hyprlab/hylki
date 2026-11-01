//! The Mail Merge dialog (#447): where the rows come from, a preview of each
//! message as it will go, and Send for all of them.
//!
//! An `adw::MessageDialog`, as Create Event is (see `ui::dialog`).

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;

use crate::config::ComposeFormat;
use crate::contacts::Contact;
use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::mail::merge::{group_table, parse_csv, Letter, Table};

/// What the dialog opens with.
pub(crate) struct Start {
    /// The message as written, fields and all.
    pub(crate) letter: Letter,
    pub(crate) format: ComposeFormat,
    /// The contact groups that can be sent to instead of a file, by name.
    pub(crate) groups: Vec<(String, Vec<Contact>)>,
}

/// The messages the rows make, and what stands in the way of sending them.
#[derive(Default)]
struct Merged {
    letters: Vec<Letter>,
    /// Rows left out for want of an address.
    skipped: usize,
    /// Why nothing can be sent yet.
    problem: Option<String>,
}

/// The table's messages. A To with nothing in it goes to the column of
/// addresses; one with no field would send every message to the same place.
fn merge(start: &Start, table: &Table) -> Merged {
    let mut letter = start.letter.clone();
    let needs_field = || Merged {
        problem: Some(i18n(
            "To needs the field that holds the address, such as {{Email}}.",
        )),
        ..Default::default()
    };
    if letter.to.trim().is_empty() {
        match table.address_column() {
            Some(col) => letter.to = format!("{{{{{}}}}}", table.columns[col]),
            None => return needs_field(),
        }
    } else if !letter.to_has_field() {
        return needs_field();
    }
    if let Some(field) = table.unknown_fields(&letter).first() {
        return Merged {
            problem: Some(i18n_f("There is no field “{field}”.", &[("field", field)])),
            ..Default::default()
        };
    }
    let mut merged = Merged::default();
    for row in 0..table.rows.len() {
        let one = table.letter_for(&letter, row, start.format);
        if one.sendable() {
            merged.letters.push(one);
        } else {
            merged.skipped += 1;
        }
    }
    if merged.letters.is_empty() {
        merged.problem = Some(i18n("No row has an address to send to."));
    }
    merged
}

/// A label for one line of the preview's header.
fn header_line(grid: &gtk::Grid, row: i32, title: &str) -> gtk::Label {
    let name = gtk::Label::new(Some(title));
    name.set_xalign(1.0);
    name.set_yalign(0.0);
    name.add_css_class("dim-label");
    let value = gtk::Label::new(None);
    value.set_xalign(0.0);
    value.set_wrap(true);
    value.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    value.set_hexpand(true);
    value.set_selectable(true);
    grid.attach(&name, 0, row, 1, 1);
    grid.attach(&value, 1, row, 1, 1);
    value
}

/// Show the dialog over `parent`. `done` gets the messages to send, one per
/// row with an address.
#[allow(deprecated)] // adw::MessageDialog, see ui::dialog
pub(crate) fn show(parent: &gtk::Window, start: Start, done: impl Fn(Vec<Letter>) + 'static) {
    let dialog = adw::MessageDialog::new(
        Some(parent),
        Some(&i18n("Mail Merge")),
        Some(&i18n(
            "One message per row. A field written as {{Name}} is filled in from the row.",
        )),
    );
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("send", &i18n("Send"));
    dialog.set_response_appearance("send", adw::ResponseAppearance::Suggested);
    dialog.set_response_enabled("send", false);
    dialog.set_close_response("cancel");

    let form = gtk::ListBox::new();
    form.add_css_class("boxed-list");
    form.set_selection_mode(gtk::SelectionMode::None);

    // Contact groups are offered only where there are some.
    let source = (!start.groups.is_empty()).then(|| {
        let row = adw::ComboRow::new();
        row.set_title(&i18n("Send to"));
        let mut labels = vec![i18n("A CSV file")];
        labels.extend(start.groups.iter().map(|(name, _)| name.clone()));
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        row.set_model(Some(&gtk::StringList::new(&labels)));
        form.append(&row);
        row
    });

    let file_row = adw::ActionRow::new();
    file_row.set_title(&i18n("CSV file"));
    file_row.set_subtitle(&i18n("Its first row names the fields."));
    let choose = gtk::Button::with_label(&i18n("Choose…"));
    choose.set_valign(gtk::Align::Center);
    file_row.add_suffix(&choose);
    file_row.set_activatable_widget(Some(&choose));
    form.append(&file_row);

    let fields_row = adw::ActionRow::new();
    fields_row.set_title(&i18n("Fields"));
    fields_row.set_subtitle_selectable(true);
    fields_row.set_visible(false);
    form.append(&fields_row);

    let note = gtk::Label::new(None);
    note.set_xalign(0.0);
    note.set_wrap(true);
    note.set_visible(false);

    // The preview: one message at a time, stepped through with the arrows.
    let back = gtk::Button::from_icon_name("go-previous-symbolic");
    back.add_css_class("flat");
    back.set_tooltip_text(Some(&i18n("Previous")));
    let next = gtk::Button::from_icon_name("go-next-symbolic");
    next.add_css_class("flat");
    next.set_tooltip_text(Some(&i18n("Next")));
    let count = gtk::Label::new(None);
    count.add_css_class("numeric");
    let title = gtk::Label::new(Some(&i18n("Preview")));
    title.add_css_class("heading");
    title.set_xalign(0.0);
    title.set_hexpand(true);
    let stepper = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    stepper.append(&title);
    stepper.append(&back);
    stepper.append(&count);
    stepper.append(&next);

    let grid = gtk::Grid::new();
    grid.set_column_spacing(12);
    grid.set_row_spacing(4);
    let to = header_line(&grid, 0, &i18n("To"));
    let subject = header_line(&grid, 1, &i18n("Subject"));
    let body = gtk::Label::new(None);
    body.set_xalign(0.0);
    body.set_yalign(0.0);
    body.set_wrap(true);
    body.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    body.set_selectable(true);
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_min_content_height(160);
    scroller.set_max_content_height(240);
    scroller.set_child(Some(&body));
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.add_css_class("card");
    for side in [
        gtk::Widget::set_margin_top,
        gtk::Widget::set_margin_bottom,
        gtk::Widget::set_margin_start,
        gtk::Widget::set_margin_end,
    ] {
        side(grid.upcast_ref(), 10);
        side(scroller.upcast_ref(), 10);
    }
    card.append(&grid);
    card.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    card.append(&scroller);
    let preview = gtk::Box::new(gtk::Orientation::Vertical, 6);
    preview.append(&stepper);
    preview.append(&card);
    preview.set_visible(false);

    let bx = gtk::Box::new(gtk::Orientation::Vertical, 12);
    bx.set_size_request(460, -1);
    bx.append(&form);
    bx.append(&note);
    bx.append(&preview);
    dialog.set_extra_child(Some(&bx));

    let start = Rc::new(start);
    let merged = Rc::new(RefCell::new(Merged::default()));
    let shown = Rc::new(std::cell::Cell::new(0usize));

    // The message on show.
    let show_one = {
        let (merged, shown, start) = (merged.clone(), shown.clone(), start.clone());
        let (back, next, count) = (back.clone(), next.clone(), count.clone());
        move || {
            let merged = merged.borrow();
            let n = merged.letters.len();
            let i = shown.get().min(n.saturating_sub(1));
            let Some(letter) = merged.letters.get(i) else {
                return;
            };
            to.set_label(&letter.to);
            subject.set_label(&letter.subject);
            body.set_label(letter.preview_text(start.format).trim());
            count.set_label(&i18n_f(
                "{n} of {total}",
                &[("n", &(i + 1).to_string()), ("total", &n.to_string())],
            ));
            back.set_sensitive(i > 0);
            next.set_sensitive(i + 1 < n);
        }
    };
    let show_one = Rc::new(show_one);

    // A table read or picked: its fields, its messages, the count to send.
    let use_table = {
        let (merged, shown, start) = (merged.clone(), shown.clone(), start.clone());
        let (dialog, fields_row, note, preview) = (
            dialog.clone(),
            fields_row.clone(),
            note.clone(),
            preview.clone(),
        );
        let show_one = show_one.clone();
        move |table: Result<Option<Table>, String>| {
            let result = match &table {
                Ok(Some(t)) => merge(&start, t),
                Ok(None) => Merged::default(),
                Err(e) => Merged {
                    problem: Some(e.clone()),
                    ..Default::default()
                },
            };
            match &table {
                Ok(Some(t)) => {
                    let names: Vec<String> =
                        t.columns.iter().map(|c| format!("{{{{{c}}}}}")).collect();
                    fields_row.set_subtitle(&names.join("  "));
                    fields_row.set_visible(true);
                }
                _ => fields_row.set_visible(false),
            }
            note.remove_css_class("error");
            note.remove_css_class("dim-label");
            if let Some(problem) = &result.problem {
                note.set_label(problem);
                note.add_css_class("error");
            } else if result.skipped > 0 {
                let n = result.skipped as u32;
                note.set_label(&ni18n_f(
                    "{n} row has no address and is left out.",
                    "{n} rows have no address and are left out.",
                    n,
                    &[("n", &n.to_string())],
                ));
                note.add_css_class("dim-label");
            }
            note.set_visible(result.problem.is_some() || result.skipped > 0);
            let n = result.letters.len() as u32;
            let ready = result.problem.is_none() && n > 0;
            dialog.set_response_enabled("send", ready);
            dialog.set_response_label(
                "send",
                &if ready {
                    ni18n_f(
                        "Send {n} Message",
                        "Send {n} Messages",
                        n,
                        &[("n", &n.to_string())],
                    )
                } else {
                    i18n("Send")
                },
            );
            preview.set_visible(ready);
            *merged.borrow_mut() = result;
            shown.set(0);
            show_one();
        }
    };
    let use_table = Rc::new(use_table);
    // The file last read, kept for going back to it from a group.
    let file_table: Rc<RefCell<Result<Option<Table>, String>>> = Rc::new(RefCell::new(Ok(None)));

    {
        let (shown, show_one) = (shown.clone(), show_one.clone());
        back.connect_clicked(move |_| {
            shown.set(shown.get().saturating_sub(1));
            show_one();
        });
    }
    {
        let (shown, show_one) = (shown.clone(), show_one.clone());
        next.connect_clicked(move |_| {
            shown.set(shown.get() + 1);
            show_one();
        });
    }

    {
        let (use_table, file_table, file_row) =
            (use_table.clone(), file_table.clone(), file_row.clone());
        let dialog_ref = dialog.clone();
        choose.connect_clicked(move |_| {
            let picker = csv_file_dialog();
            let (use_table, file_table, file_row) =
                (use_table.clone(), file_table.clone(), file_row.clone());
            picker.open(Some(&dialog_ref), gtk::gio::Cancellable::NONE, move |res| {
                let Some(path) = res.ok().and_then(|f| f.path()) else {
                    return;
                };
                let table = read_table(&path);
                file_row.set_subtitle(
                    &path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                );
                *file_table.borrow_mut() = table.clone();
                use_table(table);
            });
        });
    }

    if let Some(row) = &source {
        let (use_table, file_table, file_row, start) = (
            use_table.clone(),
            file_table.clone(),
            file_row.clone(),
            start.clone(),
        );
        row.connect_selected_notify(move |row| {
            let picked = row.selected() as usize;
            file_row.set_visible(picked == 0);
            match picked.checked_sub(1).and_then(|i| start.groups.get(i)) {
                Some((_, members)) => use_table(Ok(Some(group_table(members)))),
                None => use_table(file_table.borrow().clone()),
            }
        });
    }

    dialog.connect_response(None, move |_, resp| {
        if resp == "send" {
            done(std::mem::take(&mut merged.borrow_mut().letters));
        }
    });
    // The file last chosen, here or in the field palette, to start from.
    if let Some(path) = crate::config::load_merge_file() {
        if let Some(name) = path.file_name() {
            file_row.set_subtitle(&name.to_string_lossy());
        }
        let table = read_table(&path);
        *file_table.borrow_mut() = table.clone();
        use_table(table);
    }
    dialog.present();
    showcase(&dialog, &file_row, &*use_table);
}

/// HYLKI_SHOWCASE_MERGE=<csv>:<png> (demo only) reads the file as if it
/// were chosen and captures the dialog two seconds later; with
/// HYLKI_SHOWCASE_MERGE_SEND it then sends.
#[allow(deprecated)] // adw::MessageDialog, see ui::dialog
fn showcase(
    dialog: &adw::MessageDialog,
    file_row: &adw::ActionRow,
    use_table: &dyn Fn(Result<Option<Table>, String>),
) {
    let (Ok(v), Some(_)) = (
        std::env::var("HYLKI_SHOWCASE_MERGE"),
        std::env::var_os("HYLKI_DEMO"),
    ) else {
        return;
    };
    let Some((csv, png)) = v.split_once(':') else {
        return;
    };
    file_row.set_subtitle(csv);
    use_table(read_table(Path::new(csv)));
    let (dialog, png) = (dialog.clone(), png.to_string());
    gtk::glib::timeout_add_seconds_local_once(2, move || {
        crate::ui::showcase::showcase_capture(dialog.upcast_ref(), &png);
        if std::env::var_os("HYLKI_SHOWCASE_MERGE_SEND").is_some() {
            dialog.response("send");
        }
    });
}

/// The file chooser for a merge's CSV file.
pub(crate) fn csv_file_dialog() -> gtk::FileDialog {
    let picker = gtk::FileDialog::builder()
        .title(i18n("Choose a CSV File"))
        .build();
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(&i18n("CSV files")));
    for pattern in ["*.csv", "*.CSV", "*.tsv", "*.txt"] {
        filter.add_pattern(pattern);
    }
    filter.add_mime_type("text/csv");
    let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let any = gtk::FileFilter::new();
    any.set_name(Some(&i18n("All files")));
    any.add_pattern("*");
    filters.append(&any);
    picker.set_filters(Some(&filters));
    picker
}

/// A CSV file's table, or why it could not be read.
fn read_table(path: &Path) -> Result<Option<Table>, String> {
    let bytes = std::fs::read(path).map_err(|e| {
        i18n_f(
            "The file could not be read: {error}",
            &[("error", &e.to_string())],
        )
    })?;
    parse_csv(&bytes).map(Some)
}
