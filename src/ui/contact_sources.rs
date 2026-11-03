//! Settings → Contacts: every place Hylki finds contacts, laid out as Mail
//! Accounts is. One row per source (the Hylki book, this computer's books,
//! each GNOME Online Account that keeps contacts, CardDAV accounts and LDAP
//! directories), each with a switch that uses it in Hylki or not, and a
//! page of its own: its address books, its sign-in, or the Hylki book's
//! import and export. **Add Address Book…** sets up CardDAV accounts and
//! LDAP directories, and says where Google and Microsoft 365 are added.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use crate::contacts::carddav::{self, Account, Status};
use crate::contacts::contact_sources::{self as sources, Source};
use crate::contacts::directory::Directory;
use crate::i18n::{i18n, i18n_f, i18n_noop};

mod forms;
mod pages;

use pages::{report_carddav, save_ldap};

/// What the editor page shows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Editing {
    /// A source's page, by its ID: its place in the list can change as
    /// the list is read again.
    Source(String),
    Add,
}

/// What can be added from **Add Address Book…**.
#[derive(Debug, Clone, Copy)]
enum AddWhat {
    /// A CardDAV account; `server` is filled in for a known service.
    CardDav {
        server: Option<&'static str>,
        hint: &'static str,
    },
    Ldap,
    /// Added in GNOME Online Accounts: Hylki cannot sign in to it itself.
    Online,
}

struct AddKind {
    label: &'static str,
    mark: &'static str,
    what: AddWhat,
}

const APP_PW: &str = i18n_noop("Use an app-specific password, not your usual one.");

const ADD_KINDS: &[AddKind] = &[
    AddKind {
        label: i18n_noop("CardDAV Server"),
        mark: "folder-remote-symbolic",
        what: AddWhat::CardDav {
            server: None,
            hint: i18n_noop("The server's address is usually enough. Two-step sign-in needs an app password."),
        },
    },
    AddKind { label: "iCloud", mark: "icloud", what: AddWhat::CardDav { server: Some("https://contacts.icloud.com"), hint: APP_PW } },
    AddKind {
        label: "Nextcloud",
        mark: "nextcloud",
        what: AddWhat::CardDav {
            server: None,
            hint: i18n_noop("Such as cloud.example.org. With two-factor sign-in, make an app password in Nextcloud under Settings → Security."),
        },
    },
    AddKind {
        label: "Fastmail",
        mark: "fastmail",
        what: AddWhat::CardDav {
            server: Some("https://carddav.fastmail.com"),
            hint: i18n_noop("Use an app password with access to contacts, made in Fastmail under Settings → Privacy & Security."),
        },
    },
    AddKind { label: "Yahoo", mark: "yahoo", what: AddWhat::CardDav { server: Some("https://carddav.address.yahoo.com"), hint: APP_PW } },
    AddKind { label: i18n_noop("LDAP Directory"), mark: "network-server-symbolic", what: AddWhat::Ldap },
    AddKind { label: "Google", mark: "gmail", what: AddWhat::Online },
    AddKind { label: "Microsoft 365 / Outlook", mark: "outlook", what: AddWhat::Online },
];

/// A brand mark, or the icon of that name when there is none.
fn mark(id: &str, px: i32) -> gtk::Widget {
    crate::desktop::brand::mark(id, px, id)
}

/// The editor page's Save, shared with the page's widgets (see
/// `ContactSources::save_action`).
type SaveAction = Rc<RefCell<Option<Rc<dyn Fn() -> bool>>>>;

pub(super) struct ContactSources {
    sources: Vec<Source>,
    /// The books new contacts can go to, the chosen default first.
    books: Vec<crate::contacts::Book>,
    local_count: usize,
    list: gtk::ListBox,
    add_btn: gtk::Button,
    no_eds: gtk::Label,
    book_row: adw::ComboRow,
    /// Set while the book row's model is replaced, which fires its notify.
    book_guard: Rc<std::cell::Cell<bool>>,
    placement_row: adw::ComboRow,
    placement_guard: Rc<std::cell::Cell<bool>>,
    toasts: adw::ToastOverlay,
    nav: adw::NavigationView,
    editor_page: adw::NavigationPage,
    editor_slot: adw::Bin,
    save_btn: gtk::Button,
    remove_btn: gtk::Button,
    editing: Option<Editing>,
    /// What the editor's Save does: reads the form and sends the save;
    /// answers whether it accepted the form. `None` on a page with nothing
    /// to save.
    save_action: SaveAction,
}

