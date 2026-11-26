//! Settings → Translation (#327): which service translates messages, the
//! key for it, the language to translate into, and whether the reader
//! offers a translation on its own. Saved as it is changed, like the other
//! settings pages; the key goes to the keyring when it is applied.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::i18n::{i18n, i18n_f};
use crate::services::translate::{self, Service, Settings, LANGUAGES};

pub(super) fn build() -> adw::PreferencesPage {
    let settings = Rc::new(RefCell::new(translate::load()));
    let page = adw::PreferencesPage::new();

    let service_group = adw::PreferencesGroup::new();
    service_group.set_title(&i18n("Translation"));
    service_group.set_description(Some(&i18n(
        "Uses a translation service under your own key. A message’s text is sent only when you translate it.",
    )));

    let names: Vec<String> = Service::ALL.iter().map(|s| s.name()).collect();
    let service_row = adw::ComboRow::new();
    service_row.set_title(&i18n("Service"));
    service_row.set_model(Some(&gtk::StringList::new(
        &names.iter().map(String::as_str).collect::<Vec<_>>(),
    )));
    service_group.add(&service_row);

    let key_row = adw::PasswordEntryRow::new();
    key_row.set_title(&i18n("Key"));
    key_row.set_show_apply_button(true);
    service_group.add(&key_row);

    let url_row = adw::EntryRow::new();
    url_row.set_title(&i18n("Server"));
    url_row.set_text(&settings.borrow().url);
    service_group.add(&url_row);

    let region_row = adw::EntryRow::new();
    region_row.set_title(&i18n("Region (empty for a global resource)"));
    region_row.set_text(&settings.borrow().region);
    service_group.add(&region_row);

    let check_row = adw::ActionRow::new();
    check_row.set_title(&i18n("Check the settings"));
    check_row.set_subtitle(&i18n("Translates a greeting to test the service."));
    check_row.set_subtitle_lines(3);
    let check = gtk::Button::with_label(&i18n("Check"));
    check.set_valign(gtk::Align::Center);
    check_row.add_suffix(&check);
    service_group.add(&check_row);
    page.add(&service_group);

    let reading_group = adw::PreferencesGroup::new();
    reading_group.set_title(&i18n("Messages"));
    let mut targets = vec![i18n_f(
        "App language ({language})",
        &[(
            "language",
            &translate::language_name(&translate::ui_language()),
        )],
    )];
    targets.extend(LANGUAGES.iter().map(|c| translate::language_name(c)));
    let target_row = adw::ComboRow::new();
    target_row.set_title(&i18n("Translate into"));
    target_row.set_model(Some(&gtk::StringList::new(
        &targets.iter().map(String::as_str).collect::<Vec<_>>(),
    )));
    target_row.set_selected(
        LANGUAGES
            .iter()
            .position(|c| *c == settings.borrow().target)
            .map_or(0, |i| i as u32 + 1),
    );
    reading_group.add(&target_row);

    let offer_row = adw::SwitchRow::new();
    offer_row.set_title(&i18n("Offer to translate"));
    offer_row.set_subtitle(&i18n(
        "Messages in another language show a Translate button. The language is detected on this computer.",
    ));
    offer_row.set_active(settings.borrow().offer);
    reading_group.add(&offer_row);
    page.add(&reading_group);

    // Which rows the chosen service needs.
    let shape = {
        let key_row = key_row.clone();
        let url_row = url_row.clone();
        let region_row = region_row.clone();
        let check_row = check_row.clone();
        let reading_group = reading_group.clone();
        move |service: Service| {
            let on = service != Service::Off;
            key_row.set_visible(on);
            key_row.set_title(&if service == Service::Libre {
                i18n("Key (if the server asks for one)")
            } else {
                i18n("Key")
            });
            url_row.set_visible(service == Service::Libre);
            region_row.set_visible(service == Service::Microsoft);
            check_row.set_visible(on);
            reading_group.set_sensitive(on);
        }
    };
    let service_now = settings.borrow().service;
    service_row.set_selected(
        Service::ALL
            .iter()
            .position(|s| *s == service_now)
            .unwrap_or(0) as u32,
    );
    shape(service_now);

    // The key is read from the keyring when the page is first shown, not
    // when the Settings window is built: that is prepared in advance and
    // must not wait on the keyring. It is read off the main thread, as the
    // keyring may wait on its user (#466); one that arrives after another
    // was asked for, or after the user typed one, is dropped.
    let key_loaded = Rc::new(std::cell::Cell::new(false));
    let key_asked = Rc::new(std::cell::Cell::new(0u64));
    let load_key = {
        let key_row = key_row.clone();
        move |service: Service| {
            let asked = key_asked.get() + 1;
            key_asked.set(asked);
            key_row.set_text("");
            let key_row = key_row.clone();
            let key_asked = key_asked.clone();
            crate::config::keyring_then(
                move || translate::load_key(service).unwrap_or_default(),
                move |key| {
                    if key_asked.get() == asked && key_row.text().is_empty() {
                        key_row.set_text(&key);
                    }
                },
            );
        }
    };
    {
        let settings = settings.clone();
        let load_key = load_key.clone();
        let key_loaded = key_loaded.clone();
        page.connect_map(move |_| {
            if !key_loaded.replace(true) {
                load_key(settings.borrow().service);
            }
        });
    }

    let save = {
        let settings = settings.clone();
        move |f: &dyn Fn(&mut Settings)| {
            f(&mut settings.borrow_mut());
            translate::save(&settings.borrow());
        }
    };

    {
        let save = save.clone();
        let shape = shape.clone();
        let load_key = load_key.clone();
        let check_row = check_row.clone();
        service_row.connect_selected_notify(move |row| {
            let service = Service::ALL
                .get(row.selected() as usize)
                .copied()
                .unwrap_or_default();
            save(&|s| s.service = service);
            shape(service);
            load_key(service);
            check_row.set_subtitle(&i18n("Translates a greeting to test the service."));
        });
    }
    {
        let settings = settings.clone();
        key_row.connect_apply(move |row| {
            let (service, key) = (settings.borrow().service, row.text().to_string());
            crate::config::keyring_later(move || translate::store_key(service, &key));
        });
    }
    {
        let save = save.clone();
        url_row.connect_changed(move |row| {
            let url = row.text().trim().to_string();
            save(&|s| s.url = url.clone());
        });
    }
    {
        let save = save.clone();
        region_row.connect_changed(move |row| {
            let region = row.text().trim().to_string();
            save(&|s| s.region = region.clone());
        });
    }
    {
        let save = save.clone();
        target_row.connect_selected_notify(move |row| {
            let target = match row.selected() {
                0 => String::new(),
                n => LANGUAGES
                    .get(n as usize - 1)
                    .map(|c| c.to_string())
                    .unwrap_or_default(),
            };
            save(&|s| s.target = target.clone());
        });
    }
    {
        let save = save.clone();
        offer_row.connect_active_notify(move |row| {
            let on = row.is_active();
            save(&|s| s.offer = on);
        });
    }
    {
        let settings = settings.clone();
        let key_row = key_row.clone();
        let check_row = check_row.clone();
        check.connect_clicked(move |b| {
            let s = settings.borrow().clone();
            let key = key_row.text().to_string();
            // Checking is applying: the key that works is the one kept.
            let (service, kept) = (s.service, key.clone());
            crate::config::keyring_later(move || translate::store_key(service, &kept));
            b.set_sensitive(false);
            check_row.set_subtitle(&i18n("Checking…"));
            let (tx, rx) = std::sync::mpsc::channel::<Result<(String, String), String>>();
            std::thread::spawn(move || {
                let _ = tx.send(translate::check(&s, &key));
            });
            let b = b.clone();
            let check_row = check_row.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
                match rx.try_recv() {
                    Ok(result) => {
                        check_row.set_subtitle(&match result {
                            Ok((sent, back)) => i18n_f(
                                "It works: “{sent}” came back as “{back}”.",
                                &[("sent", &sent), ("back", &back)],
                            ),
                            Err(why) => why,
                        });
                        b.set_sensitive(true);
                        glib::ControlFlow::Break
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Err(_) => {
                        b.set_sensitive(true);
                        glib::ControlFlow::Break
                    }
                }
            });
        });
    }
    page
}
