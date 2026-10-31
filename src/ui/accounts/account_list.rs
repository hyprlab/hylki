//! The list of accounts (drag to reorder) and the GNOME Online Accounts
//! that can still be imported.

use super::form::display_name;
use super::providers::{brand_for_account, provider_name};
use super::*;

/// GNOME Online Accounts mail accounts not (properly) configured in Hylki.
/// A configured entry counts when it has an IMAP host or runs over Graph — an
/// entry with neither is a broken pre-#36 Microsoft 365 import, so its GOA
/// account is offered again and re-importing repairs it. Accounts GOA can
/// neither serve IMAP nor Graph mail for can never connect and aren't listed.
pub(super) fn importable_goa_accounts(
    configured: &[AccountConfig],
) -> Vec<crate::services::goa::GoaMailAccount> {
    importable_of(crate::services::goa::list_mail_accounts(), configured)
}

/// The importable ones among `listed` (see [`importable_goa_accounts`]).
fn importable_of(
    listed: Vec<crate::services::goa::GoaMailAccount>,
    configured: &[AccountConfig],
) -> Vec<crate::services::goa::GoaMailAccount> {
    listed
        .into_iter()
        .filter(|g| {
            !configured.iter().any(|a| {
                a.email.eq_ignore_ascii_case(&g.email)
                    && (!a.imap_host.trim().is_empty()
                        || a.protocol == crate::config::Protocol::Graph)
            })
        })
        .filter(|g| g.can_connect())
        .collect()
}

impl AccountsWindow {
    /// Rebuild the draggable account list.
    pub(super) fn rebuild_account_list(&self, list: &gtk::ListBox, sender: &ComponentSender<Self>) {
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }

        let mut css = String::new();
        // The provider mark and the source badge each get a column of their
        // own, held to one width down the whole list. Both vary: the marks
        // are logos of different aspect (Gmail's M is wider than an envelope),
        // and "GOA" is narrower than "Hylki" (and the pair differs again in
        // each language). Left to themselves they pushed each other sideways
        // and neither read as a column.
        let mark_widths = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        let badge_widths = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        for (pos, acc) in self.accounts.iter().enumerate() {
            let row = gtk::ListBoxRow::new();
            row.set_activatable(true);

            let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            hbox.add_css_class("account-list-row");

            let handle = gtk::Image::from_icon_name("list-drag-handle-symbolic");
            handle.add_css_class("dim-label");
            hbox.append(&handle);

            hbox.append(&account_circle(acc, pos as u32 + 1, &mut css));

            hbox.append(&account_titles(acc));

            // The provider's mark, right of the name and address, named on
            // hover.
            let brand = brand_for_account(acc);
            let fallback = if acc.is_local() {
                super::local::LOCAL_ICON
            } else {
                crate::desktop::brand::GENERIC_MAIL
            };
            let mark = crate::desktop::brand::mark(brand, 24, fallback);
            mark.set_tooltip_text(Some(&provider_name(brand, acc)));
            hbox.append(&size_slot(mark.upcast_ref(), &mark_widths));

            let badge = source_badge(acc);
            hbox.append(&size_slot(badge.upcast_ref(), &badge_widths));

            hbox.append(&enable_switch(acc, pos, sender));

            let next = gtk::Image::from_icon_name("go-next-symbolic");
            next.add_css_class("dim-label");
            hbox.append(&next);

            row.set_child(Some(&hbox));

            add_reorder(&row, pos, sender);

            list.append(&row);
        }
        self.list_css.load_from_string(&css);
        if let Some(group) = &self.local_group {
            group.set_visible(!self.accounts.iter().any(AccountConfig::is_local));
        }
    }

    /// Un-import a GNOME Online Account: drop it from Hylki (the app removes
    /// the config entry and its stored copies) and return it to the "GNOME
    /// Online Accounts" import list below. The account stays in GNOME.
    pub(super) fn unimport_goa(
        &mut self,
        index: usize,
        widgets: &AccountsWindowWidgets,
        sender: &ComponentSender<Self>,
    ) {
        if index >= self.accounts.len() {
            return;
        }
        let email = self.accounts[index].email.clone();
        self.accounts.remove(index);
        let _ = sender.output(AccountsOutput::Removed { email });
        // Re-query GOA so the account's row reappears in the import list fresh.
        self.goa = importable_goa_accounts(&self.accounts);
        self.rebuild_account_list(&widgets.accounts.accounts_list, sender);
        self.rebuild_goa_list(&widgets.accounts.goa_list, sender);
        widgets.accounts.goa_group.set_visible(!self.goa.is_empty());
    }

    /// Populate the "GNOME Online Accounts" list with importable mail accounts.
    pub(super) fn rebuild_goa_list(&self, list: &gtk::ListBox, sender: &ComponentSender<Self>) {
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        for (pos, g) in self.goa.iter().enumerate() {
            let row = adw::ActionRow::new();
            row.set_title(&g.email);
            row.add_prefix(&crate::desktop::brand::mark(
                brand_for_goa(&g.provider),
                24,
                crate::desktop::brand::GENERIC_MAIL,
            ));
            let mut subtitle = if g.provider.is_empty() {
                i18n("Mail")
            } else {
                g.provider.clone()
            };
            // OAuth providers (Gmail, Microsoft 365) sign in with a token from
            // GNOME.
            if g.oauth2 && !g.password_based {
                subtitle.push_str(" · sign-in via GNOME");
            }
            row.set_subtitle(&subtitle);

            let toggle = gtk::Switch::new();
            toggle.set_valign(gtk::Align::Center);
            toggle.set_active(false);
            toggle.set_tooltip_text(Some(i18n("Use this account in Hylki").as_str()));
            let ti = sender.input_sender().clone();
            let tpos = pos;
            toggle.connect_state_set(move |_, state| {
                if state {
                    let _ = ti.send(AccountsInput::ImportGoa(tpos));
                }
                gtk::glib::Propagation::Proceed
            });
            row.add_suffix(&toggle);
            list.append(&row);
        }
    }
}

