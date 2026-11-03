//! One recipient as a pill: the widget, its keys and menu, and turning it back
//! into text.

use super::parse::normalise_separators;
use super::selection::{acting_on, drag_icon, is_selected, set_selected};
use super::RecipientRow;
use crate::i18n::{i18n, i18n_f};
use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, glib};
use std::cell::Cell;

impl RecipientRow {
    /// Make pills of every recipient in `field`, after the others.
    pub(super) fn push(&self, field: &str) {
        for (name, addr) in crate::worker::parse_recipients(&normalise_separators(field)) {
            if addr.is_empty() {
                continue;
            }
            let recipient = crate::worker::format_recipient(&name, &addr);
            let at = self.imp().pills.borrow().len();
            self.insert_pill(at, &name, &addr, &recipient);
        }
    }

    /// Make a pill of one recipient and put it `at` that place among the
    /// others.
    pub(super) fn insert_pill(
        &self,
        at: usize,
        name: &str,
        addr: &str,
        recipient: &str,
    ) -> gtk::Widget {
        let imp = self.imp();
        let pill = self.pill(name, addr, recipient).upcast::<gtk::Widget>();
        let next = imp.pills.borrow().get(at).map(|(p, _)| p.clone());
        imp.flow.place(&pill, next.as_ref());
        let mut pills = imp.pills.borrow_mut();
        let at = at.min(pills.len());
        pills.insert(at, (pill.clone(), recipient.to_string()));
        pill
    }

    /// A pill for one recipient, with its remove button, pointer and key
    /// handling, menu and drag source.
    fn pill(&self, name: &str, addr: &str, recipient: &str) -> gtk::Box {
        let pill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pill.add_css_class("recipient-pill");
        // Focusable for the keyboard (see `pill_key`), but Tab passes over
        // the pills (see the flow's `focus`).
        pill.set_focusable(true);
        // A mail merge field (#447) is an address once the merge fills it
        // in: it wears the field palette's braces, not the error's red.
        let field = crate::mail::merge::is_field(recipient);
        let valid = field || crate::worker::invalid_recipient(recipient).is_none();
        if !valid {
            pill.add_css_class("invalid");
        }
        let shown = if name.is_empty() { addr } else { name };
        pill.update_property(&[gtk::accessible::Property::Label(shown)]);
        if field {
            let icon = gtk::Image::from_icon_name("hylki-merge-field-symbolic");
            icon.set_pixel_size(12);
            icon.set_margin_end(4);
            pill.append(&icon);
        }
        let label = gtk::Label::new(Some(shown));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(32);
        label.set_single_line_mode(true);
        pill.append(&label);
        pill.set_tooltip_text(Some(
            if field {
                i18n("Filled in by Mail Merge")
            } else if valid {
                addr.to_string()
            } else {
                i18n_f("“{addr}” is not an email address.", &[("addr", addr)])
            }
            .as_str(),
        ));
        self.add_remove_button(&pill, shown);
        self.add_pill_click(&pill);
        self.add_pill_menu_and_keys(&pill);
        self.add_pill_drag(&pill);
        pill
    }

    /// The pill's own close button.
    fn add_remove_button(&self, pill: &gtk::Box, shown: &str) {
        let remove = gtk::Button::from_icon_name("window-close-symbolic");
        remove.add_css_class("flat");
        remove.add_css_class("circular");
        remove.set_focusable(false);
        remove.set_focus_on_click(false);
        remove.set_valign(gtk::Align::Center);
        remove.set_tooltip_text(Some(i18n("Remove").as_str()));
        remove.update_property(&[gtk::accessible::Property::Label(&i18n_f(
            "Remove {name}",
            &[("name", shown)],
        ))]);
        let weak = self.downgrade();
        let weak_pill = pill.downgrade();
        remove.connect_clicked(move |_| {
            let (Some(row), Some(pill)) = (weak.upgrade(), weak_pill.upgrade()) else {
                return;
            };
            row.remove_pill(pill.upcast_ref());
        });
        pill.append(&remove);
    }

