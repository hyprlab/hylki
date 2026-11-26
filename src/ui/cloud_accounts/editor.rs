//! The cloud account editor's form: one per service kind, its connection
//! check and its Save.

use super::*;

/// The editor form's widgets, built from the account being edited (see
/// [`build_editor`] for what each kind shows).
struct EditorRows {
    /// The service picker: insensitive once the account exists.
    kind: adw::ComboRow,
    /// The service's mark over the form.
    header_mark: gtk::Image,
    name: adw::EntryRow,
    url: adw::EntryRow,
    user: adw::EntryRow,
    pass: adw::PasswordEntryRow,
    code: adw::EntryRow,
    seafile_hint: gtk::Label,
    /// GNOME Online Accounts' file accounts (OneDrive).
    goa_accounts: Vec<crate::services::goa::GoaFilesAccount>,
    goa_row: adw::ComboRow,
    goa_hint: gtk::Label,
    app_key: adw::EntryRow,
    app_key_hint: gtk::Label,
    library: adw::EntryRow,
    folder: adw::EntryRow,
    cloudflare: adw::SwitchRow,
    expire: adw::SpinRow,
    protect: adw::SwitchRow,
    /// Check Connection, and what it found.
    check: gtk::Button,
    status: gtk::Label,
    /// The whole form, ready for the editor page.
    page: gtk::Box,
}

impl EditorRows {
    fn new(account: &CloudAccount, index: Option<usize>) -> Self {
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
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            // By name, not position: the row's own selected-value slot is a
            // list item with no position.
            let name = item
                .item()
                .and_downcast::<gtk::StringObject>()
                .map(|s| s.string().to_string())
                .unwrap_or_default();
            let Some(service) = cloud::SERVICES.iter().find(|s| s.name == name) else {
                return;
            };
            let Some(bx) = item.child().and_downcast::<gtk::Box>() else {
                return;
            };
            // (image, label): swap the image for the service's mark, name the label.
            let Some(old) = bx.first_child() else { return };
            let label = old.next_sibling().and_downcast::<gtk::Label>();
            bx.remove(&old);
            bx.prepend(&crate::desktop::brand::image(service.brand(), 20));
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
        code.set_title(&i18n("Two-step verification code (if used)"));
        code.set_input_purpose(gtk::InputPurpose::Digits);
        let seafile_hint = gtk::Label::new(Some(&i18n(
            "Your Seafile password, and the current code if two-step verification is on. They are swapped once for an API token, which is kept instead. A token can also be pasted as the password.",
        )));
        seafile_hint.set_wrap(true);
        seafile_hint.set_xalign(0.0);
        seafile_hint.add_css_class("dim-label");
        seafile_hint.add_css_class("caption");
        seafile_hint.set_margin_top(6);
        seafile_hint.set_margin_start(12);
        seafile_hint.set_margin_end(12);
        // OneDrive: which GNOME Online Accounts account.
        let goa_accounts = crate::services::goa::list_files_accounts();
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
            "Files over 100 MB are uploaded in 90 MB pieces.",
        ));
        cloudflare.set_active(account.cloudflare);
        let expire = adw::SpinRow::with_range(0.0, 365.0, 1.0);
        expire.set_title(&i18n("Links expire after"));
        expire.set_subtitle(&i18n("Days. 0 keeps links forever."));
        expire.set_value(account.expire_days as f64);
        let protect = adw::SwitchRow::new();
        protect.set_title(&i18n("Protect links with a password"));
        protect.set_subtitle(&i18n(
            "Each file gets its own password, shown to you to pass on.",
        ));
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
            "The upload dialog can change them for one message.",
        )));
        defaults.set_margin_top(28);
        defaults.add(&expire);
        defaults.add(&protect);

        let check_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        check_box.set_margin_top(8);
        let check = gtk::Button::with_label(&i18n("Test Connection"));
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
        EditorRows {
            kind,
            header_mark,
            name,
            url,
            user,
            pass,
            code,
            seafile_hint,
            goa_accounts,
            goa_row,
            goa_hint,
            app_key,
            app_key_hint,
            library,
            folder,
            cloudflare,
            expire,
            protect,
            check,
            status,
            page: bx,
        }
    }
}