impl AccountsWindow {
    pub(super) fn on_refresh_goa(
        &mut self,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        // Off the main thread: GOA answers over D-Bus, and a busy
        // goa-daemon is slow to.
        widgets.accounts.goa_refresh_btn.set_sensitive(false);
        sender.oneshot_command(async move {
            let listed = tokio::task::spawn_blocking(crate::services::goa::list_mail_accounts)
                .await
                .unwrap_or_default();
            AccountsCmd::GoaListed(listed)
        });
    }

    pub(super) fn on_move_row(
        &mut self,
        from: usize,
        to: usize,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        if from < self.accounts.len() {
            let acc = self.accounts.remove(from);
            let to = to.min(self.accounts.len());
            self.accounts.insert(to, acc);
            self.rebuild_account_list(&widgets.accounts.accounts_list, &sender);
            let emails = self.accounts.iter().map(|a| a.email.clone()).collect();
            let _ = sender.output(AccountsOutput::Reordered(emails));
        }
    }

    pub(super) fn on_toggle_enabled(
        &mut self,
        index: usize,
        enabled: bool,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        // A GNOME Online Account switched off here isn't paused — it is
        // un-imported: it drops out of Hylki (the config entry and its
        // stored copies go) and returns to the "GNOME Online Accounts"
        // list below, ready to import again. The account itself stays
        // in GNOME untouched.
        if !enabled && self.accounts.get(index).is_some_and(|a| a.goa_id.is_some()) {
            self.unimport_goa(index, widgets, &sender);
            return;
        }
        if let Some(acc) = self.accounts.get_mut(index) {
            if acc.enabled != enabled {
                acc.enabled = enabled;
                let email = acc.email.clone();
                let _ = sender.output(AccountsOutput::EnabledChanged { email, enabled });
            }
        }
    }

    pub(super) fn on_import_goa(
        &mut self,
        index: usize,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        if let Some(g) = self.goa.get(index).cloned() {
            // Password-based providers: pull the password now. OAuth
            // providers: authenticate with a GOA token at connect time.
            // Either way the worker asks GOA again when it connects, so an
            // account still works if this read comes back empty.
            let (password, oauth) = if g.password_based {
                (
                    crate::services::goa::mail_passwords(&g.id)
                        .0
                        .unwrap_or_default(),
                    false,
                )
            } else {
                (String::new(), true)
            };
            let account = g.to_config(password, oauth);
            self.goa.remove(index);
            self.accounts.push(account.clone());
            self.rebuild_account_list(&widgets.accounts.accounts_list, &sender);
            self.rebuild_goa_list(&widgets.accounts.goa_list, &sender);
            widgets.accounts.goa_group.set_visible(!self.goa.is_empty());
            let _ = sender.output(AccountsOutput::ImportGoa(Box::new(account)));
        }
    }

