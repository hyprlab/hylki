//! The GNOME Online Accounts page: the accounts there to import.

use super::*;

pub(super) fn rebuild_goa_rows(
    list: &gtk::ListBox,
    goa: &[crate::services::goa::GoaMailAccount],
    sender: &ComponentSender<Welcome>,
) {
    while let Some(row) = list.first_child() {
        list.remove(&row);
    }
    for (i, g) in goa.iter().enumerate() {
        let row = adw::ActionRow::new();
        row.set_title(&g.email);
        row.set_subtitle(&g.provider);
        row.add_prefix(&crate::desktop::brand::mark(
            crate::ui::accounts::brand_for_goa(&g.provider),
            24,
            crate::desktop::brand::GENERIC_MAIL,
        ));
        let btn = gtk::Button::with_label(&i18n("Add"));
        btn.add_css_class("suggested-action");
        btn.set_valign(gtk::Align::Center);
        let s = sender.clone();
        btn.connect_clicked(move |_| s.input_sender().emit(WelcomeInput::ImportGoa(i)));
        row.add_suffix(&btn);
        list.append(&row);
    }
}

impl Welcome {
    pub(super) fn on_import_goa(
        &mut self,
        index: usize,
        widgets: &WelcomeWidgets,
        sender: ComponentSender<Self>,
    ) {
        if let Some(g) = self.goa.get(index).cloned() {
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
            self.added.push(account.email.clone());
            self.goa.remove(index);
            widgets.goa_status.set_visible(true);
            widgets
                .goa_status
                .set_css_classes(&["welcome-hint", "success"]);
            widgets.goa_status.set_text(&i18n_f(
                "✓ {email} added",
                &[("email", &(account.email).to_string())],
            ));
            let _ = sender.output(WelcomeOutput::ImportGoa(Box::new(account)));
            rebuild_goa_rows(&widgets.goa_list, &self.goa, &sender);
            widgets.goa_list.set_visible(!self.goa.is_empty());
            self.goa_imported = true;
            self.refresh_footer(widgets);
            widgets
                .acct_title
                .set_text(&i18n("Add another email account"));
        }
    }

    pub(super) fn on_rescan_goa(
        &mut self,
        widgets: &WelcomeWidgets,
        sender: ComponentSender<Self>,
    ) {
        self.goa = crate::services::goa::list_mail_accounts();
        self.goa
            .retain(|g| g.can_connect() && !self.added.contains(&g.email));
        rebuild_goa_rows(&widgets.goa_list, &self.goa, &sender);
        widgets.goa_list.set_visible(!self.goa.is_empty());
        // Everything found was added already: say so, not "none found".
        widgets
            .goa_empty
            .set_visible(self.goa.is_empty() && self.added.is_empty());
    }
}
