//! Send-as aliases (#34) and hidden folders (#239) in the account editor.

use super::*;

/// The alias editor dialog (#34): a small modal for one send-as alias — its
/// identity, and optionally its own SMTP transport.
pub(super) struct AliasDialog {
    window: adw::Window,
    name_row: adw::EntryRow,
    addr_row: adw::EntryRow,
    smtp_switch: adw::SwitchRow,
    host_row: adw::EntryRow,
    port_row: adw::EntryRow,
    user_row: adw::EntryRow,
    pass_row: adw::PasswordEntryRow,
    test_btn: gtk::Button,
    test_result: gtk::Label,
}

impl AccountsWindow {
    /// Fill the Hidden Folders list (#239): one row per hidden path, with
    /// a Show button that takes it off the list (Save makes it so).
    pub(super) fn rebuild_hidden_list(
        &self,
        widgets: &AccountsWindowWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let list = &widgets.form.hidden_list;
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        // An empty boxed-list draws as a bare frame; say "none" instead.
        list.set_visible(!self.hidden_edits.is_empty());
        widgets
            .form
            .hidden_empty
            .set_visible(self.hidden_edits.is_empty());
        for (i, path) in self.hidden_edits.iter().enumerate() {
            let row = adw::ActionRow::new();
            row.set_title(&gtk::glib::markup_escape_text(&crate::mail::mutf7::decode(
                path,
            )));
            let show = gtk::Button::with_label(&i18n("Show"));
            show.set_valign(gtk::Align::Center);
            show.add_css_class("flat");
            show.set_tooltip_text(Some(i18n("Bring this folder back to the sidebar").as_str()));
            let s = sender.input_sender().clone();
            show.connect_clicked(move |_| {
                let _ = s.send(AccountsInput::UnhideFolder(i));
            });
            row.add_suffix(&show);
            list.append(&row);
        }
    }

    /// Rebuild the editor's send-as alias list from `alias_edits` (#34).
    pub(super) fn rebuild_alias_list(&self, list: &gtk::ListBox, sender: &ComponentSender<Self>) {
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        // The server's identities (#346), less the account's own plain one
        // (its address, under no name or the account's) and any address an
        // alias here already sends as.
        let (own_name, own) = self
            .editing
            .and_then(|i| self.accounts.get(i))
            .map(|a| (a.name.clone(), a.email.clone()))
            .unwrap_or_default();
        let server: Vec<&(String, String)> = self
            .server_identities
            .iter()
            .filter(|(name, addr)| {
                !(addr.eq_ignore_ascii_case(&own)
                    && (name.is_empty() || name.eq_ignore_ascii_case(addr) || *name == own_name))
            })
            .filter(|(_, addr)| {
                !self
                    .alias_edits
                    .iter()
                    .any(|a| split_identity(&a.identity).1.eq_ignore_ascii_case(addr))
            })
            .collect();
        // An empty boxed-list draws as a bare frame; hide it until there is a row.
        list.set_visible(!self.alias_edits.is_empty() || !server.is_empty());

        for (i, alias) in self.alias_edits.iter().enumerate() {
            let row = adw::ActionRow::new();
            row.set_activatable(true);
            row.set_title(&gtk::glib::markup_escape_text(&alias.identity));
            row.set_subtitle(&gtk::glib::markup_escape_text(&if alias.has_own_smtp() {
                i18n_f("Sends through {host}", &[("host", &alias.smtp_host)])
            } else {
                i18n("Sends through this account")
            }));

            // A dim pencil says "activate to edit"; the trash button removes.
            let edit = gtk::Image::from_icon_name("document-edit-symbolic");
            edit.add_css_class("dim-label");
            row.add_suffix(&edit);

            let remove = gtk::Button::from_icon_name("user-trash-symbolic");
            remove.set_valign(gtk::Align::Center);
            remove.add_css_class("flat");
            remove.set_tooltip_text(Some(i18n("Remove this alias").as_str()));
            let ri = sender.input_sender().clone();
            remove.connect_clicked(move |_| {
                let _ = ri.send(AccountsInput::AliasRemove(i));
            });
            row.add_suffix(&remove);

            list.append(&row);
        }

        for (name, addr) in server {
            let row = adw::ActionRow::new();
            let title = if name.is_empty() || name.eq_ignore_ascii_case(addr) {
                addr.clone()
            } else {
                format!("{name} <{addr}>")
            };
            row.set_title(&gtk::glib::markup_escape_text(&title));
            row.set_subtitle(&i18n(
                "Kept on the server; change it in your provider's webmail.",
            ));
            list.append(&row);
        }
    }

