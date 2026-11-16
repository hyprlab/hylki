//! A recipient field of the composer (To, Cc, Bcc): every address is a pill,
//! and the one being typed is text after them. Space after a whole address,
//! Return or a comma turns the text into a pill, so the field never ends in
//! a comma. Backspace on an empty line turns the last pill back into text to
//! edit, until the next space, Return or comma.
//!
//! Pills are selected like files: a click, Ctrl+click and Shift+click, or
//! the arrow keys from the start of the text, with Shift to extend, across
//! To, Cc and Bcc; or like text, by sweeping the pointer over them. The selection drags to another place in its field or to
//! another field, and its menu (right click, the menu key) moves it to
//! another field too; Delete removes it (#492).
//!
//! Built from libadwaita's own entry-row markup and class names (`row.entry`,
//! `.editable-area`, `.title`, `.subtitle`, `.edit-icon`, `.focused`), so the
//! row looks, focuses and turns red on an error the way the Subject row under
//! it does.

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use gtk::glib::subclass::Signal;

mod flow;
mod key_badge;
mod parse;
mod pill;
mod selection;
mod wire;

pub(crate) use flow::PillFlow;
pub(crate) use key_badge::KeyBadge;

/// Space between pills, and between lines of them.
const SPACING: i32 = 4;
/// The narrowest the text after the pills gets before it takes a line of
/// its own.
const TEXT_MIN: i32 = 96;

mod imp {
    use super::*;

    #[derive(Default)]
    pub(crate) struct RecipientRow {
        header: gtk::Box,
        pub(super) empty_title: gtk::Label,
        pub(super) title: gtk::Label,
        pub(super) flow: super::PillFlow,
        pub(super) text: gtk::Text,
        pub(super) edit_icon: gtk::Image,
        pub(super) suffixes: gtk::Box,
        /// The pills in order, each with the recipient it stands for as it
        /// goes into the field ("Name <address>").
        pub(super) pills: RefCell<Vec<(gtk::Widget, String)>>,
        /// Set while the row rewrites its own text, so that is not taken
        /// for typing.
        pub(super) busy: Cell<bool>,
        pub(super) focused: Cell<bool>,
        /// Every recipient field of the composer, this one included, in
        /// order: a selection spans them, and a pill's menu moves to them.
        pub(super) fields: RefCell<Vec<glib::WeakRef<super::RecipientRow>>>,
        /// The pill a Shift+click or Shift+arrow selects from.
        pub(super) anchor: RefCell<Option<gtk::Widget>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RecipientRow {
        const NAME: &'static str = "HylkiRecipientRow";
        type Type = super::RecipientRow;
        type ParentType = adw::PreferencesRow;
    }

    impl ObjectImpl for RecipientRow {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("changed").build(),
                    // A pill of this row starts (true) or ends (false) a drag.
                    Signal::builder("pill-drag")
                        .param_types([bool::static_type()])
                        .build(),
                ]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.add_css_class("entry");
            obj.add_css_class("recipient-row");
            obj.set_activatable(false);
            // The text takes the focus, never the row (see `focus`).
            obj.set_focusable(false);

            self.header.add_css_class("header");
            self.header.set_valign(gtk::Align::Center);

            let area = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            area.add_css_class("editable-area");
            area.set_hexpand(true);

            for label in [&self.empty_title, &self.title] {
                label.set_halign(gtk::Align::Start);
                label.set_xalign(0.0);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                label.set_can_target(false);
                label.set_use_markup(false);
                obj.bind_property("title", label, "label")
                    .sync_create()
                    .build();
            }
            self.empty_title.add_css_class("title");
            self.empty_title.add_css_class("dimmed");
            self.empty_title.set_valign(gtk::Align::Center);
            self.title.add_css_class("subtitle");

            self.text.set_hexpand(true);
            self.text.set_valign(gtk::Align::Center);
            self.text.set_enable_undo(true);
            self.text.set_accessible_role(gtk::AccessibleRole::TextBox);
            self.text
                .update_relation(&[gtk::accessible::Relation::LabelledBy(&[self
                    .title
                    .upcast_ref::<gtk::Accessible>(
                )])]);
            self.flow.add_css_class("recipient-flow");
            self.flow.set_text_child(&self.text);

            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            column.set_hexpand(true);
            column.set_valign(gtk::Align::Center);
            column.append(&self.title);
            column.append(&self.flow);
            // The title sits where the text would be until there is some, as
            // in an entry row; it overlays the column so the row keeps its
            // height either way.
            let overlay = gtk::Overlay::new();
            overlay.set_hexpand(true);
            overlay.set_child(Some(&column));
            overlay.add_overlay(&self.empty_title);
            area.append(&overlay);

            self.edit_icon
                .set_icon_name(Some("adw-entry-edit-symbolic"));
            self.edit_icon.add_css_class("edit-icon");
            self.edit_icon.set_valign(gtk::Align::Center);
            self.edit_icon.set_can_target(false);
            self.edit_icon
                .set_accessible_role(gtk::AccessibleRole::Presentation);
            area.append(&self.edit_icon);
            obj.wire_focus(&area);