/// The account editor's form: a kind, the fields that kind needs, a
/// connection check that signs in with what is typed (a browser sign-in,
/// for Dropbox). Answers with the form and what the page's Save button
/// does with it.
pub(super) fn build_editor(
    index: Option<usize>,
    account: CloudAccount,
    sender: relm4::Sender<CloudAccountsInput>,
) -> (gtk::Widget, Rc<dyn Fn() -> bool>) {
    let EditorRows {
        kind,
        header_mark,
        name,
        url,
        user,
        pass,
        code,
        seafile_hint,
        goa_accounts,
        goa_row,
        goa_hint,
        app_key,
        app_key_hint,
        library,
        folder,
        cloudflare,
        expire,
        protect,
        check,
        status,
        page: bx,
    } = EditorRows::new(&account, index);
    let set_header_mark = {
        let header_mark = header_mark.clone();
        move |id: &str| match crate::desktop::brand::texture(id, 112) {
            Some(t) => header_mark.set_paintable(Some(&t)),
            None => header_mark.set_icon_name(Some("cloud-symbolic")),
        }
    };
    set_header_mark(account.brand());

    // A sign-in that made a secret of its own leaves it here until Save:
    // the Dropbox refresh token (with the account's e-mail and name), or
    // the Seafile API token from a two-step sign-in.
    let dropbox_login: Rc<RefCell<Option<(String, String, String)>>> = Rc::new(RefCell::new(None));
    // What the connection check found the service allows on a link
    // (OneDrive plans differ); saved with the account.
    let link_terms: Rc<RefCell<Option<Option<cloud::LinkTerms>>>> = Rc::new(RefCell::new(None));

    let selected_service = {
        let kind = kind.clone();
        move || {
            cloud::SERVICES
                .get(kind.selected() as usize)
                .copied()
                .unwrap_or(cloud::SERVICES[0])
        }
    };
    let selected_kind = {
        let selected_service = selected_service.clone();
        move || selected_service().kind
    };

    // The GOA accounts the picker currently lists (those of the kind's
    // provider), in row order.
    let goa_listed: Rc<RefCell<Vec<crate::services::goa::GoaFilesAccount>>> =
        Rc::new(RefCell::new(Vec::new()));
    let fill_goa = {
        let (goa_row, goa_hint, goa_accounts, goa_listed) = (
            goa_row.clone(),
            goa_hint.clone(),
            goa_accounts.clone(),
            goa_listed.clone(),
        );
        let existing_goa = account.goa_id.clone();
        move |k: CloudKind| {
            let Some(provider) = k.goa_provider() else {
                return;
            };
            let listed: Vec<crate::services::goa::GoaFilesAccount> = goa_accounts
                .iter()
                .filter(|a| a.provider_type == provider)
                .cloned()
                .collect();
            let labels: Vec<String> = listed
                .iter()
                .map(|a| {
                    if a.files_enabled {
                        a.email.clone()
                    } else {
                        i18n_f("{email} (Files is off)", &[("email", &a.email)])
                    }
                })
                .collect();
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            goa_row.set_model(Some(&gtk::StringList::new(&refs)));
            if let Some(i) = listed.iter().position(|a| a.id == existing_goa) {
                goa_row.set_selected(i as u32);
            }
            goa_row.set_sensitive(!listed.is_empty());
            let mut hint = if listed.is_empty() {
                i18n("No Microsoft 365 account in GNOME Online Accounts yet. Add one in Settings → Online Accounts.")
            } else {
                i18n("OneDrive signs in through the online account above. No password needed.")
            };
            hint.push_str("\n\n");
            hint.push_str(&i18n(
                "Link expiry and passwords need Microsoft 365 or OneDrive for Business. A free personal OneDrive refuses links with either set.",
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
        let (
            url,
            user,
            pass,
            code,
            seafile_hint,
            app_key,
            app_key_hint,
            library,
            check,
            protect,
            expire,
            goa_row,
            goa_hint,
        ) = (
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
                check.set_label(&i18n("Test Connection"));
                expire.set_subtitle(&i18n(
                    "Days. 0 keeps links forever. Needs Microsoft 365 or OneDrive for Business.",
                ));
                protect.set_subtitle(&i18n("Each file gets its own password, shown to you to pass on. Needs Microsoft 365 or OneDrive for Business."));
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
            expire.set_subtitle(&i18n("Days. 0 keeps links forever."));
            match k {
                CloudKind::Nextcloud => {
                    user.set_title(&i18n("Username"));
                    pass.set_title(&if editing {
                        i18n("App password (leave empty to keep)")
                    } else {
                        i18n("App password")
                    });
                    protect.set_subtitle(&i18n(
                        "Each file gets its own password, shown to you to pass on.",
                    ));
                    check.set_label(&i18n("Test Connection"));
                }
                CloudKind::Seafile => {
                    user.set_title(&i18n("Email"));
                    pass.set_title(&if editing {
                        i18n("Password (leave empty to keep)")
                    } else {
                        i18n("Password")
                    });
                    protect.set_subtitle(&i18n(
                        "Each file gets its own password, shown to you to pass on.",
                    ));
                    check.set_label(&i18n("Test Connection"));
                }
                // Handled above, before the return.
                CloudKind::OneDrive => {}
                CloudKind::Dropbox => {
                    protect.set_subtitle(&i18n("Each file gets its own password, shown to you to pass on. Paid Dropbox plans only."));
                    check.set_label(&i18n("Connect with Dropbox…"));
                    app_key_hint.set_label(&i18n_f(
                        "Dropbox needs an app registered to you:\n1. At dropbox.com/developers/apps, press Create app.\n2. Choose Scoped access, then App folder or Full Dropbox.\n3. Give it a name no one else has used and press Create app.\n4. On Permissions, tick account_info.read, files.content.write and sharing.write, then press Submit.\n5. On Settings, under OAuth 2, add the redirect URI {uri} and press Add.\n6. Copy the App key into the field above.\nDevelopment status is enough. Leave the field empty to use this build’s own key, if it has one.",
                        &[("uri", &format!("http://localhost:{}/", crate::services::oauth::DROPBOX_REDIRECT_PORT))],
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
                goa_listed
                    .borrow()
                    .get(goa_row.selected() as usize)
                    .map(|a| (a.id.clone(), a.email.clone()))
            } else {
                None
            };
            let user = match (kind, dropbox_login.borrow().as_ref()) {
                (CloudKind::Dropbox, Some((_, email, _))) => email.clone(),
                (CloudKind::Dropbox, None) if existing.kind == CloudKind::Dropbox => {
                    existing.user.clone()
                }
                (CloudKind::Dropbox, None) => String::new(),
                (CloudKind::OneDrive, _) => {
                    goa.as_ref().map(|(_, e)| e.clone()).unwrap_or_default()
                }
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

    connect_check(
        &check,
        read.clone(),
        &pass,
        &status,
        account.clone(),
        dropbox_login.clone(),
        &name,
        &code,
        selected_kind.clone(),
        link_terms.clone(),
        apply_terms.clone(),
    );

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
                    let _ = sender.send(CloudAccountsInput::Save {
                        index,
                        account: Box::new(a),
                        password: String::new(),
                    });
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
                            Ok((token, _)) => CloudAccountsInput::Save {
                                index,
                                account: Box::new(a),
                                password: token,
                            },
                            Err(e) => CloudAccountsInput::Failed(e),
                        });
                    });
                    return true;
                }
                _ => pass.text().to_string(),
            }
        };
        let _ = sender.send(CloudAccountsInput::Save {
            index,
            account: Box::new(a),
            password: secret,
        });
        true
    });
    (bx.upcast(), save)
}

