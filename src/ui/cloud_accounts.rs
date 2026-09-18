//! Settings → Cloud Storage (#144): the Nextcloud, Dropbox and Seafile
//! accounts that "Upload to cloud" in the composer can put files on. Each
//! row is an account; the editor is a dialog whose fields follow the kind,
//! with a connection check (a browser sign-in, for Dropbox).

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use crate::cloud::{self, CloudAccount, CloudKind};
use crate::i18n::{i18n, i18n_f};

/// How a kind is named in the editor and the list.
pub struct CloudAccounts {
    accounts: Vec<CloudAccount>,
    /// Keyring keys of the servers already asked which product they are
    /// (accounts from before the picker told them apart), so a list
    /// rebuild does not ask again.
    probed: std::collections::HashSet<String>,
    list: gtk::ListBox,
    toasts: Option<adw::ToastOverlay>,
    nav: Option<adw::NavigationView>,
    editor_page: Option<adw::NavigationPage>,
    editor_slot: Option<adw::Bin>,
    /// The editor header's Remove, shown while an existing account is up.
    remove_btn: Option<gtk::Button>,
    /// Which account the editor shows (none for a new one).
    editing: Option<usize>,
    /// What the editor's Save does, while one is up: reads the form and
    /// sends `Save` (or starts the sign-in that will); answers whether it
    /// accepted the form.
    save_action: Rc<RefCell<Option<Rc<dyn Fn() -> bool>>>>,
}

#[derive(Debug)]
pub enum CloudAccountsInput {
    Add,
    /// The editor for a new account of that kind.
    AddOf(CloudKind),
    Edit(usize),
    /// The editor header's Remove: ask about the account being edited.
    RemoveCurrent,
    /// Ask before removing that account.
    ConfirmRemove(usize),
    /// A row's switch: keep the account, but offer it (or not) in the
    /// composer.
    ToggleEnabled { index: usize, enabled: bool },
    Remove(usize),
    /// The editor page's Save button.
    SaveClicked,
    /// Leave the editor without saving (the settings window asks to, when
    /// the user moves on).
    CloseEditor,
    /// The editor's Save: `index` is the row being replaced, or none for a
    /// new account. The password is stored only when given.
    Save { index: Option<usize>, account: CloudAccount, password: String },
    /// A sign-in the editor finished after closing went wrong.
    Failed(String),
    /// A server said which product it is (`cloud::detect_product`), for
    /// the account with that keyring key.
    ProductDetected { key: String, product: String },
}

#[derive(Debug)]
pub enum CloudAccountsOutput {
    /// The editor page is up (or gone): the settings window hides its
    /// shared header meanwhile and asks before moving on.
    EditorOpen(bool),
}

#[relm4::component(pub)]
impl SimpleComponent for CloudAccounts {
    type Init = ();
    type Input = CloudAccountsInput;
    type Output = CloudAccountsOutput;

