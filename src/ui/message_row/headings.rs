//! The single line's column headings and their resize handles, and the
//! small labels a row draws (due dates, pills, the tag menu).

use super::shared::RowLook;
use super::{column_width, default_width, resizable, LINE_ICON_PX};
use crate::config::ListColumn;
use crate::i18n::i18n;
use crate::models::Message;
use crate::ui::column_bin::ColumnBin;
use crate::ui::context_menu::MenuEntry;
use adw::prelude::*;
use relm4::prelude::*;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

/// Fill `bar` with the single line's column headings (#334), laid out as a
/// row is so each sits over its column. `sorted` is the column the list is
/// sorted by and whether it runs downwards; a heading sorts the list by its
/// column when clicked.
pub(crate) fn fill_headings(
    bar: &gtk::Box,
    look: &RowLook,
    sorted: Option<(ListColumn, bool)>,
    click: Rc<dyn Fn(ListColumn)>,
    resize: Rc<dyn Fn(ListColumn, Option<i32>, bool)>,
) -> HashMap<ListColumn, ColumnBin> {
    let mut bins = HashMap::new();
    while let Some(child) = bar.first_child() {
        bar.remove(&child);
    }
    let mut classes = vec!["message-row", "single-line", "list-headings"];
    if !look.avatars {
        classes.push("no-avatar");
    }
    bar.set_css_classes(&classes);
    bar.set_spacing(8);
    // As wide as the rows under it, so the two scroll sideways together.
    bar.set_size_request(look.min_width, -1);
    // The avatar's and the unread dot's places.
    if look.avatars {
        let room = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        room.set_size_request(16, -1);
        bar.append(&room);
    }
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.set_size_request(10, -1);
    bar.append(&dot);
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.set_hexpand(true);
    bar.append(&line);

    let subject_at = look
        .columns
        .iter()
        .position(|c| *c == ListColumn::Subject)
        .unwrap_or(0);
    for (at, &column) in look.columns.iter().enumerate() {
        let arrow =
            sorted
                .filter(|(c, _)| *c == column)
                .map(|(_, down)| if down { " \u{25BE}" } else { " \u{25B4}" });
        let text = |name: String| {
            let label = gtk::Label::new(Some(&format!("{name}{}", arrow.unwrap_or(""))));
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.add_css_class("list-heading");
            label
        };
        let icon = |name: &str, px: i32| {
            let image = gtk::Image::from_icon_name(name);
            image.set_pixel_size(px);
            image.add_css_class("list-heading");
            image.upcast::<gtk::Widget>()
        };
        let mut bin = None;
        let mut binned = |label: gtk::Label| {
            let b = ColumnBin::new(column_width(look, column));
            b.set_child(Some(&label));
            bin = Some(b.clone());
            b.upcast::<gtk::Widget>()
        };
        let cell: gtk::Widget = match column {
            ListColumn::Star => icon("starred-symbolic", LINE_ICON_PX),
            ListColumn::Replied => {
                let both = gtk::Box::new(gtk::Orientation::Horizontal, 2);
                both.append(&icon("mail-reply-sender-symbolic", LINE_ICON_PX));
                both.append(&icon("mail-forward-symbolic", LINE_ICON_PX));
                both.upcast()
            }
            ListColumn::Attachment => icon("mail-attachment-symbolic", LINE_ICON_PX),
            ListColumn::Importance => icon("emblem-important-symbolic", -1),
            ListColumn::Sender => binned(text(if look.show_recipient {
                i18n("To")
            } else {
                i18n("From")
            })),
            ListColumn::Recipients => binned(text(i18n("To"))),
            ListColumn::Correspondents | ListColumn::Account => binned(text(i18n(column.label()))),
            ListColumn::Subject => {
                let b = ColumnBin::new(0);
                b.set_child(Some(&text(i18n(column.label()))));
                b.set_hexpand(true);
                b.upcast()
            }
            ListColumn::Tags => binned(text(i18n(column.label()))),
            ListColumn::Due | ListColumn::Date => {
                let label = text(if column == ListColumn::Due {
                    i18n("Due")
                } else {
                    i18n(column.label())
                });
                label.set_width_chars(9);
                label.set_xalign(1.0);
                binned(label)
            }
        };
        if let Some(bin) = &bin {
            bins.insert(column, bin.clone());
        }
        let cell = match bin {
            Some(bin) if resizable(column) => {
                resize_handle(cell, &bin, column, at < subject_at, resize.clone())
            }
            _ => cell,
        };
        cell.set_tooltip_text(Some(&i18n(column.label())));
        if arrow.is_some() {
            cell.add_css_class("sorted");
        }
        cell.set_cursor_from_name(Some("pointer"));
        let click = click.clone();
        let gesture = gtk::GestureClick::new();
        gesture.connect_released(move |_, _, _, _| click(column));
        cell.add_controller(gesture);
        line.append(&cell);
    }
    bins
}

