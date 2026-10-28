//! Building the panel: the model, and what `view!` cannot say about the
//! widgets (list models, factories, bindings, handlers), in the order
//! `init` runs it.

use super::account_list::importable_goa_accounts;
use super::form::{
    age_labels, auto_empty_labels, left_justify_wrapping_labels, row_matches, wrap_subtitle,
    CLEAN_LISTS_DAYS,
};
use super::*;

impl AccountsWindow {
    /// The model for `init`, with the sender lists filled.
    pub(super) fn new(init: AccountsInit, sender: &ComponentSender<Self>) -> Self {
        let goa = importable_goa_accounts(&init.accounts);
        let sig_source_btn = gtk::Button::with_label(&i18n("Edit HTML"));
        sig_source_btn.add_css_class("flat");
        sig_source_btn.set_valign(gtk::Align::Center);
        sig_source_btn.set_tooltip_text(Some(&i18n("Edit the signature's HTML source")));
        {
            let s = sender.input_sender().clone();
            sig_source_btn.connect_clicked(move |_| {
                let _ = s.send(AccountsInput::SignatureSourceToggle);
            });
        }

        let senders = relm4::factory::FactoryVecDeque::builder()
            .launch(gtk::ListBox::new())
            .forward(sender.input_sender(), |out| match out {
                SenderRowOutput::Remove(addr) => AccountsInput::RemoveSenderRow(addr),
            });
        let blacklist = relm4::factory::FactoryVecDeque::builder()
            .launch(gtk::ListBox::new())
            .forward(sender.input_sender(), |out| match out {
                SenderRowOutput::Remove(addr) => AccountsInput::RemoveBlacklistRow(addr),
            });

        let mut model = AccountsWindow {
            accounts: init.accounts,
            editing: None,
            emoji: None,
            avatar: None,
            preview_css: gtk::CssProvider::new(),
            list_css: gtk::CssProvider::new(),
            picture_mode: false,
            editor_seed: None,
            sig_editor: None,
            sig_source_btn,
            sig_edits: Vec::new(),
            sig_shown: 0,
            sig_default: None,
            sig_touched: false,
            sig_reading: false,
            label_synced: String::new(),
            goa,
            pending_oauth_refresh: None,
            editor_session: 0,
            editor_secrets: None,
            secrets_reading: false,
            save_waiting: false,
            directory_granted: false,
            directory_on: false,
            labels_granted: false,
            labels_on: false,
            alias_edits: Vec::new(),
            server_identities: Vec::new(),
            hidden_edits: Vec::new(),
            alias_editing: None,
            alias_dialog: None,
            folders_by_email: std::collections::HashMap::new(),
            folder_paths: Vec::new(),
            sent_copy_paths: Vec::new(),
            senders,
            sender_addrs: Vec::new(),
            blacklist,
            blacklist_addrs: Vec::new(),
            filter_rules: init.filters,
            filters_list: None,
            filters_query: Default::default(),
            form_save: Default::default(),
            senders_query: Default::default(),
            tags: init.tags,
            tags_list: None,
            smart_folders: init.smart_folders,
            smart_list: None,
            find_tags_spinner: None,
            apply_filters_spinner: None,
            apply_filters_label: None,
            filters_applying: false,
            find_tags_label: None,
            tag_scanning: false,
        };
        {
            let mut guard = model.senders.guard();
            for addr in &init.allowed_senders {
                model.sender_addrs.push(addr.clone());
                guard.push_back(addr.clone());
            }
        }
        {
            let mut guard = model.blacklist.guard();
            for addr in &init.blacklist {
                model.blacklist_addrs.push(addr.clone());
                guard.push_back(addr.clone());
            }
        }

        model
    }