    pub(super) fn on_goa_listed(
        &mut self,
        listed: Vec<crate::services::goa::GoaMailAccount>,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
    ) {
        widgets.accounts.goa_refresh_btn.set_sensitive(true);
        self.goa = importable_of(listed, &self.accounts);
        self.rebuild_goa_list(&widgets.accounts.goa_list, &sender);
        widgets.accounts.goa_group.set_visible(!self.goa.is_empty());
    }
}

/// A slot keeps its child at its natural size and centred; the size group
/// holds the slot, not the child, so a logo is never stretched and a
/// badge's pill still hugs its text.
fn size_slot(child: &gtk::Widget, group: &gtk::SizeGroup) -> gtk::Box {
    let slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    slot.set_valign(gtk::Align::Center);
    child.set_halign(gtk::Align::Center);
    child.set_hexpand(true);
    slot.append(child);
    // The child expands to fill the slot so it lands centred — but an
    // expanding child makes its parent expand too, and an expanding
    // slot would take a share of the row's spare width and defeat the
    // whole point. Pin the slot's own answer to "no".
    slot.set_hexpand(false);
    group.add_widget(&slot);
    slot
}

/// The account's circle, as the sidebar draws it: its color (or the
/// palette accent it would get), and its picture, emoji or initials — the
/// label first, then the name, then the address. Its color rule goes into
/// `css`.
fn account_circle(acc: &AccountConfig, account_id: u32, css: &mut String) -> gtk::Box {
    let color = acc
        .color
        .clone()
        .unwrap_or_else(|| crate::worker::accent_for(account_id).to_string());
    // Ringed as in the sidebar, a pixel off the disc.
    css.push_str(&format!(
        ".acct-list-color-{account_id} {{ background-color: {color}; \
         outline: 2px solid {color}; outline-offset: 1px; }}\n"
    ));
    let circle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    circle.add_css_class("account-circle");
    circle.add_css_class(&format!("acct-list-color-{account_id}"));
    circle.set_valign(gtk::Align::Center);
    circle.set_halign(gtk::Align::Center);
    circle.set_hexpand(false);
    circle.set_size_request(30, 30);
    let shown = match acc.label.as_deref().map(str::trim) {
        Some(l) if !l.is_empty() && l != acc.email => l.to_string(),
        _ => display_name(acc),
    };
    let picture = acc
        .avatar
        .as_deref()
        .and_then(crate::config::avatar_path)
        .filter(|p| p.exists());
    // The Gravatar this address asked for and has (#189) comes first,
    // as in the sidebar circle and the editor preview.
    let gravatar = acc
        .gravatar
        .then(|| crate::services::avatar::own_gravatar(&acc.email))
        .flatten();
    let glyph: gtk::Widget = match (&gravatar, &picture, acc.emoji.as_deref()) {
        (Some(texture), ..) => {
            circle.set_overflow(gtk::Overflow::Hidden);
            crate::ui::initials::picture_from_texture(texture, 30).upcast()
        }
        (None, Some(path), _) => {
            circle.set_overflow(gtk::Overflow::Hidden);
            crate::ui::initials::avatar_picture(path, 30).upcast()
        }
        (None, None, Some(em)) if !em.is_empty() => {
            crate::ui::initials::glyph_picture(em, &color, 0.55, 30).upcast()
        }
        _ => crate::ui::initials::glyph_picture(
            &crate::ui::sidebar::account_initials(&shown, &acc.email),
            &color,
            0.47,
            30,
        )
        .upcast(),
    };
    circle.append(&glyph);
    circle
}

/// The nickname, as the sidebar names the account, over the address. With
/// no nickname the address is the first line, and the second carries the
/// sender name instead of repeating it, or nothing when there is no name
/// either.
fn account_titles(acc: &AccountConfig) -> gtk::Box {
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    vbox.set_hexpand(true);
    vbox.set_valign(gtk::Align::Center);
    let title = acc.display_label();
    let second = if acc.is_local() {
        super::local::shown_dir()
    } else if title == acc.email {
        acc.name.trim().to_string()
    } else {
        acc.email.clone()
    };
    let name = gtk::Label::new(Some(&title));
    name.set_halign(gtk::Align::Start);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.add_css_class("account-name");
    let email = gtk::Label::new(Some(&second));
    email.set_halign(gtk::Align::Start);
    email.set_ellipsize(gtk::pango::EllipsizeMode::End);
    email.add_css_class("account-email");
    email.set_visible(!second.is_empty());
    vbox.append(&name);
    vbox.append(&email);
    vbox
}

