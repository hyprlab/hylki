//! The pages of the Contacts settings: the list of sources, a source's own
//! page, the add page, and the checks run before a source is saved.

use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use crate::contacts::carddav::{self, Account, Status};
use crate::contacts::contact_sources as sources;
use crate::contacts::contact_sources::{Kind, Origin};
use crate::contacts::directory::{self, Directory};
use crate::i18n::{i18n, i18n_f, ni18n_f};

use super::forms::{
    books_group, carddav_form, info_group, kind_picker, ldap_form, name_group, source_header,
};
use super::{
    mark, AddWhat, ContactSources, ContactSourcesInput, ContactSourcesOutput, Editing, ADD_KINDS,
};

/// After a CardDAV save: show the account at once, wait for EDS to look at
/// the server, and say what it found. Blocking; `signing_in` when a new
/// password was given.
pub(super) fn report_carddav(
    s: &relm4::Sender<ContactSourcesInput>,
    saved: Result<String, String>,
    signing_in: bool,
) {
    let uid = match saved {
        Ok(uid) => uid,
        Err(e) => {
            let _ = s.send(ContactSourcesInput::Done {
                error: Some(e),
                found: None,
                checked: None,
            });
            return;
        }
    };
    let _ = s.send(ContactSourcesInput::Load);
    let found =
        carddav::await_books(&uid, std::time::Duration::from_secs(30), signing_in).map(|a| {
            let status = carddav::status(&a);
            (a, status)
        });
    let _ = s.send(ContactSourcesInput::Done {
        error: None,
        found,
        checked: None,
    });
}

/// How long Settings waits on a directory's first answer. EDS sets no
/// timeout of its own on the connection, and holds the book while it waits.
const CHECK_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

/// [`directory::check`], given up on after `limit`; the check itself runs on
/// to its end in the background.
fn check_within(dir: Directory, limit: std::time::Duration) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let name = dir.title();
    std::thread::spawn(move || {
        let _ = tx.send(directory::check(&dir));
    });
    rx.recv_timeout(limit).unwrap_or_else(|_| {
        tracing::warn!(directory = %name, "LDAP directory check gave no answer in {}s", limit.as_secs());
        Err(i18n("The server took too long to answer."))
    })
}

