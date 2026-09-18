//! The OpenPGP page of the settings window (#133, key management): the
//! user's own keys and everyone else's, with the actions that otherwise
//! need a terminal — generate, import, export, fetch by address, trust,
//! remove. Every action is the user's own gpg on the user's own keyring
//! (see `crate::pgp`); passphrases for a new key travel down a pipe to
//! gpg and are not kept.

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::pgp::{self, Gpg, ImportSummary, KeyInfo};

/// What the page needs at launch: the identities keys can be made for.
pub struct PgpKeysInit {
    /// (name, address) per account and alias.
    pub identities: Vec<(String, String)>,
}

pub struct PgpKeys {
    identities: Vec<(String, String)>,
    own: Vec<KeyInfo>,
    others: Vec<KeyInfo>,
    own_list: gtk::ListBox,
    others_list: gtk::ListBox,
    toasts: Option<adw::ToastOverlay>,
    /// A background action (generate, fetch) is running.
    busy: bool,
}

#[derive(Debug)]
pub enum PgpKeysInput {
    /// Re-read the keyring and rebuild both lists.
    Refresh,
    Generate,
    GenerateConfirmed { name: String, email: String, expire: String, passphrase: String },
    /// Pick a key file to import.
    Import,
    ImportBytes(Vec<u8>),
    /// Save a key's public half to a file.
    Export(String),
    Delete { fingerprint: String, secret: bool },
    DeleteConfirmed { fingerprint: String, secret: bool },
    Fetch,
    FetchConfirmed(String),
    Trust(String),
    TrustConfirmed(String),
}

#[derive(Debug)]
pub enum PgpKeysCmd {
    Generated(Result<String, String>),
    Fetched(Result<ImportSummary, String>),
}

/// The expiry choices offered when generating a key, in combo order.
const EXPIRIES: &[(&str, &str)] = &[
    (crate::i18n::i18n_noop("2 years"), "2y"),
    (crate::i18n::i18n_noop("1 year"), "1y"),
    (crate::i18n::i18n_noop("5 years"), "5y"),
    (crate::i18n::i18n_noop("Never"), "never"),
];

#[relm4::component(pub)]
impl Component for PgpKeys {
    type Init = PgpKeysInit;
    type Input = PgpKeysInput;
    type Output = ();
    type CommandOutput = PgpKeysCmd;