    /// Close the alias dialog, if one is open.
    pub(super) fn close_alias_dialog(&mut self) {
        if let Some(d) = self.alias_dialog.take() {
            d.window.close();
        }
    }

    /// Open the modal alias editor (#34), prefilled from `alias`. A JMAP
    /// account's server sends every message itself (#346), so its aliases
    /// have no SMTP server of their own to set.
    fn open_alias_dialog(
        &mut self,
        root: &adw::Bin,
        alias: &AliasConfig,
        jmap: bool,
        sender: &ComponentSender<Self>,
    ) {
        self.close_alias_dialog();

        let window = adw::Window::builder()
            .modal(true)
            .default_width(440)
            .title(if self.alias_editing.is_some() {
                i18n("Edit Alias")
            } else {
                i18n("Add Alias")
            })
            .build();
        // Parent to whatever window the embedded panel currently sits in.
        window.set_transient_for(root.root().and_downcast::<gtk::Window>().as_ref());

        let cancel = gtk::Button::with_label(&i18n("Cancel"));
        let save = gtk::Button::with_label(&i18n("Save"));
        save.add_css_class("suggested-action");
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();
        header.pack_start(&cancel);
        header.pack_end(&save);

        let (name, addr) = split_identity(&alias.identity);
        let identity_group = adw::PreferencesGroup::new();
        identity_group.set_description(Some(
            i18n("Will appear in the composer's From menu. Replies will come to this address.")
                .as_str(),
        ));
        let name_row = adw::EntryRow::builder()
            .title(i18n("Display name (optional)"))
            .build();
        name_row.set_text(&name);
        let addr_row = adw::EntryRow::builder()
            .title(i18n("Email address"))
            .build();
        addr_row.set_text(&addr);
        identity_group.add(&name_row);
        identity_group.add(&addr_row);

        let smtp_group = adw::PreferencesGroup::new();
        smtp_group.set_title(&i18n("Sending"));
        let smtp_switch = adw::SwitchRow::builder()
            .title(i18n("Own SMTP server"))
            .subtitle(
                "Send through the alias's own mail provider, with its own \
                 sign-in — instead of this account's server. Needed when the \
                 account's provider (e.g. Gmail) rewrites the sender address.",
            )
            .build();
        smtp_switch.set_active(alias.has_own_smtp());
        let host_row = adw::EntryRow::builder().title(i18n("SMTP server")).build();
        host_row.set_text(&alias.smtp_host);
        let port_row = adw::EntryRow::builder().title(i18n("SMTP port")).build();
        port_row.set_text(&alias.smtp_port.to_string());
        let user_row = adw::EntryRow::builder()
            .title(i18n("SMTP username"))
            .build();
        user_row.set_text(&alias.smtp_username);
        let pass_row = adw::PasswordEntryRow::builder()
            .title(i18n("SMTP password"))
            .build();
        pass_row.set_text(&alias.smtp_password);
        smtp_group.add(&smtp_switch);
        smtp_group.add(&host_row);
        smtp_group.add(&port_row);
        smtp_group.add(&user_row);
        smtp_group.add(&pass_row);

        let test_btn = gtk::Button::with_label(&i18n("Test SMTP"));
        test_btn.set_halign(gtk::Align::Start);
        let test_result = gtk::Label::new(None);
        test_result.set_visible(false);
        test_result.set_halign(gtk::Align::Start);
        test_result.set_xalign(0.0);
        test_result.set_wrap(true);
        let test_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        test_box.append(&test_btn);
        test_box.append(&test_result);

        // The SMTP fields (and their test) only apply with the switch on.
        for target in [
            host_row.upcast_ref::<gtk::Widget>(),
            port_row.upcast_ref(),
            user_row.upcast_ref(),
            pass_row.upcast_ref(),
            test_box.upcast_ref(),
        ] {
            smtp_switch
                .bind_property("active", target, "visible")
                .sync_create()
                .build();
        }

        let content = gtk::Box::new(gtk::Orientation::Vertical, 24);
        content.set_margin_top(24);
        content.set_margin_bottom(24);
        content.set_margin_start(24);
        content.set_margin_end(24);
        content.append(&identity_group);
        if !jmap {
            content.append(&smtp_group);
            content.append(&test_box);
        }

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(640)
            .child(&content)
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&scroller));
        window.set_content(Some(&toolbar));

        // The window keeps the height it opened with unless asked again:
        // a default height of -1 takes the content's, so it grows down for
        // the SMTP fields and shrinks back when they go.
        let w = window.clone();
        smtp_switch.connect_active_notify(move |_| {
            let w = w.clone();
            gtk::glib::idle_add_local_once(move || w.set_default_size(w.width(), -1));
        });
        let w = window.clone();
        cancel.connect_clicked(move |_| w.close());
        let s = sender.input_sender().clone();
        save.connect_clicked(move |_| {
            let _ = s.send(AccountsInput::AliasDialogSave);
        });
        let s = sender.input_sender().clone();
        test_btn.connect_clicked(move |_| {
            let _ = s.send(AccountsInput::AliasDialogTest);
        });
        let s = sender.input_sender().clone();
        window.connect_close_request(move |_| {
            let _ = s.send(AccountsInput::AliasDialogClosed);
            gtk::glib::Propagation::Proceed
        });

        window.present();
        self.alias_dialog = Some(AliasDialog {
            window,
            name_row,
            addr_row,
            smtp_switch,
            host_row,
            port_row,
            user_row,
            pass_row,
            test_btn,
            test_result,
        });
    }
}

