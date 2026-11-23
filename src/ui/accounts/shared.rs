//! Microsoft 365 shared mailboxes (#701) in Settings → Accounts: added by
//! address, opened with the sign-in of a Microsoft 365 account from GNOME
//! Online Accounts, and listed as accounts of their own.

use super::*;

/// The accounts a shared mailbox can be opened with: Microsoft 365 from
/// GNOME Online Accounts, whose token carries the shared-mailbox scopes.
/// Hylki's own Microsoft sign-in does not ask for them.
pub(super) fn owners(accounts: &[AccountConfig]) -> Vec<&AccountConfig> {
    accounts
        .iter()
        .filter(|a| a.protocol == Protocol::Graph && a.goa_id.is_some() && !a.shared_mailbox)
        .collect()
}

impl AccountsWindow {
    /// Ask for the mailbox's address, and with more than one Microsoft 365
    /// account, which one opens it.
    #[allow(deprecated)] // adw::MessageDialog, see crate::ui::dialog
    pub(super) fn on_add_shared_prompt(&self, sender: ComponentSender<Self>, root: &adw::Bin) {
        let owners: Vec<String> = owners(&self.accounts)
            .iter()
            .map(|a| a.email.clone())
            .collect();
        let Some(first) = owners.first().cloned() else {
            return;
        };
        let body = (owners.len() == 1).then(|| {
            i18n_f(
                "Opened with {email}, which needs access to it in Exchange.",
                &[("email", &first)],
            )
        });
        let host = root.root().and_downcast::<gtk::Window>();
        let dialog = crate::ui::dialog::message_dialog(
            host.as_ref(),
            Some(&i18n("Add Shared Mailbox")),
            body.as_deref(),
        );
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let address = adw::EntryRow::new();
        address.set_title(&i18n("Email address"));
        address.set_input_purpose(gtk::InputPurpose::Email);
        list.append(&address);
        let owner_row = adw::ComboRow::new();
        owner_row.set_title(&i18n("Open with"));
        let names: Vec<&str> = owners.iter().map(String::as_str).collect();
        owner_row.set_model(Some(&gtk::StringList::new(&names)));
        owner_row.set_visible(owners.len() > 1);
        list.append(&owner_row);
        dialog.set_extra_child(Some(&list));

        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("add", &i18n("Add"));
        dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
        dialog.set_response_enabled("add", false);
        dialog.set_default_response(Some("add"));
        dialog.set_close_response("cancel");
        let d = dialog.clone();
        address.connect_changed(move |row| {
            let ok = crate::services::graph_shared::valid_address(&row.text());
            d.set_response_enabled("add", ok);
        });
        let d = dialog.clone();
        address.connect_entry_activated(move |row| {
            if crate::services::graph_shared::valid_address(&row.text()) {
                d.response("add");
            }
        });
        let entry = address.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp != "add" {
                return;
            }
            let owner = owners
                .get(owner_row.selected() as usize)
                .cloned()
                .unwrap_or_default();
            let address = entry.text().trim().to_string();
            sender
                .input_sender()
                .emit(AccountsInput::CheckSharedMailbox { owner, address });
        });
        dialog.present();
        address.grab_focus();
    }

    /// Open the mailbox once with the owner's token before it becomes an
    /// account, so one the user cannot open says so now.
    pub(super) fn on_check_shared(
        &mut self,
        owner: String,
        address: String,
        sender: ComponentSender<Self>,
        root: &adw::Bin,
    ) {
        if self
            .accounts
            .iter()
            .any(|a| a.email.eq_ignore_ascii_case(&address))
        {
            let host = root.root().and_downcast::<gtk::Window>();
            crate::ui::dialog::alert(
                host.as_ref(),
                &i18n("Could Not Add Mailbox"),
                &i18n_f("{address} is already in Mail Accounts.", &[("address", &address)]),
            );
            return;
        }
        let Some(owner) = self.accounts.iter().find(|a| a.email == owner).cloned() else {
            return;
        };
        if let Some(group) = &self.shared_group {
            group.set_sensitive(false);
        }
        sender.oneshot_command(async move {
            let checked = tokio::task::spawn_blocking(move || {
                crate::services::graph_shared::check(&owner, &address)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            AccountsCmd::SharedMailbox(checked)
        });
    }

    /// The check's answer: the new account, which the app saves and
    /// connects as it does an account switched on from GNOME's list, or why
    /// there is none.
    pub(super) fn on_shared_checked(
        &mut self,
        checked: Result<AccountConfig, String>,
        widgets: &AccountsWindowWidgets,
        sender: ComponentSender<Self>,
        root: &adw::Bin,
    ) {
        if let Some(group) = &self.shared_group {
            group.set_sensitive(true);
        }
        match checked {
            Ok(account) => {
                if self
                    .accounts
                    .iter()
                    .any(|a| a.email.eq_ignore_ascii_case(&account.email))
                {
                    return;
                }
                self.accounts.push(account.clone());
                self.rebuild_account_list(&widgets.accounts.accounts_list, &sender);
                let _ = sender.output(AccountsOutput::ImportGoa(Box::new(account)));
            }
            Err(why) => {
                let host = root.root().and_downcast::<gtk::Window>();
                crate::ui::dialog::alert(host.as_ref(), &i18n("Could Not Add Mailbox"), &why);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a Microsoft 365 account from GNOME Online Accounts opens a
    /// shared mailbox: not one signed in here, not another provider's, and
    /// not a shared mailbox itself.
    #[test]
    fn only_microsoft_from_gnome_opens_one() {
        let base = |email: &str| AccountConfig {
            email: email.into(),
            ..crate::config::blank_account()
        };
        let goa = AccountConfig {
            protocol: Protocol::Graph,
            goa_id: Some("g1".into()),
            oauth: true,
            ..base("ann@example.com")
        };
        let own_sign_in = AccountConfig {
            protocol: Protocol::Graph,
            oauth: true,
            ..base("bo@example.com")
        };
        let gmail = AccountConfig {
            goa_id: Some("g2".into()),
            ..base("cy@gmail.com")
        };
        let shared = AccountConfig {
            shared_mailbox: true,
            ..goa.clone()
        };
        let all = [goa, own_sign_in, gmail, shared];
        let found: Vec<&str> = owners(&all).iter().map(|a| a.email.as_str()).collect();
        assert_eq!(found, ["ann@example.com"]);
    }
}
