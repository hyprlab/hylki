//! Contacts: the address books, syncing them, and importing vCards.

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::ui::contacts_page::ContactsPageInput;
use crate::ui::message_list::MessageListInput;
use crate::ui::message_view::MessageViewInput;
use crate::ui::message_window::MessageWindowInput;
use crate::ui::notifications::NotifyInput;
use crate::ui::preferences::PrefInput;
use crate::ui::sidebar::SidebarInput;

use super::demo::demo_mode;
use super::model::AppModel;
use super::msg::AppMsg;

/// Sync the address books that sync with a server, after `delay_ms` and
/// off the main thread, and read the contacts again if any did (#408).
/// Unless `force`d, a book synced in the last few minutes is left alone.
pub(super) fn sync_contact_books(sender: &ComponentSender<AppModel>, force: bool, delay_ms: u64) {
    let s = sender.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        if crate::contacts::carddav::sync_books(force) {
            s.input(AppMsg::ContactBooksSynced);
        }
    });
}

/// Runs a contact write off the main thread (EDS is talked to over
/// blocking D-Bus) and reports its error, if any, as ContactWriteDone.
fn spawn_contact_write(
    sender: &ComponentSender<AppModel>,
    write: impl FnOnce() -> Option<String> + Send + 'static,
) {
    let s = sender.clone();
    std::thread::spawn(move || {
        let err = write();
        s.input(AppMsg::ContactWriteDone(err));
    });
}

/// Read the contacts, the books new ones can go to and the Hylki book's
/// size off the main thread, after `delay_ms` (EDS takes a moment to flush
/// a write to the cache the read goes through).
pub(super) fn reload_contacts(sender: &ComponentSender<AppModel>, delay_ms: u64) {
    let s = sender.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        if demo_mode() {
            s.input(AppMsg::ContactsLoaded(crate::contacts::demo_contacts()));
            s.input(AppMsg::ContactBooksLoaded {
                books: crate::contacts::demo_books(),
                local_count: 1,
            });
            return;
        }
        let contacts = crate::contacts::read_contact_details();
        crate::services::avatar::retain_photo_thumbnails(
            &contacts.iter().map(|c| c.photo_key).collect(),
        );
        s.input(AppMsg::ContactsLoaded(contacts));
        s.input(AppMsg::ContactBooksLoaded {
            books: crate::contacts::writable_books(),
            local_count: crate::contacts::local::count(),
        });
    });
}

/// The notification text for a finished contacts import, and whether it is an error.
fn import_summary(o: &crate::contacts::local::ImportOutcome) -> (String, bool) {
    if o.added + o.updated == 0 {
        return (
            i18n("No contacts were found in the selected files"),
            o.errors > 0,
        );
    }
    let counts = [
        ("added", o.added.to_string()),
        ("updated", o.updated.to_string()),
        ("skipped", o.skipped.to_string()),
        ("errors", o.errors.to_string()),
    ];
    let args: Vec<(&str, &str)> = counts.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let text = if o.skipped + o.errors > 0 {
        i18n_f(
            "Contacts imported: {added} new, {updated} updated, {skipped} skipped, {errors} damaged",
            &args,
        )
    } else {
        i18n_f("Contacts imported: {added} new, {updated} updated", &args)
    };
    (text, false)
}

impl AppModel {
    /// Read the names "Show names from Contacts" puts on mail (#541) off
    /// the main thread, or let them go when the switch is off.
    pub(super) fn read_contact_names(&self, sender: &ComponentSender<Self>) {
        if !self.contact_names || demo_mode() {
            sender.input(AppMsg::ContactNamesRead(Default::default()));
            return;
        }
        let s = sender.clone();
        std::thread::spawn(move || {
            s.input(AppMsg::ContactNamesRead(crate::contacts::read_saved_names()));
        });
    }

    /// The window a dialog opened from Settings belongs to: Settings while
    /// it is open, else the main window.
    pub(super) fn dialog_parent(&self) -> Option<gtk::Window> {
        let prefs = self
            .prefs
            .as_ref()
            .map(|p| p.widget().clone().upcast::<gtk::Window>());
        prefs
            .filter(|w| w.is_visible())
            .or_else(|| Some(self.window.clone().upcast()))
    }

    /// Dialog to add an email to GNOME Contacts (choosing the address book).
    /// Which books can take it is asked of EDS first, off the UI thread:
    /// that opens every book, and a remote one can go to its server (#315).
    pub(super) fn show_add_contact_dialog(
        &self,
        name: &str,
        email: &str,
        sender: &ComponentSender<Self>,
    ) {
        let (name, email) = (name.to_string(), email.to_string());
        let input = sender.input_sender().clone();
        std::thread::spawn(move || {
            let books = crate::contacts::writable_books();
            let _ = input.send(AppMsg::AddContactBooks { name, email, books });
        });
    }