    fn add_pill_click(&self, pill: &gtk::Box) {
        // A click selects the pill, Ctrl+click adds it to the selection or
        // takes it out, and Shift+click selects from the last one clicked.
        // A plain click on a pill already selected keeps the others until
        // the button is up, so they can all be dragged. A double click
        // edits the pill: it becomes the text, and what was being typed is
        // kept as a pill of its own first.
        let click = gtk::GestureClick::new();
        let collapse = std::rc::Rc::new(Cell::new(false));
        let weak = self.downgrade();
        let weak_pill = pill.downgrade();
        let c = collapse.clone();
        click.connect_pressed(move |gesture, n, x, y| {
            let (Some(row), Some(pill)) = (weak.upgrade(), weak_pill.upgrade()) else {
                return;
            };
            let pill = pill.upcast::<gtk::Widget>();
            let on_remove = pill
                .pick(x, y, gtk::PickFlags::DEFAULT)
                .is_some_and(|w| w.ancestor(gtk::Button::static_type()).is_some());
            if on_remove {
                return;
            }
            let state = gesture.current_event_state();
            c.set(false);
            if state.contains(gdk::ModifierType::CONTROL_MASK) {
                set_selected(&pill, !is_selected(&pill));
                row.imp().anchor.replace(Some(pill.clone()));
            } else if state.contains(gdk::ModifierType::SHIFT_MASK) {
                row.select_range(&pill);
            } else if n == 2 {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                row.edit_pill(&pill);
                return;
            } else if is_selected(&pill) {
                c.set(true);
            } else {
                row.select_only(&pill);
            }
            pill.grab_focus();
        });
        let weak = self.downgrade();
        let weak_pill = pill.downgrade();
        let c = collapse.clone();
        click.connect_released(move |_, _, _, _| {
            let (Some(row), Some(pill)) = (weak.upgrade(), weak_pill.upgrade()) else {
                return;
            };
            if c.take() {
                row.select_only(pill.upcast_ref());
            }
        });
        // Past the click's distance it is a drag, which takes them all.
        click.connect_stopped(move |_| collapse.set(false));
        pill.add_controller(click);
    }

    /// The pill's context menu (right click) and its keys.
    fn add_pill_menu_and_keys(&self, pill: &gtk::Box) {
        let menu = gtk::GestureClick::new();
        menu.set_button(gdk::BUTTON_SECONDARY);
        let weak = self.downgrade();
        let weak_pill = pill.downgrade();
        menu.connect_pressed(move |gesture, _, x, y| {
            let (Some(row), Some(pill)) = (weak.upgrade(), weak_pill.upgrade()) else {
                return;
            };
            gesture.set_state(gtk::EventSequenceState::Claimed);
            let pill = pill.upcast::<gtk::Widget>();
            if !is_selected(&pill) {
                row.select_only(&pill);
            }
            pill.grab_focus();
            row.pill_menu(&pill, x, y);
        });
        pill.add_controller(menu);

        let keys = gtk::EventControllerKey::new();
        let weak = self.downgrade();
        let weak_pill = pill.downgrade();
        keys.connect_key_pressed(move |_, key, _, state| {
            let (Some(row), Some(pill)) = (weak.upgrade(), weak_pill.upgrade()) else {
                return glib::Propagation::Proceed;
            };
            row.pill_key(pill.upcast_ref(), key, state)
        });
        pill.add_controller(keys);
    }