    view! {
        adw::Bin {
            #[wrap(Some)]
            #[name = "nav"]
            set_child = &adw::NavigationView {
                // ---- list page ----
                add = &adw::NavigationPage {
                    set_title: &i18n("Cloud Storage"),
                    set_tag: Some("list"),

                    #[wrap(Some)]
                    set_child = &adw::ToolbarView {
                        #[wrap(Some)]
                        #[name = "toasts"]
                        set_content = &adw::ToastOverlay {
                            #[wrap(Some)]
                            set_child = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Cloud storage"),
                                    set_description: Some(&i18n("Connect your cloud storage provider to upload and share large files instead of an attachment. A cloud icon appears in the compose toolbar once a provider is added.\n\nSupported providers: Nextcloud, ownCloud, OpenCloud, OneDrive, Dropbox and Seafile.")),
                                    // Across from the heading: the services' marks
                                    // (picker order), with Add Account under them.
                                    #[wrap(Some)]
                                    set_header_suffix = &gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 20,
                                        set_valign: gtk::Align::Start,
                                        set_margin_start: 24,
                                        #[name = "brands"]
                                        gtk::Box {
                                            set_orientation: gtk::Orientation::Horizontal,
                                            set_spacing: 12,
                                            set_halign: gtk::Align::End,
                                        },
                                        gtk::Button {
                                            set_label: &i18n("Add Account…"),
                                            set_halign: gtk::Align::End,
                                            connect_clicked => CloudAccountsInput::Add,
                                        },
                                    },
                                    #[name = "list"]
                                    gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                        set_margin_top: 16,
                                        // A row opens its editor, as the Mail
                                        // Accounts list does.
                                        connect_row_activated[sender] => move |_, row| {
                                            sender.input(CloudAccountsInput::Edit(row.index() as usize));
                                        },
                                    },
                                    #[name = "empty"]
                                    gtk::Label {
                                        set_label: &i18n("No cloud accounts yet."),
                                        add_css_class: "dim-label",
                                        set_margin_top: 12,
                                    },
                                },
                            },
                        },
                    },
                },

                // ---- editor page ----
                #[name = "editor_page"]
                add = &adw::NavigationPage {
                    set_title: &i18n("Cloud Account"),
                    set_tag: Some("editor"),

                    #[wrap(Some)]
                    set_child = &adw::ToolbarView {
                        add_top_bar = &adw::HeaderBar {
                            // The window's close button lives here while the
                            // editor is up (the shared header hides).
                            set_show_end_title_buttons: true,
                            pack_end = &gtk::Button {
                                set_label: &i18n("Save"),
                                add_css_class: "suggested-action",
                                connect_clicked => CloudAccountsInput::SaveClicked,
                            },
                            // Left of Save, only while editing an existing
                            // account; asks before removing.
                            #[name = "remove_btn"]
                            pack_end = &gtk::Button {
                                set_label: &i18n("Remove"),
                                add_css_class: "destructive-action",
                                set_visible: false,
                                connect_clicked => CloudAccountsInput::RemoveCurrent,
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

    fn init(_init: (), root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let widgets = view_output!();
        for service in cloud::SERVICES.iter() {
            let mark = crate::brand::image(service.brand(), 22);
            mark.set_tooltip_text(Some(service.name));
            widgets.brands.append(&mark);
        }
        let mut model = CloudAccounts {
            accounts: cloud::load_accounts(),
            probed: std::collections::HashSet::new(),
            list: widgets.list.clone(),
            toasts: Some(widgets.toasts.clone()),
            nav: Some(widgets.nav.clone()),
            editor_page: Some(widgets.editor_page.clone()),
            editor_slot: Some(widgets.editor_slot.clone()),
            remove_btn: Some(widgets.remove_btn.clone()),
            editing: None,
            save_action: Rc::new(RefCell::new(None)),
        };
        model.rebuild(&sender);
        widgets.empty.set_visible(model.accounts.is_empty());
        // Tell the settings window when the editor page is up, every way
        // in or out (Save, the back button, a swipe).
        {
            let s = sender.output_sender().clone();
            let save_action = model.save_action.clone();
            let slot = widgets.editor_slot.clone();
            widgets.nav.connect_visible_page_notify(move |nav| {
                let editor = nav.visible_page().and_then(|p| p.tag()).is_some_and(|t| t == "editor");
                if !editor {
                    // The form is done with: drop it and what Save would do.
                    *save_action.borrow_mut() = None;
                    slot.set_child(None::<&gtk::Widget>);
                }
                let _ = s.send(CloudAccountsOutput::EditorOpen(editor));
            });
        }
        // HYLKI_SHOWCASE_EDIT_CLOUD=<index> opens that account's editor
        // for a capture (demo only); "add", "add:dropbox", "add:seafile"
        // or "add:onedrive" opens the Add page on that kind.
        if let Ok(what) = std::env::var("HYLKI_SHOWCASE_EDIT_CLOUD") {
            if std::env::var_os("HYLKI_DEMO").is_some() {
                let s = sender.input_sender().clone();
                gtk::glib::timeout_add_seconds_local_once(2, move || {
                    let _ = s.send(match what.as_str() {
                        "add" | "add:nextcloud" => CloudAccountsInput::AddOf(CloudKind::Nextcloud),
                        "add:dropbox" => CloudAccountsInput::AddOf(CloudKind::Dropbox),
                        "add:seafile" => CloudAccountsInput::AddOf(CloudKind::Seafile),
                        "add:onedrive" => CloudAccountsInput::AddOf(CloudKind::OneDrive),
                        i => CloudAccountsInput::Edit(i.parse().unwrap_or(0)),
                    });
                });
            }
        }
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            CloudAccountsInput::Add => sender.input(CloudAccountsInput::AddOf(CloudKind::Nextcloud)),
            CloudAccountsInput::AddOf(kind) => {
                let mut a = CloudAccount::empty();
                a.kind = kind;
                self.open_editor(None, a, &sender);
            }
            CloudAccountsInput::Edit(i) => {
                if let Some(a) = self.accounts.get(i).cloned() {
                    self.open_editor(Some(i), a, &sender);
                }
            }
            CloudAccountsInput::ToggleEnabled { index, enabled } => {
                if let Some(a) = self.accounts.get_mut(index) {
                    if a.enabled != enabled {
                        a.enabled = enabled;
                        cloud::save_accounts(&self.accounts);
                    }
                }
            }
            CloudAccountsInput::RemoveCurrent => {
                if let Some(i) = self.editing {
                    sender.input(CloudAccountsInput::ConfirmRemove(i));
                }
            }
            CloudAccountsInput::ConfirmRemove(i) => {
                let Some(a) = self.accounts.get(i) else { return };
                let name = if a.name.trim().is_empty() { a.where_shown() } else { a.name.clone() };
                let parent = relm4::main_application().active_window();
                let dialog = adw::MessageDialog::new(
                    parent.as_ref(),
                    Some(&i18n_f("Remove {name}?", &[("name", &name)])),
                    Some(&i18n("Hylki forgets the account and its sign-in. Files already uploaded, and the links in messages you sent, stay where they are.")),
                );
                dialog.add_response("cancel", &i18n("Cancel"));
                dialog.add_response("remove", &i18n("Remove"));
                dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                let s = sender.input_sender().clone();
                dialog.connect_response(None, move |_, response| {
                    if response == "remove" {
                        let _ = s.send(CloudAccountsInput::Remove(i));
                    }
                });
                dialog.present();
            }
            CloudAccountsInput::Remove(i) => {
                if i < self.accounts.len() {
                    if self.editing == Some(i) {
                        self.editing = None;
                        self.close_editor();
                    }
                    let a = self.accounts.remove(i);
                    crate::config::delete_cloud_password(&a.key());
                    cloud::save_accounts(&self.accounts);
                    self.rebuild(&sender);
                    self.toast(&i18n_f("Removed {name}", &[("name", &a.name)]));
                }
            }
            CloudAccountsInput::SaveClicked => {
                let action = self.save_action.borrow().clone();
                if let Some(f) = action {
                    if f() {
                        self.close_editor();
                    }
                }
            }
            CloudAccountsInput::CloseEditor => self.close_editor(),
            CloudAccountsInput::Failed(e) => self.toast(&e),
            CloudAccountsInput::ProductDetected { key, product } => {
                let mut changed = false;
                for a in self.accounts.iter_mut().filter(|a| a.key() == key && a.product.is_empty()) {
                    a.product = product.clone();
                    changed = true;
                }
                if changed {
                    cloud::save_accounts(&self.accounts);
                    self.rebuild(&sender);
                }
            }
            CloudAccountsInput::Save { index, account, password } => {
                if !password.is_empty() && account.has_secret() {
                    if let Err(e) = crate::config::store_cloud_password(&account.key(), &password) {
                        self.toast(&i18n_f("Could not store the sign-in in the keyring: {e}", &[("e", &e.to_string())]));
                    }
                }
                match index {
                    Some(i) if i < self.accounts.len() => {
                        // A changed server or user name moves the keyring
                        // entry: drop the old one.
                        if self.accounts[i].key() != account.key() {
                            crate::config::delete_cloud_password(&self.accounts[i].key());
                        }
                        self.accounts[i] = account;
                    }
                    _ => self.accounts.push(account),
                }
                cloud::save_accounts(&self.accounts);
                self.rebuild(&sender);
            }
        }
    }
}

impl CloudAccounts {
    fn toast(&self, text: &str) {
        if let Some(t) = &self.toasts {
            t.add_toast(adw::Toast::new(text));
        }
    }

    /// Slide the editor page in with the form for `account`.
    fn open_editor(&mut self, index: Option<usize>, account: CloudAccount, sender: &ComponentSender<Self>) {
        let (Some(nav), Some(page), Some(slot)) = (&self.nav, &self.editor_page, &self.editor_slot) else { return };
        page.set_title(&if index.is_some() { i18n("Edit Cloud Account") } else { i18n("Add Cloud Account") });
        self.editing = index;
        if let Some(b) = &self.remove_btn {
            b.set_visible(index.is_some());
        }
        let (form, save) = build_editor(index, account, sender.input_sender().clone());
        slot.set_child(Some(&form));
        *self.save_action.borrow_mut() = Some(save);
        if nav.visible_page().and_then(|p| p.tag()).is_some_and(|t| t == "editor") {
            return;
        }
        nav.push_by_tag("editor");
    }

    fn close_editor(&self) {
        if let Some(nav) = &self.nav {
            if nav.visible_page().and_then(|p| p.tag()).is_some_and(|t| t == "editor") {
                nav.pop();
            }
        }
    }

    fn rebuild(&mut self, sender: &ComponentSender<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for (i, a) in self.accounts.iter().enumerate() {
            // The same card as a Mail Accounts row: mark, name over
            // details, then a chevron; the row itself opens the editor.
            let row = gtk::ListBoxRow::new();
            row.set_activatable(true);
            let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            hbox.add_css_class("account-list-row");
            hbox.append(&crate::brand::image(a.brand(), 28));
            let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
            vbox.set_hexpand(true);
            vbox.set_valign(gtk::Align::Center);
            let title = gtk::Label::new(Some(&if a.name.trim().is_empty() { a.where_shown() } else { a.name.clone() }));
            title.set_halign(gtk::Align::Start);
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            title.add_css_class("account-name");
            let mut sub = format!("{} · {}", a.where_shown(), a.user);
            if a.expire_days > 0 {
                sub.push_str(&format!(" · {}", i18n_f("links expire after {n} days", &[("n", &a.expire_days.to_string())])));
            }
            if a.password {
                sub.push_str(&format!(" · {}", i18n("password-protected")));
            }
            let subtitle = gtk::Label::new(Some(&sub));
            subtitle.set_halign(gtk::Align::Start);
            subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
            subtitle.add_css_class("account-email");
            vbox.append(&title);
            vbox.append(&subtitle);
            hbox.append(&vbox);
            // A server from before the picker told Nextcloud, ownCloud and
            // OpenCloud apart: ask it once, in the background, and fill in
            // its mark when it answers.
            if a.kind == CloudKind::Nextcloud && a.product.is_empty() && self.probed.insert(a.key()) {
                let (key, base) = (a.key(), a.base());
                let s = sender.input_sender().clone();
                std::thread::spawn(move || {
                    if let Some(product) = cloud::detect_product(&base) {
                        let _ = s.send(CloudAccountsInput::ProductDetected { key, product });
                    }
                });
            }
            // On/off without removing, as a mail account's row has.
            let toggle = gtk::Switch::new();
            toggle.set_valign(gtk::Align::Center);
            toggle.set_tooltip_text(Some(&i18n("Offer this account in the composer")));
            toggle.set_active(a.enabled);
            let s = sender.input_sender().clone();
            toggle.connect_state_set(move |_, state| {
                let _ = s.send(CloudAccountsInput::ToggleEnabled { index: i, enabled: state });
                gtk::glib::Propagation::Proceed
            });
            hbox.append(&toggle);
            let next = gtk::Image::from_icon_name("co.hyprlab.Hylki-go-next-symbolic");
            next.add_css_class("dim-label");
            hbox.append(&next);
            row.set_child(Some(&hbox));
            self.list.append(&row);
        }
        self.list.set_visible(!self.accounts.is_empty());
        if let Some(next) = self.list.next_sibling() {
            next.set_visible(self.accounts.is_empty());
        }
    }
}

/// The account editor's form: a kind, the fields that kind needs, a
/// connection check that signs in with what is typed (a browser sign-in,
/// for Dropbox). Answers with the form and what the page's Save button
/// does with it.
fn build_editor(
    index: Option<usize>,
    account: CloudAccount,
    sender: relm4::Sender<CloudAccountsInput>,
) -> (gtk::Widget, Rc<dyn Fn() -> bool>) {
    let group = adw::PreferencesGroup::new();
    group.set_title(&i18n("Account"));
    let names: Vec<&str> = cloud::SERVICES.iter().map(|s| s.name).collect();
    let kind = adw::ComboRow::new();
    kind.set_title(&i18n("Service"));
    kind.set_model(Some(&gtk::StringList::new(&names)));
    // Each entry with the service's mark before its name, in the row and
    // in the list that drops down.
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            let bx = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            let label = gtk::Label::new(None);
            label.set_xalign(0.0);
            bx.append(&gtk::Image::new());
            bx.append(&label);
            item.set_child(Some(&bx));
        }
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        // By name, not position: the row's own selected-value slot is a
        // list item with no position.
        let name = item.item().and_downcast::<gtk::StringObject>().map(|s| s.string().to_string()).unwrap_or_default();
        let Some(service) = cloud::SERVICES.iter().find(|s| s.name == name) else { return };
        let Some(bx) = item.child().and_downcast::<gtk::Box>() else { return };
        // (image, label): swap the image for the service's mark, name the label.
        let Some(old) = bx.first_child() else { return };
        let label = old.next_sibling().and_downcast::<gtk::Label>();
        bx.remove(&old);
        bx.prepend(&crate::brand::image(service.brand(), 20));
        if let Some(label) = label {
            label.set_label(service.name);
        }
    });
    kind.set_factory(Some(&factory));
    kind.set_selected(account.service_index().unwrap_or(0) as u32);
    // The service's mark over the form, following the picker.
    let header_mark = gtk::Image::new();
    header_mark.set_pixel_size(56);
    header_mark.set_halign(gtk::Align::Center);
    header_mark.set_margin_bottom(18);
    let set_header_mark = {
        let header_mark = header_mark.clone();
        move |id: &str| {
            match crate::brand::texture(id, 112) {
                Some(t) => header_mark.set_paintable(Some(&t)),
                None => header_mark.set_icon_name(Some("co.hyprlab.Hylki-cloud-symbolic")),
            }
        }
    };
    set_header_mark(account.brand());
    // The kind is chosen when the account is made; afterwards the
    // sign-in and the keyring entry belong to it.
    kind.set_sensitive(index.is_none());
    // What the account is called in the list; optional, the server or
    // service stands in when empty.
    let name = adw::EntryRow::new();
    name.set_title(&i18n("Nickname (optional)"));
    name.set_text(&account.name);
    let url = adw::EntryRow::new();
    url.set_title(&i18n("Server URL"));
    url.set_text(&account.url);
    let user = adw::EntryRow::new();
    user.set_text(&account.user);
    let pass = adw::PasswordEntryRow::new();
    let code = adw::EntryRow::new();
    code.set_title(&i18n("Two-step verification code, if the account uses it"));
    code.set_input_purpose(gtk::InputPurpose::Digits);
    let seafile_hint = gtk::Label::new(Some(&i18n(
        "Sign in with your Seafile password. If the account uses two-step verification, also enter the current code from your authenticator app: Hylki turns it into an API token once and keeps that instead of the password. A token obtained another way can be pasted in the password field.",
    )));
    seafile_hint.set_wrap(true);
    seafile_hint.set_xalign(0.0);
    seafile_hint.add_css_class("dim-label");
    seafile_hint.add_css_class("caption");
    seafile_hint.set_margin_top(6);
    seafile_hint.set_margin_start(12);
    seafile_hint.set_margin_end(12);
    // OneDrive: which GNOME Online Accounts account.
    let goa_accounts = crate::goa::list_files_accounts();
    let goa_row = adw::ComboRow::new();
    goa_row.set_title(&i18n("Online account"));
    let goa_hint = gtk::Label::new(None);
    goa_hint.set_wrap(true);
    goa_hint.set_xalign(0.0);
    goa_hint.add_css_class("dim-label");
    goa_hint.add_css_class("caption");
    goa_hint.set_margin_top(6);
    goa_hint.set_margin_start(12);
    goa_hint.set_margin_end(12);
    let app_key = adw::EntryRow::new();
    app_key.set_title(&i18n("Dropbox app key"));
    app_key.set_text(&account.client_id);
    let app_key_hint = gtk::Label::new(None);
    app_key_hint.set_wrap(true);
    app_key_hint.set_xalign(0.0);
    app_key_hint.add_css_class("dim-label");
    app_key_hint.add_css_class("caption");
    app_key_hint.set_margin_top(6);
    app_key_hint.set_margin_start(12);
    app_key_hint.set_margin_end(12);
    let library = adw::EntryRow::new();
    library.set_title(&i18n("Library"));
    library.set_text(&account.library);
    let folder = adw::EntryRow::new();
    folder.set_title(&i18n("Upload folder"));
    folder.set_text(&account.folder);
    let cloudflare = adw::SwitchRow::new();
    cloudflare.set_title(&i18n("Server is behind Cloudflare"));
    cloudflare.set_subtitle(&i18n(
        "Cloudflare's proxy refuses a request over 100 MB, so bigger files are uploaded in 90 MB pieces the server puts back together. For a self-hosted server on a Cloudflare domain or tunnel.",
    ));
    cloudflare.set_active(account.cloudflare);
    let expire = adw::SpinRow::with_range(0.0, 365.0, 1.0);
    expire.set_title(&i18n("Links expire after"));
    expire.set_subtitle(&i18n("Days; 0 keeps the link indefinitely"));
    expire.set_value(account.expire_days as f64);
    let protect = adw::SwitchRow::new();
    protect.set_title(&i18n("Protect links with a password"));
    protect.set_subtitle(&i18n("A download password is made for each file and shown to you, to pass on separately"));
    protect.set_active(account.password);
    group.add(&kind);
    group.add(&name);
    group.add(&url);
    group.add(&user);
    group.add(&pass);
    group.add(&code);
    group.add(&goa_row);
    group.add(&app_key);
    group.add(&library);
    group.add(&folder);
    group.add(&cloudflare);
    // The link terms: the defaults for this account, changeable for each
    // upload in the composer's upload dialog.
    let defaults = adw::PreferencesGroup::new();
    defaults.set_title(&i18n("Link defaults"));
    defaults.set_description(Some(&i18n(
        "How share links from this account are made unless you choose otherwise for an email: the upload dialog in the composer shows these values and lets you change them for that upload alone.",
    )));
    defaults.set_margin_top(28);
    defaults.add(&expire);
    defaults.add(&protect);


    let check_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    check_box.set_margin_top(8);
    let check = gtk::Button::with_label(&i18n("Check Connection"));
    check.set_valign(gtk::Align::Start);
    let status = gtk::Label::new(None);
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.set_hexpand(true);
    status.add_css_class("dim-label");
    check_box.append(&check);
    check_box.append(&status);

    let bx = gtk::Box::new(gtk::Orientation::Vertical, 0);
    bx.append(&header_mark);
    bx.append(&group);
    bx.append(&app_key_hint);
    bx.append(&goa_hint);
    bx.append(&seafile_hint);
    bx.append(&check_box);
    bx.append(&defaults);

    // A sign-in that made a secret of its own leaves it here until Save:
    // the Dropbox refresh token (with the account's e-mail and name), or
    // the Seafile API token from a two-step sign-in.
    let dropbox_login: Rc<RefCell<Option<(String, String, String)>>> = Rc::new(RefCell::new(None));
    // What the connection check found the service allows on a link
    // (OneDrive plans differ); saved with the account.
    let link_terms: Rc<RefCell<Option<Option<cloud::LinkTerms>>>> = Rc::new(RefCell::new(None));

    let selected_service = {
        let kind = kind.clone();
        move || cloud::SERVICES.get(kind.selected() as usize).copied().unwrap_or(cloud::SERVICES[0])
    };
    let selected_kind = {
        let selected_service = selected_service.clone();
        move || selected_service().kind
    };

    // The GOA accounts the picker currently lists (those of the kind's
    // provider), in row order.
    let goa_listed: Rc<RefCell<Vec<crate::goa::GoaFilesAccount>>> = Rc::new(RefCell::new(Vec::new()));
    let fill_goa = {
        let (goa_row, goa_hint, goa_accounts, goa_listed) =
            (goa_row.clone(), goa_hint.clone(), goa_accounts.clone(), goa_listed.clone());
        let existing_goa = account.goa_id.clone();
        move |k: CloudKind| {
            let Some(provider) = k.goa_provider() else { return };
            let listed: Vec<crate::goa::GoaFilesAccount> =
                goa_accounts.iter().filter(|a| a.provider_type == provider).cloned().collect();
            let labels: Vec<String> = listed
                .iter()
                .map(|a| if a.files_enabled { a.email.clone() } else { i18n_f("{email} (Files is off)", &[("email", &a.email)]) })
                .collect();
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            goa_row.set_model(Some(&gtk::StringList::new(&refs)));
            if let Some(i) = listed.iter().position(|a| a.id == existing_goa) {
                goa_row.set_selected(i as u32);
            }
            goa_row.set_sensitive(!listed.is_empty());
            let mut hint = if listed.is_empty() {
                i18n("No Microsoft 365 account in GNOME Online Accounts yet. Add one under Settings, Online Accounts; OneDrive then signs in through it, and nothing more is needed here.")
            } else {
                i18n("OneDrive signs in through the GNOME Online Accounts account chosen above; there is no password to enter. An account marked Files is off still works here, but you may want to turn Files on for it under Settings, Online Accounts.")
            };
            hint.push_str("\n\n");
            hint.push_str(&i18n(
                "Uploads and plain share links work with any OneDrive. Link expiry and download passwords are a Microsoft 365 subscription or OneDrive for Business feature: on a free personal OneDrive the link is refused when either is set, so keep Links expire after at 0 and the password switch off there.",
            ));
            goa_hint.set_label(&hint);
            *goa_listed.borrow_mut() = listed;
        }
    };

    // The expiry and password rows follow what the service allows.
    let apply_terms = {
        let (expire, protect) = (expire.clone(), protect.clone());
        move |a: &CloudAccount| {
            expire.set_sensitive(a.expiry_allowed());
            if !a.expiry_allowed() {
                expire.set_value(0.0);
                expire.set_subtitle(&a.link_note);
            }
            protect.set_sensitive(a.password_allowed());
            if !a.password_allowed() {
                protect.set_active(false);
                protect.set_subtitle(&a.link_note);
            }
        }
    };
    apply_terms(&account);

    // The fields each kind wants.
    let apply_kind = {
        let (url, user, pass, code, seafile_hint, app_key, app_key_hint, library, check, protect, expire, goa_row, goa_hint) = (
            url.clone(),
            user.clone(),
            pass.clone(),
            code.clone(),
            seafile_hint.clone(),
            app_key.clone(),
            app_key_hint.clone(),
            library.clone(),
            check.clone(),
            protect.clone(),
            expire.clone(),
            goa_row.clone(),
            goa_hint.clone(),
        );
        let cloudflare = cloudflare.clone();
        let editing = index.is_some();
        let fill_goa = fill_goa.clone();
        let apply_terms = apply_terms.clone();
        let known = account.clone();
        move |service: cloud::Service| {
            let k = service.kind;
            let dropbox = k == CloudKind::Dropbox;
            let goa = k.via_goa();
            url.set_visible(!dropbox && !goa);
            user.set_visible(!dropbox && !goa);
            pass.set_visible(!dropbox && !goa);
            app_key.set_visible(dropbox);
            app_key_hint.set_visible(dropbox);
            goa_row.set_visible(goa);
            goa_hint.set_visible(goa);
            library.set_visible(k == CloudKind::Seafile);
            code.set_visible(k == CloudKind::Seafile);
            seafile_hint.set_visible(k == CloudKind::Seafile);
            // Self-hosted kinds only: Dropbox and OneDrive are not behind
            // anyone's proxy.
            cloudflare.set_visible(!dropbox && !goa);
            if goa {
                fill_goa(k);
                check.set_label(&i18n("Check Connection"));
                expire.set_subtitle(&i18n("Days; 0 keeps the link indefinitely. Needs Microsoft 365 or OneDrive for Business"));
                protect.set_subtitle(&i18n("A download password is made for each file and shown to you, to pass on separately. Needs Microsoft 365 or OneDrive for Business"));
                // A new OneDrive account starts with both off, so a free
                // personal OneDrive works as it is.
                if !editing {
                    expire.set_value(0.0);
                    protect.set_active(false);
                }
                // What the plan was found to allow, over the generic words.
                if known.kind == k {
                    apply_terms(&known);
                }
                return;
            }
            expire.set_subtitle(&i18n("Days; 0 keeps the link indefinitely"));
            match k {
                CloudKind::Nextcloud => {
                    user.set_title(&i18n("User name"));
                    pass.set_title(&if editing { i18n("App password (leave empty to keep)") } else { i18n("App password") });
                    protect.set_subtitle(&i18n("A download password is made for each file and shown to you, to pass on separately"));
                    check.set_label(&i18n("Check Connection"));
                }
                CloudKind::Seafile => {
                    user.set_title(&i18n("E-mail"));
                    pass.set_title(&if editing { i18n("Password (leave empty to keep)") } else { i18n("Password") });
                    protect.set_subtitle(&i18n("A download password is made for each file and shown to you, to pass on separately"));
                    check.set_label(&i18n("Check Connection"));
                }
                // Handled above, before the return.
                CloudKind::OneDrive => {}
                CloudKind::Dropbox => {
                    protect.set_subtitle(&i18n("A download password is made for each file and shown to you, to pass on separately. Dropbox allows link passwords and expiry dates on paid plans only."));
                    check.set_label(&i18n("Connect with Dropbox…"));
                    app_key_hint.set_label(&i18n_f(
                        "Dropbox only lets an app sign in when it is registered, so make one for yourself (it takes a minute and stays private):\n\
                         1. Open dropbox.com/developers/apps, signed in to your Dropbox, and press Create app.\n\
                         2. Choose Scoped access, then App folder (Hylki sees only its own folder under Apps) or Full Dropbox (uploads go to the folder named above).\n\
                         3. Give the app a name that no one else has used, such as Hylki for your name, and press Create app.\n\
                         4. On the Permissions tab, tick account_info.read, files.content.write and sharing.write, then press Submit.\n\
                         5. On the Settings tab, under OAuth 2, add the redirect URI {uri} and press Add.\n\
                         6. Copy the App key from the top of the Settings tab into the field above.\n\
                         The app may stay in Development status; that allows your own account. Leave the field empty to use the app key this build was made with, when it has one.",
                        &[("uri", &format!("http://localhost:{}/", crate::oauth::DROPBOX_REDIRECT_PORT))],
                    ));
                }
            }
        }
    };
    apply_kind(cloud::SERVICES[account.service_index().unwrap_or(0)]);
    {
        let apply_kind = apply_kind.clone();
        let status = status.clone();
        let selected_service = selected_service.clone();
        kind.connect_selected_notify(move |_| {
            let service = selected_service();
            apply_kind(service);
            set_header_mark(service.brand());
            status.set_label("");
        });
    }

    let read = {
        let (name, url, user, app_key, library, folder, expire, protect) = (
            name.clone(),
            url.clone(),
            user.clone(),
            app_key.clone(),
            library.clone(),
            folder.clone(),
            expire.clone(),
            protect.clone(),
        );
        let cloudflare = cloudflare.clone();
        let selected_service = selected_service.clone();
        let dropbox_login = dropbox_login.clone();
        let existing = account.clone();
        let (goa_row, goa_listed) = (goa_row.clone(), goa_listed.clone());
        let link_terms = link_terms.clone();
        move || {
            let service = selected_service();
            let kind = service.kind;
            let goa = if kind.via_goa() {
                goa_listed.borrow().get(goa_row.selected() as usize).map(|a| (a.id.clone(), a.email.clone()))
            } else {
                None
            };
            let user = match (kind, dropbox_login.borrow().as_ref()) {
                (CloudKind::Dropbox, Some((_, email, _))) => email.clone(),
                (CloudKind::Dropbox, None) if existing.kind == CloudKind::Dropbox => existing.user.clone(),
                (CloudKind::Dropbox, None) => String::new(),
                (CloudKind::OneDrive, _) => goa.as_ref().map(|(_, e)| e.clone()).unwrap_or_default(),
                _ => user.text().trim().to_string(),
            };
            let mut a = CloudAccount {
                name: name.text().trim().to_string(),
                kind,
                product: service.product.to_string(),
                enabled: existing.enabled,
                url: url.text().trim().to_string(),
                user,
                folder: folder.text().trim().to_string(),
                library: library.text().trim().to_string(),
                client_id: app_key.text().trim().to_string(),
                goa_id: goa.map(|(id, _)| id).unwrap_or_default(),
                expire_days: expire.value() as u32,
                password: protect.is_active(),
                link_expiry: None,
                link_password: None,
                link_note: String::new(),
                cloudflare: !kind.via_goa() && kind != CloudKind::Dropbox && cloudflare.is_active(),
            };
            let probed = link_terms.borrow().clone();
            match probed {
                Some(t) => a.set_link_terms(t.as_ref()),
                // Not probed this time: keep what the account already
                // knew, if it is still the same kind of account.
                None if existing.kind == kind => {
                    a.link_expiry = existing.link_expiry;
                    a.link_password = existing.link_password;
                    a.link_note = existing.link_note.clone();
                    if !a.expiry_allowed() {
                        a.expire_days = 0;
                    }
                    if !a.password_allowed() {
                        a.password = false;
                    }
                }
                None => {}
            }
            a
        }
    };

    if account.kind == CloudKind::Dropbox && !account.user.is_empty() {
        status.set_label(&i18n_f("Connected as {who}.", &[("who", &account.user)]));
    }

    {
        let read = read.clone();
        let pass = pass.clone();
        let status = status.clone();
        let existing = account.clone();
        let dropbox_login = dropbox_login.clone();
        let name = name.clone();
        let code = code.clone();
        let selected_kind = selected_kind.clone();
        let link_terms = link_terms.clone();
        let apply_terms = apply_terms.clone();
        check.connect_clicked(move |b| {
            let a = read();
            enum Job {
                Verify(CloudAccount, String),
                Dropbox(CloudAccount),
                SeafileCode(CloudAccount, String, String),
            }
            type Outcome = (String, Option<(String, String, String)>, Option<Option<cloud::LinkTerms>>);
            let job = if a.kind.via_goa() {
                if a.goa_id.is_empty() {
                    status.set_label(&i18n("Choose an online account first."));
                    return;
                }
                status.set_label(&i18n("Signing in…"));
                Job::Verify(a, String::new())
            } else if a.kind == CloudKind::Dropbox {
                if cloud::dropbox_client_id(&a).is_empty() {
                    status.set_label(&i18n("Enter the app key of a Dropbox app first."));
                    return;
                }
                status.set_label(&i18n("Waiting for the sign-in in your browser…"));
                Job::Dropbox(a)
            } else {
                let pw = match pass.text().to_string() {
                    p if !p.is_empty() => p,
                    _ => crate::config::load_cloud_password(&existing.key()).unwrap_or_default(),
                };
                if a.url.is_empty() || a.user.is_empty() || pw.is_empty() {
                    status.set_label(&if a.kind == CloudKind::Seafile {
                        i18n("Fill in the server URL, e-mail and password first.")
                    } else {
                        i18n("Fill in the server URL, user name and app password first.")
                    });
                    return;
                }
                status.set_label(&i18n("Signing in…"));
                let otp = code.text().trim().to_string();
                if a.kind == CloudKind::Seafile && !otp.is_empty() {
                    Job::SeafileCode(a, pw, otp)
                } else {
                    Job::Verify(a, pw)
                }
            };
            b.set_sensitive(false);
            let (tx, rx) = std::sync::mpsc::channel::<Result<Outcome, String>>();
            std::thread::spawn(move || {
                let r = match job {
                    Job::Verify(a, pw) => cloud::verify(&a, &pw).map(|who| {
                        let terms = cloud::probe_link_terms(&a, &pw).ok();
                        (who, None, terms)
                    }),
                    Job::Dropbox(a) => cloud::dropbox_connect(&a)
                        .map(|(refresh, email, who)| (format!("{who} ({email})"), Some((refresh, email, who)), None)),
                    Job::SeafileCode(a, pw, otp) => cloud::seafile_login_with_code(&a, &pw, &otp)
                        .map(|(token, who)| (who.clone(), Some((token, a.user.clone(), who)), None)),
                };
                let _ = tx.send(r);
            });
            let status = status.clone();
            let b = b.clone();
            let dropbox_login = dropbox_login.clone();
            let name = name.clone();
            let selected_kind_now = selected_kind.clone();
            let link_terms = link_terms.clone();
            let apply_terms = apply_terms.clone();
            let read = read.clone();
            gtk::glib::timeout_add_local(std::time::Duration::from_millis(200), move || match rx.try_recv() {
                Ok(Ok((who, login, terms))) => {
                    status.set_label(&i18n_f("Signed in as {who}.", &[("who", &who)]));
                    if let Some(t) = terms {
                        *link_terms.borrow_mut() = Some(t);
                        let a = read();
                        apply_terms(&a);
                        // Say what the plan means in practice, not only
                        // what it lacks: uploading and plain links work.
                        let terms = match (a.expiry_allowed(), a.password_allowed()) {
                            (false, false) => Some(i18n(
                                "You can upload files and share plain links with this OneDrive. Link expiry and download passwords are not available on a free personal OneDrive (they need a Microsoft 365 subscription), so both are turned off for this account.",
                            )),
                            (true, false) => Some(i18n(
                                "You can upload files, share links and set an expiry with this OneDrive. Download passwords are not available on OneDrive for Business links, so that option is turned off for this account.",
                            )),
                            (false, true) => Some(i18n(
                                "You can upload files and share links with this OneDrive, with a download password. Link expiry is not available on it, so that option is turned off for this account.",
                            )),
                            (true, true) => None,
                        };
                        if let Some(t) = terms {
                            status.set_label(&i18n_f("Signed in as {who}. {terms}", &[("who", &who), ("terms", &t)]));
                        }
                    }
                    if let Some(l) = login {
                        if name.text().trim().is_empty() && selected_kind_now() == CloudKind::Dropbox {
                            name.set_text("Dropbox");
                        }
                        *dropbox_login.borrow_mut() = Some(l);
                    }
                    b.set_sensitive(true);
                    gtk::glib::ControlFlow::Break
                }
                Ok(Err(e)) => {
                    status.set_label(&e);
                    b.set_sensitive(true);
                    gtk::glib::ControlFlow::Break
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
                Err(_) => {
                    b.set_sensitive(true);
                    gtk::glib::ControlFlow::Break
                }
            });
        });
    }

    let link_terms = link_terms.clone();
    let save: Rc<dyn Fn() -> bool> = Rc::new(move || {
        let a = read();
        let secret = if a.kind.via_goa() {
            if a.goa_id.is_empty() {
                return false;
            }
            // Not checked this time: find out what the plan allows on
            // the way, so the rows can say so next time.
            if link_terms.borrow().is_none() {
                let sender = sender.clone();
                std::thread::spawn(move || {
                    let mut a = a;
                    if let Ok(t) = cloud::probe_link_terms(&a, "") {
                        a.set_link_terms(t.as_ref());
                    }
                    let _ = sender.send(CloudAccountsInput::Save { index, account: a, password: String::new() });
                });
                return true;
            }
            String::new()
        } else if a.kind == CloudKind::Dropbox {
            // Without a sign-in there is nothing to save; a re-opened
            // account keeps its token when none was made anew.
            match dropbox_login.borrow().as_ref() {
                Some((refresh, _, _)) => refresh.clone(),
                None if !a.user.is_empty() => String::new(),
                None => return false,
            }
        } else {
            if a.url.is_empty() || a.user.is_empty() {
                return false;
            }
            let otp = code.text().trim().to_string();
            match dropbox_login.borrow().as_ref() {
                // The token a two-step sign-in made.
                Some((token, _, _)) if a.kind == CloudKind::Seafile => token.clone(),
                // A code typed but never checked: exchange it now, and
                // save once the token is here.
                _ if a.kind == CloudKind::Seafile && !otp.is_empty() && !pass.text().is_empty() => {
                    let pw = pass.text().to_string();
                    let sender = sender.clone();
                    std::thread::spawn(move || {
                        let _ = sender.send(match cloud::seafile_login_with_code(&a, &pw, &otp) {
                            Ok((token, _)) => CloudAccountsInput::Save { index, account: a, password: token },
                            Err(e) => CloudAccountsInput::Failed(e),
                        });
                    });
                    return true;
                }
                _ => pass.text().to_string(),
            }
        };
        let _ = sender.send(CloudAccountsInput::Save { index, account: a, password: secret });
        true
    });
    (bx.upcast(), save)
}