    #[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
    fn add_contact_dialog(
        &self,
        name: &str,
        email: &str,
        books: Vec<crate::contacts::Book>,
        sender: &ComponentSender<Self>,
    ) {
        if books.is_empty() || email.trim().is_empty() {
            self.notice_error(i18n("No address book available to add contacts"));
            return;
        }

        let dialog = adw::MessageDialog::new(
            Some(&self.window),
            Some(i18n("Add to Contacts").as_str()),
            None,
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("add", &i18n("Add"));
        dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("add"));
        dialog.set_close_response("cancel");

        let form = gtk::ListBox::new();
        form.add_css_class("boxed-list");
        form.set_selection_mode(gtk::SelectionMode::None);
        let name_row = adw::EntryRow::new();
        name_row.set_title(&i18n("Name"));
        name_row.set_text(name);
        let email_row = adw::EntryRow::new();
        email_row.set_title(&i18n("Email"));
        email_row.set_text(email);
        let book_row = adw::ComboRow::new();
        book_row.set_title(&i18n("Address book"));
        let labels: Vec<&str> = books.iter().map(|b| b.name.as_str()).collect();
        book_row.set_model(Some(&gtk::StringList::new(&labels)));
        form.append(&name_row);
        form.append(&email_row);
        form.append(&book_row);
        dialog.set_extra_child(Some(&form));

        let input = sender.input_sender().clone();
        dialog.connect_response(None, move |_, resp| {
            if resp != "add" {
                return;
            }
            let name = name_row.text().trim().to_string();
            let email = email_row.text().trim().to_string();
            let idx = book_row.selected() as usize;
            let Some(book) = books.get(idx).cloned() else {
                return;
            };
            if email.is_empty() {
                return;
            }
            // Writing talks to EDS over D-Bus (blocking) — do it off the UI thread.
            let input = input.clone();
            std::thread::spawn(move || {
                let result = crate::contacts::add_or_merge(&book.uid, &name, &email);
                let _ = input.send(AppMsg::ContactAdded(result));
            });
        });
        dialog.present();
    }