impl AccountsWindow {
    pub(super) fn on_alias_add(
        &mut self,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
        root: &adw::Bin,
    ) {
        self.alias_editing = None;
        let jmap = form_protocol(widgets) == Protocol::Jmap;
        self.open_alias_dialog(root, &AliasConfig::default(), jmap, &sender);
    }

    pub(super) fn on_alias_edit(
        &mut self,
        i: usize,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
        root: &adw::Bin,
    ) {
        let Some(alias) = self.alias_edits.get(i).cloned() else {
            return;
        };
        self.alias_editing = Some(i);
        let jmap = form_protocol(widgets) == Protocol::Jmap;
        self.open_alias_dialog(root, &alias, jmap, &sender);
    }

    pub(super) fn on_unhide_folder(
        &mut self,
        i: usize,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        if i < self.hidden_edits.len() {
            self.hidden_edits.remove(i);
            self.rebuild_hidden_list(widgets, &sender);
        }
    }

    pub(super) fn on_alias_remove(
        &mut self,
        i: usize,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        if i < self.alias_edits.len() {
            // The keyring entry (if the alias had its own SMTP) is
            // dropped on Save, when the removal actually takes effect.
            self.alias_edits.remove(i);
            self.rebuild_alias_list(&widgets.form.aliases_list, &sender);
        }
    }

