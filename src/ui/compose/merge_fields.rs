//! The field palette (#447): the mail merge fields a click away, put in at
//! the cursor in the subject or the body, from a button beside each or with
//! Ctrl+Shift+M. It offers a contact group's fields and the columns of the
//! CSV file last chosen, and a file can be chosen from it.

use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use super::messages::ComposeInput;
use super::{Compose, ComposeWidgets};
use crate::i18n::i18n;
use crate::mail::merge::{csv_columns, group_columns};

/// Where a field picked in the palette goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum FieldTarget {
    Subject,
    Body,
    /// The subject if the cursor is in it, the body otherwise: the shortcut.
    Focused,
}

/// The GtkText inside the subject row, which holds its cursor.
fn subject_text(widgets: &ComposeWidgets) -> Option<gtk::Text> {
    let mut stack = vec![widgets.subject_row.clone().upcast::<gtk::Widget>()];
    while let Some(w) = stack.pop() {
        if let Ok(t) = w.clone().downcast::<gtk::Text>() {
            return Some(t);
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            stack.push(c);
        }
    }
    None
}

impl Compose {
    /// Handles [`ComposeInput::MergeFields`].
    pub(super) fn on_merge_fields(
        &mut self,
        target: FieldTarget,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let Some(text) = subject_text(widgets) else {
            return;
        };
        let target = match target {
            FieldTarget::Focused if text.has_focus() => FieldTarget::Subject,
            FieldTarget::Focused => FieldTarget::Body,
            other => other,
        };
        let choose: Rc<dyn Fn()> = {
            let s = sender.input_sender().clone();
            let row = widgets.subject_row.clone();
            Rc::new(move || {
                let s = s.clone();
                let parent = row.root().and_downcast::<gtk::Window>();
                crate::ui::merge_dialog::csv_file_dialog().open(
                    parent.as_ref(),
                    gtk::gio::Cancellable::NONE,
                    move |res| {
                        if let Some(path) = res.ok().and_then(|f| f.path()) {
                            crate::config::save_merge_file(&path);
                            s.emit(ComposeInput::MergeFields(target));
                        }
                    },
                );
            })
        };
        match target {
            FieldTarget::Subject => {
                // Read now: the palette takes the focus, and a selection
                // is what the field replaces.
                let (start, end) = text.selection_bounds().unwrap_or_else(|| {
                    let at = text.position();
                    (at, at)
                });
                let put = {
                    let text = text.clone();
                    Rc::new(move |field: &str| {
                        text.delete_text(start, end);
                        let mut at = start;
                        text.insert_text(field, &mut at);
                        text.grab_focus_without_selecting();
                        text.set_position(at);
                    })
                };
                let popover = palette(put, choose);
                let row = widgets.subject_row.clone();
                popover.set_parent(&row);
                let (cursor, _) = text.compute_cursor_extents(start as usize);
                let point = gtk::graphene::Point::new(cursor.x(), cursor.y());
                if let Some(p) = text.compute_point(&row, &point) {
                    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                        p.x() as i32,
                        p.y() as i32,
                        1,
                        cursor.height().max(1.0) as i32,
                    )));
                }
                popover.connect_closed(|p| {
                    let p = p.clone();
                    gtk::glib::idle_add_local_once(move || p.unparent());
                });
                popover.popup();
                let text = text.clone();
                showcase(&popover, move |log| log(text.text().to_string()));
            }
            _ => {
                let editor = self.editor.clone();
                let put = Rc::new(move |field: &str| editor.insert_at_caret(field));
                let popover = palette(put, choose);
                self.editor.popup_at_caret(popover.clone());
                let editor = self.editor.clone();
                showcase(&popover, move |log| {
                    editor.eval(
                        "(document.getElementById('src')||{}).value||document.body.innerHTML",
                        log,
                    )
                });
            }
        }
    }
}