    /// Show Contacts in place of the mail panes, read afresh.
    fn show_contacts_view(&mut self, sender: &ComponentSender<Self>) {
        self.close_sidebar_peek();
        self.showing_outbox = false;
        self.leave_gallery();
        self.showing_contacts = true;
        // Read EDS off the UI thread (SQLite + photo decoding); the page
        // shows its loading face until the list lands.
        self.contacts_page.emit(ContactsPageInput::SetLoading);
        reload_contacts(sender, 0);
        if !demo_mode() {
            sync_contact_books(sender, false, 0);
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_contacts(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::AddContactAddr(addr) => {
                // From an address's right-click menu: only the address is
                // known; the dialog's name field starts blank for the user.
                self.show_add_contact_dialog("", &addr, &sender);
            }

            AppMsg::AddContactBooks { name, email, books } => {
                self.add_contact_dialog(&name, &email, books, &sender);
            }
            AppMsg::ContactAdded(result) => {
                use crate::contacts::AddOutcome;
                let (text, error) = match result {
                    Ok(AddOutcome::Created) => (i18n("Added to Contacts"), false),
                    Ok(AddOutcome::Merged(name)) => {
                        (i18n_f("Added email to {name}", &[("name", &name)]), false)
                    }
                    Ok(AddOutcome::AlreadyPresent(name)) => (
                        i18n_f("Already in Contacts ({name})", &[("name", &name)]),
                        false,
                    ),
                    Err(e) => (
                        i18n_f("Could not add contact: {e}", &[("e", &e.to_string())]),
                        true,
                    ),
                };
                self.notifications.emit(NotifyInput::Push {
                    text,
                    error,
                    connectivity: false,
                });
            }
            AppMsg::ContactPhotosChanged => {
                // The list skips the work when avatars are off; the
                // reader's cards draw initials only, so it has nothing to do.
                self.message_list
                    .emit(MessageListInput::ContactPhotosChanged);
                if self.contact_names {
                    self.read_contact_names(&sender);
                }
                // The books' files changed: a sync landed, which EDS writes
                // after its refresh call has already returned (#408).
                if self.showing_contacts && !demo_mode() {
                    reload_contacts(&sender, 0);
                }
            }

            AppMsg::ShowContactsPage => {
                crate::desktop::session::present_window(&self.window);
                sender.input(AppMsg::OpenContacts);
            }

            AppMsg::OpenContacts => {
                self.mirror_selection(crate::ui::sidebar::Sel::Contacts);
                self.show_contacts_view(&sender);
            }

            AppMsg::ContactTree(tree) => {
                self.sidebar
                    .emit(SidebarInput::SetContactTree(tree.clone()));
                self.peek_sidebar.emit(SidebarInput::SetContactTree(tree));
            }

            AppMsg::ContactScope(scope) => {
                // A book or group under the sidebar's Contacts row opens
                // Contacts on it.
                if !self.showing_contacts {
                    self.show_contacts_view(&sender);
                } else {
                    self.close_sidebar_peek();
                }
                self.contacts_page.emit(ContactsPageInput::SetScope(scope));
            }

            AppMsg::ContactBooksSynced => {
                // EDS writes what it fetched just after the sync returns.
                // Read even while Contacts is hidden: the page keeps its
                // list between visits, and this keeps that list current.
                reload_contacts(&sender, 1500);
                if self.filters.iter().any(|r| r.needs_contacts()) {
                    self.saved_addresses.refresh();
                }
            }

            AppMsg::ContactBooksLoaded { books, local_count } => {
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::SetContactBooks(books.clone()));
                    p.emit(PrefInput::SetLocalContactCount(local_count));
                }
                self.contacts_page.emit(ContactsPageInput::SetBooks(books));
            }

            AppMsg::ContactSuggestions => {
                let own: Vec<(String, String)> = self
                    .config
                    .iter()
                    .map(|c| (c.name.clone(), c.email.clone()))
                    .collect();
                let page = self.contacts_page.sender().clone();
                std::thread::spawn(move || {
                    let _ = page.send(ContactsPageInput::SuggestionsLoaded(
                        crate::contacts::suggestions(&own),
                    ));
                });
            }

            AppMsg::ContactsLoaded(contacts) => {
                self.contacts_page
                    .emit(ContactsPageInput::SetContacts(contacts));
                // A contact was saved, deleted or imported: the rules on
                // whether a sender is in Contacts should know.
                if self.filters.iter().any(|r| r.needs_contacts()) {
                    self.saved_addresses.refresh();
                }
                if self.contact_names {
                    self.read_contact_names(&sender);
                }
            }

            AppMsg::ContactNamesRead(names) => {
                // A late read must not bring back names switched off since.
                let names = if self.contact_names {
                    names
                } else {
                    Default::default()
                };
                if crate::contacts::set_contact_names(names) {
                    self.message_list
                        .emit(MessageListInput::ContactNamesChanged);
                    self.message_view.emit(MessageViewInput::FacesChanged);
                    self.popouts_emit(|| MessageWindowInput::FacesChanged);
                }
            }

            AppMsg::LaunchGnomeContacts => crate::ui::contacts_browser::launch_gnome_contacts(),

            AppMsg::SaveContact { book_uid, vcard } => {
                spawn_contact_write(&sender, move || {
                    let result = crate::contacts::modify_contact(&book_uid, &vcard);
                    result.err()
                });
            }

            AppMsg::CreateContact { book_uid, vcard } => {
                spawn_contact_write(&sender, move || {
                    // No book picked (none was listed yet): the default one.
                    let book_uid = match book_uid {
                        Some(uid) => Some(uid),
                        None => crate::contacts::writable_books()
                            .first()
                            .map(|b| b.uid.clone()),
                    };
                    let result = match book_uid {
                        Some(uid) => crate::contacts::create_contact(&uid, &vcard),
                        None => Err(i18n("No address book available")),
                    };
                    result.err()
                });
            }

            AppMsg::SaveGroup {
                book_uid,
                format,
                original,
                edit,
            } => {
                spawn_contact_write(&sender, move || {
                    let result =
                        crate::contacts::save_group(&book_uid, format, original.as_deref(), edit);
                    result.err()
                });
            }

            AppMsg::DeleteContacts(keys) => {
                spawn_contact_write(&sender, move || {
                    // One call per book.
                    let mut by_book: Vec<(String, Vec<String>)> = Vec::new();
                    for (book, uid) in keys {
                        match by_book.iter_mut().find(|(b, _)| *b == book) {
                            Some((_, uids)) => uids.push(uid),
                            None => by_book.push((book, vec![uid])),
                        }
                    }
                    by_book
                        .iter()
                        .filter_map(|(book, uids)| {
                            crate::contacts::delete_contacts(book, uids).err()
                        })
                        .next()
                });
            }

            AppMsg::MoveGroup {
                from,
                uid,
                to,
                format,
                edit,
            } => {
                spawn_contact_write(&sender, move || {
                    let result = crate::contacts::move_group(&from, &uid, &to, format, edit);
                    result.err()
                });
            }

            AppMsg::ContactDrop { keys, on, moving } => {
                self.contacts_page
                    .emit(ContactsPageInput::DropContacts { keys, on, moving });
            }

            AppMsg::ContactTreeAction(action) => {
                self.close_sidebar_peek();
                self.contacts_page.emit(ContactsPageInput::Tree(action));
            }

            AppMsg::ContactsFolded(keys) => {
                self.contacts_folded = keys;
                self.save_sidebar_state();
            }

            AppMsg::RenameBook { uid, name } => {
                if let Err(e) = crate::contacts::contact_sources::set_name(&uid, name.as_deref()) {
                    self.notice_error(i18n_f(
                        "Could not rename the address book: {e}",
                        &[("e", &e)],
                    ));
                }
                // Every name of the book is read again with the books.
                reload_contacts(&sender, 0);
            }

            AppMsg::CopyContacts { to, cards } => {
                spawn_contact_write(&sender, move || {
                    let failed: Vec<String> = cards
                        .iter()
                        .filter_map(|card| crate::contacts::create_contact(&to, card).err())
                        .collect();
                    failed.first().map(|e| {
                        let n = failed.len() as u32;
                        ni18n_f(
                            "{n} contact was not copied: {e}",
                            "{n} contacts were not copied: {e}",
                            n,
                            &[("n", &n.to_string()), ("e", e)],
                        )
                    })
                });
            }

            AppMsg::MoveContacts { to, cards } => {
                spawn_contact_write(&sender, move || {
                    let failed: Vec<String> = cards
                        .iter()
                        .filter_map(|(book, uid, vcard)| {
                            crate::contacts::move_contact(book, uid, vcard, &to).err()
                        })
                        .collect();
                    failed.first().map(|e| {
                        let n = failed.len() as u32;
                        ni18n_f(
                            "{n} contact was not moved: {e}",
                            "{n} contacts were not moved: {e}",
                            n,
                            &[("n", &n.to_string()), ("e", e)],
                        )
                    })
                });
            }

            AppMsg::ContactWriteDone(err) => {
                if let Some(e) = err {
                    // What was taken off the list may still be there.
                    self.contacts_page.emit(ContactsPageInput::WriteFailed);
                    self.notice_error(i18n_f(
                        "Could not update contact: {e}",
                        &[("e", &(e).to_string())],
                    ));
                }
                // Success or not, re-read so the card shows what EDS holds.
                // A short pause lets EDS flush the write to its SQLite cache
                // (that is what the read goes through).
                reload_contacts(&sender, 500);
            }

            AppMsg::ImportContacts(paths) => {
                let s = sender.clone();
                std::thread::spawn(move || {
                    s.input(AppMsg::ContactsImported(crate::contacts::import_vcf_files(
                        &paths,
                    )));
                });
            }

            AppMsg::ContactsImported(result) => {
                let (text, error) = match &result {
                    Err(e) => (i18n_f("Could not import contacts: {e}", &[("e", e)]), true),
                    Ok(o) => import_summary(o),
                };
                // The bar opens for errors only; a good import shows in the
                // list, and its counts wait in the bar for whoever opens it.
                if error {
                    self.notifications.emit(NotifyInput::Push {
                        text,
                        error,
                        connectivity: false,
                    });
                } else {
                    self.notifications.emit(NotifyInput::SetStatus(text));
                }
                if let Ok(o) = &result {
                    if !o.failed_files.is_empty() {
                        self.notice_error(i18n_f(
                            "Could not read: {files}",
                            &[("files", &o.failed_files.join("; "))],
                        ));
                    }
                }
                reload_contacts(&sender, 0);
            }
            _ => unreachable!("not routed to on_contacts"),
        }
    }
}