#[derive(Debug)]
pub(super) enum ContactSourcesInput {
    /// Ask EDS, GNOME Online Accounts and the keyring again.
    Load,
    Loaded {
        sources: Vec<Source>,
        eds: bool,
    },
    /// The books new contacts can go to, and the Hylki book's size, from
    /// the app.
    SetBooks(Vec<crate::contacts::Book>),
    SetLocalCount(usize),
    ChangeBook(u32),
    Open(usize),
    Add,
    /// The kind picked in the add page's dropdown.
    AddKind(u32),
    Toggle {
        index: usize,
        on: bool,
    },
    ToggleBook {
        book: String,
        on: bool,
    },
    /// A name given in Hylki to an account or an address book, by its ID
    /// (`None`: its own name again).
    Rename {
        id: String,
        name: Option<String>,
    },
    SaveClicked,
    CloseEditor,
    RemoveCurrent,
    ConfirmRemove(usize),
    Remove(usize),
    SaveCardDav {
        account: Account,
        password: String,
    },
    SaveLdap {
        dir: Directory,
        password: String,
    },
    /// A save, switch or removal went through (or not). `found` is a saved
    /// CardDAV account once EDS had looked at the server, `checked` a
    /// saved directory's first answer.
    Done {
        error: Option<String>,
        found: Option<(Account, Status)>,
        checked: Option<(String, Result<(), String>)>,
    },
    ImportLocal,
    ExportLocal,
    DeleteAllLocal,
    /// Settings → Sidebar's Contacts placement changed.
    SetPlacement(crate::config::SectionPlacement),
}

#[derive(Debug)]
pub(crate) enum ContactSourcesOutput {
    /// A page is pushed over the list (or gone); `form` when it holds
    /// changes Save keeps, so leaving it asks first.
    PageOpen {
        open: bool,
        form: bool,
    },
    /// What Hylki reads contacts from changed: read them again.
    Changed,
    ImportLocal,
    ExportLocal,
    DeleteAllLocal,
    SetContactBook(String),
    /// The page is showing: read the books new contacts can go to.
    LoadBooks,
    /// Keep the Contacts search field open.
    SetSearchOpen(bool),
    /// Saved contacts go by their address-book names in mail (#541).
    SetContactNames(bool),
    /// Where Contacts sits in the sidebar.
    SetPlacement(crate::config::SectionPlacement),
}

pub(super) struct ContactSourcesInit {
    /// Whether the Contacts search field is kept open.
    pub(super) search_open: bool,
    pub(super) placement: crate::config::SectionPlacement,
    /// Whether saved contacts go by their address-book names in mail.
    pub(super) contact_names: bool,
}

#[relm4::component(pub(crate))]
impl SimpleComponent for ContactSources {
    type Init = ContactSourcesInit;
    type Input = ContactSourcesInput;
    type Output = ContactSourcesOutput;

