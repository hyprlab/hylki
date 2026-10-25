//! The chip editors: the reader toolbar's three drop zones and the message
//! list's single-line columns (#334).

use super::*;

/// A drag-and-drop chip editor (Settings → Appearance → Toolbar, and
/// Settings → Message List → Columns): its zones are rows of one raised
/// card, each a wrapping row of draggable chips that slide apart under a
/// drag to show where the drop will land.
struct ChipZones {
    /// Each zone's chips, and the caption shown while it has none.
    zones: Vec<(ChipFlow, gtk::Label)>,
    /// The size of the chip being dragged (zero when none), written by the
    /// chip's drag source and read by every zone for its gap.
    drag_size: Rc<std::cell::Cell<(i32, i32)>>,
}

impl ChipZones {
    /// One zone per `(title, hint)`. `has_room` says whether a zone takes
    /// another chip; `on_drop` is handed the chip's key, the zone and the
    /// slot, and says whether it took it.
    pub(super) fn build(
        host: &gtk::Box,
        heads: &[(String, String)],
        has_room: impl Fn(usize, &ChipFlow) -> bool + Clone + 'static,
        on_drop: impl Fn(String, usize, usize) -> bool + Clone + 'static,
    ) -> Self {
        let mut zones = Vec::new();
        // One chip's size, learnt from whichever zone has chips, so an
        // empty zone opens a gap of the right size too.
        let chip_size = Rc::new(std::cell::Cell::new((0, 0)));
        let drag_size = Rc::new(std::cell::Cell::new((0, 0)));
        // The zones are rows of one raised card, like the rows of the
        // groups around it, not loose elements on the page.
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        host.append(&list);
        for (zone, (title, hint)) in heads.iter().enumerate() {
            let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
            column.set_margin_top(10);
            column.set_margin_bottom(12);
            column.set_margin_start(12);
            column.set_margin_end(12);
            let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let label = gtk::Label::new(Some(title));
            label.add_css_class("heading");
            label.set_halign(gtk::Align::Start);
            heading.append(&label);
            let hint = gtk::Label::new(Some(hint));
            hint.add_css_class("dim-label");
            hint.add_css_class("caption");
            hint.set_halign(gtk::Align::Start);
            hint.set_valign(gtk::Align::Baseline);
            heading.append(&hint);
            column.append(&heading);

            let flow = ChipFlow::new(chip_size.clone(), drag_size.clone());
            flow.set_halign(gtk::Align::Fill);
            flow.set_valign(gtk::Align::Start);
            // The dashed frame.
            let frame: gtk::Widget = flow.clone().upcast();
            frame.add_css_class("toolbar-zone");

            let empty = gtk::Label::new(Some(&i18n("Empty")));
            empty.add_css_class("dim-label");
            empty.set_can_target(false);
            empty.set_halign(gtk::Align::Center);
            empty.set_valign(gtk::Align::Center);
            let overlay = gtk::Overlay::new();
            overlay.set_child(Some(&frame));
            overlay.add_overlay(&empty);
            column.append(&overlay);
            let row = gtk::ListBoxRow::new();
            row.set_activatable(false);
            row.set_selectable(false);
            row.set_can_focus(false);
            row.set_child(Some(&column));
            list.append(&row);

            // The zone takes a chip from any zone (itself included). While
            // the drag moves over it, a gap opens at the slot the pointer
            // is nearest and the other chips slide aside; the drop takes
            // that slot. The dragged chip is hidden in its own zone for
            // the duration, so the hole it left closes the same way.
            let drop = gtk::DropTarget::new(gtk::glib::Type::STRING, gtk::gdk::DragAction::MOVE);
            let fb = flow.clone();
            let room = has_room.clone();
            let take = on_drop.clone();
            drop.connect_drop(move |_, value, x, y| {
                let Ok(key) = value.get::<String>() else {
                    return false;
                };
                if !room(zone, &fb) {
                    fb.set_gap(None);
                    return false;
                }
                let index = fb.insertion_index(x, y);
                fb.set_gap(None);
                take(key, zone, index)
            });
            let fb = flow.clone();
            let fr = frame.clone();
            let room = has_room.clone();
            drop.connect_enter(move |_, x, y| {
                // A full zone takes nothing: no gap, no drop.
                if !room(zone, &fb) {
                    return gtk::gdk::DragAction::empty();
                }
                fr.add_css_class("drop-active");
                fb.set_gap(Some(fb.insertion_index(x, y)));
                gtk::gdk::DragAction::MOVE
            });
            let fb = flow.clone();
            let el = empty.clone();
            let room = has_room.clone();
            drop.connect_motion(move |_, x, y| {
                if !room(zone, &fb) {
                    return gtk::gdk::DragAction::empty();
                }
                el.set_visible(false);
                fb.set_gap(Some(fb.insertion_index(x, y)));
                gtk::gdk::DragAction::MOVE
            });
            let fb = flow.clone();
            let fr = frame.clone();
            let el = empty.clone();
            drop.connect_leave(move |_| {
                fr.remove_css_class("drop-active");
                fb.set_gap(None);
                // The "Empty" caption comes back with the next rebuild if
                // the zone is still empty.
                el.set_visible(fb.first_child().is_none());
            });
            flow.add_controller(drop);

            zones.push((flow, empty));
        }
        ChipZones { zones, drag_size }
    }

