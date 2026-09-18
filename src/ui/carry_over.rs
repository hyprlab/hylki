//! The one-time notice after an earlier install's data was carried across
//! (see `legacy`): what came over, that the old app is untouched, and how
//! to remove it once everything checks out.

use adw::prelude::*;

use crate::i18n::{i18n, i18n_f};
use crate::legacy::Predecessor;

pub fn show(parent: &impl IsA<gtk::Window>, from: &'static Predecessor) {
    let heading = i18n_f("Welcome to Hylki, formerly {old}", &[("old", from.name)]);
    let mut body = i18n_f(
        "Your accounts, settings and cached mail were carried over from {old}. \
         There is nothing to set up again, and nothing in {old} was changed or \
         removed. Once everything is here, you can remove {old}.",
        &[("old", from.name)],
    );
    if crate::legacy::default_mailer().is_some_and(|p| std::ptr::eq(p, from)) {
        body.push_str("\n\n");
        body.push_str(&i18n_f(
            "{old} is still the app your desktop opens email links with. To hand \
             that to Hylki, choose it under Default Apps in Settings.",
            &[("old", from.name)],
        ));
    }
    let dialog = adw::MessageDialog::new(Some(parent.as_ref()), Some(&heading), Some(&body));
    dialog.set_size_request(480, -1);

    if crate::platform::is_flatpak() {
        let extra = gtk::Box::new(gtk::Orientation::Vertical, 6);
        extra.set_margin_top(6);
        let caption = gtk::Label::new(Some(i18n_f("To remove {old}, run:", &[("old", from.name)]).as_str()));
        caption.set_xalign(0.0);
        caption.add_css_class("dim-label");
        caption.add_css_class("caption");
        extra.append(&caption);
        extra.append(&command_row(&format!("flatpak uninstall {}", from.app_id)));
        dialog.set_extra_child(Some(&extra));
    }

    dialog.add_response("ok", &i18n("Got it"));
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("ok");
    dialog.present();
}

/// A command in a monospace field with a Copy button.
fn command_row(cmd: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let text = gtk::Label::builder()
        .label(cmd)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .xalign(0.0)
        .hexpand(true)
        .margin_start(10)
        .margin_end(10)
        .margin_top(8)
        .margin_bottom(8)
        .build();
    text.add_css_class("monospace");
    let frame = gtk::Frame::new(None);
    frame.add_css_class("view");
    frame.set_child(Some(&text));
    frame.set_hexpand(true);
    row.append(&frame);
    let copy = gtk::Button::from_icon_name("co.hyprlab.Hylki-edit-copy-symbolic");
    copy.set_tooltip_text(Some(i18n("Copy").as_str()));
    copy.set_valign(gtk::Align::Center);
    {
        let cmd = cmd.to_string();
        copy.connect_clicked(move |b| {
            b.clipboard().set_text(&cmd);
            b.set_icon_name("co.hyprlab.Hylki-verified-checkmark-symbolic");
            let b = b.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
                b.set_icon_name("co.hyprlab.Hylki-edit-copy-symbolic");
            });
        });
    }
    row.append(&copy);
    row
}