    view! {
        adw::Bin {
            #[wrap(Some)]
            #[name = "nav"]
            set_child = &adw::NavigationView {
                add = &adw::NavigationPage {
                    set_title: &i18n("Contacts"),
                    set_tag: Some("list"),

                    #[wrap(Some)]
                    set_child = &adw::ToolbarView {
                        #[wrap(Some)]
                        #[name = "toasts"]
                        set_content = &adw::ToastOverlay {
                            #[wrap(Some)]
                            set_child = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Address Books"),
                                    set_description: Some(&i18n("Where contacts come from for Contacts, suggestions, sender pictures and filters. Switching one off leaves it out of Hylki.")),
                                    #[wrap(Some)]
                                    set_header_suffix = &gtk::Box {
                                        set_orientation: gtk::Orientation::Horizontal,
                                        set_spacing: 6,
                                        set_valign: gtk::Align::Start,
                                        set_halign: gtk::Align::End,
                                        set_margin_start: 24,
                                        // One just added in GNOME Settings,
                                        // without leaving the page.
                                        gtk::Button {
                                            set_icon_name: "view-refresh-symbolic",
                                            set_tooltip_text: Some(&i18n("Look again for address books")),
                                            connect_clicked => ContactSourcesInput::Load,
                                        },
                                        #[name = "add_btn"]
                                        gtk::Button {
                                            set_label: &i18n("Add Address Book…"),
                                            connect_clicked => ContactSourcesInput::Add,
                                        },
                                    },
                                    #[name = "list"]
                                    gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                        connect_row_activated[sender] => move |_, row| {
                                            sender.input(ContactSourcesInput::Open(row.index() as usize));
                                        },
                                    },
                                    #[name = "no_eds"]
                                    gtk::Label {
                                        set_label: &i18n("Address books need Evolution Data Server, which is not installed. Install the evolution-data-server package (it runs without GNOME) and reopen this page."),
                                        add_css_class: "dim-label",
                                        set_wrap: true,
                                        set_xalign: 0.0,
                                        set_margin_top: 12,
                                        set_visible: false,
                                    },
                                },
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("New Contacts"),
                                    #[name = "book_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Save new contacts in"),
                                        set_subtitle: &i18n("Unless another is picked when the contact is made."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(ContactSourcesInput::ChangeBook(row.selected()));
                                        },
                                    },
                                },
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("In Mail"),
                                    // Thunderbird's "Show only the display
                                    // name for people in my address book".
                                    adw::SwitchRow {
                                        set_title: &i18n("Show names from Contacts"),
                                        set_subtitle: &i18n("Saved addresses show the contact's name, not the mail's."),
                                        set_active: init.contact_names,
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(ContactSourcesOutput::SetContactNames(row.is_active()));
                                        },
                                    },
                                },
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Layout"),
                                    // The same row as Settings → Sidebar's.
                                    #[name = "placement_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Contacts placement"),
                                        set_subtitle: &i18n("'In the footer' disables expand/collapse."),
                                        set_model: Some(&crate::ui::preferences::contacts_placement_model()),
                                        set_selected: crate::ui::preferences::contacts_placement_index(init.placement),
                                    },
                                    adw::SwitchRow {
                                        set_title: &i18n("Keep the search field open"),
                                        set_active: init.search_open,
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(ContactSourcesOutput::SetSearchOpen(row.is_active()));
                                        },
                                    },
                                },
                            },
                        },
                    },
                },

                #[name = "editor_page"]
                add = &adw::NavigationPage {
                    set_title: &i18n("Address Book"),
                    set_tag: Some("editor"),

                    #[wrap(Some)]
                    set_child = &adw::ToolbarView {
                        add_top_bar = &adw::HeaderBar {
                            set_show_end_title_buttons: true,
                            #[name = "save_btn"]
                            pack_end = &gtk::Button {
                                set_label: &i18n("Save"),
                                add_css_class: "suggested-action",
                                connect_clicked => ContactSourcesInput::SaveClicked,
                            },
                            #[name = "remove_btn"]
                            pack_end = &gtk::Button {
                                set_label: &i18n("Remove"),
                                add_css_class: "destructive-action",
                                set_visible: false,
                                connect_clicked => ContactSourcesInput::RemoveCurrent,
                            },
                        },

                        #[wrap(Some)]
                        set_content = &gtk::ScrolledWindow {
                            set_hscrollbar_policy: gtk::PolicyType::Never,
                            #[wrap(Some)]
                            set_child = &adw::Clamp {
                                set_maximum_size: 640,
                                set_tightening_threshold: 480,
                                set_margin_top: 24,
                                set_margin_bottom: 24,
                                set_margin_start: 24,
                                set_margin_end: 24,
                                #[wrap(Some)]
                                #[name = "editor_slot"]
                                set_child = &adw::Bin {},
                            },
                        },
                    },
                },
            },
        }
    }

    fn init(
        init: ContactSourcesInit,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let widgets = view_output!();
        // Connected once its choice is set, and quiet while the Sidebar
        // page's row sets it.
        let placement_guard = Rc::new(std::cell::Cell::new(false));
        {
            let (out, guard) = (sender.output_sender().clone(), placement_guard.clone());
            widgets.placement_row.connect_selected_notify(move |row| {
                if !guard.get() {
                    let _ = out.send(ContactSourcesOutput::SetPlacement(
                        crate::ui::preferences::contacts_placement_from_index(row.selected()),
                    ));
                }
            });
        }
        let model = ContactSources {
            sources: Vec::new(),
            books: Vec::new(),
            local_count: 0,
            list: widgets.list.clone(),
            add_btn: widgets.add_btn.clone(),
            no_eds: widgets.no_eds.clone(),
            book_row: widgets.book_row.clone(),
            book_guard: Rc::new(std::cell::Cell::new(false)),
            placement_row: widgets.placement_row.clone(),
            placement_guard,
            toasts: widgets.toasts.clone(),
            nav: widgets.nav.clone(),
            editor_page: widgets.editor_page.clone(),
            editor_slot: widgets.editor_slot.clone(),
            save_btn: widgets.save_btn.clone(),
            remove_btn: widgets.remove_btn.clone(),
            editing: None,
            save_action: Rc::new(RefCell::new(None)),
        };
        {
            let s = sender.output_sender().clone();
            let save_action = model.save_action.clone();
            let slot = widgets.editor_slot.clone();
            widgets.nav.connect_visible_page_notify(move |nav| {
                let open = nav
                    .visible_page()
                    .and_then(|p| p.tag())
                    .is_some_and(|t| t == "editor");
                let form = open && save_action.borrow().is_some();
                if !open {
                    *save_action.borrow_mut() = None;
                    slot.set_child(None::<&gtk::Widget>);
                }
                let _ = s.send(ContactSourcesOutput::PageOpen { open, form });
            });
        }
        // Read each time the page is shown: an account may have been added
        // in GNOME Settings, or a sign-in gone through, since.
        {
            let (s, out) = (
                sender.input_sender().clone(),
                sender.output_sender().clone(),
            );
            root.connect_map(move |_| {
                let _ = s.send(ContactSourcesInput::Load);
                let _ = out.send(ContactSourcesOutput::LoadBooks);
            });
        }
        // HYLKI_SHOWCASE_EDIT_CONTACTS=<index>|add|add<kind> opens that
        // source's page or the add page (on a kind), for a capture (demo
        // only), once the page has read its sources.
        if let Ok(what) = std::env::var("HYLKI_SHOWCASE_EDIT_CONTACTS") {
            if std::env::var_os("HYLKI_DEMO").is_some() {
                let s = sender.input_sender().clone();
                gtk::glib::timeout_add_seconds_local_once(7, move || {
                    if let Some(kind) = what.strip_prefix("add") {
                        let _ = s.send(ContactSourcesInput::Add);
                        if let Ok(k) = kind.parse() {
                            let _ = s.send(ContactSourcesInput::AddKind(k));
                        }
                    } else {
                        let _ = s.send(ContactSourcesInput::Open(what.parse().unwrap_or(0)));
                    }
                });
            }
        }
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            ContactSourcesInput::Load => {
                let s = sender.input_sender().clone();
                std::thread::spawn(move || {
                    let (sources, eds) = sources::list_with_eds();
                    let _ = s.send(ContactSourcesInput::Loaded { sources, eds });
                });
            }
            ContactSourcesInput::Loaded { sources, eds } => {
                self.sources = sources;
                // Every kind that can be added is kept by EDS.
                self.add_btn.set_sensitive(eds);
                self.no_eds.set_visible(!eds);
                self.rebuild(&sender);
                // An open page about a source follows it, wherever it is
                // listed now, unless it holds a form being typed in.
                let at = match &self.editing {
                    Some(Editing::Source(id)) => self.sources.iter().position(|s| s.id == *id),
                    _ => None,
                };
                let typing = self.save_action.borrow().is_some();
                if let (Some(i), false) = (at, typing) {
                    self.open_source(i, &sender);
                }
            }
            ContactSourcesInput::SetBooks(books) => {
                self.books = books;
                let labels: Vec<&str> = self.books.iter().map(|b| b.name.as_str()).collect();
                self.book_guard.set(true);
                self.book_row
                    .set_model(Some(&gtk::StringList::new(&labels)));
                self.book_row.set_selected(0);
                self.book_row.set_sensitive(self.books.len() > 1);
                self.book_guard.set(false);
            }
            ContactSourcesInput::SetLocalCount(n) => self.local_count = n,
            ContactSourcesInput::SetPlacement(placement) => {
                let idx = crate::ui::preferences::contacts_placement_index(placement);
                if self.placement_row.selected() != idx {
                    self.placement_guard.set(true);
                    self.placement_row.set_selected(idx);
                    self.placement_guard.set(false);
                }
            }
            ContactSourcesInput::ChangeBook(i) => {
                if self.book_guard.get() {
                    return;
                }
                if let Some(b) = self.books.get(i as usize) {
                    let _ = sender.output(ContactSourcesOutput::SetContactBook(b.uid.clone()));
                }
            }
            ContactSourcesInput::Open(i) => self.open_source(i, &sender),
            ContactSourcesInput::Add => self.open_add(0, &sender),
            ContactSourcesInput::AddKind(k) => {
                if self.editing == Some(Editing::Add) {
                    self.open_add(k, &sender);
                }
            }
            ContactSourcesInput::Toggle { index, on } => {
                let Some(source) = self.sources.get_mut(index) else {
                    return;
                };
                if source.on == on {
                    return;
                }
                source.on = on;
                let source = source.clone();
                let s = sender.input_sender().clone();
                std::thread::spawn(move || {
                    let error = sources::switch_source(&source, on).err();
                    let _ = s.send(ContactSourcesInput::Done {
                        error,
                        found: None,
                        checked: None,
                    });
                });
            }
            ContactSourcesInput::ToggleBook { book, on } => {
                let s = sender.input_sender().clone();
                std::thread::spawn(move || {
                    let error = sources::set_off(&book, !on).err();
                    let _ = s.send(ContactSourcesInput::Done {
                        error,
                        found: None,
                        checked: None,
                    });
                });
            }
            ContactSourcesInput::Rename { id, name } => {
                let error = sources::set_name(&id, name.as_deref()).err();
                sender.input(ContactSourcesInput::Done {
                    error,
                    found: None,
                    checked: None,
                });
            }
            ContactSourcesInput::SaveClicked => {
                let action = self.save_action.borrow().clone();
                if let Some(f) = action {
                    if f() {
                        *self.save_action.borrow_mut() = None;
                        self.close_editor();
                    }
                }
            }
            ContactSourcesInput::CloseEditor => {
                *self.save_action.borrow_mut() = None;
                self.close_editor();
            }
            ContactSourcesInput::RemoveCurrent => {
                if let Some(Editing::Source(id)) = &self.editing {
                    if let Some(i) = self.sources.iter().position(|s| s.id == *id) {
                        sender.input(ContactSourcesInput::ConfirmRemove(i));
                    }
                }
            }
            ContactSourcesInput::ConfirmRemove(i) => self.confirm_remove(i, &sender),
            ContactSourcesInput::Remove(i) => self.remove_source(i, &sender),
            ContactSourcesInput::SaveCardDav { account, password } => {
                self.toast(&i18n_f(
                    "Looking for address books on {name}…",
                    &[("name", &account.title())],
                ));
                let s = sender.input_sender().clone();
                std::thread::spawn(move || {
                    let password = (!password.is_empty()).then_some(password);
                    let saved = carddav::save(&account, password.as_deref());
                    report_carddav(&s, saved, password.is_some());
                });
            }
            ContactSourcesInput::SaveLdap { dir, password } => save_ldap(dir, password, &sender),
            ContactSourcesInput::Done {
                error,
                found,
                checked,
            } => self.report_done(error, found, checked, &sender),
            ContactSourcesInput::ImportLocal => {
                let _ = sender.output(ContactSourcesOutput::ImportLocal);
            }
            ContactSourcesInput::ExportLocal => {
                let _ = sender.output(ContactSourcesOutput::ExportLocal);
            }
            ContactSourcesInput::DeleteAllLocal => {
                let _ = sender.output(ContactSourcesOutput::DeleteAllLocal);
            }
        }
    }
}
