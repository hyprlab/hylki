//! Composing → Grammar (#452): which checker reads what is written, and
//! getting it. Harper and LanguageTool are each one Download button away;
//! a LanguageTool server takes an address, and for Premium a username and
//! key. Saved to `grammar.toml` as it is changed, as the Translation page
//! saves its own; the Premium key goes to the keyring when it is applied.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::i18n::{i18n, i18n_f};
use crate::services::grammar::{self, Engine, Settings};

mod get;

/// The engines in the order the Checker row lists them.
const ENGINES: [Engine; 3] = [Engine::Harper, Engine::Local, Engine::Server];

type Save = Rc<dyn Fn(&dyn Fn(&mut Settings))>;

pub(in crate::ui::preferences) fn group() -> adw::PreferencesGroup {
    let settings = Rc::new(RefCell::new(grammar::load()));
    // Someone who has not chosen is offered the checker for the languages
    // they write in; it is saved with the first thing they change.
    if settings.borrow().engine.is_none() && settings.borrow().url.trim().is_empty() {
        let langs = spelling_languages();
        settings.borrow_mut().engine = Some(grammar::suggested_engine(&langs));
    }
    let group = adw::PreferencesGroup::new();
    group.set_title(&i18n("Grammar"));

    let check_row = adw::SwitchRow::new();
    check_row.set_title(&i18n("Check grammar and style"));
    check_row.set_subtitle(&i18n("Right-click a marked phrase for suggestions."));
    check_row.set_active(settings.borrow().enabled);
    group.add(&check_row);

    let engine_row = adw::ComboRow::new();
    engine_row.set_title(&i18n("Checker"));
    engine_row.set_model(Some(&gtk::StringList::new(&[
        &i18n("Harper"),
        &i18n("LanguageTool"),
        &i18n("LanguageTool server"),
    ])));
    let at = ENGINES
        .iter()
        .position(|e| *e == settings.borrow().engine())
        .unwrap_or(0);
    engine_row.set_selected(at as u32);
    group.add(&engine_row);

    let save: Save = {
        let settings = settings.clone();
        Rc::new(move |f: &dyn Fn(&mut Settings)| {
            f(&mut settings.borrow_mut());
            grammar::save(&settings.borrow());
        })
    };

    let get_rows = [
        get::Get::new(Engine::Harper, save.clone()),
        get::Get::new(Engine::Local, save.clone()),
    ];
    for row in &get_rows {
        group.add(row.widget());
    }

    let server = server_rows(&settings, save.clone());
    for row in &server.rows {
        group.add(row);
    }

    let shape: Rc<dyn Fn()> = {
        let (engine_row, get_rows, check_row) =
            (engine_row.clone(), get_rows.clone(), check_row.clone());
        let server_rows = server.rows.clone();
        Rc::new(move || {
            let on = check_row.is_active();
            let engine = ENGINES[(engine_row.selected() as usize).min(2)];
            engine_row.set_subtitle(&match engine {
                Engine::Harper => i18n("English only. Nothing leaves this computer."),
                Engine::Local => {
                    i18n("Multi-language support. Nothing leaves this computer. Uses 1GB of RAM")
                }
                Engine::Server => i18n("What you write is sent to the server as you type."),
            });
            // One download at a time, and it is for the checker shown.
            engine_row.set_sensitive(on && !get_rows.iter().any(get::Get::busy));
            for row in &get_rows {
                row.show(engine, on);
            }
            for r in &server_rows {
                r.set_visible(engine == Engine::Server);
                r.set_sensitive(on);
            }
        })
    };
    for row in &get_rows {
        row.on_change(shape.clone());
    }
    shape();

    {
        let (save, shape) = (save.clone(), shape.clone());
        check_row.connect_active_notify(move |row| {
            let on = row.is_active();
            save(&|s| s.enabled = on);
            shape();
        });
    }
    {
        let (save, shape) = (save.clone(), shape.clone());
        engine_row.connect_selected_notify(move |row| {
            let engine = ENGINES[(row.selected() as usize).min(2)];
            save(&|s| s.engine = Some(engine));
            // LanguageTool's memory is let go as soon as it is not wanted.
            if engine != Engine::Local {
                grammar::local::stop();
            }
            shape();
        });
    }
    {
        // The key is only read for an account that has one, the first time
        // the page is shown: the Settings window is built in advance and
        // must not wait on the keyring, which may wait on its user (#466).
        let loaded = std::cell::Cell::new(false);
        let (settings, key_row) = (settings.clone(), server.key_row.clone());
        group.connect_map(move |_| {
            if loaded.replace(true) || settings.borrow().username.trim().is_empty() {
                return;
            }
            let key_row = key_row.clone();
            crate::config::keyring_then(grammar::load_key, move |key| {
                if key_row.text().is_empty() {
                    key_row.set_text(&key);
                }
            });
        });
    }
    group
}