    view! {
        #[name = "toasts"]
        adw::ToastOverlay {
            #[wrap(Some)]
            set_child = &adw::PreferencesPage {
                add = &adw::PreferencesGroup {
                    set_title: &i18n("GnuPG"),
                    #[name = "status_row"]
                    adw::ActionRow {
                        set_title: &i18n("GnuPG"),
                        set_subtitle: &i18n("Hylki reads and writes the keyring of the GnuPG on this \
                                       computer, the same one the gpg command and other mail \
                                       programs use."),
                    },
                },

                #[name = "own_group"]
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Your keys"),
                    set_description: Some(
                        i18n("A key of your own signs what you send and opens what is encrypted \
                              to you. Send its public half to the people who should write to \
                              you encrypted.").as_str()
                    ),
                    #[wrap(Some)]
                    set_header_suffix = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 12,
                        set_valign: gtk::Align::Start,
                        set_halign: gtk::Align::End,
                        set_margin_start: 24,
                        gtk::Button {
                            set_label: &i18n("Import…"),
                            set_size_request: (130, -1),
                            connect_clicked => PgpKeysInput::Import,
                        },
                        gtk::Button {
                            set_label: &i18n("Generate…"),
                            set_size_request: (130, -1),
                            connect_clicked => PgpKeysInput::Generate,
                        },
                    },
                    #[local_ref]
                    own_list -> gtk::ListBox {
                        add_css_class: "boxed-list",
                        set_selection_mode: gtk::SelectionMode::None,
                    },
                },

                #[name = "others_group"]
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Other people's keys"),
                    set_description: Some(
                        i18n("The keys you encrypt to and check signatures with. A key is \
                              trusted once you have vouched for it; check its fingerprint \
                              with its owner first.").as_str()
                    ),
                    #[wrap(Some)]
                    set_header_suffix = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 12,
                        set_valign: gtk::Align::Start,
                        set_halign: gtk::Align::End,
                        set_margin_start: 24,
                        gtk::Button {
                            set_label: &i18n("Import…"),
                            set_size_request: (130, -1),
                            connect_clicked => PgpKeysInput::Import,
                        },
                        gtk::Button {
                            set_label: &i18n("Fetch by address…"),
                            set_size_request: (130, -1),
                            connect_clicked => PgpKeysInput::Fetch,
                        },
                    },
                    #[local_ref]
                    others_list -> gtk::ListBox {
                        add_css_class: "boxed-list",
                        set_selection_mode: gtk::SelectionMode::None,
                    },
                },
            },
        }
    }

    fn init(init: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let own_list = gtk::ListBox::new();
        let others_list = gtk::ListBox::new();
        let mut model = PgpKeys {
            identities: init.identities,
            own: Vec::new(),
            others: Vec::new(),
            own_list: own_list.clone(),
            others_list: others_list.clone(),
            toasts: None,
            busy: false,
        };
        let widgets = view_output!();
        model.toasts = Some(widgets.toasts.clone());

        // Probing gpg runs it; done a moment after the Settings window is
        // up rather than before it can appear.
        {
            let status_row = widgets.status_row.clone();
            let own_group = widgets.own_group.clone();
            let others_group = widgets.others_group.clone();
            let s = sender.clone();
            gtk::glib::idle_add_local_full(gtk::glib::Priority::LOW, move || {
                let available = pgp::available();
                if available {
                    let version = gpg_version().unwrap_or_default();
                    status_row.set_subtitle(&i18n_f(
                        "GnuPG {version} found. Hylki reads and writes its keyring, the same one the gpg \
                         command and other mail programs use.",
                        &[("version", &version)],
                    ));
                } else {
                    status_row.set_subtitle(&i18n(
                        "GnuPG was not found. Install the gnupg package to read encrypted mail and \
                         check signatures; the Flatpak build carries it.",
                    ));
                    status_row.add_css_class("error");
                }
                own_group.set_sensitive(available);
                others_group.set_sensitive(available);
                if available {
                    s.input(PgpKeysInput::Refresh);
                }
                gtk::glib::ControlFlow::Break
            });
        }

        let empty_own = gtk::Label::new(Some(&i18n("No key of your own yet. Generate one for your address.")));
        empty_own.add_css_class("dim-label");
        empty_own.set_margin_top(14);
        empty_own.set_margin_bottom(14);
        own_list.set_placeholder(Some(&empty_own));
        let empty_others = gtk::Label::new(Some(&i18n("No keys from other people yet.")));
        empty_others.add_css_class("dim-label");
        empty_others.set_margin_top(14);
        empty_others.set_margin_bottom(14);
        others_list.set_placeholder(Some(&empty_others));

        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match message {
            PgpKeysInput::Refresh => self.reload(&sender),

            PgpKeysInput::Generate => self.generate_dialog(&sender),
            PgpKeysInput::GenerateConfirmed { name, email, expire, passphrase } => {
                if self.busy {
                    return;
                }
                self.busy = true;
                self.toast(&i18n("Generating the key…"));
                sender.oneshot_command(async move {
                    let r = tokio::task::spawn_blocking(move || {
                        pgp::generate_key(&Gpg::system(), &name, &email, &expire, &passphrase)
                    })
                    .await
                    .unwrap_or_else(|_| Err("task failed".into()));
                    PgpKeysCmd::Generated(r)
                });
            }

            PgpKeysInput::Import => {
                let dialog = gtk::FileDialog::builder().title(&i18n("Import Key")).build();
                let filter = gtk::FileFilter::new();
                filter.set_name(Some(&i18n("OpenPGP keys")));
                for pattern in ["*.asc", "*.gpg", "*.pgp", "*.key", "*.pub"] {
                    filter.add_pattern(pattern);
                }
                let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                let any = gtk::FileFilter::new();
                any.set_name(Some(&i18n("All files")));
                any.add_pattern("*");
                filters.append(&any);
                dialog.set_filters(Some(&filters));
                let parent = relm4::main_application().active_window();
                let s = sender.clone();
                dialog.open(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                    if let Ok(file) = res {
                        if let Some(path) = file.path() {
                            match std::fs::read(&path) {
                                Ok(bytes) => s.input(PgpKeysInput::ImportBytes(bytes)),
                                Err(e) => tracing::warn!("could not read key file: {e}"),
                            }
                        }
                    }
                });
            }
            PgpKeysInput::ImportBytes(bytes) => {
                match pgp::import_keys(&Gpg::system(), &bytes) {
                    Ok(s) => self.toast(&import_message(&s)),
                    Err(e) => self.toast(&i18n_f("Could not import: {e}", &[("e", &e)])),
                }
                self.reload(&sender);
            }

            PgpKeysInput::Export(fpr) => {
                let key = self.own.iter().chain(self.others.iter()).find(|k| k.fingerprint == fpr).cloned();
                let name = key
                    .as_ref()
                    .and_then(|k| k.emails().into_iter().next())
                    .map(|e| format!("{e}-public.asc"))
                    .unwrap_or_else(|| "public-key.asc".to_string());
                let dialog = gtk::FileDialog::builder().title(&i18n("Export Public Key")).initial_name(&name).build();
                let parent = relm4::main_application().active_window();
                let s = sender.clone();
                dialog.save(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                    if let Ok(file) = res {
                        if let Some(path) = file.path() {
                            match pgp::export_public(&Gpg::system(), &fpr) {
                                Ok(bytes) => {
                                    if let Err(e) = std::fs::write(&path, bytes) {
                                        tracing::warn!("could not write key file: {e}");
                                    }
                                }
                                Err(e) => tracing::warn!("could not export key: {e}"),
                            }
                            // Nothing to say on success beyond the file's existence.
                            s.input(PgpKeysInput::Refresh);
                        }
                    }
                });
            }

            PgpKeysInput::Delete { fingerprint, secret } => {
                let Some(key) = self.own.iter().chain(self.others.iter()).find(|k| k.fingerprint == fingerprint).cloned() else {
                    return;
                };
                let parent = relm4::main_application().active_window();
                let (title, body) = if secret {
                    (
                        i18n("Delete your key?"),
                        i18n_f(
                            "{uid} will be removed, secret half included. Mail encrypted to it \
                             can never be read again, unless you have a backup of the key.",
                            &[("uid", key.primary_uid())],
                        ),
                    )
                } else {
                    (
                        i18n("Remove this key?"),
                        i18n_f("{uid} will be removed from your keyring. It can be imported or fetched again later.", &[("uid", key.primary_uid())]),
                    )
                };
                let dialog = adw::MessageDialog::new(parent.as_ref(), Some(&title), Some(&body));
                dialog.add_response("cancel", &i18n("Cancel"));
                dialog.add_response("delete", &if secret { i18n("Delete") } else { i18n("Remove") });
                dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                let s = sender.clone();
                dialog.connect_response(None, move |_, resp| {
                    if resp == "delete" {
                        s.input(PgpKeysInput::DeleteConfirmed { fingerprint: fingerprint.clone(), secret });
                    }
                });
                dialog.present();
            }
            PgpKeysInput::DeleteConfirmed { fingerprint, secret } => {
                if let Err(e) = pgp::delete_key(&Gpg::system(), &fingerprint, secret) {
                    self.toast(&i18n_f("Could not delete: {e}", &[("e", &e)]));
                }
                self.reload(&sender);
            }

            PgpKeysInput::Fetch => {
                let parent = relm4::main_application().active_window();
                let dialog = adw::MessageDialog::new(
                    parent.as_ref(),
                    Some(&i18n("Fetch a Key")),
                    Some(&i18n("Looks the address up where its provider publishes keys (WKD), then on the keyservers.")),
                );
                dialog.add_response("cancel", &i18n("Cancel"));
                dialog.add_response("fetch", &i18n("Fetch"));
                dialog.set_response_appearance("fetch", adw::ResponseAppearance::Suggested);
                dialog.set_default_response(Some("fetch"));
                let form = gtk::ListBox::new();
                form.add_css_class("boxed-list");
                form.set_selection_mode(gtk::SelectionMode::None);
                let addr_row = adw::EntryRow::new();
                addr_row.set_title(&i18n("Email address"));
                addr_row.set_input_purpose(gtk::InputPurpose::Email);
                form.append(&addr_row);
                dialog.set_extra_child(Some(&form));
                let s = sender.clone();
                dialog.connect_response(None, move |_, resp| {
                    if resp == "fetch" {
                        s.input(PgpKeysInput::FetchConfirmed(addr_row.text().trim().to_string()));
                    }
                });
                dialog.present();
            }
            PgpKeysInput::FetchConfirmed(addr) => {
                if addr.is_empty() || self.busy {
                    return;
                }
                self.busy = true;
                self.toast(&i18n_f("Looking for a key for {addr}…", &[("addr", &addr)]));
                sender.oneshot_command(async move {
                    let r = tokio::task::spawn_blocking(move || pgp::fetch_key(&Gpg::system(), &addr, None))
                        .await
                        .unwrap_or_else(|_| Err("task failed".into()));
                    PgpKeysCmd::Fetched(r)
                });
            }

            PgpKeysInput::Trust(fpr) => {
                let Some(key) = self.others.iter().find(|k| k.fingerprint == fpr).cloned() else {
                    return;
                };
                if self.own.iter().all(|k| !(k.usable() && k.can_sign)) {
                    self.toast(&i18n("Generate a key of your own first: trusting a key means signing it with yours."));
                    return;
                }
                let parent = relm4::main_application().active_window();
                let dialog = adw::MessageDialog::new(
                    parent.as_ref(),
                    Some(&i18n("Trust this key?")),
                    Some(&i18n_f(
                        "Compare the fingerprint with the one {uid} gives you in person or over another channel. \
                         Trusting a key you have not checked lets an impostor's signature pass as theirs.",
                        &[("uid", key.primary_uid())],
                    )),
                );
                let fpr_label = gtk::Label::new(Some(&key.fingerprint_display()));
                fpr_label.add_css_class("monospace");
                fpr_label.set_wrap(true);
                fpr_label.set_selectable(true);
                fpr_label.set_justify(gtk::Justification::Center);
                dialog.set_extra_child(Some(&fpr_label));
                dialog.add_response("cancel", &i18n("Cancel"));
                dialog.add_response("trust", &i18n("Trust"));
                dialog.set_response_appearance("trust", adw::ResponseAppearance::Suggested);
                dialog.set_default_response(Some("cancel"));
                let s = sender.clone();
                dialog.connect_response(None, move |_, resp| {
                    if resp == "trust" {
                        s.input(PgpKeysInput::TrustConfirmed(fpr.clone()));
                    }
                });
                dialog.present();
            }
            PgpKeysInput::TrustConfirmed(fpr) => {
                let signer = self.own.iter().find(|k| k.usable() && k.can_sign).map(|k| k.fingerprint.clone());
                match pgp::trust_key(&Gpg::system(), &fpr, signer.as_deref()) {
                    Ok(()) => self.toast(&i18n("Key trusted.")),
                    Err(e) => self.toast(&i18n_f("Could not sign the key: {e}", &[("e", &e)])),
                }
                self.reload(&sender);
            }
        }
    }

    fn update_cmd(&mut self, message: Self::CommandOutput, sender: ComponentSender<Self>, _root: &Self::Root) {
        self.busy = false;
        match message {
            PgpKeysCmd::Generated(Ok(_)) => self.toast(&i18n("Your key is ready.")),
            PgpKeysCmd::Generated(Err(e)) => self.toast(&i18n_f("Could not generate the key: {e}", &[("e", &e)])),
            PgpKeysCmd::Fetched(Ok(s)) => self.toast(&import_message(&s)),
            PgpKeysCmd::Fetched(Err(e)) => self.toast(&i18n_f("No key found: {e}", &[("e", &e)])),
        }
        self.reload(&sender);
    }
}