/// Nothing keeps a CardDAV or Online Accounts address book in step
/// unless an app holds it open, and off GNOME there may be no other
/// (#408). A moment after the window, not in its way.
pub(super) fn start_contacts_sync(sender: &ComponentSender<AppModel>) {
    if !demo_mode() {
        sync_contact_books(sender, false, 5000);
    }
    // Contacts is read and listed in the background, so the first visit
    // shows it at once (#477); a visit re-reads it to catch up.
    reload_contacts(sender, 4000);
    // Refresh visible avatars when the GNOME Contacts photo index
    // changes (first load finishing, or an EDS/CardDAV sync).
    crate::contacts::watch_photo_changes({
        let input = sender.input_sender().clone();
        move || {
            let _ = input.send(AppMsg::ContactPhotosChanged);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_import_summary_counts() {
        use crate::contacts::local::ImportOutcome;
        let none = ImportOutcome {
            errors: 1,
            ..Default::default()
        };
        assert_eq!(
            import_summary(&none),
            (
                "No contacts were found in the selected files".to_string(),
                true
            )
        );
        let ok = ImportOutcome {
            added: 2,
            updated: 1,
            ..Default::default()
        };
        assert_eq!(
            import_summary(&ok),
            ("Contacts imported: 2 new, 1 updated".to_string(), false)
        );
        let mixed = ImportOutcome {
            added: 1,
            skipped: 3,
            ..Default::default()
        };
        assert_eq!(
            import_summary(&mixed).0,
            "Contacts imported: 1 new, 0 updated, 3 skipped, 0 damaged"
        );
    }
}