/// Source badge: is this account from GNOME Online Accounts, or added
/// directly in Hylki?
fn source_badge(acc: &AccountConfig) -> gtk::Label {
    let from_goa = acc.goa_id.is_some();
    let badge = gtk::Label::new(Some(
        if from_goa { i18n("GOA") } else { i18n("Hylki") }.as_str(),
    ));
    badge.set_valign(gtk::Align::Center);
    badge.add_css_class("account-source-badge");
    if from_goa {
        badge.add_css_class("goa");
        badge.set_tooltip_text(Some(i18n("Imported GNOME Online Account").as_str()));
    } else {
        badge.set_tooltip_text(Some(i18n("Added directly in Hylki").as_str()));
    }
    badge
}

/// Enable/disable toggle. Disabled accounts stay configured but don't sync
/// or appear in the sidebar.
fn enable_switch(
    acc: &AccountConfig,
    pos: usize,
    sender: &ComponentSender<AccountsWindow>,
) -> gtk::Switch {
    let toggle = gtk::Switch::new();
    toggle.set_valign(gtk::Align::Center);
    if acc.goa_mail_disabled {
        toggle.set_tooltip_text(Some(&i18n(
            "Paused: Mail is switched off for this account in GNOME Settings",
        )));
        toggle.set_sensitive(false);
    } else {
        toggle.set_tooltip_text(Some(i18n("Enable this account").as_str()));
    }
    toggle.set_active(acc.enabled);
    let ti = sender.input_sender().clone();
    let tpos = pos;
    toggle.connect_state_set(move |_, state| {
        let _ = ti.send(AccountsInput::ToggleEnabled {
            index: tpos,
            enabled: state,
        });
        gtk::glib::Propagation::Proceed
    });
    toggle
}

/// Drag a row to reorder the accounts.
fn add_reorder(row: &gtk::ListBoxRow, pos: usize, sender: &ComponentSender<AccountsWindow>) {
    let drag = gtk::DragSource::new();
    drag.set_actions(gtk::gdk::DragAction::MOVE);
    let from = pos as u32;
    drag.connect_prepare(move |_, _, _| {
        Some(gtk::gdk::ContentProvider::for_value(&from.to_value()))
    });
    row.add_controller(drag);

    let drop = gtk::DropTarget::new(gtk::glib::Type::U32, gtk::gdk::DragAction::MOVE);
    let to = pos;
    let input = sender.input_sender().clone();
    drop.connect_drop(move |_, value, _, _| {
        if let Ok(from) = value.get::<u32>() {
            let _ = input.send(AccountsInput::MoveRow {
                from: from as usize,
                to,
            });
            true
        } else {
            false
        }
    });
    row.add_controller(drop);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goa(
        email: &str,
        imap_host: &str,
        oauth2: bool,
        provider_type: &str,
    ) -> crate::services::goa::GoaMailAccount {
        crate::services::goa::GoaMailAccount {
            id: format!("account_{email}"),
            email: email.into(),
            name: String::new(),
            provider: String::new(),
            imap_host: imap_host.into(),
            imap_port: 993,
            imap_user: String::new(),
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_user: String::new(),
            smtp_separate: false,
            password_based: !oauth2,
            oauth2,
            provider_type: provider_type.into(),
            security: Default::default(),
        }
    }

    #[test]
    fn importable_goa_accounts_skip_configured_and_unconnectable_ones() {
        let listed = vec![
            goa("a@example.org", "imap.example.org", false, "imap_smtp"),
            goa("b@example.org", "imap.example.org", false, "imap_smtp"),
            goa("c@example.org", "", true, "ms_graph"),
            goa("d@example.org", "", true, "google"),
            goa("e@example.org", "imap.example.org", false, "imap_smtp"),
        ];
        let configured = vec![
            // Configured, case aside: not offered again.
            AccountConfig {
                email: "A@Example.org".into(),
                imap_host: "imap.example.org".into(),
                ..crate::config::blank_account()
            },
            // A broken import with neither an IMAP host nor Graph: offered again.
            AccountConfig {
                email: "b@example.org".into(),
                imap_host: "  ".into(),
                ..crate::config::blank_account()
            },
            // Over Graph with no IMAP host: configured.
            AccountConfig {
                email: "e@example.org".into(),
                protocol: Protocol::Graph,
                ..crate::config::blank_account()
            },
        ];
        let emails: Vec<String> = importable_of(listed, &configured)
            .into_iter()
            .map(|g| g.email)
            .collect();
        assert_eq!(emails, ["b@example.org", "c@example.org"]);
    }
}