    /// Refill zone `zone` with chips: (key, icon, untranslated label).
    fn fill(&self, zone: usize, chips: &[(&'static str, &'static str, &'static str)]) {
        let Some((flow, empty)) = self.zones.get(zone) else {
            return;
        };
        flow.remove_all();
        empty.set_visible(chips.is_empty());
        for (key, icon, label) in chips {
            flow.append(&self.chip(key, icon, label));
        }
    }

    /// One draggable chip: an icon over its name.
    fn chip(&self, key: &'static str, icon: &str, label: &str) -> gtk::Box {
        let chip = gtk::Box::new(gtk::Orientation::Vertical, 4);
        chip.add_css_class("toolbar-chip");
        chip.set_widget_name(key);
        chip.set_tooltip_text(Some(&i18n("Drag to move")));
        let image = gtk::Image::from_icon_name(icon);
        image.set_pixel_size(16);
        chip.append(&image);
        let text = gtk::Label::new(Some(&i18n(label)));
        text.add_css_class("caption");
        chip.append(&text);

        let drag = gtk::DragSource::new();
        drag.set_actions(gtk::gdk::DragAction::MOVE);
        // The chip's likeness is taken while it is still on screen: it is
        // hidden once the drag is under way, and a hidden widget paints
        // nothing.
        let likeness: Rc<std::cell::RefCell<Option<gtk::gdk::Paintable>>> =
            Rc::new(std::cell::RefCell::new(None));
        let c = chip.clone();
        let l = likeness.clone();
        let ds = self.drag_size.clone();
        drag.connect_prepare(move |_, _, _| {
            *l.borrow_mut() = Some(gtk::WidgetPaintable::new(Some(&c)).current_image());
            // The zones open their gap at this chip's own size.
            ds.set((c.width(), c.height()));
            Some(gtk::gdk::ContentProvider::for_value(&key.to_value()))
        });
        let c = chip.clone();
        let l = likeness.clone();
        drag.connect_drag_begin(move |source, _| {
            if let Some(p) = l.borrow().as_ref() {
                source.set_icon(Some(p), c.width() / 2, c.height() / 2);
            }
            // Lifted out of its row; the neighbours close the hole.
            c.set_visible(false);
        });
        let c = chip.clone();
        let ds = self.drag_size.clone();
        drag.connect_drag_end(move |_, _, _| {
            ds.set((0, 0));
            // Dropped nowhere (or somewhere that rebuilt the zones, in
            // which case this widget is already gone): back in place.
            c.set_visible(true);
        });
        chip.add_controller(drag);
        chip
    }
}

/// The reader toolbar editor (Settings → Appearance → Toolbar): one drop
/// zone per side plus a "not shown" pool.
pub(super) struct ToolbarEditor {
    zones: ChipZones,
}

/// The toolbar editor's zones, in order.
const TOOLBAR_ZONES: [Option<ToolbarSide>; 3] =
    [Some(ToolbarSide::Left), Some(ToolbarSide::Right), None];

impl ToolbarEditor {
    pub(super) fn build(host: &gtk::Box, sender: &ComponentSender<Preferences>) -> Self {
        let heads = [
            (
                i18n("Left group"),
                crate::i18n::i18n_f(
                    "Always shown · up to {n}",
                    &[("n", &crate::config::TOOLBAR_LEFT_MAX.to_string())],
                ),
            ),
            (i18n("Right group"), i18n("Folds into ⋯ when narrow")),
            (i18n("Not shown"), i18n("Drop a button here to hide it")),
        ];
        let input = sender.input_sender().clone();
        let zones = ChipZones::build(
            host,
            &heads,
            |zone, flow| zone_has_room(flow, TOOLBAR_ZONES[zone]),
            move |key, zone, index| {
                let _ = input.send(PrefInput::ToolbarDrop {
                    key,
                    side: TOOLBAR_ZONES[zone],
                    index,
                });
                true
            },
        );
        ToolbarEditor { zones }
    }
}

/// The single-line list's column editor (Settings → Message List →
/// Columns, #334): the columns shown, left to right, and the rest.
pub(super) struct ColumnEditor {
    zones: ChipZones,
}

impl ColumnEditor {
    pub(super) fn build(host: &gtk::Box, sender: &ComponentSender<Preferences>) -> Self {
        let heads = [
            (i18n("Shown"), i18n("Left to right")),
            (i18n("Not shown"), i18n("Drop a column here to hide it")),
        ];
        let input = sender.input_sender().clone();
        let zones = ChipZones::build(
            host,
            &heads,
            |_, _| true,
            move |key, zone, index| {
                // The subject is the column that takes the room the others
                // leave, so it stays.
                if zone == 1 && key == crate::config::ListColumn::Subject.key() {
                    return false;
                }
                let _ = input.send(PrefInput::ColumnDrop {
                    key,
                    shown: zone == 0,
                    index,
                });
                true
            },
        );
        ColumnEditor { zones }
    }
}

/// Whether a zone can take the chip being dragged: the hidden zone and the
/// right side always, the left side while fewer than `TOOLBAR_LEFT_MAX`
/// chips are on it. The chip being dragged is hidden in its own zone, so it
/// is not counted there and moving within a full side still works.
fn zone_has_room(flow: &ChipFlow, side: Option<ToolbarSide>) -> bool {
    if side != Some(ToolbarSide::Left) {
        return true;
    }
    let mut shown = 0;
    let mut child = flow.first_child();
    while let Some(c) = child {
        if c.is_visible() {
            shown += 1;
        }
        child = c.next_sibling();
    }
    shown < crate::config::TOOLBAR_LEFT_MAX
}

impl Preferences {
    /// Refill the editor's zones from the layout being edited.
    pub(super) fn rebuild_toolbar_chips(&self) {
        let Some(editor) = &self.toolbar_editor else {
            return;
        };
        for (zone, side) in TOOLBAR_ZONES.iter().enumerate() {
            let items: Vec<ToolbarItem> = match side {
                Some(ToolbarSide::Left) => self.toolbar.left.clone(),
                Some(ToolbarSide::Right) => self.toolbar.right.clone(),
                None => self.toolbar.hidden(),
            };
            let chips: Vec<_> = items
                .iter()
                .map(|i| (i.key(), i.icon(), i.label()))
                .collect();
            editor.zones.fill(zone, &chips);
        }
    }

    /// Refill the column editor's zones from the columns being edited.
    pub(super) fn rebuild_column_chips(&self) {
        use crate::config::ListColumn;
        let Some(editor) = &self.column_editor else {
            return;
        };
        let chip = |c: &ListColumn| (c.key(), c.icon(), c.label());
        let shown: Vec<_> = self.list_columns.iter().map(chip).collect();
        let hidden: Vec<_> = ListColumn::ALL
            .iter()
            .filter(|c| !self.list_columns.contains(c))
            .map(chip)
            .collect();
        editor.zones.fill(0, &shown);
        editor.zones.fill(1, &hidden);
    }
}

impl Preferences {
    pub(super) fn on_toolbar_drop(
        &mut self,
        key: String,
        side: Option<ToolbarSide>,
        index: usize,
        sender: ComponentSender<Self>,
    ) {
        if let Some(item) = ToolbarItem::from_key(&key) {
            self.toolbar.place(item, side, index);
            self.rebuild_toolbar_chips();
            let _ = sender.output(PrefOutput::SetReaderToolbar(self.toolbar.clone()));
        }
    }

    pub(super) fn on_toolbar_gap_preview(&mut self, zone: usize, index: usize) {
        if let Some(editor) = &self.toolbar_editor {
            // A gap the size of the first chip found, as a drag of
            // it would open.
            let zones = &editor.zones;
            if let Some(chip) = zones.zones.iter().find_map(|(flow, _)| flow.first_child()) {
                zones.drag_size.set((chip.width(), chip.height()));
            }
            if let Some((flow, _)) = zones.zones.get(zone) {
                flow.add_css_class("drop-active");
                flow.set_gap(Some(index));
            }
        }
    }

    pub(super) fn on_column_drop(
        &mut self,
        key: String,
        shown: bool,
        index: usize,
        sender: ComponentSender<Self>,
    ) {
        if let Some(column) = crate::config::ListColumn::from_key(&key) {
            // The slot counts the chips still on screen, the dragged
            // one hidden: the same list with it taken out.
            let mut columns = self.list_columns.clone();
            columns.retain(|c| *c != column);
            if shown {
                columns.insert(index.min(columns.len()), column);
            }
            self.list_columns =
                crate::config::ListColumn::from_keys(&crate::config::ListColumn::to_keys(&columns));
            self.rebuild_column_chips();
            let _ = sender.output(PrefOutput::SetListColumns(self.list_columns.clone()));
        }
    }

    pub(super) fn on_column_restore(&mut self, sender: ComponentSender<Self>) {
        let _ = sender.output(PrefOutput::ResetColumnWidths);
        let default = crate::config::ListColumn::DEFAULT.to_vec();
        if self.list_columns != default {
            self.list_columns = default;
            self.rebuild_column_chips();
            let _ = sender.output(PrefOutput::SetListColumns(self.list_columns.clone()));
        }
    }

    pub(super) fn on_toolbar_restore(&mut self, sender: ComponentSender<Self>) {
        if self.toolbar != ReaderToolbar::default() {
            self.toolbar = ReaderToolbar::default();
            self.rebuild_toolbar_chips();
            let _ = sender.output(PrefOutput::SetReaderToolbar(self.toolbar.clone()));
        }
    }
}