    /// Hand the lists their widgets and fill them: filters, tags, smart
    /// folders, accounts, and the GNOME Online Accounts to import.
    pub(super) fn fill_lists(
        &mut self,
        widgets: &AccountsWindowWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let senders_box = self.senders.widget().clone();
        let blacklist_box = self.blacklist.widget().clone();
        self.filters_list = Some(widgets.filters.filters_list.clone());
        {
            let q = self.filters_query.clone();
            widgets
                .filters
                .filters_list
                .set_filter_func(move |row| row_matches(row, &q.borrow()));
            let q = self.senders_query.clone();
            senders_box.set_filter_func(move |row| row_matches(row, &q.borrow()));
            let q = self.senders_query.clone();
            blacklist_box.set_filter_func(move |row| row_matches(row, &q.borrow()));
        }
        self.rebuild_filter_rows(sender);
        self.tags_list = Some(widgets.tags.tags_list.clone());
        self.find_tags_spinner = Some(widgets.tags.find_tags_spinner.clone());
        self.apply_filters_spinner = Some(widgets.filters.apply_filters_spinner.clone());
        self.apply_filters_label = Some(widgets.filters.apply_filters_label.clone());
        self.find_tags_label = Some(widgets.tags.find_tags_label.clone());
        self.rebuild_tag_rows(sender);
        self.smart_list = Some(widgets.smart.smart_list.clone());
        self.rebuild_smart_rows(sender);
        let t_list = std::time::Instant::now();
        self.rebuild_account_list(&widgets.accounts.accounts_list, sender);
        tracing::debug!(
            "settings window: accounts list built in {:?}",
            t_list.elapsed()
        );
        self.rebuild_goa_list(&widgets.accounts.goa_list, sender);
        widgets.accounts.goa_group.set_visible(!self.goa.is_empty());
    }

