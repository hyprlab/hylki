//! Settings that act on data: importing and exporting settings, logs and
//! local contacts.

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::ui::notifications::NotifyInput;
use crate::ui::preferences::PrefOutput;

use super::contacts::{reload_contacts, sync_contact_books};
use super::demo::demo_mode;
use super::model::AppModel;
use super::msg::AppMsg;
use crate::ui::dialog::confirm_destructive;

impl AppModel {
    /// Asks where to save, writes the file there with `write` and says in
    /// the status bar how it went: `saved` names the place, a failure is an
    /// error card.
    fn export_to_chosen_file(
        &self,
        title: &str,
        initial_name: &str,
        write: impl FnOnce(&std::path::Path) -> Result<(), String> + 'static,
        saved: impl FnOnce(&std::path::Path) -> String + 'static,
    ) {
        let dialog = gtk::FileDialog::builder()
            .title(title)
            .initial_name(initial_name)
            .build();
        let win = self.window.clone();
        let notif = self.notifications.sender().clone();
        dialog.save(Some(&win), gtk::gio::Cancellable::NONE, move |res| {
            let Ok(file) = res else { return };
            let Some(path) = file.path() else { return };
            let outcome = write(&path);
            let _ = notif.send(match outcome {
                Ok(()) => NotifyInput::Push {
                    text: saved(&path),
                    error: false,
                    connectivity: false,
                },
                Err(e) => NotifyInput::Push {
                    text: i18n_f("Export failed: {e}", &[("e", &(e).to_string())]),
                    error: true,
                    connectivity: false,
                },
            });
        });
    }

