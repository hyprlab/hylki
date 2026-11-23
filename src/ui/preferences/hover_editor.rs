//! The hover actions editor (Settings → Message List → Hover Actions): a
//! sample row wearing its strip of buttons, where chips are dropped to show
//! them, and a pool of the actions not shown.

use super::chips::{dashed_zone, wire_drop, zone_column, zone_row, ChipZones};
use super::*;
use crate::config::{HoverAction, HOVER_ACTIONS_MAX};

/// The strip's size for an empty zone's gap: one button.
const BUTTON_PX: i32 = 28;

pub(super) struct HoverEditor {
    zones: ChipZones,
}

impl HoverEditor {
    pub(super) fn build(host: &gtk::Box, sender: &ComponentSender<Preferences>) -> Self {
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        host.append(&list);

        let input = sender.input_sender().clone();
        let on_drop = move |key: String, zone: usize, index: usize| {
            let _ = input.send(PrefInput::HoverDrop {
                key,
                shown: zone == 0,
                index,
            });
            true
        };
        let has_room = |zone: usize, flow: &ChipFlow| zone == 1 || shown(flow) < HOVER_ACTIONS_MAX;

        // The preview: a row as the list draws it under the pointer, its
        // strip the drop zone. The whole row takes the drop, so the chip
        // need not land on the strip itself.
        let column = zone_column(
            &i18n("Preview"),
            &i18n_f(
                "Drop actions on the row · up to {n}",
                &[("n", &HOVER_ACTIONS_MAX.to_string())],
            ),
        );
        let row = sample_row();
        // The strip keeps its own sizes: its chips are buttons, not the
        // pool's labelled chips.
        let strip = ChipFlow::new(
            Rc::new(std::cell::Cell::new((BUTTON_PX, BUTTON_PX))),
            Rc::new(std::cell::Cell::new((0, 0))),
        );
        strip.add_css_class("hover-preview-strip");
        strip.set_halign(gtk::Align::End);
        let empty = gtk::Label::new(Some(&i18n("Drop actions here")));
        empty.add_css_class("dim-label");
        empty.add_css_class("caption");
        empty.set_can_target(false);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        line.add_css_class("row-hover-actions");
        // At the row's end, where the list puts it.
        line.set_halign(gtk::Align::End);
        line.append(&empty);
        line.append(&strip);
        row.append(&line);
        column.append(&row);
        list.append(&zone_row(&column));
        wire_drop(
            row.upcast_ref(),
            &strip,
            strip.upcast_ref(),
            &empty,
            0,
            has_room,
            on_drop.clone(),
        );

        // The pool. A chip's drag writes its size for the pool's gap; the
        // strip's chips leave it alone.
        let drag_size = Rc::new(std::cell::Cell::new((0, 0)));
        let column = zone_column(&i18n("Not shown"), &i18n("Drop an action here to hide it"));
        let pool = ChipFlow::new(Rc::new(std::cell::Cell::new((0, 0))), drag_size.clone());
        let (overlay, frame, pool_empty) = dashed_zone(&pool);
        column.append(&overlay);
        list.append(&zone_row(&column));
        wire_drop(&frame, &pool, &frame, &pool_empty, 1, has_room, on_drop);

        HoverEditor {
            zones: ChipZones {
                zones: vec![(strip, empty), (pool, pool_empty)],
                drag_size,
                icon_only: vec![0],
            },
        }
    }
}

/// How many chips a zone shows, the one being dragged out of it not
/// counted, so moving within a full strip still works.
fn shown(flow: &ChipFlow) -> usize {
    let mut n = 0;
    let mut child = flow.first_child();
    while let Some(c) = child {
        if c.is_visible() {
            n += 1;
        }
        child = c.next_sibling();
    }
    n
}

/// A message row, as the list draws one under the pointer: a circle, the
/// sender and the time, a subject and a line of text.
fn sample_row() -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
    row.add_css_class("hover-preview");
    let main = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let avatar = adw::Avatar::new(38, Some("Ada Lovelace"), true);
    avatar.set_valign(gtk::Align::Center);
    main.append(&avatar);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let name = gtk::Label::new(Some("Ada Lovelace"));
    name.set_xalign(0.0);
    name.set_hexpand(true);
    name.add_css_class("message-sender");
    top.append(&name);
    let date = gtk::Label::new(Some(&crate::datefmt::time(crate::datefmt::now())));
    date.add_css_class("message-date");
    top.append(&date);
    text.append(&top);
    let subject = gtk::Label::new(Some(&i18n("Lunch on Friday?")));
    subject.set_xalign(0.0);
    subject.set_ellipsize(gtk::pango::EllipsizeMode::End);
    subject.add_css_class("message-subject");
    text.append(&subject);
    let preview = gtk::Label::new(Some(&i18n(
        "Are you free at noon? There is a new place by the station.",
    )));
    preview.set_xalign(0.0);
    preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
    preview.add_css_class("message-preview");
    text.append(&preview);
    main.append(&text);
    row.append(&main);
    row
}

impl Preferences {
    /// Refill the editor from the actions being edited.
    pub(super) fn rebuild_hover_chips(&self) {
        let Some(editor) = &self.hover_editor else {
            return;
        };
        let chip = |a: &HoverAction| (a.key(), a.icon(), a.label());
        let shown: Vec<_> = self.hover_action_buttons.iter().map(chip).collect();
        let hidden: Vec<_> = HoverAction::ALL
            .iter()
            .filter(|a| !self.hover_action_buttons.contains(a))
            .map(chip)
            .collect();
        editor.zones.fill(0, &shown);
        editor.zones.fill(1, &hidden);
    }

    pub(super) fn send_hover_actions(&self, sender: &ComponentSender<Self>) {
        let _ = sender.output(PrefOutput::SetHoverActions(
            self.hover_actions,
            self.hover_action_buttons.clone(),
        ));
    }

    pub(super) fn on_hover_drop(
        &mut self,
        key: String,
        shown: bool,
        index: usize,
        sender: ComponentSender<Self>,
    ) {
        let Some(action) = HoverAction::from_key(&key) else {
            return;
        };
        // The slot counts the chips still on screen, the dragged one
        // hidden: the same list with it taken out.
        let mut actions = self.hover_action_buttons.clone();
        actions.retain(|a| *a != action);
        if shown {
            if actions.len() >= HOVER_ACTIONS_MAX {
                return;
            }
            actions.insert(index.min(actions.len()), action);
        }
        self.hover_action_buttons = actions;
        self.rebuild_hover_chips();
        self.send_hover_actions(&sender);
    }

    pub(super) fn on_hover_restore(&mut self, sender: ComponentSender<Self>) {
        let default = HoverAction::DEFAULT.to_vec();
        if self.hover_action_buttons != default {
            self.hover_action_buttons = default;
            self.rebuild_hover_chips();
            self.send_hover_actions(&sender);
        }
    }
}