    pub(super) fn on_alias_dialog_save(
        &mut self,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        let Some(d) = self.alias_dialog.as_ref() else {
            return;
        };
        for row in [&d.addr_row, &d.host_row, &d.user_row] {
            row.remove_css_class("error");
        }
        d.pass_row.remove_css_class("error");

        let name = trimmed(&d.name_row);
        let addr = trimmed(&d.addr_row);
        let own_smtp = d.smtp_switch.is_active();
        let host = trimmed(&d.host_row);
        let port: u16 = trimmed(&d.port_row).parse().unwrap_or(587);
        let user = trimmed(&d.user_row);
        let pass = d.pass_row.text().to_string();

        // The address must look like one. Several names may share an
        // address (a manager and a director writing from one mailbox), but
        // then only through the account's own SMTP: the send path picks an
        // alias's server by the From address alone, so an alias with its
        // own server at a shared address, or at the account's address,
        // would decide which server sends for all of them.
        let account_email = trimmed(&widgets.form.email_row);
        let shared = addr.eq_ignore_ascii_case(&account_email)
            || self.alias_edits.iter().enumerate().any(|(i, a)| {
                Some(i) != self.alias_editing && a.address().eq_ignore_ascii_case(&addr)
            });
        let conflict = self.alias_edits.iter().enumerate().any(|(i, a)| {
            Some(i) != self.alias_editing
                && a.has_own_smtp()
                && a.address().eq_ignore_ascii_case(&addr)
        });
        let mut bad = false;
        d.addr_row.set_tooltip_text(None);
        if addr.is_empty() || !addr.contains('@') {
            d.addr_row.add_css_class("error");
            bad = true;
        } else if conflict || (shared && own_smtp) {
            d.addr_row.add_css_class("error");
            d.addr_row.set_tooltip_text(Some(
                i18n("Names that share an address send through this account's server.").as_str(),
            ));
            bad = true;
        }
        if own_smtp {
            if host.is_empty() {
                d.host_row.add_css_class("error");
                bad = true;
            }
            if user.is_empty() {
                d.user_row.add_css_class("error");
                bad = true;
            }
            if pass.is_empty() {
                d.pass_row.add_css_class("error");
                bad = true;
            }
        }
        if bad {
            return;
        }

        let alias = AliasConfig {
            identity: if name.is_empty() {
                addr.clone()
            } else {
                format!("{name} <{addr}>")
            },
            smtp_host: if own_smtp { host } else { String::new() },
            smtp_port: port,
            smtp_username: if own_smtp { user } else { String::new() },
            smtp_password: if own_smtp { pass } else { String::new() },
        };
        match self.alias_editing {
            Some(i) if i < self.alias_edits.len() => self.alias_edits[i] = alias,
            _ => self.alias_edits.push(alias),
        }
        self.alias_editing = None;
        self.close_alias_dialog();
        self.rebuild_alias_list(&widgets.form.aliases_list, &sender);
    }

    pub(super) fn on_alias_dialog_test(
        &mut self,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        let Some(d) = self.alias_dialog.as_ref() else {
            return;
        };
        let alias = AliasConfig {
            identity: trimmed(&d.addr_row),
            smtp_host: trimmed(&d.host_row),
            smtp_port: trimmed(&d.port_row).parse().unwrap_or(587),
            smtp_username: trimmed(&d.user_row),
            smtp_password: d.pass_row.text().to_string(),
        };
        let email = trimmed(&widgets.form.email_row);
        d.test_btn.set_sensitive(false);
        d.test_result.set_visible(true);
        d.test_result.set_css_classes(&["dim-label"]);
        d.test_result.set_label(&i18n("Testing…"));
        sender.oneshot_command(async move {
            let r =
                tokio::task::spawn_blocking(move || worker::test_alias_smtp_blocking(email, alias))
                    .await
                    .unwrap_or_else(|_| Err("test could not run".into()));
            AccountsCmd::AliasTested(r)
        });
    }

    pub(super) fn on_alias_dialog_closed(&mut self) {
        // The notification is queued behind the close, so by the time it
        // arrives a replacement dialog may already be open — only clear
        // state when the dialog we track is really the one that closed.
        if self
            .alias_dialog
            .as_ref()
            .is_none_or(|d| !d.window.is_visible())
        {
            self.alias_dialog = None;
            self.alias_editing = None;
        }
    }

    pub(super) fn on_alias_tested(&mut self, result: Result<(), String>) {
        let Some(d) = self.alias_dialog.as_ref() else {
            return;
        };
        d.test_btn.set_sensitive(true);
        d.test_result.set_visible(true);
        match result {
            Ok(()) => {
                d.test_result.set_css_classes(&["success"]);
                d.test_result.set_label(&i18n("✓ SMTP: connected"));
            }
            Err(e) => {
                d.test_result.set_css_classes(&["error"]);
                d.test_result
                    .set_label(&i18n_f("✗ SMTP: {e}", &[("e", &(e).to_string())]));
            }
        }
    }
}