/// Check Connection: sign in with what the form holds (a browser sign-in for
/// Dropbox, a password or two-step code for Seafile, the GOA account for
/// OneDrive), on a thread, and report on `status`. A sign-in's own secret
/// waits in `dropbox_login` for Save, the link terms found in `link_terms`.
// The pieces of the form the check reads and reports on, passed one by one
// as the closure that held them captured them.
#[allow(clippy::too_many_arguments)]
fn connect_check(
    check: &gtk::Button,
    read: impl Fn() -> CloudAccount + Clone + 'static,
    pass: &adw::PasswordEntryRow,
    status: &gtk::Label,
    existing: CloudAccount,
    dropbox_login: Rc<RefCell<Option<(String, String, String)>>>,
    name: &adw::EntryRow,
    code: &adw::EntryRow,
    selected_kind: impl Fn() -> CloudKind + Clone + 'static,
    link_terms: Rc<RefCell<Option<Option<cloud::LinkTerms>>>>,
    apply_terms: impl Fn(&CloudAccount) + Clone + 'static,
) {
    let (pass, status, name, code) = (pass.clone(), status.clone(), name.clone(), code.clone());
    check.connect_clicked(move |b| {
        let a = read();
        /// A typed password, or else the saved one, read on the sign-in's
        /// thread: the keyring may wait on its user (#466).
        struct Password {
            typed: String,
            saved: Option<String>,
            missing: String,
        }
        impl Password {
            fn get(self) -> Result<String, String> {
                if !self.typed.is_empty() {
                    return Ok(self.typed);
                }
                let Some(key) = self.saved else { return Ok(String::new()) };
                crate::config::load_cloud_password(&key).filter(|p| !p.is_empty()).ok_or(self.missing)
            }
        }
        enum Job {
            Verify(CloudAccount, Password),
            Dropbox(CloudAccount),
            SeafileCode(CloudAccount, Password, String),
        }
        type Outcome = (String, Option<(String, String, String)>, Option<Option<cloud::LinkTerms>>);
        let job = if a.kind.via_goa() {
            if a.goa_id.is_empty() {
                status.set_label(&i18n("Choose an online account first."));
                return;
            }
            status.set_label(&i18n("Signing in…"));
            Job::Verify(a, Password { typed: String::new(), saved: None, missing: String::new() })
        } else if a.kind == CloudKind::Dropbox {
            if cloud::dropbox_client_id(&a).is_empty() {
                status.set_label(&i18n("Enter the app key of a Dropbox app first."));
                return;
            }
            status.set_label(&i18n("Waiting for the sign-in in your browser…"));
            Job::Dropbox(a)
        } else {
            let missing = if a.kind == CloudKind::Seafile {
                i18n("Fill in the server URL, email and password first.")
            } else {
                i18n("Fill in the server URL, user name and app password first.")
            };
            if a.url.is_empty() || a.user.is_empty() {
                status.set_label(&missing);
                return;
            }
            status.set_label(&i18n("Signing in…"));
            let pw = Password { typed: pass.text().to_string(), saved: Some(existing.key()), missing };
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
                Job::Verify(a, pw) => pw.get().and_then(|pw| {
                    cloud::verify(&a, &pw).map(|who| {
                        let terms = cloud::probe_link_terms(&a, &pw).ok();
                        (who, None, terms)
                    })
                }),
                Job::Dropbox(a) => cloud::dropbox_connect(&a)
                    .map(|(refresh, email, who)| (format!("{who} ({email})"), Some((refresh, email, who)), None)),
                Job::SeafileCode(a, pw, otp) => pw.get().and_then(|pw| {
                    cloud::seafile_login_with_code(&a, &pw, &otp)
                        .map(|(token, who)| (who.clone(), Some((token, a.user.clone(), who)), None))
                }),
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
                            "Uploads and plain links work. Link expiry and passwords need a Microsoft 365 subscription, so both are off for this account.",
                        )),
                        (true, false) => Some(i18n(
                            "Uploads, links and expiry work. OneDrive for Business links take no password, so that is off for this account.",
                        )),
                        (false, true) => Some(i18n(
                            "Uploads, links and passwords work. Link expiry is not available, so it is off for this account.",
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