/// HYLKI_SHOWCASE_MERGE_FIELDS=<subject|body>:<png>[:<n>] (demo only; see
/// `compose::showcase`): the palette captured a second after it opens,
/// then its `n`th field clicked and what the subject or body holds logged.
fn showcase(popover: &gtk::Popover, read: impl Fn(Box<dyn FnOnce(String)>) + 'static) {
    let (Ok(v), Some(_)) = (
        std::env::var("HYLKI_SHOWCASE_MERGE_FIELDS"),
        std::env::var_os("HYLKI_DEMO"),
    ) else {
        return;
    };
    let mut parts = v.splitn(3, ':').skip(1);
    let png = parts.next().unwrap_or_default().to_string();
    let pick = parts.next().and_then(|n| n.parse::<usize>().ok());
    let popover = popover.clone();
    gtk::glib::timeout_add_seconds_local_once(1, move || {
        crate::ui::showcase::showcase_capture(popover.upcast_ref(), &png);
        let Some(n) = pick else { return };
        let mut buttons = Vec::new();
        let mut stack = vec![popover.clone().upcast::<gtk::Widget>()];
        while let Some(w) = stack.pop() {
            if let Ok(b) = w.clone().downcast::<gtk::Button>() {
                buttons.push(b);
                continue;
            }
            let mut child = w.last_child();
            while let Some(c) = child {
                child = c.prev_sibling();
                stack.push(c);
            }
        }
        if let Some(b) = buttons.get(n) {
            b.emit_clicked();
        }
        gtk::glib::timeout_add_seconds_local_once(1, move || {
            read(Box::new(|s| tracing::info!("showcase merge field: {s}")));
        });
    });
}

/// The palette: the last file's columns under its name, a contact group's
/// fields, and Choose a CSV File…. A pick is put in once the popover has
/// closed and given the focus back.
fn palette(put: Rc<dyn Fn(&str)>, choose: Rc<dyn Fn()>) -> gtk::Popover {
    let popover = gtk::Popover::new();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    list.add_css_class("context-menu-list");
    let heading = |title: &str| {
        let label = gtk::Label::new(Some(title));
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        label.add_css_class("dim-label");
        label.add_css_class("caption-heading");
        label.add_css_class("context-menu-item");
        label
    };
    let item = |label: &str, then: Box<dyn Fn()>| {
        let button = gtk::Button::new();
        button.add_css_class("flat");
        button.add_css_class("context-menu-item");
        let text = gtk::Label::new(Some(label));
        text.set_xalign(0.0);
        button.set_child(Some(&text));
        let popover = popover.downgrade();
        button.connect_clicked(move |_| {
            if let Some(p) = popover.upgrade() {
                p.popdown();
            }
            then();
        });
        button
    };
    let field = |name: &str| {
        let put = put.clone();
        let written = format!("{{{{{name}}}}}");
        item(
            name,
            Box::new(move || {
                let (put, written) = (put.clone(), written.clone());
                gtk::glib::idle_add_local_once(move || put(&written));
            }),
        )
    };

    // The file chosen comes first: it is what a merge is likeliest to use.
    if let Some(path) = crate::config::load_merge_file() {
        let columns = csv_columns(&path);
        if !columns.is_empty() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            list.append(&heading(&name));
            for column in &columns {
                list.append(&field(column));
            }
            list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        }
    }
    list.append(&heading(&i18n("Contact Groups")));
    for name in group_columns() {
        list.append(&field(&name));
    }
    list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    list.append(&item(
        &i18n("Choose a CSV File…"),
        Box::new(move || {
            let choose = choose.clone();
            gtk::glib::idle_add_local_once(move || choose());
        }),
    ));

    // A file with many columns scrolls rather than run off the screen.
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_propagate_natural_height(true);
    scroller.set_propagate_natural_width(true);
    scroller.set_max_content_height(640);
    scroller.set_child(Some(&list));
    popover.set_child(Some(&scroller));
    popover
}