    fn add_pill_drag(&self, pill: &gtk::Box) {
        // Dragged to another place in this field or into another, with the
        // rest of the selection when it is part of one. The drag carries
        // the pill itself, which only a recipient field takes.
        let drag = gtk::DragSource::new();
        drag.set_actions(gdk::DragAction::MOVE);
        let weak = self.downgrade();
        let weak_pill = pill.downgrade();
        drag.connect_prepare(move |source, x, y| {
            let row = weak.upgrade()?;
            let pill = weak_pill.upgrade()?.upcast::<gtk::Widget>();
            if !is_selected(&pill) {
                row.select_only(&pill);
            }
            // The pill as it looks now, before it dims for the drag.
            let image = gtk::WidgetPaintable::new(Some(&pill)).current_image();
            source.set_icon(
                Some(&drag_icon(&image, acting_on(&pill).len())),
                x as i32,
                y as i32,
            );
            Some(gdk::ContentProvider::for_value(&pill.to_value()))
        });
        let weak = self.downgrade();
        let weak_pill = pill.downgrade();
        drag.connect_drag_begin(move |_, _| {
            if let Some(pill) = weak_pill.upgrade() {
                for (_, p) in acting_on(pill.upcast_ref()) {
                    p.add_css_class("dragged");
                }
            }
            if let Some(row) = weak.upgrade() {
                row.emit_by_name::<()>("pill-drag", &[&true]);
            }
        });
        let weak = self.downgrade();
        drag.connect_drag_end(move |_, _, _| {
            let Some(row) = weak.upgrade() else { return };
            for field in row.fields() {
                for p in field.pill_list() {
                    p.remove_css_class("dragged");
                }
            }
            row.emit_by_name::<()>("pill-drag", &[&false]);
        });
        pill.add_controller(drag);
    }

    /// Keys on a focused pill: the arrows, Home and End select another
    /// (Shift to extend the selection) or go back to the text, Ctrl+A
    /// selects the field's pills, Delete or Backspace removes the
    /// selection, Return edits the pill, the menu key opens its menu, and a
    /// letter goes to the text.
    fn pill_key(
        &self,
        pill: &gtk::Widget,
        key: gdk::Key,
        state: gdk::ModifierType,
    ) -> glib::Propagation {
        let Some(i) = self.index_of(pill) else {
            return glib::Propagation::Proceed;
        };
        let pills = self.pill_list();
        let rtl = self.direction() == gtk::TextDirection::Rtl;
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
        let (back, forward) = if rtl {
            (gdk::Key::Right, gdk::Key::Left)
        } else {
            (gdk::Key::Left, gdk::Key::Right)
        };
        // Select the pill at `j`, alone or from the anchor with Shift.
        let go = |j: usize| {
            let p = &pills[j];
            if shift {
                if self.imp().anchor.borrow().is_none() {
                    self.imp().anchor.replace(Some(pill.clone()));
                }
                self.select_range(p);
            } else {
                self.select_only(p);
            }
            p.grab_focus();
        };
        match key {
            gdk::Key::a | gdk::Key::A if ctrl => self.select_all(),
            _ if ctrl || state.contains(gdk::ModifierType::ALT_MASK) => {
                return glib::Propagation::Proceed
            }
            k if k == back => {
                if i > 0 {
                    go(i - 1);
                }
            }
            k if k == forward => {
                if i + 1 < pills.len() {
                    go(i + 1);
                } else if !shift {
                    self.focus_text();
                }
            }
            gdk::Key::Home => go(0),
            gdk::Key::End if shift => go(pills.len() - 1),
            gdk::Key::End => self.focus_text(),
            gdk::Key::BackSpace | gdk::Key::Delete | gdk::Key::KP_Delete => {
                let gone = acting_on(pill);
                let stays = |p: &&gtk::Widget| !gone.iter().any(|(_, g)| g == *p);
                // Backspace leaves the focus on the pill before, Delete on
                // the one after, as they would on letters.
                let before = pills[..i].iter().rev().find(stays).cloned();
                let after = pills[i + 1..].iter().find(stays).cloned();
                let next = if key == gdk::Key::BackSpace {
                    before.or(after)
                } else {
                    after.or(before)
                };
                for (field, p) in &gone {
                    field.remove_pill(p);
                }
                match next {
                    Some(p) => {
                        self.select_only(&p);
                        p.grab_focus();
                    }
                    None => self.focus_text(),
                }
            }
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter | gdk::Key::F2 => {
                self.edit_pill(pill)
            }
            gdk::Key::Menu => self.pill_menu(pill, pill.width() as f64 / 2.0, pill.height() as f64),
            gdk::Key::F10 if shift => {
                self.pill_menu(pill, pill.width() as f64 / 2.0, pill.height() as f64)
            }
            // Typing goes on in the text, after the pills.
            _ => match key.to_unicode().filter(|c| !c.is_control()) {
                Some(c) => {
                    self.focus_text();
                    let text = &self.imp().text;
                    let mut end = text.text().chars().count() as i32;
                    text.insert_text(&c.to_string(), &mut end);
                    text.set_position(end);
                }
                None => return glib::Propagation::Proceed,
            },
        }
        glib::Propagation::Stop
    }