/// Save an LDAP directory off the main thread, then try it once it shows up
/// in the registry, and report with Done.
pub(super) fn save_ldap(
    dir: Directory,
    password: String,
    sender: &ComponentSender<ContactSources>,
) {
    let s = sender.input_sender().clone();
    std::thread::spawn(move || {
        let password = (!password.is_empty()).then_some(password);
        let msg = match directory::save(&dir, password.as_deref()) {
            Err(e) => ContactSourcesInput::Done {
                error: Some(e),
                found: None,
                checked: None,
            },
            Ok(uid) => {
                // A new source takes a moment to appear in the
                // registry; check it once it has.
                let mut found = None;
                for _ in 0..10 {
                    found = directory::list()
                        .ok()
                        .and_then(|l| l.into_iter().find(|d| d.uid == uid));
                    if found.is_some() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                let _ = s.send(ContactSourcesInput::Load);
                let checked = found.map(|d| (d.title(), check_within(d, CHECK_LIMIT)));
                ContactSourcesInput::Done {
                    error: None,
                    found: None,
                    checked,
                }
            }
        };
        let _ = s.send(msg);
    });
}

impl ContactSources {
    pub(super) fn toast(&self, text: &str) {
        let toast = adw::Toast::new(text);
        toast.set_timeout(6);
        self.toasts.add_toast(toast);
    }

    fn push_editor(&self) {
        if !self
            .nav
            .visible_page()
            .and_then(|p| p.tag())
            .is_some_and(|t| t == "editor")
        {
            self.nav.push_by_tag("editor");
        }
    }

    pub(super) fn close_editor(&mut self) {
        self.editing = None;
        if self
            .nav
            .visible_page()
            .and_then(|p| p.tag())
            .is_some_and(|t| t == "editor")
        {
            self.nav.pop();
        }
    }

    /// Tell the window whether a form with changes to keep is up, which the
    /// save action decides.
    fn announce(&self, sender: &ComponentSender<Self>) {
        let form = self.save_action.borrow().is_some();
        let _ = sender.output(ContactSourcesOutput::PageOpen { open: true, form });
    }

    /// The page about one source.
    pub(super) fn open_source(&mut self, index: usize, sender: &ComponentSender<Self>) {
        let Some(source) = self.sources.get(index).cloned() else {
            return;
        };
        self.editing = Some(Editing::Source(source.id.clone()));
        *self.save_action.borrow_mut() = None;
        self.editor_page.set_title(&source.title);
        let removable = matches!(source.kind, Kind::CardDav(_) | Kind::Ldap(_));
        self.remove_btn.set_visible(removable);
        let page = gtk::Box::new(gtk::Orientation::Vertical, 24);
        page.append(&source_header(&source));
        page.append(&name_group(&source, sender.input_sender().clone()));
        if let Some(why) = &source.paused {
            let note = adw::PreferencesGroup::new();
            note.set_description(Some(why));
            page.append(&note);
        }
        let form: Option<(gtk::Widget, Rc<dyn Fn() -> bool>)> = match &source.kind {
            Kind::Hylki => {
                page.append(&self.hylki_group(sender));
                None
            }
            Kind::CardDav(a) => Some(carddav_form(
                a.clone(),
                None,
                ADD_KINDS[0].what,
                sender.input_sender().clone(),
            )),
            Kind::Ldap(d) => Some(ldap_form(d.clone(), sender.input_sender().clone())),
            Kind::ThisComputer => {
                page.append(&info_group(
                    &i18n("Kept by Evolution Data Server"),
                    &i18n("The address books GNOME Contacts and Evolution keep on this computer."),
                    &i18n("Open GNOME Contacts"),
                    crate::ui::contacts_browser::launch_gnome_contacts,
                ));
                None
            }
            Kind::Online => {
                page.append(&info_group(
                    &i18n("Set up in GNOME Online Accounts"),
                    &i18n("Its sign-in and its Contacts switch are in GNOME Settings."),
                    &i18n("Open Online Accounts"),
                    crate::ui::accounts::open_online_accounts,
                ));
                None
            }
            Kind::Other => None,
        };
        if let Some((widget, save)) = form {
            page.append(&widget);
            *self.save_action.borrow_mut() = Some(save);
        }
        if !source.books.is_empty() {
            page.append(&books_group(&source, sender.input_sender().clone()));
        }
        self.save_btn
            .set_visible(self.save_action.borrow().is_some());
        self.editor_slot.set_child(Some(&page));
        self.push_editor();
        self.announce(sender);
    }

    /// The add page, on the kind at `kind` in the dropdown.
    pub(super) fn open_add(&mut self, kind: u32, sender: &ComponentSender<Self>) {
        self.editing = Some(Editing::Add);
        self.editor_page.set_title(&i18n("Add Address Book"));
        self.remove_btn.set_visible(false);
        let what = ADD_KINDS.get(kind as usize).unwrap_or(&ADD_KINDS[0]).what;
        match what {
            AddWhat::CardDav { server, .. } => {
                let mut account = Account::empty();
                account.url = server.unwrap_or_default().to_string();
                self.show_carddav_form(account, kind, what, sender);
            }
            AddWhat::Ldap => {
                let page = gtk::Box::new(gtk::Orientation::Vertical, 24);
                page.append(&kind_picker(kind, sender.input_sender().clone()));
                let (form, save) = ldap_form(Directory::empty(), sender.input_sender().clone());
                page.append(&form);
                *self.save_action.borrow_mut() = Some(save);
                self.save_btn.set_visible(true);
                self.editor_slot.set_child(Some(&page));
            }
            AddWhat::Online => {
                let page = gtk::Box::new(gtk::Orientation::Vertical, 24);
                page.append(&kind_picker(kind, sender.input_sender().clone()));
                page.append(&info_group(
                    &i18n("Added in GNOME Online Accounts"),
                    &i18n("Google and Microsoft 365 sign in through GNOME Online Accounts. Add the account there with Contacts on, and it will be listed here."),
                    &i18n("Open Online Accounts"),
                    crate::ui::accounts::open_online_accounts,
                ));
                *self.save_action.borrow_mut() = None;
                self.save_btn.set_visible(false);
                self.editor_slot.set_child(Some(&page));
            }
        }
        self.push_editor();
        self.announce(sender);
    }

    /// The add page's CardDAV form, under the kind dropdown.
    fn show_carddav_form(
        &mut self,
        account: Account,
        kind: u32,
        what: AddWhat,
        sender: &ComponentSender<Self>,
    ) {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 24);
        page.append(&kind_picker(kind, sender.input_sender().clone()));
        let (form, save) = carddav_form(account, Some(kind), what, sender.input_sender().clone());
        page.append(&form);
        *self.save_action.borrow_mut() = Some(save);
        self.save_btn.set_visible(true);
        self.editor_slot.set_child(Some(&page));
    }

    /// The Hylki book's own rows: its size, import, export, emptying.
    fn hylki_group(&self, sender: &ComponentSender<Self>) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::new();
        group.set_title(&i18n("Hylki Address Book"));
        group.set_description(Some(&i18n(
            "Kept in Hylki's own data folder. Works without GNOME.",
        )));
        let count = adw::ActionRow::new();
        count.set_title(&i18n("Contacts in the Hylki book"));
        let n = gtk::Label::new(Some(&self.local_count.to_string()));
        n.add_css_class("dim-label");
        count.add_suffix(&n);
        group.add(&count);
        let action =
            |title: String, subtitle: String, msg: fn() -> ContactSourcesInput, enabled: bool| {
                let row = adw::ActionRow::new();
                row.set_title(&title);
                row.set_subtitle(&subtitle);
                row.set_activatable(true);
                row.set_sensitive(enabled);
                row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                let s = sender.input_sender().clone();
                row.connect_activated(move |_| {
                    let _ = s.send(msg());
                });
                row
            };
        group.add(&action(
            i18n("Import vCard or CSV files"),
            i18n("Adds contacts from .vcf or .csv files. Importing a file again updates existing entries."),
            || ContactSourcesInput::ImportLocal,
            true,
        ));
        group.add(&action(
            i18n("Export as a vCard file"),
            i18n("Saves every contact in the Hylki book in one .vcf file."),
            || ContactSourcesInput::ExportLocal,
            self.local_count > 0,
        ));
        let delete = action(
            i18n("Delete all contacts"),
            i18n("Other address books are not touched."),
            || ContactSourcesInput::DeleteAllLocal,
            self.local_count > 0,
        );
        delete.add_css_class("error");
        group.add(&delete);
        group
    }

    pub(super) fn rebuild(&mut self, sender: &ComponentSender<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        // Marks and badges each get a column, held to one width down the
        // list, as on Mail Accounts.
        let mark_widths = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        let badge_widths = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        let slot = |child: &gtk::Widget, group: &gtk::SizeGroup| {
            let slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            slot.set_valign(gtk::Align::Center);
            child.set_halign(gtk::Align::Center);
            child.set_hexpand(true);
            slot.append(child);
            slot.set_hexpand(false);
            group.add_widget(&slot);
            slot
        };
        for (i, s) in self.sources.iter().enumerate() {
            let row = gtk::ListBoxRow::new();
            row.set_activatable(true);
            let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            hbox.add_css_class("account-list-row");
            hbox.append(&slot(&mark(s.mark, 24), &mark_widths));
            let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
            vbox.set_hexpand(true);
            vbox.set_valign(gtk::Align::Center);
            let title = gtk::Label::new(Some(&s.title));
            title.set_halign(gtk::Align::Start);
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            title.add_css_class("account-name");
            let detail = gtk::Label::new(Some(s.paused.as_deref().unwrap_or(&s.detail)));
            detail.set_halign(gtk::Align::Start);
            detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
            detail.add_css_class("account-email");
            detail.set_tooltip_text(Some(s.paused.as_deref().unwrap_or(&s.detail)));
            if s.status.as_ref().and_then(Status::problem).is_some() && s.on {
                detail.add_css_class("error");
            }
            vbox.append(&title);
            vbox.append(&detail);
            hbox.append(&vbox);
            let (label, tip) = match s.origin {
                Origin::Hylki => (i18n("Hylki"), i18n("Set up in Hylki")),
                Origin::Online => (i18n("GOA"), i18n("From GNOME Online Accounts")),
                Origin::Evolution => (
                    i18n("Evolution"),
                    i18n("Kept by Evolution Data Server, set up in Evolution or GNOME Contacts"),
                ),
            };
            let badge = gtk::Label::new(Some(&label));
            badge.add_css_class("account-source-badge");
            if s.origin == Origin::Online {
                badge.add_css_class("goa");
            }
            badge.set_tooltip_text(Some(&tip));
            hbox.append(&slot(badge.upcast_ref(), &badge_widths));
            let toggle = gtk::Switch::new();
            toggle.set_valign(gtk::Align::Center);
            toggle.set_active(s.on);
            toggle.set_sensitive(s.paused.is_none());
            toggle.set_tooltip_text(Some(&i18n("Use in Hylki")));
            let input = sender.input_sender().clone();
            toggle.connect_state_set(move |_, on| {
                let _ = input.send(ContactSourcesInput::Toggle { index: i, on });
                gtk::glib::Propagation::Proceed
            });
            hbox.append(&toggle);
            let next = gtk::Image::from_icon_name("go-next-symbolic");
            next.add_css_class("dim-label");
            hbox.append(&next);
            row.set_child(Some(&hbox));
            self.list.append(&row);
        }
    }

    /// Ask before removing source `i`; Remove does it.
    pub(super) fn confirm_remove(&self, i: usize, sender: &ComponentSender<Self>) {
        let Some(source) = self.sources.get(i) else {
            return;
        };
        let body = match source.kind {
            Kind::Ldap(_) => {
                i18n("Evolution and other apps stop seeing it too. Nothing on the server changes.")
            }
            _ => i18n(
                "Evolution and other apps stop seeing them too. Nothing on the server changes.",
            ),
        };
        let parent = relm4::main_application().active_window();
        let s = sender.input_sender().clone();
        crate::ui::dialog::confirm_destructive(
            parent.as_ref(),
            &i18n_f("Remove {name}?", &[("name", &source.title)]),
            &body,
            "remove",
            &i18n("Remove"),
            move || {
                let _ = s.send(ContactSourcesInput::Remove(i));
            },
        );
    }

    /// Remove source `i` from EDS, off the main thread, with its switch.
    pub(super) fn remove_source(&mut self, i: usize, sender: &ComponentSender<Self>) {
        let Some(source) = self.sources.get(i).cloned() else {
            return;
        };
        *self.save_action.borrow_mut() = None;
        self.close_editor();
        let s = sender.input_sender().clone();
        std::thread::spawn(move || {
            let error = match &source.kind {
                Kind::CardDav(a) => carddav::remove(a).err(),
                Kind::Ldap(d) => directory::remove(d).err(),
                _ => None,
            };
            // Its switch goes with it.
            let _ = sources::set_off(&source.id, false);
            let _ = s.send(ContactSourcesInput::Done {
                error,
                found: None,
                checked: None,
            });
        });
    }

    /// Report how a save, a removal or a check went, and read the sources
    /// again.
    pub(super) fn report_done(
        &self,
        error: Option<String>,
        found: Option<(Account, Status)>,
        checked: Option<(String, Result<(), String>)>,
        sender: &ComponentSender<Self>,
    ) {
        if let Some(e) = error {
            self.toast(&e);
        }
        if let Some((a, status)) = found {
            let n = a.books.len() as u32;
            match status.problem() {
                Some(problem) => self.toast(&i18n_f(
                    "{name}: {problem}",
                    &[("name", &a.title()), ("problem", &problem)],
                )),
                None if n == 0 => self.toast(&i18n_f(
                    "No address books were found on {name} yet.",
                    &[("name", &a.title())],
                )),
                None => self.toast(&ni18n_f(
                    "Found {n} address book on {name}",
                    "Found {n} address books on {name}",
                    n,
                    &[("n", &n.to_string()), ("name", &a.title())],
                )),
            }
        }
        match checked {
            Some((name, Ok(()))) => self.toast(&i18n_f("Connected to {name}", &[("name", &name)])),
            Some((name, Err(e))) => self.toast(&i18n_f(
                "{name} did not answer: {error}",
                &[("name", &name), ("error", &e)],
            )),
            None => {}
        }
        let _ = sender.output(ContactSourcesOutput::Changed);
        sender.input(ContactSourcesInput::Load);
    }
}