    /// The settings changes the router sends here (see `AppModel::route`).
    pub(super) fn on_prefs_data_pref(&mut self, pref: PrefOutput, sender: ComponentSender<Self>) {
        match pref {
            PrefOutput::SetContactBook(uid) => {
                if pref!(self.contact_book = uid) && self.showing_contacts {
                    reload_contacts(&sender, 0);
                }
            }
            PrefOutput::LoadContactBooks => {
                if demo_mode() {
                    sender.input(AppMsg::ContactBooksLoaded {
                        books: crate::contacts::demo_books(),
                        local_count: 1,
                    });
                    return;
                }
                let s = sender.clone();
                std::thread::spawn(move || {
                    s.input(AppMsg::ContactBooksLoaded {
                        books: crate::contacts::writable_books(),
                        local_count: crate::contacts::local::count(),
                    });
                });
            }
            PrefOutput::ImportLocalContacts => {
                let s = sender.clone();
                crate::ui::contacts_page::choose_vcf_files(
                    self.dialog_parent().as_ref(),
                    move |paths| {
                        s.input(AppMsg::ImportContacts(paths));
                    },
                );
            }
            PrefOutput::ExportLocalContacts => {
                let dialog = gtk::FileDialog::builder()
                    .title(i18n("Export Contacts"))
                    .initial_name("hylki-contacts.vcf")
                    .build();
                let notif = self.notifications.sender().clone();
                dialog.save(
                    self.dialog_parent().as_ref(),
                    gtk::gio::Cancellable::NONE,
                    move |res| {
                        let Ok(file) = res else { return };
                        let Some(path) = file.path() else { return };
                        let notif = notif.clone();
                        std::thread::spawn(move || {
                            let _ = notif.send(match crate::contacts::local::export_to(&path) {
                                Ok(n) => NotifyInput::SetStatus(ni18n_f(
                                    "{n} contact exported to {path}",
                                    "{n} contacts exported to {path}",
                                    n as u32,
                                    &[("n", &n.to_string()), ("path", &path.display().to_string())],
                                )),
                                Err(e) => NotifyInput::Push {
                                    text: i18n_f("Could not export contacts: {e}", &[("e", &e)]),
                                    error: true,
                                    connectivity: false,
                                },
                            });
                        });
                    },
                );
            }
            PrefOutput::DeleteAllLocalContacts => {
                let n = crate::contacts::local::count();
                let s = sender.clone();
                confirm_destructive(
                    self.dialog_parent().as_ref(),
                    &i18n("Delete All Contacts?"),
                    &ni18n_f(
                        "The {n} contact in the Hylki address book is deleted. This cannot be undone; export the book first to keep a copy.",
                        "The {n} contacts in the Hylki address book are deleted. This cannot be undone; export the book first to keep a copy.",
                        n as u32,
                        &[("n", &n.to_string())],
                    ),
                    "delete",
                    &i18n("Delete All"),
                    move || {
                        std::thread::spawn(move || {
                            let err = crate::contacts::local::delete_all().err();
                            crate::contacts::local_changed();
                            s.input(AppMsg::ContactWriteDone(err));
                        });
                    },
                );
            }

            PrefOutput::ExportSettings => self.export_to_chosen_file(
                &i18n("Export Settings"),
                "hylki-settings.toml",
                |path| {
                    // It carries every account's server and address: no
                    // more readable than the files it was made from.
                    crate::config::export_bundle().and_then(|t| {
                        crate::config::write_private(path, &t).map_err(|e| e.to_string())
                    })
                },
                |path| {
                    i18n_f(
                        "Settings exported to {display}",
                        &[("display", &(path.display()).to_string())],
                    )
                },
            ),

            PrefOutput::ExportLog => {
                let title = i18n("Export Log");
                let name = format!(
                    "hylki-log-{}.txt",
                    chrono::Local::now().format("%Y%m%d-%H%M")
                );
                // Measured now, while the model is at hand: the chooser's
                // callback runs later, without it.
                let memory = self.memory_report();
                self.export_to_chosen_file(
                    &title,
                    &name,
                    move |path| {
                        // Redaction shortens addresses only: names, subjects
                        // and hosts are still in it.
                        crate::config::write_private(
                            path,
                            &crate::desktop::console_log::export_text(&memory),
                        )
                        .map_err(|e| e.to_string())
                    },
                    |path| {
                        i18n_f(
                            "Log exported to {display}",
                            &[("display", &(path.display()).to_string())],
                        )
                    },
                );
            }

            PrefOutput::ImportSettings => {
                let dialog = gtk::FileDialog::builder()
                    .title(i18n("Import Settings"))
                    .build();
                let win = self.window.clone();
                let s = sender.clone();
                dialog.open(Some(&win), gtk::gio::Cancellable::NONE, move |res| {
                    // Only carry the choice out of the chooser's callback —
                    // the work (and any dialog) runs on a clean loop turn.
                    if let Ok(file) = res {
                        if let Some(path) = file.path() {
                            s.input(AppMsg::ImportSettingsFrom(path));
                        }
                    }
                });
            }

            PrefOutput::ContactSourcesChanged => {
                // A book switched on or off, or renamed, counts at once
                // everywhere, the sidebar's Contacts list among them.
                reload_contacts(&sender, 0);
                if self.filters.iter().any(|r| r.needs_contacts()) {
                    self.saved_addresses.refresh();
                }
                // Settings synced a saved account's books already; any it
                // did not are opened and synced now.
                sync_contact_books(&sender, false, 0);
            }
            _ => unreachable!("not routed to on_prefs_data_pref"),
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_prefs_data(&self, msg: AppMsg) {
        match msg {
            AppMsg::ImportSettingsFrom(path) => self.import_settings_from(path),
            _ => unreachable!("not routed to on_prefs_data"),
        }
    }

    /// Second stage of ImportSettings: the chosen file, applied on a clean
    /// main-loop turn (working inside the chooser's completion callback froze
    /// the app when the confirmation dialog presented there).
    #[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
    fn import_settings_from(&self, path: std::path::PathBuf) {
        let outcome = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| crate::config::import_bundle(&t));
        match outcome {
            Ok(n) => {
                // Imported files are on disk; the running model still
                // holds the old world, so a clean restart is the
                // honest way to apply everything at once.
                // Parent on whichever window is focused (Settings,
                // normally) — a modal behind the focused window reads
                // as a freeze.
                let parent = relm4::main_application()
                    .active_window()
                    .unwrap_or_else(|| self.window.clone().upcast());
                let alert = adw::MessageDialog::new(
                    Some(&parent),
                    Some(i18n("Settings Imported").as_str()),
                    Some(&i18n_f(
                        "{n} mail account(s), the cloud storage accounts and all \
                                 preferences were imported. Restart Hylki to apply them. \
                                 Passwords and sign-ins are not part of a backup; re-enter \
                                 them on first use if this is a new machine.",
                        &[("n", &n.to_string())],
                    )),
                );
                alert.add_response("later", &i18n("Later"));
                alert.add_response("restart", &i18n("Restart Hylki"));
                alert.set_response_appearance("restart", adw::ResponseAppearance::Suggested);
                alert.connect_response(None, |_, resp| {
                    if resp == "restart" {
                        // A detached shell starts the next instance
                        // once this one has quit and released its
                        // D-Bus name (started immediately, the new
                        // instance would just relay to the dying
                        // primary and exit).
                        if let Ok(exe) = std::env::current_exe() {
                            let _ = std::process::Command::new("sh")
                                .arg("-c")
                                .arg(format!("sleep 1.5; exec '{}'", exe.display()))
                                .spawn();
                        }
                        relm4::main_application().quit();
                    }
                });
                alert.present();
            }
            Err(e) => {
                self.notice_error(i18n_f("Import failed: {e}", &[("e", &(e).to_string())]));
            }
        }
    }
}
