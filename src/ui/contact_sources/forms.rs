//! The widgets the pages are built from: headers, the name and books
//! groups, and the CardDAV and LDAP forms.

use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use crate::contacts::carddav::{self, Account};
use crate::contacts::contact_sources::{Kind, Source};
use crate::contacts::directory::{self, Directory, Security};
use crate::i18n::{i18n, i18n_f};

use super::{mark, AddWhat, ContactSourcesInput, ADD_KINDS};

/// The big mark and name over a source's page, as over a mail account's.
pub(super) fn source_header(source: &Source) -> gtk::Widget {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 6);
    b.set_halign(gtk::Align::Center);
    b.append(&mark(source.mark, 56));
    let detail = gtk::Label::new(Some(&source.detail));
    detail.add_css_class("dim-label");
    detail.set_wrap(true);
    detail.set_justify(gtk::Justification::Center);
    b.append(&detail);
    b.upcast()
}

/// A note with a button to where the thing is changed.
pub(super) fn info_group(
    title: &str,
    text: &str,
    button: &str,
    open: fn(),
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    let row = adw::ActionRow::new();
    row.set_title(&gtk::glib::markup_escape_text(title));
    row.set_subtitle(&gtk::glib::markup_escape_text(text));
    row.set_subtitle_lines(4);
    let b = gtk::Button::with_label(button);
    b.set_valign(gtk::Align::Center);
    b.connect_clicked(move |_| open());
    row.add_suffix(&b);
    group.add(&row);
    group
}

/// A name typed for something whose own name is `own`, as kept: `None`
/// when it is left blank or is the own name again.
fn given_name(typed: &str, own: &str) -> Option<String> {
    let typed = typed.trim();
    (!typed.is_empty() && typed != own).then(|| typed.to_string())
}

/// The name the account goes by in Hylki: here, and under its address
/// books in Contacts. GNOME and Evolution keep theirs.
pub(super) fn name_group(
    source: &Source,
    sender: relm4::Sender<ContactSourcesInput>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    let own = [("own", source.own_title.as_str())];
    group.set_description(Some(&match source.kind {
        Kind::Hylki => i18n_f("The name used here and in Contacts. Left blank, it is {own}.", &own),
        _ => i18n_f(
            "The name used here and in Contacts; GNOME Contacts keeps its own. Left blank, it is {own}.",
            &own,
        ),
    }));
    let row = adw::EntryRow::new();
    row.set_title(&i18n("Name"));
    row.set_text(&source.title);
    row.set_show_apply_button(true);
    let (s, id, own) = (sender, source.id.clone(), source.own_title.clone());
    row.connect_apply(move |r| {
        let _ = s.send(ContactSourcesInput::Rename {
            id: id.clone(),
            name: given_name(&r.text(), &own),
        });
    });
    group.add(&row);
    group
}

/// A source's address books, each with its name in Hylki and its own
/// switch.
pub(super) fn books_group(
    source: &Source,
    sender: relm4::Sender<ContactSourcesInput>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title(&i18n("Address books"));
    group.set_description(Some(&i18n(
        "Rename a book here, or switch it off to leave it out.",
    )));
    for book in &source.books {
        let row = adw::EntryRow::new();
        row.set_title(&gtk::glib::markup_escape_text(&book.name));
        row.set_text(book.shown());
        row.set_show_apply_button(true);
        let (s, uid, own) = (sender.clone(), book.uid.clone(), book.name.clone());
        row.connect_apply(move |r| {
            let _ = s.send(ContactSourcesInput::Rename {
                id: uid.clone(),
                name: given_name(&r.text(), &own),
            });
        });
        let switch = gtk::Switch::new();
        switch.set_valign(gtk::Align::Center);
        switch.set_active(book.on);
        switch.set_tooltip_text(Some(&i18n("Use this address book")));
        // The account's own switch decides first.
        switch.set_sensitive(source.on);
        let (s, uid) = (sender.clone(), book.uid.clone());
        switch.connect_active_notify(move |w| {
            let _ = s.send(ContactSourcesInput::ToggleBook {
                book: uid.clone(),
                on: w.is_active(),
            });
        });
        row.add_suffix(&switch);
        group.add(&row);
    }
    group
}

/// The add page's dropdown of what can be added, with its mark.
pub(super) fn kind_picker(
    selected: u32,
    sender: relm4::Sender<ContactSourcesInput>,
) -> gtk::Widget {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 18);
    let what = ADD_KINDS.get(selected as usize).unwrap_or(&ADD_KINDS[0]);
    let m = mark(what.mark, 56);
    m.set_halign(gtk::Align::Center);
    b.append(&m);
    let group = adw::PreferencesGroup::new();
    let row = adw::ComboRow::new();
    row.set_title(&i18n("Address book"));
    let labels: Vec<String> = ADD_KINDS.iter().map(|k| i18n(k.label)).collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    row.set_model(Some(&gtk::StringList::new(&refs)));
    row.set_selected(selected);
    row.connect_selected_notify(move |r| {
        let _ = sender.send(ContactSourcesInput::AddKind(r.selected()));
    });
    group.add(&row);
    b.append(&group);
    b.upcast()
}