impl PgpKeys {
    fn toast(&self, text: &str) {
        if let Some(t) = &self.toasts {
            t.add_toast(adw::Toast::new(text));
        }
    }

    /// Re-read the keyring and rebuild both lists.
    fn reload(&mut self, sender: &ComponentSender<Self>) {
        let gpg = Gpg::system();
        self.own = pgp::list_keys(&gpg, true);
        let own_fprs: Vec<String> = self.own.iter().map(|k| k.fingerprint.clone()).collect();
        self.others = pgp::list_keys(&gpg, false)
            .into_iter()
            .filter(|k| !own_fprs.contains(&k.fingerprint))
            .collect();
        for list in [&self.own_list, &self.others_list] {
            while let Some(row) = list.row_at_index(0) {
                list.remove(&row);
            }
        }
        for key in &self.own {
            self.own_list.append(&key_row(key, true, sender));
        }
        for key in &self.others {
            self.others_list.append(&key_row(key, false, sender));
        }
    }

    /// The form for a new key: which identity, how long it lasts, and the
    /// passphrase that protects it.
    fn generate_dialog(&self, sender: &ComponentSender<Self>) {
        let parent = relm4::main_application().active_window();
        let dialog = adw::MessageDialog::new(
            parent.as_ref(),
            Some(&i18n("Generate a Key")),
            Some(&i18n("A signing key with an encryption subkey, made by GnuPG and kept in its keyring. \
                        The passphrase is asked for when the key is used; the agent remembers it for a while.")),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("generate", &i18n("Generate"));
        dialog.set_response_appearance("generate", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("generate"));

        let form = gtk::ListBox::new();
        form.add_css_class("boxed-list");
        form.set_selection_mode(gtk::SelectionMode::None);

        let name_row = adw::EntryRow::new();
        name_row.set_title(&i18n("Name"));
        let email_row = adw::ComboRow::new();
        email_row.set_title(&i18n("Address"));
        let emails: Vec<String> = self.identities.iter().map(|(_, e)| e.clone()).collect();
        let email_refs: Vec<&str> = emails.iter().map(String::as_str).collect();
        email_row.set_model(Some(&gtk::StringList::new(&email_refs)));
        if let Some((name, _)) = self.identities.first() {
            name_row.set_text(name);
        }
        {
            let identities = self.identities.clone();
            let name_row = name_row.clone();
            email_row.connect_selected_notify(move |row| {
                if let Some((name, _)) = identities.get(row.selected() as usize) {
                    name_row.set_text(name);
                }
            });
        }
        let expiry_row = adw::ComboRow::new();
        expiry_row.set_title(&i18n("Expires"));
        let labels: Vec<String> = EXPIRIES.iter().map(|(l, _)| i18n(l)).collect();
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        expiry_row.set_model(Some(&gtk::StringList::new(&label_refs)));
        let pass_row = adw::PasswordEntryRow::new();
        pass_row.set_title(&i18n("Passphrase"));
        let confirm_row = adw::PasswordEntryRow::new();
        confirm_row.set_title(&i18n("Confirm passphrase"));
        for row in [name_row.clone().upcast::<gtk::Widget>(), email_row.clone().upcast(), expiry_row.clone().upcast(), pass_row.clone().upcast(), confirm_row.clone().upcast()] {
            form.append(&row);
        }
        dialog.set_extra_child(Some(&form));

        let s = sender.clone();
        let identities = self.identities.clone();
        dialog.connect_response(None, move |dialog, resp| {
            if resp != "generate" {
                return;
            }
            let email = identities.get(email_row.selected() as usize).map(|(_, e)| e.clone()).unwrap_or_default();
            let pass = pass_row.text().to_string();
            if pass != confirm_row.text() {
                let parent = dialog.transient_for();
                let warn = adw::MessageDialog::new(parent.as_ref(), Some(&i18n("The passphrases differ")), Some(&i18n("Type the same passphrase twice.")));
                warn.add_response("ok", &i18n("OK"));
                warn.present();
                return;
            }
            let expire = EXPIRIES.get(expiry_row.selected() as usize).map(|(_, v)| v.to_string()).unwrap_or_else(|| "2y".into());
            s.input(PgpKeysInput::GenerateConfirmed { name: name_row.text().to_string(), email, expire, passphrase: pass });
        });
        dialog.present();
    }
}

/// One key as a row: who it is for, its id and standing, and its actions.
fn key_row(key: &KeyInfo, own: bool, sender: &ComponentSender<PgpKeys>) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(&gtk::glib::markup_escape_text(key.primary_uid()));
    let mut parts = vec![pgp::key_display(&key.key_id)];
    if let Some(exp) = key.expires {
        let date = chrono::DateTime::from_timestamp(exp, 0)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        parts.push(if exp > now_secs() {
            i18n_f("expires {date}", &[("date", &date)])
        } else {
            i18n_f("expired {date}", &[("date", &date)])
        });
    }
    if own {
        parts.push(if key.can_encrypt {
            i18n("signs and encrypts")
        } else {
            i18n("signs only: no encryption subkey")
        });
    } else {
        parts.push(key.validity.label());
    }
    if key.uids.len() > 1 {
        parts.push(ni18n_f("{n} more address", "{n} more addresses", (key.uids.len() - 1) as u32, &[("n", &(key.uids.len() - 1).to_string())]));
    }
    row.set_subtitle(&gtk::glib::markup_escape_text(&parts.join(" · ")));
    row.set_tooltip_text(Some(&key.fingerprint_display()));
    if !key.usable() {
        row.add_css_class("dim-label");
    }
    let action = |icon: &str, tip: &str| {
        let b = gtk::Button::from_icon_name(icon);
        b.set_valign(gtk::Align::Center);
        b.add_css_class("flat");
        b.set_tooltip_text(Some(tip));
        b
    };
    let fpr = key.fingerprint.clone();
    if own {
        let export = action("co.hyprlab.Hylki-document-save-symbolic", &i18n("Export public key…"));
        let s = sender.clone();
        let f = fpr.clone();
        export.connect_clicked(move |_| s.input(PgpKeysInput::Export(f.clone())));
        row.add_suffix(&export);
        let delete = action("co.hyprlab.Hylki-user-trash-symbolic", &i18n("Delete this key"));
        let s = sender.clone();
        delete.connect_clicked(move |_| s.input(PgpKeysInput::Delete { fingerprint: fpr.clone(), secret: true }));
        row.add_suffix(&delete);
    } else {
        if key.validity.trusted() {
            let seal = gtk::Image::from_icon_name("co.hyprlab.Hylki-verified-checkmark-symbolic");
            seal.set_tooltip_text(Some(&i18n("Trusted")));
            seal.add_css_class("accent");
            row.add_suffix(&seal);
        } else if key.usable() {
            let trust = gtk::Button::with_label(&i18n("Trust…"));
            trust.set_valign(gtk::Align::Center);
            trust.add_css_class("flat");
            let s = sender.clone();
            let f = fpr.clone();
            trust.connect_clicked(move |_| s.input(PgpKeysInput::Trust(f.clone())));
            row.add_suffix(&trust);
        }
        let remove = action("co.hyprlab.Hylki-user-trash-symbolic", &i18n("Remove this key"));
        let s = sender.clone();
        remove.connect_clicked(move |_| s.input(PgpKeysInput::Delete { fingerprint: fpr.clone(), secret: false }));
        row.add_suffix(&remove);
    }
    row
}

fn import_message(s: &ImportSummary) -> String {
    if s.imported > 0 {
        ni18n_f("{n} key imported.", "{n} keys imported.", s.imported, &[("n", &s.imported.to_string())])
    } else {
        i18n("That key was already in your keyring.")
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// "2.4.9", from `gpg --version`'s first line.
fn gpg_version() -> Option<String> {
    let out = std::process::Command::new("gpg").arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next()?.split_whitespace().last().map(str::to_string)
}