    /// The menu of `pill` and the rest of the selection, at `(x, y)` in the
    /// pill: move them to another field, edit the pill, remove them.
    fn pill_menu(&self, pill: &gtk::Widget, x: f64, y: f64) {
        use crate::ui::context_menu::{show_context_menu, MenuEntry};
        let Some(p) = pill.compute_point(self, &gtk::graphene::Point::new(x as f32, y as f32))
        else {
            return;
        };
        let acting = acting_on(pill);
        let mut moves = Vec::new();
        for field in self.fields().into_iter().filter(|f| f != self) {
            let label = i18n_f("Move to {name}", &[("name", &field.title())]);
            let acting = acting.clone();
            let which = acting.iter().position(|(_, p)| p == pill).unwrap_or(0);
            moves.push(MenuEntry::new(label, move || {
                field.set_visible(true);
                let at = field.imp().pills.borrow().len();
                let placed = field.take_pills(acting.clone(), at);
                // The menu hands the focus back to the field's text as it
                // closes, which lets go of the selection: once it has, the
                // moved pills are selected again, and the one the menu was
                // opened on takes the focus.
                let Some(focus) = placed.get(which).or(placed.first()).cloned() else {
                    return;
                };
                glib::idle_add_local_once(move || {
                    focus.grab_focus();
                    for p in &placed {
                        set_selected(p, true);
                    }
                });
            }));
        }
        let (row, weak) = (self.downgrade(), pill.downgrade());
        let edit = MenuEntry::new(i18n("Edit"), move || {
            if let (Some(row), Some(pill)) = (row.upgrade(), weak.upgrade()) {
                row.edit_pill(&pill);
            }
        })
        .enabled(acting.len() == 1);
        let row = self.downgrade();
        let remove = MenuEntry::new(i18n("Remove"), move || {
            for (field, p) in &acting {
                field.remove_pill(p);
            }
            if let Some(row) = row.upgrade() {
                row.focus_text();
            }
        });
        show_context_menu(
            self,
            p.x() as f64,
            p.y() as f64,
            vec![moves, vec![edit, remove]],
        );
    }

    fn remove_pill(&self, pill: &gtk::Widget) {
        let imp = self.imp();
        imp.pills.borrow_mut().retain(|(p, _)| p != pill);
        imp.flow.remove(pill);
        self.emit_changed();
    }

    fn edit_pill(&self, pill: &gtk::Widget) {
        let imp = self.imp();
        let typed = self.typed();
        if !typed.is_empty() {
            self.push(&typed);
        }
        let recipient = {
            let mut pills = imp.pills.borrow_mut();
            let Some(i) = pills.iter().position(|(p, _)| p == pill) else {
                return;
            };
            pills.remove(i).1
        };
        imp.flow.remove(pill);
        self.set_typed(&recipient);
        imp.text.grab_focus();
        imp.text.set_position(-1);
        self.emit_changed();
    }
}