/// The CardDAV form: a new account for the add page's `kind`, or the
/// account itself; and what Save does with it.
pub(super) fn carddav_form(
    account: Account,
    kind: Option<u32>,
    what: AddWhat,
    sender: relm4::Sender<ContactSourcesInput>,
) -> (gtk::Widget, Rc<dyn Fn() -> bool>) {
    let (preset, hint) = match what {
        AddWhat::CardDav { server, hint } => (server.filter(|_| kind.is_some()), hint),
        _ => (None, ""),
    };
    let new = account.uid.is_empty();
    let server = adw::PreferencesGroup::new();
    server.set_title(&i18n("Server"));
    if !hint.is_empty() && new {
        server.set_description(Some(&i18n(hint)));
    }
    let name = adw::EntryRow::new();
    name.set_title(&i18n("Name (optional)"));
    name.set_text(&account.name);
    let url = adw::EntryRow::new();
    url.set_title(&i18n("Server"));
    url.set_text(&account.url);
    url.set_input_purpose(gtk::InputPurpose::Url);
    // A known service's server is not the user's to type.
    url.set_visible(preset.is_none());
    server.add(&name);
    server.add(&url);
    // The same account's calendars (#715), which EDS finds on the server.
    let calendars = adw::SwitchRow::new();
    calendars.set_title(&i18n("Calendars"));
    calendars.set_subtitle(&i18n("Its calendars show in Calendar."));
    calendars.set_active(account.calendars);
    server.add(&calendars);

    let signin = adw::PreferencesGroup::new();
    signin.set_title(&i18n("Sign-in"));
    let user = adw::EntryRow::new();
    user.set_title(&i18n("Username"));
    user.set_text(&account.user);
    let pass = adw::PasswordEntryRow::new();
    pass.set_title(&i18n("Password"));
    // Asked off the main thread: the keyring may wait on its user (#466).
    if !new {
        let (a, pass) = (account.clone(), pass.clone());
        crate::config::keyring_then(
            move || carddav::has_password(&a),
            move |saved| {
                if saved {
                    pass.set_title(&i18n("Password (leave empty to keep)"));
                }
            },
        );
    }
    signin.add(&user);
    signin.add(&pass);
    // Return in the last field saves, as the button does.
    {
        let sender = sender.clone();
        pass.connect_entry_activated(move |_| {
            let _ = sender.send(ContactSourcesInput::SaveClicked);
        });
    }
    // A new account starts at the first field it still needs.
    if new {
        let (url, user, pass) = (url.clone(), user.clone(), pass.clone());
        gtk::glib::idle_add_local_once(move || {
            if url.is_visible() && url.text().is_empty() {
                url.grab_focus();
            } else if user.text().is_empty() {
                user.grab_focus();
            } else {
                pass.grab_focus();
            }
        });
    }

    let error = gtk::Label::new(None);
    error.add_css_class("error");
    error.set_wrap(true);
    error.set_xalign(0.0);
    error.set_visible(false);

    let form = gtk::Box::new(gtk::Orientation::Vertical, 24);
    form.append(&server);
    form.append(&signin);
    form.append(&error);

    let save: Rc<dyn Fn() -> bool> = Rc::new(move || {
        let fail = |text: &str, row: &adw::EntryRow| {
            error.set_label(text);
            error.set_visible(true);
            row.grab_focus();
            false
        };
        let address = preset
            .map(str::to_string)
            .unwrap_or_else(|| url.text().trim().to_string());
        if let Err(e) = carddav::parse_url(&address) {
            return fail(&e, &url);
        }
        if user.text().trim().is_empty() {
            return fail(&i18n("Enter the user name."), &user);
        }
        if new && pass.text().is_empty() {
            return fail(&i18n("Enter the password."), pass.upcast_ref());
        }
        let mut a = account.clone();
        a.name = name.text().trim().to_string();
        a.url = address;
        a.user = user.text().trim().to_string();
        a.calendars = calendars.is_active();
        let _ = sender.send(ContactSourcesInput::SaveCardDav {
            account: a,
            password: pass.text().to_string(),
        });
        true
    });
    (form.upcast(), save)
}

const SECURITY: [Security; 3] = [Security::StartTls, Security::Ldaps, Security::None];