            self.header.append(&area);
            self.suffixes.add_css_class("suffixes");
            self.suffixes.set_spacing(6);
            self.suffixes.set_visible(false);
            self.header.append(&self.suffixes);
            obj.set_child(Some(&self.header));

            obj.wire();
            obj.update_state();
        }
    }

    impl WidgetImpl for RecipientRow {
        fn grab_focus(&self) -> bool {
            self.text.grab_focus()
        }

        fn focus(&self, direction: gtk::DirectionType) -> bool {
            self.header.child_focus(direction)
        }
    }

    impl ListBoxRowImpl for RecipientRow {}
    impl PreferencesRowImpl for RecipientRow {}
}

glib::wrapper! {
    pub(crate) struct RecipientRow(ObjectSubclass<imp::RecipientRow>)
        @extends adw::PreferencesRow, gtk::ListBoxRow, gtk::Widget,
        @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for RecipientRow {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl RecipientRow {
    /// The whole field as one recipient list: the pills, then whatever is
    /// still being typed, comma separated.
    pub(crate) fn text(&self) -> String {
        let imp = self.imp();
        let mut all: Vec<String> = imp.pills.borrow().iter().map(|(_, r)| r.clone()).collect();
        let typed = self.typed();
        if !typed.is_empty() {
            all.push(typed);
        }
        all.join(", ")
    }

    /// Replace the field with these recipients, every one a pill.
    pub(crate) fn set_text(&self, field: &str) {
        let imp = self.imp();
        for (pill, _) in imp.pills.take() {
            imp.flow.remove(&pill);
        }
        self.set_typed("");
        self.push(field);
        self.emit_changed();
    }

    /// What is being typed after the pills, which suggestions are for.
    pub(crate) fn typed(&self) -> String {
        self.imp().text.text().trim().to_string()
    }

    /// Put a recipient in place of what is being typed (a suggestion
    /// picked from the list).
    pub(crate) fn complete(&self, recipient: &str) {
        self.set_typed("");
        self.push(recipient);
        self.emit_changed();
    }

    /// Add a recipient after the others, leaving what is being typed alone.
    pub(crate) fn add_recipient(&self, recipient: &str) {
        self.push(recipient);
        self.emit_changed();
    }

    pub(crate) fn add_suffix(&self, widget: &impl IsA<gtk::Widget>) {
        let imp = self.imp();
        imp.suffixes.append(widget);
        imp.suffixes.set_visible(true);
    }

    pub(super) fn set_input_purpose(&self, purpose: gtk::InputPurpose) {
        self.imp().text.set_input_purpose(purpose);
    }

    /// Called after every change to the field: typing, a pill made or
    /// taken away.
    pub(crate) fn connect_changed<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local("changed", false, move |values| {
            let row = values[0]
                .get::<Self>()
                .expect("a signal's first value is the row it was emitted on");
            f(&row);
            None
        })
    }

    fn emit_changed(&self) {
        self.update_state();
        self.emit_by_name::<()>("changed", &[]);
    }

    /// Every recipient field of the composer, this one among them, in
    /// order, for a selection to span and a pill's menu to move it to.
    pub(super) fn set_fields(&self, fields: &[&RecipientRow]) {
        *self.imp().fields.borrow_mut() = fields.iter().map(|f| f.downgrade()).collect();
    }

    fn fields(&self) -> Vec<RecipientRow> {
        let fields: Vec<_> = self
            .imp()
            .fields
            .borrow()
            .iter()
            .filter_map(|f| f.upgrade())
            .collect();
        if fields.is_empty() {
            vec![self.clone()]
        } else {
            fields
        }
    }

    /// Called as a pill of this field starts (true) and ends (false) a
    /// drag.
    pub(super) fn connect_pill_drag<F: Fn(&Self, bool) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_local("pill-drag", false, move |values| {
            let row = values[0]
                .get::<Self>()
                .expect("a signal's first value is the row it was emitted on");
            f(&row, values[1].get::<bool>().unwrap_or(false));
            None
        })
    }

    fn focus_text(&self) {
        let text = &self.imp().text;
        text.grab_focus();
        text.set_position(-1);
    }

    /// Set the typed text without it counting as typing.
    fn set_typed(&self, s: &str) {
        let imp = self.imp();
        imp.busy.set(true);
        imp.text.set_text(s.trim_start());
        imp.text.set_position(-1);
        imp.busy.set(false);
    }

    /// The floating title, the focus ring and the pencil, as an entry row
    /// shows them.
    fn update_state(&self) {
        let imp = self.imp();
        let focused = imp.focused.get();
        let empty = imp.pills.borrow().is_empty() && imp.text.text().is_empty();
        let floating = focused || !empty;
        imp.empty_title.set_visible(!floating);
        imp.title.set_opacity(if floating { 1.0 } else { 0.0 });
        imp.edit_icon.set_visible(!focused);
        if focused {
            self.add_css_class("focused");
        } else {
            self.remove_css_class("focused");
        }
    }
}
