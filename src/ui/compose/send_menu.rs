//! The menu beside Send: send now or later, Mail Merge (#447), a read
//! receipt (#520), S/MIME signing (#478) and the message's importance
//! (#563).

use adw::prelude::*;
use relm4::prelude::*;

use super::messages::ComposeInput;
use super::Compose;
use crate::i18n::{i18n, i18n_f};
use crate::models::Importance;
use crate::ui::later::{next_monday, preset_time};

/// The Send button's popover, set up as the message being written says.
pub(super) fn send_menu(sender: &ComponentSender<Compose>, model: &Compose) -> gtk::Popover {
    let (request_receipt, importance) = (model.request_receipt, model.importance);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    // Menu rows, not buttons: regular weight like every other popover
    // menu (#167).
    list.add_css_class("context-menu-list");
    // What a row sends is made at the click: a preset time counts from then.
    let item = |label: &str, then: Box<dyn Fn() -> Vec<ComposeInput>>| {
        let button = gtk::Button::new();
        button.add_css_class("flat");
        button.add_css_class("context-menu-item");
        button.set_halign(gtk::Align::Fill);
        let text = gtk::Label::new(Some(label));
        text.set_halign(gtk::Align::Start);
        button.set_child(Some(&text));
        let input = sender.input_sender().clone();
        button.connect_clicked(move |b| {
            if let Some(p) = b
                .ancestor(gtk::Popover::static_type())
                .and_downcast::<gtk::Popover>()
            {
                p.popdown();
            }
            for msg in then() {
                input.emit(msg);
            }
        });
        list.append(&button);
    };
    let at = |time: u32| crate::datefmt::clock_label(time);

    item(
        &i18n("Send now"),
        Box::new(|| vec![ComposeInput::ClearSendAt, ComposeInput::Send]),
    );
    list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    item(
        &i18n_f("Tomorrow morning ({time})", &[("time", &at(8))]),
        Box::new(|| vec![ComposeInput::SendAt(preset_time(1, 8))]),
    );
    item(
        &i18n_f("Tomorrow afternoon ({time})", &[("time", &at(13))]),
        Box::new(|| vec![ComposeInput::SendAt(preset_time(1, 13))]),
    );
    item(
        &i18n_f("Monday morning ({time})", &[("time", &at(8))]),
        Box::new(|| vec![ComposeInput::SendAt(next_monday(8))]),
    );
    list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    item(
        &i18n("Pick a date and time…"),
        Box::new(|| vec![ComposeInput::PickSendTime]),
    );
    list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    item(
        &i18n("Mail Merge…"),
        Box::new(|| vec![ComposeInput::MailMerge]),
    );

    list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let receipt = gtk::CheckButton::with_label(&i18n("Request a read receipt"));
    receipt.add_css_class("context-menu-item");
    receipt.set_tooltip_text(Some(&i18n("Not every mail app sends one.")));
    receipt.set_active(request_receipt);
    let input = sender.input_sender().clone();
    receipt.connect_toggled(move |b| input.emit(ComposeInput::ToggleReceipt(b.is_active())));
    list.append(&receipt);
    list.append(&model.smime_check);

    // One of three, as Thunderbird's Priority menu and Outlook's pair of
    // toggles allow: a radio group.
    list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let heading = gtk::Label::new(Some(&i18n("Importance")));
    heading.set_xalign(0.0);
    heading.add_css_class("dim-label");
    heading.add_css_class("caption-heading");
    heading.add_css_class("context-menu-item");
    list.append(&heading);
    let mut first: Option<gtk::CheckButton> = None;
    for (label, level) in [
        (i18n("High"), Importance::High),
        (i18n("Normal"), Importance::Normal),
        (i18n("Low"), Importance::Low),
    ] {
        let radio = gtk::CheckButton::with_label(&label);
        radio.add_css_class("context-menu-item");
        radio.set_group(first.as_ref());
        radio.set_active(level == importance);
        let input = sender.input_sender().clone();
        radio.connect_toggled(move |b| {
            if b.is_active() {
                input.emit(ComposeInput::SetImportance(level));
            }
        });
        list.append(&radio);
        first.get_or_insert(radio);
    }

    let popover = gtk::Popover::new();
    popover.set_child(Some(&list));
    popover
}

/// The Send menu's S/MIME check, which the composer shows, ticks and
/// greys as the From account and the OpenPGP toggles say.
pub(super) fn smime_check(sender: &ComponentSender<Compose>) -> gtk::CheckButton {
    let check = gtk::CheckButton::with_label(&i18n("Sign with S/MIME"));
    check.add_css_class("context-menu-item");
    check.set_visible(false);
    let input = sender.input_sender().clone();
    check.connect_toggled(move |b| input.emit(ComposeInput::ToggleSmime(b.is_active())));
    check
}

impl Compose {
    /// Handles [`ComposeInput::ToggleReceipt`].
    pub(super) fn on_toggle_receipt(&mut self, on: bool) {
        self.request_receipt = on;
        self.fields_dirty = true;
    }

    /// Handles [`ComposeInput::SetImportance`].
    pub(super) fn on_set_importance(&mut self, importance: Importance) {
        self.importance = importance;
        self.fields_dirty = true;
    }
}