    /// The editor's live preview and the rows that follow each other.
    pub(super) fn connect_editor(
        &self,
        widgets: &AccountsWindowWidgets,
        sender: &ComponentSender<Self>,
    ) {
        // Show the SMTP credential fields only when the toggle is on.
        widgets
            .form
            .smtp_separate_row
            .bind_property("active", &widgets.form.smtp_user_row, "visible")
            .sync_create()
            .build();
        widgets
            .form
            .smtp_separate_row
            .bind_property("active", &widgets.form.smtp_pass_row, "visible")
            .sync_create()
            .build();

        // Auto-fill the label from the email as it's typed (until customized).
        let es = sender.clone();
        widgets
            .form
            .email_row
            .connect_changed(move |_| es.input_sender().emit(AccountsInput::EmailChanged));

        // The preview circle follows the name, label and email (its
        // initials) as they are typed; its color is a stylesheet.
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &self.preview_css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            gtk::style_context_add_provider_for_display(
                &display,
                &self.list_css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        for row in [
            &widgets.form.name_row,
            &widgets.form.label_row,
            &widgets.form.email_row,
        ] {
            let s = sender.clone();
            row.connect_changed(move |_| s.input_sender().emit(AccountsInput::RefreshPreview));
        }
    }
}

/// The editor page's bindings and layout fixes, then the page taken out of
/// the navigation view until an editor opens.
pub(super) fn prepare_editor_page(
    widgets: &AccountsWindowWidgets,
    sender: &ComponentSender<AccountsWindow>,
) {
    // Narrowing to the directory means nothing without it.
    widgets
        .form
        .graph_directory_row
        .bind_property("active", &widgets.form.directory_only_row, "sensitive")
        .sync_create()
        .build();
    // Long enough for the hint, short enough that the combo's own value
    // is not what gets shortened instead.
    wrap_subtitle(&widgets.form.folder_sent_copy_row, 34);
    wrap_subtitle(&widgets.form.reply_copies_row, 30);
    // Left-justify the editor's wrapping labels (its group descriptions):
    // libadwaita 1.9 renders a group description fill-justified when its
    // text does not naturally fill the label's width, stretching the word
    // gaps (most visible on Special Folders, whose description spans the
    // full width). Single-line row labels are untouched.
    left_justify_wrapping_labels(&widgets.editor_page.clone().upcast());
    // The editor's sixty-odd rows stay out of the widget tree until an
    // editor opens, so the Settings window's first layout skips them.
    widgets.nav.remove(&widgets.editor_page);
    {
        // Leaving the editor page (however it was left) lets its
        // signature view go; see ReleaseSignatureEditor.
        let s = sender.clone();
        widgets.nav.connect_popped(move |_, page| {
            if page.tag().is_some_and(|t| t == "editor") {
                s.input_sender().emit(AccountsInput::ReleaseSignatureEditor);
            }
        });
    }
}

/// The editor's combo rows: their choices and the factories that show them
/// whole.
pub(super) fn fill_editor_choices(widgets: &AccountsWindowWidgets) {
    widgets
        .form
        .protocol_row
        .set_model(Some(&gtk::StringList::new(&["IMAP", "POP3"])));

    // The Provider dropdown picks both the sign-in method and (for known
    // providers) the servers. The default popup ellipsizes items; a factory
    // whose labels don't lets the list widen to the full option text.
    let provider_labels: Vec<&str> = PROVIDERS.iter().map(|p| p.label).collect();
    widgets
        .form
        .provider_row
        .set_model(Some(&gtk::StringList::new(&provider_labels)));
    widgets
        .form
        .provider_row
        .set_factory(Some(&provider_factory()));

    // Push override choices mirror AccountConfig::push (None / Some(true)
    // / Some(false), in that order).
    widgets
        .form
        .push_row
        .set_model(Some(&gtk::StringList::new(&[
            i18n("Follow Settings").as_str(),
            i18n("On").as_str(),
            i18n("Off").as_str(),
        ])));
    widgets
        .form
        .push_row
        .set_list_factory(Some(&non_ellipsizing_factory()));
    let sort_labels: Vec<String> = std::iter::once(i18n("Follow Settings"))
        .chain(crate::config::FolderSort::ALL.iter().map(|s| s.label()))
        .collect();
    let sort_labels: Vec<&str> = sort_labels.iter().map(String::as_str).collect();
    widgets
        .form
        .folder_sort_row
        .set_model(Some(&gtk::StringList::new(&sort_labels)));
    widgets
        .form
        .folder_sort_row
        .set_list_factory(Some(&non_ellipsizing_factory()));
    // The chosen value whole: "Follow Settings" cut to "Follow …" says nothing.
    widgets
        .form
        .folder_sort_row
        .set_factory(Some(&non_ellipsizing_factory()));
    let empty_labels = auto_empty_labels();
    let empty_labels: Vec<&str> = empty_labels.iter().map(String::as_str).collect();
    for row in [&widgets.form.empty_junk_row, &widgets.form.empty_trash_row] {
        row.set_model(Some(&gtk::StringList::new(&empty_labels)));
        row.set_list_factory(Some(&non_ellipsizing_factory()));
    }
    let list_labels = age_labels(CLEAN_LISTS_DAYS);
    let list_labels: Vec<&str> = list_labels.iter().map(String::as_str).collect();
    widgets
        .form
        .clean_lists_row
        .set_model(Some(&gtk::StringList::new(&list_labels)));
    widgets
        .form
        .clean_lists_row
        .set_list_factory(Some(&non_ellipsizing_factory()));
    let offline_labels: Vec<String> = crate::config::OfflineMail::ALL
        .iter()
        .map(|o| o.label())
        .collect();
    let offline_labels: Vec<&str> = offline_labels.iter().map(String::as_str).collect();
    widgets
        .form
        .offline_mail_row
        .set_model(Some(&gtk::StringList::new(&offline_labels)));
    widgets
        .form
        .offline_mail_row
        .set_list_factory(Some(&non_ellipsizing_factory()));
    widgets
        .form
        .offline_mail_row
        .set_factory(Some(&non_ellipsizing_factory()));
    wrap_subtitle(&widgets.form.offline_mail_row, 30);
    // Attachments ride along with a period's mail; the newest messages
    // have their own attachment download already.
    let attachments = widgets.form.offline_attachments_row.clone();
    widgets
        .form
        .offline_mail_row
        .connect_selected_notify(move |row| attachments.set_sensitive(row.selected() != 0));
    widgets.form.offline_attachments_row.set_sensitive(false);
    let reply_labels: Vec<String> = crate::config::ReplyCopies::ALL
        .iter()
        .map(|c| c.label())
        .collect();
    let reply_labels: Vec<&str> = reply_labels.iter().map(String::as_str).collect();
    widgets
        .form
        .reply_copies_row
        .set_model(Some(&gtk::StringList::new(&reply_labels)));
    widgets
        .form
        .reply_copies_row
        .set_list_factory(Some(&non_ellipsizing_factory()));
    widgets
        .form
        .reply_copies_row
        .set_factory(Some(&non_ellipsizing_factory()));
}

/// Tell the combined settings window when an editor page is up: every way
/// in or out (Save, back button, swipe) lands here.
pub(super) fn connect_editor_open(
    widgets: &AccountsWindowWidgets,
    sender: &ComponentSender<AccountsWindow>,
    form_save: FormSave,
) {
    // Tell the combined settings window when the editor subpage is up —
    // every way in or out (Save, back button, swipe) lands here.
    {
        let s = sender.output_sender().clone();
        widgets.nav.connect_visible_page_notify(move |nav| {
            let tag = nav.visible_page().and_then(|p| p.tag());
            let editor = match tag.as_deref() {
                Some("editor") => Some("accounts"),
                Some("filter") => Some("filters"),
                Some("tag") => Some("tags"),
                Some("smart") => Some("smart"),
                _ => None,
            };
            if !matches!(editor, Some("filters") | Some("tags") | Some("smart")) {
                form_save.borrow_mut().take();
            }
            let _ = s.send(AccountsOutput::EditorOpen(editor));
        });
    }
}