/// A heading with a handle for dragging its column's width (#334), on the
/// edge it shares with the subject: the right edge of a column before it,
/// the left of one after it, so the edge follows the pointer. Each step of
/// a drag is handed to `resize`, the last marked done; a double click puts
/// the column back to its own width.
fn resize_handle(
    cell: gtk::Widget,
    bin: &ColumnBin,
    column: ListColumn,
    before_subject: bool,
    resize: Rc<dyn Fn(ListColumn, Option<i32>, bool)>,
) -> gtk::Widget {
    const MIN: i32 = 32;
    const MAX: i32 = 800;
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&cell));
    let handle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    handle.add_css_class("column-resize-handle");
    handle.set_size_request(8, -1);
    handle.set_halign(if before_subject {
        gtk::Align::End
    } else {
        gtk::Align::Start
    });
    handle.set_cursor_from_name(Some("col-resize"));
    handle.set_tooltip_text(Some(&i18n(
        "Drag to resize, double-click for the usual width",
    )));
    overlay.add_overlay(&handle);

    let drag = gtk::GestureDrag::new();
    let start = Rc::new(Cell::new(0));
    let last = Rc::new(Cell::new(0));
    // Where the press was, across the window. The drag's own offsets are
    // measured from the handle, which moves with the edge it drags, so each
    // step would count the last one's move again and the edge would shake.
    let pressed_at = Rc::new(Cell::new(0.0f32));
    // The pointer across the window, from a point on the handle as it is now.
    fn across(handle: &gtk::Widget, x: f64, y: f64) -> Option<f32> {
        let root = handle.root()?;
        handle
            .compute_point(&root, &gtk::graphene::Point::new(x as f32, y as f32))
            .map(|p| p.x())
    }
    {
        let (bin, start, last, pressed_at) =
            (bin.clone(), start.clone(), last.clone(), pressed_at.clone());
        drag.connect_drag_begin(move |g, x, y| {
            if let Some(at) = g.widget().and_then(|h| across(&h, x, y)) {
                pressed_at.set(at);
            }
            // Its own, so the heading under it does not take the press for
            // a click that sorts.
            g.set_state(gtk::EventSequenceState::Claimed);
            let asked = bin.asked_width();
            let now = if asked > 0 { asked } else { bin.width() };
            start.set(now);
            last.set(now);
        });
    }
    {
        let (start, last, resize) = (start.clone(), last.clone(), resize.clone());
        drag.connect_drag_update(move |g, dx, dy| {
            let Some((x, y)) = g.start_point() else {
                return;
            };
            let Some(now) = g.widget().and_then(|h| across(&h, x + dx, y + dy)) else {
                return;
            };
            let dx = (now - pressed_at.get()).round() as i32;
            let px = (start.get() + if before_subject { dx } else { -dx }).clamp(MIN, MAX);
            if px != last.get() {
                last.set(px);
                // The list sets the heading's width with the rows', fitted
                // to the pane the same way.
                resize(column, Some(px), false);
            }
        });
    }
    {
        let (start, last, resize) = (start.clone(), last.clone(), resize.clone());
        // A press that never moved (half of a double click) changes nothing.
        drag.connect_drag_end(move |_, _, _| {
            if last.get() != start.get() {
                resize(column, Some(last.get()), true);
            }
        });
    }
    let click = gtk::GestureClick::new();
    {
        let (bin, resize) = (bin.clone(), resize.clone());
        click.connect_pressed(move |g, n, _, _| {
            if n == 2 {
                g.set_state(gtk::EventSequenceState::Claimed);
                bin.set_width(default_width(column));
                resize(column, None, true);
            }
        });
    }
    // One group: the drag claiming the press must not deny the double
    // click on the same handle.
    click.group_with(&drag);
    handle.add_controller(drag);
    handle.add_controller(click);
    overlay.upcast()
}

/// A follow-up flag's due date for the Due column (#334): the day alone,
/// the year too when it is not this one. Empty when there is none.
pub(super) fn due_label(due: i64) -> String {
    if due <= 0 {
        String::new()
    } else if crate::datefmt::year(due) == crate::datefmt::year(crate::datefmt::now()) {
        crate::datefmt::day_month(due)
    } else {
        crate::datefmt::day_month_year(due)
    }
}

/// A pill as a GTK label: a tag in its color class, a Gmail label in the
/// palette color its name picks (both from the app's tag stylesheet).
pub(crate) fn pill_label(pill: &crate::models::Pill) -> gtk::Label {
    match pill {
        crate::models::Pill::Tag(t) => {
            let chip = gtk::Label::new(Some(&t.name));
            chip.add_css_class("tag-chip");
            chip.add_css_class(&t.css_class());
            chip
        }
        crate::models::Pill::Label(name) => {
            let chip = gtk::Label::new(Some(name));
            chip.add_css_class("tag-chip");
            chip.add_css_class("label-chip");
            chip.add_css_class(&format!("label-hue-{}", crate::models::label_hue(name)));
            chip.set_tooltip_text(Some(name));
            chip
        }
    }
}

/// The tag section of a message menu (#71): one entry per tag, its swatch
/// filled where the message carries it; choosing an entry toggles that tag
/// through `toggle(keyword, add)`. `tags` is the whole list in Settings
/// order, so each entry can name its number key.
pub(crate) fn tag_menu_entries(
    tags: &[crate::config::Tag],
    msg: &Message,
    toggle: impl Fn(String, bool) + Clone + 'static,
) -> Vec<MenuEntry> {
    tags.iter()
        .enumerate()
        .map(|(i, t)| {
            let on = msg.has_tag(t);
            let keyword = t.keyword.clone();
            let toggle = toggle.clone();
            MenuEntry::new(t.name.clone(), move || toggle(keyword.clone(), !on))
                .swatch(t.color.clone(), on)
                .keys(crate::ui::keymap::tag_key(i))
        })
        .collect()
}