/// The LDAP directory form, and what Save does with it.
pub(super) fn ldap_form(
    dir: Directory,
    sender: relm4::Sender<ContactSourcesInput>,
) -> (gtk::Widget, Rc<dyn Fn() -> bool>) {
    let server = adw::PreferencesGroup::new();
    server.set_title(&i18n("Server"));
    server.set_description(Some(&i18n(
        "Recipients are looked up here as you type, and Contacts searches it too.",
    )));
    let name = adw::EntryRow::new();
    name.set_title(&i18n("Name (optional)"));
    name.set_text(&dir.name);
    let host = adw::EntryRow::new();
    host.set_title(&i18n("Server"));
    host.set_text(&dir.host);
    host.set_input_purpose(gtk::InputPurpose::Url);
    let security = adw::ComboRow::new();
    security.set_title(&i18n("Encryption"));
    security.set_model(Some(&gtk::StringList::new(&[
        &i18n("StartTLS"),
        &i18n("TLS (LDAPS)"),
        &i18n("None"),
    ])));
    security.set_selected(
        SECURITY
            .iter()
            .position(|s| *s == dir.security)
            .unwrap_or(0) as u32,
    );
    let port = adw::SpinRow::with_range(1.0, 65535.0, 1.0);
    port.set_title(&i18n("Port"));
    port.set_value(f64::from(dir.port));
    // Follow the encryption with the port, unless it was set to something
    // other than the usual one.
    {
        let port = port.clone();
        security.connect_selected_notify(move |row| {
            let chosen = SECURITY
                .get(row.selected() as usize)
                .copied()
                .unwrap_or(Security::StartTls);
            if SECURITY
                .iter()
                .any(|s| f64::from(s.default_port()) == port.value())
            {
                port.set_value(f64::from(chosen.default_port()));
            }
        });
    }
    let base = adw::EntryRow::new();
    base.set_title(&i18n("Search base, such as dc=example,dc=com"));
    base.set_text(&dir.base_dn);
    let scope = adw::ComboRow::new();
    scope.set_title(&i18n("Search"));
    scope.set_model(Some(&gtk::StringList::new(&[
        &i18n("Whole tree"),
        &i18n("First level"),
    ])));
    scope.set_selected(if dir.subtree { 0 } else { 1 });
    for w in [
        name.upcast_ref::<gtk::Widget>(),
        host.upcast_ref(),
        security.upcast_ref(),
        port.upcast_ref(),
        base.upcast_ref(),
        scope.upcast_ref(),
    ] {
        server.add(w);
    }

    let signin = adw::PreferencesGroup::new();
    signin.set_title(&i18n("Sign-in"));
    signin.set_description(Some(&i18n(
        "Leave empty for a directory that needs no sign-in.",
    )));
    let bind = adw::EntryRow::new();
    bind.set_title(&i18n(
        "Sign in as, such as cn=jane,ou=people,dc=example,dc=com",
    ));
    bind.set_text(&dir.bind_dn);
    let pass = adw::PasswordEntryRow::new();
    pass.set_title(&i18n("Password"));
    if !dir.uid.is_empty() {
        let (dir, pass) = (dir.clone(), pass.clone());
        crate::config::keyring_then(
            move || directory::has_password(&dir),
            move |saved| {
                if saved {
                    pass.set_title(&i18n("Password (leave empty to keep)"));
                }
            },
        );
    }
    signin.add(&bind);
    signin.add(&pass);

    let error = gtk::Label::new(None);
    error.add_css_class("error");
    error.set_wrap(true);
    error.set_xalign(0.0);
    error.set_visible(false);

    let form = gtk::Box::new(gtk::Orientation::Vertical, 24);
    form.append(&server);
    form.append(&signin);
    form.append(&error);

    let save: Rc<dyn Fn() -> bool> = Rc::new(move || {
        // A server pasted as a URL names its encryption and port too.
        let (host_text, typed_security, typed_port) = directory::parse_server(&host.text());
        if host_text.is_empty() {
            error.set_label(&i18n("Enter the directory’s server."));
            error.set_visible(true);
            host.grab_focus();
            return false;
        }
        let mut d = dir.clone();
        d.name = name.text().trim().to_string();
        d.host = host_text;
        let chosen = SECURITY
            .get(security.selected() as usize)
            .copied()
            .unwrap_or(Security::StartTls);
        d.security = typed_security.unwrap_or(chosen);
        d.port = match typed_port {
            Some(p) => p,
            // The usual port of the encryption picked, when the URL changed it.
            None if SECURITY
                .iter()
                .any(|s| f64::from(s.default_port()) == port.value()) =>
            {
                d.security.default_port()
            }
            None => port.value() as u16,
        };
        d.base_dn = base.text().trim().to_string();
        d.subtree = scope.selected() == 0;
        d.bind_dn = bind.text().trim().to_string();
        let _ = sender.send(ContactSourcesInput::SaveLdap {
            dir: d,
            password: pass.text().to_string(),
        });
        true
    });
    (form.upcast(), save)
}