/// The languages chosen for spelling, or the system's.
fn spelling_languages() -> Vec<String> {
    let chosen =
        crate::desktop::spell::parse_languages(&crate::config::load_privacy().spellcheck_langs);
    if chosen.is_empty() {
        crate::desktop::spell::system_spell_languages()
    } else {
        chosen
    }
}

struct ServerRows {
    rows: Vec<gtk::Widget>,
    key_row: adw::PasswordEntryRow,
}

/// A LanguageTool server's address, the Premium account, and a check that
/// they work.
fn server_rows(settings: &Rc<RefCell<Settings>>, save: Save) -> ServerRows {
    let url_row = adw::EntryRow::new();
    url_row.set_title(&i18n("LanguageTool server"));
    url_row.set_text(&settings.borrow().url);

    let user_row = adw::EntryRow::new();
    user_row.set_title(&i18n("Username (LanguageTool Premium)"));
    user_row.set_text(&settings.borrow().username);

    let key_row = adw::PasswordEntryRow::new();
    key_row.set_title(&i18n("API key (LanguageTool Premium)"));
    key_row.set_show_apply_button(true);

    let test_row = adw::ActionRow::new();
    test_row.set_title(&i18n("Check the settings"));
    test_row.set_subtitle(&i18n("Sends a test sentence to the server."));
    test_row.set_subtitle_lines(3);
    let test = gtk::Button::with_label(&i18n("Check"));
    test.set_valign(gtk::Align::Center);
    test_row.add_suffix(&test);

    {
        let save = save.clone();
        url_row.connect_changed(move |row| {
            let url = row.text().trim().to_string();
            save(&|s| s.url = url.clone());
        });
    }
    user_row.connect_changed(move |row| {
        let user = row.text().trim().to_string();
        save(&|s| s.username = user.clone());
    });
    key_row.connect_apply(move |row| {
        let key = row.text().to_string();
        crate::config::keyring_later(move || grammar::store_key(&key));
    });
    {
        let (settings, key_row, test_row) = (settings.clone(), key_row.clone(), test_row.clone());
        test.connect_clicked(move |b| {
            let s = settings.borrow().clone();
            let key = key_row.text().to_string();
            // Checking is applying: the key that works is the one kept.
            {
                let key = key.clone();
                crate::config::keyring_later(move || grammar::store_key(&key));
            }
            b.set_sensitive(false);
            test_row.set_subtitle(&i18n("Checking…"));
            let (b, test_row) = (b.clone(), test_row.clone());
            glib::spawn_future_local(async move {
                let result =
                    gtk::gio::spawn_blocking(move || grammar::check_server(&s, key.trim()))
                        .await
                        .unwrap_or_else(|_| Err(i18n("The check stopped unexpectedly.")));
                test_row.set_subtitle(&match result {
                    Ok(server) => i18n_f("It works: {server} answered.", &[("server", &server)]),
                    Err(why) => why,
                });
                b.set_sensitive(true);
            });
        });
    }
    ServerRows {
        rows: vec![
            url_row.upcast(),
            user_row.upcast(),
            key_row.clone().upcast(),
            test_row.upcast(),
        ],
        key_row,
    }
}
